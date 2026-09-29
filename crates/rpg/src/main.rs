//! The `rpg` command line: fetch, build and test the pinned Postgres tree, and record what happened.

mod asmaudit;
mod baseline;
mod build;
mod cli;
mod compiler;
mod configdiff;
mod cross;
mod demands;
mod frames;
mod pins;
mod process;
mod records;
mod regress;
mod repo;
mod repro;
mod settings;
mod stress;
mod suite;
mod triage;

use cli::Args;
use compiler::Compiler;
use repo::Repo;
use settings::{BuildConfig, Level, Rows, System};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let result = Args::parse(&words).and_then(|args| {
        if args.command == "help" || args.has("help") {
            print!("{}", cli::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        let repo = Repo::find()?;
        match args.command.as_str() {
            "fetch" => fetch(&repo, &args),
            "build" => build_command(&repo, &args),
            "test" => test_command(&repo, &args),
            "baseline" => baseline_command(&repo, &args),
            "demands" => demands_command(&repo, &args),
            "config-diff" => config_diff(&repo, &args),
            "repro" => repro_command(&args),
            "asm-audit" => asm_audit_command(&repo, &args),
            "frames" => frames_command(&repo, &args),
            "cross-modules" => cross_command(&args),
            "stress" => stress_command(&repo, &args),
            "triage" => triage_command(&repo, &args),
            _ => unreachable!("the parser only accepts known commands"),
        }
    });
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("rpg: {message}");
            ExitCode::from(2)
        }
    }
}

fn load_pin(repo: &Repo, args: &Args) -> Result<pins::Pin, String> {
    Ok(pins::Pins::load(&repo.pins())?
        .get(args.get("pin"))?
        .clone()
        .with_fetched_commit())
}

fn fetch(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let pin = load_pin(repo, args)?;
    if pin.branch.is_none() {
        eprintln!(
            "rpg: {} is Postgres {} at {}",
            pin.name, pin.version, pin.commit
        );
    }
    let source = pins::fetch(&pin, !args.has("no-upstream-check"))?;
    println!("{}", source.display());
    Ok(ExitCode::SUCCESS)
}

/// The source tree of a pin, which `rpg fetch` must have unpacked.
fn source_of(pin: &pins::Pin) -> Result<PathBuf, String> {
    let source = pin.source_dir();
    if source.join("configure").is_file() {
        Ok(source)
    } else {
        Err(format!(
            "{} is not unpacked at {}; run rpg fetch first",
            pin.name,
            source.display()
        ))
    }
}

struct BuildChoice {
    system: System,
    level: Level,
    config: String,
}

fn plan(
    repo: &Repo,
    args: &Args,
    cc: &str,
    choice: &BuildChoice,
    out: Option<&str>,
) -> Result<build::Plan, String> {
    let pin = load_pin(repo, args)?;
    let source = source_of(&pin)?;
    let config = BuildConfig::load(&repo.config(&choice.config))?;
    let compiler = Compiler::identify(cc)?;
    let out = out.map_or_else(
        || {
            build::default_out(
                &repo.work(),
                &pin.name,
                &config.name,
                choice.system,
                choice.level,
                &compiler.short_name(),
            )
        },
        PathBuf::from,
    );
    std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let out = std::fs::canonicalize(&out).unwrap_or(out);
    Ok(build::Plan {
        pin,
        source,
        config,
        system: choice.system,
        level: choice.level,
        compiler,
        out,
        jobs: args.number("jobs", process::cores())?,
        twice: args.has("twice"),
        configure_only: args.has("configure-only"),
    })
}

fn print_build(info: &build::BuildInfo, out: &Path) {
    let c = &info.compiles;
    println!("build:     {}", out.display());
    println!("compiler:  {} ({})", info.compiler, info.cc);
    println!(
        "phase:     {:?}, configure {:.1}s, build {}",
        info.phase,
        info.configure_seconds,
        info.build_seconds
            .map_or_else(|| "not run".to_string(), |s| format!("{s:.1}s"))
    );
    println!(
        "compiles:  {} probe calls ({} failed), {} build calls with {} compiles ({} failed), {:.1} user seconds, peak {} KiB{}",
        c.probe_calls,
        c.probe_failures,
        c.build_calls,
        c.build_compiles,
        c.build_failures,
        c.build_user_seconds,
        c.peak_rss_kb,
        c.peak_rss_file
            .as_ref()
            .map_or_else(String::new, |f| format!(" in {f}"))
    );
    if let Some(n) = c.nondeterministic {
        println!("twice:     {n} compiles gave different output on a second run");
    }
    if let Some(error) = &info.first_error {
        println!("stopped:   {error}");
    }
}

