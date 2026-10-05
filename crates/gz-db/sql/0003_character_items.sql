-- Character identity on the wire, and the item array.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0002. Idempotent.
--
-- `characters.game_unique` is declared by 0001; this file adds the constraints
-- and the allocation sequence around it.

-- The client indexes its `CharUniqueInfo` table by this value
-- (`SetCharReservedChar`), so it has to stay positive and below `0x4E20`
-- (20000).
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conname = 'characters_game_unique_range'
  ) THEN
    ALTER TABLE characters
      ADD CONSTRAINT characters_game_unique_range
      CHECK (game_unique IS NULL OR (game_unique > 0 AND game_unique < 20000));
  END IF;
END
$$;

CREATE UNIQUE INDEX IF NOT EXISTS characters_game_unique_key
  ON characters (game_unique)
  WHERE game_unique IS NOT NULL;

-- Allocation source for `game_unique`. The lower bound is 1000 so it cannot
-- collide with the low hand-pinned ids the legacy world handed out (the dev
-- character is pinned to 1471); the upper bound matches the CHECK above. No
-- CYCLE: exhausting the range should fail loudly rather than hand out a live id.
CREATE SEQUENCE IF NOT EXISTS character_game_unique_seq
  START WITH 1000 MINVALUE 1000 MAXVALUE 19999;

-- Start above whatever is already pinned. `is_called = false` means the next
-- `nextval` returns exactly this value. The floor is the sequence's MINVALUE, so
-- a fresh database hands out 1000 rather than tripping the bound.
SELECT setval(
  'character_game_unique_seq',
  GREATEST(1000, COALESCE((SELECT MAX(game_unique) FROM characters), 0) + 1),
  false
);

-- One row per occupied `cltPItemInfo::clItem` slot.
--
-- `slot` is the **absolute** index into that 80-entry array, which is what the
-- client sends in `GAMEMSG_REQUEST_ITEMMOVE`'s `uiFrom`/`uiTo`
-- (`MouseOperation.cpp:2241-2248`). It is not a per-container index, so a move
-- needs no translation between representations.
--
-- `container` duplicates the range the slot falls in so SQL can answer "what is
-- equipped" without knowing the ranges; it is written only by the importer,
-- from [`gz_proto::item`]. The ranges are binary-derived (equipment 0..=14, main
-- bag 15..=34, horse 35..=54, extra bags 55..=66 and 67..=78) — the
-- `ItemUnit.h:388-402` `PERSONITEM_*` anchors agree on where each container
-- starts but its declared six-slot bag is wrong for the shipped build. Re-run
-- `game import-capture` after changing the layout so this column is re-derived.
--
-- `raw` is the complete 84-byte `cltItem`. Only `siUnique` and `siItemNum` have
-- confirmed offsets (see `gz_proto::item`), so promoting them to columns while
-- keeping the record verbatim makes the table queryable *and* lossless: nothing
-- is thrown away that the replay still needs.
CREATE TABLE IF NOT EXISTS character_items (
  character_id INTEGER  NOT NULL REFERENCES characters(character_id) ON DELETE CASCADE,
  slot         SMALLINT NOT NULL,
  container    SMALLINT NOT NULL,
  item_unique  INTEGER  NOT NULL,
  count        INTEGER  NOT NULL DEFAULT 0,
  raw          BYTEA    NOT NULL,
  PRIMARY KEY (character_id, slot),
  CHECK (slot >= 0 AND slot < 80)
);

CREATE INDEX IF NOT EXISTS character_items_by_unique
  ON character_items (item_unique);