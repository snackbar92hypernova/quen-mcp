//! Where a game is installed, found rather than asked: Steam's library folders and app manifests, and GOG's
//! registry entries (the Windows layer reads those and passes them in). Every find is a candidate: with two (a 5.01
//! and a 4.04 copy, say), the agent asks the user which one. quen never picks.
//!
//! The tools quen uses beside the game are found the same way, where their installers put them ([`installed`]).
//!
//! GOG installs are untested: GOG's exe may differ from Steam's, and mods built against Steam's are reported
//! not to work there. Every GOG candidate says so, so the user hears it before picking one.

use std::fs;
use std::path::{Path, PathBuf};

/// An install found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub dir: PathBuf,
    /// `steam` or `gog`.
    pub platform: &'static str,
    /// The store's own build or version: Steam's `buildid`, GOG's `ver`.
    pub build: Option<String>,
}

impl Install {
    pub fn status(&self) -> &'static str {
        status(self.platform)
    }
}

/// `tested` for Steam, the platform quen is verified on; `untested` for any other.
pub fn status(platform: &str) -> &'static str {
    if platform == "steam" {
        "tested"
    } else {
        "untested"
    }
}

/// A Valve KeyValues text node (`libraryfolders.vdf`, `appmanifest_*.acf`): a string, or a block of keyed nodes.
#[derive(Debug, PartialEq)]
pub enum Vdf {
    Str(String),
    Block(Vec<(String, Vdf)>),
}

impl Vdf {
    /// The whole file as one block of its top-level pairs; `None` if it isn't KeyValues text.
    pub fn parse(text: &str) -> Option<Vdf> {
        let mut tokens = tokens(text)?.into_iter();
        let block = block(&mut tokens, false)?;
        Some(Vdf::Block(block))
    }

    /// The first value under `key` (case-insensitive, as Steam reads them).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Block(pairs) => pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Block(_) => None,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Open,
    Close,
    Str(String),
}

/// Quoted strings (with `\\`, `\"`, `\n`, `\t`), bare words, braces and `//` comments.
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => out.push(Token::Open),
            '}' => out.push(Token::Close),
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => s.push(match chars.next()? {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        }),
                        other => s.push(other),
                    }
                }
                out.push(Token::Str(s));
            }
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|&c| c != '\n').is_some() {},
            c if c.is_whitespace() => {}
            c => {
                let mut s = String::from(c);
                while let Some(c) = chars.next_if(|c| !c.is_whitespace() && !"{}\"".contains(*c)) {
                    s.push(c);
                }
                out.push(Token::Str(s));
            }
        }
    }
    Some(out)
}

/// Key-value pairs up to the closing brace (`inner`) or the end of the file.
fn block(tokens: &mut impl Iterator<Item = Token>, inner: bool) -> Option<Vec<(String, Vdf)>> {
    let mut pairs = Vec::new();
    loop {
        let key = match tokens.next() {
            Some(Token::Str(key)) => key,
            Some(Token::Close) if inner => return Some(pairs),
            None if !inner => return Some(pairs),
            _ => return None,
        };
        let value = match tokens.next()? {
            Token::Str(s) => Vdf::Str(s),
            Token::Open => Vdf::Block(block(tokens, true)?),
            Token::Close => return None,
        };
        pairs.push((key, value));
    }
}

/// Steam's libraries: each one `steamapps\libraryfolders.vdf` lists, in either format (a block with a `path` per
/// library, or before 2021 a path per number), then Steam's own folder, once each. The file's spelling comes first:
/// the registry's is lowercase with forward slashes.
pub fn steam_libraries(steam: &Path) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    let vdf = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf"));
    let vdf = vdf.ok().and_then(|text| Vdf::parse(&text));
    let root = vdf.as_ref().and_then(|v| v.get("libraryfolders"));
    if let Some(Vdf::Block(entries)) = root {
        for (_, entry) in entries {
            let path = entry.str().or_else(|| entry.get("path")?.str());
            // Old files also hold keys like "TimeNextStatsReport": numbers, not paths.
            if let Some(path) = path.filter(|p| p.contains(['\\', '/'])) {
                libraries.push(PathBuf::from(path));
            }
        }
    }
    libraries.push(steam.to_owned());
    dedup(libraries, |p| p)
}

