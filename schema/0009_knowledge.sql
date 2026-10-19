-- quen ledger, migration 0009: game knowledge. Named things (entities) and where they sit per
-- build, typed links between them on one basis each, every answered REA query (extractions, the cache keyed by
-- everything that determines an answer, each kept whole in a bundle file), the KB's prose index (sections and the
-- entities they mention), and the claim grades. crates/quen-core/src/ledger.rs runs it in one transaction while PRAGMA
-- user_version is 8. Conventions as in 0001; kind and rel are lowercase identifiers, with their vocabularies in
-- schema/README.md. Nothing here is game-specific: every row is.

-- An entity's identity is (game, kind, key); game is the title (games.name), not a build. id never leaves the ledger.
CREATE TABLE entities (
  id    INTEGER PRIMARY KEY,
  game  TEXT NOT NULL CHECK (trim(game) <> ''),
  kind  TEXT NOT NULL CHECK (kind <> '' AND kind NOT GLOB '*[^a-z0-9_]*'),
  key   TEXT NOT NULL CHECK (trim(key) <> ''),
  label TEXT,
  t     TEXT NOT NULL CHECK (t IS strftime('%Y-%m-%dT%H:%M:%SZ', t)),
  UNIQUE (game, kind, key)
);

-- One REA analysis profile (or a decompiler's name and version), stored once.
CREATE TABLE extraction_profiles (
  digest     TEXT PRIMARY KEY CHECK (length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'),
  tool       TEXT NOT NULL CHECK (tool <> '' AND tool NOT GLOB '*[^a-z0-9_]*'),
  provider   TEXT NOT NULL CHECK (json_valid(provider) AND json_type(provider) = 'object'),
  parameters TEXT NOT NULL CHECK (json_valid(parameters) AND json_type(parameters) = 'object')
) WITHOUT ROWID;

-- One answered tool query. key_sha256 = sha256 of the canonical JSON {operation, parameters, profile_digest,
-- target_sha256}: the same key is the same query on the same analysis. tool_version is stored, not keyed.
CREATE TABLE extractions (
  id             INTEGER PRIMARY KEY,
  tool           TEXT NOT NULL CHECK (tool <> '' AND tool NOT GLOB '*[^a-z0-9_]*'),
  tool_version   TEXT,
  target_sha256  TEXT NOT NULL CHECK (length(target_sha256) = 64 AND target_sha256 NOT GLOB '*[^0-9a-f]*'),
  profile_digest TEXT NOT NULL REFERENCES extraction_profiles (digest),
  operation      TEXT NOT NULL CHECK (trim(operation) <> ''),
  params         TEXT NOT NULL CHECK (json_valid(params) AND json_type(params) = 'object'),
  key_sha256     TEXT NOT NULL CHECK (length(key_sha256) = 64 AND key_sha256 NOT GLOB '*[^0-9a-f]*'),
  evidence_id    TEXT UNIQUE CHECK (evidence_id IS NULL
                                    OR (length(evidence_id) = 67 AND substr(evidence_id, 1, 3) = 'ev_'
                                        AND substr(evidence_id, 4) NOT GLOB '*[^0-9a-f]*')),
  result_sha256  TEXT NOT NULL CHECK (length(result_sha256) = 64 AND result_sha256 NOT GLOB '*[^0-9a-f]*'),
  confidence     TEXT CHECK (confidence IN ('observed', 'derived', 'inferred')),
  rva            INTEGER CHECK (rva >= 0),
  entity_id      INTEGER REFERENCES entities (id),
  capture_id     INTEGER NOT NULL REFERENCES captures (id),
  t              TEXT NOT NULL CHECK (t IS strftime('%Y-%m-%dT%H:%M:%SZ', t))
);
CREATE INDEX extractions_key ON extractions (key_sha256);
CREATE INDEX extractions_target ON extractions (target_sha256, rva);
CREATE INDEX extractions_entity ON extractions (entity_id);

-- Where an entity sits in one build, on exactly one basis. Rows with kb_source are replaced by each KB scan.
CREATE TABLE entity_addresses (
  entity_id     INTEGER NOT NULL REFERENCES entities (id),
  game_id       INTEGER NOT NULL REFERENCES games (id),
  rva           INTEGER CHECK (rva >= 0),
  offset        INTEGER CHECK (offset >= 0),
  size          INTEGER CHECK (size >= 0),
  bytes_len     INTEGER CHECK (bytes_len > 0),
  bytes_sha256  TEXT CHECK (bytes_sha256 IS NULL
                            OR (length(bytes_sha256) = 64 AND bytes_sha256 NOT GLOB '*[^0-9a-f]*')),
  kb_source     TEXT CHECK (kb_source IS NULL OR trim(kb_source) <> ''),
  extraction_id INTEGER REFERENCES extractions (id),
  claim_id      INTEGER REFERENCES claims (id),
  confidence    TEXT,
  PRIMARY KEY (entity_id, game_id),
  CHECK ((rva IS NULL) <> (offset IS NULL)),
  CHECK ((bytes_len IS NULL) = (bytes_sha256 IS NULL)),
  CHECK ((kb_source IS NOT NULL) + (extraction_id IS NOT NULL) + (claim_id IS NOT NULL) = 1)
) WITHOUT ROWID;
CREATE INDEX entity_addresses_rva ON entity_addresses (game_id, rva);

-- A typed relation between two entities, on exactly one basis: a claim (as proven as it is), an extraction (the
-- tool's own confidence) or a curated KB table row at a commit (kb_source '<file>@<commit>'). Rows with kb_source are
-- replaced by each KB scan; no other link is ever deleted.
CREATE TABLE links (
  id            INTEGER PRIMARY KEY,
  src_id        INTEGER NOT NULL REFERENCES entities (id),
  dst_id        INTEGER NOT NULL REFERENCES entities (id),
  rel           TEXT NOT NULL CHECK (rel <> '' AND rel NOT GLOB '*[^a-z0-9_]*'),
  claim_id      INTEGER REFERENCES claims (id),
  extraction_id INTEGER REFERENCES extractions (id),
  kb_source     TEXT CHECK (kb_source IS NULL OR trim(kb_source) <> ''),
  confidence    TEXT CHECK (claim_id IS NULL OR confidence IS NULL),
  t             TEXT NOT NULL CHECK (t IS strftime('%Y-%m-%dT%H:%M:%SZ', t)),
  CHECK (src_id <> dst_id),
  CHECK ((kb_source IS NOT NULL) + (extraction_id IS NOT NULL) + (claim_id IS NOT NULL) = 1)
);
CREATE UNIQUE INDEX links_basis ON links (src_id, rel, dst_id, coalesce(claim_id, 0), coalesce(extraction_id, 0),
                                          coalesce(kb_source, ''));
CREATE INDEX links_dst ON links (dst_id);

-- What a claim is about.
CREATE TABLE claim_subjects (
  claim_id  INTEGER NOT NULL REFERENCES claims (id),
  entity_id INTEGER NOT NULL REFERENCES entities (id),
  PRIMARY KEY (claim_id, entity_id)
) WITHOUT ROWID;
CREATE INDEX claim_subjects_entity ON claim_subjects (entity_id);

-- The KB's prose index: one row per heading section of a note (H2 / H3; anchor '' is the text before the first H2).
-- The text stays in git: `git show <kb_commit>:<path>`, lines line_start..line_end. A section a scan no longer finds
-- keeps its row, with gone_at.
CREATE TABLE kb_sections (
  id         INTEGER PRIMARY KEY,
  path       TEXT NOT NULL CHECK (trim(path) <> '' AND instr(path, '\') = 0 AND substr(path, 1, 1) <> '/'),
  anchor     TEXT NOT NULL,
  heading    TEXT,
  level      INTEGER NOT NULL CHECK (level BETWEEN 1 AND 3),
  parent_id  INTEGER REFERENCES kb_sections (id),
  line_start INTEGER NOT NULL CHECK (line_start >= 1),
  line_end   INTEGER NOT NULL CHECK (line_end >= line_start),
  sha256     TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  kb_commit  TEXT CHECK (kb_commit IS NULL OR (length(kb_commit) IN (40, 64) AND kb_commit NOT GLOB '*[^0-9a-f]*')),
  scanned_at TEXT NOT NULL CHECK (scanned_at IS strftime('%Y-%m-%dT%H:%M:%SZ', scanned_at)),
  gone_at    TEXT CHECK (gone_at IS strftime('%Y-%m-%dT%H:%M:%SZ', gone_at)),
  UNIQUE (path, anchor)
);

-- The entities a section names, from the last scan of its note.
CREATE TABLE kb_mentions (
  section_id INTEGER NOT NULL REFERENCES kb_sections (id),
  entity_id  INTEGER NOT NULL REFERENCES entities (id),
  n          INTEGER NOT NULL CHECK (n >= 1),
  PRIMARY KEY (section_id, entity_id)
) WITHOUT ROWID;
CREATE INDEX kb_mentions_entity ON kb_mentions (entity_id);

-- The author's grades and a stable key on claims. Grades are labels: they never count towards proof, and
-- claims_status is unchanged.
ALTER TABLE claims ADD COLUMN key TEXT CHECK (key IS NULL OR trim(key) <> '');
ALTER TABLE claims ADD COLUMN confidence TEXT CHECK (confidence IN ('H', 'M', 'L'));
ALTER TABLE claims ADD COLUMN review TEXT CHECK (review IN ('V', 'C', 'U'));
CREATE UNIQUE INDEX claims_key ON claims (key) WHERE key IS NOT NULL;

-- The extraction a static_re (or decompiler) measurement was read from.
ALTER TABLE measurements ADD COLUMN extraction_id INTEGER REFERENCES extractions (id);

-- Each link with its basis's provenance and standing, one row per link. A claim-backed link is exactly as proven as
-- its claim.
CREATE VIEW links_evidence AS
SELECT
  l.id,
  l.rel,
  l.src_id,
  s.kind AS src_kind,
  s.key AS src_key,
  l.dst_id,
  d.kind AS dst_kind,
  d.key AS dst_key,
  CASE WHEN l.claim_id IS NOT NULL THEN 'claim' WHEN l.extraction_id IS NOT NULL THEN 'extraction' ELSE 'kb' END
    AS basis,
  l.claim_id,
  c.key AS claim_key,
  c.status AS claim_status,
  e.proven,
  e.families,
  e.contradictions,
  e.supported_by,
  l.extraction_id,
  x.tool,
  x.tool_version,
  x.evidence_id,
  x.target_sha256,
  l.kb_source,
  l.confidence
FROM links l
JOIN entities s ON s.id = l.src_id
JOIN entities d ON d.id = l.dst_id
LEFT JOIN claims c ON c.id = l.claim_id
LEFT JOIN claims_evidence e ON e.id = l.claim_id
LEFT JOIN extractions x ON x.id = l.extraction_id;
