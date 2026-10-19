//! w3ctl.py and console_type.py as data: key tokens, the timed plan of key events game_keys sends (what
//! `w3ctl.py keys --dry-run` prints), a console line's plan (each character by its US virtual key, sent as the scancode
//! the game's layout gives that key), and the checks of the console's prompt row (the typed text before Enter, the
//! prompt gone after the close). The Windows layer sends a plan with SendInput and checks the game's focus before every
//! event.

/// w3ctl's key hold (REDengine drops very short synthetic taps) and its pause between key tokens.
pub const HOLD_MS: u64 = 200;
pub const GAP_MS: u64 = 400;
/// The longest hold or wait a token may ask for: a plan blocks the session thread while it runs.
pub const MAX_MS: u64 = 60_000;
/// Console typing. Every key event of a line's text is `CHAR_MS` after the one before (down, 30, up, 30, next, shift
/// included): live boot 2's holds arrived at 30 ms 4 of 4, 16 ms 2 of 2, 8 ms 1 of 2 and 0 ms 0 of 4, so the game drops
/// a tap shorter than its frame. About 60 ms a character, faster than a person types. The console and Enter keys keep
/// w3ctl's hold, then `CONSOLE_OPEN_MS` for the console to take the text, `SETTLE_MS` before each read of the prompt
/// row (the text before Enter, the prompt gone after the close), and `ENTER_MS` for the line to run before the close.
/// ponytail: 30 ms is about 1.5 frames at boot 1's ~50 fps; below about 33 fps a 30 ms tap may drop again (the check
/// before Enter then refuses the line), and CHAR_MS goes up.
pub const CHAR_MS: u64 = 30;
pub const CONSOLE_OPEN_MS: u64 = 300;
pub const ENTER_MS: u64 = 300;
pub const SETTLE_MS: u64 = 300;
/// The Set-1 scancode of the key left of 1 (US `~`). The console's own key is VK_OEM_3 on the game's keyboard layout,
/// which the Windows layer looks up (verified live on a non-US layout); this is the fallback, and `grave` / `tilde`.
pub const GRAVE: u16 = 0x29;

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key by Set-1 scancode (>= 0x100: E0-extended), down or up.
    Key { name: String, sc: u16, up: bool },
    /// A pause; `why` is w3ctl's label (hold, gap, wait) or console_type's (tap, shift, settle).
    Sleep { ms: u64, why: &'static str },
}

const NAMED: [(&str, u16); 33] = [
    ("escape", 0x01),
    ("esc", 0x01),
    ("tab", 0x0F),
    ("backspace", 0x0E),
    ("enter", 0x1C),
    ("space", 0x39),
    ("lshift", 0x2A),
    ("rshift", 0x36),
    ("lctrl", 0x1D),
    ("lalt", 0x38),
    ("rctrl", 0x11D),
    ("ralt", 0x138),
    ("up", 0x148),
    ("down", 0x150),
    ("left", 0x14B),
    ("right", 0x14D),
    ("insert", 0x152),
    ("delete", 0x153),
    ("home", 0x147),
    ("end", 0x14F),
    ("pgup", 0x149),
    ("pageup", 0x149),
    ("pgdn", 0x151),
    ("pagedown", 0x151),
    ("ctrl", 0x1D),
    ("shift", 0x2A),
    ("alt", 0x38),
    ("period", 0x34),
    ("minus", 0x0C),
    ("comma", 0x33),
    ("grave", GRAVE),
    ("tilde", GRAVE),
    ("console", GRAVE),
];

/// w3ctl's scancode table: a-z, 0-9, f1-f12 and the names above (lower case).
pub fn scancode(name: &str) -> Option<u16> {
    if let [c] = name.as_bytes() {
        let rows = [("qwertyuiop", 0x10), ("asdfghjkl", 0x1E), ("zxcvbnm", 0x2C)];
        for (row, first) in rows {
            if let Some(i) = row.find(char::from(*c)) {
                return Some(first + i as u16);
            }
        }
        if c.is_ascii_digit() {
            return Some(0x02 + (u16::from(c - b'0') + 9) % 10); // 1 = 0x02 ... 9 = 0x0A, 0 = 0x0B
        }
    }
    let f = name
        .strip_prefix('f')
        .filter(|n| !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()));
    match f.and_then(|n| n.parse::<u16>().ok()) {
        Some(n @ 1..=10) => return Some(0x3A + n),
        Some(n @ 11..=12) => return Some(0x4C + n),
        _ => {}
    }
    NAMED.iter().find(|(n, _)| *n == name).map(|&(_, sc)| sc)
}

