//! An Nsight Graphics frame capture read from its own file (`.ngfx-capture`, Nsight 2026.3.1; headless research §3):
//! the container of chunks, the root record, the object table and the shaders stored as chunks. Each table is checked
//! against ngfx-replay's own list and refused as "layout not recognised" when it doesn't line up, so another Nsight
//! version fails loudly instead of decoding wrong. Read-only: a `Read + Seek` over the file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};

use serde::Deserialize;

use crate::ngfx::{self, Shader, ShaderScan, u32_at, u64_at};

/// What every refusal of a file that doesn't line up starts with.
pub const LAYOUT: &str = "layout not recognised";
/// The largest chunk read (a 4K screenshot is 33 MB): a bigger size is a misread.
const CHUNK_MAX: u64 = 1 << 30;
/// How much of an object's creation record its table row shows.
const RECORD_SHOWN: usize = 2048;

fn layout(what: impl std::fmt::Display) -> String {
    format!("{LAYOUT}: {what}")
}

/// A chunk's entry in the footer index.
#[derive(Debug, Clone, Copy)]
struct Entry {
    compressed: bool,
    /// What the file stores: the LZ4 block's size, or the bytes themselves.
    stored: u64,
    size: u64,
    /// Of the chunk's 48-byte header.
    offset: u64,
}

/// Bytes in a chunk: an object's record, a call's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ref {
    pub chunk: u64,
    pub offset: u64,
    pub len: u64,
}

impl Ref {
    fn at(b: &[u8], at: usize) -> Option<Ref> {
        Some(Ref {
            chunk: u64_at(b, at)?,
            offset: u64_at(b, at + 8)?,
            len: u64_at(b, at + 16)?,
        })
    }
}

/// An open capture: its chunk index, and the chunks loaded so far.
pub struct Capture<R> {
    file: R,
    index: BTreeMap<u64, Entry>,
    loaded: HashMap<u64, Vec<u8>>,
}

impl<R: Read + Seek> Capture<R> {
    /// The container's index. The file is `nlyp`, then chunks, each after a 48-byte header (`elif`, u32 version 1,
    /// u32 kind, u32 1 = an LZ4 block or 0 = stored, u64 stored size (0 when stored), u64 size, u64 id, u64 the
    /// header's own offset), then a footer repeating every header, and last `enod`, u32, u64 the count, u64 the
    /// footer's offset.
    pub fn open(mut file: R) -> Result<Capture<R>, String> {
        let io = |e: io::Error| format!("reading the capture: {e}");
        let len = file.seek(SeekFrom::End(0)).map_err(io)?;
        let not = || layout("not an .ngfx-capture container");
        if len < 28 {
            return Err(not());
        }
        let mut capture = Capture {
            file,
            index: BTreeMap::new(),
            loaded: HashMap::new(),
        };
        if capture.read_at(0, 4)? != b"nlyp" {
            return Err(not());
        }
        let tail = capture.read_at(len - 24, 24)?;
        let (count, footer) = (u64_at(&tail, 8).unwrap(), u64_at(&tail, 16).unwrap());
        if &tail[..4] != b"enod"
            || count.checked_mul(48).and_then(|n| n.checked_add(footer)) != Some(len - 24)
        {
            return Err(layout("the container's footer"));
        }
        let index = capture.read_at(footer, (count * 48) as usize)?;
        for h in index.chunks_exact(48) {
            let field = |at| u64_at(h, at).unwrap();
            let (version, compression) = (u32_at(h, 4).unwrap(), u32_at(h, 12).unwrap());
            let (compressed, size, id, offset) =
                (compression == 1, field(24), field(32), field(40));
            let stored = if compressed { field(16) } else { size };
            let fits = stored
                .checked_add(48)
                .and_then(|n| n.checked_add(offset))
                .is_some_and(|end| end <= footer);
            if &h[..4] != b"elif" || version != 1 || compression > 1 || !fits || size > CHUNK_MAX {
                return Err(layout(format!("chunk {id}'s index entry")));
            }
            let entry = Entry {
                compressed,
                stored,
                size,
                offset,
            };
            if capture.index.insert(id, entry).is_some() {
                return Err(layout(format!("chunk {id} twice in the index")));
            }
        }
        Ok(capture)
    }

