//! `focus`, the focus switch: during a live session the game is in front and the user keeps their hands off it, so a
//! question in the agent's chat goes unseen. The driving agent brings its client's window to the front to ask, then
//! the game back. Each connection's client is found once, at connect: the first process up the parent chain of its
//! `quen mcp` that shows a window, so any client with a window works (Claude Desktop, VS Code, a terminal). Only
//! windows move, and on the way back one escape closes the menu the game opened when it lost the front: no file
//! changes, nothing is journaled.

use serde_json::{Map, Value, json};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongW, IsIconic, SW_RESTORE, ShowWindow, WS_EX_TOOLWINDOW,
};

use crate::game::{self, Fields, Focus};
use crate::session::{Call, Done, Session};
use crate::{capture, doctor, input, win};

/// What `to: client` says of the game it left.
const PAUSED: &str = "Witcher 3 pauses into its menu while it isn't in front: focus to game before the next game call";

pub fn tool() -> Value {
    json!({
        "name": "focus",
        "description": "Move the foreground during a live session, to ask the user in the chat. to client: brings \
            this connection's client window to the front (found at connect: the first process up from its quen mcp \
            with a visible window that isn't a tool window), a minimized one restored; client names its image. \
            Witcher 3 pauses into its menu while it isn't in front. to game: brings the game back, waits up to 10 s \
            for gameplay or photo mode in quen's state, then closes that menu as game_keys focus does, even if the \
            game was in front already (menu: what it did). Only windows move, and that escape: no file, not \
            journaled. Refused, \
            nothing moved: no live session, a wrong token, another connection's session, to client when no window \
            was found for this client at connect or it shows none now, to game while quen's state shows neither \
            gameplay nor photo mode; with needs: to game without quen's in-game package. Failed: a window would not \
            come to the front, the game came but no gameplay or photo mode followed, or its menu stayed open.",
        "inputSchema": game::schema(json!({
            "to": {"type": "string", "enum": ["client", "game"], "description": "client: this connection's client window; game: the game's window"},
        }), &["to"]),
    })
}

pub(crate) fn call(session: &Session, call: &mut Call, args: &Value) -> Done {
    to(session, call, args).map_or_else(|done| done, Done::Ok)
}

fn to(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = game::ctx(session, args, &["to"])?;
    let fields = match args.get("to").and_then(Value::as_str) {
        Some("client") => {
            let (pid, name) = call.conn.client.clone()?;
            let already = to_client(pid, &name)?;
            Map::from_iter([
                ("client".into(), json!(name)),
                ("already_in_front".into(), json!(already)),
                ("game".into(), json!(PAUSED)),
            ])
        }
        Some("game") => {
            // Bringing it back waits for quen's state, which also shows the menu to close (feature tests 4: the user
            // may have clicked into the game already, and its pause menu stays open then too).
            doctor::installed(&[&doctor::PACKAGE], cx.config, "focus to game")?;
            let (_, window) = cx.window()?;
            let already = input::in_front(window);
            let menu = match already {
                false => cx.bring(call, window, Focus::BringToGame)?,
                true => Some(
                    cx.close_menu(call, game::READY_MAX)
                        .map_err(|why| Done::Failed(why, Map::new()))?,
                ),
            };
            Map::from_iter([
                ("already_in_front".into(), json!(already)),
                ("menu".into(), json!(menu)),
            ])
        }
        _ => return Err("to must be \"client\" or \"game\"".to_string().into()),
    };
    call.lap("focus");
    Ok(fields)
}

/// Brings process `pid`'s window ([`window_of`]) to the front, a minimized one restored first; whether it was there
/// already. Refused, nothing moved, when it shows none now.
fn to_client(pid: u32, name: &str) -> Result<bool, Done> {
    let window = window_of(pid)?.ok_or(format!(
        "{name}, this connection's client, shows no window now (closed, hidden, or on another virtual desktop): \
         nothing moved"
    ))?;
    // SAFETY: plain window calls; a window that closed meanwhile just fails them. Activating a minimized window would
    // leave it minimized.
    if unsafe { IsIconic(window) }.as_bool() {
        let _ = unsafe { ShowWindow(window, SW_RESTORE) };
    }
    input::focus(window).map_err(|_| {
        let fields = Map::from_iter([("focused".into(), json!(input::in_front(window)))]);
        Done::Failed(
            format!("{name}'s window would not come to the front"),
            fields,
        )
    })
}

/// Process `pid`'s first [`switchable`] top-level window in z-order: the one it had in front last.
fn window_of(pid: u32) -> Result<Option<HWND>, String> {
    let all = capture::top_windows()?;
    Ok(all.into_iter().find(|&w| switchable(w) == Some(pid)))
}

/// The process of window `hwnd` if the user can switch to it: shown ([`capture::shown`]) and not a tool window (a
/// palette, a tray helper, quen's own pin).
fn switchable(hwnd: HWND) -> Option<u32> {
    // SAFETY: a plain query; a window that closed meanwhile gives 0.
    let style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if style & WS_EX_TOOLWINDOW.0 != 0 {
        return None;
    }
    capture::shown(hwnd).map(|(pid, ..)| pid)
}

