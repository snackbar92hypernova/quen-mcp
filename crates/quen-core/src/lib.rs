//! quen-core: the platform-neutral logic of quen (docs/intent.md, "Core and adapters"). No Win32 here, so `cargo test`
//! runs on any OS.

pub mod boot; // the native W3 boot's file changes (ported from ini_session.py boot) and their journaled apply
pub mod bridge_log; // W3QuenState.log flags (the bridge's line form) and the "loaded" predicate
pub mod build; // the build pin: PE stamp, exe sha256, known builds, the gate, the KB's build line
pub mod cvar; // config vars found by name in a memory image, read, and their writes planned (cvar_poke's port)
pub mod detect; // the game found in Steam's libraries and GOG's registry entries, as candidates
pub mod disk; // the capture root on disk: usage by boot, kind and age, and a cleanup's plan
pub mod drift; // measure_ab's drift guard: window 1 against the rest, the world fingerprint between windows
pub mod gamedata; // the game's bundles and CR2W files (and REDkit's depot), read-only, and the data tools' questions
pub mod hdr; // half float -> scRGB -> BT.2020 PQ, 16-bit PNG with cICP, EXR, .npy
pub mod image; // measure_image: quen's screenshots decoded (srgb8, PQ PNG, scRGB .npy), crops and their metrics
pub mod input; // w3ctl key tokens and input plans, console_type lines and its prompt check
pub mod journal; // append-only session journal and replay
pub mod ledger; // SQLite schema, migrations, ingest, doc-reference scanner, harness history import
pub mod map; // world_map: the grid's classes, clearance, chokepoints, regions and edges, trace heat layers, profiles, walkers, the picture
pub mod mem; // live-memory reads, scans, pointer chains, struct walks and patch plans (the Windows layer reads and writes)
pub mod mods; // mod fingerprints (content and config hashes), the mod-set hash and diff
pub mod ngfx; // Nsight Graphics captures: the shader decoder, the metadata cleaner, the counts, the perf CSV
pub mod park; // park planner and never-park list
pub mod protocol; // command-channel JSON lines
pub mod reshade; // ReShade-style ini edits, byte-identical to the harness's ini_writer
pub mod saves; // gamesaves snapshots, the guard, the quiet wait and the backup copy
pub mod scene; // placement: the camera pose that frames a subject from a side, the yaw that faces a point
pub mod seeds; // names.tsv -> REA seed file
pub mod settings; // dx12user.settings overrides (W3_SETTINGS / W3_SECTION_SETTINGS semantics)
pub mod stats; // PresentMon command line, CSV stats and the A/B verdict
pub mod sweep; // camera sweeps: data model, the reference interpolation the cam.path verb ports, frame schedules
pub mod trace; // actor traces: samples, tracks, behaviour metrics and their A/B verdicts
pub mod vortex; // Vortex's records: manifests, mods.settings, its mods by quen's units, link states, the launch's refusals
pub mod wcc; // REDkit's wcc_lite compilescripts: its log's script messages, import stubs, the settings it rewrites
