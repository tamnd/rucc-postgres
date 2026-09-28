//! `rpg demands`: what the pinned tree asks of a C compiler beyond plain C11.
//!
//! This is a text scan, not a parse, and it says so in its output. Comments are removed and the
//! contents of string and character literals are blanked first, keeping every line where it was,
//! so that a builtin named in a comment or an error message is not counted. Then each line is
//! matched against a fixed list of features, and every `__builtin_*` name, every attribute written
//! as `__attribute__((name` and every `pg_attribute_*` macro gets an entry of its own.
//!
//! The scan counts what is written, not what is compiled. A header of atomics for another
//! architecture is counted although x86-64 never includes it, and a feature hidden behind a
//! Postgres macro is counted where the macro is defined and not at each use. The per file line
//! lists are there so a reader can check.
//!
//! Each entry has `rucc-status` and `rucc-issue` fields that are left empty by the scan and filled
//! in by hand. A new scan keeps whatever they held.

use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One feature the scan looks for.
struct Feature {
    tag: &'static str,
    what: &'static str,
    pattern: &'static str,
}

/// The fixed list. Order is the order of the output.
const FEATURES: &[Feature] = &[
    Feature {
        tag: "int128",
        what: "128 bit integer types",
        pattern: r"\b(__int128|u?int128|PG_INT128_TYPE)\b",
    },
    Feature {
        tag: "overflow-builtins",
        what: "__builtin_add_overflow and relatives",
        pattern: r"\b__builtin_(add|sub|mul)_overflow(_p)?\b",
    },
    Feature {
        tag: "atomic-builtins",
        what: "__atomic_* builtins",
        pattern: r"\b__atomic_\w+",
    },
    Feature {
        tag: "sync-builtins",
        what: "__sync_* builtins",
        pattern: r"\b__sync_\w+",
    },
    Feature {
        tag: "bit-builtins",
        what: "clz, ctz, popcount, ffs, parity and bswap builtins",
        pattern: r"\b__builtin_(clz|ctz|popcount|ffs|parity|bswap)\w*",
    },
    Feature {
        tag: "computed-goto",
        what: "labels as values and goto *",
        pattern: r"\bgoto\s*\*|(^|[{,=(])\s*&&[A-Za-z_]\w*",
    },
    Feature {
        tag: "inline-asm",
        what: "inline assembly",
        pattern: r"\b(__asm__|__asm|asm)\b\s*(volatile|__volatile__|goto|inline)?\s*\(",
    },
    Feature {
        tag: "target-attribute",
        what: "per function target attributes",
        pattern: r#"\bpg_attribute_target\b|\btarget\s*\(\s*""#,
    },
    Feature {
        tag: "x86-intrinsic-headers",
        what: "x86 intrinsic headers",
        pattern: r"<(immintrin|nmmintrin|smmintrin|emmintrin|xmmintrin|tmmintrin|x86intrin|cpuid)\.h>",
    },
    Feature {
        tag: "sse42-intrinsics",
        what: "SSE 4.2 CRC32C intrinsics",
        pattern: r"\b_mm_crc32_u\w+",
    },
    Feature {
        tag: "sse2-intrinsics",
        what: "SSE2 and other 128 bit x86 intrinsics and types",
        pattern: r"\b(_mm_\w+|__m128i?|__m128d)\b",
    },
    Feature {
        tag: "avx2-intrinsics",
        what: "256 bit x86 intrinsics and types",
        pattern: r"\b(_mm256_\w+|__m256i?)\b",
    },
    Feature {
        tag: "avx512-intrinsics",
        what: "512 bit x86 intrinsics and types",
        pattern: r"\b(_mm512_\w+|__m512i?|__mmask\d+)\b",
    },
    Feature {
        tag: "arm-intrinsic-headers",
        what: "Arm intrinsic headers",
        pattern: r"<(arm_neon|arm_acle|arm_sve)\.h>",
    },
    Feature {
        tag: "neon-intrinsics",
        what: "Neon intrinsics and vector types",
        pattern: r"\b(v(ld1|st1|dup|ceq|cgt|clt|orr|and|max|min|get|shr|shl|add|sub|reinterpret|qsub|bsl|cnt|mvn)\w*_[us](8|16|32|64)\w*|u?int(8|16|32|64)x\d+_t)\b",
    },
    Feature {
        tag: "acle-crc",
        what: "Arm CRC32 intrinsics",
        pattern: r"\b__crc32\w+",
    },
    Feature {
        tag: "sve-intrinsics",
        what: "Arm SVE intrinsics and types",
        pattern: r"\b(sv[a-z0-9]+_[a-z0-9_]*[us](8|16|32|64)\w*|svbool_t|svuint\d+_t)\b",
    },
    Feature {
        tag: "cpuid",
        what: "cpuid and xgetbv",
        pattern: r"\b(__get_cpuid\w*|__cpuid\w*|_xgetbv|__builtin_cpu_supports|__builtin_cpu_init)\b",
    },
    Feature {
        tag: "sigsetjmp",
        what: "sigsetjmp and siglongjmp, which elog's PG_TRY rests on",
        pattern: r"\b(sigsetjmp|siglongjmp|sigjmp_buf)\b",
    },
    Feature {
        tag: "setjmp",
        what: "setjmp and longjmp",
        pattern: r"\b(_?setjmp|_?longjmp|jmp_buf)\b",
    },
    Feature {
        tag: "builtin-setjmp",
        what: "__builtin_setjmp and __builtin_longjmp",
        pattern: r"\b__builtin_(setjmp|longjmp)\b",
    },
    Feature {
        tag: "returns-twice",
        what: "the returns_twice attribute",
        pattern: r"\breturns_twice\b",
    },
    Feature {
        tag: "dllimport",
        what: "PGDLLIMPORT, PGDLLEXPORT and __declspec",
        pattern: r"\b(PGDLLIMPORT|PGDLLEXPORT|__declspec|dllimport|dllexport)\b",
    },
    Feature {
        tag: "thread-local",
        what: "thread local storage",
        pattern: r"\b(__thread|_Thread_local|thread_local|pg_thread_local)\b",
    },
    Feature {
        tag: "visibility",
        what: "symbol visibility attributes",
        pattern: r"\bvisibility\s*\(",
    },
    Feature {
        tag: "always-inline",
        what: "forced inlining",
        pattern: r"\b(always_inline|pg_attribute_always_inline|__forceinline)\b",
    },
    Feature {
        tag: "noreturn",
        what: "noreturn functions",
        pattern: r"\b(pg_noreturn|noreturn|_Noreturn|__noreturn__)\b",
    },
    Feature {
        tag: "printf-format",
        what: "printf format checking",
        pattern: r"\b(pg_attribute_printf|format\s*\(\s*(printf|gnu_printf|__printf__|PG_PRINTF_ATTRIBUTE))",
    },
    Feature {
        tag: "typeof",
        what: "typeof",
        pattern: r"\b(__typeof__|__typeof|typeof|typeof_unqual)\b",
    },
    Feature {
        tag: "statement-expression",
        what: "GNU statement expressions",
        pattern: r"\(\s*\{",
    },
    Feature {
        tag: "generic-selection",
        what: "_Generic",
        pattern: r"\b_Generic\b",
    },
    Feature {
        tag: "static-assert",
        what: "_Static_assert and the StaticAssert macros",
        pattern: r"\b(_Static_assert|static_assert|StaticAssert(Stmt|Expr|Decl)|StaticAssertVariableIsOfType\w*)\b",
    },
    Feature {
        tag: "alignment",
        what: "alignment specifiers and attributes",
        pattern: r"\b(_Alignas|alignas|_Alignof|alignof|__alignof__|pg_attribute_aligned)\b",
    },
    Feature {
        tag: "range-designator",
        what: "GNU case ranges and array range designators",
        pattern: r"\bcase\s[^:]*\.\.\.|\[\s*\w+\s*\.\.\.\s*\w+\s*\]",
    },
    Feature {
        tag: "flexible-array",
        what: "flexible array members",
        pattern: r"\bFLEXIBLE_ARRAY_MEMBER\b",
    },
    Feature {
        tag: "has-feature-macros",
        what: "__has_builtin, __has_attribute, __has_include and relatives",
        pattern: r"\b__has_(builtin|attribute|include|feature|extension|c_attribute)\b",
    },
    Feature {
        tag: "varargs",
        what: "variadic functions",
        pattern: r"\b(va_start|va_arg|va_copy|va_end|__builtin_va_\w+)\b",
    },
    Feature {
        tag: "variadic-macro",
        what: "variadic macros",
        pattern: r"__VA_ARGS__|__VA_OPT__",
    },
    Feature {
        tag: "alloca",
        what: "alloca",
        pattern: r"\b(alloca|__builtin_alloca)\s*\(",
    },
    Feature {
        tag: "pragma",
        what: "pragmas",
        pattern: r"^\s*#\s*pragma\b|\b_Pragma\s*\(",
    },
    Feature {
        tag: "predefined-macros",
        what: "compiler and target predefined macros",
        pattern: r"\b(__GNUC__|__GNUC_MINOR__|__clang__|_MSC_VER|__x86_64__|__aarch64__|__i386__|__SSE4_2__|__AVX2__|__AVX512\w*__|__ARM_FEATURE_\w+|__ARM_NEON\w*|__SIZEOF_INT128__|__BYTE_ORDER__|__OPTIMIZE__|__STDC_VERSION__|__has_c_attribute)\b",
    },
];

