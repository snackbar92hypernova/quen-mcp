//! An Nsight Graphics capture's calls read from its own file (headless research §3.5): the call table's device and
//! queue calls, the command lists' commands recorded in it, both lined up event for event with `--metadata-functions`,
//! and the arguments of the functions that matter decoded (struct layouts from Microsoft's public `d3d12.h`). Each
//! call's arguments are its C parameters in order: plain values as they are, objects as uids, a pointer as Nsight's tag
//! `0xFA0000xx` and then what it points to. A frame that doesn't line up is refused as "layout not recognised".

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Seek};

use serde::Deserialize;

use crate::ngfx::{self, u32_at, u64_at};
use crate::ngfx_capture::{Capture, Listed, Object, Ref, layout, object_name, raw};

/// How much of an undecoded call's arguments its row shows.
const RAW_SHOWN: usize = 4096;
/// Nsight's own first event, which has no row in the call table.
const CAPTURE_BEGIN: &str = "CaptureBegin";
const RECORDING: &str = "CommandRecording";
const SUBMIT: &str = "ID3D12CommandQueue_ExecuteCommandLists";
const RESET: &str = "ID3D12GraphicsCommandList_Reset";
const LIST: &str = "ID3D12GraphicsCommandList_";
/// What ends each command in a recording.
const END: [u8; 4] = 0x9999_AAAAu32.to_le_bytes();

/// One row of the call table: a device or queue call, in sequence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub thread: u32,
    /// 1 Nsight's own events, 3 D3D12, 4 DXGI, 5 NvAPI, 6 NGX.
    pub api: u32,
    pub code: u32,
    /// The call's CPU time during the capture, µs.
    pub start_us: u32,
    pub end_us: u32,
    pub this: u64,
    pub args: Ref,
}

/// The call table: 72-byte rows, row k sequence id k: `u64 sequence id, 12 bytes, u32 thread, u32 api, u32 function,
/// u32 CPU start and end (µs), u64 this, {u64 chunk, u64 offset, u64 length}` of its arguments.
pub fn records(table: &[u8]) -> Result<Vec<Record>, String> {
    if !table.len().is_multiple_of(72) {
        return Err(layout("the call table's size"));
    }
    table
        .chunks_exact(72)
        .enumerate()
        .map(|(k, r)| {
            let row = Record {
                seq: u64_at(r, 0).unwrap(),
                thread: u32_at(r, 20).unwrap(),
                api: u32_at(r, 24).unwrap(),
                code: u32_at(r, 28).unwrap(),
                start_us: u32_at(r, 32).unwrap(),
                end_us: u32_at(r, 36).unwrap(),
                this: u64_at(r, 40).unwrap(),
                args: Ref::at(r, 48).unwrap(),
            };
            match row.seq == k as u64 {
                true => Ok(row),
                false => Err(layout(format!("call table row {k}'s sequence id"))),
            }
        })
        .collect()
}

/// One event of `--metadata-functions`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Event {
    pub event_index: u64,
    pub function_name: String,
    pub thread_index: u32,
    /// A device or queue call's (a row of the call table); none for a command list's command.
    #[serde(default)]
    pub sequence_id: Option<u64>,
    /// One of ExecuteIndirect's commands.
    #[serde(default)]
    pub indirect_index: Option<u64>,
}

/// `--metadata-functions`' list, every event at its index.
pub fn events(text: &str) -> Result<Vec<Event>, String> {
    let events: Vec<Event> = ngfx::first(ngfx::json_start(text, '[')?)?;
    match events
        .iter()
        .enumerate()
        .all(|(k, e)| e.event_index == k as u64)
    {
        true => Ok(events),
        false => Err(layout("--metadata-functions' event indices")),
    }
}

/// A recorded command's bytes in its recording's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Command {
    api: u32,
    code: u32,
    start: usize,
    end: usize,
}

/// The commands a `CommandRecording` holds: u64 count, then per command u32 api, u32 function, its arguments, the end
/// marker `0x9999AAAA`.
fn commands(payload: &[u8]) -> Option<Vec<Command>> {
    let n = u64_at(payload, 0)?;
    let (mut out, mut p) = (Vec::new(), 8);
    while (out.len() as u64) < n {
        let (api, code, start) = (u32_at(payload, p)?, u32_at(payload, p + 4)?, p + 8);
        // ponytail: the first end marker ends the arguments; an argument holding those 4 bytes misparses the
        // recording, which the count and length checks then refuse. Per-function argument sizes if that ever bites.
        let end = start + payload.get(start..)?.windows(4).position(|w| w == END)?;
        out.push(Command {
            api,
            code,
            start,
            end,
        });
        p = end + 4;
    }
    (p == payload.len()).then_some(out)
}

/// Where an event of `--metadata-functions` is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Nsight's own `CaptureBegin`, with no row.
    Nsight,
    /// A row of the call table.
    Call(usize),
    /// A command of command list `list`, in the `CommandRecording` row `recording`; `pipeline` the one set then.
    Command {
        list: u64,
        recording: usize,
        start: usize,
        end: usize,
        pipeline: u64,
    },
    /// One of ExecuteIndirect's commands, read by the GPU from a buffer.
    Indirect { list: u64, pipeline: u64 },
}

/// The frame's calls lined up with `--metadata-functions`.
pub struct Frame {
    pub records: Vec<Record>,
    /// Per event.
    pub sources: Vec<Source>,
}

/// Function codes against names, both ways: each code one name, each name one code.
#[derive(Default)]
struct Names<'a> {
    name: HashMap<(u32, u32), &'a str>,
    code: HashMap<&'a str, (u32, u32)>,
}

impl<'a> Names<'a> {
    fn fits(&self, code: (u32, u32), name: &str) -> bool {
        self.name.get(&code).is_none_or(|n| *n == name)
            && self.code.get(name).is_none_or(|c| *c == code)
    }

    fn add(&mut self, code: (u32, u32), name: &'a str, what: &str) -> Result<(), String> {
        if !self.fits(code, name) {
            return Err(layout(format!(
                "{what} function code {}/{} and {name} don't map one to one",
                code.0, code.1
            )));
        }
        self.name.insert(code, name);
        self.code.insert(name, code);
        Ok(())
    }
}

/// A submit's commands, in order: (list, recording row, command, the list's first?, its initial pipeline).
type Block = VecDeque<(u64, usize, Command, bool, u64)>;

