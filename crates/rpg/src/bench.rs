//! `rpg bench`: how fast the server a build produced runs, kept so that two builds can be
//! compared, and `rpg bench-compare`, which compares them.
//!
//! PG7 grades the code rucc makes by the server it makes. A run is pgbench's `select-only` and
//! `tpcb-like` scripts against a database at `--scale` (100 by default), taking turns, `--runs`
//! times each (10 by default) for `--seconds` each (60 by default), then the twenty queries of the
//! analytic set at `--sf` (1 by default) `--runs` times each, and then the main regression suite
//! `--runs` times, timed by the wall clock. Every sample is kept in `bench/bench.json`, and a
//! measure is summed up as the median with the interquartile range beside it.
//!
//! Two builds are compared measure by measure. A difference between the medians that is smaller
//! than the two interquartile ranges added together is reported as no measurable difference,
//! since that much moves between runs of one build. The analytic set is summed up as well by the
//! geometric mean of its twenty ratios, which is how PG7 states its bound.

use crate::analytic;
use crate::process::hostname;
use crate::records::{FailClass, Outcome, TestRecord};
use crate::stress::{Install, read_pgbench};
use crate::suite::{SuitePlan, record};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

/// What a bench run needs beyond the build.
#[derive(Debug, Clone)]
pub struct BenchPlan {
    /// The build, with `suite` set to `bench`.
    pub suite: SuitePlan,
    /// How many times each measure is taken.
    pub runs: u32,
    /// How long each pgbench run lasts.
    pub seconds: u64,
    /// pgbench clients, and as many threads.
    pub clients: usize,
    /// pgbench's scale factor, 100,000 accounts each.
    pub scale: u32,
    /// The analytic set's scale factor.
    pub scale_factor: f64,
    /// Run pgbench.
    pub pgbench: bool,
    /// Time the analytic set.
    pub analytic: bool,
    /// Time the regression suite.
    pub regress: bool,
}

/// One thing measured, with every sample taken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Measure {
    /// For example `pgbench select-only`.
    pub name: String,
    /// `tps` or `s`.
    pub unit: String,
    /// Whether a larger number is the better one, as for a rate and not for a time.
    pub higher_is_better: bool,
    /// One per run, in the order they were taken.
    pub samples: Vec<f64>,
    /// Why the measure stopped short, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// The fingerprint of what a query printed, the same on every run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

impl Measure {
    fn new(name: &str, unit: &str, higher_is_better: bool) -> Self {
        Self {
            name: name.to_string(),
            unit: unit.to_string(),
            higher_is_better,
            samples: Vec::new(),
            failed: None,
            answer: None,
        }
    }
}

/// `bench.json`: what was measured, on what, and the samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Bench {
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
    /// pgbench clients.
    pub clients: usize,
    /// pgbench's scale factor.
    pub scale: u32,
    /// Seconds per pgbench run.
    pub seconds: u64,
    /// The analytic set's scale factor.
    #[serde(default)]
    pub scale_factor: f64,
    /// The measures, in the order they were taken.
    pub measures: Vec<Measure>,
}

impl Bench {
    /// Read a `bench.json`, or the one under a build directory.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() {
            path.join("bench").join("bench.json")
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
}

/// The median of a set of samples and the quartiles either side of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    /// The first quartile.
    pub low: f64,
    /// The median.
    pub median: f64,
    /// The third quartile.
    pub high: f64,
}

impl Spread {
    /// The interquartile range.
    #[must_use]
    pub fn range(&self) -> f64 {
        self.high - self.low
    }
}

/// The value a fraction of the way through sorted samples, between the two nearest when it
/// falls between them, as R and numpy compute quantiles by default.
fn quantile(sorted: &[f64], fraction: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let at = (sorted.len() - 1) as f64 * fraction;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let below = at.floor() as usize;
    let above = (below + 1).min(sorted.len() - 1);
    sorted[below] + (at - at.floor()) * (sorted[above] - sorted[below])
}

/// The median and quartiles of some samples, or nothing when there are none.
#[must_use]
pub fn spread(samples: &[f64]) -> Option<Spread> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Spread {
        low: quantile(&sorted, 0.25),
        median: quantile(&sorted, 0.5),
        high: quantile(&sorted, 0.75),
    })
}

/// What `b` comes to against `a` for one measure: how much faster or slower, or no measurable
/// difference when the medians are closer than the two ranges added together.
#[must_use]
pub fn verdict(higher_is_better: bool, a: Spread, b: Spread) -> String {
    if (b.median - a.median).abs() <= a.range() + b.range() {
        return "no measurable difference".to_string();
    }
    faster_or_slower(speed(higher_is_better, a, b))
}

