-- Items lying on the map, i.e. what a later `GAMEMSG_REQUEST_INVPICKUP` (308)
-- will pick back up.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0003. Idempotent.
--
-- ## Why this table exists at all
--
-- The legacy drop path (`cltServer::DoMsg_GAMEMSG_REQUEST_INVDROP`) ends by
-- sending an `sDBRequest_DropItem` to the DB layer, which persists the drop and
-- returns `sDBResponse_DropItem`. Only then does the game server spawn the
-- ground world object (`cltServer::DoMsg_DBMSG_RESPONSE_DROPITEM`,
-- `ServerDB-Item.cpp:2792`) and clear the client's slot via 7210.
--
-- This server has no world-object model yet, so a drop handled as "just remove
-- it from the bag" would silently destroy the item. Storing the drop keeps it
-- recoverable and gives 308 something to look up without waiting for the map
-- layer.
--
-- ## Known deviation from the legacy
--
-- The legacy calls `cltMapManager::FindProperAreaForDrop` to nudge the item to a
-- free neighbouring cell, and the client is told the resulting cell in
-- `GAMEMSG_RESPONSE_ITEMDROP` (309). We cannot search for a free cell without the
-- map model, so a drop lands on the dropper's own coordinates.
--
-- 309 itself is now sent: `cltMapCommon::SetItemMap` (`map.cpp:289`) writes the
-- client's *own* per-cell grid, so the frame needs no map model here. The handle
-- it carries lives in `0005_world_item_handle.sql`.
--
-- `map_id` is `characters.map_id`; `x`/`y` are map cells. Note that the server
-- does not track movement yet, so these are the character's *entry* coordinates
-- rather than wherever the player walked to.
CREATE TABLE IF NOT EXISTS world_items (
  world_item_id BIGSERIAL PRIMARY KEY,
  item_unique   INTEGER     NOT NULL,
  count         INTEGER     NOT NULL CHECK (count > 0),
  raw           BYTEA       NOT NULL,
  map_id        INTEGER     NOT NULL,
  x             INTEGER     NOT NULL,
  y             INTEGER     NOT NULL,
  -- NULL rather than ON DELETE CASCADE: deleting a character must not silently
  -- delete what it dropped on the floor.
  dropped_by    INTEGER     REFERENCES characters(character_id) ON DELETE SET NULL,
  dropped_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 308 looks items up by the cell the player is standing on.
CREATE INDEX IF NOT EXISTS world_items_by_cell
  ON world_items (map_id, x, y);

CREATE INDEX IF NOT EXISTS world_items_by_dropper
  ON world_items (dropped_by);