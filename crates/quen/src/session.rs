//! The session tools, run one at a time on the session thread. `session_launch` is the native boot
//! (boot.rs): each file change journaled before it is made, the game started inside the session's Job Object.
//! `session_end` and `session_recover` replay the journal. One `quen serve` holds at most one session, owned by the connection that launched it, and holds the lock exactly while
//! it has a journal: from launch to end, and while it replays one a dead quen left. A quen that can't take the lock
//! is read-only and never replays.

use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use quen_core::build::Build;
use quen_core::journal::{Fs, Journal, replay, sha256_file, sha256_hex};
use quen_core::ledger::{BootEnd, BootStart, Ledger, Outcome, Save};
use quen_core::mods::HashCache;
use quen_core::saves::{self, Snapshot};
use serde_json::{Map, Value, json};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::boot::{self, Spec};
use crate::config::{self, Config};
use crate::hands::{self, Hands};
use crate::mcp::{self, Conn, Msg};
use crate::w3;
use crate::win::{self, Child, Job, try_lock};
use crate::{camera, data, disk, doctor, focus, game, measure, mem, notice, profile, save};

/// What an unfinished harness session leaves, relative to the harness folder (ini_session.py's own `recover` list).
const HARNESS_LEFTOVERS: &[&str] = &[
    r"cap\ini\orig\ini",
    r"cap\ini\orig\preset",
    r"cap\ini\orig\settings",
    r"cap\ini\orig\general",
    r"cap\ini\orig\rdx_mod",
    r"cap\ini\orig\signals.json",
    r"cap\ini\state.json",
    r"cap\ini\session.lock",
    r"cap\ini\orig404\dx12user.settings.pending",
];

/// What the connection threads, the session thread and the shutdown path share.
pub struct Shared {
    /// The config in use: the session thread swaps it when it takes up an edited file (`Session::reload`).
    config: RwLock<Arc<Result<Config, String>>>,
    /// Request ids the clients cancelled, with their connection: a flag the session thread checks between
    /// journal steps.
    cancelled: Mutex<Vec<(u64, Value)>>,
    /// Set on Ctrl+C or the daemon's exit: the call in progress stops at its next step and the session ends.
    stopping: AtomicBool,
    /// The connection whose launch is booting the game (0: none): what a shutdown, or that connection closing, kills
    /// at once.
    booting: AtomicU64,
    /// The live session's job, for status and for the shutdown kill.
    job: Mutex<Option<Arc<Job>>>,
    /// The processes a boot's kill on this thread took (a shutdown, or the launching connection closing): the end waits
    /// for them to leave the process list too, before its replay (feature tests 2b: the job was empty by then, and the
    /// still-listed game read as one outside quen's job).
    killed: Mutex<Vec<u32>>,
    /// The game in that job, held open once the launch found it, so the state can say when it exited.
    game: Mutex<Option<Child>>,
    /// The live session's boot row, token and owning connection: the envelope's state, the boot ledger_ingest
    /// attaches rows to for the token's holder, and who drives the session ([`Shared::refusal`]).
    boot: Mutex<Option<(i64, String, u64)>>,
    /// The lock file while this process holds it. Taking it and reading it for status both lock this mutex, so
    /// status never sees the file held while this quen is still taking it.
    lock: Mutex<Option<File>>,
    /// Who has the controls of the live session's game (hands.rs); None without one.
    hands: Mutex<Option<Hands>>,
    /// When the connection that launched the live session last called a tool, or a call of its ended: how long the
    /// agent has been waiting (notice.rs).
    last_call: Mutex<Option<Instant>>,
    /// What the replay at start did ("running" until it is done), if there was one: status reports it.
    startup: Mutex<Option<Value>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub fn new(config: Result<Config, String>) -> Shared {
        Shared {
            cancelled: Mutex::default(),
            stopping: AtomicBool::new(false),
            booting: AtomicU64::new(0),
            job: Mutex::default(),
            killed: Mutex::default(),
            game: Mutex::default(),
            boot: Mutex::default(),
            startup: Mutex::new(
                config
                    .as_ref()
                    .is_ok_and(|c| journal_path(c).exists())
                    .then(|| json!("running")),
            ),
            lock: Mutex::default(),
            hands: Mutex::default(),
            last_call: Mutex::default(),
            config: RwLock::new(Arc::new(config)),
        }
    }

    /// The config in use now.
    pub fn config(&self) -> Arc<Result<Config, String>> {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_config(&self, config: Result<Config, String>) {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(config);
    }

    /// The game lock while this quen holds it. Holding the guard keeps this quen from taking or releasing it.
    pub fn lock_guard(&self) -> std::sync::MutexGuard<'_, Option<File>> {
        locked(&self.lock)
    }

    /// The live session's boot row in the ledger, if a session is live.
    pub fn live_boot(&self) -> Option<i64> {
        locked(&self.boot).as_ref().map(|(boot, ..)| *boot)
    }

    /// The live session's boot row, if `token` is that session's.
    pub fn boot_for(&self, token: &str) -> Option<i64> {
        locked(&self.boot)
            .as_ref()
            .and_then(|(boot, t, _)| (t == token).then_some(*boot))
    }

    /// Why a session-thread call `tool` from connection `conn` is refused while a session is live, if it is: a launch or
    /// plan while any session is, anything else from a connection other than the one that launched it. The
    /// session thread asks before every call, and the connection threads before queueing one, so another agent's
    /// refusal never waits behind the call in progress.
    pub fn refusal(&self, conn: u64, tool: &str) -> Option<String> {
        let owner = locked(&self.boot).as_ref()?.2;
        if matches!(tool, "session_launch" | "session_plan") {
            return Some(
                "a session is live: session_end first (only the connection that launched it can); plan before a \
                 launch or after its end, while the game's files are the user's"
                    .into(),
            );
        }
        (owner != conn).then(|| {
            "the session belongs to another connection (another agent session): only that one drives and ends it"
                .into()
        })
    }

    pub fn cancel(&self, conn: u64, id: &Value) {
        locked(&self.cancelled).push((conn, id.clone()));
    }

    /// Shutdown: stops the call in progress at its next step, and kills a boot in flight (as soon as gamesaves is quiet).
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if self.booting.load(Ordering::SeqCst) != 0 {
            self.kill_boot();
        }
    }

    /// A connection closed: its call in progress stops at its next step, and a boot it launched is killed (once
    /// gamesaves is quiet). The
    /// session thread then ends a session it owns (`Msg::Dropped`).
    pub fn dropped(&self, conn: &Conn) {
        conn.closed.store(true, Ordering::SeqCst);
        if self.booting.load(Ordering::SeqCst) == conn.id {
            self.kill_boot();
        }
    }

    /// Kills the boot in flight, as soon as gamesaves is quiet.
    fn kill_boot(&self) {
        let job = locked(&self.job).clone();
        if let Some(job) = job {
            let config = self.config();
            let saves = config.as_ref().as_ref().ok().and_then(Config::saves_dir);
            saves_quiet(&job, saves.as_deref());
            locked(&self.killed).extend(job.pids().unwrap_or_default());
            let _ = job.kill();
        }
    }

    /// Whether a session is live (its job exists): the daemon doesn't exit while one is.
    pub fn live(&self) -> bool {
        locked(&self.job).is_some()
    }

    /// Who has the controls of the live session's game; None without one.
    pub(crate) fn hands(&self) -> Option<Hands> {
        *locked(&self.hands)
    }

    pub(crate) fn set_hands(&self, hands: Option<Hands>) {
        *locked(&self.hands) = hands;
    }

    /// Connection `conn` called a tool, or a call of its ended: noted when it launched the live session.
    pub fn touch(&self, conn: u64) {
        if locked(&self.boot).as_ref().is_some_and(|b| b.2 == conn) {
            *locked(&self.last_call) = Some(Instant::now());
        }
    }

    pub(crate) fn last_call(&self) -> Option<Instant> {
        *locked(&self.last_call)
    }
}

/// One tool call in progress: its timing, artifacts and progress reports.
pub(crate) struct Call<'a> {
    shared: &'a Shared,
    /// The connection that made it, where its progress and response go.
    pub(crate) conn: Arc<Conn>,
    id: Value,
    progress: Option<Value>,
    /// Its job, when a client asked for it: what job_status reports.
    job: Option<Arc<mcp::Job>>,
    started: Instant,
    /// The progress last sent, in seconds.
    sent: Cell<Option<u64>>,
    last: Instant,
    timing: Map<String, Value>,
    artifacts: Vec<Value>,
    /// The ledger rows it wrote, ids by table.
    ledger: Map<String, Value>,
}

impl Call<'_> {
    /// Names a ledger row it wrote in the envelope.
    pub(crate) fn row(&mut self, table: &str, id: i64) {
        if let Value::Array(ids) = self.ledger.entry(table).or_insert(json!([]))
            && !ids.contains(&json!(id))
        {
            ids.push(json!(id));
        }
    }

    /// Records the seconds since the previous lap as `step`, and reports it as progress.
    pub(crate) fn lap(&mut self, step: &str) {
        let secs = (self.last.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        self.timing.insert(step.into(), json!(secs));
        self.last = Instant::now();
        self.progress(&format!("{step} done"));
    }

    /// The client cancelled this call or closed its connection, or quen is shutting down.
    pub(crate) fn stopped(&self) -> bool {
        self.shared.stopping.load(Ordering::SeqCst)
            || self.conn.closed.load(Ordering::SeqCst)
            || locked(&self.shared.cancelled).contains(&(self.conn.id, self.id.clone()))
    }

    /// Why [`Call::stopped`] says so.
    pub(crate) fn stop_cause(&self) -> &'static str {
        if self.shared.stopping.load(Ordering::SeqCst) {
            "quen is shutting down"
        } else if self.conn.closed.load(Ordering::SeqCst) {
            "the connection that made the call closed"
        } else {
            "the call was cancelled"
        }
    }

    /// Lists `path` in the envelope with its sha256 (streamed: a capture may be gigabytes), which it returns.
    pub(crate) fn artifact(&mut self, path: &Path) -> Option<String> {
        let sha = sha256_file(path).ok();
        self.artifacts.push(json!({"path": path, "sha256": sha}));
        sha
    }

    /// Notes the call's progress for job_status, and sends a progress notification if the client asked for them, still
    /// waits for this call and didn't cancel it. Its progress is the call's whole seconds so far, sent only when it
    /// grew: MCP wants it to increase with every notification.
    pub(crate) fn progress(&self, message: &str) {
        if let Some(job) = &self.job {
            job.note(message);
        }
        let secs = self.started.elapsed().as_secs();
        let waits = !self.stopped() && !self.job.as_ref().is_some_and(|j| j.detached());
        if let Some(token) = self.progress.as_ref().filter(|_| waits)
            && self.sent.get().is_none_or(|last| secs > last)
        {
            self.sent.set(Some(secs));
            let params = json!({"progressToken": token, "progress": secs, "message": message});
            let note =
                json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params});
            self.conn.send(&note);
        }
    }
}

/// How a tool ended, before the envelope is filled in.
pub(crate) enum Done {
    Ok(Map<String, Value>),
    /// Nothing was changed.
    Refused(String),
    /// Nothing was changed: a dependency is missing. `needs` says which, for what, and where to get it.
    Needs(String, Vec<Value>),
    /// Nothing was changed; the fields say more: `game_text`, what the game wrote when it said no.
    RefusedWith(String, Map<String, Value>),
    /// Something was changed, then this went wrong; the fields say what is left.
    Failed(String, Map<String, Value>),
}

impl From<String> for Done {
    fn from(why: String) -> Done {
        Done::Refused(why)
    }
}

