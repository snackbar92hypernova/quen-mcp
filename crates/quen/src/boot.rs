//! The native boot, ported from the harness's `ini_session.py boot`: the checked launch spec and
//! its file plan (quen-core's [`boot`](quen_core::boot)), the command that starts the game in the session's job, and the
//! in-game part: the game's window, ReShade's add-on line, the save asked for loaded at the main menu through the
//! command channel, or Continue (one key) and then that save from gameplay (game_load) when Continue loaded another,
//! and photo mode. Not ported,
//! on purpose: the menu and photo-UI OCR (Continue and modQuen replace the Load Game list; hiding the photo UI needs a
//! check OCR gave), the ORB scene check (its references were shots in the SKIV library: quen's own capture is its only
//! capture path, and nothing here binds, presses or reads SKIV's or ReShade's screenshot keys), the harness's test mod
//! and W3_SIGNALS pack, and the 4.04 / vcmp switching. ReShade is only the RenoDX add-on's host.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::boot::{self, Change, Inputs, RenoDx};
use quen_core::bridge_log;
use quen_core::journal::sha256_hex;
use quen_core::park::{self, Entry, Learned, Listing};
use quen_core::saves;
use quen_core::vortex::Inventory;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::profile::{Profile, Tool};
use crate::session::{Call, Done, JournalFs, Session, outside};
use crate::{capture, channel, doctor, game, profile, w3, win};

/// The boot switches `extra_env` may carry, as the harness named them.
const EXTRA_ENV: &[&str] = &[
    "W3_ADDON_ANY",
    "W3_FOREIGN_ADDON",
    "W3_PARK_RENODX",
    "W3_NGFX_FRAMES",
];
/// How long the game may take to show its window, and ReShade to log the add-on (the harness's 60 s each).
const WINDOW_MAX: Duration = Duration::from_secs(60);
const RESHADE_MAX: Duration = Duration::from_secs(60);
/// The first load of a launch (live boot 1): from `MENU_FIRST` after the launch, a key every `MENU_EVERY`, E (Continue)
/// and Space by turns, until the first push of quen's state log; then none until plain gameplay, at most `CONTINUE_MAX` (a
/// first launch after a script change compiles first). Boot 1's Continue took 97 s: its first E came before the menu
/// took input (the first experiment boot: the menu at ~23 s), Space doesn't select Continue, and only the E a minute
/// later started the load. Live boot 2: the E's before about 28 s found no menu, and the one that started the load came
/// at about 33 s, so the first key waits 25 s (about 5 wasted keys fewer, none lost). ponytail: guesses from the first
/// experiment boot and boots 1-2.
const MENU_FIRST: Duration = Duration::from_secs(25);
const MENU_EVERY: Duration = Duration::from_secs(2);
const CONTINUE_MAX: Duration = Duration::from_secs(300);
/// [unproven live] A named save that isn't the newest loads at the main menu through the channel's load verb (the game
/// ticks there, live boot 3) in place of Continue: tried at a key's turn, up to `MENU_LOAD_TRIES` times while the game
/// refuses it or doesn't answer (a Space between tries passes the start screen, which takes any key), then Continue.
/// Once the game took it, only Space for `MENU_LOAD_GRACE` (at the main menu E selects Continue), then E and Space
/// again if no state shows. ponytail: guesses until a live boot times a menu load.
const MENU_LOAD_TRIES: usize = 3;
const MENU_LOAD_GRACE: Duration = Duration::from_secs(45);
/// How late quen's state log may show a change: modQuen checks at most every 0.25 s while the game ticks.
const PUSH_LATENCY_MS: i64 = 250;
/// How long photo mode may take to show on quen's state log after U.
const PHOTO_WAIT: Duration = Duration::from_secs(3);
/// How long the Native Bridge may take to register quen's script functions (live boot 3: about 5.7 s after the launch,
/// before the window).
const NATIVES_MAX: Duration = Duration::from_secs(60);
/// How long RenderDoc may take to log the game's wrapped D3D12 device, and the game's Tracy to log a connected capture
/// (after the natives).
const RENDERDOC_MAX: Duration = Duration::from_secs(60);
const TRACY_MAX: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);

/// A checked `session_launch` request and the file changes it makes.
pub(crate) struct Spec {
    pub(crate) user_ok: bool,
    /// Launch an exe of a build quen doesn't know; the boot row records it.
    pub(crate) allow_unknown_build: bool,
    /// A play session: the user's game as it is, only what was asked changed.
    pub(crate) play: bool,
    /// The save the boot loads (a play session without one sends no key: the user picks a save), and what it changes,
    /// for its ledger row.
    pub(crate) save: Option<PathBuf>,
    pub(crate) settings_diff: Map<String, Value>,
    pub(crate) devkit: bool,
    pub(crate) fg: bool,
    pub(crate) vsync: bool,
    pub(crate) photo: bool,
    /// Start the game with `-debugscripts` (finding 50), a condition on the boot row.
    debugscripts: bool,
    /// `renderdoc:NAME`, `nsys:NAME` or `ngfx:NAME`, as asked, and its launch profile.
    pub(crate) profiler: Option<String>,
    pub(crate) profile: Option<Profile>,
    /// Every file change, in order: quen-core's plan, then the profile's.
    pub(crate) changes: Vec<Change>,
    /// Vortex's records as the plan read them, its refusals passed; None without a Vortex manifest.
    pub(crate) vortex: Option<Inventory>,
    /// session_plan's and session_launch's `park`: the pairs learned from the files, the folders they can't settle,
    /// and each Script Merger folder parked with a mod it merged.
    pub(crate) park: Map<String, Value>,
    /// The ReShade.log line that says the RenoDX add-on loaded, when the boot waits for it.
    mark: Option<&'static str>,
    extra: Map<String, Value>,
}

/// Whether `name` is non-empty and only ASCII letters, digits, `_` and the `also` characters.
pub(crate) fn plain(name: &str, also: &[char]) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || also.contains(&c))
}

