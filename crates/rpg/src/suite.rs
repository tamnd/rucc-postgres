//! `rpg test`: run a suite against a finished build and write one record per test.
//!
//! The suites are the ones PG2 grades: `regress`, `isolation`, `ecpg`, and the `pg_regress` runs of
//! `contrib` and `src/test/modules`, and PG3's `world`, which is `make check-world` with its TAP
//! scripts. Under meson it is `meson test --suite setup` followed by
//! `meson test --suite <suite>`, as upstream's CI does it, for the three that are one run each.
//! Under autoconf it is `make check` for the main suite and `make -C <dir> check` for the others,
//! with `-k` for the two that are a run per module. `world` is `make -k -j<jobs> -Otarget
//! check-world PROVE_FLAGS=--timer`, as upstream's CI runs it, where `-Otarget` keeps each run's
//! lines together in the log. The per test results come from `pg_regress`'s
//! own output, and the logs and diffs of every run are copied out of the build tree into
//! `results/<suite>/run-<n>/`, because the next run overwrites them.
//!
//! On Windows a crash leaves a minidump rather than a core. Postgres writes one itself when its
//! data directory has a `crashdumps` directory in it, so under meson that directory goes into the
//! initdb template every test cluster is copied from, and the dumps written during the run are
//! moved into `results/<suite>/run-<n>/crashdumps/` afterwards for `rpg triage` to read.

use crate::build::{BuildInfo, Phase, environment, round};
use crate::process::Step;
use crate::records::{FailClass, Outcome, TestRecord};
use crate::regress::{
    MesonTest, RegressOutput, parse_regress, parse_sections, parse_testlog, world_from_testlog,
};
use crate::settings::System;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What a run needs.
#[derive(Debug, Clone)]
pub struct SuitePlan {
    /// The build directory `rpg build` wrote.
    pub out: PathBuf,
    /// Its `build.json`.
    pub info: BuildInfo,
    /// The suite.
    pub suite: String,
    /// The row, when the run is for one.
    pub row: Option<String>,
    /// `PG_TEST_TIMEOUT_DEFAULT`.
    pub timeout: u32,
    /// The baseline to grade against, when there is one.
    pub baseline: Option<Sets>,
    /// The repetition number, for baselines.
    pub run: u32,
    /// The name the records and the results directory carry, when it is not the suite's, as for
    /// `rpg cross-modules`, which runs `contrib` as `cross-contrib`.
    pub label: Option<String>,
}

impl SuitePlan {
    /// The name the records and the results directory carry.
    fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.suite)
    }
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct SuiteRun {
    /// One per test, or one for the whole suite when it produced no per test results.
    pub records: Vec<TestRecord>,
    /// Wall clock seconds for the suite, setup included.
    pub seconds: f64,
    /// What `pg_regress` said.
    pub output: RegressOutput,
}

/// The passing, failing and flaky sets of one suite in a baseline, with the time of each run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sets {
    /// Passed every run.
    #[serde(default)]
    pub passing: BTreeSet<String>,
    /// Failed every run.
    #[serde(default)]
    pub failing: BTreeSet<String>,
    /// Passed some runs and failed others.
    #[serde(default)]
    pub flaky: BTreeSet<String>,
    /// Wall clock seconds of each run, setup included.
    #[serde(default)]
    pub seconds: Vec<f64>,
}

impl Sets {
    /// What the baseline says about a test.
    #[must_use]
    pub fn verdict(&self, test: &str) -> Option<&'static str> {
        if self.passing.contains(test) {
            Some("passed")
        } else if self.failing.contains(test) {
            Some("failed")
        } else if self.flaky.contains(test) {
            Some("flaky")
        } else {
            None
        }
    }
}

/// The suites `rpg test` knows how to run.
pub const SUITES: &[&str] = &[
    "regress",
    "isolation",
    "ecpg",
    "contrib",
    "modules",
    "world",
];

