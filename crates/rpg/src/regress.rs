//! Reading what the suites say.
//!
//! Two formats. `pg_regress` prints one TAP line per test, which is the same under make and under
//! meson and is also written to `regression.out` in its output directory. Since Postgres 16 the
//! line is `ok N - name  123 ms` for a test run on its own and `ok N + name  123 ms` for one run in
//! a parallel group, with `not ok` for a failure (see `test_status_print` in
//! `src/test/regress/pg_regress.c`). Meson, above that, writes `meson-logs/testlog.json` with one
//! JSON object per meson test, which is where a timeout or a test that never started shows up.

use serde::Deserialize;
use std::path::Path;

/// One `pg_regress` test result.
#[derive(Debug, Clone, PartialEq)]
pub struct RegressResult {
    /// The test number `pg_regress` gave it.
    pub number: u32,
    /// The test name, which is the name of its `.sql` file.
    pub name: String,
    /// Whether it matched an expected output.
    pub ok: bool,
    /// Whether it ran in a parallel group.
    pub parallel: bool,
    /// Its run time, from `pg_regress`'s own clock.
    pub seconds: f64,
}

/// What a `pg_regress` run printed, parsed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegressOutput {
    /// Every test line, in order.
    pub results: Vec<RegressResult>,
    /// The TAP plan, `1..N`, when `pg_regress` got as far as printing it.
    pub planned: Option<u32>,
    /// The reason, when `pg_regress` bailed out.
    pub bailed: Option<String>,
}

impl RegressOutput {
    /// How many passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.ok).count()
    }

    /// How many failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| !r.ok).count()
    }
}

/// Parse `pg_regress` output.
///
/// Anything that is not a test line, the plan or a bail out is ignored, which covers the `#`
/// comment lines and whatever meson or make wrapped around the output.
#[must_use]
pub fn parse_regress(text: &str) -> RegressOutput {
    let mut out = RegressOutput::default();
    let mut last_comment = None;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("# ") {
            last_comment = Some(rest.to_string());
        }
        if let Some(plan) = line.strip_prefix("1..") {
            out.planned = plan.trim().parse().ok();
            continue;
        }
        if line.starts_with("Bail out!") {
            let said = line.trim_start_matches("Bail out!").trim();
            out.bailed = Some(if said.is_empty() {
                last_comment.clone().unwrap_or_default()
            } else {
                said.to_string()
            });
            continue;
        }
        if let Some(result) = parse_line(line) {
            out.results.push(result);
        }
    }
    out
}

/// One `pg_regress` run out of several in one log.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// The directory it ran in, relative to the top of the build tree, `contrib/amcheck`.
    pub subdir: String,
    /// What kind of run the makefile said it was, `regress`, `isolation` or `tap`.
    pub kind: String,
    /// What it printed.
    pub output: RegressOutput,
}

