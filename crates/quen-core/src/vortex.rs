//! Vortex, the mod manager: what its records say it deployed into the game, read-only. Its deployment manifests
//! (`vortex.deployment[.<mod type>].json`, plain JSON, one per mod type, in the folder they deploy into), its staging
//! folder's marker, the Witcher 3 extension's `mods.settings` and its mod folders' names. The caller reads the files and
//! their ids through [`Files`]; here each deployed file gets its Vortex mod, quen's unit and its link state as Vortex's
//! own check before a deploy classes it (LinkingDeployment: `deleted`, `srcdeleted`, `refchange`). quen never writes
//! Vortex's files and never starts, deploys or purges it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::park::{self, Entry};

/// What Vortex leaves in every folder it creates in the game: never a mod's content (mods.rs ignores it).
pub const FOLDER_MARKER: &str = "__folder_managed_by_vortex";
/// Its manifests' names, as a mods.rs pattern.
pub const MANIFESTS: &str = "vortex.deployment*.json";
/// The marker in Vortex's staging folder: `{"instance": ..., "game": ...}`.
pub const STAGING_MARKER: &str = "__vortex_staging_folder";
/// The one deployment method quen parks Vortex's files under (the owner's Q6): hardlinks, the one seen.
pub const HARDLINK: &str = "hardlink_activator";
/// Vortex's process image.
pub const PROCESS: &str = "Vortex.exe";
/// 5.01's menu folder, game-relative and lowercase (boot.rs `menu_dir`).
const MENU: [&str; 5] = ["bin", "config", "r4game", "user_config_matrix", "pc"];

/// The reads the inventory takes, all read-only.
pub trait Files {
    fn read(&self, path: &Path) -> Option<Vec<u8>>;
    /// The file's identity and how many names it has; None when nothing is there.
    fn stat(&self, path: &Path) -> Option<Stat>;
    /// The names in the folder `dir`, each with whether it is a folder; empty when it can't be read.
    fn list(&self, dir: &Path) -> Vec<(String, bool)>;
}

/// A file as [`Files::stat`] read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    /// What a rename keeps and a copy doesn't (the volume and file index on Windows).
    pub id: String,
    pub links: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    instance: String,
    deployment_method: String,
    #[serde(default)]
    deployment_time: Option<f64>,
    staging_path: PathBuf,
    files: Vec<Deployed>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Deployed {
    rel_path: String,
    source: String,
}

#[derive(Deserialize)]
struct Marker {
    instance: String,
}

/// One manifest found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    /// Game-relative; one in the documents folder by its full path.
    pub file: String,
    /// `default` for `vortex.deployment.json`, else its name's middle (`witcher3tl`).
    pub mod_type: String,
    pub method: String,
    pub instance: String,
    /// When Vortex last deployed it, in ms since 1970.
    pub deployed_ms: Option<u64>,
    pub entries: usize,
    /// Why its files count as outside Vortex: it doesn't parse, or its staging folder is gone or another Vortex
    /// instance's.
    pub stale: Option<String>,
}

/// A Vortex mod by its staging folder's name, `<name> <Nexus id> <version> <YYYY-MM-DDTHH-MMZ> <key>`: name, Nexus id
/// and version are null when the name isn't of that form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Named {
    pub name: Option<String>,
    pub nexus_id: Option<u64>,
    pub version: Option<String>,
    pub vortex: String,
}

impl Named {
    pub fn new(vortex: &str) -> Named {
        let label = label(vortex);
        Named {
            name: label.as_ref().map(|l| l.0.to_owned()),
            nexus_id: label.as_ref().map(|l| l.1),
            version: label.map(|l| l.2.to_owned()),
            vortex: vortex.to_owned(),
        }
    }

    /// `W3LoadFix 1.0.8 (nexus 13881)`, or the folder's name.
    pub fn display(&self) -> String {
        match (&self.name, self.nexus_id, &self.version) {
            (Some(name), Some(id), Some(version)) => format!("{name} {version} (nexus {id})"),
            _ => self.vortex.clone(),
        }
    }
}

/// Name, Nexus id and version of a Vortex mod folder's name (see [`Named`]), read from the right: a name may hold
/// spaces and numbers.
fn label(folder: &str) -> Option<(&str, u64, &str)> {
    let mut parts = folder.rsplitn(5, ' ');
    let (key, time, version, id, name) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let stamp = time.len() == 17
        && time.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 | 13 => b == b'-',
            10 => b == b'T',
            16 => b == b'Z',
            _ => b.is_ascii_digit(),
        });
    let ok = stamp
        && digits(id)
        && !version.is_empty()
        && !name.trim().is_empty()
        && !key.is_empty()
        && key.bytes().all(|b| b.is_ascii_alphanumeric());
    if !ok {
        return None;
    }
    Some((name, id.parse().ok()?, version))
}

/// One `[<Mods folder>]` section of `mods.settings`: the game's load order, which Vortex writes on every deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Order {
    pub folder: String,
    pub enabled: Option<bool>,
    pub priority: Option<i64>,
    /// `VK`: the Vortex mod that put it there; none for a folder Vortex doesn't own.
    pub vortex: Option<String>,
}

/// The sections of `mods.settings`, in the file's order.
pub fn load_order(text: &str) -> Vec<Order> {
    let mut out: Vec<Order> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if let Some(folder) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push(Order {
                folder: folder.to_owned(),
                enabled: None,
                priority: None,
                vortex: None,
            });
        } else if let (Some(o), Some((key, value))) = (out.last_mut(), line.split_once('=')) {
            let value = value.trim();
            match key.trim() {
                "Enabled" => o.enabled = Some(value == "1"),
                "Priority" => o.priority = value.parse().ok(),
                "VK" => o.vortex = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    out
}

/// What a deployed file is to quen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// In a Mods folder.
    Mods,
    /// A file in bin\x64_dx12.
    Bin,
    /// A menu XML in 5.01's menu folder.
    Menu,
    /// In a DLC folder.
    Dlc,
    /// Elsewhere under bin, Mods or content, or in the documents folder: the game may load it, and quen has no home
    /// for it.
    Other,
    /// At the game root or in another top folder: readmes and licences (the owner's Q8).
    Inert,
}

/// The kind of the game-relative path `rel` (either separator) and its unit's path: the Mods or DLC folder it is in,
/// else the file itself.
pub fn unit(rel: &str) -> (Kind, String) {
    let parts: Vec<&str> = rel.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
    let path = parts.join("\\");
    let menu = parts.len() == 6
        && lower[..5].iter().map(String::as_str).eq(MENU)
        && lower[5].ends_with(".xml");
    match (lower.first().map(String::as_str), parts.len()) {
        (Some("mods"), 3..) => (Kind::Mods, format!("Mods\\{}", parts[1])),
        (Some("dlc"), 3..) => (Kind::Dlc, format!("DLC\\{}", parts[1])),
        (Some("bin"), 3) if lower[1] == "x64_dx12" => {
            (Kind::Bin, format!("bin\\x64_dx12\\{}", parts[2]))
        }
        (Some("bin"), _) if menu => (Kind::Menu, path),
        (Some("bin" | "mods" | "dlc" | "content"), _) => (Kind::Other, path),
        _ => (Kind::Inert, path),
    }
}