/// Where a suite lives in the Postgres build tree under autoconf, which is where its `make check`
/// runs.
fn suite_dir(suite: &str) -> &'static str {
    match suite {
        "isolation" => "src/test/isolation",
        "ecpg" => "src/interfaces/ecpg",
        "contrib" => "contrib",
        "modules" => "src/test/modules",
        "world" => "",
        _ => "src/test/regress",
    }
}

/// Whether a suite is one `pg_regress` or prove run for each module in it rather than one run.
fn per_module(suite: &str) -> bool {
    matches!(suite, "contrib" | "modules" | "world")
}

/// Where `pg_regress` writes for a suite that is one run, relative to the Postgres build tree.
fn regress_dir(system: System, suite: &str) -> String {
    match (system, suite) {
        (System::Meson, suite) => format!("testrun/{suite}/{suite}"),
        (System::Autoconf, "isolation") => "src/test/isolation/output_iso".to_string(),
        (System::Autoconf, "ecpg") => "src/interfaces/ecpg/test".to_string(),
        (System::Autoconf, _) => "src/test/regress".to_string(),
    }
}

/// Where one run of a per module suite wrote, relative to the Postgres build tree. An isolation
/// run keeps its output apart from the regress run in the same module.
fn section_dir(subdir: &str, kind: &str) -> String {
    if kind == "isolation" {
        format!("{subdir}/output_iso")
    } else {
        subdir.to_string()
    }
}

/// initdb refuses to run as root, so a suite run as root fails in a way that looks like a
/// build problem. Say so before starting.
pub(crate) fn refuse_root() -> Result<(), String> {
    let uid = crate::process::capture(Path::new("id"), &["-u"]).unwrap_or_default();
    if uid.trim() == "0" {
        return Err(
            "the suites cannot run as root, because initdb refuses to; run rpg test as an unprivileged user such as pg (provision/l64.sh creates one)"
                .to_string(),
        );
    }
    Ok(())
}

/// Whether a server log says a backend died of a signal, or of an exception on Windows.
fn crashed_in(log: &Path) -> bool {
    std::fs::read_to_string(log).is_ok_and(|text| {
        text.contains("terminated by signal") || text.contains("terminated by exception")
    })
}

/// The initdb template meson's setup suite writes, which pg_regress and the TAP scripts copy each
/// cluster from instead of running initdb.
fn initdb_template(build_dir: &Path) -> PathBuf {
    build_dir.join("tmp_install").join("initdb-template")
}

/// Every minidump under a directory: the files in a `crashdumps` directory that end in `.mdmp`.
fn minidumps(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "mdmp")
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "crashdumps")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Move the minidumps the run left in the build tree into the run's artifacts, so that the next
/// run does not find them again. A dump is named for its PID and the tick count, which do not
/// repeat within a run. A copy stands in for the move when the two are on different drives.
fn keep_minidumps(build_dir: &Path, artifacts: &Path) {
    let dumps = minidumps(build_dir);
    if dumps.is_empty() {
        return;
    }
    let kept = artifacts.join("crashdumps");
    if std::fs::create_dir_all(&kept).is_err() {
        return;
    }
    for dump in dumps {
        let Some(name) = dump.file_name() else {
            continue;
        };
        let to = kept.join(name);
        if std::fs::rename(&dump, &to).is_err() && std::fs::copy(&dump, &to).is_ok() {
            std::fs::remove_file(&dump).ok();
        }
    }
}

