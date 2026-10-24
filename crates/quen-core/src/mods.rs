//! Which mods were in use, as fingerprints rather than version numbers. An entry's identity is its kind and
//! name (`asi:W3ElementMap`), its build is a content hash, and its config is a hash plus the values of the keys that
//! change behaviour. Files a mod writes while the game runs are not hashed, so a new log doesn't make a new mod set.
//! Files are read through [`FileSource`], so this runs on any OS, and a [`HashCache`] keyed by (path, size, mtime)
//! rereads only what changed.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::journal::sha256_hex;
use crate::ledger::glob;

/// File-name patterns never hashed in a mod folder, whatever its kind: what mods write while the game runs, and
/// backups, and Vortex's marker and manifests, so a mod hashes the same installed by hand or by Vortex. The adapter
/// extends it per kind through [`ModSpec::ignore`]. W3's `*.cache` files in Mods and dlc folders are content
/// (texture.cache, collision.cache), so they are hashed.
pub const IGNORED: &[&str] = &[
    "*.log",
    "*.bak",
    "*.userbak",
    crate::vortex::FOLDER_MARKER,
    crate::vortex::MANIFESTS,
];

/// What a mod is, by how it is installed. Its name is the prefix of an identity, as in `asi:W3ElementMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A folder in `Mods`.
    #[serde(rename = "mods")]
    ModsFolder,
    /// An `.asi` in bin.
    Asi,
    /// A ReShade add-on, an `.addon64` in bin.
    Addon,
    /// ReShade itself, `dxgi.dll` in bin.
    ReShade,
    /// A folder in `dlc`.
    Dlc,
    /// Another file in bin that a mod manager's records name as a mod's (a DLL, a data file).
    Bin,
    /// A mod's menu XML in the menu folder, as a mod manager's records name it.
    Menu,
}

impl Kind {
    pub const ALL: &[Kind] = &[
        Kind::ModsFolder,
        Kind::Asi,
        Kind::Addon,
        Kind::ReShade,
        Kind::Dlc,
        Kind::Bin,
        Kind::Menu,
    ];

    /// The serde name, also stored in the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::ModsFolder => "mods",
            Kind::Asi => "asi",
            Kind::Addon => "addon",
            Kind::ReShade => "reshade",
            Kind::Dlc => "dlc",
            Kind::Bin => "bin",
            Kind::Menu => "menu",
        }
    }

    /// Mods and dlc entries are folders, hashed as a tree; the others are one file.
    pub fn is_folder(self) -> bool {
        matches!(self, Kind::ModsFolder | Kind::Dlc)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The read-only file access fingerprinting needs. Paths are the source's own (absolute on a real file system).
pub trait FileSource {
    /// Every file under the folder `dir`, at any depth, as a path relative to it, in any order.
    fn list(&self, dir: &Path) -> io::Result<Vec<PathBuf>>;
    fn size(&self, path: &Path) -> io::Result<u64>;
    /// The last write time.
    fn mtime(&self, path: &Path) -> io::Result<SystemTime>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// File sha256s keyed by (path, size, mtime): a file is read again only when its size or mtime changed. Keep one for
/// as long as quen runs and pass it to every [`fingerprint`].
#[derive(Debug, Clone, Default)]
pub struct HashCache {
    files: HashMap<PathBuf, (u64, SystemTime, String)>,
}

impl HashCache {
    /// The sha256 and size of the file at `path`.
    fn sha256(&mut self, src: &dyn FileSource, path: &Path) -> io::Result<(String, u64)> {
        let (size, mtime) = (src.size(path)?, src.mtime(path)?);
        if let Some((s, t, sha)) = self.files.get(path)
            && (*s, *t) == (size, mtime)
        {
            return Ok((sha.clone(), size));
        }
        let sha = sha256_hex(&src.read(path)?);
        self.files
            .insert(path.to_owned(), (size, mtime, sha.clone()));
        Ok((sha, size))
    }
}

/// An installed mod as the adapter found it, before hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSpec {
    pub kind: Kind,
    pub name: String,
    /// The mod's file, or its folder for a folder kind ([`Kind::is_folder`]).
    pub home: PathBuf,
    pub parked: bool,
    /// Patterns not hashed in this folder on top of [`IGNORED`]: the adapter's list for this kind. Like those, each is
    /// matched against every segment of a file's relative path, so a cache folder's name skips the whole folder.
    pub ignore: Vec<String>,
    /// In order of precedence: a later source's value for a key overrides an earlier one's, as ReShade.ini `[W3Perf]`
    /// overrides W3Perf's own ini.
    pub config: Vec<ConfigSource>,
}

/// A config file of a mod, or one `[Section]` of a file it shares with others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    pub path: PathBuf,
    /// Only the lines under this section's headers count, for both the hash and the values.
    pub section: Option<String>,
    /// The keys whose values are recorded, matched exactly; empty records every key in scope. Within one source, a
    /// key's first line wins.
    pub keys: Vec<String>,
    /// With a section: each value is recorded as `<section>.<key>`, so it neither overrides nor is overridden by
    /// another source's same key (RenoDX's global and preset sections).
    pub qualify: bool,
}