impl Spec {
    /// Checks the arguments, that the save and the game's exe are there, the optional features against the config,
    /// and plans the boot's file changes (backups into `backups`), all read-only: an error means nothing was changed.
    pub(crate) fn parse(config: &Config, args: &Value, backups: &Path) -> Result<Spec, String> {
        let args = args.as_object().ok_or("arguments must be an object")?;
        let known = [
            "mode",
            "save",
            "settings",
            "preset",
            "park",
            "keep",
            "devkit",
            "fg",
            "vsync",
            "profiler",
            "photo",
            "extra_env",
            "stage",
            "stage_mods",
            "debugscripts",
            "user_ok",
            "allow_unknown_build",
        ];
        if args.contains_key("dry_run") {
            return Err("dry_run moved: session_plan takes the same arguments and plans".into());
        }
        if let Some(key) = args.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(format!("unknown argument {key:?}"));
        }
        let text = |key: &str| -> Result<Option<&str>, String> {
            match args.get(key) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(Some)
                    .ok_or(format!("{key} must be a string")),
            }
        };
        let flag = |key: &str, default: bool| -> Result<bool, String> {
            match args.get(key) {
                None => Ok(default),
                Some(v) => v.as_bool().ok_or(format!("{key} must be true or false")),
            }
        };
        // Without them the gamesaves guard and the parking would quietly be off.
        let folder = |path: &Option<PathBuf>, key: &str| {
            let why = format!("a launch needs {key} in the local config: an existing folder");
            path.clone().filter(|p| p.is_dir()).ok_or(why)
        };
        let play = match text("mode")? {
            None | Some("test") => false,
            Some("play") => true,
            Some(other) => return Err(format!("mode {other:?}: test (the default) or play")),
        };
        // A play session launches the user's game as it is.
        let refused = [
            "park",
            "keep",
            "profiler",
            "preset",
            "extra_env",
            "stage",
            "stage_mods",
        ];
        if let Some(key) = refused.iter().find(|k| play && args.contains_key(**k)) {
            return Err(format!(
                "{key} isn't taken in a play session (mode play): it launches the game with the user's mods and \
                 files as they are"
            ));
        }
        let (game_dir, documents) = (
            folder(&config.game_dir, "game_dir")?,
            folder(&config.documents, "documents")?,
        );
        let save = match (text("save")?, play) {
            (None, true) => None,
            (name, _) => Some(find_save(&documents.join("gamesaves"), name)?),
        };
        let bin = boot::bin_dir(&game_dir);
        let exe = bin.join(&config.game_exe);
        if !exe.is_file() {
            return Err(format!("no game exe {}", exe.display()));
        }
        let (stage, stage_mods) = sources(args)?;
        let canonical = |path: &Path| fs::canonicalize(path).ok();
        let inside = |dir: &Path| {
            canonical(dir)
                .zip(canonical(&game_dir))
                .is_some_and(|(d, g)| d.starts_with(g))
        };
        let mods_dir = game_dir.join("Mods");
        // A staged .xml goes into the menu folder (finding 49), anything else into bin.
        let menu_dir = boot::menu_dir(&game_dir);
        let menus = stage.iter().any(|p| boot::is_menu(p));
        for (key, used, dir) in [
            ("stage", stage.iter().any(|p| !boot::is_menu(p)), &bin),
            ("stage", menus, &menu_dir),
            ("stage_mods", !stage_mods.is_empty(), &mods_dir),
        ] {
            if used && !inside(dir) {
                return Err(format!(
                    "{key}: {} is missing or leads out of the game folder (a link or junction)",
                    dir.display()
                ));
            }
        }
        let mut extra = Map::new();
        if let Some(env) = args.get("extra_env") {
            for (k, v) in env.as_object().ok_or("extra_env must be an object")? {
                let v = v
                    .as_str()
                    .ok_or(format!("extra_env {k} must be a string"))?;
                if !EXTRA_ENV.contains(&k.as_str()) {
                    return Err(format!(
                        "extra_env {k:?} is not a boot switch quen has ({})",
                        EXTRA_ENV.join(", ")
                    ));
                }
                // A NUL would end an argument early.
                if v.contains('\0') {
                    return Err(format!("extra_env {k}: {v:?} holds a NUL character"));
                }
                extra.insert(k.clone(), json!(v));
            }
        }
        let on = |key: &str| extra.get(key).is_some_and(|v| v == "1");
        if let Some(frames) = extra.get("W3_NGFX_FRAMES").and_then(Value::as_str)
            && !plain(frames, &[])
        {
            return Err(format!(
                "extra_env W3_NGFX_FRAMES {frames:?} is not a number"
            ));
        }

        let mut settings_diff = Map::new();
        // RenoDX preset-1 values, as the harness's boot took them: Key, or +Key for a key preset 1 lacks.
        let mut preset = Vec::new();
        if let Some(values) = text("preset")? {
            let mut diff = Map::new();
            for pair in values.split(',').filter(|p| !p.trim().is_empty()) {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                let key = key.trim();
                // A JSON number: no inf or NaN, and 250 stays an integer in the boot row.
                let number = serde_json::from_str::<serde_json::Number>(value.trim())
                    .ok()
                    .filter(|_| plain(key.strip_prefix('+').unwrap_or(key), &[]));
                let number = number.ok_or(format!(
                    "preset {pair:?}: Key=number, the key letters, digits and _ (a leading + adds a key preset 1 \
                     lacks)"
                ))?;
                preset.push((key, number.as_f64().unwrap_or_default()));
                diff.insert(key.into(), json!(number));
            }
            if !preset.is_empty() && config.renodx_addon.is_none() {
                return Err("preset needs renodx_addon in the local config".into());
            }
            settings_diff.insert("preset".into(), json!(diff));
        }
        let settings = args
            .get("settings")
            .map(|s| s.as_object().ok_or("settings must be an object"));
        let settings = settings.transpose()?.cloned().unwrap_or_default();
        if let Some(key) = settings
            .keys()
            .find(|k| !["flat", "sections"].contains(&k.as_str()))
        {
            return Err(format!("unknown settings key {key:?} (flat, sections)"));
        }
        let spec = |key: &str| {
            settings
                .get(key)
                .map(|v| v.as_str().ok_or(format!("settings.{key} must be a string")))
        };
        let flat = spec("flat").transpose()?.unwrap_or_default();
        let sections = spec("sections").transpose()?.unwrap_or_default();
        if !flat.is_empty() || !sections.is_empty() {
            settings_diff.insert("flat".into(), json!(flat));
            settings_diff.insert("sections".into(), json!(sections));
        }
        let park = text("park")?.unwrap_or_default();
        let keep = text("keep")?.unwrap_or_default();
        if park.trim().is_empty() && !keep.is_empty() {
            return Err("keep needs park".into());
        }
        let profiler = text("profiler")?;
        let mut profile = profiler
            .map(|p| {
                let signed = doctor::signed_interposer();
                let package = doctor::package_version(config);
                let tracy = &profile::tracy_version;
                Profile::parse(
                    config,
                    p,
                    &bin,
                    backups,
                    &signed,
                    &win::authenticode,
                    package,
                    tracy,
                )
            })
            .transpose()?;
        let (fg, vsync) = (flag("fg", play)?, flag("vsync", true)?);
        // ReShade loads the devkit from its ini's `[ADDON] LoadFromDllMain`, with or without the RenoDX add-on (feature
        // tests 2b). Parking ReShade takes the devkit with it, so a profile that parks it turns the devkit's default
        // off, as does a missing ReShade or devkit file; asked for, each is refused.
        let parks_reshade = profile
            .as_ref()
            .is_some_and(|p| p.park.iter().any(|n| n == "dxgi.dll"));
        let ini = bin.join("ReShade.ini");
        let devkit_file = config.devkit.as_deref().map(|path| bin.join(path));
        let missing = if !bin.join("dxgi.dll").is_file() || !ini.is_file() {
            Some("devkit needs ReShade in bin (dxgi.dll and ReShade.ini): the devkit is a ReShade add-on".into())
        } else {
            devkit_file.as_ref().filter(|f| !f.is_file()).map(|f| {
                format!(
                    "devkit: no {} (devkit in quen's config, relative to bin)",
                    f.display()
                )
            })
        };
        let asked = args.contains_key("devkit");
        let mut devkit = flag(
            "devkit",
            !play && config.devkit.is_some() && !parks_reshade && missing.is_none(),
        )?;
        if devkit && config.devkit.is_none() {
            return Err(
                "devkit needs devkit in the local config (the devkit add-on's path)".into(),
            );
        }
        // The run's conditions, on the boot row: the devkit loaded, the profile's tool and what it changes.
        let mut park = park.to_owned();
        let mut conditions = Vec::new();
        if devkit {
            conditions.push("devkit");
        }
        let debugscripts = flag("debugscripts", false)?;
        if debugscripts {
            // ngfx starts the game from its own arguments (`--exe`), where quen passes none.
            if profile.as_ref().is_some_and(|p| p.tool == Tool::Ngfx) {
                return Err("debugscripts isn't taken under profiler ngfx".into());
            }
            conditions.push("debugscripts");
        }
        if let (Some(profile), Some(profiler)) = (&profile, profiler) {
            if devkit && parks_reshade {
                return Err(format!(
                    "profiler {profiler} parks ReShade, which the devkit needs: devkit: false"
                ));
            }
            if fg && profile.tool == Tool::RenderDoc {
                return Err("profiler renderdoc turns frame generation off: leave fg out".into());
            }
            if let Some(key) = profile.overridden(flat, sections) {
                return Err(format!(
                    "profiler {profiler} sets {key} itself ({}): leave it out of settings",
                    profile.settings
                ));
            }
            let named = |list: &str, name: &str| {
                list.split(',').any(|n| n.trim().eq_ignore_ascii_case(name))
            };
            // ReShade under renderdoc, nsys and ngfx; under tracy the mods that hook the functions W3Quen's Tracy hooks.
            for name in &profile.park {
                if named(keep, name) {
                    return Err(format!(
                        "profiler {profiler} parks {name}: leave it out of keep"
                    ));
                }
                if stage
                    .iter()
                    .chain(&stage_mods)
                    .any(|p| p.file_name().is_some_and(|f| f.eq_ignore_ascii_case(name)))
                {
                    return Err(format!(
                        "profiler {profiler} parks {name}: leave it out of stage and stage_mods"
                    ));
                }
                if !named(&park, name) {
                    park = [park.as_str(), name.as_str()]
                        .into_iter()
                        .filter(|p| !p.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(",");
                }
            }
            settings_diff.insert("profile".into(), profile.record());
            conditions.extend(profile.conditions());
        }
        if let Some(why) = missing.filter(|_| devkit) {
            return Err(why);
        }
        if !conditions.is_empty() {
            settings_diff.insert("conditions".into(), json!(conditions));
        }
        let flat = [flat, profile.as_ref().map_or("", |p| p.settings)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let mods = match park.trim().is_empty() && stage_mods.is_empty() {
            true => Vec::new(),
            false => listing(&mods_dir)?,
        };
        let bin_entries = listing(&bin)?;
        // The build under test is the one installed (the harness's add-on check). A play session tests no build.
        if let (Some(addon), Some(build)) = (&config.renodx_addon, &config.renodx_build)
            && !on("W3_ADDON_ANY")
            && !play
        {
            let sha = |p: &Path| fs::read(p).map(|b| sha256_hex(&b)).ok();
            let installed = bin.join(addon);
            if sha(&installed).is_none() || sha(&installed) != sha(build) {
                return Err(format!(
                    "the installed {} differs from the build {}: copy it in first, or extra_env W3_ADDON_ANY=1",
                    installed.display(),
                    build.display()
                ));
            }
        }
        let never_park = w3::never_park(on("W3_PARK_RENODX"));
        // What a park sees beyond Mods and bin: Vortex's mods with their menu and DLC units, the pairs the files settle,
        // Script Merger's merges. All read here, so a refusal changes nothing.
        let parking = !park.trim().is_empty();
        let inv = crate::vortex::read(config);
        let managed = inv.as_ref().map(Inventory::managed).unwrap_or_default();
        let dlc_dir = boot::dlc_dir(&game_dir);
        let read_if = |used: bool, dir: &Path| match used && dir.is_dir() {
            true => listing(dir),
            false => Ok(Vec::new()),
        };
        let menu = match menus {
            true => listing(&menu_dir)?,
            false => read_if(parking, &menu_dir)?,
        };
        let dlc = read_if(parking, &dlc_dir)?;
        let learned = match parking {
            true => w3::pairs(
                &game_dir,
                &config.game_exe,
                &mods,
                &bin_entries,
                &never_park,
                &managed,
            )?,
            false => Learned::default(),
        };
        let pairs: Vec<[&str; 2]> = learned
            .pairs
            .iter()
            .map(|[a, b]| [a.as_str(), b.as_str()])
            .collect();
        let merges = match parking {
            true => w3::merges(&game_dir),
            false => Ok(Vec::new()),
        };
        let more = park::More {
            menu: Some(Listing {
                dir: &menu_dir,
                entries: &menu,
            }),
            dlc: Some(Listing {
                dir: &dlc_dir,
                entries: &dlc,
            }),
            managed: &managed,
            unsettled: &learned.unsettled,
            merges: merges.as_deref().unwrap_or_default(),
            merges_unread: merges.as_ref().err().map(String::as_str),
        };
        let renodx = config.renodx_addon.as_deref().filter(|_| !play);
        let renodx = renodx.map(|addon| RenoDx {
            addon,
            preset: &preset,
        });
        // The devkit's line: its path to load it; `""` on a devkit: false boot, where ReShade has an ini. A play session
        // leaves it as the user's ini has it, unless devkit was asked.
        let plan = |devkit: bool| {
            let load = config.devkit.as_deref().filter(|_| asked || !play);
            let load = load.and_then(|path| {
                if devkit {
                    Some(path)
                } else {
                    ini.is_file().then_some("")
                }
            });
            boot::plan(
                &JournalFs,
                &Inputs {
                    game_dir: &game_dir,
                    documents: &documents,
                    backups,
                    fg,
                    vsync,
                    flat: &flat,
                    sections,
                    park: &park,
                    keep,
                    never_park: &never_park,
                    pairs: &pairs,
                    mods: &mods,
                    bin: &bin_entries,
                    more,
                    renodx,
                    load_from_dll_main: load,
                    play,
                },
            )
        };
        let mut changes = plan(devkit)?;
        let parks_dxgi = changes.iter().any(|c| {
            matches!(c, Change::Move { from, .. } if from.parent() == Some(bin.as_path())
                && from.file_name().is_some_and(|f| f.eq_ignore_ascii_case("dxgi.dll")))
        });
        if devkit && parks_dxgi {
            if asked {
                return Err(
                    "park parks ReShade (dxgi.dll), which loads the devkit: name dxgi.dll in keep, or devkit: false"
                        .into(),
                );
            }
            devkit = false;
            changes = plan(false)?;
            if let Some(Value::Array(list)) = settings_diff.get_mut("conditions") {
                list.retain(|c| c != "devkit");
            }
            if settings_diff.get("conditions") == Some(&json!([])) {
                settings_diff.remove("conditions");
            }
        }
        if let Some(profile) = &mut profile {
            changes.append(&mut profile.changes);
        }
        let never: Vec<&str> = never_park
            .iter()
            .copied()
            .chain([config.game_exe.as_str()])
            .collect();
        let staged = boot::stage(
            &JournalFs,
            &game_dir,
            &stage,
            &never,
            &bin_entries,
            &menu,
            &changes,
        )?;
        changes.extend(staged);
        let staged = boot::stage_mods(
            &JournalFs,
            &game_dir,
            &stage_mods,
            &never,
            &mods,
            backups,
            &changes,
        )?;
        changes.extend(staged);
        // With Vortex: what would undo the launch's parks, or a restore that breaks a link, refused (vortex.rs).
        let vortex = crate::vortex::check(inv, &changes)?;
        let mut park = Map::new();
        if !learned.pairs.is_empty() {
            park.insert("pairs".into(), json!(learned.pairs));
        }
        if !learned.unsettled.is_empty() {
            park.insert("unsettled".into(), json!(learned.unsettled));
        }
        let moved = |name: &str| {
            changes.iter().any(|c| {
                matches!(c, Change::Move { from, .. } if from.parent() == Some(mods_dir.as_path())
                    && from.file_name().is_some_and(|f| f.eq_ignore_ascii_case(name)))
            })
        };
        let merged: Vec<Value> = merges
            .iter()
            .flatten()
            .filter(|m| moved(&m.merged) && m.included.iter().any(|i| moved(i)))
            .map(|m| json!({"folder": format!(r"Mods\{}", m.merged), "merges": m.included}))
            .collect();
        if !merged.is_empty() {
            park.insert("merged".into(), json!(merged));
        }
        let mark = config.renodx_addon.as_ref().filter(|_| !play).map(|_| {
            if on("W3_FOREIGN_ADDON") {
                "Registered add-on \"RenoDX\""
            } else {
                "DevAutoReload: watching"
            }
        });
        Ok(Spec {
            user_ok: flag("user_ok", false)?,
            allow_unknown_build: flag("allow_unknown_build", false)?,
            play,
            save,
            settings_diff,
            devkit,
            fg,
            vsync,
            photo: flag("photo", !play)?,
            debugscripts,
            profiler: profiler.map(str::to_owned),
            profile,
            changes,
            vortex,
            park,
            mark,
            extra,
        })
    }

    /// Every file change as a line, then the profiler's output folder in `boot` (the boot's capture folder), made before
    /// the launch and kept (feature tests 2): session_plan's and session_launch's `changes`.
    pub(crate) fn change_lines(&self, boot: &Path) -> Vec<String> {
        let mut changes: Vec<String> = self.changes.iter().map(Change::summary).collect();
        changes.extend(
            self.profile
                .as_ref()
                .map(|p| format!("create {}", p.output_dir(boot).display())),
        );
        changes
    }

    /// What starts the game, and in which folder: the exe itself in bin, or its profile's tool around it, writing into
    /// `out` (profile.rs); with `debugscripts` the game's argument after the exe, which ends the command line but under
    /// ngfx (refused with it).
    pub(crate) fn command(&self, config: &Config, out: &Path) -> (Vec<String>, PathBuf) {
        let bin = config.bin_dir().unwrap_or_default();
        let exe = bin.join(&config.game_exe).display().to_string();
        let (mut argv, cwd) = match &self.profile {
            Some(profile) => {
                let frames = self.extra.get("W3_NGFX_FRAMES").and_then(Value::as_str);
                profile.command(config, exe, &bin, out, frames)
            }
            None => (vec![exe], bin),
        };
        if self.debugscripts {
            argv.push("-debugscripts".into());
        }
        (argv, cwd)
    }
}

