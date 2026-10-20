//! The ledger tools: `ledger_query`, `ledger_ingest`, `ledger_claim`, and for game knowledge
//! `ledger_import` and `ledger_about` over MCP; the `quen ledger` CLI the Python analysis scripts call (`add`), the
//! import of the harness's history (`migrate`) and `about`; `quen kb` (scan, claims, export) and `quen re import`.
//! They run on the reader thread, so a query never waits behind a boot. The ledger is `<data root>\state\quen.db`.
//!
//! Only the holder of the game lock writes rows about a boot (captures, measurements, load status). A writer
//! here holds it (a live session) or finds it free; while another quen holds it, those writes are refused.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use quen_core::journal::sha256_hex;
use quen_core::ledger::{About, Claim, Harness, Ledger, Row};
use serde_json::{Map, Value, json};

use crate::config::{self, Config};
use crate::session::{Done, Shared, envelope};
use crate::win;

/// Query results longer than this go to a file; the envelope keeps the first rows that fit.
pub(crate) const INLINE_BYTES: usize = 16 * 1024;
const TIME_LIMIT_S: f64 = 10.0;
const TIME_LIMIT_MAX_S: f64 = 30.0;
const LOCK_HELD: &str = "another quen holds the game lock (a live session): only the lock holder records captures, \
    measurements and load status. Record them through that session's ledger_ingest, or after its session_end";

pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "ledger_query",
            "description": "Read-only SQL over the ledger of boots, captures and measurements: one SELECT or WITH \
                statement on a read-only connection, positional parameters ?1, ?2, ... from params, interrupted at \
                time_limit_s (default 10, at most 30). Tables: games, saves, boots (harness_info: the Python \
                harness's info dict), poses, captures (path relative to the capture root, the harness's cap folder), \
                measurements, claims, claim_support, doc_refs, re_refs, mod_sets, mod_set_entries, mod_versions, \
                boot_mod_status, game knowledge (entities, links, extractions, kb_sections; one thing at a time: \
                ledger_about); the view claims_status says which claims are proven. Every table and view, with \
                example queries: quen_guide topic ledger_schema. Columns: SELECT * FROM \
                pragma_table_info('boots'). A result over 16 KB comes back as its first rows and the path of a JSON \
                file holding all of them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sql": {"type": "string", "description": "one SELECT or WITH statement"},
                    "params": {"type": "array", "items": {"type": "string"}, "description": "?1, ?2, ...: text (numbers and null too); SQLite reads text as a number where it meets a numeric column"},
                    "time_limit_s": {"type": "number", "description": "seconds, default 10, at most 30"},
                },
                "required": ["sql"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "ledger_ingest",
            "description": "Record rows in the ledger, all or none; returns each row's id (null for tables \
                without ids). Each row is one JSON object, as text, naming its table and carrying that table's \
                columns (rows says which tables; their columns: ledger_query). A measurement or capture row without a \
                boot_id key belongs to the live session's boot (state.boot in every result) only when token is that \
                session's; without token it stays without a boot, as with boot_id null. Boots are recorded by the \
                session tools. Refused, nothing written: any bad row; a token that isn't the live session's; \
                captures, measurements or boot_mod_status while another quen holds the game lock.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rows": {
                        "type": "array",
                        "items": {"type": "string", "description": "one row: a JSON object"},
                        "description": "each row one JSON object as text, e.g. {\"table\": \"measurements\", \"boot_id\": \
                            214, \"capture_id\": null, \"metric\": \"frame_ms\", \"value\": 23.1, \"unit\": \"ms\", \
                            \"state_label\": \"A\", \"method_family\": \"tracy\", \"tag\": \"M\", \"run_id\": null, \"note\": \
                            null}. Tables: measurements (method_family one of presentmon, tracy, nsight, \
                            renodx_devkit, screenshot, renderdoc, static_re, mem, files, any other refused; tag \
                            M, O, I, U or static_re), captures (path relative to the harness's cap folder; t in UTC \
                            like 2026-10-17T13:15:00Z), saves, poses (camera [x, y, z], rotation [pitch, yaw, roll], fov, \
                            player {position, optional yaw}), mod_versions, boot_mod_status, re_refs (target \
                            {\"measurement\": id} or {\"claim\": id}, kb_path)",
                    },
                    "token": {"type": "string", "description": "the token session_launch returned: rows without a boot_id key go on its boot"},
                },
                "required": ["rows"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "ledger_claim",
            "description": "Open, support or withdraw a claim (two-independent-methods rule). action open: text, \
                optional scope and support; support: claim_id and support; withdraw: claim_id and reason (the row \
                and the reason stay). support lists {measurement_id, supports} (supports false: the measurement \
                contradicts the claim). Returns the claim's claims_status row: proven is derived, never set, from \
                supporting measurements tagged M, O or static_re in at least two method families and no standing \
                contradiction. The write is committed even when that row can't be read: claim is then null, with status_error.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["open", "support", "withdraw"], "description": "what to do"},
                    "claim_id": {"type": "integer", "description": "the claim (support, withdraw)"},
                    "text": {"type": "string", "description": "the claim (open)"},
                    "scope": {"type": "string", "description": "where it holds (open, optional)"},
                    "reason": {"type": "string", "description": "why it is withdrawn (withdraw)"},
                    "support": {
                        "type": "array",
                        "description": "measurements for or against it (open, support)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "measurement_id": {"type": "integer", "description": "a measurements row"},
                                "supports": {"type": "boolean", "description": "false: it contradicts the claim (default true)"},
                            },
                            "required": ["measurement_id"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["action"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "ledger_import",
            "description": "Import game knowledge, all or none, and say what was added; importing the same thing \
                again adds nothing. kind rea_bundle: an REA evidence bundle (export_evidence_bundle's file, path), \
                copied into the capture root unless it is there, kept as a pinned capture; each record with a subject \
                and an analysis profile becomes an extraction, keyed by the target's sha256, the profile, the \
                operation and its parameters. kind kb: re-index the RE knowledge base (path, default quen's config \
                kb) at its git commit: its tables as entities, addresses and links, each note's sections and the \
                known entities they mention. kind kb_claims: one claim per table row of a KB note's sections, with \
                the row's grades and no support. Refused, nothing written: a malformed bundle or KB table, a KB \
                outside a git work tree, a KB whose build the ledger has no game row for.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["rea_bundle", "kb", "kb_claims"], "description": "what to import"},
                    "path": {"type": "string", "description": "rea_bundle: the bundle file; kb, kb_claims: the KB root (default: kb in quen's config)"},
                    "rea_version": {"type": "string", "description": "rea_bundle: REA's version, e.g. 6.5.0 (required: its records don't carry it)"},
                    "note": {"type": "string", "description": "kb_claims: the note, relative to the KB root, e.g. frame/PIPELINE_MAP.md"},
                    "sections": {"type": "array", "items": {"type": "string"}, "description": "kb_claims: section anchors, or heading labels such as 2d"},
                    "scope": {"type": "string", "description": "kb_claims: the claims' scope (the conditions they hold in)"},
                },
                "required": ["kind"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "ledger_about",
            "description": "Read-only: everything the ledger knows about one thing, and whether REA answered a query \
                on it before. Ask by kind and key (with game when several games have them), or by build (the exe's \
                sha256) and rva. Returns the entity, its addresses per build, the claims about it with their grades \
                and standing, its links with their basis and standing, the KB sections that mention it, and its REA \
                extractions: exact (the profile asked for: its bundle file holds the answer, no need to start \
                Ghidra), near (another profile, or none asked for: compare the seeds), gone (the bundle file was \
                removed). operation, params and profile narrow the extractions; with build, the three give the cache \
                key (key_sha256). A result over 16 KB comes back as counts and the path of a JSON file holding it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "game": {"type": "string", "description": "the game's title (games.name), when several have the kind and key"},
                    "kind": {"type": "string", "description": "the entity kind: function, label, hook, patch, field, shader, ... (quen_guide ledger_schema)"},
                    "key": {"type": "string", "description": "the entity key, e.g. a function's KB name or a shader hash 0x382CDBDB"},
                    "build": {"type": "string", "description": "the exe's sha256: with rva, or to narrow the extractions to one build"},
                    "rva": {"type": "string", "description": "0x-prefixed hex, with build"},
                    "operation": {"type": "string", "description": "an REA operation, e.g. procedure_pseudo_code"},
                    "params": {"type": "string", "description": "the operation's parameters as JSON object text, e.g. {\"procedure\": \"0x14034c880\"}"},
                    "profile": {"type": "string", "description": "an analysis profile digest (64 hex): its extractions are exact hits"},
                },
                "additionalProperties": false,
            },
        }),
    ]
}