    fn read_at(&mut self, at: u64, n: usize) -> Result<Vec<u8>, String> {
        let mut b = vec![0; n];
        self.file
            .seek(SeekFrom::Start(at))
            .and_then(|_| self.file.read_exact(&mut b))
            .map_err(|e| format!("reading the capture at {at}: {e}"))?;
        Ok(b)
    }

    fn entry(&self, id: u64) -> Result<Entry, String> {
        self.index
            .get(&id)
            .copied()
            .ok_or_else(|| layout(format!("no chunk {id}")))
    }

    /// Chunk `id`, decompressed, after its header in place agrees with the index.
    pub fn chunk(&mut self, id: u64) -> Result<Vec<u8>, String> {
        let e = self.entry(id)?;
        let h = self.read_at(e.offset, 48)?;
        if &h[..4] != b"elif" || u64_at(&h, 32) != Some(id) || u64_at(&h, 40) != Some(e.offset) {
            return Err(layout(format!("chunk {id}'s header")));
        }
        let stored = self.read_at(e.offset + 48, e.stored as usize)?;
        if !e.compressed {
            return Ok(stored);
        }
        ngfx::lz4_block(&stored, 0, e.size as usize)
            .filter(|b| b.len() as u64 == e.size)
            .ok_or_else(|| layout(format!("chunk {id} doesn't decode")))
    }

    /// The chunks `refs` name read into memory once, for [`Capture::bytes`].
    pub fn load(&mut self, refs: impl IntoIterator<Item = Ref>) -> Result<(), String> {
        for r in refs {
            if r.len > 0 && !self.loaded.contains_key(&r.chunk) {
                let b = self.chunk(r.chunk)?;
                self.loaded.insert(r.chunk, b);
            }
        }
        Ok(())
    }

    /// The bytes `r` names, in a chunk [`Capture::load`] read; None outside it.
    pub fn bytes(&self, r: Ref) -> Option<&[u8]> {
        if r.len == 0 {
            return Some(&[]);
        }
        let end = r.offset.checked_add(r.len)?;
        self.loaded
            .get(&r.chunk)?
            .get(r.offset as usize..end as usize)
    }

    /// The root record (chunk 1, a protobuf message): the object table's chunk (field 2) and the call table's
    /// (field 3). Its other fields are skipped unread: field 6 is the metadata, the game's environment included.
    pub fn root(&mut self) -> Result<Root, String> {
        let b = self.chunk(1)?;
        let bad = || layout("the root record");
        let (mut objects, mut calls, mut p) = (None, None, 0);
        while p < b.len() {
            let key = varint(&b, &mut p).ok_or_else(bad)?;
            let skip = match key & 7 {
                0 => {
                    let v = varint(&b, &mut p).ok_or_else(bad)?;
                    match key >> 3 {
                        2 => objects = Some(v),
                        3 => calls = Some(v),
                        _ => {}
                    }
                    0
                }
                1 => 8,
                2 => varint(&b, &mut p).ok_or_else(bad)?,
                5 => 4,
                _ => return Err(bad()),
            };
            p = usize::try_from(skip)
                .ok()
                .and_then(|n| p.checked_add(n))
                .filter(|&end| end <= b.len())
                .ok_or_else(bad)?;
        }
        Ok(Root {
            objects: objects.ok_or_else(bad)?,
            calls: calls.ok_or_else(bad)?,
        })
    }

    /// The object table ([`objects`]) the root names.
    pub fn objects(&mut self) -> Result<Vec<Object>, String> {
        let root = self.root()?;
        objects(&self.chunk(root.objects)?)
    }

