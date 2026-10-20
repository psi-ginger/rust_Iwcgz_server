# MUST FIX

Tracking for the repository audit. It began as the two **Must fix** findings from
the 2026-10-10 audit — the only two that either let someone in without a password
or lose/corrupt player data — and now also carries the **Should fix** items from
the same audit, all of which are closed. The remaining *Nice to have* items are
listed at the end.

**Assumed load:** M1 local test cluster — 1–2 clients, PostgreSQL on localhost,
dev-run front + game. Where a fix only bites at higher concurrency, the item says
so.

Validation command for every item:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## #1 — Game login accepted any password

**Status: DONE** (2026-10-10)

### Root cause

`handle_game_login` (`services/game/src/auth.rs`) checked only that the packet's
`auth_key` equalled the account's own idnum, then loaded the character list. The
password field of the login packet was never checked. The front's own login *does*
check the password, but the front tells the client where the game server is and the
client connects to the game port directly — so the front's check is bypassable.

### Concrete case

Account `bob` has `account_id = 3` (`accounts.account_id` is a `SERIAL`, so ids are
small, sequential and guessable). A client sends a game login with
`userid = "bob"`, `auth_key = 3`, and *any* password. Before the fix that entered
bob's account without ever passing the front's login.

### Affected file

- `services/game/src/auth.rs` — `handle_game_login`.

### Fix

New pure function `admit_game_login(account_id, password_hash, login)` in
`auth.rs` checks **both** the key (`account_id == login.auth_key`) and the password
(`gz_db::password::verify_password(stored, login.password)`); `handle_game_login`
calls it and refuses on either mismatch. The decision is a pure function so it can
be tested without a database. Legacy parity: `Server-Login.cpp:82` forwards
`siAuthKey` **and** `szPassword` to the auth server.

### Validation

- `services/game/src/tests.rs::game_login_refuses_a_wrong_password` — correct
  key + correct password is admitted; wrong password, empty password, and wrong key
  are all refused.
- `cargo test -p game` → 99 passed, 0 failed.

---

## #2 — Item merge ignored pileability

**Status: DONE** (2026-10-10) — the ItemType `ITEMATB_OBJECT` attribute is ported
into `gz-data` and all three merge decisions read it.

### Root cause

The move-merge (`move_slot`), the split-merge (`split_slot`) and the pickup-stack
path merged any two slots holding the same `siUnique`, regardless of whether the
item can actually stack. The legacy merges only when both items are *pileable*:
`cltItem::CanPile` reads the item type's `ITEMATB_OBJECT` attribute
(`ItemUnit.cpp:1458`), and the pickup path stacks only `if(pileswitch)`
(`ItemPerson.cpp:315`). The server had no item-type attribute table to consult —
`gz-data` was an empty stub.

### Concrete cases

1. A player has a weapon equipped and picks up a second, identical weapon. The
   pickup candidate query matched the item in the *equipment* slot and merged the
   ground copy into it, leaving the equipped weapon with count 2. Legacy
   `CanAddInv` searches only the carry range, so the copy would have gone to a free
   bag slot.
2. A hostile client sends an `itemmove` of an equipped item onto a carry slot
   holding the same unique. The server merged instead of swapping. The real client
   never sends this (its drag handler refuses a non-pileable destination), so this
   is hardening.
3. A non-pileable item stored in a **bag** slot (a ring off the hand, say) is
   merged by unique when picked onto another copy, because the slot alone cannot
   say the item is equipment.

### Affected files

- `crates/gz-data/src/lib.rs` — new: the pileability table and `is_pileable`.
- `crates/gz-db/src/item.rs` — `move_slot`, `split_slot`, the pickup-stack
  candidate loop, driven by `stackable`.
- `crates/gz-db/Cargo.toml` — depends on `gz-data`.

### Fix

