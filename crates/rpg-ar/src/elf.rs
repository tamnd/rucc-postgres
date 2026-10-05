//! The names an ELF object defines, which is all an archive's symbol index needs from it.
//!
//! `rucc-archive` is told what each member defines rather than reading the members, so this is the
//! reading. It is the part GNU ar does with BFD: walk the symbol table and keep every global, weak
//! or unique symbol that is defined here or is common, hidden ones included, since visibility is a
//! question for the linker after it has pulled the member in.

/// A symbol with no section, which some other object defines.
const SHN_UNDEF: u16 = 0;
/// `SHT_SYMTAB`, the full symbol table.
const SHT_SYMTAB: u32 = 2;
/// `STB_GLOBAL`, `STB_WEAK` and `STB_GNU_UNIQUE`.
const EXTERNAL: [u8; 3] = [1, 2, 10];

/// Whether the bytes are an ELF object.
#[must_use]
pub fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

/// The external names an ELF object defines, in symbol table order.
pub fn defines(bytes: &[u8]) -> Result<Vec<String>, String> {
    let file = File::read(bytes)?;
    let mut names = Vec::new();
    for index in 0..file.sections {
        let section = file.section(index)?;
        if section.kind != SHT_SYMTAB {
            continue;
        }
        let strings = file.section(section.link)?;
        let size = if file.wide { 24 } else { 16 };
        // Entry zero is the null symbol every table starts with.
        for at in (size..section.size).step_by(size) {
            let entry = file.slice(section.offset + at, size)?;
            let (name, info, shndx) = if file.wide {
                (file.u32(entry, 0), entry[4], file.u16(entry, 6))
            } else {
                (file.u32(entry, 0), entry[12], file.u16(entry, 14))
            };
            if shndx == SHN_UNDEF || !EXTERNAL.contains(&(info >> 4)) {
                continue;
            }
            names.push(file.string(&strings, name as usize)?);
        }
    }
    Ok(names)
}

/// The header fields this needs, with the class and byte order that say how to read the rest.
struct File<'a> {
    bytes: &'a [u8],
    wide: bool,
    big: bool,
    shoff: usize,
    shentsize: usize,
    sections: u32,
}

struct Section {
    kind: u32,
    offset: usize,
    size: usize,
    link: u32,
}

impl<'a> File<'a> {
    fn read(bytes: &'a [u8]) -> Result<Self, String> {
        if !is_elf(bytes) || bytes.len() < 52 {
            return Err("not an ELF object".to_string());
        }
        let wide = match bytes[4] {
            1 => false,
            2 => true,
            other => return Err(format!("ELF class {other} is neither 32 nor 64 bit")),
        };
        let big = match bytes[5] {
            1 => false,
            2 => true,
            other => return Err(format!("ELF data encoding {other} is neither byte order")),
        };
        let mut file = File {
            bytes,
            wide,
            big,
            shoff: 0,
            shentsize: 0,
            sections: 0,
        };
        let (shoff, shentsize, shnum) = if wide {
            if bytes.len() < 64 {
                return Err("ELF header cut short".to_string());
            }
            (
                usize::try_from(file.u64(bytes, 0x28))
                    .map_err(|_| "section headers out of reach")?,
                file.u16(bytes, 0x3a),
                file.u16(bytes, 0x3c),
            )
        } else {
            (
                file.u32(bytes, 0x20) as usize,
                file.u16(bytes, 0x2e),
                file.u16(bytes, 0x30),
            )
        };
        file.shoff = shoff;
        file.shentsize = usize::from(shentsize);
        file.sections = u32::from(shnum);
        // More sections than the header's sixteen bits hold puts the count in section zero's size.
        if shnum == 0 && shoff != 0 {
            file.sections = u32::try_from(file.section(0)?.size)
                .map_err(|_| "more sections than anything could hold")?;
        }
        Ok(file)
    }

