//! `rpg frames`: how much stack each function of one build takes against the same function of
//! another.
//!
//! Postgres runs deep recursion in the planner, the executor and the parser, and it checks its own
//! stack depth against `max_stack_depth` rather than waiting for the kernel. A compiler whose frames
//! are a few times larger than gcc's passes most of the regression suite and then fails the tests
//! that recurse on purpose, with `stack depth limit exceeded`. This command measures that before it
//! can surprise anyone.
//!
//! For each of two finished builds it takes every call in `compile.jsonl` that compiled a C file to
//! an object, and runs it again in its recorded directory with the recorded compiler, or the one
//! `--a-cc` or `--b-cc` names, adding `-fstack-usage` and sending the object to a scratch directory
//! so the build tree is never touched. The dependency file options are dropped, as `rpg repro` drops
//! them. The compiler writes a `.su` file beside the object, one line per function, with the
//! location and name, the bytes and a qualifier separated by tabs:
//! `/src/backend/parser/gram.c:1234:1:base_yyparse`, `1408`, `static`.
//!
//! Each function is keyed by the file that was compiled, named relative to the source or build tree,
//! the object the build wrote, and the function's name. A static function with the same name in two
//! files is two entries. A file the build compiled more than once, as autoconf and meson both do for
//! `src/common` and `src/port` with and without `-fPIC` and for the backend, stays one entry per
//! object, because each of those compiles sees different defines. Calls whose source file is gone,
//! which are configure's probes, are left out.
//!
//! gcc names the clones it makes with a suffix after a dot, `.isra.0`, `.constprop.0` or `.part.0`,
//! and often keeps no function under the plain name. A function that has no exact match on the
//! other side is matched a second time by the name before the first dot, taking the largest frame
//! of each side, and the report counts how many pairs were matched that way.
//!
//! gcc's partial inlining splits a function in two. The start keeps the plain name and the rest
//! moves to a `.part.N` clone that the start calls, so the plain name alone can be an 8 byte frame
//! for a function whose body takes kilobytes. A `.part` clone left over after matching is added to
//! the pair with its base name in the same object, which is the stack a call through it takes.

use crate::build::{BuildInfo, lexical};
use crate::compiler::checkout_commit;
use crate::process::capture;
use crate::repro::{is_dependency_option, names};
use rpg_shim::args::{self, Mode};
use rpg_shim::record::{CompileRecord, read_log};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What a `.su` line says about how the frame was sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    /// The frame is the same size on every call.
    Static,
    /// The frame grows at run time, by `alloca` or a variable length array, with no known limit.
    Dynamic,
    /// The frame grows at run time, but no further than a limit the compiler knows.
    Bounded,
}

impl Qualifier {
    /// Read the third field of a `.su` line.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "static" => Some(Self::Static),
            "dynamic" => Some(Self::Dynamic),
            "dynamic,bounded" => Some(Self::Bounded),
            _ => None,
        }
    }

    /// The name as a `.su` file writes it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
            Self::Bounded => "dynamic,bounded",
        }
    }

    /// Whether the frame can grow at run time.
    #[must_use]
    pub fn is_dynamic(self) -> bool {
        self != Self::Static
    }
}

/// One line of a `.su` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The function, as the compiler named it.
    pub function: String,
    /// Where it is defined, `path:line:col`, which is a header for a static inline function.
    pub location: String,
    /// The bytes of stack it takes.
    pub bytes: u64,
    /// How that was decided.
    pub qualifier: Qualifier,
}

/// Split `path:line:col:name` after the column, or after the line when there is no column.
///
/// The path is taken to end at the first colon that is followed by digits and another colon, so a
/// function name with colons in it, as a C++ name has, stays whole.
fn split_location(head: &str) -> Option<(&str, &str)> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    for (at, _) in head.match_indices(':') {
        let rest = &head[at + 1..];
        let line = digits(rest);
        if line == 0 || rest.as_bytes().get(line) != Some(&b':') {
            continue;
        }
        let mut end = at + 1 + line + 1;
        let after = &head[end..];
        let column = digits(after);
        if column > 0 && after.as_bytes().get(column) == Some(&b':') {
            end += column + 1;
        }
        let name = &head[end..];
        if at == 0 || name.is_empty() {
            return None;
        }
        return Some((&head[..end - 1], name));
    }
    None
}

/// Read one line of a `.su` file.
#[must_use]
pub fn parse_line(line: &str) -> Option<Frame> {
    let mut fields = line.trim_end_matches(['\r', '\n']).rsplitn(3, '\t');
    let qualifier = Qualifier::parse(fields.next()?)?;
    let bytes = fields.next()?.trim().parse().ok()?;
    let (location, function) = split_location(fields.next()?)?;
    Some(Frame {
        function: function.to_string(),
        location: location.to_string(),
        bytes,
        qualifier,
    })
}

/// Read a `.su` file: the frames, and how many nonblank lines did not parse.
#[must_use]
pub fn parse_su(text: &str) -> (Vec<Frame>, usize) {
    let mut frames = Vec::new();
    let mut unreadable = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match parse_line(line) {
            Some(frame) => frames.push(frame),
            None => unreadable += 1,
        }
    }
    (frames, unreadable)
}

