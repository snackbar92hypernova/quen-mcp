//! The game's bundles (`POTATO70`): a 32-byte header, the table of contents, then the entries' bytes. Two table
//! layouts: 304-byte entries with a u64 offset (the remaster's bundles and REDkit's packer: the header's word at 20 ends
//! in 5), and 320-byte entries with a u32 offset (an older packer a community mod ships: it ends in 3). An entry is raw
//! (compression 0), a zlib stream (1) or an LZ4 block (4, 5); its crc32 is of the decompressed bytes. Read-only.

use std::io::{self, Read, Seek, SeekFrom};

use flate2::read::ZlibDecoder;
use sha2::{Digest, Sha256};

use crate::journal::hex;

const MAGIC: &[u8; 8] = b"POTATO70";
const HEADER: usize = 32;
/// A table of contents larger than this isn't a bundle's (the game's largest is 10 MB).
const TOC_MAX: usize = 256 << 20;

/// One entry of the table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// As the table spells it, `\` between folders.
    pub name: String,
    pub offset: u64,
    pub size: u32,
    pub zsize: u32,
    /// Of the decompressed bytes.
    pub crc32: u32,
    pub compression: u32,
}

/// A bundle's table of contents.
#[derive(Debug, Clone)]
pub struct Toc {
    /// The entry stride: 304 or 320.
    pub layout: usize,
    pub entries: Vec<Entry>,
    /// sha256 of the header and the table: the bundle's identity without reading its gigabytes.
    pub sha256: String,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// The table of contents of the bundle `r`. Err: not a bundle, or a table layout quen doesn't know (the header's
/// words in the reason).
pub fn toc(r: &mut (impl Read + Seek)) -> Result<Toc, String> {
    let mut header = [0u8; HEADER];
    r.seek(SeekFrom::Start(0))
        .and_then(|_| r.read_exact(&mut header))
        .map_err(|e| format!("its header: {e}"))?;
    if &header[..8] != MAGIC {
        return Err(format!("not a bundle: it begins {}", hex(&header[..8])));
    }
    let size = u32_at(&header, 16) as usize;
    let word = u32_at(&header, 20);
    let layout = match word & 0xFFFF {
        5 => 304,
        3 => 320,
        _ => 0,
    };
    if layout == 0 || !size.is_multiple_of(layout) || size > TOC_MAX {
        return Err(format!(
            "a table layout quen doesn't know: header words {word:#010x} {:#010x} {:#010x}, table size {size}",
            u32_at(&header, 24),
            u32_at(&header, 28)
        ));
    }
    let mut table = vec![0; size];
    r.read_exact(&mut table)
        .map_err(|e| format!("its table of contents: {e}"))?;
    let mut sha = Sha256::new();
    sha.update(header);
    sha.update(&table);
    let entries = table
        .chunks_exact(layout)
        .map(|e| {
            let name = e[..256].split(|&b| b == 0).next().unwrap_or_default();
            let name = name.iter().map(|&b| char::from(b)).collect();
            let at = |i| u32_at(e, i);
            match layout {
                304 => Entry {
                    name,
                    offset: u64::from(at(272)) | u64::from(at(276)) << 32,
                    size: at(280),
                    zsize: at(284),
                    crc32: at(288),
                    compression: at(292),
                },
                _ => Entry {
                    name,
                    offset: u64::from(at(284)),
                    size: at(276),
                    zsize: at(280),
                    crc32: at(312),
                    compression: at(316),
                },
            }
        })
        .collect();
    Ok(Toc {
        layout,
        entries,
        sha256: hex(&sha.finalize()),
    })
}

/// Entry `e`'s bytes from the bundle `r` of `len` bytes: whole (`limit` None: exactly `size` bytes, else an error), or
/// at least its first `limit` bytes (fewer when the entry is shorter). The crc is the caller's to check
/// ([`crc_ok`]). Err: the entry runs past the bundle's end, a compression quen doesn't read, or bytes that don't
/// decompress.
pub fn read(
    r: &mut (impl Read + Seek),
    len: u64,
    e: &Entry,
    limit: Option<usize>,
) -> Result<Vec<u8>, String> {
    if e.offset.saturating_add(e.zsize.into()) > len {
        return Err(format!(
            "runs past the bundle's end: offset {} + {} bytes > {len}",
            e.offset, e.zsize
        ));
    }
    let size = e.size as usize;
    let want = limit.map_or(size, |l| l.min(size));
    let io = |e: io::Error| e.to_string();
    r.seek(SeekFrom::Start(e.offset)).map_err(io)?;
    let mut raw = r.take(e.zsize.into());
    // Grown as read: `size` is the table's word, not yet the bytes'.
    let mut out = Vec::new();
    match e.compression {
        0 => {
            raw.take(want as u64).read_to_end(&mut out).map_err(io)?;
        }
        1 => {
            let unzip = ZlibDecoder::new(raw)
                .take(want as u64)
                .read_to_end(&mut out);
            unzip.map_err(|e| format!("zlib: {e}"))?;
        }
        4 | 5 => {
            let mut src = Vec::with_capacity(e.zsize as usize);
            raw.read_to_end(&mut src).map_err(io)?;
            out = lz4_block(&src, want)?;
        }
        c => return Err(format!("compression {c}, which quen doesn't read")),
    }
    if limit.is_none() && out.len() != size {
        return Err(format!(
            "decompressed to {} bytes, the table says {size}",
            out.len()
        ));
    }
    Ok(out)
}

/// Whether `bytes` (the whole entry) have the crc32 the table gives.
pub fn crc_ok(e: &Entry, bytes: &[u8]) -> bool {
    crc32fast::hash(bytes) == e.crc32
}

/// Decodes an LZ4 block (no frame), stopping once `limit` bytes are out. Malformed input is an error, never a panic.
pub fn lz4_block(src: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let bad = || "a malformed LZ4 block".to_string();
    let mut out = Vec::with_capacity(limit.min(src.len().saturating_mul(4)));
    let mut i = 0;
    // A length: the token's nibble, then while it is 15 each next byte added until one under 255.
    let length = |i: &mut usize, mut n: usize| -> Result<usize, String> {
        if n == 15 {
            loop {
                let b = *src.get(*i).ok_or_else(bad)?;
                *i += 1;
                n += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    while i < src.len() && out.len() < limit {
        let token = src[i];
        i += 1;
        let n = length(&mut i, usize::from(token >> 4))?;
        out.extend_from_slice(src.get(i..i + n).ok_or_else(bad)?);
        i += n;
        // The last sequence has literals only.
        if i >= src.len() || out.len() >= limit {
            break;
        }
        let back = usize::from(u16::from_le_bytes([
            src[i],
            *src.get(i + 1).ok_or_else(bad)?,
        ]));
        i += 2;
        if back == 0 || back > out.len() {
            return Err(bad());
        }
        let m = (length(&mut i, usize::from(token & 15))? + 4).min(limit - out.len());
        // Byte by byte: a match may overlap what it copies.
        let from = out.len() - back;
        for k in 0..m {
            out.push(out[from + k]);
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::{Cursor, Write};

    /// "abc", then a match 3 back for 9, then "!": 13 bytes.
    pub(crate) const LZ4: [u8; 8] = [0x35, b'a', b'b', b'c', 3, 0, 0x10, b'!'];
    pub(crate) const LZ4_OUT: &[u8] = b"abcabcabcabc!";

    pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(data).unwrap();
        z.finish().unwrap()
    }

    /// A bundle of `layout` holding `files`: (name, the bytes as stored, the size and crc32 of the decompressed bytes,
    /// compression).
    pub(crate) fn bundle(layout: usize, files: &[(&str, Vec<u8>, &[u8], u32)]) -> Vec<u8> {
        let toc = layout * files.len();
        let mut out = MAGIC.to_vec();
        let word: u32 = if layout == 304 {
            0x8df0_0005
        } else {
            0x0001_0003
        };
        for w in [0u32, 0, toc as u32, word, 0, 0] {
            out.extend(w.to_le_bytes());
        }
        let mut data: Vec<u8> = Vec::new();
        let mut table = Vec::new();
        for (name, stored, plain, compression) in files {
            let offset = (HEADER + toc + data.len()) as u32;
            let mut e = vec![0u8; layout];
            e[..name.len()].copy_from_slice(name.as_bytes());
            let put = |e: &mut Vec<u8>, at: usize, v: u32| {
                e[at..at + 4].copy_from_slice(&v.to_le_bytes())
            };
            let (size, zsize, crc) = (
                plain.len() as u32,
                stored.len() as u32,
                crc32fast::hash(plain),
            );
            if layout == 304 {
                for (at, v) in [
                    (272, offset),
                    (280, size),
                    (284, zsize),
                    (288, crc),
                    (292, *compression),
                ] {
                    put(&mut e, at, v);
                }
            } else {
                for (at, v) in [
                    (276, size),
                    (280, zsize),
                    (284, offset),
                    (312, crc),
                    (316, *compression),
                ] {
                    put(&mut e, at, v);
                }
            }
            table.extend(e);
            data.extend(stored);
        }
        out.extend(table);
        out.extend(data);
        out
    }

    /// Both layouts read back byte for byte, raw, zlib and LZ4, whole and as a prefix; the crc checks.
    #[test]
    fn both_layouts_read_back() {
        let text = b"hello, bundle".repeat(20);
        let b304 = bundle(
            304,
            &[
                ("a\\raw.txt", b"raw bytes".to_vec(), b"raw bytes", 0),
                ("b\\z.txt", zlib(&text), &text, 1),
                ("c\\lz.txt", LZ4.to_vec(), LZ4_OUT, 5),
            ],
        );
        let b320 = bundle(320, &[("d\\lz.txt", LZ4.to_vec(), LZ4_OUT, 4)]);
        for (bytes, layout, want) in [
            (&b304, 304, vec![&b"raw bytes"[..], &text, LZ4_OUT]),
            (&b320, 320, vec![LZ4_OUT]),
        ] {
            let mut r = Cursor::new(bytes);
            let toc = toc(&mut r).unwrap();
            assert_eq!(toc.layout, layout);
            assert_eq!(toc.sha256.len(), 64);
            for (e, want) in toc.entries.iter().zip(want) {
                let got = read(&mut r, bytes.len() as u64, e, None).unwrap();
                assert_eq!(got, want, "{}", e.name);
                assert!(crc_ok(e, &got));
                let head = read(&mut r, bytes.len() as u64, e, Some(3)).unwrap();
                assert!(head.len() >= 3 && want.starts_with(&head), "{}", e.name);
            }
        }
        let mut r = Cursor::new(&b304);
        let toc = toc(&mut r).unwrap();
        assert_eq!(toc.entries[1].name, "b\\z.txt");
        assert!(!crc_ok(&toc.entries[1], b"other"));
    }

    /// An unknown header word or an entry past the end is an error naming why, never a panic; so is a broken LZ4
    /// block.
    #[test]
    fn bad_bundles_and_blocks_are_errors() {
        let mut b = bundle(304, &[("x", b"abc".to_vec(), b"abc", 0)]);
        let mut odd = b.clone();
        odd[20] = 7;
        let why = toc(&mut Cursor::new(&odd)).unwrap_err();
        assert!(
            why.contains("layout") && why.contains("0x8df00007"),
            "{why}"
        );
        assert!(toc(&mut Cursor::new(b"POTATO71")).is_err());
        let e = toc(&mut Cursor::new(&b)).unwrap().entries.remove(0);
        let short = b.len() as u64 - 1;
        let why = read(&mut Cursor::new(&b), short, &e, None).unwrap_err();
        assert!(why.contains("past the bundle's end"), "{why}");
        b.truncate(b.len() - 1);
        assert!(read(&mut Cursor::new(&b), b.len() as u64 + 1, &e, None).is_err());
        let unknown = Entry {
            compression: 2,
            ..e.clone()
        };
        assert!(read(&mut Cursor::new(&b), 1 << 20, &unknown, None).is_err());
        for broken in [&[0x35, b'a'][..], &[0x05, 9, 0], &[0xf0]] {
            assert!(lz4_block(broken, 100).is_err(), "{broken:?}");
        }
        assert_eq!(lz4_block(&LZ4, 5).unwrap().len(), 5);
    }
}