/// A deployed file's state, as Vortex's check before a deploy classes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// Still Vortex's hardlink: more than one name, the staging copy's id.
    Ok,
    /// The deployed file is gone. Vortex's dialog offers to restore it.
    Deleted,
    /// The staging copy is gone, the deployed file is there.
    Srcdeleted,
    /// Both are there, but not one file: a rewrite or a copy. Vortex's dialog by default keeps the newer one.
    Refchange,
}

/// [`Link`] of a deployed file and its staging copy. Neither there counts as `deleted`.
pub fn link(deployed: Option<&Stat>, staged: Option<&Stat>) -> Link {
    match (deployed, staged) {
        (Some(d), Some(s)) if d.links > 1 && d.id == s.id => Link::Ok,
        (Some(_), Some(_)) => Link::Refchange,
        (Some(_), None) => Link::Srcdeleted,
        (None, _) => Link::Deleted,
    }
}

/// Link states counted, each broken file named by its path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Links {
    pub checked: usize,
    pub ok: usize,
    pub deleted: Vec<String>,
    pub srcdeleted: Vec<String>,
    pub refchange: Vec<String>,
    /// Broken already before the launch: not the session's doing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub before_launch: Vec<String>,
}

impl Links {
    fn add(&mut self, file: &File, before: &[String]) {
        self.checked += 1;
        let rel = file.rel.clone();
        match file.link {
            Link::Ok => self.ok += 1,
            _ if before.contains(&rel) => self.before_launch.push(rel),
            Link::Deleted => self.deleted.push(rel),
            Link::Srcdeleted => self.srcdeleted.push(rel),
            Link::Refchange => self.refchange.push(rel),
        }
    }
}

/// A unit of a Vortex mod: a Mods or DLC folder with how many of its files the mod deployed, else one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unit {
    pub kind: Kind,
    pub path: String,
    pub files: usize,
}

/// A Vortex mod with deployed files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mod {
    #[serde(flatten)]
    pub id: Named,
    pub mod_type: String,
    pub method: String,
    /// What the game loads, by quen's units.
    pub units: Vec<Unit>,
    /// Files the game doesn't load (the root's and other top folders').
    pub inert: Vec<String>,
    pub links: Links,
    /// Its staging folder, and the folder it deploys into with that folder's game-relative prefix (None: documents).
    #[serde(skip)]
    staging: PathBuf,
    #[serde(skip)]
    target: (PathBuf, Option<String>),
}

/// A path two Vortex mods claim, or a Mods folder holding files of two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shared {
    pub path: String,
    pub mods: Vec<String>,
}

/// One path both mods of a pair carry in their staging folders, and whose copy Vortex deployed there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overlap {
    pub path: String,
    pub deployed_from: Option<String>,
}

/// Two mods compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pair {
    pub mods: [String; 2],
    pub paths: Vec<Overlap>,
    pub mods_folders: Vec<String>,
}

/// quen's kinds outside Vortex: Mods folders and bin's `.asi`, `.addon64` and `dxgi.dll` that no live manifest lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outside {
    pub mods: Vec<String>,
    pub bin: Vec<String>,
}

/// A deployed file of a live manifest.
#[derive(Debug, Clone)]
struct File {
    /// Its full path, lowercase with `\` ([`key`]).
    key: String,
    rel: String,
    kind: Kind,
    unit: String,
    source: String,
    link: Link,
}

/// What Vortex deployed, from its records.
#[derive(Debug, Clone)]
pub struct Inventory {
    pub manifests: Vec<Manifest>,
    pub mods: Vec<Mod>,
    pub load_order: Vec<Order>,
    /// Mods in Vortex's staging folder that no manifest lists.
    pub undeployed: Vec<Named>,
    pub shared: Vec<Shared>,
    game_dir: PathBuf,
    files: Vec<File>,
}

/// A path compared without case or separator differences.
fn key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

/// `rel` under the folder a manifest deploys into: game-relative after `prefix`, or (no prefix: the documents folder)
/// the full path.
fn shown(prefix: Option<&str>, dir: &Path, rel: &str) -> String {
    let rel = rel.replace('/', "\\");
    match prefix {
        Some("") => rel,
        Some(prefix) => format!("{prefix}\\{rel}"),
        None => dir.join(rel).display().to_string(),
    }
}

