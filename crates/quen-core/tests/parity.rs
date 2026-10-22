//! Offline parity with the Python harness: the same synthetic inputs go through the harness's own
//! functions and through quen-core, and the answers must match. `parity/driver.py` runs the harness side from the folder
//! named by `QUEN_HARNESS_DIR`; without it, or without Python, each test prints a skip line and passes.
//!
//! A case marked `known` is a reviewed difference: the test fails when it stops differing, so the mark goes once a side
//! changes. `cargo test --test parity -- --nocapture` prints one line per case.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use quen_core::boot::{self, Inputs, RenoDx};
use quen_core::cvar::{self, Layout, Set};
use quen_core::journal::{Fs, Journal, replay, sha256_hex};
use quen_core::mem::{self, Memory, Region};
use quen_core::park::{self, Entry, Listing};
use quen_core::stats::{self, Summary};
use quen_core::{bridge_log, input, settings};
use serde_json::{Value, json};

struct Case {
    name: &'static str,
    input: Value,
    /// Why the two sides are known to answer differently; `None`: they must agree.
    // ponytail: a known difference only asserts "still differs", not both answers; pin them if one ever matters.
    known: Option<&'static str>,
}

/// Runs `cases` through the driver as `kind` and through `rust`, and compares each pair of answers with `eq`. Both
/// refusing counts as equal: the messages differ by design.
fn parity(
    kind: &str,
    cases: Vec<Case>,
    rust: impl Fn(&Value) -> Value,
    eq: impl Fn(&Value, &Value) -> bool,
) {
    let Some(harness) = std::env::var_os("QUEN_HARNESS_DIR") else {
        println!("parity {kind}: skipped, QUEN_HARNESS_DIR is not set");
        return;
    };
    let scratch = Scratch::new(kind);
    let inputs: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| {
            let mut input = case.input.clone();
            input["kind"] = kind.into();
            input["dir"] = scratch.0.join(i.to_string()).to_string_lossy().into();
            input
        })
        .collect();
    let Some(python) = driver(&harness, &inputs) else {
        return;
    };
    assert_eq!(python.len(), cases.len(), "one answer per case");
    let mut wrong = Vec::new();
    for ((case, input), py) in cases.iter().zip(&inputs).zip(&python) {
        let rs = rust(input);
        let equal = match (py.get("ok"), rs.get("ok")) {
            (Some(p), Some(r)) => eq(p, r),
            (None, None) => true,
            _ => false,
        };
        let known = case
            .known
            .map(|why| format!(" ({why})"))
            .unwrap_or_default();
        let shown = if equal { "equal" } else { "differs" };
        println!("parity {kind:<10} {:<30} {shown}{known}", case.name);
        if equal == case.known.is_some() {
            let what = if equal {
                "now equal: drop its known mark"
            } else {
                "differs"
            };
            wrong.push(format!(
                "{}: {what}\n  input  {input}\n  python {py}\n  rust   {rs}",
                case.name
            ));
        }
    }
    assert!(wrong.is_empty(), "parity {kind}:\n{}", wrong.join("\n"));
}

/// The harness's answers, one per input; `None` (skipped) when Python can't be started.
fn driver(harness: &OsStr, inputs: &[Value]) -> Option<Vec<Value>> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/driver.py");
    let spawned = Command::new("python")
        .arg("-B")
        .arg(&script)
        .env("QUEN_HARNESS_DIR", harness)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            println!("parity: skipped, python can't be started: {e}");
            return None;
        }
    };
    let request = serde_json::to_vec(inputs).unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "driver failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).expect("driver answers JSON"))
}

/// One test's scratch folder in the OS temp folder; everything the driver made in it goes with it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(kind: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("quen-parity-{kind}-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(input: &Value, key: &str) -> String {
    input[key].as_str().unwrap().to_string()
}

fn refused(e: impl std::fmt::Display) -> Value {
    json!({"err": e.to_string()})
}

// ---- dx12user.settings overrides -------------------------------------------------------------------------------------

/// A synthetic settings file: `MotionBlur` is in two sections, `[Empty]` has no keys.
const LF: &str = "[Rendering]\nAllowFG=true\nMotionBlur=true\n[Rendering/MtRender]\nThreads=4\n\
                  [Empty]\n[Gameplay]\nAutoRotate=true\nMotionBlur=false\n";

/// Bytes as a string of chars U+0000..U+00FF, one per byte: the driver reads it as Latin-1.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

