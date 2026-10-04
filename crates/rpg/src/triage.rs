//! `rpg triage`: what the failures of a run have in common.
//!
//! A compiler bug in one function the executor calls for every row fails a hundred regression
//! tests at once, and read one diff at a time that looks like a hundred problems. This reads what
//! `rpg test` kept under `results/` and puts each failure under a signature, the first thing that
//! went wrong in it, so that the hundred show up as one group with a hundred names under it.
//!
//! A test's signature is the crash when the server it ran against crashed: the `TRAP:` line of a
//! failed assertion, or else the line saying which signal the backend died of. Otherwise it is the
//! first line the test printed that it should not have, or the first it should have printed and
//! did not, from the test's piece of the diffs. A TAP script's is the first crash in its log, or
//! else its first failed test. Numbers become `N` and addresses `<addr>` in all of them, so that
//! an OID or a PID does not split a group.
//!
//! Core files are the other half. Each one is given to `gdb` with the executable it came from, the
//! backtrace of every thread is written next to the report, and the core is grouped by the signal
//! and the functions at the top of the crashing thread's stack. A core from `SIGQUIT` is listed
//! apart and not counted: Postgres sends that signal to every child on an immediate shutdown and
//! to every other backend after one crashes, so such a core is expected, and the crash that
//! caused it has a core of its own.
//!
//! macOS writes no core files unless asked, and writes a crash report instead: a `.ips` file under
//! `~/Library/Logs/DiagnosticReports` for every process that dies of a signal. On macOS those take
//! the place of cores. The reports written since the run started by a Postgres program, one under
//! the build directory or one of the names Postgres installs, are read, the faulting thread's
//! frames are written next to the report, and the crash is grouped by its signal and the top three
//! of those frames, the same way a core is.
//!
//! Windows has neither, and Postgres writes a minidump of its own instead, which `rpg test` moves
//! into the run's `crashdumps/`. Each one is given to `cdb` when it is installed, and grouped by
//! the exception and the top three frames of the thread it happened on, which `cdb` names from
//! what `postgres.exe` exports. Without `cdb` a minidump is still counted, under a group that says
//! it was not read. Postgres writes the dump from inside the dying process and now and then gives
//! up partway, which the server log calls `could not write crash dump`. `cdb` cannot open what is
//! left, so that dump is listed as unread with `cdb`'s output beside it, and the crash is still
//! grouped by its line in the server log. Windows Error Reporting writes a whole one from outside
//! the process when `LocalDumps` in the registry asks it to, as `postgres.exe.<pid>.dmp` in the
//! folder named there. Those are read the same way when the folder is under `results/`, and a
//! crash that left both kinds is counted once, by the one Windows wrote.

use regex::Regex;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

/// One failure: which test, and the file its signature came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Failure {
    /// The test, named as closely to the records as the file allows.
    pub test: String,
    /// The file, relative to the build directory.
    pub file: String,
}

/// What the triage found.
#[derive(Debug, Default)]
pub struct Triage {
    /// Signature to the failures under it.
    pub groups: BTreeMap<String, Vec<Failure>>,
    /// Core files that could not be read, with the reason.
    pub unread: Vec<(String, String)>,
    /// How many core files, crash reports and minidumps were counted.
    pub cores: usize,
    /// Cores from `SIGQUIT`, by file name, which are not failures.
    pub quit: Vec<String>,
}

impl Triage {
    fn add(&mut self, signature: String, test: String, file: String) {
        self.groups
            .entry(signature)
            .or_default()
            .push(Failure { test, file });
    }

    /// How many failures there are over every group.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.groups.values().map(Vec::len).sum()
    }

    /// The groups, largest first.
    #[must_use]
    pub fn sorted(&self) -> Vec<(&String, &Vec<Failure>)> {
        let mut groups: Vec<_> = self.groups.iter().collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
        groups
    }
}

static LOG_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d(\.\d+)? \S+ \[\d+\] ").expect("a valid regex")
});
static HEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]+").expect("a valid regex"));
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("a valid regex"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("a valid regex"));
static FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#\d+\s+(?:0x[0-9a-fA-F]+ in )?([^\s(]+) \(").expect("a valid regex")
});
static EXCEPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Last event: .*code ([0-9a-fA-F]{8})").expect("a valid regex"));
static CDB_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[0-9a-f]{2,} )?(\S+)$").expect("a valid regex"));
static SIGNAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Program terminated with signal (\w+)").expect("a valid regex"));

/// The longest a signature gets, in characters.
const LONGEST: usize = 160;

/// A line with what changes from run to run taken out: the server log's time and PID prefix,
/// addresses and numbers, and runs of white space.
#[must_use]
pub fn normalize(line: &str) -> String {
    let line = LOG_PREFIX.replace(line.trim(), "");
    let line = HEX.replace_all(&line, "<addr>");
    let line = DIGITS.replace_all(&line, "N");
    let line = SPACE.replace_all(line.trim(), " ");
    line.chars().take(LONGEST).collect()
}