/// The files `stage` names, copied into bin for the session (quen-core's boot::stage), and the mod folders
/// `stage_mods` names, copied into Mods (boot::stage_mods; finding 38): on this PC, each checked as a launch takes it.
fn sources(args: &Map<String, Value>) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let mut stage = Vec::new();
    if let Some(list) = args.get("stage") {
        for path in list.as_array().ok_or("stage must be an array of paths")? {
            let path = Path::new(path.as_str().ok_or("stage: each entry must be a path")?);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let why = if !path.is_absolute() {
                "not an absolute path"
            } else if !plain(&name, &['.', '-']) || name.ends_with('.') {
                "its name must be ASCII letters, digits, _ . and -"
            } else if !path.is_file() {
                "no such file (missing, or a folder)"
            } else {
                stage.push(path.to_owned());
                continue;
            };
            return Err(format!("stage {}: {why}", path.display()));
        }
    }
    let mut stage_mods = Vec::new();
    if let Some(list) = args.get("stage_mods") {
        for path in list
            .as_array()
            .ok_or("stage_mods must be an array of paths")?
        {
            let path = Path::new(
                path.as_str()
                    .ok_or("stage_mods: each entry must be a path")?,
            );
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let why = if !path.is_absolute() {
                "not an absolute path".to_string()
            } else if !plain(&name, &['.', '-']) || name.ends_with('.') {
                "its name must be ASCII letters, digits, _ . and -".to_string()
            } else if !fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                "no such folder (missing, a file or a link)".to_string()
            } else if let Some(why) = link_in(path) {
                why
            } else {
                stage_mods.push(path.to_owned());
                continue;
            };
            return Err(format!("stage_mods {}: {why}", path.display()));
        }
    }
    Ok((stage, stage_mods))
}

