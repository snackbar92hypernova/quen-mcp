//! Focus-safe input into the game, ported from w3ctl.py (keys) and console_type.py (console lines). quen-core
//! plans the events; this sends them with SendInput as Set-1 scancodes, only to the game's own window (the window of
//! the session's game process, never one found by title), and only while that window is in front: the game pauses into
//! its menu when it loses focus, and keys sent then would land in another window or act on that menu.

use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::input::{Event, GRAVE, describe};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, MapVirtualKeyExW,
    SendInput, VIRTUAL_KEY, VK_OEM_3,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
};

/// Marks quen's own key events (their dwExtraInfo), so a window can tell them from the user's keys.
pub(crate) const MARK: usize = 0x5155_454E; // "QUEN"

pub(crate) fn in_front(window: HWND) -> bool {
    // SAFETY: a plain query.
    unsafe { GetForegroundWindow() == window }
}

/// The game's console key and where it came from. The game has no console binding to read (input.settings binds no
/// console action, bin\config only switches the console on): the engine opens it on IK_Tilde, the virtual key
/// VK_OEM_3, which sits wherever the active layout puts it (w3ctl's rule, verified live on a non-US layout). So it is
/// that key's scancode on the layout of the window's thread, or the US position if the layout has no VK_OEM_3.
pub(crate) fn console_key(window: HWND) -> (u16, &'static str) {
    match layout_scancode(window)(VK_OEM_3.0) {
        Some(sc) => (sc, "VK_OEM_3 on the game window's keyboard layout"),
        None => (
            GRAVE,
            "the US position: the game window's layout has no VK_OEM_3",
        ),
    }
}

/// Virtual key → scancode on the keyboard layout of the window's thread, the layout the game reads keys through (the
/// console maps each key's virtual key there through a US table). None: the layout has no key for it.
pub(crate) fn layout_scancode(window: HWND) -> impl Fn(u16) -> Option<u16> {
    // SAFETY: plain queries; a window that closed meanwhile gives thread 0, the current thread's layout.
    let layout = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(window, None)) };
    move |vk| {
        // SAFETY: a plain query.
        let sc = unsafe { MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, Some(layout)) };
        u16::try_from(sc).ok().filter(|&sc| sc != 0)
    }
}

/// w3ctl's `focus`: brings `window` to the front. Ok(true) if it was there already, Ok(false) if it came; Err if it
/// wouldn't (no input sent).
pub(crate) fn focus(window: HWND) -> Result<bool, String> {
    if in_front(window) {
        return Ok(true);
    }
    // SAFETY: plain window calls; the thread input attached here is detached again before the next attempt.
    unsafe {
        let me = GetCurrentThreadId();
        for attempt in 0..6u64 {
            if attempt > 0 {
                // PowerToys' trick: an empty mouse input (no move, no button) makes quen the source of the last input,
                // which lifts the foreground lock without a keystroke to another window.
                let empty = INPUT {
                    r#type: INPUT_MOUSE,
                    ..Default::default()
                };
                SendInput(&[empty], size_of::<INPUT>() as i32);
            }
            let front = GetForegroundWindow();
            let thread = if front.is_invalid() {
                0
            } else {
                GetWindowThreadProcessId(front, None)
            };
            let attached =
                thread != 0 && thread != me && AttachThreadInput(me, thread, true).as_bool();
            let _ = BringWindowToTop(window);
            let _ = SetForegroundWindow(window);
            if attached {
                let _ = AttachThreadInput(me, thread, false);
            }
            sleep(Duration::from_millis(50 + 50 * attempt));
            if in_front(window) {
                return Ok(false);
            }
        }
    }
    Err("the game's window would not come to the front; no input sent".into())
}

/// Why a plan stopped part-way, and how many keys went out before it did.
#[derive(Debug)]
pub(crate) struct Stopped {
    pub(crate) why: String,
    pub(crate) keys: usize,
}

