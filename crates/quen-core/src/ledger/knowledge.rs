//! Game knowledge (docs/intent.md, "Game knowledge"): entities, REA's answers as extractions, `about` (everything known
//! about one thing) and the KB export of the claims. An extraction is a cache entry keyed by everything that determines
//! the answer ([`extraction_key`]); the answer itself stays whole in its REA evidence bundle, a pinned capture file
//! under the capture root, never in a ledger text column. The KB scan and its claim import are kb.rs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as Sql;
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Error, Ledger, Result};
use crate::journal::sha256_hex;
use crate::seeds::IMAGE_BASE;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// `value` as canonical JSON text: object keys sorted, no whitespace. The cache key and stored parameters use it, so
/// the same query always hashes alike. ponytail: RFC 8785 also fixes number forms; serde_json's integers already match,
/// a float parameter would only make two spellings of one query miss each other.
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::from(k.as_str()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// The cache key of a tool query: the sha256 of the canonical JSON of the analysed file's sha256, the analysis
/// profile's digest, the operation and its parameters. The same key is the same query on the same analysis.
pub fn extraction_key(
    target_sha256: &str,
    profile: &str,
    operation: &str,
    params: &Value,
) -> String {
    let key = json!({
        "operation": operation,
        "parameters": params,
        "profile_digest": profile,
        "target_sha256": target_sha256,
    });
    sha256_hex(canonical(&key).as_bytes())
}

/// An address as REA writes it, `0x14034d4d0` or Ghidra's `ram:0x14034d4d0`.
fn address(text: &str) -> Option<u64> {
    let hex = text.rsplit(':').next()?.strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

/// A VA as an RVA of the image quen-core knows the base of ([`IMAGE_BASE`]), if it falls inside 32 bits of it.
fn rva_of(va: u64) -> Option<i64> {
    va.checked_sub(IMAGE_BASE)
        .filter(|r| *r <= u64::from(u32::MAX))
        .map(|r| r as i64)
}

/// The address a record's parameters ask about, as REA 6.5.0 names it: `procedure` (procedure_pseudo_code,
/// procedure_assembly, procedure_callers, ...) or `address` (xrefs, read_bytes, resolve_containing_procedure). Its
/// records of these leave `locations` empty.
fn param_address(params: &Value) -> Option<u64> {
    ["procedure", "address"]
        .into_iter()
        .find_map(|k| address(params[k].as_str()?))
}

fn sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One REA evidence record, as the cache keeps it.
#[derive(Debug, Clone)]
struct Record {
    evidence_id: String,
    target: String,
    profile_digest: String,
    provider: Value,
    profile_params: Value,
    operation: String,
    params: Value,
    result_sha256: String,
    confidence: String,
    rva: Option<i64>,
    raw: Value,
}

/// The cacheable records of an REA evidence bundle (`export_evidence_bundle`'s JSON), and how many records it skipped
/// for having no subject or no analysis profile (they stay in the file). Refused: not a bundle, a malformed record, a
/// repeated evidence id.
fn parse_bundle(text: &str) -> std::result::Result<(Vec<Record>, usize), String> {
    let bundle: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let records = bundle
        .get("records")
        .and_then(Value::as_array)
        .ok_or("not an REA evidence bundle: no records list")?;
    let (mut out, mut skipped, mut seen) = (Vec::new(), 0, BTreeSet::new());
    for (i, r) in records.iter().enumerate() {
        let bad = |what: &str| format!("record {i}: {what}");
        let id = r["evidence_id"].as_str().unwrap_or_default();
        if !(id.starts_with("ev_") && sha256(&id[3..])) {
            return Err(bad("evidence_id isn't ev_ and 64 lowercase hex"));
        }
        if !seen.insert(id) {
            return Err(bad(&format!("{id} appears twice")));
        }
        let (subject, profile) = (&r["subject"], &r["analysis_profile"]);
        if subject.is_null() || profile.is_null() {
            skipped += 1;
            continue;
        }
        let target = subject["digest"]["sha256"].as_str().unwrap_or_default();
        let digest = profile["digest"].as_str().unwrap_or_default();
        if !sha256(target) || !sha256(digest) {
            return Err(bad(
                "subject.digest.sha256 and analysis_profile.digest must be 64 lowercase hex",
            ));
        }
        let operation = r["operation"].as_str().filter(|o| !o.trim().is_empty());
        let confidence = r["confidence"].as_str();
        match (
            operation,
            confidence,
            r["parameters"].is_object(),
            profile["provider"].is_object(),
            profile["parameters"].is_object(),
        ) {
            (Some(_), Some("observed" | "derived" | "inferred"), true, true, true) => {}
            _ => {
                return Err(bad(
                    "needs operation, parameters (an object), confidence (observed, derived or inferred) and an \
                     analysis_profile with provider and parameters",
                ));
            }
        }
        let first_address = r["locations"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| l["kind"] == "address")
            .and_then(|l| address(l["address"].as_str()?))
            .or_else(|| param_address(&r["parameters"]));
        out.push(Record {
            evidence_id: id.into(),
            target: target.into(),
            profile_digest: digest.into(),
            provider: profile["provider"].clone(),
            profile_params: profile["parameters"].clone(),
            operation: operation.unwrap_or_default().into(),
            params: r["parameters"].clone(),
            result_sha256: sha256_hex(canonical(&r["raw_result"]).as_bytes()),
            confidence: confidence.unwrap_or_default().into(),
            rva: first_address.and_then(rva_of),
            raw: r["raw_result"].clone(),
        });
    }
    Ok((out, skipped))
}

/// The addresses a callers or callees record lists: its raw result as a list of addresses, or as `{value: [...]}`,
/// each a string or `{address}`.
fn listed_addresses(raw: &Value) -> Vec<u64> {
    let list = raw.as_array().or_else(|| raw["value"].as_array());
    list.into_iter()
        .flatten()
        .filter_map(|a| address(a.as_str().or_else(|| a["address"].as_str())?))
        .collect()
}

/// What [`Ledger::import_rea_bundle`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundleImport {
    pub capture_id: i64,
    /// Relative to the capture root.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    /// Copied into the capture root (it was elsewhere).
    pub copied: bool,
    pub records: usize,
    /// New extractions; the rest of the records were imported before (same evidence id).
    pub extractions_new: usize,
    /// Records without a subject or an analysis profile: kept in the file, not cached.
    pub skipped: usize,
    pub profiles_new: usize,
    /// `calls` links from callers / callees records, where both ends are known functions of that build.
    pub links_new: usize,
}

impl Ledger {
    /// The id of the entity (game, kind, key), added on first sight. A label replaces the stored one; none keeps it.
    pub(super) fn entity(
        &self,
        game: &str,
        kind: &str,
        key: &str,
        label: Option<&str>,
    ) -> Result<i64> {
        Ok(self.conn.query_row(
            &format!(
                "INSERT INTO entities (game, kind, key, label, t) VALUES (?1, ?2, ?3, ?4, {NOW})
                 ON CONFLICT (game, kind, key) DO UPDATE SET label = coalesce(excluded.label, label) RETURNING id"
            ),
            params![game, kind, key, label],
            |r| r.get(0),
        )?)
    }

    /// The function or label at `rva` in the build `exe_sha256`, if the ledger knows one.
    fn entity_at(&self, exe_sha256: &str, rva: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT e.id FROM entity_addresses a JOIN games g ON g.id = a.game_id JOIN entities e ON e.id = a.entity_id
                 WHERE g.exe_sha256 = ?1 AND a.rva = ?2 AND e.kind IN ('function', 'label')
                 ORDER BY e.kind = 'function' DESC, e.id LIMIT 1",
                params![exe_sha256, rva],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Fills in the rva and entity of extractions imported without an rva, from their stored parameters: quen 0.1.0
    /// read the rva from `locations` only, which REA 6.5.0 leaves empty for single-address queries. Only NULL rvas are
    /// filled, and the bundle file isn't read, so one that's gone doesn't matter. Runs at every read-write open.
    pub(super) fn fill_rvas(&mut self) -> Result<()> {
        self.atomic(|l| {
            let rows: Vec<(i64, String, String)> = l
                .conn
                .prepare("SELECT id, target_sha256, params FROM extractions WHERE rva IS NULL")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, target, params) in rows {
                let rva = serde_json::from_str(&params)
                    .ok()
                    .and_then(|p| param_address(&p))
                    .and_then(rva_of);
                let Some(rva) = rva else { continue };
                let entity = l.entity_at(&target, rva)?;
                l.conn.execute(
                    "UPDATE extractions SET rva = ?1, entity_id = coalesce(entity_id, ?2) WHERE id = ?3 AND rva IS NULL",
                    params![rva, entity, id],
                )?;
            }
            Ok(())
        })
    }

    /// Imports an REA evidence bundle: the file is copied into `<capture root>/re/` unless it is under the
    /// capture root already, recorded as a pinned capture, and each record with a subject and an analysis profile
    /// becomes an extraction keyed by [`extraction_key`], with `rea_version` (REA's records don't carry it). A record
    /// whose evidence id is in the ledger already is skipped, so importing a bundle again adds nothing. Callers and
    /// callees records also give `calls` links, where both ends are functions the ledger knows in that build. Refused,
    /// nothing written: a file that isn't a bundle, a malformed record, a repeated evidence id.
    pub fn import_rea_bundle(
        &mut self,
        capture_root: &Path,
        bundle: &Path,
        rea_version: &str,
    ) -> Result<BundleImport> {
        let refused =
            |e: &dyn std::fmt::Display| Error::Refused(format!("{}: {e}", bundle.display()));
        if rea_version.trim().is_empty() {
            return Err(Error::Refused(
                "the REA version is required (REA's records don't carry it)".into(),
            ));
        }
        let bytes = fs::read(bundle).map_err(|e| refused(&e))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| refused(&e))?;
        let (records, skipped) = parse_bundle(text).map_err(|e| refused(&e))?;
        let sha = sha256_hex(&bytes);

        // Where it goes: where it is, if under the capture root; else a file named by its content, so a second import
        // of the same bundle finds it.
        fs::create_dir_all(capture_root).map_err(|e| refused(&e))?;
        let root = fs::canonicalize(capture_root).map_err(|e| refused(&e))?;
        let source = fs::canonicalize(bundle).map_err(|e| refused(&e))?;
        let known: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM captures WHERE sha256 = ?1 AND method = 'rea' AND removed_at IS NULL ORDER BY id LIMIT 1",
                [&sha],
                |r| r.get(0),
            )
            .optional()?;
        let (rel, copied) = match (source.strip_prefix(&root), known) {
            (Ok(rel), _) => (rel_path(rel), false),
            (Err(_), Some(path))
                if sha256_of(&root.join(&path)).as_deref() == Some(sha.as_str()) =>
            {
                (path, false)
            }
            (Err(_), _) => {
                let rel = format!("re/rea-{}.json", &sha[..16]);
                (
                    rel.clone(),
                    place(&root.join(&rel), &bytes, &sha).map_err(|e| refused(&e))?,
                )
            }
        };

        self.atomic(|l| {
            let at: Option<(i64, String)> = l
                .conn
                .query_row("SELECT id, sha256 FROM captures WHERE path = ?1", [&rel], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let capture_id = match at {
                Some((id, s)) if s == sha => id,
                Some(_) => {
                    return Err(Error::Refused(format!(
                        "the capture root's {rel} is recorded with another sha256: a different file"
                    )));
                }
                None => {
                    l.conn.execute(
                        &format!(
                            "INSERT INTO captures (boot_id, path, sha256, bytes, kind, method, t, pinned)
                             VALUES (NULL, ?1, ?2, ?3, 'json', 'rea', {NOW}, 1)"
                        ),
                        params![rel, sha, bytes.len() as i64],
                    )?;
                    l.conn.last_insert_rowid()
                }
            };
            let mut out = BundleImport {
                capture_id,
                path: rel.clone(),
                sha256: sha.clone(),
                bytes: bytes.len() as u64,
                copied,
                records: records.len() + skipped,
                extractions_new: 0,
                skipped,
                profiles_new: 0,
                links_new: 0,
            };
            for r in &records {
                out.profiles_new += l.conn.execute(
                    "INSERT INTO extraction_profiles (digest, tool, provider, parameters) VALUES (?1, 'rea', ?2, ?3)
                     ON CONFLICT DO NOTHING",
                    params![r.profile_digest, canonical(&r.provider), canonical(&r.profile_params)],
                )?;
                let entity = match r.rva {
                    Some(rva) => l.entity_at(&r.target, rva)?,
                    None => None,
                };
                let n = l.conn.execute(
                    &format!(
                        "INSERT INTO extractions (tool, tool_version, target_sha256, profile_digest, operation, params,
                           key_sha256, evidence_id, result_sha256, confidence, rva, entity_id, capture_id, t)
                         VALUES ('rea', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, {NOW})
                         ON CONFLICT (evidence_id) DO NOTHING"
                    ),
                    params![
                        rea_version,
                        r.target,
                        r.profile_digest,
                        r.operation,
                        canonical(&r.params),
                        extraction_key(&r.target, &r.profile_digest, &r.operation, &r.params),
                        r.evidence_id,
                        r.result_sha256,
                        r.confidence,
                        r.rva,
                        entity,
                        capture_id,
                    ],
                )?;
                if n == 0 {
                    continue;
                }
                out.extractions_new += 1;
                let extraction = l.conn.last_insert_rowid();
                let callers = match r.operation.as_str() {
                    "procedure_callers" => true,
                    "procedure_callees" => false,
                    _ => continue,
                };
                let Some(center) = entity else { continue };
                for other in listed_addresses(&r.raw).into_iter().filter_map(rva_of) {
                    let Some(other) = l.entity_at(&r.target, other)? else { continue };
                    let (src, dst) = if callers { (other, center) } else { (center, other) };
                    if src == dst {
                        continue;
                    }
                    out.links_new += l.conn.execute(
                        &format!(
                            "INSERT INTO links (src_id, dst_id, rel, extraction_id, confidence, t)
                             VALUES (?1, ?2, 'calls', ?3, ?4, {NOW}) ON CONFLICT DO NOTHING"
                        ),
                        params![src, dst, extraction, r.confidence],
                    )?;
                }
            }
            Ok(out)
        })
    }
}

