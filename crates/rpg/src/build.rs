//! `rpg build`: configure and build the pinned tree through the shim.
//!
//! The build directory holds everything about one build:
//!
//! - `bin/cc` and `bin/gcc`, copies of `rpg-cc`, with `bin/rpg-cc.toml` naming the real compiler;
//! - `build/`, the Postgres build tree, always out of the source tree so the cached source stays
//!   clean and can be shared by every build;
//! - `configure.log` and `build.log`;
//! - `compile.jsonl`, one line per compiler call, and `compile_commands.json` derived from it;
//! - `build.json`, what was built with what, how long each step took and where it stopped.

use crate::compiler::{Compiler, Kind};
use crate::pins::Pin;
use crate::process::{Finished, Step, first_error};
use crate::settings::{BuildConfig, Level, System};
use rpg_shim::args::{self, Mode};
use rpg_shim::config::ShimConfig;
use rpg_shim::record::{CompileRecord, read_log};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Everything a build needs decided before it starts.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The pin being built.
    pub pin: Pin,
    /// The unpacked source.
    pub source: PathBuf,
    /// The configuration.
    pub config: BuildConfig,
    /// The build system.
    pub system: System,
    /// The level.
    pub level: Level,
    /// The compiler under test or the reference.
    pub compiler: Compiler,
    /// The build directory.
    pub out: PathBuf,
    /// Parallel jobs.
    pub jobs: usize,
    /// Compile every translation unit twice and compare.
    pub twice: bool,
    /// Stop after configure, for comparing what two compilers answered to the probes.
    pub configure_only: bool,
}

/// The default build directory for a combination, under `work/`.
#[must_use]
pub fn default_out(
    work: &Path,
    pin: &str,
    config: &str,
    system: System,
    level: Level,
    cc: &str,
) -> PathBuf {
    let cc = Path::new(cc)
        .file_name()
        .map_or_else(|| cc.to_string(), |n| n.to_string_lossy().into_owned());
    work.join(format!(
        "{pin}-{config}-{system}-{}-{cc}",
        level.name().trim_start_matches('-')
    ))
}

/// How far a build got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Configure or meson setup did not finish.
    ConfigureFailed,
    /// Configured, and the build did not finish.
    BuildFailed,
    /// Configured, and no build was asked for.
    Configured,
    /// Built.
    Built,
}

/// `build.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BuildInfo {
    /// The pin.
    pub pin: String,
    /// The pin's commit.
    pub commit: String,
    /// The configuration name.
    pub config: String,
    /// The build system.
    pub system: String,
    /// The level.
    pub level: String,
    /// The compiler's path.
    pub cc: String,
    /// The first line of its `--version`.
    pub compiler: String,
    /// Its family.
    pub kind: String,
    /// The rucc checkout commit, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rucc_commit: Option<String>,
    /// Whether the shim asked rucc for a trace.
    pub rucc_trace: bool,
    /// Whether every compile ran twice.
    pub twice: bool,
    /// The machine.
    pub host: String,
    /// The date, UTC.
    pub date: String,
    /// Jobs.
    pub jobs: usize,
    /// The source tree.
    pub source: String,
    /// The Postgres build tree.
    pub build_dir: String,
    /// How far it got.
    pub phase: Phase,
    /// Seconds configuring.
    pub configure_seconds: f64,
    /// Seconds building, when it got that far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_seconds: Option<f64>,
    /// What the compile trace says.
    pub compiles: CompileSummary,
    /// The first error line of the step that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_error: Option<String>,
}