/// One mod's fingerprint: identity (kind and name), build and config. It is a row of `mod_set_entries`, and
/// deserializes from JSON for `ledger.ingest`, refusing unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModEntry {
    pub kind: Kind,
    pub name: String,
    /// Where it was hashed. No hash covers it, so parking or moving a mod doesn't change its fingerprint.
    pub home: PathBuf,
    #[serde(default)]
    pub parked: bool,
    /// The file's sha256; for a folder, the sha256 of its sorted `relative/path<TAB>file sha256` lines (each ending in
    /// a line feed), leaving out ignored files and config sources.
    pub content_hash: String,
    /// How many files the content hash covers, and their total size.
    pub file_count: u64,
    pub bytes: u64,
    /// The sha256 of the sorted `file name[Section]<TAB>sha256 of the lines in scope` lines of its config sources (`-`
    /// for a missing file); `None` when it has none.
    pub config_hash: Option<String>,
    /// The recorded config values, as text, by key (`<section>.<key>` from a [`ConfigSource::qualify`] source).
    #[serde(default)]
    pub config_values: BTreeMap<String, String>,
}

impl ModEntry {
    /// The identity, `kind:name`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }
}

/// Fingerprints one installed mod. In a folder, files matching [`IGNORED`] or `spec.ignore` in any segment of their
/// relative path (ignoring ASCII case) are not hashed, nor are its config sources. A missing config file is part of
/// the config hash; any other read error is returned.
pub fn fingerprint(
    src: &dyn FileSource,
    cache: &mut HashCache,
    spec: &ModSpec,
) -> io::Result<ModEntry> {
    let (content_hash, file_count, bytes) = if spec.kind.is_folder() {
        let patterns: Vec<String> = IGNORED
            .iter()
            .copied()
            .chain(spec.ignore.iter().map(String::as_str))
            .map(str::to_ascii_lowercase)
            .collect();
        let mut lines = Vec::new();
        let mut bytes = 0;
        for rel in src.list(&spec.home)? {
            let path = spec.home.join(&rel);
            let rel: Vec<_> = rel.iter().map(|s| s.to_string_lossy()).collect();
            let ignored = rel.iter().any(|segment| {
                let segment = segment.to_ascii_lowercase();
                patterns.iter().any(|p| glob(p, &segment))
            });
            if ignored || spec.config.iter().any(|c| c.path == path) {
                continue;
            }
            let (sha, size) = cache.sha256(src, &path)?;
            bytes += size;
            lines.push(format!("{}\t{sha}\n", rel.join("/")));
        }
        (hash_lines(&mut lines), lines.len() as u64, bytes)
    } else {
        let (sha, size) = cache.sha256(src, &spec.home)?;
        (sha, 1, size)
    };
    let (config_hash, config_values) = config(src, &spec.config)?;
    Ok(ModEntry {
        kind: spec.kind,
        name: spec.name.clone(),
        home: spec.home.clone(),
        parked: spec.parked,
        content_hash,
        file_count,
        bytes,
        config_hash,
        config_values,
    })
}

