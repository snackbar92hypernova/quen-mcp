//! The live-memory tools: `mem_read`, `mem_scan`, `mem_follow`, `mem_patch` and `mem_unpatch`, and the Windows
//! half of quen-core's `mem`. [`Process`] reads the game through a handle with read access only, on the pid from the
//! session's job (never "the first witcher3.exe"). [`Ctx::write`] journals a planned write, then makes it with a handle
//! open for that write only. game_cvar and measure_ab's cvar actions (game.rs) go through the same two.

use std::ffi::c_void;
use std::fmt::Display;
use std::fs;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use quen_core::mem::{self, Image, Kind, Memory, Op, Pattern, Region, Scope, Section, Type, Write};
use serde_json::{Map, Value, json};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FlushInstructionCache, ReadProcessMemory, WriteProcessMemory,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE,
    PAGE_GUARD, PAGE_NOACCESS, VirtualProtectEx, VirtualQueryEx,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
    PROCESS_VM_READ, PROCESS_VM_WRITE,
};

use crate::doctor;
use crate::game::{self, Ctx, Fields, ctx, text};
use crate::session::{Call, Done, Session};

/// A scan page's hits when the call gives no `limit`.
const LIMIT: usize = 100;

pub fn tools() -> Vec<Value> {
    let token = json!({"type": "string", "description": "the token session_launch returned"});
    let schema = |props: Value, required: &[&str]| {
        let mut props = props;
        props["token"] = token.clone();
        let mut required = required.to_vec();
        required.insert(0, "token");
        json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
    };
    let number = |what: &str| json!({"type": "string", "description": format!("{what}: a number or 0x hex")});
    let name = json!({"type": "string", "description": "a patch's name in the KB's patches.tsv"});
    let patch = |verb: &str, does: &str| {
        json!({
            "name": format!("mem_{verb}"),
            "description": format!("{does} Refused, nothing written: no row of that name for the running build in \
                the KB's patches.tsv, boundaries-checked not set, a row that breaks the rules (1 to 8 bytes inside one \
                aligned qword, old and new the same length), or live bytes other than expected (another build or \
                another writer); with needs: no KB in the local config or no patches.tsv in it, or a GOG install \
                (the KB is Steam 5.01's). \
                The write is journaled first and taints saves the game writes after it."),
            "inputSchema": schema(json!({"name": name}), &["name"]),
        })
    };
    vec![
        json!({
            "name": "mem_read",
            "description": "Read the running game's memory: one little-endian number, or 1 to 4096 raw bytes as hex. \
                The game is the one in the session's job, with read access only. Give address, or rva (from the \
                game's image base, as the KB writes them).",
            "inputSchema": schema(json!({
                "address": number("where"),
                "rva": number("where, from the game's image base"),
                "type": {"type": "string", "enum": ["u8", "u16", "u32", "u64", "i32", "f32", "f64", "bytes"], "description": "a number's type, or bytes (with len)"},
                "len": {"type": "integer", "minimum": 1, "maximum": 4096, "description": "bytes: how many"},
            }), &["type"]),
        }),
        json!({
            "name": "mem_scan",
            "description": "Search the running game's memory for a byte pattern (IDA-style `48 8B ?? ?? 4?`, with an \
                optional hex bit mask), an ASCII string or a UTF-16LE one (exact, no terminator). Scope: sections of \
                the game's image (.text, .rdata, .data: faster), region bases from an earlier result, or by default \
                every readable region (slow on a big game). One page of hits (default 100, at most 1000); pass next \
                as from for the page after. skipped lists what couldn't be read, so may hold hits nobody saw.",
            "inputSchema": schema(json!({
                "pattern": {"type": "string", "description": "bytes in hex, ?? or 4? for any byte or nibble"},
                "mask": {"type": "string", "description": "hex, one byte per pattern byte"},
                "ascii": {"type": "string", "description": "an ASCII string to find"},
                "utf16": {"type": "string", "description": "a string to find as UTF-16LE"},
                "sections": {"type": "array", "items": {"type": "string"}, "description": "image sections: .text, .rdata, .data"},
                "regions": {"type": "array", "items": {"type": "string"}, "description": "region bases from an earlier result: numbers or 0x hex"},
                "from": number("search from"),
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "hits per page, default 100"},
            }), &[]),
        }),
        json!({
            "name": "mem_follow",
            "description": "Follow a 64-bit pointer chain in the running game: each hop adds its offset to the pointer \
                so far and reads the qword there. offsets [a, b] from base give [[base + a] + b]. Returns every hop \
                read, then end (the last value) or where and why it broke. Give base, or rva (from the image base).",
            "inputSchema": schema(json!({
                "base": number("where the chain starts"),
                "rva": number("where it starts, from the game's image base"),
                "offsets": {"type": "array", "items": {"type": "string"}, "description": "numbers or (-)0x hex"},
            }), &["offsets"]),
        }),
        patch(
            "patch",
            "Apply a live code or data patch defined in the KB's patches.tsv: its new bytes over its old ones, at its \
             RVA for the running build. Returns the write and the bytes there now.",
        ),
        patch(
            "unpatch",
            "Undo a mem_patch: the old bytes back, only while the live bytes are still the patched ones.",
        ),
    ]
}

