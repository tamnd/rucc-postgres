//! Running the tools a build needs, with their output in a log file.
//!
//! Configure, meson, ninja, make and the suites all write a great deal, and nearly all of it is
//! only interesting when something failed. So every step writes to its own log in the build
//! directory, and `rpg` prints one line per step plus the tail of the log when a step fails.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// A command to run, and where.
#[derive(Debug, Clone)]
pub struct Step {
    /// What `rpg` prints while it runs.
    pub label: String,
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Environment variables to set.
    pub env: BTreeMap<String, String>,
    /// Environment variables to remove.
    pub unset: Vec<String>,
    /// Where standard output and standard error go, together.
    pub log: PathBuf,
}

/// How a step ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// Whether it exited zero.
    pub ok: bool,
    /// The exit code, when there was one.
    pub code: Option<i32>,
    /// Wall clock seconds.
    pub seconds: f64,
}

impl Step {
    /// A step with no environment changes.
    #[must_use]
    pub fn new(label: &str, program: impl Into<PathBuf>, cwd: &Path, log: &Path) -> Self {
        Self {
            label: label.to_string(),
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            unset: Vec::new(),
            log: log.to_path_buf(),
        }
    }

    /// Add arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set environment variables.
    #[must_use]
    pub fn envs(mut self, env: &BTreeMap<String, String>) -> Self {
        self.env
            .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        self
    }

    /// Run it, writing the command line at the top of the log so the log can be replayed.
    pub fn run(&self) -> Result<Finished, String> {
        eprintln!("rpg: {} (log: {})", self.label, self.log.display());
        if let Some(parent) = self.log.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let mut header = String::new();
        for (key, value) in &self.env {
            let _ = write!(header, "{key}={} ", shell_quote(value));
        }
        header.push_str(&shell_quote(&self.program.display().to_string()));
        for arg in &self.args {
            header.push(' ');
            header.push_str(&shell_quote(arg));
        }
        std::fs::write(
            &self.log,
            format!("# in {}\n# {header}\n", self.cwd.display()),
        )
        .map_err(|e| format!("writing {}: {e}", self.log.display()))?;
        let log = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.log)
            .map_err(|e| format!("opening {}: {e}", self.log.display()))?;
        let err = log
            .try_clone()
            .map_err(|e| format!("opening {}: {e}", self.log.display()))?;
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        for key in &self.unset {
            command.env_remove(key);
        }
        command.envs(&self.env);
        let clock = Instant::now();
        let status = command
            .status()
            .map_err(|e| format!("could not run {}: {e}", self.program.display()))?;
        let finished = Finished {
            ok: status.success(),
            code: status.code(),
            seconds: clock.elapsed().as_secs_f64(),
        };
        if !finished.ok {
            eprintln!(
                "rpg: {} failed after {:.1}s, the end of its log:",
                self.label, finished.seconds
            );
            for line in tail(&self.log, 25) {
                eprintln!("  | {line}");
            }
        }
        Ok(finished)
    }
}

/// The last `n` lines of a file.
#[must_use]
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// The first line of a log that looks like an error, for the build record.
#[must_use]
pub fn first_error(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let text = std::io::read_to_string(file).ok()?;
    text.lines()
        .find(|line| {
            line.contains("error:")
                || line.starts_with("ERROR:")
                || line.contains("meson.build:") && line.contains("ERROR")
                || line.starts_with("configure: error")
        })
        .map(|line| line.trim().chars().take(400).collect())
}

/// Quote a word for a shell, only when it needs it.
#[must_use]
pub fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Run a program and return its standard output, for short questions like `--version`.
pub fn capture(program: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "{} {} exited {}: {}",
            program.display(),
            args.join(" "),
            output.status,
            text.trim()
        ))
    }
}

/// Run a program and return everything it printed, whatever its exit status.
///
/// For cdb, which exits with a failure after `q` on a dump it read without trouble, so only what it
/// printed says whether it answered.
pub fn capture_all(program: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

/// `std::fs::canonicalize`, without the `\\?\` it puts in front of a path on a Windows drive.
///
/// ninja runs in the build directory, and windres starts the preprocessor through cmd.exe, which
/// cannot start in a directory written that way and falls back to the Windows directory.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(without_verbatim)
}

fn without_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Find a program on `PATH`, or take it as given when it already names a file.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') || name.contains('\\') {
        let path = PathBuf::from(name);
        return path.is_file().then(|| canonical(&path).unwrap_or(path));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Seconds since the Unix epoch, and the UTC date they fall on.
#[must_use]
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    civil_date(i64::try_from(secs / 86_400).unwrap_or(0))
}

/// Days since 1970-01-01 to a `YYYY-MM-DD` date, by Howard Hinnant's algorithm.
#[must_use]
pub fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The machine's short name, from `hostname`.
#[must_use]
pub fn hostname() -> String {
    capture(Path::new("hostname"), &["-s"])
        .or_else(|_| capture(Path::new("hostname"), &[]))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// GNU make, by the name it has here.
///
/// The suites run `make -Otarget`, and output sync arrived in GNU make 4.0. macOS ships 3.81 as
/// `make`, and Homebrew's newer one is `gmake`, so on macOS `gmake` is taken when it is on the
/// path. Everywhere else `make` is GNU make 4 already.
#[must_use]
pub fn make() -> PathBuf {
    if cfg!(target_os = "macos")
        && let Some(gmake) = which("gmake")
    {
        return gmake;
    }
    PathBuf::from("make")
}

/// How many cores to use by default.
#[must_use]
pub fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_right_either_side_of_a_leap_day() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(19_782), "2024-02-29");
        assert_eq!(civil_date(19_783), "2024-03-01");
    }

    #[test]
    fn only_words_that_need_quoting_are_quoted() {
        assert_eq!(shell_quote("-Dicu=disabled"), "-Dicu=disabled");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn the_first_error_is_found_in_a_compiler_log() {
        let path = std::env::temp_dir().join(format!("rpg-first-error-{}", std::process::id()));
        std::fs::write(
            &path,
            "ok\n../src/x.c:3:1: error: expected ';'\nlater error: no\n",
        )
        .unwrap();
        assert_eq!(
            first_error(&path).as_deref(),
            Some("../src/x.c:3:1: error: expected ';'")
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_drive_path_loses_the_verbatim_prefix() {
        let plain = without_verbatim(PathBuf::from(r"\\?\D:\a\_temp\pg"));
        assert_eq!(plain, PathBuf::from(r"D:\a\_temp\pg"));
        let share = PathBuf::from(r"\\?\UNC\server\share");
        assert_eq!(without_verbatim(share.clone()), share);
        assert_eq!(
            without_verbatim(PathBuf::from("/tmp/pg")),
            PathBuf::from("/tmp/pg")
        );
    }
}
