//! "Proven" is derived by the claims_status view (docs/intent.md, "Claims and proof").

mod ledger_common;

use ledger_common::{boot, game, measurement, rows};
use quen_core::ledger::{Claim, Error, Ledger, Tag};
use serde_json::json;

/// (proven, families, standing contradictions) of a claim.
fn status(l: &Ledger, claim: i64) -> (bool, i64, i64) {
    let r = &rows(
        l,
        "SELECT proven, families, contradictions FROM claims_status WHERE id = ?1",
        &[json!(claim)],
    )[0];
    (
        r[0] == json!(1),
        r[1].as_i64().unwrap(),
        r[2].as_i64().unwrap(),
    )
}

fn setup() -> (Ledger, i64, i64) {
    let mut l = Ledger::open_in_memory().unwrap();
    let g = game(&mut l);
    let claim = l
        .add_claim(&Claim {
            text: "cvar X off saves 0.3 ms in scene P1".into(),
            scope: Some("5.01".into()),
        })
        .unwrap();
    (l, g, claim)
}

#[test]
fn two_families_tagged_m_or_o_prove_a_claim() {
    let (mut l, g, claim) = setup();
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    assert_eq!(status(&l, claim), (false, 0, 0));
    for _ in 0..2 {
        let m = measurement(&mut l, Some(b), "presentmon", Tag::M);
        l.support(claim, m, true).unwrap();
    }
    assert_eq!(
        status(&l, claim),
        (false, 1, 0),
        "one family twice is one method"
    );
    let m = measurement(&mut l, Some(b), "tracy", Tag::O);
    l.support(claim, m, true).unwrap();
    assert_eq!(
        status(&l, claim),
        (true, 2, 0),
        "two families from the same run count"
    );
}

#[test]
fn inferred_and_unverified_support_does_not_count() {
    let (mut l, g, claim) = setup();
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    for (family, tag) in [("presentmon", Tag::I), ("tracy", Tag::U)] {
        let m = measurement(&mut l, Some(b), family, tag);
        l.support(claim, m, true).unwrap();
    }
    assert_eq!(status(&l, claim), (false, 0, 0));
    let m = measurement(&mut l, Some(b), "nsight", Tag::M);
    l.support(claim, m, true).unwrap();
    assert_eq!(status(&l, claim), (false, 1, 0));
}

#[test]
fn static_re_facts_count_without_a_boot() {
    let (mut l, _, claim) = setup();
    for family in ["rea", "disasm"] {
        let m = measurement(&mut l, None, family, Tag::StaticRe);
        l.support(claim, m, true).unwrap();
    }
    assert_eq!(status(&l, claim), (true, 2, 0));
}

#[test]
fn a_contradiction_from_a_later_boot_demotes_until_newer_support() {
    let (mut l, g, claim) = setup();
    let b1 = boot(&mut l, g, "2026-10-10T10:00:00Z");
    for family in ["presentmon", "tracy"] {
        let m = measurement(&mut l, Some(b1), family, Tag::M);
        l.support(claim, m, true).unwrap();
    }
    assert!(status(&l, claim).0);

    let b2 = boot(&mut l, g, "2026-10-12T10:00:00Z");
    let m = measurement(&mut l, Some(b2), "presentmon", Tag::M);
    l.support(claim, m, false).unwrap();
    assert_eq!(status(&l, claim), (false, 2, 1), "demoted");

    let b3 = boot(&mut l, g, "2026-10-14T10:00:00Z");
    let m = measurement(&mut l, Some(b3), "presentmon", Tag::M);
    l.support(claim, m, true).unwrap();
    assert_eq!(
        status(&l, claim),
        (false, 2, 1),
        "tracy's only support predates the contradiction: one family again is one method"
    );
    let m = measurement(&mut l, Some(b3), "tracy", Tag::M);
    l.support(claim, m, true).unwrap();
    assert_eq!(
        status(&l, claim),
        (true, 2, 0),
        "two families' newer support outranks the contradiction"
    );
}

#[test]
fn each_contradiction_needs_two_families_after_it() {
    // b1: both families support. b2: both contradict. b3: only presentmon supports again.
    let (mut l, g, claim) = setup();
    let b1 = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let b2 = boot(&mut l, g, "2026-10-12T10:00:00Z");
    let b3 = boot(&mut l, g, "2026-10-14T10:00:00Z");
    for (b, family, supports) in [
        (b1, "presentmon", true),
        (b1, "tracy", true),
        (b2, "presentmon", false),
        (b2, "tracy", false),
        (b3, "presentmon", true),
    ] {
        let m = measurement(&mut l, Some(b), family, Tag::M);
        l.support(claim, m, supports).unwrap();
    }
    assert_eq!(status(&l, claim), (false, 2, 2));
    let b4 = boot(&mut l, g, "2026-10-16T10:00:00Z");
    let m = measurement(&mut l, Some(b4), "tracy", Tag::M);
    l.support(claim, m, true).unwrap();
    assert_eq!(
        status(&l, claim),
        (true, 2, 0),
        "presentmon (b3) and tracy (b4) both after b2"
    );
}