pub(crate) fn call(session: &mut Session, call: &mut Call, name: &str, args: &Value) -> Done {
    let out = match name {
        "mem_read" => read(session, call, args),
        "mem_scan" => scan(session, call, args),
        "mem_follow" => follow(session, call, args),
        "mem_patch" => patch(session, call, args, Op::Patch),
        "mem_unpatch" => patch(session, call, args, Op::Unpatch),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    out.map_or_else(|done| done, Done::Ok)
}

/// The game's memory, read only: a handle with `PROCESS_VM_READ | PROCESS_QUERY_INFORMATION` on one pid.
pub(crate) struct Process {
    pid: u32,
    handle: OwnedHandle,
    /// The game's image: its build stamp, and the sections its regions are named by.
    pub(crate) image: Image,
}

impl Process {
    /// Opens `pid` for reading, and reads the headers of its image `exe`.
    pub(crate) fn open(pid: u32, exe: &str) -> Result<Process, String> {
        let base = game::module_base(pid, exe)
            .map_err(|e| format!("modules of pid {pid}: {e}"))?
            .ok_or(format!("{exe} is not loaded in pid {pid}"))? as u64;
        let handle = open(pid, PROCESS_VM_READ | PROCESS_QUERY_INFORMATION)
            .map_err(|e| format!("open pid {pid} for reading: {e}"))?;
        let image = Image {
            base,
            stamp: String::new(),
            sections: Vec::new(),
        };
        let mut process = Process { pid, handle, image };
        process.image = mem::image(&process, base).map_err(|e| format!("{exe}'s headers: {e}"))?;
        Ok(process)
    }

    /// Makes `w` if the bytes there are still `w.old`: with a handle that has write access for this write only, the
    /// pages made writable around it and put back, and the instruction cache flushed (a code patch runs the new
    /// bytes). Same-length writes, so a torn one is harmless. Err says why, and whether any byte may have been
    /// written (false: it failed before WriteProcessMemory, nothing changed).
    fn write(&self, w: &Write) -> Result<(), (String, bool)> {
        let unwritten = |why: String| (why, false);
        let mut live = vec![0; w.old.len()];
        self.read(w.address, &mut live)
            .map_err(|e| unwritten(format!("can't read it again: {e}")))?;
        if live != w.old {
            return Err(unwritten(format!(
                "the bytes changed since they were read ({live:02x?}): another writer"
            )));
        }
        let handle = open(self.pid, PROCESS_VM_WRITE | PROCESS_VM_OPERATION)
            .map_err(|e| unwritten(format!("open pid {} for writing: {e}", self.pid)))?;
        let (h, at, len) = (
            handle.as_raw_handle(),
            w.address as *const c_void,
            w.new.len(),
        );
        let mut protect = 0;
        // SAFETY: a handle this opened, closed when `handle` drops; the protection change covers the bytes written
        // and is put back; the source buffer holds `len` bytes.
        unsafe {
            if VirtualProtectEx(h, at, len, PAGE_EXECUTE_READWRITE, &mut protect) == 0 {
                let why = format!("VirtualProtectEx: {}", io::Error::last_os_error());
                return Err(unwritten(why));
            }
            let mut wrote = 0;
            let ok = WriteProcessMemory(h, at, w.new.as_ptr().cast(), len, &mut wrote);
            let error = io::Error::last_os_error();
            VirtualProtectEx(h, at, len, protect, &mut protect);
            FlushInstructionCache(h, at, len);
            if ok == 0 || wrote != len {
                let why = format!("WriteProcessMemory wrote {wrote} of {len} bytes: {error}");
                return Err((why, wrote > 0));
            }
        }
        Ok(())
    }

    /// `r` cut where the image's sections begin and end, each piece named by the section it lies in.
    fn split(&self, r: Region) -> Vec<Region> {
        let end = r.base + r.size;
        let section_end = |s: &Section| (s.base + s.size).next_multiple_of(0x1000);
        let sections = &self.image.sections;
        let mut cuts: Vec<u64> = sections
            .iter()
            .flat_map(|s| [s.base, section_end(s)])
            .filter(|&c| r.base < c && c < end)
            .chain([end])
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut at = r.base;
        cuts.into_iter()
            .map(|cut| {
                let section = sections
                    .iter()
                    .find(|s| s.base <= at && at < section_end(s));
                let piece = Region {
                    base: at,
                    size: cut - at,
                    kind: r.kind,
                    section: section.map(|s| s.name.clone()),
                };
                at = cut;
                piece
            })
            .collect()
    }
}

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<OwnedHandle> {
    // SAFETY: the handle is checked, then owned (closed on drop).
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

impl Memory for Process {
    fn read(&self, address: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut got = 0;
        // SAFETY: the buffer holds `buf.len()` bytes.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle.as_raw_handle(),
                address as *const c_void,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut got,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if got != buf.len() {
            return Err(io::Error::other(format!(
                "{got} of {} bytes read",
                buf.len()
            )));
        }
        Ok(())
    }

    fn regions(&self) -> io::Result<Vec<Region>> {
        let mut out = Vec::new();
        let mut at = 0u64;
        loop {
            // SAFETY: a zeroed MEMORY_BASIC_INFORMATION is valid, and VirtualQueryEx fills it in.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
            let size = size_of::<MEMORY_BASIC_INFORMATION>();
            let h = self.handle.as_raw_handle();
            // SAFETY: as above; `info` is `size` bytes.
            if unsafe { VirtualQueryEx(h, at as *const c_void, &mut info, size) } == 0 {
                // Past the top of user space: done. Failing on the first query is an error.
                if out.is_empty() {
                    return Err(io::Error::last_os_error());
                }
                return Ok(out);
            }
            let (base, size) = (info.BaseAddress as u64, info.RegionSize as u64);
            let readable = info.State == MEM_COMMIT
                && info.Protect != 0
                && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0;
            if readable {
                let kind = match info.Type {
                    MEM_IMAGE => Kind::Image,
                    MEM_MAPPED => Kind::Mapped,
                    _ => Kind::Private,
                };
                out.extend(self.split(Region {
                    base,
                    size,
                    kind,
                    section: None,
                }));
            }
            match base.checked_add(size) {
                Some(next) if next > at => at = next,
                _ => return Ok(out),
            }
        }
    }
}

impl Ctx<'_> {
    /// The game in the session's job, open for reading.
    pub(crate) fn game(&self) -> Result<Process, String> {
        Process::open(self.game_pid()?, &self.config.game_exe)
    }

    /// The KB table `table` for `needed_for`, parsed. Refused with `needs` without it.
    pub(crate) fn kb<T, E: Display>(
        &self,
        table: &doctor::Dep,
        needed_for: &str,
        parse: fn(&str) -> Result<T, E>,
    ) -> Result<T, Done> {
        doctor::kb_matches(self.config, &self.live.build)?;
        let path = doctor::require(table, self.config, needed_for)?.path;
        let path = path.unwrap_or_default();
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(parse(&text).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    /// Makes the planned write `w` in `game`, and puts it in `fields` (`write`). It is journaled first as `what`: its
    /// time taints every save the game writes after it. A write that changes nothing is skipped (`unchanged`).
    /// Refused if the journal can't be written; failed after that.
    pub(crate) fn write(
        &self,
        game: &Process,
        what: &str,
        w: &Write,
        fields: &mut Fields,
    ) -> Result<(), Done> {
        fields.insert("write".into(), json!(w));
        if w.old == w.new {
            fields.insert("unchanged".into(), json!(true));
            return Ok(());
        }
        let what = format!("{what} at {:#x}", w.address);
        let (mut journal, id) = self.journal(&what, Some(w), true)?;
        let written = game.write(w);
        let happened = !matches!(written, Err((_, false)));
        if let Some(note) = self.settle(&mut journal, id, happened) {
            fields.insert("journal_note".into(), json!(note));
        }
        written.map_err(|(why, maybe)| {
            fields.insert("maybe_written".into(), json!(maybe));
            Done::Failed(format!("{what}: {why}"), fields.clone())
        })
    }
}

/// [`ctx`] for a mem_* tool, refused while the KB's tables name another build than the session's exe: its
/// addresses come from them.
fn checked<'s>(session: &'s Session, args: &Value, known: &[&str]) -> Result<Ctx<'s>, String> {
    let cx = ctx(session, args, known)?;
    doctor::kb_matches(cx.config, &cx.live.build)?;
    Ok(cx)
}

/// `v` as an object's fields.
fn fields(v: Value) -> Fields {
    match v {
        Value::Object(map) => map,
        other => Map::from_iter([("value".to_string(), other)]),
    }
}

/// A whole number, as JSON gives it or as text: decimal, or `0x` hex, either with a `-` before it.
fn signed(v: &Value) -> Option<i128> {
    if let Some(n) = v.as_i64() {
        return Some(n.into());
    }
    if let Some(n) = v.as_u64() {
        return Some(n.into());
    }
    let s = v.as_str()?;
    let (negative, s) = s.strip_prefix('-').map_or((false, s), |rest| (true, rest));
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None => s.parse().ok()?,
    };
    Some(if negative { -n } else { n })
}