/// One compile of a build: which file, into which object, by which recorded call.
#[derive(Debug, Clone)]
pub struct Unit<'a> {
    /// The C file, relative to the source or build tree.
    pub file: String,
    /// The object, relative to the build tree.
    pub object: String,
    /// The call.
    pub record: &'a CompileRecord,
}

/// Every successful compile of one C file to an object whose source still exists.
///
/// Configure's probes compile `conftest.c` and delete it straight after, so the existence check is
/// what leaves them out. A file compiled twice into the same object keeps its last call, as
/// `compile_commands.json` does.
#[must_use]
pub fn units<'a>(
    records: &'a [CompileRecord],
    build_dir: &Path,
    roots: &[&Path],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Unit<'a>> {
    let mut found: BTreeMap<(String, String), Unit<'a>> = BTreeMap::new();
    for record in records.iter().filter(|r| r.succeeded()) {
        let rest = record.argv.get(1..).unwrap_or_default();
        let cwd = Path::new(&record.cwd);
        let invocation = args::read(rest, cwd, &|_| true);
        if invocation.mode != Mode::Compile {
            continue;
        }
        let inputs = names(record, roots);
        let [(file, input)] = inputs.as_slice() else {
            continue;
        };
        if !exists(&cwd.join(input)) {
            continue;
        }
        let Some(object) = invocation.outputs.first() else {
            continue;
        };
        let full = lexical(&cwd.join(object));
        let object = full
            .strip_prefix(build_dir)
            .map_or_else(|_| full.display().to_string(), |p| p.display().to_string());
        found.insert(
            (file.clone(), object.clone()),
            Unit {
                file: file.clone(),
                object,
                record,
            },
        );
    }
    found.into_values().collect()
}

/// The recorded arguments without the output and the dependency options, plus `-fstack-usage` and
/// `-o object`.
#[must_use]
pub fn frame_args(rest: &[String], object: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = rest.iter();
    while let Some(arg) = words.next() {
        if matches!(arg.as_str(), "-o" | "-MF" | "-MT" | "-MQ") {
            words.next();
            continue;
        }
        if (arg.starts_with("-o") && arg.len() > 2)
            || is_dependency_option(arg)
            || arg == "-fstack-usage"
        {
            continue;
        }
        out.push(arg.clone());
    }
    out.extend([
        "-fstack-usage".to_string(),
        "-o".to_string(),
        object.to_string(),
    ]);
    out
}

/// What a function is known by: the file, the object and the name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// The C file, relative to the source or build tree.
    pub file: String,
    /// The object, relative to the build tree.
    pub object: String,
    /// The function.
    pub function: String,
}

/// The frames of one build.
#[derive(Debug, Clone, Default)]
pub struct Measured {
    /// The build directory's name.
    pub name: String,
    /// The compiler that ran.
    pub compiler: String,
    /// The first line of its `--version`.
    pub version: String,
    /// The commit of the checkout it was built in, when it sits in one.
    pub commit: Option<String>,
    /// Compiles run.
    pub units: usize,
    /// Compiles that failed, by file and object, with the start of their standard error.
    pub failures: Vec<(String, String)>,
    /// Every function's frame.
    pub frames: BTreeMap<Key, Frame>,
    /// `.su` lines that did not parse.
    pub unreadable: usize,
    /// Functions named twice in one `.su` file, which keep the larger frame.
    pub duplicates: usize,
}

/// Run `work` on `0..count`, `jobs` at a time, and return the results in order.
fn parallel<T: Send>(count: usize, jobs: usize, work: &(dyn Fn(usize) -> T + Sync)) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(count));
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, count.max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        break;
                    }
                    let result = work(i);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((i, result));
                }
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// Compile one unit again with `-fstack-usage` into `dir` and read what it wrote.
fn measure_unit(unit: &Unit, compiler: &Path, dir: &Path) -> Result<(Vec<Frame>, usize), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let object = dir.join("unit.o");
    let rest = unit.record.argv.get(1..).unwrap_or_default();
    // The recorded environment is applied except PATH, which puts the shim first, as in repro.
    let output = Command::new(compiler)
        .args(frame_args(rest, &object.display().to_string()))
        .current_dir(&unit.record.cwd)
        .envs(unit.record.env.iter().filter(|(k, _)| *k != "PATH"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", compiler.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "exited {}: {}",
            output.status,
            stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
        ));
    }
    let mut frames = Vec::new();
    let mut unreadable = 0;
    let mut found = false;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        if path.extension().is_some_and(|x| x == "su") {
            found = true;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let (mut more, bad) = parse_su(&text);
            frames.append(&mut more);
            unreadable += bad;
        }
    }
    std::fs::remove_dir_all(dir).ok();
    if found {
        Ok((frames, unreadable))
    } else {
        Err("the compile wrote no .su file".to_string())
    }
}

