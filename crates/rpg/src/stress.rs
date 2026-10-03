//! `rpg stress`: pgbench against a server a build produced, and then a check that nothing was
//! lost or made up on the way.
//!
//! The suites run one statement at a time, or a few sessions that take turns. A compiler that
//! gets memory ordering wrong in the lock free parts of the server, the spinlocks, the `LWLock`
//! wait lists, the atomics that stand in for them, passes all of that and then loses an update
//! once in a million under real concurrency. pgbench's default script is TPC-B: each transaction
//! moves an amount into one account, one teller and one branch and writes the amount into the
//! history table. So however many transactions ran, the four sums have to come out the same.
//!
//! The run is pgbench with the simple protocol for half the time and with prepared statements
//! for the other half, the sums, then an immediate stop so that the server has to replay its WAL
//! when it comes back, and the sums again. Any backend that died on a signal, any PANIC and any
//! failed assertion in the server log fails the run as well.

use crate::build::{Phase, environment};
use crate::process::{Step, capture};
use crate::records::{FailClass, Outcome, TestRecord};
use crate::suite::{SuitePlan, record};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What a stress run needs beyond the build.
#[derive(Debug, Clone)]
pub struct StressPlan {
    /// The build, with `suite` set to `stress`.
    pub suite: SuitePlan,
    /// How long pgbench runs, over both protocols.
    pub minutes: u32,
    /// pgbench clients, and as many threads.
    pub clients: usize,
    /// pgbench's scale factor, 100,000 accounts each.
    pub scale: u32,
}

/// What came of one piece of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The test name in the records.
    pub name: String,
    /// What happened.
    pub outcome: Outcome,
    /// One line on it, for the summary.
    pub note: String,
    /// Seconds, when it was timed.
    pub seconds: Option<u64>,
}

impl Check {
    fn new(name: &str, outcome: Outcome, note: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            outcome,
            note: note.into(),
            seconds: None,
        }
    }
}

/// The four sums of a TPC-B database, in the order accounts, tellers, branches, history.
#[must_use]
pub fn parse_sums(text: &str) -> Option<[i64; 4]> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut sums = [0; 4];
    let mut fields = line.split('|');
    for sum in &mut sums {
        *sum = fields.next()?.trim().parse().ok()?;
    }
    fields.next().is_none().then_some(sums)
}

/// Why a set of sums breaks the invariant, if it does.
#[must_use]
pub fn broken(sums: [i64; 4]) -> Option<String> {
    let [accounts, tellers, branches, history] = sums;
    (accounts != history || tellers != history || branches != history).then(|| {
        format!(
            "accounts sum to {accounts}, tellers to {tellers}, branches to {branches}, and the history to {history}"
        )
    })
}

