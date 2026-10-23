//! Nsight Graphics frame captures (`.ngfx-capture`) read without Nsight's window: ngfx-replay's metadata cleaned of the
//! machine's identity, the counts of its function and object lists, its perf report's iteration times, GPU Trace's
//! frame time, and whether Nsight asked its first-start question; the shader containers and the LZ4 blocks they are
//! stored in, for `ngfx_capture`'s reader. Pure: quen's ngfx.rs runs ngfx-replay and writes the files and rows.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The Nsight Graphics version quen's capture reader and shader decoder were proven on (one capture: 733 of 733
/// shaders, every call and object lined up with ngfx-replay's lists).
pub const DECODER_PROVEN_ON: &str = "2026.3.1";
/// The largest shader container the decoder takes, as the RTX Hair session's script.
pub const SHADER_MAX: usize = 8 << 20;

/// Counts as measurement metrics and their values.
pub type Counts = Vec<(&'static str, u64)>;

/// One shader container decoded from a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shader {
    /// The container's own hash (bytes 4..20), as hex: its file name.
    pub hash: String,
    /// From the DXIL, SHEX or SHDR part's version token: pixel, vertex, ..., or `?`.
    pub stage: &'static str,
    /// `<major>_<minor>`, or `?`.
    pub model: String,
    /// The part names in order (RDEF, ISGN, SHEX, DXIL, ...).
    pub parts: Vec<String>,
    pub bytes: Vec<u8>,
    /// The capture's chunks that hold it, in id order.
    pub chunks: Vec<u64>,
}

/// What `ngfx_capture::Capture::shaders` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShaderScan {
    /// Chunks that start with `DXBC`.
    pub found: usize,
    /// Found but not a valid container.
    pub failed: usize,
    /// A container already in `shaders`, in another chunk.
    pub duplicates: usize,
    /// Unique by hash, in chunk order.
    pub shaders: Vec<Shader>,
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// An LZ4 block decoded from `src[pos..]` until `want` bytes are out; None if it goes wrong (out of bounds, a match
/// before the start). Nsight 2026.3 stores its chunks as raw LZ4 blocks.
pub(crate) fn lz4_block(src: &[u8], mut pos: usize, want: usize) -> Option<Vec<u8>> {
    // A length's 255-continued extension, read from `pos`.
    fn more(src: &[u8], pos: &mut usize, mut n: usize) -> Option<usize> {
        loop {
            let b = *src.get(*pos)?;
            *pos += 1;
            n += usize::from(b);
            if b != 255 {
                return Some(n);
            }
        }
    }
    let mut out = Vec::with_capacity(want);
    while out.len() < want {
        let tok = *src.get(pos)?;
        pos += 1;
        let mut lit = usize::from(tok >> 4);
        if lit == 15 {
            lit = more(src, &mut pos, lit)?;
        }
        out.extend_from_slice(src.get(pos..pos + lit.min(want - out.len()))?);
        pos += lit;
        if out.len() >= want {
            break;
        }
        let off = usize::from(*src.get(pos)?) | usize::from(*src.get(pos + 1)?) << 8;
        pos += 2;
        if off == 0 || off > out.len() {
            return None;
        }
        let mut len = usize::from(tok & 15) + 4;
        if tok & 15 == 15 {
            len = more(src, &mut pos, len)?;
        }
        // What lies past `want` is cut anyway: a garbage length can't grow the buffer.
        for _ in 0..len.min(want - out.len()) {
            out.push(out[out.len() - off]);
        }
    }
    Some(out)
}

/// The part offsets of a DXBC container whose part table points at four-letter part names.
fn parts(b: &[u8]) -> Option<Vec<usize>> {
    let n = u32_at(b, 28)? as usize;
    if !(1..=32).contains(&n) {
        return None;
    }
    (0..n)
        .map(|k| {
            let o = u32_at(b, 32 + 4 * k)? as usize;
            let name = b.get(o..o + 4)?;
            (o + 8 <= b.len() && name.iter().all(u8::is_ascii_alphanumeric)).then_some(o)
        })
        .collect()
}

