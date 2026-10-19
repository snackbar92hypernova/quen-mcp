//! A running game's config vars ("cvars"), found by name the way the harness's cvar_poke.py finds them (this is its
//! port), read, and their writes planned. Where a var object keeps its fields is the KB's, per build
//! (`cvar_layout.tsv`): nothing here knows a game.
//!
//! The method: the var's name, with a NUL before and after it, in a section that is neither writable nor executable;
//! a var object is a pointer to that name, 8-byte aligned in a writable section, less the name's offset. A hit counts
//! only if its type code is one the layout names, its value pointer points at its own live value, and its group is the
//! one asked for (`Group/Name`). Nothing here writes: a set or a restore is a [`Write`] the platform layer journals and
//! makes. It is a plain store, like cvar_poke's: the engine's setter (its clamp and on-changed callback) doesn't run.

use std::collections::HashSet;
use std::fmt;
use std::io;

use serde::Serialize;

use crate::mem::{self, Image, Memory, SCN_EXECUTE, SCN_WRITE, Skipped, TsvError, Write};
use crate::seeds::{rva, tsv_rows};

const HEADER: [&str; 10] = [
    "build", "group", "name", "flags", "value", "default", "live", "bool", "int", "float",
];

/// One `cvar_layout.tsv` row: where one build's var objects keep their fields, as offsets from the object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The build stamp, as [`mem::image`] gives it.
    pub build: String,
    /// Pointers to the group and name strings.
    pub group: u64,
    pub name: u64,
    /// A u32 whose low byte is the var's type code.
    pub flags: u64,
    /// A pointer to the live value.
    pub value: u64,
    pub default: u64,
    pub live: u64,
    /// The type codes of bool, int and float vars.
    pub codes: [u8; 3],
}

/// The layouts of a KB `cvar_layout.tsv`: a header row of `build group name flags value default live bool int float`,
/// then one row per build: the offsets as `0x` hex, the type codes as decimal bytes.
pub fn from_layout_tsv(text: &str) -> Result<Vec<Layout>, TsvError> {
    let err = |line, msg| TsvError {
        file: "cvar_layout.tsv",
        line,
        msg,
    };
    let mut layouts: Vec<Layout> = Vec::new();
    for (line, [build, offsets @ .., b, i, f]) in
        tsv_rows(text, &HEADER).map_err(|(l, m)| err(l, m))?
    {
        let err = |msg| err(line, msg);
        if build.is_empty() || layouts.iter().any(|l| l.build == build) {
            return Err(err(format!("build {build:?} is blank or defined twice")));
        }
        let o = offsets
            .iter()
            .map(|cell| rva(cell).map(u64::from))
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        let code = |cell: &str| {
            cell.parse::<u8>()
                .map_err(|_| err(format!("type code {cell:?} is not 0-255")))
        };
        let codes = [code(b)?, code(i)?, code(f)?];
        if codes[0] == codes[1] || codes[1] == codes[2] || codes[0] == codes[2] {
            return Err(err(format!(
                "the type codes {codes:?} are not three different ones"
            )));
        }
        layouts.push(Layout {
            build: build.into(),
            group: o[0],
            name: o[1],
            flags: o[2],
            value: o[3],
            default: o[4],
            live: o[5],
            codes,
        });
    }
    Ok(layouts)
}

/// A var's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Float,
}

const KINDS: [Kind; 3] = [Kind::Bool, Kind::Int, Kind::Float];

impl Layout {
    fn kind(&self, code: u8) -> Option<Kind> {
        Some(KINDS[self.codes.iter().position(|&c| c == code)?])
    }
}

/// A var's value: a bool's byte as it is (0 or 1, unless something else wrote it), an int or a float. Serialized as a
/// JSON number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    Bool(u8),
    Int(i32),
    Float(f32),
}

