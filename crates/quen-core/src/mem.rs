//! The platform-neutral half of the live-memory tools (`mem.read`, `mem.scan`, `mem.find`, `mem.follow`, `mem.walk`,
//! `mem.patch` / `mem.unpatch`), against a [`Memory`] trait the Windows layer implements with VirtualQueryEx and
//! ReadProcessMemory.
//!
//! Results give addresses, pointers and u64 values as 0x hex strings and i64 values as decimal strings (a JSON number
//! loses precision past 2^53 in a JavaScript client); [`place`] then names where each address lies.
//!
//! Nothing here writes. A patch [`plan`] checks the KB definition (`patches.tsv`) and the live bytes, and returns the one
//! [`Write`] allowed, or a [`Refusal`]. The platform layer journals it as an
//! [`Action::GameMemory`](crate::journal::Action::GameMemory) record (its time taints later saves), then makes it with a
//! handle open only for the write. An instance patch (`instance_patches.tsv`) is planned by [`plan_instances`], one
//! write per live instance of a class, and made through the platform's write by [`write_instances`], which checks each
//! instance again right before its write.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::ops::{ControlFlow, Range};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::build::Stamp;
use crate::journal::sha256_hex;
use crate::seeds::{rva, tsv_rows};

/// A process's memory, read only: the game quen's job started, or a fake in tests.
pub trait Memory {
    /// Fills `buf` from `address` on: all of it, or an error.
    fn read(&self, address: u64, buf: &mut [u8]) -> io::Result<()>;
    /// The readable regions (committed, neither guard nor no-access pages), in address order.
    fn regions(&self) -> io::Result<Vec<Region>>;
}

/// One readable region, as VirtualQueryEx reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Region {
    #[serde(serialize_with = "hex::number")]
    pub base: u64,
    pub size: u64,
    pub kind: Kind,
    /// Its protection as VirtualQueryEx's `Protect` gives it (Windows' `PAGE_*` bits), shown as 0x hex.
    #[serde(serialize_with = "hex::number")]
    pub protect: u32,
    /// The image section the region lies in (`.text`, `.rdata`), when the platform layer knows it.
    pub section: Option<String>,
}

/// The regions of one kind, protection and section: how many, and their bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub kind: Kind,
    #[serde(serialize_with = "hex::number")]
    pub protect: u32,
    pub section: Option<String>,
    pub count: usize,
    pub bytes: u64,
}

/// `regions` grouped by kind, protection and section, most bytes first.
pub fn census(regions: &[Region]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for r in regions {
        let same =
            |g: &&mut Group| (g.kind, g.protect, &g.section) == (r.kind, r.protect, &r.section);
        match groups.iter_mut().find(same) {
            Some(g) => (g.count, g.bytes) = (g.count + 1, g.bytes + r.size),
            None => groups.push(Group {
                kind: r.kind,
                protect: r.protect,
                section: r.section.clone(),
                count: 1,
                bytes: r.size,
            }),
        }
    }
    groups.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    groups
}

/// What backs a region (VirtualQueryEx's `MEM_IMAGE`, `MEM_MAPPED`, `MEM_PRIVATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Image,
    Mapped,
    Private,
}

/// The most bytes one raw read returns.
pub const MAX_BYTES: usize = 4096;

/// What `mem.read` reads at an address: a little-endian number (`Ptr`: a u64 shown as 0x hex), or 1 to
/// [`MAX_BYTES`] raw bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    U8,
    U16,
    U32,
    U64,
    I32,
    I64,
    F32,
    F64,
    Ptr,
    Bytes(usize),
}

impl Type {
    /// The type `name` names; `bytes` takes `len`, and only it.
    pub fn named(name: &str, len: Option<usize>) -> Result<Type, String> {
        let ty = match name {
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "f32" => Type::F32,
            "f64" => Type::F64,
            "ptr" => Type::Ptr,
            "bytes" => return len.map(Type::Bytes).ok_or("bytes needs len".into()),
            other => {
                return Err(format!(
                    "type {other:?}: u8 u16 u32 u64 i32 i64 f32 f64 ptr or bytes"
                ));
            }
        };
        match len {
            Some(_) => Err("len is for type bytes only".into()),
            None => Ok(ty),
        }
    }

    /// How many bytes a value of this type takes.
    pub fn size(self) -> usize {
        match self {
            Type::U8 => 1,
            Type::U16 => 2,
            Type::U32 | Type::I32 | Type::F32 => 4,
            Type::U64 | Type::I64 | Type::F64 | Type::Ptr => 8,
            Type::Bytes(len) => len,
        }
    }

    /// The value in `b`, which is [`Type::size`] bytes.
    fn decode(self, b: &[u8]) -> Value {
        match self {
            Type::U8 => Value::U8(b[0]),
            Type::U16 => Value::U16(u16::from_le_bytes(sized(b))),
            Type::U32 => Value::U32(u32::from_le_bytes(sized(b))),
            Type::U64 => Value::U64(u64::from_le_bytes(sized(b))),
            Type::I32 => Value::I32(i32::from_le_bytes(sized(b))),
            Type::I64 => Value::I64(i64::from_le_bytes(sized(b))),
            Type::F32 => Value::F32(f32::from_le_bytes(sized(b))),
            Type::F64 => Value::F64(f64::from_le_bytes(sized(b))),
            Type::Ptr => Value::Ptr(u64::from_le_bytes(sized(b))),
            Type::Bytes(_) => Value::Bytes(b.to_vec()),
        }
    }

    /// `text` as this type's little-endian bytes: a whole number ([`number`]) that fits it, a float, or hex bytes of
    /// its length.
    fn encode(self, text: &str) -> Result<Vec<u8>, String> {
        let size = self.size();
        let int = |range: Range<i128>| {
            number(text)
                .filter(|n| range.contains(n))
                .map(|n| n.to_le_bytes()[..size].to_vec())
        };
        let bytes = match self {
            Type::F32 => text.parse::<f32>().ok().map(|f| f.to_le_bytes().to_vec()),
            Type::F64 => text.parse::<f64>().ok().map(|f| f.to_le_bytes().to_vec()),
            Type::Bytes(_) => parse_hex(text).ok().filter(|b| b.len() == size),
            Type::I32 | Type::I64 => {
                let half = 1i128 << (8 * size - 1);
                int(-half..half)
            }
            _ => int(0..1 << (8 * size)),
        };
        let ty = format!("{self:?}").to_lowercase();
        bytes.ok_or_else(|| format!("{text:?} is not a {ty}"))
    }
}

/// A value read. It serializes as a JSON number (serde_json writes a NaN or an infinity as `null`), a pointer or a u64
/// as 0x hex, an i64 as a decimal string, or raw bytes as spaced hex.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(#[serde(serialize_with = "hex::number")] u64),
    I32(i32),
    I64(#[serde(serialize_with = "hex::decimal")] i64),
    F32(f32),
    F64(f64),
    Ptr(#[serde(serialize_with = "hex::number")] u64),
    Bytes(#[serde(with = "hex")] Vec<u8>),
}

/// Reads one value of type `ty` at `address`: the value, and the bytes it was read from.
pub fn read(mem: &dyn Memory, address: u64, ty: Type) -> io::Result<(Value, Vec<u8>)> {
    if let Type::Bytes(len) = ty
        && !(1..=MAX_BYTES).contains(&len)
    {
        let why = format!("{len} bytes: a raw read is 1 to {MAX_BYTES}");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, why));
    }
    let mut b = vec![0; ty.size()];
    mem.read(address, &mut b)?;
    Ok((ty.decode(&b), b))
}

/// A whole number in text: decimal, or `0x` hex, either with a `-` before it.
pub fn number(text: &str) -> Option<i128> {
    let (negative, s) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None => s.parse().ok()?,
    };
    Some(if negative { -n } else { n })
}

fn sized<const N: usize>(b: &[u8]) -> [u8; N] {
    b.try_into().expect("read sizes the buffer by its type")
}

/// A byte pattern: memory matches at `i` where `(byte ^ bytes[i]) & mask[i] == 0`, at addresses that are a multiple
/// of `align`; or, built by [`Pattern::qwords`], where the qword is one of `qwords`. It always has a fixed bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    mask: Vec<u8>,
    align: u64,
    /// The values a qword pattern matches, sorted; empty for any other pattern.
    qwords: Vec<u64>,
}

impl Pattern {
    /// IDA-style text, one whitespace-separated token per byte: two hex digits, `??` or `?` for any byte, or a digit
    /// and a `?` (`4?`, `?5`) for half of one. `48 8B ?? ?? 05`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let half = |c: u8| match c {
            b'?' => Some((0, 0)),
            _ => nibble(c).map(|n| (n, 0xf)),
        };
        let (mut bytes, mut mask) = (Vec::new(), Vec::new());
        for token in text.split_whitespace() {
            let byte = match *token.as_bytes() {
                [b'?'] => Some((0, 0)),
                [hi, lo] => half(hi)
                    .zip(half(lo))
                    .map(|((h, hm), (l, lm))| (h << 4 | l, hm << 4 | lm)),
                _ => None,
            };
            let (byte, m) =
                byte.ok_or_else(|| format!("{token:?} is not a byte: `48`, `??` or `4?`"))?;
            bytes.push(byte);
            mask.push(m);
        }
        Self::new(bytes, mask)
    }

    /// Narrows each byte's mask by bits: `mask` is hex, one byte per pattern byte (`ff ff 00 00 f0`).
    pub fn masked(self, mask: &str) -> Result<Self, String> {
        let narrow = parse_hex(mask)?;
        if narrow.len() != self.mask.len() {
            let (n, m) = (narrow.len(), self.mask.len());
            return Err(format!("{n} mask bytes for a {m}-byte pattern"));
        }
        let mask = self.mask.iter().zip(narrow).map(|(m, n)| m & n).collect();
        Self::new(self.bytes, mask)
    }

    /// The bytes of an ASCII string, exactly (no terminator).
    pub fn ascii(text: &str) -> Result<Self, String> {
        if !text.is_ascii() {
            return Err(format!("{text:?} is not ASCII: scan it as UTF-16LE or hex"));
        }
        Self::new(text.as_bytes().to_vec(), vec![0xff; text.len()])
    }

    /// The UTF-16LE code units of a string, exactly (no terminator).
    pub fn utf16le(text: &str) -> Result<Self, String> {
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mask = vec![0xff; bytes.len()];
        Self::new(bytes, mask)
    }

    /// Any of `values` (one or more) as a little-endian qword, at 8-byte-aligned addresses only.
    pub fn qwords(values: &[u64]) -> Self {
        let mut qwords = values.to_vec();
        qwords.sort_unstable();
        qwords.dedup();
        let bytes = qwords.first().copied().unwrap_or(0).to_le_bytes().into();
        Self {
            bytes,
            mask: vec![0xff; 8],
            align: 8,
            qwords,
        }
    }

    /// Refuses a pattern that matches everywhere: empty, or wildcards only.
    fn new(bytes: Vec<u8>, mask: Vec<u8>) -> Result<Self, String> {
        if mask.iter().all(|&m| m == 0) {
            return Err("the pattern has no fixed bit: it would match everywhere".into());
        }
        let bytes = bytes.iter().zip(&mask).map(|(b, m)| b & m).collect();
        Ok(Self {
            bytes,
            mask,
            align: 1,
            qwords: Vec::new(),
        })
    }

    fn matches(&self, at: &[u8]) -> bool {
        if !self.qwords.is_empty() {
            return self
                .qwords
                .binary_search(&u64::from_le_bytes(sized(at)))
                .is_ok();
        }
        at.iter()
            .zip(&self.bytes)
            .zip(&self.mask)
            .all(|((b, p), m)| (b ^ p) & m == 0)
    }

    /// Calls `each` with the address of every match that starts in `b` (read at `at`) and lies wholly in it, ascending,
    /// until `each` breaks. Only aligned starts are tried; a qword pattern is one u64 compare against the range of its
    /// values, then a search of them, any other pattern first checks its most-fixed byte.
    fn hits(
        &self,
        at: u64,
        b: &[u8],
        mut each: impl FnMut(u64) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let n = self.bytes.len();
        let Some(last) = b.len().checked_sub(n) else {
            return ControlFlow::Continue(());
        };
        let first = (at.next_multiple_of(self.align) - at) as usize;
        let starts = (first..=last).step_by(self.align as usize);
        if let (Some(&lo), Some(&hi)) = (self.qwords.first(), self.qwords.last()) {
            for i in starts {
                let q = u64::from_le_bytes(sized(&b[i..i + 8]));
                if (lo..=hi).contains(&q) && self.qwords.binary_search(&q).is_ok() {
                    each(at + i as u64)?;
                }
            }
            return ControlFlow::Continue(());
        }
        let k = (0..n)
            .max_by_key(|&k| self.mask[k].count_ones())
            .unwrap_or(0);
        let (key, m) = (self.bytes[k], self.mask[k]);
        for i in starts {
            if (b[i + k] ^ key) & m == 0 && self.matches(&b[i..i + n]) {
                each(at + i as u64)?;
            }
        }
        ControlFlow::Continue(())
    }
}

/// Windows' `PAGE_NOCACHE | PAGE_WRITECOMBINE`: a [`Scope`] leaves out the regions whose protection has either bit,
/// unless it asks for them. Measured on one PC (2026-10-19, Witcher 3 5.01): 13.42 GB of the game's 21.06 GB readable
/// was private 0x404, read at 107 MB/s against 2,390 MB/s for private 0x4.
pub const UNCACHED: u32 = 0x200 | 0x400;

/// Which regions a scan reads: those `which` names, less the ones whose protection has an [`UNCACHED`] bit unless
/// `uncached`. A region named by base is read whatever its protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub which: Which,
    pub uncached: bool,
}

impl From<Which> for Scope {
    fn from(which: Which) -> Self {
        Scope {
            which,
            uncached: false,
        }
    }
}

/// Which regions a [`Scope`] names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Which {
    /// Every region [`Memory::regions`] lists.
    All,
    /// The regions in these sections (`.text`, `.rdata`).
    Sections(Vec<String>),
    /// The regions at these bases, as [`Memory::regions`] lists them.
    Regions(Vec<u64>),
    /// The regions of these kinds.
    Kinds(Vec<Kind>),
}

/// The most hits one scan page returns.
pub const MAX_HITS: usize = 1000;
/// The longest single read of a scan: a bigger region is read in chunks, each overlapping the next by the pattern's
/// length less one byte, so no match is lost between them.
pub(crate) const CHUNK: u64 = 1 << 20;
/// A memory page: what a chunk that can't be read whole is read again in.
pub const PAGE: u64 = 0x1000;

/// One page of scan hits, and what the page covered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanPage<H = u64> {
    /// Where the pattern matches, ascending ([`find`]'s: rows, with their context).
    pub hits: Vec<H>,
    /// The `from` of the next page when this one is full or stopped; `None` when the scope is done.
    #[serde(serialize_with = "hex::option")]
    pub next: Option<u64>,
    /// Why the page stopped before its scope was done, when its tick said so (`deadline`, `cancelled`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<&'static str>,
    /// The regions this page searched, in whole or in part, and the bytes it covered in them (from a region's base or
    /// `from`, to the region's end or where the page stopped), the skipped ones included.
    pub regions: usize,
    pub bytes: u64,
    /// What in scope couldn't be read, so may hold matches nobody saw: runs of unreadable pages, or a region that
    /// wasn't listed.
    /// ponytail: unbounded; a dying process lists every region here. Cap it if that ever floods a result.
    pub skipped: Vec<Skipped>,
    /// The regions the scope named but left out for their protection ([`UNCACHED`]), so may hold matches nobody saw.
    pub left_out: LeftOut,
}

/// How many regions a scope left out, and their bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LeftOut {
    pub regions: usize,
    pub bytes: u64,
}

/// A part of the scope that wasn't read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    #[serde(serialize_with = "hex::number")]
    pub base: u64,
    /// 0 when the region wasn't listed at all.
    pub size: u64,
    pub why: String,
}

