//! Asking a compiler what it is.
//!
//! rucc is recognized by the first line of `--version` starting with `rucc `. That is also what
//! decides whether the shim asks for a trace, and the trace option is itself probed rather than
//! assumed, because `-frucc-trace` arrived in a rucc release that not every checkout has.

use crate::process::{capture, which};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Which family a compiler belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// rucc.
    Rucc,
    /// GCC, or anything that says it is by naming the Free Software Foundation.
    Gcc,
    /// Clang, including Apple's.
    Clang,
    /// Something else.
    Unknown,
}

impl Kind {
    /// Classify the output of `--version`.
    #[must_use]
    pub fn of(version_text: &str) -> Self {
        let first = version_text.lines().next().unwrap_or_default();
        if first.starts_with("rucc ") {
            Self::Rucc
        } else if version_text.contains("Free Software Foundation") {
            Self::Gcc
        } else if version_text.contains("clang") || version_text.contains("Clang") {
            Self::Clang
        } else {
            Self::Unknown
        }
    }

    /// The name used in paths and records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rucc => "rucc",
            Self::Gcc => "gcc",
            Self::Clang => "clang",
            Self::Unknown => "unknown",
        }
    }
}

/// A compiler, identified.
#[derive(Debug, Clone)]
pub struct Compiler {
    /// Its absolute path.
    pub path: PathBuf,
    /// The first line of `--version`.
    pub version: String,
    /// Its family.
    pub kind: Kind,
    /// For rucc, the commit of the checkout the binary sits in, when it sits in one.
    pub commit: Option<String>,
    /// The words after the compiler in `--cc`, put first on every command line it is given. A
    /// cross compiler needs its target and sysroot named this way, as `CC` would carry them.
    pub args: Vec<String>,
}

impl Compiler {
    /// Find and identify a compiler named on the command line, with any arguments after it.
    pub fn identify(command: &str) -> Result<Self, String> {
        let mut words = command.split_whitespace();
        let name = words.next().ok_or("--cc names no compiler")?;
        let args: Vec<String> = words.map(String::from).collect();
        let path = which(name).ok_or_else(|| format!("cannot find a compiler called {name}"))?;
        let mut probe: Vec<&str> = args.iter().map(String::as_str).collect();
        probe.push("--version");
        let text = capture(&path, &probe)?;
        let kind = Kind::of(&text);
        let version = text.lines().next().unwrap_or_default().trim().to_string();
        let commit = (kind == Kind::Rucc)
            .then(|| checkout_commit(&path))
            .flatten();
        Ok(Self {
            path,
            version,
            kind,
            commit,
            args,
        })
    }

    /// A short name for directory names: the file name of the compiler.
    #[must_use]
    pub fn short_name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| "cc".to_string(), |n| n.to_string_lossy().into_owned())
    }

    /// Whether this rucc writes a trace line when given `-frucc-trace`.
    ///
    /// Compiles a one line file with the option and looks for the line. An older rucc either
    /// rejects the option or ignores it, and in both cases no line appears, so both read as no.
    #[must_use]
    pub fn supports_trace(&self, scratch: &Path) -> bool {
        if self.kind != Kind::Rucc {
            return false;
        }
        let dir = scratch.join("trace-probe");
        std::fs::create_dir_all(&dir).ok();
        let source = dir.join("probe.c");
        let trace = dir.join("trace.jsonl");
        std::fs::remove_file(&trace).ok();
        if std::fs::write(&source, "int rpg_probe;\n").is_err() {
            return false;
        }
        let ok = Command::new(&self.path)
            .args(&self.args)
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(dir.join("probe.o"))
            .arg(format!("-frucc-trace={}", trace.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let wrote = std::fs::read_to_string(&trace).is_ok_and(|t| t.contains("\"rucc\""));
        std::fs::remove_dir_all(&dir).ok();
        ok && wrote
    }
}

/// The commit of the git checkout a binary was built in, found by walking up from it.
///
/// `rucc --version` prints a release number and not a commit, and a record that names a release
/// when the binary came from a branch is wrong about which compiler ran. A binary under
/// `<checkout>/target/release` is the common case on the machines this runs on.
pub fn checkout_commit(binary: &Path) -> Option<String> {
    let mut dir = binary.parent();
    while let Some(here) = dir {
        if here.join(".git").exists() {
            let text = capture(
                Path::new("git"),
                &["-C", &here.display().to_string(), "rev-parse", "HEAD"],
            )
            .ok()?;
            return Some(text.trim().to_string());
        }
        dir = here.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_banners_are_classified() {
        assert_eq!(Kind::of("rucc 0.11.15\n"), Kind::Rucc);
        assert_eq!(
            Kind::of(
                "gcc-16 (Ubuntu 16.1.0-1) 16.1.0\nCopyright (C) 2026 Free Software Foundation, Inc.\n"
            ),
            Kind::Gcc
        );
        assert_eq!(
            Kind::of("Apple clang version 17.0.0 (clang-1700.0.13.5)\n"),
            Kind::Clang
        );
        assert_eq!(Kind::of("tcc version 0.9.27\n"), Kind::Unknown);
    }

    #[test]
    fn the_words_after_the_compiler_are_its_arguments() {
        let rucc = Compiler::identify("sh -c true").unwrap();
        assert!(rucc.path.ends_with("sh"), "{}", rucc.path.display());
        assert_eq!(rucc.args, ["-c", "true"]);
        assert!(Compiler::identify("  ").is_err());
    }

    #[test]
    fn a_rucc_banner_with_a_gcc_shaped_second_line_is_still_rucc() {
        let text = "rucc 0.12.0\nThis compiler accepts the command line of GCC from the Free Software Foundation\n";
        assert_eq!(Kind::of(text), Kind::Rucc);
    }
}