/// What `session_launch again` checks of the last launch's `args` while its session still runs, before it ends it
/// (finding 61): the save and the staged files and mods are still there. The launch checks the rest once the game's
/// files are the user's again.
// ponytail: the save's bytes aren't compared with the last launch's; the saves guard (session.rs) ends a session in
// which the game replaced a save other than its own autosaves.
pub(crate) fn reproducible(config: &Config, args: &Value) -> Result<(), String> {
    let args = args.as_object().ok_or("arguments must be an object")?;
    sources(args)?;
    if let (Some(save), Some(docs)) = (args.get("save").and_then(Value::as_str), &config.documents)
    {
        find_save(&docs.join("gamesaves"), Some(save))?;
    }
    Ok(())
}

/// Why quen loads no save without a file on disk (finding 34: game_listsaves lists cloud-only saves; the user loaded
/// one from the menu during a hand-over).
pub(crate) const NO_FILE: &str = "quen loads only a save with a file in gamesaves; a save the game lists without one \
    (a cloud-only save, game_listsaves' no_file) the user can load from the game's menu while they have the controls";

/// The save a boot loads: the one `.sav` whose name is `name` in any case, with or without `.sav` (the game's own list
/// gives bare lowercase names, as the first experiment boot showed; modQuen matches the same way), or else the newest
/// `QuickSave*.sav`, as ini_session.py's `save_path` picks it.
fn find_save(dir: &Path, name: Option<&str>) -> Result<PathBuf, String> {
    let all = saves::snapshot(dir);
    let stem = |file: &str| file.to_lowercase().strip_suffix(".sav").map(str::to_owned);
    let sav: Vec<(&String, String, _)> = all
        .iter()
        .filter_map(|(file, state)| Some((file, stem(file)?, state.1)))
        .collect();
    let found: Vec<&String> = match name {
        Some(name) => {
            let want = stem(name).unwrap_or_else(|| name.to_lowercase());
            sav.iter()
                .filter(|(_, s, _)| *s == want)
                .map(|(f, ..)| *f)
                .collect()
        }
        None => sav
            .iter()
            .filter(|(_, s, _)| s.starts_with("quicksave"))
            .max_by_key(|(.., modified)| *modified)
            .map(|(f, ..)| *f)
            .into_iter()
            .collect(),
    };
    match found[..] {
        [file] => Ok(dir.join(file)),
        [] => Err(format!(
            "no save {} in {}: {NO_FILE}",
            name.unwrap_or("QuickSave*.sav"),
            dir.display()
        )),
        _ => Err(format!(
            "{} saves in {} match {name:?}: name one",
            found.len(),
            dir.display()
        )),
    }
}