/// A `ledger_*` tool call, or mem_diff (it reads kept captures), answered with the envelope.
pub fn call(shared: &Shared, name: &str, args: &Value) -> Value {
    let started = Instant::now();
    let mut artifacts = Vec::new();
    let config = shared.config();
    let done = match &*config {
        Err(e) => Done::Refused(format!("quen's config: {e}")),
        Ok(config) => match name {
            "ledger_query" => query(config, args, &mut artifacts),
            "ledger_ingest" => match ingest_args(shared, args) {
                Err(why) => Done::Refused(why),
                Ok((rows, boot)) => {
                    // Held while writing, so this quen can't take the lock and launch in between.
                    let lock = shared.lock_guard();
                    let rows: Vec<Value> =
                        rows.into_iter().map(|row| with_boot(row, boot)).collect();
                    ids(ingest(config, &rows, lock.is_some()))
                }
            },
            "ledger_claim" => claim(config, args),
            "ledger_import" => done(import(config, args)),
            "ledger_about" => {
                done(about_args(args).and_then(|q| about(config, &q, &mut artifacts)))
            }
            "mem_diff" => done(crate::mem::diff(config, args, &mut artifacts)),
            _ => Done::Refused(format!("unknown tool {name:?}")),
        },
    };
    let secs = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let timing = Map::from_iter([(name.to_owned(), json!(secs))]);
    envelope(shared, name, timing, artifacts, Map::new(), done)
}

fn db_path(config: &Config) -> PathBuf {
    config.state_dir().join("quen.db")
}

fn open(config: &Config) -> Result<Ledger, String> {
    let dir = config.state_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ledger::open(db_path(config)).map_err(why)
}

/// What a ledger error says, without the "refused" its Display adds (the envelope says that).
fn why(e: quen_core::ledger::Error) -> String {
    match e {
        quen_core::ledger::Error::Refused(why) => why,
        e => e.to_string(),
    }
}

/// Whether another quen holds the game lock: the holder opens it with share mode 0, so any open fails (as status
/// reads it). ponytail: a probe, not a take; a quen that takes the lock right after writes beside this write.
fn lock_held_elsewhere(config: &Config) -> bool {
    File::open(config.state_dir().join("game.lock"))
        .is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
}

/// The arguments as an object with only `known` keys.
pub(crate) fn object<'a>(
    args: &'a Value,
    known: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let args = args.as_object().ok_or("arguments must be an object")?;
    match args.keys().find(|k| !known.contains(&k.as_str())) {
        Some(key) => Err(format!("unknown argument {key:?} ({})", known.join(", "))),
        None => Ok(args),
    }
}

fn list(args: &Map<String, Value>, key: &str) -> Result<Vec<Value>, String> {
    match args.get(key) {
        Some(Value::Array(items)) => Ok(items.clone()),
        _ => Err(format!("{key} must be a list")),
    }
}

/// ledger_ingest's rows, and the boot a row without a `boot_id` key goes on: the live session's with its token, else
/// none, so one agent's rows never land on another's boot.
fn ingest_args(shared: &Shared, args: &Value) -> Result<(Vec<Value>, Option<i64>), String> {
    let args = object(args, &["rows", "token"])?;
    let boot = match args.get("token") {
        None => None,
        Some(token) => {
            let token = token.as_str().ok_or("token must be a string")?;
            let wrong = "wrong token: not the live session's (without token, a row goes on the boot its boot_id \
                names, or none)";
            Some(shared.boot_for(token).ok_or(wrong)?)
        }
    };
    // A row is JSON text in the schema (a free-form object isn't portable: docs/clients.md); an object still serves.
    let rows = list(args, "rows")?
        .into_iter()
        .enumerate()
        .map(|(i, row)| match row {
            Value::String(text) => serde_json::from_str(&text).map_err(|e| format!("row {i}: {e}")),
            row => Ok(row),
        });
    Ok((rows.collect::<Result<_, _>>()?, boot))
}

