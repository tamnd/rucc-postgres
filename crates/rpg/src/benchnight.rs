//! `rpg bench-night`: keep what each night's bench found in git and say when rucc lost ground.
//!
//! The bench workflow runs `rpg bench` against a gcc build and a rucc build on one runner each
//! night. The runner is a different machine from one night to the next, so a rucc number on its
//! own says little, but how many times as long the rucc server takes as the gcc one on the same
//! machine moves only with the code. This reads the two `bench.json` files, turns each measure
//! into that ratio, and writes it to `runs/bench/<date>.json` with the noise beside it, which is
//! the two interquartile ranges as fractions of their medians, added together.
//!
//! Tonight is compared with the last earlier night run the same way, the same pin, configuration,
//! level, clients and sizes. A measure whose ratio grew by more than `--threshold` percent (10 by
//! default) and by more than the noise of both nights together is a regression, and when there
//! is one the text of an issue is written for the workflow to open with the `kind/perf` label.
//! `reports/bench.md` is rewritten with the headline ratios of the last thirty nights.

use crate::bench::{Bench, Measure, spread};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// How many nights the report shows.
const HISTORY: usize = 30;

/// What the analytic queries' ratios are summed up as.
const GEOMETRIC_MEAN: &str = "analytic geometric mean";

/// The measures the report shows, with the column each goes under.
const HEADLINES: [(&str, &str); 4] = [
    ("pgbench select-only", "select-only"),
    ("pgbench tpcb-like", "tpcb-like"),
    (GEOMETRIC_MEAN, "analytic"),
    ("regression suite", "make check"),
];

/// One measure's ratio on one night.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Ratio {
    /// The measure's name, as in `bench.json`.
    pub name: String,
    /// How many times as long the rucc server took as the gcc one.
    pub times: f64,
    /// Both interquartile ranges as fractions of their medians, added together.
    pub noise: f64,
}

/// `runs/bench/<date>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BenchNight {
    /// The day, UTC.
    pub date: String,
    /// The pin.
    pub pin: String,
    /// The configuration.
    pub config: String,
    /// The build system.
    pub system: String,
    /// The level.
    pub level: String,
    /// The gcc build's compiler.
    pub gcc: String,
    /// The rucc build's compiler.
    pub rucc: String,
    /// The machine.
    pub host: String,
    /// pgbench clients.
    pub clients: usize,
    /// pgbench's scale factor.
    pub scale: u32,
    /// Seconds per pgbench run.
    pub seconds: u64,
    /// The analytic set's scale factor.
    pub scale_factor: f64,
    /// The measures both builds finished with the same answers, in `bench.json`'s order.
    pub ratios: Vec<Ratio>,
    /// The measures that could not be compared, with why.
    #[serde(default)]
    pub missing: Vec<String>,
}

impl BenchNight {
    /// Turn the two builds' runs into ratios.
    pub fn from_pair(gcc: &Bench, rucc: &Bench) -> Result<Self, String> {
        fn setup(b: &Bench) -> (&str, &str, &str, &str, usize, u32, u64) {
            (
                &b.pin, &b.config, &b.system, &b.level, b.clients, b.scale, b.seconds,
            )
        }
        if setup(gcc) != setup(rucc) || gcc.scale_factor.total_cmp(&rucc.scale_factor).is_ne() {
            return Err("the gcc and rucc runs were not run the same way".into());
        }
        let mut night = Self {
            date: rucc.date.clone(),
            pin: rucc.pin.clone(),
            config: rucc.config.clone(),
            system: rucc.system.clone(),
            level: rucc.level.clone(),
            gcc: gcc.compiler.clone(),
            rucc: rucc.compiler.clone(),
            host: rucc.host.clone(),
            clients: rucc.clients,
            scale: rucc.scale,
            seconds: rucc.seconds,
            scale_factor: rucc.scale_factor,
            ratios: Vec::new(),
            missing: Vec::new(),
        };
        let mut analytic = Vec::new();
        for g in &gcc.measures {
            let Some(r) = rucc.measures.iter().find(|m| m.name == g.name) else {
                night.missing.push(format!("{}: only gcc ran it", g.name));
                continue;
            };
            match ratio(g, r) {
                Ok(ratio) => {
                    if g.answer.is_some() {
                        analytic.push(ratio.times);
                    }
                    night.ratios.push(ratio);
                }
                Err(why) => night.missing.push(format!("{}: {why}", g.name)),
            }
        }
        if !analytic.is_empty() {
            #[allow(clippy::cast_precision_loss)]
            let mean =
                (analytic.iter().copied().map(f64::ln).sum::<f64>() / analytic.len() as f64).exp();
            // A mean of many queries moves less than any one of them, so only the threshold
            // stands between it and an issue.
            night.ratios.push(Ratio {
                name: GEOMETRIC_MEAN.to_string(),
                times: mean,
                noise: 0.0,
            });
        }
        Ok(night)
    }

