//! `rpg test`: run a suite against a finished build and write one record per test.
//!
//! Only the main regression suite for now, which is what PG0 asks for. Under meson it is
//! `meson test --suite setup` followed by `meson test --suite regress`, as upstream's CI does it.
//! Under autoconf it is `make check`. Either way the per test results come from `pg_regress`'s own
//! `regression.out`, and the logs and diffs of every run are copied out of the build tree into
//! `results/<suite>/run-<n>/`, because the next run overwrites them.

use crate::build::{BuildInfo, Phase, environment, round};
use crate::process::Step;
use crate::records::{FailClass, Outcome, TestRecord};
use crate::regress::{RegressOutput, parse_regress, parse_testlog};
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
pub const SUITES: &[&str] = &["regress"];

/// Where `pg_regress` writes for the main suite, relative to the Postgres build tree.
fn regress_dir(system: System) -> &'static str {
    match system {
        System::Meson => "testrun/regress/regress",
        System::Autoconf => "src/test/regress",
    }
}

/// initdb refuses to run as root, so a suite run as root fails in a way that looks like a
/// build problem. Say so before starting.
fn refuse_root() -> Result<(), String> {
    let uid = crate::process::capture(Path::new("id"), &["-u"]).unwrap_or_default();
    if uid.trim() == "0" {
        return Err(
            "the suites cannot run as root, because initdb refuses to; run rpg test as an unprivileged user such as pg (provision/l64.sh creates one)"
                .to_string(),
        );
    }
    Ok(())
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
    let build_dir = PathBuf::from(&plan.info.build_dir);
    let artifacts = plan
        .out
        .join("results")
        .join(&plan.suite)
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
    let pg_dir = build_dir.join(regress_dir(system));
    for stale in ["regression.out", "regression.diffs"] {
        std::fs::remove_file(pg_dir.join(stale)).ok();
    }
    let log = |name: &str| plan.out.join(format!("test-{}-{name}.log", plan.suite));
    let mut seconds = 0.0;
    let mut meson_result = None;
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
            let mut step = Step::new(
                &format!("meson test --suite {}", plan.suite),
                "meson",
                &build_dir,
                &log("run"),
            )
            .args(["test", "--no-rebuild", "--suite", plan.suite.as_str()])
            .envs(&env);
            step.unset.clone_from(&unset);
            seconds += step.run()?.seconds;
            let testlog = std::fs::read_to_string(build_dir.join("meson-logs/testlog.json"))
                .unwrap_or_default();
            meson_result = parse_testlog(&testlog)
                .into_iter()
                .find(|t| t.short_name() == format!("{0}/{0}", plan.suite));
        }
        System::Autoconf => {
            let mut step = Step::new("make check", "make", &build_dir, &log("run"))
                .args(["check"])
                .envs(&env);
            step.unset.clone_from(&unset);
            seconds += step.run()?.seconds;
        }
    }

    std::fs::create_dir_all(&artifacts)
        .map_err(|e| format!("creating {}: {e}", artifacts.display()))?;
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
    let postmaster = std::fs::read_to_string(pg_dir.join("log/postmaster.log")).unwrap_or_default();
    let crashed = postmaster.contains("terminated by signal");

    let mut records: Vec<TestRecord> = output
        .results
        .iter()
        .map(|r| {
            let (outcome, class) = match (r.ok, crashed) {
                (true, _) => (Outcome::Passed, None),
                (false, true) => (Outcome::Crashed, None),
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
            .is_some_and(|t| !t.ok() && output.failed() == 0);
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

fn record(
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
        suite: plan.suite.clone(),
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