/// What pgbench said about a run: the transactions it finished and the rate, or why it failed.
pub fn read_pgbench(text: &str) -> Result<(u64, String), String> {
    if let Some(line) = text.lines().find(|l| l.contains(" aborted in command")) {
        return Err(line.trim().to_string());
    }
    if let Some(failed) = text
        .lines()
        .find_map(|l| l.strip_prefix("number of failed transactions: "))
        && !failed.starts_with("0 ")
        && failed != "0"
    {
        return Err(format!("{} transactions failed", failed.trim()));
    }
    let done = text
        .lines()
        .find_map(|l| l.strip_prefix("number of transactions actually processed: "))
        .and_then(|rest| rest.split('/').next())
        .and_then(|n| n.trim().parse().ok())
        .ok_or("pgbench did not say how many transactions it processed")?;
    let tps = text
        .lines()
        .find_map(|l| l.strip_prefix("tps = "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("an unknown number of")
        .to_string();
    Ok((done, tps))
}

/// The first line of a server log that says something went badly wrong.
#[must_use]
pub fn trouble(log: &str) -> Option<String> {
    log.lines()
        .find(|l| {
            l.contains("terminated by signal")
                || l.contains("terminated by exception")
                || l.contains("PANIC:")
                || l.starts_with("TRAP:")
        })
        .map(|l| l.trim().to_string())
}

/// The directory holding `postgres` in a temporary install, which sits under the install prefix
/// and so is a few levels down.
#[must_use]
pub fn find_bin(root: &Path) -> Option<PathBuf> {
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        if dir.join("postgres").is_file() && dir.ends_with("bin") {
            return Some(dir);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !p.is_symlink())
            .collect();
        children.sort();
        children.reverse();
        queue.extend(children);
    }
    None
}

/// The programs of the install, run with its own libraries, and a server of its own to run.
pub(crate) struct Install {
    bin: PathBuf,
    env: BTreeMap<String, String>,
    socket: PathBuf,
    data: PathBuf,
    dir: PathBuf,
}

impl Install {
    /// Install the build into its temporary install and get ready to run a server from it, with
    /// the logs in `dir`, which is emptied first.
    pub(crate) fn prepare(plan: &SuitePlan, dir: &Path, tag: &str) -> Result<Self, String> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| format!("removing {}: {e}", dir.display()))?;
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let bin = temp_install(plan, dir)?;
        let lib = bin.with_file_name("lib");
        // A socket path has to fit in about a hundred bytes, which a build directory may not.
        let socket = std::env::temp_dir().join(format!("rpg-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&socket)
            .map_err(|e| format!("creating {}: {e}", socket.display()))?;
        let mut env = BTreeMap::new();
        env.insert("LD_LIBRARY_PATH".to_string(), lib.display().to_string());
        env.insert("PGHOST".to_string(), socket.display().to_string());
        env.insert("PGDATABASE".to_string(), "postgres".to_string());
        env.insert("PGUSER".to_string(), "postgres".to_string());
        Ok(Self {
            bin,
            env,
            socket,
            data: dir.join("data"),
            dir: dir.to_path_buf(),
        })
    }

    /// The directory the logs go to.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The data directory.
    pub(crate) fn data(&self) -> &Path {
        &self.data
    }

    /// Make the data directory and add `settings` to its `postgresql.conf`, after the lines that
    /// keep the server on the private socket.
    pub(crate) fn initdb(&self, max_connections: usize, settings: &str) -> Result<(), String> {
        let data = path_str(&self.data)?;
        if !self
            .step("initdb", "initdb", "initdb.log")
            .args(["-D", data, "-A", "trust", "-N", "-U", "postgres"])
            .run()?
            .ok
        {
            return Err("initdb failed".into());
        }
        let conf = self.data.join("postgresql.conf");
        let mut text = std::fs::read_to_string(&conf)
            .map_err(|e| format!("reading {}: {e}", conf.display()))?;
        let _ = write!(
            text,
            "\nlisten_addresses = ''\nunix_socket_directories = '{}'\nmax_connections = {max_connections}\n{settings}",
            self.socket.display()
        );
        std::fs::write(&conf, text).map_err(|e| format!("writing {}: {e}", conf.display()))
    }

    /// Stop whatever server is running without waiting for it, and remove the socket directory.
    pub(crate) fn finish(&self) {
        self.pg_ctl(&["-m", "immediate", "stop"], "stop.log").ok();
        std::fs::remove_dir_all(&self.socket).ok();
    }

    /// What psql prints for a query, unaligned and without headers.
    pub(crate) fn psql(&self, query: &str) -> Result<String, String> {
        let psql = self.bin.join("psql");
        // psql needs the install's libpq, which capture cannot set, so it goes through env(1).
        let library = format!("LD_LIBRARY_PATH={}", self.env["LD_LIBRARY_PATH"]);
        capture(
            Path::new("env"),
            &[
                &library,
                path_str(&psql)?,
                "-XAtq",
                "-v",
                "ON_ERROR_STOP=1",
                "-h",
                path_str(&self.socket)?,
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-c",
                query,
            ],
        )
    }

    /// One of the install's programs.
    pub(crate) fn program(&self, name: &str) -> PathBuf {
        self.bin.join(name)
    }

    pub(crate) fn step(&self, label: &str, program: &str, log: &str) -> Step {
        self.tool(label, &self.program(program), log)
    }

    /// A program from outside the install, run with the install's environment.
    pub(crate) fn tool(&self, label: &str, program: &Path, log: &str) -> Step {
        Step::new(label, program, &self.dir, &self.dir.join(log)).envs(&self.env)
    }

    pub(crate) fn pg_ctl(&self, action: &[&str], log: &str) -> Result<bool, String> {
        let server_log = self.dir.join("server.log");
        let mut args = vec!["-D", path_str(&self.data)?, "-w", "-t", "300"];
        if action == ["start"] {
            args.extend(["-l", path_str(&server_log)?]);
        }
        args.extend(action);
        Ok(self
            .step(&format!("pg_ctl {}", action.join(" ")), "pg_ctl", log)
            .args(args)
            .run()?
            .ok)
    }

    fn sums(&self) -> Result<[i64; 4], String> {
        let query = "select (select sum(abalance) from pgbench_accounts), \
            (select sum(tbalance) from pgbench_tellers), \
            (select sum(bbalance) from pgbench_branches), \
            (select coalesce(sum(delta), 0) from pgbench_history)";
        let text = self.psql(query)?;
        parse_sums(&text)
            .ok_or_else(|| format!("could not read the sums from psql: {}", text.trim()))
    }
}

pub(crate) fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Install the build into its temporary install, as the suites do before they run.
fn temp_install(plan: &SuitePlan, dir: &Path) -> Result<PathBuf, String> {
    let build_dir = PathBuf::from(&plan.info.build_dir);
    let (env, unset) = environment(&plan.out);
    let mut step = if plan.info.system == "meson" {
        Step::new(
            "meson test --suite setup",
            "meson",
            &build_dir,
            &dir.join("install.log"),
        )
        .args(["test", "--suite", "setup"])
    } else {
        Step::new(
            "make temp-install",
            crate::process::make(),
            &build_dir,
            &dir.join("install.log"),
        )
        .args([
            format!("-j{}", plan.info.jobs.max(1)),
            "temp-install".into(),
        ])
    }
    .envs(&env);
    step.unset = unset;
    if !step.run()?.ok {
        return Err("the temporary install failed, so there is no server to stress".into());
    }
    find_bin(&build_dir.join("tmp_install"))
        .ok_or_else(|| "the temporary install has no bin/postgres".to_string())
}

/// Run pgbench for one protocol and say how it went.
fn bench(
    install: &Install,
    plan: &StressPlan,
    protocol: &str,
    seconds: u64,
) -> Result<Check, String> {
    let name = format!("pgbench {protocol}");
    let log = format!("pgbench-{protocol}.log");
    let clients = plan.clients.max(1).to_string();
    // Without `-n` pgbench truncates the history table before it starts, which would throw away
    // what the first protocol wrote and leave the sums apart for no fault of the server's.
    let done = install
        .step(&name, "pgbench", &log)
        .args([
            "-n",
            "-h",
            path_str(&install.socket)?,
            "-M",
            protocol,
            "-c",
            &clients,
            "-j",
            &clients,
            "-T",
            &seconds.to_string(),
            "postgres",
        ])
        .run()?;
    let text = std::fs::read_to_string(install.dir.join(&log)).unwrap_or_default();
    let mut check = match read_pgbench(&text) {
        Ok((transactions, tps)) if done.ok => Check::new(
            &name,
            Outcome::Passed,
            format!("{transactions} transactions, {tps} a second"),
        ),
        Ok(_) => Check::new(
            &name,
            Outcome::Failed,
            format!("pgbench exited {:?}", done.code),
        ),
        Err(why) => Check::new(&name, Outcome::Failed, why),
    };
    check.seconds = Some(seconds);
    Ok(check)
}

fn invariant(install: &Install, name: &str) -> Check {
    match install.sums() {
        Ok(sums) => match broken(sums) {
            Some(why) => Check::new(name, Outcome::Failed, why),
            None => Check::new(name, Outcome::Passed, format!("every sum is {}", sums[3])),
        },
        Err(why) => Check::new(name, Outcome::Failed, why),
    }
}

/// Run it. The checks come back in the order they ran, and a check that could not run because an
/// earlier one failed is left out.
pub fn run(plan: &StressPlan) -> Result<Vec<Check>, String> {
    let info = &plan.suite.info;
    if info.phase != Phase::Built {
        return Ok(vec![Check::new(
            "*",
            Outcome::BuildFailed,
            "the build did not finish",
        )]);
    }
    crate::suite::refuse_root()?;
    let dir = plan.suite.out.join("stress");
    let install = Install::prepare(&plan.suite, &dir, "stress")?;
    let result = session(&install, plan);
    // Whatever happened, leave no server running.
    install.finish();
    let mut checks = result?;
    let log = std::fs::read_to_string(dir.join("server.log")).unwrap_or_default();
    checks.push(match trouble(&log) {
        Some(line) => Check::new("server log", Outcome::Crashed, line),
        None => Check::new(
            "server log",
            Outcome::Passed,
            "no crash, PANIC or failed assertion",
        ),
    });
    if checks.iter().all(|c| c.outcome == Outcome::Passed) {
        // The data directory is the bulk of it and nothing needs it once the sums came out right.
        std::fs::remove_dir_all(&install.data).ok();
    }
    Ok(checks)
}

fn session(install: &Install, plan: &StressPlan) -> Result<Vec<Check>, String> {
    // A small buffer pool and frequent checkpoints keep the buffer replacement and the
    // checkpointer busy while pgbench runs, and they are where the lock free code is.
    install.initdb(
        plan.clients.max(1) + 10,
        "shared_buffers = 16MB\ncheckpoint_timeout = 30s\nmax_wal_size = 64MB\n\
         synchronous_commit = off\nlog_checkpoints = on\n",
    )?;
    if !install.pg_ctl(&["start"], "start.log")? {
        return Err("the server did not start".into());
    }
    if !install
        .step("pgbench -i", "pgbench", "pgbench-init.log")
        .args(["-i", "-s", &plan.scale.to_string(), "-q", "postgres"])
        .run()?
        .ok
    {
        return Err("pgbench -i failed".into());
    }
    let half = (u64::from(plan.minutes) * 60 / 2).max(1);
    let mut checks = vec![
        bench(install, plan, "simple", half)?,
        bench(install, plan, "prepared", half)?,
    ];
    checks.push(invariant(install, "invariant"));
    install.pg_ctl(&["-m", "immediate", "stop"], "crash.log")?;
    if install.pg_ctl(&["start"], "restart.log")? {
        checks.push(invariant(install, "invariant after recovery"));
    } else {
        checks.push(Check::new(
            "invariant after recovery",
            Outcome::Failed,
            "the server did not come back after an immediate stop",
        ));
    }
    Ok(checks)
}

/// The records of a run.
#[must_use]
pub fn records(plan: &StressPlan, checks: &[Check]) -> Vec<TestRecord> {
    let artifacts = plan.suite.out.join("stress");
    checks
        .iter()
        .map(|c| {
            let class = (c.outcome == Outcome::Failed).then_some(FailClass::Error);
            #[allow(clippy::cast_precision_loss)]
            let seconds = c.seconds.map(|s| s as f64);
            record(&plan.suite, &c.name, c.outcome, class, seconds, &artifacts)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sums_come_from_one_psql_line() {
        assert_eq!(parse_sums("12|12|12|12\n"), Some([12, 12, 12, 12]));
        assert_eq!(parse_sums("\n-5|3|3|3"), Some([-5, 3, 3, 3]));
        assert_eq!(parse_sums("1|2|3"), None);
        assert_eq!(parse_sums("1|2|3|4|5"), None);
        assert_eq!(parse_sums("|2|3|4"), None);
    }

    #[test]
    fn the_invariant_needs_all_four_sums_equal() {
        assert_eq!(broken([7, 7, 7, 7]), None);
        let why = broken([7, 7, 6, 7]).unwrap();
        assert!(why.contains("branches to 6"), "{why}");
    }

    #[test]
    fn pgbench_output_gives_the_count_and_the_rate() {
        let text = "transaction type: <builtin: TPC-B (sort of)>\n\
            number of transactions actually processed: 48213\n\
            number of failed transactions: 0 (0.000%)\n\
            tps = 1606.912345 (without initial connection time)\n";
        assert_eq!(read_pgbench(text), Ok((48213, "1606.912345".to_string())));
        let failed = text.replace(
            "failed transactions: 0 (0.000%)",
            "failed transactions: 3 (0.006%)",
        );
        assert_eq!(
            read_pgbench(&failed),
            Err("3 (0.006%) transactions failed".to_string())
        );
        let aborted =
            format!("pgbench: error: client 3 aborted in command 4 (SQL) of script 0\n{text}");
        assert!(
            read_pgbench(&aborted)
                .unwrap_err()
                .contains("client 3 aborted")
        );
        assert!(read_pgbench("pgbench: error: connection failed").is_err());
    }

    #[test]
    fn the_server_log_is_searched_for_crashes_panics_and_traps() {
        let quiet = "LOG:  checkpoint starting: time\nLOG:  checkpoint complete\n";
        assert_eq!(trouble(quiet), None);
        let crashed = format!(
            "{quiet}LOG:  server process (PID 12) was terminated by signal 11: Segmentation fault\n"
        );
        assert!(trouble(&crashed).unwrap().contains("signal 11"));
        assert!(trouble("PANIC:  could not locate a valid checkpoint record").is_some());
        assert!(trouble("TRAP: failed Assert(\"x\"), File: \"lwlock.c\"").is_some());
    }

    #[test]
    fn the_bin_directory_is_found_under_the_install_prefix() {
        let root = std::env::temp_dir().join(format!("rpg-find-bin-{}", std::process::id()));
        let bin = root.join("home/pg/install/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(root.join("home/pg/install/lib")).unwrap();
        assert_eq!(find_bin(&root), None);
        std::fs::write(bin.join("postgres"), "").unwrap();
        assert_eq!(find_bin(&root), Some(bin));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
