//! Vortex on this PC, read-only: quen-core's `vortex` over the real files (each one's id and link count from
//! [`win::file_info`]), whether Vortex runs, the `mods` tool, `quen_doctor`'s line, and what a launch adds: its
//! refusals, each parked item's owner, what it leaves loaded, the end's link report and the mod set's labels. quen never
//! writes Vortex's files and never starts, deploys or purges it.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

use quen_core::boot::{self, Change};
use quen_core::ledger::{Ledger, ModVersion, Provenance};
use quen_core::mods::{Kind, ModEntry};
use quen_core::vortex::{self, Files, Inventory, Stat};
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::session::{self, Done, Shared};
use crate::{game, win};

/// The real files.
struct Disk;

impl Files for Disk {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        fs::read(path).ok()
    }

    fn stat(&self, path: &Path) -> Option<Stat> {
        let (id, links) = win::file_info(path).ok()?;
        Some(Stat { id, links })
    }

    fn list(&self, dir: &Path) -> Vec<(String, bool)> {
        let entries = fs::read_dir(dir).into_iter().flatten().flatten();
        let entry = |e: fs::DirEntry| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.file_name().to_string_lossy().into_owned(), is_dir)
        };
        entries.map(entry).collect()
    }
}

/// Vortex's records for the game in `game_dir` (and `documents`); None without a manifest.
fn read_at(game_dir: &Path, documents: Option<&Path>) -> Option<Inventory> {
    vortex::inventory(&Disk, game_dir, documents)
}

/// Vortex's records for the configured game; None without a manifest or a game folder.
pub(crate) fn read(config: &Config) -> Option<Inventory> {
    read_at(config.game_dir.as_deref()?, config.documents.as_deref())
}

/// Whether Vortex runs: its image in one Toolhelp snapshot of the processes (no console program). `QUEN_TEST_VORTEX_IMAGE`
/// (tests only) names another image, so the user's own Vortex doesn't fail the tests.
fn running() -> Result<bool, String> {
    let image = std::env::var("QUEN_TEST_VORTEX_IMAGE").unwrap_or_else(|_| vortex::PROCESS.into());
    let all = game::processes().map_err(|e| format!("the process list: {e}"))?;
    Ok(all
        .iter()
        .any(|(_, name)| name.eq_ignore_ascii_case(&image)))
}

/// The launch's refusals with Vortex here (quen-core's `Inventory::refusal`), over the changes it makes: every move
/// parks, every rewritten or kept file gets a new file at the end. Ok(None): no Vortex, nothing checked.
pub(crate) fn check(config: &Config, changes: &[Change]) -> Result<Option<Inventory>, String> {
    let Some(inv) = read(config) else {
        return Ok(None);
    };
    let (mut parked, mut rewritten) = (Vec::new(), Vec::new());
    for change in changes {
        match change {
            Change::Move { from, .. } => parked.push(from.as_path()),
            Change::Rewrite { path, .. } | Change::Rebuilt { path, .. } => {
                rewritten.push(path.as_path())
            }
            _ => {}
        }
    }
    match inv.refusal(&Disk, &parked, &rewritten, &running) {
        Some(why) => Err(why),
        None => Ok(Some(inv)),
    }
}

/// session_plan's and session_launch's `vortex`: each parked item's owner, and with any park the Vortex files the game
/// still loads that quen can't park, by mod.
pub(crate) fn fields(inv: &Inventory, changes: &[Change]) -> Value {
    let parked: Vec<Value> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Move { from, .. } => {
                Some(json!({"item": inv.rel(from), "owner": inv.owner(from)}))
            }
            _ => None,
        })
        .collect();
    let mut out = json!({"parked": parked});
    if !parked.is_empty() {
        out["left_loaded"] = json!(inv.left_loaded());
    }
    out
}

/// The end's link report, after the replay: every deployed file as Vortex's next deploy would class it, the ones broken
/// in `before` (the plan's read) under `before_launch`. None when the manifests are gone.
pub(crate) fn report(config: &Config, before: &[String]) -> Option<Value> {
    read(config).map(|inv| json!(inv.links(before)))
}

/// Labels the entries of a boot's mod set that one Vortex mod deployed with that mod's version (`mod_versions`,
/// provenance nexus_file, source its Vortex folder: the download's name), before the set is added, so its entries get
/// them. A parked entry is looked up where it is installed. A label the ledger refuses (that build is labelled
/// otherwise already) leaves the ledger's.
pub(crate) fn label(ledger: &mut Ledger, inv: &Inventory, game_dir: &Path, entries: &[ModEntry]) {
    for e in entries {
        let installed = match (e.parked, e.home.file_name()) {
            (true, Some(name)) if e.kind == Kind::ModsFolder => game_dir.join("Mods").join(name),
            (true, Some(name)) => boot::bin_dir(game_dir).join(name),
            _ => game_dir.join(&e.home),
        };
        let [m] = inv.owners(&installed)[..] else {
            continue;
        };
        let Some(version) = &m.id.version else {
            continue;
        };
        let _ = ledger.add_mod_version(&ModVersion {
            content_hash: e.content_hash.clone(),
            kind: e.kind,
            name: e.name.clone(),
            version: version.clone(),
            provenance: Provenance::NexusFile,
            source: m.id.vortex.clone(),
        });
    }
}