**Pileability data (`gz-data`).** An item's type is fixed by the data file it is
loaded from (`cltItemManagerCommon::SetItemData`, `cltItem-SetItemData.Cpp`), and
the `ITEMATB_OBJECT` flag per type is the hard-coded table in
`cltItemManagerCommon::SetItemType` in the same file. `gz-data` ships the resulting
per-unique answer as a sorted table `PILEABLE: &[u16]` (1828 uniques) plus a pure
`is_pileable(unique)`. The table is generated from the deployed English
`Item/Data/ItemType*E.txt` files whose type lacks `ITEMATB_OBJECT` (HEAL, FOOD,
BOOK, MINERAL, ANIMAL, TREE, CLOTH, SEAFOOD, ETC, SCROLL, DOLL, TICKET, EVENTITEM,
QUEST, BOX, PREMIUM, COMPOSITIONMATERIAL), first column, deduplicated and sorted;
the generation recipe is `scripts/generate-pileable-table.ps1`, whose output
reproduces the checked-in table and whose `-Check` mode verifies it. A unique that is **not** listed is not
pileable, matching the legacy's type-0 result (`FindItemRefFromUnique` finds no
type → `CanPile` false). The pileable and object-flagged non-pileable sets were
verified non-overlapping.

Two edge cases are baked in and documented in the `gz-data` module docs: CRYSTAL is
kept non-pileable (its type carries `ITEMATB_OBJECT` with `Switch_Artifact` off,
the default branch of `SetItemType`), and files the English `SetItemData` does not
load (e.g. `ItemBoxE`, `SoulGuardE`) leave their uniques at type 0, i.e.
non-pileable.

**Wiring (`item.rs`).** The earlier `slot_is_equipment`/`merge_allowed` heuristic
was replaced by a single `stackable(item_unique)` that consults `is_pileable`, so
the slot an item sits in is irrelevant — equipment never stacks in *any* container:

- `move_slot`'s same-unique merge arm is guarded by `stackable(dst_unique)`; a
  non-pileable same-unique move falls through to the existing swap arm.
- `split_slot` refuses (`DestinationMismatch`) a same-unique merge of a
  non-pileable item, closing the pre-fix case of a count-2 equipment stack.
- The pickup candidate loop only merges when `stackable(item_unique)`; a
  non-pileable ground item goes to a free carry slot instead.

### Validation

- `crates/gz-data/src/lib.rs::tests` — the table is sorted for the binary search;
  the first item of every pileable category (including BOX, PREMIUM,
  COMPOSITIONMATERIAL) is in the table; weapons/armour/accessories are not; unknown
  uniques are not (legacy type-0 result).
- `crates/gz-db/src/item.rs::tests::equipment_never_stacks_even_inside_bag_slots`
  — same assertions through the `stackable` boundary that all three merge
  decisions share.
