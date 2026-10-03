//! `rpg profile`: where the server a build produced spends its cycles, and `rpg profile-compare`,
//! which joins two builds' profiles by function and sorts them by the difference.
//!
//! `rpg bench` says how much slower one server is than another, and this says where. The server
//! starts under the same settings `rpg bench` uses, and each load runs as the workload of a
//! `perf record` attached to the postmaster, so every backend it forks for the load is sampled
//! and nothing else is. Each load is a fixed amount of work: `--transactions` per client for
//! pgbench's `select-only` and `tpcb-like` scripts, and the analytic set once. Both servers do
//! the same work, so their cycle counts compare directly.
//!
//! Samples are counted in user space only, with `cycles:u` where the machine counts cycles and
//! `cpu-clock:u` where it does not, as on most virtual machines. A function is named by its
//! symbol and the object it lives in. GCC splits some functions into clones like `foo.isra.0` or
//! `foo.cold`, which are counted under the function they came from, so that the join does not
//! depend on what each compiler chose to clone.

use crate::analytic;
use crate::bench::SETTINGS;
use crate::process::{capture, hostname, which};
use crate::records::{FailClass, Outcome, TestRecord};
use crate::stress::{Install, path_str};
use crate::suite::{SuitePlan, record};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The loads, in the order they run.
pub const LOADS: [&str; 3] = ["select-only", "tpcb-like", "analytic"];

/// What a profile run needs beyond the build.
#[derive(Debug, Clone)]
pub struct ProfilePlan {
    /// The build, with `suite` set to `profile`.
    pub suite: SuitePlan,
    /// The loads to profile, from [`LOADS`].
    pub loads: Vec<String>,
    /// pgbench clients, and as many threads.
    pub clients: usize,
    /// Transactions each pgbench client runs.
    pub transactions: u64,
    /// pgbench's scale factor.
    pub scale: u32,
    /// The analytic set's scale factor.
    pub scale_factor: f64,
    /// The perf event, or `None` to take cycles when the machine counts them.
    pub event: Option<String>,
}

/// The cycles one function took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Function {
    /// The symbol, without a clone suffix.
    pub symbol: String,
    /// The object it lives in, like `postgres` or `libc.so.6`.
    pub object: String,
    /// The event count of the samples taken in it.
    pub count: u64,
}

/// One load's profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Load {
    /// `select-only`, `tpcb-like` or `analytic`.
    pub name: String,
    /// Wall clock seconds the load took under perf.
    pub seconds: f64,
    /// The count over every function.
    pub total: u64,
    /// The functions, the largest count first.
    pub functions: Vec<Function>,
    /// Why the load stopped short, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// `profile.json`: what was profiled, on what, and the counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Profile {
    /// The pin.
    pub pin: String,
    /// The optimization level.
    pub level: String,
    /// The build system.
    pub system: String,
    /// The configuration.
    pub config: String,
    /// The first line of the building compiler's `--version`.
    pub compiler: String,
    /// The machine the server ran on.
    pub host: String,
    /// The day of the run.
    pub date: String,
    /// The perf event counted.
    pub event: String,
    /// pgbench clients.
    pub clients: usize,
    /// Transactions per pgbench client.
    pub transactions: u64,
    /// pgbench's scale factor.
    pub scale: u32,
    /// The analytic set's scale factor.
    pub scale_factor: f64,
    /// The loads, in the order they ran.
    pub loads: Vec<Load>,
}

impl Profile {
    /// Read a `profile.json`, or the one under a build directory.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() {
            path.join("profile").join("profile.json")
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("reading {}: {e}", file.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("reading {}: {e}", file.display()))
    }

    fn label(&self) -> String {
        format!("{} {} on {}", self.compiler, self.level, self.host)
    }

    fn setup(&self) -> String {
        format!(
            "{} counted, pgbench at scale {} with {} clients running {} transactions each, the analytic set at SF {}",
            self.event, self.scale, self.clients, self.transactions, self.scale_factor
        )
    }
}