/// Parses and writes rows, all or none. `held_here`: this quen holds the game lock.
fn ingest(config: &Config, rows: &[Value], held_here: bool) -> Result<Vec<Option<i64>>, String> {
    let rows = rows
        .iter()
        .enumerate()
        .map(|(i, r)| Row::from_json(r).map_err(|e| format!("row {i}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Err("no rows".into());
    }
    if !held_here && rows.iter().any(Row::needs_lock) && lock_held_elsewhere(config) {
        return Err(LOCK_HELD.into());
    }
    open(config)?.ingest(&rows).map_err(why)
}

/// A measurement or capture row with no `boot_id` key is about `boot`, if one is given; an explicit null keeps it
/// without one.
fn with_boot(mut row: Value, boot: Option<i64>) -> Value {
    let about_a_boot = matches!(row["table"].as_str(), Some("measurements" | "captures"));
    if let (Some(boot), Some(fields)) = (boot, row.as_object_mut())
        && about_a_boot
        && !fields.contains_key("boot_id")
    {
        fields.insert("boot_id".into(), json!(boot));
    }
    row
}

fn ids(ids: Result<Vec<Option<i64>>, String>) -> Done {
    match ids {
        Ok(ids) => Done::Ok(Map::from_iter([("ledger".into(), json!(ids))])),
        Err(why) => Done::Refused(why),
    }
}

fn query(config: &Config, args: &Value, artifacts: &mut Vec<Value>) -> Done {
    let mut run = || -> Result<Map<String, Value>, String> {
        let args = object(args, &["sql", "params", "time_limit_s"])?;
        let sql = args
            .get("sql")
            .and_then(Value::as_str)
            .ok_or("sql must be a string")?;
        let params = match args.get("params") {
            None => Vec::new(),
            Some(_) => list(args, "params")?,
        };
        let limit = match args.get("time_limit_s") {
            None => TIME_LIMIT_S,
            Some(v) => v
                .as_f64()
                .filter(|s| *s > 0.0)
                .ok_or("time_limit_s must be a positive number")?
                .min(TIME_LIMIT_MAX_S),
        };
        let path = db_path(config);
        if !path.exists() {
            return Err(format!(
                "no ledger yet at {}: nothing has been recorded",
                path.display()
            ));
        }
        let rows = Ledger::open_readonly(&path)
            .and_then(|l| l.query(sql, &params, Duration::from_secs_f64(limit)))
            .map_err(why)?;
        let mut fields = Map::new();
        fields.insert("columns".into(), json!(rows.columns));
        fields.insert("row_count".into(), json!(rows.rows.len()));
        let text = json!(rows.rows).to_string();
        if text.len() <= INLINE_BYTES {
            fields.insert("rows".into(), json!(rows.rows));
            return Ok(fields);
        }
        let data =
            json!({"sql": sql, "params": params, "columns": rows.columns, "rows": rows.rows});
        let file = spill(&queries(config), "", &data, artifacts)?;
        let mut size = 0;
        let head: Vec<&Vec<Value>> = rows
            .rows
            .iter()
            .take_while(|r| {
                size += json!(r).to_string().len() + 1;
                size <= INLINE_BYTES / 2
            })
            .collect();
        fields.insert("rows".into(), json!(head));
        fields.insert("truncated".into(), json!(true));
        fields.insert("file".into(), json!(file));
        Ok(fields)
    };
    match run() {
        Ok(fields) => Done::Ok(fields),
        Err(why) => Done::Refused(why),
    }
}

/// `data` written to a new JSON file in `dir`, `<prefix><secs>-<random>.json`, listed in `artifacts`: its path.
/// ledger_query's go under `<data root>\logs\queries` ([`queries`]), the mem tools' into the session's capture folder.
pub(crate) fn spill(
    dir: &Path,
    prefix: &str,
    data: &Value,
    artifacts: &mut Vec<Value>,
) -> Result<PathBuf, String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let file = dir.join(format!(
        "{prefix}{secs}-{}.json",
        win::random_hex(4).unwrap_or_default()
    ));
    let data = data.to_string();
    fs::create_dir_all(dir)
        .and_then(|()| fs::write(&file, &data))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    artifacts.push(json!({"path": file, "sha256": sha256_hex(data.as_bytes())}));
    Ok(file)
}

/// Where ledger_query and ledger_about put a result too big to answer inline.
pub(crate) fn queries(config: &Config) -> PathBuf {
    config.data_root.join("logs").join("queries")
}

fn done(fields: Result<Map<String, Value>, String>) -> Done {
    match fields {
        Ok(fields) => Done::Ok(fields),
        Err(why) => Done::Refused(why),
    }
}

/// The KB root: `path`, else the config's `kb`.
fn kb_root(config: &Config, path: Option<&str>) -> Result<PathBuf, String> {
    path.map(PathBuf::from)
        .or_else(|| config.kb.clone())
        .ok_or_else(|| "no KB: give its path, or set kb in quen's config".into())
}

/// `ledger_import`: an REA bundle, a KB scan, or a KB note's table rows as claims. The fields of its envelope.
fn import(config: &Config, args: &Value) -> Result<Map<String, Value>, String> {
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or_default();
    let known: &[&str] = match kind {
        "rea_bundle" => &["kind", "path", "rea_version"],
        "kb" => &["kind", "path"],
        "kb_claims" => &["kind", "path", "note", "sections", "scope"],
        _ => return Err("kind must be rea_bundle, kb or kb_claims".into()),
    };
    let args = object(args, known)?;
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let out = match kind {
        "rea_bundle" => {
            let path = text("path").ok_or("rea_bundle needs path: the bundle file")?;
            let version = text("rea_version").ok_or("rea_bundle needs rea_version")?;
            let mut ledger = open(config)?;
            json!(
                ledger
                    .import_rea_bundle(&config.capture_root(), path.as_ref(), version)
                    .map_err(why)?
            )
        }
        "kb" => {
            let root = kb_root(config, text("path"))?;
            json!(open(config)?.scan_kb(&root).map_err(why)?)
        }
        _ => {
            let note = text("note").ok_or("kb_claims needs note")?;
            let sections: Vec<String> = list(args, "sections")
                .map_err(|_| "kb_claims needs sections: a list of anchors or heading labels")?
                .iter()
                .map(|s| s.as_str().map(str::to_owned).ok_or("sections are strings"))
                .collect::<Result<_, _>>()?;
            let root = kb_root(config, text("path"))?;
            let mut ledger = open(config)?;
            json!(
                ledger
                    .import_kb_claims(&root, note, &sections, text("scope"))
                    .map_err(why)?
            )
        }
    };
    Ok(Map::from_iter([("imported".into(), out)]))
}

/// ledger_about's arguments: params is JSON object text in the schema (portable), an object still serves.
fn about_args(args: &Value) -> Result<About, String> {
    let mut args = args.clone();
    if let Some(text) = args.get("params").and_then(Value::as_str) {
        args["params"] = serde_json::from_str(text).map_err(|e| format!("params: {e}"))?;
    }
    serde_json::from_value(args).map_err(|e| e.to_string())
}

/// `ledger_about`, read-only: its answer inline, or in a file when it is over [`INLINE_BYTES`].
fn about(
    config: &Config,
    q: &About,
    artifacts: &mut Vec<Value>,
) -> Result<Map<String, Value>, String> {
    let path = db_path(config);
    if !path.exists() {
        return Err(format!(
            "no ledger yet at {}: nothing has been recorded",
            path.display()
        ));
    }
    let answer = Ledger::open_readonly(&path)
        .and_then(|l| l.about(q))
        .map_err(why)?;
    if answer.to_string().len() <= INLINE_BYTES {
        return Ok(Map::from_iter([("about".into(), answer)]));
    }
    let len = |v: &Value| v.as_array().map_or(0, Vec::len);
    let x = &answer["extractions"];
    let counts = json!({
        "addresses": len(&answer["addresses"]), "claims": len(&answer["claims"]), "links": len(&answer["links"]),
        "sections": len(&answer["sections"]), "exact": len(&x["exact"]), "near": len(&x["near"]), "gone": len(&x["gone"]),
    });
    let file = spill(&queries(config), "", &answer, artifacts)?;
    Ok(Map::from_iter([
        ("entity".into(), answer["entity"].clone()),
        ("extracted".into(), answer["extracted"].clone()),
        ("counts".into(), counts),
        ("truncated".into(), json!(true)),
        ("file".into(), json!(file)),
    ]))
}

fn claim(config: &Config, args: &Value) -> Done {
    let run = || -> Result<Map<String, Value>, String> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let known: &[&str] = match action {
            "open" => &["action", "text", "scope", "support"],
            "support" => &["action", "claim_id", "support"],
            "withdraw" => &["action", "claim_id", "reason"],
            _ => return Err("action must be open, support or withdraw".into()),
        };
        let args = object(args, known)?;
        let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_owned);
        let claim_id = args.get("claim_id").and_then(Value::as_i64);
        let mut support = Vec::new();
        for (i, s) in list(args, "support").unwrap_or_default().iter().enumerate() {
            let s = object(s, &["measurement_id", "supports"])
                .map_err(|e| format!("support {i}: {e}"))?;
            let m = s.get("measurement_id").and_then(Value::as_i64);
            let supports = s.get("supports").map_or(Some(true), Value::as_bool);
            match (m, supports) {
                (Some(m), Some(supports)) => support.push((m, supports)),
                _ => {
                    return Err(format!(
                        "support {i}: measurement_id (integer) and supports (true or false)"
                    ));
                }
            }
        }
        let open_claim = match action {
            "open" => Some(Claim {
                text: text("text").ok_or("open needs text")?,
                scope: text("scope"),
            }),
            _ => None,
        };
        if open_claim.is_none() && claim_id.is_none() {
            return Err(format!("{action} needs claim_id"));
        }
        let reason = text("reason");
        if action == "withdraw" && reason.is_none() {
            return Err("withdraw needs reason".into());
        }
        if action == "support" && support.is_empty() {
            return Err("support needs a non-empty support list".into());
        }
        let mut ledger = open(config)?;
        let id = ledger
            .atomic(|l| {
                let id = match &open_claim {
                    Some(c) => l.add_claim(c)?,
                    None => claim_id.unwrap_or_default(),
                };
                if let Some(reason) = &reason {
                    l.withdraw(id, reason)?;
                }
                for &(m, supports) in &support {
                    l.support(id, m, supports)?;
                }
                Ok(id)
            })
            .map_err(why)?;
        // Committed: from here nothing refuses, or a retry would store the claim twice.
        let mut fields = Map::new();
        fields.insert("ledger".into(), json!([id]));
        let status = ledger.query(
            "SELECT * FROM claims_status WHERE id = ?1",
            &[json!(id)],
            Duration::from_secs(5),
        );
        let row = match status {
            Ok(status) => status
                .rows
                .first()
                .map(|r| Map::from_iter(status.columns.iter().cloned().zip(r.iter().cloned()))),
            Err(e) => {
                fields.insert("status_error".into(), json!(why(e)));
                None
            }
        };
        fields.insert("claim".into(), json!(row));
        Ok(fields)
    };
    match run() {
        Ok(fields) => Done::Ok(fields),
        Err(why) => Done::Refused(why),
    }
}