- `cargo test --workspace` → 0 failures (game 99, gz-db 11, gz-data 2, rest 0).
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --check` → clean.

**Residual note:** the panic/swap-vs-stack behaviour is verified at the
decision boundary (`stackable`/`is_pileable`) because `gz-db` has no DB test
harness; the swap / free-slot fall-through paths themselves are existing code. The
table is generated from the deployed GameServer data and must be regenerated when
that data changes — `scripts/generate-pileable-table.ps1 -Check` fails when it is
stale.

---

## Scrutiny follow-up (2026-10-10, post-DONE review)

An outsider review of the #2 fix caught one real omission and recorded three
residual risks. Status after the review:

1. **Resolution — missing pileable categories (BOX, PREMIUM,
   COMPOSITIONMATERIAL).** The first version of the table was generated from the
   Korea branch's loader list plus the deployed filenames, which silently dropped
   the three categories the **English** branch of `SetItemData` also loads —
   `AddBox`/`AddPremium`/`AddCompositionManterial` at
   `cltItem-SetItemData.Cpp:738/741/732` — even though their types all lack
   `ITEMATB_OBJECT`. That made ~709 uniques (466 boxes + 218 premium + 25
   composition materials) non-pileable where the legacy merges them — most visibly,
   boxes would no longer stack. The table is regenerated to **1,828** uniques, the
   pileable and object-flagged sets are still disjoint, and the per-category
   first-item test (`consumables_materials_and_boxes_stack_but_equipment_does_not`)
   asserts every one of the 17 pileable categories, so this class of miss now fails
   a test.
2. **Remark — CRYSTAL pileability is switch-dependent.** `SetItemType` gives
   `ITEMTYPE_CRYSTAL` `ITEMATB_OBJECT` only with `Switch_Artifact` **off** (the
   default `else` branch; `cltItem-SetItemData.Cpp:98-119`). The table hardcodes
   that default. If a future deployment runs with artifacts on, crystals would stop
   stacking — a functional-only regression (refusing to stack never corrupts data).
   Documented in the module docs; a config knob is deferred until an
   artifact-enabled deployment exists.
3. **Resolved (2026-10-11) — the table generator is now committed.** The recipe
   (union of the first column of the pileable English `*E.txt` files, dedup, sort)
   is `scripts/generate-pileable-table.ps1`. Run it with no arguments to print the
   `const PILEABLE` block (a byte-for-byte drop-in — it lays the body out the way
   rustfmt does), or with `-Check` to verify the committed table is current
   (exit 1 on drift). The failure mode of a missed regenerate — a silent no-stack
   regression, not a crash — is now caught by `-Check` against the deployed data.
4. **Remark — verified at the decision boundary, not against the database.** The
   swap-vs-stack fall-through is asserted through `stackable`/`is_pileable`;
   `gz-db` has no live-PostgreSQL harness, so no item path ran against a real DB.
   The full `scripts/check.ps1` gate (codegen-drift + fuzz) was also skipped. Both
   remain the standing pre-commit gap.

---

## Audit backlog status (2026-10-11)

**All Must fix and Should fix items are closed and committed; the tree is clean.**
Full workspace: `cargo fmt --check` and
`cargo clippy --workspace --all-targets -- -D warnings` clean,
`cargo test --workspace` 0 failures.

**Must fix (2026-10-10) — done.**

- #1 game-login password check.
- #2 item merge honours pileability.

**Should fix (2026-10-10 audit) — done.**

- **Drop-slot race condition.** Fixed 2026-10-11: `world_items` now has a
  UNIQUE `(map_id, x, y)` index (`sql/0010_world_items_cell_unique.sql`, which
  also dedupes the rows a used dev world already accumulated), and `drop_slot`
  no longer check-then-inserts — it tries each candidate cell with
  `INSERT ... ON CONFLICT (map_id, x, y) DO NOTHING` and moves on when a
  concurrent drop won the cell, across the full legacy 5x5 box
  (`FindProperAreaForDrop`, `MapManager.cpp:999`): requested cell first, then
  the remaining 24, nearest ring first. Two concurrent drops can no longer both
  land on one cell, and `GroundBlocked` is only reached when all 25 are taken
  (the drop then rolls back with the bag untouched). Picking an item up deletes
  its `world_items` row, so a tile is immediately re-droppable — there is no
  legacy-style per-tile 'dead cell' flag left to clear. Residual: arbitration is
  per-INSERT, so two concurrent drops can still land on different cells of the
  same box — that is the legacy's own behaviour.
- **`create_character` is not transactional.** Fixed 2026-10-11:
  `Repository::create_character` (`gz-db/src/character.rs`) now inserts the
  `characters` row and the `character_stats` row inside one transaction
  (`pool.begin()` … `tx.commit()`); a failure between them can no longer orphan a
  character without stats.
- **Slot bound validation.** Fixed 2026-10-11: the repository layer already
  bounded move/split/drop/set-items against `MAX_ITEM_PER_PERSON` and select-char
  against `CHAR_SLOTS`; the outstanding surface, make-new-char, now rejects any
  slot outside `0..CHAR_SLOTS` via `admit_make_new_char_slot`
  (`services/game/src/auth.rs`), closing a crafted `slot >= CHAR_SLOTS` that
  would otherwise write an invisible, unloseable row.
  `services/game/src/tests.rs::make_new_char_rejects_slots_at_or_above_the_roster_bound`
  covers the boundary.
- **DB credentials in config.** Fixed 2026-10-11: no password is committed any
  more. `DEFAULT_DATABASE_URL` (`services/game/src/config.rs`), the front
  fallback (`services/front/src/main.rs`) and the auth-service fallback
  (`services/auth/src/main.rs`) now default to the credential-free
  `postgres://localhost:5432/goonzu`; sqlx reads user/password from the libpq
  `PGUSER`/`PGPASSWORD` env vars, which the launcher scripts source from the
  gitignored `scripts/dev-db.ps1` on this dev box. Plan §9 (OS keychain) is not
  implemented — credentials live in a gitignored local file instead.