/// Split the log of a `make check` that ran `pg_regress` many times, as it does under `contrib`
/// and `src/test/modules`, and parse each run.
///
/// Postgres's makefiles print `# +++ regress check in contrib/amcheck +++` ahead of each run, and
/// `isolation` or `tap` in place of `regress` for the other kinds (see `pg_regress_check` in
/// `src/Makefile.global.in`). Each run numbers its tests from 1 and prints its own plan, so the
/// lines between two markers are one run. Anything before the first marker is make talking. A
/// `tap` run is prove's output and is read with [`parse_prove`].
///
/// One run in `check-world` prints no marker: ecpg's makefile calls `pg_regress` itself. Its
/// first test line, numbered 1, comes after another run's tests, after the end of a prove run or
/// before any marker, and it is
/// taken as a regress run in the directory make last said it entered, relative to `root`, the
/// top of the build tree.
#[must_use]
pub fn parse_sections(text: &str, root: &str) -> Vec<Section> {
    let mut sections: Vec<(String, String, String)> = Vec::new();
    let mut entered = String::new();
    for line in text.lines() {
        if let Some(dir) = entering(line) {
            entered = dir
                .strip_prefix(root)
                .map_or(dir, |d| d.trim_start_matches('/'))
                .to_string();
        }
        if let Some((kind, subdir)) = marker(line) {
            sections.push((subdir.to_string(), kind.to_string(), String::new()));
            continue;
        }
        let unmarked = parse_line(line.trim_end()).is_some_and(|r| r.number == 1)
            && sections.last().is_none_or(|(_, kind, body)| {
                if kind == "tap" {
                    body.contains("\nResult: ")
                } else {
                    body.lines().any(|l| parse_line(l).is_some())
                }
            });
        if unmarked {
            sections.push((entered.clone(), "regress".to_string(), String::new()));
        }
        if let Some((.., body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections
        .into_iter()
        .map(|(subdir, kind, body)| {
            let output = if kind == "tap" {
                parse_prove(&body)
            } else {
                parse_regress(&body)
            };
            Section {
                subdir,
                kind,
                output,
            }
        })
        .collect()
}

/// The directory of a `make[2]: Entering directory '/b/src/interfaces/ecpg/test'` line.
fn entering(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once(": Entering directory ")?;
    let rest = rest.trim();
    let quoted = rest
        .strip_prefix('\'')
        .or_else(|| rest.strip_prefix('`'))?
        .strip_suffix('\'')?;
    Some(quoted)
}

/// Parse what prove printed for one directory's TAP scripts, as one result per script.
///
/// prove prints a line per script, `t/001_initdb.pl .......... ok`, with the time after `ok` when
/// it runs with `--timer`, and the clock in front. A script that fails ends its line, or a later
/// line that repeats its name, with `Dubious` or `Failed N/M subtests`, and is listed again under
/// `Test Summary Report` with its wait status. `Files=N` is the number of scripts, which serves as
/// the plan, and `Result:` is the last thing prove prints, so a run without it did not finish.
#[must_use]
pub fn parse_prove(text: &str) -> RegressOutput {
    let mut out = RegressOutput::default();
    let mut finished = false;
    let mut summary = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with("Test Summary Report") {
            summary = true;
            continue;
        }
        if let Some(files) = line.strip_prefix("Files=") {
            out.planned = files.split(',').next().and_then(|n| n.trim().parse().ok());
            continue;
        }
        if line.starts_with("Result: ") {
            finished = true;
            continue;
        }
        if summary {
            if let Some((name, status)) = line.split_once(" (Wstat: ")
                && summary_failed(status)
            {
                script(&mut out, name.trim()).ok = false;
            }
            continue;
        }
        let Some((name, verdict)) = prove_line(line) else {
            continue;
        };
        let result = script(&mut out, name);
        if let Some(rest) = verdict.strip_prefix("ok") {
            result.ok = true;
            let mut words = rest.split_whitespace();
            if let (Some(n), Some("ms")) = (words.next(), words.next()) {
                result.seconds = n.parse::<f64>().unwrap_or(0.0) / 1000.0;
            }
        } else if verdict.starts_with("skipped") {
            result.ok = true;
        }
    }
    if !finished {
        out.bailed = Some("prove did not finish".to_string());
    }
    out
}

/// Whether a summary line's `0 Tests: 469 Failed: 0)` says the script failed. prove also lists a
/// script whose TODO tests passed, with a zero wait status and nothing failed, which is a pass.
fn summary_failed(status: &str) -> bool {
    let mut words = status.split_whitespace();
    let wstat = words.next().unwrap_or_default();
    let failed = status
        .split_once("Failed: ")
        .map_or("0", |(_, n)| n.trim_end_matches(')').trim());
    wstat != "0" || failed != "0"
}

/// The result for a script, added when prove has not named it before.
fn script<'a>(out: &'a mut RegressOutput, name: &str) -> &'a mut RegressResult {
    let index = out.results.iter().position(|r| r.name == name);
    let index = index.unwrap_or_else(|| {
        out.results.push(RegressResult {
            number: u32::try_from(out.results.len() + 1).unwrap_or(u32::MAX),
            name: name.to_string(),
            ok: false,
            parallel: false,
            seconds: 0.0,
        });
        out.results.len() - 1
    });
    &mut out.results[index]
}

