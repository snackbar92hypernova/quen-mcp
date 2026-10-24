-- quen ledger, migration 0016: provenance `other`. crates/quen-core/src/ledger.rs runs it in one transaction while
-- PRAGMA user_version is 15. Conventions as in 0001.
--
-- A build neither ours nor from Nexus (one built from someone else's fork, its commit unknown) can be labelled:
-- provenance `other`, its source free text naming where the build came from. mod_versions and mod_set_entries take it:
-- each is rebuilt as 0015 made it with the wider CHECK, every row and column kept, and mod_set_entries' index made
-- again. No view or trigger reads either table.

CREATE TABLE mod_set_entries_0016 (
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
  provenance    TEXT NOT NULL DEFAULT 'unknown' CHECK (provenance IN ('git_commit', 'nexus_file', 'other', 'unknown')),
  CHECK ((home IS NULL) = (file_count IS NULL) AND (file_count IS NULL) = (bytes IS NULL)),
  CHECK (home IS NULL OR kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc', 'bin', 'menu')),
  CHECK ((provenance = 'unknown') = (version_label IS NULL)),
  PRIMARY KEY (mod_set_id, kind, name)
) WITHOUT ROWID;

INSERT INTO mod_set_entries_0016 (mod_set_id, kind, name, parked, home, content_hash, file_count, bytes, config_hash,
                                  config_values, version_label, provenance)
SELECT mod_set_id, kind, name, parked, home, content_hash, file_count, bytes, config_hash, config_values, version_label,
       provenance
FROM mod_set_entries;

DROP TABLE mod_set_entries;
ALTER TABLE mod_set_entries_0016 RENAME TO mod_set_entries;
CREATE INDEX mod_set_entries_content ON mod_set_entries (content_hash);

CREATE TABLE mod_versions_0016 (
  content_hash TEXT PRIMARY KEY CHECK (length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'),
  kind         TEXT NOT NULL CHECK (kind IN ('mods', 'asi', 'addon', 'reshade', 'dlc', 'bin', 'menu')),
  name         TEXT NOT NULL,
  version      TEXT NOT NULL CHECK (trim(version) <> ''),
  provenance   TEXT NOT NULL CHECK (provenance IN ('git_commit', 'nexus_file', 'other')),
  source       TEXT NOT NULL CHECK (trim(source) <> '') -- the commit, the Nexus file name, or where it came from
) WITHOUT ROWID;

INSERT INTO mod_versions_0016 (content_hash, kind, name, version, provenance, source)
SELECT content_hash, kind, name, version, provenance, source FROM mod_versions;

DROP TABLE mod_versions;
ALTER TABLE mod_versions_0016 RENAME TO mod_versions;