/// Run the suite.
#[allow(clippy::too_many_lines)]
pub fn run(plan: &SuitePlan) -> Result<SuiteRun, String> {
    if !SUITES.contains(&plan.suite.as_str()) {
        return Err(format!(
            "rpg test knows the suites {} so far, not {}",
            SUITES.join(", "),
            plan.suite
        ));
    }
    let system = System::parse(&plan.info.system)?;
    if system == System::Meson && per_module(&plan.suite) && plan.suite != "world" {
        return Err(format!(
            "rpg test runs {} under autoconf only so far, since meson gives each module a suite of its own",
            plan.suite
        ));
    }
    let build_dir = PathBuf::from(&plan.info.build_dir);
    let artifacts = plan
        .out
        .join("results")
        .join(plan.name())
        .join(format!("run-{}", plan.run));

    if plan.info.phase != Phase::Built {
        let record = record(plan, "*", Outcome::BuildFailed, None, None, &artifacts);
        return Ok(SuiteRun {
            records: vec![record],
            seconds: 0.0,
            output: RegressOutput::default(),
        });
    }
    refuse_root()?;

    let (mut env, unset) = environment(&plan.out);
    env.insert(
        "PG_TEST_TIMEOUT_DEFAULT".to_string(),
        plan.timeout.to_string(),
    );
    let pg_dir = build_dir.join(regress_dir(system, &plan.suite));
    for stale in ["regression.out", "regression.diffs"] {
        std::fs::remove_file(pg_dir.join(stale)).ok();
    }
    let log = |name: &str| plan.out.join(format!("test-{}-{name}.log", plan.name()));
    let mut seconds = 0.0;
    let mut meson_result = None;
    let mut meson_tests = Vec::new();
    let mut make_failed = false;
    match system {
        System::Meson => {
            let mut setup = Step::new(
                "meson test --suite setup",
                "meson",
                &build_dir,
                &log("setup"),
            )
            .args(["test", "--suite", "setup"])
            .envs(&env);
            setup.unset.clone_from(&unset);
            let done = setup.run()?;
            seconds += done.seconds;
            if !done.ok {
                return Err("meson test --suite setup failed, so the suite cannot run".into());
            }
            // Copied into every cluster made from the template, which is what has Postgres write a
            // minidump when one of its processes crashes there.
            if cfg!(windows) {
                std::fs::create_dir_all(initdb_template(&build_dir).join("crashdumps")).ok();
            }
            // world is every suite but setup, run as upstream's CI runs it, a process per core.
            let jobs = plan.info.jobs.max(1).to_string();
            let args = if plan.suite == "world" {
                vec![
                    "test",
                    "--no-rebuild",
                    "--no-suite",
                    "setup",
                    "--num-processes",
                    &jobs,
                ]
            } else {
                vec!["test", "--no-rebuild", "--suite", plan.suite.as_str()]
            };
            let label = format!("meson {}", args.join(" "));
            let mut step = Step::new(&label, "meson", &build_dir, &log("run"))
                .args(args)
                .envs(&env);
            step.unset.clone_from(&unset);
            seconds += step.run()?.seconds;
            let testlog = std::fs::read_to_string(build_dir.join("meson-logs/testlog.json"))
                .unwrap_or_default();
            meson_tests = parse_testlog(&testlog);
            meson_result = meson_tests
                .iter()
                .find(|t| t.short_name() == format!("{0}/{0}", plan.suite))
                .cloned();
        }
        System::Autoconf => {
            // The main suite is the top level `make check`, as it always was here. The others run
            // in their own directory, and `-k` lets one module that fails leave the rest to run.
            let dir = suite_dir(&plan.suite);
            let jobs = format!("-j{}", plan.info.jobs.max(1));
            let mut args = Vec::new();
            if per_module(&plan.suite) {
                args.push("-k");
            }
            if plan.suite == "world" {
                args.extend([
                    jobs.as_str(),
                    "-Otarget",
                    "check-world",
                    "PROVE_FLAGS=--timer",
                ]);
            } else {
                if plan.suite != "regress" {
                    args.extend(["-C", dir]);
                }
                args.push("check");
            }
            let label = format!("make {}", args.join(" "));
            let mut step = Step::new(&label, crate::process::make(), &build_dir, &log("run"))
                .args(args)
                .envs(&env);
            step.unset.clone_from(&unset);
            let done = step.run()?;
            seconds += done.seconds;
            make_failed = !done.ok;
        }
    }

    std::fs::create_dir_all(&artifacts)
        .map_err(|e| format!("creating {}: {e}", artifacts.display()))?;
    if cfg!(windows) {
        keep_minidumps(&build_dir, &artifacts);
    }
    let (output, crashed) = if system == System::Meson && plan.suite == "world" {
        (
            meson_world(&meson_tests, &build_dir, &artifacts),
            BTreeSet::new(),
        )
    } else if per_module(&plan.suite) {
        modules(plan, &build_dir, &log("run"), &artifacts)
    } else {
        for (from, to) in [
            ("regression.diffs", "regression.diffs"),
            ("log/postmaster.log", "postmaster.log"),
            ("log/initdb.log", "initdb.log"),
        ] {
            std::fs::copy(pg_dir.join(from), artifacts.join(to)).ok();
        }
        // pg_regress deletes regression.out when nothing failed. The same lines are in meson's
        // testlog.json, and under make in the log of make check itself, which parse_regress reads
        // past the make noise of.
        let text = std::fs::read_to_string(pg_dir.join("regression.out"))
            .ok()
            .or_else(|| meson_result.as_ref().and_then(|t| t.stdout.clone()))
            .or_else(|| std::fs::read_to_string(log("run")).ok())
            .unwrap_or_default();
        std::fs::write(artifacts.join("regression.out"), &text).ok();
        let output = parse_regress(&text);
        let crashed: BTreeSet<String> = if crashed_in(&pg_dir.join("log/postmaster.log")) {
            output.results.iter().map(|r| r.name.clone()).collect()
        } else {
            BTreeSet::new()
        };
        (output, crashed)
    };

    let mut records: Vec<TestRecord> = output
        .results
        .iter()
        .map(|r| {
            let (outcome, class) = match (r.ok, crashed.contains(&r.name)) {
                (true, _) => (Outcome::Passed, None),
                (false, true) => (Outcome::Crashed, None),
                (false, false) if r.name.contains("/t/") => (Outcome::Failed, Some(FailClass::Tap)),
                (false, false) => (Outcome::Failed, Some(FailClass::Diff)),
            };
            record(plan, &r.name, outcome, class, Some(r.seconds), &artifacts)
        })
        .collect();
    let timed_out = meson_result.as_ref().is_some_and(|t| t.result == "TIMEOUT");
    let incomplete = output.bailed.is_some()
        || output
            .planned
            .is_none_or(|n| usize::try_from(n).unwrap_or(0) > output.results.len())
        || timed_out
        || meson_result
            .as_ref()
            .is_some_and(|t| !t.ok() && output.failed() == 0)
        || (make_failed && output.failed() == 0);
    if incomplete {
        let outcome = if timed_out {
            Outcome::Timeout
        } else {
            Outcome::Failed
        };
        records.push(record(
            plan,
            "*",
            outcome,
            Some(FailClass::Error),
            Some(round(seconds)),
            &artifacts,
        ));
    }
    Ok(SuiteRun {
        records,
        seconds: round(seconds),
        output,
    })
}