impl Shader {
    /// `b` if it is a whole DXBC container (its magic, its own size, a part table of named parts), with its stage and
    /// model from its DXIL (SM6) or SHEX / SHDR (SM5) part; `chunks` left empty.
    pub(crate) fn decode(b: Vec<u8>) -> Option<Shader> {
        if !b.starts_with(b"DXBC") || u32_at(&b, 24)? as usize != b.len() {
            return None;
        }
        let offsets = parts(&b)?;
        let name = |o: usize| String::from_utf8_lossy(&b[o..o + 4]).into_owned();
        let version = offsets
            .iter()
            .find(|&&o| matches!(&b[o..o + 4], b"DXIL" | b"SHEX" | b"SHDR"))
            .and_then(|&o| u32_at(&b, o + 8));
        let stage = match version.map(|v| v >> 16) {
            Some(0) => "pixel",
            Some(1) => "vertex",
            Some(2) => "geometry",
            Some(3) => "hull",
            Some(4) => "domain",
            Some(5) => "compute",
            Some(6) => "library",
            Some(13) => "mesh",
            Some(14) => "amplification",
            _ => "?",
        };
        Some(Shader {
            hash: b[4..20].iter().map(|x| format!("{x:02x}")).collect(),
            stage,
            model: version.map_or("?".into(), |v| format!("{}_{}", v >> 4 & 15, v & 15)),
            parts: offsets.iter().map(|&o| name(o)).collect(),
            bytes: b,
            chunks: Vec::new(),
        })
    }
}

/// `text` from its first `open` on (`{` or `[`), past a byte-order mark or log lines before it. Without one, what
/// ngfx-replay said instead (its first line, cut at 300 characters): `--metadata-functions` exits 0 after "ERROR: File
/// does not exist".
pub(crate) fn json_start(text: &str, open: char) -> Result<&str, String> {
    text.find(open).map(|i| &text[i..]).ok_or_else(|| {
        let said: String = text
            .trim_start_matches('\u{feff}')
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(300)
            .collect();
        format!("no JSON {open} in ngfx-replay's output; it said: {said:?}")
    })
}

/// The first JSON value in `text`, ignoring what follows it (a log line).
pub(crate) fn first<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, String> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<T>()
        .next()
        .ok_or("empty output")?
        .map_err(|e| format!("not ngfx-replay's JSON: {e}"))
}

/// The metadata fields quen knows (Nsight Graphics 2026.3.1, metadata_version 1); any other is left out of the cleaned
/// copy and named in [`Cleaned::unknown_fields`].
const KNOWN: [&str; 23] = [
    "_metadata_collection_",
    "captured_frame",
    "d3d12_core_version",
    "driver_vendor",
    "driver_version",
    "graphics_apis",
    "graphics_features",
    "has_unsupported_operation",
    "metadata_version",
    "non_portable",
    "nsight_version",
    "nsight_version_build_id",
    "os_information",
    "primary_api",
    "primary_gpu",
    "process_command_line",
    "process_environment",
    "process_name",
    "replayer_bundle",
    "request_time",
    "resolution",
    "user_files",
    "uuid",
];

/// The machine's identity in the environment, scrubbed from every string as `%NAME%` (longest value first).
pub const IDENTITY: [&str; 5] = [
    "USERPROFILE",
    "USERNAME",
    "COMPUTERNAME",
    "USERDOMAIN",
    "LOGONSERVER",
];

/// A capture's metadata, cleaned.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    /// What metadata.json holds.
    pub json: Value,
    /// A few facts about the frame, for the result and the capture's row.
    pub facts: Map<String, Value>,
    /// Top-level fields quen doesn't know, by name only: left out.
    pub unknown_fields: Vec<String>,
}

/// Whether an environment variable is dropped from the cleaned copy, its name too.
fn dropped(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    [
        "NV_TPS_LAUNCH_TOKEN",
        "COMPUTERNAME",
        "USERNAME",
        "LOGONSERVER",
    ]
    .contains(&n.as_str())
        || n.starts_with("USERDOMAIN")
        || n.starts_with("ONEDRIVE")
}

/// Whether its value is kept: Nsight's own capture knobs and Steam's ids.
fn valued(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("NV_GRAPHICS_CAPTURE_") || n == "STEAMAPPID" || n == "STEAMGAMEID"
}

