//! The untracked local config: `%USERPROFILE%\.quen\config.toml`, or the file `QUEN_CONFIG` names. Every
//! machine-specific path lives there, never in the code. Not AppData: Windows redirects an app package's AppData writes
//! (Claude Desktop is one, and so is every process it starts) into the package's private folder, which other clients
//! don't see (relocate.rs).
//!
//! ```toml
//! game_dir = '%GAME_DIR%'                                        # the folder holding bin\ and Mods\
//! documents = '%USERPROFILE%\Documents\The Witcher 3'            # dx12user.settings and gamesaves
//! game_exe = 'witcher3.exe'                                      # optional: the game's image name, in bin\x64_dx12
//! data_root = '%USERPROFILE%\.quen'                              # optional, the default
//! capture_root = '%USERPROFILE%\.quen\captures'                  # optional, the default: <data_root>\captures
//! capture_budget_gb = '20'                                       # optional, the default: GB of captures before quen
//!                                                                # warns (capture_usage, capture_clean)
//! kb = '%USERPROFILE%\Projects\witcher3-mods\re'                # optional: the RE KB the mem_* tools read
//! redkit = '%REDKIT%'                                            # optional: The Witcher 3 REDkit's folder (script_compile,
//!                                                                # the data tools' where: redkit)
//! presentmon = '%PRESENTMON%'                                    # optional: PresentMon's console exe; unset: on PATH
//! known_builds = 'steam <sha256> <label>; gog <sha256> <label>'   # optional: game builds verified here, beside the
//!                                                                # adapter's own (quen_core::build::KnownBuilds)
//! # Optional features, each off unless set:
//! renodx_addon = 'renodx-thewitcher3remastered.addon64'          # RenoDX in bin: ReShade.ini and ReShadePreset.ini
//!                                                                # patched, preset values, ReShade.log readiness
//! renodx_build = '%RENODX_BUILD%\x.addon64'                      # with it: refused while the installed one differs
//! devkit = 'devkit\renodx-devkit.addon64'                        # the devkit, as ReShade.ini [ADDON] LoadFromDllMain
//! nsys = '%NSIGHT_SYSTEMS%\nsys.exe'                             # profiler launches (profiler: nsys:NAME, ngfx:NAME)
//! ngfx = '%NSIGHT_GRAPHICS%\ngfx.exe'
//! ngfx_args = '--architecture <GPU> --metric-set-id 0'           # added to every ngfx launch
//! renderdoc = '%RENDERDOC%\renderdoccmd.exe'                     # profiler: renderdoc:NAME, with the dev interposer
//! streamline_dev_interposer = '%STREAMLINE%\_artifacts\sl.interposer\Develop_x64\sl.interposer.dll'
//!                                                                # (tools/streamline) swapped in for RenderDoc runs
//! tracy_capture = '%TRACY%\tracy-capture.exe'                    # profiler: tracy:NAME (Tracy's capture tool, the
//!                                                                # version of the client in quen's package: 0.14.1)
//! # This machine's Python harness while it exists: the game_* tools' scripts, and the interlock against its sessions.
//! harness = '%USERPROFILE%\renodx\tmp\witcher3\ini_session.py'
//! python = 'python'                                              # optional
//! ```
//!
//! (Real, absolute paths, not placeholders: nothing is expanded.)