/// The newest `.sav` in `dir`: the one the main menu's Continue loads, as the harness took it.
/// ponytail: by write time. The first experiment boot saw a save in the game's own list with no file in gamesaves (a
/// cloud one [I]); were such a one the newest, Continue would load it while this names another, and the save asked for
/// would then be typed unless it is this one.
pub(crate) fn newest_save(dir: &Path) -> Option<PathBuf> {
    let all = saves::snapshot(dir);
    all.iter()
        .filter(|(file, _)| file.to_lowercase().ends_with(".sav"))
        .max_by_key(|(_, (_, modified))| *modified)
        .map(|(file, _)| dir.join(file))
}

/// The first link in the folder tree `dir`, or why part of it can't be read: a staged mod is copied file by file.
fn link_in(dir: &Path) -> Option<String> {
    let unread = |e: std::io::Error| Some(format!("{}: {e}", dir.display()));
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return unread(e),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return unread(e),
        };
        match entry.file_type() {
            Ok(t) if t.is_symlink() => {
                return Some(format!("{} is a link", entry.path().display()));
            }
            Ok(t) if t.is_dir() => {
                if let Some(why) = link_in(&entry.path()) {
                    return Some(why);
                }
            }
            Ok(_) => {}
            Err(e) => return unread(e),
        }
    }
    None
}