/// The frame's calls lined up with `--metadata-functions` (`events`):
/// - each row of the call table is the event of its sequence id, on the same thread (Nsight's own `CaptureBegin`
///   aside);
/// - each `ExecuteCommandLists` submits its lists' commands, each list's latest recording before it (or, for a list
///   recorded before the frame began, its last recording in the frame), as one block;
/// - the events without a sequence id are those blocks' commands, in order. A queue's blocks come in submit order; a
///   block starts where the first queue's next block (by submit) whose commands' names fit the events that follow;
/// - each function code means one name and back, for calls and commands each;
/// - ExecuteIndirect's own commands follow it.
///
/// Anything else is refused as "layout not recognised".
pub fn frame<R: Read + Seek>(capture: &mut Capture<R>, events: &[Event]) -> Result<Frame, String> {
    let root = capture.root()?;
    let records = records(&capture.chunk(root.calls)?)?;
    capture.load(records.iter().map(|r| r.args))?;
    let mut by_seq: Vec<Option<usize>> = vec![None; records.len()];
    for (i, e) in events.iter().enumerate() {
        let Some(seq) = e.sequence_id.filter(|_| e.function_name != CAPTURE_BEGIN) else {
            continue;
        };
        let slot = by_seq.get_mut(seq as usize).ok_or_else(|| {
            layout(format!(
                "event {i}'s sequence id {seq}: the call table has {}",
                records.len()
            ))
        })?;
        if slot.replace(i).is_some() {
            return Err(layout(format!(
                "sequence id {seq} twice in --metadata-functions"
            )));
        }
    }
    let mut calls = Names::default();
    let mut name = Vec::with_capacity(records.len());
    for (r, rec) in records.iter().enumerate() {
        let e = by_seq[r]
            .map(|i| &events[i])
            .ok_or_else(|| layout(format!("call table row {r} has no event")))?;
        if e.thread_index != rec.thread {
            return Err(layout(format!("call table row {r}'s thread")));
        }
        calls.add((rec.api, rec.code), &e.function_name, "call")?;
        name.push(e.function_name.as_str());
    }
    let payload = |r: usize| capture.bytes(records[r].args).unwrap_or_default();
    // Each recording's commands and its list's initial pipeline (the Reset before it), and each list's last one.
    let (mut parsed, mut initial, mut last) = (HashMap::new(), HashMap::new(), HashMap::new());
    let mut reset = HashMap::new();
    let called = events
        .iter()
        .filter(|e| e.function_name != CAPTURE_BEGIN)
        .filter_map(|e| e.sequence_id);
    for r in called.map(|seq| seq as usize) {
        let rec = records[r];
        match name[r] {
            RESET => {
                let pso =
                    reset_pso(payload(r)).ok_or_else(|| layout(format!("call {r}'s Reset")))?;
                reset.insert(rec.this, pso);
            }
            RECORDING => {
                let cmds =
                    commands(payload(r)).ok_or_else(|| layout(format!("call {r}'s recording")))?;
                parsed.insert(r, cmds);
                initial.insert(r, reset.get(&rec.this).copied().unwrap_or(0));
                last.insert(rec.this, r);
            }
            _ => {}
        }
    }
    // The events without a sequence id that aren't ExecuteIndirect's own, in order: the blocks' commands.
    let listed: Vec<usize> = (0..events.len())
        .filter(|&i| events[i].sequence_id.is_none() && events[i].indirect_index.is_none())
        .collect();
    let mut commands_named = Names::default();
    let fits = |block: &Block, from: usize, known: &Names| {
        let mut local = Names::default();
        block.iter().enumerate().all(|(k, (_, _, c, ..))| {
            let Some(&i) = listed.get(from + k) else {
                return false;
            };
            let n = events[i].function_name.as_str();
            known.fits((c.api, c.code), n) && local.add((c.api, c.code), n, "").is_ok()
        })
    };
    // Each list's latest recording; each queue's submits not listed yet, (row, block).
    let mut latest: HashMap<u64, usize> = HashMap::new();
    let mut pending: BTreeMap<u64, VecDeque<(usize, Block)>> = BTreeMap::new();
    let (mut current, mut at, mut list_now, mut pipeline) = (Block::new(), 0, 0, 0);
    let mut sources = Vec::with_capacity(events.len());
    for (i, e) in events.iter().enumerate() {
        if e.function_name == CAPTURE_BEGIN {
            sources.push(Source::Nsight);
            continue;
        }
        if let Some(seq) = e.sequence_id {
            let r = seq as usize;
            let rec = records[r];
            match name[r] {
                RECORDING => {
                    latest.insert(rec.this, r);
                }
                SUBMIT => {
                    let lists = submitted(payload(r))
                        .ok_or_else(|| layout(format!("call {r}'s submit")))?;
                    let mut block = Block::new();
                    for list in lists {
                        let rr =
                            *latest
                                .get(&list)
                                .or_else(|| last.get(&list))
                                .ok_or_else(|| {
                                    layout(format!("list {list} is submitted but never recorded"))
                                })?;
                        for (k, c) in parsed[&rr].iter().enumerate() {
                            block.push_back((list, rr, *c, k == 0, initial[&rr]));
                        }
                    }
                    if !block.is_empty() {
                        pending.entry(rec.this).or_default().push_back((r, block));
                    }
                }
                _ => {}
            }
            sources.push(Source::Call(r));
            continue;
        }
        if e.indirect_index.is_some() {
            sources.push(Source::Indirect {
                list: list_now,
                pipeline,
            });
            continue;
        }
        if current.is_empty() {
            let queue = pending
                .iter()
                .filter_map(|(q, d)| Some((*q, d.front()?)))
                .filter(|(_, (_, b))| fits(b, at, &commands_named))
                .min_by_key(|(_, (r, _))| *r)
                .map(|(q, _)| q)
                .ok_or_else(|| {
                    layout(format!(
                        "event {i}: no submitted list's commands start there"
                    ))
                })?;
            current = pending
                .get_mut(&queue)
                .and_then(VecDeque::pop_front)
                .unwrap()
                .1;
        }
        let (list, recording, c, first, initial) = current.pop_front().unwrap();
        commands_named.add((c.api, c.code), &e.function_name, "command")?;
        if first {
            pipeline = initial;
        }
        if matches!(
            e.function_name.strip_prefix(LIST),
            Some("SetPipelineState" | "SetPipelineState1")
        ) {
            pipeline = u64_at(&payload(recording)[c.start..c.end], 8).unwrap_or(0);
        }
        list_now = list;
        sources.push(Source::Command {
            list,
            recording,
            start: c.start,
            end: c.end,
            pipeline,
        });
        at += 1;
    }
    if !current.is_empty() || pending.values().any(|d| !d.is_empty()) {
        return Err(layout(
            "submitted commands that --metadata-functions doesn't list",
        ));
    }
    Ok(Frame { records, sources })
}

/// A cursor over a call's arguments.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A pointer argument: the tag `0xFA0000xx`, then for xx 02 nothing (null: empty), 04 one element, 44 one after
    /// its u32 size, 08 a u8 count and the elements, 48 a u8 count and each element after its u32 size (checked).
    fn ptr<T>(&mut self, mut one: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let tag = self.u32()?;
        if tag >> 8 != 0x00FA_0000 {
            return None;
        }
        // ponytail: a count is one byte in every pointer seen (Nsight 2026.3.1); a larger array's form is unknown and
        // fails the decode, which the row then shows raw.
        let (count, sized) = match tag & 0xff {
            0x02 => return Some(Vec::new()),
            0x04 => (1, false),
            0x44 => (1, true),
            0x08 => (usize::from(self.u8()?), false),
            0x48 => (usize::from(self.u8()?), true),
            _ => return None,
        };
        (0..count)
            .map(|_| {
                let size = if sized {
                    Some(self.u32()? as usize)
                } else {
                    None
                };
                let at = self.p;
                let v = one(self)?;
                size.is_none_or(|s| self.p - at == s).then_some(v)
            })
            .collect()
    }

    fn done(&self) -> bool {
        self.p == self.b.len()
    }
}

/// A Reset's initial pipeline: u32 result, u64 allocator, u64 pipeline.
fn reset_pso(b: &[u8]) -> Option<u64> {
    let mut c = Cur { b, p: 0 };
    let (_, _, pso) = (c.u32()?, c.u64()?, c.u64()?);
    c.done().then_some(pso)
}

/// The lists an ExecuteCommandLists submits: u32 count, a pointer to their uids.
fn submitted(b: &[u8]) -> Option<Vec<u64>> {
    let mut c = Cur { b, p: 0 };
    let n = c.u32()?;
    let lists = c.ptr(Cur::u64)?;
    (c.done() && lists.len() == n as usize).then_some(lists)
}

/// A GPU address.
fn va(x: u64) -> String {
    format!("0x{x:x}")
}

/// A descriptor handle: u64 address, u32 heap type.
fn handle(c: &mut Cur) -> Option<(u64, u32)> {
    Some((c.u64()?, c.u32()?))
}