/// Read the runs of a per module suite out of the log of its `make check`, as one output whose
/// tests are named for their module, `amcheck/check_btree`, with the run's kind in between for an
/// isolation run, `amcheck/isolation/read-write-unique`. A TAP script keeps its own name after the
/// module, `src/bin/initdb/t/001_initdb.pl`. Under `world` the module is the whole directory. Also
/// copies each failing run's diffs and server log, or a failing script's log, and says which tests
/// failed in a run whose server crashed.
fn modules(
    plan: &SuitePlan,
    build_dir: &Path,
    log: &Path,
    artifacts: &Path,
) -> (RegressOutput, BTreeSet<String>) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    std::fs::write(artifacts.join("check.log"), &text).ok();
    let top = match suite_dir(&plan.suite) {
        "" => String::new(),
        dir => format!("{dir}/"),
    };
    let mut output = RegressOutput {
        planned: Some(0),
        ..RegressOutput::default()
    };
    let mut crashed = BTreeSet::new();
    let root = build_dir.display().to_string();
    for section in parse_sections(&text, &root) {
        let module = section.subdir.strip_prefix(&top).unwrap_or(&section.subdir);
        if section.kind == "tap" {
            tap(
                &mut output,
                module,
                &build_dir.join(&section.subdir),
                section.output,
                artifacts,
            );
            continue;
        }
        let prefix = if section.kind == "regress" {
            module.to_string()
        } else {
            format!("{module}/{}", section.kind)
        };
        let dir = build_dir.join(section_dir(&section.subdir, &section.kind));
        let died = crashed_in(&dir.join("log/postmaster.log"));
        if section.output.failed() > 0 || section.output.bailed.is_some() {
            let name = prefix.replace('/', "-");
            std::fs::copy(
                dir.join("regression.diffs"),
                artifacts.join(format!("{name}.diffs")),
            )
            .ok();
            std::fs::copy(
                dir.join("log/postmaster.log"),
                artifacts.join(format!("{name}.postmaster.log")),
            )
            .ok();
        }
        output.planned = match (output.planned, section.output.planned) {
            (Some(sum), Some(n)) => Some(sum + n),
            _ => None,
        };
        if output.bailed.is_none() {
            output.bailed = section
                .output
                .bailed
                .map(|reason| format!("{prefix}: {reason}"));
        }
        for mut result in section.output.results {
            result.name = format!("{prefix}/{}", result.name);
            if died {
                crashed.insert(result.name.clone());
            }
            output.results.push(result);
        }
    }
    (output, crashed)
}

