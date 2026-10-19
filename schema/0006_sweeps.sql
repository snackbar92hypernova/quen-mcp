-- quen ledger, migration 0006: camera sweeps, the poses their keyframes were recorded
-- as, and their playbacks, frame by frame. crates/quen-core/src/ledger.rs runs it in one transaction after 0005 (the
-- views, from another branch: this one is numbered 0006 so the two merge in that order). Conventions as in 0001.
--
-- A sweep is its canonical JSON (quen-core sweep.rs: angles in (-180, 180], every number on a 0.0001 grid), and its
-- sha256 names the path, so the same sweep under two names shows. A sweep row never changes: a run plays exactly the
-- row it names, so the frames of two runs (a mod on, then off) compare frame by frame.

CREATE TABLE sweeps (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL UNIQUE CHECK (trim(name) <> ''),
  sha256  TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  sweep   TEXT NOT NULL CHECK (json_valid(sweep) AND json_type(sweep) = 'object'
                               AND json_array_length(sweep, '$.keyframes') >= 2),
  save_id INTEGER REFERENCES saves (id), -- the save the sweep's world loads
  note    TEXT
);
CREATE INDEX sweeps_sha256 ON sweeps (sha256);

-- The pose row a keyframe was recorded as, counting keyframes from 0; the pose's reference capture is the screenshot
-- it was checked on.
CREATE TABLE sweep_keyframes (
  sweep_id INTEGER NOT NULL REFERENCES sweeps (id),
  keyframe INTEGER NOT NULL CHECK (keyframe >= 0),
  pose_id  INTEGER NOT NULL REFERENCES poses (id),
  PRIMARY KEY (sweep_id, keyframe)
) WITHOUT ROWID;

-- One playback of a sweep: in real time, or frame step at fps with its frames in sweep_frames.
CREATE TABLE sweep_runs (
  id       INTEGER PRIMARY KEY,
  sweep_id INTEGER NOT NULL REFERENCES sweeps (id),
  boot_id  INTEGER REFERENCES boots (id),
  mode     TEXT NOT NULL CHECK (mode IN ('realtime', 'step')),
  fps      INTEGER CHECK (fps BETWEEN 1 AND 240),
  t        TEXT NOT NULL CHECK (t IS strftime('%Y-%m-%dT%H:%M:%SZ', t)),
  CHECK ((mode = 'step') = (fps IS NOT NULL))
);
CREATE INDEX sweep_runs_sweep ON sweep_runs (sweep_id);

-- Frame n of a frame-step run shows its sweep at n / fps seconds.
CREATE TABLE sweep_frames (
  run_id     INTEGER NOT NULL REFERENCES sweep_runs (id),
  frame      INTEGER NOT NULL CHECK (frame >= 0),
  capture_id INTEGER NOT NULL UNIQUE REFERENCES captures (id),
  PRIMARY KEY (run_id, frame)
) WITHOUT ROWID;
