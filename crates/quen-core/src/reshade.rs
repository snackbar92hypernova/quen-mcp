//! Checked edits of a ReShade-style ini (ReShade.ini, its RenoDX add-on sections), byte-identical to the harness's
//! `ini_writer.set_values`: only the named keys' lines of one `[section]` change, each keeping its own line ending, and a
//! missing key (or section) is refused, skipped or appended, as asked. Values are written as RenoDX and ReShade write
//! them.

/// A value to write: a number (formatted like `std::to_chars(float)`) or a one-line string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    Number(f64),
    Text(&'a str),
}

/// ReShade's line trim: around a line, before it decides what the line is.
const WS: &[char] = &[' ', '\t', '\r', '\n'];

/// The shortest text that reads back as the same `f32`, integers without `.0`: `100`, `0.8`, `1e-05`.
pub fn number(v: f64) -> Result<String, String> {
    if !v.is_finite() {
        return Err(format!("non-finite value {v}"));
    }
    if v.fract() == 0.0 && v.abs() < (1 << 24) as f64 {
        return Ok(format!("{}", v as i64));
    }
    let f32 = v as f32;
    for p in 1..10 {
        let s = general(v, p);
        if s.parse::<f64>().is_ok_and(|back| back as f32 == f32) {
            return Ok(s);
        }
    }
    Ok(format!("{f32:?}"))
}

/// Python's `f"{v:.{p}g}"` for a finite `v` and `p >= 1`: `p` significant digits, fixed while the exponent is in
/// `-4..p`, else `d.ddde-XX`; trailing zeros and a bare point dropped.
fn general(v: f64, p: usize) -> String {
    let sci = format!("{:.*e}", p - 1, v);
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("{:e}'s exponent is a number");
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if (-4..p as i32).contains(&exp) {
        trim(&format!("{:.*}", (p as i32 - 1 - exp) as usize, v))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    }
}

fn written(value: Value) -> Result<String, String> {
    match value {
        Value::Number(v) => number(v),
        Value::Text(s) if s.contains(['\r', '\n', '\0']) || s != s.trim_matches([' ', '\t']) => {
            Err(format!("string value {s:?}: one line, no edge spaces"))
        }
        Value::Text(s) => Ok(s.into()),
    }
}

/// What a line is to ReShade: `None` for a blank or comment line (`;`, `/`, `#`), a section header's name, or a key.
#[derive(PartialEq)]
enum Kind<'a> {
    Section(&'a str),
    Key(&'a str),
}

fn kind(line: &str) -> Option<Kind<'_>> {
    let s = line.trim_matches(WS);
    match s.chars().next()? {
        ';' | '/' | '#' => None,
        '[' => {
            let name = s.find(']').map_or(s, |at| &s[..at]);
            Some(Kind::Section(name.trim_matches([' ', '\t', '[', ']'])))
        }
        _ => Some(Kind::Key(
            s.split_once('=')
                .map_or(s, |(k, _)| k)
                .trim_matches([' ', '\t']),
        )),
    }
}

