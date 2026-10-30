//! Launch profiles for the capture and profiling tools: what a launch under RenderDoc, Nsight
//! Systems or Nsight Graphics changes beyond the boot, as the boot's own journaled steps, parks and settings
//! overrides; the command that starts the game under the tool; and the conditions the boot row records. The Python
//! scripts quen grew out of learned how each tool must be started: each lesson here is a claim the live tests
//! re-prove.
//!
//! - `renderdoc`: Witcher 3 imports D3D12 and DXGI from NVIDIA Streamline's `sl.interposer.dll`, which finds the real
//!   exports by walking their export tables, so RenderDoc's hooks never see the calls. The launch checks that the
//!   game's interposer is NVIDIA's signed original (its sha256 pinned in the doctor, and Authenticode), backs it up and
//!   writes the patched dev build over it (tools/streamline: it resolves through GetProcAddress), places
//!   `sl.interposer.json` (`enableInterposer: false`, read only by non-production builds), parks ReShade, and turns
//!   DLSS RR, path tracing and ray tracing off, FXAA on, and frame generation off. The game starts under `renderdoccmd
//!   capture`; the boot waits for RenderDoc's log to show the wrapped D3D12 device, and game_screenshot method
//!   renderdoc takes a capture with F12, sending nothing until the .rdc is written. The replay puts the original
//!   back, and the end checks it by sha256 and Authenticode.
//! - `nsys`: ReShade parked (with it loaded, nsys froze the game); the capture range runs between two F11 presses
//!   (nsys refuses a hotkey range with `--duration-frames`).
//! - `ngfx`: ReShade parked (with it loaded, the game exited 4 s into the launch: feature tests 2), GPU Trace after its
//!   hotkey, its output folder made first (else "No such output directory"), never `--platform` (Qt takes it as its
//!   own option and ngfx fails to start). GPU Trace locks the GPU clocks to base, so its times are relative only. ngfx
//!   exits once its trace is exported and takes the game with it: one trace per boot.
//! - `ngfx-capture`: an Nsight Graphics frame capture (`.ngfx-capture`) rather than a GPU Trace. ReShade parked, as
//!   under ngfx; ray tracing, path tracing and DLSS RR stay on, frame generation off. ngfx-capture.exe (`--help`,
//!   2026.3.1) writes into the run's folder, waits on no prompt (nothing closed for Nsight's "interfering
//!   applications", no hang-detection crash report) and shows no HUD. The fallback, ngfx.exe's Graphics Capture
//!   activity (the RTX Hair session's launch), writes into Nsight's own captures folder: quen moves out what the launch
//!   wrote. The plan refuses until Nsight asked its first-start privacy question (its settings file, read only).
//!   game_screenshot method ngfx presses F11 and waits for the file; the end records any capture not yet recorded.
//! - `tracy`: the Tracy instrumentation in quen's own package (W3Quen.asi from quen-w3 0.1.3, w3/asi/trace.h), dormant
//!   unless the game's environment has `W3_QUEN_TRACY=1`, which only this profile sets (with `TRACY_PORT`, a free
//!   loopback port). The launch checks the capture tool is Tracy's version of the package's client, parks the mods the
//!   KB's hooks.tsv names at the functions the ASI hooks (hooks.inc), and starts Tracy's capture tool in the session's
//!   job before the game, writing `<NAME>.tracy` into the boot's capture folder. The boot waits for W3Quen.log to say a
//!   capture connected. The end lets the game go first, so the tool writes its capture, then records it.
//!
//! The boot row records the profile and the run's conditions in `settings_diff` (`profile`, `conditions`) until the
//! ledger has columns for them.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use quen_core::boot::Change;
use quen_core::journal::{sha256_file, sha256_hex};
use quen_core::ledger::{Capture, Ledger};
use serde_json::{Map, Value, json};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_NULL};
use windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE;
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

use crate::boot::plain;
use crate::config::Config;
use crate::game::{Ctx, Fields, Focus};
use crate::session::{self, Call, Done, Live};

/// The game's Streamline interposer, and the switch file its non-production builds read, beside the exe.
pub(crate) const INTERPOSER: &str = "sl.interposer.dll";
const INTERPOSER_JSON: &str = "sl.interposer.json";
const JSON: &str = "{\"enableInterposer\": false}\n";
/// ReShade, parked under every profiler.
const RESHADE: &str = "dxgi.dll";
/// rdc_boot.py's settings for RenderDoc (it has no ray tracing): DLSS RR, path tracing and ray tracing off, FXAA.
/// Frame generation is the boot's own switch (fg).
const RENDERDOC_SETTINGS: &str = "EnableDLSSRR=false,PTEnable=false,EnableRT=false,AAMode=1";
/// RenderDoc's capture key.
const CAPTURE_KEY: &str = "f12";
/// Nsight Graphics' capture key: ngfx-capture's and ngfx's Graphics Capture default.
const NGFX_KEY: &str = "f11";
const NGFX_EXT: &str = "ngfx-capture";
/// Nsight's own captures folder in Documents, where ngfx.exe's Graphics Capture wrote whatever `--output-dir` said (the
/// RTX Hair session's hand run, 2026-10-22).
/// ponytail: Nsight's default, not its settings' `CorePlugin.DefaultDocumentsFolder`: a moved one isn't watched.
const NSIGHT_CAPTURES: &str = r"NVIDIA Nsight Graphics\GraphicsCaptures";
/// What a capture tool writes (the captures row's kind), after which key, and its row's method.
struct Writes {
    ext: &'static str,
    key: &'static str,
    /// Why no file came, likely.
    hint: &'static str,
    method: &'static str,
}
const RDC: Writes = Writes {
    ext: "rdc",
    key: "F12",
    hint: "RenderDoc not hooked?",
    method: "renderdoc",
};
const NGFX: Writes = Writes {
    ext: NGFX_EXT,
    key: "F11",
    hint: "Nsight not attached?",
    method: "ngfx_capture",
};
/// What RenderDoc's log says once it wrapped the game's D3D12 device (rdc_boot.py's check).
const WRAPPED: [&str; 2] = ["Created wrapped D3D12 device", "New D3D12 device"];
/// rdc_boot.py's capture pacing; a second capture waits `GAP` after the last was written.
const PACE: Pace = Pace {
    appear: Duration::from_secs(60),
    stable: Duration::from_secs(5),
    growing: Duration::from_secs(600),
    hung: Duration::from_secs(120),
    settle: Duration::from_secs(3),
};
const GAP: Duration = Duration::from_secs(10);
/// The free space a capture needs: rdc_boot.py's 12 GB (its captures were 3-5 GB).
const FREE_MIN: u64 = 12_000_000_000;
const POLL: Duration = Duration::from_millis(250);
/// The Tracy client in quen's package (w3/third_party/tracy, its TracyVersion.hpp), and the package versions that have
/// it (`test`: the end-to-end tests' synthetic package, QUEN_TEST_PINS). Tracy wants its capture tool at the client's
/// version: 0.14.1's tracy-capture refuses a client of another protocol version.
pub(crate) const TRACY_CLIENT: &str = "0.14.1";
const TRACY_PACKAGES: &[&str] = &[
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
    "test",
];
/// The functions W3Quen.asi hooks in a tracy launch: the list it is built from.
const TRACY_HOOKS: &str = include_str!("../../../w3/asi/hooks.inc");
/// The capture tool's output, in the session's logs.
pub(crate) const TRACY_LOG: &str = "tracy-capture.log";
/// W3Quen.asi's log beside the exe, where it says what its Tracy part did.
const QUEN_LOG: &str = "W3Quen.log";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    RenderDoc,
    Nsys,
    Ngfx,
    NgfxCapture,
    Tracy,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::RenderDoc => "renderdoc",
            Tool::Nsys => "nsys",
            Tool::Ngfx => "ngfx",
            Tool::NgfxCapture => "ngfx-capture",
            Tool::Tracy => "tracy",
        }
    }
}

/// A checked `profiler` request: the tool and the run's name, the file changes it adds to the boot, its settings
/// overrides and what it parks.
pub(crate) struct Profile {
    pub(crate) tool: Tool,
    /// Names the tool's output in the boot's capture folder.
    name: String,
    exe: PathBuf,
    /// Made after the boot's own changes, journaled like them: the Streamline swap.
    pub(crate) changes: Vec<Change>,
    /// dx12user.settings overrides, applied after the user's.
    pub(crate) settings: &'static str,
    /// What it parks of what is there: ReShade's dxgi.dll for renderdoc, nsys and ngfx; for tracy, the ASIs of the mods
    /// that hook the functions W3Quen's Tracy hooks (hooks.tsv).
    pub(crate) park: Vec<String>,
    /// tracy: the hooks.tsv rows at those functions, for the boot row.
    collisions: Vec<Value>,
    /// tracy: the loopback port the game's Tracy listens on (`TRACY_PORT`) and the capture tool connects to.
    port: u16,
}