/// The session a quen process drives.
pub(crate) struct Live {
    /// The connection that launched it: only it drives and ends the session, and its closing ends it.
    pub(crate) owner: u64,
    pub(crate) token: String,
    /// The game's W3_QUEN_TOKEN: its command channel's pipe name and gate (channel.rs).
    pub(crate) channel: String,
    pub(crate) id: String,
    pub(crate) job: Arc<Job>,
    pub(crate) logs: PathBuf,
    /// Its ledger rows: the boot, and the game its saves belong to.
    pub(crate) boot: i64,
    pub(crate) game: i64,
    /// The user's saves, and how they were at launch.
    pub(crate) saves: PathBuf,
    pub(crate) saves_before: Snapshot,
    /// The files game_save accepted (save.rs), as it found them: the guard passes each while it is still so.
    pub(crate) quen_saves: RefCell<Snapshot>,
    /// Memory patches, cvar writes and teleports, each with its time: the game.* and measure.* tools push
    /// here as they journal them. The boot row records them at the end, and every save the game wrote after the first
    /// is tainted.
    pub(crate) patches: RefCell<Vec<(SystemTime, Value)>>,
    /// When the game was started: a state log older than this is a past launch's.
    pub(crate) launched: SystemTime,
    /// `renderdoc:NAME` / `nsys:NAME` / `ngfx:NAME` / `tracy:NAME`, when the game runs under a profiler.
    pub(crate) profiler: Option<String>,
    /// A tracy launch's capture tool connected (boot::play): the end lets the game go first, so the tool writes.
    pub(crate) tracy_connected: Cell<bool>,
    /// The game's build as the launch's gate read it: the KB's build line is checked against it.
    pub(crate) build: Build,
    /// A play session (session_launch mode play): the user has the controls from the launch, and plays on when the
    /// connection that launched closes.
    pub(crate) play: bool,
    /// The session's hand-overs of the controls, and the saves sorted at each (hands.rs).
    pub(crate) handovers: hands::Handovers,
    /// quen's pause, as the game's last pause reply said it (game_pause, sweep_play, hands_give), and whether a
    /// sweep's camera path may still be loaded (its stop failed): what hands_give undoes.
    pub(crate) paused: Cell<bool>,
    pub(crate) path: Cell<bool>,
    /// The launching connection's client with a window (`Conn::client`), which quen brings forward when the agent
    /// waits; when quen last showed a notice in the game; whether it brought the client forward itself, so the next
    /// game call brings the game back; the stretch without calls it last acted on (notice.rs).
    pub(crate) client: Result<(u32, String), String>,
    pub(crate) notice_at: Cell<Option<Instant>>,
    pub(crate) away: Cell<bool>,
    pub(crate) idled: Cell<Option<Instant>>,
    /// Keeps the PC and its display awake for exactly as long as the session lives.
    pub(crate) _awake: crate::win::Awake,
}

impl Live {
    /// What changed in gamesaves since the launch, and the guard's part of it: beyond the game's own AutoSave /
    /// CheckPoint saves and the files game_save accepted, each while it is as game_save found it.
    fn unexpected(&self) -> (Vec<String>, Vec<String>) {
        self.sorted(&saves::snapshot(&self.saves))
    }

    /// [`Live::unexpected`] for gamesaves as `now` holds it. Gamesaves is sorted at every hand-over, so the guard's part
    /// is only what changed since the last sort (hands.rs).
    pub(crate) fn sorted(&self, now: &Snapshot) -> (Vec<String>, Vec<String>) {
        let changed = saves::changed(&self.saves_before, now);
        let since = saves::changed(&self.handovers.baseline.borrow(), now);
        let ours = self.quen_saves.borrow();
        let bad = saves::unexpected(&since, saves::W3_AUTO_SAVES)
            .into_iter()
            .filter(|name| {
                ours.get(*name)
                    .is_none_or(|was| now.get(*name) != Some(was))
            })
            .map(String::from)
            .collect();
        (changed, bad)
    }
}

/// The session thread's own state.
pub(crate) struct Session<'a> {
    pub(crate) shared: &'a Shared,
    pub(crate) live: Option<Live>,
    /// File hashes for the mod sets, kept while quen runs.
    pub(crate) cache: HashCache,
    /// The config in use, as the session thread's calls see it: changed only by [`Session::reload`].
    config: Arc<Result<Config, String>>,
    /// The config file's text the config in use came from; `None` outside `quen serve`, which never reloads.
    text: Option<String>,
}

/// Why an edited config isn't taken up while a session is live, as quen_setup and the reload say it.
pub(crate) const RELOAD_AFTER_SESSION: &str = "a session is live, so this quen keeps the config it has until the \
    session ends; then every connection to it takes up the new one, with no reconnect";

/// How often the session thread looks whether a play session's game has exited, between messages.
const EXIT_POLL: Duration = Duration::from_secs(1);

/// The session thread: replays a journal a dead quen left (if it can take the lock), then runs calls in order until
/// shutdown, which ends a live session. A test session also ends when the connection that launched it closes; a play
/// session when its game exits. Before every message, and after a call or an end, it takes up an edited config
/// when it can (`text`: the file's text the config came from).
pub fn work(shared: &Shared, rx: Receiver<Msg>, text: String) {
    let mut session = Session::new(shared, Some(text));
    session.start();
    loop {
        let msg = rx.recv_timeout(EXIT_POLL);
        session.end_if_gone();
        notice::idle(&session);
        let msg = match msg {
            Ok(msg) => msg,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let synced = session.reload();
        match msg {
            Msg::Call {
                job,
                args,
                progress,
            } => {
                let (conn, id, name) = (job.conn.clone(), job.request.clone(), job.tool.clone());
                job.start();
                let mut call = session.call(conn.clone(), id.clone(), progress);
                call.job = Some(job.clone());
                // Only the connection that launched the session drives and ends it, and only while the user lets it.
                let refused = shared.refusal(conn.id, &name).or_else(|| {
                    let user = shared.hands() == Some(Hands::User);
                    (user && !hands::user_may(&name, &args)).then(|| hands::user_has(&name))
                });
                let done = match refused {
                    Some(why) => Done::Refused(why),
                    None => match notice::back(&session, &call, &name, &args) {
                        Ok(back) => {
                            let done = session.run(&mut call, &name, &args);
                            notice::with_back(done, back)
                        }
                        Err(done) => done,
                    },
                };
                let cancelled = call.stopped();
                let envelope = session.envelope(&name, call, done);
                shared.touch(conn.id);
                locked(&shared.cancelled).retain(|c| *c != (conn.id, id.clone()));
                // An end or a replay may have unblocked it: taken up before the answer, so the caller's next call,
                // on any connection, runs on it.
                let _ = session.reload();
                // What a cancelled call started was already undone.
                job.finish(envelope, cancelled);
            }
            Msg::Dropped(conn) => match &session.live {
                // The agent's writes of the hand-over undone, the user plays on; the session ends when the game exits.
                Some(live) if live.owner == conn && live.play => {
                    let undo = hands::dropped(&session);
                    eprintln!(
                        "quen: the connection that launched the play session closed: the user has the controls, \
                         and the session ends when the game exits. The hand-over's writes: {undo}"
                    );
                }
                Some(live) if live.owner == conn => {
                    session.end_unasked("the connection that launched it closed");
                    let _ = session.reload();
                }
                _ => {}
            },
            Msg::Reload(reply) => {
                if let Some(reply) = reply {
                    let _ = reply.send(synced);
                }
            }
            Msg::Shutdown(why) => {
                session.end_unasked(why);
                session.retry_left_journal();
                return;
            }
        }
    }
}

/// `quen recover`: `session_recover` from a terminal, for a journal a crash, a power loss or a logoff left
/// before any client starts quen again. Refused while another quen holds the game lock. Prints the envelope; exit code
/// 0 when it is ok.
pub fn cli_recover() -> i32 {
    let shared = Shared::new(crate::config::load());
    let mut session = Session::new(&shared, None);
    let mut call = session.call(Conn::nobody(), Value::Null, None);
    let done = session.recover(&mut call, true);
    let out = session.envelope("session_recover", call, done);
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    i32::from(out["ok"] != true)
}

/// `session_status`, on the reader thread: read-only, so it never waits for a launch in flight.
pub fn status(shared: &Shared) -> Value {
    let mut fields = Map::new();
    if let Ok(config) = &*shared.config() {
        fields.insert("harness_leftovers".into(), json!(harness_leftovers(config)));
        let parked = config.game_dir.as_deref().map(w3::parked);
        fields.insert("parked".into(), json!(parked.unwrap_or_default()));
        // state.game is this quen's job only: a copy of the game started elsewhere shows here (feature tests 1).
        let job = locked(&shared.job).clone();
        let elsewhere = outside(&config.game_exe, job.as_deref());
        let elsewhere = elsewhere.map_or_else(|why| json!(why), |pids| json!(pids));
        fields.insert("game_outside_job".into(), elsewhere);
        fields.insert("files".into(), files(config));
        let started = locked(&shared.game).as_ref().and_then(Child::started_at);
        fields.insert("game_state".into(), w3::state(config, started));
        fields.insert("data_root".into(), json!(config.data_root));
    }
    if let Some(startup) = locked(&shared.startup).as_ref() {
        fields.insert("startup_replay".into(), startup.clone());
    }
    fields.insert("serve_pid".into(), json!(std::process::id()));
    // Finding 22: a client keeps the tool list it got when it connected, maybe from an older quen.
    fields.insert("quen".into(), json!(env!("CARGO_PKG_VERSION")));
    // Finding 38: a client kept an older quen's schemas after a reconnect; tools/list's result, fingerprinted.
    let tools = crate::mcp::tools();
    let list = serde_json::to_string(&json!({"tools": tools})).unwrap_or_default();
    let count = tools.as_array().map_or(0, Vec::len);
    let fingerprint = json!({"count": count, "sha256": sha256_hex(list.as_bytes())});
    fields.insert("tools".into(), fingerprint);
    envelope(
        shared,
        "session_status",
        Map::new(),
        Vec::new(),
        Map::new(),
        Done::Ok(fields),
    )
}

/// The result envelope: ok, refused + reason, state, timing_s, artifacts, ledger (row ids by table), then the
/// tool's own fields.
pub(crate) fn envelope(
    shared: &Shared,
    tool: &str,
    timing: Map<String, Value>,
    artifacts: Vec<Value>,
    ledger: Map<String, Value>,
    done: Done,
) -> Value {
    let (ok, refused, reason, mut fields) = match done {
        Done::Ok(fields) => (true, false, None, fields),
        Done::Refused(why) => (false, true, Some(why), Map::new()),
        Done::RefusedWith(why, fields) => (false, true, Some(why), fields),
        Done::Needs(why, needs) => (
            false,
            true,
            Some(why),
            Map::from_iter([("needs".into(), json!(needs))]),
        ),
        Done::Failed(why, fields) => (false, false, Some(why), fields),
    };
    let mut state = state(shared);
    // A tool's own state (the mem tools' world) goes into the envelope's.
    if let (Some(Value::Object(own)), Some(state)) = (fields.remove("state"), state.as_object_mut())
    {
        state.extend(own);
    }
    let mut out = json!({
        "ok": ok,
        "tool": tool,
        "refused": refused,
        "reason": reason,
        "state": state,
        "timing_s": timing,
        "artifacts": artifacts,
        "ledger": ledger,
    });
    out.as_object_mut().expect("an object").extend(fields);
    out
}

/// The `state` of every envelope: lock, game (the processes in this quen's job) and journal.
fn state(shared: &Shared) -> Value {
    let config = shared.config();
    let config = match &*config {
        Ok(config) => config,
        Err(why) => return json!({"config": why}),
    };
    let path = config.state_dir().join("game.lock");
    let held = locked(&shared.lock);
    let lock = if held.is_some() {
        json!("held here")
    } else {
        match fs::read_to_string(&path) {
            Ok(last) => {
                json!({"free": true, "last_holder": serde_json::from_str::<Value>(&last).ok()})
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => json!({"free": true}),
            Err(_) => json!("held by another quen (read-only here)"),
        }
    };
    drop(held);
    let pids = locked(&shared.job)
        .as_ref()
        .and_then(|job| job.pids().ok())
        .unwrap_or_default();
    let mut game = json!({"running": !pids.is_empty(), "pids": pids});
    // The job's game exited by itself (feature tests 2: ngfx closes it after its trace): when.
    if let Some(t) = locked(&shared.game).as_ref().and_then(Child::exited) {
        game["exited"] = json!(utc(t));
    }
    let journal = journal_path(config);
    let mut state = json!({
        "lock": lock,
        "game": game,
        "boot": shared.live_boot(),
        "journal": journal.exists().then_some(journal),
    });
    if let Some(hands) = shared.hands() {
        state["hands"] = json!(hands.name());
    }
    state
}

pub(crate) fn journal_path(config: &Config) -> PathBuf {
    config.state_dir().join("journal.jsonl")
}

/// Whether another process holds the game lock: its holder opens it exclusively, so even a read fails (`try_lock`).
pub(crate) fn locked_elsewhere(config: &Config) -> bool {
    let read = fs::read(config.state_dir().join("game.lock"));
    matches!(read, Err(e) if e.kind() != io::ErrorKind::NotFound)
}

/// What an unfinished Python session left: the harness's backups, markers, state and lock, and its parking folder.
/// Only with a harness in the config (this machine, while it exists): elsewhere there is none to look for.
fn harness_leftovers(config: &Config) -> Vec<PathBuf> {
    let Some(harness) = config.harness.as_deref().and_then(Path::parent) else {
        return Vec::new();
    };
    let in_harness = HARNESS_LEFTOVERS.iter().map(|rel| harness.join(rel));
    let parked = config
        .game_dir
        .iter()
        .map(|dir| dir.join(w3::HARNESS_PARKED));
    in_harness.chain(parked).filter(|p| p.exists()).collect()
}

/// session_status: each file the live journal backed up, beside its backup, and whether the live file is still the
/// original (while a session runs it isn't: the boot patched it).
fn files(config: &Config) -> Value {
    let journal = fs::read(journal_path(config)).unwrap_or_default();
    let backups = journal
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<quen_core::journal::Record>(line).ok())
        .filter_map(|record| match record {
            quen_core::journal::Record::Step {
                action: quen_core::journal::Action::Backup { src, backup, sha },
                ..
            } => Some((src, backup, sha)),
            _ => None,
        });
    let rows: Vec<Value> = backups
        .map(|(src, backup, sha)| {
            let same = fs::read(&src).is_ok_and(|b| sha256_hex(&b) == sha);
            json!({"file": src, "backup": backup, "same": same})
        })
        .collect();
    json!(rows)
}

/// The interlock: no launch or recovery while a Python session's leftovers exist: quen's journal never accounts
/// for them.
fn interlock(config: &Config) -> Result<(), String> {
    let left = harness_leftovers(config);
    if left.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = left.iter().map(|p| p.display().to_string()).collect();
    Err(format!(
        "a Python harness session was not ended: it left {}. Run `python ini_session.py recover` in the harness \
         folder first (it ends that session and restores its files)",
        list.join(", ")
    ))
}

/// Seconds since 1970: the lock's start time and the log folder names.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `t` in UTC as the ledger writes times, `2026-10-18T10:47:27Z`.
pub(crate) fn utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, s) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days, for days since 1970 (none before it here).
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, min) = (s / 3600, s / 60 % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{:02}Z", s % 60)
}