/// The symbol a clone was made from: `foo` for `foo.isra.0`, `foo.part.1` or `foo.cold`. C
/// names have no dots, so everything from the first one on is the compiler's.
fn base_symbol(symbol: &str) -> &str {
    if symbol.starts_with('[') || symbol.starts_with("0x") {
        return symbol;
    }
    symbol.split('.').next().unwrap_or(symbol)
}

/// Read `perf report` output made with `-F period,dso,sym` and a tab between the fields. A
/// symbol perf could not name is counted as `[unknown]` in its object.
fn read_report(text: &str) -> Vec<Function> {
    let mut counts: BTreeMap<(String, String), u64> = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t').map(str::trim);
        let (Some(count), Some(object), Some(symbol)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            continue;
        };
        // perf marks a user space symbol `[.]` and a kernel one `[k]`.
        let symbol = match symbol.split_once("] ") {
            Some((mark, rest)) if mark.starts_with('[') => rest.trim(),
            _ => symbol,
        };
        let symbol = if symbol.is_empty() || symbol.starts_with("0x") {
            "[unknown]"
        } else {
            base_symbol(symbol)
        };
        let object = Path::new(object)
            .file_name()
            .map_or(object.to_string(), |n| n.to_string_lossy().into_owned());
        *counts.entry((symbol.to_string(), object)).or_default() += count;
    }
    let mut functions: Vec<Function> = counts
        .into_iter()
        .map(|((symbol, object), count)| Function {
            symbol,
            object,
            count,
        })
        .collect();
    functions.sort_by(|x, y| y.count.cmp(&x.count).then_with(|| x.symbol.cmp(&y.symbol)));
    functions
}

/// A count in thousands, millions or billions.
fn human(count: f64) -> String {
    let size = count.abs();
    if size >= 1e9 {
        format!("{:.2}G", count / 1e9)
    } else if size >= 1e6 {
        format!("{:.1}M", count / 1e6)
    } else if size >= 1e3 {
        format!("{:.1}K", count / 1e3)
    } else {
        format!("{count:.0}")
    }
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(count: u64) -> f64 {
    count as f64
}

/// One function in both profiles.
struct Row<'a> {
    symbol: &'a str,
    object: &'a str,
    a: u64,
    b: u64,
}

impl Row<'_> {
    fn difference(&self) -> f64 {
        as_f64(self.b) - as_f64(self.a)
    }
}

/// The two profiles as a Markdown report, one table per load with the `top` functions whose
/// counts differ the most, sorted by how many more `b` spent in them than `a`.
#[must_use]
pub fn compare(a: &Profile, b: &Profile, top: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {} against {}\n", b.label(), a.label());
    let _ = writeln!(out, "Setup: {}.\n", b.setup());
    if a.setup() != b.setup() {
        let _ = writeln!(
            out,
            "The two runs differ in more than the compiler, so their counts do not compare: {} for {}.\n",
            a.setup(),
            a.label()
        );
    }
    for load in &b.loads {
        let Some(base) = a.loads.iter().find(|l| l.name == load.name) else {
            let _ = writeln!(
                out,
                "## {}\n\nOnly {} ran this load.\n",
                load.name,
                b.label()
            );
            continue;
        };
        let _ = writeln!(out, "## {}\n", load.name);
        if let Some(why) = base.failed.as_ref().or(load.failed.as_ref()) {
            let _ = writeln!(out, "The load failed, {why}.\n");
            continue;
        }
        load_table(&mut out, base, load, top);
    }
    for load in a
        .loads
        .iter()
        .filter(|l| !b.loads.iter().any(|m| m.name == l.name))
    {
        let _ = writeln!(
            out,
            "## {}\n\nOnly {} ran this load.\n",
            load.name,
            a.label()
        );
    }
    out
}