#[test]
fn settings_overrides() {
    let crlf = LF.replace('\n', "\r\n");
    let lit = "Key=1\nKeyX=2\n Indented=3\nkey=4\nA.B=5\nSpaced = 6\nA*=7\n";
    let bom = b"\xef\xbb\xbfAllowFG=true\r\nX=1\r\n";
    let template = "Python's section replace reads the value as a regex template";
    let appended = "the harness refuses a missing section, quen appends it (finding 63)";
    let c = |name, data: &[u8], flat: &str, sections: &str, known| Case {
        name,
        input: json!({"data": latin1(data), "flat": flat, "sections": sections}),
        known,
    };
    let (lf, crlf) = (LF.as_bytes(), crlf.as_bytes());
    let cases = vec![
        c("flat_lf", lf, "AllowFG=false", "", None),
        c("flat_crlf", crlf, "AllowFG=false", "", None),
        c(
            "flat_later_wins",
            lf,
            "AllowFG=1,Threads=8,AllowFG=2",
            "",
            None,
        ),
        c("flat_missing_key", lf, "AllowFG=false,Missing=1", "", None),
        c(
            "flat_key_in_two_sections",
            crlf,
            "MotionBlur=false",
            "",
            None,
        ),
        c("flat_literal_key", lit.as_bytes(), "Key=9", "", None),
        c(
            "flat_regex_chars_in_key",
            lit.as_bytes(),
            "A.B=0,A*=0",
            "",
            None,
        ),
        c("flat_indented_line", lit.as_bytes(), "Indented=0", "", None),
        c("flat_other_case", lit.as_bytes(), "KEY=0", "", None),
        c("flat_spaced_key", lit.as_bytes(), "Spaced=0", "", None),
        c(
            "flat_value_literal",
            lf,
            r"Threads=a\1\\b$0${x}=c\n",
            "",
            None,
        ),
        c("flat_last_line_no_eol", b"A=1\nB=2", "B=3", "", None),
        c("flat_last_line_cr_only", b"A=1\r\nB=2\r", "B=3", "", None),
        c("flat_bom_first_line", bom, "AllowFG=false", "", None),
        c("flat_bom_other_line", bom, "X=2", "", None),
        c(
            "flat_non_utf8_bytes",
            b"Name=\xe9t\xe9\nX=1\n",
            "X=2",
            "",
            None,
        ),
        c(
            "flat_unicode_key",
            "Cl\u{e9}=1\nX=1\n".as_bytes(),
            "Cl\u{e9}=2",
            "",
            None,
        ),
        c(
            "flat_bare_cr_in_value",
            b"A=1\r2\nB=3\n",
            "A=9",
            "",
            Some("Python's value stops at a bare CR, Rust's runs to the LF"),
        ),
        c(
            "flat_line_break_in_value",
            b"A=0\nC=0\n",
            "A=1\nC=2",
            "",
            Some("Python writes the line break (two C= lines), Rust refuses"),
        ),
        c("flat_no_equals", lf, "NoEquals", "", None),
        c("flat_empty_key", lf, "=1", "", None),
        c("flat_header_as_key", lf, "[Rendering]=1", "", None),
        c("flat_bad_second_item", lf, "AllowFG=1,Threads", "", None),
        c("sec_replace", lf, "", "Gameplay:MotionBlur=true", None),
        c(
            "sec_replace_crlf",
            crlf,
            "",
            "Rendering/MtRender:Threads=2",
            None,
        ),
        c("sec_insert", lf, "", "Gameplay:Hdr=1", None),
        c("sec_insert_crlf", crlf, "", "Gameplay:Hdr=1", None),
        c("sec_insert_mixed_eol", b"[A]\nX=1\r\n", "", "A:Y=2", None),
        c("sec_insert_empty_section", lf, "", "Empty:New=1", None),
        c(
            "sec_insert_last_section",
            b"[A]\nX=1\n[B]\n",
            "",
            "B:Y=2",
            None,
        ),
        c("sec_insert_header_only", b"[B]\r\n", "", "B:Y=2", None),
        c("sec_last_line_no_eol", b"[S]\nA=1", "", "S:A=2", None),
        c("sec_missing", lf, "", "Missing:A=1", Some(appended)),
        c("sec_other_case", lf, "", "rendering:A=1", None),
        c(
            "sec_header_trailing_space",
            b"[A] \nX=1\n",
            "",
            "A:X=2",
            None,
        ),
        c("sec_header_no_eol", b"X=1\n[A]", "", "A:X=2", None),
        c(
            "sec_regex_chars",
            b"[AxB]\nX=0\n[A.B]\nX=0\n",
            "",
            "A.B:X=1",
            None,
        ),
        c(
            "sec_bom_header",
            b"\xef\xbb\xbf[A]\r\nX=1\r\n",
            "",
            "A:X=2",
            None,
        ),
        c(
            "sec_duplicate_header",
            b"[A]\nX=1\n[A]\nY=1\n",
            "",
            "A:X=2",
            Some("Python edits the first [A], Rust refuses a repeated header"),
        ),
        c(
            "sec_key_twice",
            b"[A]\nX=1\nX=2\n[B]\nX=3\n",
            "",
            "A:X=9",
            None,
        ),
        c(
            "sec_key_in_other_section",
            b"[A]\nX=1\n[B]\nX=3\n",
            "",
            "B:X=9",
            None,
        ),
        c(
            "sec_later_wins",
            lf,
            "",
            "Gameplay:Hdr=1;Gameplay:Hdr=2;Rendering:AllowFG=0",
            None,
        ),
        c("sec_insert_backslash", lf, "", r"Empty:New=a\1", None),
        c(
            "sec_replace_group_ref",
            lf,
            "",
            r"Gameplay:MotionBlur=a\1b",
            Some(template),
        ),
        c(
            "sec_replace_double_backslash",
            lf,
            "",
            r"Gameplay:MotionBlur=a\\b",
            Some(template),
        ),
        c("sec_no_colon", lf, "", "NoColon=1", None),
        c("sec_empty_section", lf, "", ":A=1", None),
        c("sec_no_equals", lf, "", "Gameplay:NoEquals", None),
        c(
            "sec_empty_key",
            lf,
            "",
            "Gameplay:=1",
            Some("Python inserts a line =1, Rust refuses an empty key"),
        ),
        c("empty_specs", lf, "", "", None),
        c("separators_only", lf, ",AllowFG=0,", ";;", None),
        c(
            "flat_then_sections",
            crlf,
            "Threads=8,AllowFG=false",
            "Rendering/MtRender:Threads=9;Gameplay:Hdr=1",
            None,
        ),
        c(
            "flat_ok_sections_bad",
            crlf,
            "AllowFG=false",
            "Nope:A=1",
            Some(appended),
        ),
        c(
            "flat_bad_sections_ok",
            crlf,
            "Nope=1",
            "Gameplay:Hdr=1",
            None,
        ),
        c("empty_file_flat", b"", "A=1", "", None),
        c("empty_file_section", b"", "", "A:B=1", Some(appended)),
    ];
    parity(
        "settings",
        cases,
        |input| {
            let data: Vec<u8> = text(input, "data")
                .chars()
                .map(|c| u8::try_from(c).unwrap())
                .collect();
            match settings::validate(&data, &text(input, "flat"), &text(input, "sections")) {
                Ok(out) => json!({"ok": latin1(&out)}),
                Err(e) => refused(e),
            }
        },
        |p, r| p == r,
    );
}

// ---- park plans ------------------------------------------------------------------------------------------------------

/// A synthetic game: the bridges, the harness's own mods, a junction, a stray file, two plain mods and an add-on.
const MODS: &[(&str, &str)] = &[
    ("modZeta", "dir"),
    ("modAlpha", "dir"),
    ("modLinked", "junction"),
    ("readme.txt", "file"),
    ("modW3GameStateBridge", "dir"),
    ("modNativeBridge", "dir"),
    ("modRenoDXTest", "dir"),
];
const BIN: &[(&str, &str)] = &[
    ("witcher3.exe", "file"),
    ("Tweaks.asi", "file"),
    ("W3GameStateBridge.asi", "file"),
    ("WitcherNativeBridge.asi", "file"),
    ("Other.ADDON64", "file"),
    ("dxgi.dll", "file"),
    ("d3d12.dll", "file"),
    ("renodx-thewitcher3remastered.addon64", "file"),
    ("renodx-devkit.addon64", "file"),
];
/// The harness's own names it never parks (W3_PARK_RENODX unset): what the adapter adds to `W3_NEVER_PARK` (park.rs).
/// The W3 Game State Bridge's pair, as quen learns it from the files (park::learn): the harness never parks it.
const GSB: &[[&str; 2]] = &[["W3GameStateBridge.asi", "modW3GameStateBridge"]];

const HARNESS_OWN: &[&str] = &[
    "modRenoDXTest",
    "renodx-devkit.addon64",
    "renodx-thewitcher3remastered.addon64",
];

fn listing(dir: &Path) -> Vec<Entry> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: kind.is_dir(),
                is_link: kind.is_symlink(),
            }
        })
        .collect()
}

