//! The e2e fixture `session.rs` and `concurrency.rs` share: a synthetic game, harness and data root in a temp
//! folder, and `quen mcp` proxies driven over stdio (see session.rs).
#![allow(dead_code)] // each test binary uses its own part

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::ptr::null;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::ledger::Ledger;
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

/// A synthetic dx12user.settings and general.ini, CRLF like the game's.
pub const SETTINGS: &str = "[Rendering]\r\nDLSSGMode=2\r\nVSync=true\r\nMotionBlur=true\r\n[Gameplay]\r\n\
                        CameraAutoRotX=true\r\nCameraAutoRotY=true\r\n[Version]\r\nConfigVersion=7\r\n";
pub const GENERAL: &str = "[General]\r\nGameLanguage=en\r\n";
/// When the synthetic compiled scripts and input.settings were written.
pub fn compiled_at() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// The game's reactions to quen's input, faked: the daemon runs this (`QUEN_TEST_INPUT`) with what it would send
/// instead of sending it, as the fake game takes no keys. Each call is appended to `calls.txt`. `keys e` (Continue at
/// the main menu) holds at `continue` (see `hold`), writes `continue_done` after it, then the load's pushes (0, then 1)
/// unless `no_continue` exists; `keys u:200` (photo mode) pushes flag 129 to quen's state log.
/// `console quenload(...)`: the load's pushes (0, then 1), unless `no_load` exists. A hold point writes `at_<point>`, then waits while `hold_<point>` exists and `go` doesn't.
/// Each push line starts with the system tick, as W3Quen.asi's do; a load's gameplay push is stamped 17 s after its
/// first, the fade of live boot 1. With the W3 Game State Bridge installed, each push also puts a contrary one (0:
/// loading) in the bridge's log, which quen must never read.
pub const FAKE_SCRIPT: &str = r#"
import ctypes, os, sys, time
here = os.path.dirname(os.path.abspath(__file__))
log = os.path.join(here, "..", "game", "bin", "x64_dx12", "W3QuenState.log")
what, args = sys.argv[1], sys.argv[2:]
open(os.path.join(here, "calls.txt"), "a").write(" ".join(sys.argv[1:]) + "\n")
def hold(point):
    open(os.path.join(here, "at_" + point), "w").close()
    while os.path.exists(os.path.join(here, "hold_" + point)) and not os.path.exists(os.path.join(here, "go")):
        time.sleep(0.05)
def push(*flags):
    tick = ctypes.windll.kernel32.GetTickCount64
    tick.restype = ctypes.c_uint64
    t = tick()
    open(log, "a").write("".join("%d push flags=%d\n" % (t + 17000 * i, f) for i, f in enumerate(flags)))
    if os.path.exists(os.path.join(os.path.dirname(log), "W3GameStateBridge.asi")):
        open(os.path.join(os.path.dirname(log), "W3GameStateBridge.log"), "a").write("%d push=1 flags=0\n" % t)
if what == "keys" and "e" in args:
    hold("continue")
    open(os.path.join(here, "continue_done"), "w").close()
    if not os.path.exists(os.path.join(here, "no_continue")):
        push(0, 1)
if what == "keys" and "u:200" in args:
    push(129)
if what == "console" and args[0].startswith("quenload(") and not os.path.exists(os.path.join(here, "no_load")):
    push(0, 1)
"#;
/// A synthetic input.settings; the fake game writes its lines in another order at every start, as the game does.
pub const INPUT: &str =
    "[Exploration]\r\nIK_W=(Action=GI_AxisLeftY)\r\nIK_S=(Action=GI_AxisLeftY)\r\n";

/// The in-game parts a launch checks for, relative to the game folder: the Native Bridge (its ASI and loader, and the
/// Mods folder its download installs, which quen doesn't need) and quen's package (the last two), which [`test_pins`]
/// makes the version quen expects. No W3 Game State Bridge: quen doesn't need it.
pub const PARTS: [&str; 5] = [
    r"bin\x64_dx12\WitcherNativeBridge.asi",
    r"bin\x64_dx12\dinput8.dll",
    r"Mods\modNativeBridge\content\scripts\imports.ws",
    r"bin\x64_dx12\W3Quen.asi",
    r"Mods\modQuen\content\scripts\local\quen.ws",
];

/// The W3 Game State Bridge's two parts, which RenoDX uses: installed only where a test says so.
pub const W3GSB: [&str; 2] = [
    r"bin\x64_dx12\W3GameStateBridge.asi",
    r"Mods\modW3GameStateBridge\content\scripts\local\w3gsb.ws",
];