fn shown(unit: &str, spread: Spread) -> String {
    let places = usize::from(unit != "tps");
    format!(
        "{:.places$} ({:.places$} to {:.places$})",
        spread.median, spread.low, spread.high
    )
}

/// How fast b is when a is 1, whichever way round the unit runs.
fn speed(higher_is_better: bool, a: Spread, b: Spread) -> f64 {
    if higher_is_better {
        b.median / a.median
    } else {
        a.median / b.median
    }
}

fn faster_or_slower(speed: f64) -> String {
    if speed >= 1.0 {
        format!("{:.1}% faster", (speed - 1.0) * 100.0)
    } else {
        format!("{:.1}% slower", (1.0 - speed) * 100.0)
    }
}

/// The comparison of two runs as a Markdown report.
#[must_use]
pub fn compare(a: &Bench, b: &Bench) -> String {
    let setup = |x: &Bench| {
        format!(
            "{} {} {} with {} clients at scale {} and the analytic set at SF {}",
            x.pin, x.config, x.system, x.clients, x.scale, x.scale_factor
        )
    };
    let mut text = format!(
        "# {} against {}\n\na is {}, b is {}, both {}.\n",
        b.label(),
        a.label(),
        a.label(),
        b.label(),
        setup(a)
    );
    if setup(a) != setup(b) {
        let _ = write!(
            text,
            "\nThe two runs differ in more than the compiler: b is {}.\n",
            setup(b)
        );
    }
    text.push_str(
        "\nEach cell is the median with the interquartile range in brackets.\n\n\
         | measure | a | b | b against a |\n|---|---|---|---|\n",
    );
    let mut analytic = Vec::new();
    for left in &a.measures {
        let Some(right) = b.measures.iter().find(|m| m.name == left.name) else {
            let _ = writeln!(text, "| {} | | | only in a |", left.name);
            continue;
        };
        let cell = |m: &Measure| match (&m.failed, spread(&m.samples)) {
            (Some(why), _) => format!("failed: {why}"),
            (None, Some(s)) => shown(&m.unit, s),
            (None, None) => "no samples".to_string(),
        };
        let said = match (
            &left.failed,
            &right.failed,
            spread(&left.samples),
            spread(&right.samples),
        ) {
            _ if left.answer.is_some() && right.answer.is_some() && left.answer != right.answer => {
                "different answers".to_string()
            }
            (None, None, Some(x), Some(y)) => {
                if left.name.starts_with(ANALYTIC) {
                    analytic.push(speed(left.higher_is_better, x, y));
                }
                verdict(left.higher_is_better, x, y)
            }
            _ => String::new(),
        };
        let _ = writeln!(
            text,
            "| {} ({}) | {} | {} | {said} |",
            left.name,
            left.unit,
            cell(left),
            cell(right)
        );
    }
    for right in &b.measures {
        if !a.measures.iter().any(|m| m.name == right.name) {
            let _ = writeln!(text, "| {} | | | only in b |", right.name);
        }
    }
    if !analytic.is_empty() {
        #[allow(clippy::cast_precision_loss)]
        let mean =
            (analytic.iter().copied().map(f64::ln).sum::<f64>() / analytic.len() as f64).exp();
        let _ = write!(
            text,
            "\nOver the {} analytic queries both runs finished with the same answers, b is {} by the geometric mean.\n",
            analytic.len(),
            faster_or_slower(mean)
        );
    }
    text
}

/// The server settings both builds run under. The data fits in memory and nothing waits on the
/// disk, so the time goes to the code the compiler made. Autovacuum is off because pgbench
/// vacuums the tables it writes to before each run, and the analytic set is analyzed once
/// after it is loaded and never written to again.
const SETTINGS: &str = "shared_buffers = 1GB\nwork_mem = 64MB\nmaintenance_work_mem = 512MB\n\
     fsync = off\nsynchronous_commit = off\nfull_page_writes = off\nmax_wal_size = 16GB\n\
     checkpoint_timeout = 1h\nautovacuum = off\njit = off\n";

/// What the analytic queries' measures are named, before the query's own name.
const ANALYTIC: &str = "analytic ";

/// The pgbench scripts measured, by their built in names.
const SCRIPTS: [&str; 2] = ["select-only", "tpcb-like"];

