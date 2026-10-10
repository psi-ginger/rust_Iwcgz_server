-- One item per ground cell.
--
-- `world_items` cells mirror the client's per-cell item grid: the client
-- renders a single item per cell (`cltMapCommon::SetItemMap` overwrites) and
-- `ITZMap::AddItem` refuses an occupied cell, so a second row at the same
-- (map_id, x, y) renders nowhere and can never be picked up — an item lost to
-- nobody's benefit. `drop_slot` used to check-then-insert (SELECT NOT EXISTS
-- followed by INSERT), which let two concurrent drops race into the same cell;
-- the INSERT now uses this index as its arbiter via `ON CONFLICT DO NOTHING`.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0004. Idempotent.

-- Backfill first: drops converge on the character's entry coordinates, so a
-- used dev world can already hold duplicates. Drops are pure data with no
-- foreign handholds, so keep the newest row per cell and delete the rest.
DELETE FROM world_items a
USING world_items b
WHERE a.map_id = b.map_id AND a.x = b.x AND a.y = b.y
  AND (a.dropped_at, a.world_item_id) < (b.dropped_at, b.world_item_id);

-- The arbiter for `INSERT ... ON CONFLICT (map_id, x, y) DO NOTHING` in
-- `drop_slot` (gz-db/src/item.rs).
CREATE UNIQUE INDEX IF NOT EXISTS world_items_cell_unique
  ON world_items (map_id, x, y);