//! `rpg config-diff`: where two configured trees disagree about the platform.
//!
//! Postgres decides much of what it compiles at configure time, by asking the compiler. A compiler
//! that answers a probe differently from the reference gets a different Postgres, and every test
//! result after that compares two different programs. This compares the two answers: the defines in
//! `pg_config.h`, and the probe results that meson's log or configure's output printed.

use regex::Regex;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Name to value, with `None` for a define that is explicitly `#undef`.
pub type Defines = BTreeMap<String, Option<String>>;

/// Read the defines of a generated `pg_config.h`.
#[must_use]
pub fn parse_defines(text: &str) -> Defines {
    let mut out = Defines::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#define ") {
            let mut parts = rest.trim().splitn(2, char::is_whitespace);
            let Some(name) = parts.next() else { continue };
            let value = parts.next().unwrap_or_default().trim().to_string();
            out.insert(name.to_string(), Some(value));
        } else if let Some(rest) = line.strip_prefix("/* #undef ") {
            let name = rest.trim_end_matches("*/").trim();
            out.insert(name.to_string(), None);
        }
    }
    out
}

/// Read the probe results meson printed to its log, as question and answer.
#[must_use]
pub fn parse_meson_probes(text: &str) -> BTreeMap<String, String> {
    let line_re = Regex::new(
        r"^((?:Checking|Has header|Header|Library|Compiler for C supports|Fetching value of define|Run-time dependency|Program|Dependency)\b.*):\s(.*)$",
    )
    .expect("probe pattern");
    let ansi = Regex::new(r"\x1b\[[0-9;]*m").expect("ansi pattern");
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = ansi.replace_all(line, "");
        if let Some(c) = line_re.captures(&line) {
            let answer = c[2].trim().trim_end_matches("(cached)").trim().to_string();
            out.insert(c[1].trim().to_string(), answer);
        }
    }
    out
}

/// Read the probe results configure printed, `checking for x... yes`.
#[must_use]
pub fn parse_configure_probes(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("checking ") else {
            continue;
        };
        if let Some((question, answer)) = rest.split_once("... ") {
            out.insert(
                question.to_string(),
                answer
                    .trim()
                    .trim_start_matches("(cached)")
                    .trim()
                    .to_string(),
            );
        }
    }
    out
}

/// One disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// `pg_config.h` or `probe`.
    pub source: &'static str,
    /// The define or the question.
    pub name: String,
    /// What A says, `None` when A does not mention it.
    pub a: Option<String>,
    /// What B says.
    pub b: Option<String>,
}

fn diff_maps<V: Clone + PartialEq>(
    source: &'static str,
    a: &BTreeMap<String, V>,
    b: &BTreeMap<String, V>,
    show: impl Fn(&V) -> String,
) -> Vec<Difference> {
    let mut out = Vec::new();
    let names: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for name in names {
        let (x, y) = (a.get(name), b.get(name));
        if x != y {
            out.push(Difference {
                source,
                name: name.clone(),
                a: x.map(&show),
                b: y.map(&show),
            });
        }
    }
    out
}

/// Compare two sets of defines.
#[must_use]
pub fn diff_defines(a: &Defines, b: &Defines) -> Vec<Difference> {
    diff_maps("pg_config.h", a, b, |v| {
        v.clone().unwrap_or_else(|| "#undef".to_string())
    })
}