/// Sends `plan` to `window`. The keys between two sleeps go out as one SendInput call (a chord: nothing of the user's or
/// another program's input comes between them; a console line's keys are `CHAR_MS` apart); before each such run, and
/// right after it, it checks
/// that the window is in front, and before it that `stop` (a cancel) isn't set; a sleep ends early on `stop`. On
/// either, or a failed SendInput, it sends nothing more but key-ups for the keys it holds down, last first (no key stays
/// down, in whatever window is in front), and stops.
pub(crate) fn send(window: HWND, plan: &[Event], stop: &dyn Fn() -> bool) -> Result<(), Stopped> {
    let mut held: Vec<u16> = Vec::new();
    let mut keys = 0;
    let mut at = 0;
    while at < plan.len() {
        if let Event::Sleep { ms, .. } = plan[at] {
            let end = Instant::now() + Duration::from_millis(ms);
            while !stop() && Instant::now() < end {
                sleep((end - Instant::now()).min(Duration::from_millis(50)));
            }
            at += 1;
            continue;
        }
        let run: Vec<(u16, bool)> = plan[at..]
            .iter()
            .map_while(|e| match e {
                Event::Key { sc, up, .. } => Some((*sc, *up)),
                Event::Sleep { .. } => None,
            })
            .collect();
        let mut why = if stop() {
            Some("stopped (a cancel, or quen shutting down)".to_string())
        } else if !in_front(window) {
            Some("the game lost the foreground".to_string())
        } else {
            None
        };
        let mut went = 0;
        if why.is_none() {
            let out = send_keys(&run);
            went = out.as_ref().map_or_else(|(n, _)| *n, |()| run.len());
            why = out.err().map(|(_, e)| e).or_else(|| {
                (!in_front(window))
                    .then(|| "the game lost the foreground as its keys went out".into())
            });
        }
        for &(sc, up) in &run[..went] {
            keys += 1;
            if !up {
                held.push(sc);
            } else if let Some(i) = held.iter().rposition(|&h| h == sc) {
                held.remove(i);
            }
        }
        if let Some(why) = why {
            let _ = send_keys(&held.iter().rev().map(|&sc| (sc, true)).collect::<Vec<_>>());
            let next = plan[at + went..]
                .iter()
                .find(|e| matches!(e, Event::Key { .. }))
                .map_or("the end".into(), describe);
            let why = format!("{why} before `{next}`; {keys} keys were sent, none is left down");
            return Err(Stopped { why, keys });
        }
        at += run.len();
    }
    Ok(())
}

