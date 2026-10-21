//! `game_screenshot`: one frame of the game's window, which is found from the pids in the session's job (never
//! by window title or image name), written under `<capture root>\<session>\` (beside measure's CSVs; one session is
//! one boot) and recorded in the ledger under the session's boot, with its `encoding` and the display's state.
//!
//! - `sdr`, the default: grab.py ported. PrintWindow(PW_RENDERFULLCONTENT) of the window. An 8-bit sRGB PNG, clipped
//!   on an HDR display.
//! - `hdr`: Windows.Graphics.Capture of the window in FP16 (scRGB). Where WGC is unavailable or fails (not every
//!   Windows install has it working), DXGI Desktop Duplication of the window's monitor, cropped to the window. A
//!   float16 `.npy` (scRGB as captured) and a 16-bit PQ BT.2020 PNG with cICP, through quen-core's encoder. The
//!   capture session is closed before the tool returns: none may stay open into a PresentMon window. The config's
//!   test switch `force_duplication` skips WGC, so a live boot exercises the fallback.
//! - `renderdoc`: a RenderDoc frame capture in a `renderdoc:NAME` session (profile.rs).
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
use quen_core::hdr::{Encoding, Frame, Pixels};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Capture, Ledger};
use serde_json::{Map, Value, json};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
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
    CreateWindowExW, DestroyWindow, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, LWA_ALPHA,
    PW_RENDERFULLCONTENT, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, ShowWindow, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, w};

use crate::game::{self, Fields};
use crate::session::{self, Call, Done, Session};

/// grab.py's settle after showing the pin: a few frames for DWM to compose.
const PIN_SETTLE: Duration = Duration::from_millis(250);
/// How long a capture API may take to hand over its first frame.
const FRAME_WAIT: Duration = Duration::from_secs(2);

/// The `tools/list` entry.
pub fn tool() -> Value {
    json!({
        "name": "game_screenshot",
        "description": "One frame of the game's window (found from the session's job, never by title) into the \
            session's capture folder, a ledger capture row per file. method sdr (default): an 8-bit sRGB PNG (srgb8), \
            clipped on an HDR display: for OCR, UI checks and SDR A/B. method hdr: Windows.Graphics.Capture in FP16 \
            (wgc_fp16), written as a float16 .npy of linear scRGB (scrgb_f16, 1.0 = 80 nits) and a 16-bit PQ BT.2020 \
            PNG with cICP (pq_bt2020_u16: code values). If WGC fails (wgc_error says why), quen falls back to Desktop \
            Duplication of the game's screen area (desktop_duplication, includes_desktop true: an overlay can \
            show), only while the game is in front; a focus change drops the frame and refuses. method renderdoc: one \
            RenderDoc capture, minutes (see method). Compare only captures of one method, encoding and display state \
            (each row records them). Refused, nothing changed: wrong token, the tag taken in this session, no visible \
            game window, no frame; with needs: method hdr while HDR is off (take sdr).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "token": {"type": "string", "description": "the token session_launch returned"},
                "tag": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "the file name, unique in the session (A1, B2, ...)"},
                "method": {"type": "string", "enum": ["sdr", "hdr", "renderdoc"], "description": "default sdr. renderdoc, only in a session launched with profiler renderdoc:NAME: one RenderDoc capture (F12) as <capture root>\\<boot>\\<tag>.rdc (3-5 GB; 12 GB free needed), the game in front; it sends nothing more and returns once the capture is written (up to minutes: the game freezes while RenderDoc writes it), never without the Streamline swap in place"},
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
}

impl Shot {
    /// The arguments besides the token, which `game::ctx` checks.
    fn parse(args: &Value) -> Result<Shot, String> {
        let tag = game::text(args, "tag")?;
        let fine = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if tag.is_empty() || tag.len() > 64 || !tag.chars().all(fine) {
            return Err("tag: 1-64 letters, digits, _ or - (it names the file)".into());
        }
        let (hdr, renderdoc) = match args.get("method").map(Value::as_str) {
            None | Some(Some("sdr")) => (false, false),
            Some(Some("hdr")) => (true, false),
            Some(Some("renderdoc")) => (false, true),
            Some(m) => return Err(format!("method {m:?}: sdr, hdr or renderdoc")),
        };
        Ok(Shot {
            tag: tag.into(),
            hdr,
            renderdoc,
        })
    }

    /// The file extensions this shot writes.
    fn extensions(&self) -> &'static [&'static str] {
        if self.hdr { &["npy", "png"] } else { &["png"] }
    }
}