/// A resource barrier: u32 type, u32 flags, then by type: transition (u64 resource, u32 subresource, u32 before, u32
/// after), aliasing (u64 before, u64 after), UAV (u64 resource).
fn barrier(c: &mut Cur, n: &dyn Fn(u64) -> String) -> Option<String> {
    let (kind, flags) = (c.u32()?, c.u32()?);
    let flags = if flags == 0 {
        String::new()
    } else {
        format!(" flags=0x{flags:x}")
    };
    Some(match kind {
        0 => {
            let (res, sub, before, after) = (c.u64()?, c.u32()?, c.u32()?, c.u32()?);
            format!(
                "transition {} sub={sub} 0x{before:x}->0x{after:x}{flags}",
                n(res)
            )
        }
        1 => format!("aliasing {}->{}{flags}", n(c.u64()?), n(c.u64()?)),
        2 => format!("uav {}{flags}", n(c.u64()?)),
        _ => return None,
    })
}

/// A view an OMSetRenderTargets binds (its trailer): u32 kind (5 render target, 6 depth stencil), u64 resource, u32,
/// u32 format, u32 dimension, then that dimension's union member (D3D12_RTV_DIMENSION, D3D12_DSV_DIMENSION).
fn view(c: &mut Cur, n: &dyn Fn(u64) -> String) -> Option<String> {
    let (kind, res, _, format, dim) = (c.u32()?, c.u64()?, c.u32()?, c.u32()?, c.u32()?);
    let union = match (kind, dim) {
        (5, 2 | 6) | (6, 1 | 3 | 5) => 4,
        (5, 4 | 7) | (6, 6) => 8,
        (5, 1 | 3 | 8) | (6, 2 | 4) => 12,
        (5, 5) => 16,
        _ => return None,
    };
    c.take(union)?;
    Some(format!("{} format={format} dim={dim}", n(res)))
}

/// A texture copy location: u64 resource, u32 type, then a subresource index (0) or a placed footprint (1: u64
/// offset, u32 format, width, height, depth, row pitch).
fn location(c: &mut Cur, n: &dyn Fn(u64) -> String) -> Option<String> {
    let (res, kind) = (c.u64()?, c.u32()?);
    Some(match kind {
        0 => format!("{} sub={}", n(res), c.u32()?),
        1 => {
            let (offset, format) = (c.u64()?, c.u32()?);
            let (w, h, d, pitch) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
            format!(
                "{} offset=0x{offset:x} format={format} size={w}x{h}x{d} pitch={pitch}",
                n(res)
            )
        }
        _ => return None,
    })
}

/// One geometry of a bottom-level build.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    Triangles {
        flags: u32,
        transform: u64,
        index_format: u32,
        vertex_format: u32,
        index_count: u32,
        vertex_count: u32,
        index_buffer: u64,
        vertex_buffer: u64,
        vertex_stride: u64,
    },
    Aabbs {
        flags: u32,
        count: u64,
        start: u64,
        stride: u64,
    },
}

/// A BuildRaytracingAccelerationStructure's description.
#[derive(Debug, Clone, PartialEq)]
pub struct Build {
    pub dest: u64,
    pub top: bool,
    pub flags: u32,
    pub count: u32,
    /// A top-level build's instance array (a GPU address).
    pub instances: u64,
    pub geometry: Vec<Geometry>,
    pub source: u64,
    pub scratch: u64,
    pub postbuild: u32,
}

/// D3D12_RAYTRACING_GEOMETRY_DESC: u32 type, u32 flags, then triangles (u64 transform, u32 index format, u32 vertex
/// format, u32 index count, u32 vertex count, u64 index buffer, u64 vertex buffer, u64 stride) or AABBs (u64 count,
/// u64 start, u64 stride).
fn geometry(c: &mut Cur) -> Option<Geometry> {
    let (kind, flags) = (c.u32()?, c.u32()?);
    Some(match kind {
        0 => Geometry::Triangles {
            flags,
            transform: c.u64()?,
            index_format: c.u32()?,
            vertex_format: c.u32()?,
            index_count: c.u32()?,
            vertex_count: c.u32()?,
            index_buffer: c.u64()?,
            vertex_buffer: c.u64()?,
            vertex_stride: c.u64()?,
        },
        1 => Geometry::Aabbs {
            flags,
            count: c.u64()?,
            start: c.u64()?,
            stride: c.u64()?,
        },
        _ => return None,
    })
}

/// A build command's arguments after its leading u64 `lead`: a pointer to D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE
/// _DESC (u64 dest, the inputs: u32 type, u32 flags, u32 count, u32 layout 0 = array, then a top level's u64 instance
/// address or a bottom level's pointer to its geometry; u64 source, u64 scratch), u32 postbuild count, a pointer to
/// them (u64 address, u32 type); then, when `lead` isn't 0, Nsight's own trailer: u64, u32, three pointers (20-byte
/// and 24-byte entries, and one always null).
fn build(c: &mut Cur, lead: u64) -> Option<Build> {
    let mut b = c
        .ptr(|c| {
            let dest = c.u64()?;
            let (kind, flags, count, layout) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
            if kind > 1 || layout != 0 {
                return None;
            }
            let (instances, geometry) = match kind {
                0 => (c.u64()?, Vec::new()),
                _ => (0, c.ptr(geometry)?),
            };
            if kind == 1 && geometry.len() != count as usize {
                return None;
            }
            Some(Build {
                dest,
                top: kind == 0,
                flags,
                count,
                instances,
                geometry,
                source: c.u64()?,
                scratch: c.u64()?,
                postbuild: 0,
            })
        })?
        .pop()?;
    b.postbuild = c.u32()?;
    let post = c.ptr(|c| Some((c.u64()?, c.u32()?)))?;
    if post.len() != b.postbuild as usize {
        return None;
    }
    if lead != 0 {
        c.u64()?;
        c.u32()?;
        c.ptr(|c| c.take(20))?;
        c.ptr(|c| c.take(24))?;
        c.ptr(|_| None::<()>)?;
    }
    Some(b)
}

impl Build {
    fn args(&self) -> String {
        let mut s = format!(
            "dest={} {} flags=0x{:x} count={}",
            va(self.dest),
            if self.top { "top" } else { "bottom" },
            self.flags,
            self.count
        );
        if self.top {
            s += &format!(" instances={}", va(self.instances));
        }
        for g in &self.geometry {
            s += &match *g {
                Geometry::Triangles {
                    flags,
                    transform,
                    index_format,
                    vertex_format,
                    index_count,
                    vertex_count,
                    index_buffer,
                    vertex_buffer,
                    vertex_stride,
                } => format!(
                    " [triangles flags=0x{flags:x} transform={} index={}x{index_count} format={index_format} \
                     vertex={}x{vertex_count} format={vertex_format} stride={vertex_stride}]",
                    va(transform),
                    va(index_buffer),
                    va(vertex_buffer)
                ),
                Geometry::Aabbs {
                    flags,
                    count,
                    start,
                    stride,
                } => format!(
                    " [aabbs flags=0x{flags:x} {}x{count} stride={stride}]",
                    va(start)
                ),
            };
        }
        s + &format!(
            " source={} scratch={} postbuild={}",
            va(self.source),
            va(self.scratch),
            self.postbuild
        )
    }
}

/// A command's build, if `b` is a BuildRaytracingAccelerationStructure command's arguments that decode whole.
fn decode_build(b: &[u8]) -> Option<Build> {
    let mut c = Cur { b, p: 0 };
    let lead = c.u64()?;
    build(&mut c, lead).filter(|_| c.done())
}

