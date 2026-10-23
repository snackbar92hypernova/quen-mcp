//! `quen_doctor`: every dependency quen uses, found, missing or the wrong version, with its path, its version
//! or sha256, what it enables and where to get it: a name or an official page, since quen never downloads or installs a
//! third-party tool. Read-only and lock-free: file reads and hashes only. Tools check their own needs with [`require`]
//! before acting, and refuse with `needs` ([`Done::Needs`]) when one is missing. `quen doctor` is the same in a
//! terminal.

use std::cell::OnceCell;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use quen_core::boot::STEAM_APPID;
use quen_core::build::{self, Build, Gate};
use quen_core::detect;
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Ledger, git_head};
use quen_core::vortex::Inventory;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::session::{Done, Shared, envelope};
use crate::setup::{self, Machine, OnDisk};
use crate::w3;

/// A file quen expects byte for byte, relative to the game folder.
#[derive(Clone, Copy)]
pub(crate) struct Pin {
    file: &'static str,
    version: &'static str,
    sha256: &'static str,
    /// Our own parts and the game: another file is the wrong version. Third-party parts: one quen was tested with,
    /// and another shows as found, untested.
    exact: bool,
}

/// quen's in-game package: its two files, which only work as a pair from one release.
const PACKAGE_ASI: &str = r"bin\x64_dx12\W3Quen.asi";
const PACKAGE_SCRIPT: &str = r"Mods\modQuen\content\scripts\local\quen.ws";
/// From quen-w3 0.1.16, modQuen's bundle of the HUD line (w3\hud.py): both files from the ASI's release.
const PACKAGE_HUD: [&str; 2] = [
    r"Mods\modQuen\content\blob0.bundle",
    r"Mods\modQuen\content\metadata.store",
];
/// What a mod manager may own of quen's package and of the Native Bridge (`owner`).
const PACKAGE_OWNED: [&str; 2] = [PACKAGE_ASI, r"Mods\modQuen"];
const BRIDGE_OWNED: [&str; 2] = [
    r"bin\x64_dx12\WitcherNativeBridge.asi",
    r"Mods\modNativeBridge",
];
/// The version of the pure-script modQuen from before the package, pinned only to name it: the package replaces it.
const OLD_MODQUEN: &str = "the old pure-script modQuen (witcher3-mods 464ff2b)";
/// NVIDIA Streamline's interposer in the game: what a renderdoc launch swaps out and its end puts back (profile.rs).
const INTERPOSER: &str = r"bin\x64_dx12\sl.interposer.dll";
/// quen's release build of the patched dev interposer (`quen-streamline-dev-interposer-2.14.1.zip` on quen-mcp's GitHub
/// Releases): Streamline v2.14.1 with tools/streamline/getprocaddress.patch, `build.bat -develop`.
const DEV_INTERPOSER_RELEASE: &str =
    "a0483eb3de9063731f0e85e4e9b0f38e9884a33651a53c7b1e07f1d7822b6e24";

/// The in-game parts at the versions this quen speaks to (the game's own builds are the build gate's:
/// `w3::KNOWN_BUILDS` and the config's). quen's `w3/` and witcher3-mods store and build them byte for byte (no
/// line-ending conversion, /Brepro), so a sha256 names a version. One row per accepted file: add a row for a new
/// version, and keep the old one while quen still supports it.
const PINS: &[Pin] = &[
    Pin {
        file: r"bin\x64_dx12\WitcherNativeBridge.asi",
        version: "2.0",
        sha256: "3b52cc9c77f4e4604a1814158342a4e830cd39134e5128abd10ddbd63e60f53b",
        exact: false,
    },
    // 0.1.18: scene.frame projects from the free camera while it is on (world_frame, boot 238), in quen.ws; the ASI
    // differs from 0.1.17's only in its version, the HUD bundle is 0.1.16's, byte for byte.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.18",
        sha256: "65bdf7da5d58953e2748c97ff1b73fee7043b2cf5da8983b3a8ed9567e250519",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.18",
        sha256: "5e13694065b6c00fc4fa49a190116480a540569bc611c3ac419e1c7cf5196997",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[0],
        version: "quen-w3 0.1.18",
        sha256: "7592bfafef3c54ff258940f0c732a47e90903b3c4e5e74537a858d7269a1f2c1",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[1],
        version: "quen-w3 0.1.18",
        sha256: "7d05a5ee2297d231e71afe10a567a499ff23d78a769d5fa2df01804d120dab7c",
        exact: true,
    },
    // 0.1.17: the free-camera verbs (freecam, freecam.set, freecam.get: game_freecam, freecam.rs) in both files; its
    // HUD bundle is 0.1.16's, byte for byte.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.17",
        sha256: "dd115de7c36ae6f540342e6436a077385f23de232fb55bff1e42c7c40cfd57a1",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.17",
        sha256: "5042fbf01d281f3d4daafe70483c542eed945e743e93ca75dae217df559e5db5",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[0],
        version: "quen-w3 0.1.17",
        sha256: "7592bfafef3c54ff258940f0c732a47e90903b3c4e5e74537a858d7269a1f2c1",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[1],
        version: "quen-w3 0.1.17",
        sha256: "7d05a5ee2297d231e71afe10a567a499ff23d78a769d5fa2df01804d120dab7c",
        exact: true,
    },
    // 0.1.16: the lasting line on quen's own HUD movie in gameplay (modQuen's bundle, w3\hud.py), in quen.ws; the ASI
    // differs from 0.1.15's only in its version.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.16",
        sha256: "75f2d862378e8f53dfd6290c906a0ed9a91c57c8a1a0838a1556faa44f1ecbb2",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.16",
        sha256: "2589964c0deec1d9708b10951ad2160e914954e29c8a3a37a7c35a3581c78dcb",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[0],
        version: "quen-w3 0.1.16",
        sha256: "7592bfafef3c54ff258940f0c732a47e90903b3c4e5e74537a858d7269a1f2c1",
        exact: true,
    },
    Pin {
        file: PACKAGE_HUD[1],
        version: "quen-w3 0.1.16",
        sha256: "7d05a5ee2297d231e71afe10a567a499ff23d78a769d5fa2df01804d120dab7c",
        exact: true,
    },
    // 0.1.15: notice's duration, notice.hide and sound (the status line, status.rs) in both files.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.15",
        sha256: "2eea072f4da8705f07d3369b24cb4969023422feee33f8c0fb1631121b8efea3",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.15",
        sha256: "a86e13513df2e80354684daaa598719b6330fde1ff4fd9ca222ca57fc7954160",
        exact: true,
    },
    // 0.1.14: scene.frame's share and centre from a body box for actors with a head bone (finding 89), in quen.ws; the
    // ASI differs from 0.1.13's only in its version.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.14",
        sha256: "a12fb3b5d672af664ae7e31b3bf8bd79eab6d0b90df552d8f479bb394b425f1e",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.14",
        sha256: "97040312eac5b83dab98eb639f61d6514804241e56f29ad38fdf6c53e9bd33f6",
        exact: true,
    },
    // 0.1.13: the scene reads (scene.frame, scene.ground, scene.ray, nav.line), cam.noclip and the exec verb (scene.rs,
    // game_exec).
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.13",
        sha256: "4c2438d779d41b7f9ecdf7dc9906ceebd4eaaf1281f4fa2fc9fbd31a7cfd2225",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.13",
        sha256: "c1c456b639f5f4167b4044385b5cbb2147b8a90171f4c64d279957fea0bd725a",
        exact: true,
    },
    // 0.1.12: world.actors' tag and class, and each one's id and actor flag (measure_trace, measure_ab's trace).
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.12",
        sha256: "2fe14569a09db76a0c6445adb71db687044d6b9338b7f885b072522f191b3879",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.12",
        sha256: "a2caa723d08d638e2d7c5afc648b9b25ecd121f9ad693a3de2c70f16b956ff5f",
        exact: true,
    },
    // 0.1.11: world.actors' entity up to 255 characters (0.1.10 cut it at 80), and entity_cut (finding 54).
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.11",
        sha256: "7a54ad149cb4c339c6795798d3ca6a1b157f09954ce57327ddbd40e75fb08627",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.11",
        sha256: "fd9622f088568ef45f1809773bd51a8b7a728ba7b0cd88de14ef0040a45fc957",
        exact: true,
    },
    // 0.1.10 adds the items.add and world.actors verbs (game_items, world_actors, channel.rs) to both files. Re-pinned
    // in place for world.actors: 0.1.10 was never published before this.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.10",
        sha256: "2f8c0359e7935bdca896107bb0f003ee7891325d26ae1fefd76a1d060021647d",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.10",
        sha256: "f5b38b8bdf4ddb4f7c38366bff59aad4aeb662960695ac324442e678f9463f15",
        exact: true,
    },
    // 0.1.9 adds the photo.* verbs (game_photo, photo.rs) and world.get's rain, snow and sky flag to both files.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.9",
        sha256: "4a9e4c2436fe1dd13e70ffd67ebb6a35e84f8484cb3b3f8846f5fe494927fe0b",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.9",
        sha256: "c2a4da337f4f626234144590e153751a41184ba23268279543e1a1cf0e3918a3",
        exact: true,
    },
    // 0.1.8 adds the notice verb (hands_give's notice, hands.rs) to both files.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.8",
        sha256: "f5939b857a2f04f83783d91f05f367563c205296bbc1482e4fe772695b2a4aca",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.8",
        sha256: "b254797300c39f78fe5b1ca1a14559d7988f77121232d99d118ddce6f599af39",
        exact: true,
    },
    // 0.1.7 is 0.1.6 with reworded comments and two reworded reply texts (cam.set and cam.path outside photo mode).
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.7",
        sha256: "f4db50a46480acd0bcf2c813383b12ec5a0ad53a5559f92f17ec90b13f30eb0d",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.7",
        sha256: "9fc78e8715f52447194ec25652e4ab8898c83b80c512743f44b05ddbdee30730",
        exact: true,
    },
    // 0.1.6 adds the state's menu bit (game.rs close_menu) to quen.ws, and the release to W3Quen.log's natives line (in
    // a tracy launch the hook lines come before it).
    // Never published: 0.1.7 supersedes it, with the same behaviour.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.6",
        sha256: "58b1691992926683a262831b915511e927c879cc0be93199a93f449d540aa3ae",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.6",
        sha256: "e71c3d00b2b48b7bdafe4034f7a9a49ff3a75d61acfaaa9342195b2f201fa5d6",
        exact: true,
    },
    // 0.1.5 adds the world.get verb (world_get, channel.rs) to both files.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.5",
        sha256: "76b3d4dbb7bd2b7336f14175bd0de60d9e5b04885b5262328ec94533a8cdb811",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.5",
        sha256: "063ff51f9a5ce56fc6fa919881b6d821d2d7cb178a4806ff6e1f42595d7a2a6e",
        exact: true,
    },
    // 0.1.4 adds the save verb (game_save, save.rs) to both files.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.4",
        sha256: "99f340b66208bdd481ae3c20d62da52d63ab21c11e713f594a0cf35205ac8d66",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.4",
        sha256: "4ccbb9319f9f7d83f34133ea30df2b5cdbea403a5a3a6d8360ba7a7aa3f8fa61",
        exact: true,
    },
    // 0.1.3 adds the Tracy instrumentation, dormant outside a tracy launch (profile.rs); its quen.ws is 0.1.2's.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.3",
        sha256: "5e33b4f4ad47a1003d5664863c24f8851959ae888f6aa72c1eeb1f2b483f8a28",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.3",
        sha256: "11c6aed2d755382cc9f8ae2b90586fef10cbd3c6076a2d7d5bca8740e84806dc",
        exact: true,
    },
    // 0.1.2 adds the camera paths (cam.path, cam.frame, cam.stop): game_camera works with either, sweep_play with it.
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.2",
        sha256: "c14d291cf14724ee7b374c80edd449e196c49a975c9cdf286b04f383e0f8bb1e",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.2",
        sha256: "11c6aed2d755382cc9f8ae2b90586fef10cbd3c6076a2d7d5bca8740e84806dc",
        exact: true,
    },
    Pin {
        file: PACKAGE_ASI,
        version: "quen-w3 0.1.1",
        sha256: "87b86bc273690255269891634d9197396e254cbf18bf934bdc6919bc213729c7",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: "quen-w3 0.1.1",
        sha256: "ee271b148fa516cc3957931948c382d516ff4ba73cda8957a4cbb180642ff308",
        exact: true,
    },
    Pin {
        file: PACKAGE_SCRIPT,
        version: OLD_MODQUEN,
        sha256: "61c1a409b0cfe8d9feb3ec5c2de1ec64bb2ed606089be453a5bb60702479c7fd",
        exact: true,
    },
    // Exact: a renderdoc launch swaps out only this file, and its end must give back the same bytes.
    Pin {
        file: INTERPOSER,
        version: "NVIDIA Streamline 2.14.1.0, signed production build (5.01)",
        sha256: "8c87c9499461da561edd529aa9bf7831d67d7b94ebb1c1a5ed54ef4934e1ea4c",
        exact: true,
    },
];

/// The package releases from before game_save's `save` verb (quen-w3 0.1.4): game_save refuses them with needs.
pub(crate) const BEFORE_SAVE: &[&str] = &["quen-w3 0.1.1", "quen-w3 0.1.2", "quen-w3 0.1.3"];
/// The package releases from before world_get's `world.get` verb (quen-w3 0.1.5).
pub(crate) const BEFORE_WORLD: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
];
/// The package releases from before the state's menu bit (quen-w3 0.1.6): bringing the game back can't see its pause
/// menu (game.rs `Ctx::close_menu`).
pub(crate) const BEFORE_MENU: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
];