/// A chord that acts on Windows rather than the game, which game_keys never sends (security review): Alt+Tab,
/// Alt+Esc, Alt+F4, Ctrl+Esc (and Ctrl+Shift+Esc), Ctrl+Alt+anything. The Windows keys aren't in the table at all.
fn acts_on_windows(keys: &[&str]) -> bool {
    let has = |names: &[&str]| keys.iter().any(|k| names.contains(k));
    let ctrl = has(&["ctrl", "lctrl", "rctrl"]);
    let alt = has(&["alt", "lalt", "ralt"]);
    let esc = has(&["escape", "esc"]);
    (alt && (esc || has(&["tab", "f4"]))) || (ctrl && (alt || esc))
}

/// w3ctl's `parse_tokens`: each token is a key or a chord (`ctrl+shift+e`: pressed in order, released in
/// reverse), `name:ms` holds it that long instead of `hold`, `wait:ms` sleeps; `gap` goes between key tokens. The key
/// `console` is `console` (the layout's console key). Refused: an unknown key (the Windows keys among them), a chord
/// that acts on Windows (`acts_on_windows`), a bad or missing duration, one over `MAX_MS`.
pub fn plan<S: AsRef<str>>(
    tokens: &[S],
    hold: u64,
    gap: u64,
    console: u16,
) -> Result<Vec<Event>, String> {
    let mut plan = Vec::new();
    let mut keyed = false;
    for token in tokens {
        let token = token.as_ref();
        let lower = token.to_lowercase();
        let (name, ms) = lower.split_once(':').unwrap_or((&lower, ""));
        let ms = match ms {
            "" => None,
            ms if ms.bytes().all(|b| b.is_ascii_digit()) => Some(
                ms.parse()
                    .ok()
                    .filter(|ms| *ms <= MAX_MS)
                    .ok_or(format!("token {token:?}: over {MAX_MS} ms"))?,
            ),
            _ => return Err(format!("bad duration in token {token:?}")),
        };
        if name == "wait" {
            let ms = ms.ok_or("wait needs a duration, e.g. wait:500")?;
            plan.push(Event::Sleep { ms, why: "wait" });
            continue;
        }
        let keys = name
            .split('+')
            .map(|k| match k {
                "console" => Ok((k, console)),
                "win" | "lwin" | "rwin" => Err(format!(
                    "{k:?}: the Windows key acts on Windows, not the game; refused"
                )),
                _ => scancode(k).map(|sc| (k, sc)).ok_or(format!(
                    "unknown key {k:?}; known: a-z 0-9 f1-f12 and {}",
                    NAMED.map(|(n, _)| n).join(" ")
                )),
            })
            .collect::<Result<Vec<_>, String>>()?;
        let names: Vec<&str> = keys.iter().map(|&(k, _)| k).collect();
        if acts_on_windows(&names) {
            return Err(format!(
                "token {token:?}: that chord acts on Windows, not the game (Alt+Tab, Alt+Esc, Alt+F4, Ctrl+Esc, \
                 Ctrl+Shift+Esc, Ctrl+Alt+any); refused"
            ));
        }
        if keyed {
            plan.push(Event::Sleep {
                ms: gap,
                why: "gap",
            });
        }
        let key = |&(name, sc): &(&str, u16), up| Event::Key {
            name: name.into(),
            sc,
            up,
        };
        plan.extend(keys.iter().map(|k| key(k, false)));
        plan.push(Event::Sleep {
            ms: ms.unwrap_or(hold),
            why: "hold",
        });
        plan.extend(keys.iter().rev().map(|k| key(k, true)));
        keyed = true;
    }
    Ok(plan)
}

/// w3ctl's `describe`: one line of a dry run.
pub fn describe(event: &Event) -> String {
    match event {
        Event::Sleep { ms, why } => format!("sleep {ms:>5} ms  ({why})"),
        Event::Key { name, sc, up } => format!(
            "key   {:<4}  {name:<9} sc=0x{:02X}{}",
            if *up { "up" } else { "down" },
            sc & 0xFF,
            if *sc > 0xFF { " ext" } else { "" }
        ),
    }
}