#[test]
fn boot_order_is_by_start_time_not_insertion() {
    let (mut l, g, claim) = setup();
    // The later boot is recorded first, as a migration of old logs may do.
    let late = boot(&mut l, g, "2026-10-14T10:00:00Z");
    let early = boot(&mut l, g, "2026-10-01T10:00:00Z");
    for family in ["presentmon", "tracy"] {
        let m = measurement(&mut l, Some(late), family, Tag::M);
        l.support(claim, m, true).unwrap();
    }
    let m = measurement(&mut l, Some(early), "presentmon", Tag::M);
    l.support(claim, m, false).unwrap();
    assert_eq!(
        status(&l, claim),
        (true, 2, 0),
        "an older contradiction doesn't demote"
    );
}

#[test]
fn contradictions_that_cant_be_shown_older_stand() {
    // From the same boot as the newest support, with no boot at all, or with a weak tag: each demotes on its own.
    for case in ["same boot", "no boot", "inferred"] {
        let (mut l, g, claim) = setup();
        let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
        for family in ["presentmon", "tracy"] {
            let m = measurement(&mut l, Some(b), family, Tag::M);
            l.support(claim, m, true).unwrap();
        }
        let m = match case {
            "same boot" => measurement(&mut l, Some(b), "presentmon", Tag::M),
            "no boot" => measurement(&mut l, None, "rea", Tag::StaticRe),
            _ => {
                let later = boot(&mut l, g, "2026-10-11T10:00:00Z");
                measurement(&mut l, Some(later), "presentmon", Tag::I)
            }
        };
        l.support(claim, m, false).unwrap();
        assert_eq!(status(&l, claim), (false, 2, 1), "{case}");
    }
}

#[test]
fn a_withdrawn_claim_is_never_proven_and_keeps_its_reason() {
    let (mut l, g, claim) = setup();
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    for family in ["presentmon", "tracy"] {
        let m = measurement(&mut l, Some(b), family, Tag::M);
        l.support(claim, m, true).unwrap();
    }
    assert!(status(&l, claim).0);

    assert!(
        matches!(l.withdraw(claim, "  "), Err(Error::Sqlite(_))),
        "a reason is required"
    );
    l.withdraw(claim, "scene P1 had rain in boot 1").unwrap();
    assert_eq!(status(&l, claim), (false, 2, 0));
    let r = &rows(
        &l,
        "SELECT status, withdrawn_reason, text FROM claims_status WHERE id = ?1",
        &[json!(claim)],
    )[0];
    assert_eq!(
        r,
        &vec![
            json!("withdrawn"),
            json!("scene P1 had rain in boot 1"),
            json!("cvar X off saves 0.3 ms in scene P1")
        ]
    );

    assert!(
        matches!(l.withdraw(claim, "again"), Err(Error::Refused(why)) if why.contains("already withdrawn"))
    );
    assert!(matches!(l.withdraw(999, "nope"), Err(Error::Refused(why)) if why == "no claim 999"));
    let r = &rows(
        &l,
        "SELECT withdrawn_reason FROM claims WHERE id = ?1",
        &[json!(claim)],
    )[0];
    assert_eq!(
        r[0],
        json!("scene P1 had rain in boot 1"),
        "a refused withdrawal changes nothing"
    );
}

#[test]
fn support_rows_need_real_claims_and_measurements() {
    let (mut l, g, claim) = setup();
    let b = boot(&mut l, g, "2026-10-10T10:00:00Z");
    let m = measurement(&mut l, Some(b), "presentmon", Tag::M);
    // Feature tests 1: named, not SQLite's "FOREIGN KEY constraint failed".
    assert!(
        matches!(l.support(claim, 999, true), Err(Error::Refused(why)) if why == "no measurement 999")
    );
    assert!(matches!(l.support(999, m, true), Err(Error::Refused(why)) if why == "no claim 999"));
    l.support(claim, m, true).unwrap();
    assert!(
        l.support(claim, m, false).is_err(),
        "one verdict per measurement"
    );
}