/// `QUEN_TEST_PINS` for every quen these tests start: the synthetic package files are its one release.
pub fn test_pins() -> String {
    let sha = quen_core::journal::sha256_hex(b"synthetic");
    format!("{}={sha};{}={sha};{INTERPOSER}={sha}", PARTS[3], PARTS[4])
}

/// NVIDIA Streamline's interposer in the game folder: a renderdoc launch swaps it only when it is the pinned, signed
/// one. [`test_pins`] pins the bytes `synthetic` here, which carry no signature.
pub const INTERPOSER: &str = r"bin\x64_dx12\sl.interposer.dll";

/// A temp folder for one test: the fake scripts, the game folder (the fake game, a Mods folder, an ASI with its ini,
/// general.ini, steam_appid.txt), the game's documents folder (settings and a QuickSave), quen's data root, and the
/// daemons its configs started (it is removed once they have exited).
pub struct Fixture(pub PathBuf, pub Mutex<Vec<u32>>, pub String);

impl Fixture {
    pub fn new() -> Fixture {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("quen-session-test-{}-{n}", std::process::id()));
        // An image name of this fixture's own: tests run at once, and quen guards against any process of its name.
        let exe = format!("quen-test-game-{}-{n}.exe", std::process::id());
        let bin = dir.join("game").join("bin").join("x64_dx12");
        for folder in [
            dir.join("harness"),
            dir.join("game/Mods/modAlpha"),
            dir.join("game/bin/config/base"),
            dir.join("docs/gamesaves"),
            bin.clone(),
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(dir.join("harness").join("fake_input.py"), FAKE_SCRIPT).unwrap();
        fs::write(dir.join("docs/dx12user.settings"), SETTINGS).unwrap();
        let input = dir.join("docs/input.settings");
        fs::write(&input, INPUT).unwrap();
        let file = fs::File::options().write(true).open(&input).unwrap();
        file.set_modified(compiled_at()).unwrap();
        fs::write(bin.join("input_settings"), input.display().to_string()).unwrap();
        fs::write(dir.join("docs/gamesaves/QuickSave.sav"), "quick").unwrap();
        fs::write(dir.join("game/bin/config/base/general.ini"), GENERAL).unwrap();
        fs::write(dir.join("game/Mods/modAlpha/blob0.bundle"), "bundle").unwrap();
        fs::copy(fake_game(), bin.join(&exe)).unwrap();
        // The fake game is a build quen knows: a synthetic one of this fixture's config.
        let build = quen_core::journal::sha256_hex(&fs::read(bin.join(&exe)).unwrap());
        fs::write(bin.join("steam_appid.txt"), "292030").unwrap();
        // The compiled scripts, written long ago: the fake game rewrites them while a mod is parked.
        let compiled = dir.join(r"game\content\content0\x64.final.redscripts");
        fs::create_dir_all(compiled.parent().unwrap()).unwrap();
        fs::write(&compiled, "compiled with every mod").unwrap();
        let file = fs::File::options().write(true).open(&compiled).unwrap();
        file.set_modified(compiled_at()).unwrap();
        fs::write(bin.join("W3Fake.asi"), "asi").unwrap();
        fs::write(bin.join("W3Fake.ini"), "[General]\nMode=raise\n").unwrap();
        // What a launch and game_load need installed: the Native Bridge and quen's package, synthetic.
        for part in PARTS {
            let path = dir.join("game").join(part);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "synthetic").unwrap();
        }
        let config = format!(
            "harness = '{}'\ngame_dir = '{}'\ndocuments = '{}'\ndata_root = '{}'\ngame_exe = '{exe}'\n\
             known_builds = 'steam {build} synthetic (the fake game)'\n",
            dir.join("harness").join("ini_session.py").display(),
            dir.join("game").display(),
            dir.join("docs").display(),
            dir.join("data").display(),
        );
        fs::write(dir.join("config.toml"), config).unwrap();
        Fixture(dir, Mutex::default(), exe)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    pub fn settings(&self) -> String {
        self.read("docs/dx12user.settings")
    }

    pub fn config(&self, more: &str) {
        let mut config = self.read("config.toml");
        config.push_str(more);
        fs::write(self.path("config.toml"), config).unwrap();
    }

    /// Holds the fake scripts at `point` (see FAKE_SCRIPT) until `go`.
    pub fn hold(&self, point: &str) {
        fs::write(self.path(&format!("harness/hold_{point}")), "").unwrap();
    }