/// The script and what follows its dots in a `[04:50:33] t/001_initdb.pl ..... ok` line.
fn prove_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let line = if let Some(clock) = line.strip_prefix('[') {
        clock.split_once("] ")?.1
    } else {
        line
    };
    let (script, rest) = line.split_once(' ')?;
    let is_perl = Path::new(script)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pl"));
    if !script.starts_with("t/") || !is_perl {
        return None;
    }
    let verdict = rest.trim_start_matches('.');
    if verdict.len() == rest.len() {
        return None;
    }
    Some((script, verdict.trim()))
}

/// The kind and the directory of a `# +++ regress check in contrib/amcheck +++` line.
fn marker(line: &str) -> Option<(&str, &str)> {
    let inner = line.trim().strip_prefix("# +++ ")?.strip_suffix(" +++")?;
    let (kind, subdir) = inner.split_once(" check in ")?;
    Some((kind, subdir))
}

/// One `ok` or `not ok` line.
fn parse_line(line: &str) -> Option<RegressResult> {
    let (ok, rest) = if let Some(rest) = line.strip_prefix("not ok ") {
        (false, rest)
    } else {
        (true, line.strip_prefix("ok ")?)
    };
    let mut words = rest.split_whitespace();
    let number = words.next()?.parse().ok()?;
    let parallel = match words.next()? {
        "+" => true,
        "-" => false,
        _ => return None,
    };
    let name = words.next()?.to_string();
    let millis: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0.0);
    Some(RegressResult {
        number,
        name,
        ok,
        parallel,
        seconds: millis / 1000.0,
    })
}

/// One line of meson's `testlog.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MesonTest {
    /// `postgresql:regress / regress/regress` and the like, or `regress - postgresql:regress/regress`
    /// from the meson MSYS2 ships, which has no ` / ` in it.
    pub name: String,
    /// `OK`, `FAIL`, `SKIP`, `TIMEOUT`, `ERROR`, `EXPECTEDFAIL` or `UNEXPECTEDPASS`.
    pub result: String,
    /// Seconds.
    #[serde(default)]
    pub duration: f64,
    /// The exit code.
    #[serde(default)]
    pub returncode: Option<i64>,
    /// The suites meson put the test in.
    #[serde(default)]
    pub suite: Vec<String>,
    /// What the test printed, when meson kept it.
    #[serde(default)]
    pub stdout: Option<String>,
    /// Whether meson ran it alongside others.
    #[serde(default)]
    pub is_parallel: bool,
}

impl MesonTest {
    /// The part of the name after the suite, or after the project when there is no ` / `,
    /// `regress/regress`.
    #[must_use]
    pub fn short_name(&self) -> &str {
        let name = self.name.as_str();
        match name.rsplit_once(" / ") {
            Some((_, test)) => test,
            None => name.split_once(':').map_or(name, |(_, test)| test),
        }
    }

    /// Whether meson counted it as a success.
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(self.result.as_str(), "OK" | "EXPECTEDFAIL" | "SKIP")
    }
}