impl Profile {
    /// Checks `arg` (`renderdoc:NAME`, `nsys:NAME`, `ngfx:NAME`, `ngfx-capture:NAME`, `tracy:NAME`) against the config
    /// and the game's files in `bin`, read-only: an error means nothing was changed. `signed` are the pinned sha256s of NVIDIA's interposer,
    /// `verify` its Authenticode check; `package` is the version of quen's package in the game, `capture_version` the
    /// version Tracy's capture tool says it is ([`tracy_version`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn parse(
        config: &Config,
        arg: &str,
        bin: &Path,
        backups: &Path,
        signed: &[&str],
        verify: &dyn Fn(&Path) -> Result<(), String>,
        package: Option<&str>,
        capture_version: &dyn Fn(&Path) -> Result<String, String>,
    ) -> Result<Profile, String> {
        let usage = || {
            format!(
                "profiler {arg:?} is not renderdoc:NAME, nsys:NAME, ngfx:NAME, ngfx-capture:NAME or tracy:NAME (NAME: \
                 letters, digits, _ and -)"
            )
        };
        let (tool, name) = arg
            .split_once(':')
            .filter(|(_, name)| plain(name, &['-']))
            .ok_or_else(usage)?;
        // ngfx-capture: the configured exe, else the ngfx-capture.exe beside ngfx, else ngfx (the fallback route).
        let beside = config
            .ngfx
            .as_ref()
            .map(|n| n.with_file_name("ngfx-capture.exe"));
        let capture = config.ngfx_capture.clone().or_else(|| {
            beside
                .filter(|p| p.is_file())
                .or_else(|| config.ngfx.clone())
        });
        let (tool, exe, key) = match tool {
            "renderdoc" => (Tool::RenderDoc, &config.renderdoc, "renderdoc"),
            "nsys" => (Tool::Nsys, &config.nsys, "nsys"),
            "ngfx" => (Tool::Ngfx, &config.ngfx, "ngfx"),
            "ngfx-capture" => (Tool::NgfxCapture, &capture, "ngfx_capture (or ngfx)"),
            "tracy" => (Tool::Tracy, &config.tracy_capture, "tracy_capture"),
            _ => return Err(usage()),
        };
        let exe = exe.clone().filter(|p| p.is_file()).ok_or(format!(
            "profiler {arg}: {key} in the local config must be its exe"
        ))?;
        if matches!(tool, Tool::Ngfx | Tool::NgfxCapture) {
            ngfx_args(config.ngfx_args.as_deref().unwrap_or_default())?;
        }
        let mut profile = Profile {
            tool,
            name: name.into(),
            exe,
            changes: Vec::new(),
            settings: "",
            park: Vec::new(),
            collisions: Vec::new(),
            port: 0,
        };
        if tool != Tool::Tracy && bin.join(RESHADE).exists() {
            profile.park.push(RESHADE.into());
        }
        if tool == Tool::RenderDoc {
            profile.settings = RENDERDOC_SETTINGS;
            profile.changes = swap(config, bin, backups, signed, verify)?;
        }
        if tool == Tool::Tracy {
            profile.tracy(config, bin, package, capture_version)?;
        }
        Ok(profile)
    }

    /// The tracy profile's checks and parks: the capture tool is Tracy's version of the client in the game's package,
    /// that package has the client, and the mods the KB's hooks.tsv names at W3Quen's Tracy hook sites are parked (its
    /// rows for the W3Trace ASI: "collides with W3EngineTweaks (park it for captures)"). Then a free loopback port.
    fn tracy(
        &mut self,
        config: &Config,
        bin: &Path,
        package: Option<&str>,
        capture_version: &dyn Fn(&Path) -> Result<String, String>,
    ) -> Result<(), String> {
        let version = capture_version(&self.exe)?;
        if version != TRACY_CLIENT {
            return Err(format!(
                "tracy_capture {} is Tracy {version}: quen's package has Tracy {TRACY_CLIENT}'s client, and both ends \
                 must be the same version",
                self.exe.display()
            ));
        }
        if !package.is_some_and(|p| TRACY_PACKAGES.contains(&p)) {
            return Err(format!(
                "profiler tracy needs quen-w3 0.1.3 or later in the game: its W3Quen.asi has Tracy's client (installed: {})",
                package.unwrap_or("no pinned package")
            ));
        }
        let kb = config.kb.as_ref().ok_or(
            "profiler tracy needs kb in the local config: the KB's hooks.tsv names the mods that hook the functions \
             W3Quen's Tracy hooks, which the launch parks",
        )?;
        let tsv = kb.join("hooks.tsv");
        let text = fs::read_to_string(&tsv).map_err(|e| format!("{}: {e}", tsv.display()))?;
        for row in collisions(&text)? {
            let asi = format!("{}.asi", row["mod"].as_str().unwrap_or_default());
            if bin.join(&asi).is_file() && !self.park.contains(&asi) {
                self.park.push(asi);
            }
            self.collisions.push(row);
        }
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).and_then(|l| l.local_addr());
        self.port = port
            .map_err(|e| format!("no free loopback port for Tracy: {e}"))?
            .port();
        Ok(())
    }

    /// What starts the game under the tool, writing into `out` (the boot's capture folder), and in which folder.
    pub(crate) fn command(
        &self,
        config: &Config,
        exe: String,
        bin: &Path,
        out: &Path,
        frames: Option<&str>,
    ) -> (Vec<String>, PathBuf) {
        let mut argv = vec![self.exe.display().to_string()];
        let target = out.join(&self.name).display().to_string();
        match self.tool {
            // rdc_boot.py's launch: renderdoccmd starts the game with RenderDoc in it and exits; its captures are
            // <out>\<NAME>_frame<N>.rdc.
            Tool::RenderDoc => {
                argv.extend(["capture", "-d"].map(String::from));
                argv.push(bin.display().to_string());
                argv.extend(["-c".into(), target, exe]);
                (argv, bin.to_owned())
            }
            Tool::Nsys => {
                argv.extend(
                    [
                        "profile",
                        "--trace=dx12",
                        "--dx12-gpu-workload=batch",
                        "--dx12-wait-calls=true",
                        "--sample=none",
                        "--cpuctxsw=none",
                        "--capture-range=hotkey",
                        "--hotkey-capture=F11",
                        "--capture-range-end=stop",
                        "--force-overwrite=true",
                        "-o",
                    ]
                    .map(String::from),
                );
                argv.extend([target, exe]);
                (argv, bin.to_owned())
            }
            // ngfx-capture.exe, from its own folder as ngfx: no prompt that would wait (nothing closed for Nsight's
            // "interfering applications", no crash report offered past 60 s without a frame), GPU_UPLOAD heaps demoted to
            // system memory as Nsight's UI captures (the help's default, but its variable stays 0 unless passed), one
            // frame, no HUD in quen's screenshots. No --capture-hotkey: it takes the next argument as its key (boot 258,
            // 259), and without it the key is F11. ngfx_args are GPU Trace's: not passed.
            Tool::NgfxCapture if !self.fallback() => {
                argv.extend(["--exe".into(), exe, "--working-dir".into()]);
                argv.push(bin.display().to_string());
                argv.push("--output-dir".into());
                argv.push(self.output_dir(out).display().to_string());
                argv.extend(
                    [
                        "--no-block-on-interfering-application",
                        "--disable-hang-detection",
                        "--hvvm-demote",
                        "--frame-count=1",
                        "--no-hud",
                    ]
                    .map(String::from),
                );
                let dir = self.exe.parent().unwrap_or(bin).to_owned();
                (argv, dir)
            }
            // ngfx runs from its own folder (from the game's, its Qt platform init fails); --dir is the game's.
            Tool::Ngfx | Tool::NgfxCapture => {
                let activity = match self.tool {
                    Tool::Ngfx => "GPU Trace Profiler",
                    _ => "Graphics Capture",
                };
                argv.extend(["--activity", activity, "--exe"].map(String::from));
                argv.extend([exe, "--dir".into(), bin.display().to_string()]);
                argv.push("--output-dir".into());
                argv.push(self.output_dir(out).display().to_string());
                if self.tool == Tool::NgfxCapture {
                    // The fallback, as the RTX Hair session launched it: without --no-timeout Nsight closed the game
                    // minutes in. It writes into Nsight's own captures folder, whatever --output-dir says.
                    argv.extend(
                        ["--no-timeout", "--hotkey-capture", "--frame-count", "1"]
                            .map(String::from),
                    );
                } else {
                    argv.extend(
                        [
                            "--start-after-hotkey",
                            "--limit-to-frames",
                            frames.unwrap_or("3"),
                        ]
                        .map(String::from),
                    );
                    argv.extend(["--max-duration-ms", "3000", "--auto-export"].map(String::from));
                    // Only the user's config adds ngfx arguments, never the agent (security audit, 2026-10-17).
                    argv.extend(words(config.ngfx_args.as_deref().unwrap_or_default()));
                }
                let dir = self.exe.parent().unwrap_or(bin).to_owned();
                (argv, dir)
            }
            // The game itself: Tracy is in quen's package, and its capture tool is a second process ([`capture`]).
            Tool::Tracy => (vec![exe], bin.to_owned()),
        }
    }

    /// The capture tool a tracy launch starts in the session's job before the game, writing `<out>\<NAME>.tracy`
    /// (`out`: the boot's capture folder): Tracy's command-line capture, on the loopback address and the launch's port.
    pub(crate) fn capture(&self, out: &Path) -> Option<Vec<String>> {
        (self.tool == Tool::Tracy).then(|| {
            let file = out.join(format!("{}.tracy", self.name));
            let mut argv = vec![self.exe.display().to_string()];
            argv.extend(["-a", "127.0.0.1", "-p"].map(String::from));
            argv.extend([
                self.port.to_string(),
                "-o".into(),
                file.display().to_string(),
            ]);
            argv
        })
    }

    /// What the game's environment gets beside quen's token and pid: a tracy launch's trigger and Tracy's port.
    pub(crate) fn env(&self) -> Vec<(&'static str, String)> {
        match self.tool {
            Tool::Tracy => vec![
                ("W3_QUEN_TRACY", "1".into()),
                ("TRACY_PORT", self.port.to_string()),
            ],
            _ => Vec::new(),
        }
    }

    /// The folder the tool writes into, made before the launch: ngfx's `--output-dir` must exist.
    pub(crate) fn output_dir(&self, out: &Path) -> PathBuf {
        match self.tool {
            Tool::Ngfx | Tool::NgfxCapture => out.join(&self.name),
            _ => out.to_owned(),
        }
    }

    /// ngfx-capture's fallback route, ngfx.exe's Graphics Capture activity: when the exe it runs is ngfx.exe.
    fn fallback(&self) -> bool {
        self.tool == Tool::NgfxCapture
            && self
                .exe
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("ngfx.exe"))
    }

    /// The boot row's record of the profile; a tracy one's also names the hooks.tsv rows at its hook sites, an
    /// ngfx-capture one the launcher it ran (its file name).
    pub(crate) fn record(&self) -> Value {
        let mut record =
            json!({"tool": self.tool.name(), "settings": self.settings, "park": self.park});
        if self.tool == Tool::Tracy {
            record["collisions"] = json!(self.collisions);
            record["port"] = json!(self.port);
        }
        if self.tool == Tool::NgfxCapture {
            record["launcher"] = json!(self.exe.file_name().map(|n| n.to_string_lossy()));
        }
        record
    }

    /// ngfx-capture: Nsight asked its first-start privacy question for the installed version (`nsight`: an exe's
    /// version and Nsight's settings file, [`nsight`]). quen reads the answer only, and never gives it. Other profiles:
    /// Ok.
    pub(crate) fn asked(&self, nsight: &dyn Fn(&Path) -> Installed) -> Result<(), String> {
        if self.tool != Tool::NgfxCapture {
            return Ok(());
        }
        let (version, ini) = nsight(&self.exe);
        let version = version.ok_or(format!(
            "{} has no version resource: whether Nsight asked its first-start privacy question can't be checked",
            self.exe.display()
        ))?;
        match ini.is_some_and(|ini| quen_core::ngfx::privacy_asked(&ini, &version)) {
            true => Ok(()),
            false => Err(format!(
                "Nsight Graphics {version} hasn't asked its first-start privacy question yet: the user opens Nsight \
                 Graphics once and answers it, then plan again (quen never answers it)"
            )),
        }
    }

    /// The conditions the run is under, for the boot row: the tool, and what it changes about the game.
    pub(crate) fn conditions(&self) -> Vec<&'static str> {
        let mut conditions = vec![match self.tool {
            Tool::NgfxCapture => "ngfx_capture",
            tool => tool.name(),
        }];
        if self.tool == Tool::RenderDoc {
            conditions.push("streamline_dev_interposer");
        }
        if self.park.iter().any(|p| p == RESHADE) {
            conditions.push("reshade_parked");
        }
        if self.tool == Tool::Ngfx {
            conditions.push("clocks_locked_to_base");
        }
        conditions
    }

    /// The first of the profile's settings keys the user's overrides name too (`flat`: `Key=Value,...`; `sections`:
    /// `Section:Key=Value;...`): refused rather than one quietly winning.
    pub(crate) fn overridden(&self, flat: &str, sections: &str) -> Option<&'static str> {
        let keys = flat.split(',').chain(
            sections
                .split(';')
                .map(|item| item.split_once(':').map_or(item, |(_, kv)| kv)),
        );
        let user: Vec<&str> = keys
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.trim()))
            .collect();
        self.settings
            .split(',')
            .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
            .find(|k| user.contains(k))
    }
}