/// The package releases from before the notice verb (quen-w3 0.1.8): hands_give shows no notice in the game.
pub(crate) const BEFORE_NOTICE: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
];

/// The package releases from before the photo-mode verbs (quen-w3 0.1.9): game_photo refuses them with needs.
pub(crate) const BEFORE_PHOTO: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
];

/// The package releases from before the items.add and world.actors verbs (quen-w3 0.1.10): game_items and world_actors
/// refuse them with needs.
pub(crate) const BEFORE_ITEMS: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
];

/// The package releases from before world.actors' tag, class and ids (quen-w3 0.1.12): world_actors with tag or class,
/// and a trace that needs them, refuse them with needs.
pub(crate) const BEFORE_ENTITIES: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
    "quen-w3 0.1.10",
    "quen-w3 0.1.11",
];

/// The package releases from before the scene verbs, cam.noclip and exec (quen-w3 0.1.13): world_frame, world_ground,
/// world_ray, world_nav, world_map, game_frame, game_place and game_exec refuse them with needs.
pub(crate) const BEFORE_SCENE: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
    "quen-w3 0.1.10",
    "quen-w3 0.1.11",
    "quen-w3 0.1.12",
];

/// The package releases from before notice's duration and the notice.hide and sound verbs (quen-w3 0.1.15): the status
/// line shows at transitions only, for 8 s, and captures wait until 9 s after it (status.rs).
pub(crate) const BEFORE_STATUS: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
    "quen-w3 0.1.10",
    "quen-w3 0.1.11",
    "quen-w3 0.1.12",
    "quen-w3 0.1.13",
    "quen-w3 0.1.14",
];

/// The package releases from before the free-camera verbs (quen-w3 0.1.17): game_freecam refuses them with needs.
pub(crate) const BEFORE_FREECAM: &[&str] = &[
    "quen-w3 0.1.1",
    "quen-w3 0.1.2",
    "quen-w3 0.1.3",
    "quen-w3 0.1.4",
    "quen-w3 0.1.5",
    "quen-w3 0.1.6",
    "quen-w3 0.1.7",
    "quen-w3 0.1.8",
    "quen-w3 0.1.9",
    "quen-w3 0.1.10",
    "quen-w3 0.1.11",
    "quen-w3 0.1.12",
    "quen-w3 0.1.13",
    "quen-w3 0.1.14",
    "quen-w3 0.1.15",
    "quen-w3 0.1.16",
];

/// Whether measure_ab's console actions go as exec verbs: only for a pinned package with the verb (an unpinned dev
/// build may lack it, so its lines are typed, as before).
pub(crate) fn has_exec(version: Option<&str>) -> bool {
    version.is_some_and(|v| !BEFORE_SCENE.contains(&v))
}

/// Refused with needs, nothing sent, while quen's package in the game is one of `before`: releases without what
/// `tool` needs, which came in quen-w3 `since`. An unpinned package (a dev build) goes on: its ASI refuses a verb it
/// doesn't know.
pub(crate) fn package_at_least(
    config: &Config,
    before: &[&str],
    tool: &str,
    since: &str,
) -> Result<(), Done> {
    let Some(old) = package_version(config).filter(|v| before.contains(v)) else {
        return Ok(());
    };
    let mut why =
        format!("quen's package in the game is {old}: {tool} needs quen-w3 {since} or later");
    let inv = crate::vortex::read(config);
    let game = config.game_dir.as_deref();
    if let Some(m) = game.and_then(|g| vortex_owner(inv.as_ref(), g, &PACKAGE_OWNED)) {
        why.push_str(&format!(", and it is Vortex's ({m}): update it in Vortex"));
    }
    let need = json!({
        "dependency": PACKAGE.name,
        "status": "wrong_version",
        "needed_for": tool,
        "how_to_get": PACKAGE.how_to_get,
    });
    Err(Done::Needs(why, vec![need]))
}

/// [`PINS`], plus the exact pins `QUEN_TEST_PINS` lists (`file=sha256`, `;` between them; version "test"): the
/// end-to-end tests' synthetic parts. Read once.
fn pins() -> &'static [Pin] {
    static IN_USE: OnceLock<Vec<Pin>> = OnceLock::new();
    IN_USE.get_or_init(|| {
        let test = env::var("QUEN_TEST_PINS").unwrap_or_default();
        let test = test.split(';').filter_map(|e| e.split_once('='));
        let test = test.map(|(file, sha256)| Pin {
            file: String::leak(file.into()),
            version: "test",
            sha256: String::leak(sha256.into()),
            exact: true,
        });
        PINS.iter().copied().chain(test).collect()
    })
}

/// The Vortex mods whose deployment manifests (`inv`) list a file of `rels` (in the game folder `game`), None when none
/// does: then quen's installer or the user installed it. What Vortex owns is updated in Vortex; install.ps1 leaves it.
fn vortex_owner(inv: Option<&Inventory>, game: &Path, rels: &[&str]) -> Option<String> {
    let owners = rels
        .iter()
        .flat_map(|rel| inv.map(|inv| inv.owners(&game.join(rel))));
    let mut mods: Vec<String> = owners.flatten().map(|m| m.id.display()).collect();
    mods.sort();
    mods.dedup();
    (!mods.is_empty()).then(|| mods.join(" + "))
}

/// The version of quen's package in the game: its W3Quen.asi's pin, None when it has none.
pub(crate) fn package_version(config: &Config) -> Option<&'static str> {
    let asi = fs::read(config.game_dir.as_ref()?.join(PACKAGE_ASI)).ok()?;
    let sha = sha256_hex(&asi);
    let pin = pins()
        .iter()
        .find(|p| p.file == PACKAGE_ASI && p.sha256 == sha);
    pin.map(|p| p.version)
}

/// The sha256s of NVIDIA's signed interposer that quen accepts (its pins, test pins included).
pub(crate) fn signed_interposer() -> Vec<&'static str> {
    pins()
        .iter()
        .filter(|p| p.file == INTERPOSER)
        .map(|p| p.sha256)
        .collect()
}

/// One dependency, as quen_doctor lists it and a refused tool names it in `needs`.
pub(crate) struct Dep {
    pub(crate) name: &'static str,
    /// Needed for any session; the others each enable some tools.
    required: bool,
    enables: &'static str,
    pub(crate) how_to_get: &'static str,
    check: fn(&Cx) -> State,
}

pub(crate) const GAME: Dep = Dep {
    name: "The Witcher 3 5.01 (DX12)",
    required: true,
    enables: "every session and in-game tool",
    how_to_get: "The Witcher 3: Wild Hunt from Steam or GOG, at game version 5.01; with Steam, set its Properties > \
        Updates to 'Only update this game when I launch it'. quen_setup records where it is.",
    check: game,
};

pub(crate) const NATIVE_BRIDGE: Dep = Dep {
    name: "Witcher Native Bridge 2.0",
    required: true,
    enables: "the script functions of quen's in-game package",
    how_to_get: "Witcher Native Bridge on Nexus Mods (The Witcher 3, mod 13743): WitcherNativeBridge.asi and the \
        ASI loader in its download (dinput8.dll; an Ultimate ASI Loader named winmm.dll works too). It needs the \
        Microsoft Visual C++ Redistributable (x64).",
    check: |cx| {
        let loader = [r"bin\x64_dx12\dinput8.dll", r"bin\x64_dx12\winmm.dll"]
            .into_iter()
            .find(|rel| cx.game_dir.is_some_and(|game| game.join(rel).exists()))
            .unwrap_or(r"bin\x64_dx12\dinput8.dll");
        cx.files(&[r"bin\x64_dx12\WitcherNativeBridge.asi", loader])
    },
};

pub(crate) const PACKAGE: Dep = Dep {
    name: "quen in-game package",
    required: true,
    enables: "every session: quen's game state (session_launch waits for gameplay by it, game_load confirms a load \
        by it, game_console and game_keys focus check it), and its command channel: game_load and a launch of a save \
        that isn't the newest, game_listsaves, game_pause, game_settime, game_weather, game_teleport, game_camera \
        and (from quen-w3 0.1.2) sweep_play; from quen-w3 0.1.3, session_launch profiler tracy:NAME; from quen-w3 \
        0.1.4, game_save; from quen-w3 0.1.5, world_get; from quen-w3 0.1.6, the menu focus to game closes; from \
        quen-w3 0.1.8, hands_give's notice in the game; from quen-w3 0.1.9, game_photo; from quen-w3 0.1.10, \
        game_items and world_actors; from quen-w3 0.1.11, world_actors' entity up to 255 characters; from quen-w3 \
        0.1.12, world_actors' tag, class and ids (and measure_trace and measure_ab traces with them); from quen-w3 \
        0.1.13, world_frame, world_ground, world_ray, world_nav, world_map, game_frame, game_place and game_exec (and \
        measure_ab's console actions without typing); from quen-w3 0.1.14, world_frame's and game_frame's share and \
        x, y from a box around an actor's body; from quen-w3 0.1.15, the lasting status line in a test session's game and \
        its 0.5 s hide before captures and windows; from quen-w3 0.1.16, that line in the game's HUD where it is drawn; \
        from quen-w3 0.1.17, game_freecam; from quen-w3 0.1.18, world_frame under the free camera",
    how_to_get: "quen-w3-<version>.zip from the GitHub Releases of quen-mcp, unzipped into the game folder: \
        bin\\x64_dx12\\W3Quen.asi and Mods\\modQuen. Move an old Mods\\modQuen and any Mods\\modQuenRpc to the \
        Recycle Bin first. It needs the Witcher Native Bridge.",
    check: package,
};

/// quen's package: W3Quen.asi and quen.ws from one release, with nothing left of the old layout. Anything else
/// breaks the game's script compile at boot or calls functions the other half lacks, so it is the wrong version, with
/// how to fix it.
fn package(cx: &Cx) -> State {
    let Some(game) = cx.game_dir else {
        return State::missing("no game folder: run quen_setup".into());
    };
    let there = |rel: &str| game.join(rel).exists();
    let sha = fs::read(game.join(PACKAGE_SCRIPT))
        .map(|b| sha256_hex(&b))
        .ok();
    // Every release that ships these script bytes: a release may keep the last one's quen.ws (0.1.3 kept 0.1.2's).
    let scripts: Vec<&str> = cx
        .pins
        .iter()
        .filter(|p| p.file == PACKAGE_SCRIPT && Some(p.sha256) == sha.as_deref())
        .map(|p| p.version)
        .collect();
    let broken = if there(r"Mods\modQuenRpc") {
        Some(
            "Mods\\modQuenRpc is left from quen's old layout: modQuen has its functions now, and the two together \
             break the game's script compile. Move Mods\\modQuenRpc to the Recycle Bin",
        )
    } else if scripts.contains(&OLD_MODQUEN) {
        Some(
            "Mods\\modQuen is the old pure-script modQuen: replace it with the release package (move Mods\\modQuen \
             to the Recycle Bin, then unzip the package into the game folder)",
        )
    } else if there(PACKAGE_SCRIPT) && !there(PACKAGE_ASI) {
        Some(
            "Mods\\modQuen without bin\\x64_dx12\\W3Quen.asi, half the package: the game's script compile fails at \
             boot. Unzip the whole release package into the game folder, or move Mods\\modQuen to the Recycle Bin",
        )
    } else if there(PACKAGE_ASI) && !there(PACKAGE_SCRIPT) {
        Some(
            "bin\\x64_dx12\\W3Quen.asi without Mods\\modQuen, half the package. Unzip the whole release package \
             into the game folder, or move W3Quen.asi to the Recycle Bin",
        )
    } else {
        None
    };
    if let Some(why) = broken {
        return State {
            status: "wrong_version",
            ..State::missing(why.into())
        };
    }
    let mut state = cx.files(&[PACKAGE_ASI, PACKAGE_SCRIPT]);
    if state.status == "found"
        && !state
            .version
            .as_deref()
            .is_some_and(|v| scripts.contains(&v))
    {
        state.status = "wrong_version";
        let script = match scripts.is_empty() {
            true => "unpinned".to_owned(),
            false => scripts.join(" or "),
        };
        state.note = Some(format!(
            "W3Quen.asi ({}) and quen.ws ({script}) are from different releases",
            state.version.as_deref().unwrap_or("unpinned"),
        ));
    }
    // From quen-w3 0.1.16 its release's HUD bundle too (PACKAGE_HUD), when the ASI's release has pins for it.
    let version = state.version.clone().unwrap_or_default();
    let mut hud = cx
        .pins
        .iter()
        .filter(|p| PACKAGE_HUD.contains(&p.file) && p.version == version);
    let stale = hud.find(|p| {
        fs::read(game.join(p.file))
            .map(|b| sha256_hex(&b))
            .ok()
            .as_deref()
            != Some(p.sha256)
    });
    if let Some(pin) = stale.filter(|_| state.status == "found") {
        state.status = "wrong_version";
        state.note = Some(format!("{} is not {version}'s", pin.file));
    }
    if state.status == "wrong_version" {
        state.note = state
            .note
            .map(|n| format!("{n}: unzip one release package into the game folder"));
    }
    state
}

pub(crate) const PRESENTMON: Dep = Dep {
    name: "PresentMon 2.x (console)",
    required: false,
    enables: "measure_presentmon, measure_ab",
    how_to_get: "Intel PresentMon 2.x, the console application: https://github.com/GameTechDev/PresentMon/releases. \
        Set presentmon in quen's config to its exe (quen_setup), or put a PresentMon.exe on PATH. Your Windows \
        account must be in the Performance Log Users group.",
    check: presentmon,
};