/// `text` with every `value` (ASCII case aside, `\` and `/` alike) not inside a longer run of letters and digits
/// replaced by `placeholder` (`NAME-PC` loses its `NAME` too: better scrubbed than leaked).
fn scrub(text: &str, value: &str, placeholder: &str) -> String {
    let (t, v) = (text.as_bytes(), value.as_bytes());
    let word = |b: Option<&u8>| b.is_some_and(u8::is_ascii_alphanumeric);
    let same =
        |a: &u8, b: &u8| a.eq_ignore_ascii_case(b) || (b"\\/".contains(a) && b"\\/".contains(b));
    let (mut out, mut last, mut i) = (String::with_capacity(text.len()), 0, 0);
    while !v.is_empty() && i + v.len() <= t.len() {
        let here = t[i..i + v.len()].iter().zip(v).all(|(a, b)| same(a, b))
            && !word(i.checked_sub(1).and_then(|p| t.get(p)))
            && !word(t.get(i + v.len()));
        if here && text.is_char_boundary(i) && text.is_char_boundary(i + v.len()) {
            out.push_str(&text[last..i]);
            out.push_str(placeholder);
            i += v.len();
            last = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// The machine's identity to scrub, as `(%NAME%, value)`, longest value first: `local`'s (this machine's) and `env`'s
/// (a capture's own environment) values of [`IDENTITY`].
pub fn identity(local: &[(&str, &str)], env: &[(String, String)]) -> Vec<(String, String)> {
    let mut identity: Vec<(String, String)> = Vec::new();
    for name in IDENTITY {
        let ours = local.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
        let theirs = env.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
        for v in ours.map(|(_, v)| *v).chain(theirs.map(|(_, v)| v.as_str())) {
            let v = v.trim_end_matches(['\\', '/']).trim_start_matches('\\');
            if v.len() > 1 && !identity.iter().any(|(_, x)| x.eq_ignore_ascii_case(v)) {
                identity.push((format!("%{name}%"), v.to_owned()));
            }
        }
    }
    identity.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    identity
}

/// `text` with each `(placeholder, value)` of `identity` scrubbed ([`scrub`]).
pub fn scrub_identity(text: &str, identity: &[(String, String)]) -> String {
    identity
        .iter()
        .fold(text.to_owned(), |text, (placeholder, v)| {
            scrub(&text, v, placeholder)
        })
}

/// `value` with each `(placeholder, value)` of `identity` scrubbed from its strings and keys.
fn scrubbed(value: Value, identity: &[(String, String)]) -> Value {
    let s = |text: String| scrub_identity(&text, identity);
    match value {
        Value::String(text) => Value::String(s(text)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| scrubbed(v, identity)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (s(k), scrubbed(v, identity)))
                .collect(),
        ),
        other => other,
    }
}

/// `ngfx-replay --metadata`'s output, cleaned (nsight-capture design §5, the owner's answer 7): the known fields kept;
/// `process_environment` reduced to its variables' names, sorted, with values only for Nsight's `NV_GRAPHICS_CAPTURE_*`
/// knobs and Steam's ids, and the machine's identity (the token, computer, user and domain names, OneDrive) dropped,
/// names included; every string scrubbed of the identity's values as `%USERPROFILE%`, `%USERNAME%`, ...: those the
/// metadata's own environment holds, and `local`'s (this machine's, which a quen launch's allow-list leaves out of
/// the game's environment). The raw text is parsed here and never kept.
pub fn clean_metadata(text: &str, local: &[(&str, &str)]) -> Result<Cleaned, String> {
    let raw: Map<String, Value> = first(json_start(text, '{')?)?;
    let env: Vec<(String, String)> = raw
        .get("process_environment")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|var| match var.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (var.to_owned(), String::new()),
        })
        .collect();
    let identity = identity(local, &env);
    let mut out = Map::new();
    let mut unknown_fields = Vec::new();
    for (key, value) in raw {
        if !KNOWN.contains(&key.as_str()) {
            unknown_fields.push(key);
            continue;
        }
        let value = match key.as_str() {
            "process_environment" => Value::Object(
                env.iter()
                    .filter(|(k, _)| !dropped(k))
                    .map(|(k, v)| (k.clone(), if valued(k) { json!(v) } else { Value::Null }))
                    .collect(),
            ),
            _ => value,
        };
        out.insert(key, value);
    }
    let json = scrubbed(Value::Object(out), &identity);
    Ok(Cleaned {
        facts: metadata_facts(&json),
        json,
        unknown_fields,
    })
}