/// `rel`'s components joined with `/`.
fn rel_path(rel: &Path) -> String {
    let parts: Vec<_> = rel.iter().map(|p| p.to_string_lossy()).collect();
    parts.join("/")
}

fn sha256_of(path: &Path) -> Option<String> {
    crate::journal::sha256_file(path).ok()
}

/// Writes `bytes` (whose sha256 is `sha`) to `to` through a temporary file beside it, unless `to` holds them already:
/// whether it wrote. A different file at `to` is refused, never replaced.
fn place(to: &Path, bytes: &[u8], sha: &str) -> std::io::Result<bool> {
    let differs = || std::io::Error::other(format!("{} exists with other content", to.display()));
    match sha256_of(to) {
        Some(s) if s == sha => return Ok(false),
        Some(_) => return Err(differs()),
        None => {}
    }
    fs::create_dir_all(to.parent().unwrap_or(to))?;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = PathBuf::from(format!(
        "{}.{}-{nanos}-{n}.tmp",
        to.display(),
        std::process::id()
    ));
    fs::write(&tmp, bytes)?;
    let moved = fs::rename(&tmp, to);
    let _ = fs::remove_file(&tmp);
    match (moved, sha256_of(to)) {
        // Another import of the same bundle may have placed it first.
        (_, Some(s)) if s == sha => Ok(true),
        (Err(e), _) => Err(e),
        (Ok(()), _) => Err(differs()),
    }
}

