//! `rpg mixed`: find the translation unit a failure comes from, then the transformation.
//!
//! It takes two finished builds of the same pin, configuration and build system, one by gcc and
//! one by rucc, and works in the gcc build tree. For each trial it copies a chosen set of rucc's
//! objects over gcc's, lets the build system relink what depends on them, and runs the check.
//! The check is a suite, which fails when a test fails that failed with every object from rucc
//! and passed with every object from gcc, or a shell command, which fails when it exits non zero.
//!
//! The objects are put in order by path, and the search finds the smallest `k` for which the
//! first `k` of them from rucc fail. That object is the one, given the ones before it. A second
//! trial with only that object from rucc says whether it fails on its own. When it does not, the
//! ones before it stay in for the rest of the search, and the report says so.
//!
//! With the object found, and rucc having built it, the file is compiled again with
//! `-fpass-fuel-global=<n>`, which stops the optimizer after `n` transformations. Zero passing and
//! no limit failing, the search goes up by doubling and then halves back down to the first `n`
//! that fails. The file is then compiled with `n - 1` and with `n` and `-fdump-ir=all`, from its
//! preprocessed text in a scratch directory, and the first dump that differs is written out as a
//! diff. When no transformations at all still fail, the fault is in lowering or code generation
//! and not in a pass, and that is what the report says.
//!
//! Every gcc object is copied aside before the first trial and put back after the last, and the
//! tree is built once more, so the gcc build is the same afterwards as before.

use crate::build::{BuildInfo, environment};
use crate::frames::units;
use crate::process::Step;
use crate::records::Outcome;
use crate::repro::{compile_args, is_dependency_option, names, preprocess_args};
use crate::settings::System;
use crate::suite::{self, SuitePlan};
use rpg_shim::record::{CompileRecord, read_log};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

/// The most transformations the fuel search tries before giving up, a little over a million.
const FUEL_CAP: u32 = 1 << 20;

/// What decides whether a trial passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// A suite `rpg test` knows.
    Suite(String),
    /// A shell command, run in the gcc build tree.
    Command(String),
}

/// What a run needs.
#[derive(Debug, Clone)]
pub struct MixedPlan {
    /// The directory `rpg build` wrote for gcc.
    pub gcc: PathBuf,
    /// The one it wrote for rucc.
    pub rucc: PathBuf,
    /// The check.
    pub check: Check,
    /// Only objects whose C file starts with one of these, or all of them when empty.
    pub under: Vec<String>,
    /// Where the report, the logs and the IR go.
    pub out: PathBuf,
    /// `PG_TEST_TIMEOUT_DEFAULT` for a suite.
    pub timeout: u32,
    /// Whether to go on from the object to the transformation.
    pub fuel: bool,
}

/// One object both builds made.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The object, relative to the build tree.
    pub object: String,
    /// The C file, relative to the source or build tree.
    pub file: String,
    /// The argument that named the file in rucc's call.
    pub input: String,
    /// rucc's call.
    pub record: CompileRecord,
}

/// One trial.
#[derive(Debug, Clone)]
pub struct Trial {
    /// What was tried, in words.
    pub what: String,
    /// Whether it failed the check.
    pub bad: bool,
    /// Seconds, the build and the check.
    pub seconds: f64,
}

/// What the search found.
#[derive(Debug, Clone, Default)]
pub struct Found {
    /// The object, when the search got that far.
    pub culprit: Option<Candidate>,
    /// Whether it fails with every other object from gcc.
    pub alone: bool,
    /// How many objects before it stayed from rucc, when it does not fail alone.
    pub kept: usize,
    /// The first transformation that fails, when the fuel search found one.
    pub transformation: Option<u32>,
    /// Zero transformations fail as well.
    pub not_a_pass: bool,
    /// Every transformation up to the cap passed.
    pub past_cap: bool,
    /// The first IR dump that differs between `n - 1` and `n`.
    pub dump: Option<String>,
    /// Where the diff of that dump went.
    pub diff: Option<PathBuf>,
}