    /// Every chunk that holds a DXBC container: stored as it is (its first bytes `DXBC`) or as an LZ4 block (the
    /// container's header is the block's first literal run, so `DXBC` is in its first 8 bytes). Unique by hash, each
    /// with the chunks that hold it.
    pub fn shaders(&mut self) -> Result<ShaderScan, String> {
        let mut scan = ShaderScan::default();
        let mut by_hash: HashMap<String, usize> = HashMap::new();
        let candidates: Vec<(u64, Entry)> = self
            .index
            .iter()
            .filter(|(_, e)| (33..ngfx::SHADER_MAX as u64).contains(&e.size) && e.stored >= 8)
            .map(|(id, e)| (*id, *e))
            .collect();
        for (id, e) in candidates {
            let head = self.read_at(e.offset + 48, 8)?;
            let dxbc = match e.compressed {
                true => head.windows(4).any(|w| w == b"DXBC"),
                false => head.starts_with(b"DXBC"),
            };
            if !dxbc {
                continue;
            }
            scan.found += 1;
            match self.chunk(id).ok().and_then(Shader::decode) {
                None => scan.failed += 1,
                Some(s) => match by_hash.get(&s.hash) {
                    Some(&k) => {
                        scan.duplicates += 1;
                        scan.shaders[k].chunks.push(id);
                    }
                    None => {
                        by_hash.insert(s.hash.clone(), scan.shaders.len());
                        scan.shaders.push(Shader {
                            chunks: vec![id],
                            ..s
                        });
                    }
                },
            }
        }
        Ok(scan)
    }
}

/// A protobuf varint at `b[*p..]`.
fn varint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let x = *b.get(*p)?;
        *p += 1;
        v |= u64::from(x & 0x7f) << shift;
        if x < 0x80 {
            return Some(v);
        }
    }
    None
}

/// What the root record names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// The object table's chunk.
    pub objects: u64,
    /// The call table's chunk.
    pub calls: u64,
}

/// One row of the object table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object {
    pub uid: u64,
    /// 3 D3D12, 4 DXGI, 6 NGX (the codes map one to one onto `--metadata-objects`' type names).
    pub api: u32,
    pub kind: u32,
    /// The object's creation record.
    pub record: Ref,
    /// Its state.
    pub state: Ref,
}

/// The object table: 72-byte rows `u64 uid, u64 0, u32 api, u32 type`, then `{u64 chunk, u64 offset, u64 length}`
/// for the object's creation record and the same for its state.
pub fn objects(table: &[u8]) -> Result<Vec<Object>, String> {
    if !table.len().is_multiple_of(72) {
        return Err(layout("the object table's size"));
    }
    Ok(table
        .chunks_exact(72)
        .map(|r| Object {
            uid: u64_at(r, 0).unwrap(),
            api: u32_at(r, 16).unwrap(),
            kind: u32_at(r, 20).unwrap(),
            record: Ref::at(r, 24).unwrap(),
            state: Ref::at(r, 48).unwrap(),
        })
        .collect())
}

/// One object of `--metadata-objects`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Listed {
    pub uid: u64,
    pub type_name: String,
    pub object_name: String,
    #[serde(default)]
    pub creator_function: Option<String>,
}

/// `--metadata-objects`' list.
pub fn listed(text: &str) -> Result<Vec<Listed>, String> {
    ngfx::first(ngfx::json_start(text, '[')?)
}