/// Which table an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// One of the fixed features.
    Feature,
    /// A `__builtin_*` name.
    Builtin,
    /// An attribute name written as `__attribute__((name`.
    Attribute,
    /// A `pg_attribute_*` macro, which is how most of the tree spells an attribute.
    PgAttribute,
}

/// One entry of `demands.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Demand {
    /// The name, for example `int128` or `builtin:__builtin_expect`.
    pub tag: String,
    /// Which table.
    pub kind: Kind,
    /// What it is.
    pub what: String,
    /// How many times it appears.
    pub count: usize,
    /// In how many files.
    pub files: usize,
    /// Filled in by hand: `works`, `missing`, `partial`, `not-needed`, or empty.
    #[serde(default)]
    pub rucc_status: String,
    /// Filled in by hand: the rucc issue tracking it.
    #[serde(default)]
    pub rucc_issue: String,
    /// Per file, the lines it appears on.
    pub sites: BTreeMap<String, Vec<usize>>,
}

/// `demands.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Demands {
    /// The pin scanned.
    pub pin: String,
    /// Its commit.
    pub commit: String,
    /// The directories scanned, relative to the source.
    pub scanned: Vec<String>,
    /// Files scanned.
    pub file_count: usize,
    /// Lines scanned.
    pub line_count: usize,
    /// The entries.
    #[serde(default)]
    pub demand: Vec<Demand>,
}