/// Python's `str.splitlines(keepends=True)`: each line with its own break.
fn split_lines(text: &str) -> Vec<&str> {
    let breaks = [
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !breaks.contains(&c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
            chars.next();
            end += 1;
        }
        out.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// What [`set_values`] does with a key its `[section]` doesn't have.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Missing {
    /// Refuses, as it does a missing section.
    Refuse,
    /// Appends the key after the section's last non-blank line.
    Add,
    /// The same, a missing section first appended at the end of the file.
    AddSection,
    /// Leaves the key out; a missing section leaves the file as it is.
    Skip,
}

/// Sets `values` (in order) in the one `[section]` of `data`. A section appearing twice, or a key starting two lines
/// there, is refused; a missing one is handled as `missing` says. A UTF-8 BOM and every other byte stay.
pub fn set_values(
    data: &[u8],
    section: &str,
    values: &[(&str, Value)],
    missing: Missing,
) -> Result<Vec<u8>, String> {
    if values.is_empty() {
        return Err("no values".into());
    }
    let bom: &[u8] = if data.starts_with(b"\xef\xbb\xbf") {
        b"\xef\xbb\xbf"
    } else {
        b""
    };
    let text = std::str::from_utf8(&data[bom.len()..]).map_err(|e| format!("not UTF-8: {e}"))?;
    let lines = split_lines(text);
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let heads: Vec<usize> = (0..lines.len())
        .filter(|&i| kind(lines[i]) == Some(Kind::Section(section)))
        .collect();
    let head = match heads[..] {
        [head] => head,
        [] if missing == Missing::Skip => return Ok(data.to_vec()),
        [] if missing == Missing::AddSection => {
            let mut out = data.to_vec();
            if !text.is_empty() && !text.ends_with(['\n', '\r']) {
                out.extend(nl.as_bytes());
            }
            out.extend(format!("[{section}]{nl}").bytes());
            // A name that doesn't read back as `[section]` is refused here: found 0 times.
            return set_values(&out, section, values, Missing::Add);
        }
        _ => return Err(format!("[{section}] found {} times", heads.len())),
    };
    let start = head + 1;
    let end = (start..lines.len())
        .find(|&i| matches!(kind(lines[i]), Some(Kind::Section(_))))
        .unwrap_or(lines.len());
    let mut new: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    let mut inserts = Vec::new();
    for &(key, value) in values {
        if key.is_empty()
            || key != key.trim_matches(WS)
            || key.contains(['=', '[', ']', '\r', '\n', ';', '#', '/'])
        {
            return Err(format!("bad key {key:?}"));
        }
        let value = written(value)?;
        let hits: Vec<usize> = (start..end)
            .filter(|&i| kind(lines[i]) == Some(Kind::Key(key)))
            .collect();
        match hits[..] {
            [i] => {
                let body = lines[i].trim_end_matches(['\r', '\n']);
                new[i] = format!("{key}={value}{}", &lines[i][body.len()..]);
            }
            [] => match missing {
                Missing::Refuse => return Err(format!("{key} not in [{section}]")),
                Missing::Add | Missing::AddSection => inserts.push(format!("{key}={value}")),
                Missing::Skip => {}
            },
            _ => return Err(format!("{key} appears {} times in [{section}]", hits.len())),
        }
    }
    if !inserts.is_empty() {
        let last = (start..end)
            .rev()
            .find(|&i| !lines[i].trim_matches(WS).is_empty());
        let at = last.unwrap_or(head) + 1;
        if !new[at - 1].ends_with(['\n', '\r']) {
            new[at - 1].push_str(nl);
        }
        new.splice(at..at, inserts.into_iter().map(|line| line + nl));
    }
    let mut out = bom.to_vec();
    out.extend(new.concat().into_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_the_shortest_f32_text() {
        let cases = [
            (100.0, "100"),
            (0.8, "0.8"),
            (0.75, "0.75"),
            (1e-5, "1e-05"),
            (-2.0, "-2"),
            (0.30000001192092896, "0.3"),
            (-0.0, "0"),
            (16777216.0, "16777216"),
            (1.5e10, "1.5e+10"),
            (123456.7, "123456.7"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (1.23456789, "1.2345679"),
            (1e-7, "1e-07"),
            (1234567.89, "1234567.9"),
            (99999.99, "99999.99"),
            (1e20, "1e+20"),
            (-0.000123456, "-0.000123456"),
        ];
        // The expected texts are what ini_writer.fmt printed for these values.
        for (v, want) in cases {
            assert_eq!(number(v).unwrap(), want, "{v}");
        }
        assert!(number(f64::NAN).is_err() && number(f64::INFINITY).is_err());
        assert!(written(Value::Text("a\nb")).is_err() && written(Value::Text(" x")).is_err());
        assert_eq!(written(Value::Text("")).unwrap(), "");
    }

    const INI: &str = "\u{feff}[ADDON]\r\nLoadFromDllMain=\r\n\r\n[renodx]\r\n ; note\r\nSettingsMode=1\r\n\r\n\
                       [renodx-preset1]\r\nToneMapGameNits=203\r\n  Black = 0 \r\n\r\n[OTHER]\r\nToneMapGameNits=1\r\n";

    fn set(section: &str, values: &[(&str, Value)], missing: Missing) -> Result<String, String> {
        set_values(INI.as_bytes(), section, values, missing).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn edits_only_the_named_lines_of_one_section() {
        let out = set(
            "renodx-preset1",
            &[("ToneMapGameNits", Value::Number(250.0))],
            Missing::Refuse,
        )
        .unwrap();
        assert_eq!(
            out,
            INI.replace("ToneMapGameNits=203", "ToneMapGameNits=250")
        );
        // A key line with spaces around `=` is rewritten as Key=value, its line ending kept.
        let out = set(
            "renodx-preset1",
            &[("Black", Value::Number(0.5))],
            Missing::Refuse,
        )
        .unwrap();
        assert_eq!(out, INI.replace("  Black = 0 \r\n", "Black=0.5\r\n"));
        let path = r"devkit\renodx-devkit.addon64";
        let out = set(
            "ADDON",
            &[("LoadFromDllMain", Value::Text(path))],
            Missing::Refuse,
        )
        .unwrap();
        assert_eq!(
            out,
            INI.replace(
                "LoadFromDllMain=\r\n",
                &format!("LoadFromDllMain={path}\r\n")
            )
        );
    }

    #[test]
    fn a_missing_key_is_appended_after_the_last_non_blank_line() {
        let out = set(
            "renodx",
            &[("DevAutoReload", Value::Number(1.0))],
            Missing::Add,
        )
        .unwrap();
        assert_eq!(
            out,
            INI.replace(
                "SettingsMode=1\r\n",
                "SettingsMode=1\r\nDevAutoReload=1\r\n"
            )
        );
        // At the end of a file without a final line break, the break is added first.
        let data = "[A]\nX=1";
        let out = set_values(
            data.as_bytes(),
            "A",
            &[("Y", Value::Number(2.0))],
            Missing::Add,
        )
        .unwrap();
        assert_eq!(out, b"[A]\nX=1\nY=2\n");
        let out = set_values(b"[A]\n", "A", &[("Y", Value::Number(2.0))], Missing::Add).unwrap();
        assert_eq!(out, b"[A]\nY=2\n");
    }

    #[test]
    fn refuses_on_any_doubt() {
        assert!(
            set(
                "renodx",
                &[("DevAutoReload", Value::Number(1.0))],
                Missing::Refuse
            )
            .is_err()
        );
        assert!(set("nope", &[("X", Value::Number(1.0))], Missing::Add).is_err());
        assert!(set("renodx", &[("A=B", Value::Number(1.0))], Missing::Add).is_err());
        assert!(set("renodx", &[], Missing::Add).is_err());
        let twice = "[A]\nX=1\nX=2\n";
        assert!(
            set_values(
                twice.as_bytes(),
                "A",
                &[("X", Value::Number(3.0))],
                Missing::Refuse
            )
            .is_err()
        );
        let heads = "[A]\nX=1\n[A]\n";
        for missing in [
            Missing::Refuse,
            Missing::Add,
            Missing::AddSection,
            Missing::Skip,
        ] {
            let err = set_values(heads.as_bytes(), "A", &[("X", Value::Number(3.0))], missing);
            assert_eq!(err.unwrap_err(), "[A] found 2 times", "{missing:?}");
        }
    }

    /// Live 2026-10-22: a ReShade.ini as ReShade writes it fresh has no [ADDON].
    #[test]
    fn a_missing_section_or_key_is_skipped_or_added() {
        const FRESH: &str = "\u{feff}[GENERAL]\r\nPresetPath=.\\ReShadePreset.ini\r\n";
        let key = [("LoadFromDllMain", Value::Text(r"devkit\x.addon64"))];
        let empty = [("LoadFromDllMain", Value::Text(""))];
        let set = |data: &str, values: &[(&str, Value)], missing| {
            set_values(data.as_bytes(), "ADDON", values, missing)
                .map(|b| String::from_utf8(b).unwrap())
        };
        let no_key = "\u{feff}[ADDON]\r\nOther=1\r\n\r\n[GENERAL]\r\n";
        for data in [FRESH, no_key] {
            assert_eq!(set(data, &empty, Missing::Skip).unwrap(), data);
        }
        // The BOM and CRLF kept; the section at the end, the key after the section's last non-blank line.
        assert_eq!(
            set(FRESH, &key, Missing::AddSection).unwrap(),
            format!("{FRESH}[ADDON]\r\nLoadFromDllMain=devkit\\x.addon64\r\n")
        );
        assert_eq!(
            set(no_key, &key, Missing::AddSection).unwrap(),
            no_key.replace(
                "Other=1\r\n",
                "Other=1\r\nLoadFromDllMain=devkit\\x.addon64\r\n"
            )
        );
        assert_eq!(
            set("X=1", &key, Missing::AddSection).unwrap(),
            "X=1\n[ADDON]\nLoadFromDllMain=devkit\\x.addon64\n"
        );
        assert_eq!(
            set("", &key, Missing::AddSection).unwrap(),
            "[ADDON]\nLoadFromDllMain=devkit\\x.addon64\n"
        );
        assert!(set(FRESH, &key, Missing::Add).is_err());
        // A section name that doesn't read back as itself is refused.
        let odd = set_values(FRESH.as_bytes(), "A]B", &key, Missing::AddSection);
        assert_eq!(odd.unwrap_err(), "[A]B] found 0 times");
    }

    #[test]
    fn lines_split_as_python_splits_them() {
        assert_eq!(split_lines("a\r\nb\rc\nd"), ["a\r\n", "b\r", "c\n", "d"]);
        assert_eq!(split_lines("a\u{2028}b\u{c}"), ["a\u{2028}", "b\u{c}"]);
        assert!(split_lines("").is_empty());
    }
}
