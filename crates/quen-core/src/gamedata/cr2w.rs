//! CR2W, the game's asset format (and REDkit's depot files): a 160-byte header with ten table descriptors (offset,
//! count, crc32), the tables (0 strings, 1 names, 2 imports, 3 properties, 4 exports, 5 buffers), then each export's
//! data: a zero byte and a property list, each property `u16 name, u16 type, u32 size (from the size field), value`,
//! ended by a u16 0. Every property carries its size, so a type quen doesn't decode comes back raw and the next one is
//! read: the reader never fails on a type. A version quen hasn't checked, or a crc that doesn't verify, is reported,
//! not refused (a modder's file may carry a stale one). Older files (in REDkit's depot, versions 139 to 154) have
//! another header ([`old`]): their names and imports are read, not their exports. Read-only, on bytes.

use serde_json::{Map, Value, json};

use crate::journal::hex;

pub const MAGIC: &[u8; 4] = b"CR2W";
/// The versions whose layout was checked on real files: 164 (the game's 5.01 bundles), 162 (an older packer's), 159
/// (REDkit's depot).
pub const KNOWN_VERSIONS: [u32; 3] = [159, 162, 164];
pub const HEADER: usize = 160;
/// Each of tables 0 to 5's entry width: its crc32 is over `count × width` bytes (table 0 counts bytes).
const WIDTHS: [usize; 6] = [1, 8, 8, 16, 24, 24];
const TABLES: [&str; 6] = [
    "strings",
    "names",
    "imports",
    "properties",
    "exports",
    "buffers",
];
/// Raw bytes shown as hex, at most.
const RAW_HEX: usize = 256;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

#[derive(Debug, Clone, Copy)]
struct Table {
    offset: usize,
    count: usize,
    crc32: u32,
}

fn tables(header: &[u8]) -> Option<[Table; 10]> {
    let mut out = [Table {
        offset: 0,
        count: 0,
        crc32: 0,
    }; 10];
    for (i, t) in out.iter_mut().enumerate() {
        let at = 40 + 12 * i;
        *t = Table {
            offset: u32_at(header, at)? as usize,
            count: u32_at(header, at + 4)? as usize,
            crc32: u32_at(header, at + 8)?,
        };
    }
    Some(out)
}

/// Whether a CR2W file's header is the older one: `version, flags`, then (offset, count) of its names, objects, links
/// and dependencies, its names and dependencies written as strings ([`vlq_string`]). Told by the 160-byte header's
/// table 0, which starts right after it: so in every file checked (REDkit's depot, 2026-10-20: the older header in
/// versions 139 to 154, this one in 155 to 164; the game's bundles: 164), never in an older one.
fn old(header: &[u8]) -> bool {
    u32_at(header, 40) != Some(HEADER as u32)
}

/// How many bytes the header and the string, name and import tables take, from the file's first [`HEADER`] bytes:
/// what a names-level read decompresses (the whole file, `usize::MAX`, for the older header, whose tables hold strings).
/// None: not CR2W.
pub fn names_end(header: &[u8]) -> Option<usize> {
    if header.get(..4) != Some(MAGIC) {
        return None;
    }
    if old(header) {
        return Some(usize::MAX);
    }
    let t = tables(header)?;
    let ends = (0..3)
        .filter(|&i| t[i].count > 0)
        .map(|i| t[i].offset + t[i].count * WIDTHS[i]);
    Some(ends.fold(HEADER, usize::max))
}

/// An import: another file this one loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The depot path, `\` between folders.
    pub path: String,
    pub class: String,
    pub flags: u16,
}

/// A file's version, names and imports: what a names-level scan tests.
#[derive(Debug, Clone)]
pub struct Names {
    pub version: u32,
    pub names: Vec<String>,
    pub imports: Vec<Import>,
}