/// The objects both builds made from the same file, in order by object, with rucc's call.
#[must_use]
pub fn candidates(
    gcc: &[CompileRecord],
    gcc_build: &Path,
    rucc: &[CompileRecord],
    rucc_build: &Path,
    source: &Path,
    under: &[String],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Candidate> {
    let ours: BTreeSet<(String, String)> = units(gcc, gcc_build, &[gcc_build, source], exists)
        .into_iter()
        .map(|u| (u.object, u.file))
        .collect();
    let mut out: BTreeMap<String, Candidate> = BTreeMap::new();
    for unit in units(rucc, rucc_build, &[rucc_build, source], exists) {
        if unit.object.starts_with('/') || !ours.contains(&(unit.object.clone(), unit.file.clone()))
        {
            continue;
        }
        if !under.is_empty() && !under.iter().any(|p| unit.file.starts_with(p.as_str())) {
            continue;
        }
        let Some((_, input)) = names(unit.record, &[rucc_build, source])
            .into_iter()
            .find(|(name, _)| *name == unit.file)
        else {
            continue;
        };
        out.insert(
            unit.object.clone(),
            Candidate {
                object: unit.object,
                file: unit.file,
                input,
                record: unit.record.clone(),
            },
        );
    }
    out.into_values().collect()
}

/// The smallest `k` in `1..=n` for which `bad(k)` holds, given that `bad(0)` does not and
/// `bad(n)` does. Anything the test returns as an error ends the search.
pub fn first_bad(
    n: usize,
    bad: &mut dyn FnMut(usize) -> Result<bool, String>,
) -> Result<usize, String> {
    let (mut good, mut worse) = (0, n);
    while worse - good > 1 {
        let mid = good + (worse - good) / 2;
        if bad(mid)? {
            worse = mid;
        } else {
            good = mid;
        }
    }
    Ok(worse)
}

/// The first transformation count that fails, given that no limit at all fails. `Ok(None)` when
/// every count up to `cap` passes, and `Ok(Some(0))` when no transformations at all fail.
pub fn first_bad_fuel(
    cap: u32,
    bad: &mut dyn FnMut(u32) -> Result<bool, String>,
) -> Result<Option<u32>, String> {
    if bad(0)? {
        return Ok(Some(0));
    }
    let mut good = 0;
    let mut try_ = 1;
    loop {
        if bad(try_)? {
            break;
        }
        good = try_;
        if try_ >= cap {
            return Ok(None);
        }
        try_ = (try_ * 2).min(cap);
    }
    let mut worse = try_;
    while worse - good > 1 {
        let mid = good + (worse - good) / 2;
        if bad(mid)? {
            worse = mid;
        } else {
            good = mid;
        }
    }
    Ok(Some(worse))
}

/// The recorded arguments without the output and the dependency options, plus `extra` and
/// `-o object`.
#[must_use]
pub fn recompile_args(rest: &[String], object: &str, extra: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = rest.iter();
    while let Some(arg) = words.next() {
        if matches!(arg.as_str(), "-o" | "-MF" | "-MT" | "-MQ") {
            words.next();
            continue;
        }
        if (arg.starts_with("-o") && arg.len() > 2) || is_dependency_option(arg) {
            continue;
        }
        out.push(arg.clone());
    }
    out.extend(extra.iter().cloned());
    out.extend(["-o".to_string(), object.to_string()]);
    out
}

/// Run a recorded call's compiler again with other arguments, in `cwd`, with the recorded
/// environment apart from PATH, which puts the shim first.
fn run_compiler(record: &CompileRecord, args: &[String], cwd: &Path) -> Result<(), String> {
    let output = Command::new(&record.compiler)
        .args(args)
        .current_dir(cwd)
        .envs(record.env.iter().filter(|(k, _)| *k != "PATH"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", record.compiler))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{} exited {}: {}",
        record.compiler,
        output.status,
        stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
    ))
}

/// Which compiler an object in the gcc tree came from right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Gcc,
    Rucc,
    /// Compiled by this command with a fuel limit.
    Limited,
}

/// The gcc build tree and the state of every candidate object in it.
struct Tree<'a> {
    gcc_build: PathBuf,
    rucc_build: PathBuf,
    kept: PathBuf,
    candidates: &'a [Candidate],
    sides: Vec<Side>,
}