/// The first crash a server log or a TAP log records: a failed assertion's `TRAP:` line, or else
/// the line saying a process died of a signal, or of an exception on Windows.
#[must_use]
pub fn crash_line(log: &str) -> Option<String> {
    log.lines()
        .find(|l| l.contains("TRAP:"))
        .or_else(|| {
            log.lines().find(|l| {
                l.contains("terminated by signal") || l.contains("terminated by exception")
            })
        })
        .map(|l| format!("crash: {}", normalize(l)))
}

/// Each test's piece of a `regression.diffs`, as the test's name and the first line that differs.
#[must_use]
pub fn diff_chunks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, Option<String>, Option<String>)> = None;
    let finish = |c: (String, Option<String>, Option<String>), out: &mut Vec<(String, String)>| {
        let signature =
            c.1.or(c.2)
                .unwrap_or_else(|| "a diff with no changed lines".to_string());
        out.push((c.0, signature));
    };
    for line in text.lines() {
        if line.starts_with("diff ") {
            if let Some(c) = current.take() {
                finish(c, &mut out);
            }
            let file = line.split_whitespace().last().unwrap_or_default();
            let name = Path::new(file)
                .file_stem()
                .map_or_else(|| file.to_string(), |s| s.to_string_lossy().into_owned());
            current = Some((name, None, None));
            continue;
        }
        let Some(c) = current.as_mut() else { continue };
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if c.1.is_none() && !rest.trim().is_empty() {
                c.1 = Some(format!("+ {}", normalize(rest)));
            }
        } else if let Some(rest) = line.strip_prefix('-')
            && c.2.is_none()
            && !rest.trim().is_empty()
        {
            c.2 = Some(format!("- {}", normalize(rest)));
        }
    }
    if let Some(c) = current {
        finish(c, &mut out);
    }
    out
}

/// A TAP script's signature: the first crash in its log, or else its first failed test.
#[must_use]
pub fn tap_signature(log: &str) -> String {
    crash_line(log)
        .or_else(|| {
            log.lines()
                .find_map(|l| l.find("Failed test").map(|at| &l[at..]))
                .map(normalize)
        })
        .or_else(|| {
            log.lines()
                .find(|l| l.contains("Tests were run but no plan") || l.contains("died"))
                .map(normalize)
        })
        .unwrap_or_else(|| "a failed script whose log names no failed test".to_string())
}

/// The signature of a core from `gdb`'s backtrace: the signal and the first three functions of the
/// thread that crashed, which `bt` prints before `thread apply all bt` prints every thread.
#[must_use]
pub fn core_signature(program: &str, backtrace: &str) -> String {
    let signal = SIGNAL
        .captures(backtrace)
        .map_or("an unknown signal", |c| c.get(1).map_or("", |m| m.as_str()));
    let frames: Vec<&str> = backtrace
        .lines()
        .take_while(|l| !l.starts_with("Thread "))
        .filter_map(|l| FRAME.captures(l).and_then(|c| c.get(1)).map(|m| m.as_str()))
        .take(3)
        .collect();
    let stack = if frames.is_empty() {
        "no frames".to_string()
    } else {
        frames.join(" < ")
    };
    format!("core of {program}, {signal} in {stack}")
}

/// The signature of a minidump from what `cdb` printed over it: the exception and the first three
/// frames of `kc`, which follow its `Call Site` header one to a line until `quit:`, with the offsets
/// into each function left off so that two builds of the same code agree. `kc` numbers its frames
/// when `.kframes` or `kn` asks it to, so a leading frame number is allowed and dropped.
#[must_use]
pub fn minidump_signature(program: &str, text: &str) -> String {
    let exception = EXCEPTION.captures(text).and_then(|c| c.get(1)).map_or_else(
        || "an unknown exception".to_string(),
        |m| format!("exception 0x{}", m.as_str().to_ascii_lowercase()),
    );
    let frames: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.contains("Call Site"))
        .skip(1)
        .map(str::trim)
        .take_while(|l| !l.is_empty() && *l != "quit:")
        .filter_map(|l| CDB_FRAME.captures(l).and_then(|c| c.get(1)))
        .map(|m| m.as_str().split('+').next().unwrap_or_default())
        .take(3)
        .collect();
    let stack = if frames.is_empty() {
        "no frames".to_string()
    } else {
        frames.join(" < ")
    };
    format!("minidump of {program}, {exception} in {stack}")
}

/// The program and process a minidump came from. Postgres names its dumps
/// `postgres-pid<pid>-<ticks>.mdmp`, and Windows Error Reporting names its `postgres.exe.<pid>.dmp`.
#[must_use]
pub fn minidump_origin(name: &str) -> (&str, &str) {
    if let Some((program, rest)) = name.split_once("-pid") {
        return (program, rest.split('-').next().unwrap_or_default());
    }
    let mut parts = name.split('.');
    let program = parts.next().unwrap_or_default();
    let pid = parts.find(|p| p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty());
    (program, pid.unwrap_or_default())
}

