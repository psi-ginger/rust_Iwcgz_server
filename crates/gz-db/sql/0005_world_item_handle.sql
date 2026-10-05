-- The server-side handle a ground item is addressed by, as `GAMEMSG_REQUEST_INVPICKUP`
-- (308) requires.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0004. Idempotent.
--
-- ## Why a handle at all
--
-- `GAMEMSG_RESPONSE_ITEMDROP` (309) carries `cltCharPos clPos` + `UI16 uiItemUnique`
-- + `UI16 uiItemListIndex` (`MsgType-Item.h:413`). The client's whole handler is
--
--     pclMapArray[mapindex]->SetItemMap(x, y, pclinfo->uiItemUnique, pclinfo->uiItemListIndex);
--
-- (`Client-SystemNPC.cpp:2289`), which writes the pair into the client's own
-- per-cell grid. It never interprets the handle — but it hands the same value back
-- in the pickup request as `siItemIndex` (`MouseOperation.cpp:5316`:
-- `GetItemUnique(), GetItemIndex(), CurrentMapIndex, GetItemX(), GetItemY()`), and
-- the server resolves the pickup with it:
--
--     cItemNode* pitemnode = pclItemList->GetItem(pclinfo->siItemIndex);
--     if(pitemnode == NULL)return ;
--
-- (`Server-Item.cpp:2362`). So the handle has to be something we can look up, and
-- it has to be stored rather than recomputed, or a player cannot pick up what they
-- dropped.
--
-- ## Why a sequence rather than reusing `world_item_id`
--
-- `world_item_id` is a BIGSERIAL and will outgrow the `UI16` the frame can carry,
-- whereas the legacy recycles handles from a bounded pool
-- (`pclItemList->PushItem`, `Server-Item.cpp:3545`). A sequence gives collision-free
-- handles without the pooling machinery, and the CHECK below turns the eventual
-- exhaustion into a loud failure instead of a truncated handle that would resolve
-- to the wrong ground object.
--
-- ## Known deviation from the legacy
--
-- Handles are never recycled here. Once 308 lands and rows start being deleted,
-- this sequence still only grows, so a long-lived database would exhaust the
-- `UI16` range after 65535 drops. Wrapping the allocation back into the free range
-- is the fix, and it belongs with 308 rather than before it.
CREATE SEQUENCE IF NOT EXISTS world_items_obj_index_seq START WITH 1;

ALTER TABLE world_items ADD COLUMN IF NOT EXISTS obj_index INTEGER;

-- Backfill rows written before this column existed, giving each a distinct handle.
UPDATE world_items
   SET obj_index = nextval('world_items_obj_index_seq')
 WHERE obj_index IS NULL;

ALTER TABLE world_items ALTER COLUMN obj_index SET NOT NULL;

-- The handle travels as `UI16 uiItemListIndex` and comes back as `SI16 siItemIndex`,
-- so anything outside 0..=65535 could not survive the round trip.
ALTER TABLE world_items DROP CONSTRAINT IF EXISTS world_items_obj_index_fits_ui16;
ALTER TABLE world_items ADD CONSTRAINT world_items_obj_index_fits_ui16
  CHECK (obj_index >= 1 AND obj_index < 65536);

-- Uniqueness is what makes `siItemIndex` a usable key: two live ground objects
-- sharing a handle would make a pickup ambiguous.
CREATE UNIQUE INDEX IF NOT EXISTS world_items_obj_index_key ON world_items (obj_index);