**Nice to have.** The pileability table generator is committed
(`scripts/generate-pileable-table.ps1`, remark 3). The audit's remaining cleanup
items are not carded here.

---

## #3 — Base experience fired only on the killing hit

**Status: DONE** (2026-10-20) — every landed swing grants now, scaled
`×damage/maxHP/3` while the target lives and whole on the killing blow, and a
grant that crosses the `LevelUpExp.dat` threshold levels the character.

### Root cause

The port granted experience inside the death branch of `player_swing`
(`services/game/src/combat.rs`): a swing whose target survived returned
`Swing::Landed` before any grant ran, so chipping a monster down earned nothing
until the final blow — and that one grant still ran through the living-target
scaling (`×crit/maxHP/3`), paying a fraction of the legacy's whole pool. Legacy
`GetExp` branches on `IsAlive(enemyid)` (`Server-Attack.cpp:4131`): alive →
`exp × damage / maxlife / 3`, dead → full pool; `ExpOperate` runs at `Hit`'s
tail for every hit (`:2921-2928`). The level-up half (`IncreaseExp`,
`Parametabox.cpp:43`; `LevelUpAction`, `char.cpp:5763`) was not ported at all,
so no grant could ever raise a level.

### Concrete cases

1. A level-1 player chips a 60-HP monster (kind `Exp` 90) with three
   20-damage hits: the port granted 0, 0, then `90×20/60/3 = 10` on the kill,
   where the legacy pays `10`, `10`, then the whole pool `90` on the killing
   blow.
2. Reaching level 2 in the field was impossible: `LevelUpExp.dat` was never
   read, no `UPDATE character_stats` existed for a level, and the client got
   no 215/7221 and a 270 it could not reconcile against its own level-up
   expectation.

### Affected files

- `services/game/src/combat.rs` — `exp_seed`, `weak_monster_percent`,
  `exp_for_hit`, `leveled_to`, and the `player_swing` grant/level-up block.
- `crates/gz-data/src/levelup.rs` (new) — the `LevelUpExp.dat` loader.
- `crates/gz-db/src/character.rs` — `level_up` (UPDATE level +1, bonus +5,
  skill bonus +1, maxima; `RETURNING` the post-state row).
- `crates/gz-proto/src/combat.rs` — `LevelUpResponse`, the 7221 body.
- `services/game/src/main.rs` (boot + both swing call sites),
  `monsters.rs` (`battle_power` on spawned units and `MonsterHit`),
  `registry.rs` (`update_level` for the 119 sender echo), `movement.rs`
  (fake-implementation stub), `tests.rs` (fixtures + the invariant test).

### Fix

Grant before the death check on every landed swing: pool = kind `Exp` +
`(enemy level − 1)`, cut by the weak-monster bands (`Server-Attack.cpp:4003-4058`;
hunger/tiredness CUT), scaled by `damage/maxHP/3` while the enemy lives, whole
when it dies. A grant crossing `next_threshold` runs the `level_up` UPDATE,
mirrors the row into the session copy, restores life to the new maximum, and
answers 215 + 7221 to the 3×3 cell (self included) instead of the 270 — one
level per grant, capped at 120, exactly the legacy's order (frames, then grant
at `Hit`'s tail).

### Validation

- `crates/gz-data/src/levelup.rs::tests` — the shipped banner and rows parse,
  thresholds 75 (level 2) and 9_713_379_124 (level 120) pinned, short tables
  rejected, the tail past 120 ignored.
- `services/game/src/combat.rs::tests` — weak-gate band edges (0/30/40/60/80/100
  with the level-20 gate and the zero/negative-power arms), seed + scaling
  truncation, threshold boundary (74 fails, 75 passes), one-level-per-grant,
  cap 120, empty table never levels.
- `crates/gz-proto/src/combat.rs::tests::levelup_is_four_bytes_in_declared_field_order`
  — the 7221 body layout.
- `services/game/src/tests.rs::a_level_up_keeps_the_ddl_stat_invariant` —
  `abilities + bonus == level*5 + 35` across a level-up and a post-level spend.
- `cargo test --workspace` and `scripts/check.ps1` green after the change.
