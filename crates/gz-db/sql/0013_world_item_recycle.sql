-- Ground-item lifecycle (audit H4): a sweeper for unclaimed drops, and a
-- recycle pool for the UI16 handles they were addressed by.
--
-- `GAMEMSG_REQUEST_INVPICKUP` (308) names a ground item by the `obj_index`
-- handle `0005_world_item_handle.sql` minted from a sequence. A sequence only
-- ever climbs, so the 65,536th lifetime drop — not the 65,536th *concurrent*
-- one — would exhaust the UI16 space and every later drop would fail the
-- `world_items_obj_index_fits_ui16` check. The legacy's `cItemManager` kept
-- ground objects in a bounded table and recycled their slots on removal
-- (`Server-Item.cpp`); this is that, split across the sweeper and the pool:
--
--  * `sweep_expired_ground` (gz-db/src/item.rs) deletes `world_items` rows
--    older than the TTL — nobody claims a ground icon an hour later, and the
--    row is the only durable trace of the drop once the bag slot is gone.
--  * Every deletion, swept or picked up, pushes its handle into
--    `world_items_free`; the next `drop_slot` claims `MIN(free)` before it
--    asks the sequence for a fresh number.
--
-- The pool lives here rather than in the game process so it survives restarts:
-- a handle freed before a restart is still reusable after one.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0012. Idempotent.

-- The pool: recycled handles waiting for the next drop. The CHECK mirrors
-- `world_items_obj_index_fits_ui16` so a bad row cannot smuggle a handle the
-- client could never name.
CREATE TABLE IF NOT EXISTS world_items_free (
    obj_index INTEGER PRIMARY KEY
        CHECK (obj_index >= 1 AND obj_index < 65536)
);

-- The sweeper's scan. The table is bounded by concurrent ground items, so
-- this index is a courtesy to the periodic DELETE rather than a necessity.
CREATE INDEX IF NOT EXISTS world_items_by_dropped_at
    ON world_items (dropped_at);
