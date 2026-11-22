//! `game_screenshot`: one frame of the game's window, which is found from the pids in the session's job (never
//! by window title or image name), written under `<capture root>\<session>\` (beside measure's CSVs; one session is
//! one boot) and recorded in the ledger under the session's boot, with its `encoding` and the display's state.
//!
//! - `sdr`, the default: grab.py ported. PrintWindow(PW_RENDERFULLCONTENT) of the window. An 8-bit sRGB PNG, clipped
//!   on an HDR display.
//! - `hdr`: Windows.Graphics.Capture of the window in FP16 (scRGB). Where WGC is unavailable or fails (not every
//!   Windows install has it working), DXGI Desktop Duplication of the window's monitor, cropped to the window. A
//!   16-bit PQ BT.2020 PNG with cICP, through quen-core's encoder, and with `npy` a float16 `.npy` too (scRGB as
//!   captured, 66 MB at 4K). The capture session is closed before the tool returns: none may stay open into a
//!   PresentMon window. The config's test switch `force_duplication` skips WGC, so a live boot exercises the fallback.
//! - `renderdoc`: a RenderDoc frame capture in a `renderdoc:NAME` session (profile.rs).
//!
//! `hdr` with `count` takes consecutive frames of one WGC session (copies queued as they come, read once the last is
//! in), and `measure_boxes` holds one for a recording of box means. After the grab, never before it, the rows get the
//! frame's pose through the game's channel ([`frame_pose`]).
//!
//! quen picks the method; the agent can't ask for the screen. Desktop Duplication shows whatever
//! covers the game too (a toast, an overlay), so it runs only while the game is in front, checked just before and
//! just after the frame (a frame taken while the focus moved is dropped), and its rows say `includes_desktop`. Every
//! method first shows grab.py's pin, a 2x2 topmost, never-activated, nearly transparent window over the game's
//! corner, and lets it compose: under independent flip DWM's copy of the window otherwise goes stale for seconds to
//! minutes. The Win32 calls run on the session thread, per-monitor DPI aware while they do.

use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use half::f16;
use quen_core::bridge_log::VALID;
use quen_core::hdr::{Encoding, Frame, Pixels};
use quen_core::image::{BoxFrame, boxes_csv, crop_region, mean_rgb};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Capture, Ledger, PlayerPose, Pose as PoseRow};
use quen_core::protocol::Verb;
use quen_core::trace::Body;
use serde_json::{Map, Value, json};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{COLORREF, GENERIC_ALL, HMODULE, HWND, LPARAM, LUID, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC,
    IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput5,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW,
    MonitorFromPoint, MonitorFromWindow, ReleaseDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DF_ALLOWOTHERACCOUNTHOOK, OpenInputDesktop,
    SetThreadDesktop,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, LWA_ALPHA, MSG, PM_REMOVE, PW_RENDERFULLCONTENT, PeekMessageW,
    SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, ShowWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, w};

use crate::game::{self, Ctx, Fields};
use crate::session::{self, Call, Done, Session};

/// grab.py's settle after showing the pin: a few frames for DWM to compose.
const PIN_SETTLE: Duration = Duration::from_millis(250);
/// How long a capture API may take to hand over its first frame (WGC: also each next one).
const FRAME_WAIT: Duration = Duration::from_secs(2);
/// game_screenshot count: at most this many frames, holding at most this many bytes (8 a pixel) in memory until the
/// last: 32 frames at 4K.
const BURST_MAX: usize = 32;
const BURST_BYTES: u64 = 1 << 31;
/// A burst's and a recording's WGC pool: one buffer more than a single shot's, so a frame still held doesn't drop the
/// next.
const POOL: i32 = 3;
/// How long each read of a frame's pose may wait: the game answers on its next tick.
const POSE_WAIT: Duration = Duration::from_millis(500);
/// measure_boxes: at most this many boxes, seconds and frames.
const BOXES_MAX: usize = 16;
const RECORD_SECS_MAX: f64 = 600.0;
const RECORD_FRAMES_MAX: u64 = 100_000;
/// measure_boxes: the frames whose box copies are in flight before the oldest is read, so a read never waits on its
/// copy.
const RING: usize = 4;

/// The `tools/list` entry.
pub fn tool() -> Value {
    json!({
        "name": "game_screenshot",
        "description": "One frame of the game's window (found from the session's job, never by title) into the \
            session's capture folder, a captures row per file. method sdr (default): an 8-bit sRGB PNG (srgb8), \
            clipped on an HDR display: for OCR, UI checks and SDR A/B. method hdr: Windows.Graphics.Capture in FP16 \
            (wgc_fp16), written as a 16-bit PQ BT.2020 PNG with cICP (pq_bt2020_u16: code values), with npy also \
            a .npy (scrgb_f16). If WGC fails (wgc_error says why), quen falls back to Desktop \
            Duplication of the game's screen area (desktop_duplication, includes_desktop true: an overlay can \
            show), only while the game is in front; a focus change drops the frame and refuses. method renderdoc / ngfx: \
            see method. count: a burst. The rows' pose_id and frame_state: see tag. Compare only captures of one \
            method, encoding and display state (each row records them). Refused, nothing changed: wrong token, the tag \
            taken in this session, no visible game window, no frame; with needs: method hdr while HDR is off (take sdr).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "token": {"type": "string", "description": "the token session_launch returned"},
                "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "the file name, unique in the session (A1, B2, ...). After the grab, the rows get pose_id, a poses row <session>/<its first file's name> (the camera as game_camera reads it, the player as world_get), and frame_state time (the game time) and photo, read through the game's channel (up to 0.5 s a read); null when it doesn't answer"},
                "method": {"type": "string", "enum": ["sdr", "hdr", "renderdoc", "ngfx"], "description": "default sdr. renderdoc, only in a session launched with profiler renderdoc:NAME: one RenderDoc capture (F12) as <capture root>\\<boot>\\<tag>.rdc (3-5 GB; 12 GB free needed), the game in front; it sends nothing more and returns once the capture is written (up to minutes: the game freezes while RenderDoc writes it), never without the Streamline swap in place. ngfx, only in a session launched with profiler ngfx-capture:NAME: one Nsight Graphics capture (F11) as <capture root>\\<boot>\\<NAME>\\<tag>.ngfx-capture (moved there when Nsight wrote it into its own folder: moved_from), the game in front; it sends nothing more and returns once the file holds still (up to minutes), with its captures row (kind ngfx-capture: ngfx_extract takes its capture_id) and the capture root's total (capture_root_gb). Refused while the capture root is over capture_budget_gb or under 12 GB free, unless user_ok"},
                "user_ok": {"type": "boolean", "description": "method ngfx only: true only after telling the user that the capture root is over its budget or low on space and that a capture can take several GB, and they agreed"},
                "npy": {"type": "boolean", "description": "method hdr only: true also writes a float16 .npy of linear scRGB as captured (scrgb_f16, 1.0 = 80 nits; 66 MB at 4K); default false, the PNG alone"},
                "count": {"type": "integer", "minimum": 2, "maximum": BURST_MAX, "description": "method hdr only: that many consecutive frames of one Windows.Graphics.Capture session (no fallback), held in memory until the last (8 bytes a pixel, at most 2 GiB: 32 at 4K), then written as <tag>-00, <tag>-01, ... (frame_state frame, present_ms); present_ms: each frame's present time after the first's"},
                "mean": {"type": "boolean", "description": "with count: write the frames' per-pixel mean of linear scRGB instead, as <tag> (method wgc_fp16_mean; frame_state frames, present_ms)"},
            },
            "required": ["token", "tag"],
            "additionalProperties": false,
        },
    })
}

/// A checked `game_screenshot` request.
struct Shot {
    tag: String,
    hdr: bool,
    renderdoc: bool,
    ngfx: bool,
    /// method ngfx: the user agreed to a capture over the budget or low on space.
    user_ok: bool,
    /// method hdr: the `.npy` too.
    npy: bool,
    /// method hdr: a burst of this many frames of one WGC session.
    count: Option<usize>,
    /// With count: their mean instead of each frame.
    mean: bool,
}

impl Shot {
    /// The arguments besides the token, which `game::ctx` checks.
    fn parse(args: &Value) -> Result<Shot, String> {
        let tag = tag_of(args)?;
        let (hdr, renderdoc, ngfx) = match args.get("method").map(Value::as_str) {
            None | Some(Some("sdr")) => (false, false, false),
            Some(Some("hdr")) => (true, false, false),
            Some(Some("renderdoc")) => (false, true, false),
            Some(Some("ngfx")) => (false, false, true),
            Some(m) => return Err(format!("method {m:?}: sdr, hdr, renderdoc or ngfx")),
        };
        let user_ok = match args.get("user_ok") {
            None => false,
            Some(_) if !ngfx => return Err("user_ok goes with method ngfx".into()),
            Some(v) => v.as_bool().ok_or("user_ok must be true or false")?,
        };
        let npy = match args.get("npy") {
            None => false,
            Some(_) if !hdr => return Err("npy goes with method hdr".into()),
            Some(v) => v.as_bool().ok_or("npy must be true or false")?,
        };
        let count = match args.get("count") {
            None => None,
            Some(_) if !hdr => return Err("count goes with method hdr".into()),
            Some(v) => Some(
                v.as_u64()
                    .filter(|n| (2..=BURST_MAX as u64).contains(n))
                    .ok_or(format!(
                        "count must be a whole number from 2 to {BURST_MAX}"
                    ))? as usize,
            ),
        };
        let mean = match args.get("mean") {
            None => false,
            Some(_) if count.is_none() => return Err("mean goes with count".into()),
            Some(v) => v.as_bool().ok_or("mean must be true or false")?,
        };
        Ok(Shot {
            tag: tag.into(),
            hdr,
            renderdoc,
            ngfx,
            user_ok,
            npy,
            count,
            mean,
        })
    }

    /// The file extensions this shot writes.
    fn extensions(&self) -> &'static [&'static str] {
        if self.npy { &["npy", "png"] } else { &["png"] }
    }

    /// The names its files take, before the extension: the tag, or a burst's frames `<tag>-00`, `<tag>-01`, ...
    fn names(&self) -> Vec<String> {
        match self.count {
            Some(n) if !self.mean => (0..n).map(|i| format!("{}-{i:02}", self.tag)).collect(),
            _ => vec![self.tag.clone()],
        }
    }
}

/// The `tag` argument: 1-64 letters, digits, _ or -.
fn tag_of(args: &Value) -> Result<&str, String> {
    let tag = game::text(args, "tag")?;
    let fine = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if tag.is_empty() || tag.len() > 64 || !tag.chars().all(fine) {
        return Err("tag: 1-64 letters, digits, _ or - (it names the file)".into());
    }
    Ok(tag)
}

/// Refuses a burst of `count` frames of the window `rect` over [`BURST_BYTES`].
fn burst_fits(count: usize, rect: &RECT) -> Result<(), String> {
    let px = (rect.right - rect.left).max(0) as u64 * (rect.bottom - rect.top).max(0) as u64;
    let most = (BURST_BYTES / (px * 8).max(1)).min(BURST_MAX as u64);
    match count as u64 <= most {
        true => Ok(()),
        false => Err(format!(
            "count {count}: a {}x{} window's frames take {:.2} GB in memory, over {:.2} GB; at most {most} here",
            rect.right - rect.left,
            rect.bottom - rect.top,
            (count as u64 * px * 8) as f64 / 1e9,
            BURST_BYTES as f64 / 1e9
        )),
    }
}