fn build_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let choice = BuildChoice {
        system: System::parse(args.get("system").unwrap_or("meson"))?,
        level: Level::parse(args.get("level").unwrap_or("-O2"))?,
        config: args.get("config").unwrap_or("minimal").to_string(),
    };
    let plan = plan(repo, args, args.need("cc")?, &choice, args.get("out"))?;
    let info = build::build(&plan)?;
    print_build(&info, &plan.out);
    Ok(
        if matches!(info.phase, build::Phase::Built | build::Phase::Configured) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
    )
}

/// The build directory to test: `--out`, or the only one under `work/`.
fn pick_out(repo: &Repo, args: &Args) -> Result<PathBuf, String> {
    if let Some(out) = args.get("out") {
        return Ok(PathBuf::from(out));
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(repo.work())
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.join("build.json").is_file())
                .collect()
        })
        .unwrap_or_default();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("no build under work/; run rpg build first, or give --out".to_string()),
        _ => {
            found.sort();
            let names: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
            Err(format!(
                "several builds under work/, pick one with --out:\n  {}",
                names.join("\n  ")
            ))
        }
    }
}

/// `PG_TEST_TIMEOUT_DEFAULT`: `--timeout`, else the row's, else 180, and twice that at `-O0`.
fn timeout(args: &Args, row: Option<&settings::Row>, level: &str) -> Result<u32, String> {
    let base = args.number("timeout", row.map_or(180, |r| r.test_timeout))?;
    Ok(if level == "-O0" && args.get("timeout").is_none() {
        base * 2
    } else {
        base
    })
}

fn print_run(run: &suite::SuiteRun, suite: &str) {
    let out = &run.output;
    println!(
        "{suite}: {} passed, {} failed of {} planned, {:.1}s",
        out.passed(),
        out.failed(),
        out.planned
            .map_or_else(|| "an unknown number".to_string(), |n| n.to_string()),
        run.seconds
    );
    if let Some(reason) = &out.bailed {
        println!("bailed out: {reason}");
    }
    for record in run
        .records
        .iter()
        .filter(|r| r.outcome != records::Outcome::Passed)
    {
        println!(
            "  {} {}{}",
            record.outcome.name(),
            record.test,
            record
                .baseline
                .as_ref()
                .map_or_else(String::new, |b| format!(" (baseline: {b})"))
        );
    }
}

