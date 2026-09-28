//! One line of `compile.jsonl`: everything the shim saw about one call to the compiler.
//!
//! Field names are kebab case, the same convention as `records.jsonl` in rucc-real-corpus, so that
//! a reader moving between the two files does not have to switch spelling. Optional fields are left
//! out when empty rather than written as null, which keeps the ten thousand probe lines of a
//! configure run short enough to read with `less`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

/// A file and its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    /// The path as the command line spelled it, relative to `cwd` when it was relative there.
    pub path: String,
    /// Lower case hex SHA-256, or empty when the file could not be read.
    pub sha256: String,
}

/// What happened when the compile was repeated under `RPG_TWICE=1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Twice {
    /// Whether every output came out byte for byte the same the second time.
    pub identical: bool,
    /// The outputs that did not, when some did not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differing: Vec<String>,
}

/// One compiler invocation.
///
/// `PartialEq` and not `Eq`, because times are floats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CompileRecord {
    /// Seconds since the Unix epoch when the call started.
    pub started: f64,
    /// The command line as the build system wrote it, including the name it called us by.
    pub argv: Vec<String>,
    /// The compiler that actually ran.
    pub compiler: String,
    /// Arguments the shim added, which is `-frucc-trace=...` or nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// The working directory.
    pub cwd: String,
    /// The environment variables that can change what a compiler does, where they were set.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Source files, objects and archives read by the call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<FileDigest>,
    /// Files written by the call, hashed after it finished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<FileDigest>,
    /// Wall clock seconds.
    pub wall_seconds: f64,
    /// User CPU seconds of the compiler and everything it ran, where the platform says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_seconds: Option<f64>,
    /// System CPU seconds, likewise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_seconds: Option<f64>,
    /// The largest resident set of the compiler or any process it waited for, in KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_kb: Option<u64>,
    /// The exit status, absent when the compiler died on a signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// The signal the compiler died on, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// The first KiB of what the compiler wrote on standard error.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    /// The lines rucc wrote for `-frucc-trace`, one per file it compiled, parsed and kept whole.
    ///
    /// A list because one rucc call can compile several files. Kept as JSON values rather than a
    /// struct of our own, so that a field rucc adds next month arrives here without a change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rucc: Vec<serde_json::Value>,
    /// The determinism check, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twice: Option<Twice>,
}

impl CompileRecord {
    /// Whether the compiler exited zero.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }

    /// Append this record to a JSON Lines file as a single write.
    ///
    /// One `write` on a file opened for append, so that the dozen compilers a parallel ninja runs
    /// at once each land a whole line rather than pieces of lines woven together.
    pub fn append_to(&self, path: &Path) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&line)
    }
}

/// Read every record from a `compile.jsonl`, skipping lines that do not parse.
///
/// A line can be cut short when a machine loses power in the middle of a build, and one bad line
/// should not cost the other nine thousand. The count of skipped lines is returned so that the
/// caller can say so.
pub fn read_log(path: &Path) -> std::io::Result<(Vec<CompileRecord>, usize)> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_log(&text))
}

/// The parsing half of [`read_log`].
#[must_use]
pub fn parse_log(text: &str) -> (Vec<CompileRecord>, usize) {
    let mut records = Vec::new();
    let mut skipped = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) => skipped += 1,
        }
    }
    (records, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"started":1790000000.5,"argv":["cc","-c","t.c","-o","t.o"],"compiler":"/usr/bin/rucc","added":["-frucc-trace=/tmp/x"],"cwd":"/b","env":{"PATH":"/usr/bin"},"inputs":[{"path":"t.c","sha256":"aa"}],"outputs":[{"path":"t.o","sha256":"bb"}],"wall-seconds":0.02,"user-seconds":0.01,"peak-rss-kb":9644,"exit":0,"rucc":[{"rucc":"0.11.15","input":"t.c","output":"t.o","ok":true,"seconds":0.016,"peak-kb":9644,"phases":{"read":0.001}}]}"#;

    #[test]
    fn a_line_in_the_documented_shape_reads_back() {
        let (records, skipped) = parse_log(LINE);
        assert_eq!(skipped, 0);
        let record = &records[0];
        assert!(record.succeeded());
        assert_eq!(record.argv[2], "t.c");
        assert_eq!(record.peak_rss_kb, Some(9644));
        assert_eq!(record.rucc[0]["peak-kb"], 9644);
        assert!(record.twice.is_none());
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let (records, _) = parse_log(LINE);
        let again = serde_json::to_string(&records[0]).unwrap();
        let (back, _) = parse_log(&again);
        assert_eq!(back, records);
    }

    #[test]
    fn empty_fields_are_left_out_rather_than_written_as_null() {
        let record = CompileRecord {
            started: 0.0,
            argv: vec!["cc".into(), "--version".into()],
            compiler: "gcc".into(),
            added: vec![],
            cwd: "/".into(),
            env: BTreeMap::new(),
            inputs: vec![],
            outputs: vec![],
            wall_seconds: 0.0,
            user_seconds: None,
            system_seconds: None,
            peak_rss_kb: None,
            exit: Some(0),
            signal: None,
            stderr: String::new(),
            rucc: vec![],
            twice: None,
        };
        let text = serde_json::to_string(&record).unwrap();
        assert!(!text.contains("null"));
        assert!(!text.contains("rucc"));
    }

    #[test]
    fn a_cut_line_is_skipped_and_counted() {
        let text = format!("{LINE}\n{{\"started\":1,\"argv\":[\n{LINE}\n");
        let (records, skipped) = parse_log(&text);
        assert_eq!(records.len(), 2);
        assert_eq!(skipped, 1);
    }
}
