//! The RE knowledge base's index in the ledger (docs/intent.md, "Game knowledge"): `quen kb scan` mirrors the KB's
//! curated tables (names.tsv, hooks.tsv, patches.tsv, instance_patches.tsv, cvar_layout.tsv) as entities, addresses
//! and links stamped `<file>@<commit>`, splits every note at its H2 / H3 headings into addressable sections, and records
//! which known entities each section mentions; `quen kb claims` turns the rows of chosen note tables into keyed claims with the
//! author's grades. The text stays in git: the ledger holds paths, anchors, line ranges, sha256s and the commit. The
//! commit is read from the work tree's `.git` files ([`git_head`]), so no git process is started.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Error, Ledger, Result};
use crate::build::Build;
use crate::journal::sha256_hex;
use crate::seeds::{IMAGE_BASE, rva, tsv_rows};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";
const NAMES_HEADER: [&str; 6] = ["rva", "name", "kind", "confidence", "source", "date"];
const HOOKS_HEADER: [&str; 5] = ["site", "kind", "mod", "version", "note"];
/// cvar_layout.tsv's offset columns, as `field` entities `cvar_object.<column>` (the var object it lays out).
const CVAR_FIELDS: [&str; 6] = ["group", "name", "flags", "value", "default", "live"];

/// The commit checked out in the git work tree that holds `dir`: its `.git` (a folder, or a worktree's file naming
/// one), then HEAD, then the branch's ref file or `packed-refs`. None outside a work tree, or before the first commit.
pub fn git_head(dir: &Path) -> Option<String> {
    let dot = dir
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|g| g.exists())?;
    let git = if dot.is_file() {
        let text = fs::read_to_string(&dot).ok()?;
        dot.parent()?
            .join(text.trim().strip_prefix("gitdir:")?.trim())
    } else {
        dot
    };
    let head = fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(name) = head.strip_prefix("ref:").map(str::trim) else {
        return commit_id(head).then(|| head.to_owned());
    };
    // A worktree keeps its HEAD; the branches are the main repository's, which its commondir names.
    let common = fs::read_to_string(git.join("commondir"))
        .map(|c| git.join(c.trim()))
        .unwrap_or_else(|_| git.clone());
    for dir in [&git, &common] {
        if let Ok(id) = fs::read_to_string(dir.join(name))
            && commit_id(id.trim())
        {
            return Some(id.trim().to_owned());
        }
    }
    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (id, r) = line.split_once(' ')?;
        (r.trim() == name && commit_id(id)).then(|| id.to_owned())
    })
}

/// A git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
fn commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One heading section of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The slug of the heading path (`## Q1` > `### Allocation` → `q1/allocation`); `''` for the text before the
    /// first H2. A repeated one in a note gets `-2`, `-3`, ….
    pub anchor: String,
    pub heading: Option<String>,
    /// 1 for the text before the first H2 (its heading is the note's `#` title, if any), else 2 or 3.
    pub level: u8,
    /// The index of the H2 section an H3 is under.
    pub parent: Option<usize>,
    /// 1-based, inclusive.
    pub line_start: usize,
    pub line_end: usize,
    /// Of the section's lines, each ended by `\n`.
    pub sha256: String,
}

/// A heading's slug: lowercase letters and digits, every other run one `-`; `section` when nothing is left.
fn slug(heading: &str) -> String {
    let mut out = String::new();
    for c in heading.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "section".into()
    } else {
        out.into()
    }
}

/// `line`'s heading level and text, for `#` to `###` outside code fences.
fn heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    let rest = &line[hashes..];
    if !(1..=3).contains(&hashes) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    Some((
        hashes as u8,
        rest.trim().trim_end_matches('#').trim_end().into(),
    ))
}

/// A note's sections, in order: split at every H2 and H3 outside a code fence. Blank text before the first H2 is no
/// section.
pub fn sections(text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    // (start line index, level, heading)
    let mut starts: Vec<(usize, u8, Option<String>)> = vec![(0, 1, None)];
    let mut fence = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        match heading(line).filter(|_| !fence) {
            Some((1, h)) if starts.len() == 1 && starts[0].2.is_none() => starts[0].2 = Some(h),
            Some((level @ 2..=3, h)) => starts.push((i, level, Some(h))),
            _ => {}
        }
    }
    let mut out: Vec<Section> = Vec::new();
    let (mut used, mut h2) = (BTreeSet::new(), None::<(usize, String)>);
    for (n, (start, level, head)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(lines.len(), |s| s.0);
        let body = &lines[*start..end];
        if body.is_empty() || (*level == 1 && body.iter().all(|l| l.trim().is_empty())) {
            continue;
        }
        let own = head.as_deref().map(slug).unwrap_or_default();
        let (path, parent) = match level {
            1 => (String::new(), None),
            2 => (own.clone(), None),
            _ => match &h2 {
                Some((i, p)) => (format!("{p}/{own}"), Some(*i)),
                None => (own.clone(), None),
            },
        };
        let mut anchor = path.clone();
        for k in 2.. {
            if used.insert(anchor.clone()) {
                break;
            }
            anchor = format!("{path}-{k}");
        }
        if *level == 2 {
            h2 = Some((out.len(), anchor.clone()));
        }
        let text: String = body.iter().map(|l| format!("{l}\n")).collect();
        out.push(Section {
            anchor,
            heading: head.clone(),
            level: *level,
            parent,
            line_start: start + 1,
            line_end: end,
            sha256: sha256_hex(text.as_bytes()),
        });
    }
    out
}