pub(crate) fn screenshot(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(
        session,
        args,
        &["tag", "method", "user_ok", "npy", "count", "mean"],
    )?;
    let shot = Shot::parse(args)?;
    if shot.renderdoc {
        return crate::profile::capture(&cx, call, &shot.tag);
    }
    if shot.ngfx {
        return crate::profile::ngfx_capture(&cx, call, &shot.tag, shot.user_ok);
    }
    let root = cx.config.capture_root();
    // Windows' separator, so the artifact's absolute path has one kind; the ledger stores `/` either way.
    let rel = |name: &str, ext: &str| format!("{}\\{name}.{ext}", cx.live.id);
    taken(&root, &rel, &shot.names(), shot.extensions(), &shot.tag)?;
    let pids = cx.live.job.pids().unwrap_or_default();
    let _dpi = Dpi::per_monitor();
    let (window, rect) = game_window(&pids)?;
    if let Some(count) = shot.count {
        burst_fits(count, &rect)?;
        if cx.config.force_duplication {
            let why = "count needs Windows.Graphics.Capture, which force_duplication in the local config (a test \
                       switch) skips";
            return Err(why.to_string().into());
        }
    }
    if call.stopped() {
        return Err(Done::Refused(
            "cancelled before anything was changed".into(),
        ));
    }
    let mut ledger =
        Ledger::open(cx.config.state_dir().join("quen.db")).map_err(|e| format!("ledger: {e}"))?;
    call.lap("check");
    crate::status::hide(&cx, call, false).map_err(Done::Refused)?;
    let display = display_state(window);
    if shot.hdr {
        hdr_on(&display)?;
    }
    call.lap("display");
    // For the Desktop Duplication fallback, before the pin: a thread that owns a window can't change desktops.
    if shot.hdr {
        to_input_desktop();
    }
    let pin = Pin::show(&rect).map_err(|e| format!("pin window: {e}"))?;
    sleep(PIN_SETTLE);
    call.lap("pin");
    let row = |method: &str, state: Map<String, Value>, pose_id, t| Capture {
        boot_id: Some(cx.live.boot),
        method: Some(method.into()),
        includes_overlay: Some(true),
        includes_desktop: Some(method == DESKTOP_DUPLICATION),
        display_state: Some(display.clone()),
        frame_state: (!state.is_empty()).then_some(state),
        pose_id,
        t,
        ..Capture::default()
    };
    let mut fields = Map::new();
    let (captures, failed) = match shot.count {
        None => {
            let grabbed = grab(
                &shot,
                window,
                &rect,
                &|| in_front(&pids),
                cx.config.force_duplication,
            );
            let at = session::now() as i64;
            drop(pin);
            call.lap("grab");
            let Grab {
                image,
                method,
                wgc_error,
            } = grabbed.map_err(|why| format!("grab failed, nothing written: {why}"))?;
            let (pose_id, state) = posed(&cx, call, &mut ledger, &shot.tag);
            let files = encode(image, shot.npy)
                .map_err(|why| format!("{method}: {why}; nothing written"))?;
            call.lap("encode");
            let t = ledger
                .utc(at)
                .map_err(|e| format!("ledger: {e}; nothing written"))?;
            let row = row(method, state, pose_id, t);
            let name = |ext: &str| rel(&shot.tag, ext);
            let (stored, failed) = store(&root, &name, files, &row, &mut ledger);
            call.lap("store");
            fields.insert("method".into(), json!(method));
            fields.insert(
                "includes_desktop".into(),
                json!(method == DESKTOP_DUPLICATION),
            );
            if let Some(why) = wgc_error {
                fields.insert("wgc_error".into(), json!(why));
            }
            (listed(call, &root, &stored, &Map::new()), failed)
        }
        Some(count) => {
            let d3d = D3d::for_window(window).map_err(|e| format!("D3D11: {e}"))?;
            let grabbed = d3d.burst(window, count);
            let at = session::now() as i64;
            drop(pin);
            call.lap("grab");
            let (frames, size) = grabbed.map_err(|e| {
                format!("grab failed, nothing written: Windows.Graphics.Capture (no fallback with count): {e}")
            })?;
            let (pose_id, state) = posed(&cx, call, &mut ledger, &shot.names()[0]);
            let t = ledger
                .utc(at)
                .map_err(|e| format!("ledger: {e}; nothing written"))?;
            let method = if shot.mean { WGC_FP16_MEAN } else { WGC_FP16 };
            let present = present_ms(frames.iter().map(|f| f.1));
            let row = row(method, state, pose_id, t);
            fields.insert("method".into(), json!(method));
            fields.insert("includes_desktop".into(), json!(false));
            fields.insert("present_ms".into(), json!(present));
            let out = burst_files(
                call,
                &shot,
                &d3d,
                frames,
                size,
                &present,
                &row,
                &mut ledger,
                &root,
                &rel,
            )?;
            call.lap("store");
            out
        }
    };
    fields.insert("captures".into(), json!(captures));
    fields.insert(
        "window".into(),
        json!([rect.right - rect.left, rect.bottom - rect.top]),
    );
    fields.insert("display_state".into(), json!(display));
    match failed {
        None => Ok(fields),
        Some(why) => Err(Done::Failed(why, fields)),
    }
}