/// Measure every function of one build.
pub fn measure(
    build: &Path,
    compiler: Option<&Path>,
    jobs: usize,
    scratch: &Path,
) -> Result<Measured, String> {
    let info = BuildInfo::load(build)?;
    let log = build.join("compile.jsonl");
    let (records, _) = read_log(&log).map_err(|e| format!("reading {}: {e}", log.display()))?;
    let source = PathBuf::from(&info.source);
    let build_dir = PathBuf::from(&info.build_dir);
    let units = units(&records, &build_dir, &[&source, &build_dir], &|p| {
        p.is_file()
    });
    let Some(first) = units.first() else {
        return Err(format!("no compiles of C files in {}", log.display()));
    };
    let compiler =
        compiler.map_or_else(|| PathBuf::from(&first.record.compiler), Path::to_path_buf);
    let version = capture(&compiler, &["--version"])?
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let name = build.file_name().map_or_else(
        || build.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    eprintln!(
        "rpg: compiling {} translation units of {name} with {version} and -fstack-usage, {jobs} at a time",
        units.len()
    );
    let results = parallel(units.len(), jobs, &|i| {
        measure_unit(&units[i], &compiler, &scratch.join(i.to_string()))
    });
    let mut measured = Measured {
        name,
        commit: checkout_commit(&compiler),
        compiler: compiler.display().to_string(),
        version,
        units: units.len(),
        ..Measured::default()
    };
    for (unit, result) in units.iter().zip(results) {
        match result {
            Ok((frames, unreadable)) => {
                measured.unreadable += unreadable;
                for frame in frames {
                    let key = Key {
                        file: unit.file.clone(),
                        object: unit.object.clone(),
                        function: frame.function.clone(),
                    };
                    match measured.frames.get(&key) {
                        Some(old) => {
                            measured.duplicates += 1;
                            if old.bytes < frame.bytes {
                                measured.frames.insert(key, frame);
                            }
                        }
                        None => {
                            measured.frames.insert(key, frame);
                        }
                    }
                }
            }
            Err(message) => measured
                .failures
                .push((format!("{} ({})", unit.file, unit.object), message)),
        }
    }
    if measured.frames.is_empty() {
        let why = measured
            .failures
            .first()
            .map_or_else(String::new, |(f, m)| {
                format!(", the first failure is {f}: {m}")
            });
        return Err(format!(
            "no frames were measured for {}{why}",
            measured.name
        ));
    }
    Ok(measured)
}

/// A function found on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The key, with the plain name when the pair was matched by it.
    pub key: Key,
    /// The frame in build a.
    pub a: Frame,
    /// The frame in build b.
    pub b: Frame,
    /// Whether the pair was matched by the name before the first dot.
    pub by_base: bool,
    /// gcc's `.part` clone of a's function, when its frame was added to a's.
    pub part: Option<Frame>,
}

impl Pair {
    /// b over a, when a takes any stack at all.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(&self) -> Option<f64> {
        (self.a.bytes > 0).then(|| self.b.bytes as f64 / self.a.bytes as f64)
    }
}

/// The two sides joined.
#[derive(Debug, Clone, Default)]
pub struct Joined {
    /// Functions on both sides.
    pub pairs: Vec<Pair>,
    /// Functions only in a.
    pub only_a: Vec<(Key, Frame)>,
    /// Functions only in b.
    pub only_b: Vec<(Key, Frame)>,
}

/// The name before gcc's clone suffix: `foo` for `foo.isra.0`. A name with a space or a bracket
/// in it is not a C identifier and is left alone.
fn base_name(name: &str) -> &str {
    if name.contains([' ', '(', '<']) {
        name
    } else {
        name.split('.').next().unwrap_or(name)
    }
}

/// Whether a name is one of gcc's `.part` clones, the rest of a function after partial inlining.
fn is_part(name: &str) -> bool {
    base_name(name) != name && name.split('.').skip(1).any(|s| s == "part")
}

/// Join two builds' frames by key, then match what is left by the name before the first dot.
#[must_use]
pub fn join(a: &BTreeMap<Key, Frame>, b: &BTreeMap<Key, Frame>) -> Joined {
    type Groups = BTreeMap<Key, Vec<(Key, Frame)>>;
    let mut joined = Joined::default();
    let mut left_a: Groups = BTreeMap::new();
    let mut left_b: Groups = BTreeMap::new();
    let group = |key: &Key| Key {
        function: base_name(&key.function).to_string(),
        ..key.clone()
    };
    for (key, frame) in a {
        match b.get(key) {
            Some(other) => joined.pairs.push(Pair {
                key: key.clone(),
                a: frame.clone(),
                b: other.clone(),
                by_base: false,
                part: None,
            }),
            None => left_a
                .entry(group(key))
                .or_default()
                .push((key.clone(), frame.clone())),
        }
    }
    for (key, frame) in b.iter().filter(|(k, _)| !a.contains_key(*k)) {
        left_b
            .entry(group(key))
            .or_default()
            .push((key.clone(), frame.clone()));
    }
    let largest = |list: &mut Vec<(Key, Frame)>| {
        let at = (0..list.len()).max_by_key(|&i| (list[i].1.bytes, std::cmp::Reverse(i)))?;
        Some(list.remove(at).1)
    };
    for (base, mut from_a) in left_a {
        if let Some(mut from_b) = left_b.remove(&base) {
            if let (Some(fa), Some(fb)) = (largest(&mut from_a), largest(&mut from_b)) {
                joined.pairs.push(Pair {
                    key: base,
                    a: fa,
                    b: fb,
                    by_base: true,
                    part: None,
                });
            }
            joined.only_b.append(&mut from_b);
        }
        joined.only_a.append(&mut from_a);
    }
    for (_, mut rest) in left_b {
        joined.only_b.append(&mut rest);
    }
    let mut rest = Vec::new();
    for (key, frame) in std::mem::take(&mut joined.only_a) {
        let whole = group(&key);
        let pair = joined
            .pairs
            .iter_mut()
            .find(|p| p.part.is_none() && p.key == whole && is_part(&key.function));
        match pair {
            Some(pair) => {
                pair.a.bytes += frame.bytes;
                if frame.qualifier.is_dynamic() {
                    pair.a.qualifier = frame.qualifier;
                }
                pair.part = Some(frame);
            }
            None => rest.push((key, frame)),
        }
    }
    joined.only_a = rest;
    joined.pairs.sort_by(|x, y| x.key.cmp(&y.key));
    joined.only_a.sort_by(|x, y| x.0.cmp(&y.0));
    joined.only_b.sort_by(|x, y| x.0.cmp(&y.0));
    joined
}