impl BuildInfo {
    /// Read `build.json` from a build directory.
    pub fn load(out: &Path) -> Result<Self, String> {
        let path = out.join("build.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}; run rpg build first", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Numbers from `compile.jsonl`, split at the moment configuring ended.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CompileSummary {
    /// Compiler calls while configuring, which are the probes.
    pub probe_calls: usize,
    /// Probe calls that exited non zero. Many of these are expected: a probe asks a question.
    pub probe_failures: usize,
    /// Compiler calls while building.
    pub build_calls: usize,
    /// Of those, calls that compiled a C file.
    pub build_compiles: usize,
    /// Build calls that exited non zero.
    pub build_failures: usize,
    /// Lines of `compile.jsonl` that did not parse.
    pub unreadable_lines: usize,
    /// User CPU seconds summed over build calls.
    pub build_user_seconds: f64,
    /// The largest peak resident set of any call, in KiB.
    pub peak_rss_kb: u64,
    /// The call that had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_file: Option<String>,
    /// The slowest compiles, file and wall seconds, slowest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slowest: Vec<(String, f64)>,
    /// The first build call that failed: its file and the start of its standard error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_failure: Option<(String, String)>,
    /// Calls where `RPG_TWICE` found different output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nondeterministic: Option<usize>,
}

/// The name a record is known by in a summary: its C input, or its output, or its first word.
///
/// Meson names sources relative to the build directory, so a file of the cached tree comes out as
/// `../../../home/pg/.cache/rpg/src/REL_18_6/src/backend/...`. The path is resolved against the
/// call's directory and made relative to the first of `roots` it is under, which gives
/// `src/backend/...` for a source and for a generated file alike.
fn subject(record: &CompileRecord, roots: &[&Path]) -> String {
    let path = record
        .inputs
        .iter()
        .find(|d| args::is_c_source(&d.path))
        .or_else(|| record.outputs.first())
        .map(|d| d.path.clone());
    let Some(path) = path else {
        return record.argv.get(1).cloned().unwrap_or_default();
    };
    let full = lexical(&Path::new(&record.cwd).join(&path));
    roots
        .iter()
        .find_map(|root| full.strip_prefix(root).ok())
        .map_or(path, |p| p.display().to_string())
}

/// Remove `.` and `..` from a path without asking the file system.
pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Summarize a trace, counting calls that started at or after `built_from` as build calls.
/// File names are given relative to the first of `roots` they fall under.
#[must_use]
pub fn summarize(
    records: &[CompileRecord],
    unreadable: usize,
    built_from: f64,
    roots: &[&Path],
) -> CompileSummary {
    let mut summary = CompileSummary {
        unreadable_lines: unreadable,
        ..CompileSummary::default()
    };
    let mut compiles: Vec<(String, f64)> = Vec::new();
    let mut nondeterministic = 0;
    let mut checked = false;
    for record in records {
        let peak = record.peak_rss_kb.unwrap_or(0);
        if peak > summary.peak_rss_kb {
            summary.peak_rss_kb = peak;
            summary.peak_rss_file = Some(subject(record, roots));
        }
        if record.started < built_from {
            summary.probe_calls += 1;
            summary.probe_failures += usize::from(!record.succeeded());
            continue;
        }
        summary.build_calls += 1;
        summary.build_user_seconds += record.user_seconds.unwrap_or(0.0);
        if !record.succeeded() {
            summary.build_failures += 1;
            if summary.first_failure.is_none() {
                summary.first_failure = Some((subject(record, roots), record.stderr.clone()));
            }
        }
        if record.inputs.iter().any(|d| args::is_c_source(&d.path)) {
            summary.build_compiles += 1;
            compiles.push((subject(record, roots), record.wall_seconds));
        }
        if let Some(twice) = &record.twice {
            checked = true;
            nondeterministic += usize::from(!twice.identical);
        }
    }
    compiles.sort_by(|a, b| b.1.total_cmp(&a.1));
    compiles.truncate(10);
    summary.slowest = compiles;
    summary.nondeterministic = checked.then_some(nondeterministic);
    summary
}

/// One entry of `compile_commands.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileCommand {
    /// The working directory.
    pub directory: String,
    /// The source file.
    pub file: String,
    /// The command, with the real compiler in place of the shim.
    pub arguments: Vec<String>,
    /// The object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Derive `compile_commands.json` from the trace.
///
/// Only successful compiles of C files that still exist, which drops the configure and meson
/// probes since their files are deleted as soon as the probe finishes. A file compiled twice
/// keeps its last command.
#[must_use]
pub fn compile_commands(
    records: &[CompileRecord],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<CompileCommand> {
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut out: Vec<CompileCommand> = Vec::new();
    for record in records.iter().filter(|r| r.succeeded()) {
        let rest = record.argv.get(1..).unwrap_or_default();
        let cwd = Path::new(&record.cwd);
        let invocation = args::read(rest, cwd, &|_| true);
        if invocation.mode != Mode::Compile {
            continue;
        }
        for input in record.inputs.iter().filter(|d| args::is_c_source(&d.path)) {
            if !exists(&cwd.join(&input.path)) {
                continue;
            }
            let mut arguments = vec![record.compiler.clone()];
            arguments.extend(rest.iter().cloned());
            let entry = CompileCommand {
                directory: record.cwd.clone(),
                file: input.path.clone(),
                arguments,
                output: invocation.outputs.first().cloned(),
            };
            let key = (record.cwd.clone(), input.path.clone());
            if let Some(&at) = index.get(&key) {
                out[at] = entry;
            } else {
                index.insert(key, out.len());
                out.push(entry);
            }
        }
    }
    out
}

/// Where `rpg-cc` is: `RPG_SHIM`, or next to the running `rpg`.
fn shim_binary() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("RPG_SHIM") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find rpg itself: {e}"))?;
    let shim = exe.with_file_name("rpg-cc");
    if shim.is_file() {
        Ok(shim)
    } else {
        Err(format!(
            "{} is missing; build the workspace with cargo build --release, or set RPG_SHIM",
            shim.display()
        ))
    }
}

/// Put the shim in `out/bin` as `cc` and `gcc`, with its settings next to it.
fn install_shim(plan: &Plan, trace: bool) -> Result<PathBuf, String> {
    let bin = plan.out.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
    let shim = shim_binary()?;
    for name in ["cc", "gcc"] {
        let target = bin.join(name);
        std::fs::remove_file(&target).ok();
        std::fs::copy(&shim, &target)
            .map_err(|e| format!("copying {} to {}: {e}", shim.display(), target.display()))?;
    }
    let config = ShimConfig {
        real: plan.compiler.path.clone(),
        log: plan.out.join("compile.jsonl"),
        rucc_trace: trace,
        twice: plan.twice,
    };
    let path = bin.join(rpg_shim::config::FILE_NAME);
    std::fs::write(&path, config.to_toml())
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(bin)
}

/// The environment every step of a build and its tests runs with.
///
/// The shim directory goes first on `PATH`, so a tool that calls `cc` or `gcc` by name reaches it,
/// and `CC` names the shim for the build systems that read it. Flags a user may have set in their
/// shell are removed, so that nothing reaches the compiler that the configuration does not say.
#[must_use]
pub fn environment(out: &Path) -> (BTreeMap<String, String>, Vec<String>) {
    let bin = out.join("bin");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), format!("{}:{path}", bin.display()));
    env.insert("CC".to_string(), bin.join("cc").display().to_string());
    let unset = [
        "CFLAGS",
        "CPPFLAGS",
        "LDFLAGS",
        "LIBS",
        "CPATH",
        "C_INCLUDE_PATH",
        "LIBRARY_PATH",
        "MAKEFLAGS",
    ]
    .map(String::from)
    .to_vec();
    (env, unset)
}

/// Run the build.
#[allow(clippy::too_many_lines)]
pub fn build(plan: &Plan) -> Result<BuildInfo, String> {
    let build_dir = plan.out.join("build");
    if build_dir.exists() {
        eprintln!(
            "rpg: removing the previous build in {}",
            build_dir.display()
        );
        std::fs::remove_dir_all(&build_dir)
            .map_err(|e| format!("removing {}: {e}", build_dir.display()))?;
    }
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| format!("creating {}: {e}", build_dir.display()))?;
    for stale in ["compile.jsonl", "compile_commands.json", "build.json"] {
        std::fs::remove_file(plan.out.join(stale)).ok();
    }