/// The characters the console types besides letters (virtual keys A-Z) and digits (0-9): (character, US virtual key,
/// shift). The console reads a key's virtual key on the active layout through a US table (first experiment boot,
/// measured on a non-US layout: the US `-` position arrived as `=` there), so a character is its US virtual key and
/// shift, and the Windows layer sends whatever scancode the game's layout gives that virtual key.
///
/// Not typed yet, because no one has seen the console take them: `@` (VK 2 + shift), `"` (VK_OEM_7 + shift), `#` (VK 3 +
/// shift), `\` (VK_OEM_5), `|` (VK_OEM_5 + shift). `~` is VK_OEM_3 + shift, the console key itself, so it can't be
/// typed. Once a live check shows the console takes one, it is one row here.
const TYPED: [(char, u16, bool); 7] = [
    (' ', 0x20, false), // VK_SPACE
    (',', 0xBC, false), // VK_OEM_COMMA
    ('.', 0xBE, false), // VK_OEM_PERIOD
    ('-', 0xBD, false), // VK_OEM_MINUS
    ('(', 0x39, true),  // VK 9
    (')', 0x30, true),  // VK 0
    ('_', 0xBD, true),  // VK_OEM_MINUS
];

/// A character's US virtual key and whether shift goes with it: letters (capitals with shift), digits, `TYPED`.
pub fn console_vk(c: char) -> Option<(u16, bool)> {
    match c {
        'a'..='z' => Some((c.to_ascii_uppercase() as u16, false)),
        'A'..='Z' | '0'..='9' => Some((c as u16, c.is_ascii_uppercase())),
        _ => TYPED
            .iter()
            .find(|&&(t, ..)| t == c)
            .map(|&(_, vk, shift)| (vk, shift)),
    }
}

/// The scancode of a `console_vk` virtual key on the US layout: what console_type.py typed, and the layout quen plans
/// with where there is no game window to ask (dry runs, parity).
pub fn us_scancode(vk: u16) -> Option<u16> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => {
            scancode(&char::from(vk as u8).to_ascii_lowercase().to_string())
        }
        0x20 => Some(0x39),
        0xBC => Some(0x33),
        0xBE => Some(0x34),
        0xBD => Some(0x0C),
        _ => None,
    }
}

/// One console line's plan, console_type.py's keys at quen's own pace (`CHAR_MS`): open the console (its key held
/// `HOLD_MS`, then `CONSOLE_OPEN_MS`); each character down and up (shift down and up around a shifted one) by the
/// scancode `scancode_of` gives its US virtual key on the game's layout, `CHAR_MS` apart; a pause labelled `typed`
/// (`SETTLE_MS`), after which the Windows layer reads the prompt row ([`console_text`]) and presses Enter only if the
/// line shows; then Enter and `ENTER_MS`. With `close`, [`console_close`]. A load leaves `close` off: its close key
/// would land on the loading screen (first experiment boot), so game_load closes the console once gameplay is back.
/// Refused, nothing planned: a character with no US virtual key, or one whose key the layout lacks.
pub fn console_line(
    line: &str,
    console: u16,
    scancode_of: &dyn Fn(u16) -> Option<u16>,
    close: bool,
) -> Result<Vec<Event>, String> {
    let (mut bad, mut missing, mut taps) = (Vec::new(), Vec::new(), Vec::new());
    for c in line.chars() {
        match console_vk(c) {
            None => bad.push(c),
            Some((vk, shift)) => match scancode_of(vk) {
                None => missing.push(c),
                Some(sc) => taps.push((c, sc, shift)),
            },
        }
    }
    if !bad.is_empty() {
        return Err(format!(
            "characters the console can't type: {bad:?}; nothing typed (letters, digits, space and , . - ( ) _ only)"
        ));
    }
    if !missing.is_empty() {
        return Err(format!(
            "characters with no key on the game's keyboard layout: {missing:?}; nothing typed"
        ));
    }
    let open = format!("wait:{CONSOLE_OPEN_MS}");
    let mut plan = plan(&["console", &open], HOLD_MS, GAP_MS, console)?;
    let mut typed = Vec::new();
    for (c, sc, shift) in taps {
        let name = c.to_string();
        let shift = |up| shift.then(|| key("lshift", 0x2A, up));
        typed.extend(shift(false));
        typed.extend([key(&name, sc, false), key(&name, sc, true)]);
        typed.extend(shift(true));
    }
    for event in typed {
        if matches!(plan.last(), Some(Event::Key { .. })) {
            plan.push(Event::Sleep {
                ms: CHAR_MS,
                why: "char",
            });
        }
        plan.push(event);
    }
    plan.push(Event::Sleep {
        ms: SETTLE_MS,
        why: "typed",
    });
    let enter = format!("wait:{ENTER_MS}");
    plan.extend(self::plan(&["enter", &enter], HOLD_MS, GAP_MS, console)?);
    if close {
        plan.extend(console_close(console));
    }
    Ok(plan)
}

