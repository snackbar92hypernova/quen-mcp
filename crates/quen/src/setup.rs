//! `quen_setup`: every path and tool quen uses, a row each, with its source (default, detected, config),
//! whether the user confirmed it, and its status: `configured`; `configured_missing` (the config names it, but it isn't
//! there or doesn't check out now); `found_not_configured` (detected, or a default, for the user to confirm); or
//! `not_found` (with how_to_get). With `values` and `user_ok`, the values the user agreed to are checked as quen_doctor
//! checks them, then written to the config, its other lines and comments kept; the quen serve this runs in takes them
//! up for every connection (session.rs `Session::reload`). A key in the config file counts as confirmed: only the user
//! (by hand) or this tool (with user_ok) writes one. The agent asks, quen doesn't: it never picks between candidates.
//! `quen setup` is the same in a terminal. With `licence` it shows REDkit's licence dialog instead, on the session
//! thread (data.rs).

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use quen_core::boot;
use quen_core::detect::{self, Install};
use serde_json::{Map, Value, json};

use crate::config::{self, Config};
use crate::doctor::{self, Dep};
use crate::mcp::{self, Msg};
use crate::session::{self, Done, Shared, envelope};
use crate::{profile, relocate, w3, win};

/// The keys `values` may set: every config key but the Python harness's (`harness`, `python`), the game's image name
/// (`game_exe`) and the capture test switch (`force_duplication`).
const SETTABLE: [&str; 18] = [
    "data_root",
    "capture_root",
    "game_dir",
    "documents",
    "presentmon",
    "kb",
    "renodx_addon",
    "devkit",
    "nsys",
    "ngfx",
    "renderdoc",
    "streamline_dev_interposer",
    "tracy_capture",
    "redkit",
    "renodx_build",
    "ngfx_args",
    "known_builds",
    "capture_budget_gb",
];

/// The settable keys whose value is an absolute path.
const PATHS: [&str; 13] = [
    "data_root",
    "capture_root",
    "game_dir",
    "documents",
    "presentmon",
    "kb",
    "nsys",
    "ngfx",
    "renderdoc",
    "streamline_dev_interposer",
    "tracy_capture",
    "redkit",
    "renodx_build",
];

/// The settable keys that are values rather than paths or tools: quen_setup lists them under `values`.
const VALUES: [&str; 4] = [
    "renodx_build",
    "ngfx_args",
    "known_builds",
    "capture_budget_gb",
];

/// The paths the user is asked about on first use, found or not.
const REQUIRED: [&str; 4] = ["data_root", "capture_root", "game_dir", "documents"];

/// The tools quen finds by itself, by config key: where each one's installer puts it under Program Files (a folder per
/// version: every one is listed), and its exe's name on PATH.
pub(crate) const PROGRAMS: [(&str, &str, &str); 4] = [
    (
        "presentmon",
        r"Intel\PresentMon\PresentMonConsoleApplication\PresentMon-*.exe",
        "PresentMon.exe",
    ),
    (
        "nsys",
        r"NVIDIA Corporation\Nsight Systems*\target-windows-x64\nsys.exe",
        "nsys.exe",
    ),
    (
        "ngfx",
        r"NVIDIA Corporation\Nsight Graphics*\host\windows-desktop-nomad-x64\ngfx.exe",
        "ngfx.exe",
    ),
    (
        "renderdoc",
        r"RenderDoc*\renderdoccmd.exe",
        "renderdoccmd.exe",
    ),
];

/// `key`'s entry in [`PROGRAMS`]: its pattern under Program Files and its exe's name.
pub(crate) fn program(key: &str) -> (&'static str, &'static str) {
    let found = PROGRAMS.iter().find(|p| p.0 == key);
    let (_, install, exe) = found.copied().expect("one of PROGRAMS");
    (install, exe)
}

/// The KB's tables, which quen_doctor checks one by one: a folder holding none of them isn't the KB.
const KB_TABLES: [&str; 4] = ["cvar_layout.tsv", "patches.tsv", "names.tsv", "hooks.tsv"];

/// What detection finds on this machine. Tests build their own; [`Machine::read`] asks Windows.
#[derive(Default)]
pub(crate) struct Machine {
    /// Game folders, from Steam's libraries and GOG's registry entries, each holding the game's exe.
    pub(crate) installs: Vec<Install>,
    /// `<Documents>\The Witcher 3`, with Documents from the known-folder API (so a OneDrive one is found).
    pub(crate) documents: Option<PathBuf>,
    /// Each of [`PROGRAMS`]: every exe found where its installer puts it, then on PATH; and `redkit`, REDkit's folders
    /// in Steam's libraries.
    pub(crate) programs: Vec<(&'static str, Vec<PathBuf>)>,
}

impl Machine {
    pub(crate) fn read(game_exe: &str) -> Machine {
        let steam = win::reg_str(false, r"Software\Valve\Steam", "SteamPath")
            .or_else(|| win::reg_str(true, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"))
            .map(|s| PathBuf::from(s.replace('/', "\\")));
        const GOG: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";
        let gog = win::reg_subkeys(GOG)
            .iter()
            .filter_map(|id| {
                let key = format!(r"{GOG}\{id}");
                Some(Install {
                    dir: win::reg_str(true, &key, "path")?.into(),
                    platform: "gog",
                    build: win::reg_str(true, &key, "ver"),
                })
            })
            .collect();
        let is_game = |dir: &Path| w3::is_game_dir(dir, game_exe);
        let program_files = env::var_os("ProgramFiles").map(PathBuf::from);
        let mut programs = programs(program_files.as_deref(), env::var_os("PATH"));
        let redkit = steam
            .as_deref()
            .map(|s| detect::steam(s, crate::data::REDKIT_APPID));
        let redkit = redkit.unwrap_or_default().into_iter().map(|i| i.dir);
        programs.push((
            "redkit",
            redkit
                .filter(|d| d.join(crate::data::WCC_LITE).is_file())
                .collect(),
        ));
        Machine {
            installs: detect::installs(steam.as_deref(), boot::STEAM_APPID, gog, is_game),
            documents: win::documents().map(|d| d.join(w3::DOCUMENTS)),
            programs,
        }
    }

    /// What detection found for `key`, every one.
    fn found(&self, key: &str) -> Vec<String> {
        let found = self.programs.iter().filter(|(k, _)| *k == key);
        found
            .flat_map(|(_, exes)| exes)
            .map(|p| p.display().to_string())
            .collect()
    }
}

/// Each of [`PROGRAMS`] where its installer puts it under `program_files`, then on the PATH value `path`: every one.
pub(crate) fn programs(
    program_files: Option<&Path>,
    path: Option<OsString>,
) -> Vec<(&'static str, Vec<PathBuf>)> {
    let each = PROGRAMS.iter().map(|&(key, install, exe)| {
        let mut found = program_files
            .map(|pf| detect::installed(pf, install))
            .unwrap_or_default();
        if let Some(on_path) = doctor::on_path(path.clone(), exe)
            && !found.contains(&on_path)
        {
            found.push(on_path);
        }
        (key, found)
    });
    each.collect()
}

/// The RenoDX add-ons and devkits in the game's `bin` folder (devkits in its subfolders too), as the config names
/// them: relative to `bin`. Every one is listed.
fn addons(bin: &Path) -> (Vec<String>, Vec<String>) {
    let rel = |p: PathBuf| p.strip_prefix(bin).unwrap_or(&p).display().to_string();
    let devkit = |p: &PathBuf| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        name.to_lowercase().starts_with("renodx-devkit")
    };
    let (devkits, addons): (Vec<PathBuf>, Vec<PathBuf>) =
        detect::installed(bin, "renodx-*.addon64")
            .into_iter()
            .partition(devkit);
    let deeper = detect::installed(bin, r"*\renodx-devkit*.addon64");
    (
        addons.into_iter().map(rel).collect(),
        devkits.into_iter().chain(deeper).map(rel).collect(),
    )
}

/// The config file as it is on disk now, which may be newer than the one this quen read at its start.
pub(crate) struct OnDisk {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    /// What it says over the defaults; the defaults alone when it can't be read (`error` says why).
    pub(crate) config: Config,
    pub(crate) error: Option<String>,
}

impl OnDisk {
    pub(crate) fn read() -> Result<OnDisk, String> {
        OnDisk::at(config::path()?)
    }