/// The runs of letters, digits and `_` in `text`, the identifiers and hex numbers mentions are matched against.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
}

fn word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Matches the known entities of one game in text: their keys as written, a shader hash with or without `0x`,
/// the RVAs and VAs of functions and labels (`0x…`, Ghidra's `FUN_…` names), in any case. Stages and labels are left
/// out: they are ordinary words. An alias two entities share matches neither. Unknown hex is never an entity.
struct Matcher {
    /// Keys made of letters, digits and `_`: matched as whole words, exactly.
    exact: HashMap<String, Option<i64>>,
    /// Hex aliases, lowercase: matched as whole words in any case.
    hex: HashMap<String, Option<i64>>,
    /// Keys with other characters (`Group/Name`, `mod:kind:fn`): matched as written, not inside a longer word.
    phrases: BTreeMap<String, Option<i64>>,
}

impl Matcher {
    fn add(map: &mut HashMap<String, Option<i64>>, alias: String, id: i64) {
        map.entry(alias)
            .and_modify(|v| {
                if *v != Some(id) {
                    *v = None;
                }
            })
            .or_insert(Some(id));
    }

    fn new(l: &Ledger, game: &str) -> Result<Matcher> {
        let mut m = Matcher {
            exact: HashMap::new(),
            hex: HashMap::new(),
            phrases: BTreeMap::new(),
        };
        let mut stmt = l.conn.prepare(
            "SELECT e.id, e.kind, e.key, a.rva FROM entities e LEFT JOIN entity_addresses a ON a.entity_id = e.id
             WHERE e.game = ?1 AND e.kind <> 'stage'",
        )?;
        let rows = stmt.query_map([game], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for row in rows {
            let (id, kind, key, rva) = row?;
            if kind == "shader" {
                let bare = key.trim_start_matches("0x").to_ascii_lowercase();
                Matcher::add(&mut m.hex, format!("0x{bare}"), id);
                Matcher::add(&mut m.hex, bare, id);
            } else if key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                Matcher::add(&mut m.exact, key, id);
            } else {
                let v = m.phrases.entry(key).or_insert(Some(id));
                if *v != Some(id) {
                    *v = None;
                }
            }
            if let (Some(rva), "function" | "label") = (rva, kind.as_str()) {
                let va = IMAGE_BASE + rva as u64;
                Matcher::add(&mut m.hex, format!("0x{rva:x}"), id);
                Matcher::add(&mut m.hex, format!("0x{va:x}"), id);
                for prefix in ["fun_", "lab_", "sub_"] {
                    Matcher::add(&mut m.hex, format!("{prefix}{va:x}"), id);
                }
            }
        }
        Ok(m)
    }

    /// How many times each entity is mentioned in `text`.
    fn count(&self, text: &str) -> BTreeMap<i64, i64> {
        let mut out = BTreeMap::new();
        for w in words(text) {
            let hit = self
                .exact
                .get(w)
                .or_else(|| self.hex.get(&w.to_ascii_lowercase()));
            if let Some(Some(id)) = hit {
                *out.entry(*id).or_insert(0) += 1;
            }
        }
        for (phrase, id) in &self.phrases {
            let Some(id) = id else { continue };
            for (at, _) in text.match_indices(phrase.as_str()) {
                let before = text[..at].chars().next_back();
                let after = text[at + phrase.len()..].chars().next();
                if !word_char(before) && !word_char(after) {
                    *out.entry(*id).or_insert(0) += 1;
                }
            }
        }
        out
    }
}

/// What [`Ledger::scan_kb`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KbScan {
    pub commit: String,
    /// The game (its title) and build (exe sha256) the KB's build line names.
    pub game: String,
    pub build: String,
    /// Rows mirrored per table (absent tables count 0).
    pub names: usize,
    pub hooks: usize,
    pub patches: usize,
    pub instance_patches: usize,
    pub cvar_layouts: usize,
    /// patches.tsv, instance_patches.tsv and cvar_layout.tsv rows of a build stamp the ledger has no game row for: not
    /// mirrored.
    pub rows_unknown_build: usize,
    pub notes: usize,
    pub sections: usize,
    pub sections_new: usize,
    /// Sections no longer found (their rows stay, with gone_at).
    pub sections_gone: usize,
    pub mentions: usize,
    pub entities_new: i64,
    pub links: usize,
}

/// A KB file's text, None when it isn't there.
fn read(root: &Path, name: &str) -> Result<Option<String>> {
    match fs::read_to_string(root.join(name)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Refused(format!(
            "{}: {e}",
            root.join(name).display()
        ))),
    }
}

/// The build the KB's names.tsv and hooks.tsv name on their build line: both, where there, must have one, the same.
fn kb_build(names: Option<&str>, hooks: Option<&str>) -> Result<Build> {
    let mut build: Option<(&str, Build)> = None;
    for (file, text) in [("names.tsv", names), ("hooks.tsv", hooks)] {
        let Some(text) = text else { continue };
        let b = Build::of_kb(text).map_err(|e| Error::Refused(format!("{file}: {e}")))?;
        match &build {
            Some((first, other)) if *other != b => {
                return Err(Error::Refused(format!(
                    "the KB's {first} is for {other}, but its {file} for {b}: one KB, one build"
                )));
            }
            Some(_) => {}
            None => build = Some((file, b)),
        }
    }
    build.map(|(_, b)| b).ok_or_else(|| {
        Error::Refused(
            "the KB has no names.tsv or hooks.tsv with a build line: which game and build is it?"
                .into(),
        )
    })
}