pub(crate) fn screenshot(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["tag", "method"])?;
    let shot = Shot::parse(args)?;
    if shot.renderdoc {
        return crate::profile::capture(&cx, call, &shot.tag);
    }
    let root = cx.config.capture_root();
    // Windows' separator, so the artifact's absolute path has one kind; the ledger stores `/` either way.
    let rel = |ext: &str| format!("{}\\{}.{ext}", cx.live.id, shot.tag);
    if let Some(taken) = shot
        .extensions()
        .iter()
        .map(|ext| root.join(rel(ext)))
        .find(|p| p.exists())
    {
        let why = format!(
            "{} exists: tag {:?} is taken in this session",
            taken.display(),
            shot.tag
        );
        return Err(why.into());
    }
    let pids = cx.live.job.pids().unwrap_or_default();
    let _dpi = Dpi::per_monitor();
    let (window, rect) = game_window(&pids)?;
    if call.stopped() {
        return Err(Done::Refused(
            "cancelled before anything was changed".into(),
        ));
    }
    let ledger =
        Ledger::open(cx.config.state_dir().join("quen.db")).map_err(|e| format!("ledger: {e}"))?;
    call.lap("check");
    crate::notice::clear(cx.live, call).map_err(Done::Refused)?;
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
    let includes_desktop = method == DESKTOP_DUPLICATION;
    let files = encode(image).map_err(|why| format!("{method}: {why}; nothing written"))?;
    call.lap("encode");
    let t = ledger
        .utc(at)
        .map_err(|e| format!("ledger: {e}; nothing written"))?;
    let row = Capture {
        boot_id: Some(cx.live.boot),
        method: Some(method.into()),
        includes_overlay: Some(true),
        includes_desktop: Some(includes_desktop),
        display_state: Some(display.clone()),
        t,
        ..Capture::default()
    };
    let (stored, failed) = store(&root, &rel, files, &row, ledger);
    call.lap("store");
    let mut captures = Vec::new();
    for s in &stored {
        let path = root.join(&s.rel);
        call.artifact(&path);
        captures.push(json!({"path": path, "encoding": s.encoding, "id": s.id}));
        if let Some(id) = s.id {
            call.row("captures", id);
        }
    }
    let mut fields = Map::new();
    fields.insert("captures".into(), json!(captures));
    fields.insert("method".into(), json!(method));
    fields.insert("includes_desktop".into(), json!(includes_desktop));
    if let Some(why) = wgc_error {
        fields.insert("wgc_error".into(), json!(why));
    }
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

/// One encoded file: its extension, encoding and bytes.
type Encoded = (&'static str, Encoding, Vec<u8>);

/// SDR: an RGB PNG. HDR: the `.npy` of the half floats as captured, and the 16-bit PQ BT.2020 PNG.
fn encode(image: Image) -> Result<Vec<Encoded>, String> {
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
            // Live boot 1: the default deflate took 1.4 s on a real 4K frame.
            encoder.set_compression(png::Compression::Fast);
            encoder
                .write_header()
                .and_then(|mut w| w.write_image_data(&rgb))
                .map_err(|e| format!("PNG: {e}"))?;
            Ok(vec![("png", Encoding::Srgb8, png)])
        }
        Px::ScrgbF16(halves) => {
            let frame =
                Frame::new(width, height, Pixels::ScrgbF16(halves)).map_err(|e| e.to_string())?;
            let (mut npy, mut png) = (Vec::new(), Vec::new());
            frame.write_npy(&mut npy).map_err(|e| e.to_string())?;
            frame
                .write_png_pq(Cursor::new(&mut png))
                .map_err(|e| e.to_string())?;
            Ok(vec![
                ("npy", Encoding::ScrgbF16, npy),
                ("png", Encoding::PqBt2020U16, png),
            ])
        }
    }
}

