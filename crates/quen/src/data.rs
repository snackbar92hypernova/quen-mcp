//! The game-data tools: `data_find` (paths in the bundles' tables of contents, or in REDkit's depot), `data_read` (one
//! file decoded: a CR2W file's tables and property tree), `data_scan` (one names-level question over many files,
//! paged, with a deadline) and `script_compile` (REDkit's wcc_lite on a scratch copy of the scripts). The readers are
//! quen-core's `gamedata`; nothing is written into the game folder or REDkit's. The bundles' tables of contents stay in
//! memory while quen runs, each read again once its bundle's size or write time changes.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use quen_core::detect::{self, Vdf};
use quen_core::gamedata::{self, Filter, Question, bundle, cr2w};
use quen_core::journal::{Journal, sha256_file, sha256_hex};
use quen_core::ledger::Capture;
use quen_core::{boot, wcc};
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::doctor;
use crate::game::{Fields, flag, text};
use crate::ledger::{self, INLINE_BYTES};
use crate::session::{self, Call, Done, JournalFs, Session, Shared};
use crate::win::Job;
use crate::{focus, input};

/// A page's rows when the call gives no `limit`, and the most it takes.
const LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 1000;
/// A scan's `deadline_s` when the call gives none, and the most it takes.
const DEADLINE_S: u64 = 60;
const MAX_DEADLINE_S: u64 = 600;
/// A read's tree: values decoded at most, and how deep, when the call doesn't say.
const MAX_NODES: u64 = 100_000;
const DEPTH: u64 = 32;
/// The largest file data_read decodes.
const READ_MAX: u64 = 256 << 20;
/// A scan's skipped files listed, at most (all are counted).
const SKIPPED_LISTED: usize = 20;
/// script_compile's errors inline, at most (all go to messages.json beside wcc.log).
const ERRORS_INLINE: usize = 100;
/// script_compile's `time_limit_s` when the call gives none, and the most it takes.
const COMPILE_S: u64 = 600;
const MAX_COMPILE_S: u64 = 1800;
/// quen_setup licence: how long REDkit's licence dialog waits for the user's answer, and wcc_lite's run after it closes.
const ANSWER: Duration = Duration::from_secs(600);
const AFTER_ANSWER: Duration = Duration::from_secs(60);
pub(crate) const REDKIT_APPID: &str = "2684660";
pub(crate) const WCC_LITE: &str = r"bin\x64_RedKit\wcc_lite.exe";

pub fn tools() -> Vec<Value> {
    let token = json!({"type": "string", "description": "the live session's token, needed while one is live"});
    let wheres = json!({"type": "string", "enum": ["all", "game", "mods", "redkit"], "description": "all (default): \
        the game's and the mods' bundles; game: content\\content0's (and a root dlc folder's, an older layout); mods: \
        Mods\\*\\content's; redkit: REDkit's editor depot (r4data, loose files)"});
    let list =
        |what: &str| json!({"type": "array", "items": {"type": "string"}, "description": what});
    let kinds = list("extensions: w2ent, w2mi, xbm, ...");
    let pattern = json!({"type": "string", "description": "a case-insensitive substring of the path, or a glob over \
        the whole path where * matches any run of characters"});
    let from = json!({"type": "integer", "minimum": 0, "description": "where the page starts: a previous result's next"});
    vec![
        json!({
            "name": "data_find",
            "description": "Read-only: search the game's data by path. where: all (default: the game's and the mods' \
                bundles), game, mods, or redkit (REDkit's editor depot, r4data). pattern and kinds pick the paths. A \
                bundle hit lists every copy of its path (bundle, bytes, zbytes, crc32, compression), with \
                copies_differ when their crc32s differ: then data_read needs bundle. by_bundle counts the hits per \
                bundle. A page of limit hits (default 100, at most 1000) from from; next goes on. Reads only the \
                bundles' tables of contents (kept in memory while quen runs) or the depot's folders: runs from any \
                connection, beside a live session. provenance: each bundle's table sha256 and the content build \
                (Steam's buildid and depot manifests).",
            "inputSchema": {"type": "object", "properties": {
                "pattern": pattern, "kinds": kinds, "where": wheres,
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "hits per page, default 100"},
                "from": from,
            }, "additionalProperties": false},
        }),
        json!({
            "name": "data_read",
            "description": "Read-only: one file of the game's data, decoded as JSON. path as data_find lists it, where \
                as data_find's (redkit: a depot file); bundle: which copy, needed when copies differ (quen doesn't \
                guess the game's order). A CR2W file: version (version_unknown: a version quen hasn't checked), \
                crc_ok (header, tables, exports_bad: reported, never refused), names, imports, exports, buffers, and \
                tree: each export's properties (handles as {import: path} or {export: index}; a type quen doesn't \
                decode comes back raw, never failing the read) and a CMaterialInstance's params. Another file: its \
                size and first bytes. evidence: a ledger row's fields (method_family files, tag O). Over 16 KB the \
                whole result goes to a JSON file under the capture root (file, truncated). Files over 256 MiB are \
                refused.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string", "description": "the file's path, as data_find lists it"},
                "where": wheres,
                "bundle": {"type": "string", "description": "the copy's bundle, as data_find lists it"},
                "export": {"type": "string", "description": "only this export: its index (0, 1, ...) or class name"},
                "depth": {"type": "integer", "minimum": 1, "maximum": 64, "description": "how deep structs and arrays are decoded, default 32"},
                "max_nodes": {"type": "integer", "minimum": 1, "maximum": 1000000, "description": "values decoded at most, default 100000; then cut: true"},
                "token": token,
            }, "required": ["path"], "additionalProperties": false},
        }),
        json!({
            "name": "data_scan",
            "description": "Read-only: one names-level question over many CR2W files, from each file's name and \
                import tables (a few KB). kinds, pattern and where pick the files as data_find's. has_names: every \
                one in the file's name table (a class, a property, a type; exact case); lacks_names: none; imports: an import \
                whose depot path is one of them or ends in \\ and one (pc_steering.w2steer finds every file importing \
                it). Bundle copies with one crc32 count once. rows: the matching files (bundles, matched imports), at \
                most limit (default 100; 0 counts only). matched and checked count from from to next. stopped: limit, \
                deadline or cancelled; deadline_s default 60, at most 600. skipped: files that couldn't be read, and \
                why. Not between a measurement's settle and its windows.",
            "inputSchema": {"type": "object", "properties": {
                "kinds": kinds, "pattern": pattern, "where": wheres,
                "has_names": list("names the file's name table must hold"),
                "lacks_names": list("names it must not hold"),
                "imports": list("depot paths (or their ends) one of its imports must be"),
                "limit": {"type": "integer", "minimum": 0, "maximum": 1000, "description": "rows per page, default 100; 0: count only"},
                "from": from,
                "deadline_s": {"type": "integer", "minimum": 1, "maximum": 600, "description": "seconds before the page stops, default 60"},
                "token": token,
            }, "additionalProperties": false},
        }),
        json!({
            "name": "script_compile",
            "description": "Runs REDkit's wcc_lite compilescripts on a scratch copy under quen's data root \
                (logs\\<time>-compile): the game's content\\content0\\scripts, each Mods\\*\\content\\scripts over it \
                (a file two mods carry: the first in name order wins, listed in conflicts), then extra. stubs: true \
                turns the mods' import functions into plain stubs (wcc_lite can't bind them), so a clean tree exits 0. \
                wcc_lite rewrites files in the game's documents folder: each *.settings and *.settings.bak there is \
                backed up first and put back after, journaled, so a crash is undone by the replay. Result: \
                exit_code, errors (file, line, msg; file relative to the tree), warning_count, wcc_log (its log, \
                copied). Refused while a session is live, while the game or another wcc_lite runs, without REDkit, \
                and until the user agreed in REDkit's licence dialog (quen_setup licence): needs says which. About \
                25 s on the game's scripts: tell the user first. Counts as a busy machine for the measure tools.",
            "inputSchema": {"type": "object", "properties": {
                "extra": list("mod folders outside the game (absolute paths), each holding content\\scripts, copied over the rest in this order"),
                "stubs": {"type": "boolean", "description": "replace the mods' and extra's import functions with plain stubs; default false"},
                "time_limit_s": {"type": "integer", "minimum": 60, "maximum": 1800, "description": "wcc_lite is killed after this, default 600"},
            }, "additionalProperties": false},
        }),
    ]
}

