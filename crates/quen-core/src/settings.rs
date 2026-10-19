//! Byte-exact overrides of a Witcher 3 `dx12user.settings` (an INI-like file, CRLF or LF), with the semantics of
//! the harness's `W3_SETTINGS` (flat) and `W3_SECTION_SETTINGS` (per section) switches. Every byte an override doesn't
//! name stays identical. [`validate`] runs both on an in-memory copy: it is what runs before any backup.

use std::fmt;
use std::ops::Range;

/// Why a set of overrides can't be applied. Nothing is written when one comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    /// An item that isn't `Key=Value` (flat) or `Section:Key=Value` (per section) with a non-empty key and section,
    /// a key not starting with `[`, and no line break.
    BadItem(String),
    /// A flat key must start exactly one line, as `Key=`.
    KeyCount { key: String, found: usize },
    /// A section's header line `[Section]` must occur exactly once.
    SectionCount { section: String, found: usize },
    /// A key may start at most one line of its section.
    KeyInSection {
        section: String,
        key: String,
        found: usize,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadItem(item) => write!(
                f,
                "override {item:?} is not Key=Value (flat) or Section:Key=Value (per section) on one line"
            ),
            Self::KeyCount { key, found } => {
                write!(
                    f,
                    "key {key:?} starts {found} lines, an override needs exactly 1"
                )
            }
            Self::SectionCount { section, found: 0 } => write!(f, "no section header [{section}]"),
            Self::SectionCount { section, found } => {
                write!(
                    f,
                    "section header [{section}] is on {found} lines, needs exactly 1"
                )
            }
            Self::KeyInSection {
                section,
                key,
                found,
            } => {
                write!(
                    f,
                    "key {key:?} starts {found} lines of [{section}], at most 1 allowed"
                )
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// Applies a flat spec `Key=Value[,Key=Value...]` in order, so a later value wins. Each key must start exactly one line
/// of the file as `Key=` (matched literally); that line's value is replaced literally, up to its line break.
pub fn apply_flat(data: &[u8], spec: &str) -> Result<Vec<u8>, SettingsError> {
    let mut out = data.to_vec();
    for item in items(spec, ',') {
        let (key, value) = key_value(item, item)?;
        let hits: Vec<Range<usize>> = lines(&out)
            .filter_map(|l| value_of(&out, &l, key))
            .collect();
        let [hit] = hits.as_slice() else {
            return Err(SettingsError::KeyCount {
                key: key.into(),
                found: hits.len(),
            });
        };
        out.splice(hit.clone(), value.bytes());
    }
    Ok(out)
}

/// Applies a per-section spec `Section:Key=Value[;...]` in order. The header line `[Section]`, followed by its line
/// break, must occur once; the section's body runs to the next line starting with `[`, or the end. A key already in the
/// body (at most once) gets the new value; otherwise `Key=Value` goes on a new line right after the header, ending in
/// CRLF if the file holds any CRLF, else LF.
pub fn apply_sections(data: &[u8], spec: &str) -> Result<Vec<u8>, SettingsError> {
    let mut out = data.to_vec();
    for item in items(spec, ';') {
        let bad = || SettingsError::BadItem(item.into());
        let (section, kv) = item.split_once(':').ok_or_else(bad)?;
        if section.is_empty() || section.contains(['\r', '\n']) {
            return Err(bad());
        }
        let (key, value) = key_value(item, kv)?;
        let header = format!("[{section}]");
        let all: Vec<Line> = lines(&out).collect();
        let heads: Vec<usize> = (0..all.len())
            .filter(|&i| all[i].eol && out[all[i].start..all[i].end] == *header.as_bytes())
            .collect();
        let [head] = heads[..] else {
            return Err(SettingsError::SectionCount {
                section: section.into(),
                found: heads.len(),
            });
        };
        let hits: Vec<Range<usize>> = all[head + 1..]
            .iter()
            .take_while(|l| out.get(l.start) != Some(&b'['))
            .filter_map(|l| value_of(&out, l, key))
            .collect();
        match hits.as_slice() {
            [] => {
                let eol = if out.windows(2).any(|w| w == b"\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                let at = all[head].next;
                out.splice(at..at, format!("{key}={value}{eol}").into_bytes());
            }
            [hit] => {
                out.splice(hit.clone(), value.bytes());
            }
            _ => {
                return Err(SettingsError::KeyInSection {
                    section: section.into(),
                    key: key.into(),
                    found: hits.len(),
                });
            }
        }
    }
    Ok(out)
}

/// Applies `flat`, then `sections`, to an in-memory copy of `data`. Run it before any backup: an error names the key
/// or section at fault and means nothing was touched; the result is the patched file, identical outside the overrides.
pub fn validate(data: &[u8], flat: &str, sections: &str) -> Result<Vec<u8>, SettingsError> {
    apply_sections(&apply_flat(data, flat)?, sections)
}

/// The non-empty items of a spec, so an empty spec or a trailing separator is no override.
fn items(spec: &str, sep: char) -> impl Iterator<Item = &str> {
    spec.split(sep).filter(|item| !item.is_empty())
}

/// Splits `Key=Value` at the first `=`, so the value may hold more. `item` is the whole spec item, for the error.
fn key_value<'a>(item: &str, kv: &'a str) -> Result<(&'a str, &'a str), SettingsError> {
    match kv.split_once('=') {
        Some((key, value))
            if !key.is_empty() && !key.starts_with('[') && !kv.contains(['\r', '\n']) =>
        {
            Ok((key, value))
        }
        _ => Err(SettingsError::BadItem(item.into())),
    }
}

/// A line of the file: `start..end` is its text without the line break (a `\r` before the `\n` belongs to the break),
/// `next` is where the following line starts, and `eol` says whether it ends in `\n` (the last line may not).
struct Line {
    start: usize,
    end: usize,
    next: usize,
    eol: bool,
}

fn lines(data: &[u8]) -> impl Iterator<Item = Line> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == data.len() {
            return None;
        }
        let nl = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| start + i);
        let next = nl.map_or(data.len(), |i| i + 1);
        let mut end = nl.unwrap_or(data.len());
        if end > start && data[end - 1] == b'\r' {
            end -= 1;
        }
        let line = Line {
            start,
            end,
            next,
            eol: nl.is_some(),
        };
        start = next;
        Some(line)
    })
}