/// Copy a file and give the copy the time now, so the build system sees it as new.
fn copy_fresh(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map_err(|e| format!("copying {} to {}: {e}", from.display(), to.display()))?;
    std::fs::File::options()
        .write(true)
        .open(to)
        .and_then(|f| f.set_modified(SystemTime::now()))
        .map_err(|e| format!("touching {}: {e}", to.display()))
}

impl Tree<'_> {
    /// Copy every gcc object aside.
    fn keep(&self) -> Result<(), String> {
        for c in self.candidates {
            let to = self.kept.join(&c.object);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("creating {}: {e}", parent.display()))?;
            }
            std::fs::copy(self.gcc_build.join(&c.object), &to)
                .map_err(|e| format!("keeping {}: {e}", c.object))?;
        }
        Ok(())
    }

    /// Put rucc's object where `rucc(i)` holds and gcc's everywhere else, copying only what
    /// changes.
    fn place(&mut self, rucc: &dyn Fn(usize) -> bool) -> Result<(), String> {
        for (i, c) in self.candidates.iter().enumerate() {
            let want = if rucc(i) { Side::Rucc } else { Side::Gcc };
            if self.sides[i] == want {
                continue;
            }
            let from = match want {
                Side::Rucc => self.rucc_build.join(&c.object),
                _ => self.kept.join(&c.object),
            };
            copy_fresh(&from, &self.gcc_build.join(&c.object))?;
            self.sides[i] = want;
        }
        Ok(())
    }

    /// Compile one candidate with rucc and a fuel limit straight into the gcc tree.
    fn limit(&mut self, i: usize, fuel: u32) -> Result<(), String> {
        let c = &self.candidates[i];
        let object = self.gcc_build.join(&c.object);
        let rest = c.record.argv.get(1..).unwrap_or_default();
        let args = recompile_args(
            rest,
            &object.display().to_string(),
            &[format!("-fpass-fuel-global={fuel}")],
        );
        run_compiler(&c.record, &args, Path::new(&c.record.cwd))?;
        self.sides[i] = Side::Limited;
        Ok(())
    }
}

/// Runs trials against one tree.
struct Runner<'a> {
    plan: &'a MixedPlan,
    info: BuildInfo,
    system: System,
    env: BTreeMap<String, String>,
    unset: Vec<String>,
    /// Tests that fail with every object from rucc and pass with every object from gcc.
    watched: BTreeSet<String>,
    trials: Vec<Trial>,
}

impl Runner<'_> {
    /// Relink, run the check, and say whether it failed, with the failing tests of a suite.
    fn run(&mut self, what: &str) -> Result<(bool, BTreeSet<String>), String> {
        let n = self.trials.len() + 1;
        let dir = self.plan.out.join(format!("trial-{n}"));
        let clock = Instant::now();
        let build_dir = PathBuf::from(&self.info.build_dir);
        let mut step = match self.system {
            System::Meson => Step::new("ninja", "ninja", &dir, &dir.join("build.log")).args([
                "-C".to_string(),
                build_dir.display().to_string(),
                "-j".to_string(),
                self.info.jobs.to_string(),
            ]),
            System::Autoconf => Step::new(
                "make world-bin",
                crate::process::make(),
                &build_dir,
                &dir.join("build.log"),
            )
            .args([format!("-j{}", self.info.jobs), "world-bin".to_string()]),
        }
        .envs(&self.env);
        step.unset.clone_from(&self.unset);
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        if !step.run()?.ok {
            return Err(format!(
                "trial {n} ({what}) did not build, see {}",
                dir.join("build.log").display()
            ));
        }
        let (bad, failing) = match &self.plan.check {
            Check::Command(command) => {
                let log = dir.join("check.log");
                let mut check = Step::new("check", "sh", &self.plan.gcc, &log)
                    .args(["-c".to_string(), command.clone()])
                    .envs(&self.env);
                check
                    .env
                    .insert("RPG_BUILD".to_string(), build_dir.display().to_string());
                check.unset.clone_from(&self.unset);
                (!check.run()?.ok, BTreeSet::new())
            }
            Check::Suite(name) => {
                let plan = SuitePlan {
                    out: self.plan.gcc.clone(),
                    info: self.info.clone(),
                    suite: name.clone(),
                    row: None,
                    timeout: self.plan.timeout,
                    baseline: None,
                    run: u32::try_from(n).unwrap_or(u32::MAX),
                    label: Some(format!("mixed-{name}")),
                };
                let records = suite::run(&plan)?.records;
                if records.is_empty() {
                    return Err(format!(
                        "trial {n} ({what}) ran no tests of {name}, see the suite log under {}",
                        self.plan.gcc.display()
                    ));
                }
                eprintln!("rpg: trial {n} ran {} tests of {name}", records.len());
                let failing: BTreeSet<String> = records
                    .into_iter()
                    .filter(|r| !matches!(r.outcome, Outcome::Passed | Outcome::Skipped))
                    .map(|r| r.test)
                    .collect();
                (failing.iter().any(|t| self.watched.contains(t)), failing)
            }
        };
        let seconds = clock.elapsed().as_secs_f64();
        eprintln!(
            "rpg: trial {n}, {what}: {} in {seconds:.0}s",
            if bad { "fails" } else { "passes" }
        );
        self.trials.push(Trial {
            what: what.to_string(),
            bad,
            seconds,
        });
        Ok((bad, failing))
    }
}

