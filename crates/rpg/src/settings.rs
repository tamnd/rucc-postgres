//! `rows.toml` and `configs/*.toml`: the machines and the build options.
//!
//! A configuration is a list of documented `configure` or meson options and nothing else. It is
//! the same for the reference build and the rucc build, and there is deliberately no field for a
//! patch, a source edit or an extra compiler flag: a build that needs one is a finding against
//! rucc, not a configuration.

use serde::Deserialize;
use std::fmt;
use std::path::Path;

/// Which build system drives the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum System {
    /// `meson setup` and `ninja`.
    Meson,
    /// `configure` and `make`.
    Autoconf,
}

impl System {
    /// Parse the spelling on the command line.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "meson" => Ok(Self::Meson),
            "autoconf" | "configure" | "make" => Ok(Self::Autoconf),
            other => Err(format!(
                "unknown build system {other}; use meson or autoconf"
            )),
        }
    }

    /// The name used in paths and records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Meson => "meson",
            Self::Autoconf => "autoconf",
        }
    }
}

impl fmt::Display for System {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The optimization level. `-O0` and `-O2` are the graded levels; PG8 adds `-O1` and `-Os`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `-O0`.
    O0,
    /// `-O1`.
    O1,
    /// `-O2`.
    O2,
    /// `-Os`.
    Os,
}

impl Level {
    /// Accepts `-O2`, `O2` and `2`, and the same for the others.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim_start_matches('-').trim_start_matches('O') {
            "0" => Ok(Self::O0),
            "1" => Ok(Self::O1),
            "2" => Ok(Self::O2),
            "s" => Ok(Self::Os),
            _ => Err(format!("unknown level {text}; use -O0, -O1, -O2 or -Os")),
        }
    }

    /// The flag, `-O0`, `-O1`, `-O2` or `-Os`.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::Os => "-Os",
        }
    }

    /// The digit meson's `optimization` option takes.
    #[must_use]
    pub const fn digit(self) -> &'static str {
        match self {
            Self::O0 => "0",
            Self::O1 => "1",
            Self::O2 => "2",
            Self::Os => "s",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.flag())
    }
}

/// A named configuration, `configs/<name>.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildConfig {
    /// The name, which must match the file name.
    pub name: String,
    /// Options for `configure`.
    pub autoconf: Options,
    /// Options for `meson setup`.
    pub meson: Options,
}

/// The options for one build system.
#[derive(Debug, Clone, Deserialize)]
pub struct Options {
    /// Passed as they are.
    pub options: Vec<String>,
}

impl BuildConfig {
    /// Read a configuration file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for option in config.autoconf.options.iter().chain(&config.meson.options) {
            if !option.starts_with("--") && !option.starts_with("-D") {
                return Err(format!(
                    "{}: {option} is not a configure or meson option; a configuration holds only those",
                    path.display()
                ));
            }
        }
        Ok(config)
    }

    /// The options for a build system.
    #[must_use]
    pub fn options(&self, system: System) -> &[String] {
        match system {
            System::Meson => &self.meson.options,
            System::Autoconf => &self.autoconf.options,
        }
    }
}

/// `rows.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Rows {
    /// Every row.
    #[serde(rename = "row")]
    pub rows: Vec<Row>,
}

/// One row of the design's platform table.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Row {
    /// `L64`, `LA64`, `M64` or `W64`.
    pub name: String,
    /// What it is, in words.
    pub description: String,
    /// The machines it runs on, in order of preference.
    pub machines: Vec<String>,
    /// The reference compiler, as a command name or a path.
    pub reference: String,
    /// The build system the row is graded with first.
    pub build_system: System,
    /// `PG_TEST_TIMEOUT_DEFAULT` for `-O2` runs. `-O0` runs get twice this.
    pub test_timeout: u32,
}

impl Rows {
    /// Read `rows.toml`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The named row.
    pub fn get(&self, name: &str) -> Result<&Row, String> {
        self.rows.iter().find(|r| r.name == name).ok_or_else(|| {
            let known: Vec<&str> = self.rows.iter().map(|r| r.name.as_str()).collect();
            format!("no row named {name}; rows.toml has {}", known.join(", "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_in_every_spelling() {
        assert_eq!(Level::parse("-O2").unwrap(), Level::O2);
        assert_eq!(Level::parse("O0").unwrap(), Level::O0);
        assert_eq!(Level::parse("2").unwrap(), Level::O2);
        assert_eq!(Level::parse("-O1").unwrap(), Level::O1);
        assert_eq!(Level::parse("-Os").unwrap(), Level::Os);
        assert_eq!(Level::Os.digit(), "s");
        assert!(Level::parse("-O3").is_err());
    }

    #[test]
    fn the_checked_in_files_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let config = BuildConfig::load(&root.join("configs/minimal.toml")).unwrap();
        assert_eq!(config.name, "minimal");
        assert!(
            config
                .options(System::Meson)
                .iter()
                .any(|o| o == "-Dicu=disabled")
        );
        assert!(
            config
                .options(System::Autoconf)
                .iter()
                .any(|o| o == "--without-icu")
        );
        let rows = Rows::load(&root.join("rows.toml")).unwrap();
        assert_eq!(rows.get("L64").unwrap().reference, "gcc-16");
        assert_eq!(rows.rows.len(), 4);
    }
}