pub(crate) const CAPTURE: Dep = Dep {
    name: "Windows Graphics Capture",
    required: false,
    enables: "game_screenshot method hdr (the game's window alone, in FP16)",
    how_to_get: "part of Windows 10 (1903) and later. When it is unavailable, Windows usually needs \
        repairing: install pending Windows updates, then run sfc /scannow and DISM /Online \
        /Cleanup-Image /RestoreHealth in a terminal opened as administrator. On Windows 11, \
        Settings > System > Recovery > Fix problems using Windows Update reinstalls Windows and \
        keeps files and apps.",
    check: |cx| {
        cx.probe(
            crate::capture::probe_wgc,
            "game_screenshot falls back to Desktop Duplication",
        )
    },
};

pub(crate) const DUPLICATION: Dep = Dep {
    name: "DXGI Desktop Duplication",
    required: false,
    enables: "game_screenshot method hdr when Windows Graphics Capture is unavailable (its automatic fallback, \
        only while the game is in front)",
    how_to_get: "part of Windows 8 and later. Windows refuses it on the lock screen, behind an \
        administrator (UAC) prompt and in a remote desktop session; on a laptop with two GPUs, \
        the display's GPU must run the game.",
    check: |cx| cx.probe(crate::capture::probe_duplication, "no FP16 screen capture"),
};

pub(crate) const RESHADE: Dep = Dep {
    name: "ReShade (as the RenoDX add-on host)",
    required: false,
    enables: "loading the RenoDX add-on and devkit",
    how_to_get: "ReShade with full add-on support, https://reshade.me, installed for bin\\x64_dx12\\witcher3.exe \
        (DirectX 10/11/12: dxgi.dll)",
    check: |cx| {
        let mut state = cx.files(&[r"bin\x64_dx12\dxgi.dll"]);
        if let Some(dll) = state.path.clone() {
            state.version = crate::win::file_version(&dll);
            if !dll.with_file_name("ReShade.ini").is_file() {
                state.note = Some(
                    "no ReShade.ini beside dxgi.dll: ReShade never ran, or this dxgi.dll is another tool".into(),
                );
            }
        }
        state
    },
};

pub(crate) const RENODX: Dep = Dep {
    name: "RenoDX add-on for The Witcher 3",
    required: false,
    enables: "session_launch preset, and ReShade.ini set up for it (with renodx_addon in quen's config)",
    how_to_get: "RenoDX, https://github.com/clshortfuse/renodx: renodx-thewitcher3remastered.addon64 in \
        bin\\x64_dx12 (needs ReShade)",
    check: |cx| {
        let configured = cx.config.renodx_addon.as_deref();
        let name = configured.unwrap_or("renodx-thewitcher3remastered.addon64");
        let mut state = cx.files(&[format!(r"bin\x64_dx12\{name}").as_str()]);
        if state.status == "found" && configured.is_none() {
            state.note =
                Some("not in quen's config (renodx_addon): launches leave RenoDX alone".into());
        }
        if let (Some(build), Some(sha)) = (&cx.config.renodx_build, &state.sha256)
            && fs::read(build).map(|b| sha256_hex(&b)).ok().as_ref() != Some(sha)
        {
            state.status = "wrong_version";
            state.note = Some(format!(
                "not the build renodx_build names ({}): a launch refuses it",
                build.display()
            ));
        }
        state
    },
};

pub(crate) const DEVKIT: Dep = Dep {
    name: "RenoDX devkit",
    required: false,
    enables: "session_launch devkit: true (with devkit in quen's config)",
    how_to_get: "the RenoDX devkit add-on, built from https://github.com/clshortfuse/renodx, e.g. at \
        bin\\x64_dx12\\devkit\\renodx-devkit.addon64 (needs ReShade)",
    check: |cx| {
        let configured = cx.config.devkit.as_deref();
        let rel = configured.unwrap_or(r"devkit\renodx-devkit.addon64");
        let mut state = cx.files(&[format!(r"bin\x64_dx12\{rel}").as_str()]);
        if state.status == "found" && configured.is_none() {
            state.note = Some("not in quen's config (devkit): launches don't load it".into());
        }
        state
    },
};

pub(crate) const NSIGHT_SYSTEMS: Dep = Dep {
    name: "NVIDIA Nsight Systems",
    required: false,
    enables: "session_launch profiler nsys:NAME (with nsys in quen's config)",
    how_to_get: "NVIDIA Nsight Systems: https://developer.nvidia.com/nsight-systems",
    check: |cx| cx.profiler(&cx.config.nsys, "nsys"),
};

pub(crate) const NSIGHT_GRAPHICS: Dep = Dep {
    name: "NVIDIA Nsight Graphics",
    required: false,
    enables: "session_launch profiler ngfx:NAME (with ngfx in quen's config)",
    how_to_get: "NVIDIA Nsight Graphics: https://developer.nvidia.com/nsight-graphics",
    check: |cx| cx.profiler(&cx.config.ngfx, "ngfx"),
};

pub(crate) const NGFX_REPLAY: Dep = Dep {
    name: "NVIDIA Nsight Graphics replayer (ngfx-replay)",
    required: false,
    enables: "ngfx_extract (a capture's metadata, final image, calls, objects, log errors) and ngfx_replay",
    how_to_get: "part of NVIDIA Nsight Graphics (https://developer.nvidia.com/nsight-graphics): ngfx-replay.exe beside \
        the ngfx.exe quen's config names (quen_setup). Nsight asks a privacy question once per version: the user \
        opens Nsight Graphics once and answers it; quen only reads the answer",
    check: ngfx_replay,
};

/// ngfx-replay.exe beside the configured ngfx.exe (only that install runs, never a capture's bundled replayer), its
/// version, and whether that version of Nsight asked its first-start privacy question: read from Nsight's settings
/// file, never written.
fn ngfx_replay(cx: &Cx) -> State {
    let Some(ngfx) = &cx.config.ngfx else {
        return State::missing("no ngfx in quen's config: quen_setup sets it".into());
    };
    let exe = ngfx.with_file_name("ngfx-replay.exe");
    if !exe.is_file() {
        return State::missing(format!("no ngfx-replay.exe beside {}", ngfx.display()));
    }
    let version = crate::win::file_version(&exe);
    let ini = env::var_os("APPDATA")
        .map(|d| PathBuf::from(d).join(r"NVIDIA Corporation\NVIDIA Nsight Graphics.ini"));
    let asked = ini.as_ref().map(fs::read_to_string);
    let note = match (&version, asked) {
        (None, _) => "no version resource: Nsight's first-start question can't be checked".into(),
        (_, None | Some(Err(_))) => {
            "Nsight's settings file wasn't found: it hasn't asked its first-start question yet"
                .into()
        }
        (Some(v), Some(Ok(ini))) if quen_core::ngfx::privacy_asked(&ini, v) => {
            format!("Nsight {v} asked its first-start privacy question")
        }
        (Some(v), Some(Ok(_))) => format!(
            "Nsight {v} hasn't asked its first-start privacy question yet: the user opens Nsight Graphics once and \
             answers it (quen never answers it)"
        ),
    };
    State {
        version,
        note: Some(note),
        ..State::found(exe)
    }
}

pub(crate) const GPU_COUNTERS: Dep = Dep {
    name: "NVIDIA GPU performance counters for all users",
    required: false,
    enables: "session_launch profiler ngfx:NAME: GPU Trace reads them, and says \"GPU Performance Counters \
        unavailable\" while the driver keeps them to administrators",
    how_to_get: "a system setting the user changes, never quen or its agents: NVIDIA Control Panel > Desktop > \
        Enable Developer Settings, then Developer > Manage GPU Performance Counters > Allow access to the GPU \
        performance counters to all users",
    check: |cx| {
        if !cx.probes {
            return State::missing("not probed".into());
        }
        counters(profiling_admin_only())
    },
};

/// The NVIDIA driver's `RmProfilingAdminOnly`, which the Control Panel's counter setting writes (0: all users), and
/// where it was: the driver's service parameters first, then the display adapters' class keys.
/// ponytail: which copy the driver reads is inferred; on the one PC checked both held 0 after the user allowed it.
fn profiling_admin_only() -> Option<(u32, String)> {
    const VALUE: &str = "RmProfilingAdminOnly";
    const SERVICE: &str = r"SYSTEM\CurrentControlSet\Services\nvlddmkm\Parameters\Global\NVTweak";
    const DISPLAYS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let adapters = crate::win::reg_subkeys(DISPLAYS)
        .into_iter()
        .map(|sub| format!(r"{DISPLAYS}\{sub}"));
    std::iter::once(SERVICE.to_owned())
        .chain(adapters)
        .find_map(|key| Some((crate::win::reg_dword(true, &key, VALUE)?, key)))
}

/// The counters' state from [`profiling_admin_only`] (read-only: the setting is the user's to change).
fn counters(admin_only: Option<(u32, String)>) -> State {
    match admin_only {
        Some((0, key)) => State {
            path: None,
            note: Some(format!(r"allowed for all users (HKLM\{key} RmProfilingAdminOnly = 0)")),
            ..State::found(PathBuf::new())
        },
        Some((n, key)) => State::missing(format!(
            r"administrators only (HKLM\{key} RmProfilingAdminOnly = {n}): GPU Trace can't read them"
        )),
        None => State::missing(
            "RmProfilingAdminOnly is not set: the driver's default, administrators only (or no NVIDIA driver)"
                .into(),
        ),
    }
}

pub(crate) const RENDERDOC: Dep = Dep {
    name: "RenderDoc",
    required: false,
    enables: "session_launch profiler renderdoc:NAME and game_screenshot method renderdoc (with renderdoc and \
        streamline_dev_interposer in quen's config)",
    how_to_get: "RenderDoc: https://renderdoc.org; quen's config names its renderdoccmd.exe",
    check: |cx| cx.profiler(&cx.config.renderdoc, "renderdoc"),
};

pub(crate) const TRACY: Dep = Dep {
    name: "Tracy's capture tool (tracy-capture), 0.14.1",
    required: false,
    enables: "session_launch profiler tracy:NAME (with tracy_capture in quen's config, and quen-w3 0.1.3 or later in the game)",
    how_to_get: "Tracy 0.14.1's Windows release (https://github.com/wolfpld/tracy/releases, v0.14.1): its \
        tracy-capture.exe, at the version of the Tracy client in quen's package; quen's config names it \
        (tracy_capture)",
    check: |cx| {
        let Some(exe) = &cx.config.tracy_capture else {
            return State::missing("no tracy_capture in quen's config".into());
        };
        if !exe.is_file() {
            return State::missing(format!(
                "tracy_capture {} in quen's config is not a file",
                exe.display()
            ));
        }
        let found = State::found(exe.clone());
        match crate::profile::tracy_version(exe) {
            Ok(v) if v == crate::profile::TRACY_CLIENT => State {
                version: Some(v),
                ..found
            },
            Ok(v) => State {
                status: "wrong_version",
                note: Some(format!(
                    "Tracy {v}: quen's package has Tracy {}'s client, and both ends must be the same version",
                    crate::profile::TRACY_CLIENT
                )),
                version: Some(v),
                ..found
            },
            Err(why) => State {
                status: "wrong_version",
                note: Some(why),
                ..found
            },
        }
    },
};

pub(crate) const STREAMLINE: Dep = Dep {
    name: "the game's Streamline interposer (NVIDIA's signed sl.interposer.dll)",
    required: false,
    enables: "session_launch profiler renderdoc:NAME, which swaps it for the dev build only after checking it by \
        sha256 and Authenticode, and checks it the same way once it is back",
    how_to_get: "part of the game. Another file in its place (a dev build a crashed session left): session_recover \
        puts the original back, or Steam's Verify integrity of game files",
    check: |cx| {
        let mut state = cx.files(&[INTERPOSER]);
        if let (true, "found", Some(path)) = (cx.probes, state.status, &state.path)
            && let Err(why) = crate::win::authenticode(path)
        {
            state.status = "wrong_version";
            state.note = Some(format!("its Authenticode signature doesn't verify: {why}"));
        }
        state
    },
};

pub(crate) const STREAMLINE_DEV: Dep = Dep {
    name: "Streamline dev interposer (patched, for RenderDoc)",
    required: false,
    enables: "session_launch profiler renderdoc:NAME (with streamline_dev_interposer in quen's config)",
    how_to_get: "quen-streamline-dev-interposer-2.14.1.zip from quen-mcp's GitHub Releases (the DLL with \
        Streamline's licence and third-party notices), or build it from Streamline 2.14.1 with quen's two-line patch \
        (tools/streamline/README.md in quen-mcp); quen's config names the sl.interposer.dll",
    check: |cx| {
        let Some(dll) = &cx.config.streamline_dev_interposer else {
            return State::missing("no streamline_dev_interposer in quen's config".into());
        };
        let Ok(bytes) = fs::read(dll) else {
            return State::missing(format!(
                "streamline_dev_interposer {} in quen's config is not a file",
                dll.display()
            ));
        };
        let sha = sha256_hex(&bytes);
        let production = cx
            .pins
            .iter()
            .any(|p| p.file == INTERPOSER && p.sha256 == sha);
        // The dev build is unsigned: a signature that verifies is a signed build, NVIDIA's.
        let signed = !production && cx.probes && crate::win::authenticode(dll).is_ok();
        let note = match () {
            _ if production => "NVIDIA's signed production build, not the patched dev build",
            _ if signed => {
                "its Authenticode signature verifies: a signed build, not the patched dev build"
            }
            _ if sha == DEV_INTERPOSER_RELEASE => "quen's release build",
            _ => "built locally (not quen's release build)",
        };
        State {
            status: if production || signed {
                "wrong_version"
            } else {
                "found"
            },
            note: Some(note.into()),
            version: crate::win::file_version(dll),
            sha256: Some(sha),
            ..State::found(dll.clone())
        }
    },
};

