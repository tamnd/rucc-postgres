//! `rpg nightly`: keep what each night's rows got in git and say what changed since the night before.
//!
//! The world job runs one row per system and level and keeps `build.json` and `records.jsonl`
//! from each. This reads a directory holding one such pair per row and writes a night file for
//! each row to `runs/<date>/<name>.toml`, where the name is the row, pin, configuration, system
//! and level, for example `L64-REL_18_6-minimal-autoconf-O2`. The row comes from the records,
//! which have it when `rpg test` was given `--row`, and is left out of the name when they do not. A night file holds the counts and the
//! tests that did not pass, not every record, so a year of nights stays small.
//!
//! Each row is compared with the last night file of the same name from an earlier date. A test
//! that does not pass tonight and was not on the earlier night's list is a regression, and so is
//! a build that stopped when the earlier one got through. `reports/nightly.md` is rewritten with
//! the latest night of every row and the counts of the last thirty, and when anything regressed
//! the text of an issue is written for the workflow to open.

use crate::build::{BuildInfo, Phase};
use crate::records::{Outcome, TestRecord, parse};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// How many nights of history the report shows per row.
const HISTORY: usize = 30;

/// `runs/<date>/<name>.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Night {
    /// The day, UTC.
    pub date: String,
    /// The row, when the records name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// The pin.
    pub pin: String,
    /// The Postgres commit the pin named.
    pub commit: String,
    /// The configuration.
    pub config: String,
    /// The build system.
    pub system: String,
    /// The level.
    pub level: String,
    /// The first line of the compiler's `--version`.
    pub compiler: String,
    /// The machine.
    pub host: String,
    /// Whether the build got through.
    pub built: bool,
    /// The first error of the build, when it stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_error: Option<String>,
    /// Records per outcome.
    #[serde(default)]
    pub counts: BTreeMap<String, usize>,
    /// Every test that did not pass or skip, as `suite/test`.
    #[serde(default)]
    pub failing: BTreeSet<String>,
}

impl Night {
    /// Build a night from a row's `build.json` and records.
    #[must_use]
    pub fn from_row(info: &BuildInfo, records: &[TestRecord]) -> Self {
        let mut counts = BTreeMap::new();
        let mut failing = BTreeSet::new();
        for record in records {
            *counts.entry(record.outcome.name().to_string()).or_insert(0) += 1;
            if !matches!(record.outcome, Outcome::Passed | Outcome::Skipped) {
                failing.insert(format!("{}/{}", record.suite, record.test));
            }
        }
        Self {
            date: info.date.clone(),
            row: records.iter().find_map(|r| r.row.clone()),
            pin: info.pin.clone(),
            commit: info.commit.clone(),
            config: info.config.clone(),
            system: info.system.clone(),
            level: info.level.clone(),
            compiler: info.compiler.clone(),
            host: info.host.clone(),
            built: info.phase == Phase::Built,
            first_error: info.first_error.clone(),
            counts,
            failing,
        }
    }

    /// The file name without the extension.
    #[must_use]
    pub fn name(&self) -> String {
        let name = format!("{}-{}-{}{}", self.pin, self.config, self.system, self.level);
        match &self.row {
            Some(row) => format!("{row}-{name}"),
            None => name,
        }
    }

    fn count(&self, outcome: Outcome) -> usize {
        self.counts.get(outcome.name()).copied().unwrap_or(0)
    }

    fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What changed on one row since its previous night.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// The date of the night compared against, if there was one.
    pub since: Option<String>,
    /// The compiler that night.
    pub since_compiler: Option<String>,
    /// Whether the build stopped tonight after getting through before.
    pub build: bool,
    /// Tests that do not pass tonight and did before.
    pub regressed: BTreeSet<String>,
    /// Tests that pass tonight and did not before.
    pub fixed: BTreeSet<String>,
}

/// Compare tonight with an earlier night of the same row.
#[must_use]
pub fn compare(tonight: &Night, before: Option<&Night>) -> Change {
    let Some(before) = before else {
        return Change::default();
    };
    let mut change = Change {
        since: Some(before.date.clone()),
        since_compiler: Some(before.compiler.clone()),
        build: before.built && !tonight.built,
        ..Change::default()
    };
    // A night that did not build has no records, so its tests are neither worse nor better.
    if tonight.built && before.built {
        change.regressed = tonight
            .failing
            .difference(&before.failing)
            .cloned()
            .collect();
        change.fixed = before
            .failing
            .difference(&tonight.failing)
            .cloned()
            .collect();
    }
    change
}