    fn at(path: PathBuf) -> Result<OnDisk, String> {
        let text = config::read(&path)?;
        let (config, error) = match config::parse(&text, &path) {
            Ok(config) => (config, None),
            Err(e) => (config::parse("", &path)?, Some(e)),
        };
        Ok(OnDisk {
            path,
            text,
            config,
            error,
        })
    }
}

/// The platform of the game folder `dir`: a detected install's, else `gog` by GOG's marker file; `None` when neither
/// says (a copied folder, say).
pub(crate) fn platform(dir: &Path, installs: &[Install]) -> Option<&'static str> {
    let key = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    let found = installs.iter().find(|i| key(&i.dir) == key(dir));
    found
        .map(|i| i.platform)
        .or_else(|| w3::is_gog(dir).then_some("gog"))
}

fn entry(
    key: &str,
    path: Option<&Path>,
    source: &str,
    confirmed: bool,
    problem: Option<String>,
) -> Value {
    json!({"key": key, "path": path, "source": source, "confirmed": confirmed, "problem": problem})
}

/// A row's status: the config names it (and it checks out, or not), or detection or a default has one to confirm.
fn status(configured: bool, fine: bool, found: bool) -> &'static str {
    match (configured, fine, found) {
        (true, true, _) => "configured",
        (true, false, _) => "configured_missing",
        (false, _, true) => "found_not_configured",
        (false, _, false) => "not_found",
    }
}

/// What detection found for a key, every one; Err: why quen didn't look.
type Found<'a> = Result<Vec<String>, &'a str>;

/// A tool's row: the config's value, checked as quen_doctor checks it ([`usable`]); else what detection found. One
/// found fills `path`; several are for the user to choose from.
fn tool_row(key: &str, value: Option<&str>, c: &Config, dep: &Dep, found: Found) -> Value {
    let checked = value.map(|_| usable(key, c));
    let candidates = found.clone().unwrap_or_default();
    let (path, source, problem) = match (value, &checked) {
        (Some(v), Some(checked)) => (Some(v.to_owned()), Some("config"), checked.clone().err()),
        _ => match (&found, candidates.as_slice()) {
            (Err(why), _) => (None, None, Some(why.to_string())),
            (_, []) => (None, None, None),
            (_, [one]) => (Some(one.clone()), Some("detected"), None),
            (_, more) => (
                None,
                Some("detected"),
                Some(format!(
                    "{} found: ask the user which one (quen never picks)",
                    more.len()
                )),
            ),
        },
    };
    let fine = checked.as_ref().is_some_and(Result::is_ok);
    let status = status(value.is_some(), fine, !candidates.is_empty());
    let how_to_get = matches!(status, "not_found" | "configured_missing").then_some(dep.how_to_get);
    json!({"key": key, "path": path, "source": source, "confirmed": value.is_some(), "status": status,
           "problem": problem, "candidates": candidates, "how_to_get": how_to_get})
}