/// A folder's entries for the park planner.
pub(crate) fn listing(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = || -> std::io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            entries.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: kind.is_dir(),
                is_link: kind.is_symlink(),
            });
        }
        Ok(entries)
    };
    read().map_err(|e| format!("{}: {e}", dir.display()))
}

/// The in-game part of the boot, once the game was started in the live session's job: its window, ReShade's add-on
/// line (with RenoDX), Continue (E and Space by turns until quen's state log's first push, then no key) until plain
/// gameplay, the save asked for through game_load if Continue loaded another, the saves guard, then photo mode. Its
/// results go into `fields`, the launch's, so a failed launch keeps them too (live boot 2: the Continue keys). Err says
/// why the session must end.
pub(crate) fn play(
    session: &mut Session,
    call: &mut Call,
    spec: &Spec,
    fields: &mut Map<String, Value>,
) -> Result<(), String> {
    let started = Instant::now();
    let config = session.config()?.clone();
    let live = session.live.as_ref().ok_or("no live session")?;
    let (job, launched, token) = (live.job.clone(), live.launched, live.token.clone());
    let bin = config.bin_dir().unwrap_or_default();
    // Before every key, as the harness did: never into a dialog (a script compile error), never after the game left. A
    // stop first: its kill empties the job, which isn't the game exiting.
    let ready = |call: &Call| -> Result<Vec<u32>, String> {
        if call.stopped() {
            return Err(format!("stopped during the boot: {}", call.stop_cause()));
        }
        let pids = job.pids().map_err(|e| format!("the job: {e}"))?;
        if pids.is_empty() {
            return Err(
                "the game exited during the launch (a script compile error can close it)".into(),
            );
        }
        if let Some(title) = capture::dialog(&pids) {
            // The game's text, cut short: its title, never more.
            let title: String = title.chars().take(80).collect();
            return Err(format!(
                "the game shows a dialog {title:?} (a script compile error?): no key sent"
            ));
        }
        Ok(pids)
    };
    let t = Instant::now();
    while capture::game_window(&ready(call)?).is_err() {
        if t.elapsed() > WINDOW_MAX {
            return Err(format!("no game window within {} s", WINDOW_MAX.as_secs()));
        }
        sleep(POLL);
    }
    call.lap("window");
    // The game must be in the job, or quen's kill can't reach it (a profiler may start it outside the job).
    match outside(&config.game_exe, Some(&job)) {
        Ok(pids) if pids.is_empty() => fields.insert("job_covers_game".into(), json!(true)),
        Ok(pids) => {
            return Err(format!(
                "{} runs outside quen's job (pids {pids:?}){}: the session was ended",
                config.game_exe,
                if spec.profiler.is_some() {
                    ", started by the profiler"
                } else {
                    ""
                }
            ));
        }
        Err(why) => return Err(format!("{why}: the session was ended")),
    };
    // The Native Bridge registered quen's script functions (it rewrites its log at launch, before the window). A
    // game update that broke it shows here in seconds, not as a Continue that never reaches gameplay.
    let natives = bin.join(w3::NATIVE_LOG);
    let t = Instant::now();
    loop {
        let fresh = fs::metadata(&natives)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m >= launched);
        let log = fresh.then(|| fs::read(&natives).unwrap_or_default());
        match w3::natives(&String::from_utf8_lossy(&log.unwrap_or_default())) {
            Some(Ok(())) => break,
            Some(Err(why)) => return Err(why),
            None if t.elapsed() > NATIVES_MAX => {
                return Err(format!(
                    "{} shows no registration of quen's script functions within {} s: the Native Bridge didn't load \
                     (its ASI loader?), or W3Quen.asi didn't",
                    natives.display(),
                    NATIVES_MAX.as_secs()
                ));
            }
            None => {}
        }
        ready(call)?;
        sleep(POLL);
    }
    call.lap("natives");
    // The renderdoc profile: RenderDoc wrapped the game's D3D12 device, so the Streamline swap took. Otherwise no capture
    // can work, and the session ends here.
    if spec
        .profile
        .as_ref()
        .is_some_and(|p| p.tool == Tool::RenderDoc)
    {
        let t = Instant::now();
        while profile::wrapped(launched).is_none() {
            if t.elapsed() > RENDERDOC_MAX {
                return Err(format!(
                    "RenderDoc's log ({}) shows no wrapped D3D12 device within {} s: the Streamline swap didn't take, \
                     so no capture can work",
                    std::env::temp_dir().join("RenderDoc").display(),
                    RENDERDOC_MAX.as_secs()
                ));
            }
            ready(call)?;
            sleep(POLL);
        }
        call.lap("renderdoc");
    }
    // The tracy profile: W3Quen.asi hooked the game and Tracy's capture tool connected (W3Quen.log says both).
    // Otherwise the boot records nothing, and the session ends here.
    if spec.profile.as_ref().is_some_and(|p| p.tool == Tool::Tracy) {
        let t = Instant::now();
        let tracy = loop {
            match profile::tracy_ready(&bin, launched) {
                Some(said) => break said?,
                None if t.elapsed() > TRACY_MAX => {
                    return Err(format!(
                        "{} shows no Tracy capture connected within {} s (the capture tool's output is in {} in the \
                         session's logs)",
                        bin.join("W3Quen.log").display(),
                        TRACY_MAX.as_secs(),
                        profile::TRACY_LOG
                    ));
                }
                None => {}
            }
            ready(call)?;
            sleep(POLL);
        };
        fields.insert("tracy".into(), tracy);
        if let Some(live) = &session.live {
            live.tracy_connected.set(true);
        }
        call.lap("tracy");
    }
    // ReShade rewrites its log at launch; the add-on logs this in DllMain, before any input. Not while either is parked.
    let addon = config.renodx_addon.as_deref().map(|a| bin.join(a));
    if let (Some(mark), Some(addon)) = (spec.mark, addon)
        && addon.exists()
        && bin.join("dxgi.dll").exists()
    {
        let log = bin.join("ReShade.log");
        let t = Instant::now();
        loop {
            ready(call)?;
            let fresh = fs::metadata(&log)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m > launched);
            if fresh && fs::read(&log).is_ok_and(|b| String::from_utf8_lossy(&b).contains(mark)) {
                break;
            }
            if t.elapsed() > RESHADE_MAX {
                return Err(format!(
                    "{} lacks {mark:?}: the add-on didn't load",
                    log.display()
                ));
            }
            sleep(POLL);
        }
        call.lap("reshade");
    }
    // The devkit is in the game's modules (ReShade loads it from DllMain, before the window): only then does the boot
    // row say devkit (feature tests 2b: a row said so for a boot that never loaded it).
    if spec.devkit {
        let t = Instant::now();
        loop {
            ready(call)?;
            if game::ctx(session, &json!({"token": token}), &[])?.devkit_loaded() {
                break;
            }
            if t.elapsed() > RESHADE_MAX {
                return Err(format!(
                    "the devkit ({}) isn't loaded in the game within {} s, though ReShade.ini's [ADDON] \
                     LoadFromDllMain names it",
                    config.devkit.as_deref().unwrap_or_default(),
                    RESHADE_MAX.as_secs()
                ));
            }
            sleep(POLL);
        }
        call.lap("devkit");
    }
    // A play session without a save sends no key: the user picks one at the main menu.
    let Some(save) = spec.save.as_deref() else {
        return Ok(());
    };
    // Letters typed at the main menu act on it (the first experiment boot: the `e` of a console line selected
    // Continue), so nothing is typed before gameplay. Continue (one key) loads the newest save; the one asked for is
    // typed from gameplay if it differs.
    //
    // No key may reach gameplay (live boot 1: a Space after the load made Geralt jump; in gameplay Space jumps or rolls,
    // E interacts). Keys go out only before the state log's first push of this launch, read again before each key:
    // E selects Continue, Space skips the intro movies and the recap while the save loads (the game's skip key, code 32
    // in its movie menus' scripts). The first push comes once the loading screen is over, and gameplay at least 17 s
    // later (boot 1's fade, with or without keys), so a key sent before it can't reach gameplay even inside the push
    // latency. From the first push nothing is sent: input.settings binds no loading-screen skip, every key the game's
    // menus skip or select with acts in gameplay (Space, Enter, Escape, E), and boot 1's console-started loads reached
    // gameplay with no key at all. ponytail: a load that ends in a scene waits for CONTINUE_MAX; it needs a skip then.
    let newest = newest_save(save.parent().unwrap_or(Path::new(".")));
    let in_game = || {
        let flags = game::state_flags(&config, launched);
        !flags.is_empty() && bridge_log::loaded_after(&[&[0][..], &flags].concat(), 0)
    };
    // The test stand-in (QUEN_TEST_INPUT) is a menu that takes keys at once.
    let first = match std::env::var_os("QUEN_TEST_INPUT") {
        Some(_) => Duration::ZERO,
        None => MENU_FIRST,
    };
    let (t, mut next) = (Instant::now(), started + first);
    let mut sent: Vec<(&str, u64)> = Vec::new();
    // The menu load (MENU_LOAD_TRIES): its tries' replies, whether to try again, when the game took one, and whether an
    // E went after that (then Continue may have loaded instead).
    let stem = save.file_stem().unwrap_or_default().to_string_lossy();
    let (mut tries, mut menu_open) = (Vec::new(), newest.as_deref() != Some(save));
    let (mut taken, mut e_after): (Option<Instant>, bool) = (None, false);
    // Ok at plain gameplay, else why; the keys sent so far go into `fields` either way.
    let continued = loop {
        if in_game() {
            break Ok(());
        }
        if let Err(why) = ready(call) {
            break Err(why);
        }
        if t.elapsed() > CONTINUE_MAX {
            let flags = game::state_flags(&config, launched);
            let last = flags.last().map(|&f| bridge_log::names(f));
            break Err(format!(
                "no plain gameplay within {} s of Continue (state flags of this launch: {last:?})",
                CONTINUE_MAX.as_secs()
            ));
        }
        if Instant::now() >= next && game::state_flags(&config, launched).is_empty() {
            let cx = match game::ctx(session, &json!({"token": token}), &[]) {
                Ok(cx) => cx,
                Err(why) => break Err(why),
            };
            next = Instant::now() + MENU_EVERY;
            if menu_open {
                let tried = match cx.channel() {
                    Ok(channel) => channel::load(&cx, channel, &stem).map_err(|e| {
                        let why = json!({"ok": false, "why": e.why(), "game_text": e.game_text()});
                        (why, false)
                    }),
                    Err(down) => Err((json!({"ok": false, "why": down}), true)),
                };
                menu_open = false;
                match tried {
                    Ok(reply) => {
                        taken = Some(Instant::now());
                        tries.push(json!({"ok": true, "reply": reply.text}));
                        sleep(POLL);
                        continue; // no key at once onto the load
                    }
                    Err((tried, down)) => {
                        tries.push(tried);
                        menu_open = !down && tries.len() < MENU_LOAD_TRIES;
                    }
                }
            }
            let space_only = taken.map_or(menu_open, |at| at.elapsed() < MENU_LOAD_GRACE);
            let key = match space_only {
                true => "space",
                false => ["e", "space"][sent.len() % 2],
            };
            e_after |= key == "e" && taken.is_some();
            let at = win::tick();
            let pressed = cx
                .keys(call, &[key.to_owned()], false, game::Focus::Bring)
                .map_err(|done| format!("Continue ({key}): {}", reason(&done)));
            if let Err(why) = pressed {
                break Err(why);
            }
            sent.push((key, at));
        }
        sleep(POLL);
    };
    call.lap("continue");
    let menu_loaded = taken.is_some() && !e_after;
    if !tries.is_empty() {
        fields.insert("menu_load".into(), json!(tries));
    }
    let first_load = match menu_loaded {
        true => "the load verb at the main menu",
        false => "continue",
    };
    fields.insert("first_load".into(), json!(first_load));
    fields.insert("continue_keys".into(), json!(sent.len()));
    let pushes = bridge_log::pushes(&game::state_text(&config, launched));
    fields.insert(
        "keys_after_gameplay".into(),
        json!(after_gameplay(&sent, &pushes)),
    );
    let name = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let continued_with = newest.as_deref().filter(|_| !menu_loaded).map(name);
    fields.insert("continued".into(), json!(continued_with));
    continued?;
    if !menu_loaded && newest.as_deref() != Some(save) {
        // From gameplay: game_load sends the load verb, or, without the channel, types it where typing is safe and
        // returns once its console closed (the first experiment boot: the console stays open after a console-started
        // load), so no key of the boot's reaches the console.
        ready(call)?;
        let save = name(save);
        let args = json!({"token": token, "save": save});
        match game::call(session, call, "game_load", &args) {
            Done::Ok(load) => {
                fields.insert("load".into(), json!(load));
            }
            done => return Err(format!("loading {save}: {}", reason(&done))),
        }
    }
    session.guard_saves()?;
    if spec.photo {
        // The harness's three presses of U, each given 3 s to show photo mode's flag.
        let photo = || {
            game::state_flags(&config, launched)
                .last()
                .is_some_and(|&f| bridge_log::in_photo(f))
        };
        let mut seen = false;
        for _ in 0..3 {
            ready(call)?;
            let cx = game::ctx(session, &json!({"token": token}), &[])?;
            cx.keys(call, &["u:200".into()], false, game::Focus::Bring)
                .map_err(|done| format!("entering photo mode: {}", reason(&done)))?;
            let t = Instant::now();
            while !photo() && t.elapsed() < PHOTO_WAIT {
                sleep(POLL);
            }
            if photo() {
                seen = true;
                break;
            }
        }
        if !seen {
            return Err("photo mode not seen on quen's state log (flag 128)".into());
        }
        session.guard_saves()?;
        call.lap("photo");
    }
    Ok(())
}

