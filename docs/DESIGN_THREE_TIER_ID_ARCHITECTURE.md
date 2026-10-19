# DESIGN — Clean 3-Tier ID Architecture

**Player band (strict) + Tier 1 / Tier 2 / Tier 3 monster pools, with mutual safe
buffering between Tier 1 ↔ Tier 2 only.**

> **Status: v3 — P0 evidence applied; P1 implementation authorized.**
> Date: 2026-10-19. §8 items 1–4 **and** the `MonsterEntityId` enum are APPROVED by
> Cat (PsiGinger); Gate 0/0b have been executed and re-anchor the client walls
> (§2.3–§2.4 — the v2 byte-scan conclusion is retracted). This commit = docs only
> (P0); code lands as the following P1 commit.
>
> This document **supersedes the two-tier draft** (`DESIGN_TWO_TIER_HYBRID_ID_ARCHITECTURE.md`,
> removed — never committed) and incorporates Cat's finalized structural design.
>
> **Relationship to the audit:** batch 2 (H3/H4, `45303a8`) shipped the interim
> mechanism — the player band `1..=8499` with runtime band checks, and the NPC/monster
> band `8500..=19999`. **Cat's final model retains the player band as a strict,
> permanent reservation** (H3's gate is *kept*, now by specification) and re-slices the
> monster space into three tiers. Nothing here touches H4 (ground-item handles), combat
> formulas, drop tables, or the `CheckLastMsgClock` gate.

---

## 0. Change log vs. the two-tier draft

| Item | Two-tier draft | **Final (this doc)** |
|---|---|---|
| "30k+" / `HighTierMonsterId` | called Tier 2 | **Tier 3 / `Tier3MonsterId`**, `30_000..=59_999` |
| Player band | proposed merged into a shared pool (D3-I), idle slots lendable | **strict `1..=8_499`, zero monster allocation or borrowing** |
| Buffering | Tier 2 spilled into Tier 1 (one direction) | **Tier 1 ↔ Tier 2 mutual**; **Tier 3 isolated** (no spill either way) |
| D1 boundary | open | **resolved:** `29_999 | 30_000` as the *structural* boundary — Gate 0 (§2.3) measured the **live** wall at `19_999`; `20_000..=29_999` is client-rebuild-gated (§4) |
| D2 dungeon home | open | **resolved:** Tier 2 band `19_000..=29_999` |
| Evidence base (§2) | — | **revised by Gate 0** (§2.3): shipped `MAX_CHAR_NUMBER = 20000`; the v2 byte-scan's "≈30,000" was a misattribution to item-unique tables |

Terminology note: "Tier 1" in this document means *world-map monster pool* — it no
longer includes players (unlike the draft's "Tier 1 base world pool").

---

## 1. Goal and scope

- **2–3× world-map monster density** (x2/x3 spawns) with headroom, via Tier 1 capacity
  plus mutual buffering.
- **Dungeon expansion:** dungeon monsters Lv 10–100 live in Tier 2, sharing buffer with
  Tier 1 so a crowded dungeon run and a world-map peak cannot starve each other.
- **Endgame/raid headroom:** Tier 3 holds ~30,000 high-level ids (Lv > 100, tunable down
  to Lv 80+) as an isolated strong-typed pool for endgame maps.
- **Compile-time domain isolation:** a Player-Session id can never land on a monster
  entity and vice versa (`PlayerCharUnique` vs `StandardMonsterId` vs `Tier3MonsterId`).
- **Legacy 16-bit wire safety** for classic maps; Tier 3 activation is explicitly
  gated on client-side widening (§4).

---

## 2. Evidence base — what ids are actually safe

### 2.1 Legacy namespace partition (server source)

| Range | Legacy meaning | Citation |
|---|---|---|
| `1..=2999` | PC (session index → `uiMakeCharUnique`) | `MAX_PERSON_NUMBER` 3000, `Char-Server.h:1006-1037` (`FindEmptyCharUnique`) |
| `3000..=8499` | instance NPC | `MAX_INSTANCE_NPC_NUMBER` 5500, same partition |
| `8500..=19999` | open-world NPC + monsters | `NPC_CHAR_UNIQUE_BASE` 8500 (`gz-data/src/npc.rs:296`), `MAX_CHAR_NUMBER` 20000 (`CommonHeader.h:556`) |
| `<= 0` or `>= MAX_CHAR_NUMBER` | `GetIDFromCharUnique` returns 0 → cannot name a session | `CharManager.cpp:205`, cited at `registry.rs:67-69` |

Full-tree sweep: **no char-id constant of 30,000 exists in the legacy source** (the
30000s are an opcode, the ITZ ring size, `MAX_PILE`, a `FieldObject` cap). Gate 0 found
no 30k char-id constant in the **shipped client** either — its 30,000s are item-unique
tables (`MAX_ITEM_UNIQUE`, §2.3).

### 2.2 Wire signedness census (this port's `gz-proto`, matches legacy frames)

| Frame / path | Field | Width | Safe ceiling |
|---|---|---|---|
| LoginMany spawn key | `make_char_unique: u16` (`loginmany.rs:74`, `game.rs:1051`) | UI16 | 65,535 |
| Response 11 logout | `encode_logout(char_unique: i32)` (`game.rs:1291`) | SI32 | — |
| Response name | `encode_response_name(… i32 …)` (`game.rs:774`) | SI32 | — |
| Order **author** | `char_unique: i32` (`order.rs:108`) | SI32 | — |
| Order **target** (monster-carrying) | `attack_order(…, target_unique: i16)` (`order.rs:235`) | **SI16** | 32,767 |
| Hit response (monster-carrying) | `char_unique: i16` (`combat.rs:217`), `si_char_unique: i16` (`combat.rs:264,361`) | **SI16** | 32,767 |
| JIM 263 (monster host/attacker) | `encode_jim(char_unique: i16, jim_char_unique: i16)` (`game.rs:1325`) | **SI16** | 32,767 |
| Person-ip / private info (PC-only) | `encode_person_ip(char_unique: i16, …)` (`game.rs:437,464`) | SI16 | never binds: PC ids ≤ 8,499 |
| Chat / status / shop / horse (PC-only) | `char_unique: i16` | SI16 | never binds: PC ids ≤ 8,499 |

**Consequence:** any *monster* id **≥ 32,768** sign-flips into the negative when it
rides hit / JIM / order-target → the shipped client indexes its lookup table with a
negative offset → out-of-range. **32,767 is the interactive ceiling regardless of table
size.** Person frames never bind because player ids are capped at 8,499 by construction.

### 2.3 Gate 0/0b — disassembly of the shipped client (`New Era.exe`)

**Verdict: `MAX_CHAR_NUMBER = 20000` in the shipped build — the char-id wall is
19,999. The v2 byte-scan's "≈ 30,000" conclusion is retracted (misattribution to
item-unique tables).**

- **Method (2026-10-19):** REA's deep providers were unavailable on this host (Ghidra
  install dir unset, Hopper is macOS-only, IDA unconfigured), so Gate 0 ran as an
  instruction-level sweep with **Capstone 5.0.7** over the PE32 `.text`
  (image base `0x400000`, section `0x401000..0xB352A1`, **2,315,624 decoded
  instructions**; exe sha256 `b917ebe868a8b4fd70098c7e78947bcd912b35cbf9cbb61af35dc06aa1f9461a`),
  classifying every `cmp r32/m32, imm32` with its guard context and pairing
  same-register follow-up compares.
