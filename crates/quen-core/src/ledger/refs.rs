//! Doc references to captures and the cleanup rule (docs/intent.md, "Captures"). Finding the documents to scan
//! (`.md`, `.py`, `.sh`, `.ps1`, `.json`, `.toml` under the configured roots) is the caller's job; this module reads
//! their text.

use rusqlite::params;
use serde::Serialize;

use super::{Error, Ledger, Result};

/// One reference to the capture root in a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ref {
    /// 1-based.
    pub line: usize,
    /// The reference as written.
    pub raw: String,
    /// Indices into the scanner's known paths; empty means dangling.
    pub matches: Vec<usize>,
}

/// Finds references to captures in text, against the known capture paths (relative to the capture root).
///
/// A reference is a path token naming something under the root: `cap/…` or `cap\…` (`cap` being the root's last
/// folder), an absolute path under the root (`D:\…\cap\…`, `D:/…`, Git Bash `/d/…`), or a relative path that ends
/// like the root (`witcher3\cap\…`, and after `..`, `%VAR%`, `$VAR`, `$env:VAR` or `~`: `%USERPROFILE%\…\cap\…`). Case and
/// separator style don't matter. What follows the root names a file, or a folder whose files it all matches (with or
/// without a trailing separator), and may use globs (`*`, within one segment) and brace lists (`{a,b}`, nestable; a
/// group without a comma stays literal, as in a shell). The root on its own is not a reference.
pub struct Scanner {
    /// The root's parent folder, normalized, ending in `/` (empty for a one-folder relative root).
    parent: String,
    /// The root's last folder, normalized, followed by `/`.
    name: String,
    known: Vec<String>,
}

impl Scanner {
    /// Refuses a root without a last folder name: it would find nothing, and a rescan would empty every document.
    pub fn new(capture_root: &str, known: &[&str]) -> Result<Scanner> {
        let root = norm(capture_root);
        let root = root.trim_end_matches('/');
        let (parent, name) = root
            .rfind('/')
            .map_or(("", root), |i| (&root[..=i], &root[i + 1..]));
        if name.is_empty() || name == "." || name == ".." || name.contains(':') {
            return Err(Error::Refused(format!(
                "capture root {capture_root:?} doesn't end in a folder name"
            )));
        }
        Ok(Scanner {
            parent: parent.into(),
            name: format!("{name}/"),
            known: known.iter().map(|k| norm(k)).collect(),
        })
    }

    /// The references in `text`, in order; the same reference twice on one line counts once.
    pub fn refs(&self, text: &str) -> Vec<Ref> {
        let mut out: Vec<Ref> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line_no = n + 1;
            for token in tokens(line) {
                let Some(tail) = self.tail(token) else {
                    continue;
                };
                if out
                    .iter()
                    .rev()
                    .take_while(|r| r.line == line_no)
                    .any(|r| r.raw == token)
                {
                    continue;
                }
                let alternatives = expand(&tail);
                let matches = (0..self.known.len())
                    .filter(|&k| alternatives.iter().any(|a| names(a, &self.known[k])))
                    .collect();
                out.push(Ref {
                    line: line_no,
                    raw: token.into(),
                    matches,
                });
            }
        }
        out
    }

    /// What a token names under the capture root (normalized), or None when it isn't a reference to it.
    fn tail(&self, token: &str) -> Option<String> {
        let t = norm(token);
        let mut from = 0;
        while let Some(at) = t[from..].find(&self.name).map(|i| i + from) {
            let before = &t[..at];
            // `before` must end at a separator and, after leading `..`, %VAR%, $VAR or ~ segments (folders only the
            // reader knows), be a whole-segment ending of the root's parent: empty (`cap/…`), all of it (an absolute
            // path) or its last folders.
            let segments: Vec<&str> = before.split_inclusive('/').collect();
            let unknown = segments
                .iter()
                .take_while(|s| **s == "../" || s.starts_with(['%', '$', '~']))
                .count();
            let known = segments[unknown..].concat();
            let under_root = (before.is_empty() || before.ends_with('/'))
                && self.parent.strip_suffix(&known).is_some_and(|rest| {
                    rest.is_empty() || rest.ends_with('/') || known.starts_with('/')
                });
            if under_root {
                let tail = &t[at + self.name.len()..];
                return (!tail.is_empty()).then(|| tail.into());
            }
            from = at + self.name.len(); // `name/` can't overlap itself
        }
        None
    }
}

