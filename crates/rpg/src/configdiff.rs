//! `rpg config-diff`: where two configured trees disagree about the platform.
//!
//! Postgres decides much of what it compiles at configure time, by asking the compiler. A compiler
//! that answers a probe differently from the reference gets a different Postgres, and every test
//! result after that compares two different programs. This compares the two answers: the defines in
//! `pg_config.h`, the variables of `src/Makefile.global`, and the probe results that meson's log or
//! configure's output printed.
//!
//! Some differences are expected and change nothing that gets compiled, such as the compiler's
//! version string. `config-divergences.toml` lists those, each with the reason it is harmless, and a
//! difference listed there is reported as explained rather than as a failure.

use regex::Regex;
use serde::Deserialize;
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

/// Read the variables a `Makefile.global` sets, as name and value.
///
/// Only assignments at the start of a line are read, which is how configure writes every one it
/// substitutes. A continued line is joined with a space, `+=` appends, and a variable set twice keeps
/// the last value, as make would.
#[must_use]
pub fn parse_makefile(text: &str) -> BTreeMap<String, String> {
    let assign =
        Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s*([:+?]?=)\s*(.*)$").expect("assignment pattern");
    let mut out = BTreeMap::new();
    let mut lines = text.lines();
    while let Some(first) = lines.next() {
        let mut line = first.to_string();
        while line.ends_with('\\') {
            line.pop();
            line.truncate(line.trim_end().len());
            let Some(next) = lines.next() else { break };
            line.push(' ');
            line.push_str(next.trim());
        }
        let Some(c) = assign.captures(&line) else {
            continue;
        };
        let value = c[3].trim().to_string();
        if &c[2] == "+=" {
            let old: &mut String = out.entry(c[1].to_string()).or_default();
            if !old.is_empty() {
                old.push(' ');
            }
            old.push_str(&value);
        } else if &c[2] != "?=" || !out.contains_key(&c[1]) {
            out.insert(c[1].to_string(), value);
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
    /// `pg_config.h`, `Makefile.global` or `probe`.
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

/// Compare two sets of `Makefile.global` variables.
#[must_use]
pub fn diff_makefiles(
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
) -> Vec<Difference> {
    diff_maps("Makefile.global", a, b, Clone::clone)
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

    /// The variables of `src/Makefile.global`, or `None` for a tree without one.
    fn makefile(&self) -> Option<BTreeMap<String, String>> {
        self.read("src/Makefile.global")
            .map(|text| parse_makefile(&self.normalize(&text)))
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
    match (a.makefile(), b.makefile()) {
        (Some(x), Some(y)) => out.extend(diff_makefiles(&x, &y)),
        (None, None) => {}
        (x, _) => {
            let missing = if x.is_none() { &a.tree } else { &b.tree };
            return Err(format!(
                "{} has no src/Makefile.global and the other tree has one",
                missing.display()
            ));
        }
    }
    out.extend(diff_probes(&a.probes(), &b.probes()));
    Ok(out)
}

/// One entry of `config-divergences.toml`: a difference that is expected, and why it is harmless.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Divergence {
    /// `pg_config.h`, `Makefile.global` or `probe`.
    pub source: String,
    /// The define, variable or question. A name ending in `*` matches every name that starts with
    /// what comes before it.
    pub name: String,
    /// Why the difference does not change which code is compiled.
    pub why: String,
    /// The rows of `rows.toml` the entry is for. An entry without rows is for every row.
    #[serde(default)]
    pub rows: Vec<String>,
}

impl Divergence {
    /// Whether the entry applies to a comparison made on `row`. With no row given, only the
    /// entries for every row apply.
    #[must_use]
    pub fn applies_to(&self, row: Option<&str>) -> bool {
        self.rows.is_empty() || row.is_some_and(|r| self.rows.iter().any(|x| x == r))
    }

    fn matches(&self, d: &Difference) -> bool {
        self.source == d.source
            && match self.name.strip_suffix('*') {
                Some(prefix) => d.name.starts_with(prefix),
                None => self.name == d.name,
            }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DivergenceFile {
    #[serde(default)]
    divergence: Vec<Divergence>,
}

/// Read `config-divergences.toml`. Every entry has to say why, since an entry without a reason is
/// a difference nobody looked at.
pub fn parse_divergences(text: &str) -> Result<Vec<Divergence>, String> {
    let file: DivergenceFile =
        toml::from_str(text).map_err(|e| format!("config-divergences.toml: {e}"))?;
    for d in &file.divergence {
        if !["pg_config.h", "Makefile.global", "probe"].contains(&d.source.as_str()) {
            return Err(format!(
                "config-divergences.toml: {} has source {:?}, which is not pg_config.h, Makefile.global or probe",
                d.name, d.source
            ));
        }
        if d.why.trim().is_empty() {
            return Err(format!(
                "config-divergences.toml: {} {} does not say why it is harmless",
                d.source, d.name
            ));
        }
    }
    Ok(file.divergence)
}

/// The differences split by whether `config-divergences.toml` explains them, and the entries that
/// explained nothing this time.
pub struct Sorted<'a> {
    /// Differences no entry matches. Any of these fails the comparison.
    pub unexplained: Vec<Difference>,
    /// Differences with the entry that matched them.
    pub explained: Vec<(Difference, &'a Divergence)>,
    /// Entries that matched no difference.
    pub unused: Vec<&'a Divergence>,
}

/// Match each difference against the divergences for `row`, first match wins. Entries for other
/// rows neither explain anything nor count as unused.
#[must_use]
pub fn sort<'a>(
    differences: Vec<Difference>,
    divergences: &'a [Divergence],
    row: Option<&str>,
) -> Sorted<'a> {
    let divergences: Vec<&Divergence> = divergences.iter().filter(|v| v.applies_to(row)).collect();
    let mut used = vec![false; divergences.len()];
    let mut sorted = Sorted {
        unexplained: Vec::new(),
        explained: Vec::new(),
        unused: Vec::new(),
    };
    for d in differences {
        match divergences.iter().position(|v| v.matches(&d)) {
            Some(i) => {
                used[i] = true;
                sorted.explained.push((d, divergences[i]));
            }
            None => sorted.unexplained.push(d),
        }
    }
    sorted.unused = divergences
        .iter()
        .zip(used)
        .filter(|(_, u)| !u)
        .map(|(v, _)| *v)
        .collect();
    sorted
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
    fn makefile_assignments_are_read_as_make_would() {
        let text = "# comment\nCC = gcc\nCFLAGS = -O2 \\\n\t-g\nLIBS := -lm\nLIBS += -ldl\nX ?= 1\nX ?= 2\n\tRECIPE = no\nifeq ($(A),yes)\nendif\n";
        let vars = parse_makefile(text);
        assert_eq!(vars["CC"], "gcc");
        assert_eq!(vars["CFLAGS"], "-O2 -g");
        assert_eq!(vars["LIBS"], "-lm -ldl");
        assert_eq!(vars["X"], "1");
        assert_eq!(vars.len(), 4);
    }

    #[test]
    fn a_divergence_explains_its_difference_and_nothing_else() {
        let divergences = parse_divergences(
            "[[divergence]]\nsource = \"pg_config.h\"\nname = \"PG_VERSION_STR\"\nwhy = \"the compiler's name\"\n\n[[divergence]]\nsource = \"Makefile.global\"\nname = \"CFLAGS_*\"\nwhy = \"warning flags\"\n\n[[divergence]]\nsource = \"probe\"\nname = \"never\"\nwhy = \"stale\"\n",
        )
        .expect("parses");
        let a = parse_defines("#define PG_VERSION_STR \"gcc\"\n#define HAVE_X 1\n");
        let b = parse_defines("#define PG_VERSION_STR \"rucc\"\n");
        let mut differences = diff_defines(&a, &b);
        differences.extend(diff_makefiles(
            &parse_makefile("CFLAGS_SL = -fPIC\nCFLAGS = -O2\n"),
            &parse_makefile("CFLAGS_SL = -fpic\nCFLAGS = -O2\n"),
        ));
        let sorted = sort(differences, &divergences, None);
        let unexplained: Vec<&str> = sorted.unexplained.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(unexplained, ["HAVE_X"]);
        let explained: Vec<&str> = sorted
            .explained
            .iter()
            .map(|(d, _)| d.name.as_str())
            .collect();
        assert_eq!(explained, ["PG_VERSION_STR", "CFLAGS_SL"]);
        assert_eq!(sorted.unused.len(), 1);
        assert_eq!(sorted.unused[0].name, "never");
    }

    #[test]
    fn a_divergence_for_one_row_explains_nothing_on_another() {
        let divergences = parse_divergences(
            "[[divergence]]\nsource = \"pg_config.h\"\nname = \"USE_SVE_POPCNT_WITH_RUNTIME_CHECK\"\nrows = [\"LA64\"]\nwhy = \"no arm_sve.h\"\n",
        )
        .expect("parses");
        let a = parse_defines("#define USE_SVE_POPCNT_WITH_RUNTIME_CHECK 1\n");
        let b = parse_defines("/* #undef USE_SVE_POPCNT_WITH_RUNTIME_CHECK */\n");
        let on_arm = sort(diff_defines(&a, &b), &divergences, Some("LA64"));
        assert!(on_arm.unexplained.is_empty());
        assert_eq!(on_arm.explained.len(), 1);
        let on_x86 = sort(diff_defines(&a, &b), &divergences, Some("L64"));
        assert_eq!(on_x86.unexplained.len(), 1);
        assert!(on_x86.unused.is_empty());
        let nowhere = sort(diff_defines(&a, &b), &divergences, None);
        assert_eq!(nowhere.unexplained.len(), 1);
        assert!(nowhere.unused.is_empty());
    }

    #[test]
    fn a_divergence_without_a_reason_is_refused() {
        let err =
            parse_divergences("[[divergence]]\nsource = \"probe\"\nname = \"x\"\nwhy = \" \"\n")
                .expect_err("refused");
        assert!(err.contains("does not say why"), "{err}");
        let err =
            parse_divergences("[[divergence]]\nsource = \"config.h\"\nname = \"x\"\nwhy = \"y\"\n")
                .expect_err("refused");
        assert!(err.contains("not pg_config.h"), "{err}");
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