/// What `cdb` runs over a minidump: the exception, the stack of the thread it happened on, and out.
const CDB_COMMANDS: &str = ".lastevent; .ecxr; kc 30; q";

/// `cdb` when it is installed, on the path or where the Windows SDK puts it, which is where
/// GitHub's Windows images have it.
fn cdb() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("cdb.exe"))
        .chain(std::iter::once(PathBuf::from(
            r"C:\Program Files (x86)\Windows Kits\10\Debuggers\x64\cdb.exe",
        )))
        .find(|p| p.is_file())
}

/// The minidumps among the files of `results/`. A crash can leave two, one from Postgres and one
/// from Windows Error Reporting, and then only the one Windows wrote is kept, since it is written
/// from outside the process and is whole.
fn minidump_files(all: &[PathBuf]) -> Vec<&PathBuf> {
    let file_name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let has = |p: &Path, ext: &str| p.extension().is_some_and(|e| e == ext);
    let reported: Vec<(String, String)> = all
        .iter()
        .filter(|p| has(p, "dmp"))
        .map(|p| {
            let name = file_name(p);
            let (program, pid) = minidump_origin(&name);
            (program.to_string(), pid.to_string())
        })
        .collect();
    all.iter()
        .filter(|p| {
            if has(p, "dmp") {
                return true;
            }
            let name = file_name(p);
            let (program, pid) = minidump_origin(&name);
            has(p, "mdmp") && !reported.iter().any(|(r, id)| r == program && id == pid)
        })
        .collect()
}

/// Add the minidumps among the files of `results/`, read by `cdb` when there is one.
fn minidumps(triage: &mut Triage, out: &Path, all: &[PathBuf], cdb: Option<&Path>) {
    let traces = out.join("triage");
    for dump in minidump_files(all) {
        let name = dump
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let program = minidump_origin(&name).0.to_string();
        let Some(cdb) = cdb else {
            triage.cores += 1;
            let signature = format!("minidump of {program}, not read because cdb is not installed");
            triage.add(signature, name, relative_to(out, dump));
            continue;
        };
        let path = dump.display().to_string();
        let text = match crate::process::capture_all(cdb, &["-z", &path, "-c", CDB_COMMANDS]) {
            Ok(text) => text,
            Err(e) => {
                triage.unread.push((name, e));
                continue;
            }
        };
        std::fs::create_dir_all(&traces).ok();
        let trace = traces.join(format!("{name}.txt"));
        std::fs::write(&trace, &text).ok();
        if EXCEPTION.is_match(&text) {
            triage.cores += 1;
            triage.add(
                minidump_signature(&program, &text),
                name,
                relative_to(out, &trace),
            );
        } else {
            let said = format!(
                "cdb did not name the exception, see {}",
                relative_to(out, &trace)
            );
            triage.unread.push((name, said));
        }
    }
}

/// Whether a core came from `SIGQUIT`, which Postgres sends on purpose.
#[must_use]
pub fn is_quit(backtrace: &str) -> bool {
    SIGNAL
        .captures(backtrace)
        .and_then(|c| c.get(1))
        .is_some_and(|m| m.as_str() == "SIGQUIT")
}

/// The executable a core came from, when `kernel.core_pattern` has `%E` in it, which writes the
/// path with `!` for each `/`: `core.!tmp!pg!bin!postgres.4242`.
#[must_use]
pub fn core_program(name: &str) -> Option<PathBuf> {
    let rest = name.strip_prefix("core.")?;
    let (path, _) = rest.rsplit_once('.')?;
    path.starts_with('!')
        .then(|| PathBuf::from(path.replace('!', "/")))
}

/// A macOS crash report, the parts of it triage uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashReport {
    /// The process's name, `procName`.
    pub program: String,
    /// Its executable, `procPath`.
    pub path: String,
    /// Its PID.
    pub pid: Option<u64>,
    /// The signal, such as `SIGSEGV`, or the exception type when the report names no signal.
    pub signal: String,
    /// The exception's subtype, such as `KERN_INVALID_ADDRESS at 0x0`, when there is one.
    pub subtype: Option<String>,
    /// The faulting thread's frames, innermost first: the symbol when the report has one, and
    /// otherwise the image and the offset into it.
    pub frames: Vec<String>,
}

impl CrashReport {
    /// The signature, in the shape of a core's: the signal and the top three frames.
    #[must_use]
    pub fn signature(&self) -> String {
        let stack = if self.frames.is_empty() {
            "no frames".to_string()
        } else {
            self.frames
                .iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" < ")
        };
        format!(
            "crash report of {}, {} in {stack}",
            self.program, self.signal
        )
    }

    /// What goes next to the report under `triage/`.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = format!("process: {}\npath:    {}\n", self.program, self.path);
        if let Some(pid) = self.pid {
            let _ = writeln!(text, "pid:     {pid}");
        }
        let _ = writeln!(text, "signal:  {}", self.signal);
        if let Some(subtype) = &self.subtype {
            let _ = writeln!(text, "subtype: {subtype}");
        }
        text.push_str("\nThe faulting thread:\n");
        for (at, frame) in self.frames.iter().enumerate() {
            let _ = writeln!(text, "#{at:<3} {frame}");
        }
        text
    }
}

