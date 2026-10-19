-- quen ledger, migration 0010: the saves quen itself made. crates/quen-core/src/ledger.rs runs it
-- in one transaction while PRAGMA user_version is 9. Conventions as in 0001.
--
-- game_save writes a new manual save through quen's in-game package: its row names the boot it was made in. NULL: a
-- save quen didn't make (the game's own, the user's, or one from before this migration).

ALTER TABLE saves ADD COLUMN made_by_boot INTEGER REFERENCES boots (id);
