//! `quen mcp`: the thin stdio proxy Claude Code starts. It finds this config's `quen serve`, starting it when
//! none listens, and relays bytes both ways. It holds no state, so it exits at once on stdin EOF: Claude Code kills a
//! stdio server's whole process tree 300 ms after closing its stdin. The daemon runs outside that tree and the
//! client's job, sees the connection close, and ends the session the normal way (saves quiet, kill, replay).

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::process::exit;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

use crate::config;
use crate::win::{self, NamedMutex, Pipe};

/// How long starting the daemon may take, and finding it listening after.
const START_MAX: Duration = Duration::from_secs(10);

pub fn run() -> ! {
    let mut stdin = io::stdin().lock();
    // The daemon is found or started on the first line, so a client that closes at once starts nothing.
    let mut first = Vec::new();
    if !matches!(stdin.read_until(b'\n', &mut first), Ok(n) if n > 0) {
        exit(0);
    }
    // A Windows PowerShell 5.1 / .NET Framework client writes a UTF-8 BOM first, and its initialize was lost to the parse
    // error (feature tests 1).
    if first.starts_with(b"\xEF\xBB\xBF") {
        first.drain(..3);
    }
    if let Some(secs) = env::var_os("QUEN_JOB_BUDGET_S") {
        first = with_budget(&first, &secs.to_string_lossy()).unwrap_or_else(|e| {
            eprintln!("quen mcp: QUEN_JOB_BUDGET_S: {e}");
            exit(2)
        });
    }
    let pipe = match connect() {
        Ok(pipe) => pipe,
        Err(e) => {
            eprintln!("quen mcp: {e}");
            exit(1)
        }
    };
    let reader = match pipe.try_clone() {
        Ok(reader) => reader,
        Err(e) => {
            eprintln!("quen mcp: {e}");
            exit(1)
        }
    };
    thread::spawn(move || {
        let mut out = io::stdout().lock();
        let mut buf = vec![0; 64 * 1024];
        loop {
            let n = (&reader).read(&mut buf).unwrap_or(0);
            if n == 0 {
                eprintln!("quen mcp: quen serve closed the connection");
                exit(1);
            }
            if out.write_all(&buf[..n]).and_then(|()| out.flush()).is_err() {
                exit(1);
            }
        }
    });
    let relayed = (&pipe)
        .write_all(&first)
        .and_then(|()| io::copy(&mut stdin, &mut &pipe));
    exit(i32::from(relayed.is_err()))
}

/// The client's `initialize` line with `quen_job_budget_s` added to its params: the daemon can't read this process's
/// environment, and the budget is per client (mcp.rs `JOB_BUDGET`). Any other first line stays as it is.
fn with_budget(line: &[u8], secs: &str) -> Result<Vec<u8>, String> {
    let secs: u64 = secs
        .trim()
        .parse()
        .map_err(|_| format!("{secs:?} is not a whole number of seconds"))?;
    let mut msg: serde_json::Value = match serde_json::from_slice(line) {
        Ok(msg) => msg,
        Err(_) => return Ok(line.to_vec()), // the daemon answers the parse error
    };
    if msg["method"] != "initialize" || !msg["params"].is_object() {
        return Ok(line.to_vec());
    }
    msg["params"]["quen_job_budget_s"] = secs.into();
    Ok(format!("{msg}\n").into_bytes())
}

/// This config's daemon pipe, starting `quen serve` when nothing listens on it, checked to be served by this user's
/// quen.
fn connect() -> Result<Pipe, String> {
    let name = config::daemon_name()?;
    let path = format!(r"\\.\pipe\{name}");
    // One proxy at a time finds or starts the daemon, and the daemon decides to exit under the same mutex.
    let _one = NamedMutex::acquire(&format!(r"Local\{name}"), Duration::from_secs(120))
        .map_err(|e| format!("the startup mutex: {e}"))?
        .ok_or("timed out waiting for the startup mutex")?;
    let pipe = match Pipe::open(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            start()?;
            let t = Instant::now();
            loop {
                match Pipe::open(&path) {
                    Err(e) if e.kind() == ErrorKind::NotFound && t.elapsed() < START_MAX => {
                        sleep(Duration::from_millis(20));
                    }
                    other => break other,
                }
            }
        }
        other => other,
    };
    let pipe = pipe.map_err(|e| {
        format!("{path}: {e} (quen serve didn't listen: its log is serve.log in the data root's logs folder)")
    })?;
    let server = pipe.peer_pid(false).and_then(win::check_peer);
    server.map_err(|e| format!("{path} is not served by this user's quen: {e}"))?;
    Ok(pipe)
}