fn load_table(out: &mut String, a: &Load, b: &Load, top: usize) {
    let mut rows: BTreeMap<(&str, &str), Row> = BTreeMap::new();
    for (side, load) in [(0, a), (1, b)] {
        for f in &load.functions {
            let row = rows
                .entry((f.symbol.as_str(), f.object.as_str()))
                .or_insert_with(|| Row {
                    symbol: &f.symbol,
                    object: &f.object,
                    a: 0,
                    b: 0,
                });
            if side == 0 {
                row.a += f.count;
            } else {
                row.b += f.count;
            }
        }
    }
    let total = as_f64(b.total) - as_f64(a.total);
    let ratio = if a.total == 0 {
        String::new()
    } else {
        format!(", {:.2} times as many", as_f64(b.total) / as_f64(a.total))
    };
    let _ = writeln!(
        out,
        "The first build took {} in {:.1}s and the second {} in {:.1}s{ratio}.\n",
        human(as_f64(a.total)),
        a.seconds,
        human(as_f64(b.total)),
        b.seconds
    );
    let mut rows: Vec<Row> = rows.into_values().filter(|r| r.a != r.b).collect();
    rows.sort_by(|x, y| y.difference().abs().total_cmp(&x.difference().abs()));
    rows.truncate(top);
    rows.sort_by(|x, y| y.difference().total_cmp(&x.difference()));
    out.push_str("| function | object | first | second | difference | share |\n");
    out.push_str("|---|---|---:|---:|---:|---:|\n");
    for row in &rows {
        let share = if a.total == b.total {
            String::new()
        } else {
            format!("{:.1}%", row.difference() / total * 100.0)
        };
        let sign = if row.difference() > 0.0 { "+" } else { "" };
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {sign}{} | {share} |",
            row.symbol,
            row.object,
            human(as_f64(row.a)),
            human(as_f64(row.b)),
            human(row.difference())
        );
    }
    out.push('\n');
}

/// Whether perf can count an event in user space here.
fn counts(perf: &Path, event: &str) -> bool {
    capture(perf, &["stat", "-x", ",", "-e", event, "--", "true"]).is_ok_and(|text| {
        text.lines().filter(|l| l.contains(event)).any(|l| {
            l.split(',')
                .next()
                .is_some_and(|n| n.trim().parse::<u64>().is_ok())
        })
    })
}

/// The postmaster's process id, the first line of `postmaster.pid`.
fn postmaster(install: &Install) -> Result<String, String> {
    let file = install.data().join("postmaster.pid");
    let text =
        std::fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    text.lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| l.parse::<u32>().is_ok())
        .ok_or_else(|| format!("{} has no process id", file.display()))
}

/// Run one load as the workload of a `perf record` on the postmaster, then read the counts.
fn profile_load(
    install: &Install,
    plan: &ProfilePlan,
    perf: &Path,
    event: &str,
    name: &str,
) -> Load {
    let mut load = Load {
        name: name.to_string(),
        seconds: 0.0,
        total: 0,
        functions: Vec::new(),
        failed: None,
    };
    if let Err(why) = measure(install, plan, perf, event, &mut load) {
        load.failed = Some(why);
    }
    load
}