/// Refuses when a file of `names` with one of `exts` exists: `tag` is taken in this session.
fn taken(
    root: &Path,
    rel: &dyn Fn(&str, &str) -> String,
    names: &[String],
    exts: &[&str],
    tag: &str,
) -> Result<(), String> {
    for name in names {
        for ext in exts {
            let path = root.join(rel(name, ext));
            if path.exists() {
                return Err(format!(
                    "{} exists: tag {tag:?} is taken in this session",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Stored files in the envelope (artifacts, captures rows) and as the result's `captures`, each with `extra`.
fn listed(
    call: &mut Call,
    root: &Path,
    stored: &[Stored],
    extra: &Map<String, Value>,
) -> Vec<Value> {
    let mut captures = Vec::new();
    for s in stored {
        let path = root.join(&s.rel);
        call.artifact(&path);
        if let Some(id) = s.id {
            call.row("captures", id);
        }
        let mut c = Map::from_iter([
            ("path".to_string(), json!(path)),
            ("encoding".into(), json!(s.encoding)),
            ("id".into(), json!(s.id)),
        ]);
        c.extend(extra.clone());
        captures.push(Value::Object(c));
    }
    captures
}

/// Present times (QPC, in 100 ns) as ms after the first, to the microsecond.
fn present_ms(times: impl Iterator<Item = i64>) -> Vec<f64> {
    let mut first = None;
    times
        .map(|t| {
            let t0 = *first.get_or_insert(t);
            ((t - t0) as f64 / 10.0).round() / 1000.0
        })
        .collect()
}

/// A burst's files once grabbed: each frame's as `<tag>-<nn>`, or with mean the frames' per-pixel mean's as `<tag>`;
/// each frame's rows `row` with frame and present_ms (the mean's: frames and present_ms) added to its frame state.
/// The result's captures, and why it stopped if it did (a cancel between frames, a store that failed). Err, nothing
/// written: the first file couldn't be made.
#[allow(clippy::too_many_arguments)]
fn burst_files(
    call: &mut Call,
    shot: &Shot,
    d3d: &D3d,
    frames: Staged,
    [w, h]: [u32; 2],
    present: &[f64],
    row: &Capture,
    ledger: &mut Ledger,
    root: &Path,
    rel: &dyn Fn(&str, &str) -> String,
) -> Result<(Vec<Value>, Option<String>), String> {
    let with = |more: [(&str, Value); 2]| Capture {
        frame_state: Some(
            row.frame_state
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(more.map(|(k, v)| (k.to_string(), v)))
                .collect(),
        ),
        ..row.clone()
    };
    let n = frames.len();
    if shot.mean {
        let failed = |why: String| format!("the mean: {why}; nothing written");
        let mut sum = vec![0f32; w as usize * h as usize * 4];
        for (staging, _) in &frames {
            let halves = d3d.map(staging, w, h).map_err(|e| failed(e.to_string()))?;
            for (s, v) in sum.iter_mut().zip(halves) {
                *s += v.to_f32();
            }
        }
        drop(frames);
        let mean = Image {
            width: w,
            height: h,
            pixels: Px::ScrgbF16(sum.iter().map(|s| f16::from_f32(s / n as f32)).collect()),
        };
        let files = encode(mean, shot.npy).map_err(failed)?;
        let row = with([("frames", json!(n)), ("present_ms", json!(present))]);
        let name = |ext: &str| rel(&shot.tag, ext);
        let (stored, failed) = store(root, &name, files, &row, ledger);
        return Ok((listed(call, root, &stored, &Map::new()), failed));
    }
    let mut captures = Vec::new();
    for (i, (staging, _)) in frames.into_iter().enumerate() {
        if call.stopped() {
            let why = format!("{} after {i} of {n} frames were written", call.stop_cause());
            return Ok((captures, Some(why)));
        }
        let files = match staged_files(d3d, &staging, [w, h], shot.npy) {
            Ok(files) => files,
            Err(why) if i == 0 => return Err(format!("frame 0: {why}; nothing written")),
            Err(why) => return Ok((captures, Some(format!("frame {i}: {why}")))),
        };
        let row = with([("frame", json!(i)), ("present_ms", json!(present[i]))]);
        let name = format!("{}-{i:02}", shot.tag);
        let (stored, failed) = store(root, &|ext| rel(&name, ext), files, &row, ledger);
        let extra = Map::from_iter([("frame".to_string(), json!(i))]);
        captures.extend(listed(call, root, &stored, &extra));
        if failed.is_some() {
            return Ok((captures, failed));
        }
        call.progress(&format!("frame {} of {n} written", i + 1));
    }
    Ok((captures, None))
}

/// A staging texture's FP16 frame, encoded as method hdr writes it.
fn staged_files(
    d3d: &D3d,
    staging: &ID3D11Texture2D,
    [w, h]: [u32; 2],
    npy: bool,
) -> Result<Vec<Encoded>, String> {
    let halves = d3d.map(staging, w, h).map_err(|e| e.to_string())?;
    let image = Image {
        width: w,
        height: h,
        pixels: Px::ScrgbF16(halves),
    };
    encode(image, npy)
}

/// What the game says of the frame just grabbed, through its channel, each read waited for up to [`POSE_WAIT`], only
/// with a loaded game in quen's state: the camera (cam.get, as game_camera reads it) and the player (world.get, as
/// world_get reads it) as a poses row named `name`, and for frame_state `photo` (the camera is photo mode's) and `time`
/// (the game time). What doesn't answer is left out; no pose without the camera.
fn frame_pose(cx: &Ctx, name: &str) -> (Option<PoseRow>, Map<String, Value>) {
    let mut state = Map::new();
    if cx.last_flags().is_none_or(|f| f & VALID == 0) {
        return (None, state);
    }
    let read = |verb| cx.channel().ok()?.call(verb, POSE_WAIT).ok();
    let Some((camera, cam)) = read(Verb::CamGet {}).and_then(|r| crate::camera::read_back(&r).ok())
    else {
        return (None, state);
    };
    state.insert("photo".into(), json!(camera == "photomode"));
    let world = read(Verb::WorldGet {})
        .and_then(|r| r.data)
        .unwrap_or_default();
    if world["time"].is_object() {
        state.insert("time".into(), world["time"].clone());
    }
    let player = Body::player(&world).ok().map(|p| PlayerPose {
        position: p.pos,
        yaw: Some(p.heading),
    });
    let pose = PoseRow {
        name: name.into(),
        camera: cam.pos,
        rotation: cam.rot,
        fov: cam.fov,
        player,
        time: serde_json::from_value(world["time"].clone()).ok(),
        weather: world["weather"]
            .as_str()
            .filter(|w| !w.is_empty())
            .map(Into::into),
        save_id: None,
        ref_capture_id: None,
    };
    (Some(pose), state)
}

/// [`frame_pose`] of the capture whose first file is `<session>\<name>.<ext>`: its pose, named `<session>/<name>`
/// (unique, as the file is), added to the ledger (on the call's rows; None when not read or not added), and the frame
/// state.
fn posed(
    cx: &Ctx,
    call: &mut Call,
    ledger: &mut Ledger,
    name: &str,
) -> (Option<i64>, Map<String, Value>) {
    let (pose, state) = frame_pose(cx, &format!("{}/{name}", cx.live.id));
    let id = pose.and_then(|p| ledger.add_pose(&p).ok());
    if let Some(id) = id {
        call.row("poses", id);
    }
    call.lap("pose");
    (id, state)
}

/// measure_boxes' `tools/list` entry.
pub fn boxes_tool() -> Value {
    let unit = json!({"type": "number", "minimum": 0, "maximum": 1, "description": "a fraction"});
    json!({
        "name": "measure_boxes",
        "description": "Record named boxes' means on every frame the game presents: one Windows.Graphics.Capture \
            session of the game's own window (found from the session's job; no other window shows) in FP16, for secs or frames. Writes \
            <capture root>\\<session>\\<tag>-boxes.csv: frame, present_ms (after the first frame's) and per box \
            <name>_r, _g, _b, the mean of linear scRGB as captured (values: scrgb, 1.0 = 80 nits, BT.709; pixels \
            with a NaN or infinity left out, non_finite counts them; empty: none left); a captures row (method \
            wgc_boxes, frame_state boxes, frames, values) with the pose as game_screenshot's, read after the last \
            frame; with images also <tag>-first.png and <tag>-last.png as game_screenshot method hdr writes. Refused, \
            nothing written: wrong token, the tag taken in this session, a bad box, no visible game window, WGC \
            unavailable (no fallback), no first frame within 2 s. Failed, the frames so far written: a cancel \
            (job_cancel), no frame within 2 s, the window resized.",
        "inputSchema": game::schema(json!({
            "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "names the files, unique in the session"},
            "boxes": {"type": "array", "minItems": 1, "maxItems": BOXES_MAX, "description": "the boxes, names unique", "items": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "pattern": "^[a-z][a-z0-9_]{0,39}$", "description": "its columns' prefix"},
                    "box": {"type": "array", "minItems": 4, "maxItems": 4, "items": unit, "description": "[x, y, width, height] in fractions of the window from its top-left, each edge rounded to a pixel, as measure_image's crop_unit fraction"},
                },
                "required": ["name", "box"],
                "additionalProperties": false,
            }},
            "secs": {"type": "number", "minimum": 0, "maximum": RECORD_SECS_MAX, "description": "how long after the first frame's present time, above 0: the first frame at or past it is the last; or frames"},
            "frames": {"type": "integer", "minimum": 1, "maximum": RECORD_FRAMES_MAX, "description": "how many frames; or secs"},
            "images": {"type": "boolean", "description": "also the first and the last frame as images (the last only when the recording reached its end); default false"},
        }), &["tag", "boxes"]),
    })
}

/// How long a recording runs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Until {
    /// Seconds from the first frame's present time.
    Secs(f64),
    Frames(u64),
}

/// A checked measure_boxes request.
#[derive(Debug)]
struct Record {
    tag: String,
    /// Each box's name and [x, y, width, height] in fractions of the window.
    boxes: Vec<(String, [f64; 4])>,
    until: Until,
    images: bool,
}

impl Record {
    /// The arguments besides the token, which `game::ctx` checks.
    fn parse(args: &Value) -> Result<Record, String> {
        let tag = tag_of(args)?.to_string();
        let given = args["boxes"]
            .as_array()
            .filter(|b| (1..=BOXES_MAX).contains(&b.len()))
            .ok_or(format!("boxes: 1 to {BOXES_MAX} of {{name, box}}"))?;
        let mut boxes: Vec<(String, [f64; 4])> = Vec::new();
        for b in given {
            let map = b.as_object().ok_or("a box is {name, box}")?;
            if let Some(key) = map.keys().find(|k| *k != "name" && *k != "box") {
                return Err(format!("a box: unknown field {key:?}"));
            }
            let name = b["name"]
                .as_str()
                .filter(|n| crate::image::name_ok(n))
                .ok_or("a box's name: 1-40 of a-z, 0-9 and _, from a letter")?;
            if boxes.iter().any(|(n, _)| n == name) {
                return Err(format!("box name {name:?} twice"));
            }
            let fractions = b["box"]
                .as_array()
                .filter(|a| a.len() == 4)
                .and_then(|a| a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
                .map(|c| [c[0], c[1], c[2], c[3]])
                .ok_or(format!("box {name}: [x, y, width, height] in fractions"))?;
            // Inside the window at any size; whether it holds a pixel is checked at the window's.
            crop_region(1 << 16, 1 << 16, Some(fractions), true)
                .map_err(|e| format!("box {name}: {e}"))?;
            boxes.push((name.into(), fractions));
        }
        let until = match (args.get("secs"), args.get("frames")) {
            (Some(s), None) => Until::Secs(
                s.as_f64()
                    .filter(|s| *s > 0.0 && *s <= RECORD_SECS_MAX)
                    .ok_or(format!(
                        "secs must be a number above 0, at most {RECORD_SECS_MAX}"
                    ))?,
            ),
            (None, Some(f)) => Until::Frames(
                f.as_u64()
                    .filter(|f| (1..=RECORD_FRAMES_MAX).contains(f))
                    .ok_or(format!(
                        "frames must be a whole number from 1 to {RECORD_FRAMES_MAX}"
                    ))?,
            ),
            _ => return Err("give secs or frames, one of them".into()),
        };
        Ok(Record {
            tag,
            boxes,
            until,
            images: game::flag(args, "images")?,
        })
    }
}

/// What a recording read: each frame's present time (QPC, in 100 ns) and box means, the pixels left out, why it
/// ended early, and the last frame's index when its image was copied (the recording reached its end).
#[derive(Default)]
struct Recorded {
    times: Vec<i64>,
    means: Vec<Vec<Option<[f64; 3]>>>,
    non_finite: usize,
    stopped: Option<String>,
    last: Option<usize>,
}

/// measure_boxes' frames on a started session: each frame's boxes copied into a ring slot and read [`RING`] - 1
/// frames later; with `full`, the first and the last frame copied whole. Until `until`'s end, a cancel, a frame not
/// within [`FRAME_WAIT`] or a resized window (`stopped`, the frames so far kept). Err: no first frame.
fn record(
    d3d: &D3d,
    wgc: &Wgc,
    call: &Call,
    until: Until,
    regions: &[quen_core::hdr::Region],
    ring: &[Vec<ID3D11Texture2D>],
    full: Option<&[ID3D11Texture2D; 2]>,
) -> Result<Recorded, String> {
    let [w, h] = wgc.size;
    let mut out = Recorded::default();
    // The box means of the frame whose copies are in ring slot `slot`.
    let read = |slot: usize, out: &mut Recorded| -> windows::core::Result<()> {
        let mut means = Vec::new();
        for (staging, r) in ring[slot].iter().zip(regions) {
            let (mean, odd) = mean_rgb(&d3d.map(staging, r.width, r.height)?);
            out.non_finite += odd;
            means.push(mean);
        }
        out.means.push(means);
        Ok(())
    };
    loop {
        let i = out.times.len();
        if call.stopped() {
            out.stopped = Some(format!("{} after {i} frames", call.stop_cause()));
            break;
        }
        pump();
        let frame = match wgc.next() {
            Ok(frame) => frame,
            Err(e) if i == 0 => return Err(format!("no frame within 2 s: {e}")),
            Err(e) => {
                out.stopped = Some(format!("no frame within 2 s after {i} frames: {e}"));
                break;
            }
        };
        if frame.size != wgc.size {
            out.stopped = Some(format!(
                "{} after {i} frames",
                resized(frame.size, wgc.size).message()
            ));
            break;
        }
        for (staging, r) in ring[i % RING].iter().zip(regions) {
            d3d.copy(&frame.texture, staging, r.x, r.y, r.width, r.height);
        }
        let end = match until {
            Until::Frames(n) => i as u64 + 1 >= n,
            Until::Secs(s) => out
                .times
                .first()
                .is_some_and(|t0| (frame.t - t0) as f64 / 1e7 >= s),
        };
        if let Some(full) = full {
            if i == 0 {
                d3d.copy(&frame.texture, &full[0], 0, 0, w, h);
            }
            if end {
                d3d.copy(&frame.texture, &full[1], 0, 0, w, h);
                out.last = Some(i);
            }
        }
        out.times.push(frame.t);
        drop(frame);
        if i + 1 >= RING
            && let Err(e) = read((i + 1 - RING) % RING, &mut out)
        {
            out.stopped = Some(format!("reading frame {}'s boxes: {e}", i + 1 - RING));
            break;
        }
        call.progress(&format!("{} frames", i + 1));
        if end {
            break;
        }
    }
    // The frames still in the ring.
    while out.means.len() < out.times.len() {
        let j = out.means.len();
        if let Err(e) = read(j % RING, &mut out) {
            out.stopped
                .get_or_insert(format!("reading frame {j}'s boxes: {e}"));
            break;
        }
    }
    out.times.truncate(out.means.len());
    out.last = out.last.filter(|i| *i < out.means.len());
    Ok(out)
}

/// Dispatches this thread's window messages: the pin's, so a long recording doesn't leave it unresponsive.
fn pump() {
    let mut msg = MSG::default();
    // SAFETY: this thread's own queue; each message goes to its own window's procedure.
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

/// measure_boxes: one WGC session of the game's window for a duration or a frame count, the boxes' means per frame
/// into a CSV with its captures row; with images, the first and the last frame too.
pub(crate) fn boxes(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["tag", "boxes", "secs", "frames", "images"])?;
    let rec = Record::parse(args)?;
    let root = cx.config.capture_root();
    let rel = |name: &str, ext: &str| format!("{}\\{name}.{ext}", cx.live.id);
    let names = [
        format!("{}-boxes", rec.tag),
        format!("{}-first", rec.tag),
        format!("{}-last", rec.tag),
    ];
    taken(&root, &rel, &names[..1], &["csv"], &rec.tag)?;
    if rec.images {
        taken(&root, &rel, &names[1..], &["png"], &rec.tag)?;
    }
    let pids = cx.live.job.pids().unwrap_or_default();
    let _dpi = Dpi::per_monitor();
    let (window, rect) = game_window(&pids)?;
    let mut ledger =
        Ledger::open(cx.config.state_dir().join("quen.db")).map_err(|e| format!("ledger: {e}"))?;
    let d3d = D3d::for_window(window).map_err(|e| format!("D3D11: {e}"))?;
    let wgc = d3d
        .capture(window, POOL)
        .map_err(|e| format!("Windows.Graphics.Capture (measure_boxes has no fallback): {e}"))?;
    let [w, h] = wgc.size;
    let regions = rec
        .boxes
        .iter()
        .map(|(name, b)| {
            crop_region(w, h, Some(*b), true)
                .map_err(|e| format!("box {name} on the {w}x{h} window: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let staged = (|| {
        let ring = (0..RING)
            .map(|_| {
                regions
                    .iter()
                    .map(|r| d3d.staging(r.width, r.height))
                    .collect::<windows::core::Result<Vec<_>>>()
            })
            .collect::<windows::core::Result<Vec<_>>>()?;
        let full = match rec.images {
            true => Some([d3d.staging(w, h)?, d3d.staging(w, h)?]),
            false => None,
        };
        Ok::<_, windows::core::Error>((ring, full))
    })();
    let (ring, full) = staged.map_err(|e| format!("D3D11: {e}"))?;
    if call.stopped() {
        return Err(Done::Refused(
            "cancelled before anything was changed".into(),
        ));
    }
    call.lap("check");
    let secs = match rec.until {
        Until::Secs(s) => s.ceil() as u64,
        Until::Frames(n) => n.div_ceil(60),
    };
    let line = crate::status::measuring(&cx, call, secs, None, &mut None);
    crate::status::hide(&cx, call, true).map_err(Done::Refused)?;
    let display = display_state(window);
    let pin = Pin::show(&rect).map_err(|e| format!("pin window: {e}"))?;
    sleep(PIN_SETTLE);
    call.lap("pin");
    let at = session::now() as i64;
    let got = wgc
        .session
        .StartCapture()
        .map_err(|e| format!("Windows.Graphics.Capture: {e}"))
        .and_then(|()| record(&d3d, &wgc, call, rec.until, &regions, &ring, full.as_ref()));
    drop(wgc);
    drop(pin);
    crate::status::cue(&cx, call);
    call.lap("record");
    let got = got.map_err(|why| format!("{why}; nothing written"))?;
    let (pose_id, state) = posed(&cx, call, &mut ledger, &names[0]);
    let t = ledger
        .utc(at)
        .map_err(|e| format!("ledger: {e}; nothing written"))?;
    let present = present_ms(got.times.iter().copied());
    let frames: Vec<BoxFrame> = got
        .means
        .into_iter()
        .zip(&present)
        .map(|(means, &present_ms)| BoxFrame { present_ms, means })
        .collect();
    let box_names: Vec<String> = rec.boxes.iter().map(|(n, _)| n.clone()).collect();
    let boxes: Vec<Value> = rec
        .boxes
        .iter()
        .zip(&regions)
        .map(|((name, b), r)| json!({"name": name, "box": b, "px": [r.x, r.y, r.width, r.height]}))
        .collect();
    let with = |method: &str, more: Vec<(&str, Value)>| Capture {
        boot_id: Some(cx.live.boot),
        method: Some(method.into()),
        includes_overlay: Some(true),
        includes_desktop: Some(false),
        display_state: Some(display.clone()),
        frame_state: Some(
            state
                .clone()
                .into_iter()
                .chain(more.into_iter().map(|(k, v)| (k.to_string(), v)))
                .collect(),
        ),
        pose_id,
        t: t.clone(),
        ..Capture::default()
    };
    let row = with(
        WGC_BOXES,
        vec![
            ("boxes", json!(boxes)),
            ("frames", json!(frames.len())),
            ("values", json!("scrgb")),
        ],
    );
    let csv = boxes_csv(&box_names, &frames).into_bytes();
    let name = |ext: &str| rel(&names[0], ext);
    let (stored, mut failed) = store(&root, &name, vec![("csv", None, csv)], &row, &mut ledger);
    let mut captures = listed(call, &root, &stored, &Map::new());
    let images = full
        .iter()
        .flat_map(|[first, last]| [(first, &names[1], Some(0)), (last, &names[2], got.last)]);
    for (staging, image, i) in images {
        let Some(i) = i.filter(|_| failed.is_none()) else {
            continue;
        };
        let files = match staged_files(&d3d, staging, [w, h], false) {
            Ok(files) => files,
            Err(why) => {
                failed = Some(format!("{image}: {why}"));
                continue;
            }
        };
        let row = with(
            WGC_FP16,
            vec![("frame", json!(i)), ("present_ms", json!(present[i]))],
        );
        let (stored, f) = store(&root, &|ext| rel(image, ext), files, &row, &mut ledger);
        captures.extend(listed(call, &root, &stored, &Map::new()));
        failed = failed.or(f);
    }
    call.lap("store");
    let mut fields = Map::new();
    fields.insert("captures".into(), json!(captures));
    fields.insert("frames".into(), json!(frames.len()));
    fields.insert("span_ms".into(), json!(present.last()));
    fields.insert("values".into(), json!("scrgb"));
    fields.insert("boxes".into(), json!(boxes));
    if got.non_finite > 0 {
        fields.insert("non_finite".into(), json!(got.non_finite));
    }
    fields.insert("window".into(), json!([w, h]));
    fields.insert("display_state".into(), json!(display));
    if let Some(line) = line {
        fields.insert("status".into(), json!(line));
    }
    match failed.or(got.stopped) {
        None => Ok(fields),
        Some(why) => Err(Done::Failed(why, fields)),
    }
}

/// A grabbed frame, row-major from the top row.
struct Image {
    width: u32,
    height: u32,
    pixels: Px,
}

enum Px {
    /// GDI's BGRA, 8 bits each.
    Bgra8(Vec<u8>),
    /// RGBA half floats of scRGB, as WGC and Desktop Duplication hand them over in FP16.
    ScrgbF16(Vec<f16>),
}

/// One encoded file: its extension, encoding (none: not an image) and bytes.
type Encoded = (&'static str, Option<Encoding>, Vec<u8>);

/// SDR: an RGB PNG. HDR: the 16-bit PQ BT.2020 PNG, after the `.npy` of the half floats as captured with `npy`.
fn encode(image: Image, npy: bool) -> Result<Vec<Encoded>, String> {
    let Image {
        width,
        height,
        pixels,
    } = image;
    match pixels {
        Px::Bgra8(bgra) => {
            let rgb: Vec<u8> = bgra
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect();
            let mut png = Vec::new();
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            // ponytail: Fast. One real 4K frame re-encoded in memory (release build, 2026-10-26): Fast 0.04 s, 62.3 % of
            // the raw bytes; Balanced 1.0 s, 62.4 %; High 1.4 s, 62.3 %; zlib level 1 0.16 s, 63.7 %. Live boot 1:
            // Balanced (the default) took 1.4 s.
            encoder.set_compression(png::Compression::Fast);
            encoder
                .write_header()
                .and_then(|mut w| w.write_image_data(&rgb))
                .map_err(|e| format!("PNG: {e}"))?;
            Ok(vec![("png", Some(Encoding::Srgb8), png)])
        }
        Px::ScrgbF16(halves) => {
            let frame =
                Frame::new(width, height, Pixels::ScrgbF16(halves)).map_err(|e| e.to_string())?;
            let mut files = Vec::new();
            if npy {
                let mut npy = Vec::new();
                frame.write_npy(&mut npy).map_err(|e| e.to_string())?;
                files.push(("npy", Some(Encoding::ScrgbF16), npy));
            }
            let mut png = Vec::new();
            frame
                .write_png_pq(Cursor::new(&mut png))
                .map_err(|e| e.to_string())?;
            files.push(("png", Some(Encoding::PqBt2020U16), png));
            Ok(files)
        }
    }
}

/// One file written, and its ledger id if its row went in.
struct Stored {
    rel: String,
    encoding: Option<Encoding>,
    id: Option<i64>,
}

/// Writes each file at `<root>\<rel(ext)>`, never over an existing one, then adds their capture rows (`row` with each
/// file's path, sha256, size and encoding) in one transaction, so a shot's rows go in together or not at all. What
/// was written (with ids once the rows are in), and why it stopped if it did.
fn store(
    root: &Path,
    rel: &dyn Fn(&str) -> String,
    files: Vec<Encoded>,
    row: &Capture,
    ledger: &mut Ledger,
) -> (Vec<Stored>, Option<String>) {
    let (mut stored, mut rows) = (Vec::new(), Vec::new());
    for (ext, encoding, bytes) in files {
        let rel = rel(ext);
        let path = root.join(&rel);
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| File::create_new(&path))
            .and_then(|mut f| f.write_all(&bytes));
        if let Err(e) = written {
            let why = format!("{}: {e}; no ledger rows were added", path.display());
            return (stored, Some(why));
        }
        // kind is the file's lowercase extension, as for every other capture row (feature tests 1).
        rows.push(Capture {
            kind: ext.into(),
            path: rel.clone(),
            sha256: sha256_hex(&bytes),
            bytes: bytes.len() as i64,
            encoding,
            ..row.clone()
        });
        stored.push(Stored {
            rel,
            encoding,
            id: None,
        });
    }
    let ids = ledger.atomic(|l| {
        rows.iter()
            .map(|r| l.add_capture(r))
            .collect::<Result<Vec<_>, _>>()
    });
    match ids {
        Ok(ids) => {
            for (s, id) in stored.iter_mut().zip(ids) {
                s.id = Some(id);
            }
            (stored, None)
        }
        Err(e) => (
            stored,
            Some(format!(
                "the files were written, but no ledger rows were added: {e}"
            )),
        ),
    }
}

/// Per-monitor DPI awareness for the session thread while it lives, so window rectangles and grabs are in physical
/// pixels.
pub(crate) struct Dpi(DPI_AWARENESS_CONTEXT);

impl Dpi {
    pub(crate) fn per_monitor() -> Dpi {
        // SAFETY: a predefined context; the old one comes back on drop.
        Dpi(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for Dpi {
    fn drop(&mut self) {
        // SAFETY: the context this thread had before.
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

/// Moves the session thread to the user's input desktop, best effort. Desktop Duplication shows the calling thread's
/// desktop, and on another one DWM serves a tone-mapped, gamut-narrowed view even in FP16 (moonbridge's capture
/// notes, after Sunshine's syncThreadDesktop).
fn to_input_desktop() {
    // SAFETY: the handle is closed once; the thread keeps its own reference to the desktop.
    unsafe {
        if let Ok(desk) = OpenInputDesktop(
            DF_ALLOWOTHERACCOUNTHOOK,
            false,
            DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0),
        ) {
            let _ = SetThreadDesktop(desk);
            let _ = CloseDesktop(desk);
        }
    }
}

/// Every top-level window, in z-order (the top one first).
pub(crate) fn top_windows() -> Result<Vec<HWND>, String> {
    unsafe extern "system" fn each(hwnd: HWND, all: LPARAM) -> BOOL {
        // SAFETY: `all` is the Vec below, alive for the whole EnumWindows call.
        unsafe { (*(all.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `all`.
    unsafe { EnumWindows(Some(each), LPARAM(&raw mut all as isize)) }
        .map_err(|e| format!("EnumWindows: {e}"))?;
    Ok(all)
}

/// The title of a visible, titled standard dialog (class `#32770`) of a process in the job, as the harness's
/// `launch_check` looks for one before every launch key: Witcher 3 shows its script compile errors in one. Its
/// untitled loading splash is a `#32770` too, and passes.
pub(crate) fn dialog(pids: &[u32]) -> Option<String> {
    top_windows().ok()?.into_iter().find_map(|hwnd| {
        let (mut pid, mut class, mut title) = (0, [0u16; 64], [0u16; 512]);
        // SAFETY: plain queries of a window handle into locals of the sizes passed; a closed window just fails them.
        let (n_class, n_title, shown) = unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let n_class = GetClassNameW(hwnd, &mut class);
            (
                n_class,
                GetWindowTextW(hwnd, &mut title),
                IsWindowVisible(hwnd).as_bool(),
            )
        };
        let class = String::from_utf16_lossy(&class[..n_class.max(0) as usize]);
        let title = String::from_utf16_lossy(&title[..n_title.max(0) as usize]);
        (pids.contains(&pid) && shown && class == "#32770" && !title.is_empty()).then_some(title)
    })
}

/// A top-level window's process, screen rectangle and area, if DWM shows it: visible, uncloaked (a cloaked window
/// counts as visible, but DWM doesn't show it) and not empty.
pub(crate) fn shown(hwnd: HWND) -> Option<(u32, RECT, i64)> {
    let (mut pid, mut cloaked) = (0, 0u32);
    let mut rect = RECT::default();
    // SAFETY: plain queries of a window handle into locals of the sizes passed; a window that closed meanwhile just
    // fails them.
    let shown = unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        IsWindowVisible(hwnd).as_bool()
            && DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4).is_ok()
            && cloaked == 0
            && GetWindowRect(hwnd, &mut rect).is_ok()
    };
    let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
    (shown && area > 0).then_some((pid, rect, area))
}

/// The game's window: the largest [`shown`] top-level window of a process in the job, and its screen rectangle.
/// ponytail: the largest wins; a game showing two big windows at once would need a better rule.
pub(crate) fn game_window(pids: &[u32]) -> Result<(HWND, RECT), String> {
    let all = top_windows()?;
    let mut best: Option<(HWND, RECT, i64)> = None;
    for hwnd in all {
        if let Some((pid, rect, area)) = shown(hwnd)
            && pids.contains(&pid)
            && best.is_none_or(|(_, _, a)| area > a)
        {
            best = Some((hwnd, rect, area));
        }
    }
    let (hwnd, rect, _) = best.ok_or(format!(
        "no visible window of the game's processes (pids {pids:?})"
    ))?;
    // SAFETY: a window handle.
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err("the game's window is minimized: nothing to capture".into());
    }
    Ok((hwnd, rect))
}

/// The frame console_type.py checks the console's prompt on: the game's own window only (quen captures nothing else),
/// by PrintWindow behind the pin so DWM composes its current frame, as BGRA rows top-down with their width and height.
pub(crate) fn window_frame(pids: &[u32]) -> Result<(usize, usize, Vec<u8>), String> {
    let _dpi = Dpi::per_monitor();
    let (window, rect) = game_window(pids)?;
    let pin = Pin::show(&rect).map_err(|e| format!("pin window: {e}"))?;
    sleep(PIN_SETTLE);
    let image = gdi(window, &rect);
    drop(pin);
    match image? {
        Image {
            width,
            height,
            pixels: Px::Bgra8(px),
        } => Ok((width as usize, height as usize, px)),
        _ => Err("GDI handed over no BGRA".into()),
    }
}

/// Whether the foreground window belongs to a process in the job.
pub(crate) fn in_front(pids: &[u32]) -> bool {
    let mut pid = 0;
    // SAFETY: plain queries; a null foreground window gives pid 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid != 0 && pids.contains(&pid)
}

/// grab.py's pin: a 2x2 topmost, never-activated window at alpha 1/255 over the bottom-left corner of `rect`, which
/// makes DWM compose the game's window again. Destroyed on drop.
struct Pin(HWND);

impl Pin {
    fn show(rect: &RECT) -> windows::core::Result<Pin> {
        // SAFETY: a system window class and no parent; the window is destroyed on drop, on this thread.
        unsafe {
            let pin = Pin(CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                rect.left,
                rect.bottom - 2,
                2,
                2,
                None,
                None,
                None,
                None,
            )?);
            SetLayeredWindowAttributes(pin.0, COLORREF(0), 1, LWA_ALPHA)?;
            let _ = ShowWindow(pin.0, SW_SHOWNOACTIVATE);
            Ok(pin)
        }
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        // SAFETY: created on this thread and destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// The ledger's `method` for the HDR fallback; its rows say `includes_desktop`.
const DESKTOP_DUPLICATION: &str = "desktop_duplication";
/// The methods of the image files this tool writes: what `measure_image` reads.
pub(crate) const IMAGE_METHODS: [&str; 4] =
    [PRINTWINDOW, WGC_FP16, DESKTOP_DUPLICATION, WGC_FP16_MEAN];
const PRINTWINDOW: &str = "printwindow";
const WGC_FP16: &str = "wgc_fp16";
/// A burst's per-pixel mean (game_screenshot count with mean).
const WGC_FP16_MEAN: &str = "wgc_fp16_mean";
/// measure_boxes' CSV.
const WGC_BOXES: &str = "wgc_boxes";

/// A grabbed frame, the method that took it, and why WGC didn't, if it didn't.
struct Grab {
    image: Image,
    method: &'static str,
    wgc_error: Option<String>,
}

/// quen_doctor: whether Windows.Graphics.Capture works here: supported, and a capture item made for a window of
/// quen's own, the pin (2x2, never activated, alpha 1/255) in the primary monitor's corner, gone on return. No capture
/// starts, so nothing is drawn and the focus stays where it is.
pub(crate) fn probe_wgc() -> Result<(), String> {
    match GraphicsCaptureSession::IsSupported() {
        Ok(true) => {}
        Ok(false) => return Err("GraphicsCaptureSession.IsSupported() is false".into()),
        Err(e) => return Err(format!("GraphicsCaptureSession: {e}")),
    }
    let corner = RECT {
        left: 0,
        top: 0,
        right: 2,
        bottom: 2,
    };
    let pin = Pin::show(&corner).map_err(|e| format!("a window to test with: {e}"))?;
    // SAFETY: a WinRT factory and a capture item for a window this thread made and still holds.
    unsafe {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("its factory: {e}"))?;
        interop
            .CreateForWindow::<GraphicsCaptureItem>(pin.0)
            .map(drop)
            .map_err(|e| format!("CreateForWindow: {e}"))
    }
}

/// quen_doctor: whether DXGI Desktop Duplication works on the primary display: one made in FP16 on the adapter driving
/// it, then released.
pub(crate) fn probe_duplication() -> Result<(), String> {
    // DuplicateOutput1 is DXGI_ERROR_UNSUPPORTED to a caller that isn't per-monitor DPI aware.
    let _dpi = Dpi::per_monitor();
    // SAFETY: a point query; COM calls on objects created here.
    unsafe {
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        let d3d = D3d::for_monitor(monitor).map_err(|e| format!("D3D11: {e}"))?;
        let output: IDXGIOutput5 = d3d
            .output
            .cast()
            .map_err(|e| format!("IDXGIOutput5: {e}"))?;
        output
            .DuplicateOutput1(&d3d.device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT])
            .map(drop)
            .map_err(|e| format!("DuplicateOutput1: {e}"))
    }
}

/// SDR: PrintWindow. HDR: [`hdr`] over WGC and Desktop Duplication, WGC skipped with `force_duplication` (a test
/// switch). `in_front`: whether the game is the foreground window now.
fn grab(
    shot: &Shot,
    window: HWND,
    rect: &RECT,
    in_front: &dyn Fn() -> bool,
    force_duplication: bool,
) -> Result<Grab, String> {
    if !shot.hdr {
        let image = gdi(window, rect)?;
        return Ok(Grab {
            image,
            method: PRINTWINDOW,
            wgc_error: None,
        });
    }
    let d3d = D3d::for_window(window).map_err(|e| format!("D3D11: {e}"))?;
    let wgc = || match force_duplication {
        true => Err("skipped: force_duplication in the local config (a test switch)".into()),
        false => d3d.wgc(window).map_err(|e| e.to_string()),
    };
    hdr(
        wgc,
        || d3d.duplicate(rect).map_err(|e| e.to_string()),
        in_front,
    )
}

/// WGC of the window; where it is unavailable or fails, Desktop Duplication of the window's screen area, which shows
/// whatever covers the game too. So the fallback runs only while the game is in front just before the frame and still
/// just after it; a frame taken while the focus moved is dropped.
fn hdr(
    wgc: impl FnOnce() -> Result<Image, String>,
    duplicate: impl FnOnce() -> Result<Image, String>,
    in_front: &dyn Fn() -> bool,
) -> Result<Grab, String> {
    let wgc_error = match wgc() {
        Ok(image) => {
            return Ok(Grab {
                image,
                method: WGC_FP16,
                wgc_error: None,
            });
        }
        Err(e) => format!("Windows.Graphics.Capture: {e}"),
    };
    let fallback = |why: String| format!("{wgc_error}; its fallback, Desktop Duplication, {why}");
    if !in_front() {
        return Err(fallback("needs the game in front, and it isn't".into()));
    }
    let image = duplicate().map_err(|e| fallback(format!("failed: {e}")))?;
    if !in_front() {
        drop(image);
        let why = "was dropped: the game lost the focus while the frame was taken, so it may show another window";
        return Err(fallback(why.into()));
    }
    Ok(Grab {
        image,
        method: DESKTOP_DUPLICATION,
        wgc_error: Some(wgc_error),
    })
}

/// grab.py: PrintWindow(PW_RENDERFULLCONTENT) of the window, read back top-down as BGRA.
fn gdi(window: HWND, rect: &RECT) -> Result<Image, String> {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let mut data = vec![0u8; w as usize * h as usize * 4];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: every GDI object made here is released here; `data` holds w x h BGRA pixels, as `info` says.
    let (drawn, lines) = unsafe {
        let src = GetDC(Some(window));
        let dc = CreateCompatibleDC(Some(src));
        let bitmap = CreateCompatibleBitmap(src, w, h);
        let old = SelectObject(dc, bitmap.into());
        let drawn = PrintWindow(window, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        SelectObject(dc, old);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            Some(data.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        ReleaseDC(Some(window), src);
        (drawn, lines)
    };
    if !drawn || lines != h {
        return Err(format!("PrintWindow drew {lines} of {h} lines"));
    }
    Ok(Image {
        width: w as u32,
        height: h as u32,
        pixels: Px::Bgra8(data),
    })
}

/// A D3D11 device on the adapter that drives the monitor showing the window, and that monitor's output.
struct D3d {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output: IDXGIOutput,
    desc: DXGI_OUTPUT_DESC,
}

impl D3d {
    fn for_window(window: HWND) -> windows::core::Result<D3d> {
        // SAFETY: a query on a window handle.
        D3d::for_monitor(unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) })
    }

    fn for_monitor(monitor: HMONITOR) -> windows::core::Result<D3d> {
        // SAFETY: COM calls on objects created here; out-pointers are locals.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            for a in 0.. {
                let adapter: IDXGIAdapter1 = factory.EnumAdapters1(a)?;
                for o in 0.. {
                    let Ok(output) = adapter.EnumOutputs(o) else {
                        break;
                    };
                    let desc = output.GetDesc()?;
                    if desc.Monitor != monitor {
                        continue;
                    }
                    // DuplicateOutput1 refuses a device below feature level 11.1 (the default list stops at 11.0),
                    // and on some drivers one without VIDEO_SUPPORT (moonbridge's capture notes, after Sunshine).
                    let levels = [
                        D3D_FEATURE_LEVEL_12_1,
                        D3D_FEATURE_LEVEL_12_0,
                        D3D_FEATURE_LEVEL_11_1,
                        D3D_FEATURE_LEVEL_11_0,
                    ];
                    let (mut device, mut context) = (None, None);
                    D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                        Some(&levels),
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )?;
                    return Ok(D3d {
                        device: device.ok_or_else(windows::core::Error::empty)?,
                        context: context.ok_or_else(windows::core::Error::empty)?,
                        output,
                        desc,
                    });
                }
            }
            unreachable!("EnumAdapters1 ends with an error")
        }
    }

    /// Windows.Graphics.Capture of `window` in FP16: the first frame, then the session is closed.
    fn wgc(&self, window: HWND) -> windows::core::Result<Image> {
        // Two buffers: the smallest pool moonbridge's capture runs with.
        let wgc = self.capture(window, 2)?;
        wgc.session.StartCapture()?;
        let frame = wgc.next()?;
        self.read(&frame.texture, 0, 0, frame.size[0], frame.size[1])
    }

    /// A Windows.Graphics.Capture session of `window` in FP16 on this device, a pool of `buffers`, not started yet.
    fn capture(&self, window: HWND, buffers: i32) -> windows::core::Result<Wgc> {
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8000_4001u32 as i32), // E_NOTIMPL
                "not supported on this system",
            ));
        }
        // SAFETY: COM / WinRT calls on objects created here.
        let (item, device) = unsafe {
            let interop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(window)?;
            let dxgi: IDXGIDevice = self.device.cast()?;
            let device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            (item, device)
        };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            DirectXPixelFormat::R16G16B16A16Float,
            buffers,
            size,
        )?;
        let wgc = Wgc {
            session: pool.CreateCaptureSession(&item)?,
            pool,
            size: [size.Width as u32, size.Height as u32],
        };
        // Windows 11 draws no yellow border when asked not to; older builds refuse, harmlessly.
        let _ = wgc.session.SetIsBorderRequired(false);
        wgc.session.SetIsCursorCaptureEnabled(false)?;
        Ok(wgc)
    }

    /// `count` consecutive frames of one WGC session of `window`, each copied as it comes into a staging texture made
    /// before the session started, so the copies only queue; read them with [`D3d::map`]. With each its system-relative
    /// time (QPC, in 100 ns), and their size.
    fn burst(&self, window: HWND, count: usize) -> windows::core::Result<(Staged, [u32; 2])> {
        let wgc = self.capture(window, POOL)?;
        let [w, h] = wgc.size;
        let staged = (0..count)
            .map(|_| self.staging(w, h))
            .collect::<windows::core::Result<Vec<_>>>()?;
        wgc.session.StartCapture()?;
        let mut frames = Vec::with_capacity(count);
        for staging in staged {
            let frame = wgc.next()?;
            if frame.size != wgc.size {
                return Err(resized(frame.size, wgc.size));
            }
            self.copy(&frame.texture, &staging, 0, 0, w, h);
            frames.push((staging, frame.t));
        }
        Ok((frames, wgc.size))
    }

    /// A CPU-readable FP16 texture of `w` x `h`.
    fn staging(&self, w: u32, h: u32) -> windows::core::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: a texture made on this device from a description of its own.
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?
        };
        staging.ok_or_else(windows::core::Error::empty)
    }

    /// Queues a copy of `w` x `h` pixels at (`x`, `y`) of `from` to the top-left of `to`.
    fn copy(&self, from: &ID3D11Texture2D, to: &ID3D11Texture2D, x: u32, y: u32, w: u32, h: u32) {
        let region = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + w,
            bottom: y + h,
            back: 1,
        };
        // SAFETY: two textures of this device, the region inside `from` and its size inside `to` (the callers').
        unsafe {
            self.context
                .CopySubresourceRegion(to, 0, 0, 0, 0, from, 0, Some(&region))
        };
    }

    /// The `w` x `h` RGBA half floats of an FP16 staging texture, once its copies are done.
    fn map(&self, staging: &ID3D11Texture2D, w: u32, h: u32) -> windows::core::Result<Vec<f16>> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the mapped rows are read within the map, by its row pitch, `w` pixels of 4 halves each.
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let mut halves = Vec::with_capacity(w as usize * h as usize * 4);
            for row in 0..h as usize {
                let line = mapped
                    .pData
                    .cast::<u8>()
                    .add(row * mapped.RowPitch as usize);
                let line = std::slice::from_raw_parts(line.cast::<u16>(), w as usize * 4);
                halves.extend(line.iter().map(|&bits| f16::from_bits(bits)));
            }
            self.context.Unmap(staging, 0);
            Ok(halves)
        }
    }

    /// Desktop Duplication of the window's monitor in FP16, cropped to `rect`; the duplication ends on return.
    fn duplicate(&self, rect: &RECT) -> windows::core::Result<Image> {
        let o = self.desc.DesktopCoordinates;
        let (left, top) = (rect.left.max(o.left), rect.top.max(o.top));
        let (right, bottom) = (rect.right.min(o.right), rect.bottom.min(o.bottom));
        let (x, y) = ((left - o.left) as u32, (top - o.top) as u32);
        let (w, h) = ((right - left).max(0) as u32, (bottom - top).max(0) as u32);
        // SAFETY: COM calls on objects created here; each acquired frame is released before the next.
        unsafe {
            let output: IDXGIOutput5 = self.output.cast()?;
            let dup =
                output.DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT])?;
            let start = Instant::now();
            loop {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource = None;
                match dup.AcquireNextFrame(100, &mut info, &mut resource) {
                    Ok(()) => {
                        // A frame without a present time carries only a pointer update.
                        let image = (info.LastPresentTime != 0).then(|| {
                            let texture: ID3D11Texture2D =
                                resource.ok_or_else(windows::core::Error::empty)?.cast()?;
                            self.read(&texture, x, y, w, h)
                        });
                        dup.ReleaseFrame()?;
                        if let Some(image) = image {
                            return image;
                        }
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                    Err(e) => return Err(e),
                }
                if start.elapsed() > FRAME_WAIT {
                    return Err(windows::core::Error::new(
                        DXGI_ERROR_WAIT_TIMEOUT,
                        "no desktop frame",
                    ));
                }
            }
        }
    }

    /// Copies `w` x `h` pixels at (`x`, `y`) of an FP16 texture into a staging texture and reads them back.
    fn read(
        &self,
        texture: &ID3D11Texture2D,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> windows::core::Result<Image> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a query into a local.
        unsafe { texture.GetDesc(&mut desc) };
        let (w, h) = (
            w.min(desc.Width.saturating_sub(x)),
            h.min(desc.Height.saturating_sub(y)),
        );
        if desc.Format != DXGI_FORMAT_R16G16B16A16_FLOAT || w == 0 || h == 0 {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8007_000Du32 as i32), // E_INVALIDDATA
                format!(
                    "a {}x{} texture of format {}, not FP16",
                    desc.Width, desc.Height, desc.Format.0
                ),
            ));
        }
        let staging = self.staging(w, h)?;
        self.copy(texture, &staging, x, y, w, h);
        Ok(Image {
            width: w,
            height: h,
            pixels: Px::ScrgbF16(self.map(&staging, w, h)?),
        })
    }
}