const HELP: &str = "\
quen ledger: the ledger of boots, captures and measurements, at <data root>\\state\\quen.db.

quen ledger add < rows.json
    Records rows, all or none: the same rows as the MCP tool ledger_ingest, as one JSON array or one JSON object per
    line on stdin. The Python analysis scripts use it to record a second method family, e.g. one line:
      {\"table\": \"measurements\", \"boot_id\": 214, \"capture_id\": null, \"metric\": \"frame_ms\", \"value\": 23.1,
       \"unit\": \"ms\", \"state_label\": \"A\", \"method_family\": \"tracy\", \"tag\": \"M\", \"run_id\": null, \"note\": null}
    method_family is one of presentmon, tracy, nsight, renodx_devkit, screenshot, renderdoc, static_re, mem, files
    (any other is refused); tag is M, O, I, U or static_re. Other tables: captures, saves, poses, mod_versions,
    boot_mod_status, re_refs. Prints {\"ok\": true, \"ids\": [...]}.
    Refused, nothing written (exit 1): a bad row, or captures / measurements / boot_mod_status while a quen holds the
    game lock (a live session): record those after its session_end, or through its ledger_ingest.

quen ledger about '{\"kind\": \"function\", \"key\": \"Post_Tonemap\"}'
    Everything the ledger knows about one thing, as the MCP tool ledger_about: one JSON object argument (kind and
    key, or build and rva; optional game, operation, params, profile).

quen ledger migrate [--dry-run] [--harness DIR] [--game-exe PATH] [--gamesaves DIR] [--docs DIR]...
    Imports the Python harness's history, mechanically: every file under DIR\\cap except *.sh, *.py and *.ps1
    (path, sha256, size, time), one boot per cap\\perf_boot_*.log and per boot in cap\\boots.jsonl, the saves
    map\\test_saves.md lists, and a doc-reference scan of the .md/.py/.sh/.ps1/.json/.toml files under each --docs
    DIR. Running it again adds nothing. --dry-run prints the counts and writes nothing (no hashing either).
    Defaults from the local config: --harness the folder of `harness`, --game-exe game_dir\\bin\\x64_dx12\\<game_exe>,
    --gamesaves documents\\gamesaves, --docs the harness folder.

Output is one JSON line on stdout. Exit 0: done; 1: refused or failed, nothing written; 2: usage.
";

/// `quen ledger ...`: the exit code.
pub fn cli(args: &[String]) -> i32 {
    let result = match args.first().map(String::as_str) {
        Some("add") if args.len() == 1 => cli_add(),
        Some("about") if args.len() == 2 => cli_about(&args[1]),
        Some("migrate") => cli_migrate(&args[1..]),
        Some("--help" | "-h" | "help") => {
            print!("{HELP}");
            return 0;
        }
        _ => {
            eprint!("{HELP}");
            return 2;
        }
    };
    printed(result)
}

/// Prints a CLI result as one JSON line: the exit code.
fn printed(result: Result<Value, String>) -> i32 {
    let (code, out) = match result {
        Ok(Value::Object(mut fields)) => {
            fields.insert("ok".into(), json!(true));
            (0, Value::Object(fields))
        }
        Ok(other) => (0, json!({"ok": true, "result": other})),
        Err(why) => (1, json!({"ok": false, "refused": true, "reason": why})),
    };
    println!("{out}");
    code
}

fn cli_about(arg: &str) -> Result<Value, String> {
    let config = config::load()?;
    let args: Value = serde_json::from_str(arg).map_err(|e| format!("the argument: {e}"))?;
    let mut artifacts = Vec::new();
    let mut fields = about(&config, &about_args(&args)?, &mut artifacts)?;
    if !artifacts.is_empty() {
        fields.insert("artifacts".into(), json!(artifacts));
    }
    Ok(Value::Object(fields))
}

const KB_HELP: &str = "\
quen kb: the RE knowledge base's index in the ledger. KB is its root, default kb in the config.

quen kb scan [KB]
    Indexes the KB at its git commit (read from its .git files): names.tsv, hooks.tsv, patches.tsv,
    instance_patches.tsv and cvar_layout.tsv as entities, addresses and links; every .md note split at its H2 / H3
    headings; the known entities each section mentions. A scan of an unchanged KB adds nothing.

quen kb claims NOTE SECTION... [--scope TEXT] [--kb KB]
    One claim per table row of the note's sections (anchors, or heading labels such as 2d), keyed
    NOTE#<anchor>/<sha256 prefix of its text>, with the row's grades, a re_refs row and its shader hashes as
    subjects; no support. Importing again adds nothing.

quen kb export [KB]
    Writes the ledger's keyed claims and their claim-backed links to KB\\ledger\\claims.tsv and links.tsv, sorted
    by key, for the KB's git history.

Output is one JSON line on stdout. Exit 0: done; 1: refused or failed, nothing written; 2: usage.
";

/// `quen kb ...`: the exit code.
pub fn cli_kb(args: &[String]) -> i32 {
    let rest = &args[args.len().min(1)..];
    let kb = match rest {
        [] => Some(None),
        [kb] => Some(Some(kb.as_str())),
        _ => None,
    };
    let result = match (args.first().map(String::as_str), kb) {
        (Some("scan"), Some(kb)) => config::load().and_then(|c| {
            let root = kb_root(&c, kb)?;
            Ok(json!(open(&c)?.scan_kb(&root).map_err(why)?))
        }),
        (Some("export"), Some(kb)) => config::load().and_then(|c| cli_export(&c, kb)),
        (Some("claims"), _) if rest.len() >= 2 => cli_claims(rest),
        (Some("--help" | "-h" | "help"), _) => {
            print!("{KB_HELP}");
            return 0;
        }
        _ => {
            eprint!("{KB_HELP}");
            return 2;
        }
    };
    printed(result)
}

fn cli_claims(args: &[String]) -> Result<Value, String> {
    let config = config::load()?;
    let (mut scope, mut kb, mut sections) = (None, None, Vec::new());
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--scope" => scope = Some(it.next().ok_or("--scope needs a value")?.clone()),
            "--kb" => kb = Some(it.next().ok_or("--kb needs a value")?.clone()),
            _ => sections.push(arg.clone()),
        }
    }
    let root = kb_root(&config, kb.as_deref())?;
    let out = open(&config)?
        .import_kb_claims(&root, &args[0], &sections, scope.as_deref())
        .map_err(why)?;
    Ok(json!(out))
}