/// The Streamline swap's changes, after its checks: the game's interposer is NVIDIA's signed original (a pinned
/// sha256, and Authenticode), no switch file is there, and the dev build is another file.
fn swap(
    config: &Config,
    bin: &Path,
    backups: &Path,
    signed: &[&str],
    verify: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Vec<Change>, String> {
    let dev = config
        .streamline_dev_interposer
        .as_ref()
        .filter(|p| p.is_file())
        .ok_or(
            "profiler renderdoc needs streamline_dev_interposer in the local config: the patched dev build of \
             sl.interposer.dll (tools/streamline in quen-mcp)",
        )?;
    let path = bin.join(INTERPOSER);
    let original = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha = sha256_hex(&original);
    if !signed.contains(&sha.as_str()) {
        return Err(format!(
            "{} is not NVIDIA's signed original (sha256 {sha}): a dev build a crashed session left (session_recover \
             puts the original back), or a game update (Steam's Verify integrity of game files; quen's pin then \
             needs the new one)",
            path.display()
        ));
    }
    verify(&path).map_err(|why| {
        format!(
            "{}: its Authenticode signature doesn't verify: {why}",
            path.display()
        )
    })?;
    let json = bin.join(INTERPOSER_JSON);
    if json.exists() {
        return Err(format!(
            "{} exists: quen places it only where none is (rdc_boot.py's restore removes one it left)",
            json.display()
        ));
    }
    let bytes = fs::read(dev).map_err(|e| format!("{}: {e}", dev.display()))?;
    if signed.contains(&sha256_hex(&bytes).as_str()) {
        return Err(format!(
            "streamline_dev_interposer {} is NVIDIA's signed production build: the swap needs the patched dev build \
             (tools/streamline)",
            dev.display()
        ));
    }
    Ok(vec![
        Change::Rewrite {
            backup: backups.join(INTERPOSER),
            path,
            sha,
            bytes,
            ignore: Vec::new(),
        },
        Change::Place {
            stage: bin.join(format!("{INTERPOSER_JSON}.quen-stage")),
            dest: json,
            bytes: JSON.into(),
            from: None,
        },
    ])
}

/// Whether a session journal swapped the interposer: its end then checks the restore ([`restored`]).
pub(crate) fn swapped(journal: &[u8]) -> bool {
    journal
        .windows(INTERPOSER.len())
        .any(|w| w == INTERPOSER.as_bytes())
}

/// After the replay of a journal that swapped the interposer: NVIDIA's signed original is back (a pinned sha256 and
/// Authenticode) and the switch file is gone (rdc_boot.py's restore line). Err says what isn't.
pub(crate) fn restored(
    bin: &Path,
    signed: &[&str],
    verify: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Value, String> {
    let path = bin.join(INTERPOSER);
    let sha = fs::read(&path).map(|b| sha256_hex(&b));
    let sha_ok = sha.as_ref().is_ok_and(|s| signed.contains(&s.as_str()));
    let signature = verify(&path);
    let json_left = bin.join(INTERPOSER_JSON).exists();
    let check = json!({
        "path": path,
        "sha256": sha.as_ref().map_or_else(|e| e.to_string(), Clone::clone),
        "sha256_ok": sha_ok,
        "authenticode": signature.as_ref().map_or_else(Clone::clone, |()| "valid".into()),
        "json_left": json_left,
    });
    if sha_ok && signature.is_ok() && !json_left {
        return Ok(check);
    }
    Err(format!(
        "NVIDIA's signed {INTERPOSER} is not back as it was ({check}): Steam's Verify integrity of game files restores \
         it; until then the game runs another Streamline"
    ))
}

/// RenderDoc's log since `launched` that shows it wrapped the game's D3D12 device: the swap worked. RenderDoc logs to
/// `%TEMP%\RenderDoc`, and the game gets quen's TEMP.
pub(crate) fn wrapped(launched: SystemTime) -> Option<PathBuf> {
    let since = launched.checked_sub(Duration::from_secs(5))?;
    let logs = fs::read_dir(std::env::temp_dir().join("RenderDoc")).ok()?;
    logs.flatten().map(|e| e.path()).find(|log| {
        let name = log.file_name().unwrap_or_default().to_string_lossy();
        let fresh = fs::metadata(log)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m >= since);
        fresh
            && name.starts_with("RenderDoc_")
            && name.ends_with(".log")
            && fs::read(log).is_ok_and(|b| {
                let text = String::from_utf8_lossy(&b);
                WRAPPED.iter().any(|w| text.contains(w))
            })
    })
}

/// An installed Nsight's version and its settings file's text, when there are.
pub(crate) type Installed = (Option<String>, Option<String>);

/// The installed Nsight's version (`exe`'s version resource) and, with one, the text of Nsight's settings file:
/// [`Profile::asked`]'s inputs.
pub(crate) fn nsight(exe: &Path) -> Installed {
    let version = crate::win::file_version(exe);
    let ini = version
        .as_ref()
        .and_then(|_| fs::read_to_string(crate::doctor::nsight_ini()?).ok());
    (version, ini)
}

/// The version Tracy's capture tool says it is: run without arguments, it prints `tracy-capture 0.14.1 / <git ref>` on
/// its usage line and exits (0.14.1's capture.cpp). Only a file named tracy-capture.exe is run (Tracy's other exes open
/// windows), without a window, for at most 10 s.
pub(crate) fn tracy_version(exe: &Path) -> Result<String, String> {
    let named = exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("tracy-capture.exe"));
    if !named {
        return Err(format!(
            "tracy_capture {} is not tracy-capture.exe",
            exe.display()
        ));
    }
    let mut child = std::process::Command::new(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let t = Instant::now();
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if t.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            return Err(format!("{} still ran after 10 s", exe.display()));
        }
        sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let said = String::from_utf8_lossy(&out.stdout);
    said.lines()
        .find_map(|l| l.strip_prefix("tracy-capture "))
        .and_then(|rest| rest.split(' ').next())
        .map(str::to_owned)
        .ok_or(format!(
            "{} printed no `tracy-capture <version>` line",
            exe.display()
        ))
}

/// The name and RVA of each function W3Quen.asi hooks in a tracy launch, from its hooks.inc.
fn tracy_sites() -> Vec<(&'static str, u32)> {
    TRACY_HOOKS
        .lines()
        .filter_map(|line| {
            let mut args = line.strip_prefix("W3_HOOK(")?.split(", ");
            let name = args.next()?;
            let rva = u32::from_str_radix(args.next()?.strip_prefix("0x")?, 16).ok()?;
            Some((name, rva))
        })
        .collect()
}

/// The rows of the KB's hooks.tsv (`site kind mod version note`, tab-separated) where another mod changes a site W3Quen
/// hooks in a tracy launch: a hook, patch or replacement at that RVA, not a `call` or `body-check` (which change
/// nothing). The W3Trace ASI's rows and W3Quen's own are not another mod's. A malformed row is refused: it could hide
/// one.
fn collisions(tsv: &str) -> Result<Vec<Value>, String> {
    let sites = tracy_sites();
    let mut found = Vec::new();
    for (n, line) in tsv.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("site\t") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [site, kind, module, version, ..] = fields[..] else {
            return Err(format!(
                "hooks.tsv line {}: not site, kind, mod, version, note",
                n + 1
            ));
        };
        let rva = site
            .strip_prefix("0x")
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .ok_or(format!(
                "hooks.tsv line {}: site {site:?} is not 0x<hex>",
                n + 1
            ))?;
        if ["call", "body-check"].contains(&kind) || ["W3Trace", "W3Quen"].contains(&module) {
            continue;
        }
        if let Some((hook, _)) = sites.iter().find(|(_, r)| *r == rva) {
            found.push(
                json!({"mod": module, "version": version, "kind": kind, "site": site, "hook": hook,
                              "note": fields.get(4)}),
            );
        }
    }
    Ok(found)
}

/// What W3Quen.log (beside the exe in `bin`, written since `launched`) says of a tracy launch: None until a capture
/// connected, then how many hooks it installed, which it skipped, and `started_utc`: its `tracy: started` line's tick
/// (GetTickCount64, which begins each line) as UTC, read against this PC's clock now. Feature tests 4 found
/// tracy-csvexport's `-u` times counting from that line. Err once its Tracy part refused or hooked nothing.
pub(crate) fn tracy_ready(bin: &Path, launched: SystemTime) -> Option<Result<Value, String>> {
    let log = bin.join(QUEN_LOG);
    let fresh = fs::metadata(&log)
        .and_then(|m| m.modified())
        .is_ok_and(|m| m >= launched);
    let text = fresh.then(|| fs::read(&log).ok()).flatten()?;
    let text = String::from_utf8_lossy(&text);
    let said: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_once("tracy: ").map(|(_, said)| said))
        .collect();
    let installed = said.iter().find_map(|s| s.strip_suffix(" hooks installed"));
    if said.iter().any(|s| s.starts_with("REFUSED"))
        || installed.is_some_and(|n| n.starts_with("0 of"))
    {
        return Some(Err(format!(
            "W3Quen.asi installed no Tracy hook in this game ({}, kept with the session's game logs): no capture",
            log.display()
        )));
    }
    if !said.contains(&"a capture connected") {
        return None;
    }
    let skipped: Vec<&str> = said
        .iter()
        .filter(|s| s.contains("  skipped: "))
        .filter_map(|s| s.split_whitespace().next())
        .collect();
    let started = text
        .lines()
        .find(|l| l.contains(" tracy: started"))
        .and_then(|l| l.split_whitespace().next()?.parse::<u64>().ok());
    let ago = |tick| Duration::from_millis(crate::win::tick().saturating_sub(tick));
    let started_utc = started.map(|tick| session::utc_ms(SystemTime::now() - ago(tick)));
    Some(Ok(
        json!({"hooks_installed": installed, "skipped": skipped, "capture": "connected",
                   "started_utc": started_utc}),
    ))
}

/// A tracy launch's end, after the kill: the capture tool wrote `<capture root>\<boot>\<NAME>.tracy` as the game went
/// (session.rs's kill lets the game go first), and the file gets its capture row (kind and method `tracy`). The field
/// names the file and the tool's log, or says there is no capture. None for any other launch.
pub(crate) fn tracy_end(config: &Config, live: &Live, call: &mut Call) -> Option<Value> {
    let name = live.profiler.as_deref()?.strip_prefix("tracy:")?;
    let root = config.capture_root();
    let path = root
        .join(live.boot.to_string())
        .join(format!("{name}.tracy"));
    let log = live.logs.join(TRACY_LOG);
    let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or_default();
    if bytes == 0 {
        return Some(
            json!({"capture": null, "log": log, "why": "the capture tool wrote no capture: its log says why"}),
        );
    }
    let sha256 = call.artifact(&path);
    let mut field = json!({"capture": path, "bytes": bytes, "log": log});
    let at = session::now() as i64;
    match sha256
        .ok_or(format!("{}: unreadable", path.display()))
        .and_then(|sha256| {
            add_row(
                config,
                row(live.boot, sha256, bytes, "tracy", "tracy"),
                &path,
                at,
            )
        }) {
        Ok(id) => call.row("captures", id),
        Err(why) => field["ledger_error"] = json!(why),
    }
    Some(field)
}

/// A capture file's row on `boot`, before [`add_row`] gives it its path and time.
fn row(boot: i64, sha256: String, bytes: u64, kind: &str, method: &str) -> Capture {
    Capture {
        boot_id: Some(boot),
        sha256,
        bytes: bytes as i64,
        kind: kind.into(),
        method: Some(method.into()),
        ..Capture::default()
    }
}