/// A few facts about the frame from a capture's metadata (raw or cleaned): frame, resolution, API, Nsight version and
/// build, whether it holds an unsupported operation, the graphics features' names.
pub fn metadata_facts(json: &Value) -> Map<String, Value> {
    let mut facts: Map<String, Value> = [
        ("captured_frame", "frame"),
        ("resolution", "resolution"),
        ("primary_api", "primary_api"),
        ("nsight_version", "nsight_version"),
        ("nsight_version_build_id", "nsight_build_id"),
        ("has_unsupported_operation", "has_unsupported_operation"),
    ]
    .into_iter()
    .filter_map(|(from, to)| Some((to.to_owned(), json.get(from)?.clone())))
    .collect();
    if let Some(features) = json["graphics_features"].as_object() {
        facts.insert(
            "graphics_features".into(),
            json!(features.keys().collect::<Vec<_>>()),
        );
    }
    facts
}

/// The counts `--metadata-functions` gives (D3D12 names: `<interface>_<method>`), as measurement metrics.
pub fn count_functions(text: &str) -> Result<Counts, String> {
    #[derive(Deserialize)]
    struct Event {
        function_name: String,
    }
    let events: Vec<Event> = first(json_start(text, '[')?)?;
    let mut out = vec![
        ("capture_events", events.len() as u64),
        ("capture_draws", 0),
        ("capture_indexed_draws", 0),
        ("capture_indirect_calls", 0),
        ("capture_dispatches", 0),
        ("capture_as_builds", 0),
        ("capture_ray_dispatches", 0),
    ];
    for e in &events {
        let method = e.function_name.split_once('_').map_or("", |(_, m)| m);
        let k = match method {
            "DrawInstanced" => 1,
            "DrawIndexedInstanced" => 2,
            "ExecuteIndirect" => 3,
            "Dispatch" => 4,
            "BuildRaytracingAccelerationStructure" => 5,
            "DispatchRays" => 6,
            _ => continue,
        };
        out[k].1 += 1;
    }
    Ok(out)
}

/// The counts `--metadata-objects` gives, as measurement metrics (every type's count is in its file).
pub fn count_objects(text: &str) -> Result<Counts, String> {
    #[derive(Deserialize)]
    struct Object {
        type_name: String,
    }
    let objects: Vec<Object> = first(json_start(text, '[')?)?;
    let n = |t: &str| objects.iter().filter(|o| o.type_name == t).count() as u64;
    Ok(vec![
        ("capture_objects", objects.len() as u64),
        ("capture_resources", n("Resource")),
        (
            "capture_acceleration_structures",
            n("AccelerationStructure"),
        ),
        ("capture_pipeline_states", n("PipelineState")),
        ("capture_state_objects", n("StateObject")),
    ])
}

/// What a perf replay's `iteration_times.csv` says.
#[derive(Debug, Clone, PartialEq)]
pub struct Iterations {
    pub iterations: usize,
    /// Iteration 0's mean absolute error against the captured frame (`mae`): how complete the capture is.
    pub image_error: Option<f64>,
    /// The median over iterations 1.. (iteration 0 warms up) of `msGpuTime` (-1: not collected) and `msFrameTime`.
    pub gpu_frame_ms: Option<f64>,
    pub cpu_frame_ms: Option<f64>,
}

/// ngfx-replay's `iteration_times.csv` (`i, replayTotalFps, ..., msFrameTime, msGpuTime, numFrames, mae, ...`).
pub fn iteration_times(csv: &str) -> Result<Iterations, String> {
    let mut lines = csv.trim_start_matches('\u{feff}').lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or("empty CSV")?
        .split(',')
        .map(str::trim)
        .collect();
    let col = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or(format!("no {name} column"))
    };
    let (i, mae, gpu, cpu) = (
        col("i")?,
        col("mae")?,
        col("msGpuTime")?,
        col("msFrameTime")?,
    );
    let rows: Vec<Vec<f64>> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split(',')
                .map(|c| c.trim().parse().unwrap_or(f64::NAN))
                .collect()
        })
        .collect();
    let at = |r: &Vec<f64>, c: usize| r.get(c).copied().filter(|v| v.is_finite());
    let later = |c: usize| {
        let v: Vec<f64> = rows
            .iter()
            .filter(|r| at(r, i).is_some_and(|i| i >= 1.0))
            .filter_map(|r| at(r, c).filter(|v| *v >= 0.0))
            .collect();
        crate::trace::median(v)
    };
    Ok(Iterations {
        iterations: rows.len(),
        image_error: rows
            .iter()
            .find(|r| at(r, i) == Some(0.0))
            .and_then(|r| at(r, mae).filter(|v| *v >= 0.0)),
        gpu_frame_ms: later(gpu),
        cpu_frame_ms: later(cpu),
    })
}