/// Where the value of `line` is, if the line starts with `key=`.
fn value_of(data: &[u8], line: &Line, key: &str) -> Option<Range<usize>> {
    let text = &data[line.start..line.end];
    (text.starts_with(key.as_bytes()) && text.get(key.len()) == Some(&b'='))
        .then_some(line.start + key.len() + 1..line.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic settings file: `MotionBlur` is in two sections, `[Empty]` has no keys.
    const LF: &str = "[Rendering]\nAllowFG=true\nMotionBlur=true\n[Rendering/MtRender]\nThreads=4\n\
                      [Empty]\n[Gameplay]\nAutoRotate=true\nMotionBlur=false\n";

    fn crlf(text: &str) -> String {
        text.replace('\n', "\r\n")
    }

    fn flat(data: &str, spec: &str) -> Result<String, SettingsError> {
        apply_flat(data.as_bytes(), spec).map(|out| String::from_utf8(out).unwrap())
    }

    fn sections(data: &str, spec: &str) -> Result<String, SettingsError> {
        apply_sections(data.as_bytes(), spec).map(|out| String::from_utf8(out).unwrap())
    }

    #[test]
    fn flat_replaces_one_value_and_nothing_else() {
        let want = LF.replace("AllowFG=true", "AllowFG=false");
        assert_eq!(flat(LF, "AllowFG=false").unwrap(), want);
        assert_eq!(flat(&crlf(LF), "AllowFG=false").unwrap(), crlf(&want));
    }

    #[test]
    fn flat_needs_exactly_one_line_per_key() {
        let missing = flat(LF, "AllowFG=false,Missing=1").unwrap_err();
        assert_eq!(
            missing,
            SettingsError::KeyCount {
                key: "Missing".into(),
                found: 0
            }
        );
        assert!(missing.to_string().contains("\"Missing\""));
        // A key repeated in another section is ambiguous for a flat override.
        let twice = flat(&crlf(LF), "MotionBlur=false").unwrap_err();
        assert_eq!(
            twice,
            SettingsError::KeyCount {
                key: "MotionBlur".into(),
                found: 2
            }
        );
        assert!(twice.to_string().contains("\"MotionBlur\""));
    }

    #[test]
    fn flat_keys_match_literally_at_line_start() {
        let data = "Key=1\nKeyX=2\n Indented=3\nkey=4\nA.B=5\nSpaced = 6\nA*=7\n";
        assert_eq!(flat(data, "Key=9").unwrap(), data.replace("Key=1", "Key=9"));
        assert_eq!(flat(data, "A.B=0").unwrap(), data.replace("A.B=5", "A.B=0"));
        assert_eq!(flat(data, "A*=0").unwrap(), data.replace("A*=7", "A*=0"));
        for key in ["Indented", "Spaced", "KEY", "A"] {
            let err = flat(data, &format!("{key}=0")).unwrap_err();
            assert_eq!(
                err,
                SettingsError::KeyCount {
                    key: key.into(),
                    found: 0
                }
            );
        }
    }

    #[test]
    fn flat_applies_in_order_so_the_later_value_wins() {
        let out = flat(LF, "AllowFG=1,Threads=8,AllowFG=2").unwrap();
        assert_eq!(
            out,
            LF.replace("AllowFG=true", "AllowFG=2")
                .replace("Threads=4", "Threads=8")
        );
    }

    #[test]
    fn values_are_literal() {
        let value = r"a\1\\b$0${x}=c\n";
        let out = flat(LF, &format!("Threads={value}")).unwrap();
        assert_eq!(out, LF.replace("Threads=4", &format!("Threads={value}")));
        let out = sections(
            LF,
            &format!("Gameplay:MotionBlur={value};Empty:New={value}"),
        )
        .unwrap();
        let want = LF
            .replace("MotionBlur=false", &format!("MotionBlur={value}"))
            .replace("[Empty]\n", &format!("[Empty]\nNew={value}\n"));
        assert_eq!(out, want);
    }

    #[test]
    fn last_line_without_line_break() {
        assert_eq!(flat("A=1\nB=2", "B=3").unwrap(), "A=1\nB=3");
        assert_eq!(flat("A=1\r\nB=2\r", "B=3").unwrap(), "A=1\r\nB=3\r");
        assert_eq!(sections("[S]\nA=1", "S:A=2").unwrap(), "[S]\nA=2");
    }

    #[test]
    fn sections_replace_only_inside_their_section() {
        let want = LF.replace("MotionBlur=false", "MotionBlur=true");
        assert_eq!(sections(LF, "Gameplay:MotionBlur=true").unwrap(), want);
        let want = LF.replace("Threads=4", "Threads=2");
        assert_eq!(
            sections(&crlf(LF), "Rendering/MtRender:Threads=2").unwrap(),
            crlf(&want)
        );
    }

    #[test]
    fn sections_insert_right_after_the_header() {
        let want = LF.replace("[Gameplay]\n", "[Gameplay]\nHdr=1\n");
        assert_eq!(sections(LF, "Gameplay:Hdr=1").unwrap(), want);
        assert_eq!(sections(&crlf(LF), "Gameplay:Hdr=1").unwrap(), crlf(&want));
        // Any CRLF in the file makes the inserted line CRLF.
        assert_eq!(
            sections("[A]\nX=1\r\n", "A:Y=2").unwrap(),
            "[A]\nY=2\r\nX=1\r\n"
        );
    }

    #[test]
    fn sections_insert_into_an_empty_section() {
        let want = LF.replace("[Empty]\n", "[Empty]\nNew=1\n");
        assert_eq!(sections(LF, "Empty:New=1").unwrap(), want);
        assert_eq!(
            sections("[A]\nX=1\n[B]\n", "B:Y=2").unwrap(),
            "[A]\nX=1\n[B]\nY=2\n"
        );
        assert_eq!(sections("[B]\r\n", "B:Y=2").unwrap(), "[B]\r\nY=2\r\n");
    }

    #[test]
    fn section_header_must_occur_once() {
        let none = |section: &str| SettingsError::SectionCount {
            section: section.into(),
            found: 0,
        };
        assert_eq!(sections(LF, "Missing:A=1").unwrap_err(), none("Missing"));
        assert_eq!(
            sections(LF, "rendering:A=1").unwrap_err(),
            none("rendering")
        );
        // The header must be the whole line and end in a line break.
        assert_eq!(sections("[A] \nX=1\n", "A:X=2").unwrap_err(), none("A"));
        assert_eq!(sections("X=1\n[A]", "A:X=2").unwrap_err(), none("A"));
        let twice = sections("[A]\nX=1\n[A]\nY=1\n", "A:X=2").unwrap_err();
        assert_eq!(
            twice,
            SettingsError::SectionCount {
                section: "A".into(),
                found: 2
            }
        );
        assert!(none("Missing").to_string().contains("[Missing]"));
    }

    #[test]
    fn section_key_may_occur_at_most_once() {
        let err = sections("[A]\nX=1\nX=2\n[B]\nX=3\n", "A:X=9").unwrap_err();
        let want = SettingsError::KeyInSection {
            section: "A".into(),
            key: "X".into(),
            found: 2,
        };
        assert_eq!(err, want);
        assert!(err.to_string().contains("\"X\"") && err.to_string().contains("[A]"));
        // A key repeated in other sections is fine.
        assert_eq!(
            sections("[A]\nX=1\n[B]\nX=3\n", "B:X=9").unwrap(),
            "[A]\nX=1\n[B]\nX=9\n"
        );
    }

    #[test]
    fn sections_apply_in_order_so_the_later_value_wins() {
        let out = sections(LF, "Gameplay:Hdr=1;Gameplay:Hdr=2;Rendering:AllowFG=0").unwrap();
        let want = LF
            .replace("[Gameplay]\n", "[Gameplay]\nHdr=2\n")
            .replace("AllowFG=true", "AllowFG=0");
        assert_eq!(out, want);
    }

    #[test]
    fn bad_items_are_refused() {
        for spec in [
            "NoEquals",
            "=1",
            "[Rendering]=1",
            "A=1\nB=2",
            "AllowFG=1,Threads",
        ] {
            let err = flat(LF, spec).unwrap_err();
            assert!(
                matches!(err, SettingsError::BadItem(_)),
                "{spec:?}: {err:?}"
            );
        }
        for spec in [
            "NoColon=1",
            ":A=1",
            "Gameplay:NoEquals",
            "Gameplay:=1",
            "Gameplay:A=1\r",
            "A\n:B=1",
        ] {
            let err = sections(LF, spec).unwrap_err();
            assert!(
                matches!(err, SettingsError::BadItem(_)),
                "{spec:?}: {err:?}"
            );
        }
    }

    #[test]
    fn empty_specs_change_nothing() {
        assert_eq!(flat(LF, "").unwrap(), LF);
        assert_eq!(
            flat(LF, ",AllowFG=0,").unwrap(),
            LF.replace("AllowFG=true", "AllowFG=0")
        );
        assert_eq!(sections(LF, ";;").unwrap(), LF);
        assert_eq!(validate(LF.as_bytes(), "", "").unwrap(), LF.as_bytes());
    }

    #[test]
    fn validate_runs_flat_then_sections() {
        let data = crlf(LF);
        let out = validate(
            data.as_bytes(),
            "Threads=8,AllowFG=false",
            "Rendering/MtRender:Threads=9;Gameplay:Hdr=1",
        );
        let want = LF
            .replace("Threads=4", "Threads=9")
            .replace("AllowFG=true", "AllowFG=false")
            .replace("[Gameplay]\n", "[Gameplay]\nHdr=1\n");
        assert_eq!(out.unwrap(), crlf(&want).into_bytes());
        // Either half refuses the whole override.
        let err = validate(data.as_bytes(), "AllowFG=false", "Nope:A=1").unwrap_err();
        assert_eq!(
            err,
            SettingsError::SectionCount {
                section: "Nope".into(),
                found: 0
            }
        );
        let err = validate(data.as_bytes(), "Nope=1", "Gameplay:Hdr=1").unwrap_err();
        assert_eq!(
            err,
            SettingsError::KeyCount {
                key: "Nope".into(),
                found: 0
            }
        );
    }
}