/// Closes the console: its key held `HOLD_MS`, then `SETTLE_MS` before the check that its prompt is gone.
pub fn console_close(console: u16) -> Vec<Event> {
    vec![
        key("console", console, false),
        Event::Sleep {
            ms: HOLD_MS,
            why: "hold",
        },
        key("console", console, true),
        Event::Sleep {
            ms: SETTLE_MS,
            why: "settle",
        },
    ]
}

/// After a line that didn't show on the prompt (the check before Enter): a backspace per character, `CHAR_MS` apart, to
/// take back whatever of it arrived, then [`console_close`]. Enter never went, so nothing ran, and the next line starts
/// on an empty prompt.
pub fn console_clear(chars: usize, console: u16) -> Vec<Event> {
    let char_ms = || Event::Sleep {
        ms: CHAR_MS,
        why: "char",
    };
    let back = |up| key("backspace", 0x0E, up);
    let mut plan: Vec<Event> = (0..chars)
        .flat_map(|_| [back(false), char_ms(), back(true), char_ms()])
        .collect();
    plan.extend(console_close(console));
    plan
}

fn key(name: &str, sc: u16, up: bool) -> Event {
    Event::Key {
        name: name.into(),
        sc,
        up,
    }
}

/// The console's '>' prompt, as (row, column) of a 1280x720 frame: the glyph's lit points (console_type.py's).
const PROMPT_ON: [(usize, usize); 7] = [
    (106, 4),
    (106, 5),
    (106, 6),
    (107, 6),
    (107, 7),
    (108, 4),
    (108, 5),
];
/// The glyph's black outline, as 2x2 squares of a 3840x2160 frame (top row, left column): above the top arm, the notch
/// between the arms, below the bottom arm. The game draws the glyph opaque, outline and all, so these read dark over
/// any background, where console_type.py's dark points beside the glyph show what the translucent console lets through
/// (live boot 3 outdoors at dusk: up to 137 there, 0-4 here, 184 on the glyph; indoors 33 there).
/// ponytail: measured at 4K only; other sizes read the same spots by area.
const PROMPT_OUTLINE: [(usize, usize); 7] = [
    (314, 10),
    (314, 12),
    (320, 10),
    (322, 10),
    (322, 12),
    (328, 14),
    (328, 16),
];
/// How much brighter the glyph must read than its outline, and a typed column's brightest point than its darkest.
const CONTRAST: i32 = 100;
/// Where typed text sits on the prompt's row, in 1280x720 terms: from column 9 rightwards, rows 102-112 (live boot 2 at
/// 4K: the text from column 10, x-height rows 105-110, the cursor 103-110, descenders to 111).
const TEXT_COL: usize = 9;
const TEXT_ROWS: std::ops::RangeInclusive<usize> = 102..=112;
/// The widest a typed character draws, in those columns, plus room: live boot 2's `quenxyz` spanned 33 (4.7 each).
const TEXT_PER_CHAR: usize = 8;

/// A pixel of a 1280x720 frame as a square of a 3840x2160 one.
fn cell((row, col): (usize, usize)) -> (usize, usize, usize) {
    (3 * row, 3 * col, 3)
}

/// A frame (`bgra`: `w` x `h` pixels, rows top-down) read at a square of a 3840x2160 frame (top row, left column, side)
/// scaled to it by area (cv2's INTER_AREA), on its darkest channel; and the prompt's levels there: its glyph's dimmest
/// lit point and its outline's brightest point.
fn prompt_levels(
    bgra: &[u8],
    w: usize,
    h: usize,
) -> (impl Fn((usize, usize, usize)) -> i32, i32, i32) {
    let (sx, sy) = (w as f64 / 3840.0, h as f64 / 2160.0);
    let at = move |(row, col, side): (usize, usize, usize)| {
        let (x0, x1) = (col as f64 * sx, (col + side) as f64 * sx);
        let (y0, y1) = (row as f64 * sy, (row + side) as f64 * sy);
        let mut sum = [0.0f64; 3];
        for y in y0 as usize..(y1.ceil() as usize).min(h) {
            let wy = (y1.min(y as f64 + 1.0) - y0.max(y as f64)).max(0.0);
            for x in x0 as usize..(x1.ceil() as usize).min(w) {
                let wx = (x1.min(x as f64 + 1.0) - x0.max(x as f64)).max(0.0);
                for (ch, s) in sum.iter_mut().enumerate() {
                    let v = bgra.get((y * w + x) * 4 + ch).copied().unwrap_or(0);
                    *s += f64::from(v) * wx * wy;
                }
            }
        }
        let area = (x1 - x0) * (y1 - y0);
        sum.map(|s| (s / area).round().clamp(0.0, 255.0) as i32)
            .into_iter()
            .min()
            .unwrap_or(0)
    };
    let on = PROMPT_ON
        .map(|p| at(cell(p)))
        .into_iter()
        .min()
        .unwrap_or(0);
    let off = PROMPT_OUTLINE.map(|(row, col)| at((row, col, 2)));
    (at, on, off.into_iter().max().unwrap_or(255))
}