fn cli_export(config: &Config, kb: Option<&str>) -> Result<Value, String> {
    let root = kb_root(config, kb)?;
    let path = db_path(config);
    if !path.exists() {
        return Err(format!("no ledger yet at {}", path.display()));
    }
    let (claims, links) = Ledger::open_readonly(&path)
        .and_then(|l| l.kb_export())
        .map_err(why)?;
    let dir = root.join("ledger");
    let mut files = Vec::new();
    for (name, text) in [("claims.tsv", &claims), ("links.tsv", &links)] {
        let file = dir.join(name);
        fs::create_dir_all(&dir)
            .and_then(|()| fs::write(&file, text))
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let rows = text.lines().count().saturating_sub(1);
        files.push(json!({"path": file, "sha256": sha256_hex(text.as_bytes()), "rows": rows}));
    }
    Ok(json!({"files": files}))
}

const RE_HELP: &str = "\
quen re import BUNDLE --rea-version VERSION
    Imports an REA evidence bundle (export_evidence_bundle's JSON), as the MCP tool
    ledger_import with kind rea_bundle: copied into <capture root>\\re\\ unless it is under the capture root, kept
    as a pinned capture, each record with a subject and an analysis profile indexed as an extraction. VERSION is
    REA's (its records don't carry it). Importing again adds nothing. One JSON line on stdout; exit 0 done, 1
    refused (nothing written), 2 usage.
