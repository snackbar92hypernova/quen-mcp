-- quen ledger, migration 0017: the game time and weather a pose was seen in. crates/quen-core/src/ledger.rs runs it in
-- one transaction while PRAGMA user_version is 16. Conventions as in 0001.
--
-- game_pose saves them beside the camera and the player, and restores them: game_day and game_hour (game_settime's
-- arguments: both set or both NULL) and weather (the weather's name). NULL: not recorded, as in every pose before this
-- migration. ADD COLUMN keeps every row and id; its CHECKs hold for the rows already there, all NULL.

ALTER TABLE poses ADD COLUMN game_day INTEGER CHECK (game_day >= 0);
ALTER TABLE poses ADD COLUMN game_hour INTEGER
  CHECK ((game_hour IS NULL) = (game_day IS NULL) AND game_hour BETWEEN 0 AND 23);
ALTER TABLE poses ADD COLUMN weather TEXT CHECK (weather <> '');
