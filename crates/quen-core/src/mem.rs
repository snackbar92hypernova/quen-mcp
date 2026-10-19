//! The platform-neutral half of the live-memory tools (`mem.read`, `mem.scan`, `mem.find`, `mem.follow`, `mem.walk`,
//! `mem.patch` / `mem.unpatch`), against a [`Memory`] trait the Windows layer implements with VirtualQueryEx and
//! ReadProcessMemory.
//!
//! Nothing here writes. A patch [`plan`] checks the KB definition (`patches.tsv`) and the live bytes, and returns the one
//! [`Write`] allowed, or a [`Refusal`]. The platform layer journals it as an
//! [`Action::GameMemory`](crate::journal::Action::GameMemory) record (its time taints later saves), then makes it with a
//! handle open only for the write.

use std::fmt;
use std::io;
use std::ops::Range;

use serde::Serialize;
use serde_json::json;

use crate::build::Stamp;
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
    pub base: u64,
    pub size: u64,
    pub kind: Kind,
    /// The image section the region lies in (`.text`, `.rdata`), when the platform layer knows it.
    pub section: Option<String>,
}

/// What backs a region (VirtualQueryEx's `MEM_IMAGE`, `MEM_MAPPED`, `MEM_PRIVATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// A value read. It serializes as a JSON number (serde_json writes a NaN or an infinity as `null`), a pointer as 0x
/// hex, or raw bytes as spaced hex.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Ptr(#[serde(serialize_with = "hex::address")] u64),
    Bytes(#[serde(with = "hex")] Vec<u8>),
}

/// Reads one value of type `ty` at `address`.
pub fn read(mem: &dyn Memory, address: u64, ty: Type) -> io::Result<Value> {
    if let Type::Bytes(len) = ty
        && !(1..=MAX_BYTES).contains(&len)
    {
        let why = format!("{len} bytes: a raw read is 1 to {MAX_BYTES}");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, why));
    }
    let mut b = vec![0; ty.size()];
    mem.read(address, &mut b)?;
    Ok(ty.decode(&b))
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
/// of `align`. It always has a fixed bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    mask: Vec<u8>,
    align: u64,
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

    /// A qword's little-endian bytes, at 8-byte-aligned addresses only.
    pub fn qword(value: u64) -> Self {
        let (bytes, mask) = (value.to_le_bytes().into(), vec![0xff; 8]);
        Self {
            bytes,
            mask,
            align: 8,
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
        })
    }

    fn matches(&self, at: &[u8]) -> bool {
        at.iter()
            .zip(&self.bytes)
            .zip(&self.mask)
            .all(|((b, p), m)| (b ^ p) & m == 0)
    }
}

/// Which regions a scan reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
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

/// One page of scan hits, and what the page covered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanPage<H = u64> {
    /// Where the pattern matches, ascending ([`find`]'s: with their context).
    pub hits: Vec<H>,
    /// The `from` of the next page when this one is full; `None` when the scope is done.
    pub next: Option<u64>,
    /// The regions this page searched, in whole or in part, and the bytes it covered in them (from a region's base or
    /// `from`, to the region's end or where the page stopped).
    pub regions: usize,
    pub bytes: u64,
    /// What in scope couldn't be read, so may hold matches nobody saw.
    /// ponytail: unbounded; a dying process lists every region here. Cap it if that ever floods a result.
    pub skipped: Vec<Skipped>,
}

/// A part of the scope that wasn't read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub base: u64,
    /// 0 when the region wasn't listed at all.
    pub size: u64,
    pub why: String,
}