use std::env;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use quen_core::build::KnownBuilds;
use quen_core::journal::sha256_hex;

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Where quen keeps what it produces: `state\` (lock, journal) and `logs\`. Never inside the source tree.
    pub data_root: PathBuf,
    pub python: String,
    /// The game's image name. quen launches none while one runs, and replays no journal while one runs outside its job
    /// (it holds the files a replay restores, and rewrites some at exit).
    pub game_exe: String,
    /// The Python harness: the game_* tools' scripts and, while it is set, the interlock against its sessions.
    pub harness: Option<PathBuf>,
    pub game_dir: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    /// Where captures go, one folder per boot; unset: `<data_root>\captures`.
    pub capture_root: Option<PathBuf>,
    /// GB of captures before quen warns; unset: 20.
    pub capture_budget_gb: Option<f64>,
    /// The RenoDX add-on's file name in bin: turns the RenoDX boot on (ReShade.ini, ReShadePreset.ini, preset values).
    pub renodx_addon: Option<String>,
    /// With `renodx_addon`: the build a launch requires installed (unless `W3_ADDON_ANY`).
    pub renodx_build: Option<PathBuf>,
    /// The devkit add-on as ReShade.ini `[ADDON] LoadFromDllMain` takes it: launches may load the devkit.
    pub devkit: Option<String>,
    /// The profilers' exes, and the arguments every ngfx launch gets (its GPU architecture, a metric set).
    pub nsys: Option<PathBuf>,
    pub ngfx: Option<PathBuf>,
    pub ngfx_args: Option<String>,
    /// renderdoccmd.exe, and the patched dev build of Streamline's interposer that a `renderdoc:NAME` launch swaps in
    /// for the game's signed one (tools/streamline, profile.rs).
    pub renderdoc: Option<PathBuf>,
    pub streamline_dev_interposer: Option<PathBuf>,
    /// Tracy's command-line capture tool (`tracy-capture.exe`), which a `tracy:NAME` launch starts beside the game.
    pub tracy_capture: Option<PathBuf>,
    /// A test switch, not in quen_setup: `force_duplication = 'true'` makes game_screenshot's HDR path skip
    /// Windows.Graphics.Capture, so a live boot exercises its Desktop Duplication fallback.
    pub force_duplication: bool,
    /// The game's RE knowledge base: the per-build tables the `mem_*` tools and game_cvar read.
    pub kb: Option<PathBuf>,
    /// The Witcher 3 REDkit's folder (its `bin\x64_RedKit\wcc_lite.exe`, its depot `r4data`): script_compile and the data
    /// tools' `where: redkit`. The user installs it; quen never does.
    pub redkit: Option<PathBuf>,
    /// PresentMon's console exe (a prerequisite, never bundled); unset: `PresentMon.exe` on PATH.
    pub presentmon: Option<PathBuf>,
    /// The game builds a launch accepts: the adapter's own ([`crate::w3::KNOWN_BUILDS`]), then `known_builds`.
    pub known_builds: KnownBuilds,
}

impl Config {
    pub fn state_dir(&self) -> PathBuf {
        self.data_root.join("state")
    }

    pub fn capture_root(&self) -> PathBuf {
        self.capture_root
            .clone()
            .unwrap_or_else(|| self.data_root.join("captures"))
    }

    /// GB of captures before quen warns.
    pub fn capture_budget_gb(&self) -> f64 {
        self.capture_budget_gb.unwrap_or(20.0)
    }

    /// The game's binaries: the exe, the ASIs, ReShade and its add-ons.
    pub fn bin_dir(&self) -> Option<PathBuf> {
        Some(self.game_dir.as_ref()?.join("bin").join("x64_dx12"))
    }

    /// The user's saves.
    pub fn saves_dir(&self) -> Option<PathBuf> {
        Some(self.documents.as_ref()?.join("gamesaves"))
    }
}

/// quen's own folder, `%USERPROFILE%\.quen`: the default data root, and the config's folder.
/// Unit tests get a folder of their own, so none reads or writes the user's: a live session's journal there once
/// failed setup's `confirmed_values_are_written_keeping_other_keys_and_comments`, whose config names no data_root
/// (2026-10-19).
pub fn home() -> Option<PathBuf> {
    if cfg!(test) {
        let own = format!("quen-test-home-{}", std::process::id());
        return Some(env::temp_dir().join(own));
    }
    env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".quen"))
}

/// The config file, as an absolute path: `QUEN_CONFIG`, else `%USERPROFILE%\.quen\config.toml`.
pub fn path() -> Result<PathBuf, String> {
    let path = match env::var_os("QUEN_CONFIG") {
        Some(path) => PathBuf::from(path),
        None => home().ok_or("USERPROFILE is not set")?.join("config.toml"),
    };
    std::path::absolute(&path).map_err(|e| format!("config {}: {e}", path.display()))
}

/// One `quen serve` per config file. Its pipe is `\\.\pipe\` + this, and the startup mutex `Local\` + this. The
/// daemon takes up an edited config between calls while no session is live (session.rs `Session::reload`), so its
/// connections, old and new, run on the file's config, and a session never sees two.
pub fn daemon_name() -> Result<String, String> {
    let key = path()?.to_string_lossy().to_lowercase();
    Ok(format!("quen-{}", &sha256_hex(key.as_bytes())[..16]))
}

/// Reads the config and checks the data root. A missing file means the defaults.
pub fn load() -> Result<Config, String> {
    let path = path()?;
    loaded(&read(&path)?, &path)
}

/// The config `text` (the file `path`'s) holds, its roots checked.
pub fn loaded(text: &str, path: &Path) -> Result<Config, String> {
    let config = parse(text, path)?;
    check_roots(&config)?;
    Ok(config)
}

/// The config file's text; empty when there is none.
pub fn read(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("config {}: {e}", path.display())),
    }
}

/// The config `text` holds (`path` names it in errors), over the defaults. The roots aren't checked here.
pub fn parse(text: &str, path: &Path) -> Result<Config, String> {
    let mut config = Config {
        data_root: home().unwrap_or_default(),
        python: "python".into(),
        game_exe: "witcher3.exe".into(),
        ..Config::default()
    };
    let mut known_builds = "";
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = || format!("config {}:{}", path.display(), n + 1);
        let (key, value) = parse_line(line).ok_or_else(|| {
            format!(
                "{}: quen reads only `key = 'value'` lines (single quotes for paths)",
                at()
            )
        })?;
        match key {
            "data_root" => config.data_root = value.into(),
            "python" => config.python = value.into(),
            "game_exe" => config.game_exe = value.into(),
            "harness" => config.harness = Some(value.into()),
            "game_dir" => config.game_dir = Some(value.into()),
            "documents" => config.documents = Some(value.into()),
            "capture_root" => config.capture_root = Some(value.into()),
            "capture_budget_gb" => {
                let gb = value
                    .parse()
                    .ok()
                    .filter(|gb: &f64| *gb > 0.0 && gb.is_finite());
                let why = || format!("{}: capture_budget_gb is a number of GB above 0", at());
                config.capture_budget_gb = Some(gb.ok_or_else(why)?);
            }
            "renodx_addon" => config.renodx_addon = Some(value.into()),
            "renodx_build" => config.renodx_build = Some(value.into()),
            "devkit" => config.devkit = Some(value.into()),
            "nsys" => config.nsys = Some(value.into()),
            "ngfx" => config.ngfx = Some(value.into()),
            "ngfx_args" => config.ngfx_args = Some(value.into()),
            "renderdoc" => config.renderdoc = Some(value.into()),
            "streamline_dev_interposer" => config.streamline_dev_interposer = Some(value.into()),
            "tracy_capture" => config.tracy_capture = Some(value.into()),
            "force_duplication" => {
                config.force_duplication = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(format!("{}: force_duplication is 'true' or 'false'", at())),
                }
            }
            "kb" => config.kb = Some(value.into()),
            "redkit" => config.redkit = Some(value.into()),
            "presentmon" => config.presentmon = Some(value.into()),
            "known_builds" => {
                KnownBuilds::parse(value).map_err(|e| format!("{}: {e}", at()))?;
                known_builds = value;
            }
            _ => return Err(format!("{}: unknown key {key:?}", at())),
        }
    }
    // The config's own entries are numbered as the user wrote them above; here only a clash with the adapter's fails.
    config.known_builds =
        KnownBuilds::parse(&format!("{};{known_builds}", crate::w3::KNOWN_BUILDS))
            .map_err(|e| format!("config {}: known_builds: {e}", path.display()))?;
    // A relative path would resolve against wherever quen was started, and quietly miss the real folders.
    let paths = [
        ("harness", &config.harness),
        ("game_dir", &config.game_dir),
        ("documents", &config.documents),
        ("renodx_build", &config.renodx_build),
        ("nsys", &config.nsys),
        ("ngfx", &config.ngfx),
        ("renderdoc", &config.renderdoc),
        (
            "streamline_dev_interposer",
            &config.streamline_dev_interposer,
        ),
        ("tracy_capture", &config.tracy_capture),
        ("kb", &config.kb),
        ("redkit", &config.redkit),
        ("presentmon", &config.presentmon),
    ];
    for (key, value) in paths {
        if let Some(value) = value.as_ref().filter(|p| !p.is_absolute()) {
            return Err(format!(
                "config {}: {key} {:?} is not an absolute path",
                path.display(),
                value.display()
            ));
        }
    }
    Ok(config)
}

/// The `key = 'value'` pairs `text` sets, in order.
pub fn pairs(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|line| parse_line(line.trim()))
        .collect()
}

/// `text` with each of `values` set: every line that sets the key gets the new value (its comment kept), and a key
/// it doesn't set yet is added at the end. Every other line, comments included, stays as it was.
pub fn with_values(text: &str, values: &[(&str, &str)]) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    for (key, value) in values {
        let mut found = false;
        for line in &mut lines {
            let trimmed = line.trim();
            let Some((k, old)) = parse_line(trimmed).filter(|(k, _)| k == key) else {
                continue;
            };
            // What follows the closing quote: spaces and the comment, if any.
            let at = old.as_ptr() as usize - trimmed.as_ptr() as usize + old.len() + 1;
            *line = format!("{k} = '{value}'{}", &trimmed[at..]);
            found = true;
        }
        if !found {
            lines.push(format!("{key} = '{value}'"));
        }
    }
    let mut out = lines.join(eol);
    out.push_str(eol);
    out
}

/// Writes the config file atomically: a temp file beside it, flushed to disk, then renamed over it.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    let at = |e: std::io::Error| format!("config {}: {e}", path.display());
    let dir = path
        .parent()
        .ok_or_else(|| format!("config {}: no folder", path.display()))?;
    fs::create_dir_all(dir).map_err(at)?;
    let tmp = path.with_extension("toml.quen-tmp");
    let mut file = fs::File::create(&tmp).map_err(at)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(at)?;
    drop(file);
    fs::rename(&tmp, path).map_err(at)
}

/// `key = 'value'` (a TOML literal string) or `key = "value"` without escapes, then at most a `# comment`.
/// ponytail: a TOML subset; add the `toml` crate when the config needs more than flat strings.
fn parse_line(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once('=')?;
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let (value, tail) = rest[1..].split_once(quote)?;
    let tail = tail.trim();
    let escaped = quote == '"' && value.contains('\\');
    (!escaped && (tail.is_empty() || tail.starts_with('#'))).then_some((key.trim(), value))
}