    let trace = plan.compiler.supports_trace(&plan.out);
    if plan.compiler.kind == Kind::Rucc {
        eprintln!(
            "rpg: {} {} -frucc-trace",
            plan.compiler.version,
            if trace {
                "understands"
            } else {
                "does not understand"
            }
        );
    }
    install_shim(plan, trace)?;
    let (env, unset) = environment(&plan.out);

    let configure_log = plan.out.join("configure.log");
    let build_log = plan.out.join("build.log");
    let mut configure = match plan.system {
        System::Meson => Step::new("meson setup", "meson", &plan.out, &configure_log)
            .args(["setup".to_string(), build_dir.display().to_string()])
            .args([plan.source.display().to_string()])
            .args(plan.config.options(plan.system).iter().cloned())
            .args([format!("-Doptimization={}", plan.level.digit())])
            .args(plan.level.lto().then_some("-Db_lto=true")),
        System::Autoconf => Step::new(
            "configure",
            plan.source.join("configure"),
            &build_dir,
            &configure_log,
        )
        .args(plan.config.options(plan.system).iter().cloned())
        .args([
            format!("CC={}", env["CC"]),
            format!("CFLAGS={}", plan.level.flag()),
        ])
        .args(plan.level.lto().then_some("LDFLAGS=-flto")),
    }
    .envs(&env);
    configure.unset.clone_from(&unset);
    let configured = configure.run()?;