    /// Whether two nights were run the same way, so their ratios compare.
    fn same_setup(&self, other: &Self) -> bool {
        self.pin == other.pin
            && self.config == other.config
            && self.system == other.system
            && self.level == other.level
            && self.clients == other.clients
            && self.scale == other.scale
            && self.seconds == other.seconds
            && self.scale_factor.total_cmp(&other.scale_factor).is_eq()
    }

    fn ratio(&self, name: &str) -> Option<&Ratio> {
        self.ratios.iter().find(|r| r.name == name)
    }

    fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// One measure's ratio, or why there is none.
fn ratio(gcc: &Measure, rucc: &Measure) -> Result<Ratio, String> {
    if let Some(why) = gcc.failed.as_ref() {
        return Err(format!("the gcc run failed, {why}"));
    }
    if let Some(why) = rucc.failed.as_ref() {
        return Err(format!("the rucc run failed, {why}"));
    }
    if gcc.answer != rucc.answer {
        return Err("the two servers gave different answers".into());
    }
    let (Some(g), Some(r)) = (spread(&gcc.samples), spread(&rucc.samples)) else {
        return Err("no samples".into());
    };
    if g.median <= 0.0 || r.median <= 0.0 {
        return Err("a median of zero".into());
    }
    let times = if gcc.higher_is_better {
        g.median / r.median
    } else {
        r.median / g.median
    };
    Ok(Ratio {
        name: gcc.name.clone(),
        times,
        noise: g.range() / g.median + r.range() / r.median,
    })
}

/// A measure that got worse since the night before.
#[derive(Debug, Clone, PartialEq)]
pub struct Worse {
    /// The measure.
    pub name: String,
    /// Its ratio the night before.
    pub before: f64,
    /// Its ratio tonight.
    pub tonight: f64,
}

/// The measures whose ratio grew by more than `threshold` and by more than both nights' noise.
#[must_use]
pub fn regressions(before: &BenchNight, tonight: &BenchNight, threshold: f64) -> Vec<Worse> {
    tonight
        .ratios
        .iter()
        .filter_map(|now| {
            let then = before.ratio(&now.name)?;
            let grew = now.times / then.times - 1.0;
            (grew > threshold.max(now.noise + then.noise)).then(|| Worse {
                name: now.name.clone(),
                before: then.times,
                tonight: now.times,
            })
        })
        .collect()
}

/// Every night under `runs/bench`, oldest first.
fn history(dir: &Path) -> Result<Vec<BenchNight>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
        .iter()
        .map(PathBuf::as_path)
        .map(BenchNight::load)
        .collect()
}

/// What `rpg bench-night` did.
#[derive(Debug)]
pub struct Recorded {
    /// Tonight.
    pub night: BenchNight,
    /// The night it was compared with, when there was one run the same way.
    pub before: Option<BenchNight>,
    /// The measures that got worse.
    pub worse: Vec<Worse>,
    /// The night file written.
    pub path: PathBuf,
}

/// Write tonight's night file and the report, and compare tonight with the night before.
pub fn record(
    gcc: &Bench,
    rucc: &Bench,
    runs: &Path,
    reports: &Path,
    threshold: f64,
) -> Result<Recorded, String> {
    let night = BenchNight::from_pair(gcc, rucc)?;
    let before = history(runs)?
        .into_iter()
        .rev()
        .find(|n| n.date < night.date && n.same_setup(&night));
    let worse = before
        .as_ref()
        .map(|b| regressions(b, &night, threshold))
        .unwrap_or_default();
    std::fs::create_dir_all(runs).map_err(|e| format!("creating {}: {e}", runs.display()))?;
    let path = runs.join(format!("{}.json", night.date));
    let text = serde_json::to_string_pretty(&night).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("writing {}: {e}", path.display()))?;
    std::fs::create_dir_all(reports).map_err(|e| format!("creating {}: {e}", reports.display()))?;
    let report_path = reports.join("bench.md");
    std::fs::write(&report_path, report(&history(runs)?))
        .map_err(|e| format!("writing {}: {e}", report_path.display()))?;
    Ok(Recorded {
        night,
        before,
        worse,
        path,
    })
}

