//! The park planner. It decides which mods to move out of the game's way for a boot, from folder listings only
//! (no file-system access here), and refuses the whole plan on any doubt. A mod manager's mod (Vortex's, [`Managed`])
//! parks as a unit: everything of it the game loads. Which script folder needs which binary is learned from the files
//! ([`learn`]): a script folder and the binary its imports are in move both or neither, and `ALL` takes them both.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The Witcher 3 names that are never parked. Absolute: naming one is refused, and `ALL` skips them, with every unit
/// of a managed mod that holds one (the Native Bridge's loader with it). The adapter adds its RenoDX add-on unless
/// asked, and the devkit when it is on. Script imports these binaries hold pair with nothing: they never move.
pub const W3_NEVER_PARK: &[&str] = &[
    // The Native Bridge, which quen's ASI registers its script functions through.
    "WitcherNativeBridge.asi",
    "modNativeBridge",
    // quen's own package.
    "W3Quen.asi",
    "modQuen",
];

/// The pair `ALL` leaves loaded: the W3 Game State Bridge, which the RenoDX add-on reads the game's state from. Named
/// both, it parks; named one, it is refused, as any pair.
pub const W3_ALL_SKIPS: [&str; 2] = ["W3GameStateBridge.asi", "modW3GameStateBridge"];

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

/// Where a parkable item lives, and so its folder in quen's parking folder ([`Home::folder`]). A bare name lives in bin
/// when it is an `.asi`, an `.addon64` or `dxgi.dll` (any case), else in Mods; a managed mod's units also live in bin
/// whatever their kind, in the menu folder and in the DLC folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    Mods,
    Bin,
    Menu,
    Dlc,
}

impl Home {
    fn of(name: &str) -> Self {
        match bin_kind(name) {
            true => Self::Bin,
            false => Self::Mods,
        }
    }

    /// Its folder in the parking folder, and in messages.
    pub fn folder(self) -> &'static str {
        match self {
            Self::Mods => "Mods",
            Self::Bin => "bin",
            Self::Menu => "menu",
            Self::Dlc => "dlc",
        }
    }

    fn is_dir(self) -> bool {
        matches!(self, Self::Mods | Self::Dlc)
    }
}

/// A mod a mod manager deployed (one of Vortex's), as `park` and `keep` name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    /// How messages name it (`W3LoadFix 1.0.8 (nexus 13881)`).
    pub label: String,
    /// What names it, any case: `nexus:<id>`, its name, its manager's folder.
    pub names: Vec<String>,
    /// What of it the game loads: its Mods and DLC folders, its files in bin and the menu folder, by name there.
    pub units: Vec<(Home, String)>,
    /// Why it isn't parked as a unit.
    pub ties: Vec<Tie>,
}

/// Why a managed mod isn't parked as a unit, unless the mods `with` names (their labels) are parked as units beside it;
/// with none, never (a file quen doesn't park).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tie {
    pub why: String,
    pub with: Vec<String>,
}

/// A script folder whose imports the files can't pin to one binary: none holds them all, or several do. It and its
/// candidates (every binary holding one of its imports) are parked neither: by name refused, `ALL` skips them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unsettled {
    pub folder: String,
    pub candidates: Vec<String>,
    pub why: String,
}

/// Which binary each script folder imports from, as the files say ([`learn`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Learned {
    /// `[script folder, binary]`: parked both or neither.
    pub pairs: Vec<[String; 2]>,
    pub unsettled: Vec<Unsettled>,
}

/// One merge of Script Merger's (`MergeInventory.xml`): the Mods folder it wrote and the Mods folders it merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub merged: String,
    pub included: Vec<String>,
}

/// What the planner sees beyond Mods and bin; the default is none of it (no mod manager, no merges).
#[derive(Debug, Clone, Copy, Default)]
pub struct More<'a> {
    /// The menu folder's and the DLC folder's listings, where a managed mod's units may live.
    pub menu: Option<Listing<'a>>,
    pub dlc: Option<Listing<'a>>,
    pub managed: &'a [Managed],
    pub unsettled: &'a [Unsettled],
    /// Script Merger's merges: a parked Mods folder takes the folder merged from it along.
    pub merges: &'a [Merge],
    /// Why Script Merger's inventory can't be read: then no Mods folder is parked.
    pub merges_unread: Option<&'a str>,
}

/// Why a plan is refused. Nothing is parked when one comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkError {
    /// A name to park, requested or taken by `ALL`, that is not `\w[\w.-]*` with an ASCII `\w` (see [`plain`]), and
    /// no managed mod's.
    BadName(String),
    /// Not in the folder its name says it lives in: bin for `.asi`, `.addon64` and `dxgi.dll`, else Mods.
    Unknown(String),
    /// `nexus:<id>` of no managed mod.
    NoManaged(String),
    /// A name several managed mods answer to: (the name, how many).
    Ambiguous(String, usize),
    /// A managed mod's unit that isn't in its folder: (the mod, the unit as `<folder>\<name>`).
    Missing(String, String),
    /// A symbolic link or junction.
    Link(String),
    /// A file where a folder belongs (Mods, DLC), or a folder where a file does (bin, the menu folder).
    WrongKind(String, Home),
    /// Named twice (names compare case-insensitively).
    Repeated(String),
    /// On the never-park list, or a unit of a managed mod that holds such a name.
    NeverPark(String),
    /// Both requested and on the keep list.
    Kept(String),
    /// One half of a pair requested without the other: (the half named, the other half).
    Half(String, String),
    /// A managed mod named as a unit that can't be one: (the mod, why).
    Tied(String, String),
    /// A script folder whose binary the files can't settle, or one of its candidates: (the folder, why).
    Unsettled(String, String),
    /// A Mods folder parked whose Script Merger merge is kept: (the merged folder, the folder merged into it).
    MergeKept(String, String),
    /// Script Merger's inventory can't be read, and a Mods folder would be parked.
    Merges(String),
}

