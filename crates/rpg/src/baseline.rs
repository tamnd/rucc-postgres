//! `rpg baseline`: what the reference compiler gets on a row, which is what rucc is graded against.
//!
//! A baseline builds the pin with the row's reference compiler and runs each suite several times.
//! A test that passed every run is in `passing`, one that failed every run is in `failing`, and one
//! that did both is in `flaky`. Only the sets and the times are kept in git, in
//! `baselines/<row>/<pin>/<config>/baseline.toml`, next to the `records.jsonl` they came from.
//! The build tree itself stays under `work/`.

use crate::build::{BuildInfo, Phase};
use crate::records::{Outcome, TestRecord};
use crate::suite::Sets;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// `baseline.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Baseline {
    /// The row.
    pub row: String,
    /// The pin.
    pub pin: String,
    /// The Postgres commit the pin names.
    pub commit: String,
    /// The configuration.
    pub config: String,
    /// The build system.
    pub system: String,
    /// The level.
    pub level: String,
    /// The first line of the reference compiler's `--version`.
    pub reference: String,
    /// The machine it ran on.
    pub host: String,
    /// The day it ran.
    pub date: String,
    /// Parallel build jobs.
    pub jobs: usize,
    /// Runs of each suite.
    pub runs: u32,
    /// Configure wall clock seconds.
    pub configure_seconds: f64,
    /// Build wall clock seconds.
    pub build_seconds: f64,
    /// One entry per suite.
    #[serde(default)]
    pub suites: BTreeMap<String, Sets>,
}

impl Baseline {
    /// Read `baseline.toml` from a baseline directory.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("baseline.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Write it, with a comment saying where it came from.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        let text = format!(
            "# Written by rpg baseline. Regenerate rather than edit: the sets are what the reference compiler got.\n\n{body}"
        );
        let path = dir.join("baseline.toml");
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
    }

    /// Start one from the build it will describe.
    #[must_use]
    pub fn from_build(row: &str, info: &BuildInfo, runs: u32) -> Self {
        Self {
            row: row.to_string(),
            pin: info.pin.clone(),
            commit: info.commit.clone(),
            config: info.config.clone(),
            system: info.system.clone(),
            level: info.level.clone(),
            reference: info.compiler.clone(),
            host: info.host.clone(),
            date: info.date.clone(),
            jobs: info.jobs,
            runs,
            configure_seconds: info.configure_seconds,
            build_seconds: if info.phase == Phase::Built {
                info.build_seconds.unwrap_or_default()
            } else {
                0.0
            },
            suites: BTreeMap::new(),
        }
    }
}

/// Sort the tests of several runs of one suite into the three sets.
///
/// Each inner slice is one run. The whole suite line, test `*`, is left out: it says a run did not
/// finish, which the per test sets already show as missing tests.
#[must_use]
pub fn classify(runs: &[Vec<TestRecord>]) -> Sets {
    let mut passed: BTreeMap<&str, u32> = BTreeMap::new();
    let mut failed: BTreeMap<&str, u32> = BTreeMap::new();
    for run in runs {
        for record in run.iter().filter(|r| r.test != "*") {
            let slot = if record.outcome == Outcome::Passed {
                &mut passed
            } else {
                &mut failed
            };
            *slot.entry(record.test.as_str()).or_default() += 1;
        }
    }
    let names: BTreeSet<&str> = passed.keys().chain(failed.keys()).copied().collect();
    let mut sets = Sets::default();
    for name in names {
        let p = passed.get(name).copied().unwrap_or(0);
        let f = failed.get(name).copied().unwrap_or(0);
        let set = match (p, f) {
            (_, 0) => &mut sets.passing,
            (0, _) => &mut sets.failing,
            _ => &mut sets.flaky,
        };
        set.insert(name.to_string());
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(test: &str, outcome: Outcome) -> TestRecord {
        TestRecord {
            project: "postgres".into(),
            pin: "REL_18_6".into(),
            row: None,
            host: "h".into(),
            level: "-O2".into(),
            system: "meson".into(),
            config: "minimal".into(),
            suite: "regress".into(),
            test: test.into(),
            outcome,
            class: None,
            seconds: None,
            compiler: "gcc".into(),
            rucc: None,
            rucc_commit: None,
            reference: None,
            baseline: None,
            run: None,
            artifacts: None,
        }
    }

    #[test]
    fn runs_are_sorted_into_passing_failing_and_flaky() {
        let one = vec![
            rec("boolean", Outcome::Passed),
            rec("char", Outcome::Failed),
            rec("stats", Outcome::Passed),
        ];
        let two = vec![
            rec("boolean", Outcome::Passed),
            rec("char", Outcome::Failed),
            rec("stats", Outcome::Failed),
            rec("*", Outcome::Failed),
        ];
        let sets = classify(&[one, two]);
        assert_eq!(sets.passing, ["boolean".to_string()].into());
        assert_eq!(sets.failing, ["char".to_string()].into());
        assert_eq!(sets.flaky, ["stats".to_string()].into());
    }

    #[test]
    fn a_baseline_survives_a_round_trip_through_toml() {
        let mut baseline = Baseline {
            row: "L64".into(),
            runs: 3,
            ..Baseline::default()
        };
        baseline.suites.insert(
            "regress".into(),
            classify(&[vec![rec("boolean", Outcome::Passed)]]),
        );
        let text = toml::to_string(&baseline).unwrap();
        let back: Baseline = toml::from_str(&text).unwrap();
        assert_eq!(back, baseline);
    }
}