impl Value {
    fn decode(kind: Kind, b: &[u8]) -> Value {
        let word = || b[..4].try_into().expect("a 4-byte read");
        match kind {
            Kind::Bool => Value::Bool(b[0]),
            Kind::Int => Value::Int(i32::from_le_bytes(word())),
            Kind::Float => Value::Float(f32::from_le_bytes(word())),
        }
    }

    fn bytes(self) -> Vec<u8> {
        match self {
            Value::Bool(b) => vec![b],
            Value::Int(i) => i.to_le_bytes().to_vec(),
            Value::Float(f) => f.to_le_bytes().to_vec(),
        }
    }

    /// `text` as a value of `kind`: a bool is 0, 1, false or true; an int decimal, or `0x` / `0o` / `0b` with a sign
    /// before it, in i32's range; a float any finite number in f32's range. Nothing is truncated or wrapped.
    pub fn parse(kind: Kind, text: &str) -> Result<Value, String> {
        let bad = |what| format!("{text:?} is not {what}");
        match kind {
            Kind::Bool => match text.to_ascii_lowercase().as_str() {
                "0" | "false" => Ok(Value::Bool(0)),
                "1" | "true" => Ok(Value::Bool(1)),
                _ => Err(bad("a bool (0/1/true/false)")),
            },
            Kind::Int => int(text)
                .and_then(|i| i32::try_from(i).ok())
                .map(Value::Int)
                .ok_or_else(|| bad("an int in -2147483648..=2147483647")),
            // Through f64, as cvar_poke's float() and struct.pack('<f') round.
            Kind::Float => text
                .parse::<f64>()
                .ok()
                .map(|f| f as f32)
                .filter(|f| f.is_finite())
                .map(Value::Float)
                .ok_or_else(|| bad("a finite float")),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x:?}"),
        }
    }
}

/// Python's `int(text, 0)` without its underscores: a sign, then decimal (no leading zeros), or `0x` / `0o` / `0b`.
fn int(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let lower = digits.to_ascii_lowercase();
    let (radix, digits) = match lower.get(..2) {
        Some("0x") => (16, &lower[2..]),
        Some("0o") => (8, &lower[2..]),
        Some("0b") => (2, &lower[2..]),
        _ => (10, &lower[..]),
    };
    let leading_zero = radix == 10 && digits.starts_with('0') && digits.bytes().any(|b| b != b'0');
    if digits.is_empty() || leading_zero || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let n = i64::from_str_radix(digits, radix).ok()?;
    Some(if negative { -n } else { n })
}

/// A var as read: `Group/Name` as the object names itself (`Name` when it has no group), where it is, its type and
/// flags, its default and its live value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Var {
    pub name: String,
    pub address: u64,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub flags: u32,
    pub default: Value,
    pub live: Value,
}

/// What [`find`] saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Found {
    /// The var objects that pass every check, ascending.
    pub vars: Vec<u64>,
    /// Pointer hits that failed one, and which.
    pub rejected: Vec<Rejected>,
    /// What couldn't be read, so may hold a name or a var nobody saw.
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejected {
    pub address: u64,
    pub why: String,
}