/// The distribution of the ratios.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// Pairs with a ratio, which leaves out those where a takes no stack.
    pub count: usize,
    /// The middle ratio.
    pub median: f64,
    /// The 90th percentile.
    pub p90: f64,
    /// The 99th percentile.
    pub p99: f64,
    /// The largest.
    pub max: f64,
    /// How many ratios fall in each of the buckets of [`BUCKETS`].
    pub buckets: Vec<usize>,
}

/// The buckets of the ratio table: a label and the upper bound, which is inclusive.
pub const BUCKETS: &[(&str, f64)] = &[
    ("smaller in b", 0.999_999),
    ("the same", 1.000_001),
    ("up to 1.5 times", 1.5),
    ("1.5 to 2 times", 2.0),
    ("2 to 4 times", 4.0),
    ("over 4 times", f64::INFINITY),
];

/// The value at a percentile of sorted values, by the nearest rank method.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// The distribution of b over a, or nothing when no pair has one.
#[must_use]
pub fn stats(pairs: &[Pair]) -> Option<Stats> {
    let mut ratios: Vec<f64> = pairs.iter().filter_map(Pair::ratio).collect();
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(f64::total_cmp);
    let mut buckets = vec![0; BUCKETS.len()];
    for r in &ratios {
        let at = BUCKETS
            .iter()
            .position(|(_, top)| r <= top)
            .unwrap_or(BUCKETS.len() - 1);
        buckets[at] += 1;
    }
    Some(Stats {
        count: ratios.len(),
        median: percentile(&ratios, 50.0),
        p90: percentile(&ratios, 90.0),
        p99: percentile(&ratios, 99.0),
        max: ratios[ratios.len() - 1],
        buckets,
    })
}

/// A function name as a markdown table cell.
fn cell(name: &str) -> String {
    format!("`{}`", name.replace('|', "\\|").replace('`', "'"))
}

/// Where a key's function lives, with the object only when the file was compiled more than once.
fn place(key: &Key, several: &dyn Fn(&str) -> bool) -> String {
    if several(&key.file) {
        let object = Path::new(&key.object)
            .file_name()
            .map_or_else(|| key.object.clone(), |n| n.to_string_lossy().into_owned());
        format!("{} ({object})", key.file)
    } else {
        key.file.clone()
    }
}

/// The flag column: which side's frame is dynamic, and whether the pair was matched by base name.
fn flags(pair: &Pair) -> String {
    let mut out = Vec::new();
    if pair.a.qualifier.is_dynamic() {
        out.push(format!("a {}", pair.a.qualifier.name()));
    }
    if pair.b.qualifier.is_dynamic() {
        out.push(format!("b {}", pair.b.qualifier.name()));
    }
    if let Some(part) = &pair.part {
        out.push(format!("a with `{}` of {}", part.function, part.bytes));
    }
    if pair.by_base {
        out.push(format!(
            "as `{}` and `{}`",
            pair.a.function, pair.b.function
        ));
    }
    out.join(", ")
}

/// A table of pairs.
fn pair_table(text: &mut String, pairs: &[&Pair], several: &dyn Fn(&str) -> bool) {
    text.push_str("| | function | file | a | b | b/a | flags |\n");
    text.push_str("|--:|---|---|--:|--:|--:|---|\n");
    for (n, pair) in pairs.iter().enumerate() {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} | {} | {} |",
            n + 1,
            cell(&pair.key.function),
            place(&pair.key, several),
            pair.a.bytes,
            pair.b.bytes,
            pair.ratio()
                .map_or_else(|| "a is 0".to_string(), |r| format!("{r:.2}")),
            flags(pair)
        );
    }
}

/// A table of frames on one side only.
fn single_table(text: &mut String, frames: &[&(Key, Frame)], several: &dyn Fn(&str) -> bool) {
    text.push_str("| | function | file | bytes | qualifier |\n");
    text.push_str("|--:|---|---|--:|---|\n");
    for (n, (key, frame)) in frames.iter().enumerate() {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} |",
            n + 1,
            cell(&key.function),
            place(key, several),
            frame.bytes,
            frame.qualifier.name()
        );
    }
}

/// The largest frame of a side, as `name in file, N bytes`.
fn largest(side: &Measured) -> String {
    side.frames
        .iter()
        .max_by_key(|(_, f)| f.bytes)
        .map_or_else(String::new, |(k, f)| {
            format!("`{}` in {}, {} bytes", k.function, k.file, f.bytes)
        })
}