/// At connect (mcp.rs): the client of the connection whose `quen mcp` is `proxy` ([`walk`]), as (pid, image name), or
/// why there is none.
pub(crate) fn client(proxy: u32) -> Result<(u32, String), String> {
    let procs = game::tree().map_err(|e| format!("process list: {e}"))?;
    let shows: Vec<u32> = capture::top_windows()?
        .into_iter()
        .filter_map(switchable)
        .collect();
    let started = |pid| win::Child::open(pid).ok()?.started();
    walk(proxy, &procs, started, |pid| shows.contains(&pid))
}

/// The first process from `from` up the parent chain in `procs` (pid, parent, image name) that `shows` a window. The
/// chain ends at a parent that started after its child (it took a dead parent's pid) or whose start can't be read, and
/// at explorer.exe, the Windows shell, which is no client.
fn walk(
    from: u32,
    procs: &[(u32, u32, String)],
    started: impl Fn(u32) -> Option<u64>,
    shows: impl Fn(u32) -> bool,
) -> Result<(u32, String), String> {
    let mut walked = Vec::new();
    let mut pid = from;
    // At most once round a cycle of reused pids.
    for _ in 0..procs.len() {
        let Some((_, parent, name)) = procs.iter().find(|p| p.0 == pid) else {
            break;
        };
        if name.eq_ignore_ascii_case("explorer.exe") {
            break;
        }
        if shows(pid) {
            return Ok((pid, name.clone()));
        }
        walked.push(name.as_str());
        match (started(*parent), started(pid)) {
            (Some(up), Some(here)) if up <= here => pid = *parent,
            _ => break,
        }
    }
    Err(format!(
        "no window for this connection's client: no process from its quen mcp up ({}) showed one at connect",
        walked.join(" > ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::tests::{Fake, PARTS};
    use crate::mcp::Conn;
    use std::fs;
    use std::sync::Arc;

    fn table(rows: &[(u32, u32, &str)]) -> Vec<(u32, u32, String)> {
        rows.iter()
            .map(|&(p, up, n)| (p, up, n.to_string()))
            .collect()
    }

    /// The parent walk on synthetic process tables: from the proxy up to the first process with a window, never past a
    /// parent that started after its child, one whose start can't be read, or explorer.exe; round a cycle at most once.
    #[test]
    fn the_walk_finds_the_first_process_up_with_a_window() {
        let procs = table(&[
            (50, 40, "quen.exe"),
            (40, 30, "agent.exe"),
            (30, 20, "Shell.exe"),
            (20, 10, "Host.exe"),
            (10, 5, "explorer.exe"),
            (5, 0, "launcher.exe"),
        ]);
        // Started in pid order: every parent before its child.
        let in_order = |pid: u32| Some(u64::from(pid));
        let walk_to = |shows: &[u32]| walk(50, &procs, in_order, |pid| shows.contains(&pid));
        assert_eq!(walk_to(&[20, 30]), Ok((30, "Shell.exe".into())));
        assert_eq!(walk_to(&[20]), Ok((20, "Host.exe".into())));
        // The shell and what is above it are no client.
        let none = walk_to(&[10, 5]).unwrap_err();
        assert!(
            none.contains("(quen.exe > agent.exe > Shell.exe > Host.exe)"),
            "{none}"
        );
        // Shell.exe started after agent.exe: pid 30 is another process's now, and the walk stops at agent.exe.
        let reused = |pid: u32| Some(if pid == 30 { 99 } else { u64::from(pid) });
        let stopped = walk(50, &procs, reused, |pid| pid == 30).unwrap_err();
        assert!(stopped.contains("(quen.exe > agent.exe)"), "{stopped}");
        // A start that can't be read (another user's process) ends it too.
        let unread = |pid: u32| (pid != 20).then_some(u64::from(pid));
        assert!(walk(50, &procs, unread, |pid| pid == 20).is_err());
        // A cycle of reused pids with equal starts: once round, then no window.
        let cycle = table(&[(7, 8, "a.exe"), (8, 7, "b.exe")]);
        let round = walk(7, &cycle, |_| Some(0), |_| false).unwrap_err();
        assert!(round.contains("(a.exe > b.exe)"), "{round}");
        // A proxy already gone: nothing walked.
        assert!(walk(1, &procs, in_order, |_| true).is_err());
    }

    /// Refused, nothing moved: a bad argument, no live session, a wrong token, to client with no window found for the
    /// client or none now, to game without quen's package (needs) or the game's window.
    #[test]
    fn refuses_before_moving_anything() {
        let fx = Fake::new("focus");
        let headless = walk(
            7,
            &table(&[(7, 6, "quen.exe"), (6, 5, "agent.exe")]),
            |_| Some(0),
            |_| false,
        );
        let headless = Conn::new(Box::new(std::io::sink()), headless);
        // A client process with no window: the stand-in game.
        let gone = (fx.job.pids().unwrap()[0], "agent.exe".to_string());
        let gone = Conn::new(Box::new(std::io::sink()), Ok(gone));
        let run = |conn: &Arc<Conn>, args: Value, live: bool| {
            let mut session = fx.session();
            if !live {
                session.live = None;
            }
            let mut call = session.call(conn.clone(), Value::Null, None);
            let done = super::call(&session, &mut call, &args);
            session.envelope("focus", call, done)
        };
        for (conn, args, live, why) in [
            (
                &headless,
                json!({"token": "tok", "to": "client", "x": 1}),
                true,
                "unknown argument",
            ),
            (&headless, json!({"token": "tok"}), true, "to must be"),
            (
                &headless,
                json!({"token": "tok", "to": "chat"}),
                true,
                "to must be",
            ),
            (
                &gone,
                json!({"token": "tok", "to": "client"}),
                false,
                "no live session",
            ),
            (
                &gone,
                json!({"token": "nope", "to": "client"}),
                true,
                "wrong or missing token",
            ),
            (
                &headless,
                json!({"token": "tok", "to": "client"}),
                true,
                "no window for this connection's client: no process from its quen mcp up (quen.exe > agent.exe)",
            ),
            (
                &gone,
                json!({"token": "tok", "to": "client"}),
                true,
                "agent.exe, this connection's client, shows no window now",
            ),
            (
                &headless,
                json!({"token": "tok", "to": "game"}),
                true,
                "no visible window",
            ),
        ] {
            let out = run(conn, args.clone(), live);
            let reason = out["reason"].as_str().unwrap_or_default();
            assert!(
                out["refused"] == true && reason.contains(why),
                "{args}: {out}"
            );
        }
        let package = PARTS.map(|part| fx.dir.join("game").join(part));
        for part in &package {
            fs::rename(part, part.with_extension("off")).unwrap();
        }
        let out = run(&headless, json!({"token": "tok", "to": "game"}), true);
        assert_eq!(out["needs"][0]["dependency"], doctor::PACKAGE.name, "{out}");
    }

    /// The switch on this desktop: a window of this test stands in for the client (the walk from this process finds
    /// it), a tool window for the game (the walk passes tool windows by). It takes the foreground for a moment and
    /// gives it back, so it runs only when asked, never while the game runs:
    /// `cargo test -p quen -- --ignored --test-threads 1 focus::`.
    #[test]
    #[ignore = "takes the foreground: run only when the PC is free"]
    fn the_client_comes_to_the_front_and_the_game_back() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::thread::{self, sleep};
        use std::time::Duration;
        use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
            MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_MINIMIZE, WINDOW_EX_STYLE, WNDCLASSW,
            WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
        };
        use windows::core::w;

        unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            // SAFETY: the arguments Windows passed.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        let games = game::processes().unwrap();
        if games
            .iter()
            .any(|(_, n)| n.eq_ignore_ascii_case("witcher3.exe"))
        {
            return println!(
                "skipped: witcher3.exe runs, and taking the foreground would pause it"
            );
        }
        // SAFETY: a plain query.
        let before = unsafe { GetForegroundWindow() };
        let quit = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let pump = thread::spawn({
            let quit = quit.clone();
            // SAFETY: a class and windows of this test's own, destroyed on this thread.
            move || unsafe {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    lpszClassName: w!("quen-focus-test"),
                    ..Default::default()
                };
                RegisterClassW(&class);
                let make = |ex, style, x| {
                    let h = CreateWindowExW(
                        ex,
                        w!("quen-focus-test"),
                        w!("quen focus test"),
                        style | WS_VISIBLE,
                        x,
                        64,
                        240,
                        160,
                        None,
                        None,
                        None,
                        None,
                    );
                    h.unwrap().0 as isize
                };
                let client = make(WINDOW_EX_STYLE(0), WS_OVERLAPPEDWINDOW, 64);
                let game = make(WS_EX_TOOLWINDOW, WS_POPUP, 360);
                tx.send((client, game)).unwrap();
                while !quit.load(Ordering::SeqCst) {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    sleep(Duration::from_millis(2));
                }
                for h in [client, game] {
                    let _ = DestroyWindow(HWND(h as *mut _));
                }
            }
        });
        let (client, game) = rx.recv().unwrap();
        let (client, game) = (HWND(client as *mut _), HWND(game as *mut _));
        // The game in front, as during a session.
        input::focus(game).unwrap();
        let (pid, name) = super::client(std::process::id()).unwrap();
        assert_eq!(pid, std::process::id(), "{name}");
        assert!(matches!(to_client(pid, &name), Ok(false)));
        assert!(input::in_front(client));
        // to game: what Ctx::bring does once the state shows gameplay.
        input::focus(game).unwrap();
        assert!(input::in_front(game));
        // A minimized client comes back restored.
        // SAFETY: this test's own window.
        let _ = unsafe { ShowWindow(client, SW_MINIMIZE) };
        assert!(matches!(to_client(pid, &name), Ok(false)));
        // SAFETY: a plain query.
        assert!(input::in_front(client) && !unsafe { IsIconic(client) }.as_bool());
        quit.store(true, Ordering::SeqCst);
        pump.join().unwrap();
        if !before.is_invalid() {
            let _ = input::focus(before);
        }
    }
}
