//! The command line, parsed by hand as rucc-real-corpus's `rrc` does it.
//!
//! Every option takes a value, given either as the next word or after `=`, except the few listed
//! as switches. A value may start with a dash, which `--level -O0` needs.

use std::collections::BTreeMap;

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The subcommand.
    pub command: String,
    /// Options with values.
    pub values: BTreeMap<String, String>,
    /// Switches that were given.
    pub switches: Vec<String>,
}

/// Options that take no value.
const SWITCHES: &[&str] = &[
    "twice",
    "configure-only",
    "no-upstream-check",
    "no-fuel",
    "help",
];

/// Options each command accepts.
fn accepted(command: &str) -> Option<&'static [&'static str]> {
    Some(match command {
        "fetch" => &["pin", "no-upstream-check"],
        "build" => &[
            "cc",
            "level",
            "system",
            "config",
            "out",
            "pin",
            "jobs",
            "twice",
            "configure-only",
        ],
        "test" => &["suite", "out", "row", "records", "run", "timeout"],
        "baseline" => &[
            "row", "runs", "pin", "config", "system", "level", "cc", "out", "jobs", "suite",
        ],
        "demands" | "asm-audit" => &["pin", "out"],
        "config-diff" => &["a", "b", "divergences", "row"],
        "repro" => &["build", "file", "out", "object"],
        "frames" => &["a", "b", "a-cc", "b-cc", "out", "jobs"],
        "cross-modules" => &["server", "modules", "records", "timeout"],
        "stress" => &[
            "out", "row", "records", "run", "timeout", "minutes", "clients", "scale",
        ],
        "bench" => &[
            "out", "row", "records", "timeout", "runs", "seconds", "clients", "scale", "sf", "only",
        ],
        "bench-compare" => &["a", "b", "out"],
        "bench-night" => &[
            "gcc",
            "rucc",
            "runs",
            "reports",
            "threshold",
            "issue",
            "run-url",
        ],
        "profile" => &[
            "out",
            "row",
            "records",
            "timeout",
            "loads",
            "clients",
            "transactions",
            "scale",
            "sf",
            "event",
        ],
        "profile-compare" => &["a", "b", "top", "out"],
        "triage" => &["out", "cores", "reports", "since"],
        "mixed" => &[
            "gcc", "rucc", "suite", "check", "under", "out", "timeout", "no-fuel",
        ],
        "nightly" => &["nights", "runs", "reports", "issue", "run-url"],
        "help" => &[],
        _ => return None,
    })
}

/// The usage text.
pub const USAGE: &str = "\
rpg: build and test the pinned Postgres tree with rucc and with a reference compiler

usage:
  rpg fetch [--pin NAME] [--no-upstream-check]
  rpg build --cc PATH [--level -O0|-O1|-O2|-Os] [--system meson|autoconf] [--config minimal|full]
            [--out DIR] [--pin NAME] [--jobs N] [--twice] [--configure-only]
  rpg test [--suite regress|isolation|ecpg|contrib|modules|world] [--out DIR] [--row ROW]
           [--records FILE] [--run N] [--timeout S]
  rpg baseline --row ROW [--runs 3] [--system S] [--level L] [--cc PATH] [--config C] [--pin P]
  rpg demands [--pin NAME] [--out FILE]
  rpg config-diff --a DIR --b DIR [--divergences FILE] [--row ROW]
  rpg repro --build DIR --file PATH [--out DIR] [--object PART]
  rpg asm-audit [--pin NAME] [--out FILE]
  rpg frames --a DIR --b DIR [--a-cc PATH] [--b-cc PATH] [--out FILE] [--jobs N]
  rpg cross-modules --server DIR --modules DIR [--records FILE] [--timeout S]
  rpg triage [--out DIR] [--cores DIR] [--reports DIR] [--since SECONDS]
  rpg mixed --gcc DIR --rucc DIR [--suite S | --check CMD] [--under PREFIX,...] [--out DIR]
            [--timeout S] [--no-fuel]
  rpg nightly --nights DIR [--runs DIR] [--reports DIR] [--issue FILE] [--run-url URL]
  rpg stress [--out DIR] [--row ROW] [--minutes 5] [--clients N] [--scale 10] [--records FILE]
  rpg bench [--out DIR] [--row ROW] [--runs 10] [--seconds 60] [--clients N] [--scale 100]
            [--sf 1] [--only pgbench|analytic|regress] [--records FILE]
  rpg bench-compare --a FILE --b FILE [--out FILE]
  rpg bench-night --gcc DIR --rucc DIR [--runs DIR] [--reports DIR] [--threshold 10] [--issue FILE]
                  [--run-url URL]
  rpg profile [--out DIR] [--row ROW] [--loads select-only,tpcb-like,analytic] [--clients N]
              [--transactions 20000] [--scale 100] [--sf 1] [--event E] [--records FILE]
  rpg profile-compare --a FILE --b FILE [--top 40] [--out FILE]

The repository is found by walking up to pins.toml, or from RPG_ROOT. Downloads and unpacked
sources go to RPG_CACHE, or ~/.cache/rpg. Build directories default to work/ in the repository.
";

impl Args {
    /// Parse the words after the program name.
    pub fn parse(words: &[String]) -> Result<Self, String> {
        let mut words = words.iter();
        let command = match words.next() {
            None => "help".to_string(),
            Some(w) if w == "-h" || w == "--help" => "help".to_string(),
            Some(w) => w.clone(),
        };
        let allowed =
            accepted(&command).ok_or_else(|| format!("unknown command {command}; see rpg help"))?;
        let mut values = BTreeMap::new();
        let mut switches = Vec::new();
        while let Some(word) = words.next() {
            let Some(option) = word.strip_prefix("--") else {
                return Err(format!("unexpected argument {word}"));
            };
            let (name, inline) = match option.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (option, None),
            };
            if name == "help" {
                switches.push("help".to_string());
                continue;
            }
            if !allowed.contains(&name) {
                return Err(format!("rpg {command} has no option --{name}"));
            }
            if SWITCHES.contains(&name) {
                if inline.is_some() {
                    return Err(format!("--{name} takes no value"));
                }
                switches.push(name.to_string());
                continue;
            }
            let value = match inline {
                Some(v) => v,
                None => words
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("--{name} needs a value"))?,
            };
            if values.insert(name.to_string(), value).is_some() {
                return Err(format!("--{name} given twice"));
            }
        }
        Ok(Self {
            command,
            values,
            switches,
        })
    }

    /// An option's value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// An option that must be there.
    pub fn need(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("rpg {} needs --{name}", self.command))
    }

    /// Whether a switch was given.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    /// A numeric option, or its default.
    pub fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("--{name} wants a number, not {v}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Args, String> {
        let words: Vec<String> = line.split_whitespace().map(String::from).collect();
        Args::parse(&words)
    }

    #[test]
    fn values_may_start_with_a_dash_or_follow_an_equals_sign() {
        let args = parse("build --cc gcc-16 --level -O0 --system=meson --twice").unwrap();
        assert_eq!(args.command, "build");
        assert_eq!(args.get("cc"), Some("gcc-16"));
        assert_eq!(args.get("level"), Some("-O0"));
        assert_eq!(args.get("system"), Some("meson"));
        assert!(args.has("twice"));
    }

    #[test]
    fn mistakes_are_refused() {
        assert!(parse("build --suite regress").is_err());
        assert!(parse("frobnicate").is_err());
        assert!(parse("build --cc").is_err());
        assert!(parse("build --cc a --cc b").is_err());
        assert!(parse("test regress").is_err());
        assert_eq!(parse("").unwrap().command, "help");
    }
}