/// console_type.py's `console_open`: whether the console's prompt shows in a frame, read at a fixed spot
/// ([`prompt_levels`]): the glyph `CONTRAST` brighter than its own outline, on any background (console_type.py's dark
/// points missed it over a bright sky: live boot 3). ponytail: one glyph at 5.01's spot, unseen in 4.04. At a scale
/// like 1080p's 1.5 the area average thins a one-pixel glyph (the parity case: 151 of 220), so a thinner one there may
/// read as closed. A frame smaller than 720p is read by area too, where cv2 interpolates.
pub fn console_open(bgra: &[u8], w: usize, h: usize) -> bool {
    let (_, on, off) = prompt_levels(bgra, w, h);
    on - off > CONTRAST
}

/// Before Enter: what the prompt's row shows of `line`. None: no prompt shows ([`console_open`]). Else the columns
/// right of the prompt, within `TEXT_PER_CHAR` a character, whose brightest point is `CONTRAST` over their darkest (a
/// typed character is white strokes in a black outline, opaque like the prompt; the background the console lets
/// through is smooth, or dark), and the fewest that `line` lights: one a character, spaces aside (`.` `i` `l` `(`
/// light about one), and 2 at least, over the cursor's one (live boot 2 at 4K: `quenxyz` lit 32, an empty prompt 0,
/// its cursor 85 over the dark; live boot 3 over the sky: a 44-character line lit 183, the same sky with no text 0).
/// ponytail: it catches a line mostly dropped, not a character or two; the bright minimap near column 1120 counts for a
/// line over about 140 characters, and a sharp edge in the scene behind (a roof on the sky) may count too.
pub fn console_text(bgra: &[u8], w: usize, h: usize, line: &str) -> Option<(usize, usize)> {
    let (at, on, off) = prompt_levels(bgra, w, h);
    if on - off <= CONTRAST {
        return None;
    }
    let chars = line.chars().count();
    let end = (TEXT_COL + TEXT_PER_CHAR * chars).min(1280);
    let lit = (TEXT_COL..end)
        .filter(|&col| {
            let levels = TEXT_ROWS.clone().map(|row| at(cell((row, col))));
            let (lo, hi) = levels.fold((255, 0), |(lo, hi), v| (v.min(lo), v.max(hi)));
            hi - lo > CONTRAST
        })
        .count();
    Some((lit, line.chars().filter(|&c| c != ' ').count().max(2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(tokens: &[&str]) -> Vec<String> {
        plan(tokens, HOLD_MS, GAP_MS, 0x27)
            .unwrap()
            .iter()
            .map(describe)
            .collect()
    }

    #[test]
    fn tokens_plan_as_w3ctl_does() {
        assert_eq!(
            lines(&["wait:100", "Ctrl+P:150", "up", "console"]),
            [
                "sleep   100 ms  (wait)",
                "key   down  ctrl      sc=0x1D",
                "key   down  p         sc=0x19",
                "sleep   150 ms  (hold)",
                "key   up    p         sc=0x19",
                "key   up    ctrl      sc=0x1D",
                "sleep   400 ms  (gap)",
                "key   down  up        sc=0x48 ext",
                "sleep   200 ms  (hold)",
                "key   up    up        sc=0x48 ext",
                "sleep   400 ms  (gap)",
                "key   down  console   sc=0x27",
                "sleep   200 ms  (hold)",
                "key   up    console   sc=0x27",
            ]
        );
        let sc = |k| scancode(k).unwrap();
        assert_eq!(
            [
                sc("1"),
                sc("9"),
                sc("0"),
                sc("f1"),
                sc("f10"),
                sc("f11"),
                sc("f12"),
                sc("grave")
            ],
            [0x02, 0x0A, 0x0B, 0x3B, 0x44, 0x57, 0x58, 0x29]
        );
        assert_eq!(lines(&["e:"]), lines(&["e"]));
        for bad in [
            "wait", "wait:", "e:x", "e:+5", "e:1:2", "a++b", "", "f13", "f01", "f0", "nokey",
            "e:60001",
        ] {
            assert!(plan(&[bad], HOLD_MS, GAP_MS, GRAVE).is_err(), "{bad:?}");
        }
    }

    /// The security review: nothing game_keys sends may act on Windows itself.
    #[test]
    fn keys_that_act_on_windows_are_refused() {
        for bad in [
            "win",
            "lwin",
            "rwin:100",
            "lwin+l",
            "win+r",
            "alt+tab",
            "lalt+tab",
            "ralt+f4",
            "alt+f4:50",
            "alt+esc",
            "ctrl+escape",
            "lctrl+lshift+escape",
            "ctrl+shift+esc",
            "ctrl+alt+e",
            "lctrl+ralt+delete",
            "alt+ctrl+t",
        ] {
            let why = plan(&[bad], HOLD_MS, GAP_MS, GRAVE).unwrap_err();
            assert!(why.contains("acts on Windows"), "{bad:?}: {why}");
        }
        // The same keys, apart or with the game's own chords, are the game's.
        for fine in [
            &["alt", "tab"][..],
            &["lalt+e"],
            &["escape"],
            &["ctrl+shift+e"],
            &["lshift+f4"],
            &["ctrl+p"],
            &["ralt"],
        ] {
            assert!(plan(fine, HOLD_MS, GAP_MS, GRAVE).is_ok(), "{fine:?}");
        }
    }

    fn keys(plan: &[Event]) -> Vec<(u16, bool)> {
        plan.iter()
            .filter_map(|e| match e {
                Event::Key { sc, up, .. } => Some((*sc, *up)),
                Event::Sleep { .. } => None,
            })
            .collect()
    }

    #[test]
    fn console_lines_open_type_and_close() {
        let plan = console_line("a(B", 0x27, &us_scancode, true).unwrap();
        assert_eq!(
            keys(&plan),
            [
                (0x27, false),
                (0x27, true),
                (0x1E, false),
                (0x1E, true),
                (0x2A, false),
                (0x0A, false),
                (0x0A, true),
                (0x2A, true),
                (0x2A, false),
                (0x30, false),
                (0x30, true),
                (0x2A, true),
                (0x1C, false),
                (0x1C, true),
                (0x27, false),
                (0x27, true),
            ]
        );
        assert_eq!(
            plan.last(),
            Some(&Event::Sleep {
                ms: SETTLE_MS,
                why: "settle"
            })
        );
        // Live boot 2: every key event of the text is CHAR_MS after the one before (down, 30, up, 30, next, shift
        // too), then the `typed` pause where the prompt row is read, and only then Enter.
        let open = plan
            .iter()
            .position(|e| matches!(e, Event::Sleep { why: "wait", .. }))
            .unwrap();
        let text: Vec<&Event> = plan[open + 1..].iter().take(10 * 2 - 1).collect();
        for (i, event) in text.iter().enumerate() {
            let char_ms = Event::Sleep {
                ms: CHAR_MS,
                why: "char",
            };
            assert_eq!(i % 2 == 1, **event == char_ms, "event {i}: {plan:?}");
        }
        assert_eq!(
            plan[open + 1 + 10 * 2 - 1..open + 1 + 10 * 2 + 1],
            [
                Event::Sleep {
                    ms: SETTLE_MS,
                    why: "typed"
                },
                key("enter", 0x1C, false)
            ]
        );
        // A load's line stops after Enter: its close key would land on the loading screen.
        let load = console_line("a", 0x27, &us_scancode, false).unwrap();
        assert_eq!(keys(&load)[4..], [(0x1C, false), (0x1C, true)]);
        // A line that didn't show: a backspace for each character, then the close; never Enter.
        let clear = console_clear(2, 0x27);
        assert_eq!(
            keys(&clear),
            [
                (0x0E, false),
                (0x0E, true),
                (0x0E, false),
                (0x0E, true),
                (0x27, false),
                (0x27, true),
            ]
        );
        assert_eq!(clear[1], clear[3]);
        assert_eq!(
            clear[1],
            Event::Sleep {
                ms: CHAR_MS,
                why: "char"
            }
        );
        assert!(matches!(
            load.last(),
            Some(Event::Sleep {
                ms: ENTER_MS,
                why: "wait"
            })
        ));
        for (bad, named) in [
            ("say('hi')", '\''),
            ("é", 'é'),
            ("a;b", ';'),
            ("x\ty", '\t'),
            ("a@b", '@'),
            ("æøå", 'å'),
        ] {
            let why = console_line(bad, GRAVE, &us_scancode, true).unwrap_err();
            assert!(why.contains(&format!("{named:?}")), "{bad:?}: {why}");
        }
    }

    /// A character is its US virtual key: on a layout that puts VK_OEM_MINUS elsewhere, `-` and `_` go to that key, and
    /// on one without it they are refused before anything is typed.
    #[test]
    fn characters_follow_the_layout_s_virtual_keys() {
        let moved = |vk| {
            if vk == 0xBD {
                Some(0x35)
            } else {
                us_scancode(vk)
            }
        };
        let plan = console_line("a-_", GRAVE, &moved, true).unwrap();
        let typed = &keys(&plan)[2..10];
        assert_eq!(
            typed,
            [
                (0x1E, false),
                (0x1E, true),
                (0x35, false),
                (0x35, true),
                (0x2A, false),
                (0x35, false),
                (0x35, true),
                (0x2A, true),
            ]
        );
        let none = |vk| (vk != 0xBD).then(|| us_scancode(vk)).flatten();
        let why = console_line("a-b", GRAVE, &none, true).unwrap_err();
        assert!(why.contains("'-'") && why.contains("no key"), "{why}");
        assert_eq!(console_vk('q'), Some((0x51, false)));
        assert_eq!(console_vk('Q'), Some((0x51, true)));
        assert_eq!(console_vk('7'), Some((0x37, false)));
        assert_eq!(console_vk('~'), None);
    }

    /// Fills a square of a 3840x2160 frame (top row, left column, side).
    fn fill(px: &mut [u8], (row, col, side): (usize, usize, usize), level: u8) {
        for y in row..row + side {
            px[(y * 3840 + col) * 4..(y * 3840 + col + side) * 4].fill(level);
        }
    }

    /// Fills the 3x3 block of a 3840x2160 frame that is (row, col) at 1280x720.
    fn paint(px: &mut [u8], at: (usize, usize), level: u8) {
        fill(px, cell(at), level);
    }

    /// A 3840x2160 frame of `bg`, with the prompt when `prompt`: its glyph's points at `level`, its outline black, as the
    /// game draws it.
    fn frame(prompt: bool, level: u8, bg: u8) -> Vec<u8> {
        let mut px = vec![bg; 3840 * 2160 * 4];
        if prompt {
            for at in PROMPT_ON {
                paint(&mut px, at, level);
            }
            for (row, col) in PROMPT_OUTLINE {
                fill(&mut px, (row, col, 2), 0);
            }
        }
        px
    }

    /// console_type.py's dark points beside the glyph, which quen read until live boot 3.
    const BESIDE: [(usize, usize); 8] = [
        (105, 5),
        (105, 6),
        (105, 7),
        (107, 3),
        (107, 4),
        (109, 5),
        (109, 6),
        (109, 7),
    ];

    /// The sky behind the console's prompt row in live boot 3's outdoor frame, rows 102-112 (its darker band dips at the
    /// prompt): what the translucent console let through, re-created, not copied.
    const SKY: [u8; 11] = [137, 137, 136, 138, 139, 143, 151, 158, 166, 168, 176];

    /// A frame at the levels live boot 3 measured where quen reads, re-created: the glyph's points (224 255 184 255 230
    /// 224 238), its outline (4 4 0 0 0 0 0), over `bg` with the prompt row's band `band` (rows 102-112) when given.
    fn boot3(prompt: bool, bg: u8, band: Option<[u8; 11]>) -> Vec<u8> {
        let mut px = vec![bg; 3840 * 2160 * 4];
        for (row, level) in TEXT_ROWS.zip(band.unwrap_or([bg; 11])) {
            px[(3 * row * 3840) * 4..(3 * row + 3) * 3840 * 4].fill(level);
        }
        if prompt {
            for (at, level) in PROMPT_ON
                .into_iter()
                .zip([224, 255, 184, 255, 230, 224, 238])
            {
                paint(&mut px, at, level);
            }
            for ((row, col), level) in PROMPT_OUTLINE.into_iter().zip([4, 4, 0, 0, 0, 0, 0]) {
                fill(&mut px, (row, col, 2), level);
            }
        }
        px
    }

    /// The levels console_type.py's points read: the glyph's dimmest, the brightest beside it.
    fn beside(px: &[u8]) -> (i32, i32) {
        let (at, on, _) = prompt_levels(px, 3840, 2160);
        (on, BESIDE.map(|p| at(cell(p))).into_iter().max().unwrap())
    }

    /// Live boot 3: outdoors at dusk the dark points beside the glyph read the sky (up to 137 against the glyph's 184),
    /// so game_load refused a line typed right ("no prompt"), and the check that the console closed read an open one as
    /// closed. The glyph's own outline stays dark there. Its frames' levels, re-created.
    #[test]
    fn the_prompt_shows_over_a_bright_sky() {
        let sky = boot3(true, 137, Some(SKY));
        let (on, by) = beside(&sky);
        assert!(
            on - by <= 100,
            "console_type.py's points read it closed: {on} {by}"
        );
        assert!(console_open(&sky, 3840, 2160), "the outline reads it open");
        // Open indoors (the beside points 33), closed indoors (every point 1-2), closed over the sky.
        assert!(console_open(&boot3(true, 1, None), 3840, 2160));
        assert!(!console_open(&boot3(false, 2, None), 3840, 2160));
        assert!(!console_open(&boot3(false, 137, Some(SKY)), 3840, 2160));
        // The typed line over the sky: each character's strokes white, the sky between. Over the same sky with
        // nothing typed, no column (halfway from the beside points to the glyph, the sky's lower rows counted all 352).
        let line = "quenload(Synthetic_Save_Name_For_The_Sky_Ab)";
        assert_eq!(console_text(&sky, 3840, 2160, line), Some((0, 44)));
        let mut typed = sky.clone();
        for col in (10..10 + 47 * line.len() / 10).filter(|c| c % 5 != 4) {
            paint(&mut typed, (107, col), 255);
        }
        let (lit, least) = console_text(&typed, 3840, 2160, line).unwrap();
        assert!(lit >= least && lit == 165, "{lit} {least}");
    }

    /// The prompt as live boot 2's 4K frames drew it, in shape: the glyph at 184-255, and the bold glyph's inner
    /// corner and right tips lit part way (133, 70, 59), which console_type.py's dark points included.
    fn bold() -> Vec<u8> {
        let mut px = frame(true, 184, 1);
        for (at, level) in [((107, 5), 133), ((106, 7), 70), ((108, 7), 59)] {
            paint(&mut px, at, level);
        }
        px
    }

    #[test]
    fn the_prompt_shows_only_lit_against_dark() {
        assert!(console_open(&frame(true, 200, 10), 3840, 2160));
        assert!(!console_open(&frame(false, 200, 10), 3840, 2160));
        assert!(!console_open(&frame(true, 100, 10), 3840, 2160));
        assert!(console_open(&frame(true, 101, 10), 3840, 2160));
        // Over a white sky the outline stays black: seen. Without the glyph, the sky alone: closed.
        assert!(console_open(&frame(true, 255, 250), 3840, 2160));
        assert!(!console_open(&frame(false, 255, 250), 3840, 2160));
        assert!(!console_open(&[], 3840, 2160));
        // The bold 4K glyph reads as open (console_type.py's points read it as closed: 184 - 133 < 100).
        assert!(console_open(&bold(), 3840, 2160));
    }

    /// Before Enter: the text right of the prompt, counted in lit columns against the fewest the line lights.
    #[test]
    fn the_typed_text_shows_on_the_prompt_row() {
        let text = |px: &[u8], line| console_text(px, 3840, 2160, line);
        let mut px = bold();
        // An empty prompt, its cursor dimmer than halfway (live boot 2: 86): nothing typed shows.
        for row in 103..=110 {
            paint(&mut px, (row, 10), 86);
        }
        assert_eq!(text(&px, "quenload(x)"), Some((0, 11)));
        assert_eq!(text(&px, ""), Some((0, 2)));
        // `quenxyz` as live boot 2 saw it: 33 columns from column 10, all but one lit.
        for col in (10..43).filter(|&c| c != 29) {
            paint(&mut px, (107, col), 250);
        }
        // The minimap's bright pixels far right are beyond the line's columns.
        paint(&mut px, (105, 1124), 255);
        assert_eq!(text(&px, "quenxyz"), Some((32, 7)));
        // A long line of which only those 7 characters arrived: fewer lit columns than characters.
        let long = "quenload(Quen_Typing_Check_0123456789_ABCDEFGHIJ_klmnop-qrstuv)";
        assert_eq!(text(&px, long), Some((32, 63)));
        assert_eq!(
            text(&px, "a b"),
            Some((22, 2)),
            "the window is 8 columns a character"
        );
        // No prompt: nothing to read.
        assert_eq!(text(&frame(false, 200, 10), "quenxyz"), None);
    }
}