/// [`utc`] with milliseconds, `2026-10-19T00:34:01.493Z`: when a timing window began or ended.
pub(crate) fn utc_ms(t: SystemTime) -> String {
    let ms = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_millis());
    let s = utc(t);
    format!("{}.{ms:03}Z", &s[..s.len() - 1])
}

/// The last non-empty line of a harness log.
pub(crate) fn last_line(log: &Path) -> Option<String> {
    let text = fs::read_to_string(log).ok()?;
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::to_owned)
}

impl<'a> Session<'a> {
    pub(crate) fn new(shared: &'a Shared, text: Option<String>) -> Session<'a> {
        Session {
            shared,
            live: None,
            cache: HashCache::default(),
            config: shared.config(),
            text,
        }
    }

    /// What a fresh quen serve does first, at its start and after it took up an edited config: replays a journal a
    /// dead quen left in the data root (if it can take the lock), then migrates the ledger there.
    fn start(&mut self) {
        if locked(&self.shared.startup).is_some() {
            let mut call = self.call(Conn::nobody(), Value::Null, None);
            let done = self.recover(&mut call, false);
            let envelope = self.envelope("startup replay", call, done);
            eprintln!("quen: startup replay: {}", crate::mcp::for_user(&envelope));
            *locked(&self.shared.startup) = Some(envelope);
        }
        self.migrate_ledger();
    }

    /// Takes up the config file once its text differs from the one in use (quen_setup, `quen setup` or an edit by
    /// hand wrote it), as a fresh quen serve on it would: the startup replay and the ledger migration then run on the
    /// new data root. Only between calls, with no session live and no journal left in the data root, so a session
    /// never sees two configs; until then every message tries again. A file that doesn't load is taken up too, as a
    /// fresh quen serve would take it: the session tools then refuse with its error. A file that is gone isn't: its
    /// defaults would name another data root. Ok: the config in use is the file's.
    fn reload(&mut self) -> Result<(), String> {
        let Some(text) = &self.text else {
            return Ok(());
        };
        let path = config::path()?;
        let now = config::read(&path)?;
        if now == *text {
            return Ok(());
        }
        if !path.is_file() {
            return Err(format!(
                "the config file {} is gone, so this quen keeps the config it has",
                path.display()
            ));
        }
        if self.live.is_some() {
            return Err(RELOAD_AFTER_SESSION.into());
        }
        if let Ok(config) = self.config()
            && journal_path(config).exists()
        {
            return Err(format!(
                "{} holds a session's journal, so this quen keeps the config it has until session_recover has \
                 replayed it; then every connection to it takes up the new one, with no reconnect",
                journal_path(config).display()
            ));
        }
        let config = config::loaded(&now, &path);
        match &config {
            Ok(_) => eprintln!("quen: took up the edited config {}", path.display()),
            Err(e) => eprintln!("quen: took up the edited config, which doesn't load: {e}"),
        }
        self.text = Some(now);
        *locked(&self.shared.startup) = config
            .as_ref()
            .is_ok_and(|c| journal_path(c).exists())
            .then(|| json!("running"));
        self.shared.set_config(config);
        self.config = self.shared.config();
        self.cache = HashCache::default();
        self.start();
        Ok(())
    }

    pub(crate) fn call(&self, conn: Arc<Conn>, id: Value, progress: Option<Value>) -> Call<'a> {
        Call {
            shared: self.shared,
            conn,
            id,
            progress,
            job: None,
            started: Instant::now(),
            sent: Cell::new(None),
            last: Instant::now(),
            timing: Map::new(),
            artifacts: Vec::new(),
            ledger: Map::new(),
        }
    }

    pub(crate) fn envelope(&self, tool: &str, call: Call, done: Done) -> Value {
        envelope(
            self.shared,
            tool,
            call.timing,
            call.artifacts,
            call.ledger,
            done,
        )
    }

    /// Runs session-thread tool `name`, once it isn't refused.
    fn run(&mut self, call: &mut Call, name: &str, args: &Value) -> Done {
        match name {
            "session_launch" => self.launch(call, args, false),
            "session_plan" => self.launch(call, args, true),
            "session_end" => self.end(call, args),
            "capture_clean" => disk::clean(self, call, args),
            "focus" => focus::call(self, call, args),
            "hands_take" => hands::take(self, call, args),
            "hands_give" => hands::give(self, call, args),
            n @ ("world_get" | "world_actors") => game::call(self, call, n, args),
            n @ ("game_keys" | "game_console" | "game_load") => {
                let done = game::call(self, call, n, args);
                self.guard_after(call, done)
            }
            "game_save" => {
                let done = save::call(self, call, args);
                let done = done.map_or_else(|done| done, Done::Ok);
                self.guard_after(call, done)
            }
            n if n.starts_with("game_") => game::call(self, call, n, args),
            n if n.starts_with("sweep_") => camera::call(self, call, n, args),
            n if n.starts_with("measure_") => measure::call(self, call, n, args),
            n if n.starts_with("mem_") => mem::call(self, call, n, args),
            // quen_setup comes here only with licence (mcp.rs): it runs wcc_lite as script_compile does.
            n @ ("data_read" | "data_scan" | "script_compile" | "quen_setup") => {
                data::call(self, call, n, args)
            }
            _ => self.recover(call, true),
        }
    }

    /// A play session ends by itself once its game has exited (the user quit it, or it crashed), its files given
    /// back at once.
    fn end_if_gone(&mut self) {
        let Some(live) = self.live.as_ref().filter(|l| l.play) else {
            return;
        };
        let exited = locked(&self.shared.game).as_ref().and_then(Child::exited);
        if exited.is_some() || live.job.pids().is_ok_and(|pids| pids.is_empty()) {
            self.end_unasked("the game exited");
            let _ = self.reload();
        }
    }

    /// Ends the live session, if any, with no caller to answer (its connection closed, or quen is stopping): the
    /// result goes to the daemon's log.
    fn end_unasked(&mut self, why: &str) {
        if self.live.is_some() {
            let mut call = self.call(Conn::nobody(), Value::Null, None);
            let done = self.finish(&mut call, why, false);
            let envelope = self.envelope("session_end", call, done);
            eprintln!("quen: {why}: {}", crate::mcp::for_user(&envelope));
        }
    }

    /// Before the daemon exits: a journal an end left (its replay refused or left a step) is replayed again, once
    /// the game's exe runs nowhere outside quen's job, waited for at most `RETRY_MAX`, so the user's files don't stay
    /// patched until the next quen (live boot 1: the daemon exited 5 s after such an end, mods still parked). If that
    /// replay fails too, the journal stays for the next quen's startup replay. serve.log says what happened.
    fn retry_left_journal(&mut self) {
        let Ok(config) = self.config().cloned() else {
            return;
        };
        if self.live.is_some() || !journal_path(&config).exists() {
            return;
        }
        // Another quen's live session (another config over this data root) holds the lock: its journal isn't ours.
        if let Err(why) = self.take_lock(&config, "retry") {
            eprintln!("quen: before exit, a journal is left, not replayed: {why}");
            return;
        }
        self.release_lock();
        eprintln!(
            "quen: before exit, replaying the journal an end left once {} runs nowhere outside quen's job (at most {} s)",
            config.game_exe,
            RETRY_MAX.as_secs()
        );
        let t = Instant::now();
        while t.elapsed() < RETRY_MAX
            && outside(&config.game_exe, None).is_ok_and(|pids| !pids.is_empty())
        {
            sleep(Duration::from_millis(250));
        }
        let mut call = self.call(Conn::nobody(), Value::Null, None);
        let done = self.recover(&mut call, false);
        eprintln!(
            "quen: before exit: {}",
            self.envelope("session_recover", call, done)
        );
    }

    /// At start: migrates a ledger an older quen left, with its backup (`Ledger::open`), so ledger_query sees this
    /// quen's tables and views before the first write (feature tests 2: schema 4 until the first launch). Only under
    /// the lock, which only the ledger's writers hold: with a session live under another quen, it migrates at the next
    /// write, as before. serve.log says what happened.
    fn migrate_ledger(&mut self) {
        let Ok(config) = self.config().cloned() else {
            return;
        };
        if !config.state_dir().join("quen.db").exists() {
            return;
        }
        if let Err(why) = self.take_lock(&config, "migrate") {
            eprintln!("quen: the ledger migrates at the next write, not at start: {why}");
            return;
        }
        if let Err(why) = open_ledger(&config) {
            eprintln!(
                "quen: at start, the ledger didn't migrate ({why}): it migrates at the next write"
            );
        }
        self.release_lock();
    }

    pub(crate) fn config(&self) -> Result<&Config, String> {
        self.config
            .as_ref()
            .as_ref()
            .map_err(|e| format!("quen's config: {e}"))
    }

