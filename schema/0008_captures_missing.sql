-- quen ledger, migration 0008: a capture whose file was gone before capture_clean took it.
-- crates/quen-core/src/ledger.rs runs it in one transaction while PRAGMA user_version is 7. Conventions as in 0001.
--
-- Files removed by hand (batches 1-2's, before quen managed captures) left their rows with removed_at NULL. capture_clean
-- with missing: true marks such a row: removed_at when it marked it, removed_to 'missing' (it moved no file). SQLite
-- can't change a column's CHECK in place, so removed_to is replaced by a column with the wider CHECK, its values kept.

DROP VIEW captures_with_boot;
ALTER TABLE captures ADD COLUMN removed_to_0008 TEXT
  CHECK (removed_to_0008 IN ('recycle_bin', 'to_delete', 'missing'));
UPDATE captures SET removed_to_0008 = removed_to;
ALTER TABLE captures DROP COLUMN removed_to;
ALTER TABLE captures RENAME COLUMN removed_to_0008 TO removed_to;

-- 0007's view, unchanged.
CREATE VIEW captures_with_boot AS
SELECT
  c.id,
  c.boot_id,
  c.path,
  c.sha256,
  c.bytes,
  c.kind,
  c.method,
  c.encoding,
  c.includes_overlay,
  c.includes_desktop,
  c.display_state,
  c.frame_state,
  c.pose_id,
  p.name AS pose,
  c.t,
  c.pinned,
  c.removed_at,
  c.removed_to,
  b.t_start AS boot_t_start,
  b.outcome AS boot_outcome,
  b.game,
  b.exe_sha256,
  b.build_stamp,
  b.mod_set_id,
  b.mod_set_sha256,
  b.save_id,
  b.settings_diff,
  b.patches,
  b.devkit,
  b.profiler,
  b.busy,
  b.busy_with,
  b.rebar,
  b.fg,
  b.vsync,
  b.display_state AS boot_display_state
FROM captures c
LEFT JOIN poses p ON p.id = c.pose_id
LEFT JOIN boot_conditions b ON b.boot_id = c.boot_id;