/// What `rpg mixed` produced.
#[derive(Debug, Clone)]
pub struct Mixed {
    /// The objects it chose among.
    pub candidates: usize,
    /// The tests it watched, for a suite.
    pub watched: BTreeSet<String>,
    /// Every trial.
    pub trials: Vec<Trial>,
    /// What it found.
    pub found: Found,
}

/// Run the search, and put the gcc tree back however it ends.
pub fn mixed(plan: &MixedPlan) -> Result<Mixed, String> {
    let gcc_info = BuildInfo::load(&plan.gcc)?;
    let rucc_info = BuildInfo::load(&plan.rucc)?;
    for (what, a, b) in [
        ("pin", &gcc_info.pin, &rucc_info.pin),
        ("configuration", &gcc_info.config, &rucc_info.config),
        ("build system", &gcc_info.system, &rucc_info.system),
    ] {
        if a != b {
            return Err(format!(
                "the two builds differ in {what}, {a} and {b}, so their objects do not mix"
            ));
        }
    }
    let system = System::parse(&gcc_info.system)?;
    let gcc_build = PathBuf::from(&gcc_info.build_dir);
    let rucc_build = PathBuf::from(&rucc_info.build_dir);
    let source = PathBuf::from(&rucc_info.source);
    let (gcc_records, _) = read_log(&plan.gcc.join("compile.jsonl"))
        .map_err(|e| format!("reading the gcc build's compile.jsonl: {e}"))?;
    let (rucc_records, _) = read_log(&plan.rucc.join("compile.jsonl"))
        .map_err(|e| format!("reading the rucc build's compile.jsonl: {e}"))?;
    let list = candidates(
        &gcc_records,
        &gcc_build,
        &rucc_records,
        &rucc_build,
        &source,
        &plan.under,
        &|p| p.is_file(),
    )
    .into_iter()
    .filter(|c| gcc_build.join(&c.object).is_file() && rucc_build.join(&c.object).is_file())
    .collect::<Vec<_>>();
    if list.is_empty() {
        return Err("the two builds have no object in common to choose among".to_string());
    }
    eprintln!("rpg: {} objects to choose among", list.len());

    std::fs::create_dir_all(&plan.out)
        .map_err(|e| format!("creating {}: {e}", plan.out.display()))?;
    let mut tree = Tree {
        gcc_build,
        rucc_build,
        kept: plan.out.join("gcc-objects"),
        candidates: &list,
        sides: vec![Side::Gcc; list.len()],
    };
    tree.keep()?;
    let (env, unset) = environment(&plan.gcc);
    let mut runner = Runner {
        plan,
        info: gcc_info,
        system,
        env,
        unset,
        watched: BTreeSet::new(),
        trials: Vec::new(),
    };
    let rucc_is_rucc = rucc_info.kind == "rucc";
    let searched = search(plan, &mut tree, &mut runner, rucc_is_rucc);

    // Back to gcc everywhere, whatever happened, and build once more so what was linked from the
    // mixed objects is linked again from gcc's.
    let restored = tree.place(&|_| false).and_then(|()| {
        let dir = plan.out.join("restore");
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let build_dir = PathBuf::from(&runner.info.build_dir);
        let mut step = match system {
            System::Meson => Step::new("ninja", "ninja", &dir, &dir.join("build.log"))
                .args(["-C".to_string(), build_dir.display().to_string()]),
            System::Autoconf => Step::new(
                "make world-bin",
                crate::process::make(),
                &build_dir,
                &dir.join("build.log"),
            )
            .args([format!("-j{}", runner.info.jobs), "world-bin".to_string()]),
        }
        .envs(&runner.env);
        step.unset.clone_from(&runner.unset);
        if step.run()?.ok {
            Ok(())
        } else {
            Err("putting the gcc objects back, the build failed".to_string())
        }
    });
    let found = searched?;
    restored?;
    Ok(Mixed {
        candidates: list.len(),
        watched: runner.watched,
        trials: runner.trials,
        found,
    })
}