- **`MAX_CHAR_NUMBER` tables:** **239** index guards of the form
  `cmp idx, 0x4E20 ; jge skip ; … mov … [base + idx*4 + …]` — dword-scaled tables of
  **20,000** entries: the inlined `NSafeTArray<T, MAX_CHAR_NUMBER>::operator[]` of
  `CharUniqueInfo` (`SI32`, `Char-Common.h:1042`), `CR`/`SessionSwitch`
  (`CharManager.h:30-38`), `clIDInfo` (`IDManager.h:38`).
- **IsNPC macro expansion:** **54** sites pair `cmp x, 0x2134 (8500)` → `cmp x, 0x4E20
  (20000)` on one register inside a one-block window — the compiled
  `IsNPC(x) = x >= 8500 && x < MAX_CHAR_NUMBER` (`CommonHeader.h:639`) with
  **MAX = 20000** — plus `cmp x, 0x4E1F (19999)` forms and the
  `lea eax, [x - 0xBB8]` instance-NPC index transform.
- **Where "30,000" really lives:** **182** word-scaled guards
  `cmp idx, 0x7530 ; … movzx … word ptr [base + idx*2] ; cwde` bound **item**-domain
  tables — `NSafeTArray<SI16, MAX_ITEM_UNIQUE> ItemRef` with
  `#define MAX_ITEM_UNIQUE 30000` (`cltItem.h:99,1214`) and the
  `MAX_MARKET_ORDER_NUMBER 30000` SI16 buffers (`Market.h:20,292`). The v2 byte scan
  counted these raw `0x7530` immediates (~371) and attributed them to
  `MAX_CHAR_NUMBER`; it also reported "0 × `0x4E20`" while the file holds **641** raw
  `20 4E 00 00` sequences — its form filter was defective. Both numbers are superseded
  by this sweep.
- **IsPC / `MAX_PERSON_NUMBER` (Gate 0b):** **217** zero-check-then-`cmp x, 0xBB8
  (3000)` sites (`x > 0 && x < 3000`, `CommonHeader.h:635`) vs **3** against 8,500 →
  the shipped `IsPC` = **`1..=2999`**, and client classification is range-based and
  live: PC `1..=2999` | instance NPC `3000..=8499` | NPC/monster `8500..=19999` |
  `≥ 20000` → `GetIDFromCharUnique` returns 0 (`CharManager.cpp:205`) / table guards
  skip → **the entity never materializes** (no `0xC0000095` bounds-raise literal exists
  anywhere in the binary — OOB is skipped, not signalled).
