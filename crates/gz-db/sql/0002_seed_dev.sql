-- Development seed: one login account and one character to drive the M1
-- login -> char-list path. Idempotent (safe to re-run).
--
-- The password is hashed by PostgreSQL itself (pgcrypto's sha256), not pasted
-- in as a literal. Format is `sha256:<hex>`; the Rust verifier reads that
-- prefix. Development-only: real auth hardening is a separate task.
--
--   username: test
--   password: test

INSERT INTO accounts (username, password_hash, gm_level, gender, status)
VALUES ('test', 'sha256:' || encode(sha256('test'::bytea), 'hex'), 0, 0, 1)
ON CONFLICT (username) DO UPDATE
  SET password_hash = EXCLUDED.password_hash,
      gm_level      = EXCLUDED.gm_level,
      gender        = EXCLUDED.gender,
      status        = EXCLUDED.status;

-- class_kind must be non-zero. The client's char-select treats a slot as
-- occupied only when `clBI.GetKind()` (`uiKind` @ offset 26) is > 0
-- (`NSelectChar.cpp:736,923`); kind 0 renders as an empty slot even though the
-- server sent a character. 1 = KIND_HERO1 (male), matching gender 1.
--
-- map_id 158 is `MAPINDEX_PALACE` (`map.h:69`), the map the legacy's
-- new-character path assigns (`Server-Login.cpp:375`), and the client's
-- `Data\mapinfoE.txt` names it `Hanyang`. pos (27, 87) is the palace spawn --
-- inside the legacy's own random X 24..30 / Y 68..90 box, and the fixed point
-- it warps to in `NPartyDlg.cpp:533`. `(0,0)` (the 0001 default for pos_x and
-- pos_y) is outside every map and renders as a black void; 0007 corrects any
-- row written before this.
-- `game_unique` is pinned to 0x05BF (1471) because every per-character frame in
-- the replayed reference capture identifies the character by that id, and the
-- client discards records whose id does not match the one it was handed in
-- `GAMEMSG_RESPONSE_CLIENTCHARUNIQUE`. It becomes an allocated per-character id
-- once `RESPONSE_ALLCHARINFOTWO` (249) is generated from `character_items`
-- instead of replayed.
INSERT INTO characters (account_id, slot, name, class_kind, gender, map_id, pos_x, pos_y, game_unique)
SELECT account_id, 0, 'Tester', 1, 1, 158, 27, 87, 1471
FROM accounts
WHERE username = 'test'
ON CONFLICT (account_id, slot) DO UPDATE
  SET name       = EXCLUDED.name,
      class_kind = EXCLUDED.class_kind,
      gender     = EXCLUDED.gender,
      map_id     = EXCLUDED.map_id,
      pos_x      = EXCLUDED.pos_x,
      pos_y      = EXCLUDED.pos_y,
      game_unique = EXCLUDED.game_unique;

-- Level 13, not 10. `GAMEMSG_REQUEST_USEBONUS` (250) enforces the level
-- allowance `bonus + sum(abilities) <= 40 + (level-1)*5` (`Server-Person.cpp`
-- `:2472-2493`). The abilities below sum to 95; with the `character_stats`
-- `bonus_points` default of 5 that is a total of 100. At level 10 the allowance
-- is only `40 + 9*5 = 85`, so the row is over by 15 and every allocation would
-- be refused (the legacy *kicks* on the mismatch; our port rejects instead, see
-- `handle_use_bonus`). Level 13 makes the allowance `40 + 12*5 = 100`, exactly
-- the row, so the starting 5 points are spendable.
--
-- `max_life`/`max_mana` are stored columns no code path recomputes today, and the
-- seed never set them, so they sat at the 0001 defaults (40/20) while the real
-- formulas say otherwise. Both are the legacy's base terms at this level/ability:
--   max_life = vit * (8000 + level*5) / 1000 = 20 * 8065 / 1000 = 161
--   max_mana = mag * (4000 + level*5) / 1000 = 10 * 4065 / 1000 = 40
-- (`Parametabox.cpp:3374`, `:3545`; both reproduce the DDL defaults exactly at
-- level 1 — vit 5 -> 40, mag 5 -> 20 — which is what makes them the recalc source).
-- `handle_use_bonus` recomputes both on every accepted spend.
INSERT INTO character_stats
  (character_id, level, exp, str, dex, vit, mag, hnd, luk, wis,
   bonus_points, max_life, max_mana)
SELECT c.character_id, 13, 0, 20, 15, 20, 10, 10, 10, 10, 5, 161, 40
FROM characters c
JOIN accounts a ON a.account_id = c.account_id
WHERE a.username = 'test' AND c.slot = 0
ON CONFLICT (character_id) DO UPDATE
  SET level = EXCLUDED.level, exp = EXCLUDED.exp,
      str = EXCLUDED.str, dex = EXCLUDED.dex, vit = EXCLUDED.vit,
      mag = EXCLUDED.mag, hnd = EXCLUDED.hnd, luk = EXCLUDED.luk,
      wis = EXCLUDED.wis,
      bonus_points = EXCLUDED.bonus_points,
      max_life = EXCLUDED.max_life, max_mana = EXCLUDED.max_mana;