/// Key events by scancode, marked as quen's, in one SendInput call. Err: how many went out, and why the rest didn't.
fn send_keys(keys: &[(u16, bool)]) -> Result<(), (usize, String)> {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(sc, up)| {
            let mut flags = KEYEVENTF_SCANCODE;
            if sc > 0xFF {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if up {
                flags |= KEYEVENTF_KEYUP;
            }
            let ki = KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: sc & 0xFF,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MARK,
            };
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki },
            }
        })
        .collect();
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: fully initialised INPUTs of the size passed.
    let went = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    if went != inputs.len() {
        let e = io::Error::last_os_error();
        return Err((
            went,
            format!("SendInput sent {went} of {}: {e}", inputs.len()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quen_core::input::{console_line, plan};
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
    use std::sync::{Mutex, MutexGuard, mpsc};
    use std::thread;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ACTIVATE_KEYBOARD_LAYOUT_FLAGS, ActivateKeyboardLayout, GetKeyboardLayoutList, HKL,
        KLF_NOTELLSHELL, LoadKeyboardLayoutW, UnloadKeyboardLayout,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageExtraInfo, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassW, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };
    use windows::core::w;

    /// One window test at a time: they share the foreground and these statics.
    static ONE: Mutex<()> = Mutex::new(());
    /// The quen-marked key messages the test windows got: (window, scancode with 0x100 for E0, up, virtual key).
    static SEEN: Mutex<Vec<(isize, u16, bool, u16)>> = Mutex::new(Vec::new());
    /// A key message (`code`) on which window A puts window B in front: a focus loss in the middle of a plan.
    static TRIGGER: AtomicU32 = AtomicU32::new(0);
    static B: AtomicIsize = AtomicIsize::new(0);
    static QUIT: AtomicBool = AtomicBool::new(false);
    /// A keyboard layout for the windows' thread to switch to (0: none), and the one it had before, once it has.
    static LAYOUT: AtomicIsize = AtomicIsize::new(0);
    static SWITCHED: AtomicIsize = AtomicIsize::new(0);

    fn code(sc: u16, up: bool) -> u32 {
        (u32::from(sc) << 1 | u32::from(up)) + 1
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: the arguments Windows passed; the extra info is that of the message being handled.
        unsafe {
            let keyed = matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP);
            if !keyed || GetMessageExtraInfo().0 as usize != MARK {
                return DefWindowProcW(hwnd, msg, w, l);
            }
            let sc = ((l.0 >> 16) & 0xFF) as u16 | if l.0 & (1 << 24) != 0 { 0x100 } else { 0 };
            let up = matches!(msg, WM_KEYUP | WM_SYSKEYUP);
            SEEN.lock()
                .unwrap()
                .push((hwnd.0 as isize, sc, up, w.0 as u16));
            if TRIGGER.load(Ordering::SeqCst) == code(sc, up) {
                TRIGGER.store(0, Ordering::SeqCst);
                let _ = SetForegroundWindow(HWND(B.load(Ordering::SeqCst) as *mut _));
            }
            LRESULT(0)
        }
    }

    /// Two small topmost windows of this test (never the game or another app's), on a thread that pumps their
    /// messages until dropped.
    struct Windows {
        a: HWND,
        b: HWND,
        pump: Option<thread::JoinHandle<()>>,
    }

    impl Windows {
        fn open() -> Windows {
            QUIT.store(false, Ordering::SeqCst);
            SEEN.lock().unwrap().clear();
            let (tx, rx) = mpsc::channel();
            let pump = thread::spawn(move || {
                // SAFETY: a class and windows of this test's own, destroyed on this thread; a layout switch affects
                // this thread only.
                unsafe {
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(proc),
                        lpszClassName: w!("quen-input-test"),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let window = |x| {
                        let h = CreateWindowExW(
                            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                            w!("quen-input-test"),
                            w!("quen input test"),
                            WS_POPUP | WS_VISIBLE,
                            x,
                            64,
                            160,
                            100,
                            None,
                            None,
                            None,
                            None,
                        )
                        .unwrap();
                        h.0 as isize
                    };
                    let (a, b) = (window(64), window(256));
                    tx.send((a, b)).unwrap();
                    while !QUIT.load(Ordering::SeqCst) {
                        let want = LAYOUT.swap(0, Ordering::SeqCst);
                        if want != 0 {
                            let flags = ACTIVATE_KEYBOARD_LAYOUT_FLAGS(0);
                            let old = ActivateKeyboardLayout(HKL(want as *mut _), flags);
                            SWITCHED.store(old.map_or(-1, |h| h.0 as isize), Ordering::SeqCst);
                        }
                        let mut msg = MSG::default();
                        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                            DispatchMessageW(&msg);
                        }
                        sleep(Duration::from_millis(2));
                    }
                    for h in [a, b] {
                        let _ = DestroyWindow(HWND(h as *mut _));
                    }
                }
            });
            let (a, b) = rx.recv().unwrap();
            B.store(b, Ordering::SeqCst);
            Windows {
                a: HWND(a as *mut _),
                b: HWND(b as *mut _),
                pump: Some(pump),
            }
        }

        /// What `window` got since the last call, once its messages are handled: (scancode, up, virtual key).
        fn take(&self, window: HWND) -> Vec<(u16, bool, u16)> {
            sleep(Duration::from_millis(150));
            let mut all = SEEN.lock().unwrap();
            let mine = all
                .iter()
                .filter(|(h, ..)| *h == window.0 as isize)
                .map(|&(_, sc, up, vk)| (sc, up, vk))
                .collect();
            all.retain(|(h, ..)| *h != window.0 as isize);
            mine
        }

        /// The same, as (scancode, up).
        fn seen(&self, window: HWND) -> Vec<(u16, bool)> {
            let got = self.take(window);
            got.into_iter().map(|(sc, up, _)| (sc, up)).collect()
        }

        /// Switches the windows' thread (that thread only) to `layout`; the layout it had before.
        fn switch(&self, layout: HKL) -> HKL {
            SWITCHED.store(0, Ordering::SeqCst);
            LAYOUT.store(layout.0 as isize, Ordering::SeqCst);
            let start = Instant::now();
            loop {
                match SWITCHED.load(Ordering::SeqCst) {
                    0 if start.elapsed() < Duration::from_secs(2) => {
                        sleep(Duration::from_millis(5))
                    }
                    0 => panic!("the windows' thread didn't switch its layout"),
                    -1 => panic!("ActivateKeyboardLayout failed"),
                    old => return HKL(old as *mut _),
                }
            }
        }
    }

    impl Drop for Windows {
        fn drop(&mut self) {
            QUIT.store(true, Ordering::SeqCst);
            let _ = self.pump.take().map(thread::JoinHandle::join);
        }
    }

    /// What both tests start with: one at a time, never while the game runs (taking the foreground would pause it), two
    /// windows of their own with A in front, and the window that had the front, to put back at the end.
    fn start() -> Option<(MutexGuard<'static, ()>, HWND, Windows)> {
        let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let games = crate::game::processes().unwrap();
        if games
            .iter()
            .any(|(_, n)| n.eq_ignore_ascii_case("witcher3.exe"))
        {
            println!("skipped: witcher3.exe runs, and taking the foreground would pause it");
            return None;
        }
        // SAFETY: a plain query.
        let before = unsafe { GetForegroundWindow() };
        let w = Windows::open();
        if focus(w.a).is_err() {
            println!("skipped: this desktop gives a test window no foreground");
            return None;
        }
        Some((one, before, w))
    }

    fn give_back(before: HWND) {
        if !before.is_invalid() {
            let _ = focus(before);
        }
    }

    /// Sends `plan` to window A and returns what A got. This is the user's real desktop, where another window (the
    /// Claude app, a notification) can take the front mid-plan; `send` then stops, as it must, and this tries again,
    /// up to three times.
    fn deliver(w: &Windows, plan: &[Event]) -> Vec<(u16, bool, u16)> {
        let mut attempt = 1;
        loop {
            focus(w.a).unwrap();
            match send(w.a, plan, &|| false) {
                Ok(()) => return w.take(w.a),
                Err(s) if attempt < 3 && s.why.contains("lost the foreground") => {
                    println!("again: {}", s.why);
                    let _ = (w.take(w.a), w.take(w.b));
                    attempt += 1;
                }
                Err(s) => panic!("{s:?}"),
            }
        }
    }

    fn keys(plan: &[Event]) -> Vec<(u16, bool)> {
        plan.iter()
            .filter_map(|e| match e {
                Event::Key { sc, up, .. } => Some((*sc, *up)),
                Event::Sleep { .. } => None,
            })
            .collect()
    }

    /// Plans sent to two windows this test opens itself, which record the keys they get. It takes the foreground for
    /// a few seconds and puts back the window that had it, so it runs only when asked, never while the game runs:
    /// `cargo test -p quen -- --ignored --test-threads 1 input::`. Keys are plain letters, shift and arrows (no Alt,
    /// Win or F10, which would act on the desktop).
    #[test]
    #[ignore = "takes the foreground: run only when the PC is free"]
    fn sends_only_to_its_window_and_stops_when_it_loses_the_front() {
        let Some((_one, before, w)) = start() else {
            return;
        };
        let never = || false;
        let quick = |tokens: &[&str]| plan(tokens, 30, 30, 0x27).unwrap();

        // Every key arrives, by scancode, E0 keys marked; the console key is the one asked for.
        let all = quick(&["a", "lshift+b", "up:50", "wait:10", "console"]);
        let sc_up = |got: Vec<(u16, bool, u16)>| -> Vec<(u16, bool)> {
            got.into_iter().map(|(sc, up, _)| (sc, up)).collect()
        };
        assert_eq!(sc_up(deliver(&w, &all)), keys(&all));
        let (console, from) = console_key(w.a);
        println!("console key on this keyboard layout: 0x{console:X} ({from})");
        let line = console_line("q(W_1", console, &layout_scancode(w.a), true).unwrap();
        assert_eq!(sc_up(deliver(&w, &line)), keys(&line));

        // Not in front: refused, nothing sent.
        let refused = send(w.b, &quick(&["a"]), &never).unwrap_err();
        assert_eq!(refused.keys, 0, "{refused:?}");
        assert!(w.seen(w.b).is_empty() && w.seen(w.a).is_empty());

        // The front moves after `a`: `b` is never sent.
        TRIGGER.store(code(0x1E, true), Ordering::SeqCst);
        let lost = send(w.a, &plan(&["a", "b"], 30, 300, GRAVE).unwrap(), &never).unwrap_err();
        assert!(lost.why.contains("lost the foreground"), "{lost:?}");
        assert_eq!(lost.keys, 2);
        assert_eq!(w.seen(w.a), [(0x1E, false), (0x1E, true)]);
        assert!(w.seen(w.b).is_empty());

        // It moves while a chord is down: both keys come up again, last first (in the window now in front).
        focus(w.a).unwrap();
        TRIGGER.store(code(0x2E, false), Ordering::SeqCst);
        let chord = send(w.a, &quick(&["lshift+c:300"]), &never).unwrap_err();
        assert_eq!(chord.keys, 2, "{chord:?}");
        assert_eq!(w.seen(w.a), [(0x2A, false), (0x2E, false)]);
        assert_eq!(w.seen(w.b), [(0x2E, true), (0x2A, true)]);

        // A cancel ends a long wait at once, and nothing after it is sent.
        focus(w.a).unwrap();
        let start = Instant::now();
        let stop = || start.elapsed() > Duration::from_millis(200);
        let cancelled = send(w.a, &quick(&["a", "wait:5000", "b"]), &stop).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2), "{cancelled:?}");
        assert!(cancelled.why.contains("stopped"), "{cancelled:?}");
        assert_eq!(w.seen(w.a), [(0x1E, false), (0x1E, true)]);

        drop(w);
        give_back(before);
    }

    /// The layouts this test loaded (to unload again) and the one its windows' thread had: put back on drop, even
    /// after a failed assert, before the windows close.
    struct Layouts<'w> {
        windows: &'w Windows,
        original: Option<HKL>,
        loaded: Vec<HKL>,
    }

    impl Drop for Layouts<'_> {
        fn drop(&mut self) {
            if let Some(original) = self.original {
                self.windows.switch(original);
            }
            for &layout in &self.loaded {
                // SAFETY: a layout this test loaded, active nowhere now.
                let _ = unsafe { UnloadKeyboardLayout(layout) };
            }
        }
    }

    /// The same console lines under five keyboard layouts (synthetic: US, UK, German QWERTZ, French AZERTY, Swedish),
    /// each active on the receiving window's thread. The console reads virtual keys through a US table, so the
    /// window must get the same virtual keys under every layout, while the scancodes follow each layout where it moves
    /// a key (AZERTY letters, QWERTZ Y, the Nordic and German `-`). A line with a key the layout lacks is refused before
    /// anything is typed. It loads the layouts it lacks and unloads them again, and switches only its own windows'
    /// thread, so it runs only when asked, never while the game runs (see the test above).
    #[test]
    #[ignore = "takes the foreground: run only when the PC is free"]
    fn the_same_text_gives_the_same_virtual_keys_under_every_layout() {
        let Some((_one, before, w)) = start() else {
            return;
        };
        let lines = ["quit(Q,12)", "y_a-b.c"];
        // SAFETY: plain queries into a buffer of the size asked for.
        let had = unsafe {
            let mut list = vec![HKL::default(); GetKeyboardLayoutList(None) as usize];
            let n = GetKeyboardLayoutList(Some(&mut list));
            list.truncate(n.max(0) as usize);
            list
        };
        let mut layouts = Layouts {
            windows: &w,
            original: None,
            loaded: Vec::new(),
        };
        let ids = [
            ("US", w!("00000409")),
            ("UK", w!("00000809")),
            ("German", w!("00000407")),
            ("French", w!("0000040C")),
            ("Swedish", w!("0000041D")),
        ];
        // Each line's virtual keys (with up) under the first layout that types it.
        let mut want: [Option<Vec<(u16, bool)>>; 2] = [None, None];
        for (name, id) in ids {
            // SAFETY: one of Windows' own layout ids; KLF_NOTELLSHELL keeps the shell out of it.
            let layout = unsafe { LoadKeyboardLayoutW(id, KLF_NOTELLSHELL) }.unwrap();
            if !had.contains(&layout) && !layouts.loaded.contains(&layout) {
                layouts.loaded.push(layout);
            }
            let old = w.switch(layout);
            layouts.original.get_or_insert(old);
            // SAFETY: plain queries.
            let active = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(w.a, None)) };
            assert_eq!(active, layout, "{name} is active on the windows' thread");
            let (console, from) = console_key(w.a);
            let sc = layout_scancode(w.a);
            println!(
                "{name}: console key 0x{console:02X} ({from}); VK_OEM_MINUS at {:?}, VK_Q at {:?}, VK_Y at {:?}",
                sc(0xBD),
                sc(0x51),
                sc(0x59)
            );
            match name {
                "US" | "UK" => assert_eq!(sc(0xBD), Some(0x0C), "{name} -"),
                "German" | "Swedish" => assert_eq!(sc(0xBD), Some(0x35), "{name} -"),
                _ => {}
            }
            if name == "US" {
                assert_eq!(console, GRAVE);
            }
            if name == "French" {
                assert_eq!(sc(0x51), Some(0x1E), "AZERTY Q sits where US has A");
            }
            if name == "German" {
                assert_eq!(sc(0x59), Some(0x2C), "QWERTZ Y sits where US has Z");
            }
            for (line, want) in lines.iter().zip(&mut want) {
                let plan = match console_line(line, console, &sc, true) {
                    Ok(plan) => plan,
                    Err(why) => {
                        // Only a layout without VK_OEM_MINUS may refuse the second line, and it names the key.
                        assert!(name == "French" && sc(0xBD).is_none(), "{name}: {why}");
                        assert!(why.contains("'_'") || why.contains("'-'"), "{why}");
                        println!("{name}: {line:?} refused: {why}");
                        continue;
                    }
                };
                let got = deliver(&w, &plan);
                let scancodes: Vec<(u16, bool)> = got.iter().map(|&(sc, up, _)| (sc, up)).collect();
                assert_eq!(scancodes, keys(&plan), "{name}: {line:?}");
                let vks: Vec<(u16, bool)> = got.iter().map(|&(_, up, vk)| (vk, up)).collect();
                assert_eq!(want.get_or_insert(vks.clone()), &vks, "{name}: {line:?}");
            }
        }
        assert!(
            want.iter().all(Option::is_some),
            "a line typed under no layout"
        );
        drop(layouts);
        drop(w);
        give_back(before);
    }
}