/// Searches `scope` for `pattern`, from address `from` on, for at most `limit` hits (clamped to 1 ..= [`MAX_HITS`]).
/// Pass a full page's `next` as `from` for the page after it.
/// ponytail: a naive match (bytes × pattern length at worst), and a match never spans two regions, even adjacent ones.
pub fn scan(
    mem: &dyn Memory,
    pattern: &Pattern,
    scope: &Scope,
    from: u64,
    limit: usize,
) -> io::Result<ScanPage> {
    let limit = limit.clamp(1, MAX_HITS);
    let mut page = ScanPage::default();
    let mut regions = mem.regions()?;
    regions.retain(|r| match scope {
        Scope::All => true,
        Scope::Sections(names) => r.section.as_ref().is_some_and(|s| names.contains(s)),
        Scope::Regions(bases) => bases.contains(&r.base),
        Scope::Kinds(kinds) => kinds.contains(&r.kind),
    });
    if let Scope::Regions(bases) = scope {
        for &base in bases
            .iter()
            .filter(|&&b| !regions.iter().any(|r| r.base == b))
        {
            let why = "not a readable region now".into();
            page.skipped.push(Skipped { base, size: 0, why });
        }
    }
    regions.sort_by_key(|r| r.base);
    let n = pattern.bytes.len() as u64;
    for region in regions {
        let end = region.base.saturating_add(region.size);
        let start = region.base.max(from);
        if start >= end {
            continue;
        }
        let (mut at, mut stop) = (start, end);
        while end.saturating_sub(at) >= n {
            let mut buf = vec![0; (end - at).min(CHUNK + n - 1) as usize];
            if let Err(e) = mem.read(at, &mut buf) {
                let (size, why) = (end - at, e.to_string());
                page.skipped.push(Skipped {
                    base: at,
                    size,
                    why,
                });
                stop = at;
                break;
            }
            // At most CHUNK windows: matches starting past them are the next chunk's.
            for (i, window) in buf.windows(n as usize).enumerate() {
                let hit = at + i as u64;
                if hit.is_multiple_of(pattern.align) && pattern.matches(window) {
                    page.hits.push(hit);
                    if page.hits.len() == limit {
                        page.regions += 1;
                        page.bytes += hit + 1 - start;
                        page.next = Some(hit + 1);
                        return Ok(page);
                    }
                }
            }
            at = at.saturating_add(CHUNK);
        }
        if stop > start {
            page.regions += 1;
            page.bytes += stop - start;
        }
    }
    Ok(page)
}

/// The most context bytes a [`find`] hit has on each side.
pub const MAX_CONTEXT: u64 = 256;

/// A [`find`] hit, and the bytes before and after its qword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    pub address: u64,
    #[serde(with = "hex")]
    pub before: Vec<u8>,
    #[serde(with = "hex")]
    pub after: Vec<u8>,
    /// Why the context couldn't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// [`scan`]'s page for the qword `value` at 8-byte-aligned addresses in the image and private regions, each hit with up
/// to `context` bytes (clamped to [`MAX_CONTEXT`]) on each side, cut at the edges of its region.
pub fn find(
    mem: &dyn Memory,
    value: u64,
    from: u64,
    limit: usize,
    context: u64,
) -> io::Result<ScanPage<Found>> {
    let scope = Scope::Kinds(vec![Kind::Image, Kind::Private]);
    let page = scan(mem, &Pattern::qword(value), &scope, from, limit)?;
    let regions = mem.regions()?;
    let n = context.min(MAX_CONTEXT);
    let hits = page
        .hits
        .iter()
        .map(|&address| {
            let region = regions
                .iter()
                .find(|r| r.base <= address && address < r.base + r.size);
            let (lo, hi) = region.map_or((address, address + 8), |r| {
                let hi = address.saturating_add(8 + n).min(r.base + r.size);
                (address.saturating_sub(n).max(r.base), hi)
            });
            let mut b = vec![0; (hi - lo) as usize];
            let at = (address - lo) as usize;
            match mem.read(lo, &mut b) {
                Ok(()) => Found {
                    address,
                    before: b[..at].to_vec(),
                    after: b[at + 8..].to_vec(),
                    why: None,
                },
                Err(e) => Found {
                    address,
                    before: Vec::new(),
                    after: Vec::new(),
                    why: Some(e.to_string()),
                },
            }
        })
        .collect();
    Ok(ScanPage {
        hits,
        next: page.next,
        regions: page.regions,
        bytes: page.bytes,
        skipped: page.skipped,
    })
}

/// One hop of a pointer chain: the qword read at `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Hop {
    pub at: u64,
    pub value: u64,
}

/// A pointer chain followed: every hop read, then its end or where it broke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chain {
    pub hops: Vec<Hop>,
    /// The last value read (the base, with no offsets), when every hop was read.
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