/// quen_setup's fields: a row per path and tool, the values, and the keys to confirm with the user.
fn report(disk: &OnDisk, machine: &Machine) -> Map<String, Value> {
    let c = &disk.config;
    let pairs = config::pairs(&disk.text);
    // A config that doesn't parse sets nothing: quen runs on the defaults (feature tests 1: its keys showed confirmed).
    let value = |key: &str| {
        let set = pairs.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v);
        set.filter(|_| disk.error.is_none())
    };
    let has = |key: &str| value(key).is_some();
    let source = |key: &str| if has(key) { "config" } else { "default" };
    let roots = config::check_roots(c).err();
    let root_problem = |name: &str| roots.clone().filter(|e| e.starts_with(name));
    let file_source = if env::var_os("QUEN_CONFIG").is_some() {
        "QUEN_CONFIG"
    } else {
        "default"
    };
    let mut paths = vec![
        entry(
            "config_file",
            Some(&disk.path),
            file_source,
            disk.path.is_file(),
            disk.error.clone(),
        ),
        entry(
            "data_root",
            Some(&c.data_root),
            source("data_root"),
            has("data_root"),
            root_problem("data root"),
        ),
        entry(
            "state",
            Some(&c.state_dir()),
            "data_root",
            has("data_root"),
            None,
        ),
        entry(
            "logs",
            Some(&c.data_root.join("logs")),
            "data_root",
            has("data_root"),
            None,
        ),
        entry(
            "capture_root",
            Some(&c.capture_root()),
            source("capture_root"),
            has("capture_root"),
            root_problem("capture root"),
        ),
    ];
    // The defaults are there to confirm.
    paths[1]["status"] = json!(status(has("data_root"), true, true));
    paths[4]["status"] = json!(status(has("capture_root"), true, true));

    let installs = machine.installs.as_slice();
    let (game, from) = match (&c.game_dir, installs) {
        (Some(dir), _) => (Some(dir.clone()), "config"),
        (None, [one]) => (Some(one.dir.clone()), "detected"),
        (None, _) => (None, "detected"),
    };
    let problem = match &game {
        Some(dir) if !w3::is_game_dir(dir, &c.game_exe) => {
            Some(format!("no bin\\x64_dx12\\{} in it", c.game_exe))
        }
        Some(_) => None,
        None if installs.is_empty() => Some(
            "not found in Steam's libraries or GOG's entries: ask the user where the game is"
                .into(),
        ),
        None => Some(format!(
            "{} installs found: ask the user which one",
            installs.len()
        )),
    };
    let platform = game.as_deref().and_then(|dir| platform(dir, installs));
    let fine = problem.is_none();
    let mut row = entry("game_dir", game.as_deref(), from, has("game_dir"), problem);
    row["status"] = json!(status(has("game_dir"), fine, !installs.is_empty()));
    row["how_to_get"] =
        json!((!has("game_dir") && installs.is_empty()).then_some(doctor::GAME.how_to_get));
    row["platform"] = json!(platform);
    row["platform_status"] = json!(platform.map(detect::status));
    let gog_note = |p: &str| (p == "gog").then_some(w3::GOG_UNTESTED);
    row["note"] = json!(platform.and_then(gog_note));
    row["candidates"] = installs
        .iter()
        .map(|i| {
            json!({"path": i.dir, "platform": i.platform, "status": i.status(), "build": i.build,
                   "note": gog_note(i.platform)})
        })
        .collect();
    paths.push(row);

    let (docs, from) = match &c.documents {
        Some(dir) => (Some(dir.clone()), "config"),
        None => (machine.documents.clone(), "detected"),
    };
    let there = docs.as_ref().is_some_and(|dir| dir.is_dir());
    let problem = match &docs {
        Some(_) if !there => Some("not there: start the game once, it creates it".into()),
        Some(_) => None,
        None => Some("Documents wasn't found: ask the user".into()),
    };
    let mut row = entry(
        "documents",
        docs.as_deref(),
        from,
        has("documents"),
        problem,
    );
    row["status"] = json!(status(has("documents"), there, there));
    paths.push(row);

    // The tools: the config's value, else what detection finds where installers put them, or in the game folder.
    let bin = game.as_deref().map(boot::bin_dir);
    let (renodx, devkits) = match &bin {
        Some(bin) => {
            let (addons, devkits) = addons(bin);
            (Ok(addons), Ok(devkits))
        }
        None => {
            let why = "no game folder yet: confirm game_dir first";
            (Err(why), Err(why))
        }
    };
    let named = || Err("quen doesn't search for it: the user names it");
    let found = |key: &str| Ok(machine.found(key));
    let tools: [(&str, &Dep, Found); 10] = [
        ("presentmon", &doctor::PRESENTMON, found("presentmon")),
        ("kb", &doctor::KB_CVARS, named()),
        ("renodx_addon", &doctor::RENODX, renodx),
        ("devkit", &doctor::DEVKIT, devkits),
        ("nsys", &doctor::NSIGHT_SYSTEMS, found("nsys")),
        ("ngfx", &doctor::NSIGHT_GRAPHICS, found("ngfx")),
        ("renderdoc", &doctor::RENDERDOC, found("renderdoc")),
        (
            "streamline_dev_interposer",
            &doctor::STREAMLINE_DEV,
            named(),
        ),
        ("tracy_capture", &doctor::TRACY, named()),
        ("redkit", &doctor::REDKIT, found("redkit")),
    ];
    for (key, dep, found) in tools {
        paths.push(tool_row(key, value(key), c, dep, found));
    }
    // ReShade has no key: quen_doctor finds it in the game folder, where it hosts the RenoDX add-on and devkit.
    let reshade = game
        .as_deref()
        .map(|dir| doctor::check(&doctor::RESHADE, c, Some(dir)));
    let reshade_found = reshade.as_ref().is_some_and(|s| s.status == "found");
    paths.push(json!({
        "key": "reshade",
        "path": reshade.as_ref().and_then(|s| s.path.clone()),
        "source": "detected",
        "status": if reshade_found { "found" } else { "not_found" },
        "problem": reshade.and_then(|s| s.note),
        "note": "no config key: the RenoDX add-on and the devkit load through ReShade in bin\\x64_dx12",
        "how_to_get": (!reshade_found).then_some(doctor::RESHADE.how_to_get),
    }));

    let values: Vec<Value> = VALUES
        .iter()
        .map(|&key| {
            let v = value(key);
            let default = (key == "capture_budget_gb").then(|| c.capture_budget_gb().to_string());
            let source = match (v, &default) {
                (Some(_), _) => Some("config"),
                (None, Some(_)) => Some("default"),
                (None, None) => None,
            };
            json!({"key": key, "value": v.map(String::from).or(default), "source": source,
                   "confirmed": v.is_some(), "problem": v.and_then(|_| usable(key, c).err())})
        })
        .collect();

    let unconfirmed: Vec<&Value> = paths
        .iter()
        .filter(|row| row["confirmed"] == false)
        .filter(|row| {
            let key = row["key"].as_str().unwrap_or_default();
            REQUIRED.contains(&key) || row["status"] == "found_not_configured"
        })
        .map(|row| &row["key"])
        .collect();
    let unconfirmed = json!(unconfirmed);
    Map::from_iter([
        ("paths".into(), json!(paths)),
        ("values".into(), json!(values)),
        ("unconfirmed".into(), unconfirmed),
    ])
}

/// What the config file can hold for `key`: an absolute path for a path, and no `'` or line break in any value.
fn check_text(key: &str, value: &str) -> Result<(), String> {
    if PATHS.contains(&key) && !Path::new(value).is_absolute() {
        return Err(format!("{key} {value:?} is not an absolute path"));
    }
    if value.contains(['\'', '\n', '\r', '\0']) {
        return Err(format!(
            "{key} {value:?}: quen's config can't hold a ' or a line break in a value"
        ));
    }
    Ok(())
}