- **Consequence:** any char id `≥ 20000` sent to the current client is a dead or
  aliased entity, exactly as the source tree predicts. Every wall in §2.4 is re-anchored
  to this measurement; Gate 0b additionally surfaced **R-G0** (§8.7): the batch-2
  player band `3000..=8499` sits inside the client's *instance-NPC* classification and
  fails every `IsPC` gate once the login wrap crosses 2999.

### 2.4 The ceilings, mapped to Cat's tiers (Gate 0 re-anchored)

| Ceiling | Basis | Tier impact |
|---|---|---|
| **19,999** | client tables + `IsNPC` upper (`MAX_CHAR_NUMBER − 1`, Gate 0 §2.3) | **live wall for every char id**: Tier 1 + live Tier 2 slice `19_000..=19_999` |
| **29,999** | Cat's structural boundary; requires client rebuild `MAX_CHAR_NUMBER ≥ 30_000` | Tier 2 structural top — the `20_000..=29_999` slice is **P2-gated on that rebuild** (§4) |
| **2,999** | client `IsPC` upper (Gate 0b, ×217 sites) | player band currently `1..=8_499` overshoots client PC classification → **decision §8.7** |
| **32,767** | SI16 frames (§2.2) — interactive entities | **Tier 3 interim top** (P3) |
| 65,535 | UI16 spawn key | Tier 3 max `59_999` fits; full reach needs P4 widening |

### 2.5 Current `goonzu-rs` state (post batch 2 — the baseline this design re-slices)

- `MAX_CHAR_NUMBER: i16 = 20_000` (`registry.rs:70`), `PLAYER_CHAR_UNIQUE_TOP = 8499`
  (`registry.rs:82`), `SessionInfo.char_unique: i16` (`registry.rs:132`),
  `Registry::register` band check `1..=8499` (`registry.rs:358`).
- Sessions: `static NEXT_CHAR_UNIQUE: AtomicU32` + `band_char_unique(raw) =
  raw % 8499 + 1` (`main.rs:226-241`), capture override `cfg.char_unique(alloc) as i32`
  (`main.rs:605`), narrowed at register (`main.rs:1085`).
- Monsters: `Monster.unique: u16` (`monsters.rs:171`);
  `MonsterWorld { next_unique: u32, free: Vec<u32> }` (`monsters.rs:341,347`), head
  seeded `8500 + NPC_PLACEMENTS.len()` on load / `8500` on `empty()` (`monsters.rs:524,377`),
  `spawn_one` = free-first with band gate, exhaustion → `false` (A8) (`monsters.rs:592-627`),
  recycle at corpse removal (`monsters.rs:1006`).
- Static NPCs reserve fixed ids `8500 + index` (`npc.rs:311`), asserted in the legacy
  `IsNPC` range (`npc.rs:347`).

---

## 3. The architecture (Cat's finalized structural design)

### 3.1 ID partitioning

```
1        8,499  8,500      18,999 19,000      29,999 30,000                 59,999 60,000..65,535
| PLAYER  |          TIER 1     |       TIER 2          |       TIER 3          | (unused)
| 8,499   |        10,500       |        11,000         |        30,000         |
| strict  | world map x2/x3     | dungeon Lv 10–100     | high-level Lv>100     |
| reserved| ←—— mutual buffer —→ | live ≤19,999 (§2.3)  | isolated / extended   |
```

| Band | Range | Capacity | Consumers | Rules |
|---|---|---|---|---|
| **Player band** | `1..=8_499` | 8,499 | player sessions | **STRICT: no monster tier may ever allocate or borrow here** (Cat, by specification) |
| **Tier 1** | `8_500..=18_999` | ~10,500 (incl. pinned static-NPC slice `8_500..8_500+N`) | world-map monsters (x2/x3 density) | client-safe; buffers ↔ Tier 2 |
| **Tier 2** | `19_000..=29_999` | ~11,000 | dungeon monsters Lv 10–100 | structural ≤ 29,999; **live-issued ≤ 19,999** until the client rebuild (§4); buffers ↔ Tier 1 |
| **Tier 3** | `30_000..=59_999` | ~30,000 | high-level monsters Lv > 100 (tunable to Lv 80+), endgame maps | **isolated**: no spill into/out of T1/T2; strong type; client-gated (§4) |

Static NPC placements stay pinned at `8_500 + index` — a reserved slice *inside* Tier 1,
never pooled; their legacy `IsNPC` test (`npc.rs:347`) is unaffected.

### 3.2 Buffer spilling & protection policy (the matrix)

"May consumer-row draw free slots from pool-column?"

| ↓ consumer / pool → | Player band | Tier 1 | Tier 2 | Tier 3 |
|---|---|---|---|---|
| Player sessions | own | ✗ | ✗ | ✗ |
| Tier 1 spawns (world) | ✗ | — | **✓ mutual** | ✗ |
| Tier 2 spawns (dungeon) | ✗ | **✓ mutual** | — | ✗ |
| Tier 3 spawns (endgame) | ✗ | ✗ | ✗ | own |

Rules:

