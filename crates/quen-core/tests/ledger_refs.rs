//! The doc-reference scanner and the cleanup candidates (docs/intent.md, "Captures"), on synthetic docs and paths.

mod ledger_common;

use ledger_common::{boot, capture, game, rows};
use quen_core::ledger::{Error, Ledger, Ref, ScanSummary, Scanner};
use serde_json::json;

const ROOT: &str = r"D:\fixture\renodx\tmp\witcher3\cap";

const KNOWN: &[&str] = &[
    "ini/perf_A.csv",          // 0
    "ini/perf_B.csv",          // 1
    "ngfx/TAG/frame1.png",     // 2
    "ngfx/TAG/frame2.png",     // 3
    "ngfx/TAG/sub/trace.json", // 4
    "ngfx/OTHER/y.png",        // 5
    "glow_front.png",          // 6
    "glow_side.png",           // 7
];

/// What each reference in `text` matches, as (raw, known indices).
fn found(text: &str) -> Vec<(String, Vec<usize>)> {
    Scanner::new(ROOT, KNOWN)
        .unwrap()
        .refs(text)
        .into_iter()
        .map(|r| (r.raw, r.matches))
        .collect()
}

fn one(text: &str) -> Vec<usize> {
    let f = found(text);
    assert_eq!(f.len(), 1, "{text:?} gave {f:?}");
    f.into_iter().next().unwrap().1
}

#[test]
fn every_spelling_of_the_capture_root_is_found() {
    for text in [
        "cap/glow_front.png",
        r"cap\glow_front.png",
        r"D:\fixture\renodx\tmp\witcher3\cap\glow_front.png",
        "d:/fixture/renodx/tmp/witcher3/cap/glow_front.png",
        "/d/fixture/renodx/tmp/witcher3/cap/glow_front.png",
        r"tmp\witcher3\cap\glow_front.png",
        r"%USERPROFILE%\renodx\tmp\witcher3\cap\glow_front.png",
        r"$env:USERPROFILE\renodx\tmp\witcher3\cap\glow_front.png",
        r#"Copy-Item "$ENV:UserProfile\renodx\tmp\witcher3\cap\glow_front.png" ."#,
        "~/renodx/tmp/witcher3/cap/glow_front.png",
        r"..\witcher3\cap\\glow_front.png",
        "./cap/GLOW_FRONT.PNG",
        "CAP/Glow_Front.png",
    ] {
        assert_eq!(one(text), [6], "{text}");
    }
}

#[test]
fn other_paths_are_not_references() {
    for text in [
        "cap/ is where captures go",
        r"cap\",
        "mycap/glow_front.png",
        "capture/glow_front.png",
        "src/games/foo/cap/glow_front.png",
        r"E:\elsewhere\cap\glow_front.png",
        r"D:\fixture\renodx\cap\glow_front.png",
        "the cap was 30 fps",
    ] {
        assert!(found(text).is_empty(), "{text}");
    }
}

#[test]
fn references_are_cut_out_of_prose_markdown_code_and_json() {
    let text = "Shots: `cap/glow_front.png`, [side](cap/glow_side.png) and **cap/ini/perf_A.csv**.\n\
                See cap/ini/perf_B.csv:12 and \"cap\\\\ngfx\\\\OTHER\\\\y.png\", then cap/glow_front.png, cap/glow_side.png.\n\
                OUT=cap/ngfx/TAG/frame1.png; echo 'cap/ngfx/TAG/frame2.png'";
    let refs = Scanner::new(ROOT, KNOWN).unwrap().refs(text);
    let got: Vec<(usize, &str, &[usize])> = refs
        .iter()
        .map(|r| (r.line, r.raw.as_str(), r.matches.as_slice()))
        .collect();
    assert_eq!(
        got,
        [
            (1, "cap/glow_front.png", &[6][..]),
            (1, "cap/glow_side.png", &[7]),
            (1, "cap/ini/perf_A.csv", &[0]),
            (2, "cap/ini/perf_B.csv", &[1]),
            (2, r"cap\\ngfx\\OTHER\\y.png", &[5]),
            (2, "cap/glow_front.png", &[6]),
            (2, "cap/glow_side.png", &[7]),
            (3, "cap/ngfx/TAG/frame1.png", &[2]),
            (3, "cap/ngfx/TAG/frame2.png", &[3]),
        ]
    );
}

#[test]
fn a_reference_twice_on_one_line_counts_once() {
    let refs = Scanner::new(ROOT, KNOWN)
        .unwrap()
        .refs("cap/glow_side.png vs cap/glow_side.png\ncap/glow_side.png");
    assert_eq!(
        refs,
        [
            Ref {
                line: 1,
                raw: "cap/glow_side.png".into(),
                matches: vec![7]
            },
            Ref {
                line: 2,
                raw: "cap/glow_side.png".into(),
                matches: vec![7]
            },
        ]
    );
}