pub(crate) const KB_CVARS: Dep = Dep {
    name: "the RE knowledge base's cvar layout (cvar_layout.tsv in kb)",
    required: false,
    enables: "game_cvar and measure_ab's cvar actions: the config var layout per game build",
    how_to_get: "the re folder of witcher3-mods (not published yet), which quen's config names as kb",
    check: |cx| kb_table(cx, "cvar_layout.tsv"),
};

pub(crate) const KB_PATCHES: Dep = Dep {
    name: "the RE knowledge base's patch table (patches.tsv in kb)",
    required: false,
    enables: "mem_patch and mem_unpatch: the checked live patches per game build",
    how_to_get: "the re folder of witcher3-mods (not published yet), which quen's config names as kb",
    check: |cx| kb_table(cx, "patches.tsv"),
};

pub(crate) const KB_INSTANCES: Dep = Dep {
    name: "the RE knowledge base's instance patch table (instance_patches.tsv in kb)",
    required: false,
    enables: "mem_patch_instances: a field written in every live instance of a class, per game build",
    how_to_get: "the re folder of witcher3-mods (not published yet), which quen's config names as kb",
    check: |cx| kb_table(cx, "instance_patches.tsv"),
};

pub(crate) const KB_BUILD: Dep = Dep {
    name: "the RE knowledge base's build (the build line of names.tsv and hooks.tsv in kb)",
    required: false,
    enables: "the mem_* tools and game_cvar: the KB's names, RVAs and layouts are one build's, so while its tables \
        name a build, those tools refuse an exe of another",
    how_to_get: "a KB made on the game's build: its names.tsv and hooks.tsv each name it on one \
        `# build: sha256=<64 hex> stamp=0x<8 hex>` line. A game update needs tables of its own",
    check: kb_build_state,
};

pub(crate) const KB_INDEX: Dep = Dep {
    name: "the ledger's index of the RE knowledge base (quen kb scan)",
    required: false,
    enables: "ledger_about's KB sections and mentions, and the KB's names, hooks and patches as entities",
    how_to_get: "quen kb scan after each commit to the KB (the maintainer runs it), or ledger_import kind kb",
    check: kb_index_state,
};

/// The saves game_save made that are still in gamesaves, by the ledger (schema 0010), oldest first: each stays in the
/// user's save list until the user deletes it. A ledger from before 0010 has none.
fn quen_saves(config: &Config) -> Value {
    let db = config.state_dir().join("quen.db");
    let Some(dir) = config.saves_dir().filter(|_| db.exists()) else {
        return json!([]);
    };
    let read = |l: &Ledger, sql: &str| l.query(sql, &[], Duration::from_secs(5));
    let rows = Ledger::open_readonly(&db).and_then(|l| {
        let has = "SELECT 1 FROM pragma_table_info('saves') WHERE name = 'made_by_boot'";
        if read(&l, has)?.rows.is_empty() {
            return Ok(Vec::new());
        }
        let sql = "SELECT file, made_by_boot, tainted_by_boot IS NOT NULL FROM saves
                   WHERE made_by_boot IS NOT NULL ORDER BY id";
        Ok(read(&l, sql)?.rows)
    });
    match rows {
        Ok(rows) => rows
            .into_iter()
            .filter(|r| r[0].as_str().is_some_and(|file| dir.join(file).is_file()))
            .map(|r| json!({"file": r[0], "boot": r[1], "tainted": r[2] == 1}))
            .collect(),
        Err(e) => json!(format!("the ledger can't be read: {e}")),
    }
}

/// The measurement rows whose method family is outside the playbook's list, by family: written before quen refused
/// them (finding 32), reported and never rewritten. Empty when there are none.
fn unknown_families(config: &Config) -> Value {
    let db = config.state_dir().join("quen.db");
    if !db.exists() {
        return json!([]);
    }
    match Ledger::open_readonly(&db).and_then(|l| l.unknown_families()) {
        Ok(rows) => rows
            .into_iter()
            .map(|(family, ids)| json!({"family": family, "rows": ids.len(), "ids": ids}))
            .collect(),
        Err(e) => json!(format!("the ledger can't be read: {e}")),
    }
}

/// Whether the ledger's KB index is of the commit the KB has checked out: its `.git` files read, no git process.
fn kb_index_state(cx: &Cx) -> State {
    let Some(kb) = &cx.config.kb else {
        return State::missing("no kb in quen's config".into());
    };
    let Some(head) = git_head(kb) else {
        return State::missing(format!(
            "kb {} isn't in a git work tree with a commit",
            kb.display()
        ));
    };
    let db = cx.config.state_dir().join("quen.db");
    let index = match db.exists() {
        true => Ledger::open_readonly(&db).and_then(|l| l.kb_index()),
        false => Ok(None),
    };
    match index {
        Err(e) => State::missing(format!("the ledger's KB index can't be read: {e}")),
        Ok(None) => State::missing("the KB was never scanned: quen kb scan".into()),
        Ok(Some((commit, at))) => State {
            status: if commit == head {
                "found"
            } else {
                "wrong_version"
            },
            version: Some(format!("commit {commit}, scanned {at}")),
            note: (commit != head).then(|| {
                format!(
                    "the index is of commit {commit}; the KB has {head} checked out: quen kb scan"
                )
            }),
            ..State::found(kb.clone())
        },
    }
}

/// The KB's tables that name the build they were made on.
const KB_BUILT: [&str; 2] = ["names.tsv", "hooks.tsv"];

/// The build the KB's tables name: `None` while none of them has a build line (a KB not pinned yet: nothing is checked).
/// Once one has it, a table there without one or with a malformed one is refused, and two that name different builds.
pub(crate) fn kb_build(kb: &Path) -> Result<Option<Build>, String> {
    let tables: Vec<(&str, Result<Build, String>)> = KB_BUILT
        .iter()
        .filter_map(|&file| Some((file, Build::of_kb(&fs::read_to_string(kb.join(file)).ok()?))))
        .collect();
    // ponytail: "no build line" is Build::of_kb's own error for a table without one.
    let unpinned =
        |b: &Result<Build, String>| b.as_ref().is_err_and(|e| e.starts_with("no build line"));
    if tables.iter().all(|(_, b)| unpinned(b)) {
        return Ok(None);
    }
    let mut named: Option<(&str, Build)> = None;
    for (file, build) in tables {
        let build = build.map_err(|e| format!("{}: {e}", kb.join(file).display()))?;
        match &named {
            Some((first, b)) if *b != build => {
                return Err(format!(
                    "the KB's {first} is for {b}, but its {file} for {build}: one KB, one build"
                ));
            }
            Some(_) => {}
            None => named = Some((file, build)),
        }
    }
    Ok(named.map(|(_, b)| b))
}

/// Refuses unless the config's KB names no build, or names `exe`'s: names and RVAs from one build never reach
/// another. The mem_* tools and game_cvar check the build the session's launch read.
pub(crate) fn kb_matches(config: &Config, exe: &Build) -> Result<(), String> {
    match config.kb.as_deref().map(kb_build).transpose()?.flatten() {
        Some(kb) => kb.check_exe(exe),
        None => Ok(()),
    }
}

fn kb_build_state(cx: &Cx) -> State {
    let Some(kb) = &cx.config.kb else {
        return State::missing("no kb in quen's config".into());
    };
    if !kb.is_dir() {
        return State::missing(format!("kb {} is not a folder", kb.display()));
    }
    let named = match kb_build(kb) {
        Ok(Some(named)) => named,
        Ok(None) => {
            return State::missing(format!(
                "no build line in {}: the KB isn't checked against the exe",
                KB_BUILT.join(" or ")
            ));
        }
        Err(why) => {
            return State {
                status: "wrong_version",
                note: Some(why),
                ..State::found(kb.clone())
            };
        }
    };
    let checked = cx
        .game_build()
        .map_err(|e| format!("the game's exe: {e}"))
        .and_then(|game| named.check_exe(&game.build));
    State {
        status: if checked.is_ok() {
            "found"
        } else {
            "wrong_version"
        },
        version: Some(format!("PE stamp {}", named.stamp)),
        sha256: Some(named.sha256),
        note: checked.err(),
        ..State::found(kb.clone())
    }
}

pub(crate) const REDKIT: Dep = Dep {
    name: "The Witcher 3 REDkit",
    required: false,
    enables: "script_compile (its wcc_lite), and data_find, data_read and data_scan with where: redkit (its depot, \
        r4data)",
    how_to_get: "The Witcher 3 REDkit on Steam (free, app 2684660), installed by you: its licence (wcc_eula.rtf beside \
        wcc_lite.exe) is between you and CD PROJEKT RED, and quen never installs it. You accept it with Agree in \
        REDkit's own licence dialog, which quen_setup licence: true opens; quen never answers it. quen's config names \
        its folder (redkit; quen_setup finds it in Steam's libraries)",
    check: |cx| {
        let Some(dir) = &cx.config.redkit else {
            return State::missing("no redkit in quen's config".into());
        };
        let exe = dir.join(crate::data::WCC_LITE);
        if !exe.is_file() {
            return State::missing(format!("{} is not there", exe.display()));
        }
        let build = detect::steam_buildid(dir, crate::data::REDKIT_APPID);
        let version = crate::win::file_version(&exe).unwrap_or_else(|| "unknown".into());
        State {
            version: Some(match build {
                Some(build) => format!("wcc_lite {version}, Steam build {build}"),
                None => format!("wcc_lite {version}"),
            }),
            note: Some(licence().map_or_else(|why| why, String::from)),
            ..State::found(exe)
        }
    },
};

/// Where REDkit's licence dialog keeps the user's answer, under HKEY_CURRENT_USER.
const REDKIT_EULA: &str = r"Software\CD Projekt RED\Mod Tools\1.0";

/// `DoNotShowEula` under [`REDKIT_EULA`]: 1 when the user's last answer in REDkit's licence dialog was Agree (wcc_lite
/// writes 0 before it shows the dialog, 1 on Agree). quen only reads it. `QUEN_TEST_EULA` (tests only) stands in for
/// it: a number, or empty for none.
pub(crate) fn redkit_licence() -> Option<u32> {
    match env::var("QUEN_TEST_EULA") {
        Ok(v) => v.parse().ok(),
        Err(_) => crate::win::reg_dword(false, REDKIT_EULA, "DoNotShowEula"),
    }
}

/// [`redkit_licence`] as quen_doctor's note and script_compile's refusal say it: Err until it is 1.
fn licence() -> Result<&'static str, String> {
    match redkit_licence() {
        Some(1) => Ok("licence accepted (DoNotShowEula = 1: the user's Agree in REDkit's dialog)"),
        v => Err(format!(
            "REDkit's licence is not accepted (DoNotShowEula = {}): quen_setup with licence: true opens REDkit's own \
             licence dialog (tell the user first that it opens and that the choice is theirs); script_compile is \
             refused until then",
            v.map_or("not set".into(), |v| v.to_string())
        )),
    }
}

/// Refused with needs, nothing changed, until the user agreed to REDkit's licence in its own dialog.
pub(crate) fn redkit_licence_accepted(tool: &str) -> Result<(), Done> {
    licence().map(drop).map_err(|why| {
        let need = json!({
            "dependency": REDKIT.name,
            "status": "licence_not_accepted",
            "needed_for": tool,
            "how_to_get": REDKIT.how_to_get,
        });
        Done::Needs(why, vec![need])
    })
}

/// A table of the RE knowledge base (live boot 1: the KB folder was there, its tables weren't).
fn kb_table(cx: &Cx, file: &str) -> State {
    let Some(kb) = &cx.config.kb else {
        return State::missing("no kb in quen's config".into());
    };
    let table = kb.join(file);
    if !kb.is_dir() {
        State::missing(format!("kb {} is not a folder", kb.display()))
    } else if !table.is_file() {
        State::missing(format!("{} is not there", table.display()))
    } else if cx.platform == Some("gog") {
        State {
            status: "wrong_version",
            note: Some(
                "its per-build tables (RVAs, layouts) are Steam 5.01's: not applied to a GOG install"
                    .into(),
            ),
            ..State::found(table)
        }
    } else {
        State::found(table)
    }
}

/// Every dependency, required ones first, as quen_doctor lists them.
const DEPS: [&Dep; 23] = [
    &GAME,
    &NATIVE_BRIDGE,
    &PACKAGE,
    &PRESENTMON,
    &CAPTURE,
    &DUPLICATION,
    &RESHADE,
    &RENODX,
    &DEVKIT,
    &NSIGHT_SYSTEMS,
    &NSIGHT_GRAPHICS,
    &NGFX_REPLAY,
    &GPU_COUNTERS,
    &RENDERDOC,
    &STREAMLINE,
    &STREAMLINE_DEV,
    &TRACY,
    &REDKIT,
    &KB_CVARS,
    &KB_PATCHES,
    &KB_INSTANCES,
    &KB_BUILD,
    &KB_INDEX,
];

/// What a check found.
pub(crate) struct State {
    /// `found`, `missing` or `wrong_version`.
    pub(crate) status: &'static str,
    pub(crate) path: Option<PathBuf>,
    version: Option<String>,
    sha256: Option<String>,
    pub(crate) note: Option<String>,
}

impl State {
    fn found(path: PathBuf) -> State {
        State {
            status: "found",
            path: Some(path),
            version: None,
            sha256: None,
            note: None,
        }
    }

    fn missing(note: String) -> State {
        State {
            status: "missing",
            path: None,
            version: None,
            sha256: None,
            note: Some(note),
        }
    }
}

/// Where the checks look. Tests build their own, so no check reads this machine's PATH or Program Files.
pub(crate) struct Cx<'a> {
    pub(crate) config: &'a Config,
    /// The game folder: the config's (for quen_doctor, else the one install detected).
    pub(crate) game_dir: Option<&'a Path>,
    /// The game folder's platform (`steam`, `gog`, or unknown). Nothing Steam's (pins, the KB's RVAs) applies to GOG.
    platform: Option<&'static str>,
    pins: &'a [Pin],
    path: Option<OsString>,
    program_files: Option<PathBuf>,
    /// Whether the capture checks may make a window and a D3D device (tests say no, and run them on their own).
    probes: bool,
    /// The game's exe, read once for every check that needs its build ([`Cx::game_build`]).
    exe: OnceCell<Result<GameBuild, String>>,
}