/// Adds `row` for `path` (under the capture root), written at `at` (Unix seconds): its id.
fn add_row(config: &Config, mut row: Capture, path: &Path, at: i64) -> Result<i64, String> {
    let mut ledger = Ledger::open(config.state_dir().join("quen.db")).map_err(|e| e.to_string())?;
    let root = config.capture_root();
    row.path = path
        .strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string();
    row.t = ledger.utc(at).map_err(|e| e.to_string())?;
    ledger.add_capture(&row).map_err(|e| e.to_string())
}

fn renderdoc(live: &Live) -> bool {
    live.profiler
        .as_deref()
        .is_some_and(|p| p.starts_with("renderdoc:"))
}

/// The run's name in an ngfx-capture launch.
fn ngfx_name(live: &Live) -> Option<&str> {
    live.profiler.as_deref()?.strip_prefix("ngfx-capture:")
}

/// game_keys under a renderdoc or ngfx-capture launch: F12 is RenderDoc's capture key and F11 Nsight's, and a capture
/// freezes the game for seconds, so captures go only through game_screenshot method renderdoc or ngfx, which send
/// nothing until the file is written and record it.
pub(crate) fn capture_key(live: &Live, tokens: &[String]) -> Result<(), String> {
    let pressed = |key: &str| {
        tokens
            .iter()
            .any(|t| t.split([':', '+']).any(|k| k.eq_ignore_ascii_case(key)))
    };
    if renderdoc(live) && pressed(CAPTURE_KEY) {
        return Err(
            "F12 is RenderDoc's capture key in this session: take captures with game_screenshot method renderdoc, \
             which sends no key until the capture is written"
                .into(),
        );
    }
    if ngfx_name(live).is_some() && pressed(NGFX_KEY) {
        return Err(
            "F11 is Nsight's capture key in this session: take captures with game_screenshot method ngfx, which \
             sends no key until the capture is written and records it"
                .into(),
        );
    }
    Ok(())
}

/// game_screenshot method renderdoc: one RenderDoc capture (F12) into the boot's capture folder as `<tag>.rdc`, with
/// its ledger row. Only in a renderdoc session, only with the dev interposer still the game's and RenderDoc's device
/// wrapped, never without the free space; then nothing is sent and nothing returns until the capture is written.
pub(crate) fn capture(cx: &Ctx, call: &mut Call, tag: &str) -> Result<Fields, Done> {
    let config = cx.config;
    if !renderdoc(cx.live) {
        return Err(
            "method renderdoc needs a session launched with profiler renderdoc:NAME"
                .to_string()
                .into(),
        );
    }
    let bin = config
        .bin_dir()
        .ok_or_else(|| "no game_dir in the local config".to_owned())?;
    swapped_in(config, &bin)?;
    let log = wrapped(cx.live.launched).ok_or_else(|| {
        "RenderDoc's log shows no wrapped D3D12 device in this launch: no capture".to_owned()
    })?;
    let root = config.capture_root();
    let out = root.join(cx.live.boot.to_string());
    let dest = out.join(format!("{tag}.rdc"));
    if dest.exists() {
        let why = format!(
            "{} exists: tag {tag:?} is taken in this boot",
            dest.display()
        );
        return Err(why.into());
    }
    let free = free_bytes(&out)?;
    if free < FREE_MIN {
        return Err(format!(
            "{} GB free in {}: a capture needs {} GB (they are 3-5 GB each)",
            free / 1_000_000_000,
            out.display(),
            FREE_MIN / 1_000_000_000
        )
        .into());
    }
    let pids = cx.live.job.pids().unwrap_or_default();
    let (window, _) = crate::capture::game_window(&pids)?;
    let list = || files(&out, RDC.ext, SystemTime::UNIX_EPOCH);
    let (at, before) = pressed(cx, call, &list, CAPTURE_KEY)?;
    // From the key on, nothing is sent and nothing returns, a cancel included, until the capture is written: the game
    // is frozen meanwhile, and the next tool's keys would land in it.
    let polled = || {
        call.progress("RenderDoc is writing the capture");
        list()
    };
    let written = wait_file(&before, &polled, &|| responsive(window), &PACE, &RDC);
    let (file, bytes) = written.map_err(|why| Done::Failed(why, Map::new()))?;
    call.lap("capture");
    // RenderDoc names it <NAME>_frame<N>.rdc: renamed to the tag once written, or kept where it is.
    let path = match fs::rename(&file, &dest) {
        Ok(()) => dest,
        Err(_) => file,
    };
    let mut fields = Map::new();
    fields.insert("renderdoc_log".into(), json!(log));
    recorded(cx, call, &RDC, &path, (bytes, at), &mut fields)?;
    Ok(fields)
}

/// What a capture call does once its checks passed: waits [`GAP`] after the newest capture `list` shows, hides quen's
/// status line and presses `key` (`f12`, `f11`). When it pressed it (Unix seconds), and the captures there were then.
fn pressed(
    cx: &Ctx,
    call: &mut Call,
    list: &dyn Fn() -> Vec<(PathBuf, u64)>,
    key: &str,
) -> Result<(i64, Vec<PathBuf>), Done> {
    let before: Vec<PathBuf> = list().into_iter().map(|(p, _)| p).collect();
    let last = before
        .iter()
        .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
        .max();
    if let Some(wait) = last.and_then(|t| (t + GAP).duration_since(SystemTime::now()).ok()) {
        sleep(wait);
    }
    call.lap("check");
    crate::status::hide(cx, call, false).map_err(Done::Refused)?;
    cx.keys(call, &[format!("{key}:300")], false, Focus::Refuse)?;
    Ok((session::now() as i64, before))
}

/// A written capture's sha256 and captures row on the session's boot (`at`: when its key was pressed), its path and
/// size in `fields`. Failed, with `fields`, when the row couldn't be added.
fn recorded(
    cx: &Ctx,
    call: &mut Call,
    w: &Writes,
    path: &Path,
    (bytes, at): (u64, i64),
    fields: &mut Fields,
) -> Result<(), Done> {
    let sha256 = call.artifact(path);
    call.lap("hash");
    fields.insert("capture".into(), json!(path));
    fields.insert("bytes".into(), json!(bytes));
    let boot = cx.live.boot;
    match sha256
        .ok_or(format!("{}: unreadable", path.display()))
        .and_then(|sha256| {
            add_row(
                cx.config,
                row(boot, sha256, bytes, w.ext, w.method),
                path,
                at,
            )
        }) {
        Ok(id) => {
            call.row("captures", id);
            Ok(())
        }
        Err(why) => {
            let why = format!("the capture was written, but no ledger row was added: {why}");
            Err(Done::Failed(why, std::mem::take(fields)))
        }
    }
}

/// game_screenshot method ngfx: one Nsight Graphics capture (F11) as `<tag>.ngfx-capture` in the run's folder, with
/// its captures row. Only in an ngfx-capture session, and never over the capture budget or under 12 GB free without
/// the user's OK (`user_ok`, nsight-capture design answer 8); then nothing is sent and nothing returns until the file
/// holds still. A capture Nsight wrote into its own folder is moved in ([`take`]).
pub(crate) fn ngfx_capture(
    cx: &Ctx,
    call: &mut Call,
    tag: &str,
    user_ok: bool,
) -> Result<Fields, Done> {
    let config = cx.config;
    let name = ngfx_name(cx.live).ok_or_else(|| {
        "method ngfx needs a session launched with profiler ngfx-capture:NAME".to_owned()
    })?;
    let root = config.capture_root();
    let out = root.join(cx.live.boot.to_string()).join(name);
    let dest = out.join(format!("{tag}.{NGFX_EXT}"));
    if dest.exists() {
        let why = format!(
            "{} exists: tag {tag:?} is taken in this boot",
            dest.display()
        );
        return Err(why.into());
    }
    let total = quen_core::disk::total(&root);
    room(
        total,
        free_bytes(&out)?,
        config.capture_budget_gb(),
        user_ok,
    )?;
    let pids = cx.live.job.pids().unwrap_or_default();
    let (window, _) = crate::capture::game_window(&pids)?;
    let dirs = ngfx_dirs(&out);
    let since = cx.live.launched;
    let list = || {
        let found = dirs.iter().flat_map(|d| files(d, NGFX_EXT, since));
        found.collect::<Vec<_>>()
    };
    let (at, before) = pressed(cx, call, &list, NGFX_KEY)?;
    // As RenderDoc's: nothing sent until the file holds still. A game that closed on the capture answers nothing.
    let polled = || {
        call.progress("Nsight is writing the capture");
        list()
    };
    let answers = || responsive(window) || cx.live.job.pids().is_ok_and(|p| p.is_empty());
    let written = wait_file(&before, &polled, &answers, &PACE, &NGFX);
    let (file, bytes) = written.map_err(|why| Done::Failed(why, Map::new()))?;
    call.lap("capture");
    let mut fields = Map::new();
    let ours = file.parent() == Some(out.as_path());
    let path = match take(&file, &dest) {
        Ok(()) => dest,
        // In the run's folder already: kept under Nsight's name, as RenderDoc's.
        Err(_) if ours => file.clone(),
        Err(why) => {
            fields.insert("capture".into(), json!(file));
            let why = format!("{why}: session_end records it");
            return Err(Done::Failed(why, fields));
        }
    };
    if !ours {
        fields.insert("moved_from".into(), json!(file));
    }
    recorded(cx, call, &NGFX, &path, (bytes, at), &mut fields)?;
    let total = quen_core::disk::total(&root);
    fields.insert("capture_root_gb".into(), json!(quen_core::disk::gb(total)));
    Ok(fields)
}

/// Whether an Nsight capture may be taken (nsight-capture design answer 8): not while the capture root holds more than
/// `budget_gb` or its volume has under [`FREE_MIN`] free, unless the user agreed (`user_ok`).
fn room(total: u64, free: u64, budget_gb: f64, user_ok: bool) -> Result<(), String> {
    let over = total as f64 > budget_gb * 1e9;
    if user_ok || (!over && free >= FREE_MIN) {
        return Ok(());
    }
    Err(format!(
        "the capture root holds {} GB (capture_budget_gb {budget_gb}) and {} GB are free: an Nsight capture is refused \
         over the budget or under {} GB free unless the user agrees. Tell them a capture can take several GB, then \
         pass user_ok; capture_usage and capture_clean free space",
        quen_core::disk::gb(total),
        quen_core::disk::gb(free),
        FREE_MIN / 1_000_000_000
    ))
}

/// Where an ngfx-capture launch's captures can appear: its run's folder `out`, and Nsight's own captures folder (the
/// fallback writes there; whether ngfx-capture.exe honours `--output-dir` is unproven).
fn ngfx_dirs(out: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![out.to_owned()];
    dirs.extend(crate::win::documents().map(|d| d.join(NSIGHT_CAPTURES)));
    dirs
}

/// Moves a written capture to `dest`, where no file is: a rename on one volume; across volumes a copy whose sha256
/// must match, then the original to the Recycle Bin.
fn take(file: &Path, dest: &Path) -> Result<(), String> {
    let failed =
        |e: std::io::Error| format!("moving {} to {}: {e}", file.display(), dest.display());
    match crate::win::rename_no_replace(file, dest) {
        Ok(()) => return Ok(()),
        Err(e) if e.raw_os_error() != Some(ERROR_NOT_SAME_DEVICE as i32) => return Err(failed(e)),
        Err(_) => {}
    }
    fs::copy(file, dest).map_err(failed)?;
    let sha = |p: &Path| sha256_file(p).map_err(|e| format!("{}: {e}", p.display()));
    if sha(file)? != sha(dest)? {
        let _ = crate::win::recycle(dest); // quen's own copy
        return Err(format!(
            "the copy of {} differs from it: the original is kept",
            file.display()
        ));
    }
    crate::win::recycle(file).map_err(|e| format!("{} was copied, but stays: {e}", file.display()))
}

