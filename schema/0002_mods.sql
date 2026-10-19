-- quen ledger, migration 0002: mod fingerprints. crates/quen-core/src/ledger.rs runs it in one
-- transaction while PRAGMA user_version is 1. Conventions as in 0001.
--
-- An entry's identity is kind + name (asi:W3Perf and addon:W3Perf can share a set), so mod_set_entries is rebuilt with
-- that key: SQLite can't change a primary key in place. Its rows keep their kind, name, parked flag and sha256 (now
-- content_hash); their new columns are left NULL, '{}' or 'unknown'. From here on a set's sha256 is mods::set_hash,
-- which also covers each entry's config hash.

CREATE TABLE mod_set_entries_0002 (
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
  -- From mod_versions by content_hash when the entry is added or the build is labeled later; no label means unknown.
  version_label TEXT,
  provenance    TEXT NOT NULL DEFAULT 'unknown' CHECK (provenance IN ('git_commit', 'nexus_file', 'unknown')),
  -- A row from 0001 has no home, file count or bytes, and keeps the free-text kind it had.
  CHECK ((home IS NULL) = (file_count IS NULL) AND (file_count IS NULL) = (bytes IS NULL)),
  CHECK (home IS NULL OR kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc')),
  CHECK ((provenance = 'unknown') = (version_label IS NULL)),
  PRIMARY KEY (mod_set_id, kind, name)
) WITHOUT ROWID;

INSERT INTO mod_set_entries_0002 (mod_set_id, kind, name, parked, content_hash)
SELECT mod_set_id, kind, name, parked, sha256 FROM mod_set_entries;

DROP TABLE mod_set_entries;
ALTER TABLE mod_set_entries_0002 RENAME TO mod_set_entries;
CREATE INDEX mod_set_entries_content ON mod_set_entries (content_hash);

-- A build's label, filled once when the user installs or updates a mod: our own reproducible builds map to a git
-- commit, third-party ones to the Nexus file they came from. The hash is the truth; this is only a label.
CREATE TABLE mod_versions (
  content_hash TEXT PRIMARY KEY CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
  kind         TEXT NOT NULL CHECK (kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc')),
  name         TEXT NOT NULL,
  version      TEXT NOT NULL CHECK (trim(version) <> ''),
  provenance   TEXT NOT NULL CHECK (provenance IN ('git_commit', 'nexus_file')),
  source       TEXT NOT NULL CHECK (trim(source) <> '') -- the commit, or the Nexus file name
) WITHOUT ROWID;

-- What each mod did at load in a boot, from its log or state export. Runtime state, kept apart from the files: an
-- installed ASI can refuse at load when the game code differs.
CREATE TABLE boot_mod_status (
  boot_id INTEGER NOT NULL REFERENCES boots (id),
  kind    TEXT NOT NULL CHECK (kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc')),
  name    TEXT NOT NULL,
  status  TEXT NOT NULL CHECK (status IN ('loaded', 'refused', 'not_present')),
  reason  TEXT,
  CHECK (status <> 'refused' OR trim(coalesce(reason, '')) <> ''),
  PRIMARY KEY (boot_id, kind, name)
) WITHOUT ROWID;