#[test]
fn park_plans() {
    let with = |extra: &[(&'static str, &'static str)], base: &[(&'static str, &'static str)]| {
        [base, extra].concat()
    };
    let quen_mods = with(&[("modQuen", "dir")], MODS);
    let quen_bin = with(&[("W3Quen.asi", "file")], BIN);
    let unicode = with(&[("modB\u{e9}ta", "dir")], MODS);
    let asi_dir = with(&[("plugins.asi", "dir")], BIN);
    let spaced_mods = with(&[("mod Spaced", "dir")], MODS);
    let spaced_bin = with(&[("My Tweaks.asi", "file")], BIN);
    let link_mods = with(&[("modSym", "dirlink")], MODS);
    let link_bin = with(&[("Sym.asi", "filelink"), ("Linked.asi", "junction")], BIN);
    let c =
        |name, mods: &[(&str, &str)], bin: &[(&str, &str)], park: &str, keep: &str, known| Case {
            name,
            input: json!({"mods": mods, "bin": bin, "park": park, "keep": keep}),
            known,
        };
    let kept = "Python parks a name both requested and kept, Rust refuses";
    let quen = "Python predates quen's own mods";
    let link = "Python skips a junction in Mods but refuses the plan for any other link, Rust skips every link";
    let cases = vec![
        c("one_name", MODS, BIN, "modAlpha", "", None),
        c(
            "any_case_and_home",
            MODS,
            BIN,
            " modalpha , TWEAKS.ASI,other.addon64,DXGI.DLL",
            "",
            None,
        ),
        c("all", MODS, BIN, "ALL", "", None),
        c("all_keep", MODS, BIN, "ALL", "MODZETA, dxgi.dll", None),
        c(
            "names_beside_all",
            MODS,
            BIN,
            "tweaks.asi,ALL,modZeta",
            "modAlpha",
            None,
        ),
        c(
            "bridge_named",
            MODS,
            BIN,
            "modAlpha,w3gamestatebridge.asi",
            "",
            None,
        ),
        c(
            "bridge_pair_named",
            MODS,
            BIN,
            "W3GameStateBridge.asi,modW3GameStateBridge",
            "",
            Some(
                "quen parks W3GSB both halves together (its pair, learned from the files); the harness never parks it",
            ),
        ),
        c(
            "bridge_beside_all",
            MODS,
            BIN,
            "ALL,modNativeBridge",
            "",
            None,
        ),
        c("harness_mod_named", MODS, BIN, "modRenoDXTest", "", None),
        c(
            "renodx_addon_named",
            MODS,
            BIN,
            "renodx-thewitcher3remastered.addon64",
            "",
            None,
        ),
        c("unknown_mod", MODS, BIN, "modAlpha,modMissing", "", None),
        c("unknown_asi", MODS, BIN, "Missing.asi", "", None),
        c("junction", MODS, BIN, "modLinked", "", None),
        c("file_in_mods", MODS, BIN, "readme.txt", "", None),
        c("exe_not_parkable", MODS, BIN, "witcher3.exe", "", None),
        c("repeated", MODS, BIN, "modAlpha,MODALPHA", "", None),
        c("dot_dot", MODS, BIN, "..", "", None),
        c("path", MODS, BIN, "../modAlpha", "", None),
        c("space", MODS, BIN, "mod Alpha", "", None),
        c("leading_dot", MODS, BIN, ".hidden", "", None),
        c("leading_dash", MODS, BIN, "-x", "", None),
        c("lowercase_all", MODS, BIN, "all", "", None),
        c("empty", MODS, BIN, "", "", None),
        c("blank_items", MODS, BIN, " , ", "", None),
        c(
            "all_twice",
            MODS,
            BIN,
            "ALL,ALL",
            "",
            Some("Python drops every ALL, Rust refuses the repeat"),
        ),
        c(
            "keep_typo_with_all",
            MODS,
            BIN,
            "ALL",
            "modZetta",
            Some("Python ignores an unknown keep name and parks modZeta, Rust refuses"),
        ),
        c(
            "keep_typo_without_all",
            MODS,
            BIN,
            "modZeta",
            "modTypo",
            Some("Python reads keep only under ALL, Rust checks it always"),
        ),
        c(
            "named_and_kept",
            MODS,
            BIN,
            "modAlpha,ALL",
            "modAlpha",
            Some(kept),
        ),
        c(
            "named_and_kept_no_all",
            MODS,
            BIN,
            "modAlpha",
            "modalpha",
            Some(kept),
        ),
        c(
            "quen_mods_under_all",
            &quen_mods,
            &quen_bin,
            "ALL",
            "",
            Some(quen),
        ),
        c(
            "quen_mod_named",
            &quen_mods,
            &quen_bin,
            "modQuen",
            "",
            Some(quen),
        ),
        c("unicode_under_all", &unicode, BIN, "ALL", "", None),
        c("unicode_named", &unicode, BIN, "modb\u{c9}ta", "", None),
        c(
            "space_folder_named",
            &spaced_mods,
            BIN,
            "mod Spaced",
            "",
            None,
        ),
        c("space_folder_under_all", &spaced_mods, BIN, "ALL", "", None),
        c("space_asi_under_all", MODS, &spaced_bin, "ALL", "", None),
        c(
            "unicode_kept_under_all",
            &unicode,
            BIN,
            "ALL",
            "modb\u{c9}ta",
            None,
        ),
        c(
            "space_kept_under_all",
            &spaced_mods,
            &spaced_bin,
            "ALL",
            "mod Spaced,My Tweaks.asi",
            None,
        ),
        c("symlink_folder_named", &link_mods, BIN, "modSym", "", None),
        c(
            "symlink_folder_under_all",
            &link_mods,
            BIN,
            "ALL",
            "",
            Some(link),
        ),
        c("symlink_asi_named", MODS, &link_bin, "Sym.asi", "", None),
        c(
            "junction_asi_named",
            MODS,
            &link_bin,
            "Linked.asi",
            "",
            None,
        ),
        c(
            "links_in_bin_under_all",
            MODS,
            &link_bin,
            "ALL",
            "",
            Some(link),
        ),
        c(
            "devkit_addon_named",
            MODS,
            BIN,
            "renodx-devkit.addon64",
            "",
            None,
        ),
        c(
            "asi_folder_under_all",
            MODS,
            &asi_dir,
            "ALL",
            "",
            Some("Python lists a folder named *.asi under ALL and refuses, Rust skips it"),
        ),
        c("asi_folder_named", MODS, &asi_dir, "plugins.asi", "", None),
    ];
    let never: Vec<&str> = park::W3_NEVER_PARK
        .iter()
        .chain(HARNESS_OWN)
        .copied()
        .collect();
    parity(
        "park",
        cases,
        |input| {
            let dir = PathBuf::from(text(input, "dir"));
            let (mods_dir, bin_dir) = (dir.join("Mods"), dir.join("bin"));
            let (mods, bin) = (listing(&mods_dir), listing(&bin_dir));
            let plan = park::plan(
                &text(input, "park"),
                &text(input, "keep"),
                &never,
                GSB,
                Listing {
                    dir: &mods_dir,
                    entries: &mods,
                },
                Listing {
                    dir: &bin_dir,
                    entries: &bin,
                },
                &dir.join("parked"),
                &park::More::default(),
            );
            match plan {
                Ok(plan) => {
                    // ponytail: compared as sets; the order of independent renames carries no meaning.
                    let mut names: Vec<String> = plan
                        .moves
                        .iter()
                        .map(|m| m.from.file_name().unwrap().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    json!({"ok": names})
                }
                Err(e) => refused(e),
            }
        },
        |p, r| p == r,
    );
}

// ---- bridge log ------------------------------------------------------------------------------------------------------

/// Bridge log lines, one per flags value.
fn log(flags: &[i64]) -> String {
    flags
        .iter()
        .enumerate()
        .map(|(seq, f)| format!("push seq={seq} flags={f} weather=1 region=2 hour=12\n"))
        .collect()
}

#[test]
fn bridge_flags_and_loaded() {
    use bridge_log::{CUTSCENE, DLGFLAG, FADE, SCENE, VALID};
    let c = |name, before: String, after: String, known| Case {
        name,
        input: json!({"before": before, "after": after}),
        known,
    };
    let crlf = |flags: &[i64]| log(flags).replace('\n', "\r\n");
    let mut cases = vec![
        c("load_then_gameplay", log(&[3, 3]), log(&[0, 3]), None),
        c("fade_counts_as_load", log(&[3]), log(&[257, 73]), None),
        c("load_before_the_call", log(&[0]), log(&[3, 3]), None),
        c("no_load", String::new(), log(&[3, 3]), None),
        c("ends_loading", String::new(), log(&[3, 0]), None),
        c("photo_and_env_bits", String::new(), log(&[0, 1157]), None),
        c("nothing_after", log(&[0, 3]), String::new(), None),
        c("empty_log", String::new(), String::new(), None),
        c("crlf", crlf(&[3]), crlf(&[0, 3]), None),
        c("bom", format!("\u{feff}{}", log(&[3])), log(&[0, 3]), None),
        c(
            "word_boundary_and_junk",
            String::new(),
            "prevflags=7 my_flags=8 2flags=9 (flags=17) x.flags=33\n\
             flags=-1 flags= flags=- flags=--2 flags=x flags=flags=65\nflags=129abc flags=007\n"
                .into(),
            None,
        ),
        c("negative_last", String::new(), log(&[0, -1]), None),
        c(
            "beyond_i64",
            String::new(),
            "flags=0\nflags=99999999999999999999\nflags=3\n".into(),
            Some("Python keeps a value beyond i64, Rust skips it (flags are 11 bits)"),
        ),
    ];
    for (name, busy) in [
        ("ends_in_scene", SCENE),
        ("ends_in_cutscene", CUTSCENE),
        ("ends_in_fade", FADE),
        ("ends_in_dialogue", DLGFLAG),
    ] {
        cases.push(c(name, String::new(), log(&[0, VALID | busy]), None));
    }
    parity(
        "bridge",
        cases,
        |input| {
            let before = text(input, "before");
            let all = bridge_log::flags(&(before.clone() + &text(input, "after")));
            let loaded = bridge_log::loaded_after(&all, bridge_log::flags(&before).len());
            json!({"ok": {"flags": all, "loaded": loaded}})
        },
        |p, r| p == r,
    );
}

#[test]
fn bridge_bit_names() {
    let case = Case {
        name: "sig_bits",
        input: json!({}),
        known: Some("quen's state adds bit 2048, menu (quen-w3 0.1.6); the harness has none"),
    };
    parity(
        "bits",
        vec![case],
        |_| {
            let names: Vec<&str> = (0..bridge_log::BITS.len())
                .map(|i| bridge_log::names(1 << i)[0])
                .collect();
            json!({"ok": names})
        },
        |p, r| p == r,
    );
}

// ---- PresentMon CSV stats --------------------------------------------------------------------------------------------

const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,\
                      AllowsTearing,PresentMode,FrameType,CPUStartTime,MsBetweenSimulationStart,MsBetweenPresents,\
                      MsBetweenDisplayChange,MsInPresentAPI,MsUntilDisplayed,MsCPUBusy,MsCPUWait,MsGPULatency,\
                      MsGPUTime,MsGPUBusy,MsGPUWait";

/// A synthetic PresentMon CSV: five warm-up rows of 999 ms, then one row per frame time. GPU busy is frame − 2, GPU
/// time frame − 1, CPU busy frame / 2.
fn pm(frames: &[f64]) -> String {
    let row = |ms: f64| {
        format!(
            "game.exe,4242,0x0000000000ABCDEF,DXGI,0,0,1,Hardware: Independent Flip,Application,1234.5,NA,{ms},{ms},\
             0.1,{ms},{},0.2,NA,{},{},0.3\n",
            ms / 2.0,
            ms - 1.0,
            ms - 2.0
        )
    };
    let mut out = format!("{HEADER}\n");
    for _ in 0..stats::SKIP_ROWS {
        out += &row(999.0);
    }
    for &ms in frames {
        out += &row(ms);
    }
    out
}

/// A table of the four stats columns, five warm-up rows of 1, then `rows`.
fn table(rows: &str) -> String {
    "MsBetweenPresents,MsGPUBusy,MsCPUBusy,PresentMode\n".to_string()
        + &"1,1,1,m\n".repeat(stats::SKIP_ROWS)
        + rows
}

/// Numbers equal within `tol`, everything else exactly.
fn close(p: &Value, r: &Value, tol: f64) -> bool {
    match (p, r) {
        (Value::Number(p), Value::Number(r)) => {
            (p.as_f64().unwrap() - r.as_f64().unwrap()).abs() <= tol
        }
        (Value::Array(p), Value::Array(r)) => {
            p.len() == r.len() && p.iter().zip(r).all(|(p, r)| close(p, r, tol))
        }
        (Value::Object(p), Value::Object(r)) => {
            p.len() == r.len()
                && p.iter()
                    .all(|(k, p)| r.get(k).is_some_and(|r| close(p, r, tol)))
        }
        _ => p == r,
    }
}

#[test]
fn presentmon_stats() {
    let c = |name, csv: String, known| Case {
        name,
        input: json!({"csv": csv}),
        known,
    };
    let mut seed = 0x5eed_u64;
    let long: Vec<f64> = (0..600)
        .map(|_| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            8.0 + (seed >> 40) as f64 / f64::from(1 << 24) * 32.0
        })
        .collect();
    let ten: Vec<f64> = (1..=10).map(f64::from).collect();
    let quoted = pm(&[10.0, 12.0, 14.0])
        .replacen("PresentMode", "\"PresentMode\"", 1)
        .replacen("MsBetweenPresents", "\"MsBetweenPresents\"", 1);
    let nan = "Python averages it to NaN, Rust refuses the capture";
    let nothing = "Python reports NaN, Rust refuses";
    let ragged = "Python reads a ragged row, Rust refuses the table";
    let cases = vec![
        c("ten_frames", pm(&ten), None),
        c("one_frame", pm(&[16.0]), None),
        c("odd_count", pm(&[3.0, 1.0, 2.0]), None),
        c("long_window", pm(&long), None),
        c("crlf", pm(&ten).replace('\n', "\r\n"), None),
        // Neither side tells generated frames from real ones: every present counts (PITFALLS: frame gen).
        c(
            "frame_gen_rows",
            pm(&ten).replacen(",Application,", ",AMD_AFMF,", 9),
            None,
        ),
        c("bom_and_quoted_header", format!("\u{feff}{quoted}"), None),
        c("blank_lines", pm(&ten).replacen("\n", "\n\n", 3), None),
        c(
            "na_and_blank_cells",
            table("10,NA,,m\n20,,NA,m\n30,6,4,NA\n40,8,,\n"),
            None,
        ),
        c(
            "na_frame_cell",
            table("10,1,1,m\nNA,4,1,m\n30,6,1,m\n"),
            Some("Python's frames counts rows, Rust's count frame-time cells"),
        ),
        c(
            "spaces_around_numbers",
            table(" 10 , 8 , 5 ,m\n 20,9 ,6,m\n"),
            None,
        ),
        c(
            "space_only_cell",
            table("10,8, ,m\n20,9,6,m\n"),
            Some("Python fails on float(' '), Rust reads it as blank"),
        ),
        c(
            "lowercase_header",
            pm(&ten).replacen("MsBetweenPresents", "msbetweenpresents", 1),
            Some(
                "Python matches column names exactly as PresentMon writes them, Rust ignores case",
            ),
        ),
        c("nan_cell", table("10,1,1,m\nNaN,1,1,m\n"), Some(nan)),
        c("inf_cell", table("10,1,1,m\ninf,1,1,m\n"), Some(nan)),
        c("text_cell", table("10,1,1,m\n20,fast,1,m\n"), None),
        c(
            "bad_cell_in_warm_up",
            "MsBetweenPresents\nbad\n".to_string() + &"1\n".repeat(stats::SKIP_ROWS),
            None,
        ),
        c(
            "no_frame_column",
            "MsGPUBusy,PresentMode\n".to_string() + &"1,a\n".repeat(8),
            Some(nothing),
        ),
        c("warm_up_only", pm(&[]), Some(nothing)),
        c("short_row", pm(&ten) + "game.exe,1\n", Some(ragged)),
        c(
            "long_row",
            pm(&ten).replacen("0.3\n", "0.3,extra\n", 7),
            Some(ragged),
        ),
        c(
            "quoted_cells",
            "PresentMode,\"MsBetweenPresents\"\n".to_string()
                + &"x,1\n".repeat(stats::SKIP_ROWS)
                + "\"Mode, \"\"odd\"\"\nline\",2\n\n\"Mode, \"\"odd\"\"\nline\",4",
            None,
        ),
    ];
    parity(
        "presentmon",
        cases,
        |input| {
            let s = match stats::parse(&text(input, "csv")) {
                Ok(s) => s,
                Err(e) => return refused(e),
            };
            // The harness's NaN for a column without numbers is quen's None.
            let of = |col: &Option<Summary>, f: fn(&Summary) -> f64| {
                col.as_ref().map_or(json!("nan"), |s| json!(f(s)))
            };
            let (frame, gpu, cpu) = (&Some(s.frame_ms.clone()), &s.gpu_busy_ms, &s.cpu_busy_ms);
            json!({"ok": {
                "perf": {
                    "frames": s.frame_ms.count,
                    "frame_ms": s.frame_ms.mean,
                    "gpu_busy_ms": of(gpu, |s| s.mean),
                    "gpu_time_ms": of(&s.gpu_time_ms, |s| s.mean),
                    "cpu_busy_ms": of(cpu, |s| s.mean),
                },
                "medians": [s.frame_ms.count, s.frame_ms.median, of(gpu, |s| s.median), of(cpu, |s| s.median)],
                "p95": [of(frame, |s| s.p95), of(gpu, |s| s.p95)],
            }})
        },
        // perf rounds means to 3 places and medians to 2; numpy's p95 is the same interpolation.
        |p, r| {
            close(&p["perf"], &r["perf"], 5e-4 + 1e-9)
                && close(&p["medians"], &r["medians"], 5e-3 + 1e-9)
                && close(&p["p95"], &r["p95"], 1e-9)
        },
    );
}