    fn section(&self, index: u32) -> Result<Section, String> {
        let header = self.slice(self.shoff + index as usize * self.shentsize, self.shentsize)?;
        let need = if self.wide { 64 } else { 40 };
        if header.len() < need {
            return Err(format!("section header {index} is {} bytes", header.len()));
        }
        let size = |value: u64| usize::try_from(value).map_err(|_| "a section out of reach");
        Ok(if self.wide {
            Section {
                kind: self.u32(header, 4),
                offset: size(self.u64(header, 24))?,
                size: size(self.u64(header, 32))?,
                link: self.u32(header, 40),
            }
        } else {
            Section {
                kind: self.u32(header, 4),
                offset: self.u32(header, 16) as usize,
                size: self.u32(header, 20) as usize,
                link: self.u32(header, 24),
            }
        })
    }

    fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], String> {
        offset
            .checked_add(len)
            .and_then(|end| self.bytes.get(offset..end))
            .ok_or_else(|| format!("{len} bytes at {offset} run past the end of the object"))
    }

    fn string(&self, table: &Section, at: usize) -> Result<String, String> {
        let bytes = self.slice(table.offset, table.size)?;
        let rest = bytes
            .get(at..)
            .ok_or_else(|| format!("a symbol name at {at} is past its string table"))?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| format!("the symbol name at {at} has no end"))?;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    fn u16(&self, bytes: &[u8], at: usize) -> u16 {
        let b = [bytes[at], bytes[at + 1]];
        if self.big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    }

    fn u32(&self, bytes: &[u8], at: usize) -> u32 {
        let mut b = [0; 4];
        b.copy_from_slice(&bytes[at..at + 4]);
        if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    }

    fn u64(&self, bytes: &[u8], at: usize) -> u64 {
        let mut b = [0; 8];
        b.copy_from_slice(&bytes[at..at + 8]);
        if self.big {
            u64::from_be_bytes(b)
        } else {
            u64::from_le_bytes(b)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 64 bit little-endian object with a null section, a symbol table and its strings, holding
    /// the symbols given as name, binding and section index.
    pub fn object(symbols: &[(&str, u8, u16)]) -> Vec<u8> {
        let mut strings = vec![0u8];
        let mut table = vec![0u8; 24];
        for (name, bind, shndx) in symbols {
            let at = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            let mut entry = vec![0u8; 24];
            entry[0..4].copy_from_slice(&at.to_le_bytes());
            entry[4] = (bind << 4) | 2;
            entry[6..8].copy_from_slice(&shndx.to_le_bytes());
            table.extend(entry);
        }
        let table_at = 64;
        let strings_at = table_at + table.len();
        let headers_at = strings_at + strings.len();
        let mut bytes = vec![0u8; 64];
        bytes[0..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[0x28..0x30].copy_from_slice(&(headers_at as u64).to_le_bytes());
        bytes[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        bytes[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        bytes.extend(&table);
        bytes.extend(&strings);
        let header = |kind: u32, offset: usize, size: usize, link: u32| {
            let mut h = vec![0u8; 64];
            h[4..8].copy_from_slice(&kind.to_le_bytes());
            h[24..32].copy_from_slice(&(offset as u64).to_le_bytes());
            h[32..40].copy_from_slice(&(size as u64).to_le_bytes());
            h[40..44].copy_from_slice(&link.to_le_bytes());
            h
        };
        bytes.extend(header(0, 0, 0, 0));
        bytes.extend(header(SHT_SYMTAB, table_at, table.len(), 2));
        bytes.extend(header(3, strings_at, strings.len(), 0));
        bytes
    }

    #[test]
    fn the_index_takes_what_is_defined_and_external() {
        let bytes = object(&[
            ("local_helper", 0, 1),
            ("palloc", 1, 0),
            ("pg_strong_random", 1, 1),
            ("weak_default", 2, 1),
            ("common_counter", 1, 0xfff2),
            ("unique_table", 10, 1),
        ]);
        assert_eq!(
            defines(&bytes).unwrap(),
            [
                "pg_strong_random",
                "weak_default",
                "common_counter",
                "unique_table"
            ]
        );
    }

    #[test]
    fn an_object_with_no_symbols_defines_nothing() {
        assert!(defines(&object(&[])).unwrap().is_empty());
    }

    #[test]
    fn a_short_or_foreign_file_is_refused() {
        assert!(defines(b"\x7fELF").is_err());
        assert!(defines(b"\xcf\xfa\xed\xfe and the rest of a Mach-O file").is_err());
        let mut cut = object(&[("f", 1, 1)]);
        cut.truncate(cut.len() - 10);
        assert!(defines(&cut).is_err());
    }
}
