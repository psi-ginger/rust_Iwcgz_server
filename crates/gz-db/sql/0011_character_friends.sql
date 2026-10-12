-- Friend relationships, the port's analog of the legacy `tbl_Friend`.
--
-- A friendship is mutual and stored as two rows (one per direction), because
-- each side picks its own group and its own list slot. Rows reference the
-- `characters` rows on both sides, so a deleted character takes its edges with
-- it and names/accounts resolve through `characters`/`accounts` at load time.
--
-- `list_slot` is the 0-based index the acceptor's 6203 told the requester's
-- client the friend landed at (`Server-Friend.cpp`'s `clFriend[i]` scan), so
-- the 6219 world-entry push and the 6210 dialog refresh can reproduce the same
-- tab order across logins. No friend delete exists in the port, so slots are
-- the append order and never have gaps. Re-running stays a no-op; the table is
-- created once and every statement below is guarded.

CREATE TABLE IF NOT EXISTS character_friends (
  owner_character_id  INTEGER  NOT NULL REFERENCES characters(character_id) ON DELETE CASCADE,
  friend_character_id INTEGER  NOT NULL REFERENCES characters(character_id) ON DELETE CASCADE,
  group_id            SMALLINT NOT NULL DEFAULT 0,
  list_slot           SMALLINT NOT NULL,
  created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (owner_character_id, friend_character_id),
  UNIQUE (owner_character_id, list_slot)
);

CREATE INDEX IF NOT EXISTS character_friends_owner
  ON character_friends (owner_character_id);