fn test_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let out = pick_out(repo, args)?;
    let out = std::fs::canonicalize(&out).unwrap_or(out);
    let info = build::BuildInfo::load(&out)?;
    let suite_name = args.get("suite").unwrap_or("regress").to_string();
    let rows = Rows::load(&repo.rows())?;
    let row = args.get("row").map(|r| rows.get(r)).transpose()?;
    let baseline = row.and_then(|r| {
        baseline::Baseline::load(&repo.baseline(&r.name, &info.pin, &info.config))
            .ok()
            .and_then(|b| b.suites.get(&suite_name).cloned())
    });
    let plan = suite::SuitePlan {
        out: out.clone(),
        timeout: timeout(args, row, &info.level)?,
        info,
        suite: suite_name.clone(),
        row: row.map(|r| r.name.clone()),
        baseline,
        run: args.number("run", 1)?,
        label: None,
    };
    let run = suite::run(&plan)?;
    let path = args
        .get("records")
        .map_or_else(|| out.join("records.jsonl"), PathBuf::from);
    records::append(&path, &run.records)?;
    print_run(&run, &suite_name);
    println!("records:   {}", path.display());
    // A failure only counts against the run when the baseline says the test passes, or when there
    // is no baseline to say anything.
    let regressed = run.records.iter().any(|r| {
        r.outcome != records::Outcome::Passed
            && r.outcome != records::Outcome::Skipped
            && r.baseline.as_deref().is_none_or(|b| b == "passed")
    });
    Ok(if regressed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cross_command(args: &Args) -> Result<ExitCode, String> {
    let server = PathBuf::from(args.need("server")?);
    let server = std::fs::canonicalize(&server).unwrap_or(server);
    let modules = PathBuf::from(args.need("modules")?);
    let modules = std::fs::canonicalize(&modules).unwrap_or(modules);
    let info = build::BuildInfo::load(&server)?;
    let plan = cross::CrossPlan {
        timeout: timeout(args, None, &info.level)?,
        server: server.clone(),
        modules,
    };
    let runs = cross::cross(&plan)?;
    let path = args
        .get("records")
        .map_or_else(|| server.join("records.jsonl"), PathBuf::from);
    let mut failed = false;
    for (run, suite) in runs.iter().zip(["cross-contrib", "cross-modules"]) {
        records::append(&path, &run.records)?;
        print_run(run, suite);
        failed |= run.records.iter().any(|r| {
            r.outcome != records::Outcome::Passed && r.outcome != records::Outcome::Skipped
        });
    }
    println!("records:   {}", path.display());
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn stress_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let out = pick_out(repo, args)?;
    let out = std::fs::canonicalize(&out).unwrap_or(out);
    let info = build::BuildInfo::load(&out)?;
    let rows = Rows::load(&repo.rows())?;
    let row = args.get("row").map(|r| rows.get(r)).transpose()?;
    let plan = stress::StressPlan {
        suite: suite::SuitePlan {
            out: out.clone(),
            timeout: timeout(args, row, &info.level)?,
            info,
            suite: "stress".to_string(),
            label: None,
            row: row.map(|r| r.name.clone()),
            baseline: None,
            run: args.number("run", 1)?,
        },
        minutes: args.number("minutes", 5)?,
        clients: args.number("clients", process::cores() * 2)?,
        scale: args.number("scale", 10)?,
    };
    let checks = stress::run(&plan)?;
    let records = stress::records(&plan, &checks);
    let path = args
        .get("records")
        .map_or_else(|| out.join("records.jsonl"), PathBuf::from);
    records::append(&path, &records)?;
    for check in &checks {
        println!(
            "stress: {} {}: {}",
            check.name,
            check.outcome.name(),
            check.note
        );
    }
    println!("records:   {}", path.display());
    Ok(
        if checks.iter().all(|c| c.outcome == records::Outcome::Passed) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
    )
}

fn baseline_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let rows = Rows::load(&repo.rows())?;
    let row = rows.get(args.need("row")?)?.clone();
    let choice = BuildChoice {
        system: args
            .get("system")
            .map_or(Ok(row.build_system), System::parse)?,
        level: Level::parse(args.get("level").unwrap_or("-O2"))?,
        config: args.get("config").unwrap_or("minimal").to_string(),
    };
    let runs: u32 = args.number("runs", 3)?;
    if runs == 0 {
        return Err("--runs must be at least 1".to_string());
    }
    let suite_name = args.get("suite").unwrap_or("regress").to_string();
    let cc = args.get("cc").unwrap_or(&row.reference).to_string();
    let plan = plan(repo, args, &cc, &choice, args.get("out"))?;
    if plan.compiler.kind == compiler::Kind::Rucc {
        return Err("a baseline is made with the row's reference compiler, not with rucc".into());
    }
    eprintln!(
        "rpg: baseline for {} ({}), usually run on {}",
        row.name,
        row.description,
        row.machines.join(", ")
    );
    let info = build::build(&plan)?;
    print_build(&info, &plan.out);
    if info.phase != build::Phase::Built {
        return Err(format!(
            "the reference build did not finish, so there is no baseline: {}",
            info.first_error.unwrap_or_default()
        ));
    }
    let dir = repo.baseline(&row.name, &info.pin, &info.config);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let records_path = dir.join("records.jsonl");
    std::fs::remove_file(&records_path).ok();
    let mut all = Vec::new();
    let mut seconds = Vec::new();
    for run in 1..=runs {
        let suite_plan = suite::SuitePlan {
            out: plan.out.clone(),
            info: info.clone(),
            suite: suite_name.clone(),
            row: Some(row.name.clone()),
            timeout: timeout(args, Some(&row), &info.level)?,
            baseline: None,
            run,
            label: None,
        };
        let result = suite::run(&suite_plan)?;
        print_run(&result, &format!("{suite_name} run {run}"));
        records::append(&records_path, &result.records)?;
        seconds.push(result.seconds);
        all.push(result.records);
    }
    let mut sets = baseline::classify(&all);
    sets.seconds = seconds;
    let mut written = baseline::Baseline::from_build(&row.name, &info, runs);
    println!(
        "baseline:  {} passing, {} failing, {} flaky",
        sets.passing.len(),
        sets.failing.len(),
        sets.flaky.len()
    );
    written.suites.insert(suite_name, sets);
    written.save(&dir)?;
    println!("written:   {}", dir.display());
    Ok(ExitCode::SUCCESS)
}