/// Parse `testlog.json`, skipping lines that are not test objects.
#[must_use]
pub fn parse_testlog(text: &str) -> Vec<MesonTest> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Read a meson run of every suite, `world`, as one output.
///
/// A meson test that runs `pg_regress`, whose name ends in `regress`, `isolation` or `ecpg`, gives
/// one result per `pg_regress` test, named after the meson test, `amcheck/regress/check_btree`,
/// and one more, named for the meson test alone, when meson counted it a failure and no test in it
/// failed. Every other meson test is a TAP script and gives one result, `initdb/001_initdb`. The
/// plan is the number of results, since meson ran each test to the end or timed it out.
#[must_use]
pub fn world_from_testlog(tests: &[MesonTest]) -> RegressOutput {
    let mut out = RegressOutput::default();
    for test in tests {
        if test.suite.iter().any(|s| s.ends_with(":setup")) {
            continue;
        }
        let short = test.short_name();
        let kind = short.rsplit('/').next().unwrap_or(short);
        if matches!(kind, "regress" | "isolation" | "ecpg") {
            let inner = parse_regress(test.stdout.as_deref().unwrap_or_default());
            let failed_inside = inner.failed() > 0;
            for mut result in inner.results {
                result.name = format!("{short}/{}", result.name);
                result.number = u32::try_from(out.results.len() + 1).unwrap_or(u32::MAX);
                out.results.push(result);
            }
            if test.ok() || failed_inside {
                continue;
            }
        }
        out.results.push(RegressResult {
            number: u32::try_from(out.results.len() + 1).unwrap_or(u32::MAX),
            name: short.to_string(),
            ok: test.ok(),
            parallel: test.is_parallel,
            seconds: test.duration,
        });
    }
    out.planned = u32::try_from(out.results.len()).ok();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGRESS: &str = "\
# initializing database system by copying initdb template
# using temp instance on port 65312 with PID 2004
ok 1         - test_setup                                348 ms
# parallel group (20 tests):  varchar char name int2 boolean
ok 2         + boolean                                    62 ms
not ok 3     + char                                       44 ms
ok 4         + name                                       51 ms
# parallel group (2 tests):  select_into select_distinct
ok 5         + select_into                              1207 ms
1..5
# 1 of 5 tests failed.
# The differences that caused some tests to fail can be viewed in the file \"/b/regression.diffs\".
";

    #[test]
    fn pg_regress_lines_are_read_with_their_times() {
        let out = parse_regress(REGRESS);
        assert_eq!(out.results.len(), 5);
        assert_eq!(out.planned, Some(5));
        assert_eq!(out.passed(), 4);
        assert_eq!(out.failed(), 1);
        let first = &out.results[0];
        assert_eq!(first.name, "test_setup");
        assert!(!first.parallel);
        assert!((first.seconds - 0.348).abs() < 1e-9);
        let char_test = &out.results[2];
        assert_eq!(char_test.name, "char");
        assert!(!char_test.ok);
        assert!(char_test.parallel);
        assert!(out.bailed.is_none());
    }

    #[test]
    fn a_bail_out_is_recorded_with_its_reason() {
        let text = "ok 1         - test_setup   348 ms\n# could not start postmaster\nBail out!";
        let out = parse_regress(text);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.bailed.as_deref(), Some("could not start postmaster"));
        assert!(out.planned.is_none());
    }

    #[test]
    fn lines_that_only_look_like_results_are_not_results() {
        let out =
            parse_regress("ok\nok 1 x name 3 ms\nok one - name 3 ms\nsomething ok 1 - x 1 ms\n");
        assert!(out.results.is_empty());
    }

    const SECTIONS: &str = "\
make -C amcheck check
make[1]: Entering directory '/b/contrib/amcheck'
ok 99 - not a result, since no run has started
# +++ regress check in contrib/amcheck +++
# using temp instance on port 65312 with PID 2004
ok 1         - check                                     120 ms
ok 2         - check_btree                              2210 ms
1..2
# All 2 tests passed.
# +++ isolation check in contrib/amcheck +++
not ok 1     - read-write-unique                         900 ms
1..1
make[1]: Leaving directory '/b/contrib/amcheck'
# +++ regress check in contrib/bloom +++
# could not start postmaster
Bail out!
";

    #[test]
    fn a_log_of_many_runs_is_split_at_the_markers() {
        let sections = parse_sections(SECTIONS, "/b");
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].subdir, "contrib/amcheck");
        assert_eq!(sections[0].kind, "regress");
        assert_eq!(sections[0].output.passed(), 2);
        assert_eq!(sections[0].output.planned, Some(2));
        assert_eq!(sections[1].kind, "isolation");
        assert_eq!(sections[1].output.failed(), 1);
        assert_eq!(sections[1].output.results[0].name, "read-write-unique");
        assert_eq!(sections[2].subdir, "contrib/bloom");
        assert!(sections[2].output.results.is_empty());
        assert_eq!(
            sections[2].output.bailed.as_deref(),
            Some("could not start postmaster")
        );
    }

    #[test]
    fn a_line_that_only_looks_like_a_marker_is_not_one() {
        assert!(marker("# +++ regress check in contrib/amcheck").is_none());
        assert!(marker("# +++ regress in contrib/amcheck +++").is_none());
        assert_eq!(
            marker("  # +++ tap check in src/test/modules/test_misc +++"),
            Some(("tap", "src/test/modules/test_misc"))
        );
    }

    const WORLD: &str = "\
make[2]: Entering directory '/b/src/bin/initdb'
# +++ tap check in src/bin/initdb +++
[04:50:33] t/001_initdb.pl .......... ok     3120 ms ( 0.01 usr  0.00 sys +  1.20 cusr  0.90 csys =  2.11 CPU)
[04:50:36] t/002_skipped.pl ......... skipped: no locale
[04:50:36] t/003_broken.pl .......... 1/?
#   Failed test 'the cluster starts'
#   at t/003_broken.pl line 20.
# Looks like you failed 1 test of 4.
[04:50:40] t/003_broken.pl .......... Dubious, test returned 1 (wstat 256, 0x100)
Failed 1/4 subtests
[04:50:40]