/// Every var object for `full` (`Name` or `Group/Name`; the group may hold `/` itself) in `image`.
pub fn find(mem: &dyn Memory, image: &Image, layout: &Layout, full: &str) -> Found {
    let (group, name) = match full.rsplit_once('/') {
        Some((group, name)) => (Some(group), name),
        None => (None, full),
    };
    let mut found = Found::default();
    let needle = [&[0], name.as_bytes(), &[0]].concat();
    let mut names = HashSet::new();
    for s in image
        .sections
        .iter()
        .filter(|s| s.flags & (SCN_EXECUTE | SCN_WRITE) == 0)
    {
        let tail = needle.len() as u64 - 1;
        mem::chunks(mem, s.base, s.size, tail, &mut found.skipped, |at, b| {
            let hits = b
                .windows(needle.len())
                .enumerate()
                .filter(|(_, w)| *w == needle);
            names.extend(hits.map(|(i, _)| at + i as u64 + 1));
        });
    }
    let mut objects = Vec::new();
    for s in image.sections.iter().filter(|s| s.flags & SCN_WRITE != 0) {
        if names.is_empty() {
            break;
        }
        mem::chunks(mem, s.base, s.size, 0, &mut found.skipped, |at, b| {
            for (i, q) in b.chunks_exact(8).enumerate() {
                let site = at + 8 * i as u64;
                let pointer = u64::from_le_bytes(q.try_into().expect("8 bytes"));
                if names.contains(&pointer) && site - s.base >= layout.name {
                    objects.push(site - layout.name);
                }
            }
        });
    }
    objects.sort_unstable();
    for address in objects {
        match check(mem, layout, address, group) {
            Ok(()) => found.vars.push(address),
            Err(why) => found.rejected.push(Rejected { address, why }),
        }
    }
    found
}

/// Why the object at `obj` is not a var (of `group`, if given): its type code, its value pointer, its group.
fn check(mem: &dyn Memory, layout: &Layout, obj: u64, group: Option<&str>) -> Result<(), String> {
    let [code] = at(mem, obj + layout.flags)?;
    if layout.kind(code).is_none() {
        return Err(format!("type code {code}"));
    }
    let value = u64::from_le_bytes(at(mem, obj + layout.value)?);
    let live = obj + layout.live;
    if value != live {
        return Err(format!(
            "value pointer {value:#x}, not its live value at {live:#x}"
        ));
    }
    if let Some(group) = group {
        let found = string(mem, u64::from_le_bytes(at(mem, obj + layout.group)?));
        if found.as_deref() != Some(group) {
            return Err(format!("group {found:?}"));
        }
    }
    Ok(())
}

fn at<const N: usize>(mem: &dyn Memory, address: u64) -> Result<[u8; N], String> {
    let mut b = [0; N];
    mem.read(address, &mut b)
        .map(|()| b)
        .map_err(|e| format!("can't read {N} bytes at {address:#x}: {e}"))
}

/// The NUL-terminated string at `address`, at most 128 bytes of it; `None` for a null or unreadable pointer.
fn string(mem: &dyn Memory, address: u64) -> Option<String> {
    if address == 0 {
        return None;
    }
    // A short string may end near the end of its region: then read only to the end of its page.
    let mut b = vec![0; 128];
    if mem.read(address, &mut b).is_err() {
        b.truncate((0x1000 - address % 0x1000).min(128) as usize);
        mem.read(address, &mut b).ok()?;
    }
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    Some(String::from_utf8_lossy(&b[..end]).into_owned())
}

/// The one var `found` holds for `full`, or why not.
pub fn one(found: &Found, full: &str) -> Result<u64, String> {
    if let [obj] = found.vars[..] {
        return Ok(obj);
    }
    let list: Vec<String> = found.vars.iter().map(|v| format!("{v:#x}")).collect();
    let hint = match found.vars.len() {
        0 if !found.skipped.is_empty() => {
            format!(" ({} ranges couldn't be read)", found.skipped.len())
        }
        0 => String::new(),
        _ if !full.contains('/') => " (give Group/Name)".into(),
        _ => " (the same group and name twice)".into(),
    };
    Err(format!(
        "{full}: {} matching config objects [{}]; need exactly 1{hint}",
        found.vars.len(),
        list.join(", ")
    ))
}