/// The Continue keys that may have reached gameplay (there should be none): each sent no earlier than the log's push
/// latency before its first push of plain gameplay, with how many ms after that push it went (quen's tick just before
/// the key against the tick the log stamps the push with). None when the log has no push of plain gameplay with a tick.
/// ponytail: a key that first had to bring the game to the front went out a little later than its tick.
fn after_gameplay(sent: &[(&str, u64)], pushes: &[(u64, i64)]) -> Option<Vec<Value>> {
    let (began, _) = pushes.iter().find(|(_, flags)| bridge_log::plain(*flags))?;
    let late = sent.iter().filter_map(|&(key, at)| {
        // In 32 bits: a log may stamp GetTickCount's tick, which wraps after 49.7 days.
        let ms = i64::from((at as u32).wrapping_sub(*began as u32) as i32);
        (ms >= -PUSH_LATENCY_MS).then(|| json!({"key": key, "ms_after_push": ms}))
    });
    Some(late.collect())
}

/// Why a game tool didn't do its work.
pub(crate) fn reason(done: &Done) -> String {
    match done {
        Done::Ok(_) => String::new(),
        Done::Refused(why)
        | Done::RefusedWith(why, _)
        | Done::Needs(why, _)
        | Done::Failed(why, _) => why.clone(),
    }
}

