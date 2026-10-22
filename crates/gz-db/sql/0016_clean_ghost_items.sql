-- Clean up "ghost" (sparse) items from character_items.
--
-- A ghost item is a row whose only non-zero content is the `siUnique`/`siItemNum`
-- pair (bytes 0..4); bytes 4.. (seal date, atb, attach, ...) are all zero. The
-- client does not render these (they are invisible in the bag), so carrying them
-- only blocks slots and forces later grants (quest rewards, loot) past the
-- top-left. They originate from `archive/capture/import_capture.rs`, which used
-- to import every occupied slot verbatim, sparse ones included.
--
-- This migration removes them so the top-left placement rule
-- (INV0 = slot 15) is not defeated by invisible blockers. Idempotent.
DELETE FROM character_items
 WHERE octet_length(raw) >= 5
   AND encode(substring(raw FROM 5), 'hex') ~ '^0+$';