//! The members of an archive that is already there, for the `r` that adds to one.
//!
//! Only the System V layout GNU ar and `rucc-archive` write on ELF targets: a member called `/` for
//! the index, which is dropped since it is written again, a member called `//` holding the names
//! longer than fifteen bytes, and every other member named either in its header up to a `/` or as
//! `/` and an offset into that table.

use rucc_archive::{MAGIC, Member};

/// The size of a member header.
const HEADER: usize = 60;

/// Every member but the index, in the order the archive has them.
pub fn members(bytes: &[u8]) -> Result<Vec<Member>, String> {
    let mut rest = bytes
        .strip_prefix(MAGIC)
        .ok_or("not an archive: it does not start with !<arch>")?;
    let mut long_names: &[u8] = &[];
    let mut members = Vec::new();
    while !rest.is_empty() {
        if rest.len() < HEADER || &rest[58..60] != b"`\n" {
            return Err("a member header is cut short or malformed".to_string());
        }
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&rest[range]).trim_end().to_string()
        };
        let name = field(0..16);
        let size: usize = field(48..58)
            .parse()
            .map_err(|_| format!("member {name} has no size"))?;
        let body = rest
            .get(HEADER..HEADER + size)
            .ok_or_else(|| format!("member {name} runs past the end of the archive"))?;
        rest = rest.get(HEADER + size + size % 2..).unwrap_or_default();
        match name.as_str() {
            "/" | "/SYM64/" => {}
            "//" => long_names = body,
            _ => {
                let name = match name.strip_prefix('/') {
                    Some(offset) => long_name(long_names, offset)?,
                    None => name.strip_suffix('/').unwrap_or(&name).to_string(),
                };
                members.push(Member::new(name, body.to_vec()));
            }
        }
    }
    Ok(members)
}

/// A name in the `//` member, which ends at `/` and a new line.
fn long_name(table: &[u8], offset: &str) -> Result<String, String> {
    let at: usize = offset
        .parse()
        .map_err(|_| format!("member name /{offset} is neither a name nor an offset"))?;
    let rest = table
        .get(at..)
        .ok_or_else(|| format!("member name /{offset} is past the table of long names"))?;
    let end = rest
        .windows(2)
        .position(|w| w == b"/\n")
        .ok_or_else(|| format!("the long name at {at} has no end"))?;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_archive::{Flavour, write};

    #[test]
    fn what_rucc_archive_writes_reads_back_without_its_index() {
        let written = [
            Member::new("a.o", b"odd".to_vec()).defining(vec!["a".to_string()]),
            Member::new("pg_lzcompress_and_more.o", b"even".to_vec())
                .defining(vec!["pglz_compress".to_string()]),
        ];
        let bytes = write(Flavour::Gnu, &written).unwrap();
        let read = members(&bytes).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].name, "a.o");
        assert_eq!(read[0].body, b"odd");
        assert_eq!(read[1].name, "pg_lzcompress_and_more.o");
        assert_eq!(read[1].body, b"even");
    }

    #[test]
    fn an_empty_archive_has_no_members_and_anything_else_is_refused() {
        assert!(members(MAGIC).unwrap().is_empty());
        assert!(members(b"not an archive").is_err());
        let mut cut = MAGIC.to_vec();
        cut.extend_from_slice(
            b"a.o/            0           0     0     644     10        `\nshort",
        );
        assert!(members(&cut).is_err());
    }
}