/// Lowercase, `/` separators, no doubled `/` or leading `./`, and Git Bash's `/d/…` as `d:/…`.
fn norm(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        let c = if c == '\\' {
            '/'
        } else {
            c.to_ascii_lowercase()
        };
        if c != '/' || !out.ends_with('/') {
            out.push(c);
        }
    }
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.into();
    }
    let b = out.as_bytes();
    if b.len() > 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        out = format!("{}:{}", b[1] as char, &out[2..]);
    }
    out
}

/// The path-like runs of a line. They end at whitespace, quotes, punctuation that isn't part of a path, a comma outside
/// braces and a colon other than a drive letter's; surrounding Markdown `*` emphasis and a sentence's final dots go.
fn tokens<'a>(line: &'a str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut push = |token: &'a str| {
        let stars = token.len() - token.trim_start_matches('*').len();
        let mut t = token[stars..].trim_end_matches('.');
        for _ in 0..stars {
            t = t.strip_suffix('*').unwrap_or(t);
        }
        let t = t.trim_end_matches('.');
        if !t.is_empty() {
            out.push(t);
        }
    };
    let (mut start, mut depth) = (0, 0u32);
    for (i, c) in line.char_indices() {
        let stop = match c {
            '{' => {
                depth += 1;
                false
            }
            '}' => {
                depth = depth.saturating_sub(1);
                false
            }
            ',' => depth == 0,
            // A drive letter's colon, or PowerShell's `$env:NAME`, stays in the token.
            ':' => {
                !(i == start + 1 && line.as_bytes()[start].is_ascii_alphabetic()
                    || line[start..i].eq_ignore_ascii_case("$env"))
            }
            c => c.is_whitespace() || "\"'`()[]<>|;!?=&^#@".contains(c),
        };
        if stop {
            push(&line[start..i]);
            (start, depth) = (i + c.len_utf8(), 0);
        }
    }
    push(&line[start..]);
    out
}

/// Brace expansion: `a{b,c}d` gives `abd` and `acd`; nested groups expand too, and a group without a top-level comma
/// or without its closing brace stays as written.
fn expand(s: &str) -> Vec<String> {
    let mut from = 0;
    while let Some(open) = s[from..].find('{').map(|i| i + from) {
        let mut depth = 0;
        let mut cuts = vec![open];
        let mut close = None;
        for (i, c) in s.bytes().enumerate().skip(open) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                b',' if depth == 1 => cuts.push(i),
                _ => {}
            }
        }
        let Some(close) = close else { break };
        if cuts.len() == 1 {
            from = open + 1;
            continue;
        }
        cuts.push(close);
        let (head, rest) = (&s[..open], &s[close + 1..]);
        return cuts
            .windows(2)
            .flat_map(|w| expand(&format!("{head}{}{rest}", &s[w[0] + 1..w[1]])))
            .collect();
    }
    vec![s.into()]
}

/// Whether reference `a` names `path`: the file itself, or a folder holding it (always when `a` ends in `/`).
fn names(a: &str, path: &str) -> bool {
    let (pattern, folder) = a.strip_suffix('/').map_or((a, false), |p| (p, true));
    (!folder && glob(pattern, path))
        || path
            .match_indices('/')
            .any(|(i, _)| glob(pattern, &path[..i]))
}

/// `*` matches any run of characters within one path segment; anything else matches itself.
pub(crate) fn glob(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    pattern.len() == path.split('/').count()
        && pattern.iter().zip(path.split('/')).all(|(p, s)| {
            // Greedy, backing up only to the last `*`: never exponential, however many stars.
            let (p, s) = (p.as_bytes(), s.as_bytes());
            let (mut pi, mut si, mut star) = (0, 0, None);
            while si < s.len() {
                if p.get(pi) == Some(&b'*') {
                    (star, pi) = (Some((pi, si)), pi + 1);
                } else if p.get(pi) == Some(&s[si]) {
                    (pi, si) = (pi + 1, si + 1);
                } else if let Some((sp, ss)) = star {
                    (star, pi, si) = (Some((sp, ss + 1)), sp + 1, ss + 1);
                } else {
                    return false;
                }
            }
            p[pi..].iter().all(|&c| c == b'*')
        })
}