/// The arguments of `name` (a command's when `command`: after its leading u64) as `key=value` text: None when quen
/// decodes no arguments of that function, Some(None) when its decoder doesn't fit them whole.
fn decode(
    name: &str,
    b: &[u8],
    command: bool,
    n: &dyn Fn(u64) -> String,
) -> Option<Option<String>> {
    let mut c = Cur { b, p: 0 };
    let lead = match command {
        true => c.u64(),
        false => Some(0),
    };
    let method = match command {
        true => name.strip_prefix(LIST)?,
        false => name,
    };
    let s: Option<String> = match (command, method) {
        (true, "DrawInstanced") => (|| {
            let (v, i, sv, si) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
            Some(format!(
                "vertices={v} instances={i} start_vertex={sv} start_instance={si}"
            ))
        })(),
        (true, "DrawIndexedInstanced") => (|| {
            let (x, i, si, bv, sn) = (c.u32()?, c.u32()?, c.u32()?, c.i32()?, c.u32()?);
            Some(format!(
                "indices={x} instances={i} start_index={si} base_vertex={bv} start_instance={sn}"
            ))
        })(),
        (true, "Dispatch") => (|| Some(format!("groups={}x{}x{}", c.u32()?, c.u32()?, c.u32()?)))(),
        (true, "ExecuteIndirect") => (|| {
            let (sig, max, buf, off, count, count_off) =
                (c.u64()?, c.u32()?, c.u64()?, c.u64()?, c.u64()?, c.u64()?);
            c.take(20)?; // Nsight's own trailer
            Some(format!(
                "signature={} max={max} args={}+0x{off:x} count={}+0x{count_off:x}",
                n(sig),
                n(buf),
                n(count)
            ))
        })(),
        (true, "SetPipelineState") => (|| Some(format!("pso={}", n(c.u64()?))))(),
        (true, "SetPipelineState1") => (|| Some(format!("state_object={}", n(c.u64()?))))(),
        (true, "SetComputeRootSignature" | "SetGraphicsRootSignature") => {
            (|| Some(format!("root_signature={}", n(c.u64()?))))()
        }
        (true, "SetComputeRootDescriptorTable" | "SetGraphicsRootDescriptorTable") => (|| {
            let (param, (table, heap)) = (c.u32()?, handle(&mut c)?);
            Some(format!("param={param} table={} heap={heap}", va(table)))
        })(),
        (
            true,
            "SetComputeRootConstantBufferView"
            | "SetGraphicsRootConstantBufferView"
            | "SetComputeRootShaderResourceView"
            | "SetGraphicsRootShaderResourceView"
            | "SetComputeRootUnorderedAccessView"
            | "SetGraphicsRootUnorderedAccessView",
        ) => (|| Some(format!("param={} va={}", c.u32()?, va(c.u64()?))))(),
        (true, "SetComputeRoot32BitConstant" | "SetGraphicsRoot32BitConstant") => (|| {
            let (param, value, offset) = (c.u32()?, c.u32()?, c.u32()?);
            Some(format!("param={param} value=0x{value:x} offset={offset}"))
        })(),
        (true, "ResourceBarrier") => (|| {
            let count = c.u32()?;
            let all = c.ptr(|c| barrier(c, n))?;
            (all.len() == count as usize).then(|| format!("[{}]", all.join(", ")))
        })(),
        (true, "OMSetRenderTargets") => (|| {
            let count = c.u32()?;
            let rts = c.ptr(handle)?;
            let single = c.u8()?;
            let dsv = c.ptr(handle)?;
            // With one handle to a descriptor range (single), the pointer holds that one handle.
            if rts.len() != if single != 0 { count.min(1) } else { count } as usize {
                return None;
            }
            let (rtv, dsv) = match lead? {
                0 => (
                    rts.iter().map(|(h, _)| va(*h)).collect::<Vec<_>>(),
                    dsv.iter().map(|(h, _)| va(*h)).collect::<Vec<_>>(),
                ),
                // Nsight's trailer: the views bound, render targets then depth stencil.
                _ => (c.ptr(|c| view(c, n))?, c.ptr(|c| view(c, n))?),
            };
            Some(format!(
                "rts=[{}] single={single} dsv=[{}]",
                rtv.join(", "),
                dsv.join(", ")
            ))
        })(),
        (true, "CopyBufferRegion") => (|| {
            let (dst, dst_off, src, src_off, bytes) =
                (c.u64()?, c.u64()?, c.u64()?, c.u64()?, c.u64()?);
            Some(format!(
                "dst={}+0x{dst_off:x} src={}+0x{src_off:x} bytes={bytes}",
                n(dst),
                n(src)
            ))
        })(),
        (true, "CopyResource") => (|| Some(format!("dst={} src={}", n(c.u64()?), n(c.u64()?))))(),
        (true, "CopyTextureRegion") => (|| {
            let dst = c.ptr(|c| location(c, n))?.pop()?;
            let (x, y, z) = (c.u32()?, c.u32()?, c.u32()?);
            let src = c.ptr(|c| location(c, n))?.pop()?;
            let boxed = c.ptr(|c| {
                Some(format!(
                    "{},{},{}-{},{},{}",
                    c.u32()?,
                    c.u32()?,
                    c.u32()?,
                    c.u32()?,
                    c.u32()?,
                    c.u32()?
                ))
            })?;
            Some(format!(
                "dst=[{dst}] at={x},{y},{z} src=[{src}] box=[{}]",
                boxed.join("")
            ))
        })(),
        (true, "BuildRaytracingAccelerationStructure") => (|| Some(build(&mut c, lead?)?.args()))(),
        (true, "DispatchRays") => (|| {
            let d = c
                .ptr(|c| {
                    let (rg, rg_size) = (c.u64()?, c.u64()?);
                    let mut tables = Vec::new();
                    for _ in 0..3 {
                        tables.push(format!("{}+{}/{}", va(c.u64()?), c.u64()?, c.u64()?));
                    }
                    let (w, h, d) = (c.u32()?, c.u32()?, c.u32()?);
                    Some(format!(
                        "raygen={}+{rg_size} miss={} hit={} callable={} size={w}x{h}x{d}",
                        va(rg),
                        tables[0],
                        tables[1],
                        tables[2]
                    ))
                })?
                .pop()?;
            c.take(24)?; // Nsight's own trailer
            Some(d)
        })(),
        (false, SUBMIT) => (|| {
            let lists: Vec<String> = submitted(b)?.into_iter().map(n).collect();
            c.p = b.len();
            Some(format!("lists=[{}]", lists.join(", ")))
        })(),
        (false, "ID3D12CommandQueue_Signal" | "ID3D12CommandQueue_Wait") => (|| {
            let (hr, fence, value) = (c.u32()?, c.u64()?, c.u64()?);
            Some(format!("fence={} value={value} hr=0x{hr:x}", n(fence)))
        })(),
        (false, RESET) => (|| {
            let (hr, allocator, pso) = (c.u32()?, c.u64()?, c.u64()?);
            Some(format!(
                "allocator={} pso={} hr=0x{hr:x}",
                n(allocator),
                n(pso)
            ))
        })(),
        (false, RECORDING) => (|| {
            let cmds = commands(b)?;
            c.p = b.len();
            Some(format!("commands={}", cmds.len()))
        })(),
        (false, "MemoryUpdate") => (|| {
            let (hr, res, count) = (c.u32()?, c.u64()?, c.u32()?);
            let ranges = c.ptr(|c| {
                let (_, offset, size, chunk, at) =
                    (c.u32()?, c.u64()?, c.u64()?, c.u64()?, c.u64()?);
                Some(format!("0x{offset:x}+{size} chunk {chunk}@{at}"))
            })?;
            (ranges.len() == count as usize && hr == 0)
                .then(|| format!("resource={} ranges=[{}]", n(res), ranges.join(", ")))
        })(),
        (false, "ID3D12Resource_GetGPUVirtualAddress") => {
            (|| Some(format!("va={}", va(c.u64()?))))()
        }
        _ => return None,
    };
    Some(s.filter(|_| c.done()))
}