/// Searches `scope` for `pattern`, from address `from` on, for at most `limit` hits (clamped to 1 ..= [`MAX_HITS`]).
/// Before each chunk but the page's first it calls `tick(bytes covered so far, bytes in scope)`, which may stop the
/// page (`stopped`, its reason; `next`, the first address not searched). Pass a full or stopped page's `next` as
/// `from` for the page after it: no hit is seen twice.
/// ponytail: a match never spans two regions, even adjacent ones.
pub fn scan(
    mem: &dyn Memory,
    pattern: &Pattern,
    scope: &Scope,
    from: u64,
    limit: usize,
    tick: &mut dyn FnMut(u64, u64) -> ControlFlow<&'static str>,
) -> io::Result<ScanPage> {
    let limit = limit.clamp(1, MAX_HITS);
    let mut page = ScanPage::default();
    let mut regions = mem.regions()?;
    regions.retain(|r| match &scope.which {
        Which::All => true,
        Which::Sections(names) => r.section.as_ref().is_some_and(|s| names.contains(s)),
        Which::Regions(bases) => bases.contains(&r.base),
        Which::Kinds(kinds) => kinds.contains(&r.kind),
    });
    let named = matches!(scope.which, Which::Regions(_));
    let leave = |r: &Region| !scope.uncached && !named && r.protect & UNCACHED != 0;
    for r in regions.iter().filter(|r| leave(r)) {
        page.left_out.regions += 1;
        page.left_out.bytes += r.size;
    }
    regions.retain(|r| !leave(r));
    if let Which::Regions(bases) = &scope.which {
        for &base in bases
            .iter()
            .filter(|&&b| !regions.iter().any(|r| r.base == b))
        {
            let why = "not a readable region now".into();
            page.skipped.push(Skipped { base, size: 0, why });
        }
    }
    regions.sort_by_key(|r| r.base);
    let spans: Vec<(u64, u64)> = regions
        .iter()
        .map(|r| (r.base.max(from), r.base.saturating_add(r.size)))
        .filter(|(start, end)| start < end)
        .collect();
    let total = spans.iter().map(|(start, end)| end - start).sum();
    let tail = pattern.bytes.len() as u64 - 1;
    let (mut buf, mut done) = (Vec::new(), 0);
    for (start, end) in spans {
        page.regions += 1;
        let flow = chunks(
            mem,
            start,
            end - start,
            tail,
            &mut buf,
            &mut page.skipped,
            |at, b| {
                page.bytes = done + (at - start);
                if page.bytes > 0
                    && let ControlFlow::Break(why) = tick(page.bytes, total)
                {
                    (page.stopped, page.next) = (Some(why), Some(at));
                    return ControlFlow::Break(());
                }
                pattern.hits(at, b, |hit| {
                    page.hits.push(hit);
                    if page.hits.len() < limit {
                        return ControlFlow::Continue(());
                    }
                    (page.bytes, page.next) = (done + hit + 1 - start, Some(hit + 1));
                    ControlFlow::Break(())
                })
            },
        );
        if flow.is_break() {
            return Ok(page);
        }
        done += end - start;
        page.bytes = done;
    }
    Ok(page)
}

/// The most context bytes a [`find`] hit has on each side, and the most values one find looks for.
pub const MAX_CONTEXT: u64 = 256;
pub const MAX_VALUES: usize = 64;

/// What [`find`] looks for, and what it reads at each hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Find {
    /// The qwords to find: 1 to [`MAX_VALUES`].
    pub values: Vec<u64>,
    /// Bytes on each side of a hit's qword (clamped to [`MAX_CONTEXT`]), cut at the edges of its region.
    pub context: u64,
    /// Read at each hit, at offsets from its address; none, or as [`walk`] reads them.
    pub fields: Vec<Field>,
    pub filter: Option<Filter>,
}

/// [`scan`]'s page for `f`'s values at 8-byte-aligned addresses in `scope`. Each hit is a row: `{address, value,
/// before, after, <field>: value, ...}` (`value`: the one it matched, the qword there), or `{address, why}` when its
/// bytes can't be read; the filter drops rows, never a `why` one. With the page, the sha256 of the bytes its rows were
/// read from ([`digest`]). Err: refused, nothing read; or the regions couldn't be listed.
pub fn find(
    mem: &dyn Memory,
    f: &Find,
    scope: &Scope,
    from: u64,
    limit: usize,
    tick: &mut dyn FnMut(u64, u64) -> ControlFlow<&'static str>,
) -> Result<(ScanPage<Row>, String), String> {
    let n = f.values.len();
    if !(1..=MAX_VALUES).contains(&n) {
        return Err(format!("{n} values: a find takes 1 to {MAX_VALUES}"));
    }
    let keys = [
        "address", "value", "before", "after", "why", "rva", "module",
    ];
    let room = "0x10000 from the hit";
    let layout = Layout::new(&f.fields, f.filter.as_ref(), MAX_STRIDE, room, &keys)?;
    let listed = |e: io::Error| format!("the regions: {e}");
    let pattern = Pattern::qwords(&f.values);
    let page = scan(mem, &pattern, scope, from, limit, tick).map_err(listed)?;
    let regions = mem.regions().map_err(listed)?;
    let n = f.context.min(MAX_CONTEXT);
    let (mut hits, mut reads) = (Vec::new(), Vec::new());
    for &address in &page.hits {
        let region = regions
            .iter()
            .find(|r| r.base <= address && address < r.base + r.size);
        let (lo, hi) = region.map_or((address, address + 8), |r| {
            let hi = address.saturating_add(8 + n).min(r.base + r.size);
            (address.saturating_sub(n).max(r.base), hi)
        });
        let mut b = vec![0; (hi - lo) as usize];
        let at = (address - lo) as usize;
        let mut row = Row::from_iter([("address".into(), json!(format!("{address:#x}")))]);
        match mem.read(lo, &mut b) {
            Ok(()) => {
                let value = u64::from_le_bytes(sized(&b[at..at + 8]));
                row.insert("value".into(), json!(format!("{value:#x}")));
                row.insert("before".into(), json!(to_hex(&b[..at])));
                row.insert("after".into(), json!(to_hex(&b[at + 8..])));
                if !f.fields.is_empty() && !layout.read(mem, address, &mut row, &mut reads) {
                    continue;
                }
                reads.push((lo, b));
            }
            Err(e) => {
                row.insert("why".into(), json!(e.to_string()));
            }
        }
        hits.push(row);
    }
    let page = ScanPage {
        hits,
        next: page.next,
        stopped: page.stopped,
        regions: page.regions,
        bytes: page.bytes,
        skipped: page.skipped,
        left_out: page.left_out,
    };
    Ok((page, digest(reads)))
}

/// What changed from one [`find`]'s hits (`a`) to another's (`b`), matched by `address`: `added` (the hits only `b`
/// has), `removed` (only `a`), `changed` (`{address, <key>: {a, b}}`, every other key whose values differ, a missing one
/// as null), in address order; `counts` of each and of the hits that are the `same`.
pub fn diff(a: &[Row], b: &[Row]) -> serde_json::Value {
    fn keyed(rows: &[Row]) -> BTreeMap<i128, &Row> {
        let at = |r: &Row| number(r.get("address")?.as_str()?);
        rows.iter().filter_map(|r| Some((at(r)?, r))).collect()
    }
    let (a, b) = (keyed(a), keyed(b));
    let (mut removed, mut changed, mut same) = (Vec::new(), Vec::new(), 0);
    for (at, was) in &a {
        let Some(now) = b.get(at) else {
            removed.push(was);
            continue;
        };
        let keys: BTreeSet<&String> = was.keys().chain(now.keys()).collect();
        let mut d = Row::from_iter([("address".into(), was["address"].clone())]);
        for k in keys.into_iter().filter(|k| *k != "address") {
            if was.get(k) != now.get(k) {
                d.insert(k.clone(), json!({"a": was.get(k), "b": now.get(k)}));
            }
        }
        match d.len() {
            1 => same += 1,
            _ => changed.push(d),
        }
    }
    let added: Vec<_> = b
        .iter()
        .filter(|(at, _)| !a.contains_key(at))
        .map(|(_, r)| r)
        .collect();
    let counts = json!({"added": added.len(), "removed": removed.len(), "changed": changed.len(), "same": same});
    json!({"counts": counts, "added": added, "removed": removed, "changed": changed})
}

/// One hop of a pointer chain: the qword read at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Hop {
    #[serde(serialize_with = "hex::number")]
    pub at: u64,
    #[serde(serialize_with = "hex::number")]
    pub value: u64,
}

/// A pointer chain followed: every hop read, then its end or where it broke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chain {
    pub hops: Vec<Hop>,
    /// The last value read (the base, with no offsets), when every hop was read.
    #[serde(serialize_with = "hex::option")]
    pub end: Option<u64>,
    pub broken: Option<Broken>,
}

/// The hop a chain broke at (0-based, one per offset), and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Broken {
    pub hop: usize,
    pub why: String,
}

/// Follows a 64-bit pointer chain from `base`: each hop adds its offset to the pointer so far and reads the qword there
/// as the next. Offsets `[a, b]` give `[[base + a] + b]`; read a field at `end` plus its offset.
pub fn follow(mem: &dyn Memory, base: u64, offsets: &[i64]) -> Chain {
    let mut chain = Chain {
        hops: Vec::new(),
        end: None,
        broken: None,
    };
    let mut pointer = base;
    for (hop, &offset) in offsets.iter().enumerate() {
        let mut b = [0; 8];
        let at = match pointer.checked_add_signed(offset) {
            _ if pointer == 0 => Err(match hop {
                0 => "the base is null".to_string(),
                _ => format!("hop {} read a null pointer", hop - 1),
            }),
            None => Err(format!("{pointer:#x} plus offset {offset} overflows")),
            Some(at) => mem
                .read(at, &mut b)
                .map(|()| at)
                .map_err(|e| format!("can't read 8 bytes at {at:#x}: {e}")),
        };
        match at {
            Ok(at) => {
                pointer = u64::from_le_bytes(b);
                chain.hops.push(Hop { at, value: pointer });
            }
            Err(why) => {
                chain.broken = Some(Broken { hop, why });
                return chain;
            }
        }
    }
    chain.end = Some(pointer);
    chain
}

/// The most elements a walk reads, its longest stride, and the most bytes it reads in all: per element, its struct's
/// bytes from its first field to the end of its last, and 8 per deref hop.
pub const MAX_COUNT: u64 = 100_000;
pub const MAX_STRIDE: u64 = 64 * 1024;
pub const MAX_WALK: u64 = 16 << 20;

/// Structs to read: per element, the struct its fields are read from or, with `deref`, the start of a pointer chain to
/// it ([`follow`]'s offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    pub elements: Elements,
    pub deref: Vec<i64>,
    pub fields: Vec<Field>,
    pub filter: Option<Filter>,
}

/// Where a [`Walk`]'s elements are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elements {
    /// `count` elements `stride` bytes apart from `base`.
    Array { base: u64, stride: u64, count: u64 },
    /// These addresses, in this order.
    List(Vec<u64>),
}

/// A field of a walked struct: a value of `ty` at `offset` from the struct's start, before it when negative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub offset: i64,
    pub ty: Type,
}

/// Keeps the rows whose field `field` holds `value` (`eq`), or the rows whose field doesn't: the field's bytes against
/// `value` written as the field's type ([`number`], a float, or hex bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub field: String,
    pub eq: bool,
    pub value: String,
}

/// A walk's or a find's row, as JSON.
pub type Row = serde_json::Map<String, serde_json::Value>;

/// Fields checked for reading: the span of bytes they cover, from `lo` bytes past a struct's start, and the filter as
/// its offset in that span, eq, and the bytes it compares.
struct Layout<'a> {
    fields: &'a [Field],
    lo: i64,
    len: usize,
    filter: Option<(usize, bool, Vec<u8>)>,
}

impl<'a> Layout<'a> {
    /// Checks `fields` and `filter`: names that aren't one of the row's own `keys`, each named once, each field
    /// starting at most [`MAX_STRIDE`] bytes before a struct's start and ending at most `room` bytes after it (`what`
    /// says what bounds it).
    fn new(
        fields: &'a [Field],
        filter: Option<&Filter>,
        room: u64,
        what: &str,
        keys: &[&str],
    ) -> Result<Self, String> {
        for (i, f) in fields.iter().enumerate() {
            let name = &f.name;
            if name.is_empty() || keys.contains(&name.as_str()) {
                let keys = keys.join(", ");
                return Err(format!("field name {name:?}: blank, or one of {keys}"));
            }
            if fields[..i].iter().any(|g| g.name == *name) {
                return Err(format!("field {name} is named twice"));
            }
            if let Type::Bytes(len) = f.ty
                && !(1..=MAX_BYTES).contains(&len)
            {
                return Err(format!(
                    "field {name}: {len} bytes, a field is 1 to {MAX_BYTES}"
                ));
            }
            if f.offset < -(MAX_STRIDE as i64) {
                return Err(format!(
                    "field {name} starts over {MAX_STRIDE:#x} bytes before the struct"
                ));
            }
            if f.offset.saturating_add(f.ty.size() as i64) > room as i64 {
                return Err(format!("field {name} ends past {room:#x}: {what}"));
            }
        }
        let lo = fields.iter().map(|f| f.offset).min().unwrap_or(0);
        let hi = fields.iter().map(|f| f.offset + f.ty.size() as i64);
        let len = (hi.max().unwrap_or(0) - lo) as usize;
        let filter = match filter {
            None => None,
            Some(Filter { field, eq, value }) => {
                let f = fields.iter().find(|f| f.name == *field);
                let f = f.ok_or(format!("filter: no field {field}"))?;
                let want = f.ty.encode(value).map_err(|e| format!("filter: {e}"))?;
                Some(((f.offset - lo) as usize, *eq, want))
            }
        };
        Ok(Layout {
            fields,
            lo,
            len,
            filter,
        })
    }

    /// Reads the fields of the struct at `at` into `row`, or `why` they couldn't be read, and the bytes into `reads`:
    /// false when the filter drops the row.
    fn read(
        &self,
        mem: &dyn Memory,
        at: u64,
        row: &mut Row,
        reads: &mut Vec<(u64, Vec<u8>)>,
    ) -> bool {
        let (mut b, len) = (vec![0; self.len], self.len);
        let start = at.checked_add_signed(self.lo);
        let read = start
            .ok_or("past the top of memory".to_string())
            .and_then(|s| {
                mem.read(s, &mut b)
                    .map(|()| s)
                    .map_err(|e| format!("can't read {len} bytes at {s:#x}: {e}"))
            });
        let start = match read {
            Ok(start) => start,
            Err(why) => {
                row.insert("why".into(), json!(why));
                return true;
            }
        };
        if let Some((at, eq, want)) = &self.filter
            && (b[*at..*at + want.len()] == want[..]) != *eq
        {
            return false;
        }
        for f in self.fields {
            let at = (f.offset - self.lo) as usize;
            let value = f.ty.decode(&b[at..at + f.ty.size()]);
            row.insert(f.name.clone(), json!(value));
        }
        reads.push((start, b));
        true
    }
}

