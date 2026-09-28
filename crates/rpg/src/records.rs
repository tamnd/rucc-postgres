//! `records.jsonl`: one line per test, per run, in the shape of the design's document 10.3.
//!
//! The shape extends rucc-real-corpus's run record rather than inventing a new one: kebab case
//! field names, `project` and `level` spelled the same way, an outcome from a closed list, and
//! optional fields left out rather than written as null. The difference is the unit. There a
//! record is one project at one level. Here it is one test, because Postgres is one project with
//! several hundred tests and a summary per project would hide exactly what this harness is for.

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::Path;

/// What happened to one test, from document 10.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Matched its expected output.
    Passed,
    /// Did not. `class` says how.
    Failed,
    /// A backend died on a signal while the suite ran.
    Crashed,
    /// Ran out of time.
    Timeout,
    /// Could not run because something it needs did not build.
    BuildFailed,
    /// Skipped by the suite for a configuration reason.
    Skipped,
}

impl Outcome {
    /// The spelling in records and reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
            Self::Timeout => "timeout",
            Self::BuildFailed => "build-failed",
            Self::Skipped => "skipped",
        }
    }
}

/// How a failed test failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailClass {
    /// `pg_regress` output differed from every expected file.
    Diff,
    /// A TAP assertion failed.
    Tap,
    /// The test errored before it could be compared.
    Error,
}

/// One test, one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TestRecord {
    /// Always `postgres`, so these records can sit next to rucc-real-corpus's.
    pub project: String,
    /// The pin.
    pub pin: String,
    /// The row, when the run was made for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// The machine.
    pub host: String,
    /// The optimization level.
    pub level: String,
    /// The build system.
    pub system: String,
    /// The configuration.
    pub config: String,
    /// The suite, for example `regress`.
    pub suite: String,
    /// The test, for example `join_hash`.
    pub test: String,
    /// What happened.
    pub outcome: Outcome,
    /// How it failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<FailClass>,
    /// Seconds, from the suite's own clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// The first line of the building compiler's `--version`.
    pub compiler: String,
    /// rucc's version, when rucc built it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rucc: Option<String>,
    /// The rucc commit, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rucc_commit: Option<String>,
    /// The reference compiler the result is graded against, or that built it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// What the baseline says about this test: `passed`, `failed` or `flaky`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
    /// Which repetition this was, for baseline runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<u32>,
    /// Where the logs and diffs for this run were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
}

/// Append records to a JSON Lines file, creating it and its directory as needed.
pub fn append(path: &Path, records: &[TestRecord]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Read records back, skipping lines that do not parse.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse(text: &str) -> Vec<TestRecord> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_in_the_design_reads_as_a_record() {
        let line = r#"{"project":"postgres","pin":"REL_18_6","row":"L64","host":"server3","level":"-O2","system":"autoconf","config":"minimal","suite":"regress","test":"join_hash","outcome":"failed","class":"diff","seconds":4.1,"compiler":"rucc 0.12.3","rucc":"0.12.3","rucc-commit":"abc","reference":"gcc-16.2.0","baseline":"passed","artifacts":"runs/2026-10-14/L64-O2/regress/join_hash/"}"#;
        let records = parse(line);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.outcome, Outcome::Failed);
        assert_eq!(record.class, Some(FailClass::Diff));
        assert_eq!(record.baseline.as_deref(), Some("passed"));
        let again = serde_json::to_string(record).unwrap();
        assert_eq!(again, line);
    }

    #[test]
    fn outcomes_are_spelled_as_the_design_spells_them() {
        assert_eq!(
            serde_json::to_string(&Outcome::BuildFailed).unwrap(),
            "\"build-failed\""
        );
        assert_eq!(Outcome::Timeout.name(), "timeout");
    }
}
