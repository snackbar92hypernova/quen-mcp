-- quen ledger, migration 0001 (docs/intent.md, "The ledger"). crates/quen-core/src/ledger.rs runs it in one
-- transaction while PRAGMA user_version is below 1.
-- Conventions: times are UTC 'YYYY-MM-DDTHH:MM:SSZ' text, so they sort as text; sha256 values are 64 lowercase hex
-- digits; JSON columns hold JSON text; capture paths are relative to the capture root, with '/' separators.
-- Nothing in quen deletes rows from these tables, except that a doc-reference scan replaces the rows of the docs it
-- read.

PRAGMA application_id = 1903519086; -- 0x7175656E, 'quen': marks the file as a ledger

CREATE TABLE games (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  exe_path    TEXT NOT NULL,
  exe_sha256  TEXT NOT NULL UNIQUE CHECK (length(exe_sha256) = 64 AND exe_sha256 NOT GLOB '*[^0-9a-f]*'),
  build_stamp TEXT
);

-- A mod set is identified by the sha256 of its sorted entries (ledger/ingest.rs), so boots with the same set share
-- a row.
CREATE TABLE mod_sets (
  id     INTEGER PRIMARY KEY,
  sha256 TEXT NOT NULL UNIQUE CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE TABLE mod_set_entries (
  mod_set_id INTEGER NOT NULL REFERENCES mod_sets (id),
  name       TEXT NOT NULL,
  kind       TEXT NOT NULL,
  sha256     TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  parked     INTEGER NOT NULL CHECK (parked IN (0, 1)),
  PRIMARY KEY (mod_set_id, name)
) WITHOUT ROWID;

CREATE TABLE saves (
  id              INTEGER PRIMARY KEY,
  game_id         INTEGER NOT NULL REFERENCES games (id),
  file            TEXT NOT NULL,
  sha256          TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  note            TEXT,
  tainted_by_boot INTEGER REFERENCES boots (id), -- written after a memory patch, cvar write or teleport in that boot
  UNIQUE (game_id, file, sha256)
);

-- A boot is started (t_end and outcome NULL) and later finished; reviewed_at is set when the user has looked at it.
CREATE TABLE boots (
  id            INTEGER PRIMARY KEY,
  game_id       INTEGER NOT NULL REFERENCES games (id),
  t_start       TEXT NOT NULL CHECK (t_start IS strftime('%Y-%m-%dT%H:%M:%SZ', t_start)),
  t_end         TEXT CHECK (t_end IS strftime('%Y-%m-%dT%H:%M:%SZ', t_end) AND t_end >= t_start),
  save_id       INTEGER REFERENCES saves (id),
  mod_set_id    INTEGER REFERENCES mod_sets (id),
  settings_diff TEXT CHECK (settings_diff IS NULL
                            OR (json_valid(settings_diff) AND json_type(settings_diff) = 'object')),
  devkit        INTEGER CHECK (devkit IN (0, 1)),
  rebar         INTEGER CHECK (rebar IN (0, 1)),
  fg            INTEGER CHECK (fg IN (0, 1)),
  vsync         INTEGER CHECK (vsync IN (0, 1)),
  profiler      TEXT,
  display_state TEXT CHECK (display_state IS NULL
                            OR (json_valid(display_state) AND json_type(display_state) = 'object')),
  patches       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(patches) AND json_type(patches) = 'array'),
  outcome       TEXT CHECK (outcome IN ('ok', 'failed', 'recovered')),
  failure       TEXT,
  journal_path  TEXT,
  reviewed_at   TEXT CHECK (reviewed_at IS strftime('%Y-%m-%dT%H:%M:%SZ', reviewed_at)),
  CHECK ((t_end IS NULL) = (outcome IS NULL)),
  CHECK (outcome IS NOT 'failed' OR failure IS NOT NULL),
  CHECK (reviewed_at IS NULL OR (t_end IS NOT NULL AND reviewed_at >= t_end))
);

-- Rotation is pitch, yaw, roll in degrees. The player columns are all set or all NULL.
CREATE TABLE poses (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL UNIQUE,
  cam_x          REAL NOT NULL,
  cam_y          REAL NOT NULL,
  cam_z          REAL NOT NULL,
  cam_pitch      REAL NOT NULL,
  cam_yaw        REAL NOT NULL,
  cam_roll       REAL NOT NULL,
  fov            REAL NOT NULL,
  player_x       REAL,
  player_y       REAL,
  player_z       REAL,
  player_yaw     REAL,
  save_id        INTEGER REFERENCES saves (id),
  ref_capture_id INTEGER REFERENCES captures (id),
  CHECK ((player_x IS NULL) = (player_y IS NULL) AND (player_y IS NULL) = (player_z IS NULL)
     AND (player_z IS NULL) = (player_yaw IS NULL))
);

