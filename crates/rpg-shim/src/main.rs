//! `rpg-cc`, the compiler shim.
//!
//! Runs the real compiler with the arguments it was given, passes standard output through
//! untouched, copies standard error through while keeping its first KiB, and appends one record
//! to `compile.jsonl`. It prints nothing of its own unless it cannot run the compiler at all or
//! `RPG_TWICE` finds a difference, because configure scripts judge a compiler by what it writes on
//! standard error, and a shim that chatters there changes the answers it is meant to record.

use rpg_shim::args::{self, Mode};
use rpg_shim::config::{RECORDED_ENV, ShimConfig};
use rpg_shim::digest::sha256_file;
use rpg_shim::record::{CompileRecord, FileDigest, Twice};
use rpg_shim::usage::{self, Usage};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// How much of standard error goes on the record.
const STDERR_KEEP: usize = 1024;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let shim_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let Some(config) = ShimConfig::load(shim_dir.as_deref(), &|key| std::env::var(key).ok()) else {
        eprintln!(
            "rpg-cc: no compiler to run: set RPG_REAL_CC or put rpg-cc.toml next to the shim"
        );
        return ExitCode::from(127);
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let rest = &argv[1..];
    let invocation = args::read(rest, &cwd, &|p| p.is_file());

    let inputs = digests(&cwd, &invocation.inputs);
    let trace_file = (config.rucc_trace && invocation.compiles_c()).then(trace_path);
    let added: Vec<String> = trace_file
        .iter()
        .map(|path| format!("-frucc-trace={}", path.display()))
        .collect();

    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let before = usage::children();
    let clock = Instant::now();
    let run = match run(&config.real, rest, &added, true) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("rpg-cc: could not run {}: {e}", config.real.display());
            return ExitCode::from(127);
        }
    };
    let wall_seconds = clock.elapsed().as_secs_f64();
    let spent = match (usage::children(), before) {
        (Some(after), Some(before)) => Some(after.since(before)),
        (after, _) => after,
    };

    let outputs = digests(&cwd, &invocation.outputs);
    let rucc = trace_file.as_deref().map(take_trace).unwrap_or_default();

    let mut twice = None;
    if config.twice
        && run.exit == Some(0)
        && matches!(invocation.mode, Mode::Compile | Mode::Assemble)
        && !outputs.is_empty()
    {
        twice = Some(compile_again(&config.real, rest, &cwd, &outputs));
    }

    let record = CompileRecord {
        started,
        argv,
        compiler: config.real.display().to_string(),
        added,
        cwd: cwd.display().to_string(),
        env: recorded_env(),
        inputs,
        outputs,
        wall_seconds,
        user_seconds: spent.map(|u: Usage| u.user_seconds),
        system_seconds: spent.map(|u| u.system_seconds),
        peak_rss_kb: spent.map(|u| u.peak_rss_kb),
        exit: run.exit,
        signal: run.signal,
        stderr: String::from_utf8_lossy(&run.stderr_head).into_owned(),
        rucc,
        twice: twice.clone(),
    };
    if !config.log.as_os_str().is_empty() {
        // A lost record is a hole in the trace, but failing the compile over it would turn a full
        // disk into a compiler bug. `rpg build` compares the record count with what it expects.
        let _ = record.append_to(&config.log);
    }

    if let Some(Twice {
        identical: false,
        differing,
    }) = twice
    {
        eprintln!(
            "rpg-cc: RPG_TWICE: a second compile wrote different bytes to {}",
            differing.join(", ")
        );
        return ExitCode::from(1);
    }
    match (run.exit, run.signal) {
        (Some(code), _) => ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1)),
        (None, Some(signal)) => ExitCode::from(u8::try_from(128 + signal).unwrap_or(1)),
        (None, None) => ExitCode::from(1),
    }
}

/// How a compiler run ended.
struct Run {
    exit: Option<i32>,
    signal: Option<i32>,
    stderr_head: Vec<u8>,
}

/// Run the compiler, copying its standard error to ours when `echo` is set.
fn run(real: &Path, args: &[String], added: &[String], echo: bool) -> std::io::Result<Run> {
    let mut child = Command::new(real)
        .args(args)
        .args(added)
        .stdin(Stdio::inherit())
        .stdout(if echo {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .spawn()?;
    let mut pipe = child.stderr.take().expect("stderr was piped");
    let copier = std::thread::spawn(move || {
        let mut head = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut ours = std::io::stderr();
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if echo {
                        let _ = ours.write_all(&buffer[..n]);
                    }
                    let room = STDERR_KEEP.saturating_sub(head.len());
                    head.extend_from_slice(&buffer[..n.min(room)]);
                }
            }
        }
        head
    });
    let status = child.wait()?;
    let stderr_head = copier.join().unwrap_or_default();
    Ok(Run {
        exit: status.code(),
        signal: signal_of(status),
        stderr_head,
    })
}

#[cfg(unix)]
fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// Compile again, quietly, and compare every output with the first compile's.
fn compile_again(real: &Path, args: &[String], cwd: &Path, first: &[FileDigest]) -> Twice {
    let ran = run(real, args, &[], false);
    if !matches!(ran, Ok(Run { exit: Some(0), .. })) {
        return Twice {
            identical: false,
            differing: vec!["(the second compile failed)".to_string()],
        };
    }
    let names: Vec<String> = first.iter().map(|d| d.path.clone()).collect();
    let second = digests(cwd, &names);
    let differing: Vec<String> = first
        .iter()
        .zip(&second)
        .filter(|(a, b)| a.sha256 != b.sha256)
        .map(|(a, _)| a.path.clone())
        .collect();
    Twice {
        identical: differing.is_empty(),
        differing,
    }
}

fn digests(cwd: &Path, paths: &[String]) -> Vec<FileDigest> {
    paths
        .iter()
        .map(|path| FileDigest {
            path: path.clone(),
            sha256: sha256_file(&cwd.join(path)).unwrap_or_default(),
        })
        .collect()
}

fn recorded_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| RECORDED_ENV.contains(&key.as_str()) || key.starts_with("RUCC_"))
        .collect()
}

/// A fresh file name for rucc to write its trace line into.
fn trace_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    std::env::temp_dir().join(format!("rpg-cc-{}-{nanos}.jsonl", std::process::id()))
}

/// Read and remove the trace file. Lines that are not JSON are kept as strings, so that an
/// older or newer rucc writing something unexpected shows up on the record instead of vanishing.
fn take_trace(path: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let _ = std::fs::remove_file(path);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|_| serde_json::Value::String(line.into()))
        })
        .collect()
}