/// The game's exe as the build gate sees it.
#[derive(Clone)]
pub(crate) struct GameBuild {
    pub(crate) exe: PathBuf,
    pub(crate) build: Build,
    /// The install's platform for the gate: `gog`, `steam` when Steam's manifest names the folder, else `None` (a
    /// folder no store lists).
    platform: Option<&'static str>,
    /// Steam's `buildid` for the install: a second sign of an update, also when one leaves the exe alone.
    pub(crate) buildid: Option<String>,
}

impl GameBuild {
    /// The build gate's decision on this exe, with `config`'s known builds.
    pub(crate) fn gate(&self, config: &Config, allow_unknown: bool) -> Gate {
        build::gate(
            &self.build,
            self.platform,
            &config.known_builds,
            allow_unknown,
        )
    }

    /// The build and `gate`'s decision, as a result and the boot row give them.
    pub(crate) fn json(&self, gate: &Gate) -> Value {
        let mut out = json!(gate);
        out["sha256"] = json!(self.build.sha256);
        out["stamp"] = json!(self.build.stamp);
        out["platform"] = json!(self.platform);
        out["steam_buildid"] = json!(self.buildid);
        out
    }
}

impl<'a> Cx<'a> {
    pub(crate) fn new(config: &'a Config, game_dir: Option<&'a Path>) -> Cx<'a> {
        Cx {
            config,
            game_dir,
            platform: game_dir.and_then(|dir| w3::is_gog(dir).then_some("gog")),
            pins: pins(),
            path: env::var_os("PATH"),
            program_files: env::var_os("ProgramFiles").map(PathBuf::from),
            probes: true,
            exe: OnceCell::new(),
        }
    }

    /// The game's exe, hashed and its PE header read once. Errors say why there is none.
    pub(crate) fn game_build(&self) -> Result<&GameBuild, String> {
        let read = || {
            let dir = self.game_dir.ok_or("no game folder: run quen_setup")?;
            let exe = dir.join("bin").join("x64_dx12").join(&self.config.game_exe);
            let build = Build::read_exe(&exe).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => format!("{} is not there", exe.display()),
                _ => e.to_string(),
            })?;
            let buildid = detect::steam_buildid(dir, STEAM_APPID);
            let platform = self.platform.or(buildid.as_ref().map(|_| "steam"));
            Ok(GameBuild {
                exe,
                build,
                platform,
                buildid,
            })
        };
        self.exe.get_or_init(read).as_ref().map_err(String::clone)
    }

    /// A capability Windows has or lacks, by trying it: found, or missing with why and `without` (what that means).
    fn probe(&self, try_it: fn() -> Result<(), String>, without: &str) -> State {
        if !self.probes {
            return State::missing("not probed".into());
        }
        match try_it() {
            Ok(()) => State {
                path: None,
                ..State::found(PathBuf::new())
            },
            Err(why) => State::missing(format!("unavailable ({why}): {without}")),
        }
    }

    /// The files (or folders) a part installs into the game folder: missing if any is, the wrong version if a pinned
    /// file matches no exact pin. The state names the first file, its version and sha256.
    fn files(&self, rels: &[&str]) -> State {
        let Some(game) = self.game_dir else {
            return State::missing("no game folder: run quen_setup".into());
        };
        let mut state = State::found(game.join(rels[0]));
        let missing: Vec<&str> = rels
            .iter()
            .copied()
            .filter(|rel| !game.join(rel).exists())
            .collect();
        if !missing.is_empty() {
            return State::missing(format!("not in the game folder: {}", missing.join(", ")));
        }
        for rel in rels {
            let pins: Vec<&Pin> = self.pins.iter().filter(|p| p.file == *rel).collect();
            if pins.is_empty() || game.join(rel).is_dir() {
                continue;
            }
            let sha = fs::read(game.join(rel)).map(|b| sha256_hex(&b)).ok();
            let pin = pins.iter().find(|p| Some(p.sha256) == sha.as_deref());
            if state.sha256.is_none() {
                state.sha256 = sha.clone();
                state.version = pin.map(|p| p.version.to_owned());
            }
            if pin.is_none() {
                let known = pins.iter().map(|p| p.version).filter(|v| *v != OLD_MODQUEN);
                let known: Vec<&str> = known.collect();
                let known = known.join(", ");
                if pins.iter().any(|p| p.exact) {
                    state.status = "wrong_version";
                    state.note = Some(format!("{rel} is not the version quen expects ({known})"));
                } else {
                    state.note = Some(format!(
                        "{rel}: untested version (quen was tested with {known})"
                    ));
                }
            }
        }
        if state.sha256.is_none() && game.join(rels[0]).is_file() {
            state.sha256 = fs::read(game.join(rels[0])).map(|b| sha256_hex(&b)).ok();
        }
        state
    }

    /// A profiler: the exe quen's config names under `key`, else where it is installed ([`setup::PROGRAMS`]), which a
    /// launch can't use until the config names it.
    fn profiler(&self, configured: &Option<PathBuf>, key: &str) -> State {
        match configured {
            Some(exe) if exe.is_file() => State {
                version: crate::win::file_version(exe),
                ..State::found(exe.clone())
            },
            Some(exe) => State::missing(format!(
                "{key} {} in quen's config is not a file",
                exe.display()
            )),
            None => {
                let mut state = self.program(key);
                if state.status == "found" {
                    state.note = Some(format!(
                        "not in quen's config: add {key} = '<this path>' to launch under it"
                    ));
                }
                state
            }
        }
    }

    /// The program quen_setup finds as `key` ([`setup::PROGRAMS`]): on PATH, else the newest where its installer puts
    /// it (by name: versions sort there).
    fn program(&self, key: &str) -> State {
        let (install, exe) = setup::program(key);
        let found = on_path(self.path.clone(), exe)
            .or_else(|| detect::installed(self.program_files.as_ref()?, install).pop());
        match found {
            Some(path) => State {
                version: crate::win::file_version(&path),
                ..State::found(path)
            },
            None => State::missing(format!("no {exe} on PATH or in Program Files")),
        }
    }
}

/// PresentMon without a config entry: the newest console exe where Intel's installer puts it
/// (`%ProgramFiles%\Intel\PresentMon\PresentMonConsoleApplication\PresentMon-<version>-x64.exe`, a folder only an
/// administrator can write), else a `PresentMon.exe` on PATH.
/// ponytail: newest by name, so 2.10 sorts before 2.9; compare versions when two are ever installed side by side.
pub(crate) fn find_presentmon(
    path: Option<OsString>,
    program_files: Option<&Path>,
) -> Option<PathBuf> {
    let (install, exe) = setup::program("presentmon");
    let installed = || detect::installed(program_files?, install).pop();
    installed().or_else(|| on_path(path, exe))
}