/// How many rows each ranked table has.
pub const TOP: usize = 50;

/// The markdown report.
#[must_use]
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn report(a: &Measured, b: &Measured, joined: &Joined, date: &str, host: &str) -> String {
    let mut objects: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for key in a.frames.keys().chain(b.frames.keys()) {
        objects.entry(&key.file).or_default().insert(&key.object);
    }
    let several = |file: &str| objects.get(file).is_some_and(|o| o.len() > 1);
    let mut text = format!("# Stack frames, {} against {}\n\n", a.name, b.name);
    let _ = writeln!(
        text,
        "Written by `rpg frames` on {date} on {host}. Every compile of a C file in each build's `compile.jsonl` was run again in its recorded directory with `-fstack-usage`, and the frame sizes the compiler wrote were joined by source file, object and function. The ratio is b over a, so a ratio above 1 means b's frame is larger. A dynamic frame grows at run time by `alloca` or a variable length array, and its size here is only the fixed part.\n"
    );
    let row = |text: &mut String, label: &str, x: &str, y: &str| {
        let _ = writeln!(text, "| {label} | {x} | {y} |");
    };
    text.push_str("| | a | b |\n|---|---|---|\n");
    row(&mut text, "build", &a.name, &b.name);
    row(&mut text, "compiler", &a.version, &b.version);
    row(
        &mut text,
        "path",
        &format!("`{}`", a.compiler),
        &format!("`{}`", b.compiler),
    );
    let commit = |m: &Measured| {
        m.commit
            .clone()
            .unwrap_or_else(|| "not in a checkout".into())
    };
    row(&mut text, "commit", &commit(a), &commit(b));
    row(
        &mut text,
        "translation units",
        &a.units.to_string(),
        &b.units.to_string(),
    );
    row(
        &mut text,
        "compiles that failed",
        &a.failures.len().to_string(),
        &b.failures.len().to_string(),
    );
    row(
        &mut text,
        "functions",
        &a.frames.len().to_string(),
        &b.frames.len().to_string(),
    );
    let dynamic = |m: &Measured| {
        m.frames
            .values()
            .filter(|f| f.qualifier.is_dynamic())
            .count()
            .to_string()
    };
    row(&mut text, "dynamic frames", &dynamic(a), &dynamic(b));
    let total = |m: &Measured| m.frames.values().map(|f| f.bytes).sum::<u64>();
    row(
        &mut text,
        "bytes, all functions",
        &total(a).to_string(),
        &total(b).to_string(),
    );
    row(&mut text, "largest frame", &largest(a), &largest(b));
    if a.unreadable + b.unreadable + a.duplicates + b.duplicates > 0 {
        row(
            &mut text,
            "`.su` lines that did not parse",
            &a.unreadable.to_string(),
            &b.unreadable.to_string(),
        );
        row(
            &mut text,
            "functions named twice in one file",
            &a.duplicates.to_string(),
            &b.duplicates.to_string(),
        );
    }

    let by_base = joined.pairs.iter().filter(|p| p.by_base).count();
    let parts = joined.pairs.iter().filter(|p| p.part.is_some()).count();
    let _ = writeln!(
        text,
        "\n## Matching\n\n{} functions are on both sides, {} only in a and {} only in b. {by_base} of the pairs were matched by the name before the first dot, which is how gcc names the clones it makes (`.isra.0`, `.constprop.0`, `.part.0`). In {parts} pairs a's frame includes the `.part` clone gcc split the function into, since the plain name keeps only the start of the function and calls the clone for the rest. A function on one side only is usually one the other compiler inlined everywhere it was called, or a static inline function from a header that one compiler emitted and the other did not.",
        joined.pairs.len(),
        joined.only_a.len(),
        joined.only_b.len()
    );
    let matched_a: u64 = joined.pairs.iter().map(|p| p.a.bytes).sum();
    let matched_b: u64 = joined.pairs.iter().map(|p| p.b.bytes).sum();
    let _ = writeln!(
        text,
        "\nOver the matched functions a takes {matched_a} bytes and b takes {matched_b}, {:.2} times as much.",
        if matched_a > 0 {
            matched_b as f64 / matched_a as f64
        } else {
            f64::NAN
        }
    );

    text.push_str("\n## Ratio\n\n");
    match stats(&joined.pairs) {
        Some(s) => {
            let zero = joined.pairs.len() - s.count;
            let _ = writeln!(
                text,
                "Over {} pairs{}:\n\n| median | p90 | p99 | max |\n|--:|--:|--:|--:|\n| {:.2} | {:.2} | {:.2} | {:.2} |\n",
                s.count,
                if zero > 0 {
                    format!(", leaving out {zero} where a's frame is 0 bytes")
                } else {
                    String::new()
                },
                s.median,
                s.p90,
                s.p99,
                s.max
            );
            text.push_str("| b's frame | functions |\n|---|--:|\n");
            for ((label, _), n) in BUCKETS.iter().zip(&s.buckets) {
                let _ = writeln!(text, "| {label} | {n} |");
            }
        }
        None => text.push_str("No pair has a frame in a to divide by.\n"),
    }

    let mut by_ratio: Vec<&Pair> = joined.pairs.iter().collect();
    by_ratio.sort_by(|x, y| {
        let rx = x.ratio().unwrap_or(f64::INFINITY);
        let ry = y.ratio().unwrap_or(f64::INFINITY);
        ry.total_cmp(&rx)
            .then(y.b.bytes.cmp(&x.b.bytes))
            .then(x.key.cmp(&y.key))
    });
    by_ratio.truncate(TOP);
    let _ = writeln!(
        text,
        "\n## The {TOP} largest ratios\n\nA function whose frame in a is 0 bytes and in b is not comes first.\n"
    );
    pair_table(&mut text, &by_ratio, &several);

    let mut by_b: Vec<&Pair> = joined.pairs.iter().collect();
    by_b.sort_by(|x, y| y.b.bytes.cmp(&x.b.bytes).then(x.key.cmp(&y.key)));
    by_b.truncate(TOP);
    let _ = writeln!(text, "\n## The {TOP} largest frames in b\n");
    pair_table(&mut text, &by_b, &several);

    for (side, only) in [("a", &joined.only_a), ("b", &joined.only_b)] {
        let mut list: Vec<&(Key, Frame)> = only.iter().collect();
        list.sort_by(|x, y| y.1.bytes.cmp(&x.1.bytes).then(x.0.cmp(&y.0)));
        list.truncate(20);
        if !list.is_empty() {
            let _ = writeln!(text, "\n## The largest frames only in {side}\n");
            single_table(&mut text, &list, &several);
        }
    }

    for (side, m) in [("a", a), ("b", b)] {
        if !m.failures.is_empty() {
            let _ = writeln!(
                text,
                "\n## Compiles that failed in {side}\n\n{} of {} failed with `-fstack-usage`, the first 20:\n",
                m.failures.len(),
                m.units
            );
            for (unit, message) in m.failures.iter().take(20) {
                let _ = writeln!(text, "- {unit}: {}", message.replace('\n', " "));
            }
        }
    }
    text
}