fn search(
    plan: &MixedPlan,
    tree: &mut Tree,
    runner: &mut Runner,
    rucc_is_rucc: bool,
) -> Result<Found, String> {
    let n = tree.candidates.len();
    let (gcc_bad, gcc_failing) = runner.run("every object from gcc")?;
    tree.place(&|_| true)?;
    let (rucc_bad, rucc_failing) = runner.run("every object from rucc")?;
    match &plan.check {
        Check::Command(_) => {
            if gcc_bad {
                return Err("the check fails with every object from gcc".to_string());
            }
            if !rucc_bad {
                return Err(
                    "the check passes with every object from rucc, so there is nothing to look for"
                        .to_string(),
                );
            }
        }
        Check::Suite(_) => {
            runner.watched = rucc_failing.difference(&gcc_failing).cloned().collect();
            if runner.watched.is_empty() {
                return Err(
                    "every test that fails with rucc's objects fails with gcc's as well, so there \
                     is nothing to look for"
                        .to_string(),
                );
            }
            eprintln!(
                "rpg: watching {}",
                runner
                    .watched
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    let k = first_bad(n, &mut |k| {
        tree.place(&|i| i < k)?;
        runner
            .run(&format!("the first {k} of {n} from rucc"))
            .map(|(bad, _)| bad)
    })?;
    let index = k - 1;
    let culprit = tree.candidates[index].clone();
    eprintln!(
        "rpg: the object is {} from {}",
        culprit.object, culprit.file
    );
    tree.place(&|i| i == index)?;
    let (alone, _) = runner.run(&format!("only {} from rucc", culprit.object))?;
    let before = if alone { 0 } else { index };
    let mut found = Found {
        culprit: Some(culprit.clone()),
        alone,
        kept: before,
        ..Found::default()
    };
    if !plan.fuel || !rucc_is_rucc {
        return Ok(found);
    }

    let fuel = first_bad_fuel(FUEL_CAP, &mut |fuel| {
        tree.place(&|i| i < before)?;
        tree.limit(index, fuel)?;
        runner
            .run(&format!("{} at -fpass-fuel-global={fuel}", culprit.object))
            .map(|(bad, _)| bad)
    })?;
    match fuel {
        None => found.past_cap = true,
        Some(0) => found.not_a_pass = true,
        Some(fuel) => {
            found.transformation = Some(fuel);
            let (dump, diff) = ir_diff(&culprit, fuel, &plan.out.join("ir"))?;
            found.dump = dump;
            found.diff = diff;
        }
    }
    Ok(found)
}

/// Compile the file with `fuel - 1` and `fuel` transformations and every IR dump, and write the
/// first dump that differs as a diff. Dumps that are the same on both sides are removed.
fn ir_diff(
    c: &Candidate,
    fuel: u32,
    dir: &Path,
) -> Result<(Option<String>, Option<PathBuf>), String> {
    std::fs::remove_dir_all(dir).ok();
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let rest = c.record.argv.get(1..).unwrap_or_default();
    let i = dir.join("unit.i");
    run_compiler(
        &c.record,
        &preprocess_args(rest, &i),
        Path::new(&c.record.cwd),
    )?;
    let sides = [("before", fuel - 1), ("after", fuel)];
    for (side, count) in sides {
        let at = dir.join(side);
        std::fs::create_dir_all(&at).map_err(|e| format!("creating {}: {e}", at.display()))?;
        let mut args = compile_args(rest, &c.input, "../unit.i", "unit.o");
        args.extend([
            format!("-fpass-fuel-global={count}"),
            "-fdump-ir=all".to_string(),
        ]);
        run_compiler(&c.record, &args, &at)?;
    }
    let listing = |side: &str| -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir.join(side))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "ir"))
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    };
    let before = listing("before");
    let after: BTreeSet<String> = listing("after").into_iter().collect();
    let mut first = None;
    for name in before.iter().filter(|n| after.contains(*n)) {
        let a = std::fs::read(dir.join("before").join(name)).unwrap_or_default();
        let b = std::fs::read(dir.join("after").join(name)).unwrap_or_default();
        if a == b {
            std::fs::remove_file(dir.join("before").join(name)).ok();
            std::fs::remove_file(dir.join("after").join(name)).ok();
        } else if first.is_none() {
            first = Some(name.clone());
        }
    }
    let Some(name) = first else {
        return Ok((None, None));
    };
    let output = Command::new("diff")
        .args(["-u", &format!("before/{name}"), &format!("after/{name}")])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("could not run diff: {e}"))?;
    let path = dir.join("first.diff");
    std::fs::write(&path, &output.stdout)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok((Some(name), Some(path)))
}

