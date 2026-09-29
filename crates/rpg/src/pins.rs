//! `pins.toml` and `rpg fetch`: the pinned Postgres tree, downloaded, verified and unpacked.
//!
//! A pin is a URL, a SHA-256 and the commit the tag points at. The hash is checked every time
//! the archive is used, including when it comes out of the cache, so a cache that somebody edited
//! or a download that was cut short can never become a build. The `.sha256` file the Postgres
//! project publishes next to each tarball is fetched as well and has to agree with the pin, which
//! catches a pin written down wrong as well as an archive that changed upstream.
//!
//! A pin can name a branch instead, `REL_19_STABLE`, for the weekly run against the next release.
//! There is no tarball to hash, so `rpg fetch` clones the head of the branch afresh each time and
//! writes the commit it got to `.rpg-commit` in the tree, which is the commit every build and
//! record from that tree names.

use crate::process::shell_quote;
use crate::repo::cache_dir;
use rpg_shim::digest::{same_digest, sha256_file};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The file.
#[derive(Debug, Clone, Deserialize)]
pub struct Pins {
    /// The pin used when none is named.
    pub default: String,
    /// Every pin.
    #[serde(rename = "pin")]
    pub pins: Vec<Pin>,
}

/// One pinned release, or one branch.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Pin {
    /// The tag, for example `REL_18_6`, or the branch.
    pub name: String,
    /// The release number, for example `18.6`.
    pub version: String,
    /// The tarball.
    #[serde(default)]
    pub url: String,
    /// Its SHA-256, computed once and written here by hand.
    #[serde(default)]
    pub sha256: String,
    /// Where the project publishes the same hash, checked against the one above.
    #[serde(default)]
    pub checksum_url: Option<String>,
    /// The commit the tag points at in the Postgres repository. For a branch, the commit the last
    /// `rpg fetch` got.
    #[serde(default)]
    pub commit: String,
    /// The branch, for a pin that follows one rather than a release.
    #[serde(default)]
    pub branch: Option<String>,
    /// The repository the branch is cloned from.
    #[serde(default)]
    pub repository: Option<String>,
}

impl Pins {
    /// Read `pins.toml`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse the text of `pins.toml`. A release needs its URL, hash and commit, and a branch its
    /// repository.
    pub fn parse(text: &str) -> Result<Self, String> {
        let pins: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for pin in &pins.pins {
            let complete = if pin.branch.is_some() {
                pin.repository.is_some()
            } else {
                !pin.url.is_empty() && !pin.sha256.is_empty() && !pin.commit.is_empty()
            };
            if !complete {
                return Err(format!(
                    "pin {} needs url, sha256 and commit, or branch and repository",
                    pin.name
                ));
            }
        }
        Ok(pins)
    }

    /// The named pin, or the default one.
    pub fn get(&self, name: Option<&str>) -> Result<&Pin, String> {
        let name = name.unwrap_or(&self.default);
        self.pins.iter().find(|p| p.name == name).ok_or_else(|| {
            let known: Vec<&str> = self.pins.iter().map(|p| p.name.as_str()).collect();
            format!("no pin named {name}; pins.toml has {}", known.join(", "))
        })
    }
}

impl Pin {
    /// The archive's file name, the last part of the URL.
    #[must_use]
    pub fn archive_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or(&self.url)
    }

    /// Where the archive is kept in the cache.
    #[must_use]
    pub fn archive_path(&self) -> PathBuf {
        cache_dir().join("archives").join(self.archive_name())
    }

    /// Where the unpacked tree is kept in the cache.
    #[must_use]
    pub fn source_dir(&self) -> PathBuf {
        cache_dir().join("src").join(&self.name)
    }

    /// For a branch, take the commit from the tree the last `rpg fetch` cloned, when there is one.
    #[must_use]
    pub fn with_fetched_commit(mut self) -> Self {
        if self.branch.is_some()
            && let Ok(commit) = std::fs::read_to_string(self.source_dir().join(".rpg-commit"))
        {
            self.commit = commit.trim().to_string();
        }
        self
    }
}

/// The hash out of a `sha256sum` style line, `<hex>  <name>`.
#[must_use]
pub fn parse_checksum_file(text: &str, archive: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let hash = words.next()?;
        let name = words.next().map(|n| n.trim_start_matches('*'));
        let looks_like_hash = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (looks_like_hash && name.is_none_or(|n| n == archive)).then(|| hash.to_ascii_lowercase())
    })
}