/// Reads `w`: per element, `{index, address, <field>: value, ...}`, with `target` (the struct's address) when it has a
/// deref, or `{index, address, why}` when it can't be read; `index` is the element's position. The filter drops rows,
/// never a `why` one. With the rows, the sha256 of the bytes they were read from ([`digest`]). Err: refused, nothing
/// read.
pub fn walk(mem: &dyn Memory, w: &Walk) -> Result<(Vec<Row>, String), String> {
    let (count, room) = match &w.elements {
        Elements::Array { stride, count, .. } => {
            if !(1..=MAX_COUNT).contains(count) {
                return Err(format!("count {count}: a walk is 1 to {MAX_COUNT}"));
            }
            if !(1..=MAX_STRIDE).contains(stride) {
                return Err(format!("stride {stride}: 1 to {MAX_STRIDE}"));
            }
            // Without a deref the struct is the element; past one, it is read up to MAX_STRIDE bytes from its start.
            let room = if w.deref.is_empty() {
                *stride
            } else {
                MAX_STRIDE
            };
            (*count, room)
        }
        Elements::List(at) => {
            let n = at.len() as u64;
            if !(1..=MAX_COUNT).contains(&n) {
                return Err(format!("{n} addresses: a walk is 1 to {MAX_COUNT}"));
            }
            (n, MAX_STRIDE)
        }
    };
    if w.fields.is_empty() {
        return Err("no fields".into());
    }
    let what = format!("the stride, or {MAX_STRIDE:#x} from a deref's target or a listed address");
    let keys = ["index", "address", "target", "why", "rva", "module"];
    let layout = Layout::new(&w.fields, w.filter.as_ref(), room, &what, &keys)?;
    let total = count * (layout.len as u64 + 8 * w.deref.len() as u64);
    if total > MAX_WALK {
        return Err(format!(
            "{total} bytes to read: a walk reads at most {MAX_WALK}"
        ));
    }
    let (mut rows, mut reads) = (Vec::new(), Vec::new());
    // ponytail: one read per element (and per hop), ~100k syscalls at the cap; read whole arrays if that is slow.
    for index in 0..count {
        let address = match &w.elements {
            Elements::Array { base, stride, .. } => base.checked_add(index * stride),
            Elements::List(at) => Some(at[index as usize]),
        };
        let mut row = Row::from_iter([
            ("index".into(), json!(index)),
            ("address".into(), json!(address.map(|a| format!("{a:#x}")))),
        ]);
        let target = match address {
            None => Err("past the top of memory".to_string()),
            Some(a) if w.deref.is_empty() => Ok(a),
            Some(a) => {
                let chain = follow(mem, a, &w.deref);
                let broken = chain.broken.map(|b| format!("deref: {}", b.why));
                chain.end.ok_or_else(|| broken.unwrap_or_default())
            }
        };
        match target {
            Err(why) => {
                row.insert("why".into(), json!(why));
            }
            Ok(t) => {
                if !w.deref.is_empty() {
                    row.insert("target".into(), json!(format!("{t:#x}")));
                }
                if !layout.read(mem, t, &mut row, &mut reads) {
                    continue;
                }
            }
        }
        rows.push(row);
    }
    Ok((rows, digest(reads)))
}

/// The sha256 (hex) of `reads`, each a range read (its address and bytes), concatenated in address order.
/// ponytail: holds every range read until the end; a find's 1000 hits with fields 128 KiB wide is 128 MiB.
fn digest(mut reads: Vec<(u64, Vec<u8>)>) -> String {
    reads.sort_by_key(|(at, _)| *at);
    let bytes: Vec<&[u8]> = reads.iter().map(|(_, b)| &b[..]).collect();
    sha256_hex(&bytes.concat())
}

/// A module loaded in a process, as its module list gives it: its file name, base and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub base: u64,
    pub size: u64,
}

/// Puts, beside every 0x string in `v` (its objects and arrays walked through) that lies in one of `modules` (in
/// address order), where it lies: in the image at `image`, its RVA (`rva` beside `address`, `<key>_rva` beside
/// another key); in another module, `name+0xoffset` (`module`, `<key>_module`).
pub fn place(v: &mut serde_json::Value, image: u64, modules: &[Module]) {
    match v {
        serde_json::Value::Array(items) => items.iter_mut().for_each(|i| place(i, image, modules)),
        serde_json::Value::Object(map) => {
            let mut beside = Vec::new();
            for (key, item) in map.iter_mut() {
                place(item, image, modules);
                let hex = item.as_str().and_then(|s| s.strip_prefix("0x"));
                let Some(a) = hex.and_then(|h| u64::from_str_radix(h, 16).ok()) else {
                    continue;
                };
                let below = modules.partition_point(|m| m.base <= a);
                let module = below.checked_sub(1).map(|i| &modules[i]);
                let Some(m) = module.filter(|m| a - m.base < m.size) else {
                    continue;
                };
                let (what, at) = match m.base == image {
                    true => ("rva", format!("{:#x}", a - m.base)),
                    false => ("module", format!("{}+{:#x}", m.name, a - m.base)),
                };
                let key = match key.as_str() {
                    "address" => what.to_string(),
                    _ => format!("{key}_{what}"),
                };
                beside.push((key, json!(at)));
            }
            map.extend(beside);
        }
        _ => {}
    }
}

/// A PE image as loaded in memory, from its headers: the build stamp the KB names builds by, and its sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
    pub base: u64,
    /// The file header's TimeDateStamp, written as the KB writes it ([`Stamp`]).
    pub stamp: String,
    pub sections: Vec<Section>,
}

/// One section of an [`Image`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub name: String,
    /// The image base plus its RVA, and its VirtualSize.
    pub base: u64,
    pub size: u64,
    /// Its characteristics (`IMAGE_SCN_*`).
    pub flags: u32,
}

/// `IMAGE_SCN_MEM_EXECUTE` and `IMAGE_SCN_MEM_WRITE`.
pub const SCN_EXECUTE: u32 = 0x2000_0000;
pub const SCN_WRITE: u32 = 0x8000_0000;

/// Reads the PE headers of the image loaded at `base`.
pub fn image(mem: &dyn Memory, base: u64) -> io::Result<Image> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{base:#x}: {why}"));
    let at = |offset: u64, len: usize| {
        let mut b = vec![0; len];
        mem.read(
            base.checked_add(offset)
                .ok_or_else(|| bad("past the top"))?,
            &mut b,
        )?;
        Ok::<_, io::Error>(b)
    };
    let pe = u64::from(u32::from_le_bytes(sized(&at(0x3c, 4)?)));
    let header = at(pe, 24)?;
    if header[..4] != *b"PE\0\0" {
        return Err(bad("no PE header"));
    }
    let count = u16::from_le_bytes(sized(&header[6..8]));
    let stamp = Stamp(u32::from_le_bytes(sized(&header[8..12]))).to_string();
    let optional = u64::from(u16::from_le_bytes(sized(&header[20..22])));
    let table = at(pe + 24 + optional, 40 * usize::from(count))?;
    let sections = table
        .chunks(40)
        .map(|s| {
            let word = |i: usize| u32::from_le_bytes(sized(&s[i..i + 4]));
            let name = &s[..8];
            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(8)];
            Section {
                name: String::from_utf8_lossy(name).into_owned(),
                base: base + u64::from(word(12)),
                size: word(8).into(),
                flags: word(36),
            }
        })
        .collect();
    Ok(Image {
        base,
        stamp,
        sections,
    })
}

/// Reads `[base, base + size)` into `buf` (reused across calls) in chunks of at most [`CHUNK`] + `tail` bytes, each
/// starting [`CHUNK`] after the last, and calls `f(address, bytes)` on what it read, until `f` breaks. The windows of
/// `tail + 1` bytes that lie wholly in what `f` gets and start before the next chunk are that chunk's, so none is seen
/// twice or lost at a seam. A chunk that can't be read whole is read again page by page: `f` gets each run of readable
/// pages, and the pages that can't be read go in `skipped` (adjacent ones as one).
pub(crate) fn chunks(
    mem: &dyn Memory,
    base: u64,
    size: u64,
    tail: u64,
    buf: &mut Vec<u8>,
    skipped: &mut Vec<Skipped>,
    mut f: impl FnMut(u64, &[u8]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let end = base.saturating_add(size);
    let mut at = base;
    while end - at > tail {
        let len = (end - at).min(CHUNK + tail);
        let next = at.saturating_add(CHUNK);
        // Windows starting at or past `own` are the next chunk's, when there is one.
        let own = if end.saturating_sub(next) > tail {
            next
        } else {
            end
        };
        if buf.len() < len as usize {
            buf.resize(len as usize, 0);
        }
        let b = &mut buf[..len as usize];
        if mem.read(at, b).is_ok() {
            f(at, b)?;
        } else {
            let mut run = at;
            let mut p = at;
            while p < at + len {
                let q = (p + 1).next_multiple_of(PAGE).min(at + len);
                let page = (p - at) as usize..(q - at) as usize;
                if let Err(e) = mem.read(p, &mut b[page]) {
                    if run < p && run < own {
                        f(run, &b[(run - at) as usize..(p - at) as usize])?;
                    }
                    if p < own {
                        let size = q.min(own) - p;
                        match skipped.last_mut() {
                            Some(s) if s.base + s.size == p => s.size += size,
                            _ => skipped.push(Skipped {
                                base: p,
                                size,
                                why: e.to_string(),
                            }),
                        }
                    }
                    run = q;
                }
                p = q;
            }
            if run < own && run < at + len {
                f(run, &b[(run - at) as usize..])?;
            }
        }
        if own == end {
            break;
        }
        at = next;
    }
    ControlFlow::Continue(())
}

/// The most bytes a patch changes, all inside one 8-byte-aligned qword.
pub const MAX_PATCH: usize = 8;

const PATCHES_HEADER: [&str; 6] = ["name", "rva", "old", "new", "build", "boundaries-checked"];

/// One `patches.tsv` row: a live patch definition from the KB. The patch rules are checked by [`plan`], so a bad
/// row refuses only its own patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub name: String,
    /// Relative to the image base of the build it is for.
    pub rva: u32,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    /// The build stamp, as the KB writes it and the platform layer computes it for the running game.
    pub build: String,
    /// Who or when checked, with a disassembler, that `old` and `new` keep the same instruction boundaries. `None`
    /// when the cell is blank, `-`, `0`, `no` or `false`.
    pub boundaries_checked: Option<String>,
}

/// A bad line of a KB table (`patches.tsv`, `cvar_layout.tsv`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsvError {
    pub file: &'static str,
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for TsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} line {}: {}", self.file, self.line, self.msg)
    }
}

impl std::error::Error for TsvError {}

/// The definitions of a KB `patches.tsv`: a header row of `name rva old new build boundaries-checked`, then one row
/// per patch and build. Bytes are hex, spaced or not (`41 0f b6`).
pub fn from_patches_tsv(text: &str) -> Result<Vec<Patch>, TsvError> {
    let err = |line, msg| TsvError {
        file: "patches.tsv",
        line,
        msg,
    };
    let rows = tsv_rows(text, &PATCHES_HEADER).map_err(|(line, msg)| err(line, msg))?;
    let mut patches: Vec<Patch> = Vec::new();
    for (line, [name, rva_cell, old, new, build, checked]) in rows {
        let err = |msg| err(line, msg);
        let seen = patches.iter().any(|p| p.name == name && p.build == build);
        let boundaries_checked = patch_cells(name, build, seen, checked).map_err(err)?;
        let bytes =
            |cell: &str, which: &str| parse_hex(cell).map_err(|msg| err(format!("{which}: {msg}")));
        patches.push(Patch {
            name: name.into(),
            rva: rva(rva_cell).map_err(err)?,
            old: bytes(old, "old")?,
            new: bytes(new, "new")?,
            build: build.into(),
            boundaries_checked,
        });
    }
    Ok(patches)
}

/// Checks a KB patch row's name (not blank, no whitespace) and build (not blank), and that no row before it has both
/// (`seen`); reads its boundaries-checked cell: `None` when blank, `-`, `0`, `no` or `false`.
fn patch_cells(
    name: &str,
    build: &str,
    seen: bool,
    checked: &str,
) -> Result<Option<String>, String> {
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(format!("name {name:?} is blank or has whitespace"));
    }
    if build.is_empty() {
        return Err("the build is blank".into());
    }
    if seen {
        return Err(format!("{name} is defined twice for build {build}"));
    }
    let unchecked = matches!(
        checked.to_ascii_lowercase().as_str(),
        "" | "-" | "0" | "no" | "false"
    );
    Ok((!unchecked).then(|| checked.into()))
}

/// `mem.patch` or `mem.unpatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Patch,
    Unpatch,
}

/// The one write a plan allows: `new` at `address`, which held `old` when planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Write {
    pub address: u64,
    #[serde(with = "hex")]
    pub old: Vec<u8>,
    #[serde(with = "hex")]
    pub new: Vec<u8>,
}

/// Why a plan was refused. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No definition by this name in this table.
    Unknown {
        name: String,
        table: &'static str,
    },
    /// Defined, but not for the running build.
    Build {
        running: String,
        defined: Vec<String>,
    },
    /// `boundaries-checked` is not set.
    Unchecked,
    /// `old` and `new` must be the same length, 1 to [`MAX_PATCH`] bytes.
    Length {
        old: usize,
        new: usize,
    },
    /// `old` and `new` are the same bytes.
    NoChange,
    /// The bytes don't lie inside one 8-byte-aligned qword.
    Straddles {
        address: u64,
        len: usize,
    },
    Unreadable {
        address: u64,
        why: String,
    },
    /// The live bytes are already what this op writes.
    Already(Op),
    /// The live bytes are neither `old` nor `new`: another build, or another writer.
    Mismatch {
        address: u64,
        live: Vec<u8>,
        expected: Vec<u8>,
    },
    /// An instance field that overlaps the vtable pointer, crosses an 8-byte boundary or lies past [`MAX_STRIDE`].
    Field {
        offset: u64,
        len: usize,
    },
    /// The search found over [`MAX_INSTANCES`] instances.
    TooMany(usize),
    /// The search stopped (`deadline`, `cancelled`) after this many bytes.
    Stopped {
        why: &'static str,
        bytes: u64,
    },
    /// The regions couldn't be listed.
    Regions(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { name, table } => write!(f, "no patch named {name:?} in {table}"),
            Self::Build { running, defined } => write!(
                f,
                "defined for build {}, not the running {running}",
                defined.join(", ")
            ),
            Self::Unchecked => f.write_str(
                "boundaries-checked is not set: check the row with a disassembler first",
            ),
            Self::Length { old, new } if old != new => {
                write!(
                    f,
                    "old is {old} bytes and new {new}: they must be the same length"
                )
            }
            Self::Length { old, .. } => write!(f, "{old} bytes: a patch is 1 to {MAX_PATCH}"),
            Self::NoChange => f.write_str("old and new are the same bytes"),
            Self::Straddles { address, len } => write!(
                f,
                "{len} bytes at {address:#x} cross an 8-byte boundary: a patch stays inside one qword"
            ),
            Self::Unreadable { address, why } => write!(f, "can't read {address:#x}: {why}"),
            Self::Already(Op::Patch) => f.write_str("already patched"),
            Self::Already(Op::Unpatch) => f.write_str("not patched: the bytes are the original"),
            Self::Mismatch {
                address,
                live,
                expected,
            } => write!(
                f,
                "{address:#x} holds {}, expected {}: another build or another writer",
                to_hex(live),
                to_hex(expected)
            ),
            Self::Field { offset, len } => write!(
                f,
                "{len} bytes at +{offset:#x}: an instance field starts at +0x8 or later, below \
                 +{MAX_STRIDE:#x}, and stays inside one aligned qword"
            ),
            Self::TooMany(n) => write!(
                f,
                "{n} instances found, over the {MAX_INSTANCES} an instance patch writes: nothing written"
            ),
            Self::Stopped { why, bytes } => write!(
                f,
                "the search stopped ({why}) after {} MiB: nothing written",
                bytes >> 20
            ),
            Self::Regions(why) => write!(f, "the regions: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Plans `op` for the patch `name` in a process running `build`, its image at `image_base`: the [`Write`] to make, or
/// why not. Every patch rule is checked here, then the live bytes: a patch writes only over `old`, an unpatch only
/// over `new`.
pub fn plan(
    patches: &[Patch],
    name: &str,
    op: Op,
    build: &str,
    image_base: u64,
    mem: &dyn Memory,
) -> Result<Write, Refusal> {
    let named: Vec<&Patch> = patches.iter().filter(|p| p.name == name).collect();
    let Some(p) = named.iter().find(|p| p.build == build) else {
        return Err(match named[..] {
            [] => Refusal::Unknown {
                name: name.into(),
                table: "patches.tsv",
            },
            _ => Refusal::Build {
                running: build.into(),
                defined: named.iter().map(|p| p.build.clone()).collect(),
            },
        });
    };
    if p.boundaries_checked.is_none() {
        return Err(Refusal::Unchecked);
    }
    let len = p.old.len();
    if p.new.len() != len || !(1..=MAX_PATCH).contains(&len) {
        let new = p.new.len();
        return Err(Refusal::Length { old: len, new });
    }
    if p.old == p.new {
        return Err(Refusal::NoChange);
    }
    let Some(address) = image_base.checked_add(p.rva.into()) else {
        let why = "the image base plus the RVA overflows".into();
        return Err(Refusal::Unreadable {
            address: image_base,
            why,
        });
    };
    if address % 8 + len as u64 > 8 {
        return Err(Refusal::Straddles { address, len });
    }
    let mut live = vec![0; len];
    if let Err(e) = mem.read(address, &mut live) {
        let why = e.to_string();
        return Err(Refusal::Unreadable { address, why });
    }
    let (old, new) = match op {
        Op::Patch => (&p.old, &p.new),
        Op::Unpatch => (&p.new, &p.old),
    };
    if live == *old {
        let (old, new) = (old.clone(), new.clone());
        Ok(Write { address, old, new })
    } else if live == *new {
        Err(Refusal::Already(op))
    } else {
        let expected = old.clone();
        Err(Refusal::Mismatch {
            address,
            live,
            expected,
        })
    }
}

/// The most live instances one instance patch writes: a search that finds more refuses, nothing written.
pub const MAX_INSTANCES: usize = 1000;

/// Windows' `PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY`: an instance is
/// written only in a region whose protection has one of them.
pub const WRITABLE: u32 = 0x04 | 0x08 | 0x40 | 0x80;

const INSTANCES_HEADER: [&str; 9] = [
    "name",
    "class",
    "vtable",
    "build",
    "offset",
    "type",
    "old",
    "new",
    "boundaries-checked",
];

/// One `instance_patches.tsv` row: a field to write in every live instance of a class, from the KB. The instance rules
/// are checked by [`plan_instances`], so a bad row refuses only its own patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePatch {
    pub name: String,
    /// The class, as the KB names it.
    pub class: String,
    /// Its vtable's RVA in the build the row is for: an instance's first qword is the image base plus it.
    pub vtable: u32,
    pub build: String,
    /// The field: its offset from the instance's start, and its type, whose bytes `old` and `new` are.
    pub offset: u64,
    pub ty: Type,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    /// Who or when checked, with a disassembler, that the class has this field at this offset with this type. `None`
    /// as patches.tsv's.
    pub boundaries_checked: Option<String>,
}

impl InstancePatch {
    /// The row as results give it: its RVA and offset as 0x hex, old and new as values of its type.
    pub fn json(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "class": self.class,
            "vtable_rva": format!("{:#x}", self.vtable),
            "build": self.build,
            "offset": format!("{:#x}", self.offset),
            "type": format!("{:?}", self.ty).to_lowercase(),
            "old": self.ty.decode(&self.old),
            "new": self.ty.decode(&self.new),
            "boundaries_checked": self.boundaries_checked,
        })
    }
}