/// Why a WGC session stops: the window's size changed from its size when the session began.
fn resized(now: [u32; 2], was: [u32; 2]) -> windows::core::Error {
    windows::core::Error::new(
        windows::core::HRESULT(0x8007_000Du32 as i32), // E_INVALIDDATA
        format!(
            "the window changed size, {}x{} to {}x{}",
            was[0], was[1], now[0], now[1]
        ),
    )
}

/// A burst's frames, each in a staging texture of its own, with its system-relative time (QPC, in 100 ns).
type Staged = Vec<(ID3D11Texture2D, i64)>;

/// A Windows.Graphics.Capture session of one window in FP16 ([`D3d::capture`]); closed on drop.
struct Wgc {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// The window's size when the session was made: the pool's.
    size: [u32; 2],
}

// ponytail: no pool.Close(). Closing the free-threaded pool of another process's window crashed a WGC worker thread
// moments later (an access violation, every time in testing); releasing it does not. The session's Close ends the
// capture.
impl Drop for Wgc {
    fn drop(&mut self) {
        let _ = self.session.Close();
    }
}

/// A frame of a [`Wgc`] session: its texture on the session's device, content size and system-relative time (QPC, in
/// 100 ns); back to the pool on drop.
struct WgcFrame {
    frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
    size: [u32; 2],
    t: i64,
}