/// quen_doctor's one line: Vortex found or not, its deployment method, its mods and their links.
pub(crate) fn summary(game_dir: Option<&Path>, documents: Option<&Path>) -> Value {
    let Some(game_dir) = game_dir else {
        return Value::Null;
    };
    let Some(inv) = read_at(game_dir, documents) else {
        return json!("no Vortex deployment manifest in the game folder");
    };
    let live: Vec<&vortex::Manifest> = inv.manifests.iter().filter(|m| m.stale.is_none()).collect();
    let mut methods: Vec<&str> = live.iter().map(|m| m.method.as_str()).collect();
    methods.sort();
    methods.dedup();
    let links = inv.links(&[]);
    let mut line = format!(
        "Vortex: {} mods deployed by {} ({} manifests), {} of {} files still Vortex's links",
        inv.mods.len(),
        methods.join(" and "),
        live.len(),
        links.ok,
        links.checked
    );
    let stale = inv.manifests.len() - live.len();
    for (n, what) in [
        (
            stale,
            "manifests stale (their files count as outside Vortex)",
        ),
        (inv.undeployed.len(), "mods installed but not deployed"),
    ] {
        if n > 0 {
            line.push_str(&format!("; {n} {what}"));
        }
    }
    json!(format!("{line}: the mods tool lists them"))
}

pub fn tool() -> Value {
    json!({
        "name": "mods",
        "description": "Read-only: the game's mods by Vortex's records (its deployment manifests in the game folder, \
            Mods, DLC and documents; mods.settings). vortex: null without a manifest; else manifests (mod_type, \
            method, deployed, stale: why its files count as outside Vortex), links over every deployed file (ok: \
            still Vortex's hardlink; deleted, srcdeleted, refchange: what Vortex's next deploy lists) and running. \
            mods: each Vortex mod (name, nexus_id, version from its folder's name; vortex: the folder), units (kind \
            mods, bin, menu, dlc or other; path; files), inert files and links. load_order: mods.settings. outside: \
            Mods folders and bin's .asi, .addon64 and dxgi.dll no manifest lists (without Vortex: all). undeployed: \
            Vortex's mods no manifest lists. shared: a path two mods claim, a Mods folder with two mods' files. pair \
            [x, y] (nexus:<id>, name or Vortex folder): the paths both carry, whose copy is deployed, the Mods \
            folders both use. During a session its parked files show deleted. Any connection, beside a session.",
        "inputSchema": {"type": "object", "properties": {
            "pair": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 2,
                     "description": "two deployed Vortex mods to compare, each nexus:<id>, its name or its Vortex folder"},
        }, "additionalProperties": false},
    })
}

/// `mods`, on the connection's own thread: it reads files and listings only.
pub fn call(shared: &Shared, args: &Value) -> Value {
    let t = Instant::now();
    let config = shared.config();
    let done = match config.as_ref() {
        Ok(config) => list(config, args).map_or_else(Done::Refused, Done::Ok),
        Err(why) => Done::Refused(format!("quen's config: {why}")),
    };
    let secs = (t.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let timing = Map::from_iter([("mods".into(), json!(secs))]);
    session::envelope(shared, "mods", timing, Vec::new(), Map::new(), done)
}

fn list(config: &Config, args: &Value) -> Result<Map<String, Value>, String> {
    let args = args.as_object().ok_or("arguments must be an object")?;
    if let Some(key) = args.keys().find(|k| *k != "pair") {
        return Err(format!("unknown argument {key:?}"));
    }
    let pair = args.get("pair").map(|v| {
        let names: Option<Vec<&str>> = v
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect());
        names
            .filter(|n| n.len() == 2)
            .ok_or("pair is two mod names: [x, y]")
    });
    let pair = pair.transpose()?;
    let game_dir = config
        .game_dir
        .as_deref()
        .ok_or("mods needs game_dir in the local config")?;
    let inv = read(config);
    let listing = |dir: &Path| crate::boot::listing(dir).unwrap_or_default();
    let mods_listing = listing(&game_dir.join("Mods"));
    let outside = vortex::outside(
        inv.as_ref(),
        game_dir,
        &mods_listing,
        &listing(&boot::bin_dir(game_dir)),
    );
    let mut fields = Map::new();
    let Some(inv) = inv else {
        if pair.is_some() {
            return Err(
                "pair compares Vortex mods, and the game has no Vortex deployment manifest".into(),
            );
        }
        fields.insert("vortex".into(), Value::Null);
        fields.insert(
            "note".into(),
            json!("no Vortex deployment manifest in the game folder, Mods, DLC or the documents folder: outside lists \
                   what quen sees by its own park kinds"),
        );
        fields.insert("mods".into(), json!([]));
        fields.insert("outside".into(), json!(outside));
        return Ok(fields);
    };
    let manifests: Vec<Value> = inv
        .manifests
        .iter()
        .map(|m| {
            let mut v = json!(m);
            let deployed = m
                .deployed_ms
                .map(|ms| session::utc_ms(UNIX_EPOCH + Duration::from_millis(ms)));
            if let Some(v) = v.as_object_mut() {
                v.remove("deployed_ms");
                v.insert("deployed".into(), json!(deployed));
            }
            v
        })
        .collect();
    let vortex =
        json!({"manifests": manifests, "links": inv.links(&[]), "running": running().ok()});
    fields.insert("vortex".into(), vortex);
    fields.insert("mods".into(), json!(inv.mods));
    fields.insert("load_order".into(), json!(inv.load_order));
    fields.insert("outside".into(), json!(outside));
    fields.insert("undeployed".into(), json!(inv.undeployed));
    fields.insert("shared".into(), json!(inv.shared));
    if let Some(names) = pair {
        fields.insert("pair".into(), json!(inv.pair(&Disk, names[0], names[1])?));
    }
    Ok(fields)
}