/// The definitions of a KB `instance_patches.tsv`: a header row of `name class vtable build offset type old new
/// boundaries-checked`, then one row per patch and build. `vtable` is an RVA (0x hex), `offset` a number or 0x hex,
/// `type` one of u8 u16 u32 u64 i32 i64 f32 f64 or bytes, and `old` and `new` values of it: a number, a float, or (bytes)
/// hex bytes, whose count is the width.
pub fn from_instances_tsv(text: &str) -> Result<Vec<InstancePatch>, TsvError> {
    let err = |line, msg| TsvError {
        file: "instance_patches.tsv",
        line,
        msg,
    };
    let rows = tsv_rows(text, &INSTANCES_HEADER).map_err(|(line, msg)| err(line, msg))?;
    let mut defs: Vec<InstancePatch> = Vec::new();
    for (line, [name, class, vtable, build, offset, ty, old, new, checked]) in rows {
        let err = |msg| err(line, msg);
        let seen = defs.iter().any(|d| d.name == name && d.build == build);
        let boundaries_checked = patch_cells(name, build, seen, checked).map_err(err)?;
        if class.is_empty() || class.contains(char::is_whitespace) {
            return Err(err(format!("class {class:?} is blank or has whitespace")));
        }
        let offset = number(offset)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| err(format!("offset {offset:?} is not a number or 0x hex")))?;
        let len = (ty == "bytes").then(|| parse_hex(old).map_or(0, |b| b.len()));
        let ty = match ty {
            "ptr" => Err("type ptr: write a pointer field as u64".to_string()),
            _ => Type::named(ty, len),
        }
        .map_err(err)?;
        let value =
            |cell: &str, which: &str| ty.encode(cell).map_err(|e| err(format!("{which}: {e}")));
        defs.push(InstancePatch {
            name: name.into(),
            class: class.into(),
            vtable: rva(vtable).map_err(err)?,
            build: build.into(),
            offset,
            ty,
            old: value(old, "old")?,
            new: value(new, "new")?,
            boundaries_checked,
        });
    }
    Ok(defs)
}

/// The live instances of a class a search found, and the writes planned in them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instances {
    /// The vtable pointer searched for: the image base plus the row's RVA.
    pub vtable: u64,
    /// The qwords the search found holding it; with a list of instances, those of them not listed.
    pub found: usize,
    pub unlisted: usize,
    /// One per instance whose field held the row's old bytes, in address order: the field's address, old, new.
    pub writes: Vec<Write>,
    /// The instances whose field held the row's new bytes.
    pub already: Vec<u64>,
    /// The other qwords that held the vtable pointer (with a list, the listed ones the search didn't find too), and
    /// why each isn't written.
    pub left: Vec<(u64, String)>,
    /// What the search covered: bytes, the pages it couldn't read, the regions it left out ([`scan`]'s).
    pub bytes: u64,
    pub skipped: Vec<Skipped>,
    pub left_out: LeftOut,
}

/// Plans the instance patch `name` in a process running `build`, its image at `image_base`: checks the row's rules,
/// then finds every 8-byte-aligned qword holding the class's vtable pointer in the image and private regions
/// ([`find`]'s scope, [`UNCACHED`] regions left out), page after page with `tick` as [`scan`] takes it, and reads the
/// field of each; with `only`, of those at the addresses it lists. Refused, nothing written: a rule broken, the search
/// stopped, or over [`MAX_INSTANCES`] found.
pub fn plan_instances(
    defs: &[InstancePatch],
    name: &str,
    build: &str,
    image_base: u64,
    only: Option<&[u64]>,
    mem: &dyn Memory,
    tick: &mut dyn FnMut(u64, u64) -> ControlFlow<&'static str>,
) -> Result<(InstancePatch, Instances), Refusal> {
    let named: Vec<&InstancePatch> = defs.iter().filter(|d| d.name == name).collect();
    let Some(&d) = named.iter().find(|d| d.build == build) else {
        return Err(match named[..] {
            [] => Refusal::Unknown {
                name: name.into(),
                table: "instance_patches.tsv",
            },
            _ => Refusal::Build {
                running: build.into(),
                defined: named.iter().map(|d| d.build.clone()).collect(),
            },
        });
    };
    if d.boundaries_checked.is_none() {
        return Err(Refusal::Unchecked);
    }
    let len = d.old.len();
    if d.new.len() != len || !(1..=MAX_PATCH).contains(&len) {
        let new = d.new.len();
        return Err(Refusal::Length { old: len, new });
    }
    if d.old == d.new {
        return Err(Refusal::NoChange);
    }
    // Instances are 8-byte aligned (their first qword is the vtable pointer), so the offset says where the field lies.
    let offset = d.offset;
    if !(8..MAX_STRIDE).contains(&offset) || offset % 8 + len as u64 > 8 {
        return Err(Refusal::Field { offset, len });
    }
    let Some(vtable) = image_base.checked_add(d.vtable.into()) else {
        let why = "the image base plus the RVA overflows".into();
        return Err(Refusal::Unreadable {
            address: image_base,
            why,
        });
    };
    let pattern = Pattern::qwords(&[vtable]);
    let scope = Scope::from(Which::Kinds(vec![Kind::Image, Kind::Private]));
    let listed = |e: io::Error| Refusal::Regions(e.to_string());
    let mut out = Instances {
        vtable,
        ..Instances::default()
    };
    let (mut hits, mut from) = (Vec::new(), 0);
    loop {
        let page = scan(mem, &pattern, &scope, from, MAX_HITS, tick).map_err(listed)?;
        out.bytes += page.bytes;
        if let Some(why) = page.stopped {
            let bytes = out.bytes;
            return Err(Refusal::Stopped { why, bytes });
        }
        out.skipped.extend(page.skipped);
        out.left_out = page.left_out;
        hits.extend(page.hits);
        if hits.len() > MAX_INSTANCES {
            return Err(Refusal::TooMany(hits.len()));
        }
        match page.next {
            Some(next) => from = next,
            None => break,
        }
    }
    out.found = hits.len();
    // ponytail: linear lookups, at most 1000 x 1000.
    if let Some(only) = only {
        for &at in only.iter().filter(|at| !hits.contains(at)) {
            let why = "the search found no vtable pointer there".to_string();
            if !out.left.iter().any(|(a, _)| *a == at) {
                out.left.push((at, why));
            }
        }
        hits.retain(|at| only.contains(at));
        out.unlisted = out.found - hits.len();
    }
    let regions = mem.regions().map_err(listed)?;
    for at in hits {
        let field = at.saturating_add(offset);
        let region = regions
            .iter()
            .find(|r| r.base <= at && at < r.base + r.size);
        let why = match region {
            None => Some("not in a region listed after the search".to_string()),
            Some(r) if r.protect & WRITABLE == 0 => Some(format!(
                "in a region of protect {:#x}: not writable",
                r.protect
            )),
            Some(r) if field.saturating_add(len as u64) > r.base + r.size => {
                Some("the field lies past the end of its region".into())
            }
            _ => None,
        };
        if let Some(why) = why {
            out.left.push((at, why));
            continue;
        }
        let mut live = vec![0; len];
        match mem.read(field, &mut live) {
            Err(e) => out.left.push((at, format!("can't read the field: {e}"))),
            Ok(()) if live == d.old => out.writes.push(Write {
                address: field,
                old: d.old.clone(),
                new: d.new.clone(),
            }),
            Ok(()) if live == d.new => out.already.push(at),
            Ok(()) => {
                let held = json!(d.ty.decode(&live));
                out.left
                    .push((at, format!("the field holds {held}: neither old nor new")));
            }
        }
    }
    Ok((d.clone(), out))
}

/// Makes each of `writes` with `write`: a field `offset` bytes into an instance whose first qword was `vtable`, written
/// only while that qword is still `vtable` and the field still holds the write's old bytes, both read again right
/// before it. An instance freed and reused for another class has another first qword, so it is left. Per write, in
/// order: made, or why it was left.
/// ponytail: microseconds pass between the reads and the write, in which the game may free the instance; suspend the
/// game's threads around each write if that is ever seen.
pub fn write_instances(
    mem: &dyn Memory,
    vtable: u64,
    offset: u64,
    writes: &[Write],
    write: &mut dyn FnMut(&Write) -> Result<(), String>,
) -> Vec<Result<(), String>> {
    let mut checked = |w: &Write| -> Result<(), String> {
        let at = w.address.saturating_sub(offset);
        let mut first = [0; 8];
        mem.read(at, &mut first)
            .map_err(|e| format!("can't read the instance now: {e}"))?;
        let now = u64::from_le_bytes(first);
        if now != vtable {
            return Err(format!(
                "its first qword is {now:#x} now, not the vtable: freed, or another object"
            ));
        }
        let mut live = vec![0; w.old.len()];
        mem.read(w.address, &mut live)
            .map_err(|e| format!("can't read the field now: {e}"))?;
        if live != w.old {
            return Err(format!(
                "the field holds {} now, not {}: the game wrote it",
                to_hex(&live),
                to_hex(&w.old)
            ));
        }
        write(w)
    };
    writes.iter().map(&mut checked).collect()
}

/// Hex bytes, spaced or not (`41 0f b6`, `410fb6`): each whitespace-separated token is whole bytes.
pub(crate) fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for token in text.split_whitespace() {
        for pair in token.as_bytes().chunks(2) {
            let byte = match *pair {
                [hi, lo] => nibble(hi).zip(nibble(lo)).map(|(h, l)| h << 4 | l),
                _ => None,
            };
            bytes.push(byte.ok_or_else(|| format!("{token:?} is not hex bytes"))?);
        }
    }
    Ok(bytes)
}

fn nibble(c: u8) -> Option<u8> {
    char::from(c).to_digit(16).map(|d| d as u8)
}

/// Spaced lowercase hex (`41 0f b6`).
fn to_hex(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex.join(" ")
}