fn unsigned(v: &Value) -> Option<u64> {
    signed(v).and_then(|n| u64::try_from(n).ok())
}

/// The address `args` give: `key`, or `rva` from the game's image base; exactly one of them.
fn address(game: &Process, args: &Value, key: &str) -> Result<u64, String> {
    match (args.get(key), args.get("rva")) {
        (Some(v), None) => unsigned(v).ok_or(format!("{key} must be a number or 0x hex")),
        (None, Some(v)) => unsigned(v)
            .and_then(|r| u32::try_from(r).ok())
            .map(|r| game.image.base + u64::from(r))
            .ok_or("rva must be a number or 0x hex of at most 32 bits".into()),
        _ => Err(format!("give {key} or rva, one of them")),
    }
}

fn read(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = checked(session, args, &["address", "rva", "type", "len"])?;
    let ty = match text(args, "type")? {
        "u8" => Type::U8,
        "u16" => Type::U16,
        "u32" => Type::U32,
        "u64" => Type::U64,
        "i32" => Type::I32,
        "f32" => Type::F32,
        "f64" => Type::F64,
        "bytes" => Type::Bytes(
            args.get("len")
                .and_then(Value::as_u64)
                .ok_or_else(|| String::from("bytes needs len"))? as usize,
        ),
        other => return Err(format!("type {other:?}: u8 u16 u32 u64 i32 f32 f64 or bytes").into()),
    };
    if args.get("len").is_some() && !matches!(ty, Type::Bytes(_)) {
        return Err("len is for type bytes only".to_string().into());
    }
    let game = cx.game()?;
    let address = address(&game, args, "address")?;
    let value = mem::read(&game, address, ty).map_err(|e| format!("{address:#x}: {e}"))?;
    call.lap("read");
    Ok(fields(json!({"address": address, "value": value})))
}