/// Where Steam installed `appid`: each library holding `steamapps\appmanifest_<appid>.acf`, as
/// `steamapps\common\<installdir>`, with the manifest's `buildid`.
pub fn steam(steam: &Path, appid: &str) -> Vec<Install> {
    let found = steam_libraries(steam).into_iter().filter_map(|library| {
        let apps = library.join("steamapps");
        let (dir, build) = manifest(&apps, appid)?;
        Some(Install {
            dir: apps.join("common").join(dir),
            platform: "steam",
            build,
        })
    });
    found.collect()
}

/// Steam's `buildid` for the install in `game_dir` (`<library>\steamapps\common\<installdir>`), from the app manifest
/// beside it, whose `installdir` must name that folder: a second sign of the game's build, independent of the exe's
/// sha256. It is the build of Steam's whole download, so it also changes with an update that leaves the exe
/// alone. `None` when no manifest there names the folder (a GOG install, a copied folder) or it has no `buildid`.
pub fn steam_buildid(game_dir: &Path, appid: &str) -> Option<String> {
    let common = game_dir.parent()?;
    if !common.file_name()?.eq_ignore_ascii_case("common") {
        return None;
    }
    let (dir, build) = manifest(common.parent()?, appid)?;
    game_dir
        .file_name()?
        .eq_ignore_ascii_case(dir)
        .then_some(build)?
}

/// The `installdir` and `buildid` of `appid`'s manifest in the `steamapps` folder `apps`; `None` if there is none, it
/// isn't KeyValues text, or it names no install folder.
fn manifest(apps: &Path, appid: &str) -> Option<(String, Option<String>)> {
    let text = fs::read_to_string(apps.join(format!("appmanifest_{appid}.acf"))).ok()?;
    let vdf = Vdf::parse(&text)?;
    let state = vdf.get("AppState")?;
    let field = |key| Some(state.get(key)?.str()?.to_owned());
    let dir = field("installdir").filter(|d| !d.is_empty())?;
    Some((dir, field("buildid")))
}

/// Every install: Steam's for `appid` (if Steam was found) and the GOG entries, kept when `is_game` says the folder
/// holds the game, once each (by path, case-insensitively).
pub fn installs(
    steam_dir: Option<&Path>,
    appid: &str,
    gog: Vec<Install>,
    is_game: impl Fn(&Path) -> bool,
) -> Vec<Install> {
    let mut all = steam_dir.map(|s| steam(s, appid)).unwrap_or_default();
    all.extend(gog);
    all.retain(|i| is_game(&i.dir));
    dedup(all, |i| &i.dir)
}