#[test]
fn folder_prefixes_match_every_file_under_them() {
    for text in [
        "cap/ngfx/TAG/",
        r"cap\ngfx\TAG\",
        "cap/ngfx/TAG",
        r"D:\fixture\renodx\tmp\witcher3\cap\ngfx\tag",
    ] {
        assert_eq!(one(text), [2, 3, 4], "{text}");
    }
    assert_eq!(one("cap/ngfx/"), [2, 3, 4, 5]);
    assert!(
        one("cap/ngfx/TA").is_empty(),
        "a folder prefix ends at a separator"
    );
    assert!(
        one("cap/glow_front.png/").is_empty(),
        "a trailing separator names a folder"
    );
}

#[test]
fn globs_and_braces_expand_against_the_known_paths() {
    assert_eq!(one("cap/*.png"), [6, 7], "* doesn't cross a separator");
    assert_eq!(one("cap/glow_*.png"), [6, 7]);
    assert_eq!(one("cap/ngfx/*/frame*.png"), [2, 3]);
    assert_eq!(one("cap/ngfx/*/"), [2, 3, 4, 5]);
    assert_eq!(one("cap/ngfx/T*"), [2, 3, 4], "a glob may name folders");
    assert_eq!(one("cap/ini/perf_{A,B}.csv"), [0, 1]);
    assert_eq!(
        one("cap/{glow_{front,side}.png,ini/perf_A.csv}"),
        [0, 6, 7],
        "nested"
    );
    assert_eq!(one("cap/{ngfx/OTHER,ini}/*"), [0, 1, 5]);
    assert_eq!(
        one("cap/glow_{front,missing}.png"),
        [6],
        "one alternative is enough"
    );
    assert!(
        one("cap/glow_{front}.png").is_empty(),
        "a group without a comma is literal"
    );
    assert!(
        one("cap/ini/perf_{A,B.csv").is_empty(),
        "an unclosed group is literal"
    );
    let start = std::time::Instant::now();
    assert!(one(&format!("cap/{}z", "*g".repeat(40))).is_empty());
    assert_eq!(one(&format!("cap/{}*.png", "*".repeat(40))), [6, 7]);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "many stars stay cheap"
    );
}

#[test]
fn a_reference_matching_nothing_is_dangling() {
    assert_eq!(
        found("cap/gone.png and cap/ngfx/NOPE/ and cap/*.exr"),
        [
            ("cap/gone.png".to_string(), vec![]),
            ("cap/ngfx/NOPE/".to_string(), vec![]),
            ("cap/*.exr".to_string(), vec![]),
        ]
    );
}

#[test]
fn a_root_without_a_folder_name_is_refused() {
    for root in ["", "/", r"D:\", "D:"] {
        assert!(
            matches!(Scanner::new(root, KNOWN), Err(Error::Refused(_))),
            "{root:?}"
        );
    }
    assert_eq!(
        Scanner::new("cap", KNOWN)
            .unwrap()
            .refs("cap/glow_side.png")[0]
            .matches,
        [7],
        "a relative root works"
    );
    let unicode = Scanner::new(r"D:\fixture\Über", &["x.png"]).unwrap();
    let refs = unicode.refs("xÜber/y.png then Über/x.png");
    assert_eq!(
        refs.iter()
            .map(|r| (r.raw.as_str(), r.matches.as_slice()))
            .collect::<Vec<_>>(),
        [("Über/x.png", &[0][..])]
    );
}

const NOW: &str = "2026-10-17T12:00:00Z";
const OLD: &str = "2026-09-01T00:00:00Z";

struct Fixture {
    l: Ledger,
    ids: Vec<(&'static str, i64)>,
}

impl Fixture {
    fn id(&self, path: &str) -> i64 {
        self.ids.iter().find(|(p, _)| *p == path).unwrap().1
    }