/// The session-thread data tools.
pub(crate) fn call(session: &mut Session, call: &mut Call, name: &str, args: &Value) -> Done {
    let out = match name {
        "data_read" => read(session, call, args),
        "data_scan" => scan(session, call, args),
        "script_compile" => compile(session, call, args),
        "quen_setup" => licence(session, call, args),
        _ => Err(Done::Refused(format!("unknown tool {name:?}"))),
    };
    out.map_or_else(|done| done, Done::Ok)
}

/// `data_find`, on the connection's own thread: it reads only tables of contents and folder listings.
pub fn find(shared: &Shared, args: &Value) -> Value {
    let t = Instant::now();
    let config = shared.config();
    let done = match config.as_ref() {
        Ok(config) => find_in(config, args).map_or_else(|done| done, Done::Ok),
        Err(why) => Done::Refused(format!("quen's config: {why}")),
    };
    let timing = Map::from_iter([(
        "find".into(),
        json!((t.elapsed().as_secs_f64() * 100.0).round() / 100.0),
    )]);
    session::envelope(shared, "data_find", timing, Vec::new(), Map::new(), done)
}

/// Where a data tool reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Where {
    All,
    Game,
    Mods,
    Redkit,
}

fn where_(args: &Value) -> Result<Where, String> {
    Ok(match args.get("where").map(Value::as_str) {
        None | Some(Some("all")) => Where::All,
        Some(Some("game")) => Where::Game,
        Some(Some("mods")) => Where::Mods,
        Some(Some("redkit")) => Where::Redkit,
        _ => return Err("where is all, game, mods or redkit".into()),
    })
}

fn strings(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(|s| s.as_str().map(String::from)).collect())
            .ok_or(format!("{key} must be an array of strings")),
    }
}

fn number(args: &Value, key: &str, default: u64, range: (u64, u64)) -> Result<u64, String> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (range.0..=range.1).contains(n))
            .ok_or(format!(
                "{key} must be a whole number from {} to {}",
                range.0, range.1
            )),
    }
}

fn filter(args: &Value) -> Result<Filter, String> {
    let pattern = args
        .get("pattern")
        .map(|p| p.as_str().ok_or("pattern must be a string"))
        .transpose()?;
    Ok(Filter::new(&strings(args, "kinds")?, pattern))
}

fn game_dir(config: &Config) -> Result<PathBuf, String> {
    config
        .game_dir
        .clone()
        .ok_or("no game_dir in quen's config: run quen_setup".into())
}

/// REDkit's depot, `<redkit>\r4data`, once REDkit checks out (else refused with `needs`), with its provenance.
fn depot(config: &Config, tool: &str) -> Result<(PathBuf, Value), Done> {
    doctor::require(&doctor::REDKIT, config, &format!("{tool} where: redkit"))?;
    let dir = config.redkit.clone().unwrap_or_default();
    let root = dir.join("r4data");
    if !root.is_dir() {
        return Err(Done::Refused(format!("{} is not a folder", root.display())));
    }
    let content = content(&dir, REDKIT_APPID, std::iter::empty::<&str>());
    Ok((root, json!({"redkit_dir": dir, "content": content})))
}

/// The content build of the Steam app `appid` installed in `dir`: Steam's buildid and depot manifests from the app
/// manifest beside it (when it names that folder), and `key`, the first 16 hex of the sha256 of the manifests (with
/// none, a GOG install say, of `fallback`: the bundles' table hashes).
fn content<'a>(dir: &Path, appid: &str, fallback: impl Iterator<Item = &'a str>) -> Value {
    let manifest = steam_manifest(dir, appid);
    let (buildid, depots) = manifest.clone().unwrap_or_default();
    let fallback: Vec<&str> = fallback.collect();
    let key = match (depots.is_empty(), fallback.is_empty()) {
        (false, _) => Some(sha256_hex(json!(depots).to_string().as_bytes())),
        (true, false) => Some(sha256_hex(fallback.join("\n").as_bytes())),
        (true, true) => None,
    };
    json!({"steam_buildid": buildid, "depot_manifests": depots, "key": key.map(|k| k[..16].to_owned())})
}

/// Steam's buildid and installed depots' manifest ids for `appid` from the `appmanifest_<appid>.acf` of the library
/// holding `dir` (`<library>\steamapps\common\<installdir>`); None when it doesn't name that folder.
fn steam_manifest(dir: &Path, appid: &str) -> Option<(Option<String>, BTreeMap<String, String>)> {
    let apps = dir.parent()?.parent()?;
    let text = fs::read_to_string(apps.join(format!("appmanifest_{appid}.acf"))).ok()?;
    let vdf = Vdf::parse(&text)?;
    let state = vdf.get("AppState")?;
    let installdir = state.get("installdir")?.str()?;
    dir.file_name()?
        .to_str()?
        .eq_ignore_ascii_case(installdir)
        .then_some(())?;
    let buildid = state.get("buildid").and_then(Vdf::str).map(String::from);
    let mut depots = BTreeMap::new();
    if let Some(Vdf::Block(pairs)) = state.get("InstalledDepots") {
        for (id, depot) in pairs {
            if let Some(m) = depot.get("manifest").and_then(Vdf::str) {
                depots.insert(id.clone(), m.to_owned());
            }
        }
    }
    Some((buildid, depots))
}

/// A bundle as a call found it.
struct Opened {
    /// Relative to the game folder, as results name it.
    rel: String,
    path: PathBuf,
    len: u64,
    mtime: Option<SystemTime>,
    toc: Arc<bundle::Toc>,
}

impl Opened {
    fn json(&self) -> Value {
        json!({"bundle": self.rel, "bytes": self.len, "mtime": self.mtime.map(session::utc),
               "toc_sha256": self.toc.sha256, "layout": self.toc.layout})
    }
}

/// The tables of contents read so far, by bundle, with the size and write time they were read at.
type Tocs = BTreeMap<PathBuf, (u64, Option<SystemTime>, Arc<bundle::Toc>)>;
static TOCS: Mutex<Tocs> = Mutex::new(BTreeMap::new());

/// The bundles `w` covers in the game folder `game`, sorted, each with its table of contents (from memory while its
/// size and write time are as read); and those that couldn't be read, with why.
fn bundles(game: &Path, w: Where) -> (Vec<Opened>, Vec<Value>) {
    let mut patterns = Vec::new();
    if matches!(w, Where::All | Where::Game) {
        patterns.extend([
            r"content\content0\bundles\*.bundle",
            r"dlc\*\content\*.bundle",
        ]);
    }
    if matches!(w, Where::All | Where::Mods) {
        patterns.push(r"Mods\*\content\*.bundle");
    }
    let (mut opened, mut skipped) = (Vec::new(), Vec::new());
    for path in patterns
        .into_iter()
        .flat_map(|p| detect::installed(game, p))
    {
        let rel = path
            .strip_prefix(game)
            .unwrap_or(&path)
            .display()
            .to_string();
        let read = fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|meta| {
                let (len, mtime) = (meta.len(), meta.modified().ok());
                let known = TOCS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&path)
                    .cloned();
                let toc = match known {
                    Some((l, m, toc)) if l == len && m == mtime => toc,
                    _ => {
                        let toc = Arc::new(bundle::toc(
                            &mut File::open(&path).map_err(|e| e.to_string())?,
                        )?);
                        let mut all = TOCS.lock().unwrap_or_else(PoisonError::into_inner);
                        all.insert(path.clone(), (len, mtime, toc.clone()));
                        toc
                    }
                };
                Ok(Opened {
                    rel: rel.clone(),
                    path: path.clone(),
                    len,
                    mtime,
                    toc,
                })
            });
        match read {
            Ok(o) => opened.push(o),
            Err(why) => skipped.push(json!({"bundle": rel, "why": why})),
        }
    }
    (opened, skipped)
}