/// The directories scanned.
pub const ROOTS: &[&str] = &["src", "contrib"];

/// Blank comments and the insides of string and character literals, keeping line breaks.
#[must_use]
pub fn strip(text: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        Line,
        Block,
        Str(char),
    }
    let mut out = String::with_capacity(text.len());
    let mut state = State::Code;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            State::Code => match c {
                '/' if chars.peek() == Some(&'/') => {
                    chars.next();
                    out.push_str("  ");
                    state = State::Line;
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    out.push_str("  ");
                    state = State::Block;
                }
                '"' | '\'' => {
                    out.push(c);
                    state = State::Str(c);
                }
                _ => out.push(c),
            },
            State::Line => {
                if c == '\n' {
                    out.push('\n');
                    state = State::Code;
                } else {
                    out.push(' ');
                }
            }
            State::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("  ");
                    state = State::Code;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                }
            }
            State::Str(quote) => match c {
                '\\' => {
                    out.push(' ');
                    if let Some(next) = chars.next() {
                        out.push(if next == '\n' { '\n' } else { ' ' });
                    }
                }
                '\n' => {
                    // An unterminated literal, or an apostrophe in an #error line. Stop at the
                    // line end rather than blank the rest of the file.
                    out.push('\n');
                    state = State::Code;
                }
                _ if c == quote => {
                    out.push(c);
                    state = State::Code;
                }
                _ => out.push(' '),
            },
        }
    }
    out
}

/// The scanner, with its patterns compiled once.
pub struct Scanner {
    set: RegexSet,
    each: Vec<Regex>,
    builtin: Regex,
    attribute: Regex,
    pg_attribute: Regex,
}

/// What a scan found: per tag, per file, per line, the number of matches.
pub type Found = BTreeMap<(Kind, String), BTreeMap<String, BTreeMap<usize, usize>>>;

impl Scanner {
    /// Compile the patterns.
    ///
    /// # Panics
    ///
    /// When a pattern in the fixed list does not compile, which a test rules out.
    #[must_use]
    pub fn new() -> Self {
        let patterns: Vec<String> = FEATURES
            .iter()
            .map(|f| format!("(?m){}", f.pattern))
            .collect();
        Self {
            set: RegexSet::new(&patterns).expect("feature patterns compile"),
            each: patterns
                .iter()
                .map(|p| Regex::new(p).expect("feature pattern compiles"))
                .collect(),
            builtin: Regex::new(r"\b__builtin_\w+").expect("builtin pattern"),
            attribute: Regex::new(r"__attribute__\s*\(\(\s*([A-Za-z_]\w*)").expect("attribute"),
            pg_attribute: Regex::new(r"\bpg_attribute_\w+").expect("pg attribute"),
        }
    }

