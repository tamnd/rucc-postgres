//! Where things are: the repository, the cache, and the default build directories.
//!
//! `rpg` is run from anywhere inside the repository and finds its root by walking up to the
//! directory holding `pins.toml`, the way cargo finds a manifest. `RPG_ROOT` overrides the walk,
//! which is what a run from outside the checkout, as another user, needs.

use std::path::{Path, PathBuf};

/// The repository root and the paths hanging off it.
#[derive(Debug, Clone)]
pub struct Repo {
    /// The directory holding `pins.toml`.
    pub root: PathBuf,
}

impl Repo {
    /// Find the repository from `RPG_ROOT` or by walking up from the working directory.
    pub fn find() -> Result<Self, String> {
        if let Ok(root) = std::env::var("RPG_ROOT") {
            let root = PathBuf::from(root);
            if root.join("pins.toml").is_file() {
                return Ok(Self { root });
            }
            return Err(format!("RPG_ROOT={} has no pins.toml", root.display()));
        }
        let start = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
        let mut here = start.as_path();
        loop {
            if here.join("pins.toml").is_file() {
                return Ok(Self {
                    root: here.to_path_buf(),
                });
            }
            here = here.parent().ok_or_else(|| {
                format!(
                    "{} is not inside a rucc-postgres checkout (no pins.toml above it); set RPG_ROOT",
                    start.display()
                )
            })?;
        }
    }

    /// `pins.toml`.
    #[must_use]
    pub fn pins(&self) -> PathBuf {
        self.root.join("pins.toml")
    }

    /// `rows.toml`.
    #[must_use]
    pub fn rows(&self) -> PathBuf {
        self.root.join("rows.toml")
    }

    /// The file for a named configuration.
    #[must_use]
    pub fn config(&self, name: &str) -> PathBuf {
        self.root.join("configs").join(format!("{name}.toml"))
    }

    /// Where local build trees go unless `--out` says otherwise. Ignored by git.
    #[must_use]
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    /// The baseline directory for a row, pin and configuration.
    #[must_use]
    pub fn baseline(&self, row: &str, pin: &str, config: &str) -> PathBuf {
        self.root.join("baselines").join(row).join(pin).join(config)
    }
}

/// The download and source cache: `RPG_CACHE`, or `~/.cache/rpg`.
#[must_use]
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RPG_CACHE")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".cache").join("rpg")
}
