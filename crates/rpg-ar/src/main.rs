//! `rpg-ar`, the `ar` that Postgres's build is given when PG9 asks for rucc's own archiver.
//!
//! rucc writes archives with `rucc-archive`, but only out of what it compiles itself under
//! `--emit=archive`, and Postgres's build compiles its objects first and archives them afterwards
//! with `$(AR)` or meson's static linker. This is that step with rucc's writer in it: it reads the
//! names each ELF object defines and hands the members and the names to `rucc_archive::write`,
//! which lays out the file and its symbol index. So the archives in such a build, and the symbol
//! index every static link of `libpgport.a` and `libpgcommon.a` goes through, are rucc's.
//!
//! It takes the command lines the two build systems write. Autoconf runs `$(AR) crs lib.a a.o b.o`
//! and meson `ar csr lib.a a.o b.o`, each after removing the archive, and the `r` that both use
//! replaces a member of the same name in an archive that is already there and adds the rest, which
//! this does too. `q` appends without looking. `s`, `c`, `u`, `v` and `D` change nothing, since the
//! index is always written, nothing is printed and the archive is deterministic anyway. Given only
//! the archive, or `s` and the archive, it is `ranlib`, which has nothing left to do. `--version`
//! exits zero, which is how meson decides it is an `ar`, and the `-h` text names none of the
//! modifiers meson looks for, so meson asks for nothing this does not do.

mod elf;
mod read;

use std::path::Path;
use std::process::ExitCode;

const HELP: &str = "\
usage: rpg-ar [-]{r|q}[cs] archive member...
       rpg-ar [s] archive
Writes the archive with rucc's archive writer and an index of what each ELF member defines.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("rpg-ar: {why}");
            ExitCode::FAILURE
        }
    }
}

/// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Op {
    /// Replace members of the same name and add the others.
    Replace,
    /// Add every member at the end.
    Append,
    /// Index an archive that already has one.
    Index,
}