impl Drop for WgcFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

impl Wgc {
    /// The next frame, waited for up to [`FRAME_WAIT`]: the game's next present.
    fn next(&self) -> windows::core::Result<WgcFrame> {
        let start = Instant::now();
        let frame = loop {
            match self.pool.TryGetNextFrame() {
                Ok(frame) => break frame,
                Err(e) if start.elapsed() > FRAME_WAIT => return Err(e),
                Err(_) => sleep(Duration::from_millis(1)),
            }
        };
        let size = frame.ContentSize()?;
        let t = frame.SystemRelativeTime()?.Duration;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the frame's surface is a D3D11 texture on this device.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        Ok(WgcFrame {
            frame,
            texture,
            size: [size.Width as u32, size.Height as u32],
            t,
        })
    }
}

/// DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2 (wingdi.h, Windows 11 24H2), which windows 0.62 lacks: it carries the
/// active colour mode.
#[repr(C)]
#[derive(Default)]
struct ColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// advancedColorSupported, advancedColorActive, reserved, advancedColorLimitedByPolicy,
    /// highDynamicRangeSupported, highDynamicRangeUserEnabled, wideColorSupported, wideColorUserEnabled (bits 0-7).
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);

/// method hdr's need: HDR on the game's display. A display state that couldn't be read doesn't refuse.
fn hdr_on(display: &Map<String, Value>) -> Result<(), Done> {
    if display.get("hdr") != Some(&json!(false)) {
        return Ok(());
    }
    let need = json!({
        "dependency": "Windows HDR on the game's display",
        "status": "missing",
        "needed_for": "game_screenshot method hdr",
        "how_to_get": "Use HDR on, in Windows Settings > System > Display for the game's display; or take method sdr \
            instead (an 8-bit sRGB PNG)",
    });
    Err(Done::Needs(
        "method hdr: HDR is off on the game's display".into(),
        vec![need],
    ))
}

