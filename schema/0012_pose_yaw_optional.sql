-- quen ledger, migration 0012: a player position without a yaw. crates/quen-core/src/ledger.rs runs it in one
-- transaction while PRAGMA user_version is 11. Conventions as in 0001.
--
-- 0001 tied player_yaw to the position: the four player columns all set or all NULL. Now player_x, player_y and
-- player_z are all set or all NULL, and player_yaw may be NULL with them, never set without them. SQLite can't change
-- a table's CHECK in place, so poses is rebuilt with every row and id. PRAGMA foreign_keys can't change inside the
-- transaction, so the foreign keys are deferred instead: dropping poses counts the captures and sweep_keyframes rows
-- that point at a pose as violations, the copy back clears each one, and the commit refuses if any is left. No index or
-- trigger is on poses; captures_with_boot reads only its id and name, the same in the new table.

PRAGMA defer_foreign_keys = ON;

CREATE TABLE poses_0012 AS SELECT * FROM poses;
DROP TABLE poses;

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
     AND (player_yaw IS NULL OR player_z IS NOT NULL))
);

INSERT INTO poses (id, name, cam_x, cam_y, cam_z, cam_pitch, cam_yaw, cam_roll, fov, player_x, player_y, player_z,
                   player_yaw, save_id, ref_capture_id)
SELECT id, name, cam_x, cam_y, cam_z, cam_pitch, cam_yaw, cam_roll, fov, player_x, player_y, player_z, player_yaw,
       save_id, ref_capture_id
FROM poses_0012;

DROP TABLE poses_0012;
