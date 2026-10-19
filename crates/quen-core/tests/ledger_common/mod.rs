//! Synthetic fixtures shared by the ledger tests. Each test file uses a subset.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quen_core::ledger::{BootEnd, BootStart, Capture, Game, Ledger, Measurement, Outcome, Tag};
use serde_json::Value;

/// A ledger path, `quen.db` in a folder of its own in the OS temp folder, unique per test. The folder (with the
/// ledger's backups) goes with it.
pub struct TempDb(pub PathBuf);

impl TempDb {
    pub fn new() -> TempDb {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "quen-ledger-test-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        TempDb(dir.join("quen.db"))
    }

    pub fn dir(&self) -> &Path {
        self.0.parent().unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.dir());
    }
}

/// A synthetic sha256: `n` as 64 hex digits.
pub fn sha(n: u32) -> String {
    format!("{n:064x}")
}

pub fn game(l: &mut Ledger) -> i64 {
    l.add_game(&Game {
        name: "synthetic".into(),
        exe_path: r"%GAME_DIR%\bin\x64\game.exe".into(),
        exe_sha256: sha(1),
        build_stamp: Some("0x00000001".into()),
    })
    .unwrap()
}

/// A boot that started and ended at `t`.
pub fn boot(l: &mut Ledger, game: i64, t: &str) -> i64 {
    let id = l
        .start_boot(&BootStart {
            game_id: game,
            t_start: t.into(),
            ..Default::default()
        })
        .unwrap();
    l.finish_boot(
        id,
        &BootEnd {
            t_end: t.into(),
            outcome: Outcome::Ok,
            failure: None,
            patches: vec![],
        },
    )
    .unwrap();
    id
}

pub fn measurement(l: &mut Ledger, boot: Option<i64>, family: &str, tag: Tag) -> i64 {
    l.add_measurement(&Measurement {
        boot_id: boot,
        capture_id: None,
        metric: "frame_ms".into(),
        value: Some(23.2),
        unit: Some("ms".into()),
        state_label: Some("B".into()),
        method_family: family.into(),
        tag,
        run_id: Some("run-1".into()),
        note: None,
        extraction_id: None,
        world: None,
    })
    .unwrap()
}

pub fn capture(l: &mut Ledger, boot: Option<i64>, path: &str, t: &str) -> i64 {
    l.add_capture(&Capture {
        boot_id: boot,
        path: path.into(),
        sha256: sha(9),
        bytes: 1024,
        kind: "png".into(),
        t: t.into(),
        ..Default::default()
    })
    .unwrap()
}

/// The rows of a query that must succeed.
pub fn rows(l: &Ledger, sql: &str, params: &[Value]) -> Vec<Vec<Value>> {
    l.query(sql, params, Duration::from_secs(10)).unwrap().rows
}

/// The build stamp of the synthetic KB's build line.
pub const STAMP: &str = "0x0BADF00D";

/// The commit the synthetic KB's work tree has checked out.
pub fn commit() -> String {
    "c".repeat(40)
}