// ---- cvars (cvar_poke) -----------------------------------------------------------------------------------------------

/// Where the synthetic images load.
const CVAR_BASE: u64 = 0x7ff6_0000_0000;

/// A memory image: reads at `CVAR_BASE` + offset.
struct Flat(Vec<u8>);

impl Memory for Flat {
    fn read(&self, address: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let at = address.wrapping_sub(CVAR_BASE) as usize;
        let src = self.0.get(at..at.saturating_add(buf.len()));
        buf.copy_from_slice(src.ok_or_else(|| std::io::Error::other("outside the image"))?);
        Ok(())
    }

    fn regions(&self) -> std::io::Result<Vec<Region>> {
        Ok(vec![])
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A PE-shaped image like cvar_poke's selftest one: headers, `.text` (pointers to a name, which are not objects),
/// `.rdata` (the strings) and `.data` (the var objects, laid out by `l`). A var's type is an index into `l.codes`, or
/// `None` for a code the layout doesn't know; a var named `BadPtr` gets a null value pointer.
fn cvar_image(l: &Layout, vars: &[(&str, &str, Option<usize>, f64, f64)]) -> Vec<u8> {
    let (text, rdata, data) = (0x1000, 0x2000, 0x3000);
    let mut b = vec![0u8; 0x4000];
    let put = |b: &mut Vec<u8>, at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
    put(&mut b, 0x3c, &0x80u32.to_le_bytes());
    put(&mut b, 0x80, b"PE\0\0");
    put(&mut b, 0x86, &3u16.to_le_bytes());
    put(&mut b, 0x94, &0xf0u16.to_le_bytes());
    let sections = [
        (".text", text, 0x6000_0020u32),
        (".rdata", rdata, 0x4000_0040),
        (".data", data, 0xc000_0040),
    ];
    for (i, (name, va, flags)) in sections.into_iter().enumerate() {
        let s = 0x80 + 24 + 0xf0 + 40 * i;
        put(&mut b, s, name.as_bytes());
        put(&mut b, s + 8, &0x1000u32.to_le_bytes());
        put(&mut b, s + 12, &(va as u32).to_le_bytes());
        put(&mut b, s + 36, &flags.to_le_bytes());
    }
    let mut strings: Vec<(String, u64)> = Vec::new();
    let mut next = rdata;
    let mut string = |b: &mut Vec<u8>, s: &str| {
        if let Some((_, at)) = strings.iter().find(|(t, _)| t == s) {
            return *at;
        }
        put(b, next + 1, s.as_bytes());
        let at = CVAR_BASE + next as u64 + 1;
        next += s.len() + 2;
        strings.push((s.into(), at));
        at
    };
    let ends = [
        l.group + 8,
        l.name + 8,
        l.value + 8,
        l.flags + 4,
        l.default + 4,
        l.live + 4,
    ];
    let stride = ends.into_iter().max().unwrap().next_multiple_of(8) as usize;
    let unknown = (0..=255).find(|c| !l.codes.contains(c)).unwrap();
    for (i, &(group, name, kind, default, live)) in vars.iter().enumerate() {
        let obj = data + i * stride;
        let field = |offset: u64| obj + offset as usize;
        let (group, name_at) = (string(&mut b, group), string(&mut b, name));
        put(&mut b, field(l.group), &group.to_le_bytes());
        put(&mut b, field(l.name), &name_at.to_le_bytes());
        let code = kind.map_or(unknown, |k| l.codes[k]);
        put(
            &mut b,
            field(l.flags),
            &(0x200 | u32::from(code)).to_le_bytes(),
        );
        let value = if name == "BadPtr" {
            0
        } else {
            CVAR_BASE + field(l.live) as u64
        };
        put(&mut b, field(l.value), &value.to_le_bytes());
        for (offset, v) in [(l.default, default), (l.live, live)] {
            let bytes = match kind {
                Some(0) => vec![v as u8],
                Some(2) => (v as f32).to_le_bytes().to_vec(),
                _ => (v as i32).to_le_bytes().to_vec(),
            };
            put(&mut b, field(offset), &bytes);
        }
    }
    let use_x = string(&mut b, "UseX");
    for at in (text + 0x100..text + 0x200).step_by(8) {
        put(&mut b, at, &use_x.to_le_bytes());
    }
    b
}

/// cvar_poke's `run` (find, show, encode, the write) on a synthetic image, against quen-core's find, read and plan.
/// The layout is the KB's row (`cvar_layout.tsv` in `QUEN_KB_DIR`) for the build cvar_poke checks, so no game's
/// layout is in this repo. Both sides answer the object's offset and the whole image after the action.
#[test]
fn cvar_find_read_and_write() {
    let (Some(harness), Some(kb)) = (
        std::env::var_os("QUEN_HARNESS_DIR"),
        std::env::var_os("QUEN_KB_DIR"),
    ) else {
        println!("parity cvar: skipped, QUEN_HARNESS_DIR or QUEN_KB_DIR is not set");
        return;
    };
    let scratch = Scratch::new("cvar-build");
    let dir = scratch.0.join("0").to_string_lossy().into_owned();
    let Some(build) = driver(&harness, &[json!({"kind": "cvar_build", "dir": dir})]) else {
        return;
    };
    let build = build[0]["ok"]
        .as_str()
        .expect("the build cvar_poke checks")
        .to_string();
    let tsv = std::fs::read_to_string(Path::new(&kb).join("cvar_layout.tsv"))
        .expect("the KB's cvar_layout.tsv");
    let l = cvar::from_layout_tsv(&tsv)
        .unwrap()
        .into_iter()
        .find(|l| l.build == build)
        .unwrap_or_else(|| panic!("cvar_layout.tsv has no row for build {build}"));
    let (b, i, f) = (Some(0), Some(1), Some(2));
    let vars = [
        ("Rendering/RT", "Tlas", f, 800.0, 800.0),
        ("Rendering/RT", "Budget", i, 0.0, 0.0),
        ("Rendering", "UseX", b, 1.0, 1.0),
        ("Rendering", "Enabled", b, 1.0, 1.0),
        ("Other", "Enabled", b, 0.0, 0.0),
        ("Other", "Broken", None, 0.0, 0.0),
        ("Other", "Twice", i, 3.0, 3.0),
        ("Other", "Twice", i, 4.0, 4.0),
        ("Other", "Moved", f, 1.5, 2.5),
        ("Other", "BadPtr", i, 0.0, 0.0),
    ];
    let image = hex(&cvar_image(&l, &vars));
    let c = |name, var: &str, action: Option<&str>, value: Option<&str>, known| Case {
        name,
        input: json!({"base": CVAR_BASE, "image": image, "name": var, "action": action, "value": value}),
        known,
    };
    let (set, restore) = (Some("set"), Some("restore"));
    let cases = vec![
        c("read", "Tlas", None, None, None),
        c("read_group_name", "Rendering/RT/Tlas", None, None, None),
        c("set_float", "Tlas", set, Some("400"), None),
        c("set_float_fraction", "Tlas", set, Some("0.1"), None),
        c("set_float_exponent", "Tlas", set, Some("1e3"), None),
        c("set_float_negative", "Tlas", set, Some("-2.5"), None),
        c("set_float_too_big", "Tlas", set, Some("1e39"), None),
        c(
            "set_float_inf",
            "Tlas",
            set,
            Some("inf"),
            Some("Python writes inf, quen takes finite floats only"),
        ),
        c(
            "set_float_underscore",
            "Tlas",
            set,
            Some("1_0"),
            Some("Python's float() takes 1_0, quen doesn't"),
        ),
        c("restore", "Other/Moved", restore, None, None),
        c("restore_at_default", "Tlas", restore, None, None),
        c("set_int", "Rendering/RT/Budget", set, Some("256"), None),
        c("set_int_hex", "Budget", set, Some("0x10"), None),
        c("set_int_negative", "Budget", set, Some("-7"), None),
        c("set_int_leading_zero", "Budget", set, Some("010"), None),
        c("set_int_fraction", "Budget", set, Some("1.5"), None),
        c("set_int_too_big", "Budget", set, Some("2147483648"), None),
        c(
            "set_int_underscore",
            "Budget",
            set,
            Some("1_000"),
            Some("Python's int() takes 1_000, quen doesn't"),
        ),
        c("set_bool_false", "UseX", set, Some("false"), None),
        c("set_bool_true_upper", "UseX", set, Some("TRUE"), None),
        c("set_bool_two", "UseX", set, Some("2"), None),
        c("group_picks_one", "Other/Enabled", set, Some("1"), None),
        c("two_by_name", "Enabled", None, None, None),
        c("same_group_and_name_twice", "Other/Twice", None, None, None),
        c("unknown_type_code", "Broken", None, None, None),
        c("no_such_var", "NoSuchVar", None, None, None),
        c("wrong_group", "Rendering/RT/UseX", None, None, None),
        c("value_pointer_not_self", "BadPtr", None, None, None),
    ];
    parity(
        "cvar",
        cases,
        |input| {
            let hex_in = text(input, "image");
            let bytes = (0..hex_in.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex_in[i..i + 2], 16).unwrap());
            let mem = Flat(bytes.collect());
            let name = text(input, "name");
            let image = mem::image(&mem, CVAR_BASE).unwrap();
            let obj = match cvar::one(&cvar::find(&mem, &image, &l, &name), &name) {
                Ok(obj) => obj,
                Err(e) => return refused(e),
            };
            let var = cvar::read(&mem, &l, obj).unwrap();
            let set = match input["action"].as_str() {
                Some("set") => Some(Set::Value(input["value"].as_str().unwrap())),
                Some(_) => Some(Set::Default),
                None => None,
            };
            let mut after = mem.0.clone();
            if let Some(set) = set {
                let w = match cvar::plan(&var, &l, set) {
                    Ok(w) => w,
                    Err(e) => return refused(e),
                };
                let at = (w.address - CVAR_BASE) as usize;
                after[at..at + w.new.len()].copy_from_slice(&w.new);
            }
            json!({"ok": {"obj": obj - CVAR_BASE, "image": hex(&after)}})
        },
        |p, r| p == r,
    );
}

/// quen's PresentMon command line in perf's form: its two deliberate differences mapped back (the pid for perf's image
/// name, and quen's own trace session), so everything else must be perf's, in perf's order.
fn perf_form(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--process_id" => {
                args.next();
                out.extend(["--process_name".into(), "witcher3.exe".into()]);
            }
            "--session_name" => drop(args.next()),
            "--stop_existing_session" => {}
            _ => out.push(arg.clone()),
        }
    }
    out
}