-- boot_id is NULL for files whose boot isn't known (migrated ones); those are never cleanup candidates.
CREATE TABLE captures (
  id               INTEGER PRIMARY KEY,
  boot_id          INTEGER REFERENCES boots (id),
  path             TEXT NOT NULL UNIQUE COLLATE NOCASE,
  sha256           TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  bytes            INTEGER NOT NULL CHECK (bytes >= 0),
  kind             TEXT NOT NULL,
  method           TEXT,
  encoding         TEXT CHECK (encoding IN ('srgb8', 'pq_bt2020_u16', 'pq_codes_f16', 'scrgb_f16')),
  includes_overlay INTEGER CHECK (includes_overlay IN (0, 1)),
  includes_desktop INTEGER CHECK (includes_desktop IN (0, 1)),
  display_state    TEXT CHECK (display_state IS NULL
                               OR (json_valid(display_state) AND json_type(display_state) = 'object')),
  frame_state      TEXT CHECK (frame_state IS NULL
                               OR (json_valid(frame_state) AND json_type(frame_state) = 'object')),
  pose_id          INTEGER REFERENCES poses (id),
  t                TEXT NOT NULL CHECK (t IS strftime('%Y-%m-%dT%H:%M:%SZ', t)),
  pinned           INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1))
);
CREATE INDEX captures_boot ON captures (boot_id);

-- method_family is a lowercase identifier, so 'PresentMon' and 'presentmon' can't pass as two independent methods.
CREATE TABLE measurements (
  id            INTEGER PRIMARY KEY,
  boot_id       INTEGER REFERENCES boots (id),
  capture_id    INTEGER REFERENCES captures (id),
  metric        TEXT NOT NULL,
  value         REAL,
  unit          TEXT,
  state_label   TEXT,
  method_family TEXT NOT NULL CHECK (method_family <> '' AND method_family NOT GLOB '*[^a-z0-9_]*'),
  tag           TEXT NOT NULL CHECK (tag IN ('M', 'O', 'I', 'U', 'static_re')),
  run_id        TEXT,
  note          TEXT
);
CREATE INDEX measurements_boot ON measurements (boot_id);

-- Claims store only open / withdrawn; "proven" is derived by the claims_status view below.
CREATE TABLE claims (
  id               INTEGER PRIMARY KEY,
  text             TEXT NOT NULL CHECK (trim(text) <> ''),
  scope            TEXT,
  status           TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'withdrawn')),
  withdrawn_reason TEXT,
  CHECK ((status = 'withdrawn') = (trim(coalesce(withdrawn_reason, '')) <> ''))
);

CREATE TABLE claim_support (
  claim_id       INTEGER NOT NULL REFERENCES claims (id),
  measurement_id INTEGER NOT NULL REFERENCES measurements (id),
  supports       INTEGER NOT NULL CHECK (supports IN (0, 1)), -- 0: the measurement contradicts the claim
  PRIMARY KEY (claim_id, measurement_id)
) WITHOUT ROWID;

-- One row per (reference, matched capture); a reference that matches nothing is one row with dangling = 1.
CREATE TABLE doc_refs (
  id         INTEGER PRIMARY KEY,
  doc_path   TEXT NOT NULL,
  line       INTEGER NOT NULL CHECK (line >= 1),
  raw        TEXT NOT NULL, -- the reference as written
  capture_id INTEGER REFERENCES captures (id),
  dangling   INTEGER NOT NULL CHECK (dangling = (capture_id IS NULL)),
  t_scanned  TEXT NOT NULL CHECK (t_scanned IS strftime('%Y-%m-%dT%H:%M:%SZ', t_scanned))
);
CREATE INDEX doc_refs_doc ON doc_refs (doc_path);
CREATE INDEX doc_refs_capture ON doc_refs (capture_id);

CREATE TABLE re_refs (
  id             INTEGER PRIMARY KEY,
  measurement_id INTEGER REFERENCES measurements (id),
  claim_id       INTEGER REFERENCES claims (id),
  kb_path        TEXT NOT NULL,
  CHECK ((measurement_id IS NULL) <> (claim_id IS NULL))
);

-- A claim is proven when it is open, has supporting rows from at least two distinct method
-- families tagged M, O or static_re, and no standing contradiction. A contradiction (supports = 0, any tag) stands
-- unless both it and the newest such support have a boot and its boot started earlier, i.e. newer support outranks
-- it; contradictions from the same boot, from no boot, or against supports that have no boot always stand.
CREATE VIEW claims_status AS
WITH evidence AS (
  SELECT s.claim_id, s.supports, m.method_family, m.tag IN ('M', 'O', 'static_re') AS strong, b.t_start
  FROM claim_support s
  JOIN measurements m ON m.id = s.measurement_id
  LEFT JOIN boots b ON b.id = m.boot_id
),
support AS (
  SELECT claim_id, count(DISTINCT method_family) AS families, max(t_start) AS newest
  FROM evidence
  WHERE supports = 1 AND strong
  GROUP BY claim_id
),
standing AS (
  SELECT e.claim_id, count(*) AS contradictions
  FROM evidence e
  LEFT JOIN support s ON s.claim_id = e.claim_id
  WHERE e.supports = 0 AND NOT (e.t_start IS NOT NULL AND s.newest IS NOT NULL AND e.t_start < s.newest)
  GROUP BY e.claim_id
)
SELECT
  c.id,
  c.text,
  c.scope,
  c.status,
  c.withdrawn_reason,
  coalesce(s.families, 0) AS families,
  coalesce(k.contradictions, 0) AS contradictions,
  c.status = 'open' AND coalesce(s.families, 0) >= 2 AND k.contradictions IS NULL AS proven
FROM claims c
LEFT JOIN support s ON s.claim_id = c.id
LEFT JOIN standing k ON k.claim_id = c.id;