/// Compare two sets of probe results.
#[must_use]
pub fn diff_probes(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Vec<Difference> {
    diff_maps("probe", a, b, Clone::clone)
}

/// One side: an `rpg build` directory, or a Postgres build tree directly.
struct Side {
    out: PathBuf,
    tree: PathBuf,
}

impl Side {
    fn new(dir: &Path) -> Self {
        // Canonical, so that `.` is never what gets replaced in the files.
        let out = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let tree = if out.join("build").join("src").is_dir() {
            out.join("build")
        } else {
            out.clone()
        };
        Self { out, tree }
    }

    fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.tree.join(relative)).ok()
    }

    /// Replace this side's own paths, so that two trees in different places compare equal.
    fn normalize(&self, text: &str) -> String {
        text.replace(&self.tree.display().to_string(), "<build>")
            .replace(&self.out.display().to_string(), "<out>")
    }

    fn defines(&self) -> Result<Defines, String> {
        let text = self.read("src/include/pg_config.h").ok_or_else(|| {
            format!(
                "{} has no src/include/pg_config.h; was it configured?",
                self.tree.display()
            )
        })?;
        Ok(parse_defines(&self.normalize(&text)))
    }

    fn probes(&self) -> BTreeMap<String, String> {
        if let Some(text) = self.read("meson-logs/meson-log.txt") {
            return parse_meson_probes(&self.normalize(&text));
        }
        std::fs::read_to_string(self.out.join("configure.log"))
            .map(|t| parse_configure_probes(&self.normalize(&t)))
            .unwrap_or_default()
    }
}

/// Compare two trees.
pub fn diff(a: &Path, b: &Path) -> Result<Vec<Difference>, String> {
    let (a, b) = (Side::new(a), Side::new(b));
    let mut out = diff_defines(&a.defines()?, &b.defines()?);
    out.extend(diff_probes(&a.probes(), &b.probes()));
    Ok(out)
}

/// Print the differences, one per line.
#[must_use]
pub fn report(differences: &[Difference]) -> String {
    let mut text = String::new();
    for d in differences {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(absent)".to_string());
        let _ = writeln!(
            text,
            "{}: {}\n  a: {}\n  b: {}",
            d.source,
            d.name,
            show(&d.a),
            show(&d.b)
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_and_undefs_are_read() {
        let text = "/* comment */\n#define HAVE_INT128 1\n#define PG_INT128_TYPE __int128\n/* #undef HAVE_X86_64_POPCNTQ */\n#define ALIGNOF_DOUBLE 8\n";
        let defines = parse_defines(text);
        assert_eq!(defines["HAVE_INT128"].as_deref(), Some("1"));
        assert_eq!(defines["PG_INT128_TYPE"].as_deref(), Some("__int128"));
        assert_eq!(defines["HAVE_X86_64_POPCNTQ"], None);
        assert_eq!(defines.len(), 4);
    }

    #[test]
    fn defines_that_differ_or_are_missing_are_reported() {
        let a = parse_defines("#define A 1\n#define B 2\n/* #undef C */\n");
        let b = parse_defines("#define A 1\n#define B 3\n#define C 1\n#define D 1\n");
        let d = diff_defines(&a, &b);
        let names: Vec<&str> = d.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["B", "C", "D"]);
        assert_eq!(d[1].a.as_deref(), Some("#undef"));
        assert_eq!(d[2].a, None);
    }

    #[test]
    fn meson_probe_lines_are_read() {
        let text = "Checking for size of \"long\" : 8\nHas header \"xlocale.h\" : NO \nChecking for function \"strchrnul\" : YES (cached)\nChecking if \"x86_64: popcntq instruction\" compiles: NO\nCompiler for C supports arguments -Wmissing-prototypes: YES\nBuild targets in project: 12\n\x1b[1mLibrary m\x1b[0m found: YES\n";
        let probes = parse_meson_probes(text);
        assert_eq!(probes["Checking for size of \"long\""], "8");
        assert_eq!(probes["Has header \"xlocale.h\""], "NO");
        assert_eq!(probes["Checking for function \"strchrnul\""], "YES");
        assert_eq!(
            probes["Compiler for C supports arguments -Wmissing-prototypes"],
            "YES"
        );
        assert_eq!(probes["Library m found"], "YES");
        assert_eq!(
            probes["Checking if \"x86_64: popcntq instruction\" compiles"],
            "NO"
        );
        assert_eq!(probes.len(), 6);
    }

    #[test]
    fn configure_probe_lines_are_read() {
        let text = "checking for gcc... /w/bin/cc\nchecking size of long... 8\nchecking for __int128... (cached) yes\nconfigure: creating ./config.status\n";
        let probes = parse_configure_probes(text);
        assert_eq!(probes["size of long"], "8");
        assert_eq!(probes["for __int128"], "yes");
        assert_eq!(probes.len(), 3);
    }
}