/// The state of the display showing `window`, from read-only DisplayConfig queries: monitor, HDR on, colour
/// mode, SDR white level, bit depth and wire encoding. A query that fails is recorded as `error`, so the state differs
/// from a good one and a comparison refuses it.
fn display_state(window: HWND) -> Map<String, Value> {
    // SAFETY: read-only queries into locals sized as the API asks.
    let state = unsafe { query_display(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST)) };
    state.unwrap_or_else(|e| Map::from_iter([("error".into(), json!(e))]))
}

unsafe fn query_display(monitor: HMONITOR) -> Result<Map<String, Value>, String> {
    let wide = |s: &[u16]| {
        String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
    };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY (all below): the caller's; each packet's header names its own type and size.
    unsafe {
        if !GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()).as_bool() {
            return Err("GetMonitorInfoW failed".into());
        }
        let device = wide(&info.szDevice);
        let (mut n_paths, mut n_modes) = (0, 0);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes)
            .ok()
            .map_err(|e| format!("GetDisplayConfigBufferSizes: {e}"))?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
        .ok()
        .map_err(|e| format!("QueryDisplayConfig: {e}"))?;
        let header = |kind, size: usize, adapter: LUID, id| DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: kind,
            size: size as u32,
            adapterId: adapter,
            id,
        };
        for path in &paths[..n_paths as usize] {
            let (s, t) = (&path.sourceInfo, &path.targetInfo);
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                    s.adapterId,
                    s.id,
                ),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || wide(&source.viewGdiDeviceName) != device
            {
                continue;
            }
            // ponytail: a cloned display shows one source on several targets; the first one is recorded.
            let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                    t.adapterId,
                    t.id,
                ),
                ..Default::default()
            };
            let mut color = ColorInfo2 {
                header: header(
                    GET_ADVANCED_COLOR_INFO_2,
                    size_of::<ColorInfo2>(),
                    t.adapterId,
                    t.id,
                ),
                ..Default::default()
            };
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
                    t.adapterId,
                    t.id,
                ),
                ..Default::default()
            };
            let failed = [
                ("target name", DisplayConfigGetDeviceInfo(&mut name.header)),
                (
                    "advanced colour",
                    DisplayConfigGetDeviceInfo(&mut color.header),
                ),
                (
                    "SDR white level",
                    DisplayConfigGetDeviceInfo(&mut white.header),
                ),
            ];
            if let Some((what, code)) = failed.iter().find(|(_, code)| *code != 0) {
                return Err(format!("DisplayConfigGetDeviceInfo {what}: error {code}"));
            }
            let mode = match color.active_color_mode {
                0 => "sdr",
                1 => "wcg",
                2 => "hdr",
                _ => "unknown",
            };
            let encoding = match color.color_encoding {
                0 => "rgb",
                1 => "ycbcr444",
                2 => "ycbcr422",
                3 => "ycbcr420",
                4 => "intensity",
                _ => "unknown",
            };
            return Ok(Map::from_iter([
                // The model's name is a hardware detail: a short hash tells two monitors apart.
                (
                    "monitor_sha".into(),
                    json!(sha256_hex(wide(&name.monitorFriendlyDeviceName).as_bytes())[..8]),
                ),
                ("device".into(), json!(device)),
                ("hdr".into(), json!(color.flags & (1 << 5) != 0)),
                ("color_mode".into(), json!(mode)),
                // The level is in thousandths of 80 nits.
                (
                    "sdr_white_nits".into(),
                    json!(f64::from(white.SDRWhiteLevel) * 0.08),
                ),
                (
                    "bits_per_channel".into(),
                    json!(color.bits_per_color_channel),
                ),
                ("color_encoding".into(), json!(encoding)),
            ]));
        }
        Err(format!("no active display path shows {device}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, IsWindow, RegisterClassW, UnregisterClassW, WNDCLASSW, WS_VISIBLE,
    };

    #[test]
    fn hdr_needs_hdr_on_and_names_sdr_as_the_way_out() {
        let display = |hdr: Value| Map::from_iter([("hdr".to_owned(), hdr)]);
        assert!(hdr_on(&display(json!(true))).is_ok());
        assert!(hdr_on(&Map::from_iter([("error".to_owned(), json!("x"))])).is_ok());
        let Err(Done::Needs(why, needs)) = hdr_on(&display(json!(false))) else {
            panic!("not refused with needs");
        };
        assert!(why.contains("HDR is off"), "{why}");
        assert_eq!(needs[0]["needed_for"], "game_screenshot method hdr");
        assert!(
            needs[0]["how_to_get"]
                .as_str()
                .unwrap()
                .contains("method sdr")
        );
    }

    #[test]
    fn checks_the_arguments() {
        let shot = Shot::parse(&json!({"tag": "A1-b_2"})).unwrap();
        assert_eq!((shot.hdr, shot.extensions()), (false, &["png"][..]));
        let shot = Shot::parse(&json!({"tag": "B", "method": "hdr"})).unwrap();
        assert_eq!((shot.hdr, shot.extensions()), (true, &["png"][..]));
        let shot = Shot::parse(&json!({"tag": "B", "method": "hdr", "npy": true})).unwrap();
        assert_eq!((shot.hdr, shot.extensions()), (true, &["npy", "png"][..]));
        for bad in [
            json!({"tag": "A", "npy": true}),
            json!({"tag": "A", "method": "hdr", "npy": 1}),
            json!({}),
            json!({"tag": "../x"}),
            json!({"tag": "a.png"}),
            json!({"tag": "x".repeat(65)}),
            json!({"tag": "A", "method": "jxr"}),
            json!({"tag": "A", "method": 1}),
            json!({"tag": "A", "count": 4}),
            json!({"tag": "A", "method": "hdr", "count": 1}),
            json!({"tag": "A", "method": "hdr", "count": 33}),
            json!({"tag": "A", "method": "hdr", "mean": true}),
            json!({"tag": "A", "method": "hdr", "count": 4, "mean": 1}),
        ] {
            assert!(Shot::parse(&bad).is_err(), "{bad}");
        }
        // A burst: its frames' names, or with mean the tag alone.
        let burst = Shot::parse(&json!({"tag": "D", "method": "hdr", "count": 3})).unwrap();
        assert_eq!(burst.names(), ["D-00", "D-01", "D-02"]);
        let mean = json!({"tag": "D", "method": "hdr", "count": 3, "mean": true, "npy": true});
        let mean = Shot::parse(&mean).unwrap();
        assert_eq!(
            (mean.names(), mean.extensions()),
            (vec!["D".to_string()], &["npy", "png"][..])
        );
        assert_eq!(Shot::parse(&json!({"tag": "D"})).unwrap().names(), ["D"]);
        // At most 2 GiB of frames: 32 at 4K, fewer above.
        let rect = |w, h| RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        assert!(burst_fits(32, &rect(3840, 2160)).is_ok());
        let why = burst_fits(32, &rect(7680, 4320)).unwrap_err();
        assert!(why.contains("at most 8 here"), "{why}");
        // Present times in 100 ns, as ms after the first.
        assert_eq!(
            present_ms([1_000_000, 1_166_667, 1_333_333].into_iter()),
            [0.0, 16.667, 33.333]
        );
    }

    /// measure_boxes' arguments: named boxes in fractions inside the window, secs or frames.
    #[test]
    fn checks_the_recorders_arguments() {
        let box_ = |name: &str, b: [f64; 4]| json!({"name": name, "box": b});
        let rec = Record::parse(
            &json!({"tag": "R1", "boxes": [box_("sky", [0.0, 0.0, 1.0, 0.25]),
            box_("geralt", [0.4, 0.3, 0.2, 0.5])], "frames": 120, "images": true}),
        )
        .unwrap();
        assert_eq!(rec.boxes[1], ("geralt".to_string(), [0.4, 0.3, 0.2, 0.5]));
        assert_eq!((rec.until, rec.images), (Until::Frames(120), true));
        let secs = Record::parse(
            &json!({"tag": "R1", "boxes": [box_("a", [0.0, 0.0, 0.5, 0.5])], "secs": 2.5}),
        );
        assert_eq!(secs.unwrap().until, Until::Secs(2.5));
        let one = || json!([box_("a", [0.0, 0.0, 0.5, 0.5])]);
        for bad in [
            json!({"tag": "R", "boxes": one()}),
            json!({"tag": "R", "boxes": one(), "secs": 1, "frames": 1}),
            json!({"tag": "R", "boxes": one(), "secs": 0}),
            json!({"tag": "R", "boxes": one(), "secs": 601}),
            json!({"tag": "R", "boxes": one(), "frames": 0}),
            json!({"tag": "R", "boxes": [], "frames": 1}),
            json!({"tag": "R", "boxes": [box_("a", [0.6, 0.0, 0.5, 0.5])], "frames": 1}),
            json!({"tag": "R", "boxes": [box_("A", [0.0, 0.0, 0.5, 0.5])], "frames": 1}),
            json!({"tag": "R", "boxes": [{"name": "a", "box": [0, 0, 0.5]}], "frames": 1}),
            json!({"tag": "R", "boxes": [box_("a", [0.0; 4]), box_("a", [0.0; 4])], "frames": 1}),
            json!({"tag": "R", "boxes": [{"name": "a", "box": [0, 0, 1, 1], "x": 1}], "frames": 1}),
            json!({"tag": "R", "boxes": one(), "frames": 1, "images": "yes"}),
            json!({"tag": "../R", "boxes": one(), "frames": 1}),
        ] {
            assert!(Record::parse(&bad).is_err(), "{bad}");
        }
    }

    /// measure_boxes refuses before anything is written: its arguments, a tag taken, the game's window not found (the
    /// Fake's game has none).
    #[test]
    fn the_recorder_refuses_and_writes_nothing() {
        let fx = crate::game::tests::Fake::new("capture-boxes");
        let one = json!([{"name": "a", "box": [0, 0, 0.5, 0.5]}]);
        let taken = fx.dir.join("data/captures/s1/T-boxes.csv");
        fs::create_dir_all(taken.parent().unwrap()).unwrap();
        fs::write(&taken, "synthetic").unwrap();
        for (args, why) in [
            (
                json!({"token": "nope", "tag": "R", "boxes": one, "frames": 1}),
                "wrong or missing token",
            ),
            (
                json!({"token": "tok", "tag": "R", "boxes": one}),
                "secs or frames",
            ),
            (
                json!({"token": "tok", "tag": "R", "boxes": one, "frames": 1, "x": 1}),
                "unknown argument",
            ),
            (
                json!({"token": "tok", "tag": "T", "boxes": one, "frames": 1}),
                "is taken in this session",
            ),
            (
                json!({"token": "tok", "tag": "R", "boxes": one, "frames": 1}),
                "no visible window",
            ),
        ] {
            let out = fx.run("measure_boxes", args.clone());
            let reason = out["reason"].as_str().unwrap_or_default();
            assert!(
                out["refused"] == true && reason.contains(why),
                "{args}: {out}"
            );
        }
        let l = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        let rows = l
            .query("SELECT count(*) FROM captures", &[], Duration::from_secs(5))
            .unwrap();
        assert_eq!(rows.rows, [[json!(0)]]);
    }

    /// After a grab: the camera and the player through the channel as a poses row named for the capture, the game time
    /// and photo mode for frame_state; nothing when the game doesn't answer (each read bounded) or no game is loaded.
    #[test]
    fn reads_the_frames_pose_after_the_grab() {
        let fx = served("capture-pose");
        let server = fake_game(
            &token(&fx),
            2,
            Box::new(|r| match r.verb {
                Verb::CamGet {} => {
                    let cam = json!({"camera": "photomode", "pos": [1, 2, 3], "rot": [-5, 90, 0], "fov": 60});
                    vec![reply(r.id, true, "camera", Some(cam))]
                }
                Verb::WorldGet {} => {
                    let world = json!({"player": {"pos": [4, 5, 6], "heading": 45}, "time": {"day": 3, "hour": 22},
                                       "weather": "WT_Clear"});
                    vec![reply(r.id, true, "world", Some(world))]
                }
                _ => vec![],
            }),
        );
        let session = fx.session();
        let cx = game::ctx(&session, &json!({"token": "tok"}), &[]).unwrap();
        let mut call = session.call(crate::mcp::Conn::nobody(), Value::Null, None);
        let mut ledger = Ledger::open(fx.dir.join("data/state/quen.db")).unwrap();
        let (id, state) = posed(&cx, &mut call, &mut ledger, "A1");
        assert_eq!(server.join().unwrap().len(), 2);
        assert_eq!(
            Value::Object(state),
            json!({"photo": true, "time": {"day": 3, "hour": 22}})
        );
        let sql = "SELECT name, cam_x, cam_y, cam_z, cam_pitch, cam_yaw, cam_roll, fov, player_x, player_y, player_z, \
                   player_yaw FROM poses WHERE id = ?1";
        let row = ledger
            .query(sql, &[json!(id.unwrap())], Duration::from_secs(5))
            .unwrap()
            .rows;
        assert_eq!(
            row,
            [json!([
                "s1/A1", 1.0, 2.0, 3.0, -5.0, 90.0, 0.0, 60.0, 4.0, 5.0, 6.0, 45.0
            ])
            .as_array()
            .unwrap()
            .clone()]
        );

        // A game that doesn't answer: no pose, no state, after one bounded read.
        let silent = fake_game(&token(&fx), 1, Box::new(|_| vec![]));
        let start = Instant::now();
        let (pose, state) = frame_pose(&cx, "s1/A2");
        assert!(pose.is_none() && state.is_empty(), "{state:?}");
        assert!(start.elapsed() < POSE_WAIT * 2, "{:?}", start.elapsed());
        assert_eq!(silent.join().unwrap().len(), 1);
        // No loaded game in quen's state: nothing sent.
        fs::write(
            fx.dir.join("game/bin/x64_dx12/W3QuenState.log"),
            "push flags=0\n",
        )
        .unwrap();
        let (pose, state) = frame_pose(&cx, "s1/A3");
        assert!(pose.is_none() && state.is_empty());
    }

    /// HDR's choice with fake grabbers: WGC when it works; else Desktop Duplication, only with the game in front before
    /// and after the frame.
    #[test]
    fn hdr_falls_back_to_desktop_duplication_only_in_front() {
        use std::cell::Cell;
        let frame = || {
            Ok(Image {
                width: 1,
                height: 1,
                pixels: Px::ScrgbF16(vec![f16::ONE; 4]),
            })
        };
        let unavailable = || Err("not supported on this system".to_string());
        let unused = || -> Result<Image, String> { panic!("not called") };
        // The foreground answers, in order; a call past the list fails the test.
        let front = |answers: &'static [bool]| {
            let n = Cell::new(0);
            move || {
                n.set(n.get() + 1);
                answers[n.get() - 1]
            }
        };

        let wgc = hdr(frame, unused, &front(&[])).unwrap();
        assert_eq!((wgc.method, wgc.wgc_error), ("wgc_fp16", None));

        let fallback = hdr(unavailable, frame, &front(&[true, true])).unwrap();
        assert_eq!(fallback.method, DESKTOP_DUPLICATION);
        assert!(fallback.wgc_error.unwrap().contains("not supported"));

        let behind = hdr(unavailable, unused, &front(&[false])).err().unwrap();
        assert!(behind.contains("needs the game in front"), "{behind}");

        let taken = Cell::new(false);
        let moved = hdr(
            unavailable,
            || {
                taken.set(true);
                frame()
            },
            &front(&[true, false]),
        );
        let moved = moved.err().unwrap();
        assert!(taken.get() && moved.contains("dropped"), "{moved}");
    }

    /// Synthetic frames through encode and store: one file per encoding under the boot folder, one ledger row each,
    /// never over an existing file.
    #[test]
    fn stores_each_file_with_its_row() {
        let dir = std::env::temp_dir().join(format!("quen-capture-test-{}", std::process::id()));
        let (root, db) = (dir.join("captures"), dir.join("quen.db"));
        fs::create_dir_all(&dir).unwrap();
        let ledger = || Ledger::open(&db).unwrap();
        let row = Capture {
            method: Some("printwindow".into()),
            display_state: Some(Map::from_iter([("color_mode".into(), json!("hdr"))])),
            t: "2026-01-01T00:00:00Z".into(),
            ..Capture::default()
        };
        let sdr = || Image {
            width: 2,
            height: 1,
            pixels: Px::Bgra8(vec![30, 20, 10, 255, 3, 2, 1, 255]),
        };
        let a1 = |ext: &str| format!("boot1/A1.{ext}");
        let (stored, failed) = store(
            &root,
            &a1,
            encode(sdr(), false).unwrap(),
            &row,
            &mut ledger(),
        );
        assert_eq!(failed, None);
        assert_eq!(stored[0].rel, "boot1/A1.png");
        let png = fs::read(root.join("boot1/A1.png")).unwrap();
        let sha = sha256_hex(&png);
        let mut reader = png::Decoder::new(Cursor::new(png)).read_info().unwrap();
        let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut rgb).unwrap();
        assert_eq!(rgb, [10, 20, 30, 1, 2, 3], "BGRA becomes RGB");

        let hdr = || Image {
            width: 1,
            height: 1,
            pixels: Px::ScrgbF16([1.0, 0.5, 0.25, 1.0].map(f16::from_f32).to_vec()),
        };
        let b2 = |ext: &str| format!("boot1/B2.{ext}");
        let (stored, failed) = store(
            &root,
            &b2,
            encode(hdr(), true).unwrap(),
            &row,
            &mut ledger(),
        );
        assert_eq!(failed, None);
        let encodings: Vec<Option<Encoding>> = stored.iter().map(|s| s.encoding).collect();
        assert_eq!(
            encodings,
            [Some(Encoding::ScrgbF16), Some(Encoding::PqBt2020U16)]
        );
        // Without npy, an HDR shot is its PNG alone.
        let b3 = |ext: &str| format!("boot1/B3.{ext}");
        let (stored, failed) = store(
            &root,
            &b3,
            encode(hdr(), false).unwrap(),
            &row,
            &mut ledger(),
        );
        assert_eq!(failed, None);
        assert_eq!(stored[0].rel, "boot1/B3.png");
        assert_eq!(stored.len(), 1);
        // Each row's kind is its file's extension (feature tests 1: one meaning for captures.kind).
        let sql = "SELECT path, kind FROM captures WHERE path NOT LIKE '%.' || kind";
        let odd = ledger().query(sql, &[], Duration::from_secs(5)).unwrap();
        assert!(odd.rows.is_empty(), "{:?}", odd.rows);

        let (again, failed) = store(
            &root,
            &a1,
            encode(sdr(), false).unwrap(),
            &row,
            &mut ledger(),
        );
        assert!(again.is_empty() && failed.unwrap().contains("A1.png"));

        // A shot's rows go in together or not at all: the second row's path is taken, so the first isn't added.
        let taken = Capture {
            path: "boot1/C3.png".into(),
            sha256: "0".repeat(64),
            encoding: Some(Encoding::PqBt2020U16),
            ..row.clone()
        };
        ledger().add_capture(&taken).unwrap();
        let c3 = |ext: &str| format!("boot1/C3.{ext}");
        let one = Image {
            width: 1,
            height: 1,
            pixels: Px::ScrgbF16(vec![f16::ONE; 4]),
        };
        let (stored, failed) = store(&root, &c3, encode(one, true).unwrap(), &row, &mut ledger());
        assert!(failed.unwrap().contains("no ledger rows"));
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|s| s.id.is_none()));

        let sql =
            "SELECT path, encoding, method, json_extract(display_state, '$.color_mode'), sha256
                   FROM captures ORDER BY id";
        let rows = ledger()
            .query(sql, &[], Duration::from_secs(5))
            .unwrap()
            .rows;
        let want = [
            ["boot1/A1.png", "srgb8"],
            ["boot1/B2.npy", "scrgb_f16"],
            ["boot1/B2.png", "pq_bt2020_u16"],
            ["boot1/B3.png", "pq_bt2020_u16"],
            ["boot1/C3.png", "pq_bt2020_u16"],
        ];
        assert_eq!(rows.len(), want.len());
        for (row, want) in rows.iter().zip(want) {
            assert_eq!(row[..2], want.map(Value::from));
            assert_eq!(row[2..4], [json!("printwindow"), json!("hdr")]);
        }
        assert_eq!(rows[0][4], sha);
        fs::remove_dir_all(dir).unwrap(); // this test's own temp folder
    }

    /// A small topmost window of this process, filled with one sRGB colour by its class brush.
    struct TestWindow(HWND, HBRUSH);

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: the arguments Windows passed.
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    impl TestWindow {
        fn show([r, g, b]: [u8; 3]) -> TestWindow {
            // SAFETY: a class and window of this test's own, destroyed on drop on this thread.
            unsafe {
                let brush = CreateSolidBrush(COLORREF(u32::from_le_bytes([r, g, b, 0])));
                let class = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hbrBackground: brush,
                    lpszClassName: w!("quen-capture-test"),
                    ..Default::default()
                };
                assert_ne!(RegisterClassW(&class), 0);
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("quen-capture-test"),
                    w!("quen capture test"),
                    WS_POPUP | WS_VISIBLE,
                    64,
                    64,
                    320,
                    200,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                // Let it paint, and DWM compose it.
                let start = Instant::now();
                while start.elapsed() < Duration::from_millis(400) {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    sleep(Duration::from_millis(10));
                }
                TestWindow(hwnd, brush)
            }
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: this test's own window, class and brush, each released once.
            unsafe {
                let _ = DestroyWindow(self.0);
                let _ = UnregisterClassW(w!("quen-capture-test"), Some(HINSTANCE::default()));
                let _ = DeleteObject(self.1.into());
            }
        }
    }

    fn srgb_to_linear(c: u8) -> f32 {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Every method on a window this test shows itself (never the game or the desktop; nothing is written): found
    /// from this process's pid, and its fill read back. It shows a small topmost window for about two seconds, so it
    /// runs only when asked: `cargo test -p quen -- --ignored grabs_a_window`.
    #[test]
    #[ignore = "shows a window on the desktop"]
    fn grabs_a_window_of_its_own() {
        let _dpi = Dpi::per_monitor();
        let fill = [200, 100, 50];
        let window = TestWindow::show(fill);
        let (found, rect) = game_window(&[std::process::id()]).unwrap();
        assert_eq!(found, window.0);
        assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (320, 200));
        assert!(game_window(&[u32::MAX]).is_err());

        let display = display_state(found);
        assert!(!display.contains_key("error"), "{display:?}");
        // SDR content on an HDR desktop is composed at the SDR white level; otherwise sRGB white is scRGB 1.0.
        let scale = match display["color_mode"].as_str() {
            Some("hdr") => display["sdr_white_nits"].as_f64().unwrap() as f32 / 80.0,
            _ => 1.0,
        };

        let pin = Pin::show(&rect).unwrap();
        let pin_window = pin.0;
        drop(pin);
        // SAFETY: a plain query; the handle is only looked up.
        assert!(!unsafe { IsWindow(Some(pin_window)) }.as_bool());

        // Each grabber on its own: the test window never takes the focus, so the fallback's guard would refuse it.
        let d3d = D3d::for_window(found).unwrap();
        let grabs = [
            ("printwindow", gdi(found, &rect)),
            ("wgc", d3d.wgc(found).map_err(|e| e.to_string())),
            (
                DESKTOP_DUPLICATION,
                d3d.duplicate(&rect).map_err(|e| e.to_string()),
            ),
        ];
        for (method, image) in grabs {
            let image = image.unwrap_or_else(|e| panic!("{method}: {e}"));
            assert_eq!((image.width, image.height), (320, 200), "{method}");
            let centre = (100 * 320 + 160) * 4;
            match image.pixels {
                Px::Bgra8(p) => assert_eq!(p[centre..centre + 3], [50, 100, 200], "{method}"),
                Px::ScrgbF16(p) => {
                    let got: Vec<f32> = p[centre..centre + 3].iter().map(|v| v.to_f32()).collect();
                    for (c, v) in fill.iter().zip(&got) {
                        let want = srgb_to_linear(*c) * scale;
                        assert!(
                            (v / want - 1.0).abs() < 0.15,
                            "{method}: {got:?} for sRGB {fill:?} at scale {scale}"
                        );
                    }
                }
            }
        }
    }
}