/// Reads Vortex's records for the game in `game_dir`: the manifests in the game folder, `Mods`, `DLC` and
/// `documents`, the staging folders they name, and `documents\mods.settings`. None when there is no manifest.
pub fn inventory(
    files: &dyn Files,
    game_dir: &Path,
    documents: Option<&Path>,
) -> Option<Inventory> {
    let mut dirs = vec![
        (game_dir.to_owned(), Some("")),
        (game_dir.join("Mods"), Some("Mods")),
        (game_dir.join("DLC"), Some("DLC")),
    ];
    dirs.extend(documents.map(|d| (d.to_owned(), None)));
    let mut inv = Inventory {
        manifests: Vec::new(),
        mods: Vec::new(),
        load_order: Vec::new(),
        undeployed: Vec::new(),
        shared: Vec::new(),
        game_dir: game_dir.to_owned(),
        files: Vec::new(),
    };
    let mut stagings = BTreeSet::new();
    for (dir, prefix) in &dirs {
        let mut names: Vec<String> = files
            .list(dir)
            .into_iter()
            .filter(|(name, is_dir)| {
                let lower = name.to_lowercase();
                !is_dir && lower.starts_with("vortex.deployment") && lower.ends_with(".json")
            })
            .map(|(name, _)| name)
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let end = name.len().saturating_sub(".json".len());
            let middle = name.get("vortex.deployment".len()..end).unwrap_or_default();
            let mut info = Manifest {
                file: shown(*prefix, dir, &name),
                mod_type: middle.strip_prefix('.').unwrap_or("default").to_owned(),
                method: String::new(),
                instance: String::new(),
                deployed_ms: None,
                entries: 0,
                stale: None,
            };
            let parsed = files
                .read(&path)
                .ok_or_else(|| "it can't be read".to_owned())
                .and_then(|b| {
                    serde_json::from_slice::<ManifestFile>(strip_bom(&b)).map_err(|e| e.to_string())
                });
            let manifest = match parsed {
                Ok(m) => m,
                Err(why) => {
                    info.stale = Some(format!("it doesn't parse: {why}"));
                    inv.manifests.push(info);
                    continue;
                }
            };
            let marker = files
                .read(&manifest.staging_path.join(STAGING_MARKER))
                .and_then(|b| serde_json::from_slice::<Marker>(strip_bom(&b)).ok());
            info.stale = match marker {
                None => Some("its staging folder has no Vortex marker: gone or moved".into()),
                Some(m) if m.instance != manifest.instance => {
                    Some("its staging folder is another Vortex instance's".into())
                }
                Some(_) => None,
            };
            info.method = manifest.deployment_method.clone();
            info.instance = manifest.instance.clone();
            info.deployed_ms = manifest.deployment_time.map(|t| t as u64);
            info.entries = manifest.files.len();
            let live = info.stale.is_none();
            let mod_type = info.mod_type.clone();
            inv.manifests.push(info);
            if !live {
                continue;
            }
            stagings.insert(manifest.staging_path.clone());
            for d in &manifest.files {
                let deployed = dir.join(&d.rel_path);
                let staged = manifest.staging_path.join(&d.source).join(&d.rel_path);
                let rel = shown(*prefix, dir, &d.rel_path);
                let (kind, unit) = match prefix {
                    Some(_) => unit(&rel),
                    None => (Kind::Other, rel.clone()),
                };
                inv.files.push(File {
                    key: key(&deployed),
                    rel,
                    kind,
                    unit,
                    source: d.source.clone(),
                    link: link(files.stat(&deployed).as_ref(), files.stat(&staged).as_ref()),
                });
                if !inv.mods.iter().any(|m| m.id.vortex == d.source) {
                    inv.mods.push(Mod {
                        id: Named::new(&d.source),
                        mod_type: mod_type.clone(),
                        method: manifest.deployment_method.clone(),
                        units: Vec::new(),
                        inert: Vec::new(),
                        links: Links::default(),
                        staging: manifest.staging_path.join(&d.source),
                        target: (dir.clone(), prefix.map(str::to_owned)),
                    });
                }
            }
        }
    }
    if inv.manifests.is_empty() {
        return None;
    }
    for m in &mut inv.mods {
        let mut units: BTreeMap<(Kind, String), Unit> = BTreeMap::new();
        for f in inv.files.iter().filter(|f| f.source == m.id.vortex) {
            m.links.add(f, &[]);
            if f.kind == Kind::Inert {
                m.inert.push(f.rel.clone());
                continue;
            }
            let unit = units
                .entry((f.kind, f.unit.to_lowercase()))
                .or_insert(Unit {
                    kind: f.kind,
                    path: f.unit.clone(),
                    files: 0,
                });
            unit.files += 1;
        }
        m.units = units.into_values().collect();
    }
    inv.mods
        .sort_by_cached_key(|m| m.id.display().to_lowercase());
    inv.load_order = documents
        .and_then(|d| files.read(&d.join("mods.settings")))
        .map(|b| load_order(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    let sources: BTreeSet<&str> = inv.files.iter().map(|f| f.source.as_str()).collect();
    for staging in &stagings {
        let mut names: Vec<String> = files
            .list(staging)
            .into_iter()
            .filter(|(name, is_dir)| {
                *is_dir && !name.starts_with("__") && !sources.contains(name.as_str())
            })
            .map(|(name, _)| name)
            .collect();
        names.sort();
        inv.undeployed.extend(names.iter().map(|n| Named::new(n)));
    }
    // A path two entries claim, and a Mods folder holding files of two mods.
    let mut claims: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for f in &inv.files {
        let claim = claims
            .entry(f.key.clone())
            .or_insert((f.rel.clone(), BTreeSet::new()));
        claim.1.insert(f.source.clone());
        if f.kind == Kind::Mods {
            let folder = claims
                .entry(format!("{}\\", f.unit.to_lowercase()))
                .or_insert((f.unit.clone(), BTreeSet::new()));
            folder.1.insert(f.source.clone());
        }
    }
    inv.shared = claims
        .into_values()
        .filter(|(_, mods)| mods.len() > 1)
        .map(|(path, mods)| Shared {
            path,
            mods: mods.into_iter().collect(),
        })
        .collect();
    Some(inv)
}

impl Inventory {
    /// Every deployed file's link state; a broken one in `before` (what [`Inventory::broken`] said before the launch)
    /// is listed under `before_launch` instead of its class.
    pub fn links(&self, before: &[String]) -> Links {
        let mut links = Links::default();
        self.files.iter().for_each(|f| links.add(f, before));
        links
    }

    /// The deployed files that aren't Vortex's links.
    pub fn broken(&self) -> Vec<String> {
        let files = self.files.iter().filter(|f| f.link != Link::Ok);
        files.map(|f| f.rel.clone()).collect()
    }

    /// The Vortex mods that deployed `path`, or files inside it (a folder).
    pub fn owners(&self, path: &Path) -> Vec<&Mod> {
        let k = key(path);
        let inside = format!("{k}\\");
        let sources: BTreeSet<&str> = self
            .files
            .iter()
            .filter(|f| f.key == k || f.key.starts_with(&inside))
            .map(|f| f.source.as_str())
            .collect();
        self.mods
            .iter()
            .filter(|m| sources.contains(m.id.vortex.as_str()))
            .collect()
    }

    /// Who owns `path`: `Vortex: <mod>` (`+` between several) or `outside Vortex`.
    pub fn owner(&self, path: &Path) -> String {
        match self.owners(path).as_slice() {
            [] => "outside Vortex".into(),
            mods => format!("Vortex: {}", names(mods)),
        }
    }

    /// `path` relative to the game folder, with `\`.
    pub fn rel(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.game_dir).unwrap_or(path);
        rel.to_string_lossy().replace('/', "\\")
    }

    /// The deployed files the game may load that quen can't park (not a Mods folder, nor one of bin's kinds, nor the
    /// `<stem>.ini` of one), by mod: what a boot leaves loaded whatever it parks.
    pub fn left_loaded(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in &self.mods {
            let files = self.files.iter().filter(|f| f.source == m.id.vortex);
            let file = |f: &File| f.rel.rsplit('\\').next().unwrap_or_default().to_lowercase();
            let stems: BTreeSet<String> = files
                .clone()
                .filter(|f| f.kind == Kind::Bin && park::bin_kind(&file(f)))
                .map(|f| {
                    let name = file(f);
                    name.rsplit_once('.')
                        .map_or(name.clone(), |s| s.0.to_owned())
                })
                .collect();
            let config = |f: &File| {
                let name = file(f);
                name.strip_suffix(".ini")
                    .is_some_and(|stem| stems.contains(stem))
            };
            let left: Vec<String> = files
                .filter(|f| match f.kind {
                    Kind::Mods | Kind::Inert => false,
                    Kind::Bin => !park::bin_kind(&file(f)) && !config(f),
                    Kind::Menu | Kind::Dlc | Kind::Other => true,
                })
                .map(|f| f.rel.clone())
                .collect();
            if !left.is_empty() {
                out.insert(m.id.display(), left);
            }
        }
        out
    }

    /// Why a launch that parks (renames) `parked` and rewrites or rebuilds `rewritten` is refused with Vortex here, or
    /// None. A rewritten file with more than one name is refused whoever owns it (the owner's Q7): quen's write and its
    /// end's restore make new files, which break a link. A parked file of Vortex's is refused while Vortex runs
    /// (`running`, asked only then; the owner's Q5), when that can't be told, and under any deployment method but
    /// [`HARDLINK`] (Q6).
    pub fn refusal(
        &self,
        files: &dyn Files,
        parked: &[&Path],
        rewritten: &[&Path],
        running: &dyn Fn() -> Result<bool, String>,
    ) -> Option<String> {
        for path in rewritten {
            if let Some(stat) = files.stat(path).filter(|s| s.links > 1) {
                let whose = match self.owners(path).as_slice() {
                    [] => "in none of Vortex's manifests".into(),
                    mods => format!("Vortex's, {}", names(mods)),
                };
                return Some(format!(
                    "{} is a hardlink ({} names; {whose}): the launch writes a new file there and the end puts a copy \
                     back, which breaks the link. Refused while it is one",
                    path.display(),
                    stat.links
                ));
            }
        }
        let mut owned: Vec<&Mod> = parked.iter().flat_map(|p| self.owners(p)).collect();
        owned.sort_by(|a, b| a.id.vortex.cmp(&b.id.vortex));
        owned.dedup_by(|a, b| a.id.vortex == b.id.vortex);
        if owned.is_empty() {
            return None;
        }
        match running() {
            Ok(false) => {}
            Ok(true) => {
                return Some(format!(
                    "close Vortex first: the launch parks files Vortex deployed ({}), and a Vortex deploy or purge \
                     during the session would undo the parking ({PROCESS} runs)",
                    names(&owned)
                ));
            }
            Err(why) => return Some(format!("can't tell whether Vortex runs: {why}")),
        }
        owned.iter().find(|m| m.method != HARDLINK).map(|m| {
            format!(
                "Vortex deploys {} by {:?}: quen parks Vortex's files only when Vortex deploys by hardlink \
                 ({HARDLINK})",
                m.id.display(),
                m.method
            )
        })
    }

    /// The deployed mod `name` names: `nexus:<id>`, its name or its Vortex folder (any case).
    pub fn find(&self, name: &str) -> Result<&Mod, String> {
        let name = name.trim();
        let id = name
            .strip_prefix("nexus:")
            .map(|n| n.trim().parse::<u64>().ok());
        let hits: Vec<&Mod> = self
            .mods
            .iter()
            .filter(|m| match id {
                Some(id) => id.is_some() && m.id.nexus_id == id,
                None => {
                    m.id.vortex.eq_ignore_ascii_case(name)
                        || m.id
                            .name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(name))
                }
            })
            .collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(format!(
                "no deployed Vortex mod {name:?}: name it by nexus:<id>, its name or its Vortex folder, as the mods \
                 tool lists them"
            )),
            _ => Err(format!(
                "{name:?} names {} Vortex mods: name one by its Vortex folder",
                hits.len()
            )),
        }
    }

    /// The paths both mods carry in their staging folders (each deploys only the winner's copy), and the Mods folders
    /// both deploy into.
    pub fn pair(&self, files: &dyn Files, x: &str, y: &str) -> Result<Pair, String> {
        let (a, b) = (self.find(x)?, self.find(y)?);
        if a.id.vortex == b.id.vortex {
            return Err(format!("{x:?} and {y:?} name one mod"));
        }
        let carried = |m: &Mod| {
            let mut rels = Vec::new();
            walk(files, &m.staging, "", &mut rels);
            let (dir, prefix) = &m.target;
            let paths = rels
                .iter()
                .map(|rel| (key(&dir.join(rel)), shown(prefix.as_deref(), dir, rel)));
            paths.collect::<BTreeMap<String, String>>()
        };
        let theirs = carried(b);
        let paths = carried(a)
            .into_iter()
            .filter(|(k, _)| theirs.contains_key(k))
            .map(|(k, path)| Overlap {
                path,
                deployed_from: self
                    .files
                    .iter()
                    .find(|f| f.key == k)
                    .map(|f| f.source.clone()),
            })
            .collect();
        let folders = |m: &Mod| {
            let units = m.units.iter().filter(|u| u.kind == Kind::Mods);
            units.map(|u| u.path.clone()).collect::<Vec<_>>()
        };
        let theirs = folders(b);
        let mods_folders = folders(a)
            .into_iter()
            .filter(|f| theirs.iter().any(|t| t.eq_ignore_ascii_case(f)))
            .collect();
        Ok(Pair {
            mods: [a.id.vortex.clone(), b.id.vortex.clone()],
            paths,
            mods_folders,
        })
    }
}