impl fmt::Display for ParkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName(name) => write!(f, "{name:?} is not a mod name (ASCII \\w[\\w.-]*)"),
            Self::Unknown(name) => {
                write!(f, "no {name:?} in the {} folder", Home::of(name).folder())
            }
            Self::NoManaged(name) => write!(
                f,
                "no deployed Vortex mod {name:?}: name it by nexus:<id>, its name or its Vortex folder, as the mods \
                 tool lists them"
            ),
            Self::Ambiguous(name, n) => {
                write!(
                    f,
                    "{name:?} names {n} Vortex mods: name one by its Vortex folder"
                )
            }
            Self::Missing(label, unit) => write!(
                f,
                "{label}: {unit} isn't there (a Vortex deploy puts it back)"
            ),
            Self::Link(name) => write!(f, "{name:?} is a link or junction, never parked"),
            Self::WrongKind(name, home) => match home.is_dir() {
                true => write!(f, "{name:?} in {} is not a folder", home.folder()),
                false => write!(f, "{name:?} in {} is a folder", home.folder()),
            },
            Self::Repeated(name) => write!(f, "{name:?} is named twice"),
            Self::NeverPark(name) => write!(f, "{name:?} is never parked"),
            Self::Kept(name) => write!(f, "{name:?} is both requested and kept"),
            Self::Half(name, other) => write!(
                f,
                "{name:?} is parked only together with {other:?}: one half alone fails the game's script compile, \
                 so name both or neither"
            ),
            Self::Tied(label, why) => write!(f, "{label} isn't parked as a unit: {why}"),
            Self::Unsettled(folder, why) => write!(
                f,
                "{folder:?} imports script functions the files can't pin to one binary ({why}): quen doesn't guess, \
                 so it parks neither the folder nor those binaries"
            ),
            Self::MergeKept(merged, from) => write!(
                f,
                "{merged:?} holds {from:?}'s scripts, merged by Script Merger: it is parked with it, so it can't be \
                 kept"
            ),
            Self::Merges(why) => write!(
                f,
                "Script Merger's MergeInventory.xml can't be read ({why}): no Mods folder is parked while its merges \
                 are unknown"
            ),
        }
    }
}

impl std::error::Error for ParkError {}

/// An item a plan moves: where, its entry, whether `ALL` or a managed mod's unit took it (its config may stay), and
/// whether `ALL` did (it leaves a pair it can't take whole).
#[derive(Clone, Copy)]
struct Item<'a> {
    home: Home,
    listing: Listing<'a>,
    entry: &'a Entry,
    derived: bool,
    by_all: bool,
}

impl Item<'_> {
    fn is(&self, home: Home, name: &str) -> bool {
        self.home == home && self.entry.name.eq_ignore_ascii_case(name)
    }
}

/// What a request or keep name names.
enum Target<'a> {
    /// One folder or file: its home and the name to look up there.
    One(Home, String),
    Mod(&'a Managed),
}