/// `name` in a folder of the PATH value `path`.
pub(crate) fn on_path(path: Option<OsString>, name: &str) -> Option<PathBuf> {
    env::split_paths(&path?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The game's exe through the build gate: found when it is a known build, else the wrong version, naming its
/// sha256, its stamp and the entry to add. A GOG exe is unknown until a `gog` entry knows it.
fn game(cx: &Cx) -> State {
    let game = match cx.game_build() {
        Ok(game) => game,
        Err(why) => return State::missing(why),
    };
    let (status, version, note) = match game.gate(cx.config, false) {
        Gate::Known { label } => ("found", label, None),
        Gate::Unknown { why } | Gate::AllowedUnknown { why } => {
            let gog = if game.platform == Some("gog") {
                " (GOG)"
            } else {
                ""
            };
            let stamp = game.build.stamp;
            (
                "wrong_version",
                format!("unknown build{gog}, PE stamp {stamp}"),
                Some(why),
            )
        }
    };
    State {
        status,
        version: Some(version),
        sha256: Some(game.build.sha256.clone()),
        note,
        ..State::found(game.exe.clone())
    }
}

fn presentmon(cx: &Cx) -> State {
    let exe = match &cx.config.presentmon {
        Some(exe) if exe.is_file() => exe.clone(),
        Some(exe) => {
            return State::missing(format!(
                "PresentMon {} (presentmon in the config) is not a file",
                exe.display()
            ));
        }
        None => match find_presentmon(cx.path.clone(), cx.program_files.as_deref()) {
            Some(exe) => exe,
            None => {
                return State::missing(
                    "no presentmon in the config, none where its installer puts it, no PresentMon.exe on PATH"
                        .into(),
                );
            }
        },
    };
    // The console exe's name carries its version (PresentMon-2.4.1-x64.exe); a renamed copy may have a resource.
    let name = exe.file_name().unwrap_or_default().to_string_lossy();
    let version = name
        .strip_prefix("PresentMon-")
        .and_then(|v| v.split('-').next())
        .map(String::from)
        .or_else(|| crate::win::file_version(&exe));
    let major = version
        .as_ref()
        .and_then(|v| v.split('.').next()?.parse::<u32>().ok());
    let old = major.is_some_and(|m| m < 2);
    State {
        status: if old { "wrong_version" } else { "found" },
        note: old.then(|| "quen runs PresentMon 2.x's command line".to_owned()),
        version,
        ..State::found(exe)
    }
}

/// `dep` checked as quen_doctor checks it, over `config` and `game_dir`: quen_setup checks a value with it before
/// writing it, and shows what a configured one is now.
pub(crate) fn check(dep: &Dep, config: &Config, game_dir: Option<&Path>) -> State {
    (dep.check)(&Cx::new(config, game_dir))
}

/// `dep` for `needed_for`, checked before a tool acts: its state when found, else refused with `needs`.
pub(crate) fn require(dep: &Dep, config: &Config, needed_for: &str) -> Result<State, Done> {
    require_in(
        dep,
        &Cx::new(config, config.game_dir.as_deref()),
        needed_for,
    )
}

fn require_in(dep: &Dep, cx: &Cx, needed_for: &str) -> Result<State, Done> {
    needs_in(&[dep], cx, needed_for, |_, s| s.status != "found").map(|mut s| s.remove(0))
}

/// `deps` installed for `needed_for`: refused with every missing one in `needs`, nothing changed. Unlike [`require`],
/// another version passes: our parts were checked by version at the launch, and the game's build by its gate.
pub(crate) fn installed(
    deps: &[&Dep],
    config: &Config,
    needed_for: &str,
) -> Result<Vec<State>, Done> {
    let cx = Cx::new(config, config.game_dir.as_deref());
    needs_in(deps, &cx, needed_for, |_, s| s.status == "missing")
}

/// Each of `deps` checked; refused with `needs` for every one `fails`, else their states in order.
fn needs_in(
    deps: &[&Dep],
    cx: &Cx,
    needed_for: &str,
    fails: impl Fn(&Dep, &State) -> bool,
) -> Result<Vec<State>, Done> {
    let states: Vec<State> = deps.iter().map(|dep| (dep.check)(cx)).collect();
    let failed: Vec<(&Dep, &State)> = deps
        .iter()
        .copied()
        .zip(&states)
        .filter(|(dep, s)| fails(dep, s))
        .collect();
    if failed.is_empty() {
        return Ok(states);
    }
    let why: Vec<String> = failed
        .iter()
        .map(|(dep, s)| format!("{}: {}", dep.name, s.note.as_deref().unwrap_or(s.status)))
        .collect();
    let needs = failed
        .iter()
        .map(|(dep, s)| need(dep, s, needed_for))
        .collect();
    Err(Done::Needs(why.join("; "), needs))
}

/// session_launch's needs, before anything changes: the game and the Native Bridge installed, and quen's package found
/// exactly: its state is the launch's readiness, and a broken one (half, mismatched or the old layout) fails the boot's
/// script compile. Then the build gate: an exe of a build quen doesn't know (a GOG exe included) is refused, naming its
/// sha256 and stamp, unless `allow_unknown_build`. Otherwise the game's build, as the result and the boot row give it,
/// and as read.
pub(crate) fn launch(config: &Config, allow_unknown: bool) -> Result<(Value, GameBuild), Done> {
    launch_in(&Cx::new(config, config.game_dir.as_deref()), allow_unknown)
}

fn launch_in(cx: &Cx, allow_unknown: bool) -> Result<(Value, GameBuild), Done> {
    let deps = [&GAME, &NATIVE_BRIDGE, &PACKAGE];
    needs_in(&deps, cx, "session_launch", |dep, s| {
        if dep.name == PACKAGE.name {
            s.status != "found"
        } else {
            s.status == "missing"
        }
    })?;
    let game = cx.game_build()?.clone();
    let gate = game.gate(cx.config, allow_unknown);
    if let Some(why) = gate.refusal() {
        return Err(Done::Refused(why.into()));
    }
    Ok((game.json(&gate), game))
}

/// One entry of a refusal's `needs`.
pub(crate) fn need(dep: &Dep, state: &State, needed_for: &str) -> Value {
    json!({
        "dependency": dep.name,
        "status": state.status,
        "needed_for": needed_for,
        "how_to_get": dep.how_to_get,
    })
}

/// quen_doctor's fields: each dependency's state, then whether everything a session needs is there.
fn report(cx: &Cx) -> Map<String, Value> {
    let mut missing = Vec::new();
    // Who installed quen's package and the Native Bridge (`owner`): a Vortex mod, else none (quen's installer or the
    // user). A note on one Vortex owns that isn't found says to update it in Vortex.
    let owned = [
        (PACKAGE.name, PACKAGE_OWNED),
        (NATIVE_BRIDGE.name, BRIDGE_OWNED),
    ];
    let game = cx.game_dir.map(|g| {
        let config = Config {
            game_dir: Some(g.to_owned()),
            ..cx.config.clone()
        };
        (g, crate::vortex::read(&config))
    });
    let deps: Vec<Value> = DEPS
        .iter()
        .map(|dep| {
            let mut s = (dep.check)(cx);
            if dep.required && s.status != "found" {
                missing.push(dep.name);
            }
            let owner = owned
                .iter()
                .find(|(name, _)| *name == dep.name)
                .and_then(|(_, rels)| {
                    let (g, inv) = game.as_ref()?;
                    Some(vortex_owner(inv.as_ref(), g, rels))
                });
            if let Some(Some(m)) = &owner
                && s.status != "found"
            {
                let note = s.note.take().map_or(String::new(), |n| format!("{n}; "));
                s.note = Some(format!("{note}it is Vortex's ({m}): update it in Vortex"));
            }
            let mut entry = json!({
                "name": dep.name,
                "required": dep.required,
                "status": s.status,
                "path": s.path,
                "version": s.version,
                "sha256": s.sha256,
                "note": s.note,
                "enables": dep.enables,
                "how_to_get": dep.how_to_get,
            });
            if let Some(owner) = owner {
                entry["owner"] = json!(owner.map_or("none".into(), |m| format!("Vortex: {m}")));
            }
            entry
        })
        .collect();
    Map::from_iter([
        ("ready".into(), json!(missing.is_empty())),
        ("missing_required".into(), json!(missing)),
        ("game_dir".into(), json!(cx.game_dir)),
        (
            "platform".into(),
            json!({
                "name": cx.platform,
                "status": cx.platform.map(detect::status),
                "note": (cx.platform == Some("gog")).then_some(w3::GOG_UNTESTED),
            }),
        ),
        ("dependencies".into(), json!(deps)),
        (
            "game_build".into(),
            cx.game_build()
                .map_or(Value::Null, |g| g.json(&g.gate(cx.config, false))),
        ),
    ])
}

/// A warning for each `*.quen-current*` file beside the files a launch journals (`quen_core::boot::plan`'s patches:
/// the documents folder, the game's bin\x64_dx12 and bin\config\base): a replay kept the game's newer version there,
/// which nothing else names (feature tests 2c).
fn kept_copies(documents: Option<&Path>, game_dir: Option<&Path>) -> Vec<String> {
    let game = game_dir.into_iter().flat_map(|dir| {
        [
            quen_core::boot::bin_dir(dir),
            dir.join("bin").join("config").join("base"),
        ]
    });
    let dirs = documents.map(Path::to_path_buf).into_iter().chain(game);
    let mut out = Vec::new();
    for entry in dirs.flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((of, _)) = name.split_once(".quen-current") {
            out.push(format!(
                "{} holds the game's newer version of {of}, kept by an earlier session's restore: for the user to \
                 check (quen leaves it there)",
                entry.path().display()
            ));
        }
    }
    out
}

/// `quen_doctor` over MCP or the CLI, on the config on disk. Without a game folder in it, the one install detected is
/// checked (and said so).
pub fn call(shared: &Shared, args: &Value) -> Value {
    let started = Instant::now();
    let done = match (
        args.as_object().is_some_and(|a| a.is_empty()),
        OnDisk::read(),
    ) {
        (false, _) => Done::Refused("quen_doctor takes no arguments".into()),
        (true, Err(why)) => Done::Refused(why),
        (true, Ok(disk)) => {
            let machine = Machine::read(&disk.config.game_exe);
            let detected = match (&disk.config.game_dir, machine.installs.as_slice()) {
                (None, [one]) => Some(one.dir.as_path()),
                _ => None,
            };
            let game_dir = disk.config.game_dir.as_deref().or(detected);
            let mut cx = Cx::new(&disk.config, game_dir);
            cx.platform = game_dir.and_then(|dir| setup::platform(dir, &machine.installs));
            let mut fields = report(&cx);
            fields.insert("captures".into(), crate::disk::summary(&disk.config));
            fields.insert("quen_saves".into(), quen_saves(&disk.config));
            fields.insert("unknown_families".into(), unknown_families(&disk.config));
            let docs = disk.config.documents.as_deref();
            fields.insert("vortex".into(), crate::vortex::summary(game_dir, docs));
            // AppData redirected into an app package, or an older quen's folders in AppData (relocate.rs); always
            // there, empty when there are none.
            let places = crate::relocate::Places::here();
            let mut warnings =
                places.map_or_else(|why| vec![why], |p| crate::relocate::warnings(&p));
            warnings.extend(kept_copies(disk.config.documents.as_deref(), game_dir));
            fields.insert("warnings".into(), json!(warnings));
            if detected.is_some() {
                fields.insert(
                    "game_dir_note".into(),
                    json!("detected, not in quen's config yet: run quen_setup"),
                );
            }
            // quen serve refuses every session tool on such a config, so no session can run.
            if let Some(e) = disk.error {
                fields.insert("ready".into(), json!(false));
                let why = format!("quen's config doesn't parse, so no session can run: {e}");
                fields.insert("reason".into(), json!(why));
                fields.insert("config_error".into(), json!(e));
            }
            Done::Ok(fields)
        }
    };
    let secs = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let timing = Map::from_iter([("quen_doctor".to_owned(), json!(secs))]);
    envelope(shared, "quen_doctor", timing, Vec::new(), Map::new(), done)
}

pub fn tool() -> Value {
    json!({
        "name": "quen_doctor",
        "description": "Read-only, no lock, fast: every dependency quen uses, each with status (found, missing, \
            wrong_version), path, what it enables and where to get it (quen installs nothing). Required \
            for a session: the game at a build quen knows (5.01), the Witcher Native Bridge, and quen's in-game package \
            (W3Quen.asi + modQuen, from one release; half of it, a mismatched pair or a leftover of the old layout is \
            the wrong version, and session_launch refuses it). The W3 Game State Bridge isn't needed. Optional, each \
            enabling tools: PresentMon, ReShade (RenoDX's host), the RenoDX add-on and devkit, Nsight Systems / \
            Graphics and GPU counter access for all users (an NVIDIA Control Panel setting), RenderDoc \
            with NVIDIA's signed Streamline interposer and the patched dev build, the RE \
            knowledge base's cvar layout (game_cvar), patches (mem_patch) and build line; using them beside quen: quen_guide. \
            ready says whether a session can run. Run it on first use and after a refusal with needs; tell the user \
            what's missing and where to get it.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
    })
}

/// `quen doctor`: the same fields, printed as JSON; exit code 0 when ready.
pub fn cli() -> i32 {
    let out = call(&Shared::new(crate::config::load()), &json!({}));
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    i32::from(out["ready"] != true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quen_core::build::KnownBuilds;

    /// A synthetic game folder for one test, gone afterwards (only what the test made in it).
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = env::temp_dir().join(format!("quen-doctor-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn put(&self, rel: &str, data: impl AsRef<[u8]>) -> PathBuf {
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

    /// A synthetic PE image, not from any real binary: a DOS header pointing at a PE header with `stamp`, then `body`.
    fn pe(stamp: u32, body: &str) -> Vec<u8> {
        let mut b = vec![0u8; 0x98];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        b[0x88..0x8C].copy_from_slice(&stamp.to_le_bytes());
        b.extend_from_slice(body.as_bytes());
        b
    }

    /// `config`'s known builds: `entries` alone (KnownBuilds format), not the adapter's.
    fn knows(config: &mut Config, entries: &str) {
        config.known_builds = KnownBuilds::parse(entries).unwrap();
    }

    fn sha(data: &str) -> &'static str {
        Box::leak(sha256_hex(data.as_bytes()).into_boxed_str())
    }

    fn config(dir: &Path) -> Config {
        let mut config = crate::config::parse("", Path::new("test.toml")).unwrap();
        config.game_dir = Some(dir.join("game"));
        config
    }

    /// A Cx over `config`'s game folder with `pins`, an empty PATH and a Program Files of the test's own.
    fn cx<'a>(t: &Temp, config: &'a Config, pins: &'a [Pin]) -> Cx<'a> {
        Cx {
            config,
            game_dir: config.game_dir.as_deref(),
            platform: Some("steam"),
            pins,
            path: Some(OsString::new()),
            program_files: Some(t.0.join("pf")),
            probes: false,
            exe: OnceCell::new(),
        }
    }

    /// Feature tests 2c: every `.quen-current` copy beside a journaled file is named, nothing else.
    #[test]
    fn kept_copies_beside_the_journaled_files_are_named() {
        let t = Temp::new("kept");
        t.put(r"docs\input.settings", "keys");
        t.put(r"docs\input.settings.quen-current", "rebound");
        t.put(r"docs\dx12user.settings.quen-current-2", "newer");
        t.put(r"game\bin\x64_dx12\ReShade.ini.quen-current", "rewritten");
        t.put(r"game\bin\config\base\general.ini", "console");
        t.put(r"game\content\x.quen-current", "not a journaled folder");
        let docs = t.0.join("docs");
        let mut got = kept_copies(Some(&docs), Some(&t.0.join("game")));
        got.sort();
        assert_eq!(got.len(), 3, "{got:#?}");
        for (line, of) in got
            .iter()
            .zip(["dx12user.settings", "input.settings", "ReShade.ini"])
        {
            assert!(line.contains(&format!("newer version of {of},")), "{line}");
        }
        assert!(kept_copies(None, None).is_empty());
    }

    /// A third-party part at another version is found, flagged untested. The loader may be winmm.dll, and the Native
    /// Bridge's own Mods\modNativeBridge isn't needed.
    #[test]
    fn the_native_bridge_is_found_at_any_version_with_either_loader() {
        let t = Temp::new("pins");
        let config = config(&t.0);
        let pins = [Pin {
            file: r"bin\x64_dx12\WitcherNativeBridge.asi",
            version: "2.0",
            sha256: sha("wnb 2.0"),
            exact: false,
        }];
        let cx = cx(&t, &config, &pins);
        t.put(r"game\bin\x64_dx12\WitcherNativeBridge.asi", "wnb 2.1");
        let state = (NATIVE_BRIDGE.check)(&cx);
        assert_eq!(state.status, "missing");
        assert!(state.note.unwrap().contains("dinput8.dll"));
        t.put(r"game\bin\x64_dx12\winmm.dll", "loader");
        let state = (NATIVE_BRIDGE.check)(&cx);
        assert_eq!((state.status, state.version), ("found", None));
        assert!(state.note.unwrap().contains("untested"));
    }

    /// quen's package is found only whole, both files from one release, with nothing of the old layout left.
    /// Anything else is the wrong version with its fix, and session_launch refuses it, as it refuses no package.
    #[test]
    fn the_package_is_found_only_whole_and_from_one_release() {
        let t = Temp::new("package");
        let config = config(&t.0);
        let pin = |file, version, data| Pin {
            file,
            version,
            sha256: sha(data),
            exact: true,
        };
        let pins = [
            pin(PACKAGE_ASI, "1", "asi 1"),
            pin(PACKAGE_SCRIPT, "1", "ws 1"),
            pin(PACKAGE_ASI, "2", "asi 2"),
            pin(PACKAGE_SCRIPT, "2", "ws 2"),
            // Release 3 kept release 2's quen.ws (as quen-w3 0.1.3 kept 0.1.2's).
            pin(PACKAGE_ASI, "3", "asi 3"),
            pin(PACKAGE_SCRIPT, "3", "ws 2"),
            pin(PACKAGE_SCRIPT, OLD_MODQUEN, "old ws"),
            // Release 2 holds the HUD line's bundle too (as quen-w3 0.1.16 does).
            pin(PACKAGE_HUD[0], "2", "bundle 2"),
            pin(PACKAGE_HUD[1], "2", "store 2"),
        ];
        let cx = cx(&t, &config, &pins);
        let broken = |why: &str| {
            let state = (PACKAGE.check)(&cx);
            let note = state.note.unwrap_or_default();
            assert!(
                state.status == "wrong_version" && note.contains(why),
                "{note}"
            );
        };
        // Whether a launch's refusal names the package (the other parts are missing here, so it always refuses).
        let refuses = || {
            let Err(Done::Needs(_, needs)) = launch_in(&cx, false) else {
                panic!("launched without the Native Bridge");
            };
            needs.iter().any(|n| n["dependency"] == PACKAGE.name)
        };
        assert_eq!((PACKAGE.check)(&cx).status, "missing");
        assert!(
            refuses(),
            "every launch needs it: its state is the readiness"
        );
        let (asi, script) = (
            format!(r"game\{PACKAGE_ASI}"),
            format!(r"game\{PACKAGE_SCRIPT}"),
        );
        t.put(&script, "ws 1");
        broken(r"Mods\modQuen without bin\x64_dx12\W3Quen.asi");
        assert!(refuses(), "a half install breaks every boot");
        t.put(&asi, "asi 2");
        broken(
            "W3Quen.asi (2) and quen.ws (1) are from different releases: unzip one release package",
        );
        t.put(&asi, "asi 1");
        let state = (PACKAGE.check)(&cx);
        assert_eq!(
            (state.status, state.version.as_deref()),
            ("found", Some("1"))
        );
        assert!(!refuses());
        let off = t.0.join(&script).with_extension("off");
        fs::rename(t.0.join(&script), &off).unwrap();
        broken(r"bin\x64_dx12\W3Quen.asi without Mods\modQuen");
        fs::rename(&off, t.0.join(&script)).unwrap();
        t.put(&script, "old ws");
        broken("the old pure-script modQuen: replace it with the release package");
        t.put(&script, "ws 3");
        broken("quen.ws is not the version quen expects (1, 2, 3): unzip one release package");
        // A release's HUD bundle comes with it: missing, or another release's, it is the wrong version.
        t.put(&script, "ws 2");
        t.put(&asi, "asi 2");
        broken(r"Mods\modQuen\content\blob0.bundle is not 2's: unzip one release package");
        t.put(r"game\Mods\modQuen\content\blob0.bundle", "bundle 2");
        t.put(r"game\Mods\modQuen\content\metadata.store", "store 1");
        broken(r"Mods\modQuen\content\metadata.store is not 2's");
        t.put(r"game\Mods\modQuen\content\metadata.store", "store 2");
        // A quen.ws two releases share pairs with either's ASI, and with no other; a release without a bundle doesn't
        // look at one.
        for (bytes, version) in [("asi 2", "2"), ("asi 3", "3")] {
            t.put(&asi, bytes);
            let state = (PACKAGE.check)(&cx);
            assert_eq!(
                (state.status, state.version.as_deref()),
                ("found", Some(version))
            );
        }
        t.put(&asi, "asi 1");
        broken("W3Quen.asi (1) and quen.ws (2 or 3) are from different releases");
        t.put(&script, "ws 1");
        fs::create_dir_all(t.0.join(r"game\Mods\modQuenRpc")).unwrap();
        broken(r"Move Mods\modQuenRpc to the Recycle Bin");
        assert!(refuses());
    }

    #[test]
    fn a_gog_exe_is_an_unknown_build_and_gets_nothing_of_steams() {
        let t = Temp::new("gog");
        let mut config = config(&t.0);
        config.kb = Some(t.0.join("kb"));
        t.put(r"kb\cvar_layout.tsv", "build\n");
        t.put(r"kb\patches.tsv", "name\n");
        // Even byte for byte Steam's known build: on GOG it is never the known build.
        let exe = pe(0x1111_1111, "the known build");
        knows(&mut config, &format!("steam {} 5.01", sha256_hex(&exe)));
        t.put(r"game\bin\x64_dx12\witcher3.exe", &exe);
        t.put(r"game\goggame-1207664643.info", "{}");
        let mut cx = cx(&t, &config, &[]);
        cx.platform = Some("gog");
        let state = (GAME.check)(&cx);
        assert_eq!(state.status, "wrong_version");
        assert_eq!(
            state.version.as_deref(),
            Some("unknown build (GOG), PE stamp 0x11111111")
        );
        assert!(state.note.unwrap().contains("GOG is untested"));
        assert_eq!((KB_CVARS.check)(&cx).status, "wrong_version");
        let fields = report(&cx);
        assert_eq!(fields["platform"]["status"], "untested");
        assert_eq!(fields["ready"], false);
        assert_eq!(fields["game_build"]["status"], "unknown");
        // A tool on a GOG install: refused with needs; Cx::new finds GOG by its marker file.
        let Err(Done::Needs(why, _)) = require(&KB_PATCHES, &config, "mem_patch") else {
            panic!("the KB was used on a GOG install");
        };
        assert!(why.contains("Steam 5.01"), "{why}");
    }

    /// Live boot 1: the KB folder was there without its tables, and the doctor called it found. Each table is checked.
    #[test]
    fn each_kb_table_is_checked_on_its_own() {
        let t = Temp::new("kb");
        let mut config = config(&t.0);
        let state = |config: &Config, dep: &Dep| {
            let s = (dep.check)(&cx(&t, config, &[]));
            (s.status, s.note.unwrap_or_default())
        };
        assert_eq!(state(&config, &KB_CVARS).1, "no kb in quen's config");
        config.kb = Some(t.0.join("kb"));
        assert!(state(&config, &KB_PATCHES).1.contains("not a folder"));
        t.put(r"kb\names.tsv", "name\n");
        for dep in [&KB_CVARS, &KB_PATCHES] {
            let (status, note) = state(&config, dep);
            assert!(
                status == "missing" && note.contains("is not there"),
                "{note}"
            );
        }
        t.put(r"kb\cvar_layout.tsv", "build\n");
        assert_eq!(state(&config, &KB_CVARS).0, "found");
        assert_eq!(state(&config, &KB_PATCHES).0, "missing");
        let Err(Done::Needs(_, needs)) = require(&KB_PATCHES, &config, "mem_patch") else {
            panic!("mem_patch without patches.tsv");
        };
        assert_eq!(needs[0]["dependency"], KB_PATCHES.name);
    }

    /// The build gate. The doctor calls a known build found and an unknown one the wrong version; a launch refuses
    /// an unknown one, naming its sha256 and stamp, unless allow_unknown_build.
    #[test]
    fn the_game_is_known_by_its_sha256_and_unknown_builds_are_named() {
        let t = Temp::new("game");
        let mut config = config(&t.0);
        let (known, update) = (pe(0x1111_1111, "known"), pe(0x2222_2222, "an update"));
        knows(
            &mut config,
            &format!("steam {} 5.01 (synthetic)", sha256_hex(&known)),
        );
        // The launch's other needs, found: the Native Bridge, and quen's package at its pinned release.
        let pins = [
            Pin {
                file: PACKAGE_ASI,
                version: "1",
                sha256: sha("asi"),
                exact: true,
            },
            Pin {
                file: PACKAGE_SCRIPT,
                version: "1",
                sha256: sha("ws"),
                exact: true,
            },
        ];
        for (rel, data) in [
            (r"bin\x64_dx12\WitcherNativeBridge.asi", "wnb"),
            (r"bin\x64_dx12\dinput8.dll", "loader"),
            (PACKAGE_ASI, "asi"),
            (PACKAGE_SCRIPT, "ws"),
        ] {
            t.put(&format!(r"game\{rel}"), data);
        }
        let exe = r"game\bin\x64_dx12\witcher3.exe";
        let state = (GAME.check)(&cx(&t, &config, &pins));
        assert!(state.status == "missing" && state.note.unwrap().contains("is not there"));

        t.put(exe, &update);
        let cx_update = cx(&t, &config, &pins);
        let state = (GAME.check)(&cx_update);
        assert_eq!(
            (state.status, state.version.as_deref()),
            ("wrong_version", Some("unknown build, PE stamp 0x22222222"))
        );
        assert_eq!(state.sha256, Some(sha256_hex(&update)));
        let Err(Done::Refused(why)) = launch_in(&cx_update, false) else {
            panic!("an unknown build passed the gate");
        };
        assert!(
            why.contains(&sha256_hex(&update)) && why.contains("0x22222222"),
            "{why}"
        );
        let Ok((result, game)) = launch_in(&cx_update, true) else {
            panic!("allow_unknown_build was refused");
        };
        assert_eq!(result["status"], "allowed_unknown");
        assert_eq!(result["stamp"], "0x22222222");
        assert_eq!(result["why"], why.as_str());
        assert_eq!(game.build.sha256, sha256_hex(&update));

        t.put(exe, &known);
        let cx_known = cx(&t, &config, &pins);
        let state = (GAME.check)(&cx_known);
        assert_eq!(
            (state.status, state.version.as_deref()),
            ("found", Some("5.01 (synthetic)"))
        );
        let Ok((result, _)) = launch_in(&cx_known, false) else {
            panic!("the known build was refused");
        };
        assert_eq!(
            (&result["status"], &result["label"], &result["sha256"]),
            (
                &json!("known"),
                &json!("5.01 (synthetic)"),
                &json!(sha256_hex(&known))
            )
        );
        // Not a PE image: there's no build to read.
        t.put(exe, "MZ");
        let state = (GAME.check)(&cx(&t, &config, &pins));
        assert!(state.note.unwrap().contains("the DOS header is cut off"));
        config.game_dir = None;
        let state = (GAME.check)(&cx(&t, &config, &pins));
        assert!(state.note.unwrap().contains("quen_setup"));

        // A Steam install: Steam's buildid from the manifest that names its folder, and the platform with it.
        let common = t.0.join(r"steamapps\common");
        config.game_dir = Some(common.join("Game"));
        t.put(r"steamapps\common\Game\bin\x64_dx12\witcher3.exe", &known);
        t.put(
            r"steamapps\appmanifest_292030.acf",
            "\"AppState\"\n{\n\t\"appid\"\t\t\"292030\"\n\t\"installdir\"\t\t\"Game\"\n\t\"buildid\"\t\t\"111\"\n}\n",
        );
        let mut steam = cx(&t, &config, &pins);
        steam.platform = None;
        let fields = report(&steam);
        let build = &fields["game_build"];
        assert_eq!(
            (
                &build["status"],
                &build["platform"],
                &build["steam_buildid"]
            ),
            (&json!("known"), &json!("steam"), &json!("111"))
        );
    }

    /// The doctor says whether the ledger's KB index is of the commit the KB's work tree has checked out.
    #[test]
    fn the_kb_index_is_checked_against_the_kb_s_head() {
        let t = Temp::new("kbindex");
        let mut config = config(&t.0);
        config.data_root = t.0.join("data");
        let state = |config: &Config| {
            let s = (KB_INDEX.check)(&cx(&t, config, &[]));
            (s.status, s.note.unwrap_or_default())
        };
        assert_eq!(state(&config).1, "no kb in quen's config");
        let kb = t.0.join(r"repo\re");
        config.kb = Some(kb.clone());
        assert!(state(&config).1.contains("git work tree"));
        t.put(r"repo\.git\HEAD", "ref: refs/heads/main\n");
        t.put(r"repo\.git\refs\heads\main", "c".repeat(40));
        let header = "rva\tname\tkind\tconfidence\tsource\tdate\n";
        let names = format!(
            "# build: sha256={} stamp=0x00000001\n{header}",
            sha("synthetic exe")
        );
        t.put(r"repo\re\names.tsv", names);
        t.put(r"repo\re\notes\a.md", "## A\ntext\n");
        assert!(state(&config).1.contains("never scanned"));
        fs::create_dir_all(config.state_dir()).unwrap();
        let mut l = Ledger::open(config.state_dir().join("quen.db")).unwrap();
        l.add_game(&quen_core::ledger::Game {
            name: "synthetic".into(),
            exe_path: "%GAME_DIR%/x.exe".into(),
            exe_sha256: sha("synthetic exe").into(),
            build_stamp: None,
        })
        .unwrap();
        l.scan_kb(&kb).unwrap();
        drop(l);
        assert_eq!(state(&config), ("found", String::new()));
        t.put(r"repo\.git\refs\heads\main", "d".repeat(40));
        let (status, note) = state(&config);
        assert!(
            status == "wrong_version" && note.contains(&"d".repeat(40)),
            "{note}"
        );
    }

    /// The KB's tables name the build they were made on; while they do, the doctor and the memory tools refuse
    /// an exe of another build.
    #[test]
    fn the_kb_s_build_line_must_name_the_game_s_exe() {
        let t = Temp::new("kbbuild");
        let mut config = config(&t.0);
        let (exe, other) = (pe(0x1111_1111, "the KB's"), pe(0x2222_2222, "another"));
        let (built, another) = (Build::of_exe(&exe).unwrap(), Build::of_exe(&other).unwrap());
        t.put(r"game\bin\x64_dx12\witcher3.exe", &exe);
        let state = |config: &Config| {
            let s = (KB_BUILD.check)(&cx(&t, config, &[]));
            (s.status, s.note.unwrap_or_default())
        };
        assert_eq!(state(&config).1, "no kb in quen's config");
        assert_eq!(kb_matches(&config, &another), Ok(()));
        config.kb = Some(t.0.join("kb"));
        assert!(state(&config).1.contains("not a folder"));
        // Tables without a build line: the KB isn't pinned yet, so nothing is checked and the tools go ahead.
        t.put(r"kb\names.tsv", "# synthetic names\nrva\tname\n");
        t.put(r"kb\hooks.tsv", "site\tkind\n");
        let (status, note) = state(&config);
        assert!(
            status == "missing" && note.contains("no build line in names.tsv or hooks.tsv"),
            "{note}"
        );
        assert_eq!(kb_matches(&config, &another), Ok(()));
        // Once one table names it, every table there must, and the same build.
        let line = |b: &Build| format!("# build: sha256={} stamp={}\n", b.sha256, b.stamp);
        t.put(
            r"kb\names.tsv",
            format!("# synthetic names\n{}rva\tname\n", line(&built)),
        );
        let (status, note) = state(&config);
        assert!(
            status == "wrong_version" && note.contains("hooks.tsv: no build line"),
            "{note}"
        );
        assert!(
            kb_matches(&config, &built)
                .unwrap_err()
                .contains("hooks.tsv")
        );
        t.put(r"kb\hooks.tsv", format!("{}site\tkind\n", line(&another)));
        assert!(state(&config).1.contains("one KB, one build"));
        t.put(r"kb\hooks.tsv", format!("{}site\tkind\n", line(&built)));
        assert_eq!(state(&config), ("found", String::new()));
        assert_eq!(kb_matches(&config, &built), Ok(()));
        // Another build's exe: refused, naming both.
        let why = kb_matches(&config, &another).unwrap_err();
        assert!(
            why.contains(&format!("the KB is for {built}, but the exe is {another}")),
            "{why}"
        );
        t.put(r"game\bin\x64_dx12\witcher3.exe", &other);
        let (status, note) = state(&config);
        assert!(
            status == "wrong_version" && note.contains("another build: a game update"),
            "{note}"
        );
        // A table that isn't there names nothing: names.tsv alone pins the KB.
        fs::remove_file(t.0.join(r"kb\hooks.tsv")).unwrap(); // this test's own file
        assert_eq!(kb_matches(&config, &built), Ok(()));
        assert!(kb_matches(&config, &another).is_err());
    }

    #[test]
    fn presentmon_and_profilers_come_from_the_config_path_or_program_files() {
        let t = Temp::new("tools");
        let mut config = config(&t.0);
        let cx_path = |config: &Config, path: &str| {
            let mut cx = cx(&t, config, &[]);
            cx.path = Some(t.0.join(path).into_os_string());
            (PRESENTMON.check)(&cx)
        };
        assert_eq!(cx_path(&config, "bin").status, "missing");
        let exe = t.put(r"bin\PresentMon.exe", "pm");
        let state = cx_path(&config, "bin");
        assert_eq!((state.status, state.path.as_ref()), ("found", Some(&exe)));
        config.presentmon = Some(t.0.join("nowhere.exe"));
        assert!(
            cx_path(&config, "bin")
                .note
                .unwrap()
                .contains("is not a file")
        );
        config.presentmon = Some(t.put(r"pm\PresentMon-2.4.1-x64.exe", "pm"));
        let state = cx_path(&config, "");
        assert_eq!(
            (state.status, state.version.as_deref()),
            ("found", Some("2.4.1"))
        );
        config.presentmon = Some(t.put(r"pm\PresentMon-1.9.2-x64.exe", "pm"));
        assert_eq!(cx_path(&config, "").status, "wrong_version");
        // No config entry and nothing on PATH: where Intel's installer puts the console exe.
        config.presentmon = None;
        let installed = t.put(
            r"pf\Intel\PresentMon\PresentMonConsoleApplication\PresentMon-2.4.1-x64.exe",
            "pm",
        );
        let state = cx_path(&config, "");
        assert_eq!(
            (state.path, state.version.as_deref()),
            (Some(installed), Some("2.4.1"))
        );
        // Nsight: the newest version folder in Program Files.
        let cx = cx(&t, &config, &[]);
        assert_eq!((NSIGHT_SYSTEMS.check)(&cx).status, "missing");
        t.put(
            r"pf\NVIDIA Corporation\Nsight Systems 2026.3.2\target-windows-x64\nsys.exe",
            "",
        );
        let newest = t.put(
            r"pf\NVIDIA Corporation\Nsight Systems 2026.5.1\target-windows-x64\nsys.exe",
            "",
        );
        assert_eq!((NSIGHT_SYSTEMS.check)(&cx).path, Some(newest));
    }

    #[test]
    fn renodx_and_the_devkit_follow_quens_config() {
        let t = Temp::new("renodx");
        let mut config = config(&t.0);
        t.put(
            r"game\bin\x64_dx12\renodx-thewitcher3remastered.addon64",
            "a",
        );
        let state = (RENODX.check)(&cx(&t, &config, &[]));
        assert_eq!(state.status, "found");
        assert!(state.note.unwrap().contains("renodx_addon"));
        config.renodx_addon = Some("mine.addon64".into());
        assert_eq!((RENODX.check)(&cx(&t, &config, &[])).status, "missing");
        t.put(r"game\bin\x64_dx12\mine.addon64", "a");
        config.renodx_build = Some(t.put(r"build\mine.addon64", "b"));
        let state = (RENODX.check)(&cx(&t, &config, &[]));
        assert_eq!(state.status, "wrong_version");
        assert!(state.note.unwrap().contains("renodx_build"));
        t.put(r"build\mine.addon64", "a");
        let state = (RENODX.check)(&cx(&t, &config, &[]));
        assert_eq!((state.status, state.note), ("found", None));
        config.devkit = Some(r"dk\devkit.addon64".into());
        assert_eq!((DEVKIT.check)(&cx(&t, &config, &[])).status, "missing");
        t.put(r"game\bin\x64_dx12\dk\devkit.addon64", "d");
        let state = (DEVKIT.check)(&cx(&t, &config, &[]));
        assert_eq!((state.status, state.note), ("found", None));
        // A profiler the config names must be there; one only installed is found but not usable yet.
        config.nsys = Some(t.0.join("nsys.exe"));
        assert_eq!(
            (NSIGHT_SYSTEMS.check)(&cx(&t, &config, &[])).status,
            "missing"
        );
        config.nsys = None;
        t.put(
            r"pf\NVIDIA Corporation\Nsight Systems 1\target-windows-x64\nsys.exe",
            "",
        );
        let state = (NSIGHT_SYSTEMS.check)(&cx(&t, &config, &[]));
        assert_eq!(state.status, "found");
        assert!(state.note.unwrap().contains("not in quen's config"));
        // ngfx-replay only beside the configured ngfx.exe (a synthetic file: no version, so no privacy answer read).
        assert_eq!((NGFX_REPLAY.check)(&cx(&t, &config, &[])).status, "missing");
        config.ngfx = Some(t.put(r"ng\ngfx.exe", ""));
        assert_eq!((NGFX_REPLAY.check)(&cx(&t, &config, &[])).status, "missing");
        let replay = t.put(r"ng\ngfx-replay.exe", "");
        let state = (NGFX_REPLAY.check)(&cx(&t, &config, &[]));
        assert_eq!((state.status, state.path), ("found", Some(replay)));
        assert!(state.note.unwrap().contains("no version resource"));
    }

    /// The renderdoc profile's parts: RenderDoc from the config (its exe is renderdoccmd.exe), the game's interposer
    /// only as NVIDIA's pinned bytes, a dev build that isn't them, and the counters' setting read as it is.
    #[test]
    fn renderdoc_streamline_and_the_counters_are_each_checked() {
        let t = Temp::new("renderdoc");
        let mut config = config(&t.0);
        let pins = [Pin {
            file: INTERPOSER,
            version: "2.14.1",
            sha256: sha("signed"),
            exact: true,
        }];
        let state = (RENDERDOC.check)(&cx(&t, &config, &pins));
        assert_eq!(state.status, "missing");
        t.put(r"pf\RenderDoc\renderdoccmd.exe", "");
        let state = (RENDERDOC.check)(&cx(&t, &config, &pins));
        assert!(state.note.unwrap().contains("add renderdoc = "));
        config.renderdoc = Some(t.put(r"rd\renderdoccmd.exe", ""));
        assert_eq!((RENDERDOC.check)(&cx(&t, &config, &pins)).status, "found");
        t.put(r"game\bin\x64_dx12\sl.interposer.dll", "dev");
        assert_eq!(
            (STREAMLINE.check)(&cx(&t, &config, &pins)).status,
            "wrong_version"
        );
        t.put(r"game\bin\x64_dx12\sl.interposer.dll", "signed");
        let state = (STREAMLINE.check)(&cx(&t, &config, &pins));
        assert_eq!(
            (state.status, state.version.as_deref()),
            ("found", Some("2.14.1"))
        );
        assert_eq!(
            (STREAMLINE_DEV.check)(&cx(&t, &config, &pins)).status,
            "missing"
        );
        config.streamline_dev_interposer = Some(t.put(r"sl\sl.interposer.dll", "signed"));
        let state = (STREAMLINE_DEV.check)(&cx(&t, &config, &pins));
        assert!(state.status == "wrong_version" && state.note.unwrap().contains("production"));
        t.put(r"sl\sl.interposer.dll", "dev");
        let state = (STREAMLINE_DEV.check)(&cx(&t, &config, &pins));
        assert_eq!(
            (state.status, state.note.as_deref()),
            ("found", Some("built locally (not quen's release build)"))
        );
        assert_eq!(counters(Some((0, "K".into()))).status, "found");
        let restricted = counters(Some((1, "K".into())));
        assert!(
            restricted.status == "missing"
                && restricted.note.unwrap().contains("administrators only")
        );
        assert!(counters(None).note.unwrap().contains("default"));
    }

    /// The capture checks on this machine's own Windows: found or missing, never a panic, and the foreground window
    /// the same before and after (the user may be at the PC).
    #[test]
    fn capture_probes_answer_without_taking_the_focus() {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let config = crate::config::parse("", Path::new("test.toml")).unwrap();
        let cx = Cx::new(&config, None);
        for dep in [&CAPTURE, &DUPLICATION] {
            let state = (dep.check)(&cx);
            match state.status {
                "found" => {}
                "missing" => assert!(state.note.unwrap().starts_with("unavailable")),
                other => panic!("{}: {other}", dep.name),
            }
        }
        // Only a window of this process in front means a probe took the focus: other tests, or the user, may change the
        // foreground meanwhile.
        let mut owner = 0;
        // SAFETY: plain queries; `owner` outlives the call.
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner)) };
        assert_ne!(owner, std::process::id(), "a probe took the focus");
    }

    /// game_freecam refuses every pinned package release before 0.1.17, which brought its verbs.
    #[test]
    fn the_free_camera_needs_0_1_17_or_later() {
        let mut older: Vec<&str> = PINS
            .iter()
            .filter(|p| {
                p.file == PACKAGE_ASI && !["quen-w3 0.1.17", "quen-w3 0.1.18"].contains(&p.version)
            })
            .map(|p| p.version)
            .collect();
        older.sort();
        let mut before = BEFORE_FREECAM.to_vec();
        before.sort();
        assert_eq!(older, before);
        assert!(
            PINS.iter()
                .any(|p| p.file == PACKAGE_ASI && p.version == "quen-w3 0.1.17")
        );
    }

    #[test]
    fn a_tool_missing_a_dependency_is_refused_with_needs() {
        let t = Temp::new("needs");
        let config = config(&t.0);
        let cx = cx(&t, &config, &[]);
        let Err(Done::Needs(why, needs)) = require_in(&PRESENTMON, &cx, "measure_ab") else {
            panic!("not refused with needs");
        };
        assert!(why.starts_with("PresentMon 2.x"), "{why}");
        assert_eq!(needs.len(), 1);
        assert_eq!(needs[0]["dependency"], PRESENTMON.name);
        assert_eq!(needs[0]["needed_for"], "measure_ab");
        assert_eq!(needs[0]["status"], "missing");
        assert!(
            needs[0]["how_to_get"]
                .as_str()
                .unwrap()
                .contains("PresentMon")
        );
    }

    #[test]
    fn the_report_lists_every_dependency_and_says_whether_a_session_can_run() {
        let t = Temp::new("report");
        let config = config(&t.0);
        let fields = report(&cx(&t, &config, &[]));
        let deps = fields["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), DEPS.len());
        assert_eq!(fields["ready"], false);
        let required: Vec<&str> = deps
            .iter()
            .filter(|d| d["required"] == true)
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(fields["missing_required"], json!(required));
        assert!(deps.iter().all(|d| d["status"] == "missing"));
        let names = serde_json::to_string(&fields).unwrap().to_lowercase();
        assert!(
            !names.contains("special k") && !names.contains("skiv"),
            "{names}"
        );
    }

    /// quen's package and the Native Bridge name who installed them: the Vortex mod whose deployment manifest lists a
    /// file of theirs, else none. A package Vortex owns that isn't found says to update it in Vortex.
    #[test]
    fn the_package_and_the_native_bridge_name_their_owner() {
        let t = Temp::new("owner");
        let config = config(&t.0);
        let cx = cx(&t, &config, &[]);
        let deps = || {
            let fields = report(&cx);
            let deps = fields["dependencies"].as_array().unwrap().clone();
            let of = |dep: &Dep| deps.iter().find(|d| d["name"] == dep.name).unwrap().clone();
            [of(&PACKAGE), of(&NATIVE_BRIDGE), of(&GAME)]
        };
        let [package, bridge, game] = deps();
        assert_eq!([&package["owner"], &bridge["owner"]], ["none", "none"]);
        assert!(game.get("owner").is_none(), "{game}");
        // A synthetic Vortex deployed the package: its manifest and its staging folder's marker.
        let source = "quen-w3 1 0.1.0 2026-10-22T00-00Z key";
        let staging = t.put(r"staging\__vortex_staging_folder", r#"{"instance": "i"}"#);
        let files: Vec<Value> = [PACKAGE_ASI, PACKAGE_SCRIPT]
            .iter()
            .map(|rel| json!({"relPath": rel, "source": source, "target": "", "time": 1}))
            .collect();
        let manifest = json!({"instance": "i", "deploymentMethod": "hardlink_activator",
            "stagingPath": staging.parent().unwrap(), "files": files});
        t.put(
            r"game\vortex.deployment.witcher3tl.json",
            manifest.to_string(),
        );
        let [package, bridge, _] = deps();
        assert_eq!(package["owner"], "Vortex: quen-w3 0.1.0 (nexus 1)");
        assert_eq!(bridge["owner"], "none");
        let note = package["note"].as_str().unwrap();
        assert!(
            note.ends_with("it is Vortex's (quen-w3 0.1.0 (nexus 1)): update it in Vortex"),
            "{note}"
        );
    }

    /// quen_saves lists the saves game_save made that are still in gamesaves, by the ledger: not one gone from the
    /// folder, nor a save quen didn't make.
    #[test]
    fn quen_saves_lists_quens_saves_still_on_disk() {
        let fx = crate::game::tests::Fake::new("doctor-saves");
        let config = fx.shared.config().as_ref().clone().unwrap();
        let dir = config.saves_dir().unwrap();
        let mut ledger = Ledger::open(config.state_dir().join("quen.db")).unwrap();
        for (file, made, tainted) in [
            ("ManualSave_q.sav", Some(1), Some(1)),
            ("ManualSave_gone.sav", Some(1), None),
            ("ManualSave_user.sav", None, None),
        ] {
            let save = quen_core::ledger::Save {
                game_id: 1,
                file: file.into(),
                sha256: "0".repeat(64),
                tainted_by_boot: tainted,
                made_by_boot: made,
                ..Default::default()
            };
            ledger.add_save(&save).unwrap();
        }
        drop(ledger);
        for file in ["ManualSave_q.sav", "ManualSave_user.sav"] {
            fs::write(dir.join(file), "synthetic").unwrap();
        }
        assert_eq!(
            quen_saves(&config),
            json!([{"file": "ManualSave_q.sav", "boot": 1, "tainted": true}])
        );
    }
}