    let mut built: Option<Finished> = None;
    let build_started = now();
    if configured.ok && !plan.configure_only {
        let mut step = match plan.system {
            System::Meson => Step::new("ninja", "ninja", &plan.out, &build_log).args([
                "-C".to_string(),
                build_dir.display().to_string(),
                "-j".to_string(),
                plan.jobs.to_string(),
            ]),
            System::Autoconf => Step::new(
                "make world-bin",
                crate::process::make(),
                &build_dir,
                &build_log,
            )
            .args([format!("-j{}", plan.jobs), "world-bin".to_string()]),
        }
        .envs(&env);
        step.unset.clone_from(&unset);
        built = Some(step.run()?);
    }

    let log_path = plan.out.join("compile.jsonl");
    let (records, unreadable) = read_log(&log_path).unwrap_or_default();
    let compiles = summarize(
        &records,
        unreadable,
        build_started,
        &[&build_dir, &plan.source],
    );
    let commands = compile_commands(&records, &|p| p.is_file());
    let commands_path = plan.out.join("compile_commands.json");
    std::fs::write(
        &commands_path,
        serde_json::to_string_pretty(&commands).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("writing {}: {e}", commands_path.display()))?;

    let phase = match (&configured, &built) {
        (c, _) if !c.ok => Phase::ConfigureFailed,
        (_, None) if plan.configure_only => Phase::Configured,
        (_, Some(b)) if b.ok => Phase::Built,
        _ => Phase::BuildFailed,
    };
    let first_error = match phase {
        Phase::ConfigureFailed => first_error(&configure_log)
            .or_else(|| meson_log_error(&build_dir))
            .or_else(|| crate::process::tail(&configure_log, 1).pop()),
        Phase::BuildFailed => compiles
            .first_failure
            .as_ref()
            .map(|(file, stderr)| format!("{file}: {}", stderr.lines().next().unwrap_or_default()))
            .or_else(|| first_error(&build_log)),
        Phase::Built | Phase::Configured => None,
    };