/// Check that an archive has the pinned hash.
pub fn verify(pin: &Pin, archive: &Path) -> Result<(), String> {
    let actual = sha256_file(archive).map_err(|e| format!("hashing {}: {e}", archive.display()))?;
    if same_digest(&actual, &pin.sha256) {
        Ok(())
    } else {
        Err(format!(
            "{} has SHA-256 {actual}, but pins.toml says {} for {}",
            archive.display(),
            pin.sha256,
            pin.name
        ))
    }
}

/// Download with curl into `dest`, through a temporary name so a cut download is never used.
fn curl(url: &str, dest: &Path) -> Result<(), String> {
    let partial = dest.with_extension("partial");
    let output = Command::new("curl")
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--retry",
            "2",
            "--max-time",
            "900",
            "--output",
        ])
        .arg(&partial)
        .arg(url)
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !output.status.success() {
        std::fs::remove_file(&partial).ok();
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("curl {url}: {said}"));
    }
    std::fs::rename(&partial, dest).map_err(|e| format!("moving into {}: {e}", dest.display()))
}

/// Make sure the pinned tree is in the cache, and return where it is.
///
/// Downloads the archive if it is missing, checks its hash either way, and unpacks it if the
/// tree is missing. Unpacking goes to a staging directory that is renamed into place at the end,
/// so a tree that exists is a tree that was unpacked completely.
pub fn fetch(pin: &Pin, check_upstream: bool) -> Result<PathBuf, String> {
    if let (Some(branch), Some(repository)) = (&pin.branch, &pin.repository) {
        return clone(pin, branch, repository);
    }
    let archive = pin.archive_path();
    let dir = archive.parent().expect("the archive path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    if archive.is_file() {
        eprintln!("rpg: {} is in the cache", pin.archive_name());
    } else {
        eprintln!("rpg: downloading {}", pin.url);
        curl(&pin.url, &archive)?;
    }
    verify(pin, &archive)?;
    eprintln!("rpg: SHA-256 matches pins.toml ({})", pin.sha256);

    if let Some(url) = pin.checksum_url.as_ref().filter(|_| check_upstream) {
        let sums = archive.with_extension("sha256.txt");
        curl(url, &sums)?;
        let text = std::fs::read_to_string(&sums).unwrap_or_default();
        std::fs::remove_file(&sums).ok();
        let published = parse_checksum_file(&text, pin.archive_name())
            .ok_or_else(|| format!("{url} does not hold a SHA-256 for {}", pin.archive_name()))?;
        if !same_digest(&published, &pin.sha256) {
            return Err(format!(
                "{url} publishes {published}, but pins.toml says {}",
                pin.sha256
            ));
        }
        eprintln!("rpg: and matches the hash published at {url}");
    }

    let source = pin.source_dir();
    if source.join("configure").is_file() {
        eprintln!("rpg: {} is unpacked at {}", pin.name, source.display());
        return Ok(source);
    }
    unpack(&archive, &source)?;
    eprintln!("rpg: unpacked {} into {}", pin.name, source.display());
    Ok(source)
}