/// A synthetic RE knowledge base for the build `exe_sha256` under `dir`: a git work tree `repo` (its `.git` only HEAD
/// and the branch's ref) with the KB at `repo/re`: names.tsv, hooks.tsv, patches.tsv and cvar_layout.tsv, a note under
/// `notes/` and a small frame map under `frame/`. Returns the KB root.
pub fn fake_kb(dir: &Path, exe_sha256: &str) -> PathBuf {
    let repo = dir.join("repo");
    let kb = repo.join("re");
    for sub in [".git/refs/heads", "re/notes", "re/frame"] {
        std::fs::create_dir_all(repo.join(sub)).unwrap();
    }
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(repo.join(".git/refs/heads/main"), format!("{}\n", commit())).unwrap();
    let line = format!("# build: sha256={exe_sha256} stamp={STAMP}\n");
    let files = [
        (
            "names.tsv",
            format!(
                "# synthetic names, not from any real binary\n{line}rva\tname\tkind\tconfidence\tsource\tdate\n\
                 0x1000\tAlpha_Draw\tfunction\tO\ttest\t2026-10-18\n\
                 0x2000\tBeta_Setup\tfunction\tI\ttest\t2026-10-18\n\
                 0x3000\t\tfunction\tO\ttest\t2026-10-18\n\
                 0x3010\tGamma_Table\tlabel\tO\ttest\t2026-10-18\n\
                 0x4000\t\tcode\tO\ttest\t2026-10-18\n"
            ),
        ),
        (
            "hooks.tsv",
            format!(
                "# synthetic sites\n{line}site\tkind\tmod\tversion\tnote\n\
                 0x1000\thook\tModA\t1.0\tdraw hook\n\
                 0x1234\tmid-hook\tModB\t2.0\tinside alpha\n"
            ),
        ),
        (
            "patches.tsv",
            format!("name\trva\told\tnew\tbuild\tboundaries-checked\ncull_off\t0x1F00\t74 05\teb 05\t{STAMP}\t-\n"),
        ),
        (
            "cvar_layout.tsv",
            format!(
                "build\tgroup\tname\tflags\tvalue\tdefault\tlive\tbool\tint\tfloat\n\
                 {STAMP}\t0x08\t0x10\t0x18\t0x20\t0x28\t0x30\t0\t1\t2\n\
                 0x12345678\t0x08\t0x10\t0x18\t0x20\t0x28\t0x30\t0\t1\t2\n"
            ),
        ),
        (
            "notes/alpha.md",
            "# Alpha notes\n\nAlpha_Draw at 0x1000 (VA 0x140001000) draws 0x1A2B3C4D.\n\n## Q1 Allocation\n\
             FUN_140002000 is Beta_Setup; Beta_Setup again. Unknown 0xCAFEBABE stays unknown.\n\n### Detail [V]\n\
             ModB:mid-hook:0BADF00D_1234 sits inside Alpha_Draw.\n```\n## not a heading\n```\n"
                .into(),
        ),
        (
            "frame/MAP.md",
            "# Synthetic frame map\n\n## 2. Colour chain\n\n### 2d. Tonemap (seg 9)\n\n\
             | Draw | Pass | Limits | Status |\n|---|---|---|---|\n\
             | 10 | **0x1A2B3C4D** tonemap | clips above c8.w [C] | V / C (s1) |\n\
             | 11 | 0x00C0FFEE glare | darkens >1 (H) | V |\n\n\
             ### 2f. Composite 0x2B3C4D5E [V/C, all H for code]\n\n\
             | Step | Math | Limit |\n|---|---|---|\n\
             | 0 | `min(rgb*c0.x, c0.w)` per channel | hard clip |\n\
             | 1 | PQ encode, `|x|` | - |\n\n\
             ### 2g. Limits\n\n| # | Location | Limit | Status |\n|---|---|---|---|\n\
             | 1 | 0x1A2B3C4D | above c8.w discarded | V |\n\n## 3. Controls\ntext\n"
                .into(),
        ),
    ];
    for (name, text) in files {
        std::fs::write(kb.join(name), text).unwrap();
    }
    kb
}

/// A synthetic REA evidence id.
pub fn ev(n: u32) -> String {
    format!("ev_{n:064x}")
}

/// The analysis profile digest of [`fake_bundle`]'s records, `profile` 1 or 2.
pub fn profile(n: u32) -> String {
    sha(0x700 + n)
}

/// A synthetic REA evidence bundle for the exe `exe_sha256`, analysed with `profile(p)`, written to `path`: a
/// decompile of 0x140001000 (`ev(base)`), its callers 0x140002000 and the unknown 0x140009000 (`ev(base + 1)`), and a
/// record without a subject (`ev(base + 2)`).
pub fn fake_bundle(path: &Path, exe_sha256: &str, p: u32, base: u32) -> PathBuf {
    let provider = serde_json::json!({"id": "ghidra", "name": "Ghidra", "version": "12.1.4"});
    let record = |id: u32, subject: Value, operation: &str, raw: Value| {
        serde_json::json!({
            "evidence_id": ev(id),
            "subject": subject,
            "provider": provider,
            "analysis_profile": {
                "provider": provider,
                "parameters": {"analysis_seeds": {"path": "%KB%/seeds/synthetic.tsv", "sha256": sha(0x5eed + p)}},
                "digest": profile(p),
            },
            "predicate_type": "rea.static",
            "operation": operation,
            "parameters": {"procedure": "0x140001000", "document": "game.exe"},
            "raw_result": raw,
            "normalized_result": {},
            "confidence": "observed",
            "authority": "shipped-artifact",
            "environment": null,
            "limitations": [],
            "locations": [{"kind": "address", "address": "0x140001000"}],
            "evidence_links": [],
        })
    };
    let subject = serde_json::json!({"name": "game.exe", "digest": {"sha256": exe_sha256}, "format": "pe",
                                     "architecture": "x86_64", "local_path": "%GAME_DIR%/game.exe"});
    let bundle = serde_json::json!({
        "artifacts": [], "providers": [provider], "environments": [], "scenarios": [], "captures": [], "unknowns": [],
        "records": [
            record(base, subject.clone(), "procedure_pseudo_code", serde_json::json!("void synthetic(void) {}")),
            record(base + 1, subject, "procedure_callers", serde_json::json!(["0x140002000", "ram:0x140009000"])),
            record(base + 2, Value::Null, "build_reconstruction_obligation_ledger", serde_json::json!({})),
        ],
    });
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bundle.to_string()).unwrap();
    path.into()
}