/// Every file at `pattern` under `root`, sorted: names joined by `\`, where a name holding `*` matches each name with
/// that prefix and suffix (case aside, as Windows compares them). Where an installer puts a tool, one folder per
/// version: each version is listed, so the user picks one (quen never does). Read-only.
pub fn installed(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found = vec![root.to_owned()];
    for part in pattern.split('\\').filter(|p| !p.is_empty()) {
        found = match part.to_lowercase().split_once('*') {
            None => found.into_iter().map(|dir| dir.join(part)).collect(),
            Some((prefix, suffix)) => {
                let fits = |name: &str| {
                    let name = name.to_lowercase();
                    name.len() >= prefix.len() + suffix.len()
                        && name.starts_with(prefix)
                        && name.ends_with(suffix)
                };
                let entries = found
                    .iter()
                    .flat_map(|dir| fs::read_dir(dir).into_iter().flatten());
                entries
                    .flatten()
                    .filter(|e| fits(&e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .collect()
            }
        };
    }
    found.retain(|p| p.is_file());
    found.sort();
    found
}

/// The first of each path, compared case-insensitively and with either slash.
fn dedup<T>(items: Vec<T>, path: impl Fn(&T) -> &Path) -> Vec<T> {
    let key = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        s.trim_end_matches('\\').to_owned()
    };
    let mut seen = Vec::new();
    items
        .into_iter()
        .filter(|item| {
            let k = key(path(item));
            let new = !seen.contains(&k);
            seen.push(k);
            new
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder for one test, gone afterwards (only what the test made in it).
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir =
                std::env::temp_dir().join(format!("quen-detect-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn put(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // this test's own temp folder
        }
    }

    /// `path` as a vdf string: backslashes doubled.
    fn esc(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }

    fn manifest(appid: u32, dir: &str, build: &str) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"Synthetic\"\n\t\"installdir\"\t\t\"{dir}\"\n\
             \t\"buildid\"\t\t\"{build}\"\n\t\"InstalledDepots\"\n\t{{\n\t\t\"1\"\n\t\t{{\n\t\t\t\"manifest\"\t\t\"7\"\n\
             \t\t}}\n\t}}\n}}\n"
        )
    }

    #[test]
    fn parses_keyvalues_with_escapes_comments_and_bare_words() {
        let v = Vdf::parse("// c\n\"a\" { \"b\\\\c\" \"d\\\"e\" bare word \"n\" { } }").unwrap();
        let a = v.get("A").unwrap();
        assert_eq!(a.get("b\\c").and_then(Vdf::str), Some("d\"e"));
        assert_eq!(a.get("bare").and_then(Vdf::str), Some("word"));
        assert_eq!(a.get("n"), Some(&Vdf::Block(vec![])));
        for bad in ["\"a\" {", "\"a\" }", "\"a\"", "\"a\" \"b"] {
            assert_eq!(Vdf::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn finds_the_game_in_every_steam_library_and_on_gog() {
        let t = Temp::new("steam");
        let steam = t.0.join("Steam");
        let lib2 = t.0.join("Library Two");
        let lib3 = t.0.join("lib3");
        // The current format, which lists the Steam folder itself too.
        let vdf = format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\"\n\t\t{{\n\t\t\t\"10\"\t\t\"1\"\n\
             \t\t}}\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"2\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            esc(&steam),
            esc(&lib2),
            esc(&lib3),
        );
        t.put("Steam/steamapps/libraryfolders.vdf", &vdf);
        assert_eq!(
            steam_libraries(&steam),
            vec![steam.clone(), lib2.clone(), lib3.clone()]
        );
        if cfg!(windows) {
            // As the registry spells it: the file's spelling wins.
            let registry = steam.to_string_lossy().to_lowercase().replace('\\', "/");
            let registry = PathBuf::from(registry);
            assert_eq!(steam_libraries(&registry)[0], steam);
        }
        t.put(
            "Steam/steamapps/appmanifest_10.acf",
            &manifest(10, "Game A", "111"),
        );
        t.put(
            "Library Two/steamapps/appmanifest_10.acf",
            &manifest(10, "Game B", "222"),
        );
        t.put(
            "Library Two/steamapps/appmanifest_20.acf",
            &manifest(20, "Other", "9"),
        );
        t.put("lib3/steamapps/appmanifest_10.acf", "not keyvalues {");
        let a = steam.join("steamapps").join("common").join("Game A");
        let b = lib2.join("steamapps").join("common").join("Game B");
        let gog = t.0.join("GOG Games").join("Game");
        t.put("Steam/steamapps/common/Game A/game.exe", "");
        t.put("Library Two/steamapps/common/Game B/game.exe", "");
        t.put("GOG Games/Game/game.exe", "");
        let gogs = vec![
            Install {
                dir: gog.clone(),
                platform: "gog",
                build: Some("4.04".into()),
            },
            // The same folder twice in the registry, and one that holds no game.
            Install {
                dir: PathBuf::from(gog.to_string_lossy().to_uppercase()),
                platform: "gog",
                build: None,
            },
            Install {
                dir: t.0.join("empty"),
                platform: "gog",
                build: None,
            },
        ];
        let found = installs(Some(&steam), "10", gogs, |d| d.join("game.exe").is_file());
        let want = [
            (a, "steam", Some("111")),
            (b, "steam", Some("222")),
            (gog, "gog", Some("4.04")),
        ];
        let got: Vec<_> = found
            .iter()
            .map(|i| (i.dir.clone(), i.platform, i.build.as_deref()))
            .collect();
        assert_eq!(got, want);
        let status: Vec<&str> = found.iter().map(Install::status).collect();
        assert_eq!(status, ["tested", "tested", "untested"], "GOG is untested");
        // Each install's buildid from the manifest beside it, also without Steam's library list.
        assert_eq!(steam_buildid(&found[0].dir, "10").as_deref(), Some("111"));
        assert_eq!(steam_buildid(&found[1].dir, "10").as_deref(), Some("222"));
        assert_eq!(
            steam_buildid(&found[2].dir, "10"),
            None,
            "GOG has no Steam buildid"
        );
        assert_eq!(
            steam_buildid(&found[1].dir, "20"),
            None,
            "app 20's manifest names Other"
        );
        assert_eq!(
            steam_buildid(&lib3.join("steamapps").join("common").join("Game"), "10"),
            None
        );
        assert!(installs(Some(&steam), "30", vec![], |_| true).is_empty());
        assert!(installs(None, "10", vec![], |_| true).is_empty());
    }

    #[test]
    fn the_buildid_is_only_the_manifest_s_that_names_the_folder() {
        let t = Temp::new("buildid");
        let apps = t.0.join("lib").join("steamapps");
        t.put(
            "lib/steamapps/appmanifest_10.acf",
            &manifest(10, "Game A", "333"),
        );
        let common = apps.join("common");
        assert_eq!(
            steam_buildid(&common.join("Game A"), "10").as_deref(),
            Some("333")
        );
        // The folder's name, compared as Steam does: case aside.
        assert_eq!(
            steam_buildid(&common.join("GAME a"), "10").as_deref(),
            Some("333")
        );
        assert_eq!(
            steam_buildid(&common.join("Game B"), "10"),
            None,
            "another folder"
        );
        assert_eq!(
            steam_buildid(&apps.join("other").join("Game A"), "10"),
            None,
            "not in common"
        );
        assert_eq!(steam_buildid(Path::new("Game A"), "10"), None);
        t.put(
            "lib/steamapps/appmanifest_20.acf",
            "\"AppState\"\n{\n\t\"installdir\"\t\t\"Game C\"\n}\n",
        );
        assert_eq!(
            steam_buildid(&common.join("Game C"), "20"),
            None,
            "no buildid"
        );
    }

    #[test]
    fn every_installed_version_is_listed_and_nothing_else() {
        let t = Temp::new("installed");
        let nsys = r"NVIDIA Corporation\Nsight Systems*\target-windows-x64\nsys.exe";
        let old = t.put(
            "NVIDIA Corporation/Nsight Systems 2025.1.1/target-windows-x64/nsys.exe",
            "",
        );
        // A `*` name matches case aside, on any file system.
        let new = t.put(
            "NVIDIA Corporation/nsight systems 2025.3.2/target-windows-x64/nsys.exe",
            "",
        );
        // Another tool's folder, a version without the exe, and a folder where the exe should be.
        t.put(
            "NVIDIA Corporation/Nsight Graphics 2025.2/host/ngfx.exe",
            "",
        );
        t.put("NVIDIA Corporation/Nsight Systems 2024.1/readme.txt", "");
        let dir = "NVIDIA Corporation/Nsight Systems 9/target-windows-x64/nsys.exe";
        fs::create_dir_all(t.0.join(dir)).unwrap();
        assert_eq!(installed(&t.0, nsys), vec![old, new]);
        let pm = t.put("Intel/PresentMon/App/PresentMon-2.4.1-x64.exe", "");
        t.put("Intel/PresentMon/App/PresentMonService.exe", "");
        let pattern = r"Intel\PresentMon\App\PresentMon-*.exe";
        assert_eq!(installed(&t.0, pattern), vec![pm]);
        assert!(installed(&t.0.join("missing"), nsys).is_empty());
    }

    #[test]
    fn reads_the_old_libraryfolders_format_and_no_file_at_all() {
        let t = Temp::new("old");
        let steam = t.0.join("Steam");
        assert_eq!(steam_libraries(&steam), vec![steam.clone()]);
        let old = format!(
            "\"LibraryFolders\"\n{{\n\t\"TimeNextStatsReport\"\t\t\"1234\"\n\t\"ContentStatsID\"\t\t\"-5\"\n\
             \t\"1\"\t\t\"{}\"\n}}\n",
            esc(&t.0.join("D")),
        );
        t.put("Steam/steamapps/libraryfolders.vdf", &old);
        assert_eq!(steam_libraries(&steam), vec![t.0.join("D"), steam.clone()]);
    }
}