/// What [`Ledger::scan_docs`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScanSummary {
    pub docs: usize,
    pub refs: usize,
    pub dangling: usize,
}

/// A capture the user may send to the Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub capture_id: i64,
    pub path: String,
    pub bytes: i64,
    pub t: String,
    pub boot_id: i64,
}

/// A capture row whose file is still on disk, as [`Ledger::capture_rows`] gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptureRow {
    pub id: i64,
    /// Relative to the capture root, as the row has it (`/` or `\`).
    pub path: String,
    pub boot_id: Option<i64>,
    /// Kept by a cleanup unless named: pinned, a claim's evidence, a pose's reference image, or in a document.
    pub cited: bool,
}

impl Ledger {
    /// Scans documents, given as (doc path, text), for references to the ledger's captures under `capture_root`, and
    /// replaces each document's doc_refs rows with one row per matched capture, or one dangling row for a reference
    /// that matches nothing. All documents are written in one transaction.
    pub fn scan_docs(
        &mut self,
        docs: &[(&str, &str)],
        capture_root: &str,
        t_scanned: &str,
    ) -> Result<ScanSummary> {
        let captures = self
            .conn
            .prepare("SELECT id, path FROM captures ORDER BY id")?
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let paths: Vec<&str> = captures.iter().map(|(_, p)| p.as_str()).collect();
        let scanner = Scanner::new(capture_root, &paths)?;
        // Checked here because a document without references writes no row the schema could check.
        self.check_time(t_scanned)?;
        let mut summary = ScanSummary {
            docs: docs.len(),
            refs: 0,
            dangling: 0,
        };
        self.atomic(|l| {
            {
                let mut insert = l.conn.prepare(
                    "INSERT INTO doc_refs (doc_path, line, raw, capture_id, dangling, t_scanned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for (doc, text) in docs {
                    l.conn
                        .execute("DELETE FROM doc_refs WHERE doc_path = ?1", [doc])?;
                    for r in scanner.refs(text) {
                        summary.refs += 1;
                        let ids: Vec<Option<i64>> = if r.matches.is_empty() {
                            summary.dangling += 1;
                            vec![None]
                        } else {
                            r.matches.iter().map(|&k| Some(captures[k].0)).collect()
                        };
                        for id in ids {
                            insert.execute(params![
                                doc,
                                r.line as i64,
                                r.raw,
                                id,
                                id.is_none(),
                                t_scanned
                            ])?;
                        }
                    }
                }
            }
            Ok(summary)
        })
    }