/// Neither the data root nor the capture root is inside quen's source tree, its install folder, any git
/// work tree, the game folder, the game's documents folder or Pictures (a personal folder, often cloud-synced).
pub fn check_roots(config: &Config) -> Result<(), String> {
    let install = env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.to_owned()));
    let forbidden = [
        install.map(|dir| (dir, "quen's install folder")),
        config.game_dir.clone().map(|dir| (dir, "the game folder")),
        config
            .documents
            .clone()
            .map(|dir| (dir, "the game's documents folder")),
        crate::win::pictures().map(|dir| (dir, "Pictures (a personal folder)")),
    ];
    for (root, name) in [
        (config.data_root.clone(), "data root"),
        (config.capture_root(), "capture root"),
    ] {
        if !root.is_absolute() {
            return Err(format!(
                "{name} {:?} is not an absolute path (set {}, or USERPROFILE)",
                root.display(),
                name.replace(' ', "_")
            ));
        }
        for (dir, what) in forbidden.iter().flatten() {
            if inside(&root, dir) {
                return Err(format!("{name} {} is inside {what}", root.display()));
            }
        }
        // quen's source tree by its own files, a copy without .git too: never by a build-time path, which would name
        // the build machine's folders in every binary.
        let source = |dir: &Path| {
            dir.join("AGENTS.md").is_file() && dir.join(r"crates\quen\Cargo.toml").is_file()
        };
        if let Some(tree) = root.ancestors().find(|dir| source(dir)) {
            return Err(format!(
                "{name} {} is inside quen's source tree {}",
                root.display(),
                tree.display()
            ));
        }
        if let Some(tree) = root.ancestors().find(|dir| dir.join(".git").exists()) {
            return Err(format!(
                "{name} {} is inside the git work tree {}",
                root.display(),
                tree.display()
            ));
        }
    }
    Ok(())
}