/// The config hash and recorded values of `sources` (see [`ModEntry::config_hash`]). Files are read as UTF-8 (lossy,
/// without a BOM): a header is a line `[Name]`, a value a line `Key=Value` outside comments (`;`, `#`), both trimmed.
fn config(
    src: &dyn FileSource,
    sources: &[ConfigSource],
) -> io::Result<(Option<String>, BTreeMap<String, String>)> {
    if sources.is_empty() {
        return Ok((None, BTreeMap::new()));
    }
    let mut lines = Vec::new();
    let mut values = BTreeMap::new();
    for source in sources {
        let file = source.path.file_name().unwrap_or(source.path.as_os_str());
        let label = match &source.section {
            Some(section) => format!("{}[{section}]", file.to_string_lossy()),
            None => file.to_string_lossy().into_owned(),
        };
        let data = match src.read(&source.path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                lines.push(format!("{label}\t-\n"));
                continue;
            }
            Err(e) => return Err(e),
        };
        let text = String::from_utf8_lossy(&data);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let mut current = None;
        let scope: Vec<&str> = text
            .split_inclusive('\n')
            .filter(|line| {
                let header = line
                    .trim()
                    .strip_prefix('[')
                    .and_then(|h| h.strip_suffix(']'));
                if header.is_some() {
                    current = header;
                }
                match &source.section {
                    None => true,
                    Some(section) => header.is_none() && current == Some(section.as_str()),
                }
            })
            .collect();
        lines.push(format!(
            "{label}\t{}\n",
            sha256_hex(scope.concat().as_bytes())
        ));
        let mut found = BTreeMap::new();
        let prefix = source.section.as_deref().filter(|_| source.qualify);
        for line in scope.iter().map(|line| line.trim()) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !line.starts_with([';', '#', '['])
                && !key.is_empty()
                && (source.keys.is_empty() || source.keys.iter().any(|k| k == key))
            {
                let key = prefix.map_or_else(|| key.to_string(), |s| format!("{s}.{key}"));
                found.entry(key).or_insert_with(|| value.trim().to_string());
            }
        }
        values.extend(found);
    }
    Ok((Some(hash_lines(&mut lines)), values))
}

/// The sha256 of `lines`, sorted, each already ending in a line feed.
fn hash_lines(lines: &mut [String]) -> String {
    lines.sort();
    sha256_hex(lines.concat().as_bytes())
}

/// Why entries can't form a mod set. Nothing is hashed when one comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// An empty name, or one holding a tab or line break, which would make the set hash ambiguous.
    BadName(String),
    /// Two entries with this identity.
    Repeated(String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName(name) => {
                write!(f, "mod name {name:?} is empty or holds a tab or line break")
            }
            Self::Repeated(id) => write!(f, "{id} is in the mod set twice"),
        }
    }
}

impl std::error::Error for SetError {}

/// The sha256 that names a mod set ("same mods" = same hash): over one `kind<TAB>name<TAB>parked (0 or 1)<TAB>content
/// hash<TAB>config hash` line per entry (`-` for no config), sorted, each ending in a line feed. The entries' order,
/// homes, sizes and config values don't change it.
pub fn set_hash(entries: &[ModEntry]) -> Result<String, SetError> {
    let mut seen = HashSet::new();
    let mut lines = Vec::with_capacity(entries.len());
    for e in entries {
        if e.name.is_empty() || e.name.contains(['\t', '\n', '\r']) {
            return Err(SetError::BadName(e.name.clone()));
        }
        if !seen.insert(e.id()) {
            return Err(SetError::Repeated(e.id()));
        }
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\n",
            e.kind,
            e.name,
            u8::from(e.parked),
            e.content_hash,
            e.config_hash.as_deref().unwrap_or("-")
        ));
    }
    Ok(hash_lines(&mut lines))
}

/// How mod set `b` differs from `a`, entry by entry, each list sorted by identity. Sets are compared by what loaded,
/// so a parked entry counts as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub build_changed: Vec<BuildChange>,
    pub config_changed: Vec<ConfigChange>,
}

/// An entry in both sets with a different content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildChange {
    pub entry: String,
    pub before: String,
    pub after: String,
}

/// An entry in both sets with a different config hash or values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigChange {
    pub entry: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The recorded values that differ: key → [before, after], `None` where it wasn't recorded.
    pub values: BTreeMap<String, [Option<String>; 2]>,
}