/// The provenance of a read of `opened` (the bundles that held what the call found) in the game folder `game`.
fn provenance(game: &Path, all: &[Opened], used: &[&Opened]) -> Value {
    let content = content(
        game,
        boot::STEAM_APPID,
        all.iter().map(|o| o.toc.sha256.as_str()),
    );
    json!({"game_dir": game, "content": content, "bundles": used.iter().map(|o| o.json()).collect::<Vec<_>>()})
}

/// Every file under the depot `root` that `filter` asks for: (its path relative to `root`, bytes), in key order.
/// Links aren't followed.
fn walk(root: &Path, filter: &Filter) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_owned()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                todo.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if filter.matches(&gamedata::key(&rel)) {
                    out.push((rel, entry.metadata().map_or(0, |m| m.len())));
                }
            }
        }
    }
    out.sort_by_cached_key(|(p, _)| gamedata::key(p));
    out
}

fn find_in(config: &Config, args: &Value) -> Result<Fields, Done> {
    ledger::object(args, &["pattern", "kinds", "where", "limit", "from"])?;
    let (filter, w) = (filter(args)?, where_(args)?);
    let limit = number(args, "limit", LIMIT, (1, MAX_LIMIT))? as usize;
    let from = number(args, "from", 0, (0, u64::MAX))? as usize;
    if w == Where::Redkit {
        let (root, provenance) = depot(config, "data_find")?;
        let files = walk(&root, &filter);
        let hits: Vec<Value> = files
            .iter()
            .skip(from)
            .take(limit)
            .map(|(path, bytes)| json!({"path": path, "bytes": bytes}))
            .collect();
        let next = (from.saturating_add(limit) < files.len()).then_some(from + limit);
        return Ok(Fields::from_iter([
            ("total".into(), json!(files.len())),
            ("hits".into(), json!(hits)),
            ("next".into(), json!(next)),
            ("provenance".into(), provenance),
        ]));
    }
    let game = game_dir(config)?;
    let (opened, skipped) = bundles(&game, w);
    let index = gamedata::index(opened.iter().map(|o| &*o.toc), &filter);
    let mut by_bundle: BTreeMap<usize, usize> = BTreeMap::new();
    for &(b, _) in index.values().flatten() {
        *by_bundle.entry(b).or_default() += 1;
    }
    let hits: Vec<Value> = index
        .values()
        .skip(from)
        .take(limit)
        .map(|copies| {
            let entry = |&(b, e): &(usize, usize)| &opened[b].toc.entries[e];
            let list: Vec<Value> = copies
                .iter()
                .map(|c| {
                    let e = entry(c);
                    json!({"bundle": opened[c.0].rel, "bytes": e.size, "zbytes": e.zsize, "crc32": e.crc32,
                           "compression": e.compression})
                })
                .collect();
            let differ = copies.iter().any(|c| entry(c).crc32 != entry(&copies[0]).crc32);
            json!({"path": entry(&copies[0]).name, "copies": list, "copies_differ": differ})
        })
        .collect();
    let next = (from.saturating_add(limit) < index.len()).then_some(from + limit);
    let counts: Map<String, Value> = by_bundle
        .iter()
        .map(|(&b, &n)| (opened[b].rel.clone(), json!(n)))
        .collect();
    let used: Vec<&Opened> = by_bundle.keys().map(|&b| &opened[b]).collect();
    Ok(Fields::from_iter([
        ("total".into(), json!(index.len())),
        ("hits".into(), json!(hits)),
        ("next".into(), json!(next)),
        ("by_bundle".into(), Value::Object(counts)),
        ("skipped".into(), json!(skipped)),
        ("provenance".into(), provenance(&game, &opened, &used)),
    ]))
}

/// A session-thread data tool's arguments: only `known` and `token`, and while a session is live, its token.
fn checked<'s>(session: &'s Session, args: &Value, known: &[&str]) -> Result<&'s Config, String> {
    let mut all = known.to_vec();
    all.push("token");
    ledger::object(args, &all)?;
    if let Some(live) = &session.live
        && args.get("token").and_then(Value::as_str) != Some(live.token.as_str())
    {
        return Err("a session is live: pass its token (only the agent that launched it reads the game's data \
                    meanwhile)"
            .into());
    }
    session.config()
}

/// A depot path inside the depot `root`: relative, no `..`.
fn inside(root: &Path, path: &str) -> Result<PathBuf, String> {
    let rel = Path::new(path);
    let plain = rel
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    match plain {
        true => Ok(root.join(rel)),
        false => Err(format!(
            "{path:?} is not a depot path: relative, with no .."
        )),
    }
}

fn read(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = ["path", "where", "bundle", "export", "depth", "max_nodes"];
    let config = checked(session, args, &known)?;
    let path = text(args, "path")?;
    let w = where_(args)?;
    let export = args
        .get("export")
        .map(|e| e.as_str().ok_or("export must be a string".to_owned()))
        .transpose()?
        .map(|e| {
            e.parse()
                .map_or_else(|_| cr2w::Export::Class(e.into()), cr2w::Export::Index)
        });
    let opts = cr2w::Opts {
        export,
        depth: number(args, "depth", DEPTH, (1, 64))? as usize,
        max_nodes: number(args, "max_nodes", MAX_NODES, (1, 1_000_000))? as usize,
    };
    let (bytes, mut out, provenance, source) = if w == Where::Redkit {
        if args.get("bundle").is_some() {
            return Err("bundle is for the game's bundles, not where: redkit"
                .to_owned()
                .into());
        }
        let (root, provenance) = depot(config, "data_read")?;
        let file = inside(&root, path)?;
        let meta = fs::metadata(&file).map_err(|_| format!("no {path} in REDkit's depot"))?;
        if !meta.is_file() || meta.len() > READ_MAX {
            return Err(format!("{path}: not a file of at most {READ_MAX} bytes").into());
        }
        let bytes = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let out = Fields::from_iter([
            ("path".into(), json!(path)),
            ("source".into(), json!("redkit")),
            ("bytes".into(), json!(bytes.len())),
            (
                "mtime".into(),
                json!(meta.modified().ok().map(session::utc)),
            ),
        ]);
        (bytes, out, provenance, "redkit".to_owned())
    } else {
        let game = game_dir(config)?;
        let (opened, _) = bundles(&game, w);
        let want = gamedata::key(path);
        let copies: Vec<(&Opened, &bundle::Entry)> = opened
            .iter()
            .flat_map(|o| o.toc.entries.iter().map(move |e| (o, e)))
            .filter(|(_, e)| gamedata::key(&e.name) == want)
            .collect();
        let list: Vec<Value> = copies
            .iter()
            .map(|(o, e)| json!({"bundle": o.rel, "bytes": e.size, "crc32": e.crc32}))
            .collect();
        let picked = match args.get("bundle") {
            Some(b) => {
                let b = gamedata::key(b.as_str().ok_or("bundle must be a string".to_owned())?);
                copies.iter().find(|(o, _)| gamedata::key(&o.rel) == b)
            }
            None if copies.iter().all(|(_, e)| e.crc32 == copies[0].1.crc32) => copies.first(),
            None => {
                let why = "its copies differ (crc32): give bundle, one of copies (quen doesn't guess the game's order)";
                return Err(Done::RefusedWith(
                    why.into(),
                    Fields::from_iter([("copies".into(), json!(list))]),
                ));
            }
        };
        let Some(&(o, e)) = picked else {
            let why = format!(
                "no {path} in the bundles where: {w:?} covers (bundle, if given, among copies)"
            );
            return Err(Done::RefusedWith(
                why,
                Fields::from_iter([("copies".into(), json!(list))]),
            ));
        };
        if u64::from(e.size) > READ_MAX {
            return Err(format!("{path}: {} bytes, over {READ_MAX}", e.size).into());
        }
        let bytes = File::open(&o.path)
            .map_err(|err| err.to_string())
            .and_then(|mut f| bundle::read(&mut f, o.len, e, None))
            .map_err(|why| format!("{}!{}: {why}", o.rel, e.name))?;
        let out = Fields::from_iter([
            ("path".into(), json!(e.name)),
            ("bundle".into(), json!(o.rel)),
            ("bytes".into(), json!(bytes.len())),
            ("zbytes".into(), json!(e.zsize)),
            ("compression".into(), json!(e.compression)),
            ("crc32".into(), json!(e.crc32)),
            ("crc_ok".into(), json!(bundle::crc_ok(e, &bytes))),
            ("copies".into(), json!(list)),
        ]);
        let provenance = provenance(&game, &opened, &[o]);
        (bytes, out, provenance, o.rel.clone())
    };
    let sha = sha256_hex(&bytes);
    out.insert("sha256".into(), json!(sha));
    match cr2w::read(&bytes, &opts) {
        Ok(mut v) => {
            let tree = v
                .as_object_mut()
                .and_then(|m| m.remove("tree"))
                .unwrap_or_default();
            out.insert("cr2w".into(), v);
            out.insert("tree".into(), tree);
        }
        Err(why) => {
            let mut v = cr2w::not_cr2w(&bytes);
            v["why"] = json!(why);
            out.insert("not_cr2w".into(), v);
        }
    }
    let key = provenance["content"]["key"]
        .as_str()
        .unwrap_or("none")
        .to_owned();
    let note = format!(
        "{source}!{} sha256={sha} content={key}",
        out["path"].as_str().unwrap_or(path)
    );
    out.insert(
        "evidence".into(),
        json!({"method_family": "files", "tag": "O", "note": note}),
    );
    out.insert("provenance".into(), provenance);
    spill(session, call, config, "data_read", args, out, "tree")
}