fn measure(
    install: &Install,
    plan: &ProfilePlan,
    perf: &Path,
    event: &str,
    load: &mut Load,
) -> Result<(), String> {
    let name = load.name.clone();
    let name = name.as_str();
    let data = install.dir().join(format!("{name}.data"));
    let pid = postmaster(install)?;
    let mut workload: Vec<String> = Vec::new();
    if name == "analytic" {
        let sql = install.dir().join("analytic-queries.sql");
        let mut text = String::new();
        for (_, query) in analytic::queries() {
            let _ = writeln!(text, "{query}");
        }
        std::fs::write(&sql, text).map_err(|e| format!("writing {}: {e}", sql.display()))?;
        workload.extend(
            [
                path_str(&install.program("psql"))?,
                "-XAtq",
                "-v",
                "ON_ERROR_STOP=1",
                "-f",
                path_str(&sql)?,
                "postgres",
            ]
            .map(String::from),
        );
    } else {
        let clients = plan.clients.max(1).to_string();
        let transactions = plan.transactions.to_string();
        workload.extend(
            [
                path_str(&install.program("pgbench"))?,
                "-n",
                "-b",
                name,
                "-M",
                "prepared",
                "-c",
                clients.as_str(),
                "-j",
                clients.as_str(),
                "-t",
                transactions.as_str(),
                "postgres",
            ]
            .map(String::from),
        );
    }
    let done = install
        .tool(&format!("perf record {name}"), perf, &format!("{name}.log"))
        .args(["record", "-N", "-e", event, "-F", "4999", "-p", &pid, "-o"])
        .args([path_str(&data)?, "--"])
        .args(workload)
        .run()?;
    if !done.ok {
        return Err(format!("the load exited {:?}", done.code));
    }
    load.seconds = done.seconds;
    let report = capture(
        perf,
        &[
            "report",
            "-i",
            path_str(&data)?,
            "--stdio",
            "--no-children",
            "-q",
            "--sort",
            "dso,sym",
            "-F",
            "period,dso,sym",
            "-t",
            "\t",
        ],
    )?;
    load.functions = read_report(&report);
    load.total = load.functions.iter().map(|f| f.count).sum();
    if load.total == 0 {
        return Err("perf took no samples in the server".into());
    }
    // The samples are kept in the counts, and the raw data is large.
    std::fs::remove_file(&data).ok();
    Ok(())
}

/// Fill the database, warm the server up, and profile each load.
fn session(
    install: &Install,
    plan: &ProfilePlan,
    perf: &Path,
    event: &str,
) -> Result<Vec<Load>, String> {
    install.initdb(plan.clients.max(1) + 10, SETTINGS)?;
    if !install.pg_ctl(&["start"], "start.log")? {
        return Err("the server did not start".into());
    }
    let pgbench = plan.loads.iter().any(|l| l != "analytic");
    if pgbench {
        if !install
            .step("pgbench -i", "pgbench", "pgbench-init.log")
            .args(["-i", "-s", &plan.scale.to_string(), "-q", "postgres"])
            .run()?
            .ok
        {
            return Err("pgbench -i failed".into());
        }
        // Read the tables into memory before anything is counted.
        install.psql("select count(*) from pgbench_accounts")?;
    }
    if plan.loads.iter().any(|l| l == "analytic") {
        let sql = install.dir().join("analytic-load.sql");
        std::fs::write(&sql, analytic::load(plan.scale_factor))
            .map_err(|e| format!("writing {}: {e}", sql.display()))?;
        if !install
            .step("load the analytic set", "psql", "analytic-load.log")
            .args(["-XAtq", "-v", "ON_ERROR_STOP=1", "-f"])
            .args([path_str(&sql)?, "postgres"])
            .run()?
            .ok
        {
            return Err("loading the analytic set failed".into());
        }
        for (_, query) in analytic::queries() {
            install.psql(query)?;
        }
    }
    Ok(plan
        .loads
        .iter()
        .map(|name| profile_load(install, plan, perf, event, name))
        .collect())
}