impl Change {
    /// Whether this is something to open an issue about.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        self.build || !self.regressed.is_empty()
    }
}

/// Every night file under `runs/`, oldest first, grouped by name.
fn history(runs: &Path) -> Result<BTreeMap<String, Vec<Night>>, String> {
    let mut all: BTreeMap<String, Vec<Night>> = BTreeMap::new();
    let Ok(dates) = std::fs::read_dir(runs) else {
        return Ok(all);
    };
    let mut dirs: Vec<PathBuf> = dates
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for file in files {
            let night = Night::load(&file)?;
            all.entry(night.name()).or_default().push(night);
        }
    }
    Ok(all)
}

/// The rows found under a directory of downloaded artifacts: any directory, at any depth, that
/// holds a `build.json`.
fn rows(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join("build.json").is_file() {
            found.push(dir);
            continue;
        }
        for entry in
            std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// What one run of `rpg nightly` did.
#[derive(Debug, Default)]
pub struct Recorded {
    /// The night files written.
    pub written: Vec<PathBuf>,
    /// Tonight's nights with what changed, by name.
    pub changes: BTreeMap<String, (Night, Change)>,
}

impl Recorded {
    /// The rows with a regression.
    #[must_use]
    pub fn regressions(&self) -> usize {
        self.changes
            .values()
            .filter(|(_, c)| c.is_regression())
            .count()
    }
}

/// Write tonight's night files and the report.
pub fn record(nights: &Path, runs: &Path, reports: &Path) -> Result<Recorded, String> {
    let before = history(runs)?;
    let mut recorded = Recorded::default();
    for dir in rows(nights)? {
        let info = BuildInfo::load(&dir)?;
        let records = std::fs::read_to_string(dir.join("records.jsonl"))
            .map(|text| parse(&text))
            .unwrap_or_default();
        let night = Night::from_row(&info, &records);
        let name = night.name();
        let earlier = before
            .get(&name)
            .and_then(|list| list.iter().rev().find(|n| n.date < night.date));
        let change = compare(&night, earlier);
        let path = runs.join(&night.date).join(format!("{name}.toml"));
        std::fs::create_dir_all(path.parent().unwrap_or(runs))
            .map_err(|e| format!("creating {}: {e}", runs.display()))?;
        let text = toml::to_string(&night).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
        recorded.written.push(path);
        recorded.changes.insert(name, (night, change));
    }
    let all = history(runs)?;
    std::fs::create_dir_all(reports).map_err(|e| format!("creating {}: {e}", reports.display()))?;
    let path = reports.join("nightly.md");
    std::fs::write(&path, report(&all, &recorded))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(recorded)
}

fn counts_cell(night: &Night) -> String {
    if !night.built {
        return "did not build".to_string();
    }
    let broken = night.count(Outcome::Failed)
        + night.count(Outcome::Crashed)
        + night.count(Outcome::Timeout)
        + night.count(Outcome::BuildFailed);
    format!(
        "{} passed, {broken} not, {} skipped",
        night.count(Outcome::Passed),
        night.count(Outcome::Skipped)
    )
}

/// `reports/nightly.md`.
#[must_use]
pub fn report(all: &BTreeMap<String, Vec<Night>>, tonight: &Recorded) -> String {
    let mut text = String::from("# Nightly\n\n");
    text.push_str(
        "Written by `rpg nightly` after each night's world run. The night files it reads are \
         under `runs/`.\n\n",
    );
    text.push_str("## Latest\n\n| Row | Date | Compiler | Tests | Since the night before |\n");
    text.push_str("|---|---|---|---|---|\n");
    for (name, nights) in all {
        let Some(last) = nights.last() else { continue };
        let change = tonight.changes.get(name).map_or_else(
            || compare(last, nights.iter().rev().find(|n| n.date < last.date)),
            |(_, c)| c.clone(),
        );
        let _ = writeln!(
            text,
            "| {name} | {} | {} | {} | {} |",
            last.date,
            last.compiler,
            counts_cell(last),
            change_cell(&change)
        );
    }
    for (name, (night, change)) in &tonight.changes {
        if change.regressed.is_empty() && change.fixed.is_empty() && !change.build {
            continue;
        }
        let _ = writeln!(text, "\n## {name} on {}\n", night.date);
        if change.build {
            let _ = writeln!(
                text,
                "The build stopped: {}\n",
                night
                    .first_error
                    .as_deref()
                    .unwrap_or("no error line was kept")
            );
        }
        list(&mut text, "Not passing any more", &change.regressed);
        list(&mut text, "Passing again", &change.fixed);
    }
    text.push_str("\n## History\n");
    for (name, nights) in all {
        let _ = writeln!(
            text,
            "\n### {name}\n\n| Date | Compiler | Tests |\n|---|---|---|"
        );
        for night in nights.iter().rev().take(HISTORY) {
            let _ = writeln!(
                text,
                "| {} | {} | {} |",
                night.date,
                night.compiler,
                counts_cell(night)
            );
        }
    }
    text
}

fn change_cell(change: &Change) -> String {
    match &change.since {
        None => "first night".to_string(),
        Some(_) if change.build => "the build stopped".to_string(),
        Some(_) => format!(
            "{} worse, {} better",
            change.regressed.len(),
            change.fixed.len()
        ),
    }
}

fn list(text: &mut String, title: &str, tests: &BTreeSet<String>) {
    if tests.is_empty() {
        return;
    }
    let _ = writeln!(text, "{title}:\n");
    for test in tests {
        let _ = writeln!(text, "- `{test}`");
    }
    text.push('\n');
}

/// The body of the issue to open when a row regressed, or `None`.
#[must_use]
pub fn issue(recorded: &Recorded, run_url: Option<&str>) -> Option<(String, String)> {
    let bad: Vec<_> = recorded
        .changes
        .iter()
        .filter(|(_, (_, c))| c.is_regression())
        .collect();
    let (_, (first, _)) = bad.first()?;
    let title = format!(
        "Postgres nightly {}: {} row{} regressed",
        first.date,
        bad.len(),
        if bad.len() == 1 { "" } else { "s" }
    );
    let mut body = String::new();
    for (name, (night, change)) in &bad {
        let _ = writeln!(body, "### {name}\n");
        let _ = writeln!(
            body,
            "Tonight with {}, compared with {} with {}.\n",
            night.compiler,
            change.since.as_deref().unwrap_or("nothing"),
            change.since_compiler.as_deref().unwrap_or("nothing")
        );
        if change.build {
            let _ = writeln!(
                body,
                "The build stopped: {}\n",
                night
                    .first_error
                    .as_deref()
                    .unwrap_or("no error line was kept")
            );
        }
        list(&mut body, "Not passing any more", &change.regressed);
    }
    if let Some(url) = run_url {
        let _ = writeln!(
            body,
            "The run, with the records and the triage report: {url}"
        );
    }
    Some((title, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn night(date: &str, built: bool, failing: &[&str]) -> Night {
        Night {
            date: date.to_string(),
            pin: "REL_18_6".to_string(),
            config: "minimal".to_string(),
            system: "autoconf".to_string(),
            level: "-O2".to_string(),
            compiler: "rucc 0.12.3".to_string(),
            built,
            failing: failing.iter().map(ToString::to_string).collect(),
            ..Night::default()
        }
    }

    #[test]
    fn a_new_failure_is_a_regression_and_a_gone_one_is_a_fix() {
        let before = night("2026-09-28", true, &["regress/join", "regress/int8"]);
        let tonight = night("2026-09-29", true, &["regress/int8", "isolation/deadlock"]);
        let change = compare(&tonight, Some(&before));
        assert_eq!(change.since.as_deref(), Some("2026-09-28"));
        assert!(change.is_regression());
        assert_eq!(
            change.regressed.iter().collect::<Vec<_>>(),
            ["isolation/deadlock"]
        );
        assert_eq!(change.fixed.iter().collect::<Vec<_>>(), ["regress/join"]);
    }

    #[test]
    fn a_first_night_and_a_steady_night_are_not_regressions() {
        let tonight = night("2026-09-29", true, &["regress/int8"]);
        assert!(!compare(&tonight, None).is_regression());
        let before = night("2026-09-28", true, &["regress/int8"]);
        assert!(!compare(&tonight, Some(&before)).is_regression());
    }

    #[test]
    fn a_build_that_stops_is_a_regression_without_blaming_tests() {
        let before = night("2026-09-28", true, &[]);
        let tonight = night("2026-09-29", false, &[]);
        let change = compare(&tonight, Some(&before));
        assert!(change.build);
        assert!(change.regressed.is_empty());
        assert!(change.is_regression());
    }

    #[test]
    fn the_name_is_the_row_pin_config_system_and_level() {
        let mut n = night("2026-09-29", true, &[]);
        assert_eq!(n.name(), "REL_18_6-minimal-autoconf-O2");
        n.row = Some("LA64".to_string());
        assert_eq!(n.name(), "LA64-REL_18_6-minimal-autoconf-O2");
    }

    #[test]
    fn a_night_file_reads_back_the_same() {
        let mut n = night("2026-09-29", true, &["regress/int8"]);
        n.counts.insert("passed".to_string(), 3);
        let text = toml::to_string(&n).unwrap();
        assert_eq!(toml::from_str::<Night>(&text).unwrap(), n);
    }

    #[test]
    fn the_issue_names_the_row_and_the_tests() {
        let before = night("2026-09-28", true, &[]);
        let tonight = night("2026-09-29", true, &["regress/int8"]);
        let change = compare(&tonight, Some(&before));
        let mut recorded = Recorded::default();
        recorded
            .changes
            .insert(tonight.name(), (tonight.clone(), change));
        assert_eq!(recorded.regressions(), 1);
        let (title, body) = issue(&recorded, Some("https://example.com/run")).unwrap();
        assert_eq!(title, "Postgres nightly 2026-09-29: 1 row regressed");
        assert!(body.contains("### REL_18_6-minimal-autoconf-O2"));
        assert!(body.contains("- `regress/int8`"));
        assert!(body.contains("compared with 2026-09-28"));
        let report = report(
            &BTreeMap::from([(tonight.name(), vec![before, tonight])]),
            &recorded,
        );
        assert!(report.contains("| 1 worse, 0 better |"));
        assert!(report.contains("Not passing any more"));
    }

    #[test]
    fn record_writes_night_files_and_compares_with_the_last_one() {
        let dir = std::env::temp_dir().join(format!("rpg-nightly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs = dir.join("runs");
        let reports = dir.join("reports");
        let mut earlier = night("2026-09-28", true, &[]);
        earlier.row = Some("L64".to_string());
        std::fs::create_dir_all(runs.join("2026-09-28")).unwrap();
        std::fs::write(
            runs.join("2026-09-28")
                .join(format!("{}.toml", earlier.name())),
            toml::to_string(&earlier).unwrap(),
        )
        .unwrap();
        let row = dir.join("nights/rpg-world-REL_18_6-autoconf-O2");
        std::fs::create_dir_all(&row).unwrap();
        let info = serde_json::json!({
            "pin": "REL_18_6", "commit": "abc", "config": "minimal", "system": "autoconf",
            "level": "-O2", "cc": "/rucc", "compiler": "rucc 0.12.3", "kind": "rucc",
            "rucc-trace": false, "twice": false, "host": "runner", "date": "2026-09-29",
            "jobs": 4, "source": "/src", "build-dir": "/build", "phase": "built",
            "configure-seconds": 1.0, "compiles": {
                "probe-calls": 0, "probe-failures": 0, "build-calls": 0, "build-compiles": 0,
                "build-failures": 0, "unreadable-lines": 0, "build-user-seconds": 0.0,
                "peak-rss-kb": 0
            }
        });
        std::fs::write(row.join("build.json"), info.to_string()).unwrap();
        let line = r#"{"project":"postgres","pin":"REL_18_6","row":"L64","host":"runner","level":"-O2","system":"autoconf","config":"minimal","suite":"regress","test":"int8","outcome":"failed","compiler":"rucc 0.12.3"}"#;
        std::fs::write(row.join("records.jsonl"), format!("{line}\n")).unwrap();
        let recorded = record(&dir.join("nights"), &runs, &reports).unwrap();
        assert_eq!(recorded.written.len(), 1);
        assert!(
            runs.join("2026-09-29/L64-REL_18_6-minimal-autoconf-O2.toml")
                .is_file()
        );
        assert_eq!(recorded.regressions(), 1);
        let text = std::fs::read_to_string(reports.join("nightly.md")).unwrap();
        assert!(text.contains("| L64-REL_18_6-minimal-autoconf-O2 | 2026-09-29 | rucc 0.12.3 |"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
