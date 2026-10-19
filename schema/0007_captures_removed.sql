-- quen ledger, migration 0007: a capture whose file capture_clean took off the disk.
-- crates/quen-core/src/ledger.rs runs it in one transaction while PRAGMA user_version is 6. Conventions as in 0001.
--
-- The row stays, with its path and sha256, so a claim keeps the record of its evidence. removed_at says when the file
-- went, removed_to where: the Recycle Bin, or <data root>\_to_delete\<date>\ for the user to empty (what the Recycle
-- Bin can't take). Both are NULL while the file is where path says.

ALTER TABLE captures ADD COLUMN removed_at TEXT
  CHECK (removed_at IS NULL OR removed_at IS strftime('%Y-%m-%dT%H:%M:%SZ', removed_at));
ALTER TABLE captures ADD COLUMN removed_to TEXT CHECK (removed_to IN ('recycle_bin', 'to_delete'));

-- 0005's view, with the two columns after pinned.
DROP VIEW captures_with_boot;
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