/// `reports/bench.md`: the headline ratios of the last thirty nights, the latest first.
#[must_use]
pub fn report(all: &[BenchNight]) -> String {
    let mut text = String::from(
        "# The bench, night by night\n\n\
         Each number is how many times as long the rucc server took as the gcc one, built from the \
         same pin at the same level and run one after the other on the same runner, by the median \
         of the night's runs. The analytic column is the geometric mean over the twenty queries. \
         Written by `rpg bench-night`.\n\n\
         | night | pin | level | rucc | ",
    );
    text.push_str(&HEADLINES.map(|(_, column)| column).join(" | "));
    text.push_str(" |\n|---|---|---|---|");
    text.push_str(&"---:|".repeat(HEADLINES.len()));
    text.push('\n');
    for night in all.iter().rev().take(HISTORY) {
        let _ = write!(
            text,
            "| {} | {} | {} | {} |",
            night.date, night.pin, night.level, night.rucc
        );
        for (name, _) in HEADLINES {
            match night.ratio(name) {
                Some(r) => {
                    let _ = write!(text, " {:.2} |", r.times);
                }
                None => text.push_str(" |"),
            }
        }
        text.push('\n');
    }
    text
}

/// The title and body of the issue to open when a measure got worse, or `None`.
#[must_use]
pub fn issue(recorded: &Recorded, run_url: Option<&str>) -> Option<(String, String)> {
    let before = recorded.before.as_ref()?;
    if recorded.worse.is_empty() {
        return None;
    }
    let night = &recorded.night;
    let count = recorded.worse.len();
    let title = format!(
        "Bench {}: rucc lost ground on {count} measure{}",
        night.date,
        if count == 1 { "" } else { "s" }
    );
    let mut body = format!(
        "Tonight with {} against {}, compared with {} with {} against {}, both {} {} {} at {}. \
         Each number is how many times as long the rucc server took as the gcc one on the same \
         runner.\n\n| measure | {} | {} | change |\n|---|---:|---:|---:|\n",
        night.rucc,
        night.gcc,
        before.date,
        before.rucc,
        before.gcc,
        night.pin,
        night.config,
        night.system,
        night.level,
        before.date,
        night.date
    );
    for w in &recorded.worse {
        let _ = writeln!(
            body,
            "| {} | {:.2} | {:.2} | +{:.1}% |",
            w.name,
            w.before,
            w.tonight,
            (w.tonight / w.before - 1.0) * 100.0
        );
    }
    body.push_str(
        "\nA measure counts when its ratio grew by more than the threshold and by more than the \
         interquartile ranges of both nights allow. `rpg profile-compare` on the two runs' \
         profiles says where the time went.\n",
    );
    if let Some(url) = run_url {
        let _ = writeln!(
            body,
            "\nThe run, with both nights' bench and profile files: {url}"
        );
    }
    Some((title, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(name: &str, unit: &str, higher: bool, samples: &[f64]) -> Measure {
        Measure {
            name: name.to_string(),
            unit: unit.to_string(),
            higher_is_better: higher,
            samples: samples.to_vec(),
            failed: None,
            answer: None,
        }
    }

    fn bench(compiler: &str, date: &str, measures: Vec<Measure>) -> Bench {
        Bench {
            pin: "REL_18_6".into(),
            level: "-O2".into(),
            system: "autoconf".into(),
            config: "minimal".into(),
            compiler: compiler.into(),
            host: "runner".into(),
            date: date.into(),
            clients: 4,
            scale: 100,
            seconds: 60,
            scale_factor: 1.0,
            measures,
        }
    }

    fn pair(date: &str, rucc_tps: f64, rucc_ms: f64) -> (Bench, Bench) {
        let mut query = measure(
            "analytic pricing summary",
            "ms",
            false,
            &[100.0, 100.0, 100.0],
        );
        query.answer = Some("abc".into());
        let gcc = bench(
            "gcc 13",
            date,
            vec![
                measure(
                    "pgbench select-only",
                    "tps",
                    true,
                    &[1000.0, 1000.0, 1000.0],
                ),
                query.clone(),
                measure("regression suite", "s", false, &[60.0, 60.0, 60.0]),
            ],
        );
        query.samples = vec![rucc_ms; 3];
        let rucc = bench(
            "rucc 0.18.8",
            date,
            vec![
                measure("pgbench select-only", "tps", true, &[rucc_tps; 3]),
                query,
                measure("regression suite", "s", false, &[90.0, 90.0, 90.0]),
            ],
        );
        (gcc, rucc)
    }

    #[test]
    fn ratios_say_how_many_times_as_long_rucc_took() {
        let (gcc, rucc) = pair("2026-10-03", 250.0, 300.0);
        let night = BenchNight::from_pair(&gcc, &rucc).unwrap();
        let names: Vec<_> = night
            .ratios
            .iter()
            .map(|r| format!("{} {:.3}", r.name, r.times))
            .collect();
        assert_eq!(
            names,
            vec![
                "pgbench select-only 4.000",
                "analytic pricing summary 3.000",
                "regression suite 1.500",
                "analytic geometric mean 3.000",
            ]
        );
        assert!(night.missing.is_empty());
    }

    #[test]
    fn a_failed_or_different_measure_is_left_out_with_why() {
        let (gcc, mut rucc) = pair("2026-10-03", 250.0, 300.0);
        rucc.measures[0].failed = Some("run 2: pgbench exited Some(2)".into());
        rucc.measures[1].answer = Some("abd".into());
        let night = BenchNight::from_pair(&gcc, &rucc).unwrap();
        assert_eq!(night.ratios.len(), 1);
        assert_eq!(
            night.missing,
            vec![
                "pgbench select-only: the rucc run failed, run 2: pgbench exited Some(2)",
                "analytic pricing summary: the two servers gave different answers",
            ]
        );
    }

    #[test]
    fn noise_is_both_ranges_over_their_medians() {
        let g = measure("m", "s", false, &[9.0, 10.0, 11.0]);
        let r = measure("m", "s", false, &[18.0, 20.0, 22.0]);
        let got = ratio(&g, &r).unwrap();
        assert!((got.times - 2.0).abs() < 1e-9);
        assert!((got.noise - 0.2).abs() < 1e-9, "{got:?}");
    }

    #[test]
    fn a_ratio_that_grew_past_the_threshold_and_the_noise_is_a_regression() {
        let (gcc, rucc) = pair("2026-10-02", 250.0, 300.0);
        let before = BenchNight::from_pair(&gcc, &rucc).unwrap();
        // select-only from 4 to 5 times as long, 25% worse, and the query from 3 to 3.15, 5%.
        let (gcc, rucc) = pair("2026-10-03", 200.0, 315.0);
        let tonight = BenchNight::from_pair(&gcc, &rucc).unwrap();
        let worse = regressions(&before, &tonight, 0.10);
        assert_eq!(worse.len(), 1, "{worse:?}");
        assert_eq!(worse[0].name, "pgbench select-only");
        assert!(regressions(&before, &tonight, 0.30).is_empty());
        let mut noisy = tonight.clone();
        noisy.ratios[0].noise = 0.3;
        assert!(regressions(&before, &noisy, 0.10).is_empty());
        assert!(
            regressions(&tonight, &before, 0.10).is_empty(),
            "getting faster is not worse"
        );
    }

    #[test]
    fn record_compares_with_the_last_night_run_the_same_way_and_writes_an_issue() {
        let dir = std::env::temp_dir().join(format!("rpg-bench-night-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let runs = dir.join("runs");
        let reports = dir.join("reports");
        let (gcc, rucc) = pair("2026-10-01", 250.0, 300.0);
        let first = record(&gcc, &rucc, &runs, &reports, 0.10).unwrap();
        assert!(first.before.is_none() && issue(&first, None).is_none());
        // A night at another scale does not count as the one before.
        let (mut gcc, mut rucc) = pair("2026-10-02", 100.0, 300.0);
        gcc.scale = 10;
        rucc.scale = 10;
        record(&gcc, &rucc, &runs, &reports, 0.10).unwrap();
        let (gcc, rucc) = pair("2026-10-03", 200.0, 300.0);
        let third = record(&gcc, &rucc, &runs, &reports, 0.10).unwrap();
        assert_eq!(third.before.as_ref().unwrap().date, "2026-10-01");
        let (title, body) = issue(&third, Some("https://example.com/run/1")).unwrap();
        assert_eq!(title, "Bench 2026-10-03: rucc lost ground on 1 measure");
        assert!(
            body.contains("| pgbench select-only | 4.00 | 5.00 | +25.0% |"),
            "{body}"
        );
        assert!(body.contains("https://example.com/run/1"), "{body}");
        let report = std::fs::read_to_string(reports.join("bench.md")).unwrap();
        let rows: Vec<_> = report.lines().filter(|l| l.starts_with("| 2026")).collect();
        assert_eq!(rows.len(), 3, "{report}");
        assert_eq!(
            rows[0],
            "| 2026-10-03 | REL_18_6 | -O2 | rucc 0.18.8 | 5.00 | | 3.00 | 1.50 |"
        );
        assert!(runs.join("2026-10-03.json").is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_runs_made_differently_are_refused() {
        let (gcc, mut rucc) = pair("2026-10-03", 250.0, 300.0);
        rucc.clients = 8;
        assert!(BenchNight::from_pair(&gcc, &rucc).is_err());
    }
}