    /// Takes the lock for `session`, or says who has it.
    pub(crate) fn take_lock(&mut self, config: &Config, session: &str) -> Result<(), String> {
        let dir = config.state_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let holder = json!({"pid": std::process::id(), "session": session, "t_start": now()});
        let path = dir.join("game.lock");
        let mut held = locked(&self.shared.lock);
        match try_lock(&path, &holder) {
            Ok(Some(file)) => {
                *held = Some(file);
                Ok(())
            }
            Ok(None) => Err("another quen holds the game lock: this one is read-only".into()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub(crate) fn release_lock(&mut self) {
        *locked(&self.shared.lock) = None;
    }

    /// What a launch does under the lock (only its holder writes the ledger) before its first change: the game's and
    /// the save's ledger rows (a play session may have no save), and the copy of gamesaves into the data root.
    /// Refused until the first row is written; Failed after it: quen's own rows and copies exist, nothing
    /// of the user's changed.
    fn prepare(
        &self,
        config: &Config,
        spec: &Spec,
        exe: &doctor::GameBuild,
        call: &mut Call,
    ) -> Result<Prep, Done> {
        let save = spec.save.as_deref();
        let data = save
            .map(|save| fs::read(save).map_err(|e| format!("{}: {e}", save.display())))
            .transpose()?;
        let mut ledger = open_ledger(config)?;
        let row = w3::game(&config.game_exe, &exe.exe, &exe.build);
        let game = ledger.add_game(&row).map_err(ledger_error)?;
        call.row("games", game);
        let failed = |why: String| {
            Done::Failed(
                format!("{why}; nothing of the game's was changed"),
                Map::new(),
            )
        };
        let row = save.zip(data).map(|(save, data)| Save {
            game_id: game,
            file: save
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            sha256: sha256_hex(&data),
            ..Save::default()
        });
        let save = row
            .map(|row| ledger.add_save(&row).map_err(|e| failed(ledger_error(e))))
            .transpose()?;
        if let Some(save) = save {
            call.row("saves", save);
        }
        call.lap("ledger");
        let saves = config.saves_dir().unwrap_or_default();
        let to = config.data_root.join("gamesaves");
        let copy = saves::backup(&saves, &to)
            .map_err(|e| failed(format!("copying gamesaves to {}: {e}", to.display())))?;
        call.lap("saves_copy");
        Ok(Prep {
            ledger,
            game,
            save,
            saves_before: saves::snapshot(&saves),
            saves,
            copy: json!({"to": to, "copied": copy.copied, "bytes": copy.bytes, "kept_older": copy.kept}),
        })
    }

    /// A launch whose boot never ran: its journal (empty) goes to its logs with nothing to undo, and the lock is
    /// released. Failed: quen's own rows and copies exist; the game's files are untouched.
    fn abandon(
        &mut self,
        config: &Config,
        journal_to: &Path,
        why: String,
        fields: Map<String, Value>,
    ) -> Done {
        let kept = win::rename_no_replace(&journal_path(config), journal_to);
        self.release_lock();
        match kept {
            Ok(()) => Done::Failed(format!("{why}; nothing of the game's was changed"), fields),
            Err(k) => Done::Failed(format!("{why}; the journal stays: {k}"), fields),
        }
    }

    /// `session_launch`, or with `plan` `session_plan`: every check and the plan, nothing changed and no lock (the
    /// client prompts for every launch, so the plan is a tool of its own that subagents may call).
    /// While a session is live, both are refused before they get here (`Shared::refusal`).
    fn launch(&mut self, call: &mut Call, args: &Value, plan: bool) -> Done {
        let config = match self.config() {
            Ok(config) => config.clone(),
            Err(why) => return why.into(),
        };
        if plan && args.get("user_ok").is_some() {
            return "unknown argument \"user_ok\": session_plan only plans"
                .to_string()
                .into();
        }
        // The session's id first: its backups go into its own logs folder.
        let id = match win::random_hex(8) {
            Ok(id) => id,
            Err(e) => return format!("session id: {e}").into(),
        };
        let logs = config
            .data_root
            .join("logs")
            .join(format!("{}-{id}", now()));
        let mut spec = match Spec::parse(&config, args, &logs.join("backup")) {
            Ok(spec) => spec,
            Err(why) => return why.into(),
        };
        // Before the journal: a live session's journal is no dead quen's (feature tests 1: a second daemon said so).
        if locked_elsewhere(&config) {
            return Done::Refused(
                "another quen holds the game lock: a session is live there (another quen serve, e.g. after the \
                 config changed): end it there first"
                    .into(),
            );
        }
        if journal_path(&config).exists() {
            return Done::Refused(
                "a journal a dead quen left exists: session_recover first".into(),
            );
        }
        // Audit J1: mods parked with no journal here are another data root's (or config's) session's: planning from
        // the game as it is would take quen's changes for the user's.
        if let Some(game_dir) = config.game_dir.as_deref()
            && let parked = w3::parked(game_dir)
            && !parked.is_empty()
        {
            return format!(
                "{} holds {parked:?}, and this data root has no journal for them: a session under another data root or \
                 config parked them. session_recover with the quen that launched it (its config), then launch",
                game_dir.join(quen_core::boot::PARKED).display()
            )
            .into();
        }
        if let Err(why) = interlock(&config) {
            return why.into();
        }
        // What the launch needs, then the build gate, checked for a dry run too.
        let (game_build, exe) = match doctor::launch(&config, spec.allow_unknown_build) {
            Ok(checked) => checked,
            Err(refused) => return refused,
        };
        // The build goes on the boot row, until the ledger has columns for it; an unknown one is among its conditions.
        spec.settings_diff
            .insert("build".into(), game_build.clone());
        if game_build["status"] == "allowed_unknown" {
            let conditions = spec.settings_diff.entry("conditions").or_insert(json!([]));
            if let Some(conditions) = conditions.as_array_mut() {
                conditions.push(json!("unknown_build"));
            }
        }
        call.lap("check");
        // The capture root over its budget: said before a session adds to it.
        let warnings: Vec<String> = disk::over_budget(&config).into_iter().collect();
        if plan {
            let mut fields = boot::plan_fields(&config, &spec);
            fields.insert("game_build".into(), game_build);
            if !warnings.is_empty() {
                fields.insert("warnings".into(), json!(warnings));
            }
            return Done::Ok(fields);
        }
        if !spec.user_ok {
            return Done::Refused(
                "user_ok: tell the user the game is about to launch and get their OK first".into(),
            );
        }
        match outside(&config.game_exe, None) {
            Ok(running) if running.is_empty() => {}
            Ok(running) => {
                let why = format!(
                    "{} runs (pids {running:?}): close the game first",
                    config.game_exe
                );
                return why.into();
            }
            Err(why) => return why.into(),
        }
        if call.stopped() {
            return Done::Refused("cancelled before anything was changed".into());
        }
        if let Err(why) = self.take_lock(&config, &id) {
            return why.into();
        }
        let Prep {
            mut ledger,
            game,
            save,
            saves,
            saves_before,
            copy,
        } = match self.prepare(&config, &spec, &exe, call) {
            Ok(prep) => prep,
            Err(done) => {
                self.release_lock();
                return done;
            }
        };
        // From here quen's own rows and copies exist: a stop is Failed, with what they are.
        let mut fields = Map::new();
        fields.insert("saves_copy".into(), copy);
        let made = fs::create_dir_all(logs.join("backup"))
            .map_err(|e| format!("{}: {e}", logs.display()))
            .and_then(|()| Job::new().map_err(|e| format!("the session's job: {e}")))
            .and_then(|job| {
                let tokens = win::random_hex(16).and_then(|t| Ok((t, win::random_hex(16)?)));
                let tokens = tokens.map_err(|e| format!("session token: {e}"))?;
                if call.stopped() {
                    return Err("cancelled before the game's files were changed".into());
                }
                Ok((Arc::new(job), tokens))
            });
        let (job, (token, channel)) = match made {
            Ok(made) => made,
            Err(why) => {
                self.release_lock();
                return Done::Failed(format!("{why}; nothing of the game's was changed"), fields);
            }
        };
        // The session's journal is on disk from here until its end, empty until the first change.
        let path = journal_path(&config);
        let journal = Journal::create(&JournalFs, &path)
            .and_then(|journal| JournalFs.write_atomic(&path, b"").map(|()| journal));
        let mut journal = match journal {
            Ok(journal) => journal,
            Err(e) => {
                self.release_lock();
                let why = format!("journal: {e}; nothing of the game's was changed");
                return Done::Failed(why, fields);
            }
        };
        call.lap("journal");
        // The boot's ledger row, written before anything changes; its mod set comes once the boot has parked.
        // The row names the journal where the end puts it, in the logs. fg and vsync are recorded only where the launch
        // forces them off: otherwise they are as the user set them.
        let journal_to = logs.join("journal.jsonl");
        // The devkit goes on the row once the boot saw it loaded in the game (boot::play).
        let mut diff = spec.settings_diff.clone();
        if let Some(Value::Array(conditions)) = diff.get_mut("conditions") {
            conditions.retain(|c| c != "devkit");
        }
        if diff.get("conditions") == Some(&json!([])) {
            diff.remove("conditions");
        }
        let boot = ledger.utc(now() as i64).and_then(|t_start| {
            ledger.start_boot(&BootStart {
                game_id: game,
                t_start,
                save_id: save,
                settings_diff: Some(diff).filter(|d| !d.is_empty()),
                devkit: Some(false),
                fg: (!spec.fg).then_some(false),
                vsync: (!spec.vsync).then_some(false),
                profiler: spec.profiler.clone(),
                journal_path: Some(journal_to.display().to_string()),
                mode: spec.play.then(|| "play".into()),
                ..BootStart::default()
            })
        });
        let boot = match boot {
            Ok(boot) => boot,
            Err(e) => {
                drop(journal);
                return self.abandon(&config, &journal_to, ledger_error(e), fields);
            }
        };
        call.row("boots", boot);
        *locked(&self.shared.job) = Some(job.clone());
        *locked(&self.shared.boot) = Some((boot, token.clone(), call.conn.id));
        self.live = Some(Live {
            owner: call.conn.id,
            token: token.clone(),
            channel: channel.clone(),
            id: id.clone(),
            job: job.clone(),
            logs: logs.clone(),
            boot,
            game,
            saves: saves.clone(),
            saves_before: saves_before.clone(),
            quen_saves: RefCell::default(),
            patches: RefCell::default(),
            launched: SystemTime::now(),
            profiler: spec.profiler.clone(),
            tracy_connected: Cell::new(false),
            build: exe.build,
            play: spec.play,
            handovers: hands::Handovers::new(saves_before),
            paused: Cell::new(false),
            path: Cell::new(false),
            client: call.conn.client.clone(),
            notice_at: Cell::new(None),
            away: Cell::new(false),
            idled: Cell::new(None),
            _awake: crate::win::Awake::hold(),
        });
        fields.insert("session".into(), json!(id));
        fields.insert("game_build".into(), game_build);
        // The first change. Each is journaled before it is made; from here on, failures unwind through the journal.
        let out = config.capture_root().join(boot.to_string());
        fields.insert("changes".into(), json!(spec.change_lines(&out)));
        let applied = w3::make_parking(&spec.changes).and_then(|()| {
            quen_core::boot::apply(&JournalFs, &mut journal, &spec.changes, &|| call.stopped())
        });
        drop(journal);
        call.lap("files");
        if let Err(e) = applied {
            return self.fail(call, format!("changing the game's files: {e}"), fields);
        }
        // The game, inside the job; a profiler's output goes to the boot's capture folder.
        let (argv, cwd) = spec.command(&config, &out);
        // A tracy launch's capture tool, started in the job just before the game, so it connects as the game's Tracy
        // starts listening.
        let capture = spec.profile.as_ref().and_then(|p| p.capture(&out));
        // The command channel's pipe opens only with a per-launch token, and serves only this process. A profile's
        // variables (a tracy launch's trigger and port) come from quen alone, never from the agent.
        let pid = std::process::id().to_string();
        let extra = spec.profile.as_ref().map(|p| p.env()).unwrap_or_default();
        let env: Vec<(&str, &str)> = [
            ("W3_QUEN_TOKEN", channel.as_str()),
            ("W3_QUEN_PID", pid.as_str()),
        ]
        .into_iter()
        .chain(extra.iter().map(|(k, v)| (*k, v.as_str())))
        .collect();
        let log = logs.join("game.log");
        // A shutdown, or this connection closing, kills the job at once while the boot runs (else the session ends the
        // normal way, saves first): flag first, then check, so either this or `stop` / `dropped` sees the other.
        self.shared.booting.store(call.conn.id, Ordering::SeqCst);
        if let Some(live) = self.live.as_mut() {
            live.launched = SystemTime::now();
        }
        let out = spec.profile.as_ref().map(|p| p.output_dir(&out));
        let spawn = |argv: &[String], env: &[(&str, &str)], cwd: &Path, log: &Path| {
            File::create(log)
                .and_then(|f| job.spawn(argv, env, cwd, &f))
                .map_err(|e| format!("{} did not start: {e}", argv[0]))
        };
        let started = (out
            .as_ref()
            .is_none_or(|out| fs::create_dir_all(out).is_ok()))
        .then(|| {
            if let (Some(capture), Some(out)) = (&capture, &out) {
                spawn(capture, &[], out, &logs.join(profile::TRACY_LOG))?;
            }
            spawn(&argv, &env, &cwd, &log)
        });
        let played = match started {
            _ if self.shared.stopping.load(Ordering::SeqCst)
                || call.conn.closed.load(Ordering::SeqCst) =>
            {
                Err(format!("stopped during the boot: {}", call.stop_cause()))
            }
            None => Err(format!(
                "the profiler's output folder {}",
                out.unwrap_or_default().display()
            )),
            Some(Err(why)) => Err(why),
            Some(Ok(_child)) => {
                call.lap("launch");
                fields.insert("command".into(), json!({"argv": argv, "cwd": cwd}));
                boot::play(self, call, &spec, &mut fields)
            }
        };
        self.shared.booting.store(0, Ordering::SeqCst);
        // A play session's devkit loads as the user's ReShade.ini says, and the boot row says whether it did.
        let loaded = |live: &Live| {
            let cx = game::Ctx {
                config: &config,
                live,
                hands: None,
            };
            cx.devkit_loaded()
        };
        let devkit = || spec.devkit || (spec.play && self.live.as_ref().is_some_and(loaded));
        let played = played.and_then(|()| match devkit() {
            true => {
                let mut diff = spec.settings_diff.clone();
                let conditions = diff.entry("conditions").or_insert(json!([]));
                if let Some(list) = conditions.as_array_mut()
                    && !list.contains(&json!("devkit"))
                {
                    list.push(json!("devkit"));
                }
                ledger
                    .set_boot_devkit(boot, &diff)
                    .map_err(|e| format!("the devkit loaded, but the boot row can't say so: {e}"))
            }
            false => Ok(()),
        });
        if let Err(why) = played {
            return self.fail(call, why, fields);
        }
        if let Some(game_dir) = &config.game_dir {
            // The mods that loaded, now that the boot has parked its set.
            let set = w3::mod_set(game_dir, &mut self.cache)
                .map_err(|e| format!("mod set: {e}"))
                .and_then(|entries| ledger.add_mod_set(&entries).map_err(ledger_error))
                .and_then(|set| {
                    let mark = ledger.set_boot_mod_set(boot, set);
                    mark.map(|()| set).map_err(ledger_error)
                });
            match set {
                Ok(set) => call.row("mod_sets", set),
                Err(e) => {
                    fields.insert("mod_set_error".into(), json!(e));
                }
            }
            call.lap("mod_set");
        }
        // The job's game, held so the state can say when it exits by itself.
        let pid = self.live.as_ref().and_then(|live| {
            let cx = game::Ctx {
                config: &config,
                live,
                hands: None,
            };
            cx.game_pid().ok()
        });
        *locked(&self.shared.game) = pid.and_then(|pid| Child::open(pid).ok());
        // In a play session the controls go to the user once the game runs; in a test session they are the agent's.
        let hands = if spec.play { Hands::User } else { Hands::Agent };
        self.shared.set_hands(Some(hands));
        fields.insert("token".into(), json!(token));
        fields.insert("pids".into(), json!(job.pids().unwrap_or_default()));
        if !warnings.is_empty() {
            fields.insert("warnings".into(), json!(warnings));
        }
        Done::Ok(fields)
    }

    /// A launch that changed something, then failed for `why`: the session ends (saves quiet, kill, replay), and the
    /// result says what that end did.
    fn fail(&mut self, call: &mut Call, why: String, mut fields: Map<String, Value>) -> Done {
        let end = self.finish(call, &why, true);
        fields.insert("end".into(), self.envelope_fields(end));
        Done::Failed(why, fields)
    }

    /// ini_session.py's `guard_saves`: a change in gamesaves other than the game's own AutoSave / CheckPoint
    /// saves (a manual save written, the quicksave replaced) means a key confirmed something it shouldn't have, so the
    /// session must end before another key can. Run it after every input batch and around every scripted load (the
    /// game.* tools), and on Err end the session with `finish`, as the launch does. A play session's guard gives the
    /// controls back instead ([`Session::guard_after`]), and while the user has them their saves are theirs.
    pub(crate) fn guard_saves(&self) -> Result<(), String> {
        let Some(live) = &self.live else {
            return Ok(());
        };
        if self.shared.hands() == Some(Hands::User) {
            return Ok(());
        }
        let (_, bad) = live.unexpected();
        if bad.is_empty() {
            return Ok(());
        }
        let then = match live.play {
            true => {
                "the controls went back to the user (hands_take takes them again); the end lists them"
            }
            false => "the session was ended",
        };
        Err(format!(
            "gamesaves changed beyond the game's own AutoSave / CheckPoint saves: {bad:?}: {then}"
        ))
    }

    /// After an input batch or a scripted load: if gamesaves changed beyond the game's own saves, a key confirmed
    /// something it shouldn't have, so the session ends now, before another key can, the tool's own fields kept. In a
    /// play session the controls go back to the user instead, and nothing is killed.
    fn guard_after(&mut self, call: &mut Call, done: Done) -> Done {
        let Err(why) = self.guard_saves() else {
            return done;
        };
        let mut fields = match done {
            Done::Ok(fields) | Done::Failed(_, fields) => fields,
            Done::Refused(_) | Done::Needs(..) => Map::new(),
            Done::RefusedWith(_, fields) => fields,
        };
        if let Some(live) = self.live.as_ref().filter(|l| l.play) {
            hands::sort(live, false);
            self.shared.set_hands(Some(Hands::User));
            return Done::Failed(why, fields);
        }
        let end = self.finish(call, &why, true);
        fields.insert("end".into(), self.envelope_fields(end));
        Done::Failed(why, fields)
    }

    /// The fields of a nested result (the end a failed launch ran).
    fn envelope_fields(&self, done: Done) -> Value {
        match done {
            Done::Ok(fields) => json!({"ok": true, "fields": fields}),
            Done::Refused(why) => json!({"ok": false, "refused": true, "reason": why}),
            Done::RefusedWith(why, fields) => {
                json!({"ok": false, "refused": true, "reason": why, "fields": fields})
            }
            Done::Needs(why, needs) => {
                json!({"ok": false, "refused": true, "reason": why, "needs": needs})
            }
            Done::Failed(why, fields) => json!({"ok": false, "reason": why, "fields": fields}),
        }
    }

    fn end(&mut self, call: &mut Call, args: &Value) -> Done {
        let token = args.get("token").and_then(Value::as_str);
        let extra = args.as_object().and_then(|a| {
            a.keys()
                .find(|k| !["token", "close_game"].contains(&k.as_str()))
        });
        if let Some(key) = extra {
            return format!("unknown argument {key:?}").into();
        }
        let close = match game::flag(args, "close_game") {
            Ok(close) => close,
            Err(why) => return why.into(),
        };
        let Some(live) = &self.live else {
            return Done::Refused("this quen has no live session".into());
        };
        if token != Some(live.token.as_str()) {
            return Done::Refused(
                "wrong or missing token: only the launching agent ends its session".into(),
            );
        }
        // A play session's game holds the user's progress; the session ends by itself when the user quits it.
        if live.play && !close && live.job.pids().is_ok_and(|p| !p.is_empty()) {
            return Done::Refused(
                "the play session's game runs, holding the user's progress: the session ends by itself when the user \
                 quits the game. To close the game now, the user saves first and agrees, then close_game: true"
                    .into(),
            );
        }
        self.finish(call, "session_end", false)
    }

    /// Ends the live session: waits for the gamesaves folder to go quiet, kills the job and waits for it to empty, lists
    /// the saves the game wrote, takes the same replay path as a recovery, records the boot's end and releases the
    /// lock. `failed`: the session ends because of `why`, an error.
    fn finish(&mut self, call: &mut Call, why: &str, failed: bool) -> Done {
        let Some(live) = self.live.take() else {
            return Done::Refused("this quen has no live session".into());
        };
        let mut fields = Map::new();
        fields.insert("session".into(), json!(live.id));
        fields.insert("why".into(), json!(why));
        if let Some(wait) = saves_quiet(&live.job, Some(&live.saves)) {
            fields.insert("saves_wait".into(), wait);
            call.lap("saves");
        }
        let game_exe = self
            .config()
            .map(|c| c.game_exe.clone())
            .unwrap_or_default();
        let taken = std::mem::take(&mut *locked(&self.shared.killed));
        let killed = kill(&live, &game_exe, &taken, &|| game_pids(&game_exe));
        *locked(&self.shared.job) = None;
        *locked(&self.shared.game) = None;
        *locked(&self.shared.boot) = None;
        call.lap("kill");
        if let Err(e) = killed {
            fields.insert("kill_error".into(), json!(e.to_string()));
        }
        // A tracy launch's capture, written as the game went: its ledger row.
        if let Some(tracy) = self
            .config()
            .ok()
            .and_then(|c| profile::tracy_end(c, &live, call))
        {
            fields.insert("tracy".into(), tracy);
        }
        // The game is gone, so gamesaves is final: what it wrote since the launch, tainted saves first. The last
        // changes are the user's or unexpected, as the controls were at the end.
        let (changed, last) = live.unexpected();
        // Findings 56, 62: a save the game deleted (its autosave rotation reaches the user's other playthroughs) has
        // its own list, with quen's pre-launch copy; quen never writes it back.
        let (gone, changed): (Vec<String>, Vec<String>) = changed
            .into_iter()
            .partition(|name| !live.saves.join(name).exists());
        let copies = self.config().ok().map(|c| c.data_root.join("gamesaves"));
        let deleted: Vec<Value> = gone
            .iter()
            .map(|name| {
                let copy = copies
                    .as_ref()
                    .map(|d| d.join(name))
                    .filter(|p| p.is_file());
                json!({"file": name, "copy": copy})
            })
            .collect();
        let h = &live.handovers;
        let to = match self.shared.hands() {
            Some(Hands::User) => &h.user,
            _ => &h.unexpected,
        };
        to.borrow_mut().extend(last);
        self.shared.set_hands(None);
        let unexpected: Vec<String> = h.unexpected.borrow().iter().cloned().collect();
        let user: Vec<String> = h.user.borrow().iter().cloned().collect();
        let since = live
            .patches
            .borrow()
            .iter()
            .filter(|(_, patch)| patch["taints"] != false)
            .map(|(t, _)| *t)
            .min();
        let tainted: Vec<&String> = changed
            .iter()
            .filter(|name| {
                let modified = fs::metadata(live.saves.join(name)).and_then(|m| m.modified());
                since.is_some_and(|since| modified.is_ok_and(|m| m >= since))
            })
            .collect();
        let quen: Vec<String> = live.quen_saves.borrow().keys().cloned().collect();
        let saves = json!({"tainted": tainted, "changed": changed, "deleted": deleted, "unexpected": unexpected,
            "quen": quen, "user": user});
        fields.insert("saves".into(), saves);
        // What the session left in the capture root, against the budget.
        if let Ok(config) = self.config() {
            fields.insert(
                "captures".into(),
                disk::after_end(config, live.boot, &live.id),
            );
        }
        if let Ok(config) = self.config()
            && let Some(bin) = config.bin_dir()
        {
            let docs = config.documents.clone();
            let kept = keep_game_logs(call, &bin, docs.as_deref(), live.launched, &live.logs);
            fields.insert("game_logs".into(), kept);
        }
        let done = self.unwind(call, &live.logs.join("journal.jsonl"));
        let failure = match (&done, failed) {
            (Ok(_), false) => None,
            (Ok(_), true) => Some(why.to_owned()),
            (Err((left, _)), false) => Some(format!("{LEFT}{left}")),
            (Err((left, _)), true) => Some(format!("{why}; {LEFT}{left}")),
        };
        // An end that leaves the journal leaves its boot open, with why: the recovery that replays the journal closes it
        // as recovered, the journal where the row says (live boot 1).
        // ponytail: a quen killed between the replay retiring the journal and this leaves the boot open; the next
        // recovery that has a journal closes it as recovered.
        let held = self.config().is_ok_and(|c| journal_path(c).exists());
        if let Err(e) = self.record_end(call, &live, &changed, &tainted, failure, held) {
            fields.insert("ledger_error".into(), json!(e));
        }
        self.release_lock();
        match done {
            Ok(more) => {
                fields.extend(more);
                Done::Ok(fields)
            }
            Err((why, more)) => {
                fields.extend(more);
                Done::Failed(why, fields)
            }
        }
    }

    /// Records how the live session's boot ended (failed when there is a `failure`), with a row for every save the game
    /// wrote, tainted ones marked; `held`: its journal stayed, so the boot stays open with the failure for the recovery
    /// to close. Never stops an end: the caller reports an error.
    fn record_end(
        &self,
        call: &mut Call,
        live: &Live,
        changed: &[String],
        tainted: &[&String],
        failure: Option<String>,
        held: bool,
    ) -> Result<(), String> {
        call.row("boots", live.boot);
        let mut ledger = open_ledger(self.config()?)?;
        for name in changed {
            // A save that is gone has nothing to hash; its name is in the envelope.
            let data = fs::read(live.saves.join(name));
            let Ok(data) = data.as_ref().map(Vec::as_slice) else {
                continue;
            };
            if !name.to_lowercase().ends_with(".sav") {
                continue;
            }
            let save = Save {
                game_id: live.game,
                file: name.clone(),
                sha256: sha256_hex(data),
                note: None,
                tainted_by_boot: tainted.contains(&name).then_some(live.boot),
                made_by_boot: None,
            };
            call.row("saves", ledger.add_save(&save).map_err(ledger_error)?);
        }
        let outcome = match failure {
            Some(_) => Outcome::Failed,
            None => Outcome::Ok,
        };
        let patches: Vec<Value> = live
            .patches
            .borrow()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        match failure {
            Some(why) if held => ledger.hold_boot(live.boot, &why, &patches),
            failure => end_boot(&mut ledger, live.boot, outcome, failure, patches),
        }
        .map_err(ledger_error)
    }

    /// `session_recover`, and the replay at start: takes the lock and unwinds a journal a dead quen left, then closes
    /// the boots it left open as recovered.
    fn recover(&mut self, call: &mut Call, asked: bool) -> Done {
        let config = match self.config() {
            Ok(config) => config.clone(),
            Err(why) => return why.into(),
        };
        if asked && self.live.is_some() {
            return Done::Refused(
                "this quen has a live session: end it with session_end and its token".into(),
            );
        }
        if let Err(why) = interlock(&config) {
            return why.into();
        }
        if !journal_path(&config).exists() {
            let mut fields = Map::new();
            fields.insert("replayed".into(), json!(false));
            return Done::Ok(fields);
        }
        if let Err(why) = self.take_lock(&config, "recover") {
            return why.into();
        }
        let logs = config
            .data_root
            .join("logs")
            .join(format!("{}-recover", now()));
        // The ledger is the record, not the restore: the replay runs without it.
        let ledger = open_ledger(&config).and_then(|ledger| Ok((open_boots(&ledger)?, ledger)));
        // With one boot open, its journal goes where that boot's own end would have put it, as its row says.
        let only = match ledger.as_ref().map(|(open, _)| open.as_slice()) {
            Ok([one]) => one.journal.as_deref().map(Path::new),
            _ => None,
        };
        let journal_to = match only {
            Some(path) if !path.exists() && path.parent().is_some_and(Path::is_dir) => {
                path.to_owned()
            }
            _ => logs.join("journal.jsonl"),
        };
        // The open boots' game logs, before a next launch overwrites them: since the earliest of them started.
        let since = ledger
            .as_ref()
            .ok()
            .and_then(|(open, _)| open.iter().map(|b| b.t_start).min())
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        let kept_logs = match (since, config.bin_dir(), journal_to.parent()) {
            (Some(since), Some(bin), Some(folder)) if fs::create_dir_all(folder).is_ok() => {
                let docs = config.documents.as_deref();
                Some(keep_game_logs(call, &bin, docs, since, folder))
            }
            _ => None,
        };
        let mut done = fs::create_dir_all(&logs)
            .map_err(|e| (format!("{}: {e}", logs.display()), Map::new()))
            .and_then(|()| self.unwind(call, &journal_to));
        if let (Some(kept), Ok(fields) | Err((_, fields))) = (kept_logs, &mut done) {
            fields.insert("game_logs".into(), kept);
        }
        let recorded = ledger.and_then(|(open, mut ledger)| {
            let at = utc(SystemTime::now());
            for boot in open.into_iter().filter(|_| done.is_ok()) {
                let failure = recovered(boot.failure, &at);
                end_boot(
                    &mut ledger,
                    boot.id,
                    Outcome::Recovered,
                    failure,
                    boot.patches,
                )
                .map_err(ledger_error)?;
                call.row("boots", boot.id);
            }
            Ok(())
        });
        if let Err(e) = recorded {
            let (Ok(fields) | Err((_, fields))) = &mut done;
            fields.insert("ledger_error".into(), json!(e));
        }
        self.release_lock();
        match done {
            Ok(mut fields) => {
                fields.insert("replayed".into(), json!(true));
                Done::Ok(fields)
            }
            Err((why, fields)) => Done::Failed(why, fields),
        }
    }

    /// Replays the journal, newest step first, and retires it to `journal_to` only when the replay was clean; then the
    /// parking folders it emptied go. Otherwise the journal stays for `session_recover`. Never while the game runs
    /// outside quen's job: it holds the files open and rewrites some as it exits. Needs the lock: a quen without it
    /// never gets here.
    pub(crate) fn unwind(
        &mut self,
        call: &mut Call,
        journal_to: &Path,
    ) -> Result<Map<String, Value>, (String, Map<String, Value>)> {
        let config = self.config().map_err(|why| (why, Map::new()))?.clone();
        let stays = "the journal stays for session_recover";
        let others = match outside(&config.game_exe, None) {
            Ok(others) if others.is_empty() => None,
            Ok(others) => Some(format!(
                "{} runs outside quen's job (pids {others:?}): close it, then session_recover",
                config.game_exe
            )),
            Err(e) => Some(e),
        };
        if let Some(why) = others {
            return Err((format!("{why}; {stays}"), Map::new()));
        }
        let path = journal_path(&config);
        let bytes =
            fs::read(&path).map_err(|e| (format!("{}: {e}", path.display()), Map::new()))?;
        let report = replay(&bytes, &JournalFs);
        call.lap("replay");
        let mut fields = Map::new();
        let clean = report.clean();
        fields.insert("replay".into(), json!(report));
        if !clean {
            return Err((
                format!("the replay left steps (replay.left): {stays}"),
                fields,
            ));
        }
        if let Some(game_dir) = &config.game_dir {
            w3::tidy_parking(game_dir);
        }
        if let Err(e) = win::rename_no_replace(&path, journal_to) {
            return Err((format!("replayed, but the journal stays: {e}"), fields));
        }
        fields.insert("journal".into(), json!(journal_to));
        // A renderdoc launch's swap, checked on its own after the replay: NVIDIA's signed interposer is back.
        if profile::swapped(&bytes)
            && let Some(bin) = config.bin_dir()
        {
            let signed = doctor::signed_interposer();
            match profile::restored(&bin, &signed, &win::authenticode) {
                Ok(check) => fields.insert("streamline".into(), check),
                Err(why) => return Err((why, fields)),
            };
        }
        Ok(fields)
    }
}

/// The gamesaves folder must be unchanged this long before a kill, waiting at most `SAVES_MAX`.
const SAVES_QUIET: Duration = Duration::from_secs(2);
const SAVES_MAX: Duration = Duration::from_secs(30);
/// How long the job's processes may take to exit after the kill, and nsys or Tracy's capture tool to write its report
/// or capture once the game is gone.
const JOB_EMPTY_MAX: Duration = Duration::from_secs(10);
const TOOL_WRITE_MAX: Duration = Duration::from_secs(120);
/// How long a daemon about to exit waits for a game outside its job to go before it replays a journal an end left.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Kills the session's job and waits for it to empty, then until `listed` (the process list the replay asks: tasklist)
/// shows none of the processes the job held: a killed game leaves the job at once but stays listed for seconds while
/// Windows takes it down (live boot 1: 4-8 s, and the replay refused it as a game outside the job). Under nsys the game
/// goes first, alone, so nsys writes a report whose capture range is still open as the game exits (one whose range
/// ended is written already: feature tests 2; the harness never killed nsys); the job's kill then takes whatever is
/// left. So under tracy once its capture tool connected: tracy-capture writes its capture when the game's connection
/// ends, then exits (0.14.1's capture.cpp); one that never connected would wait for the game forever.
fn kill(
    live: &Live,
    game_exe: &str,
    taken: &[u32],
    listed: &dyn Fn() -> io::Result<Vec<u32>>,
) -> io::Result<()> {
    let until = |max: Duration, done: &dyn Fn() -> io::Result<bool>| -> io::Result<bool> {
        let t = Instant::now();
        while !done()? {
            if t.elapsed() > max {
                return Ok(false);
            }
            sleep(Duration::from_millis(50));
        }
        Ok(true)
    };
    let empty = || Ok(live.job.pids()?.is_empty());
    // With what an earlier kill took from the job: they may still be listed.
    let held: Vec<u32> = live
        .job
        .pids()?
        .into_iter()
        .chain(taken.iter().copied())
        .collect();
    let writes =
        |p: &str| p.starts_with("nsys:") || (p.starts_with("tracy:") && live.tracy_connected.get());
    if live.profiler.as_deref().is_some_and(writes) {
        for (pid, name) in game::processes()? {
            if held.contains(&pid) && name.eq_ignore_ascii_case(game_exe) {
                win::terminate(pid)?;
            }
        }
        until(TOOL_WRITE_MAX, &empty)?;
    }
    live.job.kill()?;
    if !until(JOB_EMPTY_MAX, &empty)? {
        return Err(io::Error::other("the job's processes outlived the kill"));
    }
    let gone = || Ok(!listed()?.iter().any(|pid| held.contains(pid)));
    if !until(JOB_EMPTY_MAX, &gone)? {
        let why = format!("{game_exe} (pids {held:?}) is still listed after the kill");
        return Err(io::Error::other(why));
    }
    Ok(())
}

/// The game's logs from a boot: every `*.log` in its bin folder and the script log in `documents` (finding 50) written
/// since `since`, copied into `<logs>\game-logs\` (the next launch overwrites them; W3BoneFix's and W3Quen's were the
/// tests' evidence). Copies only: the game's files stay as they are. Each copy goes into the result's artifacts, with
/// its sha256; the field says where, how many, and what didn't copy. They can hold personal data (game.log holds a
/// Steam id): they stay in the data root.
fn keep_game_logs(
    call: &mut Call,
    bin: &Path,
    documents: Option<&Path>,
    since: SystemTime,
    logs: &Path,
) -> Value {
    let (copied, failed) = copy_logs(bin, documents, since, &logs.join("game-logs"));
    for path in &copied {
        call.artifact(path);
    }
    json!({"folder": logs.join("game-logs"), "files": copied.len(), "failed": failed})
}

/// [`keep_game_logs`]'s copy: the copies made, and each log that didn't copy, with why.
fn copy_logs(
    bin: &Path,
    documents: Option<&Path>,
    since: SystemTime,
    to: &Path,
) -> (Vec<PathBuf>, Vec<String>) {
    let (mut copied, mut failed) = (Vec::new(), Vec::new());
    let logs = fs::read_dir(bin)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("log")));
    let scripts = documents.map(|d| d.join(w3::SCRIPTS_LOG));
    for log in logs.chain(scripts) {
        let written = fs::metadata(&log).and_then(|m| m.modified());
        if !written.is_ok_and(|t| t >= since) {
            continue;
        }
        let dest = to.join(log.file_name().unwrap_or_default());
        match fs::create_dir_all(to).and_then(|()| fs::copy(&log, &dest)) {
            Ok(_) => copied.push(dest),
            Err(e) => failed.push(format!("{}: {e}", log.display())),
        }
    }
    (copied, failed)
}

/// Waits, while the job's game runs, until gamesaves stops changing, so the game never dies mid-save. Every kill
/// comes after it. The wait, if there was one.
fn saves_quiet(job: &Job, saves: Option<&Path>) -> Option<Value> {
    let saves = saves.filter(|dir| dir.is_dir())?;
    if job.pids().unwrap_or_default().is_empty() {
        return None;
    }
    let (secs, quiet) = saves::wait_quiet(saves, SAVES_QUIET, SAVES_MAX);
    Some(json!({"s": secs, "quiet": quiet}))
}

/// What [`Session::prepare`] recorded and copied.
struct Prep {
    ledger: Ledger,
    game: i64,
    save: Option<i64>,
    saves: PathBuf,
    saves_before: Snapshot,
    copy: Value,
}

/// The ledger, `<data root>\state\quen.db`. Only the lock holder opens it to write.
pub(crate) fn open_ledger(config: &Config) -> Result<Ledger, String> {
    Ledger::open(config.state_dir().join("quen.db")).map_err(ledger_error)
}

pub(crate) fn ledger_error(e: quen_core::ledger::Error) -> String {
    format!("ledger: {e}")
}

/// What a held boot's failure says before what its end left (feature tests 2b: a recovered boot kept "close it, then
/// session_recover", a step quen had then taken itself).
const LEFT: &str = "the end left the journal: ";

/// A held boot's failure once a replay recovered it: why the session ended, what made its end leave the journal
/// without the steps that were the user's to take then, and when quen replayed it.
fn recovered(failure: Option<String>, at: &str) -> Option<String> {
    let failure = failure?;
    let (why, left) = match failure.split_once(LEFT) {
        Some((why, left)) => (why.trim_end_matches("; "), Some(left)),
        None => (failure.as_str(), None),
    };
    let steps = [": close it", ": the journal stays", "; the journal stays"];
    let cause = left.map(|l| {
        &l[..steps
            .iter()
            .filter_map(|s| l.find(s))
            .min()
            .unwrap_or(l.len())]
    });
    let parts = [
        Some(why.to_owned()).filter(|w| !w.is_empty()),
        cause.map(|c| format!("its end left the journal ({c})")),
        Some(format!("quen replayed it at {at}")),
    ];
    Some(parts.into_iter().flatten().collect::<Vec<_>>().join("; "))
}

/// Records how a boot ended.
fn end_boot(
    ledger: &mut Ledger,
    boot: i64,
    outcome: Outcome,
    failure: Option<String>,
    patches: Vec<Value>,
) -> quen_core::ledger::Result<()> {
    let end = BootEnd {
        t_end: ledger.utc(now() as i64)?,
        outcome,
        failure,
        patches,
    };
    ledger.finish_boot(boot, &end)
}

/// A boot nobody closed: its journal path, and why its end failed and its patches, if that end left the journal.
struct Open {
    id: i64,
    /// When it started, in seconds since 1970.
    t_start: u64,
    journal: Option<String>,
    failure: Option<String>,
    patches: Vec<Value>,
}

/// The boots nobody closed. A dead quen leaves one, and so does an end that left its journal; the lock holder closes
/// its own otherwise, and a recovery runs only under the lock.
fn open_boots(ledger: &Ledger) -> Result<Vec<Open>, String> {
    let sql = "SELECT id, journal_path, failure, patches, CAST(strftime('%s', t_start) AS INTEGER)
               FROM boots WHERE t_end IS NULL";
    let rows = ledger
        .query(sql, &[], Duration::from_secs(5))
        .map_err(ledger_error)?;
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let row = |r: &Vec<Value>| {
        Some(Open {
            id: r[0].as_i64()?,
            t_start: r[4].as_u64().unwrap_or_default(),
            journal: text(&r[1]),
            failure: text(&r[2]),
            patches: r[3]
                .as_str()
                .and_then(|p| serde_json::from_str(p).ok())
                .unwrap_or_default(),
        })
    };
    Ok(rows.rows.iter().filter_map(row).collect())
}

/// The pids of processes with this image name (read-only, via tasklist). An error means quen can't tell.
fn game_pids(image: &str) -> io::Result<Vec<u32>> {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("tasklist exited {}", out.status)));
    }
    Ok(pids_in(&String::from_utf8_lossy(&out.stdout)))
}