/// Whether `key`'s value in `c` checks out as quen_doctor checks it; Err says why not. quen_setup refuses such a value
/// before writing anything, and shows a configured one as `configured_missing`. The roots, `known_builds` and
/// `capture_budget_gb` are checked when the config is parsed.
fn usable(key: &str, c: &Config) -> Result<(), String> {
    let doctor = |dep: &Dep| {
        let state = doctor::check(dep, c, c.game_dir.as_deref());
        match state.status {
            "found" => Ok(()),
            status => Err(format!(
                "{key}: {}",
                state.note.unwrap_or_else(|| status.into())
            )),
        }
    };
    let exe = |path: &Option<PathBuf>, name: &str| match path {
        Some(p) if !p.is_file() => Err(format!("{key} {} is not a file", p.display())),
        Some(p) if !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) => {
            Err(format!("{key} {} is not {name}", p.display()))
        }
        _ => Ok(()),
    };
    match key {
        "game_dir" => match &c.game_dir {
            Some(dir) if !w3::is_game_dir(dir, &c.game_exe) => Err(format!(
                "game_dir {}: no bin\\x64_dx12\\{} in it, so it isn't the game folder",
                dir.display(),
                c.game_exe
            )),
            _ => Ok(()),
        },
        "documents" => match &c.documents {
            Some(dir) if !dir.is_dir() => Err(format!(
                "documents {} is not a folder (the game creates it at its first start)",
                dir.display()
            )),
            _ => Ok(()),
        },
        "presentmon" => doctor(&doctor::PRESENTMON),
        "nsys" => exe(&c.nsys, "nsys.exe"),
        "ngfx" => exe(&c.ngfx, "ngfx.exe"),
        "renderdoc" => exe(&c.renderdoc, "renderdoccmd.exe"),
        "streamline_dev_interposer" => doctor(&doctor::STREAMLINE_DEV),
        "tracy_capture" => doctor(&doctor::TRACY),
        "redkit" => doctor(&doctor::REDKIT),
        "kb" => match &c.kb {
            Some(kb) if !kb.is_dir() => Err(format!("kb {} is not a folder", kb.display())),
            Some(kb) if !KB_TABLES.iter().any(|t| kb.join(t).is_file()) => Err(format!(
                "kb {} holds none of the KB's tables ({}): not the KB",
                kb.display(),
                KB_TABLES.join(", ")
            )),
            Some(kb) => doctor::kb_build(kb).map(|_| ()),
            None => Ok(()),
        },
        "renodx_addon"
            if c.renodx_addon
                .as_deref()
                .is_some_and(|n| n.contains(['\\', '/'])) =>
        {
            Err("renodx_addon is the add-on's file name in bin\\x64_dx12, not a path".into())
        }
        "renodx_build" if c.renodx_addon.is_none() => {
            Err("renodx_build goes with renodx_addon: set that too".into())
        }
        "renodx_build" if !c.renodx_build.as_ref().is_some_and(|b| b.is_file()) => {
            Err("renodx_build is not a file".into())
        }
        "renodx_addon" | "renodx_build" => doctor(&doctor::RENODX),
        "devkit"
            if c.devkit
                .as_deref()
                .is_some_and(|d| Path::new(d).is_absolute()) =>
        {
            Err(
                "devkit is relative to bin\\x64_dx12, as ReShade.ini's LoadFromDllMain takes it"
                    .into(),
            )
        }
        "devkit" => doctor(&doctor::DEVKIT),
        "ngfx_args" => profile::ngfx_args(c.ngfx_args.as_deref().unwrap_or_default()),
        _ => Ok(()),
    }
}

/// The report, or with `values` and `user_ok` the config rewritten first. Refused, nothing written, on any bad value.
fn run(args: &Value, disk: OnDisk, machine: &dyn Fn(&str) -> Machine) -> Done {
    let Some(map) = args.as_object() else {
        return "arguments must be an object".to_string().into();
    };
    if let Some(key) = map
        .keys()
        .find(|k| !["values", "user_ok"].contains(&k.as_str()))
    {
        return format!("unknown argument {key:?}").into();
    }
    let user_ok = match map.get("user_ok") {
        None => false,
        Some(v) => match v.as_bool() {
            Some(ok) => ok,
            None => return "user_ok must be true or false".to_string().into(),
        },
    };
    let mut values = Vec::new();
    for (key, value) in map
        .get("values")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if !SETTABLE.contains(&key.as_str()) {
            return format!("values: {key:?} is not one of {SETTABLE:?}").into();
        }
        let Some(value) = value.as_str() else {
            return format!("values: {key} must be a string").into();
        };
        values.push((key.as_str(), value));
    }
    if map.get("values").is_some_and(|v| !v.is_object()) {
        return "values must be an object of paths and values"
            .to_string()
            .into();
    }
    if values.is_empty() {
        if user_ok {
            return "user_ok with no values: pass every value the user agreed to, defaults included"
                .to_string()
                .into();
        }
        return Done::Ok(report(&disk, &machine(&disk.config.game_exe)));
    }
    if !user_ok {
        return "values are written only with user_ok: true, after the user agreed to them in this \
                conversation; nothing written"
            .to_string()
            .into();
    }
    if let Some(e) = &disk.error {
        return format!("the config can't be read, so quen won't rewrite it: {e}").into();
    }
    for (key, value) in &values {
        if let Err(why) = check_text(key, value) {
            return why.into();
        }
    }
    let text = config::with_values(&disk.text, &values);
    let new = match config::loaded(&text, &disk.path) {
        Ok(new) => new,
        Err(why) => return format!("{why}; nothing written").into(),
    };
    for (key, _) in &values {
        if let Err(why) = usable(key, &new) {
            return format!("{why}; nothing written").into();
        }
    }
    // The lock and the journal live in the data root, so a new one would strand a session's journal there,
    // and the next launch would back up quen's patched files as the user's.
    let dir = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    if dir(&new.data_root) != dir(&disk.config.data_root) {
        if session::journal_path(&disk.config).exists() {
            return format!(
                "{} holds a session's journal: end the session or session_recover first, then change data_root; \
                 nothing written",
                session::journal_path(&disk.config).display()
            )
            .into();
        }
        if session::locked_elsewhere(&disk.config) {
            return "another quen holds the game lock in the current data root (a session is live there): end it \
                    first, then change data_root; nothing written"
                .to_string()
                .into();
        }
    }
    // A failed write may leave config.toml.quen-tmp beside the config.
    if let Err(why) = config::write(&disk.path, &text) {
        let fields = Map::from_iter([("config".into(), json!(disk.path))]);
        return Done::Failed(why, fields);
    }
    let keys: Vec<&str> = values.iter().map(|(k, _)| *k).collect();
    let disk = OnDisk {
        text,
        config: new,
        error: None,
        ..disk
    };
    let mut fields = report(&disk, &machine(&disk.config.game_exe));
    fields.insert("written".into(), json!(keys));
    Done::Ok(fields)
}

/// Whether the config just written is in use by every connection now (`reloaded`), and if not, when it will be:
/// `daemon` is the session thread of the quen serve this runs in, which takes it up (`Session::reload`).
fn applies(shared: &Shared, daemon: Option<&Sender<Msg>>) -> (bool, String) {
    let Some(tx) = daemon else {
        return (
            false,
            "a quen serve already running takes it up for every connection at its next connection or session tool \
             call while no session is live; one started from now on reads it"
                .into(),
        );
    };
    if shared.live() {
        return (false, session::RELOAD_AFTER_SESSION.into());
    }
    match mcp::reload(tx) {
        Some(Ok(())) => (
            true,
            "every connection to this quen uses it now, with no reconnect".into(),
        ),
        Some(Err(why)) => (false, why),
        None => (
            false,
            "this quen is running another call: it takes the new config up for every connection after that call, \
             or once the session that call starts ends"
                .into(),
        ),
    }
}

/// `move`: the one-time move of an older quen's folders into `%USERPROFILE%\.quen` (relocate.rs): what moves, or with
/// `user_ok` the move, refused while this quen has a session or holds the game lock, which it can't take meanwhile.
fn moved(shared: &Shared, args: &Value) -> Done {
    let extra = args.as_object().and_then(|a| {
        a.keys()
            .find(|k| !["move", "user_ok"].contains(&k.as_str()))
    });
    if let Some(key) = extra {
        return format!("{key:?} doesn't go with move").into();
    }
    if args["move"] != true {
        return "move must be true".to_string().into();
    }
    let places = match relocate::Places::here() {
        Ok(places) => places,
        Err(why) => return why.into(),
    };
    let held = shared.lock_guard();
    let busy = (shared.live() || held.is_some())
        .then_some("this quen has a live session or holds the game lock: end it first");
    relocate::run(&places, args.get("user_ok") == Some(&json!(true)), busy)
}