#[test]
fn presentmon_command_line() {
    let c = |name, flags: &str, secs: u32, known| Case {
        name,
        input: json!({"flags": flags, "tag": "parity", "secs": secs}),
        known,
    };
    let any = "perf passes any W3_PM_FLAGS on, quen only --track_* / --no_track_* switches";
    let cases = vec![
        c("no_flags", "", 10, None),
        c("one_second", "", 1, None),
        c("pc_latency", "--track_pc_latency", 15, None),
        c(
            "two_switches_spaced",
            "  --track_pc_latency   --track_frame_type ",
            600,
            None,
        ),
        c("no_track_switch", "--no_track_input", 15, None),
        c("another_target", "--process_name other.exe", 10, Some(any)),
        c("no_csv", "--output_stdout", 10, Some(any)),
    ];
    parity(
        "presentmon_argv",
        cases,
        |input| {
            let csv =
                Path::new(&text(input, "dir")).join(format!("perf_{}.csv", text(input, "tag")));
            let flags: Vec<String> = text(input, "flags")
                .split_whitespace()
                .map(String::from)
                .collect();
            let secs = input["secs"].as_u64().unwrap() as u32;
            match stats::presentmon_args("pm.exe", 4242, secs, &csv.to_string_lossy(), &flags) {
                Ok(argv) => json!({"ok": perf_form(&argv[1..])}),
                Err(e) => refused(e),
            }
        },
        |p, r| p == r,
    );
}

