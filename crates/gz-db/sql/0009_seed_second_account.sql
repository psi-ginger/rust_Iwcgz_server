-- A second login account, so two clients can be in the world at once.
--
-- Applied by `goonzu-rs/scripts/db-init.ps1` after 0008. Idempotent.
--
-- ## Why this exists
--
-- T-018 slice 6 B1 fans the 200 `CHARUPDATE` to nearby observers, and B3 makes
-- 214 -> 215 an observer pull. Neither can be *observed* with one account: a
-- single client can only ever be the mover, never the bystander. Every B1 live
-- test needs two simultaneous sessions on the same map.
--
-- ## Why a second *account* rather than a second slot
--
-- `accounts.username` is unique (`0001_schema.sql`) and the front service
-- authenticates on it, so one account cannot hold two live sessions -- the second
-- login either replaces the first or is refused, depending on what the front
-- service does with a duplicate `GAMEMSG_REQUEST_LOGIN`. A second slot on
-- `test` would have the same problem for the same reason. Two accounts is the
-- shape the auth path actually supports.
--
-- ## Why `Tester2` and not a second character on `test`
--
-- `characters.name` is unique, and more importantly the *registry* keys sessions
-- by a per-connection serial precisely so two sessions never shadow each other
-- (`registry.rs` module docs). Two distinct names also keep the two-client log
-- readable: `game.log` names the character on every selectchar line.
--
-- ## The character is deliberately blank
--
-- A fresh level-1 character with DDL-default stats (abilities 5, `bonus_points` 5,
-- `max_life` 40, `max_mana` 20) rather than a second copy of `Tester`'s tuned row.
-- Those defaults are self-consistent -- the derived maxima reproduce them exactly at
-- level 1 (vit 5 -> 40, mag 5 -> 20, `Parametabox.cpp:3374`/`:3545`), and the total
-- 35 + 5 = 40 equals the level-1 allowance `40 + 0*5` -- so the `handle_use_bonus`
-- level-allowance check accepts its points and nothing needs repairing. It is also
-- the honest baseline for the two-client run: `Tester` has already spent its points
-- during the Part A GUI pass, `Tester2` has not.
--
-- `move_speed` stays at the `0008` default 10 (the bare class base,
-- `KindInfo_Create.txt`), which is correct for a character with no boots. Do not
-- copy `Tester`'s 12: that value is the +2 boots `Tester` wears.
--
-- ## `game_unique`
--
-- Left NULL on purpose, unlike `Tester`'s pinned 1471. The pin exists because every
-- replayed frame in the capture prologue carries 1471 and the client discards
-- records that do not match its handshake id (`0002`'s comment, `config.rs`
-- `Prologue::Capture`). A second character pinned to the same id would be
-- indistinguishable from the first in capture mode. NULL makes
-- `resolve_game_unique` allocate a fresh id from `character_game_unique_seq`
-- (`character.rs:516-534`), so under `GAME_PROLOGUE=live` -- the mode a two-client
-- run needs anyway, since capture pins *every* session to 1471 and the two clients
-- would then share one character id -- the two get distinct ids and each client
-- only accepts records for its own character.
--
--   username: test1
--   password: test
--
-- Same development-only sha256-by-pgcrypto scheme as 0002; real auth hardening is
-- a separate task.

INSERT INTO accounts (username, password_hash, gm_level, gender, status)
VALUES ('test1', 'sha256:' || encode(sha256('test'::bytea), 'hex'), 0, 1, 1)
ON CONFLICT (username) DO UPDATE
  SET password_hash = EXCLUDED.password_hash,
      gm_level      = EXCLUDED.gm_level,
      gender        = EXCLUDED.gender,
      status        = EXCLUDED.status;

-- gender 1 (male) on the account and `class_kind` 1 (KIND_HERO1, the male hero)
-- to match, same as `Tester`. `class_kind` must be non-zero or the char-select
-- client renders the slot empty (`NSelectChar.cpp:736,923`).
--
-- Spawn: map 158 (`MAPINDEX_PALACE`, `Server-Login.cpp:375`) at (27, 87) -- the same
-- palace point `Tester` uses, inside the legacy's random X 24..30 / Y 68..90 box
-- (`Server-Login.cpp:377-378`) and the fixed point the client itself warps to
-- (`NPartyDlg.cpp:533`).
--
-- **The spawn is deliberately the same tile as `Tester`'s.** B1 fans the 200 over a
-- 3x3 cell block, so the two clients have to start inside one another's near set for
-- the observer to receive anything: at (27, 87) both are in cell (2, 5) and the fan
-- reaches each other. Spawning them apart would make B1 look broken when it is
-- working. Move them apart *after* confirming the fan, to test the boundary.
INSERT INTO characters (account_id, slot, name, class_kind, gender, map_id, pos_x, pos_y)
SELECT account_id, 0, 'Tester2', 1, 1, 158, 27, 87
FROM accounts
WHERE username = 'test1'
ON CONFLICT (account_id, slot) DO UPDATE
  SET name       = EXCLUDED.name,
      class_kind = EXCLUDED.class_kind,
      gender     = EXCLUDED.gender,
      map_id     = EXCLUDED.map_id,
      pos_x      = EXCLUDED.pos_x,
      pos_y      = EXCLUDED.pos_y;

-- Stats are left at their DDL defaults on purpose (see the header): the row is
-- created only if it is missing, so a re-run cannot stomp points `Tester2` has
-- since spent through the 250 handler. `0002` uses a full `ON CONFLICT DO UPDATE`
-- because that row is a fixed reference the GUI pass mutates and expects back; this
-- one is not.
INSERT INTO character_stats (character_id)
SELECT c.character_id
FROM characters c
JOIN accounts a ON a.account_id = c.account_id
WHERE a.username = 'test1' AND c.slot = 0
ON CONFLICT (character_id) DO NOTHING;