/// The default report: `frames/<a>-vs-<b>.md` in the repository.
#[must_use]
pub fn default_out(root: &Path, a: &str, b: &str) -> PathBuf {
    root.join("frames").join(format!("{a}-vs-{b}.md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpg_shim::record::parse_log;

    fn frame(function: &str, bytes: u64, qualifier: Qualifier) -> Frame {
        Frame {
            function: function.to_string(),
            location: "x.c:1:1".to_string(),
            bytes,
            qualifier,
        }
    }

    fn key(file: &str, function: &str) -> Key {
        Key {
            file: file.to_string(),
            object: file.replace(".c", ".o"),
            function: function.to_string(),
        }
    }

    #[test]
    fn a_su_line_has_a_location_a_size_and_one_of_three_qualifiers() {
        let text = "/pg/src/backend/parser/gram.c:1234:1:base_yyparse\t1408\tstatic\n\
                    t.c:4:5:dyn\t32\tdynamic\n\
                    t.c:5:5:bnd\t48\tdynamic,bounded\n\n";
        let (frames, unreadable) = parse_su(text);
        assert_eq!(unreadable, 0);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].function, "base_yyparse");
        assert_eq!(frames[0].location, "/pg/src/backend/parser/gram.c:1234:1");
        assert_eq!(frames[0].bytes, 1408);
        assert_eq!(frames[0].qualifier, Qualifier::Static);
        assert_eq!(frames[1].qualifier, Qualifier::Dynamic);
        assert_eq!(frames[2].qualifier, Qualifier::Bounded);
        assert!(frames[2].qualifier.is_dynamic());
        assert_eq!(frames[2].qualifier.name(), "dynamic,bounded");
    }

    #[test]
    fn a_name_with_colons_and_spaces_stays_whole() {
        // What g++-16 writes for a function in a namespace and for a template instance.
        let f = parse_line("u.cpp:1:20:int ns::f(int)\t24\tstatic").unwrap();
        assert_eq!(f.function, "int ns::f(int)");
        assert_eq!(f.location, "u.cpp:1:20");
        let f = parse_line("u.cpp:1:105:T ns::g(T) [with T = long int]\t16\tstatic").unwrap();
        assert_eq!(f.function, "T ns::g(T) [with T = long int]");
        let f = parse_line("C:/pg/x.c:7:3:f\t8\tstatic").unwrap();
        assert_eq!(f.location, "C:/pg/x.c:7:3");
        assert_eq!(f.function, "f");
    }

    #[test]
    fn a_line_without_a_column_or_with_a_bad_field_is_handled() {
        let f = parse_line("x.c:7:f.isra.0\t8\tstatic").unwrap();
        assert_eq!(f.location, "x.c:7");
        assert_eq!(f.function, "f.isra.0");
        assert!(parse_line("x.c:7:1:f\t8\tsometimes").is_none());
        assert!(parse_line("x.c:7:1:f\tmany\tstatic").is_none());
        assert!(parse_line("x.c:7:1:\t8\tstatic").is_none());
        assert!(parse_line("f\t8\tstatic").is_none());
        let (frames, unreadable) = parse_su("garbage\nx.c:1:1:g\t16\tstatic\n");
        assert_eq!((frames.len(), unreadable), (1, 1));
    }

    /// A configure probe, a compile of a source, the same source again with -fPIC into another
    /// object, a compile of a generated file, and a link.
    const LOG: &str = r#"{"started":1.0,"argv":["/w/b/bin/cc","-c","-O2","conftest.c"],"compiler":"/usr/bin/gcc-16","cwd":"/w/b/build","inputs":[{"path":"conftest.c","sha256":"a"}],"wall-seconds":0.1,"exit":0}
{"started":2.0,"argv":["/w/b/bin/cc","-O2","-DFRONTEND","-c","-o","path.o","/w/src/REL_18_6/src/port/path.c"],"compiler":"/usr/bin/gcc-16","cwd":"/w/b/build/src/port","inputs":[{"path":"/w/src/REL_18_6/src/port/path.c","sha256":"c"}],"wall-seconds":0.1,"exit":0}
{"started":3.0,"argv":["/w/b/bin/cc","-O2","-fPIC","-c","-o","path_shlib.o","/w/src/REL_18_6/src/port/path.c"],"compiler":"/usr/bin/gcc-16","cwd":"/w/b/build/src/port","inputs":[{"path":"/w/src/REL_18_6/src/port/path.c","sha256":"c"}],"wall-seconds":0.1,"exit":0}
{"started":4.0,"argv":["/w/b/bin/cc","-O2","-MD","-MF","gram.o.d","-c","gram.c","-ogram.o"],"compiler":"/usr/bin/gcc-16","cwd":"/w/b/build/src/backend/parser","inputs":[{"path":"gram.c","sha256":"d"}],"wall-seconds":0.1,"exit":0}
{"started":5.0,"argv":["/w/b/bin/cc","-o","postgres","gram.o"],"compiler":"/usr/bin/gcc-16","cwd":"/w/b/build","inputs":[{"path":"gram.o","sha256":"e"}],"wall-seconds":0.1,"exit":0}
"#;

    #[test]
    fn every_object_of_a_file_is_its_own_unit_and_probes_are_left_out() {
        let (records, _) = parse_log(LOG);
        let build = Path::new("/w/b/build");
        let roots = [Path::new("/w/src/REL_18_6"), build];
        let found = units(&records, build, &roots, &|p| !p.ends_with("conftest.c"));
        let names: Vec<(&str, &str)> = found
            .iter()
            .map(|u| (u.file.as_str(), u.object.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("src/backend/parser/gram.c", "src/backend/parser/gram.o"),
                ("src/port/path.c", "src/port/path.o"),
                ("src/port/path.c", "src/port/path_shlib.o"),
            ]
        );
    }

    #[test]
    fn the_rerun_writes_to_the_scratch_object_and_no_dependency_file() {
        let (records, _) = parse_log(LOG);
        assert_eq!(
            frame_args(&records[3].argv[1..], "/tmp/f/unit.o"),
            [
                "-O2",
                "-c",
                "gram.c",
                "-fstack-usage",
                "-o",
                "/tmp/f/unit.o"
            ]
        );
        assert_eq!(
            frame_args(&records[1].argv[1..], "/tmp/f/unit.o"),
            [
                "-O2",
                "-DFRONTEND",
                "-c",
                "/w/src/REL_18_6/src/port/path.c",
                "-fstack-usage",
                "-o",
                "/tmp/f/unit.o"
            ]
        );
    }

    #[test]
    fn a_static_function_with_one_name_in_two_files_stays_two_entries() {
        let mut a = BTreeMap::new();
        a.insert(key("x.c", "helper"), frame("helper", 16, Qualifier::Static));
        a.insert(key("y.c", "helper"), frame("helper", 32, Qualifier::Static));
        a.insert(key("y.c", "gone"), frame("gone", 8, Qualifier::Static));
        let mut b = BTreeMap::new();
        b.insert(key("x.c", "helper"), frame("helper", 64, Qualifier::Static));
        b.insert(
            key("y.c", "helper"),
            frame("helper", 32, Qualifier::Dynamic),
        );
        b.insert(key("z.c", "new"), frame("new", 8, Qualifier::Static));
        let joined = join(&a, &b);
        assert_eq!(joined.pairs.len(), 2);
        assert_eq!(joined.pairs[0].ratio(), Some(4.0));
        assert_eq!(joined.pairs[1].ratio(), Some(1.0));
        assert_eq!(flags(&joined.pairs[1]), "b dynamic");
        assert_eq!(joined.only_a.len(), 1);
        assert_eq!(joined.only_a[0].0.function, "gone");
        assert_eq!(joined.only_b[0].0.function, "new");
    }

    #[test]
    fn a_gcc_clone_is_matched_by_the_name_before_the_dot() {
        let mut a = BTreeMap::new();
        a.insert(
            key("x.c", "walk.isra.0"),
            frame("walk.isra.0", 48, Qualifier::Static),
        );
        a.insert(
            key("x.c", "walk.part.0"),
            frame("walk.part.0", 16, Qualifier::Static),
        );
        let mut b = BTreeMap::new();
        b.insert(key("x.c", "walk"), frame("walk", 96, Qualifier::Static));
        let joined = join(&a, &b);
        assert_eq!(joined.pairs.len(), 1);
        let pair = &joined.pairs[0];
        assert!(pair.by_base);
        assert_eq!(pair.key.function, "walk");
        assert_eq!(pair.a.function, "walk.isra.0");
        assert_eq!(pair.part.as_ref().unwrap().function, "walk.part.0");
        assert_eq!(pair.a.bytes, 64);
        assert_eq!(pair.ratio(), Some(1.5));
        assert!(joined.only_a.is_empty());
        assert!(joined.only_b.is_empty());
        assert_eq!(
            base_name("T ns::g(T) [with T = long int]"),
            "T ns::g(T) [with T = long int]"
        );
    }

    #[test]
    fn a_split_function_counts_the_part_gcc_moved_out_of_it() {
        let mut a = BTreeMap::new();
        a.insert(key("x.c", "stop"), frame("stop", 8, Qualifier::Static));
        a.insert(
            key("x.c", "stop.part.0"),
            frame("stop.part.0", 2080, Qualifier::Static),
        );
        a.insert(
            key("y.c", "stop.part.0"),
            frame("stop.part.0", 64, Qualifier::Static),
        );
        a.insert(
            key("x.c", "copy.constprop.0"),
            frame("copy.constprop.0", 32, Qualifier::Static),
        );
        let mut b = BTreeMap::new();
        b.insert(key("x.c", "stop"), frame("stop", 2064, Qualifier::Static));
        b.insert(key("x.c", "copy"), frame("copy", 16, Qualifier::Static));
        let joined = join(&a, &b);
        let stop = joined
            .pairs
            .iter()
            .find(|p| p.key.function == "stop")
            .unwrap();
        assert!(!stop.by_base);
        assert_eq!(stop.a.bytes, 2088);
        assert_eq!(stop.part.as_ref().unwrap().bytes, 2080);
        assert!(flags(stop).contains("a with `stop.part.0` of 2080"));
        let copy = joined
            .pairs
            .iter()
            .find(|p| p.key.function == "copy")
            .unwrap();
        assert!(copy.part.is_none());
        assert_eq!(joined.only_a.len(), 1);
        assert_eq!(joined.only_a[0].0.file, "y.c");
        assert!(is_part("f.part.0") && is_part("f.isra.0.part.0"));
        assert!(!is_part("f.constprop.0") && !is_part("part") && !is_part("f(a.part)"));
    }

    #[test]
    fn percentiles_are_taken_by_nearest_rank() {
        let values: Vec<f64> = (1..=10).map(f64::from).collect();
        assert!((percentile(&values, 50.0) - 5.0).abs() < 1e-9);
        assert!((percentile(&values, 90.0) - 9.0).abs() < 1e-9);
        assert!((percentile(&values, 99.0) - 10.0).abs() < 1e-9);
        assert!((percentile(&values, 0.0) - 1.0).abs() < 1e-9);
        assert!(percentile(&[], 50.0).is_nan());
    }

    #[test]
    fn the_statistics_leave_out_empty_frames_in_a_and_count_the_buckets() {
        let pair = |a: u64, b: u64| Pair {
            key: key("x.c", &format!("f{a}_{b}")),
            a: frame("f", a, Qualifier::Static),
            b: frame("f", b, Qualifier::Static),
            by_base: false,
            part: None,
        };
        let pairs = [
            pair(16, 16),
            pair(16, 8),
            pair(16, 24),
            pair(16, 64),
            pair(16, 160),
            pair(0, 32),
        ];
        let s = stats(&pairs).unwrap();
        assert_eq!(s.count, 5);
        assert!((s.median - 1.5).abs() < 1e-9);
        assert!((s.max - 10.0).abs() < 1e-9);
        assert_eq!(s.buckets, [1, 1, 1, 0, 1, 1]);
        assert!(stats(&[pair(0, 8)]).is_none());
    }

    #[test]
    fn the_report_names_both_sides_and_ranks_the_worst_first() {
        let side = |name: &str, bytes: u64| {
            let mut m = Measured {
                name: name.to_string(),
                compiler: "/usr/bin/cc".to_string(),
                version: format!("{name} 1.0"),
                units: 2,
                ..Measured::default()
            };
            m.frames
                .insert(key("x.c", "f"), frame("f", 16, Qualifier::Static));
            m.frames
                .insert(key("y.c", "g|h"), frame("g|h", bytes, Qualifier::Bounded));
            m
        };
        let a = side("gcc", 32);
        let mut b = side("rucc", 128);
        b.failures
            .push(("z.c (z.o)".to_string(), "exited 1".to_string()));
        let joined = join(&a.frames, &b.frames);
        let text = report(&a, &b, &joined, "2026-09-28", "gpc");
        assert!(text.starts_with("# Stack frames, gcc against rucc\n"));
        assert!(text.contains("| compiler | gcc 1.0 | rucc 1.0 |"));
        assert!(text.contains("2 functions are on both sides, 0 only in a and 0 only in b."));
        assert!(text.contains("| 1.00 | 4.00 | 4.00 | 4.00 |"));
        assert!(text.contains(
            "| 1 | `g\\|h` | y.c | 32 | 128 | 4.00 | a dynamic,bounded, b dynamic,bounded |"
        ));
        assert!(text.contains("## Compiles that failed in b"));
        assert!(text.contains("- z.c (z.o): exited 1"));
        assert_eq!(
            default_out(Path::new("/r"), "A", "B"),
            Path::new("/r/frames/A-vs-B.md")
        );
    }
}