/// A scan's unit: one path's copies that share a crc32 (a bundle source), or a depot file.
enum Candidate {
    Bundle { copies: Vec<(usize, usize)> },
    Depot { rel: String },
}

fn scan(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let known = [
        "kinds",
        "pattern",
        "where",
        "has_names",
        "lacks_names",
        "imports",
        "limit",
        "from",
        "deadline_s",
    ];
    let config = checked(session, args, &known)?;
    let question = Question {
        has_names: strings(args, "has_names")?,
        lacks_names: strings(args, "lacks_names")?,
        imports: strings(args, "imports")?,
    };
    if question.is_empty() {
        return Err(
            "give has_names, lacks_names or imports: the question each file is tested on"
                .to_owned()
                .into(),
        );
    }
    let (filter, w) = (filter(args)?, where_(args)?);
    let limit = number(args, "limit", LIMIT, (0, MAX_LIMIT))? as usize;
    let from = number(args, "from", 0, (0, u64::MAX))? as usize;
    let deadline = Instant::now()
        + Duration::from_secs(number(args, "deadline_s", DEADLINE_S, (1, MAX_DEADLINE_S))?);
    let (candidates, opened, root, prov, bundle_skips) = if w == Where::Redkit {
        let (root, prov) = depot(config, "data_scan")?;
        let files = walk(&root, &filter)
            .into_iter()
            .map(|(rel, _)| Candidate::Depot { rel });
        (files.collect(), Vec::new(), root, prov, Vec::new())
    } else {
        let game = game_dir(config)?;
        let (opened, skips) = bundles(&game, w);
        let index = gamedata::index(opened.iter().map(|o| &*o.toc), &filter);
        let all: Vec<&Opened> = opened.iter().collect();
        let prov = provenance(&game, &opened, &all);
        let mut out = Vec::new();
        for copies in index.into_values() {
            let mut groups: Vec<Vec<(usize, usize)>> = Vec::new();
            for c in copies {
                let crc = |c: &(usize, usize)| opened[c.0].toc.entries[c.1].crc32;
                match groups.iter_mut().find(|g| crc(&g[0]) == crc(&c)) {
                    Some(g) => g.push(c),
                    None => groups.push(vec![c]),
                }
            }
            out.extend(
                groups
                    .into_iter()
                    .map(|copies| Candidate::Bundle { copies }),
            );
        }
        (out, opened, PathBuf::new(), prov, skips)
    };
    let mut files: BTreeMap<usize, File> = BTreeMap::new();
    // At least the first `want` bytes of a candidate, or why not.
    let mut prefix = |c: &Candidate, want: usize| -> Result<Vec<u8>, String> {
        match c {
            Candidate::Bundle { copies } => {
                let (b, e) = copies[0];
                let o = &opened[b];
                let f = match files.entry(b) {
                    std::collections::btree_map::Entry::Occupied(f) => f.into_mut(),
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert(File::open(&o.path).map_err(|e| e.to_string())?)
                    }
                };
                bundle::read(f, o.len, &o.toc.entries[e], Some(want))
            }
            Candidate::Depot { rel } => {
                let mut out = Vec::new();
                File::open(root.join(rel))
                    .and_then(|f| f.take(want as u64).read_to_end(&mut out))
                    .map_err(|e| e.to_string())?;
                Ok(out)
            }
        }
    };
    let (mut rows, mut checked_n, mut matched, mut not_cr2w, mut decoded) =
        (Vec::new(), 0, 0, 0, 0usize);
    let (mut skipped, mut skipped_n, mut stopped, mut next) = (Vec::new(), 0, None, None);
    for (i, c) in candidates.iter().enumerate().skip(from) {
        if call.stopped() {
            (stopped, next) = (Some("cancelled"), Some(i));
            break;
        }
        if Instant::now() >= deadline {
            (stopped, next) = (Some("deadline"), Some(i));
            break;
        }
        if (i - from).is_multiple_of(500) {
            call.progress(&format!(
                "{} of {} files checked, {matched} matched",
                i - from,
                candidates.len() - from
            ));
        }
        let (path, bundles) = match c {
            Candidate::Bundle { copies } => {
                let names: Vec<&str> = copies
                    .iter()
                    .map(|&(b, _)| opened[b].rel.as_str())
                    .collect();
                (
                    opened[copies[0].0].toc.entries[copies[0].1].name.as_str(),
                    names,
                )
            }
            Candidate::Depot { rel } => (rel.as_str(), Vec::new()),
        };
        let names = prefix(c, cr2w::HEADER).and_then(|head| {
            decoded += head.len();
            let Some(end) = cr2w::names_end(&head) else {
                return Ok(None);
            };
            let all = if end > head.len() {
                prefix(c, end)?
            } else {
                head
            };
            decoded += all.len();
            cr2w::names(&all).map(Some)
        });
        checked_n += 1;
        let names = match names {
            Ok(Some(n)) => n,
            Ok(None) => {
                not_cr2w += 1;
                continue;
            }
            Err(why) => {
                skipped_n += 1;
                if skipped.len() < SKIPPED_LISTED {
                    skipped.push(json!({"path": path, "bundles": bundles, "why": why}));
                }
                continue;
            }
        };
        let Some(hits) = question.test(&names) else {
            continue;
        };
        matched += 1;
        if rows.len() < limit {
            let mut row = json!({"path": path, "version": names.version});
            if !bundles.is_empty() {
                row["bundles"] = json!(bundles);
            }
            if !hits.is_empty() {
                row["imports"] = json!(hits);
            }
            rows.push(row);
            if rows.len() == limit {
                (stopped, next) = (Some("limit"), (i + 1 < candidates.len()).then_some(i + 1));
                break;
            }
        }
    }
    let key = prov["content"]["key"].as_str().unwrap_or("none").to_owned();
    let to = next.map_or("the end".to_owned(), |n| n.to_string());
    let note = format!(
        "data_scan where={w:?} kinds={:?} pattern={:?} has_names={:?} lacks_names={:?} imports={:?}: {matched} of \
         {checked_n} files checked from {from} to {to} match, content={key}",
        strings(args, "kinds")?,
        args.get("pattern").and_then(Value::as_str).unwrap_or(""),
        question.has_names,
        question.lacks_names,
        question.imports,
    );
    let out = Fields::from_iter([
        ("candidates".into(), json!(candidates.len())),
        ("from".into(), json!(from)),
        ("next".into(), json!(next)),
        ("stopped".into(), json!(stopped)),
        ("checked".into(), json!(checked_n)),
        ("matched".into(), json!(matched)),
        ("not_cr2w".into(), json!(not_cr2w)),
        ("skipped_count".into(), json!(skipped_n)),
        ("skipped".into(), json!(skipped)),
        ("bundles_skipped".into(), json!(bundle_skips)),
        ("bytes_decoded".into(), json!(decoded)),
        ("rows".into(), json!(rows)),
        (
            "evidence".into(),
            json!({"method_family": "files", "tag": "O", "note": note}),
        ),
        ("provenance".into(), prov),
    ]);
    spill(session, call, config, "data_scan", args, out, "rows")
}