/// Clone the head of a branch, one commit deep, into a staging directory that replaces the tree
/// once the clone is complete, and write down the commit it got.
fn clone(pin: &Pin, branch: &str, repository: &str) -> Result<PathBuf, String> {
    let source = pin.source_dir();
    let parent = source.parent().expect("the source path has a parent");
    std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    let staging = parent.join(format!(".{}.staging", pin.name));
    std::fs::remove_dir_all(&staging).ok();
    eprintln!("rpg: cloning {branch} from {repository}");
    let output = Command::new("git")
        .args([
            "clone", "--quiet", "--depth", "1", "--branch", branch, repository,
        ])
        .arg(&staging)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !output.status.success() {
        std::fs::remove_dir_all(&staging).ok();
        return Err(format!(
            "git clone {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(&staging)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    let commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
    std::fs::write(staging.join(".rpg-commit"), format!("{commit}\n"))
        .map_err(|e| format!("writing the commit: {e}"))?;
    std::fs::remove_dir_all(&source).ok();
    std::fs::rename(&staging, &source)
        .map_err(|e| format!("moving into {}: {e}", source.display()))?;
    eprintln!("rpg: {} is at {commit} in {}", pin.name, source.display());
    Ok(source)
}

/// Unpack a Postgres tarball, dropping its single top level directory.
fn unpack(archive: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().expect("the source path has a parent");
    std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    let staging = parent.join(format!(
        ".{}.staging",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::remove_dir_all(&staging).ok();
    std::fs::remove_dir_all(dest).ok();
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("creating {}: {e}", staging.display()))?;
    let output = Command::new("tar")
        .arg("-x")
        .arg("-f")
        .arg(archive)
        .arg("-C")
        .arg(&staging)
        .output()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !output.status.success() {
        std::fs::remove_dir_all(&staging).ok();
        return Err(format!(
            "tar -xf {}: {}",
            shell_quote(&archive.display().to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(&staging)
        .map_err(|e| format!("reading {}: {e}", staging.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    let [inner] = entries.as_slice() else {
        std::fs::remove_dir_all(&staging).ok();
        return Err(format!(
            "{} did not unpack to a single directory",
            archive.display()
        ));
    };
    std::fs::rename(inner, dest).map_err(|e| format!("moving into {}: {e}", dest.display()))?;
    std::fs::remove_dir_all(&staging).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINS: &str = r#"
default = "REL_18_6"

[[pin]]
name = "REL_18_6"
version = "18.6"
url = "https://ftp.postgresql.org/pub/source/v18.6/postgresql-18.6.tar.bz2"
sha256 = "555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f"
checksum-url = "https://ftp.postgresql.org/pub/source/v18.6/postgresql-18.6.tar.bz2.sha256"
commit = "724edf9bde9d356724ad384a2e196edc3c9f80f7"
"#;

    #[test]
    fn the_default_pin_is_found_by_name_and_by_default() {
        let pins = Pins::parse(PINS).unwrap();
        assert_eq!(pins.get(None).unwrap().version, "18.6");
        assert_eq!(
            pins.get(Some("REL_18_6")).unwrap().archive_name(),
            "postgresql-18.6.tar.bz2"
        );
        assert!(pins.get(Some("REL_19_0")).unwrap_err().contains("REL_18_6"));
    }

    #[test]
    fn a_branch_pin_needs_a_repository_and_a_release_needs_its_hash() {
        let branch = format!(
            "{PINS}\n[[pin]]\nname = \"REL_19_STABLE\"\nversion = \"19\"\nbranch = \"REL_19_STABLE\"\nrepository = \"https://github.com/postgres/postgres.git\"\n"
        );
        let pins = Pins::parse(&branch).unwrap();
        let pin = pins.get(Some("REL_19_STABLE")).unwrap();
        assert_eq!(pin.branch.as_deref(), Some("REL_19_STABLE"));
        assert!(pin.commit.is_empty());
        let no_repository = format!(
            "{PINS}\n[[pin]]\nname = \"REL_19_STABLE\"\nversion = \"19\"\nbranch = \"REL_19_STABLE\"\n"
        );
        assert!(
            Pins::parse(&no_repository)
                .unwrap_err()
                .contains("REL_19_STABLE")
        );
        let no_hash = format!(
            "{PINS}\n[[pin]]\nname = \"REL_19_0\"\nversion = \"19.0\"\nurl = \"https://x/y.tar.bz2\"\ncommit = \"abc\"\n"
        );
        assert!(Pins::parse(&no_hash).unwrap_err().contains("REL_19_0"));
    }

    #[test]
    fn the_published_checksum_file_is_read() {
        let text = "555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f  postgresql-18.6.tar.bz2\n";
        assert_eq!(
            parse_checksum_file(text, "postgresql-18.6.tar.bz2").as_deref(),
            Some("555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f")
        );
        assert!(parse_checksum_file(text, "postgresql-18.5.tar.bz2").is_none());
        assert!(parse_checksum_file("not a hash\n", "x").is_none());
    }

    #[test]
    fn an_archive_with_the_wrong_bytes_does_not_verify() {
        let path = std::env::temp_dir().join(format!("rpg-verify-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let mut pin = Pins::parse(PINS).unwrap().pins.remove(0);
        let error = verify(&pin, &path).unwrap_err();
        assert!(error.contains("ba7816bf"));
        pin.sha256 = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD".into();
        assert!(verify(&pin, &path).is_ok());
        std::fs::remove_file(&path).ok();
    }
}