/// The mods' displays, with `+` between.
fn names(mods: &[&Mod]) -> String {
    let names: Vec<String> = mods.iter().map(|m| m.id.display()).collect();
    names.join(" + ")
}

/// Every file under `dir`, relative to it with `\`, after `at`.
fn walk(files: &dyn Files, dir: &Path, at: &str, out: &mut Vec<String>) {
    for (name, is_dir) in files.list(dir) {
        let rel = match at {
            "" => name.clone(),
            at => format!("{at}\\{name}"),
        };
        match is_dir {
            true => walk(files, &dir.join(&name), &rel, out),
            false => out.push(rel),
        }
    }
}

/// What quen sees in `mods` (the Mods folder's listing) and `bin` (bin\x64_dx12's) by its own park kinds that no live
/// manifest of `inv` lists: all of it without Vortex.
pub fn outside(inv: Option<&Inventory>, game_dir: &Path, mods: &[Entry], bin: &[Entry]) -> Outside {
    let owned = |path: PathBuf| inv.is_some_and(|i| !i.owners(&path).is_empty());
    let pick = |entries: &[Entry], dir: PathBuf, folders: bool| {
        let mut out: Vec<String> = entries
            .iter()
            .filter(|e| !e.is_link && e.is_dir == folders && (folders || park::bin_kind(&e.name)))
            .filter(|e| !owned(dir.join(&e.name)))
            .map(|e| e.name.clone())
            .collect();
        out.sort_by_key(|n| n.to_lowercase());
        out
    };
    Outside {
        mods: pick(mods, game_dir.join("Mods"), true),
        bin: pick(bin, crate::boot::bin_dir(game_dir), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file system of files only, folders implied, kept by [`key`] with each path's own spelling.
    #[derive(Default)]
    struct Fake(BTreeMap<String, (String, Vec<u8>, Stat)>);

    impl Fake {
        fn put(&mut self, path: &Path, data: &str, id: &str, links: u32) {
            let shown = path.to_string_lossy().replace('/', "\\");
            let stat = Stat {
                id: id.into(),
                links,
            };
            self.0.insert(key(path), (shown, data.into(), stat));
        }

        /// A hardlink: the same file at both paths.
        fn linked(&mut self, a: &Path, b: &Path, id: &str) {
            self.put(a, id, id, 2);
            self.put(b, id, id, 2);
        }
    }

    impl Files for Fake {
        fn read(&self, path: &Path) -> Option<Vec<u8>> {
            self.0.get(&key(path)).map(|f| f.1.clone())
        }

        fn stat(&self, path: &Path) -> Option<Stat> {
            self.0.get(&key(path)).map(|f| f.2.clone())
        }

        fn list(&self, dir: &Path) -> Vec<(String, bool)> {
            let inside = format!("{}\\", key(dir));
            let mut out: Vec<(String, bool)> = Vec::new();
            for (k, (shown, ..)) in &self.0 {
                if let Some(rest) = k.strip_prefix(&inside) {
                    let rest = &shown[shown.len() - rest.len()..];
                    let (name, more) = rest
                        .split_once('\\')
                        .map_or((rest, false), |(n, _)| (n, true));
                    if !out.iter().any(|(n, _)| n == name) {
                        out.push((name.to_owned(), more));
                    }
                }
            }
            out
        }
    }

    const ID: &str = "0b5e0e19-0000-4000-8000-000000000001";
    const W3LOADFIX: &str = "W3LoadFix 13881 1.0.8 2026-10-19T17-54Z Ukn0RyUyK";
    const BRIDGE: &str = "Native Bridge - Remastered 13743 2.0 2026-10-08T18-24Z arYjoKJEf";
    const BEARD: &str = "Bushy Beards 1.0 14605 1 2026-10-19T01-40Z 6VaC3P6l5";
    const WORLD: &str = "Immersive World - 2.2.4 14126 2.2.4 2026-10-21T18-16Z vo9Lcevqp";
    const MEADOWS: &str = "Meadows 11762 1.1 2026-10-16T05-58Z p5316CpXX";

    fn manifest(staging: &Path, method: &str, files: &[(&str, &str)]) -> String {
        let files: Vec<serde_json::Value> = files
            .iter()
            .map(|(rel, source)| serde_json::json!({"relPath": rel, "source": source, "target": "", "time": 1}))
            .collect();
        serde_json::json!({"instance": ID, "version": 1, "deploymentMethod": method, "gameId": "witcher3",
            "deploymentTime": 1_792_623_500_298u64, "stagingPath": staging, "targetPath": "T", "files": files})
        .to_string()
    }

    /// A synthetic game with Vortex: a default manifest in Mods, a `witcher3tl` and a `witcher3menumodroot` one at the
    /// root, every file a hardlink of its staging copy but where a test breaks one; an undeployed mod in staging; a
    /// readme two mods claim; mods.settings.
    fn game() -> (Fake, PathBuf, PathBuf) {
        let (game, docs, staging) = (Path::new("G"), Path::new("D"), Path::new("S"));
        let mut fs = Fake::default();
        fs.put(
            &staging.join(STAGING_MARKER),
            &format!(r#"{{"instance":"{ID}","game":"witcher3"}}"#),
            "m",
            1,
        );
        let tl = [
            (r"bin\x64_dx12\dinput8.dll", BRIDGE),
            (r"bin\x64_dx12\WitcherNativeBridge.asi", BRIDGE),
            (r"mods\modNativeBridge\content\scripts\imports.ws", BRIDGE),
            (r"Mods\modBeards\content\blob0.bundle", BEARD),
            ("Readme.txt", BEARD),
        ];
        let root = [
            (r"bin\x64_dx12\dsound.dll", W3LOADFIX),
            (r"bin\x64_dx12\w3loadfix.dll", W3LOADFIX),
            (r"bin\x64_dx12\w3loadfix.ini", W3LOADFIX),
            (
                r"bin\config\r4game\user_config_matrix\pc\w3loadfix.xml",
                W3LOADFIX,
            ),
            ("W3LoadFix_README.txt", W3LOADFIX),
            (r"W3LoadFix_licenses\LICENSE.txt", W3LOADFIX),
            (r"bin\x64_dx12\ImmersiveWorld.asi", WORLD),
            (r"bin\x64_dx12\ImmersiveWorld.ini", WORLD),
            (r"bin\x64_dx12\BIW_edges_0.bin", WORLD),
            (r"Mods\modImmersiveWorld\content\scripts\local\iw.ws", WORLD),
            ("Readme.txt", WORLD),
        ];
        let mods = [
            (r"modMeadows\content\blob0.bundle", MEADOWS),
            (r"modMeadows\content\metadata.store", MEADOWS),
        ];
        for (n, (dir, list, name)) in [
            (
                game.to_owned(),
                &tl[..],
                "vortex.deployment.witcher3tl.json",
            ),
            (
                game.to_owned(),
                &root[..],
                "vortex.deployment.witcher3menumodroot.json",
            ),
            (game.join("Mods"), &mods[..], "vortex.deployment.json"),
        ]
        .into_iter()
        .enumerate()
        {
            fs.put(&dir.join(name), &manifest(staging, HARDLINK, list), "x", 1);
            for (i, (rel, source)) in list.iter().enumerate() {
                let id = format!("f{n}-{i}");
                fs.linked(&dir.join(rel), &staging.join(source).join(rel), &id);
            }
        }
        // The readme both claim: one file with both staging copies, as Vortex's External Changes dialog left it.
        fs.put(&game.join("Readme.txt"), "r", "readme", 4);
        fs.put(&staging.join(BEARD).join("Readme.txt"), "r", "readme", 4);
        fs.put(&staging.join(WORLD).join("Readme.txt"), "r", "readme", 4);
        fs.put(
            &staging
                .join("Barber Shop V1.1 13878 1 2026-10-08T19-36Z eHJrqDRtZ")
                .join(r"Mods\modBarber\a.ws"),
            "b",
            "b",
            1,
        );
        fs.put(
            &game.join(r"Mods\modBeards").join(FOLDER_MARKER),
            "marker",
            "mk",
            1,
        );
        let settings = "\u{feff}[modNativeBridge]\r\nEnabled=1\r\nPriority=1\r\nVK=Native\r\n[modOwn]\r\nEnabled=0\r\nPriority=2\r\n";
        fs.put(&docs.join("mods.settings"), settings, "ms", 1);
        (fs, game.to_owned(), docs.to_owned())
    }

    fn inv(fs: &Fake) -> Inventory {
        inventory(fs, Path::new("G"), Some(Path::new("D"))).unwrap()
    }

    #[test]
    fn folder_names_give_name_nexus_id_and_version_from_the_right() {
        let named = Named::new(BEARD);
        assert_eq!(
            (
                named.name.as_deref(),
                named.nexus_id,
                named.version.as_deref()
            ),
            (Some("Bushy Beards 1.0"), Some(14605), Some("1"))
        );
        assert_eq!(named.display(), "Bushy Beards 1.0 1 (nexus 14605)");
        assert_eq!(
            Named::new(WORLD).name.as_deref(),
            Some("Immersive World - 2.2.4")
        );
        for bad in [
            "modFoo",
            "Foo 12a4 1.0 2026-10-19T01-40Z key",
            "Foo 1234 1.0 2026-10-19 01-40Z key",
            "Foo 1234 1.0 2026-10-19T01-40Z ke-y",
            " 1234 1.0 2026-10-19T01-40Z key",
        ] {
            let named = Named::new(bad);
            assert_eq!(
                (named.name, named.nexus_id, named.version),
                (None, None, None),
                "{bad}"
            );
            assert_eq!(Named::new(bad).display(), bad);
        }
    }

    #[test]
    fn mods_settings_gives_the_load_order() {
        let order = load_order(
            "\u{feff}[modA]\r\nEnabled=1\r\nPriority=1\r\nVK=A 1 1 x y\r\n[modB]\nEnabled=0\nPriority=x\n",
        );
        assert_eq!(
            order,
            [
                Order {
                    folder: "modA".into(),
                    enabled: Some(true),
                    priority: Some(1),
                    vortex: Some("A 1 1 x y".into())
                },
                Order {
                    folder: "modB".into(),
                    enabled: Some(false),
                    priority: None,
                    vortex: None
                },
            ]
        );
        assert_eq!(load_order(""), []);
    }

    #[test]
    fn deployed_paths_fall_into_quens_units() {
        for (rel, kind, want) in [
            (r"mods\modA\content\x.ws", Kind::Mods, r"Mods\modA"),
            ("Mods/modA/a", Kind::Mods, r"Mods\modA"),
            (r"DLC\dlcA\content\b", Kind::Dlc, r"DLC\dlcA"),
            (r"bin\x64_dx12\A.asi", Kind::Bin, r"bin\x64_dx12\A.asi"),
            (
                r"BIN\X64_DX12\dsound.dll",
                Kind::Bin,
                r"bin\x64_dx12\dsound.dll",
            ),
            (
                r"bin\config\r4game\user_config_matrix\pc\m.xml",
                Kind::Menu,
                r"bin\config\r4game\user_config_matrix\pc\m.xml",
            ),
            (
                r"bin\config\base\general.ini",
                Kind::Other,
                r"bin\config\base\general.ini",
            ),
            (
                r"bin\x64_dx12\plugins\p.dll",
                Kind::Other,
                r"bin\x64_dx12\plugins\p.dll",
            ),
            (
                r"content\content0\scripts\a.ws",
                Kind::Other,
                r"content\content0\scripts\a.ws",
            ),
            (r"Mods\loose.txt", Kind::Other, r"Mods\loose.txt"),
            ("Readme.txt", Kind::Inert, "Readme.txt"),
            (r"Licenses\L.txt", Kind::Inert, r"Licenses\L.txt"),
        ] {
            assert_eq!(unit(rel), (kind, want.to_owned()), "{rel}");
        }
    }

    #[test]
    fn link_states_are_vortexs_own_classes() {
        let stat = |id: &str, links| Stat {
            id: id.into(),
            links,
        };
        assert_eq!(link(Some(&stat("a", 2)), Some(&stat("a", 2))), Link::Ok);
        assert_eq!(
            link(Some(&stat("a", 1)), Some(&stat("a", 1))),
            Link::Refchange
        );
        assert_eq!(
            link(Some(&stat("a", 2)), Some(&stat("b", 2))),
            Link::Refchange
        );
        assert_eq!(link(Some(&stat("a", 2)), None), Link::Srcdeleted);
        assert_eq!(link(None, Some(&stat("a", 2))), Link::Deleted);
        assert_eq!(link(None, None), Link::Deleted);
    }

    #[test]
    fn the_inventory_groups_vortexs_files_by_mod_and_unit() {
        let (fs, ..) = game();
        let inv = inv(&fs);
        let types: Vec<(&str, &str, usize, Option<&String>)> = inv
            .manifests
            .iter()
            .map(|m| {
                (
                    m.file.as_str(),
                    m.mod_type.as_str(),
                    m.entries,
                    m.stale.as_ref(),
                )
            })
            .collect();
        assert_eq!(
            types,
            [
                (
                    "vortex.deployment.witcher3menumodroot.json",
                    "witcher3menumodroot",
                    11,
                    None
                ),
                ("vortex.deployment.witcher3tl.json", "witcher3tl", 5, None),
                (r"Mods\vortex.deployment.json", "default", 2, None),
            ]
        );
        assert_eq!(inv.manifests[0].deployed_ms, Some(1_792_623_500_298));
        let names: Vec<String> = inv.mods.iter().map(|m| m.id.display()).collect();
        assert_eq!(
            names,
            [
                "Bushy Beards 1.0 1 (nexus 14605)",
                "Immersive World - 2.2.4 2.2.4 (nexus 14126)",
                "Meadows 1.1 (nexus 11762)",
                "Native Bridge - Remastered 2.0 (nexus 13743)",
                "W3LoadFix 1.0.8 (nexus 13881)",
            ]
        );
        let fix = inv.find("nexus:13881").unwrap();
        let units: Vec<(Kind, &str, usize)> = fix
            .units
            .iter()
            .map(|u| (u.kind, u.path.as_str(), u.files))
            .collect();
        assert_eq!(
            units,
            [
                (Kind::Bin, r"bin\x64_dx12\dsound.dll", 1),
                (Kind::Bin, r"bin\x64_dx12\w3loadfix.dll", 1),
                (Kind::Bin, r"bin\x64_dx12\w3loadfix.ini", 1),
                (
                    Kind::Menu,
                    r"bin\config\r4game\user_config_matrix\pc\w3loadfix.xml",
                    1
                ),
            ]
        );
        assert_eq!(
            fix.inert,
            ["W3LoadFix_README.txt", r"W3LoadFix_licenses\LICENSE.txt"]
        );
        assert_eq!((fix.links.checked, fix.links.ok), (6, 6));
        assert_eq!(fix.mod_type, "witcher3menumodroot");
        let bridge = inv.find("Native Bridge - Remastered").unwrap();
        assert_eq!(
            bridge.units[0],
            Unit {
                kind: Kind::Mods,
                path: r"Mods\modNativeBridge".into(),
                files: 1
            }
        );
        assert_eq!(bridge.units.len(), 3);
        let meadows = inv.find(MEADOWS).unwrap();
        assert_eq!(
            meadows.units,
            [Unit {
                kind: Kind::Mods,
                path: r"Mods\modMeadows".into(),
                files: 2
            }]
        );
        // The readme two mods claim; the staging folder no manifest lists; mods.settings.
        assert_eq!(
            inv.shared,
            [Shared {
                path: "Readme.txt".into(),
                mods: vec![BEARD.into(), WORLD.into()]
            }]
        );
        let undeployed: Vec<String> = inv.undeployed.iter().map(Named::display).collect();
        assert_eq!(undeployed, ["Barber Shop V1.1 1 (nexus 13878)"]);
        assert_eq!(inv.load_order.len(), 2);
        assert_eq!(inv.load_order[1].enabled, Some(false));
        let links = inv.links(&[]);
        assert_eq!((links.checked, links.ok), (18, 18));
        assert!(inv.broken().is_empty());
    }

    #[test]
    fn broken_links_are_classed_and_the_ones_before_the_launch_kept_apart() {
        let (mut fs, game, _) = game();
        let staging = Path::new("S");
        // Gone from the game; rewritten (a new file); the staging copy gone.
        fs.0.remove(&key(&game.join(r"bin\x64_dx12\dsound.dll")));
        fs.put(
            &game.join(r"Mods\modMeadows\content\blob0.bundle"),
            "new",
            "new",
            1,
        );
        fs.0.remove(&key(&staging
            .join(BRIDGE)
            .join(r"bin\x64_dx12\dinput8.dll")));
        let inv = inv(&fs);
        let links = inv.links(&[]);
        assert_eq!((links.checked, links.ok), (18, 15));
        assert_eq!(links.deleted, [r"bin\x64_dx12\dsound.dll"]);
        assert_eq!(links.refchange, [r"Mods\modMeadows\content\blob0.bundle"]);
        assert_eq!(links.srcdeleted, [r"bin\x64_dx12\dinput8.dll"]);
        assert!(links.before_launch.is_empty());
        let before = inv.broken();
        assert_eq!(before.len(), 3);
        let after = inv.links(&before[..2]);
        assert_eq!(after.before_launch.len(), 2);
        assert_eq!(
            after.deleted.len() + after.srcdeleted.len() + after.refchange.len(),
            1
        );
        let json = serde_json::to_value(inv.links(&[])).unwrap();
        assert!(json.get("before_launch").is_none(), "{json}");
    }

    #[test]
    fn stale_and_unreadable_manifests_count_as_outside_vortex() {
        let (mut fs, game, _) = game();
        let other = Path::new("S2");
        fs.put(
            &other.join(STAGING_MARKER),
            r#"{"instance":"another","game":"witcher3"}"#,
            "m2",
            1,
        );
        let dlc = game.join("DLC");
        fs.put(
            &dlc.join("vortex.deployment.witcher3dlc.json"),
            &manifest(
                other,
                HARDLINK,
                &[(r"dlcX\content\a", "X 1 1 2026-10-19T17-54Z k")],
            ),
            "d",
            1,
        );
        fs.put(&dlc.join(r"dlcX\content\a"), "a", "a", 1);
        fs.put(
            &game.join(r"Mods\vortex.deployment.json"),
            "{not json",
            "x",
            1,
        );
        let inv = inv(&fs);
        let stale: Vec<(&str, &str)> = inv
            .manifests
            .iter()
            .filter_map(|m| Some((m.file.as_str(), m.stale.as_deref()?)))
            .collect();
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(
            stale[0].0 == r"Mods\vortex.deployment.json"
                && stale[0].1.starts_with("it doesn't parse"),
            "{stale:?}"
        );
        assert_eq!(
            stale[1],
            (
                r"DLC\vortex.deployment.witcher3dlc.json",
                "its staging folder is another Vortex instance's"
            )
        );
        // Their files are nobody's: Meadows and the DLC are gone from the mods.
        assert!(inv.find("nexus:11762").is_err());
        assert!(inv.owners(&dlc.join("dlcX")).is_empty());
        assert_eq!(inv.links(&[]).checked, 16);
        // No staging marker at all.
        let (mut fs, ..) = self::game();
        fs.0.remove(&key(&Path::new("S").join(STAGING_MARKER)));
        let inv = super::inventory(&fs, Path::new("G"), None).unwrap();
        assert!(inv.manifests.iter().all(|m| m.stale.as_deref()
            == Some("its staging folder has no Vortex marker: gone or moved")));
        assert!(inv.mods.is_empty());
    }

    #[test]
    fn no_manifest_is_no_vortex() {
        let mut fs = Fake::default();
        fs.put(Path::new(r"G\Mods\modA\a.ws"), "a", "a", 1);
        assert!(inventory(&fs, Path::new("G"), Some(Path::new("D"))).is_none());
    }

    #[test]
    fn owners_name_the_mod_of_a_file_or_a_folder() {
        let (fs, game, _) = game();
        let inv = inv(&fs);
        assert_eq!(
            inv.owner(&game.join(r"Mods\modImmersiveWorld")),
            "Vortex: Immersive World - 2.2.4 2.2.4 (nexus 14126)"
        );
        assert_eq!(
            inv.owner(&game.join(r"MODS\MODMEADOWS")),
            "Vortex: Meadows 1.1 (nexus 11762)"
        );
        assert_eq!(
            inv.owner(&game.join(r"bin\x64_dx12\ImmersiveWorld.asi")),
            "Vortex: Immersive World - 2.2.4 2.2.4 (nexus 14126)"
        );
        assert_eq!(inv.owner(&game.join(r"Mods\modOwn")), "outside Vortex");
        assert_eq!(inv.owner(&game.join(r"Mods\modMeadow")), "outside Vortex");
        assert_eq!(
            inv.owner(&game.join("Readme.txt")),
            "Vortex: Bushy Beards 1.0 1 (nexus 14605) + Immersive World - 2.2.4 2.2.4 (nexus 14126)"
        );
        assert_eq!(inv.rel(&game.join("Mods").join("modA")), r"Mods\modA");
    }

    #[test]
    fn what_a_boot_leaves_loaded_is_what_quen_cant_park() {
        let (fs, ..) = game();
        let left = inv(&fs).left_loaded();
        assert_eq!(
            left,
            BTreeMap::from([
                (
                    "Immersive World - 2.2.4 2.2.4 (nexus 14126)".to_owned(),
                    vec![r"bin\x64_dx12\BIW_edges_0.bin".to_owned()]
                ),
                (
                    "Native Bridge - Remastered 2.0 (nexus 13743)".to_owned(),
                    vec![r"bin\x64_dx12\dinput8.dll".to_owned()]
                ),
                (
                    "W3LoadFix 1.0.8 (nexus 13881)".to_owned(),
                    vec![
                        r"bin\x64_dx12\dsound.dll".to_owned(),
                        r"bin\x64_dx12\w3loadfix.dll".to_owned(),
                        r"bin\x64_dx12\w3loadfix.ini".to_owned(),
                        r"bin\config\r4game\user_config_matrix\pc\w3loadfix.xml".to_owned(),
                    ]
                ),
            ])
        );
    }

    #[test]
    fn launches_that_touch_vortexs_files_are_refused_as_the_owner_decided() {
        let (mut fs, game, docs) = game();
        let inv = inv(&fs);
        let (theirs, own) = (game.join(r"Mods\modMeadows"), game.join(r"Mods\modOwn"));
        let input = docs.join("input.settings");
        fs.put(&input, "i", "input", 1);
        let (yes, no) = (|| Ok(true), || Ok(false));
        let unknown = || Err("no process list".to_owned());
        // Parking nothing of Vortex's: never refused, Vortex running or not.
        assert_eq!(inv.refusal(&fs, &[&own], &[&input], &yes), None);
        assert_eq!(inv.refusal(&fs, &[], &[], &yes), None);
        assert_eq!(inv.refusal(&fs, &[&own], &[], &unknown), None);
        // Parking Vortex's while it runs, or when that can't be told.
        let why = inv.refusal(&fs, &[&theirs], &[], &unknown).unwrap();
        assert_eq!(why, "can't tell whether Vortex runs: no process list");
        let why = inv.refusal(&fs, &[&own, &theirs], &[], &yes).unwrap();
        assert!(
            why.starts_with("close Vortex first")
                && why.contains("Meadows 1.1 (nexus 11762)")
                && why.contains(PROCESS),
            "{why}"
        );
        assert_eq!(inv.refusal(&fs, &[&theirs], &[&input], &no), None);
        // A settings file that is a link, Vortex's or another's, running or not.
        fs.put(&input, "i", "input", 2);
        let why = inv.refusal(&fs, &[], &[&input], &no).unwrap();
        assert!(
            why.contains("is a hardlink (2 names; in none of Vortex's manifests)"),
            "{why}"
        );
        let readme = game.join("Readme.txt");
        let why = inv.refusal(&fs, &[], &[&readme], &no).unwrap();
        assert!(why.contains("4 names; Vortex's, Bushy Beards"), "{why}");
        // Another deployment method.
        let (mut fs, ..) = self::game();
        let staging = Path::new("S");
        fs.put(
            &game.join(r"Mods\vortex.deployment.json"),
            &manifest(
                staging,
                "symlink_activator",
                &[(r"modMeadows\content\blob0.bundle", MEADOWS)],
            ),
            "x",
            1,
        );
        let inv = self::inv(&fs);
        let why = inv.refusal(&fs, &[&theirs], &[], &no).unwrap();
        assert!(
            why.contains("by \"symlink_activator\"") && why.contains(HARDLINK),
            "{why}"
        );
        assert_eq!(
            inv.refusal(&fs, &[&game.join(r"Mods\modBeards")], &[], &no),
            None
        );
    }

    #[test]
    fn a_pair_shows_the_paths_both_carry_and_whose_copy_is_deployed() {
        let (mut fs, game, _) = game();
        let staging = Path::new("S");
        // Both carry a file in modShared; only Bushy Beards' copy is deployed. Both deploy into modShared.
        let shared = r"Mods\modShared\content\x.ws";
        fs.put(&staging.join(WORLD).join(shared), "w", "w", 1);
        let tl = [
            (r"bin\x64_dx12\dinput8.dll", BRIDGE),
            (shared, BEARD),
            (r"Mods\modShared\content\y.ws", WORLD),
        ];
        fs.put(
            &game.join("vortex.deployment.witcher3tl.json"),
            &manifest(staging, HARDLINK, &tl),
            "x",
            1,
        );
        fs.put(&staging.join(BEARD).join(shared), "b", "b", 1);
        fs.put(
            &staging.join(WORLD).join(r"Mods\modShared\content\y.ws"),
            "y",
            "y",
            1,
        );
        let inv = inv(&fs);
        let pair = inv
            .pair(&fs, "nexus:14605", "immersive world - 2.2.4")
            .unwrap();
        assert_eq!(pair.mods, [BEARD.to_owned(), WORLD.to_owned()]);
        assert_eq!(
            pair.paths,
            [
                Overlap {
                    path: r"Mods\modShared\content\x.ws".into(),
                    deployed_from: Some(BEARD.into())
                },
                Overlap {
                    path: "Readme.txt".into(),
                    deployed_from: Some(WORLD.into())
                },
            ]
        );
        assert_eq!(pair.mods_folders, [r"Mods\modShared"]);
        assert!(
            inv.shared
                .iter()
                .any(|s| s.path == r"Mods\modShared" && s.mods.len() == 2),
            "{:?}",
            inv.shared
        );
        assert!(inv.pair(&fs, "nexus:14605", "nexus:14605").is_err());
        assert!(
            inv.pair(&fs, "nexus:1", "nexus:14605")
                .unwrap_err()
                .starts_with("no deployed Vortex mod \"nexus:1\"")
        );
        assert!(inv.find("nexus:x").is_err());
    }

    #[test]
    fn outside_lists_quens_kinds_no_manifest_lists() {
        let (fs, game, _) = game();
        let inv = inv(&fs);
        let entry = |name: &str, is_dir| Entry {
            name: name.into(),
            is_dir,
            is_link: false,
        };
        let mods = [
            entry("modMeadows", true),
            entry("modQuen", true),
            entry("modNativeBridge", true),
            entry("vortex.deployment.json", false),
        ];
        let bin = [
            entry("W3Quen.asi", false),
            entry("ImmersiveWorld.asi", false),
            entry("dsound.dll", false),
            entry("dxgi.dll", false),
        ];
        let out = outside(Some(&inv), &game, &mods, &bin);
        assert_eq!(
            out,
            Outside {
                mods: vec!["modQuen".into()],
                bin: vec!["dxgi.dll".into(), "W3Quen.asi".into()]
            }
        );
        let all = outside(None, &game, &mods, &bin);
        assert_eq!(all.mods, ["modMeadows", "modNativeBridge", "modQuen"]);
        assert_eq!(all.bin, ["dxgi.dll", "ImmersiveWorld.asi", "W3Quen.asi"]);
    }
}