/// Whether `path` is `dir` or under it, compared lexically and case-insensitively (as Windows paths are).
/// ponytail: links and junctions aren't resolved.
fn inside(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
        let mut parts = Vec::new();
        for part in p.components() {
            match part {
                std::path::Component::ParentDir => {
                    parts.pop();
                }
                std::path::Component::CurDir => {}
                part => parts.push(part.as_os_str().to_string_lossy().to_lowercase()),
            }
        }
        parts
    };
    norm(path).starts_with(&norm(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_single_and_plain_double_quoted_values() {
        assert_eq!(
            parse_line(r"harness = 'C:\h\ini_session.py'  # the harness"),
            Some(("harness", r"C:\h\ini_session.py"))
        );
        assert_eq!(parse_line(r#"python = "py""#), Some(("python", "py")));
        assert_eq!(parse_line(r#"harness = "C:\\h""#), None);
        assert_eq!(parse_line("harness = C:/h"), None);
        assert_eq!(parse_line("harness = 'C:/h' trailing"), None);
    }

    #[test]
    fn the_capture_test_switch_is_off_unless_set_true() {
        let path = Path::new("test.toml");
        assert!(!parse("", path).unwrap().force_duplication);
        assert!(
            parse("force_duplication = 'true'", path)
                .unwrap()
                .force_duplication
        );
        assert!(parse("force_duplication = 'yes'", path).is_err());
        let relative = parse("streamline_dev_interposer = 'sl.interposer.dll'", path);
        assert!(relative.unwrap_err().contains("not an absolute path"));
    }

    #[test]
    fn known_builds_are_the_adapter_s_then_the_config_s() {
        let path = Path::new("test.toml");
        let adapter = KnownBuilds::parse(crate::w3::KNOWN_BUILDS).unwrap();
        assert!(!adapter.0.is_empty());
        assert_eq!(parse("", path).unwrap().known_builds, adapter);
        let sha = sha256_hex(b"synthetic build");
        let config = parse(&format!("known_builds = 'gog {sha} test (GOG)'"), path).unwrap();
        let known = config.known_builds.0;
        assert_eq!(known[..adapter.0.len()], adapter.0);
        assert_eq!(
            (known[adapter.0.len()].platform, known.len()),
            ("gog", adapter.0.len() + 1)
        );
        let err = parse(
            "
known_builds = 'epic x y'",
            path,
        )
        .unwrap_err();
        assert!(
            err.contains("test.toml:2: known_builds entry 1") && err.contains("not steam or gog"),
            "{err}"
        );
        // The adapter's build under another label: which is it?
        let first = &adapter.0[0];
        let clash = format!(
            "known_builds = '{} {} relabelled'",
            first.platform, first.sha256
        );
        let err = parse(&clash, path).unwrap_err();
        assert!(err.contains("listed before as"), "{err}");
    }

    #[test]
    fn inside_is_case_insensitive_and_on_component_boundaries() {
        assert!(inside(
            Path::new(r"C:\Src\Quen\state"),
            Path::new(r"c:\src\quen")
        ));
        assert!(inside(
            Path::new(r"C:\src\quen"),
            Path::new(r"C:\src\quen\")
        ));
        assert!(inside(
            Path::new(r"C:\src\x\..\quen\a"),
            Path::new(r"C:\src\quen")
        ));
        assert!(!inside(
            Path::new(r"C:\src\quen-data"),
            Path::new(r"C:\src\quen")
        ));
    }

    #[test]
    fn refuses_a_data_or_capture_root_in_the_source_tree_or_pictures() {
        let mut config = Config {
            data_root: Path::new(env!("CARGO_MANIFEST_DIR")).join("data"),
            python: "python".into(),
            game_exe: "witcher3.exe".into(),
            ..Config::default()
        };
        let err = check_roots(&config).unwrap_err();
        assert!(
            err.starts_with("data root") && err.contains("source tree"),
            "{err}"
        );
        config.data_root = std::env::temp_dir().join("quen-roots-test");
        config.capture_root = Some(config.data_root.join("x"));
        assert_eq!(check_roots(&config), Ok(()));
        config.capture_root = Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("captures"));
        let err = check_roots(&config).unwrap_err();
        assert!(
            err.starts_with("capture root") && err.contains("source tree"),
            "{err}"
        );
        // A copy of the source without .git, known by its own files.
        let copy = std::env::temp_dir().join(format!("quen-roots-copy-{}", std::process::id()));
        fs::create_dir_all(copy.join(r"crates\quen")).unwrap();
        fs::write(copy.join("AGENTS.md"), "synthetic").unwrap();
        fs::write(copy.join(r"crates\quen\Cargo.toml"), "synthetic").unwrap();
        config.capture_root = Some(copy.join(r"x\captures"));
        let err = check_roots(&config).unwrap_err();
        let _ = fs::remove_dir_all(&copy); // this test's own temp folder
        assert!(
            err.starts_with("capture root") && err.contains("quen's source tree"),
            "{err}"
        );
        // A folder under Pictures, if this account has one at all.
        if let Some(pictures) = crate::win::pictures() {
            config.capture_root = Some(pictures.join("quen"));
            let err = check_roots(&config).unwrap_err();
            assert!(err.contains("Pictures"), "{err}");
        }
    }
}