/// Reads the var object at `obj`.
pub fn read(mem: &dyn Memory, layout: &Layout, obj: u64) -> io::Result<Var> {
    let field = |offset: u64, len: usize| {
        let mut b = vec![0; len];
        mem.read(obj + offset, &mut b).map(|()| b)
    };
    let pointer =
        |offset| field(offset, 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")));
    let flags = u32::from_le_bytes(field(layout.flags, 4)?.try_into().expect("4 bytes"));
    let code = flags as u8;
    let kind = layout.kind(code).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{obj:#x}: type code {code}, not a var's"),
        )
    })?;
    let size = if kind == Kind::Bool { 1 } else { 4 };
    let name = string(mem, pointer(layout.name)?).unwrap_or_default();
    let name = match string(mem, pointer(layout.group)?) {
        Some(group) => format!("{group}/{name}"),
        None => name,
    };
    Ok(Var {
        name,
        address: obj,
        kind,
        flags,
        default: Value::decode(kind, &field(layout.default, size)?),
        live: Value::decode(kind, &field(layout.live, size)?),
    })
}

/// What a write puts in a var.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set<'a> {
    /// A value as text, parsed as the var's type ([`Value::parse`]).
    Value(&'a str),
    /// The var's default (not the settings file's value, if that differs).
    Default,
}