/// One pgbench run of a built in script, and its rate.
fn pgbench(
    install: &Install,
    plan: &BenchPlan,
    script: &str,
    seconds: u64,
    log: &str,
) -> Result<f64, String> {
    let clients = plan.clients.max(1).to_string();
    let done = install
        .step(&format!("pgbench {script}"), "pgbench", log)
        .args([
            "-b",
            script,
            "-M",
            "prepared",
            "-c",
            &clients,
            "-j",
            &clients,
            "-T",
            &seconds.to_string(),
            "postgres",
        ])
        .run()?;
    let text = std::fs::read_to_string(install.dir().join(log)).unwrap_or_default();
    let (_, tps) = read_pgbench(&text)?;
    if !done.ok {
        return Err(format!("pgbench exited {:?}", done.code));
    }
    tps.parse()
        .map_err(|_| format!("pgbench gave a rate of {tps}"))
}

/// The measures taken against a running server, pgbench's and the analytic set's.
fn server_measures(plan: &BenchPlan, dir: &Path) -> Result<Vec<Measure>, String> {
    let install = Install::prepare(&plan.suite, dir, "bench")?;
    let result = session(&install, plan);
    install.finish();
    // The database is several gigabytes and nothing reads it again.
    std::fs::remove_dir_all(install.data()).ok();
    result
}

fn session(install: &Install, plan: &BenchPlan) -> Result<Vec<Measure>, String> {
    install.initdb(plan.clients.max(1) + 10, SETTINGS)?;
    if !install.pg_ctl(&["start"], "start.log")? {
        return Err("the server did not start".into());
    }
    let mut measures = Vec::new();
    if plan.pgbench {
        measures.extend(pgbench_measures(install, plan)?);
    }
    if plan.analytic {
        measures.extend(analytic_measures(install, plan)?);
    }
    Ok(measures)
}

/// The pgbench measures, taken in turns.
fn pgbench_measures(install: &Install, plan: &BenchPlan) -> Result<Vec<Measure>, String> {
    if !install
        .step("pgbench -i", "pgbench", "pgbench-init.log")
        .args(["-i", "-s", &plan.scale.to_string(), "-q", "postgres"])
        .run()?
        .ok
    {
        return Err("pgbench -i failed".into());
    }
    // The first run reads the tables into memory, so it is not counted.
    pgbench(
        install,
        plan,
        "select-only",
        plan.seconds.min(30),
        "pgbench-warmup.log",
    )
    .map_err(|why| format!("the warm up run failed: {why}"))?;
    let mut measures: Vec<Measure> = SCRIPTS
        .iter()
        .map(|s| Measure::new(&format!("pgbench {s}"), "tps", true))
        .collect();
    for run in 1..=plan.runs {
        for (script, measure) in SCRIPTS.iter().zip(&mut measures) {
            if measure.failed.is_some() {
                continue;
            }
            let log = format!("pgbench-{script}-{run}.log");
            match pgbench(install, plan, script, plan.seconds, &log) {
                Ok(tps) => measure.samples.push(tps),
                Err(why) => measure.failed = Some(format!("run {run}: {why}")),
            }
        }
    }
    Ok(measures)
}

/// The analytic set, loaded and then each query run once to warm up and keep its answer, and
/// then `--runs` times in turns. A query that answers differently on a later run fails.
fn analytic_measures(install: &Install, plan: &BenchPlan) -> Result<Vec<Measure>, String> {
    let sql = install.dir().join("analytic-load.sql");
    std::fs::write(&sql, analytic::load(plan.scale_factor))
        .map_err(|e| format!("writing {}: {e}", sql.display()))?;
    if !install
        .step("load the analytic set", "psql", "analytic-load.log")
        .args(["-XAtq", "-v", "ON_ERROR_STOP=1", "-f"])
        .args([crate::stress::path_str(&sql)?, "postgres"])
        .run()?
        .ok
    {
        return Err("loading the analytic set failed".into());
    }
    let queries = analytic::queries();
    let mut measures = Vec::new();
    for (name, query) in &queries {
        let mut measure = Measure::new(&format!("{ANALYTIC}{name}"), "ms", false);
        match install.psql(query) {
            Ok(text) => measure.answer = Some(analytic::fingerprint(&text)),
            Err(why) => measure.failed = Some(format!("the first run: {why}")),
        }
        measures.push(measure);
    }
    for run in 1..=plan.runs {
        eprintln!("rpg: the analytic set, run {run} of {}", plan.runs);
        for ((_, query), measure) in queries.iter().zip(&mut measures) {
            if measure.failed.is_some() {
                continue;
            }
            let clock = Instant::now();
            match install.psql(query) {
                Ok(text) if measure.answer.as_deref() == Some(&analytic::fingerprint(&text)) => {
                    measure.samples.push(clock.elapsed().as_secs_f64() * 1000.0);
                }
                Ok(_) => {
                    measure.failed = Some(format!("run {run}: the answer changed"));
                }
                Err(why) => measure.failed = Some(format!("run {run}: {why}")),
            }
        }
    }
    Ok(measures)
}