";

/// `quen re ...`: the exit code.
pub fn cli_re(args: &[String]) -> i32 {
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["import", bundle, "--rea-version", version] => config::load().and_then(|c| {
            let mut ledger = open(&c)?;
            let out = ledger
                .import_rea_bundle(&c.capture_root(), bundle.as_ref(), version)
                .map_err(why)?;
            Ok(json!(out))
        }),
        ["--help" | "-h" | "help"] => {
            print!("{RE_HELP}");
            return 0;
        }
        _ => {
            eprint!("{RE_HELP}");
            return 2;
        }
    };
    printed(result)
}

fn cli_add() -> Result<Value, String> {
    let config = config::load()?;
    let mut text = String::new();
    io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| format!("stdin: {e}"))?;
    let rows: Vec<Value> = if text.trim_start().starts_with('[') {
        serde_json::from_str(&text).map_err(|e| format!("stdin: {e}"))?
    } else {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
            .collect::<Result<_, _>>()?
    };
    Ok(json!({"ids": ingest(&config, &rows, false)?}))
}

fn cli_migrate(args: &[String]) -> Result<Value, String> {
    let config = config::load()?;
    let (mut dry_run, mut docs) = (false, Vec::new());
    let mut harness = config
        .harness
        .as_deref()
        .and_then(|h| h.parent())
        .map(PathBuf::from);
    let mut game_exe = config
        .game_dir
        .as_ref()
        .map(|g| g.join("bin").join("x64_dx12").join(&config.game_exe));
    let mut gamesaves = config.documents.as_ref().map(|d| d.join("gamesaves"));
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .map(PathBuf::from)
                .ok_or(format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--harness" => harness = Some(value()?),
            "--game-exe" => game_exe = Some(value()?),
            "--gamesaves" => gamesaves = Some(value()?),
            "--docs" => docs.push(value()?),
            _ => return Err(format!("unknown argument {arg:?} (quen ledger --help)")),
        }
    }
    let dir = harness.ok_or("no harness folder: --harness, or harness in the local config")?;
    let h = Harness {
        doc_roots: if docs.is_empty() {
            vec![dir.clone()]
        } else {
            docs
        },
        game_exe: game_exe.ok_or("no game exe: --game-exe, or game_dir in the local config")?,
        gamesaves,
        dir,
    };
    let path = db_path(&config);
    if dry_run {
        let ledger = if path.exists() {
            Ledger::open_readonly(&path)
        } else {
            Ledger::open_in_memory()
        };
        let plan = ledger.and_then(|l| l.plan_migration(&h)).map_err(why)?;
        return Ok(json!({"dry_run": true, "ledger": path, "summary": plan.summary}));
    }
    if lock_held_elsewhere(&config) {
        return Err(LOCK_HELD.into());
    }
    let mut ledger = open(&config)?;
    let plan = ledger.plan_migration(&h).map_err(why)?;
    eprintln!(
        "quen ledger migrate: hashing {} files ({:.1} GB), then writing",
        plan.summary.files_new,
        plan.summary.bytes_new as f64 / 1e9
    );
    let summary = ledger.apply_migration(&h, plan).map_err(why)?;
    Ok(json!({"dry_run": false, "ledger": path, "summary": summary}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_without_a_boot_id_gets_the_given_boot() {
        let row = |extra: Value| {
            let mut row = json!({"table": "measurements", "metric": "frame_ms"});
            row.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            row
        };
        assert_eq!(with_boot(row(json!({})), Some(7))["boot_id"], 7);
        let explicit = with_boot(row(json!({"boot_id": null})), Some(7));
        assert!(explicit["boot_id"].is_null() && explicit.get("boot_id").is_some());
        assert_eq!(with_boot(row(json!({"boot_id": 3})), Some(7))["boot_id"], 3);
        assert!(with_boot(row(json!({})), None).get("boot_id").is_none());
        let save = with_boot(json!({"table": "saves"}), Some(7));
        assert!(save.get("boot_id").is_none(), "only rows about a boot");
    }
}
