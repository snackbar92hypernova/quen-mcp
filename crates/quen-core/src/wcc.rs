//! REDkit's `wcc_lite compilescripts`, the platform-neutral part: the script errors and warnings in its log, the stubs
//! that replace a mod's `import function`s (wcc_lite has no Native Bridge to bind them), and the journal's changes
//! that put back what it writes in the game's documents folder.

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

/// `text` with each `import function Name(params) [: type];` replaced by a plain function returning that type's empty
/// value, as the offline compile in w3/README.md does; and how many it replaced. Anything else is left as it is.
pub fn stub_imports(text: &str) -> (String, usize) {
    const IMPORT: &str = "import function ";
    let (mut out, mut rest, mut n) = (String::with_capacity(text.len()), text, 0);
    while let Some(at) = rest.find(IMPORT) {
        out.push_str(&rest[..at]);
        let after = &rest[at + IMPORT.len()..];
        match stub(after) {
            Some((stub, used)) => {
                out.push_str(&stub);
                rest = &after[used..];
                n += 1;
            }
            None => {
                out.push_str(IMPORT);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, n)
}

/// `Name(params) [: type] ;` at `s`'s start as a stub, and the bytes it took.
fn stub(s: &str) -> Option<(String, usize)> {
    let word = |s: &str| {
        s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(s.len())
    };
    let name = &s[..word(s)];
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

    /// Each import becomes a plain function returning its type's empty value; other text stays byte for byte.
    #[test]
    fn imports_become_stubs() {
        let text = "import function QUEN_Next() : string;\n\
                    import function QUEN_State(flags : int);\r\n\
                    import function WNB_Flag( on : bool )  :  bool ;\n\
                    // import function broken(;\n\
                    function Keep() {}\n";
        let (out, n) = stub_imports(text);
        assert_eq!(n, 3);
        assert_eq!(
            out,
            "function QUEN_Next() : string { return \"\"; }\n\
             function QUEN_State(flags : int) { }\r\n\
             function WNB_Flag( on : bool ) : bool { return false; }\n\
             // import function broken(;\n\
             function Keep() {}\n"
        );
        assert_eq!(stub_imports("no imports").0, "no imports");
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