/// The object table against `--metadata-objects`: the same objects (count and uids), each (api, type) code one type
/// name and each name one code. The listed objects, by uid.
pub fn check_objects<'a>(
    objects: &[Object],
    listed: &'a [Listed],
) -> Result<HashMap<u64, &'a Listed>, String> {
    let by: HashMap<u64, &Listed> = listed.iter().map(|o| (o.uid, o)).collect();
    let uids: HashSet<u64> = objects.iter().map(|o| o.uid).collect();
    if uids.len() != objects.len() || objects.len() != by.len() || by.len() != listed.len() {
        return Err(layout(format!(
            "the object table holds {} objects, --metadata-objects {}",
            objects.len(),
            listed.len()
        )));
    }
    let (mut name_of, mut code_of) = (HashMap::new(), HashMap::new());
    for o in objects {
        let l = by
            .get(&o.uid)
            .ok_or_else(|| layout(format!("object {} isn't in --metadata-objects", o.uid)))?;
        let code = (o.api, o.kind);
        let name = l.type_name.as_str();
        if *name_of.entry(code).or_insert(name) != name
            || *code_of.entry(name).or_insert(code) != code
        {
            return Err(layout(format!(
                "type code {}/{} and type {name} don't map one to one",
                o.api, o.kind
            )));
        }
    }
    Ok(by)
}

/// `uid` as a table shows it: its listed name, with `#uid` unless the name already ends with `_<uid>`; `-` for 0.
pub fn object_name(names: &HashMap<u64, &Listed>, uid: u64) -> String {
    match names.get(&uid) {
        _ if uid == 0 => "-".into(),
        Some(l) if l.object_name.ends_with(&format!("_{uid}")) => l.object_name.clone(),
        Some(l) => format!("{}#{uid}", l.object_name),
        None => format!("#{uid}"),
    }
}

/// `b` as a table cell: hex, with runs of 4 or more printable ASCII characters as `"text"` and of UTF-16 ones as
/// `u"text"`, so a name or a path in it reads as text (and quen can scrub it); at most `max` bytes, then `...+<n>`.
pub fn raw(b: &[u8], max: usize) -> String {
    let shown = &b[..b.len().min(max)];
    let printable = |x: &u8| (0x20..0x7f).contains(x);
    let (mut tokens, mut hex, mut i) = (Vec::new(), String::new(), 0);
    while i < shown.len() {
        let ascii = shown[i..].iter().take_while(|x| printable(x)).count();
        let wide = shown[i..]
            .chunks_exact(2)
            .take_while(|c| printable(&c[0]) && c[1] == 0)
            .count();
        let text = if ascii >= 4 {
            let text = String::from_utf8_lossy(&shown[i..i + ascii]);
            Some((format!("\"{text}\""), ascii))
        } else if wide >= 4 {
            let text: String = shown[i..i + 2 * wide]
                .iter()
                .step_by(2)
                .map(|&x| x as char)
                .collect();
            Some((format!("u\"{text}\""), 2 * wide))
        } else {
            None
        };
        match text {
            Some((text, n)) => {
                if !hex.is_empty() {
                    tokens.push(std::mem::take(&mut hex));
                }
                tokens.push(text);
                i += n;
            }
            None => {
                hex += &format!("{:02x}", shown[i]);
                i += 1;
            }
        }
    }
    if !hex.is_empty() {
        tokens.push(hex);
    }
    if b.len() > shown.len() {
        tokens.push(format!("...+{}", b.len() - shown.len()));
    }
    tokens.join(" ")
}

/// The object table as `object_table.tsv`, by uid: the listed name, type and creator, the file's codes, where the
/// creation record and the state are (`chunk:offset:bytes`), and the record's first bytes ([`raw`]).
pub fn object_table<R: Read + Seek>(
    capture: &mut Capture<R>,
    objects: &[Object],
    names: &HashMap<u64, &Listed>,
) -> Result<String, String> {
    capture.load(objects.iter().map(|o| o.record))?;
    let mut rows: Vec<&Object> = objects.iter().collect();
    rows.sort_by_key(|o| o.uid);
    let mut out =
        String::from("uid\tname\ttype\tapi\ttype_code\tcreator\trecord\tstate\trecord_bytes\n");
    let at = |r: Ref| format!("{}:{}:{}", r.chunk, r.offset, r.len);
    for o in rows {
        let l = names
            .get(&o.uid)
            .ok_or_else(|| layout(format!("object {} isn't in --metadata-objects", o.uid)))?;
        let record = capture
            .bytes(o.record)
            .ok_or_else(|| layout(format!("object {}'s creation record", o.uid)))?;
        out += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            o.uid,
            l.object_name,
            l.type_name,
            o.api,
            o.kind,
            l.creator_function.as_deref().unwrap_or("-"),
            at(o.record),
            at(o.state),
            raw(record, RECORD_SHOWN)
        );
    }
    Ok(out)
}

