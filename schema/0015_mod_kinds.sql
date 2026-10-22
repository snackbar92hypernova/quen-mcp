-- quen ledger, migration 0015: two mod kinds, bin and menu. crates/quen-core/src/ledger.rs runs it in one transaction
-- while PRAGMA user_version is 14. Conventions as in 0001.
--
-- A test can park a mod manager's mod (Vortex's) as a unit: every file of it the game loads, its other files in bin (a
-- proxy DLL, a data file) and its menu XML included. A boot's mod set lists them, so two boots that differ by such a
-- file get different sets. Kind `bin`: another file in bin that the manager's records name a mod's (its name: the file
-- name); kind `menu`: a mod's menu XML (its name: the file's stem). mod_set_entries, mod_versions and boot_mod_status
-- take them: each is rebuilt as 0002 made it with the wider CHECK, every row and column kept. boot_comparison reads
-- boot_mod_status, so it is dropped first and made again, 0011's view unchanged.

DROP VIEW boot_comparison;

CREATE TABLE mod_set_entries_0015 (
  mod_set_id    INTEGER NOT NULL REFERENCES mod_sets (id),
  kind          TEXT NOT NULL,
  name          TEXT NOT NULL,
  parked        INTEGER NOT NULL CHECK (parked IN (0, 1)),
  home          TEXT,
  content_hash  TEXT NOT NULL CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
  file_count    INTEGER CHECK (file_count >= 0),
  bytes         INTEGER CHECK (bytes >= 0),
  config_hash   TEXT CHECK (length(config_hash) = 64 AND config_hash NOT GLOB '*[^0-9a-f]*'),
  config_values TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_values) AND json_type(config_values) = 'object'),
  version_label TEXT,
  provenance    TEXT NOT NULL DEFAULT 'unknown' CHECK (provenance IN ('git_commit', 'nexus_file', 'unknown')),
  CHECK ((home IS NULL) = (file_count IS NULL) AND (file_count IS NULL) = (bytes IS NULL)),
  CHECK (home IS NULL OR kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc', 'bin', 'menu')),
  CHECK ((provenance = 'unknown') = (version_label IS NULL)),
  PRIMARY KEY (mod_set_id, kind, name)
) WITHOUT ROWID;

INSERT INTO mod_set_entries_0015 (mod_set_id, kind, name, parked, home, content_hash, file_count, bytes, config_hash,
                                  config_values, version_label, provenance)
SELECT mod_set_id, kind, name, parked, home, content_hash, file_count, bytes, config_hash, config_values, version_label,
       provenance
FROM mod_set_entries;

DROP TABLE mod_set_entries;
ALTER TABLE mod_set_entries_0015 RENAME TO mod_set_entries;
CREATE INDEX mod_set_entries_content ON mod_set_entries (content_hash);

CREATE TABLE mod_versions_0015 (
  content_hash TEXT PRIMARY KEY CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
  kind         TEXT NOT NULL CHECK (kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc', 'bin', 'menu')),
  name         TEXT NOT NULL,
  version      TEXT NOT NULL CHECK (trim(version) <> ''),
  provenance   TEXT NOT NULL CHECK (provenance IN ('git_commit', 'nexus_file')),
  source       TEXT NOT NULL CHECK (trim(source) <> '')
) WITHOUT ROWID;

INSERT INTO mod_versions_0015 (content_hash, kind, name, version, provenance, source)
SELECT content_hash, kind, name, version, provenance, source FROM mod_versions;

DROP TABLE mod_versions;
ALTER TABLE mod_versions_0015 RENAME TO mod_versions;

CREATE TABLE boot_mod_status_0015 (
  boot_id INTEGER NOT NULL REFERENCES boots (id),
  kind    TEXT NOT NULL CHECK (kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc', 'bin', 'menu')),
  name    TEXT NOT NULL,
  status  TEXT NOT NULL CHECK (status IN ('loaded', 'refused', 'not_present')),
  reason  TEXT,
  CHECK (status <> 'refused' OR trim(coalesce(reason, '')) <> ''),
  PRIMARY KEY (boot_id, kind, name)
) WITHOUT ROWID;

INSERT INTO boot_mod_status_0015 (boot_id, kind, name, status, reason)
SELECT boot_id, kind, name, status, reason FROM boot_mod_status;

DROP TABLE boot_mod_status;
ALTER TABLE boot_mod_status_0015 RENAME TO boot_mod_status;

-- 0011's view, unchanged.
CREATE VIEW boot_comparison AS
WITH pairs AS (
  SELECT
    a.boot_id AS a_id,
    b.boot_id AS b_id,
    ltrim(iif(a.mode IS NOT b.mode, ',mode', '')
      || iif(a.exe_sha256 <> b.exe_sha256, ',game_build', '')
      || iif(a.devkit <> b.devkit, ',devkit', '')
      || iif(substr(a.profiler, 1, instr(a.profiler || ':', ':') - 1)
             IS NOT substr(b.profiler, 1, instr(b.profiler || ':', ':') - 1), ',profiler', '')
      || iif(a.rebar IS NOT b.rebar, ',rebar', '')
      || iif(a.fg IS NOT b.fg, ',fg', '')
      || iif(a.vsync IS NOT b.vsync, ',vsync', '')
      || iif(a.display_state IS NOT b.display_state, ',display_state', ''), ',') AS differs,
    ltrim(iif(a.devkit IS NULL OR b.devkit IS NULL, ',devkit', '')
      || iif(a.busy IS NULL OR b.busy IS NULL, ',busy', ''), ',') AS unknown,
    ltrim(iif(a.busy, ',a', '') || iif(b.busy, ',b', ''), ',') AS busy,
    ltrim(iif(a.outcome IN ('failed', 'recovered'), ',a', '')
      || iif(b.outcome IN ('failed', 'recovered'), ',b', ''), ',') AS failed,
    ltrim(iif(a.mod_set_sha256 IS NOT b.mod_set_sha256, ',mods', '')
      || iif(EXISTS (SELECT kind, name, status FROM boot_mod_status WHERE boot_id = a.boot_id
                     EXCEPT SELECT kind, name, status FROM boot_mod_status WHERE boot_id = b.boot_id)
             OR EXISTS (SELECT kind, name, status FROM boot_mod_status WHERE boot_id = b.boot_id
                        EXCEPT SELECT kind, name, status FROM boot_mod_status WHERE boot_id = a.boot_id),
             ',mod_status', '')
      || iif(a.settings_diff IS NOT b.settings_diff, ',settings', '')
      || iif(a.save_id IS NOT b.save_id, ',save', '')
      || iif(a.patches IS NOT b.patches, ',patches', ''), ',') AS varies
  FROM boot_conditions a
  JOIN boot_conditions b ON b.boot_id <> a.boot_id
)
SELECT
  a_id,
  b_id,
  differs = '' AND unknown = '' AND busy = '' AND failed = '' AS comparable,
  differs,
  unknown,
  busy,
  failed,
  varies
FROM pairs;
