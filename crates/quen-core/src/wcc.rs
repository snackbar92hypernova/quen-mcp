//! REDkit's `wcc_lite compilescripts`, the platform-neutral part: the script errors and warnings in its log, the stubs
//! that replace the `import function`s it can't bind (it has no Native Bridge to bind a mod's natives), and the
//! journal's changes that put back what it writes in the game's documents folder.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::boot::Change;

/// What wcc_lite rewrites in the game's documents folder on every run (checked 2026-10-20: these four, the same bytes
/// with a new write time, nothing else there and no registry value).
pub const WRITES: [&str; 4] = [
    "editoruser.settings",
    "editoruser.settings.bak",
    "profile.settings",
    "profile.settings.bak",
];

/// One script message: the file as wcc_lite names it (relative to the compiled tree), its line, the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub file: String,
    pub line: u32,
    pub msg: String,
}

/// The script errors and warnings in a wcc_lite log: lines with `[Script]: Error <file>[<line>]: <text>` or
/// `[Script]: Warning ...`.
pub fn messages(log: &str) -> (Vec<Message>, Vec<Message>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    for line in log.lines() {
        let Some((_, rest)) = line.split_once("[Script]: ") else {
            continue;
        };
        let (list, rest) = match rest.split_once(' ') {
            Some(("Error", rest)) => (&mut errors, rest),
            Some(("Warning", rest)) => (&mut warnings, rest),
            _ => continue,
        };
        let parsed = rest.split_once("]: ").and_then(|(at, msg)| {
            let (file, line) = at.rsplit_once('[')?;
            Some(Message {
                file: file.into(),
                line: line.parse().ok()?,
                msg: msg.into(),
            })
        });
        list.push(parsed.unwrap_or_else(|| Message {
            file: String::new(),
            line: 0,
            msg: rest.into(),
        }));
    }
    (errors, warnings)
}

/// The imports wcc_lite couldn't bind, from its errors `Cannot find global function '<name>' to import.`
pub fn unbound(errors: &[Message]) -> BTreeSet<String> {
    errors
        .iter()
        .filter_map(|e| {
            e.msg
                .strip_prefix("Cannot find global function '")?
                .strip_suffix("' to import.")
        })
        .map(Into::into)
        .collect()
}