1. **Player band protection:** no monster tier may allocate or borrow `1..=8_499` —
   enforced by range-disjoint pools, type constructors, and the retained `register` band
   gate (three layers).
2. **Tier 1 ↔ Tier 2 mutual safe buffering:** both sit below Cat's structural boundary
   (≤ 29,999) — and the slice the current client can actually display is ≤ 19,999
   (§2.3) — so either side may draw the other's unused slots during a spawn burst
   (world x2/x3 peak, crowded dungeon run).
3. **Tier 3 isolation:** standalone pool — Tier 3 does not borrow Tier 1/2, Tier 1/2
   never spill into Tier 3. Capacity tuning for Tier-3 *pressure* is done via the
   **level-boundary config** (below), not by borrowing.
4. **Free routes by range:** a freed id returns to the pool that owns its numeric range
   (`8_500..=18_999` → T1, `19_000..=29_999` → T2, `30_000..=59_999` → T3,
   `1..=8_499` → player). A borrowed id therefore returns to its origin pool with no
   extra bookkeeping, and pool membership is a pure function of the value.

### 3.3 Spawn → tier assignment policy

```
if tier3_level_floor is enabled (config) and level >= floor   → Tier 3  (isolated pool)
else if spawn context == dungeon/instance                      → Tier 2 first (borrow T1)
else                                                           → Tier 1 first (borrow T2)
```

- `tier3_level_floor: Option<u16>` — **the tunable Cat specified**: nominal `Some(100)`
  ("Lv > 100"), may be lowered to `Some(80)` if Tier 2 shows sustained pressure.
  **Default `None` (disabled)** until the client gates of §4 pass — while disabled,
  high-level monsters spawn through the standard pools exactly as today, so no
  behavior regresses before the client rebuild.
- Static NPCs are not allocated at all (pinned ids, §3.1).
- Edge: dungeon spawns below Lv 10 still use Tier 2 (the Lv 10–100 band is descriptive;
  the level split that matters is the Tier-3 floor).

### 3.4 Domain types (`ids.rs`) — Cat deliverable 1

```rust
use std::num::NonZeroU16;

/// Session domain — STRICTLY the player band. Construction is the only way in.
pub struct PlayerCharUnique(NonZeroU16);      // invariant: 1..=8_499

/// Standard monster domain — covers Tier 1 AND Tier 2 (8_500..=29_999).
/// Tier membership is allocator state, decided by value range, not by type.
pub struct StandardMonsterId(NonZeroU16);     // invariant: 8_500..=T2_TOP

/// Tier 3 strong type — extended/endgame range, isolated pool, client-gated.
pub struct Tier3MonsterId(NonZeroU16);        // invariant: 30_000..=59_999

/// World storage: one `monsters: Vec<Monster>` for AI/combat as today; the enum
/// exists only so both strong types can be stored without ever exposing a bare
/// int — variants wrap the strong types, nothing else can construct them.
pub enum MonsterEntityId {
    Standard(StandardMonsterId),
    Tier3(Tier3MonsterId),
}
```

- Private-field constructors + `TryFrom` validate the band (`0`, negative, foreign band
  all rejected); **no `From` impl exists between `PlayerCharUnique` and either monster
  type** — cross-assignment is a compile error (Cat spec 3).
- Wire conversions centralized on the types, so exactly one place proves
  range-vs-width: `StandardMonsterId::si16()` is infallible (`≤ 29_999 < 32_767`);
  `Tier3MonsterId::try_si16() -> Option<i16>` **refuses `> 32_767`** until the P4 frame
  widening; `MonsterEntityId::u16()` serves the UI16 spawn key / SI32 paths.
- Constants live beside the types:
  `PLAYER_BAND_TOP = 8_499`, `T1_TOP = 18_999`, `T2_FLOOR = 19_000`,
  `T2_TOP = 19_999` (= `CLIENT_TABLE_TOP − 1`, the live wall; the **P2** phase rewrites
  this one constant to `29_999` only after the §4 client rebuild),
  `T3_FLOOR = 30_000`, `T3_TOP = 59_999`, `WIRE_SI16_TOP = 32_767`,
  `CLIENT_TABLE_TOP = 20_000` (Gate 0, §2.3).

### 3.5 Allocator design — Cat deliverable 2

```rust
/// Two isolated locks. The player band has NO pool here at all: batch-2's wrap
/// counter in main.rs keeps minting it (T6), so nothing in this allocator can
/// produce a player id — isolation by absence, not by discipline.
pub struct IdAllocator {
    standard: Mutex<DualTierPool>,  // T1 ↔ T2 — the only borrowing pair
    tier3:    Mutex<WindowPool>,    // 30_000..=59_999 — standalone
}

struct WindowPool { floor: u16, top: u16, free: Vec<u16>, head: u16 } // floor<=head<=top+1

struct DualTierPool {
    t1: WindowPool,  // floor = 8_500 + NPC_PLACEMENTS.len() (static NPC slice pre-reserved), top = 18_999
    t2: WindowPool,  // floor = 19_000, top = T2_TOP (= 19_999, §2.3)
}

impl IdAllocator {
    pub fn alloc_tier1(&self) -> Option<StandardMonsterId>; // t1 free → t1 head → borrow t2 free/head
    pub fn alloc_tier2(&self) -> Option<StandardMonsterId>; // t2 free → t2 head → borrow t1 free/head
    pub fn alloc_tier3(&self) -> Option<Tier3MonsterId>;    // isolated; None on exhaustion (A8 refuse)
    pub fn free_monster(&self, id: MonsterEntityId);        // corpse removal, after the 11 (existing H3 rule)
}
```