    fn candidates(&self) -> Vec<String> {
        self.l
            .cleanup_candidates(NOW, 30, &[r"INI\ORIG\", "tpl_menu_"])
            .unwrap()
            .into_iter()
            .map(|c| c.path)
            .collect()
    }
}

/// One capture per cleanup rule, plus two plain old ones from a reviewed boot.
fn fixture() -> Fixture {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let reviewed = boot(&mut l, g, "2026-08-30T10:00:00Z");
    l.review(reviewed, "2026-09-02T09:00:00Z").unwrap();
    let unreviewed = boot(&mut l, g, "2026-08-31T10:00:00Z");
    let mut ids = Vec::new();
    for (path, boot, t) in [
        ("a.png", Some(reviewed), OLD),
        ("b.png", Some(reviewed), OLD),
        ("ini/orig/general.ini", Some(reviewed), OLD),
        ("tpl_menu_load.png", Some(reviewed), OLD),
        ("recent.png", Some(reviewed), "2026-10-10T00:00:00Z"),
        ("thirty_days.png", Some(reviewed), "2026-09-17T12:00:00Z"),
        ("unreviewed.png", Some(unreviewed), OLD),
        ("no_boot.png", None, OLD),
    ] {
        ids.push((path, capture(&mut l, boot, path, t)));
    }
    let pinned = l
        .add_capture(&quen_core::ledger::Capture {
            boot_id: Some(reviewed),
            path: "pinned.png".into(),
            sha256: ledger_common::sha(4),
            kind: "png".into(),
            t: OLD.into(),
            pinned: true,
            ..Default::default()
        })
        .unwrap();
    ids.push(("pinned.png", pinned));
    Fixture { l, ids }
}

#[test]
fn cleanup_lists_only_unreferenced_unpinned_old_reviewed_captures() {
    let f = fixture();
    assert_eq!(f.candidates(), ["a.png", "b.png"]);
    assert!(matches!(
        f.l.cleanup_candidates("yesterday", 30, &[]),
        Err(Error::Refused(_))
    ));
    let all = f.l.cleanup_candidates(NOW, 30, &[]).unwrap();
    let paths: Vec<&str> = all.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "a.png",
            "b.png",
            "ini/orig/general.ini",
            "tpl_menu_load.png"
        ],
        "without hard pins"
    );
    assert_eq!(
        (all[0].capture_id, all[0].bytes, all[0].t.as_str()),
        (f.id("a.png"), 1024, OLD)
    );
}

#[test]
fn a_capture_added_after_the_review_was_not_reviewed() {
    let mut f = fixture();
    let b = rows(
        &f.l,
        "SELECT boot_id FROM captures WHERE path = 'a.png'",
        &[],
    )[0][0]
        .as_i64();
    // The fixture's boot was reviewed at 2026-09-02T09:00:00Z.
    capture(&mut f.l, b, "at_review.png", "2026-09-02T09:00:00Z");
    capture(&mut f.l, b, "after_review.png", "2026-09-02T09:00:01Z");
    assert_eq!(f.candidates(), ["a.png", "b.png", "at_review.png"]);
}

#[test]
fn scanning_records_references_and_a_rescan_replaces_them() {
    let mut f = fixture();
    let docs = [
        (
            "PROGRESS.md",
            "Kept: cap/a.png and cap/gone.png\n\nAll old ones: cap/{b,zzz}.png\n",
        ),
        ("cap/run.sh", "cp out.png $ROOT/cap/x.png\nls cap/*.png"),
    ];
    let summary = f.l.scan_docs(&docs, ROOT, NOW).unwrap();
    assert_eq!(
        summary,
        ScanSummary {
            docs: 2,
            refs: 5,
            dangling: 2
        }
    );
    assert_eq!(
        f.candidates(),
        [] as [&str; 0],
        "a.png and b.png are referenced"
    );

    let progress = rows(
        &f.l,
        "SELECT line, raw, capture_id, dangling, t_scanned FROM doc_refs WHERE doc_path = 'PROGRESS.md' ORDER BY id",
        &[],
    );
    assert_eq!(
        progress,
        [
            vec![
                json!(1),
                json!("cap/a.png"),
                json!(f.id("a.png")),
                json!(0),
                json!(NOW)
            ],
            vec![
                json!(1),
                json!("cap/gone.png"),
                json!(null),
                json!(1),
                json!(NOW)
            ],
            vec![
                json!(3),
                json!("cap/{b,zzz}.png"),
                json!(f.id("b.png")),
                json!(0),
                json!(NOW)
            ],
        ]
    );
    let glob_rows = rows(
        &f.l,
        "SELECT count(*) FROM doc_refs WHERE doc_path = 'cap/run.sh'",
        &[],
    );
    assert_eq!(
        glob_rows,
        [vec![json!(9)]],
        "cap/*.png matches the 8 top-level pngs; $ROOT/cap/x.png is a dangling reference"
    );

    // The edited PROGRESS.md no longer mentions a.png; run.sh still references both through its glob.
    f.l.scan_docs(
        &[("PROGRESS.md", "Only cap/b.png now")],
        ROOT,
        "2026-10-17T13:00:00Z",
    )
    .unwrap();
    let progress = rows(
        &f.l,
        "SELECT raw, dangling FROM doc_refs WHERE doc_path = 'PROGRESS.md'",
        &[],
    );
    assert_eq!(progress, [vec![json!("cap/b.png"), json!(0)]]);
    assert_eq!(f.candidates(), [] as [&str; 0]);
    f.l.scan_docs(&[("cap/run.sh", "")], ROOT, "2026-10-17T13:00:00Z")
        .unwrap();
    assert_eq!(f.candidates(), ["a.png"], "b.png is still in PROGRESS.md");
}

#[test]
fn a_refused_scan_leaves_the_rows_alone() {
    let mut f = fixture();
    f.l.scan_docs(&[("PROGRESS.md", "cap/a.png")], ROOT, NOW)
        .unwrap();
    assert!(matches!(
        f.l.scan_docs(&[("PROGRESS.md", "")], r"D:\", NOW),
        Err(Error::Refused(_))
    ));
    assert!(
        f.l.scan_docs(&[("PROGRESS.md", "")], ROOT, "not a time")
            .is_err()
    );
    let left = rows(&f.l, "SELECT raw FROM doc_refs", &[]);
    assert_eq!(left, [vec![json!("cap/a.png")]]);
}