/// Every `.md` file under `dir` (hidden folders skipped), as paths relative to `root` with `/`, sorted.
fn notes(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let refused = |e: std::io::Error| Error::Refused(format!("{}: {e}", dir.display()));
    for entry in fs::read_dir(dir).map_err(refused)? {
        let entry = entry.map_err(refused)?;
        let (path, ty) = (entry.path(), entry.file_type().map_err(refused)?);
        let name = entry.file_name().to_string_lossy().into_owned();
        if ty.is_dir() && !name.starts_with('.') {
            notes(root, &path, out)?;
        } else if ty.is_file() && name.to_ascii_lowercase().ends_with(".md") {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let parts: Vec<_> = rel.iter().map(|p| p.to_string_lossy()).collect();
            out.push(parts.join("/"));
        }
    }
    out.sort();
    Ok(())
}

/// A KB table's bad line, refused.
fn tsv_err(file: &'static str) -> impl Fn((usize, String)) -> Error {
    move |(line, msg)| Error::Refused(format!("{file} line {line}: {msg}"))
}

/// A curated table's row, ready to mirror: the entity, its address in one game build, and its confidence.
struct Mirrored {
    kind: &'static str,
    key: String,
    label: Option<String>,
    game_id: i64,
    rva: Option<i64>,
    offset: Option<i64>,
    confidence: Option<String>,
}