Test Summary Report
t/001_initdb.pl (Wstat: 0 Tests: 50 Failed: 0)
  TODO passed:   4-6
t/003_broken.pl (Wstat: 256 (exited 1) Tests: 4 Failed: 1)
  Failed test:  2
  Non-zero exit status: 1
Files=3, Tests=54,  7 wallclock secs ( 0.02 usr  0.01 sys +  2.40 cusr  1.80 csys =  4.23 CPU)
Result: FAIL
make[2]: Leaving directory '/b/src/bin/initdb'
make[3]: Entering directory '/b/src/interfaces/ecpg/test'
ok 1         - connect/test1                              91 ms
not ok 2     - sql/twophase                               40 ms
1..2
make[3]: Leaving directory '/b/src/interfaces/ecpg/test'
make[2]: Entering directory '/b/src/bin/pg_ctl'
# +++ tap check in src/bin/pg_ctl +++
[04:51:02] t/001_start_stop.pl ...... ok     2210 ms ( 0.01 usr  0.00 sys +  1.20 cusr  0.90 csys =  2.11 CPU)
";

    #[test]
    fn prove_output_gives_one_result_per_script() {
        let sections = parse_sections(WORLD, "/b");
        assert_eq!(sections.len(), 3);
        let initdb = &sections[0];
        assert_eq!(initdb.kind, "tap");
        assert_eq!(initdb.subdir, "src/bin/initdb");
        let names: Vec<&str> = initdb
            .output
            .results
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["t/001_initdb.pl", "t/002_skipped.pl", "t/003_broken.pl"]
        );
        assert!(initdb.output.results[0].ok);
        assert!((initdb.output.results[0].seconds - 3.12).abs() < 1e-9);
        assert!(initdb.output.results[1].ok);
        assert!(!initdb.output.results[2].ok);
        assert_eq!(initdb.output.planned, Some(3));
        assert!(initdb.output.bailed.is_none());
    }

    #[test]
    fn a_run_without_a_marker_is_named_for_the_directory_make_entered() {
        let sections = parse_sections(WORLD, "/b");
        let ecpg = &sections[1];
        assert_eq!(ecpg.subdir, "src/interfaces/ecpg/test");
        assert_eq!(ecpg.kind, "regress");
        assert_eq!(ecpg.output.passed(), 1);
        assert_eq!(ecpg.output.failed(), 1);
        assert_eq!(ecpg.output.planned, Some(2));
    }

    #[test]
    fn a_prove_run_that_never_reached_its_result_did_not_finish() {
        let sections = parse_sections(WORLD, "/b");
        let pg_ctl = &sections[2];
        assert_eq!(pg_ctl.output.passed(), 1);
        assert_eq!(
            pg_ctl.output.bailed.as_deref(),
            Some("prove did not finish")
        );
    }

    #[test]
    fn a_script_only_in_the_summary_still_counts_as_failed() {
        let out = parse_prove(
            "Test Summary Report\nt/009_x.pl (Wstat: 9 Tests: 0 Failed: 0)\nFiles=1, Tests=0\nResult: FAIL\n",
        );
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].name, "t/009_x.pl");
        assert!(!out.results[0].ok);
    }

    #[test]
    fn a_summary_line_fails_a_script_only_when_its_status_says_so() {
        assert!(!summary_failed("0 Tests: 469 Failed: 0)"));
        assert!(summary_failed("256 (exited 1) Tests: 4 Failed: 1)"));
        assert!(summary_failed("9 Tests: 0 Failed: 0)"));
    }

    #[test]
    fn lines_that_only_look_like_scripts_are_not_scripts() {
        assert!(prove_line("t/001_x.pl").is_none());
        assert!(prove_line("t/001_x.pm ... ok").is_none());
        assert!(prove_line("# t/001_x.pl ... ok").is_none());
        assert_eq!(prove_line("t/001_x.pl .. ok"), Some(("t/001_x.pl", "ok")));
        assert_eq!(
            entering("make[3]: Entering directory '/b/src'"),
            Some("/b/src")
        );
    }

    #[test]
    fn a_meson_world_run_gives_regress_tests_and_tap_scripts() {
        let log = r##"{"name": "postgresql:setup / tmp_install", "result": "OK", "duration": 3.5, "suite": ["postgresql:setup"]}
{"name": "postgresql:regress / regress/regress", "stdout": "ok 1         - test_setup   348 ms\nnot ok 2     + boolean   62 ms\n1..2\n", "result": "FAIL", "duration": 71.25, "suite": ["postgresql:regress"], "is_parallel": true}
{"name": "postgresql:initdb / initdb/001_initdb", "result": "OK", "duration": 4.0, "suite": ["postgresql:initdb"], "is_parallel": true}
{"name": "postgresql:amcheck / amcheck/regress", "stdout": "# could not start postmaster\nBail out!\n", "result": "FAIL", "duration": 2.0, "suite": ["postgresql:amcheck"]}
{"name": "postgresql:recovery / recovery/027_stream_regress", "result": "TIMEOUT", "duration": 1000.0, "suite": ["postgresql:recovery"]}
{"name": "postgresql:psql / psql/010_tab_completion", "result": "SKIP", "duration": 0.1, "suite": ["postgresql:psql"]}
"##;
        let out = world_from_testlog(&parse_testlog(log));
        let names: Vec<(&str, bool)> = out
            .results
            .iter()
            .map(|r| (r.name.as_str(), r.ok))
            .collect();
        assert_eq!(
            names,
            [
                ("regress/regress/test_setup", true),
                ("regress/regress/boolean", false),
                ("initdb/001_initdb", true),
                ("amcheck/regress", false),
                ("recovery/027_stream_regress", false),
                ("psql/010_tab_completion", true),
            ]
        );
        assert_eq!(out.planned, Some(6));
        assert!((out.results[2].seconds - 4.0).abs() < 1e-9);
    }

    const TESTLOG: &str = r#"{"name": "postgresql:setup / tmp_install", "stdout": "", "result": "OK", "starttime": 1790000000.0, "duration": 3.5, "returncode": 0, "env": {}, "command": ["meson", "install"], "suite": ["postgresql:setup"], "is_parallel": false}
{"name": "postgresql:regress / regress/regress", "stdout": "ok 1         - test_setup   348 ms\n1..1\n", "result": "OK", "starttime": 1790000004.0, "duration": 71.25, "returncode": 0, "env": {}, "command": ["perl"], "suite": ["postgresql:regress"], "is_parallel": true}
{"name": "postgresql:isolation / isolation/isolation", "result": "TIMEOUT", "duration": 1000.0, "returncode": -15, "suite": ["postgresql:isolation"]}
"#;

    #[test]
    fn meson_testlog_lines_are_read() {
        let tests = parse_testlog(TESTLOG);
        assert_eq!(tests.len(), 3);
        assert_eq!(tests[1].short_name(), "regress/regress");
        assert!(tests[1].ok());
        assert!((tests[1].duration - 71.25).abs() < 1e-9);
        let inner = parse_regress(tests[1].stdout.as_deref().unwrap());
        assert_eq!(inner.results[0].name, "test_setup");
        assert_eq!(tests[2].result, "TIMEOUT");
        assert!(!tests[2].ok());
        let line = r#"{"name": "regress - postgresql:regress/regress", "result": "OK"}"#;
        assert_eq!(parse_testlog(line)[0].short_name(), "regress/regress");
        assert!(tests[2].stdout.is_none());
    }
}
