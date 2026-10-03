-- goonzu-rs development schema (plain SQL; blueprint 03 section 2).
-- Applied by `goonzu-rs/scripts/db-init.ps1` with psql. Every statement is
-- idempotent so re-running is safe and existing data is preserved.
--
-- `accounts` predates this file (created during T-012) and already matches the
-- Rust model in `gz-db::repository`; the CREATE IF NOT EXISTS is a no-op there.

CREATE TABLE IF NOT EXISTS accounts (
  account_id     SERIAL       PRIMARY KEY,
  username       VARCHAR(50)  NOT NULL UNIQUE,
  password_hash  VARCHAR(255) NOT NULL,
  gm_level       INTEGER      NOT NULL DEFAULT 0,
  gender         INTEGER      NOT NULL DEFAULT 0,
  status         INTEGER      NOT NULL DEFAULT 1,
  block_mode     INTEGER      NOT NULL DEFAULT 0,
  block_reason   VARCHAR(100),
  dormancy_state INTEGER      NOT NULL DEFAULT 0,
  playing_time   INTEGER      NOT NULL DEFAULT 0,
  created_at     TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- Legacy tbl_Person, trimmed to what the login/char-list path reads today.
CREATE TABLE IF NOT EXISTS characters (
  character_id SERIAL       PRIMARY KEY,
  account_id   INTEGER      NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
  slot         SMALLINT     NOT NULL,          -- legacy Index
  name         VARCHAR(20)  NOT NULL UNIQUE,
  class_kind   SMALLINT     NOT NULL DEFAULT 0,-- legacy Kind
  gender       SMALLINT     NOT NULL DEFAULT 0,
  map_id       INTEGER      NOT NULL DEFAULT 1,
  pos_x        INTEGER      NOT NULL DEFAULT 0,
  pos_y        INTEGER      NOT NULL DEFAULT 0,
  school_id    SMALLINT     NOT NULL DEFAULT 0,
  created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
  deleted_at   TIMESTAMPTZ,
  UNIQUE (account_id, slot)
);

-- Legacy tbl_IP ("important parameta"), the stats half of cltCharListInfo.
CREATE TABLE IF NOT EXISTS character_stats (
  character_id INTEGER  PRIMARY KEY REFERENCES characters(character_id) ON DELETE CASCADE,
  level        SMALLINT NOT NULL DEFAULT 1,
  exp          BIGINT   NOT NULL DEFAULT 0,
  str          INTEGER  NOT NULL DEFAULT 5,
  dex          INTEGER  NOT NULL DEFAULT 5,
  vit          INTEGER  NOT NULL DEFAULT 5,
  mag          INTEGER  NOT NULL DEFAULT 5,
  hnd          INTEGER  NOT NULL DEFAULT 5,
  luk          INTEGER  NOT NULL DEFAULT 5,
  wis          INTEGER  NOT NULL DEFAULT 5,
  bonus_points SMALLINT NOT NULL DEFAULT 5,
  skill_bonus  SMALLINT NOT NULL DEFAULT 0,
  max_life     INTEGER  NOT NULL DEFAULT 40,
  max_mana     INTEGER  NOT NULL DEFAULT 20
);