/// Reads the operation out of the first word, which `ar` lets go without a dash.
fn op(word: &str) -> Result<Op, String> {
    let letters = word.strip_prefix('-').unwrap_or(word);
    let mut op = None;
    for letter in letters.chars() {
        let this = match letter {
            'r' => Op::Replace,
            'q' => Op::Append,
            'c' | 's' | 'u' | 'v' | 'D' => continue,
            other => {
                return Err(format!(
                    "`{other}` in `{word}` is not something this ar does; it takes r or q with c, s, u, v and D"
                ));
            }
        };
        if op.replace(this).is_some() {
            return Err(format!("`{word}` asks for two operations"));
        }
    }
    Ok(op.unwrap_or(Op::Index))
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [] => Err(HELP.trim_end().to_string()),
        [one] if one == "--version" || one == "-V" => {
            println!(
                "rpg-ar {}, writing with rucc-archive {RUCC_ARCHIVE}",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        [one] if one == "-h" || one == "--help" => {
            print!("{HELP}");
            Ok(())
        }
        [archive] => index(Path::new(archive)),
        [word, archive, members @ ..] => match op(word)? {
            Op::Index if members.is_empty() => index(Path::new(archive)),
            Op::Index => Err(format!(
                "`{word}` names no operation for the members after it"
            )),
            op => write(Path::new(archive), members, &op),
        },
    }
}

/// The release of `rucc-archive` this is built with, which the workspace holds to one.
const RUCC_ARCHIVE: &str = "0.20.0";

/// `ranlib`, or `ar s`: the index is written with every archive, so there is only checking that the
/// file is one.
fn index(archive: &Path) -> Result<(), String> {
    let bytes =
        std::fs::read(archive).map_err(|e| format!("reading {}: {e}", archive.display()))?;
    read::members(&bytes)
        .map(|_| ())
        .map_err(|why| format!("{}: {why}", archive.display()))
}

fn write(archive: &Path, paths: &[String], op: &Op) -> Result<(), String> {
    let mut members = match std::fs::read(archive) {
        Ok(bytes) => {
            read::members(&bytes).map_err(|why| format!("{}: {why}", archive.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("reading {}: {e}", archive.display())),
    };
    for path in paths {
        let body = std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?;
        let name = Path::new(path)
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        let member = rucc_archive::Member::new(name, body);
        let slot = if *op == Op::Replace {
            members.iter().position(|m| m.name == member.name)
        } else {
            None
        };
        match slot {
            Some(at) => members[at] = member,
            None => members.push(member),
        }
    }
    for member in &mut members {
        if !elf::is_elf(&member.body) {
            return Err(format!(
                "{} is not an ELF object, and this ar indexes only those",
                member.name
            ));
        }
        member.defines =
            elf::defines(&member.body).map_err(|why| format!("{}: {why}", member.name))?;
    }
    let bytes = rucc_archive::write(rucc_archive::Flavour::Gnu, &members)
        .map_err(|why| format!("{}: {why}", archive.display()))?;
    // Written beside the archive and renamed over it, so that a build stopped halfway never finds a
    // file that looks like an archive and is not one.
    let mut partial = archive.as_os_str().to_owned();
    partial.push(".rpg-ar");
    std::fs::write(&partial, &bytes).map_err(|e| format!("writing {}: {e}", archive.display()))?;
    std::fs::rename(&partial, archive).map_err(|e| format!("writing {}: {e}", archive.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_the_build_systems_write_are_understood() {
        assert_eq!(op("crs").unwrap(), Op::Replace);
        assert_eq!(op("csr").unwrap(), Op::Replace);
        assert_eq!(op("csrD").unwrap(), Op::Replace);
        assert_eq!(op("-rc").unwrap(), Op::Replace);
        assert_eq!(op("qc").unwrap(), Op::Append);
        assert_eq!(op("s").unwrap(), Op::Index);
        assert!(op("csrT").is_err());
        assert!(op("rq").is_err());
        assert!(op("x").is_err());
    }

    #[test]
    fn an_archive_is_written_with_its_index_and_replaced_by_name() {
        let dir = std::env::temp_dir().join(format!("rpg-ar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("pgstrcasecmp.o");
        let second = dir.join("a_name_longer_than_sixteen_bytes.o");
        std::fs::write(&first, elf::tests::object(&[("pg_strcasecmp", 1, 1)])).unwrap();
        std::fs::write(&second, elf::tests::object(&[("long_one", 1, 1)])).unwrap();
        let archive = dir.join("libpgport.a");
        let words = |list: &[&Path]| -> Vec<String> {
            list.iter().map(|p| p.display().to_string()).collect()
        };
        run(&[vec!["crs".to_string()], words(&[&archive, &first, &second])].concat()).unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        assert!(bytes.starts_with(rucc_archive::MAGIC));
        let names: Vec<String> = read::members(&bytes)
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(
            names,
            ["pgstrcasecmp.o", "a_name_longer_than_sixteen_bytes.o"]
        );

        // `r` over an archive that is there puts the new body in the old member's place.
        std::fs::write(&first, elf::tests::object(&[("pg_strncasecmp", 1, 1)])).unwrap();
        run(&[vec!["r".to_string()], words(&[&archive, &first])].concat()).unwrap();
        let members = read::members(&std::fs::read(&archive).unwrap()).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(elf::defines(&members[0].body).unwrap(), ["pg_strncasecmp"]);
        run(&[archive.display().to_string()]).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_member_that_is_not_elf_is_refused() {
        let dir = std::env::temp_dir().join(format!("rpg-ar-foreign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let object = dir.join("a.o");
        std::fs::write(&object, b"\xcf\xfa\xed\xfe").unwrap();
        let archive = dir.join("liba.a");
        let why = run(&[
            "crs".to_string(),
            archive.display().to_string(),
            object.display().to_string(),
        ])
        .unwrap_err();
        assert!(why.contains("not an ELF object"), "{why}");
        assert!(!archive.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
