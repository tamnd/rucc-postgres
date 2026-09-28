//! Reading what the suites say.
//!
//! Two formats. `pg_regress` prints one TAP line per test, which is the same under make and under
//! meson and is also written to `regression.out` in its output directory. Since Postgres 16 the
//! line is `ok N - name  123 ms` for a test run on its own and `ok N + name  123 ms` for one run in
//! a parallel group, with `not ok` for a failure (see `test_status_print` in
//! `src/test/regress/pg_regress.c`). Meson, above that, writes `meson-logs/testlog.json` with one
//! JSON object per meson test, which is where a timeout or a test that never started shows up.

use serde::Deserialize;

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
    /// `postgresql:regress / regress/regress` and the like.
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
}

impl MesonTest {
    /// The part of the name after the suite, `regress/regress`.
    #[must_use]
    pub fn short_name(&self) -> &str {
        self.name.rsplit(" / ").next().unwrap_or(&self.name)
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
        assert!(tests[2].stdout.is_none());
    }
}