/// The regression suite, timed by the wall clock, setup included, as `make check` is.
fn regress_measure(plan: &BenchPlan) -> Measure {
    let mut measure = Measure::new("regression suite", "s", false);
    for run in 1..=plan.runs {
        let suite = SuitePlan {
            suite: "regress".to_string(),
            label: Some("bench-regress".to_string()),
            run,
            baseline: None,
            ..plan.suite.clone()
        };
        let done = match crate::suite::run(&suite) {
            Ok(done) => done,
            Err(why) => {
                measure.failed = Some(format!("run {run}: {why}"));
                break;
            }
        };
        let missed = done
            .records
            .iter()
            .filter(|r| r.outcome != Outcome::Passed && r.outcome != Outcome::Skipped)
            .count();
        if missed > 0 {
            // A suite that fails a test may have skipped work, so its time says nothing.
            measure.failed = Some(format!("run {run}: {missed} tests did not pass"));
            break;
        }
        measure.samples.push(done.seconds);
    }
    measure
}

/// Run it and write `bench/bench.json`.
pub fn run(plan: &BenchPlan) -> Result<Bench, String> {
    let info = &plan.suite.info;
    if info.phase != crate::build::Phase::Built {
        return Err("the build did not finish, so there is nothing to measure".into());
    }
    if plan.runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    if plan.analytic && (plan.scale_factor <= 0.0 || plan.scale_factor.is_nan()) {
        return Err("--sf must be above 0".into());
    }
    crate::suite::refuse_root()?;
    let dir = plan.suite.out.join("bench");
    let mut measures = Vec::new();
    if plan.pgbench || plan.analytic {
        measures.extend(server_measures(plan, &dir)?);
    } else {
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    if plan.regress {
        measures.push(regress_measure(plan));
    }
    let bench = Bench {
        pin: info.pin.clone(),
        level: info.level.clone(),
        system: info.system.clone(),
        config: info.config.clone(),
        compiler: info.compiler.clone(),
        host: hostname(),
        date: crate::process::today(),
        clients: plan.clients.max(1),
        scale: plan.scale,
        seconds: plan.seconds,
        scale_factor: plan.scale_factor,
        measures,
    };
    let path = dir.join("bench.json");
    let text = serde_json::to_string_pretty(&bench).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(bench)
}

/// One record per measure, passed when every run of it finished.
#[must_use]
pub fn records(plan: &BenchPlan, bench: &Bench) -> Vec<TestRecord> {
    let artifacts = plan.suite.out.join("bench");
    bench
        .measures
        .iter()
        .map(|m| {
            let (outcome, class) = match m.failed {
                Some(_) => (Outcome::Failed, Some(FailClass::Error)),
                None => (Outcome::Passed, None),
            };
            record(&plan.suite, &m.name, outcome, class, None, &artifacts)
        })
        .collect()
}

/// One line per measure for the terminal.
#[must_use]
pub fn summary(bench: &Bench) -> Vec<String> {
    bench
        .measures
        .iter()
        .map(|m| match (&m.failed, spread(&m.samples)) {
            (Some(why), _) => format!("bench: {} failed: {why}", m.name),
            (None, Some(s)) => format!(
                "bench: {}: {} {} over {} runs",
                m.name,
                shown(&m.unit, s),
                m.unit,
                m.samples.len()
            ),
            (None, None) => format!("bench: {}: no samples", m.name),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bench(compiler: &str, measures: Vec<Measure>) -> Bench {
        Bench {
            pin: "REL_18_6".into(),
            level: "-O2".into(),
            system: "autoconf".into(),
            config: "minimal".into(),
            compiler: compiler.into(),
            host: "server3".into(),
            date: "2026-10-03".into(),
            clients: 8,
            scale: 100,
            seconds: 60,
            scale_factor: 1.0,
            measures,
        }
    }

    fn measure(name: &str, unit: &str, higher: bool, samples: &[f64]) -> Measure {
        Measure {
            samples: samples.to_vec(),
            ..Measure::new(name, unit, higher)
        }
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn the_quartiles_fall_between_samples_as_r_puts_them() {
        let s = spread(&[4.0, 1.0, 3.0, 2.0]).unwrap();
        assert_eq!((s.low, s.median, s.high), (1.75, 2.5, 3.25));
        let one = spread(&[7.0]).unwrap();
        assert_eq!((one.low, one.median, one.high), (7.0, 7.0, 7.0));
        assert_eq!(spread(&[]), None);
        let five = spread(&[10.0, 50.0, 20.0, 40.0, 30.0]).unwrap();
        assert_eq!((five.low, five.median, five.high), (20.0, 30.0, 40.0));
    }

    #[test]
    fn a_difference_inside_the_ranges_is_not_measurable() {
        let a = Spread {
            low: 99.0,
            median: 100.0,
            high: 101.0,
        };
        let near = Spread {
            low: 101.0,
            median: 103.0,
            high: 104.0,
        };
        assert_eq!(verdict(true, a, near), "no measurable difference");
        let far = Spread {
            low: 79.0,
            median: 80.0,
            high: 81.0,
        };
        assert_eq!(verdict(true, a, far), "20.0% slower");
        assert_eq!(verdict(false, a, far), "25.0% faster");
    }

    #[test]
    fn the_report_lines_up_measures_by_name_and_says_which_side_lacks_one() {
        let a = bench(
            "gcc (GCC) 16.2.0",
            vec![
                measure(
                    "pgbench select-only",
                    "tps",
                    true,
                    &[96.0, 100.0, 104.0, 98.0, 102.0],
                ),
                measure("regression suite", "s", false, &[60.0, 61.0, 59.0]),
            ],
        );
        let mut slow = measure("pgbench select-only", "tps", true, &[50.0, 51.0, 49.0]);
        slow.samples.push(50.0);
        let mut broke = measure("regression suite", "s", false, &[]);
        broke.failed = Some("run 1: 3 tests did not pass".into());
        let b = bench(
            "rucc 0.18.11",
            vec![
                slow,
                broke,
                measure("pgbench tpcb-like", "tps", true, &[9.0]),
            ],
        );
        let text = compare(&a, &b);
        assert!(
            text.starts_with(
                "# rucc 0.18.11 -O2 on server3 against gcc (GCC) 16.2.0 -O2 on server3\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "| pgbench select-only (tps) | 100 (98 to 102) | 50 (50 to 50) | 50.0% slower |"
            ),
            "{text}"
        );
        assert!(text.contains("| regression suite (s) | 60.0 (59.5 to 60.5) | failed: run 1: 3 tests did not pass |  |"), "{text}");
        assert!(
            text.contains("| pgbench tpcb-like | | | only in b |"),
            "{text}"
        );
        assert!(!text.contains("differ in more than the compiler"), "{text}");
    }

    #[test]
    fn the_analytic_set_is_summed_up_by_the_geometric_mean_and_a_wrong_answer_is_called_out() {
        let query = |name: &str, answer: &str, samples: &[f64]| Measure {
            answer: Some(answer.to_string()),
            ..measure(&format!("{ANALYTIC}{name}"), "ms", false, samples)
        };
        let a = bench(
            "gcc (GCC) 16.2.0",
            vec![
                query("one", "aa", &[100.0, 100.0, 100.0]),
                query("two", "bb", &[100.0, 100.0, 100.0]),
                query("three", "cc", &[100.0, 100.0, 100.0]),
            ],
        );
        let mut b = bench(
            "rucc 0.18.11",
            vec![
                query("one", "aa", &[200.0, 200.0, 200.0]),
                query("two", "bb", &[50.0, 50.0, 50.0]),
                query("three", "cd", &[100.0, 100.0, 100.0]),
            ],
        );
        let text = compare(&a, &b);
        assert!(text.contains("| analytic one (ms) | 100.0 (100.0 to 100.0) | 200.0 (200.0 to 200.0) | 50.0% slower |"), "{text}");
        assert!(text.contains("| analytic three (ms) | 100.0 (100.0 to 100.0) | 100.0 (100.0 to 100.0) | different answers |"), "{text}");
        assert!(text.contains("Over the 2 analytic queries both runs finished with the same answers, b is 0.0% faster by the geometric mean."), "{text}");
        b.scale_factor = 0.1;
        assert!(compare(&a, &b).contains("differ in more than the compiler"));
    }

    #[test]
    fn a_bench_file_reads_back_as_it_was_written() {
        let mut failed = measure("pgbench tpcb-like", "tps", true, &[1.5]);
        failed.failed = Some("run 2: pgbench exited Some(2)".into());
        let mut answered = measure("analytic top supplier", "ms", false, &[12.5]);
        answered.answer = Some("cbf29ce484222325".into());
        let b = bench(
            "rucc 0.18.11",
            vec![
                failed,
                answered,
                measure("regression suite", "s", false, &[61.25]),
            ],
        );
        let text = serde_json::to_string(&b).unwrap();
        assert!(text.contains("\"higher-is-better\":true"), "{text}");
        assert_eq!(serde_json::from_str::<Bench>(&text).unwrap(), b);
    }
}