/// The functions whose rows say which pipeline was set: the draws, dispatches and ray dispatches.
fn runs_pipeline(name: &str) -> bool {
    matches!(
        name.strip_prefix(LIST),
        Some(
            "DrawInstanced"
                | "DrawIndexedInstanced"
                | "Dispatch"
                | "DispatchMesh"
                | "DispatchRays"
                | "ExecuteIndirect"
        )
    )
}

/// What [`calls`] made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calls {
    /// `calls.tsv`.
    pub tsv: String,
    /// Rows (every event), those whose arguments quen decoded, those shown raw, and of these the ones of a function
    /// quen decodes whose decoder didn't fit them.
    pub calls: usize,
    pub decoded: usize,
    pub raw: usize,
    pub unfit: usize,
    /// Per function, how many rows didn't fit its decoder.
    pub unfit_by: BTreeMap<String, usize>,
}

/// `calls.tsv`, one row per event of `--metadata-functions` ([`frame`]): event, function, thread, `this` (a command's
/// list), CPU µs (a call's, during the capture), pipeline (a draw's or dispatch's, the one set on its list then), and
/// the arguments, decoded as `key=value` or `raw: <hex>` ([`raw`]).
pub fn calls<R: Read + Seek>(
    capture: &mut Capture<R>,
    events: &[Event],
    names: &HashMap<u64, &Listed>,
) -> Result<Calls, String> {
    let frame = frame(capture, events)?;
    let n = |uid: u64| object_name(names, uid);
    let mut out = Calls {
        tsv: "event\tfunction\tthread\tthis\tcpu_us\tpipeline\targs\n".into(),
        calls: events.len(),
        ..Calls::default()
    };
    for (i, (e, s)) in events.iter().zip(&frame.sources).enumerate() {
        let name = e.function_name.as_str();
        let (this, cpu, pipeline, args) = match *s {
            Source::Nsight => ("-".into(), "-".into(), 0, None),
            Source::Indirect { list, pipeline } => (n(list), "-".into(), pipeline, None),
            Source::Call(r) => {
                let rec = frame.records[r];
                let b = capture.bytes(rec.args).unwrap_or_default();
                let cpu = rec.end_us.saturating_sub(rec.start_us).to_string();
                (n(rec.this), cpu, 0, Some((b, false)))
            }
            Source::Command {
                list,
                recording,
                start,
                end,
                pipeline,
            } => {
                let b = capture
                    .bytes(frame.records[recording].args)
                    .unwrap_or_default();
                (n(list), "-".into(), pipeline, Some((&b[start..end], true)))
            }
        };
        let args = match args {
            None => String::from(if matches!(s, Source::Indirect { .. }) {
                "indirect"
            } else {
                "-"
            }),
            Some((b, command)) => match decode(name, b, command, &n) {
                Some(Some(text)) => {
                    out.decoded += 1;
                    text
                }
                fitted => {
                    out.raw += 1;
                    if fitted.is_some() {
                        out.unfit += 1;
                        *out.unfit_by.entry(name.to_owned()).or_default() += 1;
                    }
                    format!("raw: {}", raw(b, RAW_SHOWN))
                }
            },
        };
        let pipeline = match runs_pipeline(name) || matches!(s, Source::Indirect { .. }) {
            true => n(pipeline),
            false => "-".into(),
        };
        out.tsv += &format!(
            "{i}\t{name}\t{}\t{this}\t{cpu}\t{pipeline}\t{args}\n",
            e.thread_index
        );
    }
    Ok(out)
}

/// Each resource's GPU address during the capture, from its creation record: Nsight's string tag `0xFB000000` at 56,
/// the name's u64 byte count at 60 and the name at 68, then a null pointer tag and, 36 bytes after it, the address.
/// Checked against every GetGPUVirtualAddress of the frame; a resource whose record has another shape has none.
fn resource_vas<R: Read + Seek>(
    capture: &Capture<R>,
    resources: &[&Object],
    events: &[Event],
    frame: &Frame,
) -> Result<HashMap<u64, u64>, String> {
    let mut vas = HashMap::new();
    for o in resources {
        let Some(b) = capture.bytes(o.record) else {
            continue;
        };
        let name = u64_at(b, 60).and_then(|n| usize::try_from(n).ok());
        let end = name.and_then(|n| n.checked_add(68));
        let at = end
            .filter(|&e| u32_at(b, 56) == Some(0xFB00_0000) && u32_at(b, e) == Some(0xFA00_0002));
        if let Some(va) = at.and_then(|e| u64_at(b, e + 36)) {
            vas.insert(o.uid, va);
        }
    }
    for (e, s) in events.iter().zip(&frame.sources) {
        let Source::Call(r) = *s else { continue };
        if e.function_name != "ID3D12Resource_GetGPUVirtualAddress" {
            continue;
        }
        let rec = frame.records[r];
        let called = capture.bytes(rec.args).and_then(|b| u64_at(b, 0));
        match (vas.get(&rec.this), called) {
            (Some(va), Some(c)) if *va != c => {
                return Err(layout(format!(
                    "resource {}'s address in its record isn't GetGPUVirtualAddress's",
                    rec.this
                )));
            }
            (None, Some(c)) => {
                vas.insert(rec.this, c);
            }
            _ => {}
        }
    }
    Ok(vas)
}

/// One top-level build's instance table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tlas {
    /// The build's event.
    pub event: usize,
    pub instances: u32,
    /// `tlas_<n>.tsv`; None when the file doesn't hold the instances (written by the GPU, or not found).
    pub tsv: Option<String>,
    /// Instances whose BLAS address is an acceleration structure's.
    pub named: usize,
}

/// What [`raytracing`] read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raytracing {
    /// `blas_builds.tsv`.
    pub blas: String,
    pub blas_builds: usize,
    pub geometries: usize,
    /// Builds whose arguments didn't decode.
    pub undecoded: usize,
    pub tlas: Vec<Tlas>,
}