// ---- w3ctl key plans, console_type lines and its prompt check --------------------------------------------------------

/// The Rust plan's lines, as the driver's `keys` prints them.
fn plan_lines(input: &Value) -> Value {
    let tokens: Vec<String> = serde_json::from_value(input["tokens"].clone()).unwrap();
    let (hold, gap) = (
        input["hold"].as_u64().unwrap(),
        input["gap"].as_u64().unwrap(),
    );
    match input::plan(&tokens, hold, gap, input::GRAVE) {
        Ok(plan) => json!({"ok": plan.iter().map(input::describe).collect::<Vec<_>>()}),
        Err(e) => refused(e),
    }
}

#[test]
fn key_plans() {
    let (hold, gap) = (input::HOLD_MS, input::GAP_MS);
    let c = |name, tokens: &[&str], known| Case {
        name,
        input: json!({"tokens": tokens, "hold": hold, "gap": gap}),
        known,
    };
    let letters: Vec<String> = ('a'..='z').map(String::from).collect();
    let letters: Vec<&str> = letters.iter().map(String::as_str).collect();
    let named = "escape esc tab backspace enter space lshift rshift lctrl lalt rctrl ralt up down left right insert \
                 delete home end pgup pageup pgdn pagedown ctrl shift alt period minus comma";
    let named: Vec<&str> = named.split(' ').collect();
    let cap = "Rust caps a hold or wait at 60 s: a plan blocks the session thread";
    let os = "quen refuses the Windows keys and chords that act on Windows, not the game (security review)";
    let mut cases = vec![
        c("one_key", &["e"], None),
        c(
            "doc_example",
            &["u", "wait:1500", "e", "down", "right", "right"],
            None,
        ),
        c("hold_override", &["escape:300"], None),
        c("chord", &["ctrl+shift+e:150"], None),
        c("console_keys", &["grave", "tilde", "console"], None),
        c("upper_case", &["E", "WAIT:10", "LCTRL+A"], None),
        c("empty_duration", &["e:"], None),
        c("zero", &["e:0", "wait:0"], None),
        c("leading_zeros", &["e:0050"], None),
        c(
            "waits_then_keys",
            &["wait:100", "wait:5", "a", "wait:5", "b"],
            None,
        ),
        c("f_keys", &["f1", "f9", "f10", "f11", "f12"], None),
        c(
            "digits",
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
            None,
        ),
        c("letters", &letters, None),
        c("named", &named, None),
        c("repeat_in_chord", &["a+a"], None),
        c("at_the_cap", &["wait:60000", "e:60000"], None),
        c("wait_bare", &["wait"], None),
        c("wait_empty", &["wait:"], None),
        c("wait_word", &["wait:x"], None),
        c("negative", &["e:-1"], None),
        c("plus_sign", &["e:+5"], None),
        c("two_colons", &["e:1:2"], None),
        c("decimal", &["e:1.5"], None),
        c("space_in_ms", &["e: 5"], None),
        c("empty_chord_part", &["a++b"], None),
        c("plus_only", &["+"], None),
        c("empty_token", &[""], None),
        c("unknown", &["nokey"], None),
        c("wait_in_chord", &["wait+a"], None),
        c("f13", &["f13"], None),
        c("f01", &["f01"], None),
        c("bad_after_good", &["e", "nokey"], None),
        c("superscript_digit", &["e:\u{b2}"], None),
        c(
            "arabic_indic_digit",
            &["e:\u{663}"],
            Some(
                "Python's isdigit and int take any Unicode digit, Rust only ASCII (game_keys passes ASCII only)",
            ),
        ),
        c("over_the_cap", &["wait:60001"], Some(cap)),
        c("beyond_u64", &["wait:99999999999999999999"], Some(cap)),
        c("windows_key", &["lwin"], Some(os)),
        c("windows_chord", &["win+r"], Some(os)),
        c("alt_tab", &["alt+tab"], Some(os)),
        c("alt_f4", &["lalt+f4"], Some(os)),
        c("ctrl_shift_esc", &["ctrl+shift+escape"], Some(os)),
        c("ctrl_alt_any", &["ctrl+alt+e"], Some(os)),
    ];
    cases.push(Case {
        name: "own_hold_and_gap",
        input: json!({"tokens": ["a", "b:100", "c"], "hold": 250, "gap": 500}),
        known: None,
    });
    parity("keys", cases, plan_lines, |p, r| p == r);
}