/// A result over [`INLINE_BYTES`] goes whole to `<tool>-<secs>-<random>.json`, in the live session's capture folder,
/// else in `<capture root>\data\reads`, with a captures row (none while another quen holds the lock: `captures_row`
/// says why); inline only the first of its `list` that fit (`truncated`, `file`).
fn spill(
    session: &Session,
    call: &mut Call,
    config: &Config,
    tool: &str,
    args: &Value,
    mut out: Fields,
    list: &str,
) -> Result<Fields, Done> {
    if Value::Object(out.clone()).to_string().len() <= INLINE_BYTES {
        return Ok(out);
    }
    let mut args = args.clone();
    if let Some(args) = args.as_object_mut() {
        args.remove("token");
    }
    let mut data = Value::Object(out.clone());
    data["header"] =
        json!({"tool": tool, "args": args, "boot": session.live.as_ref().map(|l| l.boot)});
    let rel = match &session.live {
        Some(live) => PathBuf::from(&live.id),
        None => Path::new("data").join("reads"),
    };
    let mut listed = Vec::new();
    let file = ledger::spill(
        &config.capture_root().join(&rel),
        &format!("{tool}-"),
        &data,
        &mut listed,
    )?;
    call.artifact(&file);
    if let Some(Value::Array(items)) = out.get_mut(list) {
        let mut size = 0;
        let fit = items.iter().take_while(|item| {
            size += item.to_string().len() + 1;
            size <= INLINE_BYTES / 2
        });
        let fit = fit.count();
        items.truncate(fit);
    }
    out.insert("truncated".into(), json!(true));
    out.insert("file".into(), json!(file));
    if session.live.is_none() && session::locked_elsewhere(config) {
        out.insert(
            "captures_row".into(),
            json!("none: another quen holds the game lock"),
        );
        return Ok(out);
    }
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    // A data root where no session ever ran has no state folder, where the ledger lives.
    let _ = fs::create_dir_all(config.state_dir());
    let row = Capture {
        boot_id: session.live.as_ref().map(|l| l.boot),
        path: format!("{}/{name}", rel.display()).replace('\\', "/"),
        sha256: listed[0]["sha256"].as_str().unwrap_or_default().into(),
        bytes: fs::metadata(&file).map_or(0, |m| m.len() as i64),
        kind: "json".into(),
        method: Some(tool.into()),
        t: session::utc(SystemTime::now()),
        ..Capture::default()
    };
    let added = session::open_ledger(config)
        .and_then(|mut l| l.add_capture(&row).map_err(session::ledger_error));
    match added {
        Ok(id) => {
            call.row("captures", id);
            Ok(out)
        }
        Err(why) => Err(Done::Failed(
            format!(
                "{}: written, but no captures row was added: {why}",
                file.display()
            ),
            out,
        )),
    }
}

/// One source of the compile's tree: a folder copied over what is there, its files counted, and whether its
/// import functions were stubbed.
struct Source {
    name: String,
    scripts: PathBuf,
    stub: bool,
}

/// Copies every file under `from` into `to` (folders made as needed), each `.ws` with its import functions stubbed
/// when `stub`; the relative paths copied, and how many imports were stubbed.
fn copy_scripts(from: &Path, to: &Path, stub: bool) -> io::Result<(Vec<String>, usize)> {
    let (mut copied, mut stubbed) = (Vec::new(), 0);
    let mut todo = vec![from.to_owned()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let (kind, path) = (entry.file_type()?, entry.path());
            if kind.is_dir() {
                todo.push(path);
                continue;
            }
            let rel = path.strip_prefix(from).unwrap_or(&path).to_owned();
            let dest = to.join(&rel);
            fs::create_dir_all(dest.parent().unwrap_or(to))?;
            let bytes = fs::read(&path)?;
            let is_ws = rel
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ws"));
            let out = match (stub && is_ws, bytes.strip_prefix(&[0xFF, 0xFE])) {
                (false, _) => bytes,
                // UTF-16LE with its BOM, as some script files are.
                (true, Some(utf16)) => {
                    let units: Vec<u16> = utf16
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    let (text, n) = wcc::stub_imports(&String::from_utf16_lossy(&units));
                    stubbed += n;
                    [0xFF, 0xFE]
                        .into_iter()
                        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                        .collect()
                }
                (true, None) => match String::from_utf8(bytes) {
                    Ok(text) => {
                        let (text, n) = wcc::stub_imports(&text);
                        stubbed += n;
                        text.into_bytes()
                    }
                    Err(e) => e.into_bytes(),
                },
            };
            fs::write(&dest, out)?;
            copied.push(rel.display().to_string());
        }
    }
    Ok((copied, stubbed))
}

fn compile(session: &mut Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    ledger::object(args, &["extra", "stubs", "time_limit_s"])?;
    let config = session.config()?.clone();
    let stubs = flag(args, "stubs")?;
    let limit = Duration::from_secs(number(
        args,
        "time_limit_s",
        COMPILE_S,
        (60, MAX_COMPILE_S),
    )?);
    let extra: Vec<PathBuf> = strings(args, "extra")?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if let Some(bad) = extra
        .iter()
        .find(|p| !p.is_absolute() || !p.join(r"content\scripts").is_dir())
    {
        return Err(format!(
            "extra {}: not an absolute path to a mod folder holding content\\scripts",
            bad.display()
        )
        .into());
    }
    let (exe, docs) = wcc_ready(session, &config, "script_compile")?;
    doctor::redkit_licence_accepted("script_compile")?;
    let game = game_dir(&config)?;
    let scratch = config
        .data_root
        .join("logs")
        .join(format!("{}-compile", session::now()));
    if scratch.display().to_string().contains(' ') {
        return Err(format!(
            "{}: wcc_lite can't take a path with a space (set data_root to one without)",
            scratch.display()
        )
        .into());
    }
    let mut mods: Vec<Source> = fs::read_dir(game.join("Mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().join(r"content\scripts"),
            )
        })
        .filter(|(_, scripts)| scripts.is_dir())
        .map(|(name, scripts)| Source {
            name,
            scripts,
            stub: stubs,
        })
        .collect();
    mods.sort_by_key(|m| m.name.to_lowercase());
    // The first in name order wins a file two mods carry: copied last.
    let mut sources = vec![Source {
        name: r"content\content0\scripts".into(),
        scripts: game.join(r"content\content0\scripts"),
        stub: false,
    }];
    sources.extend(mods.into_iter().rev());
    sources.extend(extra.iter().map(|p| Source {
        name: p.display().to_string(),
        scripts: p.join(r"content\scripts"),
        stub: stubs,
    }));
    let (tree, out_dir) = (scratch.join("scripts"), scratch.join("out"));
    let mut copied = Vec::new();
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, s) in sources.iter().enumerate() {
        let (files, stubbed) = copy_scripts(&s.scripts, &tree, s.stub).map_err(|e| {
            Done::Failed(format!("copying {}: {e}", s.scripts.display()), Map::new())
        })?;
        if k > 0 && k < sources.len() - extra.len() {
            for f in &files {
                by_file
                    .entry(gamedata::key(f))
                    .or_default()
                    .push(s.name.clone());
            }
        }
        copied.push(json!({"source": s.name, "files": files.len(), "imports_stubbed": stubbed}));
    }
    let conflicts: Vec<Value> = by_file
        .into_iter()
        .filter(|(_, mods)| mods.len() > 1)
        .map(|(file, mods)| json!({"file": file, "used": mods.last(), "mods": mods}))
        .collect();
    fs::create_dir_all(&out_dir)
        .map_err(|e| Done::Failed(format!("{}: {e}", out_dir.display()), Map::new()))?;
    call.lap("copy");
    let mut fields = Fields::new();
    fields.insert("scratch".into(), json!(scratch));
    fields.insert("sources".into(), json!(copied));
    fields.insert("conflicts".into(), json!(conflicts));
    let argv = [
        exe.display().to_string(),
        "compilescripts".into(),
        tree.display().to_string(),
        format!("-out={}", out_dir.display()),
        "-agreetoterms".into(),
        "-noninteractivecrash".into(),
    ];
    let started = SystemTime::now();
    let (code, log) = run_journaled(
        session,
        call,
        &docs,
        &scratch,
        &argv,
        &mut fields,
        |_, t| (t > limit).then(|| "it ran past time_limit_s".into()),
    )?;
    fields.insert("exit_code".into(), json!(code));
    fields.insert("log".into(), json!(log));
    let bin = exe.parent().and_then(Path::parent).unwrap_or(&exe);
    let (log_in, copy) = (bin.join("wcc.log"), scratch.join("wcc.log"));
    // An earlier run's log isn't this one's.
    let fresh = fs::metadata(&log_in)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t >= started);
    let text = match fresh {
        true => fs::copy(&log_in, &copy).and_then(|_| fs::read(&copy)),
        false => Err(io::Error::other("this run didn't write it")),
    }
    .map(|b| String::from_utf8_lossy(&b).into_owned());
    match text {
        Ok(text) => {
            call.artifact(&copy);
            let (errors, warnings) = wcc::messages(&text);
            fields.insert("wcc_log".into(), json!(copy));
            fields.insert("error_count".into(), json!(errors.len()));
            fields.insert("warning_count".into(), json!(warnings.len()));
            if errors.len() > ERRORS_INLINE || !warnings.is_empty() {
                let all = scratch.join("messages.json");
                let written = fs::write(
                    &all,
                    json!({"errors": errors, "warnings": warnings}).to_string(),
                );
                if written.is_ok() {
                    call.artifact(&all);
                    fields.insert("messages".into(), json!(all));
                }
            }
            fields.insert(
                "errors".into(),
                json!(errors.iter().take(ERRORS_INLINE).collect::<Vec<_>>()),
            );
        }
        Err(e) => {
            fields.insert("wcc_log".into(), json!(format!("not copied: {e}")));
        }
    }
    Ok(fields)
}