fn scan(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = [
        "pattern", "mask", "ascii", "utf16", "sections", "regions", "from", "limit",
    ];
    let cx = checked(session, args, &known)?;
    let string = |key| {
        args.get(key)
            .map(|v| v.as_str().ok_or(format!("{key} must be a string")))
    };
    let pattern = match (string("pattern"), string("ascii"), string("utf16")) {
        (Some(p), None, None) => Pattern::parse(p?),
        (None, Some(a), None) => Pattern::ascii(a?),
        (None, None, Some(u)) => Pattern::utf16le(u?),
        _ => Err("give one of pattern, ascii and utf16".into()),
    }?;
    let pattern = match string("mask") {
        Some(_) if args.get("pattern").is_none() => {
            return Err("mask is for pattern only".to_string().into());
        }
        Some(mask) => pattern.masked(mask?)?,
        None => pattern,
    };
    let list = |key: &str| {
        args.get(key)
            .map(|v| v.as_array().ok_or(format!("{key} must be an array")))
    };
    let scope = match (list("sections"), list("regions")) {
        (None, None) => Scope::All,
        (Some(s), None) => Scope::Sections(
            s?.iter()
                .map(|v| v.as_str().map(String::from))
                .collect::<Option<_>>()
                .ok_or_else(|| String::from("sections must be names"))?,
        ),
        (None, Some(r)) => Scope::Regions(
            r?.iter()
                .map(unsigned)
                .collect::<Option<_>>()
                .ok_or_else(|| String::from("regions must be bases: numbers or 0x hex"))?,
        ),
        _ => return Err("sections or regions, not both".to_string().into()),
    };
    let from = match args.get("from") {
        Some(v) => unsigned(v).ok_or_else(|| String::from("from must be a number or 0x hex"))?,
        None => 0,
    };
    let limit = match args.get("limit") {
        Some(v) => {
            v.as_u64()
                .ok_or_else(|| String::from("limit must be a whole number"))? as usize
        }
        None => LIMIT,
    };
    let game = cx.game()?;
    let page = mem::scan(&game, &pattern, &scope, from, limit)
        .map_err(|e| format!("the game's regions: {e}"))?;
    call.lap("scan");
    Ok(fields(json!(page)))
}