/// Read a `.ips` crash report.
///
/// Since macOS 12 a report is two JSON documents, a line of header with `bug_type` and the
/// process's name, then the report proper with `procName`, `procPath`, `exception`,
/// `faultingThread`, `threads` and `usedImages`. A frame names its symbol when the image has
/// one, and otherwise only the image, by index into `usedImages`, and an offset into it. Reports
/// of other kinds, such as hangs, have another `bug_type` and are not crashes.
pub fn parse_ips(text: &str) -> Result<CrashReport, String> {
    let (header, body) = text.split_once('\n').unwrap_or(("", text));
    let header: serde_json::Value = serde_json::from_str(header).unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(body)
        .or_else(|_| serde_json::from_str(text))
        .map_err(|e| format!("not a JSON crash report: {e}"))?;
    if let Some(kind) = header.get("bug_type").and_then(serde_json::Value::as_str)
        && kind != "309"
    {
        return Err(format!(
            "a report of kind {kind}, which is not a crash (309)"
        ));
    }
    let string = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let program = string(&body, "procName")
        .or_else(|| string(&header, "app_name"))
        .or_else(|| string(&header, "name"))
        .ok_or("the report names no process")?;
    let exception = body.get("exception").cloned().unwrap_or_default();
    let signal = string(&exception, "signal")
        .or_else(|| string(&exception, "type"))
        .unwrap_or_else(|| "an unknown signal".to_string());
    let threads = body
        .get("threads")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let faulting = body
        .get("faultingThread")
        .and_then(serde_json::Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| threads.get(i))
        .or_else(|| {
            threads.iter().find(|t| {
                t.get("triggered")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
        });
    let images = body
        .get("usedImages")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let frames = faulting
        .and_then(|t| t.get("frames"))
        .and_then(serde_json::Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .map(|frame| {
                    if let Some(symbol) = string(frame, "symbol") {
                        return symbol;
                    }
                    let image = frame
                        .get("imageIndex")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|i| usize::try_from(i).ok())
                        .and_then(|i| images.get(i))
                        .and_then(|i| string(i, "name"))
                        .unwrap_or_else(|| "<unknown image>".to_string());
                    let offset = frame
                        .get("imageOffset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    format!("{image}+{offset:#x}")
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CrashReport {
        program,
        path: string(&body, "procPath").unwrap_or_default(),
        pid: body.get("pid").and_then(serde_json::Value::as_u64),
        signal,
        subtype: string(&exception, "subtype"),
        frames,
    })
}

/// The programs a Postgres build installs or runs its tests with, other than the `pg_` ones.
const PROGRAMS: [&str; 16] = [
    "postgres",
    "postmaster",
    "initdb",
    "psql",
    "pgbench",
    "ecpg",
    "isolationtester",
    "createdb",
    "dropdb",
    "createuser",
    "dropuser",
    "clusterdb",
    "reindexdb",
    "vacuumdb",
    "vacuumlo",
    "oid2name",
];

/// Whether a crash report is from Postgres: an executable under the build directory, or a
/// program Postgres installs, by name.
#[must_use]
pub fn is_postgres(report: &CrashReport, out: &Path) -> bool {
    (!report.path.is_empty() && Path::new(&report.path).starts_with(out))
        || report.program.starts_with("pg_")
        || PROGRAMS.contains(&report.program.as_str())
}

/// Where macOS keeps the crash reports of the user's processes.
#[must_use]
pub fn diagnostic_reports() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Logs/DiagnosticReports"))
}

/// Crash reports to read: the directory, and the moment the run started, before which a report
/// is from something else.
#[derive(Debug, Clone)]
pub struct Reports {
    /// Usually `~/Library/Logs/DiagnosticReports`.
    pub dir: PathBuf,
    /// Reports older than this are left out.
    pub since: SystemTime,
}

/// Every file under a directory, sorted, so the report comes out the same each time.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Logs `rpg test` keeps that are not a TAP script's.
fn not_tap(name: &str) -> bool {
    name == "postmaster.log"
        || name == "initdb.log"
        || name == "check.log"
        || name.ends_with(".postmaster.log")
}

/// Group the failures a build directory's `results/` holds, the cores in `cores`, the crash
/// reports in `reports` and the minidumps under `results/`, writing each core's backtrace, each
/// report's frames and what `cdb` says of each minidump into `<out>/triage/`.
pub fn triage(out: &Path, cores: &Path, reports: Option<&Reports>) -> Triage {
    let mut triage = Triage::default();
    let relative = |p: &Path| p.strip_prefix(out).unwrap_or(p).display().to_string();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let results = out.join("results");
    let all = files(&results);
    for path in &all {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = path.parent().unwrap_or(&results);
        let run = relative(dir);
        let run = run.trim_start_matches("results/");
        if let Some(stem) = name.strip_suffix(".diffs") {
            let server = if stem == "regression" {
                dir.join("postmaster.log")
            } else {
                dir.join(format!("{stem}.postmaster.log"))
            };
            let crash = crash_line(&read(&server));
            for (test, signature) in diff_chunks(&read(path)) {
                let test = if stem == "regression" {
                    format!("{run}: {test}")
                } else {
                    format!("{run}: {stem}/{test}")
                };
                let signature = crash.clone().unwrap_or(signature);
                triage.add(signature, test, relative(path));
            }
        } else if let Some(stem) = name.strip_suffix(".log") {
            let paired = dir.join(format!("{stem}.diffs"));
            if not_tap(&name) || name.starts_with("test-") || all.contains(&paired) {
                continue;
            }
            triage.add(
                tap_signature(&read(path)),
                format!("{run}: {stem}"),
                relative(path),
            );
        }
    }

    let traces = out.join("triage");
    for core in files(cores) {
        let name = core
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.starts_with("core") {
            continue;
        }
        let Some(program) = core_program(&name) else {
            triage.unread.push((
                name,
                "the name does not carry the executable's path, which kernel.core_pattern gives with %E".into(),
            ));
            continue;
        };
        let core_path = core.display().to_string();
        let program_path = program.display().to_string();
        let backtrace = match crate::process::capture(
            Path::new("gdb"),
            &[
                "-batch",
                "-nx",
                "-ex",
                "bt",
                "-ex",
                "thread apply all bt full",
                &program_path,
                &core_path,
            ],
        ) {
            Ok(text) => text,
            Err(e) => {
                triage.unread.push((name, e));
                continue;
            }
        };
        std::fs::create_dir_all(&traces).ok();
        let trace = traces.join(format!("{name}.txt"));
        std::fs::write(&trace, &backtrace).ok();
        let short = program.file_name().map_or_else(
            || program_path.clone(),
            |n| n.to_string_lossy().into_owned(),
        );
        if is_quit(&backtrace) {
            triage.quit.push(name);
            continue;
        }
        triage.cores += 1;
        triage.add(core_signature(&short, &backtrace), name, relative(&trace));
    }
    if let Some(reports) = reports {
        crash_reports(&mut triage, out, reports);
    }
    minidumps(&mut triage, out, &all, cdb().as_deref());
    triage
}

/// Add the Postgres crash reports written since the run started.
fn crash_reports(triage: &mut Triage, out: &Path, reports: &Reports) {
    let traces = out.join("triage");
    for found in files(&reports.dir) {
        let path = found.as_path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.extension().is_none_or(|e| e != "ips") {
            continue;
        }
        let recent = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= reports.since);
        if !recent {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                triage.unread.push((name, e.to_string()));
                continue;
            }
        };
        // Other programs crash on a machine too, and their reports are none of this run's
        // business, so a report is only counted as unread when it may be Postgres's.
        let report = match parse_ips(&text) {
            Ok(report) => report,
            Err(e) => {
                if PROGRAMS.iter().any(|p| name.starts_with(p)) || name.starts_with("pg_") {
                    triage.unread.push((name, e));
                }
                continue;
            }
        };
        if !is_postgres(&report, out) {
            continue;
        }
        std::fs::create_dir_all(&traces).ok();
        let trace = traces.join(format!("{name}.txt"));
        std::fs::write(&trace, report.text()).ok();
        if report.signal == "SIGQUIT" {
            triage.quit.push(name);
            continue;
        }
        triage.cores += 1;
        let file = relative_to(out, &trace);
        triage.add(report.signature(), name, file);
    }
}

fn relative_to(out: &Path, path: &Path) -> String {
    path.strip_prefix(out).unwrap_or(path).display().to_string()
}

/// The most names a group lists before it says how many more there are.
const LISTED: usize = 40;

/// The report, as markdown.
#[must_use]
pub fn report(triage: &Triage) -> String {
    let mut text = String::from("# Triage\n\n");
    let groups = triage.sorted();
    let _ = writeln!(
        text,
        "{} failures in {} groups, {} of them core files, crash reports or minidumps.",
        triage.failures(),
        groups.len(),
        triage.cores,
    );
    for (signature, failures) in &groups {
        let _ = write!(text, "\n## {}: `{signature}`\n\n", failures.len());
        for f in failures.iter().take(LISTED) {
            let _ = writeln!(text, "- {} ({})", f.test, f.file);
        }
        if failures.len() > LISTED {
            let _ = writeln!(text, "- and {} more", failures.len() - LISTED);
        }
    }
    if !triage.quit.is_empty() {
        text.push_str(
            "\n## Cores from SIGQUIT, not counted\n\nPostgres sends SIGQUIT to every child on an \
             immediate shutdown and to every other backend after one crashes.\n\n",
        );
        for name in &triage.quit {
            let _ = writeln!(text, "- {name}");
        }
    }
    if !triage.unread.is_empty() {
        text.push_str("\n## Cores, crash reports and minidumps that could not be read\n\n");
        for (name, why) in &triage.unread {
            let _ = writeln!(text, "- {name}: {why}");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_addresses_and_the_log_prefix_do_not_split_a_group() {
        assert_eq!(
            normalize(
                "2026-09-29 14:05:28.123 UTC [4242] LOG:  server process (PID 4251) was terminated by signal 11: Segmentation fault"
            ),
            "LOG: server process (PID N) was terminated by signal N: Segmentation fault"
        );
        assert_eq!(normalize("  at 0x7ffc1234   here "), "at <addr> here");
    }

    #[test]
    fn an_assertion_comes_before_the_signal_it_raised() {
        let log = "\
2026-09-29 14:05:28.100 UTC [4251] LOG:  statement: select 1
TRAP: failed Assert(\"lsn != 0\"), File: \"xlog.c\", Line: 812, PID: 4251
2026-09-29 14:05:28.200 UTC [4242] LOG:  server process (PID 4251) was terminated by signal 6: Aborted
";
        assert_eq!(
            crash_line(log).as_deref(),
            Some("crash: TRAP: failed Assert(\"lsn != N\"), File: \"xlog.c\", Line: N, PID: N")
        );
        assert_eq!(crash_line("LOG:  database system is ready"), None);
    }

    #[test]
    fn each_test_in_a_diff_gets_its_first_changed_line() {
        let text = "\
diff -U3 /b/src/test/regress/expected/boolean.out /b/src/test/regress/results/boolean.out
--- /b/src/test/regress/expected/boolean.out	2026-09-29 14:00:00
+++ /b/src/test/regress/results/boolean.out	2026-09-29 14:01:00
@@ -10,7 +10,7 @@
  select 1;
-  t
+  f
diff -U3 /b/src/test/regress/expected/int4.out /b/src/test/regress/results/int4.out
--- /b/src/test/regress/expected/int4.out
+++ /b/src/test/regress/results/int4.out
@@ -1,3 +1,2 @@
 select 2147483647 + 1;
-ERROR:  integer out of range
";
        assert_eq!(
            diff_chunks(text),
            vec![
                ("boolean".to_string(), "+ f".to_string()),
                (
                    "int4".to_string(),
                    "- ERROR: integer out of range".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_tap_log_is_its_crash_or_its_first_failed_test() {
        let failed = "\
[14:05:28.100](0.1s) ok 1 - initdb
[14:05:28.200](0.1s) not ok 2 - replica caught up
[14:05:28.200](0.0s) #   Failed test 'replica caught up'
[14:05:28.200](0.0s) #   at t/001_stream_rep.pl line 97.
";
        assert_eq!(tap_signature(failed), "Failed test 'replica caught up'");
        let crashed =
            format!("{failed}LOG:  server process (PID 12) was terminated by signal 11\n");
        assert_eq!(
            tap_signature(&crashed),
            "crash: LOG: server process (PID N) was terminated by signal N"
        );
    }

    #[test]
    fn a_quit_core_is_told_apart() {
        assert!(is_quit(
            "Core was generated by `cp a b'.\nProgram terminated with signal SIGQUIT, Quit.\n"
        ));
        assert!(!is_quit(
            "Program terminated with signal SIGSEGV, Segmentation fault.\n"
        ));
        assert!(!is_quit("no signal line at all\n"));
    }

    #[test]
    fn a_core_is_the_signal_and_the_top_of_the_crashing_stack() {
        let backtrace = "\
Core was generated by `postgres: runner regression [local] SELECT'.
Program terminated with signal SIGSEGV, Segmentation fault.
#0  0x000055d1c2a3b4c5 in ExecInterpExpr (state=0x1, econtext=0x2, isnull=0x3) at execExprInterp.c:512
#1  0x000055d1c2a3b000 in ExecEvalExprSwitchContext (state=0x1) at executor.h:356
#2  ExecProject (projInfo=0x4) at executor.h:390
#3  0x000055d1c2a3a000 in ExecScan (node=0x5) at execScan.c:180

Thread 1 (Thread 0x7f00 (LWP 4251)):
#0  0x000055d1c2a3b4c5 in ExecInterpExpr (state=0x1) at execExprInterp.c:512
";
        assert_eq!(
            core_signature("postgres", backtrace),
            "core of postgres, SIGSEGV in ExecInterpExpr < ExecEvalExprSwitchContext < ExecProject"
        );
    }

    #[test]
    fn the_program_comes_from_a_core_pattern_with_the_path_in_it() {
        assert_eq!(
            core_program("core.!tmp!pg!bin!postgres.4242"),
            Some(PathBuf::from("/tmp/pg/bin/postgres"))
        );
        assert_eq!(core_program("core.postgres.4242"), None);
        assert_eq!(core_program("core"), None);
    }

    const IPS: &str = include_str!("testdata/postgres-segv.ips");

    #[test]
    fn a_crash_report_gives_the_faulting_threads_frames() {
        let report = parse_ips(IPS).unwrap();
        assert_eq!(report.program, "postgres");
        assert_eq!(report.pid, Some(4251));
        assert_eq!(report.signal, "SIGSEGV");
        assert_eq!(
            report.subtype.as_deref(),
            Some("KERN_INVALID_ADDRESS at 0x0000000000000000")
        );
        assert_eq!(
            report.frames,
            [
                "ExecInterpExpr",
                "ExecProject",
                "postgres+0x19f104",
                "ExecScan",
                "start"
            ]
        );
        assert_eq!(
            report.signature(),
            "crash report of postgres, SIGSEGV in ExecInterpExpr < ExecProject < postgres+0x19f104"
        );
        assert!(report.text().contains("#0   ExecInterpExpr\n"));
    }

    #[test]
    fn a_report_that_is_not_a_crash_or_not_json_is_refused() {
        let hang = IPS.replacen("\"bug_type\":\"309\"", "\"bug_type\":\"288\"", 1);
        assert!(parse_ips(&hang).unwrap_err().contains("288"));
        assert!(parse_ips("not json\nat all").is_err());
        // Without the header line, the body alone still reads.
        let body = IPS.split_once('\n').unwrap().1;
        assert_eq!(parse_ips(body).unwrap().program, "postgres");
    }

    #[test]
    fn only_postgres_programs_count() {
        let out = Path::new("/Users/runner/work/_temp/pg");
        let mut report = parse_ips(IPS).unwrap();
        assert!(is_postgres(&report, out));
        report.program = "a.out".into();
        assert!(is_postgres(&report, out), "under the build directory");
        report.path = "/Applications/Safari.app/Contents/MacOS/Safari".into();
        report.program = "Safari".into();
        assert!(!is_postgres(&report, out));
        report.program = "pg_dump".into();
        assert!(is_postgres(&report, out));
    }

    #[test]
    fn crash_reports_since_the_run_started_are_grouped() {
        let out = std::env::temp_dir().join(format!("rpg-triage-ips-{}", std::process::id()));
        let dir = out.join("DiagnosticReports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("postgres-2026-09-30-101112.ips"), IPS).unwrap();
        std::fs::write(
            dir.join("Safari-2026-09-30-101113.ips"),
            IPS.replace("\"procName\" : \"postgres\"", "\"procName\" : \"Safari\"")
                .replace("_temp\\/pg\\/build", "elsewhere"),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not a report").unwrap();
        let reports = Reports {
            dir: dir.clone(),
            since: SystemTime::UNIX_EPOCH,
        };
        let found = triage(&out, &out.join("cores"), Some(&reports));
        let later = Reports {
            dir,
            since: SystemTime::now() + std::time::Duration::from_secs(3600),
        };
        let none = triage(&out, &out.join("cores"), Some(&later));
        std::fs::remove_dir_all(&out).ok();
        assert_eq!(found.cores, 1);
        assert_eq!(found.failures(), 1);
        let sorted = found.sorted();
        assert_eq!(
            sorted[0].0,
            "crash report of postgres, SIGSEGV in ExecInterpExpr < ExecProject < postgres+0x19f104"
        );
        assert_eq!(
            sorted[0].1[0].file,
            "triage/postgres-2026-09-30-101112.ips.txt"
        );
        assert_eq!(none.cores, 0);
    }

    #[test]
    fn an_exception_is_how_a_windows_server_log_says_it_crashed() {
        assert_eq!(
            crash_line("LOG:  server process (PID 5120) was terminated by exception 0xC0000005\n"),
            Some("crash: LOG: server process (PID N) was terminated by exception <addr>".into())
        );
    }

    #[test]
    fn a_minidump_is_the_exception_and_the_top_of_its_stack() {
        // What cdb printed over a backend that crashed in a module's _PG_init on the W64 runner.
        let text = "Loading Dump File [D:\\a\\_temp\\pg\\crashdumps\\postgres-pid1512-1349359.mdmp]\n\
                    0:000> cdb: Reading initial command '.lastevent; .ecxr; kc 30; q'\n\
                    Last event: 5e8.acc: Access violation - code c0000005 (first/second chance not available)\n\
                    \x20 debugger time: Sun Oct  4 07:14:33.588 2026 (UTC + 0:00)\n\
                    rax=0000000000000000 rbx=00007ff7ac39ca90 rcx=0000000a3e7ff450\n\
                    crashme!PG_init+0xe7:\n\
                    00007ffc`5f461524 c70000000000    mov     dword ptr [rax],0 ds:00000000`00000000=????????\n\
                    Call Site\n\
                    crashme!PG_init\n\
                    postgres!lookup_external_function\n\
                    postgres!load_file\n\
                    postgres!ValidatePgVersion\n\
                    postgres\n\
                    kernel32!BaseThreadInitThunk\n\
                    ntdll!RtlUserThreadStart\n\
                    quit:\n\
                    NatVis script unloaded from 'C:\\Debuggers\\x64\\Visualizers\\winrt.natvis'\n";
        assert_eq!(
            minidump_signature("postgres", text),
            "minidump of postgres, exception 0xc0000005 in crashme!PG_init < postgres!lookup_external_function < postgres!load_file"
        );
        let numbered = "Last event: 1400.1a2c: Access violation - code c0000005 (first/second chance not available)\n\
                        \x20# Call Site\n\
                        00 postgres!ExecInterpExpr+0x1a3\n\
                        01 postgres!ExecScan+0x88\n\
                        quit:\n";
        assert_eq!(
            minidump_signature("postgres", numbered),
            "minidump of postgres, exception 0xc0000005 in postgres!ExecInterpExpr < postgres!ExecScan"
        );
        assert_eq!(
            minidump_signature("postgres", "nothing useful"),
            "minidump of postgres, an unknown exception in no frames"
        );
        assert_eq!(
            minidump_origin("postgres-pid5120-99.mdmp"),
            ("postgres", "5120")
        );
        assert_eq!(
            minidump_origin("postgres.exe.1924.dmp"),
            ("postgres", "1924")
        );
    }

    #[test]
    fn a_minidump_without_cdb_is_still_counted() {
        let out = std::env::temp_dir().join(format!("rpg-triage-mdmp-{}", std::process::id()));
        let dumps = out.join("results/world/run-1/crashdumps");
        std::fs::create_dir_all(&dumps).unwrap();
        std::fs::write(dumps.join("postgres-pid5120-99.mdmp"), b"MDMP").unwrap();
        let mut found = Triage::default();
        minidumps(&mut found, &out, &files(&out.join("results")), None);
        std::fs::remove_dir_all(&out).ok();
        assert_eq!(found.cores, 1);
        let sorted = found.sorted();
        assert_eq!(
            sorted[0].0,
            "minidump of postgres, not read because cdb is not installed"
        );
        assert_eq!(
            sorted[0].1[0].file,
            "results/world/run-1/crashdumps/postgres-pid5120-99.mdmp"
        );
    }

    #[test]
    fn a_crash_windows_reported_is_counted_once() {
        let out = std::env::temp_dir().join(format!("rpg-triage-wer-{}", std::process::id()));
        let dumps = out.join("results/world/run-1/crashdumps");
        let reported = out.join("results/windows-error-reporting");
        std::fs::create_dir_all(&dumps).unwrap();
        std::fs::create_dir_all(&reported).unwrap();
        std::fs::write(dumps.join("postgres-pid1924-99.mdmp"), b"MDMP").unwrap();
        std::fs::write(dumps.join("postgres-pid2048-99.mdmp"), b"MDMP").unwrap();
        std::fs::write(reported.join("postgres.exe.1924.dmp"), b"MDMP").unwrap();
        let mut found = Triage::default();
        minidumps(&mut found, &out, &files(&out.join("results")), None);
        std::fs::remove_dir_all(&out).ok();
        assert_eq!(found.cores, 2);
        let mut kept: Vec<String> = found.sorted()[0].1.iter().map(|f| f.file.clone()).collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                "results/windows-error-reporting/postgres.exe.1924.dmp",
                "results/world/run-1/crashdumps/postgres-pid2048-99.mdmp",
            ]
        );
    }

    #[test]
    fn a_crash_puts_every_test_of_its_run_in_one_group() {
        let out = std::env::temp_dir().join(format!("rpg-triage-{}", std::process::id()));
        let run = out.join("results/regress/run-1");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(
            run.join("regression.diffs"),
            "diff -U3 a/expected/x.out a/results/x.out\n+server closed the connection unexpectedly\n\
             diff -U3 a/expected/y.out a/results/y.out\n+connection to server was lost\n",
        )
        .unwrap();
        std::fs::write(
            run.join("postmaster.log"),
            "LOG:  server process (PID 9) was terminated by signal 11: Segmentation fault\n",
        )
        .unwrap();
        let tap = out.join("results/world/run-1");
        std::fs::create_dir_all(&tap).unwrap();
        std::fs::write(
            tap.join("src-bin-initdb-001_initdb.log"),
            "not ok 3 - locale\n#   Failed test 'locale'\n",
        )
        .unwrap();
        std::fs::write(tap.join("check.log"), "make check-world\n").unwrap();
        let triage = triage(&out, &out.join("cores"), None);
        std::fs::remove_dir_all(&out).ok();
        assert_eq!(triage.failures(), 3);
        let sorted = triage.sorted();
        assert_eq!(
            sorted[0].0,
            "crash: LOG: server process (PID N) was terminated by signal N: Segmentation fault"
        );
        assert_eq!(
            sorted[0]
                .1
                .iter()
                .map(|f| f.test.as_str())
                .collect::<Vec<_>>(),
            ["regress/run-1: x", "regress/run-1: y"]
        );
        assert_eq!(sorted[1].0, "Failed test 'locale'");
        assert!(report(&triage).contains("## 2: `crash: LOG:"));
    }
}
