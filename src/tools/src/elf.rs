//! What a 64 bit ELF file links (`DT_NEEDED`) and whether it is static, read
//! straight from its headers. Enough for the AppImage: the interface, the
//! libraries it pulls in, and the AppImage runtime.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;

/// Whether the file starts like an ELF file.
pub fn is_elf(path: &Path) -> bool {
    let mut head = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == *b"\x7fELF"
}

struct Header {
    little: bool,
    phoff: u64,
    phentsize: u16,
    phnum: u16,
}

fn read_at(f: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn u16_of(b: &[u8], little: bool) -> u16 {
    let a = [b[0], b[1]];
    if little {
        u16::from_le_bytes(a)
    } else {
        u16::from_be_bytes(a)
    }
}

fn u32_of(b: &[u8], little: bool) -> u32 {
    let a: [u8; 4] = b[..4].try_into().unwrap();
    if little {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    }
}

fn u64_of(b: &[u8], little: bool) -> u64 {
    let a: [u8; 8] = b[..8].try_into().unwrap();
    if little {
        u64::from_le_bytes(a)
    } else {
        u64::from_be_bytes(a)
    }
}

/// The ELF header of a 64 bit file; `None` for a 32 bit or non ELF one.
fn header(f: &mut File) -> io::Result<Option<Header>> {
    let ident = read_at(f, 0, 64)?;
    if &ident[..4] != b"\x7fELF" || ident[4] != 2 {
        return Ok(None);
    }
    let little = ident[5] == 1;
    Ok(Some(Header {
        little,
        phoff: u64_of(&ident[32..], little),
        phentsize: u16_of(&ident[54..], little),
        phnum: u16_of(&ident[56..], little),
    }))
}

/// `(type, offset, vaddr, filesz)` of every program header.
fn segments(f: &mut File, h: &Header) -> io::Result<Vec<(u32, u64, u64, u64)>> {
    let mut out = Vec::with_capacity(h.phnum as usize);
    for i in 0..u64::from(h.phnum) {
        let p = read_at(f, h.phoff + i * u64::from(h.phentsize), 40)?;
        out.push((
            u32_of(&p, h.little),
            u64_of(&p[8..], h.little),
            u64_of(&p[16..], h.little),
            u64_of(&p[32..], h.little),
        ));
    }
    Ok(out)
}

/// The libraries a 64 bit ELF file names in its `DT_NEEDED` entries, in
/// order. Anything else answers an empty list.
pub fn needed(path: &Path) -> io::Result<Vec<String>> {
    let mut f = File::open(path)?;
    let Some(h) = header(&mut f)? else {
        return Ok(Vec::new());
    };
    let segs = segments(&mut f, &h)?;
    let Some(&(_, dyn_off, _, dyn_size)) = segs.iter().find(|s| s.0 == PT_DYNAMIC) else {
        return Ok(Vec::new());
    };
    let table = read_at(&mut f, dyn_off, dyn_size as usize)?;
    let (mut names, mut strtab) = (Vec::new(), None);
    for entry in table.as_chunks::<16>().0 {
        let tag = u64_of(entry, h.little) as i64;
        let value = u64_of(&entry[8..], h.little);
        match tag {
            DT_NULL => break,
            DT_NEEDED => names.push(value),
            DT_STRTAB => strtab = Some(value),
            _ => {}
        }
    }
    let Some(strtab) = strtab else { return Ok(Vec::new()) };
    let Some(base) = segs
        .iter()
        .filter(|s| s.0 == PT_LOAD)
        .find(|s| s.2 <= strtab && strtab < s.2 + s.3)
        .map(|s| s.1 + strtab - s.2)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for at in names {
        f.seek(SeekFrom::Start(base + at))?;
        let mut raw = Vec::new();
        (&mut f).take(4096).read_to_end(&mut raw)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        out.push(String::from_utf8_lossy(&raw[..end]).into_owned());
    }
    Ok(out)
}

/// Whether a 64 bit ELF file asks for a program interpreter (`PT_INTERP`):
/// a dynamically linked executable.
pub fn has_interpreter(path: &Path) -> io::Result<bool> {
    let mut f = File::open(path)?;
    let Some(h) = header(&mut f)? else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a 64 bit ELF file"));
    };
    Ok(segments(&mut f, &h)?.iter().any(|s| s.0 == PT_INTERP))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64 bit little endian ELF file: one load segment holding a
    /// string table, a dynamic segment naming two libraries, and an
    /// interpreter segment when asked.
    pub(crate) fn tiny_elf(interp: bool) -> Vec<u8> {
        let strtab = b"\0libfoo.so.1\0libbar.so.2\0";
        let phnum: u16 = if interp { 3 } else { 2 };
        let ph_at = 64u64;
        let data_at = ph_at + 56 * u64::from(phnum);
        let str_at = data_at;
        let dyn_at = str_at + strtab.len() as u64;
        let mut dynamic = Vec::new();
        for (tag, value) in [(DT_NEEDED, 1u64), (DT_NEEDED, 13), (DT_STRTAB, str_at), (DT_NULL, 0)] {
            dynamic.extend_from_slice(&tag.to_le_bytes());
            dynamic.extend_from_slice(&value.to_le_bytes());
        }
        let mut f = vec![0u8; 64];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        f[32..40].copy_from_slice(&ph_at.to_le_bytes());
        f[54..56].copy_from_slice(&56u16.to_le_bytes());
        f[56..58].copy_from_slice(&phnum.to_le_bytes());
        let total = dyn_at + dynamic.len() as u64;
        let ph = |kind: u32, off: u64, size: u64| {
            let mut p = vec![0u8; 56];
            p[..4].copy_from_slice(&kind.to_le_bytes());
            p[8..16].copy_from_slice(&off.to_le_bytes());
            p[16..24].copy_from_slice(&off.to_le_bytes());
            p[32..40].copy_from_slice(&size.to_le_bytes());
            p
        };
        f.extend(ph(PT_LOAD, 0, total));
        f.extend(ph(PT_DYNAMIC, dyn_at, dynamic.len() as u64));
        if interp {
            f.extend(ph(PT_INTERP, 0, 1));
        }
        f.extend_from_slice(strtab);
        f.extend(dynamic);
        f
    }

    #[test]
    fn the_libraries_an_elf_file_needs_are_read_in_order() {
        let dir = std::env::temp_dir().join(format!("corvus-elf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny");
        std::fs::write(&path, tiny_elf(true)).unwrap();
        assert!(is_elf(&path));
        assert_eq!(needed(&path).unwrap(), ["libfoo.so.1", "libbar.so.2"]);
        assert!(has_interpreter(&path).unwrap());
        std::fs::write(&path, tiny_elf(false)).unwrap();
        assert!(!has_interpreter(&path).unwrap(), "a static file has no interpreter");
        std::fs::write(&path, b"#!/bin/sh\necho\n").unwrap();
        assert!(!is_elf(&path));
        assert!(has_interpreter(&path).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