/// The write that puts `set` in `var`'s live value, as just read; or why not.
pub fn plan(var: &Var, layout: &Layout, set: Set) -> Result<Write, String> {
    let new = match set {
        Set::Default => var.default,
        Set::Value(text) => Value::parse(var.kind, text)
            .map_err(|e| format!("{e} for {:?} var {}", var.kind, var.name))?,
    };
    Ok(Write {
        address: var.address + layout.live,
        old: var.live.bytes(),
        new: new.bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{Kind as RegionKind, Region};

    /// A synthetic layout, not any game's.
    fn layout(build: &str) -> Layout {
        Layout {
            build: build.into(),
            group: 0x10,
            name: 0x08,
            flags: 0x20,
            value: 0x28,
            default: 0x18,
            live: 0x1c,
            codes: [7, 8, 9],
        }
    }

    /// A PE-shaped image built in a byte vector: headers, `.text`, `.rdata` (the strings) and `.data` (the objects),
    /// and every read at `base` + offset into it. Like cvar_poke's selftest image, laid out by [`layout`].
    struct Img {
        base: u64,
        bytes: std::cell::RefCell<Vec<u8>>,
        /// Ranges every read fails on.
        bad: Vec<std::ops::Range<u64>>,
        strings: Vec<(String, u64)>,
        next_string: usize,
        next_object: usize,
    }

    const TEXT: usize = 0x1000;
    const RDATA: usize = 0x2000;
    const DATA: usize = 0x4000;
    const STAMP: u32 = 0x1234_abcd;

    impl Img {
        fn new(base: u64) -> Img {
            let mut b = vec![0u8; 0x6000];
            b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
            b[0x80..0x84].copy_from_slice(b"PE\0\0");
            b[0x86..0x88].copy_from_slice(&3u16.to_le_bytes());
            b[0x88..0x8c].copy_from_slice(&STAMP.to_le_bytes());
            b[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
            let sections = [
                (&b".text"[..], TEXT, 0x1000, 0x6000_0020u32),
                (b".rdata", RDATA, 0x2000, 0x4000_0040),
                (b".data", DATA, 0x2000, 0xc000_0040),
            ];
            for (i, (name, va, size, flags)) in sections.into_iter().enumerate() {
                let s = 0x80 + 24 + 0xf0 + 40 * i;
                b[s..s + name.len()].copy_from_slice(name);
                b[s + 8..s + 12].copy_from_slice(&(size as u32).to_le_bytes());
                b[s + 12..s + 16].copy_from_slice(&(va as u32).to_le_bytes());
                b[s + 36..s + 40].copy_from_slice(&flags.to_le_bytes());
            }
            Img {
                base,
                bytes: b.into(),
                bad: Vec::new(),
                strings: Vec::new(),
                next_string: RDATA,
                next_object: DATA,
            }
        }

        /// `text`, NUL before and after, in `.rdata` (once per text): its address.
        fn string(&mut self, text: &str) -> u64 {
            if let Some((_, at)) = self.strings.iter().find(|(s, _)| s == text) {
                return *at;
            }
            let at = self.next_string;
            let mut b = self.bytes.borrow_mut();
            b[at + 1..at + 1 + text.len()].copy_from_slice(text.as_bytes());
            self.next_string += text.len() + 2;
            let address = self.base + at as u64 + 1;
            self.strings.push((text.into(), address));
            address
        }

        fn put(&self, address: u64, bytes: &[u8]) {
            let at = (address - self.base) as usize;
            self.bytes.borrow_mut()[at..at + bytes.len()].copy_from_slice(bytes);
        }

        /// A var object in `.data` by [`layout`]: its address. `value` overrides the value pointer.
        fn var(
            &mut self,
            group: &str,
            name: &str,
            code: u8,
            default: Value,
            value: Option<u64>,
        ) -> u64 {
            let l = layout("");
            let obj = self.base + self.next_object as u64;
            self.next_object += 0x30;
            let (group, name) = (self.string(group), self.string(name));
            self.put(obj + l.group, &group.to_le_bytes());
            self.put(obj + l.name, &name.to_le_bytes());
            self.put(obj + l.flags, &(0x200 | u32::from(code)).to_le_bytes());
            self.put(obj + l.value, &value.unwrap_or(obj + l.live).to_le_bytes());
            self.put(obj + l.default, &default.bytes());
            self.put(obj + l.live, &default.bytes());
            obj
        }

        fn apply(&self, w: &Write) {
            assert_eq!(
                self.get(w.address, w.old.len()),
                w.old,
                "the plan read what is there"
            );
            self.put(w.address, &w.new);
        }

        fn get(&self, address: u64, len: usize) -> Vec<u8> {
            let mut b = vec![0; len];
            self.read(address, &mut b).unwrap();
            b
        }

        fn image(&self) -> Image {
            mem::image(self, self.base).unwrap()
        }
    }

    impl Memory for Img {
        fn read(&self, address: u64, buf: &mut [u8]) -> io::Result<()> {
            let b = self.bytes.borrow();
            let end = address.saturating_add(buf.len() as u64);
            let gone = self.bad.iter().any(|r| r.start < end && address < r.end);
            match address.checked_sub(self.base).map(|at| at as usize) {
                Some(at) if !gone && at + buf.len() <= b.len() => {
                    buf.copy_from_slice(&b[at..at + buf.len()]);
                    Ok(())
                }
                _ => Err(io::Error::other(format!(
                    "nothing readable at {address:#x}"
                ))),
            }
        }

        fn regions(&self) -> io::Result<Vec<Region>> {
            let size = self.bytes.borrow().len() as u64;
            Ok(vec![Region {
                base: self.base,
                size,
                kind: RegionKind::Image,
                section: None,
            }])
        }
    }

    const BASE: u64 = 0x7ff6_0000_0000;

    /// cvar_poke's selftest cases, on the synthetic layout.
    fn sample() -> (Img, Vec<u64>) {
        let mut img = Img::new(BASE);
        // A pointer to a name in .text is not an object: only writable sections hold them.
        let use_x = img.string("UseX");
        for at in (0x1100..0x1200).step_by(8) {
            img.put(BASE + at, &use_x.to_le_bytes());
        }
        let objs = vec![
            img.var("Rendering/RT", "Tlas", 9, Value::Float(800.0), None),
            img.var("Rendering/RT", "Budget", 8, Value::Int(0), None),
            img.var("Rendering", "UseX", 7, Value::Bool(1), None),
            img.var("Rendering", "Enabled", 7, Value::Bool(1), None),
            img.var("Other", "Enabled", 7, Value::Bool(0), None),
            img.var("Other", "Broken", 5, Value::Int(0), None),
            img.var("Other", "Twice", 8, Value::Int(3), None),
            img.var("Other", "Twice", 8, Value::Int(4), None),
        ];
        (img, objs)
    }

    #[test]
    fn images_give_their_stamp_and_sections() {
        let img = Img::new(BASE);
        let image = img.image();
        assert_eq!(image.stamp, "0x1234ABCD");
        let names: Vec<(&str, u64, u64)> = image
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.base - BASE, s.size))
            .collect();
        assert_eq!(
            names,
            [
                (".text", 0x1000, 0x1000),
                (".rdata", 0x2000, 0x2000),
                (".data", 0x4000, 0x2000)
            ]
        );
        assert_eq!(image.sections[2].flags & SCN_WRITE, SCN_WRITE);
        img.put(BASE + 0x80, b"NE\0\0");
        assert!(
            mem::image(&img, BASE)
                .unwrap_err()
                .to_string()
                .contains("no PE header")
        );
        assert!(mem::image(&img, 0x10).is_err());
    }

    #[test]
    fn finds_one_var_by_name_or_group_and_name() {
        let (img, objs) = sample();
        let image = img.image();
        let l = layout("b");
        let one = |full: &str| super::one(&find(&img, &image, &l, full), full);
        assert_eq!(one("Tlas"), Ok(objs[0]));
        assert_eq!(one("Rendering/RT/Tlas"), Ok(objs[0]));
        assert_eq!(one("Other/Enabled"), Ok(objs[4]));
        let two = one("Enabled").unwrap_err();
        assert!(
            two.contains("2 matching config objects") && two.contains("give Group/Name"),
            "{two}"
        );
        assert!(one("Other/Twice").unwrap_err().contains("2 matching"));
        assert!(one("NoSuchVar").unwrap_err().contains("0 matching"));
        assert!(one("Rendering/RT/UseX").unwrap_err().contains("0 matching"));
        let broken = find(&img, &image, &l, "Broken");
        assert!(broken.vars.is_empty());
        assert_eq!(
            broken.rejected,
            [Rejected {
                address: objs[5],
                why: "type code 5".into()
            }]
        );
        let wrong_group = find(&img, &image, &l, "Rendering/RT/UseX");
        assert_eq!(
            wrong_group.rejected.len(),
            1,
            "the .text pointers are not even hits"
        );
        assert_eq!(wrong_group.rejected[0].why, "group Some(\"Rendering\")");
        // A value pointer that isn't the object's own live value.
        img.put(objs[1] + l.value, &0u64.to_le_bytes());
        let found = find(&img, &image, &l, "Budget");
        assert!(
            found.rejected[0]
                .why
                .starts_with("value pointer 0x0, not its live value"),
            "{found:?}"
        );
    }

    #[test]
    fn find_lists_what_it_could_not_read() {
        let (mut img, objs) = sample();
        img.bad.push(BASE + 0x4000..BASE + 0x4001);
        let image = img.image();
        let found = find(&img, &image, &layout("b"), "Tlas");
        assert!(found.vars.is_empty());
        assert_eq!(
            (found.skipped.len(), found.skipped[0].base),
            (1, BASE + 0x4000)
        );
        let why = one(&found, "Tlas").unwrap_err();
        assert!(why.ends_with("(1 ranges couldn't be read)"), "{why}");
        img.bad.clear();
        assert_eq!(find(&img, &image, &layout("b"), "Tlas").vars, [objs[0]]);
    }

    #[test]
    fn reads_and_plans_sets_and_restores() {
        let (img, objs) = sample();
        let l = layout("b");
        let var = read(&img, &l, objs[0]).unwrap();
        assert_eq!(
            var,
            Var {
                name: "Rendering/RT/Tlas".into(),
                address: objs[0],
                kind: Kind::Float,
                flags: 0x209,
                default: Value::Float(800.0),
                live: Value::Float(800.0),
            }
        );
        let json = serde_json::to_value(&var).unwrap();
        assert_eq!(
            (json["type"].clone(), json["live"].clone()),
            ("float".into(), 800.0.into())
        );
        let set = plan(&var, &l, Set::Value("400")).unwrap();
        assert_eq!(
            (set.address, &set.old[..], &set.new[..]),
            (
                objs[0] + l.live,
                &800f32.to_le_bytes()[..],
                &400f32.to_le_bytes()[..]
            )
        );
        img.apply(&set);
        let var = read(&img, &l, objs[0]).unwrap();
        assert_eq!(var.live, Value::Float(400.0));
        img.apply(&plan(&var, &l, Set::Default).unwrap());
        assert_eq!(read(&img, &l, objs[0]).unwrap().live, Value::Float(800.0));
        // An int, a bool by word, and the bool's neighbour bytes left alone.
        let budget = read(&img, &l, objs[1]).unwrap();
        img.apply(&plan(&budget, &l, Set::Value("256")).unwrap());
        assert_eq!(read(&img, &l, objs[1]).unwrap().live, Value::Int(256));
        let use_x = read(&img, &l, objs[2]).unwrap();
        let off = plan(&use_x, &l, Set::Value("FALSE")).unwrap();
        assert_eq!((off.old.clone(), off.new.clone()), (vec![1], vec![0]));
        img.apply(&off);
        assert_eq!(read(&img, &l, objs[2]).unwrap().live, Value::Bool(0));
        for (var, text, why) in [
            (&use_x, "2", "is not a bool"),
            (&budget, "1.5", "is not an int"),
            (&budget, "2147483648", "is not an int"),
            (&var, "1e39", "finite float"),
            (&var, "nan", "finite float"),
        ] {
            let e = plan(var, &l, Set::Value(text)).unwrap_err();
            assert!(e.contains(why) && e.contains(&var.name), "{text}: {e}");
        }
        assert!(
            read(&img, &l, objs[5])
                .unwrap_err()
                .to_string()
                .contains("type code 5")
        );
    }

    #[test]
    fn values_parse_as_cvar_poke_s_do() {
        let int = |t| Value::parse(Kind::Int, t);
        for (text, n) in [
            ("0", 0),
            ("-7", -7),
            ("+7", 7),
            ("0x10", 16),
            ("-0X10", -16),
            ("0o17", 15),
            ("0b101", 5),
            ("000", 0),
        ] {
            assert_eq!(int(text), Ok(Value::Int(n)), "{text}");
        }
        for text in [
            "",
            "-",
            "010",
            "0x",
            "0x+5",
            "1_000",
            " 1",
            "1.0",
            "0xffffffff",
        ] {
            assert!(int(text).is_err(), "{text:?}");
        }
        assert_eq!(Value::parse(Kind::Float, "0.1"), Ok(Value::Float(0.1)));
        assert_eq!(Value::parse(Kind::Float, "-2"), Ok(Value::Float(-2.0)));
        assert_eq!(Value::parse(Kind::Bool, "True"), Ok(Value::Bool(1)));
        assert_eq!(Value::Float(800.0).to_string(), "800.0");
    }

    #[test]
    fn layout_rows_parse() {
        let header = HEADER.join("\t");
        let text =
            format!("# synthetic\n{header}\nb-1\t0x10\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t8\t9\n");
        assert_eq!(from_layout_tsv(&text).unwrap(), [layout("b-1")]);
        for (row, why) in [
            (
                "b-1\t0x10\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t8\t9",
                "defined twice",
            ),
            ("\t0x10\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t8\t9", "blank"),
            (
                "b-2\t16\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t8\t9",
                "not 0x-prefixed hex",
            ),
            (
                "b-2\t0x10\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t8\t256",
                "not 0-255",
            ),
            (
                "b-2\t0x10\t0x8\t0x20\t0x28\t0x18\t0x1c\t7\t7\t9",
                "three different",
            ),
            ("b-2\t0x10", "2 columns"),
        ] {
            let e = from_layout_tsv(&format!("{text}{row}\n")).unwrap_err();
            assert!(e.msg.contains(why) && e.line == 4, "{row:?}: {e}");
            assert!(e.to_string().starts_with("cvar_layout.tsv line 4: "));
        }
    }
}