impl Ledger {
    /// The KB's game: the games row of the build its tables' build line names (id, title), else refused.
    fn kb_game(&self, build: &Build) -> Result<(i64, String)> {
        self.conn
            .query_row("SELECT id, name FROM games WHERE exe_sha256 = ?1", [&build.sha256], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or_else(|| {
                Error::Refused(format!(
                    "the KB's build line names {build}, which the ledger has no game row for: boot or import that \
                     build first"
                ))
            })
    }

    /// The game row of a build stamp: the KB's own build, else the one game row with that stamp.
    fn stamp_game(&self, stamp: &str, kb: &Build, kb_game: i64) -> Result<Option<i64>> {
        if stamp.eq_ignore_ascii_case(&kb.stamp.to_string()) {
            return Ok(Some(kb_game));
        }
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM games WHERE upper(build_stamp) = upper(?1)")?
            .query_map([stamp], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok((ids.len() == 1).then(|| ids[0]))
    }

    /// Replaces the addresses mirrored from `file` with `rows` (their entities added on first sight), stamped
    /// `<file>@<commit>`: the entity ids, in order.
    fn mirror(&self, file: &str, commit: &str, game: &str, rows: &[Mirrored]) -> Result<Vec<i64>> {
        let prefix = format!("{file}@");
        self.conn.execute(
            "DELETE FROM entity_addresses WHERE substr(kb_source, 1, length(?1)) = ?1",
            [&prefix],
        )?;
        let source = format!("{prefix}{commit}");
        let mut ids = Vec::new();
        for r in rows {
            let id = self.entity(game, r.kind, &r.key, r.label.as_deref())?;
            self.conn.execute(
                "INSERT INTO entity_addresses (entity_id, game_id, rva, offset, kb_source, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING",
                params![id, r.game_id, r.rva, r.offset, source, r.confidence],
            )?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Makes `file`'s links exactly `want` (src, rel, dst), stamped `<file>@<commit>`: unchanged ones keep their row
    /// (and id), with the new stamp.
    fn mirror_links(
        &self,
        file: &str,
        commit: &str,
        want: &BTreeSet<(i64, &str, i64)>,
    ) -> Result<()> {
        let prefix = format!("{file}@");
        let source = format!("{prefix}{commit}");
        let existing: Vec<(i64, i64, String, i64)> = self
            .conn
            .prepare("SELECT id, src_id, rel, dst_id FROM links WHERE substr(kb_source, 1, length(?1)) = ?1")?
            .query_map([&prefix], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut left = want.clone();
        for (id, src, rel, dst) in existing {
            let same = left
                .iter()
                .find(|k| (k.0, k.1, k.2) == (src, rel.as_str(), dst))
                .copied();
            if let Some(k) = same {
                left.remove(&k);
                self.conn.execute(
                    "UPDATE links SET kb_source = ?2 WHERE id = ?1",
                    params![id, source],
                )?;
            } else {
                self.conn.execute("DELETE FROM links WHERE id = ?1", [id])?;
            }
        }
        for (src, rel, dst) in left {
            self.conn.execute(
                &format!("INSERT INTO links (src_id, dst_id, rel, kb_source, t) VALUES (?1, ?2, ?3, ?4, {NOW})"),
                params![src, dst, rel, source],
            )?;
        }
        Ok(())
    }

    /// Indexes the RE knowledge base at `root`: its curated tables as entities, addresses and links, every note's
    /// sections, and the known entities each section mentions, all at the commit its work tree has checked out.
    /// Everything is read and checked first, then written in one transaction; a scan of an unchanged KB adds nothing.
    /// Refused, nothing written: a KB outside a git work tree (or before its first commit), no names.tsv or hooks.tsv
    /// with a build line, two tables naming different builds, a build the ledger has no game row for, a bad table row,
    /// a name defined twice (a patch's in patches.tsv and instance_patches.tsv too), a note that isn't UTF-8.
    pub fn scan_kb(&mut self, root: &Path) -> Result<KbScan> {
        let commit = git_head(root).ok_or_else(|| {
            Error::Refused(format!(
                "{} isn't in a git work tree with a commit: the index needs one to address the text",
                root.display()
            ))
        })?;
        let (names, hooks) = (read(root, "names.tsv")?, read(root, "hooks.tsv")?);
        let (patches, layouts) = (read(root, "patches.tsv")?, read(root, "cvar_layout.tsv")?);
        let instances = read(root, "instance_patches.tsv")?;
        let build = kb_build(names.as_deref(), hooks.as_deref())?;
        let stamp = format!("{:08X}", build.stamp.0);

        // names.tsv: (line, kind, key, rva, confidence). A code seed has no name, so no entity.
        let mut named: Vec<(usize, &'static str, String, i64, Option<String>)> = Vec::new();
        if let Some(text) = &names {
            for (line, [cell, name, kind, confidence, _, _]) in
                tsv_rows(text, &NAMES_HEADER).map_err(tsv_err("names.tsv"))?
            {
                let at = rva(cell).map_err(|m| tsv_err("names.tsv")((line, m)))?;
                let name = name.split_whitespace().collect::<Vec<_>>().join("_");
                let (kind, key) = match (kind, name.is_empty()) {
                    ("code", _) => continue,
                    ("function", true) => ("function", format!("fn_{stamp}_{at:X}")),
                    ("function", false) => ("function", name),
                    ("label", false) => ("label", name),
                    _ => {
                        return Err(tsv_err("names.tsv")((
                            line,
                            format!("kind {kind:?} with name {name:?}"),
                        )));
                    }
                };
                if let Some(first) = named.iter().find(|n| n.2 == key) {
                    return Err(tsv_err("names.tsv")((
                        line,
                        format!("{key} is defined at line {} too", first.0),
                    )));
                }
                let confidence = (!confidence.is_empty()).then(|| confidence.to_owned());
                named.push((line, kind, key, i64::from(at), confidence));
            }
        }
        // hooks.tsv: (kind, mod, site, note).
        let mut hooked = Vec::new();
        if let Some(text) = &hooks {
            for (line, [site, kind, owner, _, note]) in
                tsv_rows(text, &HOOKS_HEADER).map_err(tsv_err("hooks.tsv"))?
            {
                let at = rva(site).map_err(|m| tsv_err("hooks.tsv")((line, m)))?;
                if kind.is_empty() || owner.is_empty() {
                    return Err(tsv_err("hooks.tsv")((
                        line,
                        "kind and mod are required".into(),
                    )));
                }
                hooked.push((
                    kind.to_owned(),
                    owner.to_owned(),
                    i64::from(at),
                    (!note.is_empty()).then(|| note.to_owned()),
                ));
            }
        }
        let patch_rows = match &patches {
            Some(text) => {
                crate::mem::from_patches_tsv(text).map_err(|e| Error::Refused(e.to_string()))?
            }
            None => Vec::new(),
        };
        let instance_rows = match &instances {
            Some(text) => {
                crate::mem::from_instances_tsv(text).map_err(|e| Error::Refused(e.to_string()))?
            }
            None => Vec::new(),
        };
        if let Some(p) = instance_rows
            .iter()
            .find(|p| patch_rows.iter().any(|q| q.name == p.name))
        {
            return Err(Error::Refused(format!(
                "{} names a patch in patches.tsv and one in instance_patches.tsv: one name, one patch",
                p.name
            )));
        }
        let layout_rows = match &layouts {
            Some(text) => {
                crate::cvar::from_layout_tsv(text).map_err(|e| Error::Refused(e.to_string()))?
            }
            None => Vec::new(),
        };
        let mut docs = Vec::new();
        let mut paths = Vec::new();
        notes(root, root, &mut paths)?;
        for rel in paths {
            let file = root.join(&rel);
            let text = fs::read_to_string(&file)
                .map_err(|e| Error::Refused(format!("{}: {e}", file.display())))?;
            docs.push((rel, text));
        }

        self.atomic(|l| {
            let (game_id, game) = l.kb_game(&build)?;
            let entities_before: i64 = l.conn.query_row("SELECT count(*) FROM entities", [], |r| r.get(0))?;
            let mut out = KbScan {
                commit: commit.clone(),
                game: game.clone(),
                build: build.sha256.clone(),
                names: named.len(),
                hooks: hooked.len(),
                patches: 0,
                instance_patches: 0,
                cvar_layouts: 0,
                rows_unknown_build: 0,
                notes: docs.len(),
                sections: 0,
                sections_new: 0,
                sections_gone: 0,
                mentions: 0,
                entities_new: 0,
                links: 0,
            };

            let rows: Vec<Mirrored> = named
                .iter()
                .map(|(_, kind, key, rva, confidence)| Mirrored {
                    kind,
                    key: key.clone(),
                    label: None,
                    game_id,
                    rva: Some(*rva),
                    offset: None,
                    confidence: confidence.clone(),
                })
                .collect();
            let name_ids = l.mirror("names.tsv", &commit, &game, &rows)?;
            // A hook at a function's start is keyed by the function and hooks it; any other by its site.
            let function_at: HashMap<i64, (i64, &str)> = named
                .iter()
                .zip(&name_ids)
                .filter(|((_, kind, ..), _)| *kind == "function")
                .map(|((_, _, key, rva, _), id)| (*rva, (*id, key.as_str())))
                .collect();
            let rows: Vec<Mirrored> = hooked
                .iter()
                .map(|(kind, owner, at, note)| Mirrored {
                    kind: "hook",
                    key: match function_at.get(at) {
                        Some((_, f)) => format!("{owner}:{kind}:{f}"),
                        None => format!("{owner}:{kind}:{stamp}_{at:X}"),
                    },
                    label: note.clone(),
                    game_id,
                    rva: Some(*at),
                    offset: None,
                    confidence: None,
                })
                .collect();
            let hook_ids = l.mirror("hooks.tsv", &commit, &game, &rows)?;
            let want: BTreeSet<(i64, &str, i64)> = hooked
                .iter()
                .zip(&hook_ids)
                .filter_map(|((_, _, at, _), hook)| Some((*hook, "hooks", function_at.get(at)?.0)))
                .collect();
            out.links = want.len();
            l.mirror_links("hooks.tsv", &commit, &want)?;

            let mut rows = Vec::new();
            for p in &patch_rows {
                match l.stamp_game(&p.build, &build, game_id)? {
                    Some(g) => rows.push(Mirrored {
                        kind: "patch",
                        key: p.name.clone(),
                        label: None,
                        game_id: g,
                        rva: Some(i64::from(p.rva)),
                        offset: None,
                        confidence: None,
                    }),
                    None => out.rows_unknown_build += 1,
                }
            }
            out.patches = rows.len();
            l.mirror("patches.tsv", &commit, &game, &rows)?;
            // An instance patch: at its class's vtable, and the field `<class>+0x<offset>` it sets.
            let mut rows = Vec::new();
            for p in &instance_rows {
                let Some(g) = l.stamp_game(&p.build, &build, game_id)? else {
                    out.rows_unknown_build += 1;
                    continue;
                };
                out.instance_patches += 1;
                let mirrored = |kind, key, rva, offset| Mirrored {
                    kind,
                    key,
                    label: None,
                    game_id: g,
                    rva,
                    offset,
                    confidence: None,
                };
                let field = format!("{}+{:#x}", p.class, p.offset);
                rows.push(mirrored("patch", p.name.clone(), Some(i64::from(p.vtable)), None));
                rows.push(mirrored("field", field, None, Some(p.offset as i64)));
            }
            let ids = l.mirror("instance_patches.tsv", &commit, &game, &rows)?;
            let want: BTreeSet<(i64, &str, i64)> = ids.chunks(2).map(|p| (p[0], "sets", p[1])).collect();
            out.links += want.len();
            l.mirror_links("instance_patches.tsv", &commit, &want)?;
            let mut rows = Vec::new();
            for layout in &layout_rows {
                let Some(g) = l.stamp_game(&layout.build, &build, game_id)? else {
                    out.rows_unknown_build += 1;
                    continue;
                };
                out.cvar_layouts += 1;
                let offsets = [layout.group, layout.name, layout.flags, layout.value, layout.default, layout.live];
                for (field, offset) in CVAR_FIELDS.iter().zip(offsets) {
                    rows.push(Mirrored {
                        kind: "field",
                        key: format!("cvar_object.{field}"),
                        label: None,
                        game_id: g,
                        rva: None,
                        offset: Some(offset as i64),
                        confidence: None,
                    });
                }
            }
            l.mirror("cvar_layout.tsv", &commit, &game, &rows)?;

            // The notes, after the tables: mentions resolve against the entities just mirrored.
            let matcher = Matcher::new(l, &game)?;
            let mut seen = BTreeSet::new();
            for (path, text) in &docs {
                let lines: Vec<&str> = text.lines().collect();
                let mut ids: Vec<i64> = Vec::new();
                for s in sections(text) {
                    let existed: bool = l.conn.query_row(
                        "SELECT count(*) > 0 FROM kb_sections WHERE path = ?1 AND anchor = ?2",
                        params![path, s.anchor],
                        |r| r.get(0),
                    )?;
                    let id: i64 = l.conn.query_row(
                        &format!(
                            "INSERT INTO kb_sections (path, anchor, heading, level, parent_id, line_start, line_end, sha256,
                               kb_commit, scanned_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, {NOW})
                             ON CONFLICT (path, anchor) DO UPDATE SET heading = excluded.heading, level = excluded.level,
                               parent_id = excluded.parent_id, line_start = excluded.line_start,
                               line_end = excluded.line_end, sha256 = excluded.sha256, kb_commit = excluded.kb_commit,
                               scanned_at = excluded.scanned_at, gone_at = NULL
                             RETURNING id"
                        ),
                        params![
                            path,
                            s.anchor,
                            s.heading,
                            s.level,
                            s.parent.map(|i| ids[i]),
                            s.line_start,
                            s.line_end,
                            s.sha256,
                            commit,
                        ],
                        |r| r.get(0),
                    )?;
                    out.sections_new += usize::from(!existed);
                    ids.push(id);
                    seen.insert(id);
                    l.conn.execute("DELETE FROM kb_mentions WHERE section_id = ?1", [id])?;
                    let body = lines[s.line_start - 1..s.line_end].join("\n");
                    for (entity, n) in matcher.count(&body) {
                        l.conn.execute(
                            "INSERT INTO kb_mentions (section_id, entity_id, n) VALUES (?1, ?2, ?3)",
                            params![id, entity, n],
                        )?;
                        out.mentions += 1;
                    }
                }
                out.sections += ids.len();
            }
            let present: Vec<i64> = l
                .conn
                .prepare("SELECT id FROM kb_sections WHERE gone_at IS NULL")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in present.into_iter().filter(|id| !seen.contains(id)) {
                l.conn
                    .execute(&format!("UPDATE kb_sections SET gone_at = {NOW} WHERE id = ?1"), [id])?;
                l.conn.execute("DELETE FROM kb_mentions WHERE section_id = ?1", [id])?;
                out.sections_gone += 1;
            }
            let entities_after: i64 = l.conn.query_row("SELECT count(*) FROM entities", [], |r| r.get(0))?;
            out.entities_new = entities_after - entities_before;
            Ok(out)
        })
    }

    /// The commit the ledger's KB index was last scanned at, and when; None before the first scan.
    pub fn kb_index(&self) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT kb_commit, scanned_at FROM kb_sections WHERE kb_commit IS NOT NULL
                 ORDER BY scanned_at DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
}

/// A grade letter: review V / C / U or confidence H / M / L.
fn grade_letters(text: &str) -> Vec<char> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(|w| match w {
            "V" | "C" | "U" | "H" | "M" | "L" => w.chars().next(),
            _ => None,
        })
        .collect()
}

/// The review grade of `letters`: C when any part was corrected (the corrected form is the one written), else U
/// when any part is unreviewed, else V.
fn review(letters: &[char]) -> Option<char> {
    ['C', 'U', 'V'].into_iter().find(|g| letters.contains(g))
}

/// The confidence of `letters`: the lowest one named.
fn confidence(letters: &[char]) -> Option<char> {
    ['L', 'M', 'H'].into_iter().find(|g| letters.contains(g))
}

/// `text` without its grade marks, and the marks' letters: bracket groups whose first word is a grade letter
/// (`[V]`, `[V/C, all H for code]`, `[U, H by author]`) and `(H)`, `(M)`, `(L)`.
fn strip_marks(text: &str) -> (String, Vec<char>) {
    let (mut out, mut letters, mut rest) = (String::new(), Vec::new(), text);
    while let Some(open) = rest.find(['[', '(']) {
        let close = if rest[open..].starts_with('[') {
            ']'
        } else {
            ')'
        };
        let Some(len) = rest[open..].find(close) else {
            break;
        };
        let inner = &rest[open + 1..open + len];
        let first = inner.split([' ', ',', '/']).next().unwrap_or_default();
        let mark = if close == ']' {
            matches!(first, "V" | "C" | "U" | "H" | "M" | "L")
        } else {
            matches!(inner, "H" | "M" | "L")
        };
        if mark {
            out += &rest[..open];
            letters.extend(grade_letters(inner));
        } else {
            out += &rest[..open + len + 1];
        }
        rest = &rest[open + len + 1..];
    }
    out += rest;
    (
        out.split_whitespace().collect::<Vec<_>>().join(" "),
        letters,
    )
}

/// A Markdown table row's cells: split at `|` outside code spans and not escaped, outer pipes dropped, trimmed.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let (mut out, mut cell, mut code, mut chars) =
        (Vec::new(), String::new(), false, line.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => cell.push(chars.next().unwrap_or('|')),
            '`' => {
                code = !code;
                cell.push(c);
            }
            '|' if !code => out.push(std::mem::take(&mut cell).trim().to_owned()),
            c => cell.push(c),
        }
    }
    if !cell.trim().is_empty() {
        out.push(cell.trim().to_owned());
    }
    out
}

/// A table's separator row: `|---|:--:|`.
fn separator(line: &str) -> bool {
    let c = cells(line);
    !c.is_empty()
        && c.iter()
            .all(|c| !c.is_empty() && c.trim_matches(':').bytes().all(|b| b == b'-'))
}

/// One claim from a table row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Statement {
    text: String,
    confidence: Option<char>,
    review: Option<char>,
    /// Shader hashes it names, `0x` + 8 uppercase hex.
    shaders: BTreeSet<String>,
}

/// The shader hashes `text` names: `0x` and exactly 8 hex digits (a VA has more), as `0x` + uppercase.
fn shader_hashes(text: &str) -> impl Iterator<Item = String> + '_ {
    words(text).filter_map(|w| {
        let hex = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X"))?;
        (hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| format!("0x{}", hex.to_ascii_uppercase()))
    })
}