/// An array of structs to read: `count` elements `stride` bytes apart from `base`. Each element is the struct its
/// fields are read from or, with `deref`, the start of a pointer chain to it ([`follow`]'s offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    pub base: u64,
    pub stride: u64,
    pub count: u64,
    pub deref: Vec<i64>,
    pub fields: Vec<Field>,
    pub filter: Option<Filter>,
}

/// A field of a walked struct: a value of `ty` at `offset` from the struct's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub offset: u64,
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

/// A walk's row, as JSON.
pub type Row = serde_json::Map<String, serde_json::Value>;

/// Reads `w`: per element, `{index, address, <field>: value, ...}`, with `target` (the struct's address) when it has a
/// deref, or `{index, address, why}` when it can't be read. The filter drops rows, never a `why` one. Err: refused,
/// nothing read.
pub fn walk(mem: &dyn Memory, w: &Walk) -> Result<Vec<Row>, String> {
    if !(1..=MAX_COUNT).contains(&w.count) {
        return Err(format!("count {}: a walk is 1 to {MAX_COUNT}", w.count));
    }
    if !(1..=MAX_STRIDE).contains(&w.stride) {
        return Err(format!("stride {}: 1 to {MAX_STRIDE}", w.stride));
    }
    if w.fields.is_empty() {
        return Err("no fields".into());
    }
    // Without a deref the struct is the element; past one, it is read up to MAX_STRIDE bytes from its start.
    let room = if w.deref.is_empty() {
        w.stride
    } else {
        MAX_STRIDE
    };
    for (i, f) in w.fields.iter().enumerate() {
        let name = &f.name;
        if name.is_empty() || ["index", "address", "target", "why"].contains(&name.as_str()) {
            return Err(format!(
                "field name {name:?}: blank, or index, address, target or why"
            ));
        }
        if w.fields[..i].iter().any(|g| g.name == *name) {
            return Err(format!("field {name} is named twice"));
        }
        if let Type::Bytes(len) = f.ty
            && !(1..=MAX_BYTES).contains(&len)
        {
            return Err(format!(
                "field {name}: {len} bytes, a field is 1 to {MAX_BYTES}"
            ));
        }
        if f.offset.saturating_add(f.ty.size() as u64) > room {
            return Err(format!(
                "field {name} ends past {room:#x}: the stride, or {MAX_STRIDE:#x} from a deref's target"
            ));
        }
    }
    let lo = w.fields.iter().map(|f| f.offset).min().unwrap_or(0);
    let hi = w.fields.iter().map(|f| f.offset + f.ty.size() as u64);
    let len = hi.max().unwrap_or(0) - lo;
    let total = w.count * (len + 8 * w.deref.len() as u64);
    if total > MAX_WALK {
        return Err(format!(
            "{total} bytes to read: a walk reads at most {MAX_WALK}"
        ));
    }
    let filter = match &w.filter {
        None => None,
        Some(Filter { field, eq, value }) => {
            let f = w.fields.iter().find(|f| f.name == *field);
            let f = f.ok_or(format!("filter: no field {field}"))?;
            let want = f.ty.encode(value).map_err(|e| format!("filter: {e}"))?;
            Some(((f.offset - lo) as usize, *eq, want))
        }
    };
    let mut rows = Vec::new();
    let mut buf = vec![0; len as usize];
    // ponytail: one read per element (and per hop), ~100k syscalls at the cap; read whole arrays if that is slow.
    for index in 0..w.count {
        let address = w.base.checked_add(index * w.stride);
        let mut row = Row::from_iter([
            ("index".into(), json!(index)),
            ("address".into(), json!(address)),
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
        let read = target.and_then(|t| {
            if !w.deref.is_empty() {
                row.insert("target".into(), json!(t));
            }
            let at = t.checked_add(lo).ok_or("past the top of memory")?;
            mem.read(at, &mut buf)
                .map_err(|e| format!("can't read {len} bytes at {at:#x}: {e}"))
        });
        if let Err(why) = read {
            row.insert("why".into(), json!(why));
            rows.push(row);
            continue;
        }
        if let Some((at, eq, want)) = &filter
            && (buf[*at..*at + want.len()] == want[..]) != *eq
        {
            continue;
        }
        for f in &w.fields {
            let at = (f.offset - lo) as usize;
            let value = f.ty.decode(&buf[at..at + f.ty.size()]);
            row.insert(f.name.clone(), json!(value));
        }
        rows.push(row);
    }
    Ok(rows)
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

/// Reads `[base, base + size)` in chunks of at most [`CHUNK`] + `tail` bytes, each starting [`CHUNK`] after the last,
/// and calls `f(address, bytes)` on each: its windows of `tail + 1` bytes start in that chunk only, so none is seen
/// twice or lost at a seam. A chunk that can't be read goes in `skipped`.
pub(crate) fn chunks(
    mem: &dyn Memory,
    base: u64,
    size: u64,
    tail: u64,
    skipped: &mut Vec<Skipped>,
    mut f: impl FnMut(u64, &[u8]),
) {
    let end = base.saturating_add(size);
    let mut at = base;
    while at < end {
        let mut buf = vec![0; (end - at).min(CHUNK + tail) as usize];
        match mem.read(at, &mut buf) {
            Ok(()) => f(at, &buf),
            Err(e) => skipped.push(Skipped {
                base: at,
                size: (end - at).min(CHUNK),
                why: e.to_string(),
            }),
        }
        at = at.saturating_add(CHUNK);
    }
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
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(err(format!("name {name:?} is blank or has whitespace")));
        }
        if build.is_empty() {
            return Err(err("the build is blank".into()));
        }
        if patches.iter().any(|p| p.name == name && p.build == build) {
            return Err(err(format!("{name} is defined twice for build {build}")));
        }
        let bytes =
            |cell: &str, which: &str| parse_hex(cell).map_err(|msg| err(format!("{which}: {msg}")));
        let unchecked = matches!(
            checked.to_ascii_lowercase().as_str(),
            "" | "-" | "0" | "no" | "false"
        );
        patches.push(Patch {
            name: name.into(),
            rva: rva(rva_cell).map_err(err)?,
            old: bytes(old, "old")?,
            new: bytes(new, "new")?,
            build: build.into(),
            boundaries_checked: (!unchecked).then(|| checked.into()),
        });
    }
    Ok(patches)
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
    /// No definition by this name.
    Unknown(String),
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
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "no patch named {name:?} in patches.tsv"),
            Self::Build { running, defined } => write!(
                f,
                "defined for build {}, not the running {running}",
                defined.join(", ")
            ),
            Self::Unchecked => f.write_str(
                "boundaries-checked is not set: check old and new with a disassembler first",
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
            [] => Refusal::Unknown(name.into()),
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

/// Serde for bytes as spaced lowercase hex, and for an address as 0x hex.
mod hex {
    use serde::Serializer;

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(bytes))
    }

    pub fn address<S: Serializer>(address: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{address:#x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ops::Range;

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
                section: section.map(Into::into),
            };
            self.regions.push((region, bytes));
            self
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
        let at = |offset: u64, ty| read(&mem, 0x1000 + offset, ty).unwrap();
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
        assert_eq!(json!(at(0, Type::Ptr)), "0x807060504030201");
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
        let page = scan(&mem, &p, &Scope::All, 0, 100).unwrap();
        assert_eq!(page.hits, all, "overlapping matches too");
        assert_eq!((page.next, page.regions, page.bytes), (None, 3, 11));
        assert!(page.skipped.is_empty());
        let (mut hits, mut from) = (Vec::new(), 0);
        loop {
            let page = scan(&mem, &p, &Scope::All, from, 2).unwrap();
            assert!(page.hits.len() <= 2);
            hits.extend(page.hits);
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        assert_eq!(hits, all, "pages resume after their last hit");
        let text = Scope::Sections(vec![".text".into()]);
        assert_eq!(scan(&mem, &p, &text, 0x1002, 100).unwrap().hits, [0x1004]);
        let regions = Scope::Regions(vec![0x9000, 0x2000]);
        assert_eq!(
            scan(&mem, &p, &regions, 0, 100).unwrap().hits,
            [0x2001, 0x9000]
        );
        let page = scan(&mem, &p, &Scope::All, 0, 0).unwrap();
        assert_eq!(
            (page.hits, page.next, page.bytes),
            (vec![0x1000], Some(0x1001), 1)
        );
        let page = scan(&mem, &p, &Scope::All, 0, usize::MAX).unwrap();
        assert_eq!(
            page.hits.len(),
            all.len(),
            "the limit is clamped, not refused"
        );
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
        let page = scan(&mem, &p, &Scope::Sections(vec![".text".into()]), 0, 100).unwrap();
        assert_eq!(page.hits, [0x1000, 0x1001, 0x1002, 0x1003]);
        assert_eq!((page.regions, page.bytes), (1, 4));
        let why = "no 4 readable bytes at 0x2000".into();
        assert_eq!(
            page.skipped,
            [Skipped {
                base: 0x2000,
                size: 4,
                why
            }]
        );
        let page = scan(&mem, &p, &Scope::Regions(vec![0x1000, 0x5000]), 0, 100).unwrap();
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
    fn scans_match_across_chunk_seams_and_keep_what_preceded_a_failed_chunk() {
        let base = 0x10_0000;
        let size = 2 * CHUNK + 8;
        let mut bytes = vec![0; size as usize];
        let seam = CHUNK as usize;
        bytes[seam - 2..seam + 2].copy_from_slice(&[1, 2, 3, 4]);
        bytes[size as usize - 4..].copy_from_slice(&[1, 2, 3, 4]);
        let mut mem = Fake::default().with(base, bytes, None);
        let p = Pattern::parse("01 02 03 04").unwrap();
        let page = scan(&mem, &p, &Scope::All, 0, 100).unwrap();
        assert_eq!(page.hits, [base + CHUNK - 2, base + size - 4]);
        assert_eq!((page.regions, page.bytes), (1, size));
        mem.bad.push(base + CHUNK + 100..base + CHUNK + 101);
        let page = scan(&mem, &p, &Scope::All, 0, 100).unwrap();
        assert_eq!(page.hits, [base + CHUNK - 2]);
        assert_eq!((page.regions, page.bytes), (1, CHUNK));
        let (at, rest) = (base + CHUNK, size - CHUNK);
        assert!(matches!(&page.skipped[..], [s] if (s.base, s.size) == (at, rest)));
    }

    #[test]
    fn chunks_see_each_window_once_and_list_what_failed() {
        let base = 0x10_0000;
        let size = 2 * CHUNK + 5;
        let mut mem = Fake::default().with(base, vec![0; size as usize], None);
        let mut starts = Vec::new();
        let mut skipped = Vec::new();
        chunks(&mem, base, size, 3, &mut skipped, |at, b| {
            starts.extend(b.windows(4).enumerate().map(|(i, _)| at + i as u64));
        });
        let all: Vec<u64> = (base..base + size - 3).collect();
        assert_eq!((starts, skipped), (all, vec![]));
        mem.bad.push(base + CHUNK..base + CHUNK + 1);
        let mut skipped = Vec::new();
        chunks(&mem, base, size, 3, &mut skipped, |_, _| {});
        let skips: Vec<(u64, u64)> = skipped.iter().map(|s| (s.base, s.size)).collect();
        assert_eq!(
            skips,
            [(base, CHUNK), (base + CHUNK, CHUNK)],
            "a chunk's tail reads into the next one"
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
            serde_json::to_value(&chain).unwrap()["hops"][1],
            json!({"at": base + 0x18, "value": 0x7700})
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
            section: None,
        };
        mem.regions.push((mapped, v.to_le_bytes().into()));
        let page = find(&mem, v, 0, 100, 4).unwrap();
        let found = |address, before, after| Found {
            address,
            before,
            after,
            why: None,
        };
        assert_eq!(
            page.hits,
            [
                found(0x1008, vec![0x11; 4], vec![0x11; 4]),
                found(0x3000, vec![], vec![0x22; 4])
            ],
            "unaligned and mapped copies aren't hits; context stops at the region's edge"
        );
        assert_eq!((page.next, page.regions), (None, 2));
        assert_eq!(
            json!(page.hits[1]),
            json!({"address": 0x3000, "before": "", "after": "22 22 22 22"})
        );
        let wide = find(&mem, v, 0, 100, 10_000).unwrap();
        let sides: Vec<(usize, usize)> = wide
            .hits
            .iter()
            .map(|h| (h.before.len(), h.after.len()))
            .collect();
        assert_eq!(sides, [(8, 16), (0, 8)], "the region's bytes, at most");
        let first = find(&mem, v, 0, 1, 0).unwrap();
        assert_eq!(first.next, Some(0x1009));
        let second = find(&mem, v, 0x1009, 1, 0).unwrap();
        assert_eq!(
            (second.hits[0].address, second.next),
            (0x3000, Some(0x3001))
        );
        assert!(find(&mem, v, 0x3001, 1, 0).unwrap().hits.is_empty());
    }

    /// Three 16-byte elements: an id, a float and a pointer to a 32-byte struct (null for the second).
    fn array() -> Fake {
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

    fn field(name: &str, offset: u64, ty: Type) -> Field {
        let name = name.into();
        Field { name, offset, ty }
    }

    fn elements(fields: Vec<Field>) -> Walk {
        Walk {
            base: 0x1000,
            stride: 0x10,
            count: 3,
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
        let mem = array();
        let fields = vec![
            field("id", 0, Type::U32),
            field("f", 4, Type::F32),
            field("p", 8, Type::Ptr),
        ];
        let rows = walk(&mem, &elements(fields.clone())).unwrap();
        assert_eq!(
            json!(rows),
            json!([
                {"index": 0, "address": 0x1000, "id": 7, "f": 1.5, "p": "0x2000"},
                {"index": 1, "address": 0x1010, "id": 8, "f": 1.5, "p": "0x0"},
                {"index": 2, "address": 0x1020, "id": 9, "f": 1.5, "p": "0x2020"},
            ])
        );
        let derefs = Walk {
            deref: vec![8],
            ..elements(vec![
                field("name", 0, Type::Bytes(4)),
                field("n", 0x10, Type::I64),
            ])
        };
        let rows = walk(&mem, &derefs).unwrap();
        assert_eq!(
            json!(rows),
            json!([
                {"index": 0, "address": 0x1000, "target": 0x2000, "name": "61 62 63 64", "n": -5},
                {"index": 1, "address": 0x1010, "target": 0, "why": "can't read 24 bytes at 0x0: no 24 readable bytes at 0x0"},
                {"index": 2, "address": 0x1020, "target": 0x2020, "name": "77 78 79 7a", "n": 0},
            ])
        );
        let two = Walk {
            deref: vec![8, 0],
            ..derefs.clone()
        };
        assert_eq!(
            walk(&mem, &two).unwrap()[1]["why"],
            "deref: hop 0 read a null pointer"
        );
        let index = |rows: Vec<Row>| -> Vec<u64> {
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
            assert_eq!(index(walk(&mem, &w).unwrap()), kept, "{filter:?}");
        }
        let named = Walk {
            filter: filter("name", true, "61 62 63 64"),
            ..derefs
        };
        assert_eq!(
            index(walk(&mem, &named).unwrap()),
            [0, 1],
            "a why row stays"
        );
    }

    #[test]
    fn walks_refuse_before_reading() {
        let mem = array();
        let one = || elements(vec![field("id", 0, Type::U32)]);
        for (w, why) in [
            (Walk { count: 0, ..one() }, "count 0"),
            (
                Walk {
                    count: MAX_COUNT + 1,
                    ..one()
                },
                "count 100001",
            ),
            (Walk { stride: 0, ..one() }, "stride 0"),
            (
                Walk {
                    stride: MAX_STRIDE + 1,
                    ..one()
                },
                "stride 65537",
            ),
            (elements(vec![]), "no fields"),
            (elements(vec![field("why", 0, Type::U8)]), "field name"),
            (
                elements(vec![field("a", 0, Type::U8), field("a", 1, Type::U8)]),
                "named twice",
            ),
            (elements(vec![field("b", 0, Type::Bytes(0))]), "0 bytes"),
            (elements(vec![field("x", 0xc, Type::F64)]), "ends past 0x10"),
            (
                Walk {
                    deref: vec![8],
                    ..elements(vec![field("x", MAX_STRIDE, Type::U8)])
                },
                "ends past 0x10000",
            ),
            (
                Walk {
                    count: MAX_COUNT,
                    stride: MAX_STRIDE,
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
            Err(Refusal::Unknown("mask_z".into()))
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
}