/// Run it and write `profile/profile.json`.
pub fn run(plan: &ProfilePlan) -> Result<Profile, String> {
    let info = &plan.suite.info;
    if info.phase != crate::build::Phase::Built {
        return Err("the build did not finish, so there is nothing to profile".into());
    }
    if let Some(other) = plan.loads.iter().find(|l| !LOADS.contains(&l.as_str())) {
        return Err(format!(
            "{other} is not a load, the loads are {}",
            LOADS.join(", ")
        ));
    }
    if plan.transactions == 0 {
        return Err("--transactions must be at least 1".into());
    }
    if plan.scale_factor <= 0.0 || plan.scale_factor.is_nan() {
        return Err("--sf must be above 0".into());
    }
    crate::suite::refuse_root()?;
    let perf = which("perf").ok_or("perf is not on PATH")?;
    let event = match &plan.event {
        Some(event) => event.clone(),
        None if counts(&perf, "cycles:u") => "cycles:u".to_string(),
        None => "cpu-clock:u".to_string(),
    };
    let dir = plan.suite.out.join("profile");
    let install = Install::prepare(&plan.suite, &dir, "profile")?;
    let loads = session(&install, plan, &perf, &event);
    install.finish();
    std::fs::remove_dir_all(install.data()).ok();
    let profile = Profile {
        pin: info.pin.clone(),
        level: info.level.clone(),
        system: info.system.clone(),
        config: info.config.clone(),
        compiler: info.compiler.clone(),
        host: hostname(),
        date: crate::process::today(),
        event,
        clients: plan.clients.max(1),
        transactions: plan.transactions,
        scale: plan.scale,
        scale_factor: plan.scale_factor,
        loads: loads?,
    };
    let path = dir.join("profile.json");
    let text = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(profile)
}

/// One record per load, passed when perf counted it.
#[must_use]
pub fn records(plan: &ProfilePlan, profile: &Profile) -> Vec<TestRecord> {
    let artifacts = plan.suite.out.join("profile");
    profile
        .loads
        .iter()
        .map(|l| {
            let (outcome, class) = match l.failed {
                Some(_) => (Outcome::Failed, Some(FailClass::Error)),
                None => (Outcome::Passed, None),
            };
            record(
                &plan.suite,
                &format!("profile {}", l.name),
                outcome,
                class,
                None,
                &artifacts,
            )
        })
        .collect()
}

/// One line per load for the terminal, with the function that took the most.
#[must_use]
pub fn summary(profile: &Profile) -> Vec<String> {
    profile
        .loads
        .iter()
        .map(|l| match (&l.failed, l.functions.first()) {
            (Some(why), _) => format!("profile: {} failed: {why}", l.name),
            (None, Some(f)) => format!(
                "profile: {}: {} {} in {:.1}s, most in {} ({:.1}%)",
                l.name,
                human(as_f64(l.total)),
                profile.event,
                l.seconds,
                f.symbol,
                as_f64(f.count) / as_f64(l.total) * 100.0
            ),
            (None, None) => format!("profile: {}: no samples", l.name),
        })
        .collect()
}