/// session_plan's fields: the save, every file change, what dx12user.settings gets (asked, and each line written: the
/// forced keys and the overrides) and the command.
pub(crate) fn plan_fields(config: &Config, spec: &Spec) -> Map<String, Value> {
    let mut fields = Map::new();
    // Its file name only: the folder may name the user or an organisation (OneDrive).
    fields.insert(
        "save".into(),
        json!(
            spec.save
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy())
        ),
    );
    let boot = config.capture_root().join("<boot>");
    fields.insert("changes".into(), json!(spec.change_lines(&boot)));
    fields.insert("settings_diff".into(), json!(spec.settings_diff));
    let written: Vec<String> = spec
        .changes
        .iter()
        .filter_map(|change| match change {
            Change::Rewrite { path, bytes, .. } if path.ends_with("dx12user.settings") => Some(
                boot::changed_lines(&fs::read(path).unwrap_or_default(), bytes),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    fields.insert("settings_written".into(), json!(written));
    let (argv, cwd) = spec.command(config, &boot);
    fields.insert("command".into(), json!({"argv": argv, "cwd": cwd}));
    if let Some(argv) = spec.profile.as_ref().and_then(|p| p.capture(&boot)) {
        fields.insert("capture_tool".into(), json!({"argv": argv, "cwd": boot}));
    }
    fields.insert(
        "mode".into(),
        json!(if spec.play { "play" } else { "test" }),
    );
    fields.insert("devkit".into(), json!(spec.devkit));
    fields.insert("photo".into(), json!(spec.photo));
    if let Some(conditions) = spec.settings_diff.get("conditions") {
        fields.insert("conditions".into(), conditions.clone());
    }
    if let Some(inv) = &spec.vortex {
        fields.insert("vortex".into(), crate::vortex::fields(inv, &spec.changes));
    }
    if !spec.park.is_empty() {
        fields.insert("park".into(), json!(spec.park));
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_near_or_after_gameplay_are_reported() {
        // The fade (257, 259), gameplay (3) at tick 19000, photo mode after it.
        let pushes = [(1000, 257), (1300, 259), (19_000, 3), (20_000, 129)];
        let sent = [
            ("e", 500),
            ("space", 18_700),
            ("space", 18_800),
            ("e", 19_500),
        ];
        assert_eq!(
            after_gameplay(&sent, &pushes).unwrap(),
            [
                json!({"key": "space", "ms_after_push": -200}),
                json!({"key": "e", "ms_after_push": 500})
            ],
            "within the push latency, and after"
        );
        assert_eq!(
            after_gameplay(&sent[..1], &pushes).unwrap(),
            [] as [Value; 0]
        );
        assert_eq!(after_gameplay(&sent, &pushes[..2]), None, "no gameplay yet");
        // quen's 64-bit tick against a log's 32-bit one that wrapped.
        assert_eq!(
            after_gameplay(&[("e", (1 << 32) + 10)], &[(5, 1)]).unwrap(),
            [json!({"key": "e", "ms_after_push": 5})]
        );
    }
}