/// The frame's ray-tracing builds (headless research §3.6):
/// - acceleration structures by address: each one's resource (its record's u64 at 40) at its offset (u64 at 48),
///   a resource's address from [`resource_vas`];
/// - `blas_builds.tsv`, one row per geometry of each bottom-level build: event, list, destination (address and the
///   acceleration structure there), build flags, geometry index, kind, its flags, transform, index format, count and
///   buffer, vertex format, count, buffer and stride (an AABB geometry: its count, start and stride in the vertex
///   columns), source, scratch;
/// - each top-level build's instance array, found in its list's state as the u64 size (64 per instance) followed by
///   the u64 id of a chunk of that size (the list's k-th build of that size, its k-th such pair): `tlas_<n>.tsv` in
///   build order, one row per D3D12_RAYTRACING_INSTANCE_DESC: instance, id, mask, hit group offset, flags, BLAS
///   address and name, the 3x4 transform by rows.
pub fn raytracing<R: Read + Seek>(
    capture: &mut Capture<R>,
    events: &[Event],
    objects: &[Object],
    names: &HashMap<u64, &Listed>,
) -> Result<Raytracing, String> {
    let frame = frame(capture, events)?;
    let typed = |t: &str| -> Vec<&Object> {
        objects
            .iter()
            .filter(|o| names.get(&o.uid).is_some_and(|l| l.type_name == t))
            .collect()
    };
    let (resources, structures, lists) = (
        typed("Resource"),
        typed("AccelerationStructure"),
        typed("GraphicsCommandList"),
    );
    capture.load(resources.iter().chain(&structures).map(|o| o.record))?;
    capture.load(lists.iter().map(|o| o.state))?;
    let resource_va = resource_vas(capture, &resources, events, &frame)?;
    let mut at: HashMap<u64, u64> = HashMap::new();
    for o in &structures {
        let Some(b) = capture.bytes(o.record) else {
            continue;
        };
        let header = u32_at(b, 0) == Some(16) && u32_at(b, 8) == Some(20);
        let (res, offset) = (u64_at(b, 40), u64_at(b, 48));
        if let (true, Some(res), Some(offset)) = (header, res, offset)
            && let Some(base) = resource_va.get(&res)
        {
            at.insert(base + offset, o.uid);
        }
    }
    let n = |uid: u64| object_name(names, uid);
    let named = |va: u64| at.get(&va).map_or("-".into(), |uid| n(*uid));
    let state: HashMap<u64, Ref> = lists.iter().map(|o| (o.uid, o.state)).collect();
    let mut out = Raytracing {
        blas: "event\tlist\tdest\tdest_name\tflags\tgeometry\tkind\tgeometry_flags\ttransform\tindex_format\t\
               index_count\tindex_buffer\tvertex_format\tvertex_count\tvertex_buffer\tvertex_stride\tsource\tscratch\n"
            .into(),
        ..Raytracing::default()
    };
    // Per list and instance array size, the top-level builds seen so far.
    let mut seen: HashMap<(u64, u64), usize> = HashMap::new();
    for (i, (e, s)) in events.iter().zip(&frame.sources).enumerate() {
        let Source::Command {
            list,
            recording,
            start,
            end,
            ..
        } = *s
        else {
            continue;
        };
        if e.function_name.strip_prefix(LIST) != Some("BuildRaytracingAccelerationStructure") {
            continue;
        }
        let args = capture
            .bytes(frame.records[recording].args)
            .unwrap_or_default();
        let Some(b) = decode_build(&args[start..end]) else {
            out.undecoded += 1;
            continue;
        };
        if b.top {
            let size = 64 * u64::from(b.count);
            let k = seen.entry((list, size)).or_default();
            let chunk = state
                .get(&list)
                .and_then(|r| capture.bytes(*r))
                .and_then(|st| instance_chunks(st, size, |id| capture.size(id)).nth(*k));
            *k += 1;
            let rows = match chunk {
                Some(id) => Some(capture.chunk(id)?),
                None => None,
            };
            let mut tlas = Tlas {
                event: i,
                instances: b.count,
                ..Tlas::default()
            };
            if let Some(rows) = rows {
                let mut tsv = String::from(
                    "instance\tid\tmask\thit_group\tflags\tblas\tblas_name\ttransform\n",
                );
                for (k, row) in rows.chunks_exact(64).enumerate() {
                    let t: Vec<String> = (0..12)
                        .map(|j| {
                            f32::from_le_bytes(row[4 * j..4 * j + 4].try_into().unwrap())
                                .to_string()
                        })
                        .collect();
                    let (id, hit, blas) = (
                        u32_at(row, 48).unwrap(),
                        u32_at(row, 52).unwrap(),
                        u64_at(row, 56).unwrap(),
                    );
                    tlas.named += usize::from(at.contains_key(&blas));
                    tsv += &format!(
                        "{k}\t{}\t0x{:02x}\t{}\t0x{:02x}\t{}\t{}\t{}\n",
                        id & 0xff_ffff,
                        id >> 24,
                        hit & 0xff_ffff,
                        hit >> 24,
                        va(blas),
                        named(blas),
                        t.join(",")
                    );
                }
                tlas.tsv = Some(tsv);
            }
            out.tlas.push(tlas);
            continue;
        }
        out.blas_builds += 1;
        for (g, geometry) in b.geometry.iter().enumerate() {
            out.geometries += 1;
            let cells = match *geometry {
                Geometry::Triangles {
                    flags,
                    transform,
                    index_format,
                    vertex_format,
                    index_count,
                    vertex_count,
                    index_buffer,
                    vertex_buffer,
                    vertex_stride,
                } => format!(
                    "triangles\t0x{flags:x}\t{}\t{index_format}\t{index_count}\t{}\t{vertex_format}\t{vertex_count}\t{}\t\
                     {vertex_stride}",
                    va(transform),
                    va(index_buffer),
                    va(vertex_buffer)
                ),
                Geometry::Aabbs {
                    flags,
                    count,
                    start,
                    stride,
                } => {
                    format!(
                        "aabbs\t0x{flags:x}\t-\t-\t-\t-\t-\t{count}\t{}\t{stride}",
                        va(start)
                    )
                }
            };
            out.blas += &format!(
                "{i}\t{}\t{}\t{}\t0x{:x}\t{g}\t{cells}\t{}\t{}\n",
                n(list),
                va(b.dest),
                named(b.dest),
                b.flags,
                va(b.source),
                va(b.scratch)
            );
        }
    }
    Ok(out)
}