    /// Waits until the fake scripts reached `point`.
    pub fn wait_at(&self, point: &str) {
        let t = Instant::now();
        while !self.path(&format!("harness/at_{point}")).exists() {
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "boot never reached {point}"
            );
            sleep(Duration::from_millis(20));
        }
    }

    /// The fake game's pid, as it wrote it.
    pub fn game_pid(&self) -> u32 {
        self.read("game/bin/x64_dx12/pid").parse().unwrap()
    }

    /// What the fake scripts were called with, in order.
    pub fn calls(&self) -> Vec<String> {
        let calls = fs::read_to_string(self.path("harness/calls.txt")).unwrap_or_default();
        calls.lines().map(str::to_owned).collect()
    }

    /// Rows of a read-only query of quen's ledger.
    pub fn ledger(&self, sql: &str) -> Vec<Vec<Value>> {
        let ledger = Ledger::open_readonly(self.path("data/state/quen.db")).unwrap();
        ledger.query(sql, &[], Duration::from_secs(5)).unwrap().rows
    }

    /// The user's state is back, byte for byte: settings and general.ini restored, mods unparked, no parking folder,
    /// steam_appid.txt as it was, no journal.
    pub fn assert_restored(&self) {
        assert_eq!(self.settings(), SETTINGS);
        let compiled = self.path(r"game\content\content0\x64.final.redscripts");
        assert_eq!(
            fs::read_to_string(&compiled).unwrap(),
            "compiled with every mod"
        );
        let at = fs::metadata(&compiled).unwrap().modified().unwrap();
        assert_eq!(at, compiled_at(), "the compiled scripts' write time");
        assert_eq!(self.read("docs/input.settings"), INPUT);
        let at = fs::metadata(self.path("docs/input.settings"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(at, compiled_at(), "input.settings' write time");
        assert_eq!(self.read("game/bin/config/base/general.ini"), GENERAL);
        assert_eq!(
            self.read("game/Mods/modAlpha/blob0.bundle"),
            "bundle",
            "a mod stays parked"
        );
        assert!(
            !self.path("game/quen-parked").exists(),
            "the parking folder remains"
        );
        assert!(
            !self.path("data/state/journal.jsonl").exists(),
            "journal remains"
        );
    }

    /// Waits for the daemon to end the session on its own (no caller) and give everything back, then checks it. Returns
    /// the end's result, which the daemon writes to serve.log last, after the restore and the boot's ledger row: until
    /// it is there, the boot's outcome may still be NULL (CI, 2026-10-18).
    pub fn wait_restored(&self) -> Value {
        let t = Instant::now();
        loop {
            let log = fs::read_to_string(self.path("data/logs/serve.log")).unwrap_or_default();
            // Whole lines only: the daemon may be writing the last one.
            let whole = log.rsplit_once('\n').map_or("", |(lines, _)| lines);
            let end = whole
                .lines()
                .rfind(|l| l.contains(r#""tool":"session_end""#));
            let restored =
                self.settings() == SETTINGS && !self.path("data/state/journal.jsonl").exists();
            if let (Some(end), true) = (end, restored) {
                self.assert_restored();
                return serde_json::from_str(&end[end.find('{').unwrap()..]).unwrap();
            }
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "never restored and ended: {log}"
            );
            sleep(Duration::from_millis(100));
        }
    }

    /// A second config file with the same contents: its own daemon, over the same data root.
    pub fn second_config(&self) -> &str {
        fs::copy(self.path("config.toml"), self.path("config2.toml")).unwrap();
        "config2.toml"
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A daemon keeps its log open until it exits: no connection and no session for a few seconds.
        for &pid in self.1.lock().unwrap().iter() {
            dies_within(pid, Duration::from_secs(60));
        }
        let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
    }
}

/// The fake game cargo built beside quen (an example of this crate).
pub fn fake_game() -> PathBuf {
    example("fake_game")
}

/// The stand-in for Tracy's capture tool (`examples/fake_tracy_capture.rs`).
pub fn fake_tracy_capture() -> PathBuf {
    example("fake_tracy_capture")
}

/// The stand-in for REDkit's wcc_lite (`examples/fake_wcc_lite.rs`).
pub fn fake_wcc_lite() -> PathBuf {
    example("fake_wcc_lite")
}

fn example(name: &str) -> PathBuf {
    let quen = Path::new(env!("CARGO_BIN_EXE_quen"));
    let exe = quen
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("{name}.exe"));
    assert!(
        exe.exists(),
        "no {}: `cargo build -p quen --example {name}` (cargo test builds it)",
        exe.display()
    );
    exe
}

/// A running `quen mcp` proxy, its pipes, and the daemon serving it.
pub struct Quen {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: BufReader<ChildStdout>,
    pub next_id: u64,
    pub daemon: u32,
    /// The notifications read so far, in order.
    pub notes: Vec<Value>,
}

impl Quen {
    /// Starts a proxy for the fixture's config and initializes it; it starts the daemon if none runs for that config.
    pub fn start(fx: &Fixture) -> Quen {
        let mut quen = Quen::spawn(fx, "config.toml", 0);
        quen.init(fx);
        quen
    }

    /// Starts a proxy for `config` with these process creation flags, and sends it nothing yet: it finds or starts the
    /// daemon on its first line. cargo's own job (kill on close, no breakaway) holds the daemon too, so the proxy is
    /// told that's expected. What the daemon sends to the Recycle Bin goes into the fixture instead.
    pub fn spawn(fx: &Fixture, config: &str, flags: u32) -> Quen {
        Quen::spawn_with(fx, config, flags, &[])
    }

    pub fn spawn_with(fx: &Fixture, config: &str, flags: u32, env: &[(&str, &str)]) -> Quen {
        let mut child = Command::new(env!("CARGO_BIN_EXE_quen"))
            .arg("mcp")
            .env("QUEN_CONFIG", fx.path(config))
            .env("QUEN_TEST_IN_JOB", "1")
            .env("QUEN_TEST_PINS", test_pins())
            .env("QUEN_TEST_RECYCLE_TO", fx.path("recycled"))
            .env("QUEN_TEST_WCC_IMAGE", "quen-test-no-such-wcc.exe")
            // REDkit's licence answer, accepted: no test reads the user's.
            .env("QUEN_TEST_EULA", "1")
            .env("QUEN_TEST_INPUT", fx.path("harness/fake_input.py"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(flags)
            .spawn()
            .unwrap();
        Quen {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 0,
            daemon: 0,
            notes: Vec::new(),
        }
    }

    /// The MCP handshake as Claude Code, which waits for every call, then status for the daemon's pid.
    pub fn init(&mut self, fx: &Fixture) {
        self.init_as(fx, "claude-code");
    }

    /// The handshake as `client`: one other than Claude Code gets long calls as jobs once they outlive its budget.
    pub fn init_as(&mut self, fx: &Fixture, client: &str) {
        let init = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": client, "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "quen");
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        let status = self.tool("session_status", json!({}));
        // Finding 22: the running quen's version, to tell a tool list kept from an older one.
        assert_eq!(status["quen"], init["result"]["serverInfo"]["version"]);
        self.daemon = status["serve_pid"].as_u64().unwrap() as u32;
        assert_ne!(self.daemon, self.child.id(), "the proxy served itself");
        fx.1.lock().unwrap().push(self.daemon);
    }

    /// A daemon crash: it dies (and with it the session's job); its proxy follows.
    pub fn kill_daemon(&mut self) {
        let pid = self.daemon.to_string();
        let out = Command::new("taskkill")
            .args(["/F", "/PID", &pid])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        self.wait_exit();
    }

    pub fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request and returns its id without waiting.
    pub fn post(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Reads lines until the response to `id`, keeping notifications in `notes`.
    pub fn response(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "quen closed stdout"
            );
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == id {
                return msg;
            }
            assert!(
                msg.get("id").is_none(),
                "a response to another request: {msg}"
            );
            self.notes.push(msg);
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.post(method, params);
        self.response(id)
    }

    /// Calls a tool and returns its envelope, polling job_status if the call answered a job.
    pub fn tool(&mut self, name: &str, args: Value) -> Value {
        let answer = self.call(name, args);
        self.poll(&answer)
    }

    /// Calls a tool and returns what it answered: an envelope, or a job's `{job, state}`.
    pub fn call(&mut self, name: &str, args: Value) -> Value {
        let msg = self.request("tools/call", json!({"name": name, "arguments": args}));
        result(&msg)
    }

    /// `answer` if it is an envelope, else that job's envelope, from job_status once the call ended.
    pub fn poll(&mut self, answer: &Value) -> Value {
        let mut answer = answer.clone();
        while answer.get("ok").is_none() {
            let state = answer["state"].as_str().unwrap();
            assert!(["queued", "running"].contains(&state), "{answer}");
            answer = self.call("job_status", json!({"job": answer["job"]}));
        }
        answer
    }

    pub fn wait_exit(&mut self) {
        let t = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(t.elapsed() < Duration::from_secs(60), "quen did not exit");
            sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Quen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `tools/call` response's JSON: an envelope (isError unless ok), or a job's `{job, state}` (never an error).
pub fn result(msg: &Value) -> Value {
    let text = msg["result"]["content"][0]["text"].as_str().unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    let job = value.get("ok").is_none();
    assert_eq!(
        msg["result"]["isError"],
        !job && value["ok"] != true,
        "{msg}"
    );
    value
}

pub fn alive(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

/// Waits up to 10 s for `pid` to be gone.
pub fn dies(pid: u32) -> bool {
    dies_within(pid, Duration::from_secs(10))
}

pub fn dies_within(pid: u32, max: Duration) -> bool {
    let t = Instant::now();
    while alive(pid) {
        if t.elapsed() > max {
            return false;
        }
        sleep(Duration::from_millis(100));
    }
    true
}

/// A Job Object with `limits` holding `child`, like the one the MCP client starts its servers in. Closing the handle
/// closes the job.
pub fn job_holding(child: &Child, limits: JOB_OBJECT_LIMIT) -> HANDLE {
    // SAFETY: a fresh job; the limits struct outlives the call; the child's handle is valid while it is owned.
    unsafe {
        let job = CreateJobObjectW(null(), null());
        assert!(!job.is_null());
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = limits;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of_val(&info) as u32,
        );
        assert_ne!(set, 0);
        assert_ne!(AssignProcessToJobObject(job, child.as_raw_handle()), 0);
        job
    }
}

pub fn launch(quen: &mut Quen) -> Value {
    launch_with(quen, json!({}))
}

pub fn launch_with(quen: &mut Quen, mut args: Value) -> Value {
    args["user_ok"] = json!(true);
    let envelope = quen.tool("session_launch", args);
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope
}

/// A copy of the fake game under this fixture's image name, started outside quen's job: a game the user started.
pub fn outside_game(fx: &Fixture) -> Child {
    let dir = fx.path("outside");
    fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(&fx.2);
    fs::copy(fake_game(), &exe).unwrap();
    Command::new(exe).spawn().unwrap()
}

/// Game knowledge for the build `exe_sha256`, synthetic: an RE knowledge base in a git work tree
/// `<dir>/repo` (only HEAD and its branch's ref in `.git`), the KB at `repo/re` with a names.tsv, a hooks.tsv and a
/// note; and an REA evidence bundle `<dir>/rea.json` with a decompile of the KB's `Alpha_Draw` (0x140001000) and its
/// callers (`Beta_Setup`). Returns the KB root and the bundle.
pub fn knowledge(dir: &Path, exe_sha256: &str) -> (PathBuf, PathBuf) {
    let repo = dir.join("repo");
    for sub in [".git/refs/heads", "re/notes"] {
        fs::create_dir_all(repo.join(sub)).unwrap();
    }
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(repo.join(".git/refs/heads/main"), "c".repeat(40)).unwrap();
    let line = format!("# build: sha256={exe_sha256} stamp=0x0BADF00D\n");
    let kb = repo.join("re");
    fs::write(
        kb.join("names.tsv"),
        format!(
            "{line}rva\tname\tkind\tconfidence\tsource\tdate\n0x1000\tAlpha_Draw\tfunction\tO\ttest\t2026-10-18\n\
             0x2000\tBeta_Setup\tfunction\tO\ttest\t2026-10-18\n"
        ),
    )
    .unwrap();
    fs::write(
        kb.join("hooks.tsv"),
        format!("{line}site\tkind\tmod\tversion\tnote\n0x1000\thook\tModA\t1.0\tdraw\n"),
    )
    .unwrap();
    fs::write(
        kb.join("notes/alpha.md"),
        "# Alpha\n\n## Q1\nAlpha_Draw at 0x1000.\n",
    )
    .unwrap();
    let provider = json!({"id": "ghidra", "name": "Ghidra", "version": "12.1.4"});
    let record = |n: u32, operation: &str, raw: Value| {
        json!({
            "evidence_id": format!("ev_{n:064x}"),
            "subject": {"name": "game.exe", "digest": {"sha256": exe_sha256}, "format": "pe",
                        "architecture": "x86_64", "local_path": "%GAME_DIR%/game.exe"},
            "provider": provider,
            "analysis_profile": {"provider": provider, "parameters": {}, "digest": "7".repeat(64)},
            "predicate_type": "rea.static", "operation": operation,
            "parameters": {"procedure": "0x140001000"},
            "raw_result": raw, "normalized_result": {}, "confidence": "observed",
            "authority": "shipped-artifact", "environment": null, "limitations": [],
            "locations": [{"kind": "address", "address": "0x140001000"}], "evidence_links": [],
        })
    };
    let bundle = json!({"records": [
        record(1, "procedure_pseudo_code", json!("void synthetic(void) {}")),
        record(2, "procedure_callers", json!(["0x140002000"])),
    ]});
    let path = dir.join("rea.json");
    fs::write(&path, bundle.to_string()).unwrap();
    (kb, path)
}