/// Lists the entries `b` added and removed relative to `a`, and those whose build or config changed (an entry can be
/// in both). Entries are matched by identity, which [`set_hash`] keeps unique.
pub fn diff(a: &[ModEntry], b: &[ModEntry]) -> Diff {
    fn loaded(set: &[ModEntry]) -> BTreeMap<String, &ModEntry> {
        set.iter()
            .filter(|e| !e.parked)
            .map(|e| (e.id(), e))
            .collect()
    }
    let (a, b) = (loaded(a), loaded(b));
    let mut d = Diff {
        removed: a
            .keys()
            .filter(|id| !b.contains_key(*id))
            .cloned()
            .collect(),
        ..Diff::default()
    };
    for (id, y) in &b {
        let Some(x) = a.get(id) else {
            d.added.push(id.clone());
            continue;
        };
        if x.content_hash != y.content_hash {
            d.build_changed.push(BuildChange {
                entry: id.clone(),
                before: x.content_hash.clone(),
                after: y.content_hash.clone(),
            });
        }
        if x.config_hash != y.config_hash || x.config_values != y.config_values {
            let keys: BTreeSet<&String> = x
                .config_values
                .keys()
                .chain(y.config_values.keys())
                .collect();
            let values = keys
                .into_iter()
                .map(|k| {
                    (
                        k,
                        [
                            x.config_values.get(k).cloned(),
                            y.config_values.get(k).cloned(),
                        ],
                    )
                })
                .filter(|(_, [before, after])| before != after)
                .map(|(k, pair)| (k.clone(), pair))
                .collect();
            d.config_changed.push(ConfigChange {
                entry: id.clone(),
                before: x.config_hash.clone(),
                after: y.config_hash.clone(),
                values,
            });
        }
    }
    d
}

/// The real file system. A link to a folder is walked like a folder.
/// ponytail: a junction loop would never end; nothing in a game folder makes one.
pub struct Disk;

impl FileSource for Disk {
    fn list(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut todo = vec![PathBuf::new()];
        while let Some(rel) = todo.pop() {
            for entry in std::fs::read_dir(dir.join(&rel))? {
                let rel = rel.join(entry?.file_name());
                if std::fs::metadata(dir.join(&rel))?.is_dir() {
                    todo.push(rel);
                } else {
                    files.push(rel);
                }
            }
        }
        Ok(files)
    }

    fn size(&self, path: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }

    fn mtime(&self, path: &Path) -> io::Result<SystemTime> {
        std::fs::metadata(path)?.modified()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

/// An in-memory [`FileSource`] for tests. It lists files in the order they were first put, gives every put a later
/// mtime, and counts reads. A folder exists while it holds a file.
#[derive(Debug, Default)]
pub struct MemSource {
    files: Vec<(PathBuf, Vec<u8>, SystemTime)>,
    clock: u64,
    reads: Cell<usize>,
}

impl MemSource {
    /// Creates or rewrites `path`, as a program would.
    pub fn put(&mut self, path: impl AsRef<Path>, data: impl AsRef<[u8]>) {
        self.clock += 1;
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(self.clock);
        let file = (path.as_ref().to_owned(), data.as_ref().to_vec(), mtime);
        match self.files.iter_mut().find(|f| f.0 == file.0) {
            Some(old) => *old = file,
            None => self.files.push(file),
        }
    }

    /// How many file reads there have been.
    pub fn reads(&self) -> usize {
        self.reads.get()
    }

    fn file(&self, path: &Path) -> io::Result<&(PathBuf, Vec<u8>, SystemTime)> {
        self.files
            .iter()
            .find(|f| f.0 == path)
            .ok_or_else(|| missing(path))
    }
}

fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} not found", path.display()),
    )
}

impl FileSource for MemSource {
    fn list(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let files: Vec<PathBuf> = self
            .files
            .iter()
            .filter_map(|f| f.0.strip_prefix(dir).ok())
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(Path::to_owned)
            .collect();
        if files.is_empty() {
            return Err(missing(dir));
        }
        Ok(files)
    }

    fn size(&self, path: &Path) -> io::Result<u64> {
        Ok(self.file(path)?.1.len() as u64)
    }