/// An ngfx-capture launch's end, before the kill (which could cut a capture short): each `.ngfx-capture` of the launch
/// without a captures row, once it holds still, gets its row on the boot (moved into the run's folder when Nsight wrote
/// it into its own). The field lists them, and what failed. None for any other launch.
pub(crate) fn ngfx_end(config: &Config, live: &Live, call: &mut Call) -> Option<Value> {
    let name = ngfx_name(live)?;
    let out = config.capture_root().join(live.boot.to_string()).join(name);
    let dirs = ngfx_dirs(&out);
    Some(sweep(
        config,
        live.boot,
        &out,
        &dirs,
        live.launched,
        &PACE,
        call,
    ))
}

/// [`ngfx_end`]'s sweep of `dirs` for captures written since `since`, `out` the run's folder.
fn sweep(
    config: &Config,
    boot: i64,
    out: &Path,
    dirs: &[PathBuf],
    since: SystemTime,
    pace: &Pace,
    call: &mut Call,
) -> Value {
    let root = config.capture_root();
    let ledger = Ledger::open(config.state_dir().join("quen.db"));
    let known = |p: &Path| {
        let rel = p.strip_prefix(&root).map(|r| r.display().to_string());
        let found = |rel: String| ledger.as_ref().ok()?.capture_ref(None, Some(&rel)).ok()?;
        rel.ok().and_then(found).is_some()
    };
    let found: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| files(d, NGFX_EXT, since))
        .map(|(p, _)| p)
        .filter(|p| !known(p))
        .collect();
    let t = Instant::now();
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for file in found {
        let size = || fs::metadata(&file).ok().map(|m| m.len());
        let one = settled(&size, pace, t, &file, "the end began").and_then(|bytes| {
            let path = match file.parent() == Some(out) {
                true => file.clone(),
                false => {
                    let dest = out.join(file.file_name().unwrap_or_default());
                    take(&file, &dest)?;
                    dest
                }
            };
            let sha256 = call
                .artifact(&path)
                .ok_or(format!("{}: unreadable", path.display()))?;
            let row = row(boot, sha256, bytes, NGFX.ext, NGFX.method);
            let id = add_row(config, row, &path, session::now() as i64)?;
            call.row("captures", id);
            Ok(json!({"capture": path, "bytes": bytes, "id": id, "moved_from": (path != file).then_some(&file)}))
        });
        match one {
            Ok(v) => done.push(v),
            Err(why) => failed.push(json!({"file": file, "why": why})),
        }
    }
    json!({"recorded": done, "failed": failed})
}

/// The dev interposer is still the game's and its switch file is there: no capture without the swap.
fn swapped_in(config: &Config, bin: &Path) -> Result<(), String> {
    let sha = |p: &Path| fs::read(p).map(|b| sha256_hex(&b)).ok();
    let dev = config.streamline_dev_interposer.as_deref().and_then(sha);
    if dev.is_none() || sha(&bin.join(INTERPOSER)) != dev || !bin.join(INTERPOSER_JSON).exists() {
        return Err(format!(
            "the Streamline swap isn't in place ({INTERPOSER} is not streamline_dev_interposer's build, or \
             {INTERPOSER_JSON} is missing): no capture without it"
        ));
    }
    Ok(())
}

/// The `.<ext>` files in `dir` written since `since`, and their sizes.
fn files(dir: &Path, ext: &str, since: SystemTime) -> Vec<(PathBuf, u64)> {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)))
        .filter_map(|p| {
            let meta = fs::metadata(&p).ok()?;
            (meta.modified().ok()? >= since).then_some((p, meta.len()))
        })
        .collect()
}

/// How long each stage of a capture may take.
struct Pace {
    /// From the key to the new file.
    appear: Duration,
    /// Its size unchanged this long means written.
    stable: Duration,
    /// The longest a capture may take to write.
    growing: Duration,
    /// The window must answer again within this.
    hung: Duration,
    /// Then this, before the next key.
    settle: Duration,
}

/// rdc_boot.py's `wait_rdc`: the first file `list` shows that isn't in `before`, once its size held for
/// `pace.stable` and `responsive` (the game's window answers again: a capture blocks it while written), then
/// `pace.settle`. Err once a stage runs out of time; the caller sends no key then either.
fn wait_file(
    before: &[PathBuf],
    list: &dyn Fn() -> Vec<(PathBuf, u64)>,
    responsive: &dyn Fn() -> bool,
    pace: &Pace,
    w: &Writes,
) -> Result<(PathBuf, u64), String> {
    let t = Instant::now();
    let new = || list().into_iter().find(|(p, _)| !before.contains(p));
    let file = loop {
        if let Some((file, _)) = new() {
            break file;
        }
        if t.elapsed() > pace.appear {
            return Err(format!(
                "no new .{} {} s after {} ({}): nothing more was sent",
                w.ext,
                pace.appear.as_secs(),
                w.key,
                w.hint
            ));
        }
        sleep(POLL.min(pace.stable));
    };
    let size = || list().into_iter().find(|(p, _)| *p == file).map(|(_, n)| n);
    let bytes =
        settled(&size, pace, t, &file, w.key).map_err(|e| format!("{e}: nothing more was sent"))?;
    let hung = Instant::now();
    while !responsive() {
        if hung.elapsed() > pace.hung {
            return Err(format!(
                "the game's window doesn't answer {} s after the capture was written: nothing more was sent",
                pace.hung.as_secs()
            ));
        }
        sleep(POLL.min(pace.hung));
    }
    sleep(pace.settle);
    Ok((file, bytes))
}

/// `size` once it held above 0 for `pace.stable`; Err once `pace.growing` passed since `t` (`after`: what began then).
fn settled(
    size: &dyn Fn() -> Option<u64>,
    pace: &Pace,
    t: Instant,
    file: &Path,
    after: &str,
) -> Result<u64, String> {
    let mut last = None;
    loop {
        let now = size();
        if now.is_some_and(|n| n > 0) && now == last {
            return Ok(now.unwrap_or_default());
        }
        if t.elapsed() > pace.growing {
            return Err(format!(
                "{} still grows {} s after {after}",
                file.display(),
                pace.growing.as_secs()
            ));
        }
        last = now;
        sleep(pace.stable);
    }
}

/// Whether the window answers a message that does nothing within 2 s: a capture blocks the game while written.
fn responsive(window: HWND) -> bool {
    // SAFETY: a window handle (a closed one only fails the call); WM_NULL does nothing.
    let answered = unsafe {
        SendMessageTimeoutW(
            window,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        )
    };
    answered.0 != 0
}