/// Starts `quen serve --spawn` out of the client's job (CREATE_BREAKAWAY_FROM_JOB). That middle process starts the
/// daemon and exits at once, so the daemon has no living parent in the client's process tree, and `taskkill /T` on
/// this proxy doesn't reach it.
///
/// Breakaway leaves the jobs that allow it and stays in the first one that doesn't (under the Claude desktop app,
/// an outer job with no limits). When this proxy's own job refuses it, the start is retried without: a job with
/// SILENT_BREAKAWAY_OK (libuv's) lets the child out anyway, and the middle process refuses loudly if it is still in
/// a job that would kill it on close.
fn start() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("quen's own path: {e}"))?;
    let config = config::path()?;
    // Its errors go to this proxy's stderr, which the client logs.
    let out = io::stderr()
        .as_handle()
        .try_clone_to_owned()
        .map(File::from);
    let out = out.or_else(|_| nul()).map_err(|e| format!("NUL: {e}"))?;
    let argv = [exe.display().to_string(), "serve".into(), "--spawn".into()];
    let env = [("QUEN_CONFIG", &*config.to_string_lossy())];
    let spawn = |flags| win::spawn_detached(&argv, &env, home(&exe), &out, flags);
    let child = match spawn(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => spawn(CREATE_NO_WINDOW),
        started => started,
    };
    let child = child.map_err(|e| format!("starting quen serve --spawn: {e}"))?;
    match child.wait(START_MAX) {
        Ok(Some(0)) => Ok(()),
        Ok(Some(code)) => Err(format!("quen serve --spawn failed (exit {code})")),
        Ok(None) => Err("quen serve --spawn did not finish".into()),
        Err(e) => Err(format!("waiting for quen serve --spawn: {e}")),
    }
}

/// `quen serve --spawn`, the short-lived middle process: starts `quen serve` detached (no console, its own process
/// group, NUL in, its log out, no other handle) and exits.
pub fn spawn_serve() -> ! {
    match spawn_daemon() {
        Ok(()) => exit(0),
        Err(e) => {
            eprintln!("quen serve --spawn: {e}");
            exit(1)
        }
    }
}

fn spawn_daemon() -> io::Result<()> {
    // Closing the client's job must not reach the daemon, nor through it the game. QUEN_TEST_IN_JOB is for tests
    // only: cargo runs them in a job of its own that kills on close and refuses breakaway.
    if win::job_kills_on_close()? && env::var_os("QUEN_TEST_IN_JOB").is_none() {
        return Err(io::Error::other(
            "the client's job refused breakaway, and it kills its processes when it closes: quen serve won't run \
             inside it, where closing the client would kill the game mid-save",
        ));
    }
    let exe = env::current_exe()?;
    let log = match config::load() {
        Ok(config) => {
            let dir = config.data_root.join("logs");
            fs::create_dir_all(&dir)?;
            let path = dir.join("serve.log");
            OpenOptions::new().append(true).create(true).open(path)?
        }
        // The daemon reports a bad config in session_status.
        Err(_) => nul()?,
    };
    let argv = [exe.display().to_string(), "serve".into()];
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    win::spawn_detached(&argv, &[], home(&exe), &log, flags)?;
    Ok(())
}

/// The daemon's working folder: quen's own, so it never holds the client's project folder open.
fn home(exe: &Path) -> &Path {
    exe.parent().unwrap_or(Path::new("."))
}

fn nul() -> io::Result<File> {
    OpenOptions::new().write(true).open("NUL")
}