/// The report, in markdown.
#[must_use]
pub fn report(plan: &MixedPlan, run: &Mixed) -> String {
    let mut text = String::from("# rpg mixed\n\n");
    let _ = writeln!(text, "gcc build: `{}`\n", plan.gcc.display());
    let _ = writeln!(text, "rucc build: `{}`\n", plan.rucc.display());
    let _ = match &plan.check {
        Check::Suite(name) => writeln!(text, "Check: the `{name}` suite."),
        Check::Command(command) => writeln!(text, "Check: `{command}`."),
    };
    let _ = writeln!(text, "\n{} objects to choose among.\n", run.candidates);
    if !run.watched.is_empty() {
        let names: Vec<String> = run.watched.iter().map(|t| format!("`{t}`")).collect();
        let _ = writeln!(
            text,
            "Tests that fail with rucc's objects and pass with gcc's: {}.\n",
            names.join(", ")
        );
    }
    let found = &run.found;
    text.push_str("## Found\n\n");
    if let Some(c) = &found.culprit {
        let _ = writeln!(text, "The object is `{}`, from `{}`.", c.object, c.file);
        if found.alone {
            text.push_str(" It fails with every other object from gcc.\n");
        } else {
            let _ = writeln!(
                text,
                " It does not fail on its own, only with the {} objects before it from rucc as \
                 well, which stayed in for the rest of the search.",
                found.kept
            );
        }
    }
    if found.not_a_pass {
        text.push_str(
            "\nWith no transformations at all it still fails, so the fault is in lowering or \
             code generation and not in an optimization pass.\n",
        );
    }
    if found.past_cap {
        let _ = writeln!(
            text,
            "\nEvery limit up to {FUEL_CAP} transformations passes, and no limit fails, so the \
             file does more than that or the limit changes something besides the passes."
        );
    }
    if let Some(n) = found.transformation {
        let _ = writeln!(
            text,
            "\nThe first failing limit is `-fpass-fuel-global={n}`, so transformation {n} is the \
             one."
        );
        match (&found.dump, &found.diff) {
            (Some(dump), Some(diff)) => {
                let _ = writeln!(
                    text,
                    "The first IR dump that differs between {} and {n} is `{dump}`, and the diff \
                     is in `{}`:\n",
                    n - 1,
                    diff.display()
                );
                let body = std::fs::read_to_string(diff).unwrap_or_default();
                text.push_str("```diff\n");
                for line in body.lines().take(80) {
                    text.push_str(line);
                    text.push('\n');
                }
                text.push_str("```\n");
            }
            _ => text.push_str("No IR dump differs between the two limits.\n"),
        }
    }
    text.push_str("\n## Trials\n\n| # | what | result | seconds |\n|---|---|---|---|\n");
    for (i, t) in run.trials.iter().enumerate() {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {:.0} |",
            i + 1,
            t.what,
            if t.bad { "fails" } else { "passes" },
            t.seconds
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_bad_prefix_is_found_in_log_steps() {
        for culprit in 1..=37 {
            let mut asked = 0;
            let k = first_bad(37, &mut |k| {
                asked += 1;
                Ok(k >= culprit)
            })
            .unwrap();
            assert_eq!(k, culprit);
            assert!(asked <= 6, "{asked} trials for {culprit}");
        }
        assert_eq!(first_bad(1, &mut |_| unreachable!()).unwrap(), 1);
    }

    #[test]
    fn an_error_ends_the_search() {
        let e = first_bad(8, &mut |_| Err("no".to_string())).unwrap_err();
        assert_eq!(e, "no");
    }

    #[test]
    fn the_fuel_search_doubles_then_halves() {
        for culprit in [1, 2, 3, 5, 64, 65, 1000, 70_001] {
            let mut tried = Vec::new();
            let n = first_bad_fuel(FUEL_CAP, &mut |f| {
                tried.push(f);
                Ok(f >= culprit)
            })
            .unwrap();
            assert_eq!(n, Some(culprit), "{tried:?}");
        }
        assert_eq!(
            first_bad_fuel(FUEL_CAP, &mut |_| Ok(true)).unwrap(),
            Some(0)
        );
        assert_eq!(first_bad_fuel(100, &mut |_| Ok(false)).unwrap(), None);
        assert_eq!(
            first_bad_fuel(100, &mut |f| Ok(f >= 100)).unwrap(),
            Some(100)
        );
    }

    #[test]
    fn the_recompile_drops_the_output_and_dependencies_and_adds_the_limit() {
        let rest: Vec<String> = [
            "-O2", "-MD", "-MQ", "x.o", "-MF", "x.o.d", "-o", "x.o", "-c", "x.c",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let args = recompile_args(&rest, "/t/x.o", &["-fpass-fuel-global=7".to_string()]);
        assert_eq!(
            args,
            ["-O2", "-c", "x.c", "-fpass-fuel-global=7", "-o", "/t/x.o"]
        );
    }

    fn record(cwd: &str, argv: &[&str]) -> CompileRecord {
        let text = serde_json::json!({
            "started": 1.0,
            "argv": argv,
            "compiler": "/usr/bin/cc",
            "cwd": cwd,
            "inputs": [{"path": argv[argv.len() - 1], "sha256": ""}],
            "wall-seconds": 0.1,
            "exit": 0,
            "stderr": "",
        });
        serde_json::from_value(text).unwrap()
    }

    #[test]
    fn candidates_are_objects_both_builds_made_from_the_same_file() {
        let gcc = [
            record("/g/build", &["cc", "-c", "-o", "a.o", "/src/a.c"]),
            record("/g/build", &["cc", "-c", "-o", "b.o", "/src/b.c"]),
        ];
        let rucc = [
            record("/r/build", &["cc", "-c", "-o", "b.o", "/src/b.c"]),
            record("/r/build", &["cc", "-c", "-o", "a.o", "/src/a.c"]),
            record("/r/build", &["cc", "-c", "-o", "c.o", "/src/c.c"]),
        ];
        let found = candidates(
            &gcc,
            Path::new("/g/build"),
            &rucc,
            Path::new("/r/build"),
            Path::new("/src"),
            &[],
            &|_| true,
        );
        let objects: Vec<&str> = found.iter().map(|c| c.object.as_str()).collect();
        assert_eq!(objects, ["a.o", "b.o"]);
        assert_eq!(found[0].file, "a.c");
        assert_eq!(found[0].input, "/src/a.c");
        assert_eq!(found[0].record.cwd, "/r/build");

        let only = candidates(
            &gcc,
            Path::new("/g/build"),
            &rucc,
            Path::new("/r/build"),
            Path::new("/src"),
            &["b".to_string()],
            &|_| true,
        );
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].object, "b.o");
    }
}