/// The chunks a list's state names as `size` bytes: each u64 `size` (at any offset) followed by a u64 that is the id
/// of a chunk of exactly that size, in order.
fn instance_chunks(
    state: &[u8],
    size: u64,
    chunk_size: impl Fn(u64) -> Option<u64>,
) -> impl Iterator<Item = u64> {
    (0..state.len().saturating_sub(15)).filter_map(move |k| {
        let id = u64_at(state, k + 8)?;
        (size > 0 && u64_at(state, k)? == size && chunk_size(id) == Some(size)).then_some(id)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ngfx_capture::tests::{file, listed_json, root};
    use crate::ngfx_capture::{LAYOUT, listed};
    use serde_json::json;
    use std::io::Cursor;

    pub(crate) const ECL: (u32, u32) = (3, 110);

    /// A pointer to one element after its u32 size (tag 0x44).
    pub(crate) fn sized(element: &[u8]) -> Vec<u8> {
        [
            &0xFA00_0044u32.to_le_bytes()[..],
            &(element.len() as u32).to_le_bytes(),
            element,
        ]
        .concat()
    }

    /// A recording's payload: the count, then each (code, arguments after the leading u64) with the end marker.
    pub(crate) fn recording(cmds: &[(u32, u64, Vec<u8>)]) -> Vec<u8> {
        let mut out = (cmds.len() as u64).to_le_bytes().to_vec();
        for (code, lead, args) in cmds {
            out.extend(3u32.to_le_bytes());
            out.extend(code.to_le_bytes());
            out.extend(lead.to_le_bytes());
            out.extend(args);
            out.extend(END);
        }
        out
    }

    pub(crate) fn submit(list: u64) -> Vec<u8> {
        [&1u32.to_le_bytes()[..], &sized(&list.to_le_bytes())].concat()
    }

    /// A synthetic capture whose call table holds `calls` ((api, code), this, arguments) on thread 1, CPU 10..15 µs
    /// each, the arguments in chunk 3, `more` chunks after; the object table (chunk 4) holds `objects`.
    pub(crate) fn capture(
        calls: &[((u32, u32), u64, Vec<u8>)],
        objects: Vec<u8>,
        more: &[(u64, Vec<u8>, bool)],
    ) -> Vec<u8> {
        let (mut table, mut args) = (Vec::new(), Vec::new());
        for (seq, ((api, code), this, a)) in calls.iter().enumerate() {
            table.extend((seq as u64).to_le_bytes());
            table.extend([0u8; 12]);
            for v in [1, *api, *code, 10, 15] {
                table.extend(v.to_le_bytes());
            }
            table.extend(this.to_le_bytes());
            for v in [3, args.len() as u64, a.len() as u64] {
                table.extend(v.to_le_bytes());
            }
            args.extend(a);
        }
        let mut chunks = vec![
            (1, root(4, 2), true),
            (2, table, true),
            (3, args, false),
            (4, objects, false),
        ];
        chunks.extend(more.iter().cloned());
        file(&chunks)
    }

    pub(crate) fn event(name: &str, seq: Option<u64>, indirect: bool) -> serde_json::Value {
        let mut e = json!({"function_name": name, "thread_index": 1});
        if let Some(s) = seq {
            e["sequence_id"] = json!(s);
        }
        if indirect {
            e["indirect_index"] = json!(0);
        }
        e
    }

    pub(crate) fn functions(list: &[serde_json::Value]) -> Vec<Event> {
        let list: Vec<_> = list
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut e = e.clone();
                e["event_index"] = json!(i);
                e
            })
            .collect();
        events(&format!("\u{feff}{}", serde_json::Value::from(list))).unwrap()
    }

    const L1: u64 = 101;
    const L3: u64 = 103;

    /// Queue 900 submits list 103 before the frame records it (its last recording serves) and again later; queue 901
    /// submits list 101, listed after 900's second submit, which the names choose. 101 starts with the Reset's
    /// pipeline, then sets another; an ExecuteIndirect's own Dispatch follows it.
    fn frame_calls() -> (Vec<u8>, Vec<serde_json::Value>) {
        let reset = [
            &0u32.to_le_bytes()[..],
            &50u64.to_le_bytes(),
            &60u64.to_le_bytes(),
        ]
        .concat();
        let mut ei = 80u64.to_le_bytes().to_vec();
        ei.extend(1u32.to_le_bytes());
        ei.extend(81u64.to_le_bytes());
        ei.extend([0u8; 24 + 20]);
        let l1 = recording(&[
            (
                40,
                0,
                [2u32, 1, 1].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (51, 0, 61u64.to_le_bytes().to_vec()),
            (85, 0, ei),
            (35, 0, 0u32.to_le_bytes().to_vec()),
        ]);
        let l3 = recording(&[(35, 0, 0u32.to_le_bytes().to_vec())]);
        let signal = [
            &0u32.to_le_bytes()[..],
            &70u64.to_le_bytes(),
            &3u64.to_le_bytes(),
        ]
        .concat();
        let calls = capture(
            &[
                (ECL, 900, submit(L3)),
                ((3, 36), L1, reset),
                ((3, 35), L1, 0u32.to_le_bytes().to_vec()),
                ((1, 2), L1, l1),
                (ECL, 901, submit(L1)),
                (ECL, 900, submit(L3)),
                ((3, 114), 900, signal),
                ((1, 2), L3, l3),
            ],
            Vec::new(),
            &[],
        );
        let close = "ID3D12GraphicsCommandList_Close";
        let list = vec![
            event("CaptureBegin", Some(0), false),
            event(SUBMIT, Some(0), false),
            event(close, None, false),
            event(RESET, Some(1), false),
            event(close, Some(2), false),
            event(RECORDING, Some(3), false),
            event(SUBMIT, Some(4), false),
            event(SUBMIT, Some(5), false),
            event(close, None, false),
            event("ID3D12GraphicsCommandList_Dispatch", None, false),
            event("ID3D12GraphicsCommandList_SetPipelineState", None, false),
            event("ID3D12GraphicsCommandList_ExecuteIndirect", None, false),
            event("ID3D12GraphicsCommandList_Dispatch", None, true),
            event(close, None, false),
            event("ID3D12CommandQueue_Signal", Some(6), false),
            event(RECORDING, Some(7), false),
        ];
        (calls, list)
    }

    #[test]
    fn calls_line_up_with_the_function_list() {
        let (bytes, list) = frame_calls();
        let mut c = Capture::open(Cursor::new(bytes)).unwrap();
        let named = listed(&listed_json(&[
            (900, "CommandQueue", "DirectQueue"),
            (L1, "GraphicsCommandList", "GraphicsCommandList_101"),
            (60, "PipelineState", "PipelineState_60"),
            (61, "PipelineState", "PipelineState_61"),
            (70, "Fence", "FrameFence"),
        ]))
        .unwrap();
        let names: HashMap<u64, &Listed> = named.iter().map(|l| (l.uid, l)).collect();
        let calls = calls(&mut c, &functions(&list), &names).unwrap();
        let rows: Vec<&str> = calls.tsv.lines().collect();
        assert_eq!(rows.len(), 17, "{}", calls.tsv);
        let counts = (calls.calls, calls.decoded, calls.raw, calls.unfit);
        assert_eq!(counts, (16, 10, 4, 0), "{}", calls.tsv);
        let list = "GraphicsCommandList_101";
        for want in [
            "0\tCaptureBegin\t1\t-\t-\t-\t-".to_owned(),
            "1\tID3D12CommandQueue_ExecuteCommandLists\t1\tDirectQueue#900\t5\t-\tlists=[#103]".into(),
            "2\tID3D12GraphicsCommandList_Close\t1\t#103\t-\t-\traw: 000000000000000000000000".into(),
            format!("3\t{RESET}\t1\t{list}\t5\t-\tallocator=#50 pso=PipelineState_60 hr=0x0"),
            format!("5\tCommandRecording\t1\t{list}\t5\t-\tcommands=4"),
            "8\tID3D12GraphicsCommandList_Close\t1\t#103\t-\t-\traw: 000000000000000000000000".into(),
            format!("9\tID3D12GraphicsCommandList_Dispatch\t1\t{list}\t-\tPipelineState_60\tgroups=2x1x1"),
            format!("10\tID3D12GraphicsCommandList_SetPipelineState\t1\t{list}\t-\t-\tpso=PipelineState_61"),
            format!(
                "11\tID3D12GraphicsCommandList_ExecuteIndirect\t1\t{list}\t-\tPipelineState_61\t\
                 signature=#80 max=1 args=#81+0x0 count=-+0x0"
            ),
            format!("12\tID3D12GraphicsCommandList_Dispatch\t1\t{list}\t-\tPipelineState_61\tindirect"),
            "14\tID3D12CommandQueue_Signal\t1\tDirectQueue#900\t5\t-\tfence=FrameFence#70 value=3 hr=0x0".into(),
        ] {
            assert!(rows.contains(&want.as_str()), "{want}\n{}", calls.tsv);
        }
    }

    #[test]
    fn a_frame_that_doesnt_line_up_is_refused() {
        let (bytes, list) = frame_calls();
        let run = |list: Vec<serde_json::Value>| {
            let mut c = Capture::open(Cursor::new(bytes.clone())).unwrap();
            frame(&mut c, &functions(&list)).err().unwrap_or_default()
        };
        // Another thread; a name for a code that has one; a command event no submit covers; a call with no event.
        let mut thread = list.clone();
        thread[3]["thread_index"] = json!(2);
        let mut renamed = list.clone();
        renamed[13]["function_name"] = json!("ID3D12GraphicsCommandList_Dispatch");
        let mut extra = list.clone();
        extra.push(event("ID3D12GraphicsCommandList_Close", None, false));
        let short = list[..15].to_vec();
        for (why, list) in [
            ("thread", thread),
            ("start there", renamed),
            ("start there", extra),
            ("no event", short),
        ] {
            let err = run(list);
            assert!(err.starts_with(LAYOUT) && err.contains(why), "{why}: {err}");
        }
    }

    #[test]
    fn arguments_decode_whole_or_not_at_all() {
        let n = |uid: u64| format!("#{uid}");
        let barrier = [
            &3u32.to_le_bytes()[..],
            &0xFA00_0008u32.to_le_bytes(),
            &[2],
            &[0u8; 8],
            &7u64.to_le_bytes(),
            &[0xff; 4],
            &1u32.to_le_bytes(),
            &4u32.to_le_bytes(),
            &[2, 0, 0, 0, 0, 0, 0, 0],
            &9u64.to_le_bytes(),
        ]
        .concat();
        let lead = |b: &[u8]| [&[0u8; 8][..], b].concat();
        let name = "ID3D12GraphicsCommandList_ResourceBarrier";
        // The count says 3, the pointer holds 2: no fit; with 2 it decodes.
        assert_eq!(decode(name, &lead(&barrier), true, &n), Some(None));
        let mut two = barrier.clone();
        two[0] = 2;
        assert_eq!(
            decode(name, &lead(&two), true, &n),
            Some(Some(
                "[transition #7 sub=4294967295 0x1->0x4, uav #9]".into()
            ))
        );
        // A byte too many; a function quen doesn't decode.
        let mut more = lead(&two);
        more.push(0);
        assert_eq!(decode(name, &more, true, &n), Some(None));
        let close = "ID3D12GraphicsCommandList_Close";
        assert_eq!(decode(close, &lead(&[0; 4]), true, &n), None);
    }

    fn le(values: &[u64], widths: &[usize]) -> Vec<u8> {
        values
            .iter()
            .zip(widths)
            .flat_map(|(v, w)| v.to_le_bytes()[..*w].to_vec())
            .collect()
    }

    /// A build command's arguments after its leading u64 4: the description (`inputs` its type, flags, count,
    /// layout and then the instance address or the geometry pointer), no postbuild, Nsight's trailer.
    fn build_args(dest: u64, inputs: Vec<u8>, scratch: u64, top: bool) -> Vec<u8> {
        let ptr = |b: Vec<u8>| [&0xFA00_0004u32.to_le_bytes()[..], &b].concat();
        let null = 0xFA00_0002u32.to_le_bytes().to_vec();
        let desc = [
            dest.to_le_bytes().to_vec(),
            inputs,
            le(&[0, scratch], &[8, 8]),
        ]
        .concat();
        let entry = if top { null.clone() } else { ptr(vec![0; 20]) };
        [
            ptr(desc),
            le(&[0], &[4]),
            null.clone(),
            le(&[4, 0], &[8, 4]),
            entry,
            ptr(vec![0; 24]),
            null,
        ]
        .concat()
    }

    /// Resource 40 (a buffer at 0x2500000000, named in its record) holds BLAS 41 at 0x100; list 101 builds it, then a
    /// TLAS of its two instances, whose array is chunk 9, named in the list's state.
    fn raytracing_capture(called_va: u64) -> (Vec<u8>, Vec<serde_json::Value>) {
        let base = 0x25_0000_0000u64;
        let name: Vec<u8> = "BlasBuffer\0"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        let mut resource = vec![0u8; 56];
        resource.extend(le(&[0xFB00_0000, name.len() as u64], &[4, 8]));
        resource.extend(&name);
        resource.extend(le(&[0xFA00_0002], &[4]));
        resource.extend([0u8; 32]);
        resource.extend(base.to_le_bytes());
        let mut blas = le(
            &[16, 2, 20, 0, 0x900, 0x900, 0x900, 40, 0x100],
            &[4, 4, 4, 4, 8, 8, 8, 8, 8],
        );
        blas.extend([0u8; 16]);
        let state = [vec![7u8; 5], le(&[128, 9], &[8, 8]), vec![0; 3]].concat();
        let records = [resource.clone(), blas.clone(), state.clone()].concat();
        let r = |offset: usize, len: usize| Ref {
            chunk: 5,
            offset: offset as u64,
            len: len as u64,
        };
        let (ro, bo, so) = (0, resource.len(), resource.len() + blas.len());
        let table = [
            crate::ngfx_capture::tests::object(40, 3, 6, r(ro, resource.len())),
            crate::ngfx_capture::tests::object(41, 3, 0x86d1_6224, r(bo, blas.len())),
            [
                &101u64.to_le_bytes()[..],
                &[0u8; 8],
                &3u32.to_le_bytes(),
                &14u32.to_le_bytes(),
                &[0u8; 24],
                &le(&[5, so as u64, state.len() as u64], &[8, 8, 8]),
            ]
            .concat(),
        ]
        .concat();
        let geometry = [
            vec![0x04, 0, 0, 0xFA],
            le(
                &[0, 2, 0x1020, 57, 6, 48, 32, 0x2400, 0x2500, 64],
                &[4, 4, 8, 4, 4, 4, 4, 8, 8, 8],
            ),
        ]
        .concat();
        let bottom = build_args(
            base + 0x100,
            [le(&[1, 8, 1, 0], &[4, 4, 4, 4]), geometry].concat(),
            0x99,
            false,
        );
        let top = build_args(
            0x7000,
            le(&[0, 4, 2, 0, 0x8000], &[4, 4, 4, 4, 8]),
            0x98,
            true,
        );
        let l1 = recording(&[(98, 4, bottom), (98, 4, top), (35, 0, vec![0; 4])]);
        let mut instances = Vec::new();
        for (id, hit, blas) in [(5u64, 3u64, base + 0x100), (6, 0, 0x1234)] {
            let t: Vec<u8> = (0..12).flat_map(|j| (j as f32).to_le_bytes()).collect();
            instances.extend(t);
            instances.extend(le(&[id | 0xff << 24, hit | 1 << 24, blas], &[4, 4, 8]));
        }
        let calls = capture(
            &[
                ((3, 19), 40, called_va.to_le_bytes().to_vec()),
                ((1, 2), 101, l1),
                (ECL, 900, submit(101)),
            ],
            table,
            &[(5, records, true), (9, instances, false)],
        );
        let build = "ID3D12GraphicsCommandList_BuildRaytracingAccelerationStructure";
        let list = vec![
            event("ID3D12Resource_GetGPUVirtualAddress", Some(0), false),
            event(RECORDING, Some(1), false),
            event(SUBMIT, Some(2), false),
            event(build, None, false),
            event(build, None, false),
            event("ID3D12GraphicsCommandList_Close", None, false),
        ];
        (calls, list)
    }

    #[test]
    fn ray_tracing_tables_read_from_the_frame() {
        let base = 0x25_0000_0000u64;
        let (bytes, list) = raytracing_capture(base);
        let mut c = Capture::open(Cursor::new(bytes)).unwrap();
        let objs = c.objects().unwrap();
        let named = listed(&listed_json(&[
            (40, "Resource", "BlasBuffer"),
            (41, "AccelerationStructure", "BLAS_41"),
            (101, "GraphicsCommandList", "GraphicsCommandList_101"),
        ]))
        .unwrap();
        let names = crate::ngfx_capture::check_objects(&objs, &named).unwrap();
        let rt = raytracing(&mut c, &functions(&list), &objs, &names).unwrap();
        assert_eq!(
            (rt.blas_builds, rt.geometries, rt.undecoded),
            (1, 1, 0),
            "{}",
            rt.blas
        );
        let row = rt.blas.lines().nth(1).unwrap();
        assert_eq!(
            row,
            "3\tGraphicsCommandList_101\t0x2500000100\tBLAS_41\t0x8\t0\ttriangles\t0x2\t0x1020\t57\t48\t0x2400\t6\t32\t\
             0x2500\t64\t0x0\t0x99"
        );
        assert_eq!(rt.tlas.len(), 1);
        let t = &rt.tlas[0];
        assert_eq!((t.event, t.instances, t.named), (4, 2, 1));
        let tsv = t.tsv.as_deref().unwrap();
        let rows: Vec<&str> = tsv.lines().collect();
        assert_eq!(
            rows[1],
            "0\t5\t0xff\t3\t0x01\t0x2500000100\tBLAS_41\t0,1,2,3,4,5,6,7,8,9,10,11"
        );
        assert_eq!(
            rows[2],
            "1\t6\t0xff\t0\t0x01\t0x1234\t-\t0,1,2,3,4,5,6,7,8,9,10,11"
        );

        // GetGPUVirtualAddress says another address than the resource's record: refused.
        let (bytes, list) = raytracing_capture(base + 0x10);
        let mut c = Capture::open(Cursor::new(bytes)).unwrap();
        let err = raytracing(&mut c, &functions(&list), &objs, &names).unwrap_err();
        assert!(
            err.starts_with(LAYOUT) && err.contains("GetGPUVirtualAddress"),
            "{err}"
        );
    }
}