/// A plan's keys and sleeps without names (console_type's taps are all named "k").
fn bare(plan: &[input::Event]) -> Vec<String> {
    plan.iter()
        .map(|e| match e {
            input::Event::Key { sc, up, .. } => {
                format!("key {} 0x{sc:03X}", if *up { "up" } else { "down" })
            }
            input::Event::Sleep { ms, .. } => format!("sleep {ms}"),
        })
        .collect()
}

#[test]
fn console_lines() {
    let c = |name, line: &str, known| Case {
        name,
        input: json!({ "line": line }),
        known,
    };
    let cases = vec![
        c("settime", "settime(1,22)", None),
        c("quenload", "quenload(QuickSave_01)", None),
        c("weather_name", "changeweather(WT_Clear)", None),
        c("punctuation", "a b,c.d-e_f", None),
        c("digits", "0123456789", None),
        c("capitals", "ABC xyz", None),
        c("empty", "", None),
        c("quote", "say('hi')", None),
        c("accent", "\u{e9}", None),
        c("accented_capital", "\u{c9}", None),
        c("semicolon", "a;b", None),
        c("tab", "x\ty", None),
        c(
            "kelvin_sign",
            "\u{212a}",
            Some(
                "Python lowercases the Kelvin sign to k and types shift+k; Rust refuses non-ASCII (so does game_console)",
            ),
        ),
    ];
    // The same keys in the same order; the pace is quen's own since live boot 1 (a line's text back to back, not
    // console_type's 60 ms taps), so sleeps aren't compared.
    let keys = |plan: &Value| {
        let lines = plan.as_array().into_iter().flatten();
        let keys = lines.filter(|l| !l.as_str().is_some_and(|l| l.starts_with("sleep")));
        keys.cloned().collect::<Vec<Value>>()
    };
    parity(
        "console",
        cases,
        |i| match input::console_line(&text(i, "line"), input::GRAVE, &input::us_scancode, true) {
            Ok(plan) => json!({ "ok": bare(&plan) }),
            Err(e) => refused(e),
        },
        |p, r| keys(p) == keys(r),
    );
}

#[test]
fn console_prompt() {
    let c = |name, w: u32, h: u32, prompt: bool, fg: Value, bg: u8| Case {
        name,
        input: json!({"w": w, "h": h, "prompt": prompt, "fg": fg, "bg": bg}),
        known: None,
    };
    let lit = || json!(220);
    let mut cases = vec![
        c("uhd_open", 3840, 2160, true, lit(), 12),
        c("uhd_closed", 3840, 2160, false, lit(), 12),
        Case {
            known: Some(
                "the glyph's rounded 1.5x edge lights a dark point console_type.py reads beside it (81 against the \
                 glyph's 151); quen reads the glyph's outline (12) since live boot 3, so it sees the prompt",
            ),
            ..c("fhd_glyph_too_thin", 1920, 1080, true, lit(), 12)
        },
        c("fhd_closed", 1920, 1080, false, lit(), 12),
        c("qhd_open", 2560, 1440, true, lit(), 12),
        c("hd_open", 1280, 720, true, lit(), 12),
        c("odd_size_glyph_too_thin", 1366, 768, true, json!(255), 0),
        c("ultrawide_open", 3440, 1440, true, lit(), 12), // 2.6875 x 2
        c("at_the_threshold", 3840, 2160, true, json!(100), 0),
        c("just_over", 3840, 2160, true, json!(101), 0),
        c("white_sky", 3840, 2160, true, json!(255), 250),
        c(
            "one_dark_channel",
            3840,
            2160,
            true,
            json!([40, 220, 220]),
            0,
        ),
    ];
    cases.push(Case {
        name: "noise",
        input: json!({"w": 1920, "h": 1080, "prompt": true, "fg": 255, "noise": 7}),
        known: None,
    });
    parity(
        "prompt",
        cases,
        |i| {
            let frame = std::fs::read(Path::new(&text(i, "dir")).join("frame.bgra")).unwrap();
            let (w, h) = (i["w"].as_u64().unwrap(), i["h"].as_u64().unwrap());
            json!({"ok": input::console_open(&frame, w as usize, h as usize)})
        },
        |p, r| p == r,
    );
}

// ---- the boot's file changes and the end's restore ---------------------------------------------------------------

/// The real file system for the Rust side of the boot cases. A file's id is its creation time (a rename keeps it), a
/// folder's sha covers its files' names and bytes, and recycling moves a file into the case's own trash folder.
struct Disk {
    trash: PathBuf,
}

impl Fs for Disk {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".quen-tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.exists(to) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "exists"));
        }
        std::fs::rename(from, to)
    }
    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }
    fn sha256(&self, path: &Path) -> io::Result<String> {
        if !path.is_dir() {
            return Ok(sha256_hex(&std::fs::read(path)?));
        }
        let files = tree(path, None);
        Ok(sha256_hex(serde_json::to_string(&files)?.as_bytes()))
    }
    fn file_id(&self, path: &Path) -> io::Result<String> {
        Ok(format!("{:?}", std::fs::metadata(path)?.created()?))
    }
    fn fsync(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
    fn recycle(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(&self.trash)?;
        let n = std::fs::read_dir(&self.trash)?.count();
        std::fs::rename(path, self.trash.join(n.to_string()))
    }
    fn mtime(&self, path: &Path) -> io::Result<u64> {
        let since = std::fs::metadata(path)?
            .modified()?
            .duration_since(UNIX_EPOCH);
        Ok(since.map_or(0, |d| d.as_nanos() as u64))
    }
    fn set_mtime(&self, path: &Path, nanos: u64) -> io::Result<()> {
        let t = UNIX_EPOCH + std::time::Duration::from_nanos(nanos);
        std::fs::File::options()
            .write(true)
            .open(path)?
            .set_modified(t)
    }
}

/// Every folder (a name ending in `/`) and file (its bytes as Latin-1) under `root` but `skip`, as the driver's `tree`.
fn tree(root: &Path, skip: Option<&Path>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_owned()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if Some(path.as_path()) == skip {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.insert(format!("{rel}/"), Value::Null);
                todo.push(path);
            } else {
                out.insert(rel, json!(latin1(&std::fs::read(&path).unwrap())));
            }
        }
    }
    out
}

/// A synthetic game and documents folder like the user's: ReShade with RenoDX, the state bridge, three mods (one of
/// them the bridge's), two loose ASIs, steam_appid.txt, CRLF settings and a ReShade.ini with a BOM.
fn boot_files() -> BTreeMap<&'static str, String> {
    let settings = "[Rendering]\r\nDLSSGMode=2\r\nVSync=true\r\nMotionBlur=true\r\n[Rendering/DRS]\r\nAllowed=1\r\n\
                    [Gameplay]\r\nCameraAutoRotX=true\r\nCameraAutoRotY=true\r\n[Version]\r\nConfigVersion=7\r\n";
    let reshade = "\u{feff}[ADDON]\r\nLoadFromDllMain=\r\n\r\n[GENERAL]\r\nPresetPath=.\\ReShadePreset.ini\r\n\r\n\
                   [renodx-thewitcher3remastered]\r\nSettingsMode=1\r\n\r\n[renodx-thewitcher3remastered-preset1]\r\n\
                   ToneMapGameNits=203\r\nToneMapPeakNits=1000\r\n";
    BTreeMap::from([
        ("docs/dx12user.settings", settings.to_owned()),
        ("docs/gamesaves/QuickSave.sav", "quick".to_owned()),
        (
            "game/bin/config/base/general.ini",
            "[General]\r\nGameLanguage=en\r\n".to_owned(),
        ),
        ("game/bin/x64_dx12/witcher3.exe", "MZ stand-in".to_owned()),
        ("game/bin/x64_dx12/steam_appid.txt", "292030".to_owned()),
        ("game/bin/x64_dx12/ReShade.ini", latin1(reshade.as_bytes())),
        (
            "game/bin/x64_dx12/ReShadePreset.ini",
            "Techniques=SyntheticHdr\r\nX=1\r\n".to_owned(),
        ),
        ("game/bin/x64_dx12/dxgi.dll", "reshade".to_owned()),
        (
            "game/bin/x64_dx12/renodx-thewitcher3remastered.addon64",
            "renodx".to_owned(),
        ),
        (
            "game/bin/x64_dx12/W3GameStateBridge.asi",
            "bridge".to_owned(),
        ),
        ("game/bin/x64_dx12/Tweaks.asi", "tweaks".to_owned()),
        (
            "game/Mods/modAlpha/content/blob0.bundle",
            "alpha".to_owned(),
        ),
        (
            "game/Mods/modBeta/content/scripts/local/beta.ws",
            "beta".to_owned(),
        ),
        (
            "game/Mods/modW3GameStateBridge/content/scripts/local/w3gsb.ws",
            "w3gsb".to_owned(),
        ),
        (
            "game/content/content0/x64.final.redscripts",
            "compiled".to_owned(),
        ),
    ])
}