/// The bytes free to this account on `dir`'s volume.
fn free_bytes(dir: &Path) -> Result<u64, String> {
    let name: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path that outlives the call; the other outputs are optional.
    let ok = unsafe { GetDiskFreeSpaceExW(name.as_ptr(), &mut free, null_mut(), null_mut()) };
    if ok == 0 {
        return Err(format!(
            "free space on {}: {}",
            dir.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(free)
}

/// ngfx_args, the config's arguments for every ngfx launch, may hold anything but `--platform` (quen_setup checks it
/// before writing it, and a launch before starting ngfx).
pub(crate) fn ngfx_args(args: &str) -> Result<(), String> {
    if args.contains("--platform") {
        return Err(
            "ngfx_args holds --platform: Qt takes it as its own option and ngfx fails to start (\"no Qt platform \
             plugin\"). Leave it out: Windows is the default"
                .into(),
        );
    }
    Ok(())
}

/// Arguments in one string: split at whitespace, but not inside double quotes, which are dropped
/// (`--architecture "A B"` is two arguments).
pub(crate) fn words(s: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted) = (Vec::new(), None::<String>, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_default();
            }
            c if c.is_whitespace() && !quoted => out.extend(word.take()),
            c => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    out
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::env;
    use std::io::Write;

    use quen_core::journal::{Fs, Journal, MemFs, replay};
    use quen_core::ledger::{BootStart, Game};

    use super::*;
    use crate::mcp::Conn;
    use crate::session::{Session, Shared};

    /// A synthetic game bin folder and tool exes for one test, gone afterwards (only what the test made in it).
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = env::temp_dir().join(format!("quen-profile-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
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

    const SIGNED: &str = "signed by NVIDIA";
    const DEV: &str = "dev build with the GetProcAddress patch";

    /// The Authenticode check, faked: only the bytes [`SIGNED`] carry a valid signature.
    fn verify(path: &Path) -> Result<(), String> {
        match fs::read(path) {
            Ok(b) if b == SIGNED.as_bytes() => Ok(()),
            _ => Err("TRUST_E_NOSIGNATURE".into()),
        }
    }

    /// A game with NVIDIA's interposer and ReShade, the tools, and a config naming them and the dev build.
    fn game(t: &Temp) -> (Config, PathBuf, String) {
        t.put(r"bin\sl.interposer.dll", SIGNED);
        t.put(r"bin\dxgi.dll", "reshade");
        let config = Config {
            renderdoc: Some(t.put(r"rd\renderdoccmd.exe", "")),
            nsys: Some(t.put(r"nsys\nsys.exe", "")),
            ngfx: Some(t.put(r"ngfx\host\ngfx.exe", "")),
            ngfx_args: Some(r#"--architecture "X Y" --metric-set-id 0"#.into()),
            streamline_dev_interposer: Some(t.put(r"sl\sl.interposer.dll", DEV)),
            ..Config::default()
        };
        (config, t.0.join("bin"), sha256_hex(SIGNED.as_bytes()))
    }

    fn parse(config: &Config, arg: &str, bin: &Path, signed: &str) -> Result<Profile, String> {
        let tracy = |_: &Path| Ok(TRACY_CLIENT.to_owned());
        let package = Some("quen-w3 0.1.3");
        Profile::parse(
            config,
            arg,
            bin,
            Path::new(r"Q:\backup"),
            &[signed],
            &verify,
            package,
            &tracy,
        )
    }

    #[test]
    fn renderdoc_swaps_only_nvidias_signed_interposer() {
        let t = Temp::new("swap");
        let (mut config, bin, signed) = game(&t);
        let p = parse(&config, "renderdoc:frame_1", &bin, &signed).unwrap();
        assert_eq!(
            (p.settings, &p.park[..]),
            (RENDERDOC_SETTINGS, &[RESHADE.to_owned()][..])
        );
        assert_eq!(
            p.conditions(),
            ["renderdoc", "streamline_dev_interposer", "reshade_parked"]
        );
        let [
            Change::Rewrite {
                path,
                sha,
                bytes,
                backup,
                ..
            },
            Change::Place {
                dest, bytes: json, ..
            },
        ] = &p.changes[..]
        else {
            panic!("{:?}", p.changes);
        };
        assert_eq!(
            (path, sha, &bytes[..]),
            (&bin.join(INTERPOSER), &signed, DEV.as_bytes())
        );
        assert_eq!(backup, Path::new(r"Q:\backup\sl.interposer.dll"));
        assert_eq!(
            (dest, &json[..]),
            (&bin.join(INTERPOSER_JSON), JSON.as_bytes())
        );
        // Refused, nothing changed: unpinned bytes, a signature that doesn't verify, a switch file there, no dev build
        // or the signed one as the dev build.
        let refused = |config: &Config, signed: &str, why: &str| {
            let err = parse(config, "renderdoc:x", &bin, signed).err().unwrap();
            assert!(err.contains(why), "{err}");
        };
        refused(&config, "another pin", "not NVIDIA's signed original");
        t.put(r"bin\sl.interposer.dll", "unsigned copy");
        refused(
            &config,
            &sha256_hex(b"unsigned copy"),
            "TRUST_E_NOSIGNATURE",
        );
        t.put(r"bin\sl.interposer.dll", SIGNED);
        t.put(r"bin\sl.interposer.json", "{}");
        refused(&config, &signed, "places it only where none is");
        fs::remove_file(bin.join(INTERPOSER_JSON)).unwrap(); // this test's own file
        config.streamline_dev_interposer = Some(t.0.join("missing.dll"));
        refused(&config, &signed, "needs streamline_dev_interposer");
        config.streamline_dev_interposer = Some(bin.join(INTERPOSER));
        refused(&config, &signed, "signed production build");
        config.renderdoc = None;
        refused(&config, &signed, "renderdoc in the local config");
    }

    /// The swap is journaled like the boot's own steps: the replay puts NVIDIA's bytes and write time back and recycles
    /// the switch file, and a journal that swapped is known by its end.
    #[test]
    fn the_swap_replays_back_to_the_signed_interposer() {
        let t = Temp::new("replay");
        let (config, bin, signed) = game(&t);
        let p = parse(&config, "renderdoc:x", &bin, &signed).unwrap();
        let fs = MemFs::default();
        fs.put(bin.join(INTERPOSER), SIGNED);
        let before = fs.files();
        let written = fs.mtime(&bin.join(INTERPOSER)).unwrap();
        let path = Path::new("QUEN/journal.jsonl");
        let mut journal = Journal::create(&fs, path).unwrap();
        quen_core::boot::apply(&fs, &mut journal, &p.changes, &|| false).unwrap();
        assert_eq!(fs.read(&bin.join(INTERPOSER)).unwrap(), DEV.as_bytes());
        assert!(fs.exists(&bin.join(INTERPOSER_JSON)));
        let text = fs.read(path).unwrap();
        assert!(swapped(&text) && !swapped(b"{\"op\":\"patch\",\"path\":\"dx12user.settings\"}"));
        let report = replay(&text, &fs);
        assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
        let after: Vec<_> = fs
            .files()
            .into_iter()
            .filter(|(p, _)| !p.starts_with("QUEN") && !p.starts_with(r"Q:\backup"))
            .collect();
        assert_eq!(after, before.into_iter().collect::<Vec<_>>());
        // Its write time too (feature tests 2: it came back dated the session's end).
        assert_eq!(fs.mtime(&bin.join(INTERPOSER)).unwrap(), written);
        assert!(fs.recycled().contains(&bin.join(INTERPOSER_JSON)));
    }

    #[test]
    fn the_restore_is_checked_by_sha256_authenticode_and_the_switch_file() {
        let t = Temp::new("restored");
        let (_, bin, signed) = game(&t);
        let check = restored(&bin, &[&signed], &verify).unwrap();
        assert_eq!(
            (
                &check["sha256_ok"],
                &check["authenticode"],
                &check["json_left"]
            ),
            (&json!(true), &json!("valid"), &json!(false))
        );
        t.put(r"bin\sl.interposer.json", "{}");
        assert!(
            restored(&bin, &[&signed], &verify)
                .unwrap_err()
                .contains("\"json_left\":true")
        );
        fs::remove_file(bin.join(INTERPOSER_JSON)).unwrap(); // this test's own file
        t.put(r"bin\sl.interposer.dll", DEV);
        let err = restored(&bin, &[&signed], &verify).unwrap_err();
        assert!(
            err.contains("\"sha256_ok\":false") && err.contains("TRUST_E_NOSIGNATURE"),
            "{err}"
        );
    }

    #[test]
    fn each_tool_wraps_the_exe_and_writes_into_the_boots_folder() {
        let t = Temp::new("command");
        let (config, bin, signed) = game(&t);
        let out = Path::new(r"C:\cap\7");
        let exe = || r"C:\g\witcher3.exe".to_owned();
        let p = parse(&config, "renderdoc:f", &bin, &signed).unwrap();
        let (argv, cwd) = p.command(&config, exe(), &bin, out, None);
        assert_eq!(
            argv[1..],
            [
                "capture",
                "-d",
                &bin.display().to_string(),
                "-c",
                r"C:\cap\7\f",
                &exe()
            ]
        );
        assert_eq!((cwd, p.output_dir(out)), (bin.clone(), out.to_owned()));
        let p = parse(&config, "nsys:run_1", &bin, &signed).unwrap();
        assert_eq!(
            (&p.park[..], p.changes.len(), p.settings),
            (&[RESHADE.to_owned()][..], 0, "")
        );
        let (argv, _) = p.command(&config, exe(), &bin, out, None);
        assert!(argv.contains(&"--capture-range-end=stop".into()));
        assert_eq!(
            argv[argv.len() - 2..],
            [r"C:\cap\7\run_1".to_owned(), exe()]
        );
        // ngfx: ReShade parked, its own folder, an output folder of the run's own (made before the launch), never
        // --platform.
        let p = parse(&config, "ngfx:t", &bin, &signed).unwrap();
        assert_eq!(
            (&p.park[..], p.conditions()),
            (
                &[RESHADE.to_owned()][..],
                vec!["ngfx", "reshade_parked", "clocks_locked_to_base"]
            )
        );
        let (argv, cwd) = p.command(&config, exe(), &bin, out, Some("5"));
        assert_eq!(
            (cwd, p.output_dir(out)),
            (t.0.join(r"ngfx\host"), out.join("t"))
        );
        let line = argv.join(" ");
        assert!(
            line.contains(r"--output-dir C:\cap\7\t --start-after-hotkey --limit-to-frames 5")
                && line.ends_with("--auto-export --architecture X Y --metric-set-id 0")
                && !line.contains("--platform"),
            "{line}"
        );
        let mut qt = config.clone();
        qt.ngfx_args = Some(r#"--platform "Windows (x86_64)""#.into());
        assert!(
            parse(&qt, "ngfx:t", &bin, &signed)
                .err()
                .unwrap()
                .contains("Qt")
        );
        assert_eq!(
            words(r#" --arch "A B"  -x "" "#),
            ["--arch", "A B", "-x", ""]
        );
        for bad in ["renderdoc", "perf:x", "nsys:", "nsys:a b", "tracy:../x"] {
            assert!(
                parse(&config, bad, &bin, &signed)
                    .err()
                    .unwrap()
                    .contains("is not"),
                "{bad}"
            );
        }
    }

    /// ngfx-capture: without ngfx-capture.exe beside ngfx, ngfx.exe's Graphics Capture as the RTX Hair session
    /// launched it; with it (or ngfx_capture in the config), ngfx-capture.exe into the run's folder with no prompt, F11,
    /// one frame and no HUD; ngfx.exe as ngfx_capture picks the fallback. Both park ReShade and leave ray tracing, path
    /// tracing and DLSS RR alone, never pass ngfx_args (GPU Trace's); a missing exe and --platform in ngfx_args are
    /// refused.
    #[test]
    fn ngfx_capture_routes_park_reshade_and_keep_ray_tracing() {
        let t = Temp::new("ngfx-capture");
        let (mut config, bin, signed) = game(&t);
        let (exe, dir) = (r"C:\g\witcher3.exe".to_owned(), bin.display().to_string());
        let out = Path::new(r"C:\cap\7");
        let p = parse(&config, "ngfx-capture:hair", &bin, &signed).unwrap();
        assert_eq!(
            (&p.park[..], p.settings, p.changes.len(), p.conditions()),
            (
                &[RESHADE.to_owned()][..],
                "",
                0,
                vec!["ngfx_capture", "reshade_parked"]
            )
        );
        let (argv, cwd) = p.command(&config, exe.clone(), &bin, out, None);
        assert_eq!(
            argv[1..],
            [
                "--activity",
                "Graphics Capture",
                "--exe",
                &exe,
                "--dir",
                &dir,
                "--output-dir",
                r"C:\cap\7\hair",
                "--no-timeout",
                "--hotkey-capture",
                "--frame-count",
                "1"
            ]
        );
        assert_eq!(
            (cwd, p.output_dir(out), &p.record()["launcher"]),
            (t.0.join(r"ngfx\host"), out.join("hair"), &json!("ngfx.exe"))
        );
        let beside = t.put(r"ngfx\host\ngfx-capture.exe", "");
        let p = parse(&config, "ngfx-capture:hair", &bin, &signed).unwrap();
        let (argv, cwd) = p.command(&config, exe.clone(), &bin, out, None);
        assert_eq!(argv[0], beside.display().to_string());
        assert_eq!(
            argv[1..],
            [
                "--exe",
                &exe,
                "--working-dir",
                &dir,
                "--output-dir",
                r"C:\cap\7\hair",
                "--no-block-on-interfering-application",
                "--disable-hang-detection",
                "--hvvm-demote",
                "--frame-count=1",
                "--no-hud"
            ]
        );
        assert_eq!(
            (cwd, p.conditions(), &p.record()["launcher"]),
            (
                t.0.join(r"ngfx\host"),
                vec!["ngfx_capture", "reshade_parked"],
                &json!("ngfx-capture.exe")
            )
        );
        let elsewhere = t.put(r"other\ngfx-capture.exe", "");
        config.ngfx_capture = Some(elsewhere.clone());
        let p = parse(&config, "ngfx-capture:x", &bin, &signed).unwrap();
        assert_eq!(
            p.command(&config, exe.clone(), &bin, out, None).0[0],
            elsewhere.display().to_string()
        );
        config.ngfx_capture = config.ngfx.clone();
        let p = parse(&config, "ngfx-capture:x", &bin, &signed).unwrap();
        assert_eq!(
            p.command(&config, exe.clone(), &bin, out, None).0[1],
            "--activity"
        );
        let refused = |config: &Config, why: &str| {
            let err = parse(config, "ngfx-capture:x", &bin, &signed)
                .err()
                .unwrap();
            assert!(err.contains(why), "{why}: {err}");
        };
        let mut qt = config.clone();
        qt.ngfx_args = Some("--platform \"Windows (x86_64)\"".into());
        refused(&qt, "Qt");
        qt.ngfx_capture = Some(beside);
        refused(&qt, "Qt");
        config.ngfx_capture = Some(t.0.join("missing.exe"));
        refused(&config, "ngfx_capture (or ngfx) in the local config");
        config.ngfx_capture = None;
        config.ngfx = None;
        refused(&config, "ngfx_capture (or ngfx) in the local config");
    }

    /// Nsight's settings file as it records the first-start answer for `version`, among other keys (synthetic).
    fn nsight_ini(version: &str) -> String {
        format!(
            "[CorePlugin.Environment]\nShowVersionUpdateNotifications=true\n\n[CorePlugin.Privacy]\n\
             AllowAnalytics=false\nAskedFor_NVIDIA%20Nsight%20Graphics_{version}=true\n"
        )
    }

    /// The plan's last check under ngfx-capture: Nsight asked its first-start question for the installed version (its
    /// settings file, read only). Another version's answer, none, no settings file or no version resource refuse;
    /// other profiles don't read it.
    #[test]
    fn ngfx_capture_needs_nsights_own_first_start_answer() {
        let t = Temp::new("ngfx-asked");
        let (config, bin, signed) = game(&t);
        let p = parse(&config, "ngfx-capture:x", &bin, &signed).unwrap();
        let v = "2026.3.1.0";
        let fake = |version: Option<&str>, ini: Option<String>| {
            let said = (version.map(String::from), ini);
            move |_: &Path| said.clone()
        };
        assert_eq!(p.asked(&fake(Some(v), Some(nsight_ini(v)))), Ok(()));
        let other = "[CorePlugin.Privacy]\nAllowAnalytics=false\n".to_owned();
        for (version, ini, why) in [
            (
                Some(v),
                Some(nsight_ini("2026.3.0.0")),
                "2026.3.1.0 hasn't asked",
            ),
            (Some(v), Some(other), "hasn't asked"),
            (Some(v), None, "hasn't asked"),
            (None, Some(nsight_ini(v)), "no version resource"),
        ] {
            let err = p.asked(&fake(version, ini)).unwrap_err();
            assert!(err.contains(why), "{why}: {err}");
        }
        let ngfx = parse(&config, "ngfx:t", &bin, &signed).unwrap();
        let unread = |_: &Path| -> (Option<String>, Option<String>) { panic!("read") };
        assert_eq!(ngfx.asked(&unread), Ok(()));
    }

    /// Design answer 8: an Nsight capture is refused over the capture budget or under 12 GB free, unless the user
    /// agreed.
    #[test]
    fn an_nsight_capture_needs_room_or_the_users_ok() {
        let gb = 1_000_000_000;
        assert_eq!(room(19 * gb, 50 * gb, 20.0, false), Ok(()));
        for (total, free) in [(21 * gb, 50 * gb), (5 * gb, 11 * gb)] {
            let err = room(total, free, 20.0, false).unwrap_err();
            assert!(
                err.contains("unless the user agrees") && err.contains("user_ok"),
                "{err}"
            );
            assert_eq!(room(total, free, 20.0, true), Ok(()));
        }
    }

    /// A data root whose ledger holds boot 1, and a capture root, in `t`.
    fn ledger_config(t: &Temp) -> Config {
        let config = Config {
            data_root: t.0.join("data"),
            capture_root: Some(t.0.join("cap")),
            ..Config::default()
        };
        fs::create_dir_all(config.state_dir()).unwrap();
        let mut ledger = Ledger::open(config.state_dir().join("quen.db")).unwrap();
        let game = Game {
            name: "synthetic".into(),
            exe_path: "game.exe".into(),
            exe_sha256: "0".repeat(64),
            build_stamp: None,
        };
        ledger.add_game(&game).unwrap();
        let boot = BootStart {
            game_id: 1,
            t_start: "2026-10-24T10:00:00Z".into(),
            ..BootStart::default()
        };
        assert_eq!(ledger.start_boot(&boot).unwrap(), 1);
        config
    }

    /// Writes `chunks` KB into `path` with `gap` before each, as a capture tool writes its file.
    fn grow(path: PathBuf, chunks: usize, gap: Duration) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut file = fs::File::create(&path).unwrap();
            for _ in 0..chunks {
                sleep(gap);
                file.write_all(&[7; 1024]).unwrap();
                file.flush().unwrap();
            }
        })
    }

    /// Captures rows as `path, kind, method, boot`.
    fn rows(config: &Config) -> Vec<Vec<Value>> {
        let ledger = Ledger::open(config.state_dir().join("quen.db")).unwrap();
        let sql = "SELECT path, kind, method, boot_id, bytes FROM captures ORDER BY id";
        ledger.query(sql, &[], Duration::from_secs(5)).unwrap().rows
    }

    const QUICK: Pace = Pace {
        appear: Duration::from_secs(5),
        stable: Duration::from_millis(300),
        growing: Duration::from_secs(10),
        hung: Duration::from_secs(1),
        settle: Duration::ZERO,
    };

    /// A real file that grows: the wait takes it once it holds still, written since the launch, in Nsight's own folder
    /// or the run's (an older capture in Nsight's folder is never taken); it is moved to the tag (Nsight's folder keeps
    /// nothing) and recorded on the boot, kind ngfx-capture, method ngfx_capture.
    #[test]
    fn a_growing_capture_is_waited_for_moved_in_and_recorded() {
        let t = Temp::new("ngfx-wait");
        let config = ledger_config(&t);
        let (out, nsight) = (config.capture_root().join(r"1\hair"), t.0.join("nsight"));
        fs::create_dir_all(&out).unwrap();
        fs::create_dir_all(&nsight).unwrap();
        t.put(
            r"nsight\older.ngfx-capture",
            "a capture from before the launch",
        );
        sleep(Duration::from_millis(50));
        let since = SystemTime::now();
        let dirs = [out.clone(), nsight.clone()];
        let list = || {
            let found = dirs.iter().flat_map(|d| files(d, NGFX_EXT, since));
            found.collect::<Vec<_>>()
        };
        assert!(list().is_empty(), "the older capture isn't the launch's");
        let writing = grow(
            nsight.join("witcher3_frame1.ngfx-capture"),
            5,
            Duration::from_millis(20),
        );
        let (file, bytes) = wait_file(&[], &list, &|| true, &QUICK, &NGFX).unwrap();
        writing.join().unwrap();
        assert_eq!(
            (&file, bytes),
            (&nsight.join("witcher3_frame1.ngfx-capture"), 5 * 1024)
        );
        let dest = out.join("A.ngfx-capture");
        take(&file, &dest).unwrap();
        assert!(!file.exists() && fs::metadata(&dest).unwrap().len() == bytes);
        assert!(nsight.join("older.ngfx-capture").exists());
        let sha256 = sha256_file(&dest).unwrap();
        add_row(
            &config,
            row(1, sha256, bytes, NGFX.ext, NGFX.method),
            &dest,
            1_792_000_000,
        )
        .unwrap();
        // Written into the run's folder (ngfx-capture.exe's --output-dir): renamed to its tag there.
        let before: Vec<PathBuf> = list().into_iter().map(|(p, _)| p).collect();
        let writing = grow(
            out.join("witcher3_frame2.ngfx-capture"),
            3,
            Duration::from_millis(20),
        );
        let (file, bytes) = wait_file(&before, &list, &|| true, &QUICK, &NGFX).unwrap();
        writing.join().unwrap();
        assert_eq!((file.parent(), bytes), (Some(out.as_path()), 3 * 1024));
        take(&file, &out.join("B.ngfx-capture")).unwrap();
        assert_eq!(
            rows(&config),
            [vec![
                json!("1/hair/A.ngfx-capture"),
                json!("ngfx-capture"),
                json!("ngfx_capture"),
                json!(1),
                json!(5 * 1024)
            ]]
        );
        // Nothing new: no file, and no key after it.
        let quick = Pace {
            appear: Duration::from_millis(100),
            ..QUICK
        };
        let before: Vec<PathBuf> = list().into_iter().map(|(p, _)| p).collect();
        let err = wait_file(&before, &list, &|| true, &quick, &NGFX).unwrap_err();
        assert!(
            err.contains("no new .ngfx-capture") && err.contains("after F11"),
            "{err}"
        );
    }

    /// session_end's sweep under ngfx-capture: a capture in the run's folder without a row is recorded where it is, one
    /// Nsight wrote into its own folder since the launch is moved in and recorded; one already recorded, and one in
    /// Nsight's folder from before the launch, are left as they are.
    #[test]
    fn the_end_records_the_launchs_captures_not_yet_recorded() {
        let t = Temp::new("ngfx-sweep");
        let config = ledger_config(&t);
        let (out, nsight) = (config.capture_root().join(r"1\hair"), t.0.join("nsight"));
        t.put(r"nsight\older.ngfx-capture", "before the launch");
        sleep(Duration::from_millis(50));
        let since = SystemTime::now();
        // NTFS stamps a write with a coarser clock than SystemTime::now: one right after `since` can read as before it.
        sleep(Duration::from_millis(50));
        let recorded = t.put(r"cap\1\hair\A.ngfx-capture", "recorded by game_screenshot");
        let sha256 = sha256_file(&recorded).unwrap();
        add_row(
            &config,
            row(1, sha256, 27, NGFX.ext, NGFX.method),
            &recorded,
            1_792_000_000,
        )
        .unwrap();
        t.put(r"cap\1\hair\witcher3_frame2.ngfx-capture", "no row yet");
        t.put(
            r"nsight\witcher3_frame3.ngfx-capture",
            "written into Nsight's folder",
        );
        let shared = Shared::new(Ok(config.clone()));
        let session = Session::new(&shared, None);
        let mut call = session.call(Conn::nobody(), Value::Null, None);
        let dirs = [out.clone(), nsight.clone()];
        let field = sweep(&config, 1, &out, &dirs, since, &QUICK, &mut call);
        assert_eq!(field["failed"], json!([]), "{field}");
        let moved: Vec<&Value> = field["recorded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["moved_from"])
            .collect();
        assert_eq!(
            moved,
            [
                &json!(null),
                &json!(nsight.join("witcher3_frame3.ngfx-capture"))
            ]
        );
        let paths: Vec<Value> = rows(&config).into_iter().map(|r| r[0].clone()).collect();
        assert_eq!(
            paths,
            [
                json!("1/hair/A.ngfx-capture"),
                json!("1/hair/witcher3_frame2.ngfx-capture"),
                json!("1/hair/witcher3_frame3.ngfx-capture")
            ]
        );
        assert!(out.join("witcher3_frame3.ngfx-capture").is_file());
        assert!(nsight.join("older.ngfx-capture").is_file());
        assert!(!nsight.join("witcher3_frame3.ngfx-capture").exists());
        // A second end finds nothing left.
        let again = sweep(&config, 1, &out, &dirs, since, &QUICK, &mut call);
        assert_eq!(again, json!({"recorded": [], "failed": []}));
    }

    /// A synthetic hooks.tsv: rows at W3Quen's Tracy sites (two of one mod, one of a mod that isn't installed), rows
    /// that change nothing there or are W3Trace's own, a row elsewhere, comments and the build line.
    const HOOKS_TSV: &str = "# Every witcher3.exe build 26238013 site our mods touch\n\
        site\tkind\tmod\tversion\tnote\n\
        0x1C22050\thook\tW3EngineTweaks\t1.12.4\tRT split consumer\n\
        0x1BF4AE0\thook\tW3EngineTweaks\t1.12.4\tRT split per-batch mesh update\r\n\
        0x353330\thook\tW3LightCache\t1.1\tPT light sort\n\
        0x1BF92E0\tcall\tW3RtWalk\t1.6\tstage join\n\
        0x1C34E50\thook\tW3Trace\t1.2\tTracy zone\n\
        0x1CF3370\thook\tW3ElementMap\t1.1\tregister\n\
        # build: sha256=0000000000000000000000000000000000000000000000000000000000000000 stamp=0x6ADFBD7A\n";

    /// A game with W3EngineTweaks and ReShade installed, a KB with [`HOOKS_TSV`], and Tracy's capture tool.
    fn tracy_game(t: &Temp) -> (Config, PathBuf) {
        t.put(r"bin\W3EngineTweaks.asi", "mod");
        t.put(r"bin\dxgi.dll", "reshade");
        t.put(r"kb\hooks.tsv", HOOKS_TSV);
        let config = Config {
            tracy_capture: Some(t.put(r"tracy\tracy-capture.exe", "")),
            kb: Some(t.0.join("kb")),
            ..Config::default()
        };
        (config, t.0.join("bin"))
    }

    fn tracy(
        config: &Config,
        bin: &Path,
        package: Option<&str>,
        version: Result<&str, &str>,
    ) -> Result<Profile, String> {
        let said = |_: &Path| version.map(str::to_owned).map_err(str::to_owned);
        Profile::parse(
            config,
            "tracy:run_1",
            bin,
            Path::new(r"Q:\backup"),
            &[],
            &verify,
            package,
            &said,
        )
    }

    /// tracy: the plain game (Tracy is in quen's package), its capture tool beside it on the launch's loopback port,
    /// the trigger and the port in the game's environment, ReShade kept, and the installed mods hooks.tsv names at the
    /// functions W3Quen hooks parked once each; a mod that isn't installed is only recorded.
    #[test]
    fn tracy_parks_what_hooks_tsv_names_at_its_sites_and_runs_the_plain_game() {
        let t = Temp::new("tracy");
        let (config, bin) = tracy_game(&t);
        let p = tracy(&config, &bin, Some("quen-w3 0.1.3"), Ok(TRACY_CLIENT)).unwrap();
        assert_eq!(
            (&p.park[..], p.conditions(), p.settings, p.changes.len()),
            (&["W3EngineTweaks.asi".to_owned()][..], vec!["tracy"], "", 0)
        );
        let record = p.record();
        let mods: Vec<(&str, &str)> = record["collisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["mod"].as_str().unwrap(), c["hook"].as_str().unwrap()))
            .collect();
        assert_eq!(
            mods,
            [
                ("W3EngineTweaks", "RT_UpdateRayTracingData"),
                ("W3EngineTweaks", "RT_MeshListSubmit"),
                ("W3LightCache", "PT_LightSort")
            ]
        );
        assert_eq!(
            record["collisions"][1]["note"],
            "RT split per-batch mesh update"
        );
        let port = p.port.to_string();
        assert!(p.port != 0 && record["port"] == p.port);
        let exe = r"C:\g\witcher3.exe".to_owned();
        let out = Path::new(r"C:\cap\7");
        assert_eq!(
            p.command(&config, exe.clone(), &bin, out, None),
            (vec![exe], bin.clone())
        );
        let tool = config.tracy_capture.as_ref().unwrap().display().to_string();
        assert_eq!(
            p.capture(out).unwrap(),
            [
                &tool,
                "-a",
                "127.0.0.1",
                "-p",
                &port,
                "-o",
                r"C:\cap\7\run_1.tracy"
            ]
        );
        assert_eq!(
            p.env(),
            [("W3_QUEN_TRACY", "1".to_owned()), ("TRACY_PORT", port)]
        );
        assert_eq!(p.output_dir(out), out);
        // Without W3EngineTweaks installed there is nothing to park; the rows are still recorded.
        fs::remove_file(bin.join("W3EngineTweaks.asi")).unwrap(); // this test's own file
        let p = tracy(&config, &bin, Some("quen-w3 0.1.3"), Ok(TRACY_CLIENT)).unwrap();
        assert!(p.park.is_empty() && p.collisions.len() == 3);
        // The other profiles get no capture tool and no variables.
        let nsys = Config {
            nsys: Some(t.put(r"nsys\nsys.exe", "")),
            ..config
        };
        let n = parse(&nsys, "nsys:x", &bin, "").unwrap();
        assert!(n.capture(out).is_none() && n.env().is_empty());
    }

    #[test]
    fn tracy_refuses_before_anything_changes() {
        let t = Temp::new("tracy-refused");
        let (mut config, bin) = tracy_game(&t);
        let refused = |config: &Config, package, version, why: &str| {
            let err = tracy(config, &bin, package, version).err().unwrap();
            assert!(err.contains(why), "{why}: {err}");
        };
        let ok = Some("quen-w3 0.1.3");
        refused(
            &config,
            ok,
            Ok("0.13.1"),
            "both ends must be the same version",
        );
        refused(
            &config,
            ok,
            Err("printed no `tracy-capture <version>` line"),
            "printed no",
        );
        refused(
            &config,
            Some("quen-w3 0.1.2"),
            Ok(TRACY_CLIENT),
            "needs quen-w3 0.1.3",
        );
        refused(&config, None, Ok(TRACY_CLIENT), "no pinned package");
        t.put(
            r"kb\hooks.tsv",
            "site\tkind\tmod\tversion\tnote\n1C224A0\thook\tX\t1\tno 0x\n",
        );
        refused(&config, ok, Ok(TRACY_CLIENT), "hooks.tsv line 2: site");
        t.put(r"kb\hooks.tsv", "0x1C224A0\thook\n");
        refused(&config, ok, Ok(TRACY_CLIENT), "hooks.tsv line 1: not site");
        config.kb = Some(t.0.join("no-kb"));
        refused(&config, ok, Ok(TRACY_CLIENT), "hooks.tsv");
        config.kb = None;
        refused(
            &config,
            ok,
            Ok(TRACY_CLIENT),
            "needs kb in the local config",
        );
        config.tracy_capture = None;
        refused(
            &config,
            ok,
            Ok(TRACY_CLIENT),
            "tracy_capture in the local config",
        );
        // Only a file named tracy-capture.exe is run.
        let other = tracy_version(&t.put(r"tracy\tracy-profiler.exe", ""));
        assert!(other.unwrap_err().contains("is not tracy-capture.exe"));
    }

    /// The hook list quen reads is the ASI's (w3/asi/hooks.inc), and the client version is the vendored Tracy's.
    #[test]
    fn tracy_sites_and_version_are_the_packages() {
        let sites = tracy_sites();
        assert_eq!(sites.len(), 75);
        assert!(sites.contains(&("RT_UpdateRayTracingData", 0x1C22050)));
        assert!(sites.contains(&("Post_Tonemap", 0x34DA80)));
        let header = include_str!("../../../w3/third_party/tracy/public/common/TracyVersion.hpp");
        let part = |name: &str| {
            let at = header.find(&format!("{name} = ")).unwrap() + name.len() + 3;
            header[at..].split(';').next().unwrap().to_owned()
        };
        assert_eq!(
            TRACY_CLIENT,
            format!("{}.{}.{}", part("Major"), part("Minor"), part("Patch"))
        );
    }

    /// The boot's check of W3Quen.log: nothing until a capture connected, then the hooks; a refusal or no hook at all
    /// fails; a log older than the launch is a past launch's.
    #[test]
    fn tracy_ready_reads_w3quen_log_of_this_launch() {
        let t = Temp::new("tracy-ready");
        let bin = t.0.join("bin");
        let launched = SystemTime::now() - Duration::from_secs(60);
        assert!(tracy_ready(&bin, launched).is_none(), "no log");
        let log = |text: &str| t.put(r"bin\W3Quen.log", text);
        log("1 tracy: 75 of 75 hooks installed\n2 tracy: started (timer 1.0 ticks/us)\n");
        assert!(tracy_ready(&bin, launched).is_none(), "no capture yet");
        log(
            "1 tracy: Lighting_AO                                0x1C14B20  skipped: entry bytes differ (Detours 0)\n\
             2 tracy: 74 of 75 hooks installed\n3 W3Quen: natives queued=1\n4 tracy: a capture connected\n",
        );
        assert_eq!(
            tracy_ready(&bin, launched).unwrap().unwrap(),
            json!({"hooks_installed": "74 of 75", "skipped": ["Lighting_AO"], "capture": "connected",
                   "started_utc": null})
        );
        // A `tracy: started` line whose tick is 1.5 s old, as UTC: 1.5 s ago, give or take the tick's 16 ms steps.
        let ago = |ms| session::utc_ms(SystemTime::now() - Duration::from_millis(ms));
        let before = ago(1_550);
        let at = crate::win::tick() - 1_500;
        log(&format!(
            "{at} tracy: 75 of 75 hooks installed\n{at} tracy: started (timer 1.0 ticks/us)\n{at} tracy: a capture \
             connected\n"
        ));
        let said = tracy_ready(&bin, launched).unwrap().unwrap()["started_utc"].clone();
        let (said, after) = (said.as_str().unwrap().to_owned(), ago(1_450));
        assert!(before <= said && said <= after, "{before} {said} {after}");
        assert!(
            tracy_ready(&bin, SystemTime::now() + Duration::from_secs(60)).is_none(),
            "stale"
        );
        for bad in [
            "1 tracy: REFUSED: the exe's TimeDateStamp is 0x1, not witcher3.exe build 26238013's 0x6ADFBD7A\n",
            "1 tracy: 0 of 75 hooks installed\n",
        ] {
            log(bad);
            let err = tracy_ready(&bin, launched).unwrap().unwrap_err();
            assert!(err.contains("installed no Tracy hook"), "{err}");
        }
    }

    #[test]
    fn the_users_overrides_may_not_name_the_profiles_keys() {
        let t = Temp::new("overrides");
        let (config, bin, signed) = game(&t);
        let p = parse(&config, "renderdoc:x", &bin, &signed).unwrap();
        assert_eq!(p.overridden("MotionBlur=false", "Rendering:VSync=1"), None);
        assert_eq!(
            p.overridden("MotionBlur=false,EnableRT=true", ""),
            Some("EnableRT")
        );
        assert_eq!(p.overridden("", "Rendering:AAMode=2"), Some("AAMode"));
        let nsys = parse(&config, "nsys:x", &bin, &signed).unwrap();
        assert_eq!(nsys.overridden("EnableRT=true", ""), None);
    }

    /// rdc_boot.py's pacing on fakes: the new file once its size holds and the window answers; Err (and no key)
    /// when a stage runs out of time.
    #[test]
    fn a_capture_counts_as_written_once_it_holds_still_and_the_game_answers() {
        let pace = Pace {
            appear: Duration::from_millis(200),
            stable: Duration::from_millis(5),
            growing: Duration::from_millis(500),
            hung: Duration::from_secs(1),
            settle: Duration::ZERO,
        };
        let old = PathBuf::from("old.rdc");
        let new = PathBuf::from("w3_frame9.rdc");
        let polls = Cell::new(0u64);
        // Appears on the third poll, grows for a few, then holds at 30.
        let list = || {
            polls.set(polls.get() + 1);
            let mut files = vec![(old.clone(), 5)];
            if polls.get() >= 3 {
                files.push((new.clone(), (polls.get() * 10).min(30)));
            }
            files
        };
        let answers = Cell::new(0);
        let responsive = || {
            answers.set(answers.get() + 1);
            answers.get() > 2
        };
        let got = wait_file(std::slice::from_ref(&old), &list, &responsive, &pace, &RDC).unwrap();
        assert_eq!(got, (new.clone(), 30));
        assert_eq!(answers.get(), 3, "waited for the window");
        let none = || vec![(old.clone(), 5)];
        assert!(
            wait_file(std::slice::from_ref(&old), &none, &|| true, &pace, &RDC)
                .unwrap_err()
                .contains("no new .rdc")
        );
        let growing = Cell::new(0u64);
        let grows = || {
            growing.set(growing.get() + 1);
            vec![(new.clone(), growing.get())]
        };
        assert!(
            wait_file(&[], &grows, &|| true, &pace, &RDC)
                .unwrap_err()
                .contains("still grows")
        );
        let hung = wait_file(&[], &|| vec![(new.clone(), 9)], &|| false, &pace, &RDC).unwrap_err();
        assert!(hung.contains("doesn't answer"), "{hung}");
    }
}