/// The pids of `image` that run outside `job`, or why quen can't tell.
pub(crate) fn outside(image: &str, job: Option<&Job>) -> Result<Vec<u32>, String> {
    let inside = job.map_or(Ok(Vec::new()), Job::pids);
    let all = game_pids(image).and_then(|all| Ok((all, inside?)));
    let (all, inside) = all.map_err(|e| format!("can't tell whether {image} runs: {e}"))?;
    Ok(all.into_iter().filter(|p| !inside.contains(p)).collect())
}

/// The pid column of tasklist's CSV rows (`"image","pid",...`); its "no tasks" message has no rows.
fn pids_in(csv: &str) -> Vec<u32> {
    csv.lines()
        .filter_map(|line| line.split("\",\"").nth(1)?.parse().ok())
        .collect()
}

/// The journal's [`Fs`]: the real file system. A rename never replaces anything and keeps the file id; what is
/// removed goes to the Recycle Bin.
pub(crate) struct JournalFs;

impl Fs for JournalFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    /// The rename is written through and the file flushed after it, which commits NTFS's log up to the rename: a step
    /// is on disk before its action starts, even when that action is on another volume.
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".quen-tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        win::rename_over(Path::new(&tmp), path)?;
        File::options().write(true).open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        win::rename_no_replace(from, to)
    }

    /// File by file into a new folder (never into one that exists); a link in the tree fails the copy, which the
    /// launch refuses before.
    fn copy_tree(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let (kind, dest) = (entry.file_type()?, to.join(entry.file_name()));
            if kind.is_symlink() {
                let why = format!("{} is a link", entry.path().display());
                return Err(io::Error::other(why));
            }
            match kind.is_dir() {
                true => self.copy_tree(&entry.path(), &dest)?,
                false => drop(fs::copy(entry.path(), &dest)?),
            }
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    /// A file's bytes; a folder (a parked mod) by its files' paths, sizes and write times, not their bytes, so parking
    /// a mod of gigabytes reads none of it.
    /// ponytail: a rewrite that keeps a file's size and write time passes; nothing rewrites a parked mod that way.
    fn sha256(&self, path: &Path) -> io::Result<String> {
        if !fs::symlink_metadata(path)?.is_dir() {
            return Ok(sha256_hex(&fs::read(path)?));
        }
        let mut lines = Vec::new();
        let mut todo = vec![path.to_owned()];
        while let Some(dir) = todo.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let meta = fs::symlink_metadata(entry.path())?;
                let rel = entry
                    .path()
                    .strip_prefix(path)
                    .unwrap_or(&entry.path())
                    .to_owned();
                if meta.is_dir() {
                    todo.push(entry.path());
                }
                let mtime = meta
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                lines.push(format!(
                    "{}\t{}\t{}\n",
                    rel.display(),
                    meta.len(),
                    mtime.as_nanos()
                ));
            }
        }
        lines.sort();
        Ok(sha256_hex(lines.concat().as_bytes()))
    }

    fn file_id(&self, path: &Path) -> io::Result<String> {
        win::file_id(path)
    }

    fn fsync(&self, path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path)?.is_dir() {
            return Ok(()); // a folder's rename is NTFS metadata, journaled by the file system itself
        }
        File::options().write(true).open(path)?.sync_all()
    }

    fn recycle(&self, path: &Path) -> io::Result<()> {
        win::recycle(path)
    }

    fn mtime(&self, path: &Path) -> io::Result<u64> {
        let since = fs::metadata(path)?.modified()?.duration_since(UNIX_EPOCH);
        Ok(since.map_or(0, |d| d.as_nanos() as u64))
    }

    fn set_mtime(&self, path: &Path, nanos: u64) -> io::Result<()> {
        let t = UNIX_EPOCH + Duration::from_nanos(nanos);
        File::options().write(true).open(path)?.set_modified(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quen_core::park::Entry;

    /// Feature tests 2b: a boot quen recovered kept the end's "close it, then session_recover". Once replayed, the
    /// failure says why the session ended, what made its end leave the journal, and when quen replayed it; an older
    /// row's text is kept whole.
    #[test]
    fn a_recovered_boot_says_why_it_ended_and_when_it_was_replayed() {
        let at = "2026-10-18T14:30:46Z";
        let outside = "y runs outside quen's job (pids [7])";
        let stays = "the journal stays for session_recover";
        let held = format!(
            "stopped during the boot: x; {LEFT}{outside}: close it, then session_recover; {stays}"
        );
        assert_eq!(
            recovered(Some(held), at).unwrap(),
            format!(
                "stopped during the boot: x; its end left the journal ({outside}); quen replayed it at {at}"
            )
        );
        let left = format!(
            "{LEFT}the replay left steps (replay.left): the journal stays for session_recover"
        );
        assert_eq!(
            recovered(Some(left), at).unwrap(),
            format!(
                "its end left the journal (the replay left steps (replay.left)); quen replayed it at {at}"
            )
        );
        assert_eq!(
            recovered(Some("older text".into()), at).unwrap(),
            format!("older text; quen replayed it at {at}")
        );
        assert_eq!(
            recovered(None, at),
            None,
            "a dead quen's boot says nothing new"
        );
    }

    #[test]
    fn tasklist_rows_give_pids_and_its_message_none() {
        let rows = "\"witcher3.exe\",\"4242\",\"Console\",\"1\",\"9,000 K\"\r\n\"witcher3.exe\",\"7\",\"Console\",\"1\",\"1 K\"\r\n";
        assert_eq!(pids_in(rows), [4242, 7]);
        assert!(
            pids_in("INFO: No tasks are running which match the specified criteria.\r\n")
                .is_empty()
        );
    }

    /// Feature tests 2: ngfx closes the game after its trace, and quen saw only `running: false` at the next call. Every
    /// envelope's state now says when the job's game exited.
    #[test]
    fn the_state_says_when_the_jobs_game_exited() {
        let fx = crate::game::tests::Fake::new("exited");
        *locked(&fx.shared.job) = Some(fx.job.clone());
        *locked(&fx.shared.game) = Some(Child::open(fx.job.pids().unwrap()[0]).unwrap());
        assert_eq!(state(&fx.shared)["game"].get("exited"), None);
        fx.job.kill().unwrap();
        let t = Instant::now();
        while state(&fx.shared)["game"].get("exited").is_none() {
            assert!(t.elapsed() < Duration::from_secs(5), "no exit time");
            sleep(Duration::from_millis(20));
        }
        let game = &state(&fx.shared)["game"];
        assert_eq!(game["running"], false, "{game}");
        let exited = game["exited"].as_str().unwrap_or_default();
        assert!(exited.len() == 20 && exited.ends_with('Z'), "{game}");
        for (secs, want) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ] {
            assert_eq!(utc(UNIX_EPOCH + Duration::from_secs(secs)), want);
            let ms = UNIX_EPOCH + Duration::from_millis(secs * 1000 + 7);
            assert_eq!(utc_ms(ms), want.replace('Z', ".007Z"));
        }
    }

    /// Feature tests 2: the game's logs were the evidence, and the next launch overwrote them. A boot's own (written
    /// since its launch) are copied into its logs folder; an older one, and anything not a log, aren't.
    #[test]
    fn the_boots_game_logs_are_copied_and_older_ones_left() {
        let dir = std::env::temp_dir().join(format!("quen-game-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // this test's own temp folder, from an earlier run
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let launched = SystemTime::now() - Duration::from_secs(60);
        fs::write(bin.join("W3Quen.log"), "this boot").unwrap();
        fs::write(bin.join("W3Quen.asi"), "not a log").unwrap();
        let old = bin.join("Old.LOG");
        fs::write(&old, "an earlier boot").unwrap();
        let file = fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(launched - Duration::from_secs(3600))
            .unwrap();
        drop(file);
        // Finding 50: the script log in the documents folder, written this boot, is copied too.
        let docs = dir.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join(w3::SCRIPTS_LOG), "[Quen] this boot").unwrap();
        let to = dir.join(r"logs\game-logs");
        let (copied, failed) = copy_logs(&bin, Some(&docs), launched, &to);
        assert_eq!(copied, [to.join("W3Quen.log"), to.join(w3::SCRIPTS_LOG)]);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(fs::read_to_string(&copied[0]).unwrap(), "this boot");
        assert_eq!(fs::read_to_string(&copied[1]).unwrap(), "[Quen] this boot");
        assert!(bin.join("W3Quen.log").exists(), "copied, not moved");
        let _ = fs::remove_dir_all(&dir); // this test's own temp folder
    }

    /// The end lists the files game_save accepted as saves.quen and leaves them out of unexpected; a menu save beside
    /// them is unexpected, and both are changed.
    #[test]
    fn the_end_lists_quens_saves_and_only_the_others_as_unexpected() {
        let fx = crate::game::tests::Fake::new("end-saves");
        let mut session = fx.session();
        let dir = fx.dir.join("gamesaves");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ManualSave_q.sav"), "quen's").unwrap();
        let live = session.live.as_ref().unwrap();
        live.quen_saves.borrow_mut().extend(saves::snapshot(&dir));
        fs::write(dir.join("ManualSave_m.sav"), "the menu's").unwrap();
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let done = session.end(&mut call, &json!({"token": "tok"}));
        let out = session.envelope("session_end", call, done);
        let saves = &out["saves"];
        assert_eq!(saves["quen"], json!(["ManualSave_q.sav"]), "{out}");
        assert_eq!(saves["unexpected"], json!(["ManualSave_m.sav"]), "{out}");
        let both = json!(["ManualSave_m.sav", "ManualSave_q.sav"]);
        assert_eq!(saves["changed"], both, "{out}");
        assert_eq!(saves["deleted"], json!([]), "{out}");
    }

    /// Findings 56, 62: a save the game deleted during the boot is listed under deleted, not changed, with quen's
    /// pre-launch copy when there is one; quen writes nothing back.
    #[test]
    fn the_end_lists_a_save_the_game_deleted_with_quens_copy() {
        let fx = crate::game::tests::Fake::new("end-deleted");
        let mut session = fx.session();
        let dir = fx.dir.join("gamesaves");
        let copies = fx.dir.join("data").join("gamesaves");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&copies).unwrap();
        for name in ["AutoSave_old.sav", "AutoSave_nocopy.sav"] {
            fs::write(dir.join(name), "the user's").unwrap();
        }
        fs::write(copies.join("AutoSave_old.sav"), "the user's").unwrap();
        session.live.as_mut().unwrap().saves_before = saves::snapshot(&dir);
        for name in ["AutoSave_old.sav", "AutoSave_nocopy.sav"] {
            fs::remove_file(dir.join(name)).unwrap();
        }
        fs::write(dir.join("AutoSave_new.sav"), "this boot's").unwrap();
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let done = session.end(&mut call, &json!({"token": "tok"}));
        let out = session.envelope("session_end", call, done);
        let saves = &out["saves"];
        assert_eq!(saves["changed"], json!(["AutoSave_new.sav"]), "{out}");
        let copy = copies.join("AutoSave_old.sav");
        let want = json!([{"file": "AutoSave_nocopy.sav", "copy": null}, {"file": "AutoSave_old.sav", "copy": copy}]);
        assert_eq!(saves["deleted"], want, "{out}");
        assert_eq!(saves["unexpected"], json!([]), "{out}");
        assert!(
            !dir.join("AutoSave_old.sav").exists(),
            "quen wrote a save back"
        );
    }

    /// In a play session the user's saves are allowed (the guard passes while they have the controls, and a hand-over
    /// sorts them as theirs); a save during a hand-over fails that call and gives the controls back, nothing killed.
    /// The end refuses while the game runs unless close_game, and lists both kinds. A test session's guard still ends
    /// the session (tests/session.rs).
    #[test]
    fn a_play_sessions_saves_are_the_users_and_one_in_a_hand_over_fails_the_call() {
        let fx = crate::game::tests::Fake::new("play-saves");
        let mut session = fx.session();
        let dir = fx.dir.join("gamesaves");
        fs::create_dir_all(&dir).unwrap();
        let live = session.live.as_mut().unwrap();
        (live.play, live.handovers) = (true, hands::Handovers::new(saves::snapshot(&dir)));
        fx.shared.set_hands(Some(Hands::User));
        fs::write(dir.join("ManualSave_user.sav"), "the user's").unwrap();
        assert_eq!(session.guard_saves(), Ok(()));
        let live = session.live.as_ref().unwrap();
        assert_eq!(hands::sort(live, true), ["ManualSave_user.sav"]);
        fx.shared.set_hands(Some(Hands::Agent));
        assert_eq!(session.guard_saves(), Ok(()));
        fs::write(dir.join("ManualSave_menu.sav"), "a key saved").unwrap();
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let fields = Map::from_iter([("sent".to_string(), json!(1))]);
        let done = session.guard_after(&mut call, Done::Ok(fields));
        let out = session.envelope("game_keys", call, done);
        assert_eq!(out["ok"], false, "{out}");
        assert_eq!(out["refused"], false, "{out}");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("ManualSave_menu.sav"),
            "{out}"
        );
        assert_eq!(out["sent"], 1, "the call's own fields: {out}");
        assert_eq!(out["state"]["hands"], "user", "{out}");
        assert!(out.get("end").is_none() && session.live.is_some(), "{out}");
        assert!(!fx.job.pids().unwrap().is_empty(), "nothing killed");
        // The user saves again, then the end: refused while the game runs, unless close_game.
        fs::write(dir.join("ManualSave_later.sav"), "the user's again").unwrap();
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let done = session.end(&mut call, &json!({"token": "tok"}));
        let out = session.envelope("session_end", call, done);
        assert_eq!(out["refused"], true, "{out}");
        assert!(session.live.is_some());
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let args = json!({"token": "tok", "close_game": true});
        let done = session.end(&mut call, &args);
        let out = session.envelope("session_end", call, done);
        let user = json!(["ManualSave_later.sav", "ManualSave_user.sav"]);
        assert_eq!(out["saves"]["user"], user, "{out}");
        assert_eq!(
            out["saves"]["unexpected"],
            json!(["ManualSave_menu.sav"]),
            "{out}"
        );
        assert!(out["state"].get("hands").is_none(), "{out}");
    }

    /// A test session handed to the user: their save is theirs, listed at the end as saves.user, not an end of the
    /// session; once the agent has the controls back, a save ends it.
    #[test]
    fn a_test_sessions_hand_over_lists_the_users_saves() {
        let fx = crate::game::tests::Fake::new("test-saves");
        let mut session = fx.session();
        let dir = fx.dir.join("gamesaves");
        fs::create_dir_all(&dir).unwrap();
        session.live.as_mut().unwrap().handovers = hands::Handovers::new(saves::snapshot(&dir));
        fx.shared.set_hands(Some(Hands::User));
        fs::write(dir.join("ManualSave_user.sav"), "the user's").unwrap();
        assert_eq!(session.guard_saves(), Ok(()));
        let live = session.live.as_ref().unwrap();
        assert_eq!(hands::sort(live, true), ["ManualSave_user.sav"]);
        fx.shared.set_hands(Some(Hands::Agent));
        assert_eq!(session.guard_saves(), Ok(()));
        fs::write(dir.join("ManualSave_menu.sav"), "a key saved").unwrap();
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let done = session.guard_after(&mut call, Done::Ok(Map::new()));
        let out = session.envelope("game_keys", call, done);
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("the session was ended"),
            "{out}"
        );
        assert!(session.live.is_none(), "{out}");
        let saves = &out["end"]["fields"]["saves"];
        assert_eq!(saves["user"], json!(["ManualSave_user.sav"]), "{out}");
        assert_eq!(saves["unexpected"], json!(["ManualSave_menu.sav"]), "{out}");
    }

    /// Live boot 1: the job empties at once on the kill, but tasklist (which the replay asks) lists the dying game for
    /// seconds more. The end waits until the probe lists none of the job's processes; a probe that can't tell fails it.
    #[test]
    fn the_kill_waits_until_the_process_list_lets_the_game_go() {
        let fx = crate::game::tests::Fake::new("kill");
        let session = fx.session();
        let live = session.live.as_ref().unwrap();
        let held = fx.job.pids().unwrap();
        assert!(!held.is_empty());
        let asked = Cell::new(0);
        // Still listed three times after the job let them go, beside another process of that name.
        let lingering = || {
            asked.set(asked.get() + 1);
            let mut listed = vec![7];
            if asked.get() <= 3 {
                listed.extend(&held);
            }
            Ok(listed)
        };
        kill(live, "game.exe", &[], &lingering).unwrap();
        assert!(fx.job.pids().unwrap().is_empty());
        assert_eq!(asked.get(), 4, "asked until it was gone, and no longer");
        // Feature tests 2b: a dropped connection's kill emptied the job first; the end still waits for those.
        asked.set(0);
        kill(live, "game.exe", &held, &lingering).unwrap();
        assert_eq!(
            asked.get(),
            4,
            "waited for the processes the earlier kill took"
        );
        let blind = || Err(io::Error::other("tasklist failed"));
        let err = kill(live, "game.exe", &[], &blind).unwrap_err();
        assert!(err.to_string().contains("tasklist"), "{err}");
    }

    /// The journal's real file system, dying before its `limit`-th write (a write, rename or recycle): that call and
    /// every later one fail, as if quen were killed right there. Recycling moves into `trash` instead of the Recycle
    /// Bin.
    struct Dying<'a> {
        trash: &'a Path,
        writes: Cell<usize>,
        limit: usize,
    }

    impl Dying<'_> {
        fn write(&self) -> io::Result<()> {
            let n = self.writes.get();
            self.writes.set(n + 1);
            if n >= self.limit {
                return Err(io::Error::other("killed"));
            }
            Ok(())
        }
    }

    impl Fs for Dying<'_> {
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            JournalFs.read(path)
        }
        fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
            self.write()?;
            JournalFs.write_atomic(path, data)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.write()?;
            JournalFs.rename(from, to)
        }
        fn exists(&self, path: &Path) -> bool {
            JournalFs.exists(path)
        }
        fn sha256(&self, path: &Path) -> io::Result<String> {
            JournalFs.sha256(path)
        }
        fn file_id(&self, path: &Path) -> io::Result<String> {
            JournalFs.file_id(path)
        }
        fn fsync(&self, path: &Path) -> io::Result<()> {
            JournalFs.fsync(path)
        }
        fn recycle(&self, path: &Path) -> io::Result<()> {
            self.write()?;
            fs::create_dir_all(self.trash)?;
            let n = fs::read_dir(self.trash)?.count();
            fs::rename(path, self.trash.join(n.to_string()))
        }
        fn mtime(&self, path: &Path) -> io::Result<u64> {
            JournalFs.mtime(path)
        }
        fn set_mtime(&self, path: &Path, nanos: u64) -> io::Result<()> {
            self.write()?;
            JournalFs.set_mtime(path, nanos)
        }
    }

    /// Every folder (None) and file (its bytes) under `root`, by path relative to it.
    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut out = std::collections::BTreeMap::new();
        let mut todo = vec![root.to_owned()];
        while let Some(dir) = todo.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().to_owned();
                if path.is_dir() {
                    todo.push(path);
                    out.insert(rel, None);
                } else {
                    out.insert(rel, Some(fs::read(&path).unwrap()));
                }
            }
        }
        out
    }

    /// Crash injection on the real file system: the native boot (quen-core's plan: settings, general.ini, two mods
    /// parked, steam_appid.txt placed; three files staged, one over a file it parks, one over a mod the plan parks) on a
    /// synthetic game folder through `JournalFs` (MoveFileEx without replace, file ids, folder hashes), killed before
    /// each of its writes in turn; every replay through it gives the folders back byte for byte, the staged mod's log
    /// and the parking folders gone and the spent backups recycled.
    #[test]
    fn a_native_boot_killed_before_any_write_replays_back_on_disk() {
        use quen_core::boot::{self, Inputs};
        let root = std::env::temp_dir().join(format!("quen-native-crash-{}", std::process::id()));
        let entry = |name: &str, is_dir| Entry {
            name: name.into(),
            is_dir,
            is_link: false,
        };
        let mods = [entry("modAlpha", true)];
        let bin = [entry("Tweaks.asi", false), entry("Tweaks.ini", false)];
        let mut limit = 0;
        loop {
            let dir = root.join(limit.to_string());
            let (game, docs, quen) = (dir.join("game"), dir.join("docs"), dir.join("quen"));
            let bin_dir = game.join("bin").join("x64_dx12");
            for folder in [
                game.join("Mods/modAlpha/content"),
                game.join("bin/config/base"),
                bin_dir.clone(),
                docs.clone(),
                quen.join("backup"),
            ] {
                fs::create_dir_all(folder).unwrap();
            }
            let settings = "[Rendering]\r\nDLSSGMode=2\r\n[Gameplay]\r\nCameraAutoRotX=true\r\nCameraAutoRotY=true\r\n\
                            [Version]\r\nConfigVersion=7\r\n";
            fs::write(docs.join("dx12user.settings"), settings).unwrap();
            fs::write(game.join("bin/config/base/general.ini"), "[General]\r\n").unwrap();
            fs::write(game.join("Mods/modAlpha/content/blob0.bundle"), "alpha").unwrap();
            fs::write(bin_dir.join("Tweaks.asi"), "tweaks").unwrap();
            fs::write(bin_dir.join("Tweaks.ini"), "the user's ini").unwrap();
            let pc = dir.join("pc");
            fs::create_dir_all(&pc).unwrap();
            let staged: Vec<PathBuf> = ["W3Test.asi", "Tweaks.asi", "Tweaks.ini"]
                .iter()
                .map(|name| {
                    fs::write(pc.join(name), format!("a test {name}")).unwrap();
                    pc.join(name)
                })
                .collect();
            let compiled = quen_core::boot::compiled_scripts(&game);
            fs::create_dir_all(compiled.parent().unwrap()).unwrap();
            fs::write(&compiled, "compiled").unwrap();
            let old = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
            File::options()
                .write(true)
                .open(&compiled)
                .unwrap()
                .set_modified(old)
                .unwrap();
            let (original_game, original_docs) = (tree(&game), tree(&docs));
            let backups = quen.join("backup");
            let inputs = Inputs {
                game_dir: &game,
                documents: &docs,
                backups: &backups,
                fg: false,
                vsync: true,
                flat: "",
                sections: "",
                park: "ALL",
                keep: "",
                never_park: &w3::never_park(false),
                pairs: quen_core::park::W3_PAIRS,
                mods: &mods,
                bin: &bin,
                renodx: None,
                load_from_dll_main: None,
                play: false,
            };
            let mut changes = boot::plan(&JournalFs, &inputs).unwrap();
            let never = w3::never_park(false);
            let stage = boot::stage(&JournalFs, &game, &staged, &never, &bin, &[], &changes);
            changes.extend(stage.unwrap());
            assert_eq!(changes.len(), 2 + 1 + 2 + 1 + 4 + 2, "{changes:?}");
            w3::make_parking(&changes).unwrap();
            let trash = quen.join("trash");
            let dying = Dying {
                trash: &trash,
                writes: Cell::new(0),
                limit,
            };
            let path = quen.join("journal.jsonl");
            let applied = Journal::create(&dying, &path)
                .and_then(|mut journal| boot::apply(&dying, &mut journal, &changes, &|| false));
            if applied.is_ok() {
                fs::write(&compiled, "recompiled by the game").unwrap(); // a new write time too
                fs::write(bin_dir.join("W3Test.log"), "the staged mod's log").unwrap();
            }
            let journal = fs::read(&path).unwrap_or_default();
            let alive = Dying {
                trash: &trash,
                writes: Cell::new(0),
                limit: usize::MAX,
            };
            let report = replay(&journal, &alive);
            w3::tidy_parking(&game);
            let at = format!("killed before write {limit}");
            assert!(
                report.clean() && report.kept.is_empty(),
                "{at}: {report:#?}"
            );
            // Audit J6: a staged file is journaled before it is written, so none is left either.
            assert_eq!(tree(&game), original_game, "{at}: {report:#?}");
            assert_eq!(tree(&docs), original_docs, "{at}: {report:#?}");
            let mtime = fs::metadata(&compiled).unwrap().modified().unwrap();
            assert_eq!(mtime, old, "{at}: the compiled scripts' write time");
            assert_eq!(
                fs::read_dir(&backups).unwrap().count(),
                0,
                "{at}: backups not recycled"
            );
            if applied.is_ok() {
                assert!(limit > 15, "{limit} writes");
                assert_eq!(report.undone.len(), 6 + 4 + 1, "{report:#?}");
                break;
            }
            limit += 1;
        }
        fs::remove_dir_all(root).unwrap(); // this test's own temp folder
    }
}
