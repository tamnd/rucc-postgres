//! SHA-256 over files and bytes, as lower case hex.
//!
//! Used by the shim for the inputs and outputs of every compile, and by `rpg fetch` for the pinned
//! archive, so the two agree on spelling and a hash in `compile.jsonl` can be compared with one in
//! `pins.toml` by eye.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Read in blocks this large, so that the archive is never held in memory whole.
const BLOCK: usize = 64 * 1024;

/// Hash a file.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BLOCK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Hash bytes already in memory.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Whether two digests are the same, ignoring case and surrounding whitespace.
///
/// Release pages print hashes in either case, and a file fetched from one ends in a newline.
#[must_use]
pub fn same_digest(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_input_hashes_to_the_known_value() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc_hashes_to_the_known_value() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_file_hashes_the_same_as_its_bytes() {
        let path = std::env::temp_dir().join(format!("rpg-digest-{}.bin", std::process::id()));
        let bytes = vec![7_u8; BLOCK * 2 + 13];
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(&bytes));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn case_and_whitespace_do_not_make_two_digests_different() {
        assert!(same_digest("ABCD", "abcd"));
        assert!(same_digest(" abcd\n", "abcd"));
        assert!(!same_digest("abcd", "abce"));
    }
}