/// Read a meson `world` run out of `testlog.json` with [`world_from_testlog`], and copy the diffs
/// and the TAP log of each meson test that failed out of `testrun/`, where meson keeps a directory
/// per test.
fn meson_world(tests: &[MesonTest], build_dir: &Path, artifacts: &Path) -> RegressOutput {
    for test in tests.iter().filter(|t| !t.ok()) {
        let short = test.short_name();
        let dir = build_dir.join("testrun").join(short);
        let script = short.rsplit('/').next().unwrap_or(short);
        let name = short.replace('/', "-");
        std::fs::copy(
            dir.join("regression.diffs"),
            artifacts.join(format!("{name}.diffs")),
        )
        .ok();
        std::fs::copy(
            dir.join(format!("log/regress_log_{script}")),
            artifacts.join(format!("{name}.log")),
        )
        .ok();
    }
    world_from_testlog(tests)
}

/// Add one directory's TAP scripts to a per module output, and copy the log of each script that
/// failed, which the TAP framework writes to `tmp_check/log/regress_log_<script>`.
fn tap(output: &mut RegressOutput, module: &str, dir: &Path, run: RegressOutput, artifacts: &Path) {
    output.planned = match (output.planned, run.planned) {
        (Some(sum), Some(n)) => Some(sum + n),
        _ => None,
    };
    if output.bailed.is_none() {
        output.bailed = run.bailed.map(|reason| format!("{module}: {reason}"));
    }
    for mut result in run.results {
        if !result.ok {
            let stem = result.name.trim_start_matches("t/").trim_end_matches(".pl");
            let name = format!("{module}/{stem}").replace('/', "-");
            std::fs::copy(
                dir.join(format!("tmp_check/log/regress_log_{stem}")),
                artifacts.join(format!("{name}.log")),
            )
            .ok();
        }
        result.name = format!("{module}/{}", result.name);
        output.results.push(result);
    }
}

