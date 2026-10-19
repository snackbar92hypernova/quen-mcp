//! A stand-in for the game in quen's end-to-end tests (tests/session.rs), which copy it into a synthetic game folder
//! under the configured image name. It shows a borderless 320x200 window of one colour (sRGB 200, 100, 50) and runs
//! until it is killed, its pid in `pid` beside it. Files beside it steer it, as the game's own files would:
//! - `on_screen`: the window sits at (96, 96), topmost (for the screenshot test); otherwise it is far off screen, so
//!   the tests don't show it, and it never takes the focus;
//! - `dialog`: a titled dialog instead (off screen too), as the game shows a script compile error;
//! - `dxgi.dll` (ReShade): it writes `ReShade.log` with the RenoDX add-on's DevAutoReload line, and loads the DLL
//!   its `ReShade.ini`'s `LoadFromDllMain` names (relative to it), as ReShade loads the devkit;
//! - `WitcherNativeBridge.asi`: it writes `WitcherNativeBridge.log` as the Native Bridge does, with quen's script
//!   functions registered when `W3Quen.asi` is there too (`QUEN_State` FAILED when `natives_failed` is);
//! - `manual_save`: the gamesaves folder it names gets a `ManualSave_1.sav`, as a key that confirmed a save would;
//! - `input_settings`: the input.settings it names gets its lines in reverse order, as the game reorders them at every
//!   launch (live boot 1);
//! - a Mods folder in quen's parking folder: it rewrites the compiled scripts, as the game recompiles them when a
//!   script mod is missing (seen in the first experiment boot);
//! - `W3_QUEN_TRACY=1` in its environment (a tracy launch): as W3Quen.asi's Tracy part, it writes W3Quen.log's lines and
//!   listens on 127.0.0.1:`TRACY_PORT`, and logs the capture that connects, sending it a few bytes.
#![cfg_attr(not(windows), allow(unused))]

#[cfg(windows)]
fn main() {
    use std::fs;
    use std::path::Path;

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
        SW_SHOWNOACTIVATE, ShowWindow, WINDOW_EX_STYLE, WNDCLASSW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };
    use windows::core::w;

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: the arguments Windows passed.
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    let exe = std::env::current_exe().unwrap();
    let here = exe.parent().unwrap();
    let flag = |name: &str| here.join(name).exists();
    fs::write(here.join("pid"), std::process::id().to_string()).unwrap();
    if flag("dxgi.dll") {
        fs::write(
            here.join("ReShade.log"),
            "Registered add-on \"RenoDX\"\nDevAutoReload: watching\n",
        )
        .unwrap();
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
        }
        let ini = fs::read_to_string(here.join("ReShade.ini")).unwrap_or_default();
        let devkit = ini
            .lines()
            .find_map(|l| l.strip_prefix("LoadFromDllMain="))
            .map(str::trim)
            .filter(|path| !path.is_empty());
        if let Some(path) = devkit {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = here
                .join(path)
                .as_os_str()
                .encode_wide()
                .chain([0])
                .collect();
            // SAFETY: a NUL-terminated path; the module stays loaded until the process ends.
            unsafe { LoadLibraryW(wide.as_ptr()) };
        }
    }
    if flag("WitcherNativeBridge.asi") {
        let state = if flag("natives_failed") {
            "FAILED"
        } else {
            "OK"
        };
        let quen = [
            ("QUEN_Next", "OK"),
            ("QUEN_Reply", "OK"),
            ("QUEN_State", state),
        ];
        let lines: String = quen
            .iter()
            .filter(|_| flag("W3Quen.asi"))
            .map(|(name, s)| {
                format!("00:00:00.000 WitcherNativeBridge: native registration: {name}={s}\n")
            })
            .collect();
        let log = format!("00:00:00.000 WitcherNativeBridge: startup\n{lines}");
        fs::write(here.join("WitcherNativeBridge.log"), log).unwrap();
    }
    let game = here.parent().unwrap().parent().unwrap();
    let compiled = game.join(r"content\content0\x64.final.redscripts");
    if game.join(r"quen-parked\Mods").exists() && compiled.exists() {
        fs::write(compiled, "recompiled without the parked mods").unwrap();
    }
    if let Ok(input) = fs::read_to_string(here.join("input_settings")) {
        let old = fs::read_to_string(input.trim()).unwrap_or_default();
        let new: Vec<&str> = old.split_inclusive('\n').rev().collect();
        fs::write(input.trim(), new.concat()).unwrap();
    }
    if let Ok(saves) = fs::read_to_string(here.join("manual_save")) {
        fs::write(Path::new(saves.trim()).join("ManualSave_1.sav"), "manual").unwrap();
    }
    if std::env::var("W3_QUEN_TRACY").as_deref() == Ok("1") {
        use std::io::Write;
        let port = std::env::var("TRACY_PORT").unwrap();
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        let log = here.join("W3Quen.log");
        fs::write(
            &log,
            "1 tracy: 75 of 75 hooks installed\n2 tracy: started (timer 1.0 ticks/us)\n",
        )
        .unwrap();
        std::thread::spawn(move || {
            let (mut capture, _) = listener.accept().unwrap();
            let mut text = fs::read_to_string(&log).unwrap();
            text.push_str("3 tracy: a capture connected\n");
            fs::write(&log, text).unwrap();
            capture.write_all(b"zones").unwrap();
            loop {
                std::thread::park(); // the connection stays open until the game is killed
            }
        });
    }
    // SAFETY: a class and window of this process's own; the message loop runs until the process is killed.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hbrBackground: CreateSolidBrush(COLORREF(u32::from_le_bytes([200, 100, 50, 0]))),
            lpszClassName: w!("quen-fake-game"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&class), 0);
        let (ex, at) = if flag("on_screen") {
            (WS_EX_TOPMOST, 96)
        } else {
            (WINDOW_EX_STYLE(0), -20000)
        };
        // The dialog is the system's dialog class with a title, as a message box of the game's would be.
        let (class, title) = if flag("dialog") {
            (w!("#32770"), w!("Script Compilation Errors"))
        } else {
            (w!("quen-fake-game"), w!("quen fake game"))
        };
        let hwnd = CreateWindowExW(
            ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            title,
            WS_POPUP | WS_VISIBLE,
            at,
            at,
            320,
            200,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn main() {}