/// `quen_setup` over MCP (`daemon`: its quen serve's session thread) or the CLI, on the config on disk.
pub fn call(shared: &Shared, args: &Value, daemon: Option<&Sender<Msg>>) -> Value {
    let started = Instant::now();
    let mut done = match OnDisk::read() {
        _ if args.get("move").is_some() => moved(shared, args),
        Ok(disk) => run(args, disk, &Machine::read),
        Err(why) => Done::Refused(why),
    };
    if let Done::Ok(fields) = &mut done
        && fields.contains_key("written")
    {
        let (reloaded, applies) = applies(shared, daemon);
        fields.insert("reloaded".into(), json!(reloaded));
        fields.insert("applies".into(), json!(applies));
    }
    let secs = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let timing = Map::from_iter([("quen_setup".to_owned(), json!(secs))]);
    envelope(shared, "quen_setup", timing, Vec::new(), Map::new(), done)
}

pub fn tool() -> Value {
    let s = |description: &str| json!({"type": "string", "description": description});
    json!({
        "name": "quen_setup",
        "description": "quen's paths and the tools it uses, read-only unless values and user_ok are given. paths: a \
            row each, with source, confirmed (the config names it), status (configured; configured_missing: named \
            but not there or not usable now; found_not_configured: detected or a default; not_found, with \
            how_to_get), problem and candidates (every one found: quen never picks). The reshade row has no key. \
            values: renodx_build, ngfx_args, known_builds, capture_budget_gb. First use: show the user the \
            unconfirmed rows, defaults filled in, and ask once; several candidates: ask which. GOG is untested: tell \
            the user its note first. When the user installed a tool, run it again and confirm its new rows the same \
            way. Then call with values (every one agreed) and user_ok: true: each is checked as quen_doctor checks \
            it, and any problem is refused with nothing written. reloaded / applies: whether every connection uses \
            the new config now (no reconnect), or when it will (after a live session ends). licence: REDkit's \
            licence dialog.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "values": {
                    "type": "object",
                    "description": "what the user agreed to (with user_ok); paths absolute",
                    "properties": {
                        "data_root": s("quen's folder: ledger, journal, lock, logs"),
                        "capture_root": s("where captures go"),
                        "game_dir": s("the game folder (bin\\x64_dx12\\witcher3.exe in it)"),
                        "documents": s("the game's documents folder"),
                        "presentmon": s("PresentMon 2.x's console exe"),
                        "kb": s("the RE knowledge base folder"),
                        "renodx_addon": s("the RenoDX add-on's file name in bin\\x64_dx12"),
                        "devkit": s("the devkit add-on, relative to bin\\x64_dx12"),
                        "nsys": s("Nsight Systems' nsys.exe"),
                        "ngfx": s("Nsight Graphics' ngfx.exe"),
                        "renderdoc": s("RenderDoc's renderdoccmd.exe"),
                        "streamline_dev_interposer": s("the patched dev sl.interposer.dll (unsigned)"),
                        "tracy_capture": s("Tracy 0.14.1's tracy-capture.exe"),
                        "redkit": s("The Witcher 3 REDkit's folder (bin\\x64_RedKit\\wcc_lite.exe in it)"),
                        "renodx_build": s("with renodx_addon: the .addon64 a launch requires installed"),
                        "ngfx_args": s("arguments for every ngfx launch, never --platform"),
                        "known_builds": s("verified game builds: '<steam|gog> <sha256> <label>', ';' between"),
                        "capture_budget_gb": s("GB of captures before quen warns: a number above 0"),
                    },
                    "additionalProperties": false,
                },
                "user_ok": {"type": "boolean", "description": "true only after the user agreed to these values (or the move) in this conversation"},
                "licence": {"type": "boolean", "description": "true, alone: opens The Witcher 3 REDkit's own licence dialog (CD PROJEKT RED's) for the user to answer: tell the user first that it opens and that the choice is theirs. quen runs wcc_lite once without -agreetoterms and with no command, brings the dialog to the front and waits up to 600 s; it never clicks it and never writes the answer, which REDkit keeps (DoNotShowEula under HKCU\\Software\\CD Projekt RED\\Mod Tools\\1.0). Result: accepted and do_not_show_eula (1: Agree), dialog_shown, dialog_in_front, exit_code: tell the user the answer. script_compile runs only after an Agree. The documents folder's settings, which wcc_lite rewrites, are backed up first and put back after, journaled, and it is refused as script_compile is (needs without REDkit)"},
                "move": {"type": "boolean", "description": "true: the one-time move of an older quen's config and data (AppData, or an app's private copy of it) into %USERPROFILE%\\.quen. Without user_ok, what moves; with it, every file copied and checked by sha256, then the old folders renamed quen.moved-<date>. Nothing is deleted"},
            },
            "additionalProperties": false,
        },
    })
}

