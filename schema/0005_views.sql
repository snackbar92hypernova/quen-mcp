-- quen ledger, migration 0005: the busy-machine record on boots, and the views an agent uses to
-- read results from quen and other tools together. crates/quen-core/src/ledger.rs runs it in one transaction while
-- PRAGMA user_version is 4. Conventions as in 0001; schema/README.md (ledger::schema_description) documents every table
-- and view for the agents that query them. A list in a view is comma-separated text, '' when empty.

-- What the quiet-machine checks during the boot found running that skews the numbers (compilers, RE analysis), as a
-- JSON array of strings: [] when every check found the machine quiet, NULL when no check was recorded (every boot
-- before 0005).
ALTER TABLE boots ADD COLUMN busy_with TEXT
  CHECK (busy_with IS NULL OR (json_valid(busy_with) AND json_type(busy_with) = 'array'));

-- One row per boot with the conditions its numbers depend on. busy is NULL when no check was recorded.
CREATE VIEW boot_conditions AS
SELECT
  b.id AS boot_id,
  b.t_start,
  b.t_end,
  b.outcome,
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

-- Each measurement beside its boot's conditions (NULL for a row without a boot) and its capture's path.
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
  m.note,
  c.path AS capture_path,
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
FROM measurements m
LEFT JOIN boot_conditions b ON b.boot_id = m.boot_id
LEFT JOIN captures c ON c.id = m.capture_id;

-- Each capture beside its pose's name and its boot's conditions (NULL for a file without a boot).
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

-- claims_status beside the method families behind each claim. supported_by: the families whose support counts (tag M,
-- O or static_re), the ones claims_status.families counts; weak_support: families whose only support is tagged I or U,
-- which never counts; contradicted_by: every family with a contradicting row, standing or outranked.
CREATE VIEW claims_evidence AS
WITH families AS (
  SELECT
    s.claim_id,
    m.method_family,
    max(s.supports = 1 AND m.tag IN ('M', 'O', 'static_re')) AS strong,
    max(s.supports = 1) AS supports,
    max(s.supports = 0) AS contradicts,
    count(*) AS measurements
  FROM claim_support s
  JOIN measurements m ON m.id = s.measurement_id
  GROUP BY s.claim_id, m.method_family
),
lists AS (
  SELECT
    claim_id,
    group_concat(method_family, ',' ORDER BY method_family) FILTER (WHERE strong) AS supported_by,
    group_concat(method_family, ',' ORDER BY method_family) FILTER (WHERE supports AND NOT strong) AS weak_support,
    group_concat(method_family, ',' ORDER BY method_family) FILTER (WHERE contradicts) AS contradicted_by,
    sum(measurements) AS measurements
  FROM families
  GROUP BY claim_id
)
SELECT
  c.id,
  c.text,
  c.scope,
  c.status,
  c.withdrawn_reason,
  c.proven,
  c.families,
  c.contradictions,
  coalesce(l.supported_by, '') AS supported_by,
  coalesce(l.weak_support, '') AS weak_support,
  coalesce(l.contradicted_by, '') AS contradicted_by,
  coalesce(l.measurements, 0) AS measurements,
  (SELECT count(DISTINCT m.boot_id) FROM claim_support s JOIN measurements m ON m.id = s.measurement_id
   WHERE s.claim_id = c.id) AS boots,
  coalesce((SELECT group_concat(DISTINCT r.kb_path ORDER BY r.kb_path) FROM re_refs r WHERE r.claim_id = c.id), '')
    AS kb_paths
FROM claims_status c
LEFT JOIN lists l ON l.claim_id = c.id;

-- Every ordered pair of distinct boots, and whether their numbers can be compared directly. differs: conditions both
-- boots recorded, with different values (game_build, devkit, profiler, rebar, fg, vsync, display_state). For profiler,
-- the tool before ':' counts (nsys:run_1 and nsys:run_2 are one profiler), and NULL (none attached) is a value, as it
-- is for the last four (not forced, not recorded). unknown: conditions a side didn't record (devkit, busy). busy: the
-- sides whose machine was busy (a, b); failed: the sides whose boot failed or was recovered, so may have stopped
-- part-way. varies: what a test changes on purpose (mods, mod_status, settings, save, patches). comparable only when
-- differs, unknown, busy and failed are all ''.
CREATE VIEW boot_comparison AS
WITH pairs AS (
  SELECT
    a.boot_id AS a_id,
    b.boot_id AS b_id,
    ltrim(iif(a.exe_sha256 <> b.exe_sha256, ',game_build', '')
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

-- One row per run and metric of an A/B run, from any tool's rows that carry a run_id and state
-- labels: per state, its windows (rows), their mean and drift (their spread, max - min). The first state is the one
-- recorded first. As measure_ab decides it, the verdict needs exactly two states with two windows or more each, and is
-- 'outside drift' only if |diff| exceeds both drifts; otherwise it is NULL.
CREATE VIEW ab_runs AS
WITH windows AS (
  SELECT
    boot_id,
    run_id,
    metric,
    method_family,
    state_label,
    min(id) AS first_id,
    count(*) AS windows,
    avg(value) AS mean,
    max(value) - min(value) AS drift,
    max(unit) AS unit
  FROM measurements
  WHERE run_id IS NOT NULL AND state_label IS NOT NULL AND value IS NOT NULL
  GROUP BY boot_id, run_id, metric, method_family, state_label
),
states AS (
  SELECT
    *,
    row_number() OVER (run ORDER BY first_id) AS k,
    count(*) OVER run AS states,
    min(windows) OVER run AS fewest
  FROM windows
  WINDOW run AS (PARTITION BY boot_id, run_id, metric, method_family)
)
SELECT
  a.boot_id,
  a.run_id,
  a.metric,
  a.method_family,
  a.unit,
  a.states,
  a.state_label AS first_label,
  a.windows AS first_windows,
  a.mean AS first_mean,
  a.drift AS first_drift,
  b.state_label AS second_label,
  b.windows AS second_windows,
  b.mean AS second_mean,
  b.drift AS second_drift,
  b.mean - a.mean AS diff,
  iif(a.states = 2 AND a.fewest >= 2,
      iif(abs(b.mean - a.mean) > max(a.drift, b.drift), 'outside drift', 'inside drift'), NULL) AS verdict,
  c.t_start AS boot_t_start,
  c.devkit,
  c.profiler,
  c.busy
FROM states a
LEFT JOIN states b
  ON b.boot_id IS a.boot_id AND b.run_id = a.run_id AND b.metric = a.metric AND b.method_family = a.method_family
     AND b.k = 2
LEFT JOIN boot_conditions c ON c.boot_id = a.boot_id
WHERE a.k = 1;
