//! A game state log's `flags=` values and the "loaded" predicate (docs/intent.md, "Game state"). quen reads its own
//! `W3QuenState.log`, which W3Quen.asi writes from modQuen's state in the W3 Game State Bridge's line form and numbers
//! (`<tick> push=<n> flags=<f>`), so this parser, written for the bridge's log, reads both; quen reads nothing of the
//! bridge's (live boot 3: the two agreed at every load).
//!
//! A log records a push when the state changes (the bridge's also when weather, region, an env id or the game hour
//! does), so a flags value can repeat. Flags 0 means main menu or loading. Bit 2048 (`menu`) is quen's own, from
//! quen-w3 0.1.6; the bridge has none.

pub const VALID: i64 = 1;
pub const INTERIOR: i64 = 2;
pub const CAVE: i64 = 4;
pub const DARK: i64 = 8;
pub const SCENE: i64 = 16;
pub const CUTSCENE: i64 = 32;
pub const COMBAT: i64 = 64;
pub const PHOTO: i64 = 128;
pub const FADE: i64 = 256;
pub const DLGFLAG: i64 = 512;
pub const PHOTOENV: i64 = 1024;
/// A menu is open (modQuen: the GUI manager's `IsAnyMenu()`), the pause menu among them. The game ticks on behind it, so
/// it doesn't rule out plain gameplay.
pub const MENU: i64 = 2048;

/// Every bit and its name, lowest first (the Python's `SIG_BITS`).
pub const BITS: [(&str, i64); 12] = [
    ("valid", VALID),
    ("interior", INTERIOR),
    ("cave", CAVE),
    ("dark", DARK),
    ("scene", SCENE),
    ("cutscene", CUTSCENE),
    ("combat", COMBAT),
    ("photo", PHOTO),
    ("fade", FADE),
    ("dlgflag", DLGFLAG),
    ("photoenv", PHOTOENV),
    ("menu", MENU),
];

/// Bits that rule out plain gameplay.
const NOT_PLAIN: i64 = SCENE | CUTSCENE | FADE | DLGFLAG;

/// Every `flags=<int>` in the log, in order, as the Python's `\bflags=(-?\d+)` finds them: `flags` must not follow a
/// letter, digit or `_`, and digits after the number are ignored. A number that doesn't fit an i64 is skipped.
pub fn flags(log: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for (at, key) in log.match_indices("flags=") {
        if log[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let rest = &log[at + key.len()..];
        let sign = usize::from(rest.starts_with('-'));
        let digits = rest[sign..].bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0
            && let Ok(value) = rest[..sign + digits].parse()
        {
            out.push(value);
        }
    }
    out
}

/// Each push line's tick and flags, in order: the tick is the line's first field, the system tick count in ms when the
/// bridge wrote it (it matched GetTickCount at live boot 1 [M]). A line without both is skipped.
pub fn pushes(log: &str) -> Vec<(u64, i64)> {
    log.lines()
        .filter_map(|line| {
            let tick = line.split_whitespace().next()?.parse().ok()?;
            Some((tick, *flags(line).first()?))
        })
        .collect()
}

/// A load finished after the first `n_before` values: a later value shows a load (valid bit clear, or the fade bit) and
/// the last value is plain gameplay (valid, with no scene, cutscene, fade or dialogue bit).
pub fn loaded_after(all_flags: &[i64], n_before: usize) -> bool {
    let after = all_flags.get(n_before..).unwrap_or_default();
    let Some(&last) = after.last() else {
        return false;
    };
    after.iter().any(|f| (f & VALID) == 0 || (f & FADE) != 0) && plain(last)
}

/// Plain gameplay: valid, with no scene, cutscene, fade or dialogue bit.
pub fn plain(flags: i64) -> bool {
    (flags & VALID) != 0 && (flags & NOT_PLAIN) == 0
}

/// Photo mode is open.
pub fn in_photo(last: i64) -> bool {
    (last & PHOTO) != 0
}

/// The names of the bits set in `flags`, lowest first.
pub fn names(flags: i64) -> Vec<&'static str> {
    BITS.iter()
        .filter(|(_, bit)| (flags & bit) != 0)
        .map(|(name, _)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_flags_value_with_a_word_boundary() {
        let log = "\
[00:00:01] push seq=1 flags=0 weather=clear\r
[00:00:09] push seq=2 flags=1 region=3
[00:00:10] prevflags=7 my_flags=8 2flags=9 \u{e9}flags=10 (flags=17) x.flags=33
[00:00:11] flags=-1 flags= flags=- flags=--2 flags=x flags=flags=65
[00:00:12] flags=129abc flags=99999999999999999999 flags=007";
        assert_eq!(flags(log), vec![0, 1, 17, 33, -1, 65, 129, 7]);
        assert_eq!(flags(""), Vec::<i64>::new());
        assert_eq!(flags("flags=5"), vec![5]);
    }

    #[test]
    fn pushes_carry_their_tick() {
        let log = "queued=1 thread=7\r\n1000 push=1 flags=257 minute=10\r\n1250 push=2 flags=3\npush flags=1\n9 x\n";
        assert_eq!(pushes(log), vec![(1000, 257), (1250, 3)]);
    }

    #[test]
    fn loaded_needs_a_load_then_plain_gameplay() {
        let gameplay = VALID | INTERIOR;
        // A load (0) after the first two values, then plain gameplay.
        assert!(loaded_after(&[gameplay, gameplay, 0, gameplay], 2));
        // The fade bit counts as a load even with the valid bit set.
        assert!(loaded_after(
            &[gameplay, VALID | FADE, VALID | COMBAT | DARK],
            1
        ));
        // A load before n_before doesn't count.
        assert!(!loaded_after(&[0, gameplay, gameplay], 1));
        assert!(loaded_after(&[0, gameplay, gameplay], 0));
        // No load at all.
        assert!(!loaded_after(&[gameplay, gameplay], 0));
        // Loaded, but the last value is not plain gameplay.
        for busy in [SCENE, CUTSCENE, FADE, DLGFLAG] {
            assert!(!loaded_after(&[0, VALID | busy], 0), "{busy}");
        }
        assert!(!loaded_after(&[gameplay, 0], 0));
        // Photo mode and the other env bits don't block gameplay.
        assert!(loaded_after(&[0, VALID | PHOTO | PHOTOENV | CAVE], 0));
        // Nothing after n_before.
        assert!(!loaded_after(&[0, gameplay], 2));
        assert!(!loaded_after(&[0, gameplay], 9));
        assert!(!loaded_after(&[], 0));
    }

    #[test]
    fn photo_and_names() {
        assert!(in_photo(VALID | PHOTO));
        assert!(!in_photo(VALID | PHOTOENV));
        assert_eq!(
            names(VALID | PHOTO | PHOTOENV),
            vec!["valid", "photo", "photoenv"]
        );
        assert_eq!(names(0), Vec::<&str>::new());
        assert_eq!(BITS.iter().fold(0, |all, (_, bit)| all | bit), 4095);
    }
}