/// What [`pipelines`] joined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Joined {
    /// `pipelines.tsv`.
    pub tsv: String,
    /// PipelineStates and StateObjects, those that name a shader, and the shaders named in all.
    pub pipelines: usize,
    pub with_shaders: usize,
    pub links: usize,
}

/// Each pipeline's shaders (headless research §3.4): a PipelineState's or StateObject's creation record names each of
/// its shaders by its chunk's id, a u64, so every u64 in the record (at any offset) that is the id of a chunk holding a
/// shader is one of its shaders. `pipelines.tsv` by uid, in the record's order: the pipeline's uid, name and type, and
/// each shader's stage, model, chunk, hash (its file `shaders\<hash>.dxbc`) and bytes; `-` for a pipeline that names
/// none.
pub fn pipelines<R: Read + Seek>(
    capture: &mut Capture<R>,
    objects: &[Object],
    names: &HashMap<u64, &Listed>,
    shaders: &ShaderScan,
) -> Result<Joined, String> {
    let by_chunk: HashMap<u64, &Shader> = shaders
        .shaders
        .iter()
        .flat_map(|s| s.chunks.iter().map(move |c| (*c, s)))
        .collect();
    let mut rows: Vec<(&Object, &Listed)> = objects
        .iter()
        .filter_map(|o| Some((o, *names.get(&o.uid)?)))
        .filter(|(_, l)| matches!(l.type_name.as_str(), "PipelineState" | "StateObject"))
        .collect();
    rows.sort_by_key(|(o, _)| o.uid);
    capture.load(rows.iter().map(|(o, _)| o.record))?;
    let mut joined = Joined {
        tsv: "uid\tname\ttype\tstage\tmodel\tchunk\thash\tbytes\n".into(),
        pipelines: rows.len(),
        ..Joined::default()
    };
    for (o, l) in rows {
        let record = capture
            .bytes(o.record)
            .ok_or_else(|| layout(format!("object {}'s creation record", o.uid)))?;
        let mut named: Vec<u64> = Vec::new();
        for k in 0..record.len().saturating_sub(7) {
            let id = u64_at(record, k).unwrap();
            if by_chunk.contains_key(&id) && !named.contains(&id) {
                named.push(id);
            }
        }
        let head = format!("{}\t{}\t{}", o.uid, l.object_name, l.type_name);
        if named.is_empty() {
            joined.tsv += &format!("{head}\t-\t-\t-\t-\t-\n");
        }
        joined.with_shaders += usize::from(!named.is_empty());
        joined.links += named.len();
        for id in named {
            let s = by_chunk[&id];
            joined.tsv += &format!(
                "{head}\t{}\t{}\t{id}\t{}\t{}\n",
                s.stage,
                s.model,
                s.hash,
                s.bytes.len()
            );
        }
    }
    Ok(joined)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ngfx::tests::container as dxbc;
    use std::io::Cursor;

    /// `b` as an LZ4 block of literals only.
    pub(crate) fn lz4(b: &[u8]) -> Vec<u8> {
        let mut out = vec![(b.len().min(15) as u8) << 4];
        if b.len() >= 15 {
            let mut n = b.len() - 15;
            while n >= 255 {
                out.push(255);
                n -= 255;
            }
            out.push(n as u8);
        }
        out.extend(b);
        out
    }

    /// A synthetic capture: `nlyp`, each chunk (id, bytes, compressed?) after its header, the footer, the trailer.
    pub(crate) fn file(chunks: &[(u64, Vec<u8>, bool)]) -> Vec<u8> {
        let mut out = b"nlyp".to_vec();
        let mut index: Vec<u8> = Vec::new();
        for (id, data, compressed) in chunks {
            let stored = if *compressed { lz4(data) } else { data.clone() };
            let mut h = b"elif".to_vec();
            h.extend(1u32.to_le_bytes());
            h.extend(0u32.to_le_bytes());
            h.extend(u32::from(*compressed).to_le_bytes());
            h.extend((if *compressed { stored.len() as u64 } else { 0 }).to_le_bytes());
            h.extend((data.len() as u64).to_le_bytes());
            h.extend(id.to_le_bytes());
            h.extend((out.len() as u64).to_le_bytes());
            index.extend(&h);
            out.extend(h);
            out.extend(stored);
            out.extend([0u8; 3]); // padding, as Nsight leaves between chunks
        }
        let footer = out.len() as u64;
        out.extend(index);
        out.extend(b"enod");
        out.extend(41u32.to_le_bytes());
        out.extend((chunks.len() as u64).to_le_bytes());
        out.extend(footer.to_le_bytes());
        out
    }

    /// A protobuf varint.
    pub(crate) fn pb(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let x = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(x);
                return out;
            }
            out.push(x | 0x80);
        }
    }

    /// A root record naming the object table's chunk and the call table's, with a metadata field 6 between.
    pub(crate) fn root(objects: u64, calls: u64) -> Vec<u8> {
        let mut out = [pb(1 << 3), pb(4_194_304), pb(2 << 3), pb(objects)].concat();
        out.extend(pb(6 << 3 | 2));
        out.extend(pb(9));
        out.extend(b"USERNAME=");
        out.extend([pb(3 << 3), pb(calls), pb(7 << 3 | 5), vec![1, 2, 3, 4]].concat());
        out
    }

    /// An object table row.
    pub(crate) fn object(uid: u64, api: u32, kind: u32, record: Ref) -> Vec<u8> {
        let mut out = uid.to_le_bytes().to_vec();
        out.extend(0u64.to_le_bytes());
        out.extend(api.to_le_bytes());
        out.extend(kind.to_le_bytes());
        for r in [record, Ref::default()] {
            for v in [r.chunk, r.offset, r.len] {
                out.extend(v.to_le_bytes());
            }
        }
        out
    }

    pub(crate) fn listed_json(rows: &[(u64, &str, &str)]) -> String {
        let rows: Vec<_> = rows
            .iter()
            .map(|(uid, t, name)| serde_json::json!({"uid": uid, "type_name": t, "object_name": name, "api": "D3D12"}))
            .collect();
        format!("\u{feff}{}", serde_json::Value::from(rows))
    }

    #[test]
    fn the_container_root_and_object_table_read_and_check() {
        let record = b"\x01\x02\x03\x04C:\\Games\\x\x00\x00N\x00a\x00m\x00e\x00\xff".to_vec();
        let table = [
            object(
                311,
                3,
                9,
                Ref {
                    chunk: 3,
                    offset: 2,
                    len: record.len() as u64 - 2,
                },
            ),
            object(
                20,
                3,
                16,
                Ref {
                    chunk: 3,
                    offset: 0,
                    len: 4,
                },
            ),
        ]
        .concat();
        let f = file(&[
            (1, root(2, 9), true),
            (2, table.clone(), false),
            (3, record, true),
        ]);
        let mut c = Capture::open(Cursor::new(f.clone())).unwrap();
        assert_eq!(
            c.root().unwrap(),
            Root {
                objects: 2,
                calls: 9
            }
        );
        let objs = c.objects().unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!((objs[0].uid, objs[0].api, objs[0].kind), (311, 3, 9));

        let list = listed(&listed_json(&[
            (20, "Device", "Device_20"),
            (311, "PipelineState", "Fast"),
        ]))
        .unwrap();
        let names = check_objects(&objs, &list).unwrap();
        assert_eq!(object_name(&names, 20), "Device_20");
        assert_eq!(object_name(&names, 311), "Fast#311");
        assert_eq!(
            (object_name(&names, 0), object_name(&names, 7)),
            ("-".into(), "#7".into())
        );
        let tsv = object_table(&mut c, &objs, &names).unwrap();
        let lines: Vec<&str> = tsv.lines().collect();
        assert_eq!(lines.len(), 3, "{tsv}");
        assert!(
            lines[1].starts_with("20\tDevice_20\tDevice\t3\t16\t-\t3:0:4\t0:0:0\t01020304"),
            "{tsv}"
        );
        assert!(
            lines[2].ends_with("\t0304 \"C:\\Games\\x\" 0000 u\"Name\" ff"),
            "{tsv}"
        );

        // The listed objects don't line up: a missing uid, another count, a code with two names.
        let other = listed(&listed_json(&[
            (20, "Device", "D"),
            (312, "PipelineState", "P"),
        ]))
        .unwrap();
        assert!(
            check_objects(&objs, &other)
                .unwrap_err()
                .starts_with(LAYOUT)
        );
        assert!(
            check_objects(&objs, &list[..1])
                .unwrap_err()
                .starts_with(LAYOUT)
        );
        let two = listed(&listed_json(&[(20, "Device", "D"), (311, "Device", "P")])).unwrap();
        assert!(
            check_objects(&objs, &two)
                .unwrap_err()
                .contains("one to one")
        );

        // Not a container; a header in place that disagrees with the index; a footer that doesn't add up.
        assert!(Capture::open(Cursor::new(b"nope, not a capture at all".to_vec())).is_err());
        let mut moved = f.clone();
        let at = 4 + 32; // chunk 1's id in its header
        moved[at] = 5;
        let mut c = Capture::open(Cursor::new(moved)).unwrap();
        assert!(c.chunk(1).unwrap_err().contains("header"));
        let mut short = f;
        short.truncate(short.len() - 1);
        assert!(Capture::open(Cursor::new(short)).is_err());
    }

    #[test]
    fn shaders_are_found_by_chunk_and_dedupe() {
        let (a, b) = (dxbc(1, 64), dxbc(2, 4000));
        let mut fake = b"DXBC".to_vec();
        fake.extend([0u8; 60]);
        let f = file(&[
            (1, root(2, 3), true),
            (7, a.clone(), true),
            (8, b.clone(), false),
            (9, fake, true),
            (10, a.clone(), false),
            (11, vec![0x5a; 500], true),
        ]);
        let scan = Capture::open(Cursor::new(f)).unwrap().shaders().unwrap();
        assert_eq!(
            (scan.found, scan.failed, scan.duplicates),
            (4, 1, 1),
            "{scan:?}"
        );
        assert_eq!(scan.shaders.len(), 2);
        assert_eq!(
            (
                scan.shaders[0].chunks.clone(),
                scan.shaders[0].bytes.clone()
            ),
            (vec![7, 10], a)
        );
        assert_eq!(
            (scan.shaders[1].stage, scan.shaders[1].chunks.clone()),
            ("compute", vec![8])
        );
    }

    #[test]
    fn pipelines_name_their_shaders_by_chunk() {
        let (a, b) = (dxbc(1, 64), dxbc(2, 100));
        // PipelineState 311: chunk 8 at an odd offset, then 7 twice; StateObject 400: none; a resource naming 7.
        let mut pso = vec![0xfa, 3];
        for id in [8u64, 7, 7] {
            pso.extend(id.to_le_bytes());
            pso.push(0);
        }
        let records = [pso.clone(), vec![0; 16], 7u64.to_le_bytes().to_vec()].concat();
        let r = |offset: usize, len: usize| Ref {
            chunk: 3,
            offset: offset as u64,
            len: len as u64,
        };
        let n = pso.len();
        let table = [
            object(400, 3, 24, r(n, 16)),
            object(311, 3, 9, r(0, n)),
            object(5, 3, 6, r(n + 16, 8)),
        ]
        .concat();
        let f = file(&[
            (1, root(2, 9), true),
            (2, table, false),
            (3, records, true),
            (7, a, true),
            (8, b, false),
        ]);
        let mut c = Capture::open(Cursor::new(f)).unwrap();
        let objs = c.objects().unwrap();
        let list = listed(&listed_json(&[
            (5, "Resource", "Resource_5"),
            (311, "PipelineState", "PipelineState_311"),
            (400, "StateObject", "Rt"),
        ]))
        .unwrap();
        let names = check_objects(&objs, &list).unwrap();
        let scan = c.shaders().unwrap();
        let j = pipelines(&mut c, &objs, &names, &scan).unwrap();
        assert_eq!(
            (j.pipelines, j.with_shaders, j.links),
            (2, 1, 2),
            "{}",
            j.tsv
        );
        let lines: Vec<&str> = j.tsv.lines().skip(1).collect();
        let (h1, h2) = ("01".repeat(16), "02".repeat(16));
        assert_eq!(
            lines,
            [
                format!("311\tPipelineState_311\tPipelineState\tcompute\t5_1\t8\t{h2}\t148"),
                format!("311\tPipelineState_311\tPipelineState\tcompute\t5_1\t7\t{h1}\t112"),
                "400\tRt\tStateObject\t-\t-\t-\t-\t-".into(),
            ]
        );
    }

    /// A hand check on a real capture, read-only: `QUEN_NGFX_CAPTURE` (the file), `QUEN_NGFX_FUNCTIONS` and
    /// `QUEN_NGFX_OBJECTS` (ngfx-replay's `--metadata-functions` and `--metadata-objects` of it), `QUEN_NGFX_OUT` (a
    /// folder outside the repo for the tables). Prints counts only. `cargo test -p quen-core --lib hand_check --
    /// --ignored --nocapture`
    #[test]
    #[ignore]
    fn hand_check() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let out = std::path::PathBuf::from(var("QUEN_NGFX_OUT"));
        let file = std::fs::File::open(var("QUEN_NGFX_CAPTURE")).unwrap();
        let mut c = Capture::open(io::BufReader::new(file)).unwrap();
        let objs = c.objects().unwrap();
        let text = std::fs::read_to_string(var("QUEN_NGFX_OBJECTS")).unwrap();
        let list = listed(&text).unwrap();
        let names = check_objects(&objs, &list).unwrap();
        println!(
            "chunks {} objects {} (listed {})",
            c.index.len(),
            objs.len(),
            list.len()
        );
        let table = object_table(&mut c, &objs, &names).unwrap();
        println!("object_table rows {}", table.lines().count() - 1);
        std::fs::write(out.join("object_table.tsv"), table).unwrap();
        let scan = c.shaders().unwrap();
        println!(
            "shaders found {} decoded {} failed {} duplicates {}",
            scan.found,
            scan.shaders.len(),
            scan.failed,
            scan.duplicates
        );
        let j = pipelines(&mut c, &objs, &names, &scan).unwrap();
        println!(
            "pipelines {} with shaders {} links {}",
            j.pipelines, j.with_shaders, j.links
        );
        std::fs::write(out.join("pipelines.tsv"), j.tsv).unwrap();
    }

    #[test]
    fn raw_bytes_show_their_text() {
        assert_eq!(raw(b"\x01\x02abc\x00", 64), "010261626300");
        assert_eq!(raw(b"\x01path\x02", 64), "01 \"path\" 02");
        assert_eq!(raw(b"N\0a\0m\0e\0\x07", 64), "u\"Name\" 07");
        assert_eq!(raw(&[0xab; 10], 4), "abababab ...+6");
    }
}