/// GPU Trace's `FRAME.xls` (tab-separated text): its `GPU frame time` in ms.
pub fn gpu_frame_ms(xls: &str) -> Option<f64> {
    xls.lines().find_map(|l| {
        let (k, v) = l.split_once('\t')?;
        (k.trim() == "GPU frame time").then(|| v.trim().parse().ok())?
    })
}

/// Whether Nsight Graphics `version` (its exe's file version, `2026.3.1.0`) asked its first-start privacy question, per
/// its settings file (`%APPDATA%\NVIDIA Corporation\NVIDIA Nsight Graphics.ini`, read only): `[CorePlugin.Privacy]`
/// `AskedFor_NVIDIA Nsight Graphics_<version>` set (Qt writes the spaces as `%20`).
pub fn privacy_asked(ini: &str, version: &str) -> bool {
    let key = format!("AskedFor_NVIDIA Nsight Graphics_{version}");
    let mut section = false;
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line == "[CorePlugin.Privacy]";
        } else if let Some((k, v)) = line.split_once('=')
            && section
            && k.trim().replace("%20", " ") == key
            && !v.trim().is_empty()
        {
            return true;
        }
    }
    false
}

/// Whether dotted version `a` is newer than `b` (`2026.3.1` vs `2026.3.1.0`: not); false when either isn't numbers.
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
    };
    let (Ok(mut a), Ok(mut b)) = (parse(a), parse(b)) else {
        return false;
    };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic DXBC container: one SHEX part (compute, model 5_1) with `pad` zero bytes, and `seed` in its hash.
    pub(crate) fn container(seed: u8, pad: usize) -> Vec<u8> {
        let size = 28 + 4 + 4 + 8 + 4 + pad;
        let mut b = b"DXBC".to_vec();
        b.extend([seed; 16]);
        b.extend(1u32.to_le_bytes());
        b.extend((size as u32).to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(36u32.to_le_bytes());
        b.extend(b"SHEX");
        b.extend(((4 + pad) as u32).to_le_bytes());
        b.extend(0x0005_0051u32.to_le_bytes());
        b.extend(vec![0u8; pad]);
        assert_eq!(b.len(), size);
        b
    }

    /// `b` as one LZ4 block: its first `lit` bytes as literals (the header, its run length extended past 15), then a
    /// match of offset 1 for the rest, which must repeat the last literal.
    fn lz4(b: &[u8], lit: usize) -> Vec<u8> {
        let rest = b.len() - lit;
        assert!(lit >= 15 && rest >= 4 && b[lit..].iter().all(|x| *x == b[lit - 1]));
        let mut out = vec![0xF0 | (rest - 4).min(15) as u8];
        let mut n = lit - 15;
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
        out.extend(&b[..lit]);
        out.extend([1, 0]);
        if rest - 4 >= 15 {
            let mut m = rest - 4 - 15;
            while m >= 255 {
                out.push(255);
                m -= 255;
            }
            out.push(m as u8);
        }
        out
    }

    #[test]
    fn lz4_blocks_decode_and_garbage_fails() {
        let c = container(7, 300);
        let block = lz4(&c, 48);
        assert_eq!(lz4_block(&block, 0, c.len()).as_deref(), Some(&c[..]));
        // A match before the output's start, and a block cut short.
        assert_eq!(lz4_block(&[0x00, 5, 0], 0, 8), None);
        assert_eq!(lz4_block(&block[..30], 0, c.len()), None);
        // A garbage length can't grow the output past what was asked.
        let mut big = vec![0x1F, 0xAA, 1, 0];
        big.extend([255u8; 1000]);
        big.push(0);
        assert_eq!(lz4_block(&big, 0, 64).map(|o| o.len()), Some(64));
    }

    #[test]
    fn containers_decode_and_others_fail() {
        let b = container(2, 4000);
        let s = Shader::decode(b.clone()).unwrap();
        assert_eq!(s.hash, "02".repeat(16));
        assert_eq!(
            (s.stage, s.model.as_str(), s.parts.clone()),
            ("compute", "5_1", vec!["SHEX".into()])
        );
        assert_eq!(s.bytes, b);
        // Cut short (its own size says more), not DXBC, a part table pointing past the end.
        assert!(Shader::decode(b[..b.len() - 1].to_vec()).is_none());
        assert!(Shader::decode(b"DXBX".iter().chain(&b[4..]).copied().collect()).is_none());
        let mut bad = b.clone();
        bad[32..36].copy_from_slice(&9000u32.to_le_bytes());
        assert!(Shader::decode(bad).is_none());
    }

    #[test]
    fn metadata_comes_out_cleaned() {
        let raw = json!({
            "captured_frame": "8931",
            "resolution": "3840x2160",
            "primary_api": "D3D12",
            "nsight_version": "2026.3.1",
            "nsight_version_build_id": "1",
            "has_unsupported_operation": false,
            "graphics_features": {"acceleration_structures": ["x"], "ray_dispatch": ["y"]},
            "process_name": r"P:\Profiles\Synth\Games\Game.exe",
            "process_command_line": r#""P:\Profiles\Synth\Games\Game.exe" -x"#,
            "user_files": [{"id": "1", "file_path": r"p:\profiles\synth\games\dlss.dll"}],
            "_metadata_collection_": {"info": ["host SYNTHBOX ran it"], "warnings": []},
            "process_environment": [
                r"USERPROFILE=P:\Profiles\Synth",
                "USERNAME=Synth",
                "COMPUTERNAME=SYNTHBOX",
                "USERDOMAIN=SYNTHBOX",
                "USERDOMAIN_ROAMINGPROFILE=SYNTHBOX",
                r"LOGONSERVER=\\SYNTHBOX",
                r"OneDrive=P:\Profiles\Synth\OneDrive",
                "NV_TPS_LAUNCH_TOKEN=secret-token",
                "NV_GRAPHICS_CAPTURE_OUTPUT_DIR=P:/Profiles/Synth/Documents/Captures",
                "NV_GRAPHICS_CAPTURE_FRAME_COUNT=1",
                "SteamAppId=1",
                "CLAUDE_AGENT_SDK_VERSION=9",
                r"Path=P:\Profiles\Synth\bin;C:\Windows",
            ],
            "future_field": {"x": "y"},
        });
        let text = format!("\u{feff}a log line\n{raw}\ntrailing log line");
        let c = clean_metadata(&text, &[("COMPUTERNAME", "OTHERBOX")]).unwrap();
        let out = c.json.to_string();
        for gone in [
            "Synth",
            "synth",
            "SYNTHBOX",
            "secret-token",
            "OneDrive",
            "LOGONSERVER",
            "future_field",
            "\"9\"",
        ] {
            assert!(!out.contains(gone), "{gone} in {out}");
        }
        let env = &c.json["process_environment"];
        assert_eq!(env["CLAUDE_AGENT_SDK_VERSION"], Value::Null);
        assert_eq!(env["Path"], Value::Null);
        assert_eq!(env["USERPROFILE"], Value::Null);
        assert_eq!(env["SteamAppId"], "1");
        assert_eq!(env["NV_GRAPHICS_CAPTURE_FRAME_COUNT"], "1");
        assert_eq!(
            env["NV_GRAPHICS_CAPTURE_OUTPUT_DIR"],
            "%USERPROFILE%/Documents/Captures"
        );
        assert!(env.get("USERNAME").is_none() && env.get("USERDOMAIN_ROAMINGPROFILE").is_none());
        assert_eq!(c.json["process_name"], r"%USERPROFILE%\Games\Game.exe");
        assert_eq!(
            c.json["user_files"][0]["file_path"],
            r"%USERPROFILE%\games\dlss.dll"
        );
        assert_eq!(
            c.json["_metadata_collection_"]["info"][0],
            "host %COMPUTERNAME% ran it"
        );
        assert_eq!(c.unknown_fields, ["future_field"]);
        assert_eq!(c.facts["frame"], "8931");
        assert_eq!(
            c.facts["graphics_features"],
            json!(["acceleration_structures", "ray_dispatch"])
        );
        // This machine's name, absent from a quen launch's environment, is scrubbed too.
        let local = clean_metadata(
            &json!({"uuid": "OTHERBOX-1"}).to_string(),
            &[("COMPUTERNAME", "OTHERBOX")],
        );
        assert_eq!(local.unwrap().json["uuid"], "%COMPUTERNAME%-1");
        assert!(clean_metadata("no json here", &[]).is_err());
    }

    #[test]
    fn functions_and_objects_are_counted() {
        let f = |n: &str| json!({"event_index": 0, "function_name": n, "thread_index": 1});
        let events = json!([
            f("CaptureBegin"),
            f("ID3D12GraphicsCommandList_DrawInstanced"),
            f("ID3D12GraphicsCommandList_DrawIndexedInstanced"),
            f("ID3D12GraphicsCommandList_DrawIndexedInstanced"),
            f("ID3D12GraphicsCommandList_ExecuteIndirect"),
            f("ID3D12GraphicsCommandList_Dispatch"),
            f("ID3D12GraphicsCommandList_BuildRaytracingAccelerationStructure"),
            f("ID3D12Device_GetRaytracingAccelerationStructurePrebuildInfo"),
            f("ID3D12GraphicsCommandList_DispatchRays"),
            f("IDXGISwapChain_Present"),
        ]);
        let counts = count_functions(&format!("\u{feff}{events:#}")).unwrap();
        let want = [
            ("capture_events", 10),
            ("capture_draws", 1),
            ("capture_indexed_draws", 2),
            ("capture_indirect_calls", 1),
            ("capture_dispatches", 1),
            ("capture_as_builds", 1),
            ("capture_ray_dispatches", 1),
        ];
        assert_eq!(counts, want);
        let o = |t: &str| json!({"uid": 1, "type_name": t, "api": "D3D12", "object_name": "x"});
        let objects = json!([
            o("Resource"),
            o("Resource"),
            o("AccelerationStructure"),
            o("PipelineState"),
            o("Heap")
        ]);
        let named = count_objects(&objects.to_string()).unwrap();
        assert_eq!(
            named,
            [
                ("capture_objects", 5),
                ("capture_resources", 2),
                ("capture_acceleration_structures", 1),
                ("capture_pipeline_states", 1),
                ("capture_state_objects", 0),
            ]
        );
        assert!(count_functions("{}").is_err());
    }

    #[test]
    fn perf_csv_frame_xls_ini_and_versions() {
        let csv = "i, replayTotalFps, msFrameTime, msGpuTime, numFrames, mae\n\
                   0, 21.3, 46.9, -1.000, 1, 0.00001\n\
                   1, 12.3, 14.5, 4.0, 1, -1.00000\n\
                   2, 61.6, 12.6, 5.0, 1, -1.00000\n\
                   3, 60.8, 12.5, 6.0, 1, -1.00000\n";
        let it = iteration_times(csv).unwrap();
        assert_eq!(it.iterations, 4);
        assert_eq!(
            (it.image_error, it.gpu_frame_ms, it.cpu_frame_ms),
            (Some(0.00001), Some(5.0), Some(12.6))
        );
        let none = iteration_times("i, msFrameTime, msGpuTime, mae\n0, 4, -1, 0.1\n1, 3, -1, -1\n")
            .unwrap();
        assert_eq!(none.gpu_frame_ms, None);
        assert!(iteration_times("i, x\n0, 1\n").is_err());

        assert_eq!(gpu_frame_ms("GPU frame time\t20.1127"), Some(20.1127));
        assert_eq!(gpu_frame_ms("other\t1"), None);

        let ini = "[CorePlugin.Privacy]\nAskedFor_NVIDIA%20Nsight%20Graphics_2026.3.1.0=HasAsked\n\
                   [Other]\nAskedFor_NVIDIA%20Nsight%20Graphics_2027.1.0.0=HasAsked\n";
        assert!(privacy_asked(ini, "2026.3.1.0"));
        assert!(!privacy_asked(ini, "2027.1.0.0"), "another section");
        assert!(!privacy_asked("", "2026.3.1.0"));

        assert!(!newer("2026.3.1", "2026.3.1.0"));
        assert!(newer("2026.4", "2026.3.1.0"));
        assert!(!newer("2026.3", "x"));
    }
}
