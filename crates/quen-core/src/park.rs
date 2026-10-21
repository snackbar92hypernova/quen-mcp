//! The park planner. It decides which mods to move out of the game's way for a boot, from folder listings only
//! (no file-system access here), and refuses the whole plan on any doubt.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The Witcher 3 names that are never parked. Absolute: naming one is refused, and `ALL` skips them. The adapter
/// adds its RenoDX add-on unless asked, and the devkit when it is on. Each script mod here stays with the ASI whose
/// functions it imports: parking one half of a pair fails the game's script compile.
pub const W3_NEVER_PARK: &[&str] = &[
    // The Native Bridge, which quen's ASI registers its script functions through.
    "WitcherNativeBridge.asi",
    "modNativeBridge",
    // quen's own package.
    "W3Quen.asi",
    "modQuen",
];

/// The Witcher 3 pairs parked both or neither: a script mod and the ASI it imports functions from, so one half
/// alone fails the game's script compile. A request naming one half without the other is refused, and `ALL` takes
/// neither unless both are named. The W3 Game State Bridge is RenoDX's: quen reads nothing of it, so a
/// test may park it to show quen runs without it.
pub const W3_PAIRS: &[[&str; 2]] = &[["W3GameStateBridge.asi", "modW3GameStateBridge"]];

/// The request word for every parkable mod.
const ALL: &str = "ALL";

/// One entry of a folder listing, as the caller read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// A symbolic link or junction: never parked.
    pub is_link: bool,
}

/// A folder and what it holds.
#[derive(Debug, Clone, Copy)]
pub struct Listing<'a> {
    pub dir: &'a Path,
    pub entries: &'a [Entry],
}

/// One rename, from the game's folders into the parking folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What to park, in order. Write `marker` first, listing every move (this plan, serialized), and make the moves only
/// after it is on disk; journal each move as it is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkPlan {
    pub marker: PathBuf,
    pub moves: Vec<Move>,
}

/// Why a plan is refused. Nothing is parked when one comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkError {
    /// A name to park, requested or taken by `ALL`, that is not `\w[\w.-]*` with an ASCII `\w` (see [`plain`]).
    BadName(String),
    /// Not in the folder its name says it lives in: bin for `.asi`, `.addon64` and `dxgi.dll`, else Mods.
    Unknown(String),
    /// A symbolic link or junction.
    Link(String),
    /// In Mods but not a folder, or in bin but a folder.
    WrongKind(String),
    /// Named twice (names compare case-insensitively).
    Repeated(String),
    /// On the never-park list.
    NeverPark(String),
    /// Both requested and on the keep list.
    Kept(String),
    /// One half of a pair requested without the other: (the half named, the other half).
    Half(String, String),
}

impl fmt::Display for ParkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName(name) => write!(f, "{name:?} is not a mod name (ASCII \\w[\\w.-]*)"),
            Self::Unknown(name) => {
                write!(f, "no {name:?} in the {} folder", Home::of(name).folder())
            }
            Self::Link(name) => write!(f, "{name:?} is a link or junction, never parked"),
            Self::WrongKind(name) => match Home::of(name) {
                Home::Mods => write!(f, "{name:?} in Mods is not a folder"),
                Home::Bin => write!(f, "{name:?} in bin is a folder"),
            },
            Self::Repeated(name) => write!(f, "{name:?} is named twice"),
            Self::NeverPark(name) => write!(f, "{name:?} is never parked"),
            Self::Kept(name) => write!(f, "{name:?} is both requested and kept"),
            Self::Half(name, other) => write!(
                f,
                "{name:?} is parked only together with {other:?}: one half alone fails the game's script compile, \
                 so name both or neither"
            ),
        }
    }
}

impl std::error::Error for ParkError {}

/// Where a name lives: bin for `.asi`, `.addon64` and `dxgi.dll` (any case), else the Mods folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Home {
    Mods,
    Bin,
}

impl Home {
    fn of(name: &str) -> Self {
        let name = name.to_lowercase();
        if name.ends_with(".asi") || name.ends_with(".addon64") || name == "dxgi.dll" {
            Self::Bin
        } else {
            Self::Mods
        }
    }

    fn folder(self) -> &'static str {
        match self {
            Self::Mods => "Mods",
            Self::Bin => "bin",
        }
    }
}