    /// Lists cleanup candidates, oldest first: captures no document references, not pinned, taken more than
    /// `older_than_days` before `now`, from boots the user has reviewed since they were taken, and not under any
    /// `hard_pinned` prefix (relative to the capture root, e.g. backups, templates and references; case and separators
    /// don't matter).
    /// It only lists: nothing in quen deletes a capture.
    pub fn cleanup_candidates(
        &self,
        now: &str,
        older_than_days: u32,
        hard_pinned: &[&str],
    ) -> Result<Vec<Candidate>> {
        self.check_time(now)?;
        let pins: Vec<String> = hard_pinned.iter().map(|p| norm(p)).collect();
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.path, c.bytes, c.t, c.boot_id FROM captures c JOIN boots b ON b.id = c.boot_id
             WHERE b.reviewed_at IS NOT NULL AND c.t <= b.reviewed_at AND c.pinned = 0
               AND julianday(c.t) < julianday(?1) - ?2
               AND NOT EXISTS (SELECT 1 FROM doc_refs r WHERE r.capture_id = c.id)
             ORDER BY c.t, c.id",
        )?;
        let rows = stmt.query_map(params![now, older_than_days], |r| {
            Ok(Candidate {
                capture_id: r.get(0)?,
                path: r.get(1)?,
                bytes: r.get(2)?,
                t: r.get(3)?,
                boot_id: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for c in rows {
            let c = c?;
            let path = norm(&c.path);
            if !pins.iter().any(|p| path.starts_with(p.as_str())) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// The captures whose files are still on disk (no `removed_at`), for capture_usage and capture_clean, with
    /// whether a cleanup keeps each unless it is named: pinned, a claim's evidence (through a measurement), a pose's
    /// reference image, or referenced by a document.
    pub fn capture_rows(&self) -> Result<Vec<CaptureRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.path, c.boot_id,
               c.pinned = 1
               OR EXISTS (SELECT 1 FROM measurements m JOIN claim_support s ON s.measurement_id = m.id
                          WHERE m.capture_id = c.id)
               OR EXISTS (SELECT 1 FROM poses p WHERE p.ref_capture_id = c.id)
               OR EXISTS (SELECT 1 FROM doc_refs r WHERE r.capture_id = c.id)
             FROM captures c WHERE c.removed_at IS NULL ORDER BY c.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CaptureRow {
                id: r.get(0)?,
                path: r.get(1)?,
                boot_id: r.get(2)?,
                cited: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The capture root's folder names that belong to a boot: its id (a profiler's output) and its session id, the
    /// suffix of the logs folder (`<t>-<session>`) its journal path names; then the first folder of each capture row
    /// with a boot.
    pub fn boot_folders(&self) -> Result<Vec<(String, i64)>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT id, journal_path FROM boots UNION ALL
             SELECT DISTINCT boot_id, path FROM captures WHERE boot_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (boot, path) = row?;
            let parts: Vec<&str> = path
                .as_deref()
                .unwrap_or_default()
                .split(['\\', '/'])
                .collect();
            let folder = match parts[..] {
                [.., logs, "journal.jsonl"] => logs.rsplit_once('-').map(|(_, session)| session),
                [first, _, ..] => Some(first),
                _ => None,
            };
            out.push((boot.to_string(), boot));
            out.extend(folder.map(|f| (f.to_lowercase(), boot)));
        }
        Ok(out)
    }

    /// Rewrites each `boots.journal_path` under the folder `old` (any case, either separator) to the same path under
    /// `new`: a move of the data root, whose journal paths are the only absolute data-root paths the ledger keeps
    /// (captures are relative to the capture root). The number of rows rewritten.
    pub fn rebase_journal_paths(&mut self, old: &str, new: &str) -> Result<usize> {
        let key = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
        let old = key(old) + "\\";
        let rows: Vec<(i64, String)> = self
            .conn
            .prepare("SELECT id, journal_path FROM boots WHERE journal_path IS NOT NULL")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        self.atomic(|l| {
            let mut n = 0;
            for (id, path) in &rows {
                let flat = path.replace('/', "\\");
                let (Some(head), Some(rest)) = (flat.get(..old.len()), flat.get(old.len()..))
                else {
                    continue;
                };
                if head.to_lowercase() != old {
                    continue;
                }
                let moved = format!("{}\\{rest}", new.trim_end_matches('\\'));
                l.conn.execute(
                    "UPDATE boots SET journal_path = ?2 WHERE id = ?1",
                    params![id, moved],
                )?;
                n += 1;
            }
            Ok(n)
        })
    }

    /// Marks capture `id` as taken off the disk at `t`, to `to` (`recycle_bin` or `to_delete`): the row stays.
    pub fn mark_removed(&mut self, id: i64, t: &str, to: &str) -> Result<()> {
        self.check_time(t)?;
        let n = self.conn.execute(
            "UPDATE captures SET removed_at = ?2, removed_to = ?3 WHERE id = ?1 AND removed_at IS NULL",
            params![id, t, to],
        )?;
        self.changed(n, "capture", id, "is removed already")
    }

    fn check_time(&self, t: &str) -> Result<()> {
        let ok: bool = self.conn.query_row(
            "SELECT ?1 IS strftime('%Y-%m-%dT%H:%M:%SZ', ?1)",
            [t],
            |r| r.get(0),
        )?;
        if !ok {
            return Err(Error::Refused(format!(
                "{t:?} is not a UTC time like 2026-10-17T13:15:00Z"
            )));
        }
        Ok(())
    }
}
