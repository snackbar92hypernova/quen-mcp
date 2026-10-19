-- quen ledger, migration 0011: play sessions. crates/quen-core/src/ledger.rs runs it in one
-- transaction while PRAGMA user_version is 10. Conventions as in 0001.
--
-- boots.mode: 'play' for a play session (session_launch mode play: the user plays, the agent drives only during a
-- hand-over); NULL for a quen test launch or a harness boot. measurements.world: what the world was doing during the
-- row's window, as quen saw it: 'user' (the user had the controls), 'live' (the agent had them, the world running),
-- 'paused' (quen's pause on), 'photo' (photo mode); NULL: not recorded (every row before 0011, and rows an agent ingests
-- without it).

ALTER TABLE boots ADD COLUMN mode TEXT CHECK (mode IN ('play'));
ALTER TABLE measurements ADD COLUMN world TEXT CHECK (world IN ('user', 'live', 'paused', 'photo'));

-- 0005's views (captures_with_boot as 0008 left it), with mode after outcome and measurements' world after run_id, and
-- mode among boot_comparison's differs: a play boot is never comparable with a test boot.
DROP VIEW boot_comparison;
DROP VIEW captures_with_boot;
DROP VIEW measurements_with_boot;
DROP VIEW boot_conditions;

CREATE VIEW boot_conditions AS
SELECT
  b.id AS boot_id,
  b.t_start,
  b.t_end,
  b.outcome,
  b.mode,
  b.game_id,
  g.name AS game,
  g.exe_sha256,
  g.build_stamp,
  b.mod_set_id,
  s.sha256 AS mod_set_sha256,
  b.save_id,
  b.settings_diff,
  b.patches,
  b.devkit,
  b.profiler,
  iif(b.busy_with IS NULL, NULL, json_array_length(b.busy_with) > 0) AS busy,
  b.busy_with,
  b.rebar,
  b.fg,
  b.vsync,
  b.display_state
FROM boots b
JOIN games g ON g.id = b.game_id
LEFT JOIN mod_sets s ON s.id = b.mod_set_id;

CREATE VIEW measurements_with_boot AS
SELECT
  m.id,
  m.boot_id,
  m.capture_id,
  m.metric,
  m.value,
  m.unit,
  m.state_label,
  m.method_family,
  m.tag,
  m.tag IN ('M', 'O', 'static_re') AS strong,
  m.run_id,
  m.world,
  m.note,
  c.path AS capture_path,
  b.t_start AS boot_t_start,
  b.outcome AS boot_outcome,
  b.mode AS boot_mode,
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
FROM measurements m
LEFT JOIN boot_conditions b ON b.boot_id = m.boot_id
LEFT JOIN captures c ON c.id = m.capture_id;

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
  b.mode AS boot_mode,
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