/// Plans the moves for `requested`, a comma list of names and `ALL`, matched case-insensitively to the on-disk
/// spelling. A name is a Mods folder or a bin `.asi` / `.addon64` / `dxgi.dll` (today's names), else a managed mod
/// (`nexus:<id>`, its name, its manager's folder: all its units, less those kept), else a managed mod's unit by its
/// file or folder name. `ALL` is every Mods folder, every such bin file and every managed unit that is not a link,
/// minus `keep` (a comma list of the same names), `never_park` (and every unit of a managed mod holding such a name),
/// [`W3_ALL_SKIPS`], unsettled folders and their candidates, both halves of each of `pairs` it can't take whole, and
/// the names listed explicitly, which keep their own place in the order. A bin `<stem>.ini` that `ALL` or a unit takes
/// stays while its `<stem>` binary does.
/// A Mods folder parked takes along the folder Script Merger merged it into. Refuses the whole plan on a bad, unknown,
/// repeated or never-park name, a link, a name both requested and kept, one half of a pair, an unsettled folder or
/// candidate named, a managed mod tied to what it can't park, and when `ALL` would take a name that isn't [`plain`].
/// Keep names only have to exist: they never move.
/// Every move goes to `<parking>/<home folder>/<name>`.
#[allow(clippy::too_many_arguments)]
pub fn plan<'a>(
    requested: &str,
    keep: &str,
    never_park: &[&str],
    pairs: &[[&str; 2]],
    mods: Listing<'a>,
    bin: Listing<'a>,
    parking: &Path,
    more: &More<'a>,
) -> Result<ParkPlan, ParkError> {
    let requested: Vec<&str> = names(requested).collect();
    let keep: Vec<&str> = names(keep).collect();
    let listing = |home: Home| match home {
        Home::Mods => Some(mods),
        Home::Bin => Some(bin),
        Home::Menu => more.menu,
        Home::Dlc => more.dlc,
    };
    let entry = |home: Home, name: &str| {
        let listing = listing(home)?;
        let found = listing
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name));
        found.map(|e| (listing, e))
    };
    let never_mod = |m: &Managed| m.units.iter().any(|(_, n)| listed(never_park, n));
    let never = |home: Home, name: &str| {
        listed(never_park, name)
            || more.managed.iter().any(|m| {
                never_mod(m)
                    && m.units
                        .iter()
                        .any(|(h, n)| *h == home && n.eq_ignore_ascii_case(name))
            })
    };
    let target = |name: &str| -> Result<Target<'a>, ParkError> {
        if find(name, mods, bin).is_ok() {
            return Ok(Target::One(Home::of(name), name.into()));
        }
        let hits: Vec<&Managed> = more
            .managed
            .iter()
            .filter(|m| m.names.iter().any(|n| n.eq_ignore_ascii_case(name)))
            .collect();
        match hits[..] {
            [m] => return Ok(Target::Mod(m)),
            [] => {}
            _ => return Err(ParkError::Ambiguous(name.into(), hits.len())),
        }
        if name.to_lowercase().starts_with("nexus:") {
            return Err(ParkError::NoManaged(name.into()));
        }
        if !plain(name) {
            return Err(ParkError::BadName(name.into()));
        }
        let mut units = more.managed.iter().flat_map(|m| &m.units);
        match units.find(|(_, n)| n.eq_ignore_ascii_case(name)) {
            Some((home, n)) => Ok(Target::One(*home, n.clone())),
            None => Err(ParkError::Unknown(name.into())),
        }
    };
    // What keep names: every unit of a mod it names.
    let mut kept: Vec<(Home, String)> = Vec::new();
    for &name in &keep {
        match target(name) {
            Ok(Target::One(home, n)) => {
                let (_, e) = entry(home, &n).ok_or_else(|| ParkError::Unknown(name.into()))?;
                kept.push((home, e.name.clone()));
            }
            Ok(Target::Mod(m)) => kept.extend(m.units.iter().cloned()),
            Err(ParkError::BadName(n)) => return Err(ParkError::Unknown(n)),
            Err(e) => return Err(e),
        }
    }
    let is_kept = |home: Home, name: &str| {
        kept.iter()
            .any(|(h, n)| *h == home && n.eq_ignore_ascii_case(name))
    };
    // The explicit names in order, each item once; ALL's place among them.
    let mut explicit: Vec<Item> = Vec::new();
    let mut all_at = None;
    let mut units_of: Vec<&Managed> = Vec::new();
    for (i, &name) in requested.iter().enumerate() {
        if listed(&requested[..i], name) {
            return Err(ParkError::Repeated(name.into()));
        }
        if name == ALL {
            all_at = Some(explicit.len());
            continue;
        }
        if listed(never_park, name) {
            return Err(ParkError::NeverPark(name.into()));
        }
        if listed(&keep, name) {
            return Err(ParkError::Kept(name.into()));
        }
        let mut add = |item: Item<'a>| {
            if !explicit.iter().any(|e| e.is(item.home, &item.entry.name)) {
                explicit.push(item);
            }
        };
        match target(name)? {
            Target::One(..) if !plain(name) => return Err(ParkError::BadName(name.into())),
            Target::One(home, n) => {
                let (listing, e) =
                    entry(home, &n).ok_or_else(|| ParkError::Unknown(name.into()))?;
                if never(home, &e.name) {
                    return Err(ParkError::NeverPark(name.into()));
                }
                if is_kept(home, &e.name) {
                    return Err(ParkError::Kept(name.into()));
                }
                if e.is_link {
                    return Err(ParkError::Link(name.into()));
                }
                if e.is_dir != home.is_dir() {
                    return Err(ParkError::WrongKind(name.into(), home));
                }
                add(Item {
                    home,
                    listing,
                    entry: e,
                    derived: false,
                    by_all: false,
                });
            }
            Target::Mod(m) => {
                if never_mod(m) {
                    return Err(ParkError::NeverPark(name.into()));
                }
                units_of.push(m);
                for (home, n) in m.units.iter().filter(|(h, n)| !is_kept(*h, n)) {
                    let unit = format!("{}\\{n}", home.folder());
                    let (listing, e) =
                        entry(*home, n).ok_or_else(|| ParkError::Missing(m.label.clone(), unit))?;
                    if e.is_link {
                        return Err(ParkError::Link(e.name.clone()));
                    }
                    if e.is_dir != home.is_dir() {
                        return Err(ParkError::WrongKind(e.name.clone(), *home));
                    }
                    add(Item {
                        home: *home,
                        listing,
                        entry: e,
                        derived: true,
                        by_all: false,
                    });
                }
            }
        }
    }
    // A managed mod tied to what quen doesn't park, or to mods not named beside it.
    for m in &units_of {
        let named = |label: &String| units_of.iter().any(|o| &o.label == label);
        if let Some(tie) = m
            .ties
            .iter()
            .find(|t| t.with.is_empty() || !t.with.iter().all(named))
        {
            return Err(ParkError::Tied(m.label.clone(), tie.why.clone()));
        }
    }
    let skip: Vec<&str> = more
        .unsettled
        .iter()
        .flat_map(|u| std::iter::once(&u.folder).chain(&u.candidates))
        .map(String::as_str)
        .chain(W3_ALL_SKIPS)
        .collect();
    let mut moves: Vec<Item> = Vec::new();
    if let Some(at) = all_at {
        let taken = all(&listing, more, &never, &is_kept, &skip, &explicit)?;
        moves.extend_from_slice(&explicit[..at]);
        moves.extend(taken);
        moves.extend_from_slice(&explicit[at..]);
    } else {
        moves = explicit;
    }
    let moved = |name: &str, moves: &[Item]| {
        moves
            .iter()
            .any(|m| m.entry.name.eq_ignore_ascii_case(name))
    };
    // A pair `ALL` took one half of (the other kept, never parked or not there): it leaves both.
    while let Some(half) = pairs.iter().find_map(|&[a, b]| {
        let half = match (moved(a, &moves), moved(b, &moves)) {
            (true, false) => a,
            (false, true) => b,
            _ => return None,
        };
        let by_all = |m: &Item| m.by_all && m.entry.name.eq_ignore_ascii_case(half);
        moves.iter().position(by_all)
    }) {
        moves.remove(half);
    }
    // A config `ALL` or a unit took stays with its binary: a file of bin's kinds or a managed unit there, not an ini.
    let unit = |name: &str| {
        let mut units = more.managed.iter().flat_map(|m| &m.units);
        units.any(|(h, n)| *h == Home::Bin && n.eq_ignore_ascii_case(name))
    };
    let stays = |name: &str, moves: &[Item]| {
        bin.entries.iter().any(|e| {
            !e.is_dir
                && !e.name.to_lowercase().ends_with(".ini")
                && (bin_kind(&e.name) || unit(&e.name))
                && stem(&e.name).eq_ignore_ascii_case(stem(name))
                && !moves
                    .iter()
                    .any(|m| m.home == Home::Bin && m.entry.name.eq_ignore_ascii_case(&e.name))
        })
    };
    let configs: Vec<String> = moves
        .iter()
        .filter(|m| {
            m.derived && m.home == Home::Bin && m.entry.name.to_lowercase().ends_with(".ini")
        })
        .filter(|m| stays(&m.entry.name, &moves))
        .map(|m| m.entry.name.clone())
        .collect();
    moves.retain(|m| !(m.home == Home::Bin && configs.contains(&m.entry.name)));
    for &[a, b] in pairs {
        match (moved(a, &moves), moved(b, &moves)) {
            (true, false) => return Err(ParkError::Half(a.into(), b.into())),
            (false, true) => return Err(ParkError::Half(b.into(), a.into())),
            _ => {}
        }
    }
    if let Some(u) = more
        .unsettled
        .iter()
        .find(|u| moved(&u.folder, &moves) || u.candidates.iter().any(|c| moved(c, &moves)))
    {
        return Err(ParkError::Unsettled(u.folder.clone(), u.why.clone()));
    }
    let moved_mods = |name: &str, moves: &[Item]| {
        moves
            .iter()
            .any(|m| m.home == Home::Mods && m.entry.name.eq_ignore_ascii_case(name))
    };
    if let Some(why) = more.merges_unread
        && moves.iter().any(|m| m.home == Home::Mods)
    {
        return Err(ParkError::Merges(why.into()));
    }
    for merge in more.merges {
        let Some(from) = merge.included.iter().find(|f| moved_mods(f, &moves)) else {
            continue;
        };
        if moved_mods(&merge.merged, &moves) {
            continue;
        }
        if is_kept(Home::Mods, &merge.merged) {
            return Err(ParkError::MergeKept(merge.merged.clone(), from.clone()));
        }
        if let Some((listing, e)) =
            entry(Home::Mods, &merge.merged).filter(|(_, e)| e.is_dir && !e.is_link)
        {
            moves.push(Item {
                home: Home::Mods,
                listing,
                entry: e,
                derived: true,
                by_all: false,
            });
        }
    }
    Ok(ParkPlan {
        marker: parking.join("quen-park.json"),
        moves: moves
            .into_iter()
            .map(|m| Move {
                from: m.listing.dir.join(&m.entry.name),
                to: parking.join(m.home.folder()).join(&m.entry.name),
            })
            .collect(),
    })
}