/// One file written, and its ledger id if its row went in.
struct Stored {
    rel: String,
    encoding: Encoding,
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
    mut ledger: Ledger,
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
            encoding: Some(encoding),
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
            method: "printwindow",
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
                method: "wgc_fp16",
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
        // Two buffers: the smallest pool moonbridge's capture runs with.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            DirectXPixelFormat::R16G16B16A16Float,
            2,
            item.Size()?,
        )?;
        // ponytail: no pool.Close(). Closing the free-threaded pool of another process's window crashed a WGC worker
        // thread moments later (an access violation, every time in testing); releasing it does not. The session's
        // Close ends the capture.
        pool.CreateCaptureSession(&item).and_then(|session| {
            let image = (|| {
                // Windows 11 draws no yellow border when asked not to; older builds refuse, harmlessly.
                let _ = session.SetIsBorderRequired(false);
                session.SetIsCursorCaptureEnabled(false)?;
                session.StartCapture()?;
                let start = Instant::now();
                let frame = loop {
                    match pool.TryGetNextFrame() {
                        Ok(frame) => break frame,
                        Err(e) if start.elapsed() > FRAME_WAIT => return Err(e),
                        Err(_) => sleep(Duration::from_millis(1)),
                    }
                };
                let size = frame.ContentSize()?;
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                // SAFETY: the frame's surface is a D3D11 texture on this device.
                let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
                let image = self.read(&texture, 0, 0, size.Width as u32, size.Height as u32);
                let _ = frame.Close();
                image
            })();
            let _ = session.Close();
            image
        })
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
        // SAFETY: COM calls on objects created here; the mapped rows are read within the map, by its row pitch.
        unsafe {
            texture.GetDesc(&mut desc);
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
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
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
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(windows::core::Error::empty)?;
            let region = D3D11_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + w,
                bottom: y + h,
                back: 1,
            };
            self.context
                .CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, 0, Some(&region));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let mut halves = Vec::with_capacity(w as usize * h as usize * 4);
            for row in 0..h as usize {
                let line = mapped
                    .pData
                    .cast::<u8>()
                    .add(row * mapped.RowPitch as usize);
                let line = std::slice::from_raw_parts(line.cast::<u16>(), w as usize * 4);
                halves.extend(line.iter().map(|&bits| f16::from_bits(bits)));
            }
            self.context.Unmap(&staging, 0);
            Ok(Image {
                width: w,
                height: h,
                pixels: Px::ScrgbF16(halves),
            })
        }
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
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DispatchMessageW, IsWindow, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
        UnregisterClassW, WNDCLASSW, WS_VISIBLE,
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
        assert_eq!((shot.hdr, shot.extensions()), (true, &["npy", "png"][..]));
        for bad in [
            json!({}),
            json!({"tag": "../x"}),
            json!({"tag": "a.png"}),
            json!({"tag": "x".repeat(65)}),
            json!({"tag": "A", "method": "jxr"}),
            json!({"tag": "A", "method": 1}),
        ] {
            assert!(Shot::parse(&bad).is_err(), "{bad}");
        }
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
        let (stored, failed) = store(&root, &a1, encode(sdr()).unwrap(), &row, ledger());
        assert_eq!(failed, None);
        assert_eq!(stored[0].rel, "boot1/A1.png");
        let png = fs::read(root.join("boot1/A1.png")).unwrap();
        let sha = sha256_hex(&png);
        let mut reader = png::Decoder::new(Cursor::new(png)).read_info().unwrap();
        let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut rgb).unwrap();
        assert_eq!(rgb, [10, 20, 30, 1, 2, 3], "BGRA becomes RGB");

        let halves = [1.0, 0.5, 0.25, 1.0].map(f16::from_f32).to_vec();
        let hdr = Image {
            width: 1,
            height: 1,
            pixels: Px::ScrgbF16(halves),
        };
        let b2 = |ext: &str| format!("boot1/B2.{ext}");
        let (stored, failed) = store(&root, &b2, encode(hdr).unwrap(), &row, ledger());
        assert_eq!(failed, None);
        let encodings: Vec<Encoding> = stored.iter().map(|s| s.encoding).collect();
        assert_eq!(encodings, [Encoding::ScrgbF16, Encoding::PqBt2020U16]);
        // Each row's kind is its file's extension (feature tests 1: one meaning for captures.kind).
        let sql = "SELECT path, kind FROM captures WHERE path NOT LIKE '%.' || kind";
        let odd = ledger().query(sql, &[], Duration::from_secs(5)).unwrap();
        assert!(odd.rows.is_empty(), "{:?}", odd.rows);

        let (again, failed) = store(&root, &a1, encode(sdr()).unwrap(), &row, ledger());
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
        let (stored, failed) = store(&root, &c3, encode(one).unwrap(), &row, ledger());
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
