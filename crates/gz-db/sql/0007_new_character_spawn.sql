-- Where a new character stands, and which map it stands on.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0006. Idempotent.
--
-- ## Why 158
--
-- The legacy's new-character path assigns the map by name, and the number is
-- what reaches the client:
--
--     clbasicinfo.clPos.SetMapIndex(MAPINDEX_PALACE);   -- Server-Login.cpp:375
--
-- `MAPINDEX_PALACE` is 158 (`map.h:69`). The client's `Data\mapinfoE.txt` row
-- 173 names MapIndex 158 `Hanyang`, MapUnique 12 -- and `Data\MapTypeInfo.txt`
-- row 17 resolves MapUnique 12 to `MAPTYPE_SHURIPALACE1` (map folder
-- `ShuriPalace1`), which is the same map named two ways.
--
-- The dev seed used map_id 1 instead. `mapinfoE.txt` row 16 gives MapIndex 1
-- `VillageUnique 1`, and `Data\villageinfoE.txt` row 1 names village 1
-- `Ankaran`. So the map was right in the sense that a character can stand on
-- it, and wrong in the only way that shows: the town banner named Ankaran,
-- because that is the village MapIndex 1 belongs to. Hanyang is not a village
-- at all -- it is `VillageUnique 0`, which is why it has no row in
-- `villageinfoE.txt` and why the fix is the map index, not `school_id`.
--
-- ## Why (27, 87)
--
--     SI32 RandXPos = rand()%7 + 24;   -- Server-Login.cpp:377
--     SI32 RandYPos = rand()%23 + 68;  -- Server-Login.cpp:378
--
-- so the legacy's spawn is somewhere in X 24..30, Y 68..90, freshly drawn per
-- character. (27, 87) is inside that box and is the fixed palace point the
-- legacy itself warps to (`cltWarpInfo clWarpInfo(1, MAPINDEX_PALACE, false,
-- false, 27, 87, -1, -1)`, `NPartyDlg.cpp:533`). Picking it over `rand()` is a
-- deliberate deviation: a capture-replaying server that moved the avatar on
-- every login would make every position assertion unstable.
--
-- ## The column defaults
--
-- `0001_schema.sql` declared `map_id DEFAULT 1` with `pos_x`/`pos_y` both 0,
-- and `create_character` inserted neither, so a character made through the
-- game's own make-character path landed on (0, 0) -- tile 0 of no map, which
-- the client renders as a black void outside the world. `create_character` now
-- binds all three; these defaults are the belt to that braces, so a
-- hand-written INSERT cannot recreate the same void.
ALTER TABLE characters ALTER COLUMN map_id SET DEFAULT 158;
ALTER TABLE characters ALTER COLUMN pos_x  SET DEFAULT 27;
ALTER TABLE characters ALTER COLUMN pos_y  SET DEFAULT 87;

-- Rows that predate this migration: the dev seed's `Tester` on the village map,
-- and anything the make-character path wrote at (0, 0). Both are unplaceable
-- as things stand, so both move onto the palace spawn.
--
-- Scoped to exactly those two shapes on purpose. A position of (0, 0) on some
-- future map could be legitimate, and MapIndex 1 is a real village map that a
-- player may legitimately be standing in once movement exists; neither is true
-- yet, and T-018 owns movement.
UPDATE characters
   SET map_id = 158, pos_x = 27, pos_y = 87
 WHERE map_id = 1
    OR (map_id = 0 AND pos_x = 0 AND pos_y = 0);