/// Serde for bytes as spaced lowercase hex, for a number as 0x hex (or none), and for one as a decimal string.
mod hex {
    use serde::Serializer;
    use std::fmt::{Display, LowerHex};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(bytes))
    }

    pub fn number<S: Serializer, N: LowerHex>(n: &N, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{n:#x}"))
    }

    pub fn option<S: Serializer>(n: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match n {
            Some(n) => number(n, s),
            None => s.serialize_none(),
        }
    }

    pub fn decimal<S: Serializer, N: Display>(n: &N, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A process built by the test: regions holding bytes, and address ranges every read fails on.
    #[derive(Default)]
    struct Fake {
        regions: Vec<(Region, Vec<u8>)>,
        bad: Vec<Range<u64>>,
    }

    impl Fake {
        fn with(mut self, base: u64, bytes: impl Into<Vec<u8>>, section: Option<&str>) -> Self {
            let bytes = bytes.into();
            let region = Region {
                base,
                size: bytes.len() as u64,
                kind: section.map_or(Kind::Private, |_| Kind::Image),
                protect: 0x04,
                section: section.map(Into::into),
            };
            self.regions.push((region, bytes));
            self
        }

        /// Writes `bytes` at `address`, inside one region: what a test's game does, or a write a test made.
        fn poke(&mut self, address: u64, bytes: &[u8]) {
            let (r, b) = self
                .regions
                .iter_mut()
                .find(|(r, _)| r.base <= address && address < r.base + r.size)
                .unwrap();
            let at = (address - r.base) as usize;
            b[at..at + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl Memory for Fake {
        fn read(&self, address: u64, buf: &mut [u8]) -> io::Result<()> {
            let end = address.saturating_add(buf.len() as u64);
            let gone = self.bad.iter().any(|b| b.start < end && address < b.end);
            let region = self
                .regions
                .iter()
                .find(|(r, _)| r.base <= address && end <= r.base + r.size);
            match region {
                Some((r, bytes)) if !gone => {
                    let at = (address - r.base) as usize;
                    buf.copy_from_slice(&bytes[at..at + buf.len()]);
                    Ok(())
                }
                _ => Err(io::Error::other(format!(
                    "no {} readable bytes at {address:#x}",
                    buf.len()
                ))),
            }
        }

        fn regions(&self) -> io::Result<Vec<Region>> {
            Ok(self.regions.iter().map(|(r, _)| r.clone()).collect())
        }
    }

    #[test]
    fn typed_reads_are_little_endian() {
        let mut bytes = vec![1, 2, 3, 4, 5, 6, 7, 8];
        bytes.extend(1.5f32.to_le_bytes());
        bytes.extend((-2.25f64).to_le_bytes());
        bytes.extend((-7i32).to_le_bytes());
        let mem = Fake::default().with(0x1000, bytes, None);
        let at = |offset: u64, ty| read(&mem, 0x1000 + offset, ty).unwrap().0;
        assert_eq!(read(&mem, 0x1004, Type::U16).unwrap().1, [5, 6]);
        assert_eq!(at(0, Type::U8), Value::U8(1));
        assert_eq!(at(0, Type::U16), Value::U16(0x0201));
        assert_eq!(at(0, Type::U32), Value::U32(0x0403_0201));
        assert_eq!(at(0, Type::U64), Value::U64(0x0807_0605_0403_0201));
        assert_eq!(at(8, Type::F32), Value::F32(1.5));
        assert_eq!(at(12, Type::F64), Value::F64(-2.25));
        assert_eq!(at(20, Type::I32), Value::I32(-7));
        assert_eq!(at(1, Type::Bytes(3)), Value::Bytes(vec![2, 3, 4]));
        assert_eq!(
            serde_json::to_value(at(1, Type::Bytes(3))).unwrap(),
            "02 03 04"
        );
        assert_eq!(serde_json::to_value(at(20, Type::I32)).unwrap(), -7);
        assert_eq!(at(12, Type::I64), Value::I64((-2.25f64).to_bits() as i64));
        // Past 2^53 a JSON number loses digits in a JavaScript client: u64 and ptr as 0x hex, i64 in decimal.
        assert_eq!(json!(at(0, Type::Ptr)), "0x807060504030201");
        assert_eq!(json!(at(0, Type::U64)), "0x807060504030201");
        let i64_text = ((-2.25f64).to_bits() as i64).to_string();
        assert_eq!(json!(at(12, Type::I64)), json!(i64_text));
        assert!(i64_text.starts_with('-'));
        assert_eq!(Type::named("ptr", None), Ok(Type::Ptr));
        assert_eq!(Type::named("bytes", Some(3)), Ok(Type::Bytes(3)));
        for (name, len, why) in [
            ("bytes", None, "bytes needs len"),
            ("u8", Some(1), "len is for type bytes only"),
            ("u128", None, "u8 u16"),
        ] {
            let err = Type::named(name, len).unwrap_err();
            assert!(err.contains(why), "{name}: {err}");
        }
        for len in [0, MAX_BYTES + 1] {
            let err = read(&mem, 0x1000, Type::Bytes(len)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
        assert!(read(&mem, 0x1000 + 21, Type::U32).is_err(), "past the end");
    }

    #[test]
    fn patterns_take_ida_text_masks_and_strings() {
        let p = Pattern::parse("48 8B ?? ? 4? ?5").unwrap();
        assert_eq!(p.bytes, [0x48, 0x8b, 0, 0, 0x40, 0x05]);
        assert_eq!(p.mask, [0xff, 0xff, 0, 0, 0xf0, 0x0f]);
        assert!(p.matches(&[0x48, 0x8b, 0x11, 0x22, 0x4f, 0xa5]));
        assert!(!p.matches(&[0x48, 0x8b, 0x11, 0x22, 0x5f, 0xa5]));
        let m = Pattern::parse("48 8b").unwrap().masked("ff0f").unwrap();
        assert_eq!((m.bytes, m.mask), (vec![0x48, 0x0b], vec![0xff, 0x0f]));
        assert_eq!(Pattern::ascii("Hi").unwrap().bytes, b"Hi");
        assert_eq!(Pattern::utf16le("Hé").unwrap().bytes, [0x48, 0, 0xe9, 0]);
        for (text, why) in [
            ("", "no fixed bit"),
            ("?? ?", "no fixed bit"),
            ("488B", "not a byte"),
            ("4", "not a byte"),
            ("GG", "not a byte"),
            ("48 8B ???", "not a byte"),
        ] {
            let err = Pattern::parse(text).unwrap_err();
            assert!(err.contains(why), "{text:?}: {err}");
        }
        let p = Pattern::parse("48").unwrap();
        assert!(
            p.clone()
                .masked("ff ff")
                .unwrap_err()
                .contains("2 mask bytes")
        );
        assert!(p.clone().masked("f").unwrap_err().contains("not hex bytes"));
        assert!(p.masked("00").unwrap_err().contains("no fixed bit"));
        assert!(Pattern::ascii("é").unwrap_err().contains("not ASCII"));
        assert!(Pattern::utf16le("").is_err());
    }

    #[test]
    fn scans_page_through_their_scope() {
        let mem = Fake::default()
            .with(0x1000, [0xaa, 0xaa, 0xaa, 0, 0xaa, 0xaa], Some(".text"))
            .with(0x2000, [0, 0xaa, 0xaa], Some(".rdata"))
            .with(0x9000, [0xaa, 0xaa], None);
        let p = Pattern::parse("aa aa").unwrap();
        let all = [0x1000, 0x1001, 0x1004, 0x2001, 0x9000];
        let page = scan(&mem, &p, &Which::All.into(), 0, 100, &mut go).unwrap();
        assert_eq!(page.hits, all, "overlapping matches too");
        assert_eq!((page.next, page.regions, page.bytes), (None, 3, 11));
        assert!(page.skipped.is_empty());
        let (mut hits, mut from) = (Vec::new(), 0);
        loop {
            let page = scan(&mem, &p, &Which::All.into(), from, 2, &mut go).unwrap();
            assert!(page.hits.len() <= 2);
            hits.extend(page.hits);
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        assert_eq!(hits, all, "pages resume after their last hit");
        let text = Which::Sections(vec![".text".into()]).into();
        assert_eq!(
            scan(&mem, &p, &text, 0x1002, 100, &mut go).unwrap().hits,
            [0x1004]
        );
        let regions = Which::Regions(vec![0x9000, 0x2000]).into();
        assert_eq!(
            scan(&mem, &p, &regions, 0, 100, &mut go).unwrap().hits,
            [0x2001, 0x9000]
        );
        let page = scan(&mem, &p, &Which::All.into(), 0, 0, &mut go).unwrap();
        assert_eq!(
            (page.hits, page.next, page.bytes),
            (vec![0x1000], Some(0x1001), 1)
        );
        let page = scan(&mem, &p, &Which::All.into(), 0, usize::MAX, &mut go).unwrap();
        assert_eq!(
            page.hits.len(),
            all.len(),
            "the limit is clamped, not refused"
        );
    }

    /// A tick that never stops a scan.
    fn go(_: u64, _: u64) -> ControlFlow<&'static str> {
        ControlFlow::Continue(())
    }

    fn image_private() -> Scope {
        Which::Kinds(vec![Kind::Image, Kind::Private]).into()
    }

    /// A find of `values` with `context` bytes and no fields.
    fn finding(values: &[u64], context: u64) -> Find {
        Find {
            values: values.into(),
            context,
            fields: Vec::new(),
            filter: None,
        }
    }

    /// A find page's hit addresses.
    fn addresses(page: &ScanPage<Row>) -> Vec<u64> {
        let hex = |h: &Row| number(h["address"].as_str().unwrap()).unwrap() as u64;
        page.hits.iter().map(hex).collect()
    }

    #[test]
    fn scans_state_what_they_could_not_read() {
        let mut mem = Fake::default().with(0x1000, [0xaa; 4], Some(".text")).with(
            0x2000,
            [0xaa; 4],
            Some(".text"),
        );
        mem.bad.push(0x2000..0x2001);
        let p = Pattern::parse("aa").unwrap();
        let text = Which::Sections(vec![".text".into()]).into();
        let page = scan(&mem, &p, &text, 0, 100, &mut go).unwrap();
        assert_eq!(page.hits, [0x1000, 0x1001, 0x1002, 0x1003]);
        assert_eq!(
            (page.regions, page.bytes),
            (2, 8),
            "skipped bytes count as covered"
        );
        let why = "no 4 readable bytes at 0x2000".into();
        assert_eq!(
            page.skipped,
            [Skipped {
                base: 0x2000,
                size: 4,
                why
            }]
        );
        let regions = Which::Regions(vec![0x1000, 0x5000]).into();
        let page = scan(&mem, &p, &regions, 0, 100, &mut go).unwrap();
        let why = "not a readable region now".into();
        assert_eq!(
            page.skipped,
            [Skipped {
                base: 0x5000,
                size: 0,
                why
            }]
        );
        assert_eq!(page.hits.len(), 4);
    }

    #[test]
    fn scans_match_across_chunk_seams_and_read_around_a_bad_page() {
        let base = 0x10_0000;
        let size = 2 * CHUNK + 8;
        let mut bytes = vec![0; size as usize];
        let seam = CHUNK as usize;
        // Across the first seam, either side of the second chunk's second page, and at the region's end.
        let at = [
            seam - 2,
            seam + PAGE as usize - 4,
            seam + 2 * PAGE as usize,
            size as usize - 4,
        ];
        for i in at {
            bytes[i..i + 4].copy_from_slice(&[1, 2, 3, 4]);
        }
        let all = at.map(|i| base + i as u64);
        let mut mem = Fake::default().with(base, bytes, None);
        let p = Pattern::parse("01 02 03 04").unwrap();
        let page = scan(&mem, &p, &Which::All.into(), 0, 100, &mut go).unwrap();
        assert_eq!(page.hits, all);
        assert_eq!((page.regions, page.bytes), (1, size));
        assert!(page.skipped.is_empty());
        let bad = base + CHUNK + PAGE;
        mem.bad.push(bad + 100..bad + 101);
        let page = scan(&mem, &p, &Which::All.into(), 0, 100, &mut go).unwrap();
        assert_eq!(
            page.hits, all,
            "before and after the page, and the region's rest"
        );
        assert_eq!((page.regions, page.bytes), (1, size));
        let why = format!("no {PAGE} readable bytes at {bad:#x}");
        assert_eq!(
            page.skipped,
            [Skipped {
                base: bad,
                size: PAGE,
                why
            }],
            "that page only"
        );
    }

    #[test]
    fn chunks_see_each_window_once_and_list_what_failed() {
        let base = 0x10_0000;
        let size = 2 * CHUNK + 5;
        let mut mem = Fake::default().with(base, vec![0; size as usize], None);
        let windows = |mem: &Fake, skipped: &mut Vec<Skipped>| {
            let mut starts = Vec::new();
            let flow = chunks(mem, base, size, 3, &mut Vec::new(), skipped, |at, b| {
                starts.extend((0..b.len().saturating_sub(3)).map(|i| at + i as u64));
                ControlFlow::Continue(())
            });
            assert_eq!(flow, ControlFlow::Continue(()));
            starts
        };
        let all: Vec<u64> = (base..base + size - 3).collect();
        let mut skipped = Vec::new();
        assert_eq!(windows(&mem, &mut skipped), all);
        assert!(skipped.is_empty());
        // The second chunk's first byte: the first chunk's tail reads it too.
        let bad = base + CHUNK;
        mem.bad.push(bad..bad + 1);
        let starts = windows(&mem, &mut skipped);
        assert!(
            starts.windows(2).all(|w| w[0] < w[1]),
            "each once, in order"
        );
        let lost: Vec<u64> = all
            .into_iter()
            .filter(|a| starts.binary_search(a).is_err())
            .collect();
        let touch: Vec<u64> = (bad - 3..bad + PAGE).collect();
        assert_eq!(lost, touch, "the windows that touch the page");
        let skips: Vec<(u64, u64)> = skipped.iter().map(|s| (s.base, s.size)).collect();
        assert_eq!(
            skips,
            [(bad, PAGE)],
            "listed once, by the chunk it starts in"
        );
        let mut calls = 0;
        let flow = chunks(
            &mem,
            base,
            size,
            3,
            &mut Vec::new(),
            &mut Vec::new(),
            |_, _| {
                calls += 1;
                ControlFlow::Break(())
            },
        );
        assert_eq!(
            (flow, calls),
            (ControlFlow::Break(()), 1),
            "a break ends the read"
        );
    }

    /// Synthetic bytes, from a xorshift.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// Every window in turn, as scans matched before the fast paths: what they must agree with.
    fn naive(mem: &Fake, p: &Pattern) -> Vec<u64> {
        let mut hits = Vec::new();
        for (r, bytes) in &mem.regions {
            for (i, w) in bytes.windows(p.bytes.len()).enumerate() {
                let at = r.base + i as u64;
                if at.is_multiple_of(p.align) && p.matches(w) {
                    hits.push(at);
                }
            }
        }
        hits.sort_unstable();
        hits
    }

    #[test]
    fn the_fast_paths_find_what_every_window_does() {
        let (v, w): (u64, u64) = (0x7ff6_1234_5678_9abc, 0x7ff6_1234_5678_0010);
        let c = CHUNK as usize;
        let mut mem = Fake::default();
        // Odd-sized regions over chunk seams, the value planted aligned and not, at the seams and the ends.
        for (base, len) in [
            (0x10_0000u64, c + 21),
            (0x40_0000, 2 * c + 13),
            (0x90_0000, 100),
        ] {
            let mut bytes = noise(base, len);
            for at in [
                0,
                3,
                8,
                c - 8,
                c - 4,
                c,
                2 * c - 1,
                len - 8,
                len - 13,
                len - 16,
            ] {
                if at + 8 <= len {
                    bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
                }
            }
            for at in [24, 37, len - 40] {
                bytes[at..at + 8].copy_from_slice(&w.to_le_bytes());
            }
            mem = mem.with(base, bytes, None);
        }
        let all = |p: &Pattern| {
            let (mut hits, mut from) = (Vec::new(), 0);
            loop {
                let page = scan(&mem, p, &Which::All.into(), from, MAX_HITS, &mut go).unwrap();
                hits.extend(page.hits);
                match page.next {
                    Some(next) => from = next,
                    None => return hits,
                }
            }
        };
        let hex: Vec<String> = v.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let exact = Pattern::parse(&hex.join(" ")).unwrap();
        let aligned: Vec<u64> = all(&exact)
            .into_iter()
            .filter(|a| a.is_multiple_of(8))
            .collect();
        assert_eq!(
            all(&Pattern::qwords(&[v])),
            aligned,
            "the u64 compare and the byte compare"
        );
        let mut both = [all(&Pattern::qwords(&[v])), all(&Pattern::qwords(&[w]))].concat();
        both.sort_unstable();
        assert!(both.len() > aligned.len());
        assert_eq!(all(&Pattern::qwords(&[w, v, w])), both, "two in one pass");
        assert!(
            aligned.len() >= 4 && all(&exact).len() > aligned.len(),
            "{aligned:?}"
        );
        let wild = Pattern {
            align: 8,
            ..Pattern::parse(&format!("{} ?? {}", hex[0], hex[2..].join(" "))).unwrap()
        };
        for p in [
            exact,
            Pattern::qwords(&[v]),
            Pattern::qwords(&[v, w, 0]),
            wild,
            Pattern::parse("?c 9a 7? 56").unwrap(),
            Pattern::parse("4? ?2").unwrap(),
        ] {
            assert_eq!(all(&p), naive(&mem, &p), "{p:?}");
        }
    }

    #[test]
    fn a_stopped_scan_goes_on_from_next_and_sees_no_hit_twice() {
        let base = 0x10_0000;
        let mut bytes = vec![0; 3 * CHUNK as usize + 5];
        for i in (0..bytes.len() - 1).step_by(7 * PAGE as usize + 1) {
            bytes[i..i + 2].copy_from_slice(&[0xaa, 0xbb]);
        }
        let mem = Fake::default().with(base, bytes, None).with(
            0x80_0000,
            [0xaa, 0xbb, 0, 0xaa, 0xbb],
            Some(".data"),
        );
        let p = Pattern::parse("aa bb").unwrap();
        let every = scan(&mem, &p, &Which::All.into(), 0, MAX_HITS, &mut go).unwrap();
        assert_eq!((every.next, every.stopped), (None, None));
        // A deadline at every page's second tick.
        let (mut hits, mut from, mut pages) = (Vec::new(), 0, 0);
        loop {
            let mut ticks = 0;
            let mut tick = |_, _| {
                ticks += 1;
                match ticks {
                    2 => ControlFlow::Break("deadline"),
                    _ => ControlFlow::Continue(()),
                }
            };
            let page = scan(&mem, &p, &Which::All.into(), from, MAX_HITS, &mut tick).unwrap();
            pages += 1;
            hits.extend(page.hits.iter().copied());
            let Some(next) = page.next else { break };
            assert_eq!(json!(&page)["stopped"], "deadline", "{page:?}");
            assert!(next > from);
            from = next;
        }
        assert_eq!(hits, every.hits);
        assert_eq!(pages, 3);
    }

    #[test]
    fn finds_in_the_scope_given() {
        let v: u64 = 0x1122_3344_5566_7788;
        let q = v.to_le_bytes();
        let mut mem = Fake::default()
            .with(0x1000, q, Some(".data"))
            .with(0x2000, q, Some(".rdata"))
            .with(0x3000, q, None);
        let mapped = Region {
            base: 0x5000,
            size: 8,
            kind: Kind::Mapped,
            protect: 0x02,
            section: None,
        };
        mem.regions.push((mapped, q.into()));
        let at = |scope: Scope| -> Vec<u64> {
            let (page, _) = find(&mem, &finding(&[v], 0), &scope, 0, 100, &mut go).unwrap();
            addresses(&page)
        };
        assert_eq!(at(Which::Sections(vec![".data".into()]).into()), [0x1000]);
        assert_eq!(at(Which::Kinds(vec![Kind::Mapped]).into()), [0x5000]);
        assert_eq!(at(Which::Regions(vec![0x3000]).into()), [0x3000]);
        assert_eq!(at(image_private()), [0x1000, 0x2000, 0x3000]);
    }

    /// Write-combined (0x404) and no-cache (0x204) regions are left out and counted, unless named by base or asked for.
    #[test]
    fn uncached_regions_are_left_out_unless_named_or_asked_for() {
        let v: u64 = 0x1122_3344_5566_7788;
        let q = v.to_le_bytes();
        let mut mem = Fake::default()
            .with(0x1000, q, None)
            .with(0x2000, q, None)
            .with(0x3000, [q, q].concat(), None);
        (mem.regions[1].0.protect, mem.regions[2].0.protect) = (0x404, 0x204);
        let at = |scope: Scope| {
            let (page, _) = find(&mem, &finding(&[v], 0), &scope, 0, 100, &mut go).unwrap();
            (addresses(&page), page.left_out)
        };
        let out = |regions, bytes| LeftOut { regions, bytes };
        assert_eq!(at(image_private()), (vec![0x1000], out(2, 24)));
        let named = Which::Regions(vec![0x2000, 0x3000]).into();
        assert_eq!(at(named), (vec![0x2000, 0x3000, 0x3008], out(0, 0)));
        let all = Scope {
            uncached: true,
            ..image_private()
        };
        assert_eq!(at(all), (vec![0x1000, 0x2000, 0x3000, 0x3008], out(0, 0)));
        let p = Pattern::qwords(&[v]);
        let page = scan(&mem, &p, &Which::All.into(), 0, 100, &mut go).unwrap();
        assert_eq!(
            (&page.hits, page.regions, page.bytes),
            (&vec![0x1000], 1, 8)
        );
        assert_eq!(json!(page)["left_out"], json!({"regions": 2, "bytes": 24}));
    }

    /// The scan before the fast paths (f9e9353): a fresh zeroed buffer per chunk, every window, the alignment check,
    /// then the masked compare. Kept to measure the new one against.
    fn old_scan(mem: &Fake, p: &Pattern) -> Vec<u64> {
        let n = p.bytes.len() as u64;
        let mut hits = Vec::new();
        for (r, _) in &mem.regions {
            let (mut at, end) = (r.base, r.base + r.size);
            while end - at >= n {
                let mut buf = vec![0; (end - at).min(CHUNK + n - 1) as usize];
                mem.read(at, &mut buf).unwrap();
                for (i, window) in buf.windows(n as usize).enumerate() {
                    let hit = at + i as u64;
                    if hit.is_multiple_of(p.align) && p.matches(window) {
                        hits.push(hit);
                    }
                }
                at += CHUNK;
            }
        }
        hits
    }

    /// Old and new scan over 256 MiB of synthetic memory, once each, single-threaded. A copy from a Vec stands in for
    /// ReadProcessMemory, so this measures the matching, not a game's copy rate.
    #[test]
    #[ignore = "a measurement: cargo test --release -p quen-core --lib -- --ignored --nocapture scan_speed"]
    fn scan_speed() {
        let size = 64 << 20;
        let v: u64 = 0x7ff6_1234_5678_9abc;
        let mut mem = Fake::default();
        for i in 1..=4u64 {
            let mut bytes = noise(i, size);
            for at in [8, size / 2, size - 64] {
                bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
                bytes[at + 25..at + 31].copy_from_slice(b"quen-d");
            }
            mem = mem.with(i << 28, bytes, None);
        }
        let total = (4 * size) as f64 / 1e6;
        for (name, p) in [
            ("qword (mem_find)", Pattern::qwords(&[v])),
            ("ascii, 6 bytes", Pattern::ascii("quen-d").unwrap()),
        ] {
            let t = std::time::Instant::now();
            let old = old_scan(&mem, &p);
            let old_s = t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            let new = scan(&mem, &p, &Which::All.into(), 0, MAX_HITS, &mut go).unwrap();
            let new_s = t.elapsed().as_secs_f64();
            assert_eq!(new.hits, old);
            println!(
                "{name}: {total:.0} MB, old {:.0} MB/s, new {:.0} MB/s",
                total / old_s,
                total / new_s
            );
        }
    }

    #[test]
    fn a_census_groups_regions_by_kind_protection_and_section() {
        let r = |base, size, kind, protect, section: Option<&str>| Region {
            base,
            size,
            kind,
            protect,
            section: section.map(Into::into),
        };
        let groups = census(&[
            r(0, 0x1000, Kind::Image, 0x20, Some(".text")),
            r(0x1000, 0x3000, Kind::Private, 0x04, None),
            r(0x5000, 0x1000, Kind::Private, 0x04, None),
            r(0x9000, 0x1000, Kind::Private, 0x404, None),
        ]);
        assert_eq!(
            json!(groups),
            json!([
                {"kind": "private", "protect": "0x4", "section": null, "count": 2, "bytes": 0x4000},
                {"kind": "image", "protect": "0x20", "section": ".text", "count": 1, "bytes": 0x1000},
                {"kind": "private", "protect": "0x404", "section": null, "count": 1, "bytes": 0x1000},
            ])
        );
    }

    #[test]
    fn chains_report_each_hop_and_where_they_break() {
        let base = 0x5000;
        let mut bytes = vec![0; 0x40];
        let mut put = |at: usize, v: u64| bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put(0x08, base + 0x20);
        put(0x18, 0x7700);
        let mem = Fake::default().with(base, bytes, None);
        let chain = follow(&mem, base, &[0x8, -0x8]);
        let hops = [
            Hop {
                at: base + 0x8,
                value: base + 0x20,
            },
            Hop {
                at: base + 0x18,
                value: 0x7700,
            },
        ];
        assert_eq!(
            (&chain.hops[..], chain.end, &chain.broken),
            (&hops[..], Some(0x7700), &None)
        );
        assert_eq!(
            serde_json::to_value(&chain).unwrap(),
            json!({"hops": [{"at": "0x5008", "value": "0x5020"}, {"at": "0x5018", "value": "0x7700"}],
                   "end": "0x7700", "broken": null})
        );
        let broken = |offsets: &[i64], from| {
            let chain = follow(&mem, from, offsets);
            assert_eq!(chain.end, None);
            let broken = chain.broken.unwrap();
            assert_eq!(
                chain.hops.len(),
                broken.hop,
                "every hop before the break is listed"
            );
            (broken.hop, broken.why)
        };
        assert_eq!(
            broken(&[0x30, 0x10], base),
            (1, "hop 0 read a null pointer".into())
        );
        let (hop, why) = broken(&[0x18, 0], base);
        assert_eq!(
            (hop, why.as_str()),
            (
                1,
                "can't read 8 bytes at 0x7700: no 8 readable bytes at 0x7700"
            )
        );
        assert!(broken(&[-0x20], 0x10).1.contains("overflows"));
        assert_eq!(broken(&[8], 0), (0, "the base is null".into()));
        let chain = follow(&mem, base, &[]);
        assert_eq!((chain.hops.len(), chain.end), (0, Some(base)));
    }

    #[test]
    fn finds_aligned_qwords_in_the_image_and_private_regions_with_their_context() {
        let v: u64 = 0x7ff6_0000_1234;
        let mut image = vec![0x11; 0x20];
        image[0x08..0x10].copy_from_slice(&v.to_le_bytes());
        image[0x15..0x1d].copy_from_slice(&v.to_le_bytes());
        let mut private = vec![0x22; 0x10];
        private[..8].copy_from_slice(&v.to_le_bytes());
        let mut mem = Fake::default()
            .with(0x1000, image, Some(".data"))
            .with(0x3000, private, None);
        let mapped = Region {
            base: 0x5000,
            size: 8,
            kind: Kind::Mapped,
            protect: 0x04,
            section: None,
        };
        mem.regions.push((mapped, v.to_le_bytes().into()));
        let page = |from, limit, context| {
            find(
                &mem,
                &finding(&[v], context),
                &image_private(),
                from,
                limit,
                &mut go,
            )
            .unwrap()
        };
        let (four, sha) = page(0, 100, 4);
        assert_eq!(
            json!(four.hits),
            json!([
                {"address": "0x1008", "value": "0x7ff600001234", "before": "11 11 11 11", "after": "11 11 11 11"},
                {"address": "0x3000", "value": "0x7ff600001234", "before": "", "after": "22 22 22 22"},
            ]),
            "unaligned and mapped copies aren't hits; context stops at the region's edge"
        );
        assert_eq!((four.next, four.regions), (None, 2));
        let read = [
            vec![0x11; 4],
            v.to_le_bytes().into(),
            vec![0x11; 4],
            v.to_le_bytes().into(),
            vec![0x22; 4],
        ];
        assert_eq!(
            sha,
            sha256_hex(&read.concat()),
            "the bytes read, in address order"
        );
        let (wide, _) = page(0, 100, 10_000);
        let sides: Vec<(usize, usize)> = wide
            .hits
            .iter()
            .map(|h| {
                (
                    h["before"].as_str().unwrap().len(),
                    h["after"].as_str().unwrap().len(),
                )
            })
            .collect();
        assert_eq!(sides, [(23, 47), (0, 23)], "the region's bytes, at most");
        let (first, _) = page(0, 1, 0);
        assert_eq!(json!(first)["next"], "0x1009");
        let (second, _) = page(0x1009, 1, 0);
        assert_eq!(
            (addresses(&second), second.next),
            (vec![0x3000], Some(0x3001))
        );
        assert!(page(0x3001, 1, 0).0.hits.is_empty());
    }

    /// values: several in one pass, each hit with the one it matched; fields read at each hit, before and after it,
    /// and a filter that drops hits.
    #[test]
    fn finds_several_values_and_reads_fields_at_each_hit() {
        let (a, b): (u64, u64) = (0x7ff6_0000_1000, 0x7ff6_0000_2000);
        let mut bytes = vec![0; 0x60];
        let mut put = |at: usize, v: &[u8]| bytes[at..at + v.len()].copy_from_slice(v);
        // Two objects: a header word at -8, the vtable at 0, a float at +0x10.
        put(0x08, &7u32.to_le_bytes());
        put(0x10, &a.to_le_bytes());
        put(0x20, &1.5f32.to_le_bytes());
        put(0x38, &9u32.to_le_bytes());
        put(0x40, &b.to_le_bytes());
        put(0x50, &(-2.0f32).to_le_bytes());
        let mem = Fake::default().with(0x9000, bytes, None);
        let fields = vec![field("head", -8, Type::U32), field("x", 0x10, Type::F32)];
        let both = Find {
            fields,
            ..finding(&[b, a, 0x1234], 0)
        };
        let run = |f: &Find| find(&mem, f, &image_private(), 0, 100, &mut go);
        let (page, sha) = run(&both).unwrap();
        assert_eq!(
            json!(page.hits),
            json!([
                {"address": "0x9010", "value": "0x7ff600001000", "before": "", "after": "", "head": 7, "x": 1.5},
                {"address": "0x9040", "value": "0x7ff600002000", "before": "", "after": "", "head": 9, "x": -2.0},
            ])
        );
        let spans = [&mem.regions[0].1[0x08..0x24], &mem.regions[0].1[0x38..0x54]];
        let qwords = [a.to_le_bytes(), b.to_le_bytes()];
        let read = [spans[0], &qwords[0], spans[1], &qwords[1]];
        // Address order: each span starts before its hit's qword.
        assert_eq!(sha, sha256_hex(&read.concat()));
        let kept = |filter| {
            let f = Find {
                filter,
                ..both.clone()
            };
            addresses(&run(&f).unwrap().0)
        };
        assert_eq!(kept(filter("x", true, "1.5")), [0x9010]);
        assert_eq!(kept(filter("head", false, "7")), [0x9040]);
        for (f, why) in [
            (finding(&[], 0), "0 values"),
            (finding(&[1; 65], 0), "65 values"),
            (
                Find {
                    fields: vec![field("value", 0, Type::U8)],
                    ..finding(&[a], 0)
                },
                "field name",
            ),
            (
                Find {
                    fields: vec![field("x", -0x10001, Type::U8)],
                    ..finding(&[a], 0)
                },
                "starts over 0x10000 bytes before",
            ),
            (
                Find {
                    fields: vec![field("x", 0xffff, Type::U16)],
                    ..finding(&[a], 0)
                },
                "ends past 0x10000",
            ),
            (
                Find {
                    filter: filter("x", true, "1"),
                    ..finding(&[a], 0)
                },
                "filter: no field x",
            ),
        ] {
            let err = run(&f).unwrap_err();
            assert!(err.contains(why), "{why}: {err}");
        }
    }

    /// Three 16-byte elements: an id, a float and a pointer to a 32-byte struct (null for the second).
    fn structs() -> Fake {
        let mut elements = Vec::new();
        for (id, to) in [(7u32, 0x2000u64), (8, 0), (9, 0x2020)] {
            elements.extend(id.to_le_bytes());
            elements.extend(1.5f32.to_le_bytes());
            elements.extend(to.to_le_bytes());
        }
        let mut structs = vec![0; 0x40];
        structs[..4].copy_from_slice(b"abcd");
        structs[0x10..0x18].copy_from_slice(&(-5i64).to_le_bytes());
        structs[0x20..0x24].copy_from_slice(b"wxyz");
        Fake::default()
            .with(0x1000, elements, None)
            .with(0x2000, structs, None)
    }

    fn field(name: &str, offset: i64, ty: Type) -> Field {
        let name = name.into();
        Field { name, offset, ty }
    }

    fn array(base: u64, stride: u64, count: u64) -> Elements {
        Elements::Array {
            base,
            stride,
            count,
        }
    }

    fn elements(fields: Vec<Field>) -> Walk {
        Walk {
            elements: array(0x1000, 0x10, 3),
            deref: Vec::new(),
            fields,
            filter: None,
        }
    }

    fn filter(field: &str, eq: bool, value: &str) -> Option<Filter> {
        let (field, value) = (field.into(), value.into());
        Some(Filter { field, eq, value })
    }

    #[test]
    fn walks_read_each_element_or_say_why_not() {
        let mem = structs();
        let fields = vec![
            field("id", 0, Type::U32),
            field("f", 4, Type::F32),
            field("p", 8, Type::Ptr),
        ];
        let (rows, sha) = walk(&mem, &elements(fields.clone())).unwrap();
        assert_eq!(
            json!(rows),
            json!([
                {"index": 0, "address": "0x1000", "id": 7, "f": 1.5, "p": "0x2000"},
                {"index": 1, "address": "0x1010", "id": 8, "f": 1.5, "p": "0x0"},
                {"index": 2, "address": "0x1020", "id": 9, "f": 1.5, "p": "0x2020"},
            ])
        );
        assert_eq!(sha, sha256_hex(&mem.regions[0].1), "the elements' bytes");
        let derefs = Walk {
            deref: vec![8],
            ..elements(vec![
                field("name", 0, Type::Bytes(4)),
                field("n", 0x10, Type::I64),
            ])
        };
        let (rows, _) = walk(&mem, &derefs).unwrap();
        assert_eq!(
            json!(rows),
            json!([
                {"index": 0, "address": "0x1000", "target": "0x2000", "name": "61 62 63 64", "n": "-5"},
                {"index": 1, "address": "0x1010", "target": "0x0", "why": "can't read 24 bytes at 0x0: no 24 readable bytes at 0x0"},
                {"index": 2, "address": "0x1020", "target": "0x2020", "name": "77 78 79 7a", "n": "0"},
            ])
        );
        let two = Walk {
            deref: vec![8, 0],
            ..derefs.clone()
        };
        assert_eq!(
            walk(&mem, &two).unwrap().0[1]["why"],
            "deref: hop 0 read a null pointer"
        );
        let index = |w: &Walk| -> Vec<u64> {
            let (rows, _) = walk(&mem, w).unwrap();
            rows.iter().map(|r| r["index"].as_u64().unwrap()).collect()
        };
        for (filter, kept) in [
            (filter("p", false, "0"), vec![0, 2]),
            (filter("id", true, "0x9"), vec![2]),
            (filter("f", true, "1.5"), vec![0, 1, 2]),
            (filter("id", true, "10"), vec![]),
        ] {
            let w = Walk {
                filter: filter.clone(),
                ..elements(fields.clone())
            };
            assert_eq!(index(&w), kept, "{filter:?}");
        }
        let named = Walk {
            filter: filter("name", true, "61 62 63 64"),
            ..derefs
        };
        assert_eq!(index(&named), [0, 1], "a why row stays");
    }

    /// addresses: the places listed, in their order, the row's index its position; fields before the struct too.
    #[test]
    fn walks_read_the_addresses_listed_in_their_order() {
        let mem = structs();
        let listed = Walk {
            elements: Elements::List(vec![0x2020, 0x1004, 0x2000, 0x9999]),
            ..elements(vec![
                field("before", -4, Type::U32),
                field("at", 0, Type::U8),
            ])
        };
        let (rows, sha) = walk(&mem, &listed).unwrap();
        assert_eq!(
            json!(rows),
            json!([
                {"index": 0, "address": "0x2020", "before": 0, "at": 0x77},
                {"index": 1, "address": "0x1004", "before": 7, "at": 0},
                {"index": 2, "address": "0x2000", "why": "can't read 5 bytes at 0x1ffc: no 5 readable bytes at 0x1ffc"},
                {"index": 3, "address": "0x9999", "why": "can't read 5 bytes at 0x9995: no 5 readable bytes at 0x9995"},
            ])
        );
        let (elements, structs) = (&mem.regions[0].1, &mem.regions[1].1);
        assert_eq!(
            sha,
            sha256_hex(&[&elements[..5], &structs[0x1c..0x21]].concat()),
            "the rows' bytes, in address order"
        );
        let index: Vec<u64> = rows.iter().map(|r| r["index"].as_u64().unwrap()).collect();
        assert_eq!(index, [0, 1, 2, 3]);
    }

    #[test]
    fn walks_refuse_before_reading() {
        let mem = structs();
        let one = || elements(vec![field("id", 0, Type::U32)]);
        let at = |elements| Walk { elements, ..one() };
        for (w, why) in [
            (at(array(0x1000, 0x10, 0)), "count 0"),
            (at(array(0x1000, 0x10, MAX_COUNT + 1)), "count 100001"),
            (at(array(0x1000, 0, 3)), "stride 0"),
            (at(array(0x1000, MAX_STRIDE + 1, 3)), "stride 65537"),
            (at(Elements::List(Vec::new())), "0 addresses"),
            (elements(vec![]), "no fields"),
            (elements(vec![field("why", 0, Type::U8)]), "field name"),
            (elements(vec![field("rva", 0, Type::U8)]), "field name"),
            (
                elements(vec![field("a", 0, Type::U8), field("a", 1, Type::U8)]),
                "named twice",
            ),
            (elements(vec![field("b", 0, Type::Bytes(0))]), "0 bytes"),
            (elements(vec![field("x", 0xc, Type::F64)]), "ends past 0x10"),
            (
                Walk {
                    deref: vec![8],
                    ..elements(vec![field("x", MAX_STRIDE as i64, Type::U8)])
                },
                "ends past 0x10000",
            ),
            (
                Walk {
                    elements: Elements::List(vec![0x1000]),
                    ..elements(vec![field("x", -0x10001, Type::U8)])
                },
                "starts over 0x10000 bytes before",
            ),
            (
                Walk {
                    elements: array(0x1000, MAX_STRIDE, MAX_COUNT),
                    ..elements(vec![field("b", 0, Type::Bytes(MAX_BYTES))])
                },
                "a walk reads at most",
            ),
            (
                Walk {
                    filter: filter("nope", true, "1"),
                    ..one()
                },
                "no field nope",
            ),
            (
                Walk {
                    filter: filter("id", true, "-1"),
                    ..one()
                },
                r#""-1" is not a u32"#,
            ),
        ] {
            let err = walk(&mem, &w).unwrap_err();
            assert!(err.contains(why), "{w:?}: {err}");
        }
        assert!(
            walk(
                &mem,
                &Walk {
                    deref: vec![8],
                    ..elements(vec![field("x", 0x20, Type::U8)])
                }
            )
            .is_ok(),
            "a deref's struct reaches past the stride"
        );
        for (ty, text, ok) in [
            (Type::U8, "255", true),
            (Type::U8, "256", false),
            (Type::I32, "-2147483648", true),
            (Type::I32, "2147483648", false),
            (Type::U64, "0xffffffffffffffff", true),
            (Type::Ptr, "-0x1", false),
            (Type::F32, "x", false),
            (Type::Bytes(2), "61 62", true),
            (Type::Bytes(2), "61", false),
        ] {
            assert_eq!(ty.encode(text).is_ok(), ok, "{ty:?} {text}");
        }
        assert_eq!(Type::I32.encode("-1"), Ok(vec![0xff; 4]));
    }

    /// Addresses in the image get their RVA beside them, in another module its name and offset; the rest nothing.
    #[test]
    fn places_name_the_module_an_address_lies_in() {
        let module = |name: &str, base, size| Module {
            name: name.into(),
            base,
            size,
        };
        let modules = [
            module("game.exe", IMAGE, 0x10_0000),
            module("Mod.asi", IMAGE + 0x20_0000, 0x2000),
        ];
        let mut v = json!({
            "address": format!("{:#x}", IMAGE + 0x1234),
            "hits": [{"address": format!("{:#x}", IMAGE + 0x20_1140), "value": "0x10"}],
            "end": format!("{:#x}", IMAGE + 0x10_0000),
            "vt": format!("{:#x}", IMAGE + 0x8),
            "n": 5,
            "before": "11 22",
            "protect": "0x4",
        });
        place(&mut v, IMAGE, &modules);
        assert_eq!(
            v,
            json!({
                "address": "0x7ff600001234", "rva": "0x1234",
                "hits": [{"address": "0x7ff600201140", "module": "Mod.asi+0x1140", "value": "0x10"}],
                "end": "0x7ff600100000",
                "vt": "0x7ff600000008", "vt_rva": "0x8",
                "n": 5,
                "before": "11 22",
                "protect": "0x4",
            }),
            "the image's end is past it"
        );
    }

    const HEADER: &str = "name\trva\told\tnew\tbuild\tboundaries-checked\n";
    /// A synthetic image base, as ASLR might place a 64-bit exe.
    const IMAGE: u64 = 0x7ff6_0000_0000;

    fn patch(rva: u32, old: &str, new: &str) -> Patch {
        Patch {
            name: "p".into(),
            rva,
            old: parse_hex(old).unwrap(),
            new: parse_hex(new).unwrap(),
            build: "build-1".into(),
            boundaries_checked: Some("yes".into()),
        }
    }

    #[test]
    fn patches_tsv_rows_become_definitions() {
        let text = format!(
            "# synthetic definitions, not from any real binary\n\n{HEADER}\
             mask_a\t0x1004\taa bb cc dd\t11 22 33 44\tbuild-1\t2026-01-01 by hand\r\n\
             mask_a\t0X2004\taabbccdd\t11223344\tbuild-2\tNo\n\
             mask_b\t0x10\t01\t02\tbuild-1\t\n"
        );
        let patches = from_patches_tsv(&text).unwrap();
        let mask_a = Patch {
            name: "mask_a".into(),
            boundaries_checked: Some("2026-01-01 by hand".into()),
            ..patch(0x1004, "aa bb cc dd", "11 22 33 44")
        };
        assert_eq!(patches[0], mask_a);
        let build_2 = Patch {
            rva: 0x2004,
            build: "build-2".into(),
            boundaries_checked: None,
            ..mask_a
        };
        assert_eq!(patches[1], build_2);
        assert_eq!(patches[2].boundaries_checked, None);
        assert_eq!(patches.len(), 3);
        for (row, why) in [
            ("m\t0x10\tzz\t01\tb\tyes", r#"old: "zz" is not hex bytes"#),
            ("m\t0x10\t01\t1\tb\tyes", r#"new: "1" is not hex bytes"#),
            ("m\t10\t01\t02\tb\tyes", "not 0x-prefixed hex"),
            ("m\t0x140001000\t01\t02\tb\tyes", "over 32 bits"),
            ("a b\t0x10\t01\t02\tb\tyes", "has whitespace"),
            ("\t0x10\t01\t02\tb\tyes", "is blank"),
            ("m\t0x10\t01\t02\t\tyes", "the build is blank"),
            ("m\t0x10\t01\t02\tb", "5 columns"),
            (
                "p\t0x20\t03\t04\tbuild-1\tyes",
                "p is defined twice for build build-1",
            ),
        ] {
            let text = format!("{HEADER}p\t0x10\t01\t02\tbuild-1\tyes\n{row}\n");
            match from_patches_tsv(&text) {
                Err(e @ TsvError { line: 3, .. }) => {
                    assert!(e.msg.contains(why), "{row:?}: {e}");
                    assert!(e.to_string().starts_with("patches.tsv line 3: "));
                }
                other => panic!("{row:?}: {other:?}"),
            }
        }
        assert!(matches!(
            from_patches_tsv(""),
            Err(TsvError { line: 1, .. })
        ));
        let seeds_header = "rva\tname\tkind\tconfidence\tsource\tdate\n";
        assert!(
            from_patches_tsv(seeds_header)
                .unwrap_err()
                .msg
                .contains("the header must be")
        );
        assert_eq!(from_patches_tsv(HEADER).unwrap(), []);
    }

    #[test]
    fn plans_write_only_over_the_bytes_they_expect() {
        let text = format!(
            "{HEADER}mask_a\t0x1004\taa bb cc dd\t11 22 33 44\tbuild-1\tyes\n\
             mask_a\t0x2004\taa bb cc dd\t11 22 33 44\tbuild-2\tyes\n"
        );
        let defs = from_patches_tsv(&text).unwrap();
        let image = |at, code: [u8; 4]| {
            let mut bytes = vec![0; 8];
            bytes[4..].copy_from_slice(&code);
            Fake::default().with(IMAGE + at, bytes, Some(".text"))
        };
        let (original, patched) = (
            image(0x1000, [0xaa, 0xbb, 0xcc, 0xdd]),
            image(0x1000, [0x11, 0x22, 0x33, 0x44]),
        );
        let run = |op, build, mem: &Fake| plan(&defs, "mask_a", op, build, IMAGE, mem);
        let write = Write {
            address: IMAGE + 0x1004,
            old: vec![0xaa, 0xbb, 0xcc, 0xdd],
            new: vec![0x11, 0x22, 0x33, 0x44],
        };
        assert_eq!(run(Op::Patch, "build-1", &original), Ok(write.clone()));
        assert_eq!(
            serde_json::to_value(&write).unwrap(),
            json!({"address": IMAGE + 0x1004, "old": "aa bb cc dd", "new": "11 22 33 44"})
        );
        let unpatch = Write {
            old: write.new.clone(),
            new: write.old.clone(),
            ..write
        };
        assert_eq!(run(Op::Unpatch, "build-1", &patched), Ok(unpatch));
        assert_eq!(
            run(Op::Patch, "build-1", &patched),
            Err(Refusal::Already(Op::Patch))
        );
        assert_eq!(
            run(Op::Unpatch, "build-1", &original),
            Err(Refusal::Already(Op::Unpatch))
        );
        let other = image(0x1000, [0; 4]);
        let mismatch = run(Op::Unpatch, "build-1", &other).unwrap_err();
        assert_eq!(
            mismatch,
            Refusal::Mismatch {
                address: IMAGE + 0x1004,
                live: vec![0; 4],
                expected: vec![0x11, 0x22, 0x33, 0x44]
            }
        );
        assert_eq!(
            mismatch.to_string(),
            "0x7ff600001004 holds 00 00 00 00, expected 11 22 33 44: another build or another writer"
        );
        // Each build has its own row: build-2's RVA is another address.
        let build_2 = image(0x2000, [0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(
            run(Op::Patch, "build-2", &build_2).unwrap().address,
            IMAGE + 0x2004
        );
        let wrong = run(Op::Patch, "build-3", &original).unwrap_err();
        assert_eq!(
            wrong.to_string(),
            "defined for build build-1, build-2, not the running build-3"
        );
        assert_eq!(
            plan(&defs, "mask_z", Op::Patch, "build-1", IMAGE, &original),
            Err(Refusal::Unknown {
                name: "mask_z".into(),
                table: "patches.tsv"
            })
        );
    }

    #[test]
    fn plans_refuse_definitions_that_break_the_rules() {
        let mem = Fake::default().with(IMAGE + 0x1000, [0xaa; 16], Some(".text"));
        let refusal =
            |def: Patch| plan(&[def], "p", Op::Patch, "build-1", IMAGE, &mem).unwrap_err();
        let unchecked = Patch {
            boundaries_checked: None,
            ..patch(0x1000, "aa", "bb")
        };
        assert_eq!(refusal(unchecked), Refusal::Unchecked);
        assert_eq!(
            refusal(patch(0x1000, "aa aa", "bb")),
            Refusal::Length { old: 2, new: 1 }
        );
        assert_eq!(
            refusal(patch(0x1000, "", "")),
            Refusal::Length { old: 0, new: 0 }
        );
        let nine = refusal(patch(0x1000, &"aa".repeat(9), &"bb".repeat(9)));
        assert_eq!(nine, Refusal::Length { old: 9, new: 9 });
        assert_eq!(nine.to_string(), "9 bytes: a patch is 1 to 8");
        assert_eq!(
            Refusal::Length { old: 2, new: 1 }.to_string(),
            "old is 2 bytes and new 1: they must be the same length"
        );
        assert_eq!(refusal(patch(0x1000, "aa", "aa")), Refusal::NoChange);
        let straddles = refusal(patch(0x1006, "aa aa aa", "bb bb bb"));
        assert_eq!(
            straddles,
            Refusal::Straddles {
                address: IMAGE + 0x1006,
                len: 3
            }
        );
        let eight = patch(0x1008, &"aa".repeat(8), &"bb".repeat(8));
        assert!(
            plan(&[eight], "p", Op::Patch, "build-1", IMAGE, &mem).is_ok(),
            "a whole aligned qword fits"
        );
        let why = "no 1 readable bytes at 0x7ff600002000".into();
        assert_eq!(
            refusal(patch(0x2000, "aa", "bb")),
            Refusal::Unreadable {
                address: IMAGE + 0x2000,
                why
            }
        );
        let near_top = plan(
            &[patch(0x10, "aa", "bb")],
            "p",
            Op::Patch,
            "build-1",
            u64::MAX - 8,
            &mem,
        );
        assert!(matches!(near_top, Err(Refusal::Unreadable { .. })));
    }

    const INSTANCES: &str =
        "name\tclass\tvtable\tbuild\toffset\ttype\told\tnew\tboundaries-checked\n";
    /// The synthetic class's vtable RVA, and where the test's instances live: private regions of their own.
    const VT: u32 = 0x1000;
    const HEAP: u64 = 0x2000_0000;
    const FREED: u64 = 0x3000_0000;
    /// An instance's size, and its field's offset: an f32 that starts 0.2 (`old`) and is patched to 0.35 (`new`).
    const SIZE: u64 = 0x60;
    const FIELD: u64 = 0x58;

    fn steer() -> InstancePatch {
        let text =
            format!("{INSTANCES}steer\tCFake\t{VT:#x}\tbuild-1\t{FIELD:#x}\tf32\t0.2\t0.35\tyes\n");
        from_instances_tsv(&text).unwrap().remove(0)
    }

    /// `n` instances back to back from `base`, each holding the vtable pointer and its field `value`.
    fn heap(n: u64, value: f32) -> Vec<u8> {
        let mut b = vec![0; (n * SIZE) as usize];
        for i in 0..n as usize {
            let at = i * SIZE as usize;
            b[at..at + 8].copy_from_slice(&(IMAGE + u64::from(VT)).to_le_bytes());
            let field = at + FIELD as usize;
            b[field..field + 4].copy_from_slice(&value.to_le_bytes());
        }
        b
    }

    fn plan_steer(
        defs: &[InstancePatch],
        mem: &Fake,
    ) -> Result<(InstancePatch, Instances), Refusal> {
        plan_instances(defs, "steer", "build-1", IMAGE, None, mem, &mut go)
    }

    /// The writes `write_instances` made through it, kept to be made in the fake afterwards.
    fn made(
        mem: &Fake,
        vt: u64,
        writes: &[Write],
        log: &mut Vec<Write>,
    ) -> Vec<Result<(), String>> {
        write_instances(mem, vt, FIELD, writes, &mut |w| {
            log.push(w.clone());
            Ok(())
        })
    }

    #[test]
    fn instance_patches_tsv_rows_become_definitions() {
        let text = format!(
            "# synthetic definitions, not from any real binary\n{INSTANCES}\
             steer\tCFake\t0x1000\tbuild-1\t0x58\tf32\t0.2\t0.35\t2026-01-01 by hand\r\n\
             flag\tCFake\t0x1000\tbuild-1\t12\tbytes\t01 00\t0000\tyes\n\
             count\tCFake\t0X2000\tbuild-2\t0x5c\ti32\t-1\t0x10\tno\n"
        );
        let defs = from_instances_tsv(&text).unwrap();
        let steer = InstancePatch {
            name: "steer".into(),
            class: "CFake".into(),
            vtable: 0x1000,
            build: "build-1".into(),
            offset: 0x58,
            ty: Type::F32,
            old: 0.2f32.to_le_bytes().into(),
            new: 0.35f32.to_le_bytes().into(),
            boundaries_checked: Some("2026-01-01 by hand".into()),
        };
        assert_eq!(defs[0], steer);
        assert_eq!(
            (
                defs[1].ty,
                defs[1].offset,
                &defs[1].old[..],
                &defs[1].new[..]
            ),
            (Type::Bytes(2), 12, &[1, 0][..], &[0, 0][..])
        );
        assert_eq!(
            (
                &defs[2].old[..],
                &defs[2].new[..],
                &defs[2].boundaries_checked
            ),
            (&(-1i32).to_le_bytes()[..], &16i32.to_le_bytes()[..], &None)
        );
        assert_eq!(
            defs[0].json(),
            json!({"name": "steer", "class": "CFake", "vtable_rva": "0x1000", "build": "build-1", "offset": "0x58",
                   "type": "f32", "old": 0.2f32, "new": 0.35f32, "boundaries_checked": "2026-01-01 by hand"})
        );
        for (row, why) in [
            (
                "m\tC\t0x10\tb\t0x58\tf32\tzz\t1\tyes",
                r#"old: "zz" is not a f32"#,
            ),
            (
                "m\tC\t0x10\tb\t0x58\tu8\t1\t256\tyes",
                r#"new: "256" is not a u8"#,
            ),
            ("m\tC\t0x10\tb\t0x58\tbytes\t01\t02 03\tyes", "new: "),
            ("m\tC\t10\tb\t0x58\tf32\t1\t2\tyes", "not 0x-prefixed hex"),
            ("m\tC\t0x10\tb\t-8\tf32\t1\t2\tyes", r#"offset "-8""#),
            ("m\tC\t0x10\tb\t0x58\tptr\t1\t2\tyes", "type ptr"),
            ("m\tC\t0x10\tb\t0x58\tf16\t1\t2\tyes", r#"type "f16""#),
            ("m\t\t0x10\tb\t0x58\tf32\t1\t2\tyes", r#"class """#),
            ("m\tC D\t0x10\tb\t0x58\tf32\t1\t2\tyes", "has whitespace"),
            ("m\tC\t0x10\t\t0x58\tf32\t1\t2\tyes", "the build is blank"),
            (
                "p\tC\t0x10\tb-1\t0x58\tf32\t1\t2\tyes",
                "p is defined twice for build b-1",
            ),
            ("m\tC\t0x10\tb\t0x58\tf32\t1\t2", "8 columns"),
        ] {
            let text = format!("{INSTANCES}p\tC\t0x10\tb-1\t0x58\tf32\t1\t2\tyes\n{row}\n");
            match from_instances_tsv(&text) {
                Err(e @ TsvError { line: 3, .. }) => {
                    assert!(e.msg.contains(why), "{row:?}: {e}");
                    assert!(e.to_string().starts_with("instance_patches.tsv line 3: "));
                }
                other => panic!("{row:?}: {other:?}"),
            }
        }
        assert!(from_instances_tsv(HEADER).is_err(), "patches.tsv's header");
    }

    /// Instances found, then changed by the game before their write: one reused for another object, one freed, one
    /// rewritten; one held another value, one the new one, one lay in read-only memory. Only the instances still
    /// holding the vtable and the old value are written; a second apply writes only an instance new since; an undo
    /// restores only the fields still holding the new value in an instance of the class.
    #[test]
    fn instance_patches_write_only_live_instances_holding_the_old_value() {
        let vt = IMAGE + u64::from(VT);
        let at = |i: u64| HEAP + i * SIZE;
        let mut mem = Fake::default()
            .with(IMAGE, heap(1, 0.2), Some(".rdata"))
            .with(HEAP, heap(8, 0.2), None)
            .with(FREED, heap(1, 0.2), None);
        mem.regions[0].0.protect = 0x02;
        mem.poke(at(4) + FIELD, &0.5f32.to_le_bytes());
        mem.poke(at(5) + FIELD, &0.35f32.to_le_bytes());
        // Instance 7 isn't one yet: it becomes one after the patch.
        mem.poke(at(7), &[0; 8]);
        let defs = [steer()];
        let (row, found) = plan_steer(&defs, &mem).unwrap();
        assert_eq!(row, defs[0]);
        assert_eq!(found.vtable, vt);
        let fields: Vec<u64> = found.writes.iter().map(|w| w.address - FIELD).collect();
        assert_eq!(fields, [at(0), at(1), at(2), at(3), at(6), FREED]);
        assert!(
            found
                .writes
                .iter()
                .all(|w| w.old == row.old && w.new == row.new)
        );
        assert_eq!(found.already, [at(5)]);
        let left: Vec<u64> = found.left.iter().map(|(a, _)| *a).collect();
        assert_eq!(left, [at(4), IMAGE], "{:?}", found.left);
        assert_eq!(found.left[0].1, "the field holds 0.5: neither old nor new");
        assert!(
            found.left[1].1.contains("protect 0x2: not writable"),
            "{:?}",
            found.left
        );
        assert_eq!(found.bytes, SIZE * 10);

        // Between the search and the writes: 1 freed and its memory reused, the FREED region freed, 3 rewritten by the
        // game.
        mem.poke(at(1), &0xdead_0000u64.to_le_bytes());
        mem.bad.push(FREED..FREED + SIZE);
        mem.poke(at(3) + FIELD, &0.3f32.to_le_bytes());
        let mut log = Vec::new();
        let results = made(&mem, vt, &found.writes, &mut log);
        let ok: Vec<bool> = results.iter().map(Result::is_ok).collect();
        assert_eq!(ok, [true, false, true, false, true, false], "{results:?}");
        let why = |i: usize| results[i].clone().unwrap_err();
        assert!(
            why(1).contains("0xdead0000 now, not the vtable"),
            "{results:?}"
        );
        assert!(
            why(3).contains("the field holds 9a 99 99 3e now"),
            "{results:?}"
        );
        assert!(
            why(5).contains("can't read the instance now"),
            "{results:?}"
        );
        let written: Vec<u64> = log.iter().map(|w| w.address - FIELD).collect();
        assert_eq!(written, [at(0), at(2), at(6)]);
        for w in &log {
            mem.poke(w.address, &w.new);
        }

        // A second apply: the written ones and 5 hold the new value already; 1 is no instance now, 3 holds another
        // value; 7, an instance since, is written.
        mem.poke(at(7), &vt.to_le_bytes());
        let (_, again) = plan_steer(&defs, &mem).unwrap();
        let fields: Vec<u64> = again.writes.iter().map(|w| w.address - FIELD).collect();
        assert_eq!(fields, [at(7)], "{again:?}");
        assert_eq!(again.already, [at(0), at(2), at(5), at(6)]);
        let left: Vec<u64> = again.left.iter().map(|(a, _)| *a).collect();
        assert_eq!(left, [at(3), at(4), IMAGE], "{again:?}");
        assert_eq!(again.skipped.len(), 1, "the freed region: {again:?}");
        let mut log = Vec::new();
        made(&mem, vt, &again.writes, &mut log);
        for w in &log {
            mem.poke(w.address, &w.new);
        }

        // The undo of every write made (0, 2, 6, 7): 0 freed, 6 rewritten by the game; 2 and 7 restored.
        mem.poke(at(0), &0u64.to_le_bytes());
        mem.poke(at(6) + FIELD, &0.4f32.to_le_bytes());
        let undo: Vec<Write> = [at(0), at(2), at(6), at(7)]
            .map(|a| Write {
                address: a + FIELD,
                old: row.new.clone(),
                new: row.old.clone(),
            })
            .into();
        let mut log = Vec::new();
        let results = made(&mem, vt, &undo, &mut log);
        let ok: Vec<bool> = results.iter().map(Result::is_ok).collect();
        assert_eq!(ok, [false, true, false, true], "{results:?}");
        let restored: Vec<(u64, &[u8])> = log
            .iter()
            .map(|w| (w.address - FIELD, &w.new[..]))
            .collect();
        assert_eq!(restored, [(at(2), &row.old[..]), (at(7), &row.old[..])]);
        // A failed write is why it was left.
        let failed = write_instances(&mem, vt, FIELD, &undo[3..], &mut |_| Err("no".into()));
        assert_eq!(failed, [Err("no".to_string())]);
    }

    /// With a list, only the listed instances the search found are planned; the others found are unlisted, a listed
    /// address it didn't find is left once. The cap still counts every instance found.
    #[test]
    fn instance_patches_with_a_list_plan_only_the_instances_listed() {
        let at = |i: u64| HEAP + i * SIZE;
        let mut mem = Fake::default().with(HEAP, heap(4, 0.2), None);
        mem.poke(at(2) + FIELD, &0.35f32.to_le_bytes());
        let defs = [steer()];
        let plan = |only: &[u64], mem: &Fake| {
            plan_instances(&defs, "steer", "build-1", IMAGE, Some(only), mem, &mut go)
        };
        let list = [at(1), at(2), at(2), at(3) + 8, at(3) + 8];
        let (_, found) = plan(&list, &mem).unwrap();
        let fields: Vec<u64> = found.writes.iter().map(|w| w.address - FIELD).collect();
        assert_eq!(fields, [at(1)]);
        assert_eq!(found.already, [at(2)]);
        let why = "the search found no vtable pointer there".to_string();
        assert_eq!(found.left, [(at(3) + 8, why)]);
        assert_eq!((found.found, found.unlisted), (4, 2));
        let (_, all) = plan_steer(&defs, &mem).unwrap();
        assert_eq!((all.found, all.unlisted, all.writes.len()), (4, 0, 3));
        let over = Fake::default().with(HEAP, heap(MAX_INSTANCES as u64 + 1, 0.2), None);
        assert_eq!(
            plan(&[HEAP], &over).unwrap_err(),
            Refusal::TooMany(MAX_INSTANCES + 1)
        );
    }

    /// Hits match by address, in address order (0x100 after 0x20); a key only one side has differs from null.
    #[test]
    fn diffs_match_hits_by_address() {
        let row = |v: serde_json::Value| -> Row { serde_json::from_value(v).unwrap() };
        let a = [
            row(json!({"address": "0x10", "value": "0x1", "w": 0.2})),
            row(json!({"address": "0x8", "value": "0x1", "w": 0.2})),
            row(json!({"address": "0x20", "value": "0x1", "rva": "0x20"})),
            row(json!({"address": "0x30", "why": "gone"})),
        ];
        let b = [
            row(json!({"address": "0x8", "value": "0x1", "w": 0.2})),
            row(json!({"address": "0x10", "value": "0x1", "w": 0.35})),
            row(json!({"address": "0x20", "value": "0x1"})),
            row(json!({"address": "0x100", "value": "0x1", "w": 0.2})),
        ];
        let d = diff(&a, &b);
        let counts = json!({"added": 1, "removed": 1, "changed": 2, "same": 1});
        assert_eq!(d["counts"], counts, "{d}");
        assert_eq!(
            (&d["added"], &d["removed"]),
            (&json!([b[3]]), &json!([a[3]]))
        );
        let changed = json!([
            {"address": "0x10", "w": {"a": 0.2, "b": 0.35}},
            {"address": "0x20", "rva": {"a": "0x20", "b": null}},
        ]);
        assert_eq!(d["changed"], changed, "{d}");
    }

    #[test]
    fn instance_patches_refuse_rows_that_break_the_rules_and_over_the_cap() {
        let mem = Fake::default().with(HEAP, heap(4, 0.2), None);
        let refusal = |d: InstancePatch| plan_steer(&[d], &mem).unwrap_err();
        let unchecked = InstancePatch {
            boundaries_checked: None,
            ..steer()
        };
        assert_eq!(refusal(unchecked), Refusal::Unchecked);
        let same = InstancePatch {
            new: steer().old,
            ..steer()
        };
        assert_eq!(refusal(same), Refusal::NoChange);
        let wide = InstancePatch {
            ty: Type::Bytes(9),
            old: vec![0; 9],
            new: vec![1; 9],
            ..steer()
        };
        assert_eq!(refusal(wide), Refusal::Length { old: 9, new: 9 });
        for (offset, len) in [(0, 4), (4, 4), (0x5e, 4), (0x10000, 4)] {
            let at = InstancePatch { offset, ..steer() };
            assert_eq!(refusal(at), Refusal::Field { offset, len }, "{offset:#x}");
        }
        assert_eq!(
            Refusal::Field { offset: 4, len: 4 }.to_string(),
            "4 bytes at +0x4: an instance field starts at +0x8 or later, below +0x10000, and stays inside one aligned qword"
        );
        let other = InstancePatch {
            build: "build-2".into(),
            ..steer()
        };
        assert_eq!(
            refusal(other).to_string(),
            "defined for build build-2, not the running build-1"
        );
        let unknown = plan_instances(&[steer()], "nope", "build-1", IMAGE, None, &mem, &mut go);
        assert_eq!(
            unknown.unwrap_err().to_string(),
            r#"no patch named "nope" in instance_patches.tsv"#
        );
        // A search stopped before its end writes nothing.
        let two = Fake::default()
            .with(HEAP, heap(4, 0.2), None)
            .with(FREED, heap(4, 0.2), None);
        let mut stop = |_: u64, _: u64| ControlFlow::Break("deadline");
        let stopped = plan_instances(&[steer()], "steer", "build-1", IMAGE, None, &two, &mut stop);
        assert_eq!(
            stopped.unwrap_err(),
            Refusal::Stopped {
                why: "deadline",
                bytes: 4 * SIZE
            }
        );
        // The cap: every instance found, over two pages of hits, or refused.
        let n = MAX_INSTANCES as u64;
        let full = Fake::default().with(HEAP, heap(n, 0.2), None);
        assert_eq!(
            plan_steer(&[steer()], &full).unwrap().1.writes.len(),
            MAX_INSTANCES
        );
        let over = Fake::default().with(HEAP, heap(n + 1, 0.2), None);
        let refused = plan_steer(&[steer()], &over).unwrap_err();
        assert_eq!(refused, Refusal::TooMany(MAX_INSTANCES + 1));
        assert_eq!(
            refused.to_string(),
            "1001 instances found, over the 1000 an instance patch writes: nothing written"
        );
    }
}
