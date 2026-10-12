-- Per-character quest state, the port's analog of the legacy's per-type quest
-- rows written by `rsp_QuestInfoSet` (`DBManager\GameDBManager_World\DBMsgQuest.cpp:8-33`,
-- requested by `Server-Person.cpp:1905` as `sDBRequest_SetQuestInfo(personid,
-- questtype, unit)`).
--
-- One row per character x quest type. The six columns are exactly the six
-- fields of `cltPersonQuestUnitInfo` (`common\Quest\Quest.h:137-161`): the
-- cleared-quest bitmask plus the in-progress quest's unique / step / condition
-- and its two parameters. `state` is SIGNED because the legacy field is an
-- `SI32` and the client reads it back as one; the mask only ever sets bits
-- 0..31, so the sign is not load-bearing — it is simply not masked off.
--
-- A missing row means "all zeros" for that type, which is the honest empty
-- state (nothing cleared, nothing running) — so `load` returns only the rows
-- that exist and the writer only writes a type that actually changed, exactly
-- the legacy's per-type update. Re-running stays a no-op; the table is created
-- once and the statement below is guarded.
--
-- No `updated_at` bookkeeping beyond the default: the game service owns the
-- in-memory block and writes it on every change, and nothing reads a timestamp.

CREATE TABLE IF NOT EXISTS character_quests (
  character_id INTEGER  NOT NULL REFERENCES characters(character_id) ON DELETE CASCADE,
  quest_type   SMALLINT NOT NULL,
  state        INTEGER  NOT NULL DEFAULT 0,
  cur_unique   SMALLINT NOT NULL DEFAULT 0,
  cur_step     SMALLINT NOT NULL DEFAULT 0,
  cur_cmd      SMALLINT NOT NULL DEFAULT 0,
  para1        SMALLINT NOT NULL DEFAULT 0,
  para2        SMALLINT NOT NULL DEFAULT 0,
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (character_id, quest_type)
);

-- The only read pattern is "every row for this character" at world entry, which
-- the primary key's leading column already serves.