/// What `ledger_about` is asked about: an entity by `kind` and `key` (in `game`, when several games have it), or
/// the function or label at `rva` in the build `build` (its exe sha256). `operation`, `params` and `profile` narrow
/// its extractions; with `build` and all three, the cache key is known.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct About {
    pub game: Option<String>,
    pub kind: Option<String>,
    pub key: Option<String>,
    pub build: Option<String>,
    /// `0x`-prefixed hex.
    pub rva: Option<String>,
    pub operation: Option<String>,
    pub params: Option<Map<String, Value>>,
    pub profile: Option<String>,
}

/// One row of `sql` as a JSON object, column names as keys.
fn objects(l: &Ledger, sql: &str, params: &[Sql]) -> Result<Vec<Value>> {
    let mut stmt = l.conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let rows = stmt.query_map(params_from_iter(params), |row| {
        let mut obj = Map::new();
        for (i, name) in names.iter().enumerate() {
            let v = match row.get::<_, Sql>(i)? {
                Sql::Null => Value::Null,
                Sql::Integer(n) => n.into(),
                Sql::Real(x) => json!(x),
                Sql::Text(t) => t.into(),
                Sql::Blob(b) => sha256_hex(&b).into(),
            };
            obj.insert(name.clone(), v);
        }
        Ok(Value::Object(obj))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Ledger {
    /// Everything known about one thing: the entity, its addresses per build, the claims about it with their
    /// standing, its links with their basis and standing, the KB sections that mention it, and its extractions:
    /// `exact` (the profile asked for), `near` (another profile, or no profile asked for) and `gone` (the bundle file
    /// was removed: the query ran, its answer is lost). Nothing known is an answer too: an empty one. Refused: no
    /// anchor (kind and key, or build and rva), an rva that isn't hex, a key several games have without `game`.
    pub fn about(&self, q: &About) -> Result<Value> {
        let refused = |why: &str| Err(Error::Refused(why.into()));
        let rva = match q.rva.as_deref() {
            Some(cell) => Some(i64::from(crate::seeds::rva(cell).map_err(Error::Refused)?)),
            None => None,
        };
        if rva.is_some() && q.build.is_none() {
            return refused("rva needs build: the exe sha256 it is an address in");
        }
        let entity: Option<Value> = match (&q.kind, &q.key, rva) {
            (Some(kind), Some(key), _) => {
                let found = objects(
                    self,
                    "SELECT id, game, kind, key, label, t FROM entities WHERE kind = ?1 AND key = ?2
                       AND (?3 IS NULL OR game = ?3) ORDER BY id",
                    &[kind.clone().into(), key.clone().into(), q.game.clone().map_or(Sql::Null, Sql::Text)],
                )?;
                if found.len() > 1 {
                    return refused("several games have this kind and key: give game");
                }
                found.into_iter().next()
            }
            (None, None, Some(rva)) => objects(
                self,
                "SELECT e.id, e.game, e.kind, e.key, e.label, e.t FROM entity_addresses a
                 JOIN games g ON g.id = a.game_id JOIN entities e ON e.id = a.entity_id
                 WHERE g.exe_sha256 = ?1 AND a.rva = ?2 ORDER BY e.kind = 'function' DESC, e.kind = 'label' DESC, e.id",
                &[q.build.clone().unwrap_or_default().into(), rva.into()],
            )?
            .into_iter()
            .next(),
            _ => return refused("ask about kind and key, or build and rva"),
        };
        let id = entity.as_ref().and_then(|e| e["id"].as_i64());
        let id_param = id.map_or(Sql::Null, Sql::Integer);
        let list = |sql: &str| objects(self, sql, std::slice::from_ref(&id_param));

        let addresses = list(
            "SELECT g.exe_sha256 AS build, g.build_stamp, CASE WHEN a.rva IS NOT NULL THEN printf('0x%X', a.rva) END AS rva,
               CASE WHEN a.offset IS NOT NULL THEN printf('0x%X', a.offset) END AS offset,
               a.size, a.bytes_len, a.bytes_sha256,
               CASE WHEN a.kb_source IS NOT NULL THEN 'kb' WHEN a.extraction_id IS NOT NULL THEN 'extraction' ELSE 'claim' END
                 AS basis,
               a.kb_source, a.extraction_id, a.claim_id, a.confidence
             FROM entity_addresses a JOIN games g ON g.id = a.game_id WHERE a.entity_id = ?1 ORDER BY g.id",
        )?;
        let claims = list(
            "SELECT c.id, c.key, c.text, c.scope, c.status, c.withdrawn_reason, c.confidence, c.review, e.proven,
               e.families, e.contradictions, e.supported_by, e.kb_paths
             FROM claim_subjects s JOIN claims c ON c.id = s.claim_id JOIN claims_evidence e ON e.id = c.id
             WHERE s.entity_id = ?1 ORDER BY c.status, c.id",
        )?;
        let links = list(
            "SELECT id, rel, CASE WHEN src_id = ?1 THEN 'out' ELSE 'in' END AS direction,
               CASE WHEN src_id = ?1 THEN dst_kind || ':' || dst_key ELSE src_kind || ':' || src_key END AS other,
               basis, claim_key, claim_status, proven, families, contradictions, supported_by, evidence_id, tool,
               tool_version, kb_source, confidence
             FROM links_evidence WHERE ?1 IN (src_id, dst_id) ORDER BY rel, id",
        )?;
        let sections = list(
            "SELECT k.path, k.anchor, k.heading, k.line_start, k.line_end, k.kb_commit, m.n
             FROM kb_mentions m JOIN kb_sections k ON k.id = m.section_id
             WHERE m.entity_id = ?1 AND k.gone_at IS NULL ORDER BY k.path, k.line_start",
        )?;

        // Extractions: the entity's (by id, or at one of its addresses), or at build + rva; narrowed by build,
        // operation and params.
        let params_text = q
            .params
            .as_ref()
            .map(|p| canonical(&Value::Object(p.clone())));
        let anchored = id.is_some() || rva.is_some();
        let found = if anchored {
            objects(
                self,
                "SELECT x.id, x.tool, x.tool_version, x.target_sha256, x.operation, x.params, x.evidence_id, x.confidence,
                   CASE WHEN x.rva IS NOT NULL THEN printf('0x%X', x.rva) END AS rva, x.profile_digest, json_extract(p.parameters, '$.analysis_seeds') AS seeds,
                   x.key_sha256,
                   (SELECT count(DISTINCT y.result_sha256) FROM extractions y WHERE y.key_sha256 = x.key_sha256)
                     AS results,
                   c.path AS bundle, c.removed_at, x.t
                 FROM extractions x JOIN extraction_profiles p ON p.digest = x.profile_digest
                 JOIN captures c ON c.id = x.capture_id
                 WHERE (x.entity_id = ?1
                        OR EXISTS (SELECT 1 FROM entity_addresses a JOIN games g ON g.id = a.game_id
                                   WHERE a.entity_id = ?1 AND a.rva = x.rva AND g.exe_sha256 = x.target_sha256)
                        OR (x.target_sha256 = ?2 AND x.rva = ?3))
                   AND (?2 IS NULL OR x.target_sha256 = ?2)
                   AND (?4 IS NULL OR x.operation = ?4)
                   AND (?5 IS NULL OR x.params = ?5)
                 ORDER BY x.operation, x.t DESC, x.id",
                &[
                    id_param.clone(),
                    q.build.clone().map_or(Sql::Null, Sql::Text),
                    rva.map_or(Sql::Null, Sql::Integer),
                    q.operation.clone().map_or(Sql::Null, Sql::Text),
                    params_text.map_or(Sql::Null, Sql::Text),
                ],
            )?
        } else {
            Vec::new()
        };
        let (mut exact, mut near, mut gone) = (Vec::new(), Vec::new(), Vec::new());
        let extracted = !found.is_empty();
        for x in found {
            if !x["removed_at"].is_null() {
                gone.push(x);
            } else if q.profile.is_some() && x["profile_digest"].as_str() == q.profile.as_deref() {
                exact.push(x);
            } else {
                near.push(x);
            }
        }
        let key = match (&q.build, &q.profile, &q.operation, &q.params) {
            (Some(b), Some(p), Some(o), Some(params)) => {
                Some(extraction_key(b, p, o, &Value::Object(params.clone())))
            }
            _ => None,
        };
        Ok(json!({
            "entity": entity,
            "addresses": addresses,
            "claims": claims,
            "links": links,
            "sections": sections,
            "extracted": extracted,
            "extractions": {"key_sha256": key, "exact": exact, "near": near, "gone": gone},
        }))
    }

    /// The KB's copies of the ledger's keyed claims: `claims.tsv` (key, text, scope, status,
    /// withdrawn_reason, confidence, review, proven, families, subjects, kb_refs) and `links.tsv` (src, rel, dst,
    /// claim), sorted by key for stable diffs. `proven` and `families` are as of the export. Tabs, line breaks and
    /// backslashes in a cell are written `\t`, `\n`, `\r`, `\\`.
    pub fn kb_export(&self) -> Result<(String, String)> {
        let tsv = |header: &str, sql: &str| -> Result<String> {
            let mut stmt = self.conn.prepare(sql)?;
            let width = stmt.column_count();
            let rows = stmt.query_map([], |r| {
                (0..width)
                    .map(|i| {
                        Ok(match r.get::<_, Sql>(i)? {
                            Sql::Null => String::new(),
                            Sql::Integer(n) => n.to_string(),
                            Sql::Real(x) => x.to_string(),
                            Sql::Text(t) => cell(&t),
                            Sql::Blob(_) => String::new(),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<String>>>()
            })?;
            let mut out = format!("{header}\n");
            for row in rows {
                out += &row?.join("\t");
                out.push('\n');
            }
            Ok(out)
        };
        let claims = tsv(
            "key\ttext\tscope\tstatus\twithdrawn_reason\tconfidence\treview\tproven\tfamilies\tsubjects\tkb_refs",
            "SELECT c.key, c.text, c.scope, c.status, c.withdrawn_reason, c.confidence, c.review, e.proven, e.families,
               (SELECT group_concat(x.kind || ':' || x.key, ',' ORDER BY x.kind, x.key) FROM claim_subjects s
                JOIN entities x ON x.id = s.entity_id WHERE s.claim_id = c.id),
               e.kb_paths
             FROM claims c JOIN claims_evidence e ON e.id = c.id WHERE c.key IS NOT NULL ORDER BY c.key",
        )?;
        let links = tsv(
            "src\trel\tdst\tclaim",
            "SELECT src_kind || ':' || src_key, rel, dst_kind || ':' || dst_key, claim_key FROM links_evidence
             WHERE claim_key IS NOT NULL ORDER BY claim_key, src_kind, src_key, rel, dst_kind, dst_key",
        )?;
        Ok((claims, links))
    }
}

/// A capture row as the extraction tools read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRef {
    pub id: i64,
    pub boot_id: Option<i64>,
    /// Relative to the capture root, `/` separators.
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub kind: String,
    /// `capture_clean` took its file.
    pub removed: bool,
}

/// One answer under a profile quen defines itself (ngfx-replay's flags, quen's shader decoder), as
/// [`Ledger::add_extraction`] keeps it; its params are `{}`.
#[derive(Debug, Clone)]
pub struct Extraction<'a> {
    pub tool: &'a str,
    pub tool_version: Option<&'a str>,
    /// `{id, name}`; the version goes in `tool_version`, so an update doesn't miss the cache.
    pub provider: Value,
    /// Everything the answer depends on: the exact flags, the decoder's revision.
    pub parameters: Value,
    pub target_sha256: &'a str,
    pub operation: &'a str,
    /// Of the output file.
    pub result_sha256: &'a str,
    /// The output file's capture row.
    pub capture_id: i64,
}

/// The digest of a profile quen defines itself: the sha256 of the canonical JSON of its tool, provider and parameters.
pub fn profile_digest(tool: &str, provider: &Value, parameters: &Value) -> String {
    let profile = json!({"parameters": parameters, "provider": provider, "tool": tool});
    sha256_hex(canonical(&profile).as_bytes())
}

impl Extraction<'_> {
    /// Its cache key ([`extraction_key`]).
    pub fn key(&self) -> String {
        let digest = profile_digest(self.tool, &self.provider, &self.parameters);
        extraction_key(self.target_sha256, &digest, self.operation, &json!({}))
    }
}

impl Ledger {
    /// The capture row `id`, else the one at `path` (relative to the capture root, either separator, any case).
    pub fn capture_ref(&self, id: Option<i64>, path: Option<&str>) -> Result<Option<CaptureRef>> {
        let path = path.map(|p| p.replace('\\', "/"));
        Ok(self
            .conn
            .query_row(
                "SELECT id, boot_id, path, sha256, bytes, kind, removed_at IS NOT NULL FROM captures
                 WHERE id = ?1 OR lower(path) = lower(?2) ORDER BY id = ?1 DESC LIMIT 1",
                params![id, path],
                |r| {
                    Ok(CaptureRef {
                        id: r.get(0)?,
                        boot_id: r.get(1)?,
                        path: r.get(2)?,
                        sha256: r.get(3)?,
                        bytes: r.get(4)?,
                        kind: r.get(5)?,
                        removed: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// The newest extraction under `key` whose output file is still on disk (its capture row not removed): its id and
    /// that row.
    pub fn extraction_hit(&self, key: &str) -> Result<Option<(i64, CaptureRef)>> {
        let hit: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT x.id, x.capture_id FROM extractions x JOIN captures c ON c.id = x.capture_id
                 WHERE x.key_sha256 = ?1 AND c.removed_at IS NULL ORDER BY x.id DESC LIMIT 1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, capture)) = hit else {
            return Ok(None);
        };
        Ok(self.capture_ref(Some(capture), None)?.map(|c| (id, c)))
    }

    /// Adds `x`, and its profile if new; its id.
    pub fn add_extraction(&mut self, x: &Extraction) -> Result<i64> {
        let digest = profile_digest(x.tool, &x.provider, &x.parameters);
        self.atomic(|l| {
            l.conn.execute(
                "INSERT INTO extraction_profiles (digest, tool, provider, parameters) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO NOTHING",
                params![digest, x.tool, canonical(&x.provider), canonical(&x.parameters)],
            )?;
            l.conn.execute(
                &format!(
                    "INSERT INTO extractions (tool, tool_version, target_sha256, profile_digest, operation, params,
                       key_sha256, result_sha256, capture_id, t)
                     VALUES (?1, ?2, ?3, ?4, ?5, '{{}}', ?6, ?7, ?8, {NOW})"
                ),
                params![
                    x.tool,
                    x.tool_version,
                    x.target_sha256,
                    digest,
                    x.operation,
                    x.key(),
                    x.result_sha256,
                    x.capture_id
                ],
            )?;
            Ok(l.conn.last_insert_rowid())
        })
    }

    /// Sets capture `id`'s `frame_state`, if it has none yet.
    pub fn fill_frame_state(&mut self, id: i64, state: &Map<String, Value>) -> Result<()> {
        self.conn.execute(
            "UPDATE captures SET frame_state = ?1 WHERE id = ?2 AND frame_state IS NULL",
            params![Value::Object(state.clone()).to_string(), id],
        )?;
        Ok(())
    }
}

/// A TSV cell: tab, line breaks and backslash escaped.
fn cell(text: &str) -> String {
    text.replace('\\', r"\\")
        .replace('\t', r"\t")
        .replace('\n', r"\n")
        .replace('\r', r"\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let v = json!({"b": [1, {"z": "x\"y", "a": null}], "a": true});
        assert_eq!(canonical(&v), r#"{"a":true,"b":[1,{"a":null,"z":"x\"y"}]}"#);
        let same = json!({"a": true, "b": [1, {"a": null, "z": "x\"y"}]});
        let key = |p: &Value| extraction_key(&"a".repeat(64), &"b".repeat(64), "xrefs", p);
        assert_eq!(key(&v), key(&same));
        assert_ne!(key(&v), key(&json!({})));
    }

    /// A quen-defined extraction is a hit until its output file is removed; another tool version keeps the key, other
    /// flags miss it.
    #[test]
    fn quen_extractions_hit_until_their_file_goes() {
        let mut l = Ledger::open_in_memory().unwrap();
        let path = "imports/abc/cap/functions.json";
        let row = crate::ledger::Capture {
            path: path.into(),
            sha256: "c".repeat(64),
            bytes: 1,
            kind: "json".into(),
            t: "2026-10-23T00:00:00Z".into(),
            ..Default::default()
        };
        let capture_id = l.add_capture(&row).unwrap();
        let (target, result) = ("a".repeat(64), "c".repeat(64));
        let x = Extraction {
            tool: "ngfx_replay",
            tool_version: Some("1"),
            provider: json!({"id": "ngfx_replay"}),
            parameters: json!({"args": ["--metadata-functions"]}),
            target_sha256: &target,
            operation: "functions",
            result_sha256: &result,
            capture_id,
        };
        assert_eq!(l.extraction_hit(&x.key()).unwrap(), None);
        let id = l.add_extraction(&x).unwrap();
        let (hit, out) = l.extraction_hit(&x.key()).unwrap().unwrap();
        assert_eq!((hit, out.id, out.path.as_str()), (id, capture_id, path));
        let newer = Extraction {
            tool_version: Some("2"),
            ..x.clone()
        };
        assert_eq!(newer.key(), x.key());
        let other = Extraction {
            parameters: json!({"args": []}),
            ..x.clone()
        };
        assert_ne!(other.key(), x.key());
        l.conn
            .execute(
                "UPDATE captures SET removed_at = '2026-10-23T00:00:01Z', removed_to = 'recycle_bin' WHERE id = ?1",
                [capture_id],
            )
            .unwrap();
        assert_eq!(l.extraction_hit(&x.key()).unwrap(), None);
        let by_path = l.capture_ref(None, Some(r"IMPORTS\abc\cap\functions.json"));
        assert_eq!(by_path.unwrap().map(|c| c.removed), Some(true));
    }

    #[test]
    fn addresses_and_rvas() {
        assert_eq!(address("0x14034d4d0"), Some(0x1_4034_d4d0));
        assert_eq!(address("ram:0x14034d4d0"), Some(0x1_4034_d4d0));
        assert_eq!(address("14034d4d0"), None);
        assert_eq!(rva_of(0x1_4034_d4d0), Some(0x34_d4d0));
        assert_eq!(rva_of(0x1000), None);
        assert_eq!(
            listed_addresses(
                &json!({"value": ["0x140001000", {"address": "0x140002000"}, "junk"]})
            ),
            [0x1_4000_1000, 0x1_4000_2000]
        );
    }
}