/// Plans the moves for `requested`, a comma list of names and `ALL`, matched case-insensitively to the on-disk
/// spelling. `ALL` is every Mods folder and every `.asi` / `.addon64` / `dxgi.dll` in bin that is not a link, minus
/// `keep` (a comma list), `never_park`, both halves of each of `pairs` and the names listed explicitly, which keep
/// their own place in the order. Refuses the whole plan on a bad, unknown, repeated or never-park name, a link, a name
/// both requested and kept, one half of a pair named without the other, and when `ALL` would take a name that isn't
/// [`plain`]. Keep names only have to exist: they never move.
/// Every move goes to `<parking>/Mods/<name>` or `<parking>/bin/<name>`.
pub fn plan(
    requested: &str,
    keep: &str,
    never_park: &[&str],
    pairs: &[[&str; 2]],
    mods: Listing,
    bin: Listing,
    parking: &Path,
) -> Result<ParkPlan, ParkError> {
    let requested: Vec<&str> = names(requested).collect();
    let keep: Vec<&str> = names(keep).collect();
    for name in &keep {
        find(name, mods, bin)?;
    }
    for &[a, b] in pairs {
        match (listed(&requested, a), listed(&requested, b)) {
            (true, false) => return Err(ParkError::Half(a.into(), b.into())),
            (false, true) => return Err(ParkError::Half(b.into(), a.into())),
            _ => {}
        }
    }
    let paired: Vec<&str> = pairs.iter().flatten().copied().collect();
    let to = |listing: Listing, home: Home, entry: &Entry| Move {
        from: listing.dir.join(&entry.name),
        to: parking.join(home.folder()).join(&entry.name),
    };
    let mut moves = Vec::new();
    for (i, &name) in requested.iter().enumerate() {
        if listed(&requested[..i], name) {
            return Err(ParkError::Repeated(name.into()));
        }
        if name == ALL {
            for (listing, home) in [(mods, Home::Mods), (bin, Home::Bin)] {
                let mut all: Vec<&Entry> = listing
                    .entries
                    .iter()
                    .filter(|e| {
                        !e.is_link && e.is_dir == (home == Home::Mods) && Home::of(&e.name) == home
                    })
                    .filter(|e| {
                        ![&keep[..], never_park, &paired[..], &requested[..]]
                            .iter()
                            .any(|list| listed(list, &e.name))
                    })
                    .collect();
                all.sort_by_cached_key(|e| e.name.to_lowercase());
                if let Some(e) = all.iter().find(|e| !plain(&e.name)) {
                    return Err(ParkError::BadName(e.name.clone()));
                }
                moves.extend(all.into_iter().map(|e| to(listing, home, e)));
            }
            continue;
        }
        if listed(never_park, name) {
            return Err(ParkError::NeverPark(name.into()));
        }
        if listed(&keep, name) {
            return Err(ParkError::Kept(name.into()));
        }
        if !plain(name) {
            return Err(ParkError::BadName(name.into()));
        }
        let (listing, entry) = find(name, mods, bin)?;
        let home = Home::of(name);
        if entry.is_link {
            return Err(ParkError::Link(name.into()));
        }
        if entry.is_dir != (home == Home::Mods) {
            return Err(ParkError::WrongKind(name.into()));
        }
        moves.push(to(listing, home, entry));
    }
    Ok(ParkPlan {
        marker: parking.join("quen-park.json"),
        moves,
    })
}

/// Whether `name` is one of the bin kinds quen parks: an `.asi`, an `.addon64` or `dxgi.dll` (any case).
pub fn bin_kind(name: &str) -> bool {
    Home::of(name) == Home::Bin
}