fn demands_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let pin = load_pin(repo, args)?;
    let source = source_of(&pin)?;
    let path = args
        .get("out")
        .map_or_else(|| repo.root.join("demands.toml"), PathBuf::from);
    let previous = demands::load(&path)?;
    let mut found = demands::scan_tree(&source, &previous)?;
    found.pin.clone_from(&pin.name);
    found.commit.clone_from(&pin.commit);
    demands::save(&path, &found)?;
    println!(
        "scanned {} files, {} lines of {}",
        found.file_count, found.line_count, pin.name
    );
    for demand in found
        .demand
        .iter()
        .filter(|d| d.kind == demands::Kind::Feature)
    {
        println!(
            "  {:<24} {:>6} in {:>4} files",
            demand.tag, demand.count, demand.files
        );
    }
    let count = |kind| found.demand.iter().filter(|d| d.kind == kind).count();
    println!(
        "  and {} distinct __builtin names, {} attributes written directly, {} pg_attribute macros",
        count(demands::Kind::Builtin),
        count(demands::Kind::Attribute),
        count(demands::Kind::PgAttribute)
    );
    println!("written:   {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// Group what a run kept by cause. A measurement rather than a gate, so it exits 0 either way.
fn triage_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let out = pick_out(repo, args)?;
    let out = std::fs::canonicalize(&out).unwrap_or(out);
    let cores = args
        .get("cores")
        .map_or_else(|| out.join("cores"), PathBuf::from);
    let found = triage::triage(&out, &cores);
    let path = out.join("triage.md");
    std::fs::write(&path, triage::report(&found))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!(
        "triage: {} failures in {} groups, {} core files, {} from SIGQUIT left out, {} cores unread",
        found.failures(),
        found.groups.len(),
        found.cores,
        found.quit.len(),
        found.unread.len()
    );
    for (signature, failures) in found.sorted().iter().take(10) {
        println!("  {:>5}  {signature}", failures.len());
    }
    println!("report:    {}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn config_diff(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let differences = configdiff::diff(Path::new(args.need("a")?), Path::new(args.need("b")?))?;
    let path = args
        .get("divergences")
        .map_or_else(|| repo.root.join("config-divergences.toml"), PathBuf::from);
    let divergences = match std::fs::read_to_string(&path) {
        Ok(text) => configdiff::parse_divergences(&text)?,
        Err(_) if args.get("divergences").is_none() => Vec::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let sorted = configdiff::sort(differences, &divergences);
    for (d, v) in &sorted.explained {
        println!("explained, {}: {}\n  because {}", d.source, d.name, v.why);
    }
    for v in &sorted.unused {
        println!("unused entry, {}: {}", v.source, v.name);
    }
    print!("{}", configdiff::report(&sorted.unexplained));
    println!(
        "{} unexplained, {} explained by {}",
        sorted.unexplained.len(),
        sorted.explained.len(),
        path.display()
    );
    Ok(if sorted.unexplained.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn repro_command(args: &Args) -> Result<ExitCode, String> {
    let build = PathBuf::from(args.need("build")?);
    let build = std::fs::canonicalize(&build).unwrap_or(build);
    let file = args.need("file")?;
    let out = args
        .get("out")
        .map_or_else(|| repro::default_out(&build, file), PathBuf::from);
    let bundle = repro::repro(&repro::Request {
        build: &build,
        file,
        out: &out,
        object: args.get("object"),
    })?;
    println!("bundle:    {}", bundle.dir.display());
    println!("compiler:  {}", bundle.compiler);
    println!("written:   command.txt, compile.sh, compiler.txt");
    match (&bundle.preprocessed, &bundle.preprocess_error) {
        (Some(path), _) => {
            println!("source:    {}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        (None, error) => {
            println!(
                "no preprocessed source: {}",
                error.as_deref().unwrap_or("the compiler wrote nothing")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn asm_audit_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let pin = load_pin(repo, args)?;
    let source = source_of(&pin)?;
    let path = args
        .get("out")
        .map_or_else(|| repo.root.join("asm-audit.toml"), PathBuf::from);
    let mut audit = asmaudit::scan_tree(&source)?;
    audit.pin.clone_from(&pin.name);
    audit.commit.clone_from(&pin.commit);
    asmaudit::save(&path, &audit)?;
    println!(
        "scanned {} files of {}: {} inline assembly statements in {} files",
        audit.file_count,
        pin.name,
        audit.statement_count,
        audit.files.len()
    );
    let list = |counts: &std::collections::BTreeMap<String, usize>| {
        counts
            .iter()
            .map(|(k, n)| format!("\"{k}\" {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  output constraints: {}", list(&audit.output_constraints));
    println!("  input constraints:  {}", list(&audit.input_constraints));
    println!("  clobbers:           {}", list(&audit.clobbers));
    println!("written:   {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// A build directory named on the command line, made absolute.
fn build_dir(args: &Args, name: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(args.need(name)?);
    std::fs::canonicalize(&dir).map_err(|e| format!("--{name} {}: {e}", dir.display()))
}

fn frames_command(repo: &Repo, args: &Args) -> Result<ExitCode, String> {
    let a = build_dir(args, "a")?;
    let b = build_dir(args, "b")?;
    let jobs = args.number("jobs", process::cores())?;
    let scratch = std::env::temp_dir().join(format!("rpg-frames-{}", std::process::id()));
    let measured = frames::measure(
        &a,
        args.get("a-cc").map(Path::new),
        jobs,
        &scratch.join("a"),
    )
    .and_then(|ma| {
        frames::measure(
            &b,
            args.get("b-cc").map(Path::new),
            jobs,
            &scratch.join("b"),
        )
        .map(|mb| (ma, mb))
    });
    std::fs::remove_dir_all(&scratch).ok();
    let (ma, mb) = measured?;
    let joined = frames::join(&ma.frames, &mb.frames);
    let text = frames::report(&ma, &mb, &joined, &process::today(), &process::hostname());
    let out = args.get("out").map_or_else(
        || frames::default_out(&repo.root, &ma.name, &mb.name),
        PathBuf::from,
    );
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&out, &text).map_err(|e| format!("writing {}: {e}", out.display()))?;
    println!("a:         {} ({})", ma.name, ma.version);
    println!("b:         {} ({})", mb.name, mb.version);
    println!(
        "functions: {} on both sides, {} only in a, {} only in b",
        joined.pairs.len(),
        joined.only_a.len(),
        joined.only_b.len()
    );
    if let Some(s) = frames::stats(&joined.pairs) {
        println!(
            "b/a:       median {:.2}, p90 {:.2}, p99 {:.2}, max {:.2}",
            s.median, s.p90, s.p99, s.max
        );
    }
    if !ma.failures.is_empty() || !mb.failures.is_empty() {
        println!(
            "failed:    {} compiles in a, {} in b",
            ma.failures.len(),
            mb.failures.len()
        );
    }
    println!("written:   {}", out.display());
    // A measurement, not a gate: the numbers are for reading, and the target belongs to PG2.
    Ok(ExitCode::SUCCESS)
}