/// The name and import tables from the file's first [`names_end`] bytes (or more). Err: not CR2W, or a table that
/// lies outside them.
pub fn names(prefix: &[u8]) -> Result<Names, String> {
    let end = names_end(prefix).ok_or("not CR2W")?;
    if old(prefix) {
        return old_names(prefix);
    }
    if prefix.len() < end {
        return Err(format!(
            "its tables end at {end}, past the {} bytes read",
            prefix.len()
        ));
    }
    let t = tables(prefix).ok_or("a short header")?;
    let strings = prefix
        .get(t[0].offset..t[0].offset + t[0].count)
        .ok_or("the string table lies outside the file")?;
    let string = |at: u32| -> Result<String, String> {
        let s = strings
            .get(at as usize..)
            .and_then(|s| s.split(|&b| b == 0).next())
            .ok_or(format!("a string at {at}, outside the string table"))?;
        Ok(s.iter().map(|&b| char::from(b)).collect())
    };
    let row = |table: Table, k: usize| table.offset + k * WIDTHS[1];
    let names = (0..t[1].count)
        .map(|k| string(u32_at(prefix, row(t[1], k)).unwrap_or(u32::MAX)))
        .collect::<Result<Vec<_>, _>>()?;
    let imports = (0..t[2].count)
        .map(|k| {
            let at = row(t[2], k);
            let class = u16_at(prefix, at + 4).unwrap_or(u16::MAX);
            Ok(Import {
                path: string(u32_at(prefix, at).unwrap_or(u32::MAX))?,
                class: names.get(usize::from(class)).cloned().unwrap_or_default(),
                flags: u16_at(prefix, at + 6).unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Names {
        version: u32_at(prefix, 4).unwrap_or(0),
        names,
        imports,
    })
}

/// The older header's names and dependencies (the whole file): `count` strings from each table's offset, the
/// dependencies' first one implicit (empty, not written).
fn old_names(d: &[u8]) -> Result<Names, String> {
    let word = |i: usize| {
        u32_at(d, 4 + 4 * i)
            .map(|w| w as usize)
            .ok_or("a short header")
    };
    let strings = |at: usize, count: usize| -> Result<Vec<String>, String> {
        let mut pos = at;
        (0..count.min(d.len()))
            .map(|_| {
                let (s, n) = d
                    .get(pos..)
                    .and_then(vlq_string)
                    .ok_or(format!("a string at {pos} that doesn't fit the file"))?;
                pos += n;
                Ok(s)
            })
            .collect()
    };
    let deps = strings(word(8)?, word(9)?.saturating_sub(1))?;
    Ok(Names {
        version: word(0)? as u32,
        names: strings(word(2)?, word(3)?)?,
        imports: deps
            .into_iter()
            .map(|path| Import {
                path,
                class: String::new(),
                flags: 0,
            })
            .collect(),
    })
}

/// Which exports a read decodes, and how far.
#[derive(Debug, Clone)]
pub struct Opts {
    /// An export's index or class name; None: every export.
    pub export: Option<Export>,
    /// How deep structs and arrays are decoded; below it a value is its size.
    pub depth: usize,
    /// Values decoded at most; past it the tree stops (`cut`).
    pub max_nodes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Export {
    Index(usize),
    Class(String),
}

/// The whole file as JSON: version, crcs, names, imports, exports, buffers, and each asked export's property tree.
/// Err: not CR2W, or tables that lie outside the file (the caller then reports its first bytes).
pub fn read(d: &[u8], opts: &Opts) -> Result<Value, String> {
    if d.get(..4) == Some(MAGIC) && old(d) {
        let n = old_names(d)?;
        let imports: Vec<Value> = n.imports.iter().map(|i| json!({"path": i.path})).collect();
        return Ok(json!({
            "version": n.version, "version_unknown": true, "names": n.names, "imports": imports, "tree": [],
            "tree_unread": format!("CR2W version {}, an older layout: names and imports only", n.version),
        }));
    }
    let header = d.get(..HEADER).ok_or("shorter than a CR2W header")?;
    let parsed = names(d)?;
    let t = tables(header).ok_or("a short header")?;
    let mut h = header.to_vec();
    h[32..36].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let mut crc = Map::new();
    crc.insert(
        "header".into(),
        json!(crc32fast::hash(&h) == u32_at(d, 32).unwrap_or(0)),
    );
    for (i, name) in TABLES.iter().enumerate().filter(|(i, _)| t[*i].count > 0) {
        let bytes = d.get(t[i].offset..t[i].offset + t[i].count * WIDTHS[i]);
        crc.insert(
            (*name).into(),
            json!(bytes.is_some_and(|b| crc32fast::hash(b) == t[i].crc32)),
        );
    }
    let unchecked: Vec<usize> = (6..10).filter(|&i| t[i].count > 0).collect();
    if !unchecked.is_empty() {
        crc.insert("tables_unchecked".into(), json!(unchecked));
    }
    let names = &parsed.names;
    // [class, flags, parent, size, offset, template, crc32]
    let exports: Vec<[u32; 7]> = (0..t[4].count)
        .map(|k| {
            let at = t[4].offset + 24 * k;
            let word = |j: usize| u32_at(d, at + 4 * j);
            let half = |j: usize| u16_at(d, at + 2 * j).map(u32::from);
            Some([
                half(0)?,
                half(1)?,
                word(1)?,
                word(2)?,
                word(3)?,
                word(4)?,
                word(5)?,
            ])
        })
        .collect::<Option<_>>()
        .ok_or("the export table lies outside the file")?;
    let class_of = |x: &[u32; 7]| names.get(x[0] as usize).cloned().unwrap_or_default();
    let mut bad_crc = Vec::new();
    let mut list = Vec::new();
    let mut tree = Vec::new();
    let mut cx = Cx {
        names,
        imports: &parsed.imports,
        nodes: 0,
        max_nodes: opts.max_nodes,
        depth: opts.depth,
        cut: false,
    };
    for (k, x) in exports.iter().enumerate() {
        let data = d.get(x[4] as usize..x[4] as usize + x[3] as usize);
        if data.is_none_or(|b| crc32fast::hash(b) != x[6]) {
            bad_crc.push(k);
        }
        list.push(
            json!({"index": k, "class": class_of(x), "parent": x[2], "flags": x[1], "bytes": x[3]}),
        );
        let wanted = match &opts.export {
            None => true,
            Some(Export::Index(i)) => *i == k,
            Some(Export::Class(c)) => c.eq_ignore_ascii_case(&class_of(x)),
        };
        if !wanted || cx.cut {
            continue;
        }
        let mut node = Map::from_iter([
            ("index".into(), json!(k)),
            ("class".into(), json!(class_of(x))),
        ]);
        match data {
            None => {
                node.insert("error".into(), json!("its data lies outside the file"));
            }
            Some(data) => cx.export(&class_of(x), data, &mut node),
        }
        tree.push(Value::Object(node));
    }
    crc.insert("exports_bad".into(), json!(bad_crc));
    let buffers: Vec<Value> = (0..t[5].count)
        .filter_map(|k| {
            let at = t[5].offset + 24 * k;
            let f: Vec<u32> = (0..6).map(|j| u32_at(d, at + 4 * j)).collect::<Option<_>>()?;
            Some(json!({"flags": f[0], "index": f[1], "offset": f[2], "bytes": f[3], "memory_bytes": f[4], "crc32": f[5]}))
        })
        .collect();
    let imports: Vec<Value> = parsed
        .imports
        .iter()
        .map(|i| json!({"path": i.path, "class": i.class, "flags": i.flags}))
        .collect();
    Ok(json!({
        "version": parsed.version,
        "version_unknown": !KNOWN_VERSIONS.contains(&parsed.version),
        "flags": u32_at(d, 8),
        "file_size_matches": u32_at(d, 24) == Some(d.len() as u32),
        "crc_ok": crc,
        "names": names,
        "imports": imports,
        "exports": list,
        "buffers": buffers,
        "tree": tree,
        "cut": cx.cut,
    }))
}

/// A non-CR2W file as a read reports it: its size and first bytes.
pub fn not_cr2w(d: &[u8]) -> Value {
    json!({"bytes": d.len(), "first_bytes": hex(&d[..d.len().min(16)])})
}

struct Cx<'a> {
    names: &'a [String],
    imports: &'a [Import],
    nodes: usize,
    max_nodes: usize,
    depth: usize,
    cut: bool,
}

/// Raw bytes as a result shows them: the size, and at most [`RAW_HEX`] of them as hex.
fn raw(v: &[u8]) -> Value {
    json!({"bytes": v.len(), "raw": hex(&v[..v.len().min(RAW_HEX)])})
}

/// An enum's type name: `E` and a capital (`EngineTransform` and `EntityHandle` are structs).
fn is_enum(ty: &str) -> bool {
    let b = ty.as_bytes();
    b.len() > 1 && b[0] == b'E' && b[1].is_ascii_uppercase()
}

/// The bytes a value of `ty` takes when that is fixed.
fn fixed(ty: &str) -> Option<usize> {
    Some(match ty {
        "Bool" | "Int8" | "Uint8" => 1,
        "Int16" | "Uint16" | "CName" => 2,
        "Float" | "Int32" | "Uint32" => 4,
        "Double" | "Int64" | "Uint64" => 8,
        t if t.starts_with("handle:") || t.starts_with("ptr:") => 4,
        t if is_enum(t) => 2,
        _ => return None,
    })
}

/// A CR2W string: a length (6 bits, then 7 per byte while the continue bit is set; the top bit of the first byte says
/// ASCII), then that many ASCII bytes or UTF-16LE units. With the bytes it took.
fn vlq_string(v: &[u8]) -> Option<(String, usize)> {
    let b = *v.first()?;
    let (mut n, mut shift, mut more, mut i) = (usize::from(b & 0x3F), 6, b & 0x40 != 0, 1);
    while more {
        let c = *v.get(i)?;
        i += 1;
        if shift > 27 {
            return None;
        }
        n |= usize::from(c & 0x7F) << shift;
        (shift, more) = (shift + 7, c & 0x80 != 0);
    }
    if b & 0x80 != 0 {
        let s = v.get(i..i + n)?;
        Some((s.iter().map(|&c| char::from(c)).collect(), i + n))
    } else {
        let s = v.get(i..i + 2 * n)?;
        let units: Vec<u16> = s
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some((String::from_utf16_lossy(&units), i + 2 * n))
    }
}

impl Cx<'_> {
    fn name(&self, i: u16) -> Option<&str> {
        self.names.get(usize::from(i)).map(String::as_str)
    }

    /// One export's data into `node`: its properties, a material instance's parameter block, anything after them.
    fn export(&mut self, class: &str, data: &[u8], node: &mut Map<String, Value>) {
        let parsed = (data.first() == Some(&0))
            .then(|| self.props(&data[1..], 1))
            .flatten();
        let Some((props, end)) = parsed else {
            node.insert(
                "props_unread".into(),
                json!("no property list that ends inside the export"),
            );
            node.insert("data".into(), raw(data));
            return;
        };
        node.insert("props".into(), Value::Object(props));
        let mut rest = &data[1 + end..];
        if class == "CMaterialInstance" && !rest.is_empty() {
            match self.params(rest) {
                Some((params, used)) => {
                    node.insert("params".into(), Value::Object(params));
                    rest = &rest[used..];
                }
                None => {
                    node.insert(
                        "params_unread".into(),
                        json!("a parameter block that doesn't fit"),
                    );
                }
            }
        }
        if !rest.is_empty() {
            node.insert("trailing".into(), raw(rest));
        }
    }

    /// A property list from `d`'s start to its u16 0: the properties and the bytes it took. None: not one that ends
    /// inside `d`.
    fn props(&mut self, d: &[u8], depth: usize) -> Option<(Map<String, Value>, usize)> {
        let mut out = Map::new();
        let mut pos = 0;
        loop {
            let ni = u16_at(d, pos)?;
            if ni == 0 {
                return Some((out, pos + 2));
            }
            let (ti, size) = (u16_at(d, pos + 2)?, u32_at(d, pos + 4)? as usize);
            let value = d
                .get(pos + 8..(pos + 4).checked_add(size)?)
                .filter(|_| size >= 4)?;
            let (name, ty) = (self.name(ni)?.to_owned(), self.name(ti)?.to_owned());
            let v = self.value(&ty, value, depth);
            insert(&mut out, name, json!({"type": ty, "value": v}));
            pos += 4 + size;
        }
    }

    /// A material instance's parameters: `u32 count`, then each `u32 size (the whole record), u16 name, u16 type,
    /// value`. With the bytes they took.
    fn params(&mut self, d: &[u8]) -> Option<(Map<String, Value>, usize)> {
        let count = u32_at(d, 0)? as usize;
        let (mut out, mut pos) = (Map::new(), 4);
        for _ in 0..count.min(d.len()) {
            let size = u32_at(d, pos)? as usize;
            let (name, ty) = (
                self.name(u16_at(d, pos + 4)?)?.to_owned(),
                self.name(u16_at(d, pos + 6)?)?.to_owned(),
            );
            let value = d
                .get(pos + 8..pos.checked_add(size)?)
                .filter(|_| size >= 8)?;
            let v = self.value(&ty, value, 1);
            insert(&mut out, name, json!({"type": ty, "value": v}));
            pos += size;
        }
        Some((out, pos))
    }

    fn value(&mut self, ty: &str, v: &[u8], depth: usize) -> Value {
        self.nodes += 1;
        if self.nodes > self.max_nodes {
            self.cut = true;
            return json!({"bytes": v.len(), "cut": "max_nodes"});
        }
        // Little-endian, zero-extended to 8 bytes.
        let mut b = [0u8; 8];
        b[..v.len().min(8)].copy_from_slice(&v[..v.len().min(8)]);
        let float = |f: f64| {
            if f.is_finite() {
                json!(f)
            } else {
                json!(f.to_string())
            }
        };
        let decoded = match (ty, fixed(ty) == Some(v.len())) {
            ("Float", true) => Some(float(f32::from_le_bytes([b[0], b[1], b[2], b[3]]).into())),
            ("Double", true) => Some(float(f64::from_le_bytes(b))),
            ("Bool", true) => Some(json!(v[0] != 0)),
            ("Uint8" | "Uint16" | "Uint32" | "Uint64", true) => Some(json!(u64::from_le_bytes(b))),
            ("Int8" | "Int16" | "Int32" | "Int64", true) => {
                let shift = 64 - 8 * v.len() as u32;
                Some(json!((i64::from_le_bytes(b) << shift) >> shift))
            }
            ("CName", true) => u16_at(v, 0).and_then(|i| self.name(i)).map(|n| json!(n)),
            (t, true) if is_enum(t) => u16_at(v, 0).and_then(|i| self.name(i)).map(|n| json!(n)),
            (t, true) if t.starts_with("handle:") || t.starts_with("ptr:") => {
                let h = i32::from_le_bytes(v.try_into().unwrap_or_default());
                Some(match h {
                    0 => Value::Null,
                    h if h < 0 => {
                        let i = (-(i64::from(h)) - 1) as usize;
                        self.imports.get(i).map_or(
                            json!({"import_index": i}),
                            |imp| json!({"import": imp.path}),
                        )
                    }
                    h => json!({"export": h - 1}),
                })
            }
            ("String", _) => vlq_string(v)
                .filter(|(_, n)| *n == v.len())
                .map(|(s, _)| json!(s)),
            (t, _) if t.starts_with("array:") => self.array(t, v, depth),
            _ if depth >= self.depth => Some(json!({"bytes": v.len(), "cut": "depth"})),
            // A struct: a zero byte, then a property list that ends exactly at the value's end.
            _ => (v.first() == Some(&0))
                .then(|| self.props(&v[1..], depth + 1))
                .flatten()
                .filter(|(_, end)| 1 + end == v.len())
                .map(|(props, _)| Value::Object(props)),
        };
        decoded.unwrap_or_else(|| raw(v))
    }

    /// `array:<dims>,<flags>,<element type>`: a u32 count, then the elements. When they can't be read exactly to the
    /// value's end, the count and the raw remainder.
    fn array(&mut self, ty: &str, v: &[u8], depth: usize) -> Option<Value> {
        let element = ty.splitn(3, ',').nth(2)?;
        let count = u32_at(v, 0)? as usize;
        if depth >= self.depth {
            return Some(json!({"count": count, "bytes": v.len(), "cut": "depth"}));
        }
        Some(self.elements(element, count, v, depth).map_or_else(
            || {
                let mut r = raw(&v[4..]);
                r["count"] = json!(count);
                r
            },
            Value::Array,
        ))
    }

    /// An array's elements after its count: packed when their size is fixed, one string each, or one struct each (a
    /// zero byte and a property list). None when they don't fill the value exactly.
    fn elements(
        &mut self,
        element: &str,
        count: usize,
        v: &[u8],
        depth: usize,
    ) -> Option<Vec<Value>> {
        if count > v.len() {
            return None;
        }
        let mut items = Vec::with_capacity(count);
        let mut pos = 4;
        for _ in 0..count {
            let rest = v.get(pos..)?;
            let n = match fixed(element) {
                Some(w) => w,
                None if element == "String" => vlq_string(rest)?.1,
                None if element.starts_with("array:") => return None,
                None => {
                    let (props, end) = (rest.first() == Some(&0))
                        .then(|| self.props(&rest[1..], depth + 1))
                        .flatten()?;
                    items.push(Value::Object(props));
                    pos += 1 + end;
                    continue;
                }
            };
            items.push(self.value(element, rest.get(..n)?, depth + 1));
            pos += n;
        }
        (pos == v.len()).then_some(items)
    }
}

/// `value` under `name`; a name already there gets `#2`, `#3`, ...
fn insert(map: &mut Map<String, Value>, name: String, value: Value) {
    let mut key = name.clone();
    let mut n = 1;
    while map.contains_key(&key) {
        n += 1;
        key = format!("{name}#{n}");
    }
    map.insert(key, value);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A test CR2W writer: names (index 0 is ""), imports (path, class name), exports (class name, data), the version.
    /// Every crc is set as the game sets it.
    pub(crate) fn file(
        version: u32,
        names: &[&str],
        imports: &[(&str, &str)],
        exports: &[(&str, Vec<u8>)],
    ) -> Vec<u8> {
        let mut strings = Vec::new();
        let at = |s: &str, strings: &mut Vec<u8>| {
            let at = strings.len() as u32;
            strings.extend(s.as_bytes());
            strings.push(0);
            at
        };
        let name_rows: Vec<u8> = names
            .iter()
            .flat_map(|n| [at(n, &mut strings).to_le_bytes(), [0; 4]].concat())
            .collect();
        let idx = |n: &str| names.iter().position(|x| *x == n).expect("a listed name") as u16;
        let import_rows: Vec<u8> = imports
            .iter()
            .flat_map(|(p, c)| {
                let mut r = at(p, &mut strings).to_le_bytes().to_vec();
                r.extend(idx(c).to_le_bytes());
                r.extend(0u16.to_le_bytes());
                r
            })
            .collect();
        let mut tables = [(0usize, 0usize, 0u32); 10];
        let mut body = Vec::new();
        let mut place = |i: usize, bytes: &[u8], count: usize, body: &mut Vec<u8>| {
            tables[i] = (HEADER + body.len(), count, crc32fast::hash(bytes));
            body.extend(bytes);
        };
        place(0, &strings.clone(), strings.len(), &mut body);
        place(1, &name_rows, names.len(), &mut body);
        place(2, &import_rows, imports.len(), &mut body);
        let data_at = HEADER + body.len() + 24 * exports.len();
        let mut rows = Vec::new();
        let mut data: Vec<u8> = Vec::new();
        for (class, d) in exports {
            let mut r = idx(class).to_le_bytes().to_vec();
            r.extend(0u16.to_le_bytes());
            for v in [
                0,
                d.len() as u32,
                (data_at + data.len()) as u32,
                0,
                crc32fast::hash(d),
            ] {
                r.extend(v.to_le_bytes());
            }
            rows.extend(r);
            data.extend(d);
        }
        place(4, &rows, exports.len(), &mut body);
        body.extend(data);
        let mut out = MAGIC.to_vec();
        let len = (HEADER + body.len()) as u32;
        for v in [version, 0, 0, 0, 0, len, len, 0xDEAD_BEEF, 0] {
            out.extend(v.to_le_bytes());
        }
        for (o, c, crc) in tables {
            for v in [o as u32, c as u32, crc] {
                out.extend(v.to_le_bytes());
            }
        }
        let crc = crc32fast::hash(&out);
        out[32..36].copy_from_slice(&crc.to_le_bytes());
        out.extend(body);
        out
    }

    /// One property: name, type, value bytes.
    pub(crate) fn prop(names: &[&str], name: &str, ty: &str, value: &[u8]) -> Vec<u8> {
        let idx = |n: &str| names.iter().position(|x| *x == n).expect("a listed name") as u16;
        let mut p = idx(name).to_le_bytes().to_vec();
        p.extend(idx(ty).to_le_bytes());
        p.extend((value.len() as u32 + 4).to_le_bytes());
        p.extend(value);
        p
    }

    /// A struct or export body: the zero byte, the properties, the u16 0.
    pub(crate) fn body(props: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0];
        b.extend(props.concat());
        b.extend([0, 0]);
        b
    }

    pub(crate) const NAMES: [&str; 22] = [
        "",
        "CMaterialInstance",
        "baseMaterial",
        "handle:IMaterial",
        "Float",
        "Roughness_max",
        "AOPower",
        "CName",
        "kind",
        "Thing",
        "array:2,0,SPoint",
        "points",
        "SPoint",
        "x",
        "String",
        "label",
        "Mystery",
        "odd",
        "after",
        "Uint32",
        "CMesh",
        "array:2,0,Float",
    ];

    pub(crate) fn sample() -> Vec<u8> {
        let n = &NAMES;
        let point = |x: f32| body(&[prop(n, "x", "Float", &x.to_le_bytes())]);
        let mut points = 2u32.to_le_bytes().to_vec();
        points.extend(point(1.5));
        points.extend(point(-2.0));
        let mut ascii = vec![0x80 | 5];
        ascii.extend(b"hello");
        let utf16: Vec<u8> = [2u8]
            .into_iter()
            .chain("é!".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let mesh = body(&[
            prop(
                n,
                "baseMaterial",
                "handle:IMaterial",
                &(-1i32).to_le_bytes(),
            ),
            prop(n, "kind", "CName", &8u16.to_le_bytes()),
            prop(n, "points", "array:2,0,SPoint", &points),
            prop(n, "label", "String", &ascii),
            prop(n, "label", "String", &utf16),
            prop(n, "odd", "Mystery", &[1, 2, 3]),
            prop(n, "after", "Uint32", &7u32.to_le_bytes()),
        ]);
        let mut instance = body(&[prop(
            n,
            "baseMaterial",
            "handle:IMaterial",
            &1i32.to_le_bytes(),
        )]);
        instance.extend(2u32.to_le_bytes());
        for (name, v) in [("Roughness_max", 1.0f32), ("AOPower", 0.5)] {
            instance.extend(12u32.to_le_bytes());
            instance.extend((n.iter().position(|x| *x == name).unwrap() as u16).to_le_bytes());
            instance.extend(4u16.to_le_bytes());
            instance.extend(v.to_le_bytes());
        }
        file(
            164,
            n,
            &[("base\\graph.w2mg", "CMesh")],
            &[("CMesh", mesh), ("CMaterialInstance", instance)],
        )
    }

    fn opts() -> Opts {
        Opts {
            export: None,
            depth: 32,
            max_nodes: 100_000,
        }
    }

    /// The fixture decodes: crcs checked, handles to an import and an export, a CName, an array of structs, ASCII and
    /// UTF-16 strings (a repeated name gets #2), an unknown type raw with the next property still read, and the
    /// material block.
    #[test]
    fn the_sample_decodes() {
        let d = sample();
        let v = read(&d, &opts()).unwrap();
        assert_eq!(v["version"], 164);
        assert_eq!(v["version_unknown"], false);
        assert_eq!(v["file_size_matches"], true);
        let crc = &v["crc_ok"];
        for t in ["header", "strings", "names", "imports", "exports"] {
            assert_eq!(crc[t], true, "{t}: {crc}");
        }
        assert_eq!(crc["exports_bad"], json!([]));
        assert_eq!(
            v["imports"][0],
            json!({"path": "base\\graph.w2mg", "class": "CMesh", "flags": 0})
        );
        let mesh = &v["tree"][0]["props"];
        assert_eq!(
            mesh["baseMaterial"]["value"],
            json!({"import": "base\\graph.w2mg"})
        );
        assert_eq!(mesh["kind"]["value"], "kind");
        assert_eq!(
            mesh["points"]["value"],
            json!([{"x": {"type": "Float", "value": 1.5}}, {"x": {"type": "Float", "value": -2.0}}])
        );
        assert_eq!(mesh["label"]["value"], "hello");
        assert_eq!(mesh["label#2"]["value"], "é!");
        assert_eq!(mesh["odd"]["value"], json!({"bytes": 3, "raw": "010203"}));
        assert_eq!(mesh["after"]["value"], 7);
        let inst = &v["tree"][1];
        assert_eq!(inst["props"]["baseMaterial"]["value"], json!({"export": 0}));
        assert_eq!(inst["params"]["Roughness_max"]["value"], 1.0);
        assert_eq!(inst["params"]["AOPower"]["value"], 0.5);
        assert!(inst.get("trailing").is_none(), "{inst}");
    }

    /// A broken crc, another version, a picked export, the node cap and the depth are reported, never refused; a file
    /// that isn't CR2W is an error the caller turns into its first bytes.
    #[test]
    fn mismatches_and_limits_are_reported() {
        let mut d = sample();
        let last = d.len() - 1;
        d[last] ^= 0xFF;
        d[4] = 163;
        let v = read(&d, &opts()).unwrap();
        assert_eq!(v["version"], 163);
        assert_eq!(v["version_unknown"], true);
        assert_eq!(v["crc_ok"]["header"], false);
        assert_eq!(v["crc_ok"]["exports_bad"], json!([1]));
        let pick = |e| {
            read(
                &sample(),
                &Opts {
                    export: Some(e),
                    ..opts()
                },
            )
            .unwrap()["tree"]
                .as_array()
                .unwrap()
                .len()
        };
        assert_eq!(pick(Export::Class("cmaterialinstance".into())), 1);
        assert_eq!(pick(Export::Index(7)), 0);
        let cut = read(
            &sample(),
            &Opts {
                max_nodes: 2,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(cut["cut"], true);
        let shallow = read(&sample(), &Opts { depth: 1, ..opts() }).unwrap();
        assert_eq!(
            shallow["tree"][0]["props"]["points"]["value"]["cut"],
            "depth"
        );
        assert!(read(b"RIFF....", &opts()).is_err());
        assert_eq!(not_cr2w(b"RIFF1234")["first_bytes"], "5249464631323334");
    }

    /// The older header: names and dependencies as strings, the first dependency implicit; read
    /// whole, its exports not decoded.
    #[test]
    fn the_older_layout_gives_names_and_imports() {
        let s = |t: &str| [&[0x80 | t.len() as u8][..], t.as_bytes()].concat();
        let table = [s("CEntityTemplate"), s("includes")].concat();
        let deps = s(r"gameplay\pc_steering.w2steer");
        let (n0, d0) = (44u32, 44 + table.len() as u32);
        let mut d = MAGIC.to_vec();
        for w in [141, 0, n0, 2, d0 + deps.len() as u32, 0, d0, 0, d0, 2] {
            d.extend(w.to_le_bytes());
        }
        d.extend([table, deps].concat());
        assert_eq!(names_end(&d[..HEADER.min(d.len())]), Some(usize::MAX));
        let n = names(&d).unwrap();
        assert_eq!((n.version, n.names.len()), (141, 2));
        assert_eq!(n.imports[0].path, r"gameplay\pc_steering.w2steer");
        let v = read(&d, &opts()).unwrap();
        assert_eq!(
            (v["version_unknown"].clone(), v["tree"].clone()),
            (json!(true), json!([]))
        );
        assert!(v["tree_unread"].as_str().unwrap().contains("141"));
        assert!(names(&d[..d.len() - 3]).is_err());
    }

    /// A names-level read needs only the header and the first three tables, and finds the names and imports.
    #[test]
    fn names_come_from_a_prefix() {
        let d = sample();
        let end = names_end(&d[..HEADER]).unwrap();
        assert!(end < d.len());
        let n = names(&d[..end]).unwrap();
        assert_eq!(n.version, 164);
        assert!(n.names.iter().any(|x| x == "CMaterialInstance"));
        assert_eq!(n.imports[0].path, "base\\graph.w2mg");
        assert!(names(&d[..end - 1]).is_err());
        assert_eq!(names_end(b"CR2X"), None);
    }
}