/// The rows of every table in `lines`, as statements: the cells joined with ` | `, grade marks removed. A grade column
/// (header Status, Conf or Confidence) gives the row's grades and keeps only its other words; else the row's own marks
/// do; else `context`'s (the headings above it).
fn statements(lines: &[&str], context: &[char]) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        if !(lines[i].trim_start().starts_with('|') && separator(lines[i + 1])) {
            i += 1;
            continue;
        }
        let header: Vec<String> = cells(lines[i])
            .iter()
            .map(|h| h.trim_matches('*').to_ascii_lowercase())
            .collect();
        let grade_col = header
            .iter()
            .position(|h| matches!(h.as_str(), "status" | "conf" | "confidence"));
        i += 2;
        while i < lines.len() && lines[i].trim_start().starts_with('|') {
            let row = cells(lines[i]);
            i += 1;
            if row.iter().all(|c| c.is_empty() || c == "—" || c == "-") {
                continue;
            }
            let (mut parts, mut marks) = (Vec::new(), Vec::new());
            let mut graded = Vec::new();
            for (n, c) in row.iter().enumerate() {
                if Some(n) == grade_col {
                    graded = grade_letters(c);
                    continue;
                }
                let (text, letters) = strip_marks(c);
                marks.extend(letters);
                parts.push(text);
            }
            if let Some(c) = grade_col.and_then(|n| row.get(n)) {
                let rest: Vec<&str> = c
                    .split_whitespace()
                    .filter(|w| {
                        !w.split('/')
                            .all(|p| p.is_empty() || matches!(p, "V" | "C" | "U" | "H" | "M" | "L"))
                    })
                    .collect();
                if !rest.is_empty() {
                    parts.push(rest.join(" "));
                }
            }
            let pick = |f: fn(&[char]) -> Option<char>| {
                f(&graded).or_else(|| f(&marks)).or_else(|| f(context))
            };
            let text = parts.join(" | ");
            let shaders = row.iter().flat_map(|c| shader_hashes(c)).collect();
            out.push(Statement {
                text,
                confidence: pick(confidence),
                review: pick(review),
                shaders,
            });
        }
    }
    out
}

