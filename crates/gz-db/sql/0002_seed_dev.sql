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

INSERT INTO characters (account_id, slot, name, class_kind, gender, map_id, pos_x, pos_y)
SELECT account_id, 0, 'Tester', 0, 0, 1, 100, 100
FROM accounts
WHERE username = 'test'
ON CONFLICT (account_id, slot) DO UPDATE
  SET name       = EXCLUDED.name,
      class_kind = EXCLUDED.class_kind,
      gender     = EXCLUDED.gender,
      map_id     = EXCLUDED.map_id,
      pos_x      = EXCLUDED.pos_x,
      pos_y      = EXCLUDED.pos_y;

INSERT INTO character_stats (character_id, level, exp, str, dex, vit, mag, hnd, luk, wis)
SELECT c.character_id, 10, 0, 20, 15, 20, 10, 10, 10, 10
FROM characters c
JOIN accounts a ON a.account_id = c.account_id
WHERE a.username = 'test' AND c.slot = 0
ON CONFLICT (character_id) DO UPDATE
  SET level = EXCLUDED.level, exp = EXCLUDED.exp,
      str = EXCLUDED.str, dex = EXCLUDED.dex, vit = EXCLUDED.vit,
      mag = EXCLUDED.mag, hnd = EXCLUDED.hnd, luk = EXCLUDED.luk,
      wis = EXCLUDED.wis;