    /// Scan one file's text, adding to `found` under its relative path.
    pub fn scan(&self, path: &str, text: &str, found: &mut Found) {
        let stripped = strip(text);
        for (index, line) in stripped.lines().enumerate() {
            let number = index + 1;
            let mut add = |kind: Kind, tag: &str, hits: usize| {
                if hits > 0 {
                    *found
                        .entry((kind, tag.to_string()))
                        .or_default()
                        .entry(path.to_string())
                        .or_default()
                        .entry(number)
                        .or_default() += hits;
                }
            };
            for i in &self.set.matches(line) {
                let mut hits = self.each[i].find_iter(line).count();
                if FEATURES[i].tag == "sse2-intrinsics" {
                    // The regex crate has no lookahead, so the CRC32 intrinsics, which have their
                    // own tag, are taken back out here.
                    hits -= self.each[i]
                        .find_iter(line)
                        .filter(|m| m.as_str().starts_with("_mm_crc32"))
                        .count();
                }
                add(Kind::Feature, FEATURES[i].tag, hits);
            }
            if line.contains("__builtin_") {
                let mut names: BTreeMap<&str, usize> = BTreeMap::new();
                for m in self.builtin.find_iter(line) {
                    *names.entry(m.as_str()).or_default() += 1;
                }
                for (name, hits) in names {
                    add(Kind::Builtin, name, hits);
                }
            }
            if line.contains("__attribute__") {
                for c in self.attribute.captures_iter(line) {
                    add(Kind::Attribute, c[1].trim_matches('_'), 1);
                }
            }
            if line.contains("pg_attribute_") {
                let mut names: BTreeMap<&str, usize> = BTreeMap::new();
                for m in self.pg_attribute.find_iter(line) {
                    *names.entry(m.as_str()).or_default() += 1;
                }
                for (name, hits) in names {
                    add(Kind::PgAttribute, name, hits);
                }
            }
        }
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Every C source, header, grammar and lexer file under a directory, sorted.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("c" | "h" | "y" | "l")
        ) {
            out.push(path);
        }
    }
}

/// Turn what a scan found into entries, carrying over the hand filled fields of a previous file.
#[must_use]
pub fn entries(found: &Found, previous: &Demands) -> Vec<Demand> {
    let kept: BTreeMap<&str, &Demand> = previous
        .demand
        .iter()
        .map(|d| (d.tag.as_str(), d))
        .collect();
    let mut out = Vec::new();
    let order = |kind: Kind, tag: &str| {
        let fixed = FEATURES.iter().position(|f| f.tag == tag).unwrap_or(0);
        (kind, fixed, tag.to_string())
    };
    let mut keys: Vec<&(Kind, String)> = found.keys().collect();
    keys.sort_by_key(|(kind, tag)| order(*kind, tag));
    for key in keys {
        let (kind, name) = key;
        let files = &found[key];
        let tag = match kind {
            Kind::Feature => name.clone(),
            Kind::Builtin => format!("builtin:{name}"),
            Kind::Attribute => format!("attribute:{name}"),
            Kind::PgAttribute => format!("pg-attribute:{name}"),
        };
        let what = match kind {
            Kind::Feature => FEATURES
                .iter()
                .find(|f| f.tag == name)
                .map_or("", |f| f.what)
                .to_string(),
            Kind::Builtin => "a compiler builtin".to_string(),
            Kind::Attribute => "an attribute written directly".to_string(),
            Kind::PgAttribute => "a Postgres attribute macro, defined in c.h".to_string(),
        };
        let old = kept.get(tag.as_str());
        out.push(Demand {
            what,
            kind: *kind,
            count: files.values().flat_map(BTreeMap::values).sum(),
            files: files.len(),
            rucc_status: old.map(|d| d.rucc_status.clone()).unwrap_or_default(),
            rucc_issue: old.map(|d| d.rucc_issue.clone()).unwrap_or_default(),
            sites: files
                .iter()
                .map(|(file, lines)| (file.clone(), lines.keys().copied().collect()))
                .collect(),
            tag,
        });
    }
    // An entry that no longer appears but had something written by hand is kept, with no sites,
    // so the note is not lost when the pin moves.
    for old in &previous.demand {
        let present = out.iter().any(|d| d.tag == old.tag);
        if !present && (!old.rucc_status.is_empty() || !old.rucc_issue.is_empty()) {
            out.push(Demand {
                count: 0,
                files: 0,
                sites: BTreeMap::new(),
                ..old.clone()
            });
        }
    }
    out
}

