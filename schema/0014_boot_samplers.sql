-- quen ledger, migration 0014: the samplers a boot's measurement windows ran with. crates/quen-core/src/ledger.rs runs
-- it in one transaction while PRAGMA user_version is 13. Conventions as in 0001.
--
-- boots.samplers (JSON array of text, sorted, each once): every sampler that ran during one of the boot's measurement
-- windows, as quen names it on those windows' rows (`actor_trace 10 Hz r8 max20 ref player`): measure_trace and
-- measure_ab with a trace record it as they run. NULL: none ran (every boot before 0014). The views keep their columns:
-- the rows' notes name the sampler their window ran with.

ALTER TABLE boots ADD COLUMN samplers TEXT CHECK (samplers IS NULL OR json_valid(samplers));