fn follow(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = checked(session, args, &["base", "rva", "offsets"])?;
    let offsets: Vec<i64> = args
        .get("offsets")
        .and_then(Value::as_array)
        .and_then(|o| o.iter().map(|v| signed(v)?.try_into().ok()).collect())
        .ok_or_else(|| String::from("offsets must be an array of whole numbers or (-)0x hex"))?;
    let game = cx.game()?;
    let base = address(&game, args, "base")?;
    let chain = mem::follow(&game, base, &offsets);
    call.lap("follow");
    Ok(fields(json!(chain)))
}

fn patch(session: &Session, call: &mut Call, args: &Value, op: Op) -> Result<Fields, Done> {
    let cx = checked(session, args, &["name"])?;
    let name = text(args, "name")?;
    let needed_for = "mem_patch, mem_unpatch";
    let patches = cx.kb(&doctor::KB_PATCHES, needed_for, mem::from_patches_tsv)?;
    let game = cx.game()?;
    let image = &game.image;
    let w = mem::plan(&patches, name, op, &image.stamp, image.base, &game)
        .map_err(|r| format!("{name}: {r}"))?;
    call.lap("plan");
    let what = match op {
        Op::Patch => format!("mem_patch {name}"),
        Op::Unpatch => format!("mem_unpatch {name}"),
    };
    let mut fields = Map::new();
    fields.insert("build".into(), json!(image.stamp));
    cx.write(&game, &what, &w, &mut fields)?;
    call.lap("write");
    let mut now = vec![0; w.new.len()];
    match game.read(w.address, &mut now) {
        Ok(()) => fields.insert("now".into(), json!(mem::Value::Bytes(now))),
        Err(e) => fields.insert("now".into(), json!(format!("can't read it back: {e}"))),
    };
    Ok(fields)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::game::tests::Fake;
    use crate::win::Job;
    use std::cell::UnsafeCell;
    use std::fs::File;
    use std::mem::offset_of;
    use std::path::Path;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    /// A static of this test binary's that another process may write: the cell keeps it in a writable section.
    #[repr(transparent)]
    pub(crate) struct Cell<T>(UnsafeCell<T>);

    // SAFETY: nothing in this process writes it; tests write the dummy game's copy, another process.
    unsafe impl<T> Sync for Cell<T> {}

    impl<T: Copy> Cell<T> {
        pub(crate) fn address(&self) -> u64 {
            self.0.get() as u64
        }

        /// This process's copy, never written: the value the dummy game starts with.
        pub(crate) fn initial(&self) -> T {
            // SAFETY: nothing writes this process's copy.
            unsafe { *self.0.get() }
        }
    }

    /// The dummy game's cvar `QuenTest/FrameMs`, laid out by a layout of the tests' own, not any game's.
    #[repr(C, align(8))]
    #[derive(Clone, Copy)]
    pub(crate) struct DummyVar {
        vtable: u64,
        name: *const u8,
        group: *const u8,
        default: f32,
        pub(crate) live: f32,
        flags: u32,
        pad: u32,
        value: *const f32,
    }

    /// The dummy var's float type code.
    const FLOAT: u8 = 9;
    static NAME: [u8; 9] = *b"\0FrameMs\0";
    static GROUP: [u8; 9] = *b"QuenTest\0";
    pub(crate) static VAR: Cell<DummyVar> = Cell(UnsafeCell::new(DummyVar {
        vtable: 0,
        // SAFETY: inside NAME, after its leading NUL.
        name: unsafe { NAME.as_ptr().add(1) },
        group: GROUP.as_ptr(),
        default: 20.0,
        live: 20.0,
        flags: 0x200 | FLOAT as u32,
        pad: 0,
        // SAFETY: inside VAR: its live value.
        value: unsafe {
            (&raw const VAR)
                .cast::<u8>()
                .add(offset_of!(DummyVar, live))
                .cast()
        },
    }));

    #[repr(C, align(8))]
    #[derive(Clone, Copy)]
    pub(crate) struct Aligned(pub(crate) [u8; 16]);

    /// A known buffer: a string to scan for, and a qword to patch.
    pub(crate) static BUF: Cell<Aligned> =
        Cell(UnsafeCell::new(Aligned(*b"quen-dummy-buf\x01\x02")));
    /// A pointer chain's first hop: BUF's address.
    static HOP: Cell<*const u8> = Cell(UnsafeCell::new((&raw const BUF).cast()));

    /// The dummy game's `cvar_layout.tsv`, for `build`.
    pub(crate) fn layout_tsv(build: &str) -> String {
        let o = |offset: usize| format!("{offset:#x}");
        format!(
            "build\tgroup\tname\tflags\tvalue\tdefault\tlive\tbool\tint\tfloat\n\
             {build}\t{}\t{}\t{}\t{}\t{}\t{}\t7\t8\t{FLOAT}\n",
            o(offset_of!(DummyVar, group)),
            o(offset_of!(DummyVar, name)),
            o(offset_of!(DummyVar, flags)),
            o(offset_of!(DummyVar, value)),
            o(offset_of!(DummyVar, default)),
            o(offset_of!(DummyVar, live)),
        )
    }

    /// This test binary's file name, and its image as loaded here (the dummy game's is the same file).
    pub(crate) fn this_exe() -> (String, Image) {
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        let image = Process::open(std::process::id(), &name).unwrap().image;
        (name, image)
    }

    /// Starts the dummy game in `job`: this test binary again, running only [`dummy_game`], which sleeps. Returns once
    /// its image is loaded.
    pub(crate) fn spawn_dummy(job: &Job, dir: &Path) {
        let exe = std::env::current_exe().unwrap();
        let argv = [
            &exe.display().to_string(),
            "--exact",
            "mem::tests::dummy_game",
            "--ignored",
        ]
        .map(String::from);
        let log = File::create(dir.join("game.log")).unwrap();
        job.spawn(&argv, &[("QUEN_DUMMY_GAME", "1")], dir, &log)
            .unwrap();
        let name = this_exe().0;
        let start = Instant::now();
        while !job
            .pids()
            .unwrap()
            .iter()
            .any(|&pid| Process::open(pid, &name).is_ok())
        {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the dummy game did not start"
            );
            sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "the dummy game other tests start: it only sleeps"]
    fn dummy_game() {
        if std::env::var_os("QUEN_DUMMY_GAME").is_some() {
            std::hint::black_box((&VAR, &BUF, &HOP));
            sleep(Duration::from_secs(120));
        }
    }

    fn rva(cell_address: u64) -> u64 {
        cell_address - this_exe().1.base
    }

    #[test]
    fn reads_scans_and_follows_in_the_dummy_game() {
        let fx = Fake::new("memread");
        let (session, (_, mine)) = (fx.session(), this_exe());
        let cx = ctx(&session, &json!({"token": "tok"}), &[]).unwrap();
        let game = cx.game().unwrap();
        assert_ne!(game.pid, std::process::id());
        assert_eq!(game.image.stamp, mine.stamp);
        let buf = game.image.base + rva(BUF.address());
        let regions = game.regions().unwrap();
        let at = regions
            .iter()
            .find(|r| r.base <= buf && buf < r.base + r.size)
            .unwrap();
        assert_eq!(
            (at.kind, at.section.as_deref()),
            (Kind::Image, Some(".data")),
            "{at:?}"
        );

        let out = fx.run(
            "mem_read",
            json!({"token": "tok", "rva": rva(BUF.address()), "type": "bytes", "len": 16}),
        );
        assert_eq!(
            out["value"], "71 75 65 6e 2d 64 75 6d 6d 79 2d 62 75 66 01 02",
            "{out}"
        );
        let out = fx.run(
            "mem_read",
            json!({"token": "tok", "address": format!("{:#x}", buf + 14), "type": "u16"}),
        );
        assert_eq!(out["value"], 0x0201, "{out}");
        let scan = fx.run(
            "mem_scan",
            json!({"token": "tok", "ascii": "quen-dummy-buf", "sections": [".data"]}),
        );
        assert_eq!(scan["hits"], json!([buf]), "{scan}");
        let page = fx.run(
            "mem_scan",
            json!({"token": "tok", "pattern": "71 75 65 6e 2d ?? 75", "limit": 1}),
        );
        assert_eq!(page["hits"].as_array().unwrap().len(), 1, "{page}");
        assert!(page["next"].is_u64(), "a full page names the next: {page}");
        let hop = rva(HOP.address());
        let chain = fx.run(
            "mem_follow",
            json!({"token": "tok", "rva": format!("{hop:#x}"), "offsets": [0, "0x8"]}),
        );
        let second = u64::from_le_bytes(BUF.initial().0[8..].try_into().unwrap());
        assert_eq!(
            (chain["end"].as_u64(), chain["hops"][0]["value"].as_u64()),
            (Some(second), Some(buf)),
            "{chain}"
        );
        for args in [
            json!({"token": "tok", "type": "u8"}),
            json!({"token": "tok", "address": 1, "rva": 1, "type": "u8"}),
            json!({"token": "tok", "address": 1, "type": "bytes"}),
            json!({"token": "tok", "address": 1, "type": "u8", "len": 1}),
            json!({"token": "tok", "address": "0xZZ", "type": "u8"}),
            json!({"token": "nope", "address": 1, "type": "u8"}),
        ] {
            assert_eq!(fx.run("mem_read", args.clone())["refused"], true, "{args}");
        }
        let unmapped = fx.run(
            "mem_read",
            json!({"token": "tok", "address": 8, "type": "u8"}),
        );
        assert_eq!(unmapped["refused"], true, "{unmapped}");
        for args in [
            json!({"token": "tok"}),
            json!({"token": "tok", "ascii": "a", "utf16": "a"}),
            json!({"token": "tok", "ascii": "a", "mask": "ff"}),
            json!({"token": "tok", "pattern": "??"}),
            json!({"token": "tok", "ascii": "a", "sections": [".data"], "regions": [1]}),
        ] {
            assert_eq!(fx.run("mem_scan", args.clone())["refused"], true, "{args}");
        }
        assert_eq!(
            fx.run(
                "mem_follow",
                json!({"token": "tok", "base": 8, "offsets": ["x"]})
            )["refused"],
            true
        );
    }

    #[test]
    fn patches_write_only_over_their_bytes_and_are_journaled() {
        let fx = Fake::new("mempatch");
        let (_, mine) = this_exe();
        let at = rva(BUF.address());
        let row = format!(
            "dummy\t{at:#x}\t71 75 65 6e\t51 55 45 4e\t{}\ttest\n",
            mine.stamp
        );
        let tsv = format!(
            "name\trva\told\tnew\tbuild\tboundaries-checked\n{row}other\t{at:#x}\t00\t01\t0x00000000\tyes\n"
        );
        fs::write(fx.dir.join("kb/patches.tsv"), tsv).unwrap();
        let read = |fx: &Fake| {
            fx.run(
                "mem_read",
                json!({"token": "tok", "rva": at, "type": "bytes", "len": 4}),
            )["value"]
                .clone()
        };
        let on = fx.run("mem_patch", json!({"token": "tok", "name": "dummy"}));
        assert_eq!(on["ok"], true, "{on}");
        assert_eq!(
            (on["now"].clone(), on["write"]["old"].clone()),
            (json!("51 55 45 4e"), json!("71 75 65 6e"))
        );
        assert_eq!(read(&fx), "51 55 45 4e");
        assert_eq!(
            BUF.initial().0[..4],
            *b"quen",
            "the dummy's copy was written, not ours"
        );
        let again = fx.run("mem_patch", json!({"token": "tok", "name": "dummy"}));
        assert!(
            again["refused"] == true
                && again["reason"]
                    .as_str()
                    .unwrap()
                    .contains("already patched"),
            "{again}"
        );
        let off = fx.run("mem_unpatch", json!({"token": "tok", "name": "dummy"}));
        assert_eq!(
            (off["ok"].clone(), read(&fx)),
            (json!(true), json!("71 75 65 6e")),
            "{off}"
        );
        let twice = fx.run("mem_unpatch", json!({"token": "tok", "name": "dummy"}));
        assert!(
            twice["reason"].as_str().unwrap().contains("not patched"),
            "{twice}"
        );
        let other = fx.run("mem_patch", json!({"token": "tok", "name": "other"}));
        assert!(
            other["reason"]
                .as_str()
                .unwrap()
                .contains("not the running"),
            "{other}"
        );
        let none = fx.run("mem_patch", json!({"token": "tok", "name": "nope"}));
        assert!(
            none["reason"].as_str().unwrap().contains("no patch named"),
            "{none}"
        );
        let journal = fs::read_to_string(fx.dir.join("data/state/journal.jsonl")).unwrap();
        let steps: Vec<&str> = journal
            .lines()
            .filter(|l| l.contains("game_memory"))
            .collect();
        assert_eq!(steps.len(), 2, "{journal}");
        assert!(
            steps[0].contains("mem_patch dummy at 0x") && steps[1].contains("mem_unpatch dummy"),
            "{journal}"
        );
        // Without the table, or without a KB in the config: refused with needs, nothing read or written.
        fs::remove_file(fx.dir.join("kb/patches.tsv")).unwrap(); // this test's own file
        let gone = fx.run("mem_patch", json!({"token": "tok", "name": "dummy"}));
        assert!(
            gone["reason"].as_str().unwrap().contains("patches.tsv"),
            "{gone}"
        );
        assert_eq!(
            gone["needs"][0]["dependency"],
            crate::doctor::KB_PATCHES.name,
            "{gone}"
        );
        let mut config = fx.shared.config().as_ref().clone().unwrap();
        config.kb = None;
        fx.shared.set_config(Ok(config));
        let none = fx.run("mem_patch", json!({"token": "tok", "name": "dummy"}));
        assert_eq!(none["refused"], true, "{none}");
        assert_eq!(
            none["needs"][0]["dependency"],
            crate::doctor::KB_PATCHES.name,
            "{none}"
        );
    }

    /// While the KB's tables name a build, the mem_* tools and game_cvar refuse a session whose exe is another.
    #[test]
    fn a_kb_of_another_build_is_refused() {
        let fx = Fake::new("kbbuild");
        let read = json!({"token": "tok", "rva": rva(BUF.address()), "type": "u8"});
        let names = |sha: &str| {
            let line = format!(
                "# build: sha256={sha} stamp=0x00000000
rva	name
"
            );
            fs::write(fx.dir.join("kb/names.tsv"), line).unwrap();
        };
        // Fake's session read sha256 0…0, stamp 0, at its launch.
        names(&"0".repeat(64));
        let out = fx.run("mem_read", read.clone());
        assert_eq!(out["ok"], true, "{out}");
        names(&"1".repeat(64));
        for (tool, args) in [
            ("mem_read", read),
            (
                "mem_scan",
                json!({"token": "tok", "ascii": "quen-dummy-buf"}),
            ),
            ("mem_patch", json!({"token": "tok", "name": "dummy"})),
            ("game_cvar", json!({"token": "tok", "name": "X"})),
        ] {
            let out = fx.run(tool, args);
            assert_eq!(out["refused"], true, "{tool}: {out}");
            let why = out["reason"].as_str().unwrap();
            assert!(why.contains("the KB is for sha256 1111"), "{tool}: {why}");
        }
    }

    #[test]
    fn regions_split_at_sections() {
        let (exe, image) = this_exe();
        let process = Process::open(std::process::id(), &exe).unwrap();
        let text = image.sections.iter().find(|s| s.name == ".text").unwrap();
        let whole = Region {
            base: image.base,
            size: text.base + text.size + 0x10_0000 - image.base,
            kind: Kind::Image,
            section: None,
        };
        let pieces = process.split(whole.clone());
        assert_eq!(
            (pieces[0].base, pieces[0].section.as_deref()),
            (image.base, None),
            "the headers"
        );
        assert_eq!(
            (pieces[1].base, pieces[1].section.as_deref()),
            (text.base, Some(".text"))
        );
        assert!(
            pieces
                .windows(2)
                .all(|w| w[0].base + w[0].size == w[1].base),
            "{pieces:?}"
        );
        let last = pieces.last().unwrap();
        assert_eq!(last.base + last.size, whole.base + whole.size);
    }
}