/// What every wcc_lite run needs, checked before anything changes (`tool` names the caller in needs): REDkit, the
/// documents folder in the config, no live session, no journal left, neither the game nor another wcc_lite running.
/// wcc_lite's exe and the documents folder.
fn wcc_ready(session: &Session, config: &Config, tool: &str) -> Result<(PathBuf, PathBuf), Done> {
    let exe = doctor::require(&doctor::REDKIT, config, tool)?
        .path
        .unwrap_or_default();
    let docs = config
        .documents
        .clone()
        .ok_or("no documents in quen's config: run quen_setup".to_owned())?;
    if session.live.is_some() {
        return Err(format!(
            "a session is live, and its journal owns the game's documents folder, which wcc_lite writes: {tool} \
             after session_end"
        )
        .into());
    }
    let journal = session::journal_path(config);
    if journal.exists() {
        return Err(format!("{} is left: session_recover first", journal.display()).into());
    }
    // Any wcc_lite on the PC, not only REDkit's: every run writes the same documents folder. `QUEN_TEST_WCC_IMAGE`
    // (tests only) names another image, so the user's own REDkit runs don't fail the tests (finding 78).
    let wcc = std::env::var("QUEN_TEST_WCC_IMAGE").unwrap_or_else(|_| "wcc_lite.exe".into());
    for (image, why) in [
        (
            config.game_exe.as_str(),
            "the game runs: it writes the documents folder too",
        ),
        (wcc.as_str(), "a wcc_lite runs already: one at a time"),
    ] {
        if !session::outside(image, None)?.is_empty() {
            return Err(format!("{image}: {why}").into());
        }
    }
    Ok((exe, docs))
}

/// wcc_lite (`argv`, from REDkit's bin folder) under the game lock, the documents folder's settings backed up into
/// `scratch` and journaled first, then put back by the session tools' replay, whatever happened. Killed when the call
/// is cancelled or `stop` (the job, the time since the start) gives a reason. `fields` gains the replay's (settings).
/// Its exit code and output log; Failed when it didn't run to its end or the settings weren't all put back.
fn run_journaled(
    session: &mut Session,
    call: &mut Call,
    docs: &Path,
    scratch: &Path,
    argv: &[String],
    fields: &mut Fields,
    mut stop: impl FnMut(&Job, Duration) -> Option<String>,
) -> Result<(u32, PathBuf), Done> {
    let config = session.config()?.clone();
    let backup = scratch.join("backup");
    fs::create_dir_all(&backup)
        .map_err(|e| Done::Failed(format!("{}: {e}", backup.display()), Map::new()))?;
    // The documents folder's settings, journaled under the lock before wcc_lite can write them.
    let listing: Vec<(String, String)> = fs::read_dir(docs)
        .map_err(|e| format!("{}: {e}", docs.display()))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            Some((
                e.file_name().to_string_lossy().into_owned(),
                sha256_file(&e.path()).ok()?,
            ))
        })
        .collect();
    let changes = wcc::settings_changes(docs, &listing, &backup);
    session.take_lock(&config, "wcc_lite")?;
    let ran = run_wcc(call, &config, &changes, argv, scratch, &mut stop);
    // Put back, whatever happened: the replay the session tools use.
    let replay = match session::journal_path(&config).exists() {
        true => session.unwind(call, &scratch.join("journal.jsonl"), None),
        false => Ok(Map::new()),
    };
    session.release_lock();
    let (replay_ok, replay_fields) = match replay {
        Ok(f) => (None, f),
        Err((why, f)) => (Some(why), f),
    };
    fields.insert("settings".into(), json!(replay_fields));
    let ran = ran.map_err(|why| Done::Failed(why, fields.clone()))?;
    match replay_ok {
        Some(why) => Err(Done::Failed(
            format!("wcc_lite ran, but the settings weren't all put back: {why}"),
            fields.clone(),
        )),
        None => Ok(ran),
    }
}