- **Mutual borrowing** = `alloc_tierN` falls through to the *sibling* pool's free list,
  then its head. Free routes by range (§3.2 rule 4) → borrowed ids go home, capacity
  never leaks or double-issues (a live id is in no free list; each pool mints only its
  own disjoint range).
- **Player allocation stays exactly as batch 2** (`NEXT_CHAR_UNIQUE` wrap
  `raw % 8499 + 1`, `main.rs:226-241`, T6-asserted) — deliberately **not** folded into
  `IdAllocator` (the deliverable scopes it to standard + Tier 3; relocating tested code
  buys no safety). The band is enforced on that side by the wrap, the `register` gate,
  and `PlayerCharUnique::TryFrom`.
- **Exhaustion refuses and logs** per pool (A8, mirrors `FindEmptyCharUnique` → 0) —
  never a panic, never a fallback into a forbidden band.
- Capture mode unchanged: `cfg.char_unique(…)` maps the *displayed* id verbatim
  (`main.rs:605` shape preserved).

---

## 4. Activation gates — the client is the dependency

| Tier band | Needs | Status |
|---|---|---|
| Player `1..=8_499` | nothing beyond today | live (batch 2) — **⚠ client `IsPC` covers only `1..=2999` → R-G0, §8.7** |
| Tier 1 `8_500..=18_999` | nothing beyond today | live (batch 2); re-sliced at P1 |
| Tier 2 up to `19_999` | nothing beyond today (= `CLIENT_TABLE_TOP − 1`, Gate 0) | live (batch 2) |
| Tier 2 `20_000..=29_999` | **client rebuild** `MAX_CHAR_NUMBER ≥ 30_000` (Gate 0/0b **executed**: wall measured 19,999; ids ≥ 20,000 are dead/aliased on the current client, §2.3) | **P2** |
| Tier 3 `30_000..=32_767` | client rebuild: `MAX_CHAR_NUMBER ≥ 32_768` (recommend 60_000+; one macro + rebuild, Cat's fork) | **P3** |
| Tier 3 `32_768..=59_999` | above **and** SI16 → UI16 widening on the monster-carrying frames (hit 268, JIM 263, order-target `siPara2` — server parse/encode **and** the same frames in the client fork, plus a fork-side sweep of other `CharUniqueInfo`-style index sites) | **P4** |

Until its gate passes, Tier 3 does not issue (`tier3_level_floor = None` default,
§3.3): high-level monsters keep spawning through the standard pools within today's
wire-safe ranges — **no crash path, no capability regression.** Same logic covers
`20_000..=29_999` pre-rebuild: `T2_TOP = 19_999` simply never issues there (§3.4).

**Gate 0 result (run 2026-10-19, §2.3):** verdict `MAX_CHAR_NUMBER = 20000` via
Capstone instruction sweep (REA providers unconfigured on the host) — the prior
byte-scan's ≈30,000 was `MAX_ITEM_UNIQUE` item tables. `T2_TOP = CLIENT_TABLE_TOP − 1`
keeps every tier a single-file constant if a future fork rebuild changes `N` again.

---

## 5. Exact code changes (blueprint — nothing below is applied)

### 5.0 New module `services/game/src/ids.rs`

Types, constants, and pools exactly as §3.4/§3.5, plus:

```rust
impl StandardMonsterId {
    pub fn si16(self) -> i16;                 // infallible: ≤ T2_TOP < WIRE_SI16_TOP
    pub fn u16(self) -> u16;
    pub fn tier(self) -> Tier;                // diagnostic helper: Tier1 / Tier2 by range
    pub fn try_from_u16(v: u16) -> Result<Self, IdError>;   // 8_500..=T2_TOP or reject
}
impl Tier3MonsterId {
    pub fn try_si16(self) -> Option<i16>;     // None above WIRE_SI16_TOP until P4
    pub fn u16(self) -> u16;
    pub fn try_from_u16(v: u16) -> Result<Self, IdError>;   // 30_000..=T3_TOP or reject
}
impl PlayerCharUnique {
    pub fn si16(self) -> i16;                 // infallible: ≤ 8_499
    pub fn try_from_si16(v: i16) -> Result<Self, IdError>;  // 1..=8_499 or reject
}
```

### 5.1 `main.rs`

```diff
-// (unchanged) static NEXT_CHAR_UNIQUE + band_char_unique wrap 1..=8499 — Cat: strict band
 // accept path (line 605) — unchanged in shape (capture mapping preserved):
 let char_unique = cfg.char_unique(allocate_char_unique()) as i32;

 // register call site (line 1085) — the type carries the band:
 SessionInfo {
-    char_unique: i16::try_from(char_unique).map_err(…)?,
+    char_unique: PlayerCharUnique::try_from_si16(i16::try_from(char_unique)?)?,
     …
 }

 // world load (one new argument — the shared standard/tier3 pools):
-MonsterWorld::load(&data_root)
+MonsterWorld::load(&data_root, Arc::clone(&ids))
```

`ids = Arc::new(IdAllocator::new())` is created in `main()` before the accept loop.
Session teardown keeps its order and stays as batch 2: `registry.remove(key)` fans the
11 — player ids are **not** recycled here (the wrap returns them at `8_499`, T6).

### 5.2 `MonsterWorld` (`monsters.rs`)

```diff
 pub(crate) struct MonsterWorld {
     groups: Vec<FieldGroup>,
     monsters: Vec<Monster>,
-    /// monotonic half of the id allocator (audit H3)
-    next_unique: u32,
-    /// ids freed at corpse removal, recycled by spawn_one (audit H3)
-    free: Vec<u32>,
+    /// the standard (T1↔T2) + Tier-3 pools; player band never reachable through it
+    ids: Arc<IdAllocator>,
     respawn: …, corpses: …, map: …, block_by_kind: …, dormancy: …,
 }

 fn spawn_one(…) -> bool {
-    let band = NPC_CHAR_UNIQUE_BASE..MAX_CHAR_NUMBER;
-    let unique = loop { /* free.pop with band gate; head >= MAX_CHAR_NUMBER => false */ };
+    // world-map context → Tier 1 first, borrowing Tier 2 on a burst (§3.2)
+    let unique = match self.ids.alloc_tier1() {
+        Some(u) => u,
+        None => { logln_err!("monsters: standard id pool exhausted at {} monster(s)", …); return false; } // A8 kept
+    };
-    let monster = Monster { unique: unique as u16, … }
+    let monster = Monster { unique: MonsterEntityId::Standard(unique), … }
 }

 // despawn_corpses (line 1006), same point in the sequence (after the 11):
-    self.free.push(u32::from(monster.unique));
+    self.ids.free_monster(monster.unique);
```

Future spawn APIs (P2/P3 consumers — dungeon & endgame maps):
`spawn_dungeon(…) -> Option<StandardMonsterId>` (alloc_tier2) and
`spawn_tier3(…) -> Option<Tier3MonsterId>` (isolated). `MonsterWorld::empty()` builds a
**fresh private `IdAllocator`** — test isolation, same posture as today's per-world
`free`/`next_unique`.

### 5.3 `registry.rs`

```diff
 pub const PLAYER_CHAR_UNIQUE_TOP: i16 = 8499;   // RETAINED — Cat: strict, permanent
- pub const MAX_CHAR_NUMBER: i16 = 20_000;       // retired → ids::{T1_TOP,T2_TOP,T3_TOP}
 pub struct SessionInfo {
-    pub char_unique: i16,
+    pub char_unique: PlayerCharUnique,           // cross-assignment impossible at compile time
 }
 pub fn register(…) -> bool {
     // the band check RETAINS its runtime form (Cat: strict player band = defense in depth
     // alongside the type):
-    if !(1..=PLAYER_CHAR_UNIQUE_TOP).contains(&info.char_unique) { … return false; }
+    if !(1..=PLAYER_CHAR_UNIQUE_TOP).contains(&info.char_unique.si16()) { … return false; }
     …
 }
```

`main.rs`'s world-entry rejection (`register == false` → session refused, batch 2) stays,
now failing when `try_from_si16` fails for out-of-band capture ids first.

### 5.4 Ripple inventory — and what does NOT change

Typed propagation (`.si16()` / `.u16()` at encode sites, `try_from` at parse sites):

- `Monster.unique: u16` → `MonsterEntityId` and every reader: combat.rs
  (`attacker_unique`, hit/jim/order paths), main.rs fan sites, world.rs person-info
  serialization, `position()` / lookups (`monsters.rs:798-862`).
- `SessionInfo.char_unique` readers: chat 118–122 paths, friend fans,
  `find_by_charunique` (`registry.rs:560`).
- `gz-proto`: **no struct changes** — widths already match §2.2; only argument types at
  call sites move. Inventory step at implementation:
  `grep -n "char_unique\|make_char_unique\|unique" crates/gz-proto services/game`.
- Tests: T6 (player-band wrap — keeps its assertions, band unchanged), `monsters.rs:2059-2086`
  free-list assertions → allocator, register-refusal fixtures.

**Untouched:** H4 ground handles and `sweep_expired_ground`; `cfg.char_unique` capture
semantics; static NPC id derivation (`npc.rs`) and its `IsNPC` test; combat damage /
drop formulas; `CheckLastMsgClock`; dispatch; DB schema (**no migration** — this is
pure in-memory namespacing); session allocation wrap (batch 2, `main.rs:226-241`).

---

## 6. Rollout phases P0–P4 (each = one audit-§8 batch: gates green, one commit)

| Phase | Content | Client dependency | Capacity delivered |
|---|---|---|---|
| **P0 — evidence (DONE 2026-10-19)** | Gate 0 disasm run (§2.3: verdict `MAX_CHAR_NUMBER = 20000`, prior byte scan retracted); Gate 0b classification pass (IsPC/IsInstanceNPC/IsNPC measured, R-G0 raised); SI16 monster-carrying frame inventory (hit 268 / JIM 263 / order-target confirmed; person frames excluded as PC-only) | none | — |
| **P1 — structure at today's wire behavior** | `ids.rs` types + `IdAllocator` (standard `DualTierPool` + isolated Tier-3 `WindowPool`; player wrap untouched §3.5) wired through main/registry/monsters; **T2 top capped at `19_999`** so no id beyond the measured wall (§2.3) is ever issued; tier-assignment present but `tier3_level_floor = None`; T7 reworked, T9–T12 added; `MAX_CHAR_NUMBER` retired | none | same id *ranges* as today (`8_500..=19_999`); id *sequence* shifts once T1 fills (opaque to clients) |
| **P2 — Tier 2 opens** | `T2_TOP` `19_999 → 29_999` after the client rebuild below | **client rebuild `MAX_CHAR_NUMBER ≥ 30_000`** (was "evidence sign-off" — Gate 0 converted it into a build dependency) | standard pools **21,500** (−N) ≈ 1.9× today's 11,500 — the x2/x3 world density + crowded-dungeon headroom, shared by mutual buffering |
| **P3 — Tier 3 activation** | `tier3_level_floor` default `Some(100)` (tunable to `Some(80)` under Tier-2 pressure); `spawn_tier3` / endgame consumers; **T3 issuance capped `30_000..=32_767`** (SI16-interactive) | client rebuild `MAX_CHAR_NUMBER ≥ 60_000` | +2,768 isolated endgame ids |
| **P4 — full Tier 3** | SI16 → UI16 widening on hit 268 / JIM 263 / order-target (server parse/encode + client-fork frames; fork sweep of other `CharUniqueInfo` index sites); `T3_TOP 59_999` fully reachable; `try_si16` retired at migrated sites | client fork frame widening | full **30,000** Tier-3 ids |

Capacity summary: player 8,499 (unchanged, strict) · standard 11,500 → **21,500** (P2)
· Tier 3 → 2,768 (P3) → **30,000** (P4). Total namespace 59,999.

---

## 7. Safety verification

### 7.1 Invariants (asserted by construction and by test)

1. **Disjointness:** the four bands never overlap; each pool mints only its own range;
   a live id is in no free list → no id is ever issued twice.
2. **Player-band protection:** no allocation path can mint a player id from the monster
   side — and none exists on the player side either (the batch-2 wrap, T6): asserted by
   range-disjoint pool floors (every minted id ≥ `T1_FLOOR`), the `PlayerCharUnique`
   constructor, and the retained `register` band gate.
3. **Buffer matrix enforcement:** borrowing exists *only* inside `DualTierPool`
   (T1↔T2). `alloc_tier3` never falls through (Tier 3 has no sibling).
4. **Free-by-range:** every freed id lands in its origin pool (pure function of value) —
   borrow cannot leak or duplicate capacity.
5. **Leave-frame ordering:** free only after the domain's 11 has fanned — monsters at
   corpse removal (existing H3 rule), sessions after `Registry::remove`.
6. **Exhaustion refuses** (A8): `None` + log per pool; never a panic, never a fallback
   into a forbidden band.
7. **Client-safety:** no standard id above `T2_TOP` (= `19_999` = `CLIENT_TABLE_TOP − 1`,
   Gate 0) is issued until the §4 client rebuild; interactive frames never exceed
   `WIRE_SI16_TOP` (P3 cap); `tier3_level_floor = None` until P3.
8. **Capture mode:** `cfg.char_unique` display mapping unchanged; replay ids never
   enter pool bookkeeping.

### 7.2 Test plan (house patterns: DB-free unit tests; live-DB tests `#[ignore]`)

- **T9 — pool exclusivity & matrix:** interleave `alloc_tier1 / alloc_tier2 /
  alloc_tier3 / free` to exhaustion; assert: no live id issued twice,
  every issued id ∈ its band, `alloc_tier3` never returns `< 30_000`, standard never
  returns `< 8_500` or `≥ 60_000`; exhaustion → `None`
  (A8), no panic. (Player-band issuance is covered by T6 (wrap) + T11 (type) — there
  is no player method to interleave, §3.5.)
- **T10 — mutual buffering:** T2 head exhausted → `alloc_tier2` draws T1 free/head and
  the id is a `StandardMonsterId` in the T1 range; on free it returns to the **T1**
  list (origin by range); symmetric for T1→T2. Tier-3 and player pools stay untouched
  throughout (matrix negatives).
- **T11 — type gates:** `PlayerCharUnique::try_from_si16` rejects 0/negative/`> 8_499`;
  `StandardMonsterId::try_from_u16` rejects `< 8_500`, `> 29_999`;
  `Tier3MonsterId::try_from_u16` rejects `< 30_000`, `> 59_999`;
  `Tier3MonsterId::try_si16` → `None` above `32_767`; no `From` between domains
  (compile-fail doctest or `trybuild` if the house adopts it).
- **T12 — tier assignment:** level floor routing (`Some(100)` → Lv 100 world spawn stays
  standard, Lv 101 → Tier 3), `None` default keeps everything standard; dungeon context →
  `alloc_tier2` preferred.
- **Reworks:** T6 keeps its player-band-wrap assertions (band unchanged by design);
  `monsters.rs:2059-2086` free-list assertions retarget at the allocator (fresh
  allocator via `MonsterWorld::empty()` stays DB-free).
- **Unchanged:** T8 (`#[ignore]`, live-DB ground sweeper), H1/H2 backpressure and
  handshake tests, capture-replay suites (`cfg.char_unique` path asserted unchanged).
- **Static audit step (P1):** grep that no id minting survives outside `ids.rs` —
  `fetch_add` on char-unique counters, `next_unique`, `as u16` in spawn paths.

---

## 8. Approval record and open items

**APPROVED by Cat (PsiGinger), 2026-10-19** — items 1–4 as originally listed:

1. This document (v3; filename corrected from the deleted two-tier draft).
2. Gate 0 disasm run — **executed**, §2.3 (verdict `MAX_CHAR_NUMBER = 20000`;
   method note below).
3. P1 interim cap at `19_999` — approved; Gate 0 shows it equals the true live wall,
   so the cap is now a correctness boundary, not just conservatism.
4. P3 pre-gate behavior: high-level monsters keep spawning through standard pools —
   approved.
5. `MonsterEntityId { Standard, Tier3 }` storage enum — approved.

**Still open:**

6. **Client-fork scheduling (yours):** P2 = `MAX_CHAR_NUMBER ≥ 30_000` rebuild
   (Gate 0 converted this from an evidence gate into a build dependency), P3 = ≥ 60_000,
   P4 = SI16→UI16 on hit 268 / JIM 263 / order-target + fork-side sweep of other
   char-unique index sites.
7. **R-G0 — player band vs client `IsPC` (new, from Gate 0b).** Measured `IsPC =
   1..=2999` (217 zero-check sites, §2.3); batch-2's wrap issues ids `3000..=8499`
   after ~3k cumulative logins, and the client classifies that range as **instance NPC**
   (`IsInstanceNPC`), so person paths gated on `IsPC` (draw / chat / mouse / auto-hunt)
   may mishandle such a *player*. P1 keeps `1..=8_499` exactly as specified. **My
   recommendation:** narrow `PLAYER_CHAR_UNIQUE_TOP` `8499 → 2999` (legacy-exact: wrap,
   register gate, `PlayerCharUnique` invariant, T6 — one-constant follow-up batch).
   Needs your call; not folded into P1.
8. **Audit bookkeeping:** assign H-item numbers/batches for P1–P4 in
   `AUDIT_WHOLE_SYSTEM_HARDENING.md` §8 (or track this design as a separate track).
9. **Method note:** REA's deep providers were unconfigured on this host (no Ghidra
   install, Hopper macOS-only, IDA absent), so Gate 0 ran as a Capstone 5.0.7
   instruction sweep. Set `GHIDRA_INSTALL_DIR` for a provider-backed re-run with
   Evidence records; the verdict is unlikely to move (two independent macro families —
   table guards and `IsNPC` expansions — agree at 20,000).