/// `quen setup [KEY=VALUE ...] [--user-ok]`: the same, printed as JSON; exit code 0 unless refused.
pub fn cli(args: &[String]) -> i32 {
    let mut values = Map::new();
    let mut user_ok = false;
    for arg in args {
        match arg.split_once('=') {
            _ if arg == "--user-ok" => user_ok = true,
            _ if arg == "--move" => {
                values.insert("--move".into(), json!(true));
            }
            Some((key, value)) => {
                values.insert(key.into(), json!(value));
            }
            None => {
                eprintln!(
                    "usage: quen setup [KEY=VALUE ... | --move] [--user-ok]   KEY: {}",
                    SETTABLE.join(", ")
                );
                return 2;
            }
        }
    }
    let mut args = json!({});
    if values.remove("--move").is_some() {
        args["move"] = json!(true);
    }
    if !values.is_empty() {
        args["values"] = Value::Object(values);
    }
    if user_ok {
        args["user_ok"] = json!(true);
    }
    let out = call(&Shared::new(config::load()), &args, None);
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    i32::from(out["ok"] != true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A temp folder for one test, gone afterwards (only what the test made in it).
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = env::temp_dir().join(format!("quen-setup-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn game(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            fs::create_dir_all(dir.join("bin").join("x64_dx12")).unwrap();
            fs::write(dir.join("bin").join("x64_dx12").join("witcher3.exe"), "MZ").unwrap();
            dir
        }

        fn put(&self, rel: &str, data: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, data).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    fn install(dir: &Path, platform: &'static str) -> Install {
        Install {
            dir: dir.to_owned(),
            platform,
            build: None,
        }
    }

    fn path<'v>(fields: &'v Map<String, Value>, key: &str) -> &'v Value {
        let paths = fields["paths"].as_array().unwrap();
        paths.iter().find(|p| p["key"] == key).unwrap()
    }

    fn fields(done: Done) -> Map<String, Value> {
        match done {
            Done::Ok(fields) => fields,
            Done::Refused(why)
            | Done::RefusedWith(why, _)
            | Done::Needs(why, _)
            | Done::Failed(why, _) => panic!("{why}"),
        }
    }

    fn refused(done: Done) -> String {
        match done {
            Done::Refused(why) => why,
            _ => panic!("not refused"),
        }
    }

    fn nothing(_: &str) -> Machine {
        Machine::default()
    }

    #[test]
    fn first_run_shows_defaults_and_detected_paths_unconfirmed() {
        let t = Temp::new("first");
        let steam = t.game("steam game");
        let docs = t.0.join("Documents").join("The Witcher 3");
        fs::create_dir_all(&docs).unwrap();
        let machine = |_: &str| Machine {
            installs: vec![install(&steam, "steam")],
            documents: Some(docs.clone()),
            ..Machine::default()
        };
        let disk = OnDisk::at(t.0.join("quen").join("config.toml")).unwrap();
        let f = fields(run(&json!({}), disk, &machine));
        let unconfirmed = json!(["data_root", "capture_root", "game_dir", "documents"]);
        assert_eq!(f["unconfirmed"], unconfirmed);
        let game = path(&f, "game_dir");
        assert_eq!(
            (&game["path"], &game["source"], &game["status"]),
            (
                &json!(steam),
                &json!("detected"),
                &json!("found_not_configured")
            )
        );
        assert_eq!(game["problem"], Value::Null);
        assert_eq!(game["candidates"][0]["platform"], "steam");
        assert_eq!(path(&f, "documents")["path"], json!(docs));
        assert_eq!(path(&f, "data_root")["source"], "default");
        assert_eq!(path(&f, "data_root")["status"], "found_not_configured");
        assert_eq!(path(&f, "config_file")["confirmed"], false);
        // Nothing else found: each tool says where to get it, and isn't asked about.
        let pm = path(&f, "presentmon");
        assert_eq!(pm["status"], "not_found");
        assert_eq!(pm["how_to_get"], doctor::PRESENTMON.how_to_get);
        let kb = path(&f, "kb");
        assert_eq!(
            kb["problem"],
            "quen doesn't search for it: the user names it"
        );
        assert_eq!(path(&f, "reshade")["status"], "not_found");
        let budget = &f["values"].as_array().unwrap()[3];
        assert_eq!(
            (&budget["key"], &budget["value"], &budget["source"]),
            (&json!("capture_budget_gb"), &json!("20"), &json!("default"))
        );
    }

    #[test]
    fn two_installs_are_candidates_and_never_picked() {
        let t = Temp::new("two");
        let (a, b) = (t.game("5.01"), t.game("4.04"));
        let machine = |_: &str| Machine {
            installs: vec![install(&a, "steam"), install(&b, "gog")],
            ..Machine::default()
        };
        let disk = OnDisk::at(t.0.join("config.toml")).unwrap();
        let f = fields(run(&json!({}), disk, &machine));
        let game = path(&f, "game_dir");
        assert_eq!(game["path"], Value::Null);
        assert!(game["problem"].as_str().unwrap().contains("2 installs"));
        assert_eq!(game["candidates"].as_array().unwrap().len(), 2);
        // Without a game folder, nothing in it is searched for.
        let addon = path(&f, "renodx_addon");
        assert_eq!(addon["status"], "not_found");
        assert!(
            addon["problem"]
                .as_str()
                .unwrap()
                .contains("game_dir first")
        );
        // The GOG one carries its label, for the agent to tell the user before they pick it.
        let (steam, gog) = (&game["candidates"][0], &game["candidates"][1]);
        assert_eq!(
            (&steam["status"], &steam["note"]),
            (&json!("tested"), &Value::Null)
        );
        assert_eq!(
            (&gog["platform"], &gog["status"]),
            (&json!("gog"), &json!("untested"))
        );
        assert!(gog["note"].as_str().unwrap().contains("Steam 5.01 only"));
    }

    #[test]
    fn a_gog_game_folder_is_labelled_untested() {
        let t = Temp::new("gog");
        let gog = t.game("gog game");
        let machine = |_: &str| Machine {
            installs: vec![install(&gog, "gog")],
            ..Machine::default()
        };
        let disk = OnDisk::at(t.0.join("config.toml")).unwrap();
        let f = fields(run(&json!({}), disk, &machine));
        let game = path(&f, "game_dir");
        assert_eq!(
            (&game["path"], &game["source"]),
            (&json!(gog), &json!("detected"))
        );
        assert_eq!(
            (&game["platform"], &game["platform_status"]),
            (&json!("gog"), &json!("untested"))
        );
        assert!(game["note"].as_str().unwrap().contains("untested"));
        // A folder from the config, found by GOG's marker file rather than the registry.
        let copy = t.game("copied");
        fs::write(copy.join("goggame-1.info"), "{}").unwrap();
        let cfg = t.0.join("gog.toml");
        fs::write(&cfg, format!("game_dir = '{}'\n", copy.display())).unwrap();
        let f = fields(run(&json!({}), OnDisk::at(cfg).unwrap(), &nothing));
        assert_eq!(path(&f, "game_dir")["platform_status"], "untested");
        assert_eq!(path(&f, "game_dir")["status"], "configured");
    }

    #[test]
    fn confirmed_values_are_written_keeping_other_keys_and_comments() {
        let t = Temp::new("write");
        let game = t.game("game");
        let docs = t.0.join("docs");
        fs::create_dir_all(&docs).unwrap();
        let cfg = t.0.join("config.toml");
        let before = "# quen's local config\r\npython = 'py'   # kept\r\n# game_dir = 'C:\\old'\r\n\
                      game_dir = 'C:\\elsewhere'  # the game\r\n";
        fs::write(&cfg, before).unwrap();
        let data = t.0.join("data");
        let values = json!({"game_dir": game, "documents": docs, "data_root": data});
        // Without user_ok: refused, and the file is as it was.
        let why = refused(run(
            &json!({"values": values}),
            OnDisk::at(cfg.clone()).unwrap(),
            &nothing,
        ));
        assert!(why.contains("user_ok"), "{why}");
        assert_eq!(fs::read_to_string(&cfg).unwrap(), before);
        let args = json!({"values": values, "user_ok": true});
        let f = fields(run(&args, OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert_eq!(f["written"], json!(["data_root", "documents", "game_dir"]));
        let after = fs::read_to_string(&cfg).unwrap();
        let want = format!(
            "# quen's local config\r\npython = 'py'   # kept\r\n# game_dir = 'C:\\old'\r\ngame_dir = '{}'  # the game\r\n\
             data_root = '{}'\r\ndocuments = '{}'\r\n",
            game.display(),
            data.display(),
            docs.display(),
        );
        assert_eq!(after, want);
        // Read back: the written keys are confirmed and come from the config.
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert_eq!(f["unconfirmed"], json!(["capture_root"]));
        for key in ["game_dir", "documents", "data_root", "state"] {
            assert_eq!(path(&f, key)["confirmed"], true, "{key}");
        }
        assert_eq!(path(&f, "game_dir")["source"], "config");
        assert_eq!(path(&f, "game_dir")["status"], "configured");
        assert!(!cfg.with_extension("toml.quen-tmp").exists());
        // A configured folder that has gone since.
        fs::remove_dir_all(&docs).unwrap(); // this test's own folder
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert_eq!(path(&f, "documents")["status"], "configured_missing");
    }

    /// The tools where their installers put them: two Nsight versions each are both listed, and neither picked; one
    /// RenderDoc fills the path. A configured version an update removed shows as missing, beside the one there now.
    #[test]
    fn every_installed_version_is_a_candidate_and_one_is_written_on_request() {
        let t = Temp::new("programs");
        let pf = t.0.join("Program Files");
        // Windows' separators, as detection joins them.
        let put = |rel: &str| t.put(&format!(r"Program Files\{}", rel.replace('/', r"\")), "MZ");
        let nsys = [
            put("NVIDIA Corporation/Nsight Systems 2025.1.1/target-windows-x64/nsys.exe"),
            put("NVIDIA Corporation/Nsight Systems 2025.3.2/target-windows-x64/nsys.exe"),
        ];
        let ngfx = [
            put(
                "NVIDIA Corporation/Nsight Graphics 2024.3.0/host/windows-desktop-nomad-x64/ngfx.exe",
            ),
            put(
                "NVIDIA Corporation/Nsight Graphics 2025.2.0/host/windows-desktop-nomad-x64/ngfx.exe",
            ),
        ];
        let rdc = put("RenderDoc/renderdoccmd.exe");
        let pm = put("Intel/PresentMon/PresentMonConsoleApplication/PresentMon-2.4.1-x64.exe");
        let found = programs(Some(&pf), None);
        let machine = |_: &str| Machine {
            programs: found.clone(),
            ..Machine::default()
        };
        let cfg = t.0.join("config.toml");
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &machine));
        for (key, exes) in [("nsys", &nsys), ("ngfx", &ngfx)] {
            let row = path(&f, key);
            assert_eq!(
                (&row["status"], &row["path"], &row["candidates"]),
                (&json!("found_not_configured"), &Value::Null, &json!(exes)),
                "{key}"
            );
            assert!(
                row["problem"].as_str().unwrap().contains("2 found"),
                "{row}"
            );
        }
        let row = path(&f, "renderdoc");
        assert_eq!(
            (&row["path"], &row["source"]),
            (&json!(rdc), &json!("detected"))
        );
        assert_eq!(path(&f, "presentmon")["path"], json!(pm));
        let asked = f["unconfirmed"].as_array().unwrap();
        for key in ["presentmon", "nsys", "ngfx", "renderdoc"] {
            assert!(asked.contains(&json!(key)), "{key}");
        }
        // "I installed Nsight": the user picked one, and it is written.
        let args = json!({"values": {"nsys": nsys[1]}, "user_ok": true});
        let f = fields(run(&args, OnDisk::at(cfg.clone()).unwrap(), &machine));
        assert_eq!(f["written"], json!(["nsys"]));
        assert_eq!(path(&f, "nsys")["status"], "configured");
        let asked = f["unconfirmed"].as_array().unwrap();
        assert!(!asked.contains(&json!("nsys")));
        // An update removed that version: still named, missing now, with what is installed.
        fs::remove_file(&nsys[1]).unwrap(); // this test's own file
        let machine = |_: &str| Machine {
            programs: programs(Some(&pf), None),
            ..Machine::default()
        };
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &machine));
        let row = path(&f, "nsys");
        assert_eq!(
            (&row["status"], &row["candidates"]),
            (&json!("configured_missing"), &json!([nsys[0]]))
        );
        let problem = row["problem"].as_str().unwrap();
        assert!(problem.contains("not a file"), "{row}");
        assert_eq!(row["how_to_get"], doctor::NSIGHT_SYSTEMS.how_to_get);
    }

    /// ReShade, the RenoDX add-on and its devkit in a game folder: found where they are, written as the config names
    /// them (relative to bin), and two add-ons are both listed.
    #[test]
    fn reshade_renodx_and_the_devkit_are_found_in_the_game_folder() {
        let t = Temp::new("addons");
        let game = t.game("game");
        let bin = boot::bin_dir(&game);
        fs::write(bin.join("dxgi.dll"), "MZ").unwrap();
        fs::write(bin.join("ReShade.ini"), "[GENERAL]\n").unwrap();
        fs::write(bin.join("renodx-thewitcher3remastered.addon64"), "addon").unwrap();
        fs::create_dir_all(bin.join("devkit")).unwrap();
        fs::write(bin.join(r"devkit\renodx-devkit.addon64"), "devkit").unwrap();
        let cfg = t.0.join("config.toml");
        fs::write(&cfg, format!("game_dir = '{}'\n", game.display())).unwrap();
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &nothing));
        let reshade = path(&f, "reshade");
        assert_eq!(
            (&reshade["status"], &reshade["path"]),
            (&json!("found"), &json!(bin.join("dxgi.dll")))
        );
        let addon = path(&f, "renodx_addon");
        assert_eq!(
            (&addon["status"], &addon["path"]),
            (
                &json!("found_not_configured"),
                &json!("renodx-thewitcher3remastered.addon64")
            )
        );
        assert_eq!(path(&f, "devkit")["path"], r"devkit\renodx-devkit.addon64");
        let values = json!({"renodx_addon": "renodx-thewitcher3remastered.addon64",
                            "devkit": r"devkit\renodx-devkit.addon64"});
        let args = json!({"values": values, "user_ok": true});
        let f = fields(run(&args, OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert_eq!(path(&f, "renodx_addon")["status"], "configured");
        assert_eq!(path(&f, "devkit")["status"], "configured");
        // A second add-on build beside it: both listed while the config names neither.
        fs::write(
            bin.join("renodx-thewitcher3remastered-test.addon64"),
            "addon",
        )
        .unwrap();
        let cfg2 = t.0.join("config2.toml");
        fs::write(&cfg2, format!("game_dir = '{}'\n", game.display())).unwrap();
        let f = fields(run(&json!({}), OnDisk::at(cfg2).unwrap(), &nothing));
        let addon = path(&f, "renodx_addon");
        assert_eq!(addon["candidates"].as_array().unwrap().len(), 2, "{addon}");
        assert_eq!(addon["path"], Value::Null);
    }

    /// Every bad value is refused before anything is written, whichever key it is for.
    #[test]
    fn every_bad_value_is_refused_with_nothing_written() {
        let t = Temp::new("bad");
        let game = t.game("game");
        let cfg = t.0.join("config.toml");
        let text = format!("python = 'py'\ngame_dir = '{}'\n", game.display());
        fs::write(&cfg, &text).unwrap();
        let not_game = t.0.join("not a game");
        fs::create_dir_all(&not_game).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        let other_exe = t.put("tools/nsys-ui.exe", "MZ");
        let old_pm = t.put("tools/PresentMon-1.9.2-x64.exe", "MZ");
        let gui = t.put("tools/qrenderdoc.exe", "MZ");
        let empty_kb = t.0.join("empty kb");
        fs::create_dir_all(&empty_kb).unwrap();
        let bad_kb = t.put("bad kb/names.tsv", "# build: sha256=nothex\n");
        let cases = [
            (json!({"game_dir": not_game}), "isn't the game folder"),
            (
                json!({"game_dir": "relative\\game"}),
                "not an absolute path",
            ),
            (json!({"documents": t.0.join("missing")}), "not a folder"),
            (json!({"presentmon": t.0.join("pm.exe")}), "not a file"),
            (json!({"presentmon": old_pm}), "2.x"),
            (json!({"data_root": t.0.join("it's")}), "can't hold a '"),
            (json!({"data_root": source}), "source tree"),
            (json!({"capture_root": "captures"}), "not an absolute path"),
            (json!({"kb": t.0.join("no kb")}), "not a folder"),
            (json!({"kb": empty_kb}), "none of the KB's tables"),
            (json!({"kb": bad_kb.parent().unwrap()}), "names.tsv"),
            (json!({"renodx_addon": "bin\\renodx.addon64"}), "not a path"),
            (
                json!({"renodx_addon": "renodx-none.addon64"}),
                "not in the game folder",
            ),
            (
                json!({"devkit": "devkit\\renodx-devkit.addon64"}),
                "not in the game folder",
            ),
            (
                json!({"devkit": t.0.join("devkit.addon64")}),
                "relative to bin",
            ),
            (json!({"nsys": t.0.join("nsys.exe")}), "not a file"),
            (json!({"nsys": other_exe}), "is not nsys.exe"),
            (json!({"ngfx": other_exe}), "is not ngfx.exe"),
            (json!({"renderdoc": gui}), "is not renderdoccmd.exe"),
            (
                json!({"streamline_dev_interposer": t.0.join("sl.dll")}),
                "not a file",
            ),
            (
                json!({"renodx_build": t.0.join("x.addon64")}),
                "goes with renodx_addon",
            ),
            (json!({"ngfx_args": "--platform windows"}), "--platform"),
            (json!({"known_builds": "epic x y"}), "not steam or gog"),
            (json!({"capture_budget_gb": "lots"}), "above 0"),
            (json!({"capture_budget_gb": "0"}), "above 0"),
            (json!({"capture_budget_gb": 5}), "must be a string"),
            (json!({"harness": "C:\\h"}), "not one of"),
            (json!({"game_exe": "x.exe"}), "not one of"),
            (json!({"force_duplication": "true"}), "not one of"),
        ];
        for (values, want) in cases {
            let args = json!({"values": values, "user_ok": true});
            let why = refused(run(&args, OnDisk::at(cfg.clone()).unwrap(), &nothing));
            assert!(why.contains(want), "{values}: {why}");
            assert_eq!(fs::read_to_string(&cfg).unwrap(), text, "{values}");
        }
        // One bad value among good ones: nothing at all is written.
        let mixed = json!({"capture_budget_gb": "30", "ngfx_args": "--platform x"});
        let why = refused(run(
            &json!({"values": mixed, "user_ok": true}),
            OnDisk::at(cfg.clone()).unwrap(),
            &nothing,
        ));
        assert!(why.contains("--platform"), "{why}");
        assert_eq!(fs::read_to_string(&cfg).unwrap(), text);
        // A config quen can't read isn't rewritten.
        fs::write(&cfg, "bogus = 'x'\n").unwrap();
        let args = json!({"values": {"data_root": t.0.join("d")}, "user_ok": true});
        let why = refused(run(&args, OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert!(why.contains("unknown key"), "{why}");
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "bogus = 'x'\n");
        // Nor are its keys confirmed: quen runs on the defaults (feature tests 1, a drive-relative game_dir).
        fs::write(&cfg, "game_dir = 'C:game'\nbogus = 'x'\n").unwrap();
        let f = fields(run(&json!({}), OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert_eq!(path(&f, "game_dir")["confirmed"], false);
        assert_eq!(f["unconfirmed"], json!(REQUIRED));
        assert!(path(&f, "config_file")["problem"].is_string());
    }

    /// The values that aren't tools: written and read back with their source.
    #[test]
    fn values_are_written_and_listed_with_their_source() {
        let t = Temp::new("values");
        let cfg = t.0.join("config.toml");
        let sha = quen_core::journal::sha256_hex(b"a synthetic build");
        let values = json!({"capture_budget_gb": "35", "ngfx_args": "--architecture \"X Y\" --metric-set-id 0",
                            "known_builds": format!("gog {sha} synthetic (GOG)")});
        let args = json!({"values": values, "user_ok": true});
        let f = fields(run(&args, OnDisk::at(cfg.clone()).unwrap(), &nothing));
        let listed = f["values"].as_array().unwrap();
        let value = |key: &str| listed.iter().find(|v| v["key"] == key).unwrap();
        let budget = value("capture_budget_gb");
        assert_eq!(
            (&budget["value"], &budget["source"]),
            (&json!("35"), &json!("config"))
        );
        assert_eq!(value("ngfx_args")["value"], values["ngfx_args"]);
        assert_eq!(value("known_builds")["confirmed"], true);
        assert_eq!(value("renodx_build")["source"], Value::Null);
        let config = config::parse(&fs::read_to_string(&cfg).unwrap(), &cfg).unwrap();
        assert_eq!(config.capture_budget_gb(), 35.0);
    }

    /// A new data_root would strand a session's journal in the old one, so it is refused while there is one;
    /// the same root, written again, isn't.
    #[test]
    fn data_root_stays_while_its_journal_does() {
        let t = Temp::new("j1");
        let cfg = t.0.join("config.toml");
        let old = t.0.join("data");
        let text = format!("data_root = '{}'\n", old.display());
        fs::write(&cfg, &text).unwrap();
        fs::create_dir_all(old.join("state")).unwrap();
        fs::write(old.join(r"state\journal.jsonl"), "").unwrap();
        let moved = json!({"values": {"data_root": t.0.join("elsewhere")}, "user_ok": true});
        let why = refused(run(&moved, OnDisk::at(cfg.clone()).unwrap(), &nothing));
        assert!(why.contains("session's journal"), "{why}");
        assert_eq!(fs::read_to_string(&cfg).unwrap(), text);
        let same = json!({"values": {"data_root": old}, "user_ok": true});
        fields(run(&same, OnDisk::at(cfg.clone()).unwrap(), &nothing));
    }
}