/// Journals the settings `changes`, runs wcc_lite in a job of its own (killed when the call is cancelled or `stop`
/// says why) and returns its exit code and output log. The caller replays the journal either way.
fn run_wcc(
    call: &mut Call,
    config: &Config,
    changes: &[boot::Change],
    argv: &[String],
    scratch: &Path,
    stop: &mut impl FnMut(&Job, Duration) -> Option<String>,
) -> Result<(u32, PathBuf), String> {
    let path = session::journal_path(config);
    let mut journal =
        Journal::create(&JournalFs, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    boot::apply(&JournalFs, &mut journal, changes, &|| call.stopped())
        .map_err(|e| format!("backing up the documents folder's settings: {e}"))?;
    call.lap("journal");
    let log = scratch.join("wcc_lite.log");
    let exe = Path::new(&argv[0]);
    let job = Job::new().map_err(|e| format!("wcc_lite's job: {e}"))?;
    let child = File::create(&log)
        .and_then(|f| job.spawn(argv, &[], exe.parent().unwrap_or(exe), &f))
        .map_err(|e| format!("wcc_lite did not start: {e}"))?;
    let t = Instant::now();
    let code = loop {
        match child.wait(Duration::from_millis(250)) {
            Ok(Some(code)) => break code,
            Ok(None) => {
                let why = match call.stopped() {
                    true => Some(call.stop_cause().to_owned()),
                    false => stop(&job, t.elapsed()),
                };
                if let Some(why) = why {
                    let _ = job.kill();
                    return Err(format!("wcc_lite killed: {why}"));
                }
                call.progress(&format!("wcc_lite running, {} s", t.elapsed().as_secs()));
            }
            Err(e) => {
                let _ = job.kill();
                return Err(format!("waiting for wcc_lite: {e}"));
            }
        }
    };
    call.artifact(&log);
    call.lap("wcc_lite");
    Ok((code, log))
}

/// `quen_setup` with `licence`: wcc_lite once without -agreetoterms and with no command, so REDkit's own licence dialog
/// shows, brought to the front, for the user to answer; quen never clicks it. After Agree, wcc_lite with no command
/// lists its commands and exits (its own unknown-command message says so; not run here yet: a live check); after
/// Decline it exits. Then DoNotShowEula is read again: quen never writes it. The same refusals as script_compile but
/// the licence's.
fn licence(session: &mut Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    ledger::object(args, &["licence"])?;
    if args["licence"] != true {
        return Err("licence must be true".to_string().into());
    }
    let config = session.config()?.clone();
    let (exe, docs) = wcc_ready(session, &config, "quen_setup licence")?;
    let scratch = config
        .data_root
        .join("logs")
        .join(format!("{}-licence", session::now()));
    let mut fields = Fields::from_iter([("scratch".into(), json!(scratch))]);
    let (mut shown, mut front, mut closed) = (false, None, None);
    let argv = [exe.display().to_string()];
    let ran = run_journaled(
        session,
        call,
        &docs,
        &scratch,
        &argv,
        &mut fields,
        |job, t| {
            let pids = job.pids().unwrap_or_default();
            match pids.into_iter().find_map(|pid| focus::window_of(pid).ok()?) {
                Some(dialog) if !shown => {
                    shown = true;
                    front = Some(input::focus(dialog).is_ok());
                }
                None if shown && closed.is_none() => closed = Some(t),
                _ => {}
            }
            match closed {
                Some(at) if t > at + AFTER_ANSWER => Some(format!(
                    "it ran on {} s after its dialog closed",
                    AFTER_ANSWER.as_secs()
                )),
                None if t > ANSWER => Some(format!(
                    "no answer in REDkit's licence dialog within {} s",
                    ANSWER.as_secs()
                )),
                _ => None,
            }
        },
    );
    let answer = doctor::redkit_licence();
    let with_answer = |mut f: Fields| {
        f.insert("dialog_shown".into(), json!(shown));
        f.insert("dialog_in_front".into(), json!(front));
        f.insert("do_not_show_eula".into(), json!(answer));
        f.insert("accepted".into(), json!(answer == Some(1)));
        f
    };
    let (code, log) = ran.map_err(|done| match done {
        Done::Failed(why, f) => Done::Failed(why, with_answer(f)),
        done => done,
    })?;
    let mut fields = with_answer(fields);
    fields.insert("exit_code".into(), json!(code));
    fields.insert("log".into(), json!(log));
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::Conn;

    /// A synthetic game, documents and REDkit folder for one test, gone afterwards (only what the test made in it).
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = std::env::temp_dir().join(format!("quen-data-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn put(&self, rel: &str, data: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, data).unwrap();
            path
        }

        fn config(&self) -> Config {
            Config {
                data_root: self.0.join("data"),
                game_dir: Some(self.0.join("game")),
                documents: Some(self.0.join("docs")),
                game_exe: "quen-test-no-such-game.exe".into(),
                ..Config::default()
            }
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    /// A bundle (the 304-byte layout) of raw entries.
    fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
        let toc = 304 * files.len();
        let mut out = b"POTATO70".to_vec();
        for w in [0u32, 0, toc as u32, 5, 0, 0] {
            out.extend(w.to_le_bytes());
        }
        let mut data: Vec<u8> = Vec::new();
        for (name, bytes) in files {
            let mut e = vec![0u8; 304];
            e[..name.len()].copy_from_slice(name.as_bytes());
            e[272..280].copy_from_slice(&((32 + toc + data.len()) as u64).to_le_bytes());
            for (at, v) in [
                (280, bytes.len() as u32),
                (284, bytes.len() as u32),
                (288, crc32fast::hash(bytes)),
            ] {
                e[at..at + 4].copy_from_slice(&v.to_le_bytes());
            }
            out.extend(e);
            data.extend(*bytes);
        }
        out.extend(data);
        out
    }

    /// A CR2W file with names and imports and no export: what a names-level scan reads.
    fn cr2w(names: &[&str], imports: &[&str]) -> Vec<u8> {
        let (mut strings, mut rows, mut imps) = (Vec::new(), Vec::new(), Vec::new());
        for (list, items) in [(&mut rows, names), (&mut imps, imports)] {
            for s in items {
                list.extend((strings.len() as u32).to_le_bytes());
                list.extend([0u8; 4]);
                strings.extend(s.as_bytes());
                strings.push(0);
            }
        }
        let mut out = b"CR2W".to_vec();
        out.extend(164u32.to_le_bytes());
        out.resize(160, 0);
        let mut at = 160;
        let tables = [
            (&strings, strings.len()),
            (&rows, names.len()),
            (&imps, imports.len()),
        ];
        for (i, (table, count)) in tables.into_iter().enumerate() {
            for (k, v) in [at as u32, count as u32, crc32fast::hash(table)]
                .into_iter()
                .enumerate()
            {
                let o = 40 + 12 * i + 4 * k;
                out[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
            at += table.len();
        }
        out.extend([strings, rows, imps].concat());
        out
    }

    fn run(config: &Config, tool: &str, args: Value) -> Value {
        let shared = Shared::new(Ok(config.clone()));
        let mut session = Session::new(&shared, None);
        let mut c = session.call(Conn::nobody(), Value::Null, None);
        let done = call(&mut session, &mut c, tool, &args);
        session.envelope(tool, c, done)
    }

    /// A game bundle and a mod's both carry a material, with other bytes: find lists both copies, copies_differ; a
    /// read without bundle is refused naming them, with bundle it reads that copy; a file that isn't CR2W gives its
    /// first bytes; where: redkit needs REDkit.
    #[test]
    fn find_lists_every_copy_and_a_read_picks_one() {
        let t = Temp::new("find");
        let vanilla = cr2w(&["", "CMaterialInstance"], &[r"base\g.w2mg"]);
        let modded = cr2w(&["", "CMaterialInstance", "Roughness_max"], &[]);
        t.put(
            r"game\content\content0\bundles\blob.bundle",
            bundle(&[(r"a\X.w2mi", &vanilla), (r"b\y.xbm", b"DDS raw bytes")]),
        );
        t.put(
            r"game\Mods\modA\content\blob0.bundle",
            bundle(&[(r"A\x.w2mi", &modded)]),
        );
        let config = t.config();
        let shared = Shared::new(Ok(config.clone()));
        let found = find(&shared, &json!({"pattern": "x.W2MI"}));
        assert_eq!(found["ok"], true, "{found}");
        assert_eq!(found["total"], 1);
        let hit = &found["hits"][0];
        assert_eq!(hit["path"], r"a\X.w2mi");
        assert_eq!(hit["copies"].as_array().unwrap().len(), 2);
        assert_eq!(hit["copies_differ"], true);
        assert_eq!(
            found["provenance"]["bundles"][0]["toc_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        let mods = find(&shared, &json!({"where": "mods"}));
        assert_eq!(mods["total"], 1);
        assert_eq!(mods["hits"][0]["copies"].as_array().unwrap().len(), 1);
        assert_eq!(find(&shared, &json!({"kinds": ["xbm"]}))["total"], 1);
        let page = find(&shared, &json!({"limit": 1}));
        assert_eq!((&page["total"], &page["next"]), (&json!(2), &json!(1)));
        let redkit = find(&shared, &json!({"where": "redkit"}));
        assert_eq!(
            redkit["needs"][0]["dependency"],
            doctor::REDKIT.name,
            "{redkit}"
        );

        let refused = run(&config, "data_read", json!({"path": "a/x.w2mi"}));
        assert_eq!(refused["refused"], true, "{refused}");
        assert_eq!(refused["copies"].as_array().unwrap().len(), 2);
        let bundle = r"Mods\modA\content\blob0.bundle";
        let read = run(
            &config,
            "data_read",
            json!({"path": "a/x.w2mi", "bundle": bundle}),
        );
        assert_eq!(read["ok"], true, "{read}");
        assert_eq!(read["cr2w"]["version"], 164);
        assert_eq!(read["crc_ok"], true);
        assert!(
            read["cr2w"]["names"]
                .as_array()
                .unwrap()
                .contains(&json!("Roughness_max"))
        );
        let note = read["evidence"]["note"].as_str().unwrap();
        let want = format!(r"{bundle}!A\x.w2mi sha256={}", sha256_hex(&modded));
        assert!(note.starts_with(&want), "{note}");
        let raw = run(&config, "data_read", json!({"path": r"b\y.xbm"}));
        assert_eq!(
            raw["not_cr2w"]["first_bytes"], "44445320726177206279746573",
            "{raw}"
        );
        assert_eq!(
            run(&config, "data_read", json!({"path": "nope"}))["refused"],
            true
        );
    }

    /// A refs scan pages by limit and goes on from next with no file twice; identical copies count once; limit 0
    /// counts only; a scan without a question is refused.
    #[test]
    fn scans_page_and_count() {
        let t = Temp::new("scan");
        let steer = r"gameplay\behaviors\pc\steering\pc_steering.w2steer";
        let a = cr2w(&["", "CEntityTemplate"], &[steer]);
        let files = [
            (r"t\a.w2ent", a.clone()),
            (r"t\b.w2ent", cr2w(&["", "CEntityTemplate"], &[])),
            (
                r"t\c.w2ent",
                cr2w(&["", "CMoveSTSeparateFromActors"], &[steer]),
            ),
            (r"t\d.w2ent", b"not a cr2w".to_vec()),
        ];
        let entries: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
        t.put(
            r"game\content\content0\bundles\blob.bundle",
            bundle(&entries),
        );
        t.put(
            r"game\content\content0\bundles\world.bundle",
            bundle(&[(r"T\A.w2ent", &a)]),
        );
        let config = t.config();
        let q = |extra: Value| {
            let mut args = json!({"imports": ["PC_STEERING.w2steer"], "kinds": ["w2ent"]});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            run(&config, "data_scan", args)
        };
        let first = q(json!({"limit": 1}));
        assert_eq!(first["ok"], true, "{first}");
        assert_eq!(first["rows"].as_array().unwrap().len(), 1);
        assert_eq!(first["rows"][0]["path"], r"t\a.w2ent");
        assert_eq!(first["rows"][0]["bundles"].as_array().unwrap().len(), 2);
        assert_eq!(first["rows"][0]["imports"], json!([steer]));
        assert_eq!(
            (&first["stopped"], &first["next"]),
            (&json!("limit"), &json!(1))
        );
        let rest = q(json!({"from": 1}));
        assert_eq!(rest["rows"].as_array().unwrap().len(), 1);
        assert_eq!(rest["rows"][0]["path"], r"t\c.w2ent");
        assert_eq!(
            (&rest["stopped"], &rest["next"]),
            (&Value::Null, &Value::Null)
        );
        let count = q(json!({"limit": 0}));
        assert_eq!(count["rows"], json!([]));
        assert_eq!(
            (&count["matched"], &count["checked"]),
            (&json!(2), &json!(4))
        );
        assert_eq!(count["not_cr2w"], 1);
        assert!(
            count["evidence"]["note"]
                .as_str()
                .unwrap()
                .contains("2 of 4 files")
        );
        let names = run(
            &config,
            "data_scan",
            json!({"has_names": ["CMoveSTSeparateFromActors"]}),
        );
        assert_eq!(names["matched"], 1, "{names}");
        assert_eq!(
            run(&config, "data_scan", json!({"kinds": ["w2ent"]}))["refused"],
            true
        );
    }

    /// REDkit's depot: find walks it, and a refs scan reads both CR2W headers, the older one whole (once a capacity of
    /// usize::MAX that aborted the daemon).
    #[test]
    fn the_depot_is_found_and_scanned_in_both_layouts() {
        let t = Temp::new("depot");
        t.put(
            r"redkit\bin\x64_RedKit\wcc_lite.exe",
            "a stand-in: only its presence is checked",
        );
        let steer = r"gameplay\pc_steering.w2steer";
        t.put(
            r"redkit\r4data\t\new.w2ent",
            cr2w(&["", "CEntityTemplate"], &[steer]),
        );
        let s = |t: &str| [&[0x80 | t.len() as u8][..], t.as_bytes()].concat();
        let (table, deps) = (s("CEntityTemplate"), s(steer));
        let mut old = b"CR2W".to_vec();
        let d0 = 44 + table.len() as u32;
        for w in [141, 0, 44, 1, d0 + deps.len() as u32, 0, d0, 0, d0, 2] {
            old.extend(w.to_le_bytes());
        }
        old.extend([table, deps].concat());
        t.put(r"redkit\r4data\t\old.w2ent", old);
        t.put(r"redkit\r4data\t\other.xbm", "x");
        let config = Config {
            redkit: Some(t.0.join("redkit")),
            ..t.config()
        };
        let found = find(
            &Shared::new(Ok(config.clone())),
            &json!({"where": "redkit", "kinds": ["w2ent"]}),
        );
        assert_eq!(found["total"], 2, "{found}");
        let args = json!({"where": "redkit", "imports": ["pc_steering.w2steer"], "limit": 0});
        let scan = run(&config, "data_scan", args);
        assert_eq!(
            (&scan["matched"], &scan["skipped_count"]),
            (&json!(2), &json!(0)),
            "{scan}"
        );
        let read = run(
            &config,
            "data_read",
            json!({"where": "redkit", "path": r"t\old.w2ent"}),
        );
        assert_eq!(read["cr2w"]["imports"][0]["path"], steer, "{read}");
    }

    /// The app manifest's buildid and installed depots, only when it names the folder; the key is the manifests'.
    #[test]
    fn the_content_build_comes_from_the_app_manifest() {
        let t = Temp::new("manifest");
        let game = t.0.join(r"steamapps\common\Game Folder");
        fs::create_dir_all(&game).unwrap();
        t.put(
            r"steamapps\appmanifest_123.acf",
            "\"AppState\" { \"installdir\" \"Game Folder\" \"buildid\" \"42\" \"InstalledDepots\" { \"124\" { \
             \"manifest\" \"999\" \"size\" \"1\" } \"125\" { \"manifest\" \"888\" } } }",
        );
        let c = content(&game, "123", std::iter::empty::<&str>());
        assert_eq!(c["steam_buildid"], "42");
        assert_eq!(c["depot_manifests"], json!({"124": "999", "125": "888"}));
        assert_eq!(c["key"].as_str().unwrap().len(), 16);
        let other = t.0.join(r"steamapps\common\Other");
        fs::create_dir_all(&other).unwrap();
        let c = content(&other, "123", ["a", "b"].into_iter());
        assert_eq!(c["steam_buildid"], Value::Null);
        assert_eq!(c["key"], json!(sha256_hex(b"a\nb")[..16]));
        assert_eq!(
            content(&other, "123", std::iter::empty::<&str>())["key"],
            Value::Null
        );
    }

    /// A mod's scripts are copied with their imports stubbed, UTF-8 and UTF-16 alike; without stubs, as they are.
    #[test]
    fn scripts_are_copied_with_stubs() {
        let t = Temp::new("scripts");
        t.put(r"mod\local\a.ws", "import function A() : int;\n");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "import function B();"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        t.put(r"mod\b.ws", &utf16);
        let (files, stubbed) = copy_scripts(&t.0.join("mod"), &t.0.join("tree"), true).unwrap();
        assert_eq!((files.len(), stubbed), (2, 2));
        let b = fs::read(t.0.join(r"tree\b.ws")).unwrap();
        let back: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&back).unwrap(), "function B() { }");
        let (_, stubbed) = copy_scripts(&t.0.join("mod"), &t.0.join("plain"), false).unwrap();
        assert_eq!(stubbed, 0);
        assert_eq!(fs::read(t.0.join(r"plain\b.ws")).unwrap(), utf16);
    }

    /// A depot path stays inside the depot.
    #[test]
    fn depot_paths_stay_inside() {
        let root = Path::new(r"R:\depot");
        assert_eq!(inside(root, r"a\b.w2ent").unwrap(), root.join(r"a\b.w2ent"));
        assert!(
            inside(root, r"..\x").is_err()
                && inside(root, r"C:\x").is_err()
                && inside(root, r"\x").is_err()
        );
    }
}