/// What `ALL` takes, home by home in name order: Mods folders, bin's `.asi` / `.addon64` / `dxgi.dll` and managed
/// units, then the menu's and DLC's managed units; none a link, kept, never parked, on `skip` or in `taken`.
fn all<'a>(
    listing: &dyn Fn(Home) -> Option<Listing<'a>>,
    more: &More<'a>,
    never: &dyn Fn(Home, &str) -> bool,
    kept: &dyn Fn(Home, &str) -> bool,
    skip: &[&str],
    taken: &[Item],
) -> Result<Vec<Item<'a>>, ParkError> {
    let mut out = Vec::new();
    for home in [Home::Mods, Home::Bin, Home::Menu, Home::Dlc] {
        let Some(listing) = listing(home) else {
            continue;
        };
        let managed = |e: &Entry| {
            more.managed
                .iter()
                .flat_map(|m| &m.units)
                .any(|(h, n)| *h == home && n.eq_ignore_ascii_case(&e.name))
        };
        let mut found: Vec<&Entry> = listing
            .entries
            .iter()
            .filter(|e| !e.is_link && e.is_dir == home.is_dir())
            .filter(|e| match home {
                Home::Mods | Home::Bin => Home::of(&e.name) == home || managed(e),
                Home::Menu | Home::Dlc => managed(e),
            })
            .filter(|e| {
                !never(home, &e.name)
                    && !kept(home, &e.name)
                    && !listed(skip, &e.name)
                    && !taken.iter().any(|t| t.is(home, &e.name))
            })
            .collect();
        found.sort_by_cached_key(|e| e.name.to_lowercase());
        if let Some(e) = found.iter().find(|e| !plain(&e.name)) {
            return Err(ParkError::BadName(e.name.clone()));
        }
        out.extend(found.into_iter().map(|entry| Item {
            home,
            listing,
            entry,
            derived: true,
            by_all: true,
        }));
    }
    Ok(out)
}

/// Whether `name` is one of the bin kinds quen parks by name alone: an `.asi`, an `.addon64` or `dxgi.dll` (any case).
pub fn bin_kind(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".asi") || name.ends_with(".addon64") || name == "dxgi.dll"
}

/// A file name without its last extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// The non-empty items of a comma list, trimmed.
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
        Home::Bin => bin,
        _ => mods,
    };
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name));
    entry
        .map(|e| (listing, e))
        .ok_or_else(|| ParkError::Unknown(name.into()))
}

/// Which of `wanted` (script function names) `bytes` (a binary) holds, each as a whole identifier in ASCII or UTF-16LE:
/// bounded by bytes that can't be in one, so `Foo` isn't found in `FooBar`.
pub fn held(bytes: &[u8], wanted: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let (Some(min), Some(max)) = (
        wanted.iter().map(String::len).min(),
        wanted.iter().map(String::len).max(),
    ) else {
        return out;
    };
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut check = |run: &[u8]| {
        if run.len() >= min
            && let Ok(name) = std::str::from_utf8(run)
            && wanted.contains(name)
        {
            out.insert(name.to_owned());
        }
    };
    for run in bytes.split(|&b| !word(b)).filter(|r| r.len() <= max) {
        check(run);
    }
    // UTF-16LE: a word byte then a zero, at either alignment.
    let mut run = Vec::with_capacity(max);
    for start in 0..2 {
        let (mut long, chars) = (
            false,
            bytes.get(start..).unwrap_or_default().chunks_exact(2),
        );
        for pair in chars {
            if pair[1] == 0 && word(pair[0]) {
                match run.len() < max {
                    true => run.push(pair[0]),
                    false => long = true,
                }
                continue;
            }
            if !long {
                check(&run);
            }
            run.clear();
            long = false;
        }
        if !long {
            check(&run);
        }
        run.clear();
    }
    out
}

/// Pairs each script folder of `imports` (its `import function` names) with the one binary of `held` (each binary's
/// file name and which of those names it holds, [`held`]) that holds every name it imports, leaving out names the
/// game's exe holds (`natives`: the engine's own) and names a never-park binary holds (it never moves). A folder on
/// `never_park`, or left with no name, pairs with nothing. One no binary or several binaries hold all of is unsettled,
/// with every binary holding any of its names as a candidate: never guessed.
pub fn learn(
    imports: &[(String, Vec<String>)],
    held: &[(String, BTreeSet<String>)],
    natives: &BTreeSet<String>,
    never_park: &[&str],
) -> Learned {
    let mut out = Learned::default();
    let (fixed, parkable): (Vec<_>, Vec<_>) = held.iter().partition(|(b, _)| listed(never_park, b));
    for (folder, names) in imports.iter().filter(|(f, _)| !listed(never_park, f)) {
        let names: Vec<&String> = names
            .iter()
            .filter(|n| !natives.contains(*n) && !fixed.iter().any(|(_, h)| h.contains(*n)))
            .collect();
        if names.is_empty() {
            continue;
        }
        let holding = |all: bool| {
            let found = parkable.iter().filter(|(_, h)| match all {
                true => names.iter().all(|n| h.contains(*n)),
                false => names.iter().any(|n| h.contains(*n)),
            });
            found.map(|(b, _)| b.clone()).collect::<Vec<String>>()
        };
        let every = holding(true);
        if let [one] = &every[..] {
            out.pairs.push([folder.clone(), one.clone()]);
            continue;
        }
        let why = match every.len() {
            0 => {
                let first: Vec<&str> = names.iter().take(3).map(|n| n.as_str()).collect();
                format!(
                    "no binary in bin holds all {} of its imports, such as {}",
                    names.len(),
                    first.join(", ")
                )
            }
            _ => format!(
                "{} binaries hold all its imports: {}",
                every.len(),
                every.join(", ")
            ),
        };
        out.unsettled.push(Unsettled {
            folder: folder.clone(),
            candidates: holding(false),
            why,
        });
    }
    out
}