---

## 9. References

- Legacy source: `Char-Server.h:1006-1037` (`FindEmptyCharUnique` partition),
  `CommonHeader.h:556` (`MAX_CHAR_NUMBER`), `CharManager.cpp:205,208`,
  `Char-Common.h:1042` / `char.cpp:107` (`NSafeTArray<SI32, MAX_CHAR_NUMBER>` client
  table), `Client-Person.cpp:1233` (hit index), `handoff.md:455,3317`.
- Shipped client: `New Era Live\New Era.exe` — **Gate 0 Capstone sweep (§2.3)**:
  463 × `cmp …, 0x4E20` (239 dword table guards) vs 372 × `0x7530` (182 = SI16
  *item*-unique guards, `MAX_ITEM_UNIQUE 30000`); 217 × zero-check→`0xBB8` (IsPC);
  54 × `8500→20000` IsNPC pairs; no `0xC0000095` literal. Earlier byte-scan claim
  "≈371 × `0x7530` → MAX ≈ 30,000, 0 × `0x4E20`" **retracted** (misattribution +
  broken form filter; raw file holds 641 × `20 4E 00 00`).
- This port: `registry.rs:70,82,132,351-379`, `main.rs:226-241,605,1080-1090`,
  `monsters.rs:168-171,341-347,592-627,1006`, `crates/gz-data/src/npc.rs:296-348`,
  `crates/gz-proto` census §2.2.
- Prior batches: audit H1–H4 → `a085c0d` (batch 1), `45303a8` (batch 2).