/// quen-core's boot on its own copy of the case's files: plan, apply through a journal, the files at the launch, then
/// a replay of the journal and the parking folders, empty again, removed (as quen does after a replay).
fn boot_rust(input: &Value) -> Value {
    let root = PathBuf::from(format!("{}-rust", text(input, "dir")));
    for (rel, data) in input["files"].as_object().unwrap() {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = data.as_str().unwrap().chars().map(|c| c as u8).collect();
        std::fs::write(path, bytes).unwrap();
    }
    let (game, quen) = (root.join("game"), root.join("quen"));
    let env = |key: &str| input["env"][key].as_str().unwrap_or_default().to_owned();
    let bin = boot::bin_dir(&game);
    let (mods, bin_entries) = (listing(&game.join("Mods")), listing(&bin));
    let preset: Vec<(String, f64)> = input["preset"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kv| {
            let (k, v) = kv.as_str().unwrap().split_once('=').unwrap();
            (k.to_owned(), v.parse().unwrap())
        })
        .collect();
    let preset: Vec<(&str, f64)> = preset.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let never: Vec<&str> = [park::W3_NEVER_PARK, HARNESS_OWN].concat();
    let (flat, sections, park_names, keep) = (
        env("W3_SETTINGS"),
        env("W3_SECTION_SETTINGS"),
        env("W3_PARK_MODS"),
        env("W3_PARK_KEEP"),
    );
    let devkit = if env("W3_NO_DEVKIT") == "1" {
        ""
    } else {
        r"devkit\renodx-devkit.addon64"
    };
    let backups = quen.join("backup");
    let documents = root.join("docs");
    let inputs = Inputs {
        game_dir: &game,
        documents: &documents,
        backups: &backups,
        fg: env("W3_KEEP_FG") == "1",
        vsync: env("W3_VSYNC_OFF") != "1",
        flat: &flat,
        sections: &sections,
        park: &park_names,
        keep: &keep,
        never_park: &never,
        pairs: GSB,
        mods: &mods,
        bin: &bin_entries,
        more: park::More::default(),
        renodx: Some(RenoDx {
            addon: "renodx-thewitcher3remastered.addon64",
            preset: &preset,
        }),
        load_from_dll_main: Some(devkit),
        play: false,
    };
    let fs = Disk {
        trash: quen.join("trash"),
    };
    let changes = match boot::plan(&fs, &inputs) {
        Ok(changes) => changes,
        Err(e) => return refused(e),
    };
    std::fs::create_dir_all(&backups).unwrap();
    for change in &changes {
        if let boot::Change::Move { to, .. } = change {
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        }
    }
    let path = quen.join("journal.jsonl");
    let mut journal = Journal::create(&fs, &path).unwrap();
    boot::apply(&fs, &mut journal, &changes, &|| false).unwrap();
    let at_launch = tree(&root, Some(&quen));
    let report = replay(&std::fs::read(&path).unwrap(), &fs);
    assert!(report.clean() && report.kept.is_empty(), "{report:#?}");
    let parked = game.join(boot::PARKED);
    for dir in [parked.join("Mods"), parked.join("bin"), parked.clone()] {
        let _ = std::fs::remove_dir(dir); // fails unless empty
    }
    let prefix = format!("game/{}/", boot::PARKED);
    let mut names: Vec<&str> = at_launch
        .keys()
        .filter_map(|k| k.strip_prefix(&prefix))
        .filter_map(|rest| rest.strip_prefix("Mods/").or(rest.strip_prefix("bin/")))
        .filter_map(|rest| rest.split('/').next())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let files: BTreeMap<&String, &Value> = at_launch
        .iter()
        .filter(|(k, _)| !k.starts_with(&prefix))
        .collect();
    json!({"ok": {
        "launch": {"files": files, "parked": names},
        "argv": ["game/bin/x64_dx12/witcher3.exe"],
        "after": tree(&root, Some(&quen)),
    }})
}

#[test]
fn boot_files_and_restore() {
    let base = boot_files();
    let with = |changes: &[(&'static str, &str)]| {
        let mut files = base.clone();
        for (rel, data) in changes {
            files.insert(rel, (*data).to_owned());
        }
        files
    };
    let settings = &base["docs/dx12user.settings"];
    let no_fg = settings.replace("DLSSGMode=2\r\n", "");
    let lf = settings.replace("\r\n", "\n");
    let c = |name, files: &BTreeMap<&str, String>, env: Value, preset: &[&str], known| Case {
        name,
        input: json!({"files": files, "env": env, "preset": preset, "save": null}),
        known,
    };
    let console =
        Some("quen turns the console on for every boot (its save load types a console line)");
    let all = json!({
        "W3_PARK_MODS": "ALL", "W3_PARK_KEEP": "modBeta", "W3_VSYNC_OFF": "1", "W3_NO_DEVKIT": "1",
        "W3_SETTINGS": "MotionBlur=false", "W3_SECTION_SETTINGS": "Rendering/DRS:Allowed=0;Gameplay:Hdr=1",
    });
    let general_has_it = with(&[
        ("docs/dx12user.settings", &lf),
        (
            "game/bin/config/base/general.ini",
            "[General]\nDBGConsoleOn=false\nX=1\n",
        ),
    ]);
    let cases = vec![
        c("plain", &base, json!({}), &[], console),
        c(
            "park_two",
            &base,
            json!({"W3_PARK_MODS": "modAlpha,Tweaks.asi"}),
            &[],
            None,
        ),
        c(
            "park_all_overrides_preset",
            &base,
            all,
            &[
                "ToneMapGameNits=250",
                "ToneMapPeakNits=800.5",
                "+DevNew=0.1",
            ],
            None,
        ),
        c(
            "keep_fg",
            &base,
            json!({"W3_KEEP_FG": "1", "W3_PARK_MODS": "modBeta"}),
            &[],
            None,
        ),
        c(
            "lf_settings_console_key",
            &general_has_it,
            json!({"W3_PARK_MODS": "modAlpha"}),
            &[],
            None,
        ),
        c(
            "bad_override",
            &base,
            json!({"W3_SETTINGS": "Missing=1"}),
            &[],
            None,
        ),
        c(
            "no_dlssg_line",
            &with(&[("docs/dx12user.settings", &no_fg)]),
            json!({}),
            &[],
            None,
        ),
        c(
            "settings_of_4_04",
            &with(&[("docs/w3ver.active", "404")]),
            json!({}),
            &[],
            None,
        ),
        c(
            "preset_typo",
            &base,
            json!({"W3_PARK_MODS": "modAlpha"}),
            &["ToneMapGameNitz=1"],
            None,
        ),
        c(
            "section_value_backslash",
            &base,
            json!({"W3_SECTION_SETTINGS": r"Rendering/DRS:Allowed=a\1"}),
            &[],
            Some(
                "the harness's section replace reads its value as a regex template: it refuses, quen writes it",
            ),
        ),
        c(
            "never_park_bridge",
            &base,
            json!({"W3_PARK_MODS": "W3GameStateBridge.asi"}),
            &[],
            None,
        ),
    ];
    parity("boot", cases, boot_rust, |p, r| p == r);
}