/// `text` with each `import function Name(params) [: type];` whose name is one of `names` replaced by a plain function
/// returning that type's empty value, as the offline compile in w3/README.md does. Anything else is left as it is.
pub fn stub_imports(text: &str, names: &BTreeSet<String>) -> String {
    const IMPORT: &str = "import function ";
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(at) = rest.find(IMPORT) {
        out.push_str(&rest[..at]);
        let after = &rest[at + IMPORT.len()..];
        match stub(after, names) {
            Some((stub, used)) => {
                out.push_str(&stub);
                rest = &after[used..];
            }
            None => {
                out.push_str(IMPORT);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The names a script file imports by `import function <name>`, each once in order, outside `//` and `/* */` comments:
/// the functions a binary registers for the script (the game's exe, or an ASI through the Native Bridge). The file is
/// UTF-16LE with its BOM, as some are, else UTF-8.
pub fn imports(script: &[u8]) -> Vec<String> {
    let text = match script.strip_prefix(&[0xFF, 0xFE]) {
        Some(utf16) => {
            let units: Vec<u16> = utf16
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        None => String::from_utf8_lossy(script).into_owned(),
    };
    let (mut code, mut rest) = (String::with_capacity(text.len()), text.as_str());
    while let Some(at) = [rest.find("//"), rest.find("/*")]
        .into_iter()
        .flatten()
        .min()
    {
        code.push_str(&rest[..at]);
        code.push(' ');
        let end = match &rest[at..at + 2] {
            "//" => rest[at..].find('\n').map(|n| at + n),
            _ => rest[at + 2..].find("*/").map(|n| at + 2 + n + 2),
        };
        rest = end.map_or("", |end| &rest[end..]);
    }
    code.push_str(rest);
    let words: Vec<&str> = code.split_whitespace().collect();
    let mut names: Vec<String> = Vec::new();
    for w in words
        .windows(3)
        .filter(|w| w[..2] == ["import", "function"])
    {
        let end = w[2]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(w[2].len());
        let name = &w[2][..end];
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// `Name(params) [: type] ;` at `s`'s start as a stub when `names` holds Name, and the bytes it took.
fn stub(s: &str, names: &BTreeSet<String>) -> Option<(String, usize)> {
    let word = |s: &str| {
        s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(s.len())
    };
    let name = &s[..word(s)];
    if !names.contains(name) {
        return None;
    }
    let rest = &s[name.len()..];
    let params = &rest[..rest
        .find(')')
        .filter(|_| rest.starts_with('(') && !name.is_empty())?
        + 1];
    if params[1..].contains('(') {
        return None;
    }
    let mut at = name.len() + params.len();
    let skip = |at: usize| at + s[at..].len() - s[at..].trim_start().len();
    at = skip(at);
    let mut ty = "";
    if let Some(after) = s[at..].strip_prefix(':') {
        let t = after.trim_start();
        ty = &t[..word(t)];
        if ty.is_empty() {
            return None;
        }
        at = skip(s.len() - t.len() + ty.len());
    }
    s[at..].starts_with(';').then_some(())?;
    let ret = match ty {
        "string" => " return \"\"; ",
        "int" | "float" => " return 0; ",
        "name" => " return ''; ",
        "bool" => " return false; ",
        _ => " ",
    };
    let ty = if ty.is_empty() {
        String::new()
    } else {
        format!(" : {ty}")
    };
    Some((format!("function {name}{params}{ty} {{{ret}}}"), at + 1))
}

/// The journal's changes around a wcc_lite run in the documents folder `docs`, whose top-level files are `listing`
/// (name, sha256): each one named `*.settings` or `*.settings.bak` backed up into `backups` and put back after (bytes
/// and write time), and each of [`WRITES`] that isn't there yet recycled after.
pub fn settings_changes(docs: &Path, listing: &[(String, String)], backups: &Path) -> Vec<Change> {
    let settings = |name: &str| {
        let name = name.to_lowercase();
        name.ends_with(".settings") || name.ends_with(".settings.bak")
    };
    let mut changes: Vec<Change> = listing
        .iter()
        .filter(|(name, _)| settings(name))
        .map(|(name, sha)| Change::Rebuilt {
            path: docs.join(name),
            sha: sha.clone(),
            backup: backups.join(name),
            keep: false,
        })
        .collect();
    for name in WRITES {
        if !listing.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            changes.push(Change::NewLog {
                path: docs.join(name),
            });
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of a real run's log (2026-10-20), paths aside.
    #[test]
    fn errors_and_warnings_are_parsed_with_file_and_line() {
        let log = "[2026.10.20 00:55:20][Error][Core] [Script]: Error imports.ws[1]: Cannot find global function \
                   'WNB_GetApiVersion' to import.\n\
                   [2026.10.20 00:55:20][Error][Core] [Script]: Error local\\quen.ws[49]: Cannot find global function \
                   'QUEN_Next' to import.\n\
                   [2026.10.19 23:22:51][Warning][Core] [Script]: Warning game\\cameras\\driftcamera.ws[6]: Class X is \
                   not marked as abstract, but has missing functions!\n\
                   [2026.10.19 23:22:51][Info][Core] [Script]: Compiling functions...\n\
                   [2026.10.19 23:22:51][Error][Core] [Script]: Error something unplaced\n";
        let (errors, warnings) = messages(log);
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[1].file, "local\\quen.ws");
        assert_eq!(errors[1].line, 49);
        assert!(
            errors[1]
                .msg
                .starts_with("Cannot find global function 'QUEN_Next'")
        );
        assert_eq!(errors[2].line, 0);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].line, 6);
    }

    /// Each import named becomes a plain function returning its type's empty value; other text, imports not named
    /// among it, stays byte for byte.
    #[test]
    fn named_imports_become_stubs() {
        let text = "import function QUEN_Next() : string;\n\
                    import function QUEN_State(flags : int);\r\n\
                    import function WNB_Flag( on : bool )  :  bool ;\n\
                    import function SetCommunityRadius(r : float);\n\
                    // import function broken(;\n\
                    function Keep() {}\n";
        let names = ["QUEN_Next", "QUEN_State", "WNB_Flag", "broken"]
            .map(String::from)
            .into();
        assert_eq!(
            stub_imports(text, &names),
            "function QUEN_Next() : string { return \"\"; }\n\
             function QUEN_State(flags : int) { }\r\n\
             function WNB_Flag( on : bool ) : bool { return false; }\n\
             import function SetCommunityRadius(r : float);\n\
             // import function broken(;\n\
             function Keep() {}\n"
        );
        assert_eq!(stub_imports(text, &BTreeSet::new()), text);
    }

    /// The names come from wcc_lite's binding errors only.
    #[test]
    fn unbound_imports_are_named_by_the_errors() {
        let log = "[Script]: Error local\\quen.ws[49]: Cannot find global function 'QUEN_Next' to import.\n\
                   [Script]: Error imports.ws[1]: Cannot find global function 'WNB_GetApiVersion' to import.\n\
                   [Script]: Error game\\x.ws[2]: Unknown function 'Nope'.\n";
        let names: Vec<String> = unbound(&messages(log).0).into_iter().collect();
        assert_eq!(names, ["QUEN_Next", "WNB_GetApiVersion"]);
    }

    /// The names a script imports, each once, whatever the spacing; commented-out imports aren't.
    #[test]
    fn imports_are_named_outside_comments() {
        let text = "import function QUEN_Next() : string;\n\
                    import  function\tBIW_Edge (x : int);\r\n\
                    // import function Old_One();\n\
                    /* import function Old_Two();\n import function Old_Three(); */\n\
                    function f() { x = 1; } // trailing\n\
                    import function QUEN_Next() : string;\n\
                    import latent function NotThisForm();\n";
        assert_eq!(imports(text.as_bytes()), ["QUEN_Next", "BIW_Edge"]);
        assert!(imports(b"function f() {} /* unclosed import function X();").is_empty());
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(imports(&utf16), ["QUEN_Next", "BIW_Edge"]);
    }

    /// Every settings file is put back after, and a file wcc_lite writes that isn't there yet is recycled after.
    #[test]
    fn the_settings_are_put_back() {
        let docs = Path::new("DOCS");
        let listing: Vec<(String, String)> = [
            "dx12user.settings",
            "profile.settings",
            "gamesaves.txt",
            "x.settings.bak",
        ]
        .iter()
        .map(|n| (n.to_string(), format!("sha-{n}")))
        .collect();
        let changes = settings_changes(docs, &listing, Path::new("B"));
        let rebuilt: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Rebuilt {
                    path, keep: false, ..
                } => Some(path.display().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(rebuilt.len(), 3, "{rebuilt:?}");
        let new: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                Change::NewLog { path } => Some(path.file_name()?.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(
            new,
            [
                "editoruser.settings",
                "editoruser.settings.bak",
                "profile.settings.bak"
            ]
        );
    }
}