/// The non-empty items of a comma list, trimmed (a name can't hold spaces).
fn names(list: &str) -> impl Iterator<Item = &str> {
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn listed(list: &[&str], name: &str) -> bool {
    let name = name.to_lowercase();
    list.iter().any(|n| n.to_lowercase() == name)
}

/// `\w[\w.-]*` with an ASCII `\w`: the harness's `PLAIN` (`re.A`), which its unpark checks again. A name the harness
/// refuses is refused here before anything moves, so quen's check never passes what the harness then refuses.
fn plain(name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut chars = name.chars();
    chars.next().is_some_and(word) && chars.all(|c| word(c) || c == '.' || c == '-')
}

/// The listing a name lives in and its entry there.
fn find<'a>(
    name: &str,
    mods: Listing<'a>,
    bin: Listing<'a>,
) -> Result<(Listing<'a>, &'a Entry), ParkError> {
    let listing = match Home::of(name) {
        Home::Mods => mods,
        Home::Bin => bin,
    };
    let lower = name.to_lowercase();
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name.to_lowercase() == lower);
    entry
        .map(|e| (listing, e))
        .ok_or_else(|| ParkError::Unknown(name.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.into(),
            is_dir: true,
            is_link: false,
        }
    }

    fn file(name: &str) -> Entry {
        Entry {
            name: name.into(),
            is_dir: false,
            is_link: false,
        }
    }

    fn link(name: &str) -> Entry {
        Entry {
            name: name.into(),
            is_dir: true,
            is_link: true,
        }
    }

    /// A synthetic game: mods (one a junction, one a stray file) and bin (ASIs, an add-on, ReShade's dxgi.dll).
    fn game() -> (Vec<Entry>, Vec<Entry>) {
        let mods = vec![
            dir("modZeta"),
            dir("modAlpha"),
            link("modLinked"),
            file("readme.txt"),
            dir("modQuen"),
            dir("modW3GameStateBridge"),
        ];
        let bin = vec![
            file("witcher3.exe"),
            file("Tweaks.asi"),
            file("W3GameStateBridge.asi"),
            file("renodx.ADDON64"),
            file("dxgi.dll"),
            file("d3d12.dll"),
            dir("plugins.asi"),
        ];
        (mods, bin)
    }

    fn run(requested: &str, keep: &str) -> Result<Vec<(String, String)>, ParkError> {
        let (mods, bin) = game();
        run_in(&mods, &bin, requested, keep)
    }

    fn run_in(
        mods: &[Entry],
        bin: &[Entry],
        requested: &str,
        keep: &str,
    ) -> Result<Vec<(String, String)>, ParkError> {
        let mods = Listing {
            dir: Path::new("GAME/Mods"),
            entries: mods,
        };
        let bin = Listing {
            dir: Path::new("GAME/bin/x64"),
            entries: bin,
        };
        let plan = plan(
            requested,
            keep,
            W3_NEVER_PARK,
            W3_PAIRS,
            mods,
            bin,
            Path::new("PARK"),
        )?;
        assert_eq!(plan.marker, Path::new("PARK/quen-park.json"));
        let show = |p: &Path| p.to_str().unwrap().replace('\\', "/");
        Ok(plan
            .moves
            .iter()
            .map(|m| (show(&m.from), show(&m.to)))
            .collect())
    }

    fn pair(from: &str, to: &str) -> (String, String) {
        (from.into(), to.into())
    }

    #[test]
    fn explicit_names_use_the_on_disk_spelling_and_home() {
        let moves = run(" modalpha , TWEAKS.ASI,RenoDX.addon64,DXGI.DLL", "").unwrap();
        let want = [
            pair("GAME/Mods/modAlpha", "PARK/Mods/modAlpha"),
            pair("GAME/bin/x64/Tweaks.asi", "PARK/bin/Tweaks.asi"),
            pair("GAME/bin/x64/renodx.ADDON64", "PARK/bin/renodx.ADDON64"),
            pair("GAME/bin/x64/dxgi.dll", "PARK/bin/dxgi.dll"),
        ];
        assert_eq!(moves, want);
    }

    #[test]
    fn all_takes_every_parkable_mod_but_never_park_and_keep() {
        let moves = run("ALL", "").unwrap();
        let want = [
            pair("GAME/Mods/modAlpha", "PARK/Mods/modAlpha"),
            pair("GAME/Mods/modZeta", "PARK/Mods/modZeta"),
            pair("GAME/bin/x64/dxgi.dll", "PARK/bin/dxgi.dll"),
            pair("GAME/bin/x64/renodx.ADDON64", "PARK/bin/renodx.ADDON64"),
            pair("GAME/bin/x64/Tweaks.asi", "PARK/bin/Tweaks.asi"),
        ];
        assert_eq!(moves, want);
        let moves = run("ALL", "MODZETA, dxgi.dll, modQuen").unwrap();
        assert_eq!(moves, [want[0].clone(), want[3].clone(), want[4].clone()]);
    }

    #[test]
    fn explicit_names_beside_all_keep_their_place_once() {
        let moves = run("tweaks.asi,ALL,modZeta", "modAlpha").unwrap();
        let want = [
            pair("GAME/bin/x64/Tweaks.asi", "PARK/bin/Tweaks.asi"),
            pair("GAME/bin/x64/dxgi.dll", "PARK/bin/dxgi.dll"),
            pair("GAME/bin/x64/renodx.ADDON64", "PARK/bin/renodx.ADDON64"),
            pair("GAME/Mods/modZeta", "PARK/Mods/modZeta"),
        ];
        assert_eq!(moves, want);
    }

    #[test]
    fn never_park_is_absolute() {
        for name in ["modQuen", "witchernativebridge.asi", "W3Quen.asi"] {
            // Refused whether installed or not, and also beside ALL.
            assert_eq!(
                run(&format!("modAlpha,{name}"), ""),
                Err(ParkError::NeverPark(name.into()))
            );
            assert_eq!(
                run(&format!("ALL,{name}"), ""),
                Err(ParkError::NeverPark(name.into()))
            );
        }
        assert_eq!(W3_NEVER_PARK.len(), 4);
    }

    #[test]
    fn a_pair_is_parked_both_or_neither() {
        let both = [
            pair(
                "GAME/Mods/modW3GameStateBridge",
                "PARK/Mods/modW3GameStateBridge",
            ),
            pair(
                "GAME/bin/x64/W3GameStateBridge.asi",
                "PARK/bin/W3GameStateBridge.asi",
            ),
        ];
        // Named together, in any case and order: both move, each in its place in the request.
        assert_eq!(
            run("MODW3GAMESTATEBRIDGE,w3gamestatebridge.asi", "").unwrap(),
            both
        );
        let swapped = run("W3GameStateBridge.asi,modAlpha,modW3GameStateBridge", "").unwrap();
        assert_eq!(
            swapped,
            [
                both[1].clone(),
                pair("GAME/Mods/modAlpha", "PARK/Mods/modAlpha"),
                both[0].clone()
            ]
        );
        // One half alone is refused, naming the other, also beside ALL and with the other half kept.
        let asi = ParkError::Half(
            "W3GameStateBridge.asi".into(),
            "modW3GameStateBridge".into(),
        );
        let script = ParkError::Half(
            "modW3GameStateBridge".into(),
            "W3GameStateBridge.asi".into(),
        );
        assert_eq!(run("modAlpha,w3gamestatebridge.asi", ""), Err(asi.clone()));
        assert_eq!(run("ALL,modW3GameStateBridge", ""), Err(script.clone()));
        assert_eq!(
            run("ALL,W3GameStateBridge.asi", "modW3GameStateBridge"),
            Err(asi.clone())
        );
        assert!(
            script.to_string().contains(
                "\"modW3GameStateBridge\" is parked only together with \"W3GameStateBridge.asi\""
            ),
            "{script}"
        );
        // ALL takes neither half, whatever keep says, unless both are named.
        let all = run("ALL", "").unwrap();
        assert!(
            !all.iter().any(|m| m.0.contains("GameStateBridge")),
            "{all:?}"
        );
        assert_eq!(run("ALL", "modW3GameStateBridge").unwrap(), all);
        assert_eq!(run("ALL", "MODZETA").unwrap().len(), all.len() - 1);
        let mut with_pair = run("ALL,modW3GameStateBridge,W3GameStateBridge.asi", "").unwrap();
        assert_eq!(with_pair.split_off(all.len()), both);
        assert_eq!(with_pair, all);
        // Both named: each half is still checked on its own.
        assert_eq!(
            run(
                "modW3GameStateBridge,W3GameStateBridge.asi",
                "modW3GameStateBridge"
            ),
            Err(ParkError::Kept("modW3GameStateBridge".into()))
        );
        let (mods, bin) = game();
        let bin: Vec<Entry> = bin
            .into_iter()
            .filter(|e| e.name != "W3GameStateBridge.asi")
            .collect();
        assert_eq!(
            run_in(
                &mods,
                &bin,
                "modW3GameStateBridge,W3GameStateBridge.asi",
                ""
            ),
            Err(ParkError::Unknown("W3GameStateBridge.asi".into()))
        );
    }

    #[test]
    fn refusals_name_the_culprit() {
        let cases = [
            (
                "modAlpha,modMissing",
                ParkError::Unknown("modMissing".into()),
            ),
            ("Missing.asi", ParkError::Unknown("Missing.asi".into())),
            ("modLinked", ParkError::Link("modLinked".into())),
            ("readme.txt", ParkError::WrongKind("readme.txt".into())),
            ("plugins.asi", ParkError::WrongKind("plugins.asi".into())),
            // Only `.asi`, `.addon64` and `dxgi.dll` live in bin: anything else is looked up in Mods.
            ("witcher3.exe", ParkError::Unknown("witcher3.exe".into())),
            ("modAlpha,MODALPHA", ParkError::Repeated("MODALPHA".into())),
            ("ALL,ALL", ParkError::Repeated("ALL".into())),
            ("..", ParkError::BadName("..".into())),
            ("../modAlpha", ParkError::BadName("../modAlpha".into())),
            ("mod Alpha", ParkError::BadName("mod Alpha".into())),
            (".hidden", ParkError::BadName(".hidden".into())),
            ("-x", ParkError::BadName("-x".into())),
            ("all", ParkError::Unknown("all".into())),
        ];
        for (requested, want) in cases {
            let got = run(requested, "");
            assert_eq!(got, Err(want.clone()), "{requested}");
            assert!(
                want.to_string()
                    .contains(&format!("{:?}", requested.rsplit(',').next().unwrap()))
            );
        }
    }

    #[test]
    fn names_the_harness_refuses_are_refused_before_anything_moves() {
        let (mut mods, mut bin) = game();
        mods.extend([dir("modBéta"), dir("mod Spaced")]);
        bin.push(file("My Tweaks.asi"));
        let run = |requested, keep| run_in(&mods, &bin, requested, keep);
        // Non-ASCII letters and spaces: the harness's names are ASCII \w[\w.-]*.
        assert_eq!(
            run("modbÉta", ""),
            Err(ParkError::BadName("modbÉta".into()))
        );
        // ALL refuses the whole plan, naming the first such folder or file it would take.
        assert_eq!(run("ALL", ""), Err(ParkError::BadName("mod Spaced".into())));
        assert_eq!(
            run("ALL", "mod Spaced,My Tweaks.asi"),
            Err(ParkError::BadName("modBéta".into()))
        );
        // Kept, they never move, so ALL goes ahead.
        let moves = run("ALL", "mod Spaced, modbéta, my tweaks.asi").unwrap();
        assert_eq!(moves, self::run("ALL", "").unwrap());
    }

    #[test]
    fn keep_names_are_checked_too() {
        assert_eq!(
            run("ALL", "modTypo"),
            Err(ParkError::Unknown("modTypo".into()))
        );
        // A keep name only has to exist: it never moves.
        assert_eq!(
            run("ALL", "bad name"),
            Err(ParkError::Unknown("bad name".into()))
        );
        assert_eq!(
            run("modAlpha", "modalpha"),
            Err(ParkError::Kept("modAlpha".into()))
        );
        assert_eq!(
            run("modZeta", "modAlpha").unwrap(),
            [pair("GAME/Mods/modZeta", "PARK/Mods/modZeta")]
        );
    }

    #[test]
    fn nothing_requested_is_an_empty_plan() {
        assert_eq!(run("", "").unwrap(), []);
        assert_eq!(run(" , ", "").unwrap(), []);
    }

    #[test]
    fn plan_serializes_for_the_marker() {
        let mods = [dir("modAlpha")];
        let mods = Listing {
            dir: Path::new("M"),
            entries: &mods,
        };
        let bin = Listing {
            dir: Path::new("B"),
            entries: &[],
        };
        let plan = plan(
            "modAlpha",
            "",
            W3_NEVER_PARK,
            W3_PAIRS,
            mods,
            bin,
            Path::new("P"),
        )
        .unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        let back: ParkPlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
        assert!(text.contains("\"moves\":[{\"from\":"), "{text}");
    }
}
