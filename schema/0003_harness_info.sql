-- quen ledger, migration 0003: the Python harness's info dict for a boot it ran.
-- crates/quen-core/src/ledger.rs runs it in one transaction while PRAGMA user_version is 2. Conventions as in 0001.
--
-- While quen v1 wraps `ini_session.py boot`, and for the history the migrator imports, the parked list, the save the
-- harness loaded, the add-on sha256, the preset overrides and the step timings of a boot are only in that dict (boot
-- prints it as its log's JSON line and appends it to cap/boots.jsonl). It is kept as the harness wrote it.

ALTER TABLE boots ADD COLUMN harness_info TEXT
  CHECK (harness_info IS NULL OR (json_valid(harness_info) AND json_type(harness_info) = 'object'));