/// Scan a source tree.
pub fn scan_tree(source: &Path, previous: &Demands) -> Result<Demands, String> {
    let scanner = Scanner::new();
    let mut found = Found::new();
    let mut files = Vec::new();
    for root in ROOTS {
        sources(&source.join(root), &mut files);
    }
    if files.is_empty() {
        return Err(format!("no C files under {}", source.display()));
    }
    let mut line_count = 0;
    for path in &files {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        line_count += text.lines().count();
        let relative = path.strip_prefix(source).unwrap_or(path);
        scanner.scan(&relative.display().to_string(), &text, &mut found);
    }
    Ok(Demands {
        pin: String::new(),
        commit: String::new(),
        scanned: ROOTS.iter().map(ToString::to_string).collect(),
        file_count: files.len(),
        line_count,
        demand: entries(&found, previous),
    })
}

/// The header written above the entries.
pub const HEADER: &str =
    "# What the pinned Postgres tree asks of a C compiler, written by rpg demands.
#
# A text scan with comments and literal contents removed: it counts what is written, not what a
# given target compiles, and a feature behind a Postgres macro is counted at the macro. `sites`
# lists the lines per file. `rucc-status` and `rucc-issue` are filled in by hand and survive a new
# scan; the rest is regenerated.

";

/// Read a previous file, or an empty one when there is none.
pub fn load(path: &Path) -> Result<Demands, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(_) => Ok(Demands::default()),
    }
}

/// Write the file.
pub fn save(path: &Path, demands: &Demands) -> Result<(), String> {
    let body = toml::to_string(demands).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{HEADER}{body}"))
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_literals_are_blanked_in_place() {
        let text = "int a; /* __int128\n more */ int b; // __sync_x\nchar *s = \"__atomic_x\\\"\"; char c = '\\'';\n";
        let out = strip(text);
        assert_eq!(out.lines().count(), text.lines().count());
        assert_eq!(out.len(), text.len());
        assert!(!out.contains("__int128"));
        assert!(!out.contains("__sync"));
        assert!(!out.contains("__atomic"));
        assert!(out.contains("int b;"));
        assert!(out.contains("char c = '  ';"));
    }

    #[test]
    fn features_builtins_and_attributes_are_counted_by_line() {
        let text = "\
static inline int f(uint64 x) { return __builtin_clzll(x) + __builtin_clzll(x); }
typedef __int128 int128;
extern void g(void) __attribute__((noreturn));
static void *t[] = {
    &&CASE_A,
    &&CASE_B,
};
pg_attribute_target(\"sse4.2\") uint32 c(void) { return _mm_crc32_u8(0, 1) + _mm_setzero_si128(); }
/* __sync_lock_test_and_set in a comment */
";
        let scanner = Scanner::new();
        let mut found = Found::new();
        scanner.scan("src/x.c", text, &mut found);
        let demands = entries(&found, &Demands::default());
        let get = |tag: &str| demands.iter().find(|d| d.tag == tag).unwrap();
        assert_eq!(get("builtin:__builtin_clzll").count, 2);
        assert_eq!(get("bit-builtins").sites["src/x.c"], vec![1]);
        assert_eq!(get("int128").count, 2);
        assert_eq!(get("attribute:noreturn").count, 1);
        assert_eq!(get("computed-goto").sites["src/x.c"], vec![5, 6]);
        assert_eq!(get("target-attribute").count, 1);
        assert_eq!(get("sse42-intrinsics").count, 1);
        assert_eq!(get("sse2-intrinsics").count, 1);
        assert!(demands.iter().all(|d| d.tag != "sync-builtins"));
    }

    #[test]
    fn hand_written_fields_survive_a_rescan() {
        let mut found = Found::new();
        Scanner::new().scan("a.c", "__int128 x;\n", &mut found);
        let mut previous = Demands {
            demand: entries(&found, &Demands::default()),
            ..Demands::default()
        };
        previous.demand[0].rucc_status = "works".into();
        previous.demand.push(Demand {
            tag: "gone".into(),
            kind: Kind::Feature,
            what: String::new(),
            count: 3,
            files: 1,
            rucc_status: "missing".into(),
            rucc_issue: "#12".into(),
            sites: BTreeMap::new(),
        });
        let again = entries(&found, &previous);
        assert_eq!(again[0].rucc_status, "works");
        let gone = again.iter().find(|d| d.tag == "gone").unwrap();
        assert_eq!((gone.count, gone.rucc_issue.as_str()), (0, "#12"));
        let text = toml::to_string(&Demands {
            demand: again.clone(),
            ..Demands::default()
        })
        .unwrap();
        let back: Demands = toml::from_str(&text).unwrap();
        assert_eq!(back.demand, again);
    }
}