    fn mtime(&self, path: &Path) -> io::Result<SystemTime> {
        Ok(self.file(path)?.2)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let data = self.file(path)?.1.clone();
        self.reads.set(self.reads.get() + 1);
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "GAME/bin/x64";

    /// A synthetic game: a Mods folder with its own ini and a log, W3ElementMap (an ASI with an ini beside it), W3Perf
    /// (an ASI whose ini ReShade.ini `[W3Perf]` overrides) and the logs they write.
    fn game() -> MemSource {
        let mut src = MemSource::default();
        src.put("GAME/Mods/modAlpha/content/blob0.bundle", "bundle");
        src.put("GAME/Mods/modAlpha/content/texture.cache", "textures");
        src.put(
            "GAME/Mods/modAlpha/content/scripts/alpha.ws",
            "exec function alpha() {}",
        );
        src.put("GAME/Mods/modAlpha/alpha.ini", "[Alpha]\nEnabled=true\n");
        src.put("GAME/Mods/modAlpha/alpha.log", "boot 1\n");
        src.put(format!("{BIN}/W3ElementMap.asi"), "element map");
        src.put(
            format!("{BIN}/W3ElementMap.ini"),
            "[General]\r\nMode=raise\r\nRaisePercent=40\r\n; RaisePercent=10\r\n",
        );
        src.put(format!("{BIN}/W3ElementMap.log"), "boot 1\n");
        src.put(format!("{BIN}/W3Perf.asi"), "perf");
        src.put(
            format!("{BIN}/W3Perf.ini"),
            "[Toggles]\nTlasMask=0\nSkipFog=0\n",
        );
        src.put(
            format!("{BIN}/ReShade.ini"),
            "\u{feff}[GENERAL]\nPresetPath=.\\preset.ini\n[W3Perf]\nTlasMask=1\n[OVERLAY]\nWindowPos=0,0\n",
        );
        src
    }

    fn source(path: &str, section: Option<&str>, keys: &[&str]) -> ConfigSource {
        ConfigSource {
            path: path.into(),
            section: section.map(String::from),
            keys: keys.iter().map(|k| k.to_string()).collect(),
            qualify: false,
        }
    }

    fn specs() -> Vec<ModSpec> {
        let spec = |kind, name: &str, home: String, config| ModSpec {
            kind,
            name: name.into(),
            home: home.into(),
            parked: false,
            ignore: vec!["shadercache".into()],
            config,
        };
        vec![
            spec(
                Kind::ModsFolder,
                "modAlpha",
                "GAME/Mods/modAlpha".into(),
                vec![source("GAME/Mods/modAlpha/alpha.ini", None, &["Enabled"])],
            ),
            spec(
                Kind::Asi,
                "W3ElementMap",
                format!("{BIN}/W3ElementMap.asi"),
                vec![source(
                    &format!("{BIN}/W3ElementMap.ini"),
                    None,
                    &["Mode", "RaisePercent"],
                )],
            ),
            spec(
                Kind::Asi,
                "W3Perf",
                format!("{BIN}/W3Perf.asi"),
                vec![
                    source(&format!("{BIN}/W3Perf.ini"), None, &[]),
                    source(&format!("{BIN}/ReShade.ini"), Some("W3Perf"), &[]),
                ],
            ),
        ]
    }

    fn fingerprints(src: &MemSource, cache: &mut HashCache) -> Vec<ModEntry> {
        specs()
            .iter()
            .map(|spec| fingerprint(src, cache, spec).unwrap())
            .collect()
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_folder_hash_is_over_its_sorted_paths_and_file_hashes() {
        let entries = fingerprints(&game(), &mut HashCache::default());
        let tree = format!(
            "content/blob0.bundle\t{}\ncontent/scripts/alpha.ws\t{}\ncontent/texture.cache\t{}\n",
            sha256_hex(b"bundle"),
            sha256_hex(b"exec function alpha() {}"),
            sha256_hex(b"textures"),
        );
        let alpha = &entries[0];
        assert_eq!(alpha.id(), "mods:modAlpha");
        assert_eq!(alpha.content_hash, sha256_hex(tree.as_bytes()));
        assert_eq!((alpha.file_count, alpha.bytes), (3, 6 + 8 + 24));
        let config = format!("alpha.ini\t{}\n", sha256_hex(b"[Alpha]\nEnabled=true\n"));
        assert_eq!(alpha.config_hash, Some(sha256_hex(config.as_bytes())));
        assert_eq!(alpha.config_values, values(&[("Enabled", "true")]));

        let element_map = &entries[1];
        assert_eq!(element_map.id(), "asi:W3ElementMap");
        assert_eq!(element_map.content_hash, sha256_hex(b"element map"));
        assert_eq!((element_map.file_count, element_map.bytes), (1, 11));
        assert_eq!(
            element_map.config_values,
            values(&[("Mode", "raise"), ("RaisePercent", "40")]),
            "named keys only, comments skipped"
        );

        let perf = &entries[2];
        let config = [
            format!("ReShade.ini[W3Perf]\t{}\n", sha256_hex(b"TlasMask=1\n")),
            format!(
                "W3Perf.ini\t{}\n",
                sha256_hex(b"[Toggles]\nTlasMask=0\nSkipFog=0\n")
            ),
        ];
        assert_eq!(
            perf.config_hash,
            Some(sha256_hex(config.concat().as_bytes()))
        );
        assert_eq!(
            perf.config_values,
            values(&[("SkipFog", "0"), ("TlasMask", "1")]),
            "ReShade.ini [W3Perf] overrides the ASI's ini"
        );
    }

    #[test]
    fn runtime_files_written_between_boots_change_no_hash() {
        let mut src = game();
        let mut cache = HashCache::default();
        let before = fingerprints(&src, &mut cache);
        src.put("GAME/Mods/modAlpha/alpha.log", "boot 2, a longer log\n");
        src.put("GAME/Mods/modAlpha/content/ALPHA.LOG", "new log\n");
        src.put("GAME/Mods/modAlpha/content/blob0.bundle.bak", "old bundle");
        src.put(
            "GAME/Mods/modAlpha/alpha.ini.userbak",
            "[Alpha]\nEnabled=false\n",
        );
        src.put("GAME/Mods/modAlpha/ShaderCache/0001.bin", "compiled");
        // Vortex's marker in every folder it made, and a manifest: the same mod installed by Vortex hashes the same.
        src.put("GAME/Mods/modAlpha/__folder_managed_by_vortex", "Vortex");
        src.put(
            "GAME/Mods/modAlpha/content/__folder_managed_by_vortex",
            "Vortex",
        );
        src.put("GAME/Mods/modAlpha/vortex.deployment.witcher3tl.json", "{}");
        src.put(format!("{BIN}/W3ElementMap.log"), "boot 2\n");
        src.put(
            format!("{BIN}/ReShade.ini"),
            "[GENERAL]\nPresetPath=.\\preset.ini\n[W3Perf]\nTlasMask=1\n[OVERLAY]\nWindowPos=640,480\n",
        );
        let after = fingerprints(&src, &mut HashCache::default());
        assert_eq!(after, before);
        assert_eq!(set_hash(&after), set_hash(&before));
        assert_eq!(diff(&before, &after), Diff::default());
    }

    #[test]
    fn a_changed_ini_value_changes_the_config_hash_and_shows_in_the_diff() {
        let mut src = game();
        let mut cache = HashCache::default();
        let before = fingerprints(&src, &mut cache);
        src.put("GAME/Mods/modAlpha/alpha.ini", "[Alpha]\nEnabled=false\n");
        src.put(
            format!("{BIN}/W3ElementMap.ini"),
            "[General]\r\nMode=raise\r\nRaisePercent=60\r\n",
        );
        src.put(
            format!("{BIN}/ReShade.ini"),
            "[GENERAL]\nPresetPath=.\\preset.ini\n[W3Perf]\nTlasMask=0\n",
        );
        let after = fingerprints(&src, &mut cache);
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(
                b.content_hash,
                a.content_hash,
                "{}: config isn't build",
                a.id()
            );
            assert_ne!(b.config_hash, a.config_hash, "{}", a.id());
        }
        assert_ne!(set_hash(&before), set_hash(&after));
        let change = |entry: &str, b: &ModEntry, a: &ModEntry, key: &str, was: &str, is: &str| {
            ConfigChange {
                entry: entry.into(),
                before: b.config_hash.clone(),
                after: a.config_hash.clone(),
                values: BTreeMap::from([(key.into(), [Some(was.into()), Some(is.into())])]),
            }
        };
        let want = Diff {
            config_changed: vec![
                change(
                    "asi:W3ElementMap",
                    &before[1],
                    &after[1],
                    "RaisePercent",
                    "40",
                    "60",
                ),
                change("asi:W3Perf", &before[2], &after[2], "TlasMask", "1", "0"),
                change(
                    "mods:modAlpha",
                    &before[0],
                    &after[0],
                    "Enabled",
                    "true",
                    "false",
                ),
            ],
            ..Diff::default()
        };
        assert_eq!(diff(&before, &after), want);
    }

    #[test]
    fn listing_order_changes_no_hash() {
        let first = game();
        let mut reversed = MemSource::default();
        for (path, data, _) in first.files.iter().rev() {
            reversed.put(path, data);
        }
        assert_ne!(
            reversed.list(Path::new("GAME/Mods/modAlpha")).unwrap(),
            first.list(Path::new("GAME/Mods/modAlpha")).unwrap()
        );
        let a = fingerprints(&first, &mut HashCache::default());
        let mut b = fingerprints(&reversed, &mut HashCache::default());
        assert_eq!(a, b);
        b.reverse();
        assert_eq!(set_hash(&a).unwrap(), set_hash(&b).unwrap());
    }

    #[test]
    fn the_cache_rereads_only_changed_files() {
        let mut src = game();
        let mut cache = HashCache::default();
        let specs = specs();
        let spec = &specs[0];
        let first = fingerprint(&src, &mut cache, spec).unwrap();
        assert_eq!(
            src.reads(),
            4,
            "3 content files and the ini; the log isn't read"
        );
        assert_eq!(fingerprint(&src, &mut cache, spec).unwrap(), first);
        assert_eq!(src.reads(), 5, "only the ini, for its values");

        src.put("GAME/Mods/modAlpha/content/blob0.bundle", "bundle");
        let touched = fingerprint(&src, &mut cache, spec).unwrap();
        assert_eq!(src.reads(), 7, "a new mtime is a reread");
        assert_eq!(touched, first);

        src.put("GAME/Mods/modAlpha/content/blob0.bundle", "bundle v2");
        let rebuilt = fingerprint(&src, &mut cache, spec).unwrap();
        assert_eq!(src.reads(), 9);
        assert_ne!(rebuilt.content_hash, first.content_hash);
        assert_eq!(rebuilt.bytes, first.bytes + 3);
    }

    #[test]
    fn diff_lists_added_removed_and_rebuilt_entries() {
        let mut src = game();
        let before = fingerprints(&src, &mut HashCache::default());
        src.put(format!("{BIN}/W3ElementMap.asi"), "element map v2");
        src.put("GAME/dlc/dlcBeta/content/blob0.bundle", "beta");
        let mut specs = specs();
        specs[0].parked = true;
        specs.push(ModSpec {
            kind: Kind::Dlc,
            name: "dlcBeta".into(),
            home: "GAME/dlc/dlcBeta".into(),
            parked: false,
            ignore: vec![],
            config: vec![],
        });
        let mut cache = HashCache::default();
        let after: Vec<ModEntry> = specs
            .iter()
            .map(|spec| fingerprint(&src, &mut cache, spec).unwrap())
            .collect();
        assert_eq!(after[3].config_hash, None);
        let d = diff(&before, &after);
        assert_eq!(d.added, ["dlc:dlcBeta"]);
        assert_eq!(d.removed, ["mods:modAlpha"], "parked counts as absent");
        assert_eq!(
            d.build_changed,
            [BuildChange {
                entry: "asi:W3ElementMap".into(),
                before: sha256_hex(b"element map"),
                after: sha256_hex(b"element map v2"),
            }]
        );
        assert!(d.config_changed.is_empty());
        assert_eq!(diff(&after, &before).added, ["mods:modAlpha"]);
    }

    #[test]
    fn missing_config_files_are_recorded_and_other_misses_are_errors() {
        let src = game();
        let mut spec = specs()[1].clone();
        let present = fingerprint(&src, &mut HashCache::default(), &spec).unwrap();
        spec.config[0].path = format!("{BIN}/missing.ini").into();
        let missing = fingerprint(&src, &mut HashCache::default(), &spec).unwrap();
        let line = "missing.ini\t-\n";
        assert_eq!(missing.config_hash, Some(sha256_hex(line.as_bytes())));
        assert!(missing.config_values.is_empty());
        assert_eq!(missing.content_hash, present.content_hash);

        spec.home = format!("{BIN}/gone.asi").into();
        let err = fingerprint(&src, &mut HashCache::default(), &spec).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let mut folder = specs()[0].clone();
        folder.home = "GAME/Mods/modGone".into();
        assert!(fingerprint(&src, &mut HashCache::default(), &folder).is_err());
    }

    #[test]
    fn qualified_sections_keep_a_shared_key_apart() {
        let mut src = game();
        src.put(
            format!("{BIN}/ReShade.ini"),
            "[renodx]\nMode=1\n[renodx-preset1]\nMode=2\nPeak=1000\n[W3Perf]\nMode=3\n",
        );
        let mut spec = specs()[2].clone();
        spec.config = ["renodx", "renodx-preset1", "W3Perf"]
            .map(|s| ConfigSource {
                qualify: s != "W3Perf",
                ..source(&format!("{BIN}/ReShade.ini"), Some(s), &[])
            })
            .into();
        let e = fingerprint(&src, &mut HashCache::default(), &spec).unwrap();
        let want = [
            ("Mode", "3"),
            ("renodx-preset1.Mode", "2"),
            ("renodx-preset1.Peak", "1000"),
            ("renodx.Mode", "1"),
        ];
        assert_eq!(e.config_values, values(&want));
    }

    #[test]
    fn set_hash_refuses_repeated_identities_and_bad_names() {
        let entries = fingerprints(&game(), &mut HashCache::default());
        let mut same_name = entries[1].clone();
        same_name.kind = Kind::Addon;
        let mut set = entries.clone();
        set.push(same_name);
        assert!(set_hash(&set).is_ok(), "one name, two kinds");
        set.push(entries[1].clone());
        assert_eq!(
            set_hash(&set),
            Err(SetError::Repeated("asi:W3ElementMap".into()))
        );
        for bad in ["", "W3\tPerf", "W3Perf\n"] {
            let mut e = entries[2].clone();
            e.name = bad.into();
            assert_eq!(set_hash(&[e]), Err(SetError::BadName(bad.into())));
        }
        let mut parked = entries.clone();
        parked[0].parked = true;
        assert_ne!(set_hash(&parked), set_hash(&entries));
        let mut moved = entries.clone();
        moved[0].home = "PARK/Mods/modAlpha".into();
        assert_eq!(set_hash(&moved), set_hash(&entries), "homes aren't hashed");
    }

    #[test]
    fn disk_fingerprints_a_real_folder_as_the_memory_source_does() {
        let root = std::env::temp_dir().join(format!("quen-mods-disk-{}", std::process::id()));
        let mem = game();
        for (path, data, _) in &mem.files {
            if let Ok(rel) = path.strip_prefix("GAME/Mods/modAlpha") {
                let path = root.join(rel);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, data).unwrap();
            }
        }
        let mut spec = specs()[0].clone();
        let want = fingerprint(&mem, &mut HashCache::default(), &spec).unwrap();
        spec.home = root.clone();
        spec.config[0].path = root.join("alpha.ini");
        let got = fingerprint(&Disk, &mut HashCache::default(), &spec).unwrap();
        std::fs::remove_dir_all(&root).unwrap(); // this test's own temp folder
        assert_eq!(
            (got.content_hash, got.file_count, got.config_hash),
            (want.content_hash, want.file_count, want.config_hash)
        );
    }

    #[test]
    fn kinds_have_one_spelling() {
        for &kind in Kind::ALL {
            let json = serde_json::to_value(kind).unwrap();
            assert_eq!(json, kind.as_str());
            assert_eq!(serde_json::from_value::<Kind>(json).unwrap(), kind);
        }
        let entry: ModEntry = serde_json::from_value(serde_json::json!({
            "kind": "reshade", "name": "ReShade", "home": "GAME/bin/x64/dxgi.dll", "content_hash": sha256_hex(b""),
            "file_count": 1, "bytes": 0
        }))
        .unwrap();
        assert_eq!(
            (entry.id(), entry.parked, entry.config_hash),
            ("reshade:ReShade".into(), false, None)
        );
        assert!(
            serde_json::from_value::<ModEntry>(serde_json::json!({
                "kind": "mod", "name": "x", "home": "x", "content_hash": "", "file_count": 0, "bytes": 0
            }))
            .is_err()
        );
    }
}