pub(crate) fn record(
    plan: &SuitePlan,
    test: &str,
    outcome: Outcome,
    class: Option<FailClass>,
    seconds: Option<f64>,
    artifacts: &Path,
) -> TestRecord {
    let info = &plan.info;
    let is_rucc = info.kind == "rucc";
    TestRecord {
        project: "postgres".to_string(),
        pin: info.pin.clone(),
        row: plan.row.clone(),
        host: crate::process::hostname(),
        level: info.level.clone(),
        system: info.system.clone(),
        config: info.config.clone(),
        suite: plan.name().to_string(),
        test: test.to_string(),
        outcome,
        class,
        seconds,
        compiler: info.compiler.clone(),
        rucc: is_rucc.then(|| {
            info.compiler
                .trim_start_matches("rucc ")
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        }),
        rucc_commit: info.rucc_commit.clone(),
        reference: (!is_rucc).then(|| info.compiler.clone()),
        baseline: plan
            .baseline
            .as_ref()
            .and_then(|b| b.verdict(test))
            .map(String::from),
        run: Some(plan.run),
        artifacts: Some(artifacts.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_suite_runs_and_writes_where_its_makefile_does() {
        assert_eq!(suite_dir("regress"), "src/test/regress");
        assert_eq!(suite_dir("modules"), "src/test/modules");
        assert!(per_module("contrib") && per_module("world") && !per_module("isolation"));
        assert_eq!(suite_dir("world"), "");
        assert_eq!(
            regress_dir(System::Autoconf, "isolation"),
            "src/test/isolation/output_iso"
        );
        assert_eq!(regress_dir(System::Meson, "ecpg"), "testrun/ecpg/ecpg");
        assert_eq!(
            section_dir("contrib/amcheck", "isolation"),
            "contrib/amcheck/output_iso"
        );
        assert_eq!(section_dir("contrib/amcheck", "regress"), "contrib/amcheck");
    }

    #[test]
    fn minidumps_move_out_of_the_build_tree_into_the_run() {
        let root = std::env::temp_dir().join(format!("rpg-minidumps-{}", std::process::id()));
        let build = root.join("build");
        let data = build.join("testrun/recovery/001_stream_rep/data/t_primary_data/pgdata");
        std::fs::create_dir_all(data.join("crashdumps")).unwrap();
        std::fs::write(data.join("crashdumps/postgres-pid4242-99.mdmp"), b"MDMP").unwrap();
        // A dump outside a crashdumps directory is something else's, and stays where it is.
        std::fs::write(data.join("other.mdmp"), b"MDMP").unwrap();
        std::fs::create_dir_all(initdb_template(&build).join("crashdumps")).unwrap();

        let found = minidumps(&build);
        assert_eq!(found, [data.join("crashdumps/postgres-pid4242-99.mdmp")]);
        let artifacts = root.join("results/world/run-1");
        keep_minidumps(&build, &artifacts);
        assert!(
            artifacts
                .join("crashdumps/postgres-pid4242-99.mdmp")
                .is_file()
        );
        assert!(minidumps(&build).is_empty());
        assert!(data.join("other.mdmp").is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_crash_on_windows_is_an_exception_in_the_server_log() {
        let root = std::env::temp_dir().join(format!("rpg-crashed-in-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("postmaster.log");
        std::fs::write(
            &log,
            "LOG:  server process (PID 5120) was terminated by exception 0xC0000005\n",
        )
        .unwrap();
        assert!(crashed_in(&log));
        std::fs::write(&log, "LOG:  database system is shut down\n").unwrap();
        assert!(!crashed_in(&log));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_baseline_verdict_comes_from_whichever_set_holds_the_test() {
        let sets = Sets {
            passing: ["boolean".to_string()].into(),
            failing: ["char".to_string()].into(),
            flaky: ["stats".to_string()].into(),
            seconds: Vec::new(),
        };
        assert_eq!(sets.verdict("boolean"), Some("passed"));
        assert_eq!(sets.verdict("char"), Some("failed"));
        assert_eq!(sets.verdict("stats"), Some("flaky"));
        assert_eq!(sets.verdict("nope"), None);
    }
}