/// Script Merger's merges in its `MergeInventory.xml`: each `<Merge>`'s `<MergedModName>` and `<IncludedMod>`s. Err
/// when a merge lacks either.
pub fn merges(xml: &str) -> Result<Vec<Merge>, String> {
    let texts = |block: &str, tag: &str| -> Vec<String> {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        block
            .split(&open)
            .skip(1)
            .filter_map(|s| s.split_once(&close))
            .map(|(text, _)| {
                let text = text.trim().replace("&lt;", "<").replace("&gt;", ">");
                text.replace("&apos;", "'")
                    .replace("&quot;", "\"")
                    .replace("&amp;", "&")
            })
            .collect()
    };
    let mut out = Vec::new();
    for block in xml.split("<Merge>").skip(1) {
        let block = block.split("</Merge>").next().unwrap_or_default();
        let (merged, included) = (texts(block, "MergedModName"), texts(block, "IncludedMod"));
        match (&merged[..], included.is_empty()) {
            ([merged], false) => out.push(Merge {
                merged: merged.clone(),
                included,
            }),
            _ => {
                return Err(format!(
                    "merge {} has no MergedModName or IncludedMod",
                    out.len() + 1
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The W3 Game State Bridge's pair, as the files settle it (`learn`'s test pins that they do).
    const GSB: &[[&str; 2]] = &[["modW3GameStateBridge", "W3GameStateBridge.asi"]];

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
        run_more(mods, bin, requested, keep, &More::default())
    }

    fn run_more(
        mods: &[Entry],
        bin: &[Entry],
        requested: &str,
        keep: &str,
        more: &More,
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
            GSB,
            mods,
            bin,
            Path::new("PARK"),
            more,
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
            (
                "readme.txt",
                ParkError::WrongKind("readme.txt".into(), Home::Mods),
            ),
            (
                "plugins.asi",
                ParkError::WrongKind("plugins.asi".into(), Home::Bin),
            ),
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
            ("nexus:1", ParkError::NoManaged("nexus:1".into())),
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
            GSB,
            mods,
            bin,
            Path::new("P"),
            &More::default(),
        )
        .unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        let back: ParkPlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
        assert!(text.contains("\"moves\":[{\"from\":"), "{text}");
    }

    /// Listings of Mods, bin, the menu folder and DLC, and the managed mods.
    type Game = (Vec<Entry>, Vec<Entry>, Vec<Entry>, Vec<Entry>, Vec<Managed>);

    /// A game with Vortex: W3LoadFix (two DLLs, the ini of one, a menu XML), Immersive World (a script folder and its
    /// ASI, the ASI's ini, an add-on, a data file, a menu XML), a DLC mod, the Native Bridge (its loader and both
    /// never-park halves), and a mod whose Mods folder another shares.
    fn vortex() -> Game {
        let mods = vec![
            dir("modWorld"),
            dir("modNativeBridge"),
            dir("modShared"),
            dir("modOwn"),
        ];
        let bin = vec![
            file("dsound.dll"),
            file("w3loadfix.dll"),
            file("w3loadfix.ini"),
            // The mod's log: no binary its ini stays with.
            file("w3loadfix.log"),
            file("World.asi"),
            file("World.ini"),
            file("World.addon64"),
            file("edges_0.bin"),
            file("dinput8.dll"),
            file("WitcherNativeBridge.asi"),
            file("Own.asi"),
            file("Own.ini"),
        ];
        let menu = vec![
            file("w3loadfix.xml"),
            file("World.xml"),
            file("graphics.xml"),
        ];
        let dlc = vec![dir("dlcExtra")];
        let unit = |home, name: &str| (home, name.to_owned());
        let managed = |label: &str, id: u32, units: Vec<(Home, String)>| Managed {
            label: label.into(),
            names: vec![
                format!("nexus:{id}"),
                label.into(),
                format!("{label} {id} 1 2026-10-19T17-54Z k"),
            ],
            units,
            ties: Vec::new(),
        };
        let mut shared = managed("Shared", 3, vec![unit(Home::Mods, "modShared")]);
        shared.ties.push(Tie {
            why: r"Mods\modShared holds files of Shared and Partner".into(),
            with: vec!["Partner".into()],
        });
        let mut partner = managed("Partner", 4, vec![unit(Home::Mods, "modShared")]);
        partner.ties.push(Tie {
            why: r"Mods\modShared holds files of Shared and Partner".into(),
            with: vec!["Shared".into()],
        });
        let mut content = managed("Retexture", 5, vec![unit(Home::Dlc, "dlcExtra")]);
        content.ties.push(Tie {
            why: r"it deploys content\content0\a.bundle, which quen doesn't park".into(),
            with: Vec::new(),
        });
        let all = vec![
            managed(
                "W3LoadFix",
                13881,
                vec![
                    unit(Home::Bin, "dsound.dll"),
                    unit(Home::Bin, "w3loadfix.dll"),
                    unit(Home::Bin, "w3loadfix.ini"),
                    unit(Home::Menu, "w3loadfix.xml"),
                ],
            ),
            managed(
                "Immersive World",
                14126,
                vec![
                    unit(Home::Mods, "modWorld"),
                    unit(Home::Bin, "World.asi"),
                    unit(Home::Bin, "World.ini"),
                    unit(Home::Bin, "World.addon64"),
                    unit(Home::Bin, "edges_0.bin"),
                    unit(Home::Menu, "World.xml"),
                ],
            ),
            managed(
                "Native Bridge",
                13743,
                vec![
                    unit(Home::Mods, "modNativeBridge"),
                    unit(Home::Bin, "dinput8.dll"),
                    unit(Home::Bin, "WitcherNativeBridge.asi"),
                ],
            ),
            shared,
            partner,
            content,
        ];
        (mods, bin, menu, dlc, all)
    }

    /// `run` over [`vortex`]'s game, with `pairs` learned and nothing unsettled unless given.
    fn run_vortex(
        requested: &str,
        keep: &str,
        pairs: &[[&str; 2]],
        unsettled: &[Unsettled],
        merges: &[Merge],
    ) -> Result<Vec<String>, ParkError> {
        let (mods, bin, menu, dlc, managed) = vortex();
        let more = More {
            menu: Some(Listing {
                dir: Path::new("GAME/menu"),
                entries: &menu,
            }),
            dlc: Some(Listing {
                dir: Path::new("GAME/DLC"),
                entries: &dlc,
            }),
            managed: &managed,
            unsettled,
            merges,
            merges_unread: None,
        };
        let plan = plan(
            requested,
            keep,
            W3_NEVER_PARK,
            pairs,
            Listing {
                dir: Path::new("GAME/Mods"),
                entries: &mods,
            },
            Listing {
                dir: Path::new("GAME/bin"),
                entries: &bin,
            },
            Path::new("PARK"),
            &more,
        )?;
        let show = |p: &Path| p.to_str().unwrap().replace('\\', "/");
        Ok(plan
            .moves
            .iter()
            .map(|m| format!("{} -> {}", show(&m.from), show(&m.to)))
            .collect())
    }

    const WORLD: &[[&str; 2]] = &[["modWorld", "World.asi"]];

    #[test]
    fn a_vortex_mod_parks_as_a_unit_by_nexus_id_or_name() {
        let fix = [
            "GAME/bin/dsound.dll -> PARK/bin/dsound.dll",
            "GAME/bin/w3loadfix.dll -> PARK/bin/w3loadfix.dll",
            "GAME/bin/w3loadfix.ini -> PARK/bin/w3loadfix.ini",
            "GAME/menu/w3loadfix.xml -> PARK/menu/w3loadfix.xml",
        ];
        assert_eq!(run_vortex("nexus:13881", "", WORLD, &[], &[]).unwrap(), fix);
        assert_eq!(run_vortex("w3loadfix", "", WORLD, &[], &[]).unwrap(), fix);
        assert_eq!(
            run_vortex("W3LoadFix 13881 1 2026-10-19T17-54Z k", "", WORLD, &[], &[]).unwrap(),
            fix
        );
        // Every unit of Immersive World, its pair included.
        let world = run_vortex("nexus:14126", "", WORLD, &[], &[]).unwrap();
        assert_eq!(
            world,
            [
                "GAME/Mods/modWorld -> PARK/Mods/modWorld",
                "GAME/bin/World.asi -> PARK/bin/World.asi",
                "GAME/bin/World.ini -> PARK/bin/World.ini",
                "GAME/bin/World.addon64 -> PARK/bin/World.addon64",
                "GAME/bin/edges_0.bin -> PARK/bin/edges_0.bin",
                "GAME/menu/World.xml -> PARK/menu/World.xml",
            ]
        );
        // Two mods, and a unit named twice by them moves once.
        let both = run_vortex("nexus:13881,World.asi,nexus:14126", "", WORLD, &[], &[]).unwrap();
        assert_eq!(both.len(), 10, "{both:?}");
        // A unit's own file name, alone: a partial park.
        assert_eq!(
            run_vortex("edges_0.bin,w3loadfix.xml", "", WORLD, &[], &[]).unwrap(),
            [
                "GAME/bin/edges_0.bin -> PARK/bin/edges_0.bin",
                "GAME/menu/w3loadfix.xml -> PARK/menu/w3loadfix.xml"
            ]
        );
        // The DLC folder: its own parking folder.
        let (mods, bin, menu, dlc, mut managed) = vortex();
        managed[5].ties.clear();
        let more = More {
            dlc: Some(Listing {
                dir: Path::new("GAME/DLC"),
                entries: &dlc,
            }),
            menu: Some(Listing {
                dir: Path::new("GAME/menu"),
                entries: &menu,
            }),
            managed: &managed,
            ..More::default()
        };
        assert_eq!(
            run_more(&mods, &bin, "nexus:5", "", &more).unwrap(),
            [pair("GAME/DLC/dlcExtra", "PARK/dlc/dlcExtra")]
        );
    }

    #[test]
    fn a_partial_park_keeps_what_keep_names_and_configs_stay_with_their_binary() {
        // Keeping the add-on: the ini its stem shares stays with it, the rest of the mod moves.
        let world = run_vortex("nexus:14126", "World.addon64", WORLD, &[], &[]).unwrap();
        assert!(
            !world
                .iter()
                .any(|m| m.contains("addon64") || m.contains(".ini")),
            "{world:?}"
        );
        assert_eq!(world.len(), 4);
        // Keeping a DLL: its ini stays too, the other DLL and the menu move.
        assert_eq!(
            run_vortex("nexus:13881", "w3loadfix.dll", WORLD, &[], &[]).unwrap(),
            [
                "GAME/bin/dsound.dll -> PARK/bin/dsound.dll",
                "GAME/menu/w3loadfix.xml -> PARK/menu/w3loadfix.xml"
            ]
        );
        // Named by its own name, an ini moves alone.
        assert_eq!(
            run_vortex("w3loadfix.ini", "", WORLD, &[], &[]).unwrap(),
            ["GAME/bin/w3loadfix.ini -> PARK/bin/w3loadfix.ini"]
        );
        // Keeping a whole mod by name beside ALL.
        let all = run_vortex("ALL", "nexus:14126", WORLD, &[], &[]).unwrap();
        assert!(!all.iter().any(|m| m.contains("World")), "{all:?}");
        // Keeping half of a pair while parking its mod: refused.
        assert_eq!(
            run_vortex("nexus:14126", "World.asi", WORLD, &[], &[]),
            Err(ParkError::Half("modWorld".into(), "World.asi".into()))
        );
    }

    #[test]
    fn all_takes_vortex_units_and_whole_pairs_but_never_park_mods() {
        let all = run_vortex("ALL", "", WORLD, &[], &[]).unwrap();
        assert_eq!(
            all,
            [
                "GAME/Mods/modOwn -> PARK/Mods/modOwn",
                "GAME/Mods/modShared -> PARK/Mods/modShared",
                "GAME/Mods/modWorld -> PARK/Mods/modWorld",
                "GAME/bin/dsound.dll -> PARK/bin/dsound.dll",
                "GAME/bin/edges_0.bin -> PARK/bin/edges_0.bin",
                "GAME/bin/Own.asi -> PARK/bin/Own.asi",
                "GAME/bin/w3loadfix.dll -> PARK/bin/w3loadfix.dll",
                "GAME/bin/w3loadfix.ini -> PARK/bin/w3loadfix.ini",
                "GAME/bin/World.addon64 -> PARK/bin/World.addon64",
                "GAME/bin/World.asi -> PARK/bin/World.asi",
                "GAME/bin/World.ini -> PARK/bin/World.ini",
                "GAME/menu/w3loadfix.xml -> PARK/menu/w3loadfix.xml",
                "GAME/menu/World.xml -> PARK/menu/World.xml",
                "GAME/DLC/dlcExtra -> PARK/dlc/dlcExtra",
            ]
        );
        // graphics.xml and Own.ini are no mod's units. The mod named beside ALL changes only the order; without learned
        // pairs ALL takes the same.
        let mut with = run_vortex("ALL,nexus:14126", "", WORLD, &[], &[]).unwrap();
        with.sort();
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(with, sorted);
        assert_eq!(run_vortex("ALL", "", &[], &[], &[]).unwrap(), all);
        // One half kept: ALL leaves both, and the ASI's ini with them; one half named beside it is still refused.
        let kept = run_vortex("ALL", "World.asi", WORLD, &[], &[]).unwrap();
        assert_eq!(kept.len(), all.len() - 3, "{kept:?}");
        assert!(
            !kept
                .iter()
                .any(|m| m.contains("modWorld") || m.contains("World.ini")),
            "{kept:?}"
        );
        assert_eq!(
            run_vortex("ALL,modWorld", "World.asi", WORLD, &[], &[]),
            Err(ParkError::Half("modWorld".into(), "World.asi".into()))
        );
        // The Native Bridge's mod: never, by any of its names or units.
        for name in ["nexus:13743", "Native Bridge", "dinput8.dll"] {
            assert_eq!(
                run_vortex(name, "", WORLD, &[], &[]),
                Err(ParkError::NeverPark(name.into()))
            );
        }
    }

    /// The owner's rule: what ALL parked before learned pairs it still parks; the W3 Game State Bridge is the one pair it
    /// leaves (RenoDX reads it), which parks named whole and is refused named by half.
    #[test]
    fn all_leaves_the_game_state_bridge_and_parks_other_pairs_whole() {
        let (mut mods, mut bin) = game();
        mods.push(dir("modWorld"));
        bin.push(file("World.asi"));
        let pairs: &[[&str; 2]] = &[GSB[0], ["modWorld", "World.asi"]];
        let run = |requested: &str, keep: &str| {
            let plan = plan(
                requested,
                keep,
                W3_NEVER_PARK,
                pairs,
                Listing {
                    dir: Path::new("GAME/Mods"),
                    entries: &mods,
                },
                Listing {
                    dir: Path::new("GAME/bin/x64"),
                    entries: &bin,
                },
                Path::new("PARK"),
                &More::default(),
            )?;
            let names = plan
                .moves
                .iter()
                .map(|m| m.from.file_name().unwrap().to_string_lossy().into_owned());
            Ok::<Vec<String>, ParkError>(names.collect())
        };
        let all = run("ALL", "").unwrap();
        assert!(
            all.contains(&"modWorld".into()) && all.contains(&"World.asi".into()),
            "{all:?}"
        );
        assert!(
            !all.iter().any(|n| n.contains("GameStateBridge")),
            "{all:?}"
        );
        let both = run("ALL,W3GameStateBridge.asi,modW3GameStateBridge", "").unwrap();
        assert_eq!(both.len(), all.len() + 2);
        assert_eq!(
            run("ALL,modW3GameStateBridge", ""),
            Err(ParkError::Half(
                "modW3GameStateBridge".into(),
                "W3GameStateBridge.asi".into()
            ))
        );
        assert_eq!(
            run("World.asi", ""),
            Err(ParkError::Half("World.asi".into(), "modWorld".into()))
        );
    }

    #[test]
    fn a_unit_tied_to_what_quen_doesnt_park_is_refused() {
        // A file under content\: never as a unit, but ALL and its folder's name still park what they can.
        let why = run_vortex("nexus:5", "", WORLD, &[], &[]).unwrap_err();
        assert!(
            matches!(&why, ParkError::Tied(label, why) if label == "Retexture" && why.contains(r"content\content0")),
            "{why:?}"
        );
        assert!(
            why.to_string()
                .starts_with("Retexture isn't parked as a unit: it deploys")
        );
        assert_eq!(
            run_vortex("dlcExtra", "", WORLD, &[], &[]).unwrap(),
            ["GAME/DLC/dlcExtra -> PARK/dlc/dlcExtra"]
        );
        // A Mods folder two mods share: both or neither, and by its folder name as today.
        assert!(matches!(
            run_vortex("nexus:3", "", WORLD, &[], &[]),
            Err(ParkError::Tied(label, _)) if label == "Shared"
        ));
        assert_eq!(
            run_vortex("nexus:3,nexus:4", "", WORLD, &[], &[]).unwrap(),
            ["GAME/Mods/modShared -> PARK/Mods/modShared"]
        );
        assert_eq!(
            run_vortex("modShared", "", WORLD, &[], &[]).unwrap().len(),
            1
        );
        // Names that are no mod, or several.
        assert_eq!(
            run_vortex("nexus:999", "", WORLD, &[], &[]),
            Err(ParkError::NoManaged("nexus:999".into()))
        );
        let (mods, bin, menu, dlc, mut managed) = vortex();
        managed[1].names.push("W3LoadFix".into());
        let more = More {
            menu: Some(Listing {
                dir: Path::new("GAME/menu"),
                entries: &menu,
            }),
            dlc: Some(Listing {
                dir: Path::new("GAME/DLC"),
                entries: &dlc,
            }),
            managed: &managed,
            ..More::default()
        };
        assert_eq!(
            run_more(&mods, &bin, "w3loadfix", "", &more),
            Err(ParkError::Ambiguous("w3loadfix".into(), 2))
        );
        // A unit that isn't there.
        let bin: Vec<Entry> = bin.into_iter().filter(|e| e.name != "dsound.dll").collect();
        assert_eq!(
            run_more(&mods, &bin, "nexus:13881", "", &more),
            Err(ParkError::Missing(
                "W3LoadFix".into(),
                r"bin\dsound.dll".into()
            ))
        );
    }

    #[test]
    fn an_unsettled_folder_and_its_candidates_park_neither() {
        let unsettled = [Unsettled {
            folder: "modOwn".into(),
            candidates: vec!["Own.asi".into(), "World.asi".into()],
            why: "2 binaries hold all its imports: Own.asi, World.asi".into(),
        }];
        for name in ["modOwn", "Own.asi", "nexus:14126"] {
            let why = run_vortex(name, "", &[], &unsettled, &[]).unwrap_err();
            assert_eq!(
                why,
                ParkError::Unsettled("modOwn".into(), unsettled[0].why.clone()),
                "{name}"
            );
            assert!(why.to_string().contains("quen doesn't guess"));
        }
        // ALL skips all three.
        let all = run_vortex("ALL", "", &[], &unsettled, &[]).unwrap();
        assert!(
            !all.iter()
                .any(|m| m.contains("Own") || m.contains("World.asi")),
            "{all:?}"
        );
    }

    #[test]
    fn a_parked_mod_takes_its_script_merger_merge_along() {
        let (mut mods, bin) = game();
        mods.push(dir("mod0000_MergedFiles"));
        let merges = [Merge {
            merged: "mod0000_MergedFiles".into(),
            included: vec!["modZeta".into(), "modOther".into()],
        }];
        let more = More {
            merges: &merges,
            ..More::default()
        };
        assert_eq!(
            run_more(&mods, &bin, "modzeta", "", &more).unwrap(),
            [
                pair("GAME/Mods/modZeta", "PARK/Mods/modZeta"),
                pair(
                    "GAME/Mods/mod0000_MergedFiles",
                    "PARK/Mods/mod0000_MergedFiles"
                ),
            ]
        );
        assert_eq!(
            run_more(&mods, &bin, "modAlpha", "", &more).unwrap(),
            [pair("GAME/Mods/modAlpha", "PARK/Mods/modAlpha")]
        );
        assert_eq!(
            run_more(&mods, &bin, "ALL", "mod0000_MergedFiles", &more),
            Err(ParkError::MergeKept(
                "mod0000_MergedFiles".into(),
                "modZeta".into()
            ))
        );
        let unread = More {
            merges_unread: Some("not XML"),
            ..More::default()
        };
        assert_eq!(
            run_more(&mods, &bin, "modzeta", "", &unread),
            Err(ParkError::Merges("not XML".into()))
        );
        assert_eq!(
            run_more(&mods, &bin, "Tweaks.asi", "", &unread)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn merges_are_read_from_script_mergers_inventory() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<MergeInventory xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <Merges>
    <Merge>
      <RelativePath>content\scripts\game\player\r4Player.ws</RelativePath>
      <MergedModName>mod0000_MergedFiles</MergedModName>
      <IncludedMod>modA</IncludedMod>
      <IncludedMod> modB&amp;C </IncludedMod>
    </Merge>
  </Merges>
</MergeInventory>"#;
        assert_eq!(
            merges(xml).unwrap(),
            [Merge {
                merged: "mod0000_MergedFiles".into(),
                included: vec!["modA".into(), "modB&C".into()]
            }]
        );
        assert_eq!(
            merges("<MergeInventory><Merges /></MergeInventory>").unwrap(),
            []
        );
        assert!(merges("<Merge><IncludedMod>modA</IncludedMod></Merge>").is_err());
    }

    /// The owner's condition: the W3 Game State Bridge's pair is still found, from the files alone; imports the game's
    /// exe or a never-park binary holds pair with nothing; a pair the files can't settle is never guessed.
    #[test]
    fn pairs_are_learned_from_the_files() {
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
        let gsb_asi = b"\0\0GSB_Push\0rest".to_vec();
        let mut world_asi = b"MZ...".to_vec();
        // UTF-16LE names, and one inside a longer identifier that doesn't count.
        for name in ["BIW_A", "BIW_B"] {
            world_asi.extend(name.bytes().flat_map(|b| [b, 0]));
            world_asi.extend([0, 0]);
        }
        world_asi.extend(b"BIW_CX\0");
        let exe = b"x SetCommunityRadius\0";
        let imports = vec![
            (
                "modW3GameStateBridge".to_owned(),
                vec!["GSB_Push".to_owned()],
            ),
            (
                "modWorld".to_owned(),
                vec!["BIW_A".into(), "BIW_B".into(), "WNB_Call".into()],
            ),
            (
                "modGraphics".to_owned(),
                vec!["SetCommunityRadius".to_owned()],
            ),
            ("modQuen".to_owned(), vec!["QUEN_Next".to_owned()]),
            ("modTwice".to_owned(), vec!["TWO_X".to_owned()]),
            (
                "modNone".to_owned(),
                vec!["BIW_C".to_owned(), "NONE_Y".to_owned()],
            ),
        ];
        let wanted: BTreeSet<String> = imports.iter().flat_map(|(_, n)| n.clone()).collect();
        let binaries: Vec<(&str, Vec<u8>)> = vec![
            ("W3GameStateBridge.asi", gsb_asi),
            ("World.asi", world_asi),
            ("WitcherNativeBridge.asi", b"WNB_Call\0".to_vec()),
            ("W3Quen.asi", b"QUEN_Next\0".to_vec()),
            ("a.asi", b"TWO_X\0".to_vec()),
            ("b.dll", b"TWO_X\0BIW_C\0".to_vec()),
        ];
        let held: Vec<(String, BTreeSet<String>)> = binaries
            .iter()
            .map(|(n, bytes)| (n.to_string(), super::held(bytes, &wanted)))
            .collect();
        assert_eq!(held[1].1, set(&["BIW_A", "BIW_B"]));
        let natives = super::held(exe, &wanted);
        assert_eq!(natives, set(&["SetCommunityRadius"]));
        let learned = learn(&imports, &held, &natives, W3_NEVER_PARK);
        assert_eq!(
            learned.pairs,
            [
                [
                    "modW3GameStateBridge".to_owned(),
                    "W3GameStateBridge.asi".to_owned()
                ],
                ["modWorld".to_owned(), "World.asi".to_owned()],
            ]
        );
        let unsettled: Vec<(&str, &[String])> = learned
            .unsettled
            .iter()
            .map(|u| (u.folder.as_str(), &u.candidates[..]))
            .collect();
        assert_eq!(
            unsettled,
            [
                ("modTwice", &["a.asi".to_owned(), "b.dll".to_owned()][..]),
                ("modNone", &["b.dll".to_owned()][..]),
            ]
        );
        assert_eq!(
            learned.unsettled[0].why,
            "2 binaries hold all its imports: a.asi, b.dll"
        );
        assert!(
            learned.unsettled[1]
                .why
                .starts_with("no binary in bin holds all 2 of its imports"),
            "{}",
            learned.unsettled[1].why
        );
        assert!(super::held(b"anything", &BTreeSet::new()).is_empty());
    }
}