/// Where a profile run's files go, for the paths printed at the end.
#[must_use]
pub fn json_path(out: &Path) -> PathBuf {
    out.join("profile").join("profile.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(symbol: &str, object: &str, count: u64) -> Function {
        Function {
            symbol: symbol.to_string(),
            object: object.to_string(),
            count,
        }
    }

    fn profile(compiler: &str, functions: Vec<Function>) -> Profile {
        Profile {
            pin: "18.0".into(),
            level: "-O2".into(),
            system: "autoconf".into(),
            config: "minimal".into(),
            compiler: compiler.into(),
            host: "box".into(),
            date: "2026-10-03".into(),
            event: "cycles:u".into(),
            clients: 4,
            transactions: 1000,
            scale: 10,
            scale_factor: 0.1,
            loads: vec![Load {
                name: "select-only".into(),
                seconds: 2.0,
                total: functions.iter().map(|f| f.count).sum(),
                functions,
                failed: None,
            }],
        }
    }

    #[test]
    fn clones_count_under_the_function_they_came_from() {
        assert_eq!(base_symbol("ExecInterpExpr"), "ExecInterpExpr");
        assert_eq!(base_symbol("heap_getnext.part.0"), "heap_getnext");
        assert_eq!(base_symbol("hash_search.cold"), "hash_search");
        assert_eq!(base_symbol("[unknown]"), "[unknown]");
    }

    #[test]
    fn a_report_is_summed_by_function_and_object() {
        let text = "# Period\tShared Object\tSymbol\n\
            \n\
            5000\tpostgres\t[.] ExecInterpExpr\n\
            700\tpostgres\t[.] slot_deform_heap_tuple.isra.0\n\
            300\tpostgres\t[.] slot_deform_heap_tuple\n\
            200\tlibc.so.6\t[.] __memcpy_avx_unaligned_erms\n\
            90\t/tmp/x/postgres\t[.] 0x00000000004a1b2c\n\
            10\tpostgres\t[.] 0x00000000004a1b30\n\
            junk\n";
        let functions = read_report(text);
        assert_eq!(
            functions,
            vec![
                function("ExecInterpExpr", "postgres", 5000),
                function("slot_deform_heap_tuple", "postgres", 1000),
                function("__memcpy_avx_unaligned_erms", "libc.so.6", 200),
                function("[unknown]", "postgres", 100),
            ]
        );
    }

    #[test]
    fn the_report_sorts_by_the_difference() {
        let a = profile(
            "gcc 14",
            vec![
                function("ExecInterpExpr", "postgres", 1_000_000),
                function("hash_search", "postgres", 400_000),
                function("pg_checksum_page", "postgres", 300_000),
                function("memcpy", "libc.so.6", 100_000),
            ],
        );
        let b = profile(
            "rucc 0.18",
            vec![
                function("ExecInterpExpr", "postgres", 4_000_000),
                function("hash_search", "postgres", 500_000),
                function("pg_checksum_page", "postgres", 100_000),
                function("memcpy", "libc.so.6", 100_000),
            ],
        );
        let report = compare(&a, &b, 40);
        assert!(
            report.starts_with("# rucc 0.18 -O2 on box against gcc 14 -O2 on box\n"),
            "{report}"
        );
        assert!(report.contains("## select-only"), "{report}");
        assert!(
            report.contains("1.8M in 2.0s and the second 4.7M in 2.0s, 2.61 times as many"),
            "{report}"
        );
        let exec = report.find("`ExecInterpExpr`").unwrap();
        let hash = report.find("`hash_search`").unwrap();
        let checksum = report.find("`pg_checksum_page`").unwrap();
        assert!(exec < hash && hash < checksum, "{report}");
        assert!(report.contains("| +3.0M | 103.4% |"), "{report}");
        assert!(report.contains("| -200.0K | -6.9% |"), "{report}");
        assert!(!report.contains("`memcpy`"), "{report}");
        assert!(
            !report.contains("differ in more than the compiler"),
            "{report}"
        );
        let short = compare(&a, &b, 1);
        assert!(
            short.contains("`ExecInterpExpr`") && !short.contains("`hash_search`"),
            "{short}"
        );
    }

    #[test]
    fn a_different_setup_is_called_out() {
        let a = profile("gcc 14", vec![function("f", "postgres", 10)]);
        let mut b = profile("rucc 0.18", vec![function("f", "postgres", 20)]);
        b.transactions = 2000;
        assert!(compare(&a, &b, 40).contains("differ in more than the compiler"));
        b.transactions = 1000;
        b.loads[0].failed = Some("the load exited Some(1)".into());
        assert!(compare(&a, &b, 40).contains("The load failed, the load exited Some(1)."));
    }

    #[test]
    fn json_round_trips() {
        let p = profile("gcc 14", vec![function("f", "postgres", 10)]);
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&text).unwrap(), p);
        assert!(!text.contains("failed"));
    }

    #[test]
    fn counts_read_in_thousands_millions_and_billions() {
        assert_eq!(human(999.0), "999");
        assert_eq!(human(1500.0), "1.5K");
        assert_eq!(human(-2_500_000.0), "-2.5M");
        assert_eq!(human(3_210_000_000.0), "3.21G");
    }
}