/// What [`Ledger::import_kb_claims`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KbClaims {
    pub note: String,
    /// The sections read, by anchor.
    pub sections: Vec<String>,
    pub rows: usize,
    /// Claim ids added; rows whose key is in the ledger already add nothing.
    pub claims_new: Vec<i64>,
    pub already: usize,
    /// Open claims under these sections whose row the note no longer has (edited or removed): left as they are, for
    /// whoever withdraws them.
    pub stale: Vec<String>,
}

impl Ledger {
    /// Turns the table rows of the chosen sections of a KB note into claims: one per row, keyed
    /// `<note>#<anchor>/<first 12 hex of the sha256 of its text>`, with the grades the row, else its headings, state
    /// (confidence H / M / L, review V / C / U), `scope` as given, a `re_refs` row to the section, and every shader hash
    /// it or its section's heading names (`0x` + 8 hex) as a subject. No support: the grades say what the author believed, proof needs
    /// measurements. `sections` are anchors, or heading labels such as `2d` (a heading's first word, its final `.`
    /// dropped). Importing again adds nothing. Refused, nothing written: a path outside the KB, a section not found, no
    /// table rows, the KB's game unknown (as for [`Ledger::scan_kb`]).
    pub fn import_kb_claims(
        &mut self,
        root: &Path,
        note: &str,
        wanted: &[String],
        scope: Option<&str>,
    ) -> Result<KbClaims> {
        let note = note.replace('\\', "/");
        if note.is_empty()
            || note.starts_with('/')
            || note.contains(':')
            || note.split('/').any(|p| p == "..")
        {
            return Err(Error::Refused(format!(
                "{note:?} must be a note's path relative to the KB root"
            )));
        }
        let path: PathBuf = root.join(&note);
        let text = fs::read_to_string(&path)
            .map_err(|e| Error::Refused(format!("{}: {e}", path.display())))?;
        let build = kb_build(
            read(root, "names.tsv")?.as_deref(),
            read(root, "hooks.tsv")?.as_deref(),
        )?;
        let all = sections(&text);
        let lines: Vec<&str> = text.lines().collect();
        let label = |s: &Section| {
            let first = s
                .heading
                .as_deref()
                .and_then(|h| h.split_whitespace().next())
                .unwrap_or_default();
            first.trim_end_matches('.').to_owned()
        };
        let mut chosen: Vec<(usize, Vec<Statement>)> = Vec::new();
        for want in wanted {
            let found: Vec<usize> = (0..all.len())
                .filter(|&i| {
                    all[i].anchor == *want || (all[i].level > 1 && label(&all[i]) == *want)
                })
                .collect();
            if found.is_empty() {
                return Err(Error::Refused(format!("{note} has no section {want:?}")));
            }
            let found: Vec<usize> = found
                .into_iter()
                .filter(|i| !chosen.iter().any(|c| c.0 == *i))
                .collect();
            for i in found {
                let s = &all[i];
                let mut context: Vec<char> = s
                    .heading
                    .as_deref()
                    .map(|h| strip_marks(h).1)
                    .unwrap_or_default();
                if let Some(parent) = s.parent.and_then(|p| all[p].heading.as_deref()) {
                    context.extend(strip_marks(parent).1);
                }
                // The section's own heading names the pass its rows describe.
                let mut rows = statements(&lines[s.line_start - 1..s.line_end], &context);
                for r in &mut rows {
                    r.shaders
                        .extend(s.heading.iter().flat_map(|h| shader_hashes(h)));
                }
                chosen.push((i, rows));
            }
        }
        let rows: usize = chosen.iter().map(|c| c.1.len()).sum();
        if rows == 0 {
            return Err(Error::Refused(format!(
                "no table rows in those sections of {note}"
            )));
        }
        self.atomic(|l| {
            let (_, game) = l.kb_game(&build)?;
            let mut out = KbClaims {
                note: note.clone(),
                sections: chosen.iter().map(|c| all[c.0].anchor.clone()).collect(),
                rows,
                claims_new: Vec::new(),
                already: 0,
                stale: Vec::new(),
            };
            let mut keys = BTreeSet::new();
            for (i, statements) in &chosen {
                let at = format!("{note}#{}", all[*i].anchor);
                for s in statements {
                    let key = format!("{at}/{}", &sha256_hex(s.text.as_bytes())[..12]);
                    keys.insert(key.clone());
                    let found: bool =
                        l.conn.query_row("SELECT count(*) > 0 FROM claims WHERE key = ?1", [&key], |r| r.get(0))?;
                    if found {
                        out.already += 1;
                        continue;
                    }
                    l.conn.execute(
                        "INSERT INTO claims (text, scope, key, confidence, review) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![s.text, scope, key, s.confidence.map(String::from), s.review.map(String::from)],
                    )?;
                    let claim = l.conn.last_insert_rowid();
                    for shader in &s.shaders {
                        let entity = l.entity(&game, "shader", shader, None)?;
                        l.conn.execute(
                            "INSERT INTO claim_subjects (claim_id, entity_id) VALUES (?1, ?2)",
                            params![claim, entity],
                        )?;
                    }
                    l.conn.execute(
                        "INSERT INTO re_refs (claim_id, kb_path) VALUES (?1, ?2)",
                        params![claim, at],
                    )?;
                    out.claims_new.push(claim);
                }
                let mut stmt = l.conn.prepare(
                    "SELECT key FROM claims WHERE status = 'open' AND substr(key, 1, length(?1)) = ?1 ORDER BY key",
                )?;
                let under: Vec<String> =
                    stmt.query_map([format!("{at}/")], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                out.stale.extend(under.into_iter().filter(|k| !keys.contains(k)));
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_split_at_h2_and_h3_outside_code_fences() {
        let text = "# Title\nintro\n\n## 1. Overview\ntext\n### Allocation (H)\nmore\n```\n## not a heading\n```\n\
                    #### deeper stays\n## 1. Overview\nagain\n### Q\n";
        let s = sections(text);
        let got: Vec<(&str, u8, Option<usize>, usize, usize)> = s
            .iter()
            .map(|s| {
                (
                    s.anchor.as_str(),
                    s.level,
                    s.parent,
                    s.line_start,
                    s.line_end,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("", 1, None, 1, 3),
                ("1-overview", 2, None, 4, 5),
                ("1-overview/allocation-h", 3, Some(1), 6, 11),
                ("1-overview-2", 2, None, 12, 13),
                ("1-overview-2/q", 3, Some(3), 14, 14),
            ]
        );
        assert_eq!(s[0].heading.as_deref(), Some("Title"));
        assert_eq!(sections("## Only\n")[0].anchor, "only");
        assert!(sections("\n\n").is_empty());
        assert_eq!(
            slug("2d. Tonemap → grade (A=`988E0`)"),
            "2d-tonemap-grade-a-988e0"
        );
    }

    #[test]
    fn table_rows_become_statements_with_their_grades() {
        let lines = [
            "| # | Pass | Limit | Status |",
            "|---|---|---|---|",
            "| 1 | **0x1A2B3C4D** | `min(x,|v|)` clamp [C] | V / C (s1) |",
            "| 2 | 0xdeadbeef and 0x14034D4D0 | none (L) | H |",
            "| — | | | |",
            "not a row",
        ];
        let s = statements(&lines, &['U']);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].text, "1 | **0x1A2B3C4D** | `min(x,|v|)` clamp | (s1)");
        assert_eq!((s[0].review, s[0].confidence), (Some('C'), None));
        assert_eq!(s[0].shaders.iter().collect::<Vec<_>>(), ["0x1A2B3C4D"]);
        assert_eq!(s[1].text, "2 | 0xdeadbeef and 0x14034D4D0 | none");
        // The grade column's H; review from the headings.
        assert_eq!((s[1].review, s[1].confidence), (Some('U'), Some('H')));
        assert_eq!(s[1].shaders.iter().collect::<Vec<_>>(), ["0xDEADBEEF"]);
        let (text, letters) = strip_marks("x [V/C, all H for code] [c8.w, 2] (M) (note)");
        assert_eq!(
            (text.as_str(), review(&letters), confidence(&letters)),
            ("x [c8.w, 2] (note)", Some('C'), Some('M'))
        );
    }

    #[test]
    fn git_head_follows_the_branch_ref_and_packed_refs() {
        let dir = std::env::temp_dir().join(format!("quen-kb-head-{}", std::process::id()));
        let git = dir.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::create_dir_all(dir.join("re/notes")).unwrap();
        let a = "a".repeat(40);
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(git_head(&dir.join("re/notes")), None, "no commit yet");
        fs::write(
            git.join("packed-refs"),
            format!("# pack-refs\n{a} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(git_head(&dir.join("re")), Some(a.clone()));
        let b = "b".repeat(40);
        fs::write(git.join("refs/heads/main"), format!("{b}\n")).unwrap();
        assert_eq!(git_head(&dir.join("re")), Some(b.clone()));
        fs::write(git.join("HEAD"), format!("{a}\n")).unwrap();
        assert_eq!(git_head(&dir), Some(a), "detached");
        let _ = fs::remove_dir_all(&dir);
    }
}
