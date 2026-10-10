//! `rpg build-compare`: how long two builds of the same tree took and how much memory they needed.
//!
//! The measures are the ones the compile time targets are written in: the wall time of the whole
//! build, the compiler's user time summed over every call, the largest resident set of any one
//! call, and the time of one translation unit on its own, by default `gram.c`, the largest in the
//! tree. All of it comes from `build.json` and `compile.jsonl`, so it costs nothing to run after
//! the builds a bench or a nightly makes anyway.

use crate::build::{BuildInfo, lexical};
use rpg_shim::record::{CompileRecord, read_log};
use std::fmt::Write as _;
use std::path::Path;

/// The translation unit timed on its own when no other is named.
pub const GRAM: &str = "src/backend/parser/gram.c";

/// One build, with the compile of the file timed on its own.
#[derive(Debug, Clone)]
pub struct Timed {
    /// `build.json`.
    pub info: BuildInfo,
    /// The user and wall seconds of that compile, when the trace has one.
    pub file: Option<(f64, f64)>,
}

impl Timed {
    /// Read a build directory's `build.json` and find `file` in its `compile.jsonl`.
    pub fn load(out: &Path, file: &str) -> Result<Self, String> {
        let info = BuildInfo::load(out)?;
        let path = out.join("compile.jsonl");
        let (records, _) =
            read_log(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        Ok(Self {
            info,
            file: timed(&records, file),
        })
    }
}

/// The user and wall seconds of the compile whose C input is `file`, the slowest when several are.
///
/// The input is resolved against the call's directory, so the file is found whether the build
/// named it in the source tree or, as for a generated file like `gram.c`, in the build tree. A
/// call the shim could not time has its wall seconds stand in for its user seconds.
fn timed(records: &[CompileRecord], file: &str) -> Option<(f64, f64)> {
    records
        .iter()
        .filter(|record| {
            record
                .inputs
                .iter()
                .any(|d| lexical(&Path::new(&record.cwd).join(&d.path)).ends_with(file))
        })
        .map(|record| {
            (
                record.user_seconds.unwrap_or(record.wall_seconds),
                record.wall_seconds,
            )
        })
        .max_by(|x, y| x.0.total_cmp(&y.0))
}

/// How fast b is when a takes `a` seconds and b takes `b`.
fn faster(a: f64, b: f64) -> String {
    if a <= 0.0 || b <= 0.0 {
        String::new()
    } else if a >= b {
        format!("{:.2} times as fast", a / b)
    } else {
        format!("{:.2} times as slow", b / a)
    }
}

/// A resident set in KiB, as MiB.
fn mib(kb: u64) -> f64 {
    u32::try_from(kb).map_or(f64::INFINITY, f64::from) / 1024.0
}

/// The rows for the compiler's user time: the whole of it, then what compiling and linking took.
fn user_rows(text: &mut String, a: &BuildInfo, b: &BuildInfo) {
    // A build recorded before the split was kept has no compile or link seconds, so those rows
    // are only written when both sides have them.
    let split =
        |x: &BuildInfo| x.compiles.compile_user_seconds + x.compiles.link_user_seconds > 0.0;
    let users: [(&str, fn(&BuildInfo) -> f64); 3] = [
        ("compiler user time", |x| x.compiles.build_user_seconds),
        ("of which compiling to objects", |x| {
            x.compiles.compile_user_seconds
        }),
        ("of which linking", |x| x.compiles.link_user_seconds),
    ];
    for (i, (name, user)) in users.into_iter().enumerate() {
        if i > 0 && !(split(a) && split(b)) {
            break;
        }
        let (user_a, user_b) = (user(a), user(b));
        let _ = writeln!(
            text,
            "| {name} | {user_a:.1} s | {user_b:.1} s | {} |",
            faster(user_a, user_b)
        );
    }
    let link = |x: &BuildInfo| {
        x.compiles.slowest_link.as_ref().map_or_else(
            || "none".to_string(),
            |(file, user)| format!("{user:.2} s for `{file}`"),
        )
    };
    if a.compiles.slowest_link.is_some() || b.compiles.slowest_link.is_some() {
        let _ = writeln!(
            text,
            "| slowest link, user time | {} | {} | {} |",
            link(a),
            link(b),
            match (&a.compiles.slowest_link, &b.compiles.slowest_link) {
                (Some((_, x)), Some((_, y))) => faster(*x, *y),
                _ => String::new(),
            }
        );
    }
}

/// The comparison of two builds as a Markdown report.
#[must_use]
pub fn compare(a: &Timed, b: &Timed, file: &str) -> String {
    let setup = |x: &BuildInfo| {
        format!(
            "{} {} {} at {} with {} jobs on {}",
            x.pin, x.config, x.system, x.level, x.jobs, x.host
        )
    };
    let mut text = format!(
        "# {} against {}\n\na is {}, b is {}, both building {}.\n",
        b.info.compiler,
        a.info.compiler,
        a.info.compiler,
        b.info.compiler,
        setup(&a.info)
    );
    if setup(&a.info) != setup(&b.info) {
        let _ = write!(
            text,
            "\nThe two builds differ in more than the compiler: b is {}.\n",
            setup(&b.info)
        );
    }
    text.push_str("\n| measure | a | b | b against a |\n|---|---|---|---|\n");
    let seconds =
        |s: Option<f64>| s.map_or_else(|| "did not finish".to_string(), |s| format!("{s:.1} s"));
    let both = |x: Option<f64>, y: Option<f64>| match (x, y) {
        (Some(x), Some(y)) => faster(x, y),
        _ => String::new(),
    };
    let (wall_a, wall_b) = (a.info.build_seconds, b.info.build_seconds);
    let _ = writeln!(
        text,
        "| build wall time | {} | {} | {} |",
        seconds(wall_a),
        seconds(wall_b),
        both(wall_a, wall_b)
    );
    user_rows(&mut text, &a.info, &b.info);
    let one = |x: Option<(f64, f64)>| {
        x.map_or_else(
            || "not compiled".to_string(),
            |(user, wall)| format!("{user:.2} s ({wall:.2} s wall)"),
        )
    };
    let _ = writeln!(
        text,
        "| `{file}` user time | {} | {} | {} |",
        one(a.file),
        one(b.file),
        both(a.file.map(|x| x.0), b.file.map(|x| x.0))
    );
    let largest = |x: &BuildInfo| {
        let rss = mib(x.compiles.peak_rss_kb);
        match &x.compiles.peak_rss_file {
            Some(file) => format!("{rss:.0} MiB in `{file}`"),
            None => format!("{rss:.0} MiB"),
        }
    };
    let (rss_a, rss_b) = (
        mib(a.info.compiles.peak_rss_kb),
        mib(b.info.compiles.peak_rss_kb),
    );
    let ratio = if rss_a > 0.0 {
        format!("{:.2} times as much", rss_b / rss_a)
    } else {
        String::new()
    };
    let _ = writeln!(
        text,
        "| largest compiler process | {} | {} | {ratio} |",
        largest(&a.info),
        largest(&b.info)
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpg_shim::record::parse_log;

    const LOG: &str = r#"{"started":1.0,"argv":["cc","-c","conftest.c"],"compiler":"/usr/bin/gcc","cwd":"/w/b/build","inputs":[{"path":"conftest.c","sha256":"a"}],"wall-seconds":0.1,"exit":0}
{"started":4.0,"argv":["cc","-O2","-c","gram.c","-o","gram.o"],"compiler":"/usr/bin/gcc","cwd":"/w/b/build/src/backend/parser","inputs":[{"path":"gram.c","sha256":"d"}],"wall-seconds":3.5,"user-seconds":3.25,"exit":0}
{"started":5.0,"argv":["cc","-O2","-c","../../../../src/REL_18_6/src/pl/plpgsql/src/pl_gram.c"],"compiler":"/usr/bin/gcc","cwd":"/w/b/build/src/pl/plpgsql/src","inputs":[{"path":"pl_gram.c","sha256":"e"}],"wall-seconds":9.0,"exit":0}
"#;

    fn info(compiler: &str, build: Option<f64>, user: f64, peak: u64, jobs: usize) -> BuildInfo {
        let mut info: BuildInfo = serde_json::from_value(serde_json::json!({
            "pin": "REL_18_6", "commit": "c", "config": "minimal", "system": "autoconf",
            "level": "-O2", "cc": "/usr/bin/cc", "compiler": compiler, "kind": "gcc",
            "rucc-trace": false, "twice": false, "host": "h", "date": "d", "jobs": jobs,
            "source": "/s", "build-dir": "/b", "phase": "built", "configure-seconds": 9.0,
            "compiles": {
                "probe-calls": 0, "probe-failures": 0, "build-calls": 1, "build-compiles": 1,
                "build-failures": 0, "unreadable-lines": 0, "build-user-seconds": user,
                "peak-rss-kb": peak, "peak-rss-file": "src/backend/parser/gram.c"
            }
        }))
        .unwrap();
        info.build_seconds = build;
        info
    }

    #[test]
    fn the_file_is_found_by_its_place_in_the_tree_and_not_by_its_name_alone() {
        let (records, _) = parse_log(LOG);
        assert_eq!(timed(&records, GRAM), Some((3.25, 3.5)));
        assert_eq!(timed(&records, "src/backend/parser/scan.c"), None);
        // The call with no user time has its wall time stand in.
        assert_eq!(
            timed(&records, "src/pl/plpgsql/src/pl_gram.c"),
            Some((9.0, 9.0))
        );
    }

    #[test]
    fn the_report_says_how_many_times_as_fast_and_as_much() {
        let mut a = Timed {
            info: info("gcc 13.3.0", Some(84.5), 262.1, 207_872, 4),
            file: Some((3.0, 3.25)),
        };
        let mut b = Timed {
            info: info("rucc 0.29.3", Some(34.2), 95.1, 153_600, 4),
            file: Some((6.0, 6.5)),
        };
        for (x, compile, link, slowest) in [(&mut a, 200.0, 50.0, 4.0), (&mut b, 40.0, 50.0, 8.0)] {
            x.info.compiles.compile_user_seconds = compile;
            x.info.compiles.link_user_seconds = link;
            x.info.compiles.slowest_link = Some(("src/backend/postgres".to_string(), slowest));
        }
        let text = compare(&a, &b, GRAM);
        assert!(
            text.starts_with("# rucc 0.29.3 against gcc 13.3.0\n"),
            "{text}"
        );
        assert!(!text.contains("differ in more"), "{text}");
        assert!(
            text.contains("| build wall time | 84.5 s | 34.2 s | 2.47 times as fast |"),
            "{text}"
        );
        assert!(text.contains("| 2.76 times as fast |"), "{text}");
        assert!(
            text.contains("| 3.00 s (3.25 s wall) | 6.00 s (6.50 s wall) | 2.00 times as slow |"),
            "{text}"
        );
        assert!(
            text.contains("| 203 MiB in `src/backend/parser/gram.c` | 150 MiB in"),
            "{text}"
        );
        assert!(text.contains("| 0.74 times as much |"), "{text}");
        assert!(
            text.contains(
                "| of which compiling to objects | 200.0 s | 40.0 s | 5.00 times as fast |"
            ),
            "{text}"
        );
        assert!(
            text.contains("| of which linking | 50.0 s | 50.0 s | 1.00 times as fast |"),
            "{text}"
        );
        assert!(
            text.contains(
                "| slowest link, user time | 4.00 s for `src/backend/postgres` | \
                 8.00 s for `src/backend/postgres` | 2.00 times as slow |"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_build_that_did_not_finish_or_ran_differently_is_said_to() {
        let a = Timed {
            info: info("gcc", Some(80.0), 200.0, 1024, 4),
            file: None,
        };
        let b = Timed {
            info: info("rucc", None, 50.0, 1024, 8),
            file: Some((1.0, 1.0)),
        };
        let text = compare(&a, &b, GRAM);
        assert!(
            text.contains("b is REL_18_6 minimal autoconf at -O2 with 8 jobs on h"),
            "{text}"
        );
        assert!(
            text.contains("| build wall time | 80.0 s | did not finish |  |"),
            "{text}"
        );
        assert!(
            text.contains("| not compiled | 1.00 s (1.00 s wall) |  |"),
            "{text}"
        );
        // Neither build says how its user time split, as one recorded before the split would not.
        assert!(!text.contains("of which"), "{text}");
        assert!(!text.contains("slowest link"), "{text}");
    }
}