    let info = BuildInfo {
        pin: plan.pin.name.clone(),
        commit: plan.pin.commit.clone(),
        config: plan.config.name.clone(),
        system: plan.system.name().to_string(),
        level: plan.level.name().to_string(),
        cc: plan.compiler.path.display().to_string(),
        compiler: plan.compiler.version.clone(),
        kind: plan.compiler.kind.name().to_string(),
        rucc_commit: plan.compiler.commit.clone(),
        rucc_trace: trace,
        twice: plan.twice,
        host: crate::process::hostname(),
        date: crate::process::today(),
        jobs: plan.jobs,
        source: plan.source.display().to_string(),
        build_dir: build_dir.display().to_string(),
        phase,
        configure_seconds: round(configured.seconds),
        build_seconds: built.map(|b| round(b.seconds)),
        compiles,
        first_error,
    };
    let path = plan.out.join("build.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&info).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(info)
}

/// Meson writes the reason it stopped at the end of its own log as well as to the console.
fn meson_log_error(build_dir: &Path) -> Option<String> {
    let log = build_dir.join("meson-logs").join("meson-log.txt");
    crate::process::tail(&log, 40)
        .into_iter()
        .rev()
        .find(|line| line.contains("ERROR"))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Round to a tenth of a millisecond, which is all a wall clock measurement is worth.
#[must_use]
pub fn round(seconds: f64) -> f64 {
    (seconds * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpg_shim::record::parse_log;

    const TRACE: &str = r#"{"started":10.0,"argv":["cc","-c","conftest.c"],"compiler":"/usr/bin/gcc-16","cwd":"/b/build","inputs":[{"path":"conftest.c","sha256":"a"}],"wall-seconds":0.01,"exit":1,"stderr":"conftest.c:1: error: no"}
{"started":20.0,"argv":["cc","-Isrc/include","-c","../src/backend/parser/gram.c","-o","gram.o"],"compiler":"/usr/bin/gcc-16","cwd":"/b/build","inputs":[{"path":"../src/backend/parser/gram.c","sha256":"b"}],"outputs":[{"path":"gram.o","sha256":"c"}],"wall-seconds":9.5,"user-seconds":9.0,"peak-rss-kb":500000,"exit":0,"twice":{"identical":true}}
{"started":21.0,"argv":["cc","-c","../src/x.c","-o","x.o"],"compiler":"/usr/bin/gcc-16","cwd":"/b/build","inputs":[{"path":"../src/x.c","sha256":"d"}],"outputs":[{"path":"x.o","sha256":"e"}],"wall-seconds":0.5,"user-seconds":0.4,"peak-rss-kb":1000,"exit":1,"stderr":"../src/x.c:3: error: expected\nmore"}
{"started":22.0,"argv":["cc","-o","postgres","gram.o"],"compiler":"/usr/bin/gcc-16","cwd":"/b/build","inputs":[{"path":"gram.o","sha256":"c"}],"outputs":[{"path":"postgres","sha256":"f"}],"wall-seconds":1.0,"exit":0}
"#;

    #[test]
    fn a_trace_is_split_into_probes_and_build_calls() {
        let (records, unreadable) = parse_log(TRACE);
        let summary = summarize(&records, unreadable, 15.0, &[]);
        assert_eq!(summary.probe_calls, 1);
        assert_eq!(summary.probe_failures, 1);
        assert_eq!(summary.build_calls, 3);
        assert_eq!(summary.build_compiles, 2);
        assert_eq!(summary.build_failures, 1);
        assert_eq!(summary.peak_rss_kb, 500_000);
        assert_eq!(
            summary.peak_rss_file.as_deref(),
            Some("../src/backend/parser/gram.c")
        );
        assert_eq!(summary.slowest[0].0, "../src/backend/parser/gram.c");
        assert_eq!(summary.first_failure.as_ref().unwrap().0, "../src/x.c");
        assert_eq!(summary.nondeterministic, Some(0));
    }

    #[test]
    fn compile_commands_keep_real_compiles_of_files_that_still_exist() {
        let (records, _) = parse_log(TRACE);
        let commands = compile_commands(&records, &|p| !p.ends_with("conftest.c"));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].file, "../src/backend/parser/gram.c");
        assert_eq!(commands[0].arguments[0], "/usr/bin/gcc-16");
        assert_eq!(commands[0].output.as_deref(), Some("gram.o"));
    }

    #[test]
    fn default_build_directories_name_every_axis() {
        let out = default_out(
            Path::new("/w"),
            "REL_18_6",
            "minimal",
            System::Meson,
            Level::O2,
            "/usr/bin/gcc-16",
        );
        assert_eq!(out, Path::new("/w/REL_18_6-minimal-meson-O2-gcc-16"));
    }
}
