-- The character's effective move speed, in the legacy's `GetTotalMoveSpeed()`
-- units (`Parametabox.cpp:3973`, `TotalMoveSpeed = BasicMoveSpeed +
-- ShoesMoveSpeed`).
--
-- The server has to step a moving character from *this* number, not a constant:
-- `cltCharCommon::Move` reads `clPB.GetTotalMoveSpeed()` every tick
-- (`char-move.cpp:140`) and commits one tile per `5000 / movespeed` ms, so the
-- walk paces itself off whatever gear and mounts the character currently has
-- (base class speed, boots, a horse up to ~30). The old server-side port used a
-- fixed 14 taken from the reference capture, which paced every character like
-- the one captured.
--
-- `BasicMoveSpeed` is the class base (`pclKI->siMoveSpeed`). For the shipped
-- player kinds it is 10 (`Data\KindInfo_Create.txt`, the `movespeed` column of
-- every `KIND_HERO*`/`KIND_HEROINE*` row), so 10 is the bare new-character value
-- and the default here. Per-item bonuses are not modelled yet, so the column
-- holds the *effective total* and is written by whatever gear logic lands later.
-- It is capped at the 6 bits the login record allots `uiTotalMoveSpeed`
-- (`cltMakeCharInfo`, `game.rs`), which is also wider than the i8 `siMoveSpeed`
-- a 200 carries.
ALTER TABLE character_stats
  ADD COLUMN IF NOT EXISTS move_speed SMALLINT NOT NULL DEFAULT 10
  CHECK (move_speed >= 0 AND move_speed <= 63);

-- The dev `Tester` wears +2 boots, so its effective total is 12 (base 10 + 2).
-- Until gear is modelled this is set by hand; it is what the live walk is paced
-- against. (`login_capture.pcapng`'s character was a differently-shod one and
-- moved at 14, per the `siMoveSpeed` byte of both captured 200s.)
UPDATE character_stats s
   SET move_speed = 12
  FROM characters c
  JOIN accounts a ON a.account_id = c.account_id
 WHERE s.character_id = c.character_id
   AND a.username = 'test'
   AND c.slot = 0;