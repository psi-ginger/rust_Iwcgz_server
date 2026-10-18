# Whole-System Stress Audit & Hardening Report

- **Date:** 2026-10-19
- **Baseline:** commit `0bea4e0` (working tree clean, 193 tests green as of that commit)
- **Mode:** read-only audit — no code was modified during the audit; this document is the only artifact
- **Disciplines applied:** Scrutinize (outsider attack, actual-path tracing), Debug-Mantra
  (reproduce → trace → falsify → cross-reference), RCA, preventive-protection design
- **Status:** all hardening items **decided by the owner (Cat)** — see §6. Ready to implement in the
  batches of §8. Code changes are intentionally *not* made by this document.

Evidence codes used inline: **[V]** verified by direct source read during the audit,
**[S]** subagent-verified and spot-checked at the cited line, **[R]** requires runtime confirmation.

---

## 1. Executive summary

A whole-stack attack pass over `goonzu-rs` (network framing, session lifecycle, registry, movement/
geography, combat/skill dispatch, monster AI/dormancy, items/persistence/IDs) found **two P0
liveness/backpressure gaps, two P1 resource-cliff bugs, and five P2–P3 correctness gaps**. The
packet-framing layer, registry data-race model, combat throttling, dormancy state machine, drop-roll
invariants, and item arithmetic all **passed falsification** and are recorded as verified-safe (§4) so
they are never re-audited from scratch again.

The two P0s share one root: the per-connection read loop performs socket writes inline, with no write
deadline, into an unbounded queue — a peer that stops reading can pin a session (task + registry entry
+ account login claim) and grow memory at the world's broadcast rate. The legacy bounded its send
buffers (`ITZSessionFactory.h:15`); the port lost that bound.

The two P1s share another: **unbounded resources assumed finite** — the ground-item handle sequence
cliffs at 65,535 cumulative drops (after which every drop kills the session), and the NPC id pool is
monotonic without recycling (the legacy recycled; see §6/H3 verification).

## 2. Scope & method

Target subsystems (all five, plus the shared registry):

1. Network & session lifecycle — framing, drop/reconnect edges, zombie sessions, concurrent mutation
2. World geography & movement — boundaries, gate warps, stepping, corner paths, coordinate overflow
3. Combat & skill dispatch — hit throttle, damage-formula edges, animation pacing, dead actors
4. Monster world & AI dormancy — MapDormancy transitions, idle-roll invariants, boss cooldowns,
   corpse/respawn races
5. Item & state persistence — grants, drop rolls, ID allocation/recycling

Method per finding: concrete `file:line` trace → **falsify first** (guards, existing tests, and
counter-arguments actively sought) → cross-reference the legacy tree where the cited file exists →
only then rank. Findings that failed falsification are in §4, not §3.

## 3. Phase A — attack report (ranked)

| # | Sev | Subsystem | Finding |
|---|-----|-----------|---------|
| A1 | **P0** | Session/Network | No write deadline × unbounded outbound queue × no accept cap → stalled-reader memory DoS + unremovable zombie sessions (task, registry entry, login claim) |
| A2 | **P0** | Session/Network | Handshake window resets on every partial read; established sessions have no liveness timer; doc at `main.rs:610-613` does not match code |
| A3 | **P1** | Persistence/IDs | `char_unique` wraps into the NPC id band after ~8.5k lifetime connections; player↔player collision at 19,999 — regression vs legacy banded ids |
| A4 | **P1** | Persistence | Ground-item handle sequence cliffs at 65,535 cumulative drops → every later 307 drop kills the session |
| A5 | **P2** | Persistence/Combat | Kill path consumes the monster before rewards persist; DB error mid-kill = disconnect with corpse gone; full bag silently destroys the roll |
| A6 | **P2** | Geography/Movement | Player movement has zero server-side validation (walkability/bounds/distance) while monsters get a tile-mask invariant |
| A7 | **P2** | Combat/Movement | Death-state gaps: a provably false doc premise dropped a legacy guard; a killed player keeps stepping and can warp |
| A8 | **P3** | Monster world | NPC id pool finite and monotonic (~11.5k kills); respawn silently stops; no test |
| A9 | **P3** | Registry | Global-`Mutex` O(N) fan walks per broadcast; panic → poison-recovery leaves half-mutated memory (deliberate, documented) |
| A10 | **P4** | Misc | Slot-validation asymmetry, unchecked crit `×2`, citation mismatches, stale docs |

### A1 — Stalled writer: unbounded queue + no deadline + no cap [V]

- **Evidence:** queue `main.rs:576` (`mpsc::unbounded_channel`), stored per session
  `registry.rs:160`; writers `send_to` (`registry.rs:613-618`), `broadcast_all` (`:630-646`),
  `fan`/`broadcast_near*` (`:790-851`), AI task (`main.rs:313`) — none check depth; write path
  `send_raw` (`main.rs:1759-1763`) is a bare `stream.write_all(...).await` with **no**
  `tokio::time::timeout` anywhere in production code (only registry tests `:1146,1153`);
  accept loop `main.rs:388-419` spawns per accept with no cap.
- **Fail path:** registered peer stops reading → kernel send buffer fills → `write_all` parks inside
  the `LoopTick::Send` branch (`main.rs:642-644`) → the `select!` (`:630-640`) is no longer polled,
  so reads, `close`, and `out_rx` are all frozen → every world task keeps enqueueing into a queue
  that can never drain → memory grows at broadcast rate × total sessions (the zombie also inflates
  **every other session's** fan-out — an O(N²) amplifier). FIN/RST is not noticed while parked.
  `Registered::drop` never runs → registry entry + `logins` claim pinned; the duplicate-login kick
  (`registry.rs:287-296`) only queues a notice the parked loop cannot read.
- **Falsified attempt:** cleanup-on-exit covers every error path (§4) — but exit requires the task
  to run, and it cannot while parked; `send_to` drops frames for *removed* sessions, but this
  session is not removed. A dead peer is eventually bounded by TCP retransmit failure
  (~15–30 min, **[R]**); a **live peer that never reads is unbounded**.
- **Legacy cross-ref:** `CreateFactory(..., siRecvBufSize, siSendBufSize)` (`ITZSessionFactory.h:15`)
  — the legacy had configured bounds on both buffers. Architecture regression, not parity.

### A2 — Liveness windows don't compose [V]

- **Evidence:** `main.rs:618-619` rebuilds `Some(cfg.timeouts.handshake)` **fresh on every loop
  iteration**; a successful read re-arms a full 10 s. `handshake_expired` (`session.rs:336-342`)
  returns `None` once `Established`. Post-establish `read_window` = min(movement, attack) deadline
  or `None → std::future::pending()` (`main.rs:637`) — an idle established session has no timer at
  all. `note_activity` (`registry.rs:436-446`) is consumed only by gameplay AI. The legacy's
  3-minute `AutoLogOut` is documented as unported (`session.rs:49-53`).
- **Repro (deterministic):** pre-handshake client emits 1 byte of a declared-64992 frame every 9.9 s
  → wins the read race every iteration → never `TimedOut`, never `establish()` → task + socket +
  ≤65 KB held forever; N connections → unbounded tasks. The doc comment at `main.rs:610-613`
  ("the handshake window runs from accept to the first inbound frame") states the fixed-start
  behavior the code does not implement.
- **Owner decision note:** the fix is an **absolute** pre-handshake deadline only — established
  sessions deliberately have **no idle kick** (see §6/H2; GoonZu gameplay is legitimately AFK for
  hours).

### A3 — Shared id space without a range invariant [V]

- **Evidence:** `allocate_char_unique` wraps mod 20,000 (`main.rs:190-194`), bound `19_999`
  (`main.rs:176`); monsters allocate from `8500 + NPC_PLACEMENTS.len()` upward (`monsters.rs:513`,
  base `gz-data/src/npc.rs:296`). The overlap band `[8500+len, 19999]` is reached after that many
  **lifetime** connections. Both flow the same `i16` wire space: `combat.rs:478`,
  `monsters.rs:923`, monster `target: Option<i16>` (`monsters.rs:1192`) resolved via
  `find_by_charunique` (`registry.rs:502-511`), which on collision returns the lowest key —
  documented by test (`registry.rs:1668-1693`), not prevented.
- **Blast:** after collision, frames naming an id are player-vs-monster ambiguous in the client's
  single `CharUniqueInfo` table (`main.rs:174-175`); after a player↔player wrap a monster's swing
  can land `set_life` on the **wrong session**, and `find_by_charunique`-routed replies route to
  the wrong holder.
- **Legacy cross-ref (decisive):** legacy ids were banded and recycled — see §6/H3 verification
  (`Char-Server.h:1006-1037`). The port's wrap-mod-20000 counter crossing the band is a regression.

### A4 — Handle-sequence cliff becomes a session-killer [V]

- **Evidence:** `sql/0005_world_item_handle.sql:35-40` documents the cliff ("a long-lived database
  would exhaust the `UI16` range after 65535 drops"), enforced by `CHECK (obj_index < 65536)`
  (`:54-56`), fed by `nextval` (`gz-db/src/item.rs:1262-1266`). On exhaustion `nextval` errors →
  `drop_slot` → `items.rs:201-203` `?` → dispatch loop → `handle_client` `Err` → disconnect
  (`main.rs:401-417`). From then on every drop attempt kills the dropping player — a soft resource
  limit becomes an account-griefing crash.
- **Falsified:** the `ON CONFLICT` ground-cell path never runs — the sequence fails first.

### A5 — Kill path: consume-then-persist, one `Result` for fatal and transient [V]

- **Evidence:** monster removed + corpse/respawn pushed inside the world lock (`monsters.rs:862-883`);
  afterwards `characters.add_exp(...).await?` (`combat.rs:527`) — any DB error disconnects with the
  kill consumed, corpse gone, zero exp. Drop grant failure: `Ok(None)`/`Err` → log only
  (`combat.rs:547-575`) — the rolled item is silently destroyed (the quest path at least refuses
  explicitly, `quest/reward.rs:149-162`).
- **Legacy cross-ref:** legacy `GetExp` (`Server-Attack.cpp:4138`) likewise ran after the in-memory
  kill, but its async DB layer did not tear down the session on error — session-kill on storage
  failure is a port policy (the single `?` channel), not parity.

### A6 — Player tiles unvalidated; monsters validated [V]

- **Evidence:** `OrderData::move_target` → `(i16, i16)` (`gz-proto/order.rs:159-163`) bounds
  coordinates (overflow **falsified**), but `is_walkable` has zero call sites in `movement.rs`;
  `handle_userorder` persists any dest and arms stepping (`movement.rs:737-745`); `advance` never
  checks tiles (`movement.rs:303-315`). Contrast `advance_walks`, which aborts a monster step
  outside its group mask — "a monster must never be left on a tile a client cannot draw it on"
  (`monsters.rs:1031-1039`) — and `approach_tile` which plans over walkable masks (`monsters.rs:781+`).
  The machinery exists; players just don't use it.
- **Blast:** crafted ORDER_MOVE dest → walk through walls/voids, persisted
  (`characters.update_position`), fanned as 241 to observers.
- **Legacy cross-ref:** `DoMsg_GAMEMSG_REQUEST_USERORDER` (`Server-Person.cpp:2698-2743`) has no
  alive or tile guard — straight to `LetOrder`. Entry-layer parity; the deeper `char-order.cpp`
  move chain was not traced **[R]**. The port made stepping server-authoritative, so the missing
  gate is inconsistent with the port's own model regardless — hardening approved (§6/H6).

### A7 — Death-state gaps anchored on a false doc premise [V]

- **Evidence:** `movement.rs:1144` (doc on `handle_leave_request`) asserts *"`life` never reaches
  zero"* to justify dropping the legacy's death guard on leave requests
  (`Server-System.cpp:1195/:1239` per that doc) — but the port itself zeroes life: `set_life`
  (`registry.rs:862-866`, applied at `monsters.rs:1162`) and the death branch `main.rs:341`.
  Provably false premise. Meanwhile `handle_userorder` (`movement.rs:712-841`) and the step path
  have no life gate → a killed player keeps walking to dest and can issue orders / gate warps while
  dead.
- **Falsified:** auto-attack self-clears on death (first dead swing → `NoTarget` → re-arm `None`,
  `main.rs:782`); monsters stop engaging dead players (`monsters.rs:1112, 1188`); swinging while
  dead is blocked on all three arms (`combat.rs:414`, `main.rs:1369`).
- **Legacy:** `REQUEST_USERORDER` has no alive guard either → *order acceptance* is parity; whether
  the legacy death path cleared in-progress moves is untraced **[R]**. Hardening approved (§6/H7).

### A8 — Finite NPC id pool, unpinned [V] — superseded by the H3 verification

`next_unique` only increments; `spawn_one` refuses at `MAX_CHAR_NUMBER` with one log line
(`monsters.rs:581-607`); the respawn entry is consumed before the attempt (`monsters.rs:986`) →
permanent capacity deficit after ~11.5k kills, silent thereafter, no test. The legacy mints a fresh
"empty" unique per spawn (`NPC.cpp:976`, `:1110` → `FindEmptyCharUnique`) — **not** placement-stable
— but its empty-slot search **recycles freed ids**, so it never exhausts below band size. See §6/H3.

### A9 — Registry scalability posture [V]

One `Mutex<HashMap>` for all sessions; every broadcast walks N sessions under the lock
(`registry.rs:790-851`); `info()` clones per lookup; poison recovered mid-critical-section by
design (`registry.rs:235-239`, `combat.rs:431`, `main.rs:307`) — a panic can leave
corpse-without-respawn style half-state over a running server. **Falsified as a data race**
(single-lock atomicity) and **as a deadlock** (the `Registry` holds no handle to the monster world,
so no lock-order cycle exists — monsters→registry nesting in `hit_by_player` is one-way).

### A10 — P4 consistency batch [V]

- Slot validation asymmetry: 307/7223 silently ignore out-of-range slots (`items.rs:191-198`,
  `:552-559`) while 303/341 return `RepositoryError::Invalid` → session-kill
  (`gz-db/src/item.rs:880-885`, `:1005-1010`). Same trust boundary, two behaviors.
- `apply_critical`: `damage * 2` unchecked (`combat.rs:146`) — debug panic / release wrap under
  corrupt data only (all formulas are data-trust).
- Citation nits: `combat.rs:408` cites `Server-Person.cpp:2758` for the *attacker*-life guard; `:2759`
  actually guards the *target* (the port check is a safe superset). `handle_revive`'s hand-rolled
  `Gate` literal (known parked item). Stale docs: `docs/DESIGN_MAP_DORMANCY.md` §3 (Interval=0
  premise), `docs/rust-rewrite/handoff.md` ("Open: boss cooldown numbers").

## 4. Falsified ledger (verified safe — do not re-audit)

- **Framing:** parse buffer bounded by drain-per-read (~65 KB + 8 KB per connection);
  oversize declared length rejected at the header (`gz-net/lib.rs:448-455`); LZF output capped at
  64,987 bytes with sticky fatal on overflow (`main.rs:1712-1723`, `gz-lzf:348-389`); malformed
  headers sticky-fail with no resync (`lib.rs:453`, `clear()` does not lift); frame arithmetic
  underflow-free (`lib.rs:450,456,492-496`); parse path panic-free in prod (gz-net, gz-cipher).
- **Lifecycle:** `Registered` armed at `main.rs:565-566` — every exit path (EOF, framing, cipher,
  LZF, handshake timeout, duplicate-kick, panic-unwind) runs `remove` (`registry.rs:340-387`):
  near-logout fan, map removal, **login-claim release** (`:372-375`), friend notices; kick notice
  queued before the wake (`:287-296`); frames to removed sessions are dropped, never misdelivered
  (`:613-618`); unknown opcodes log-and-continue (`main.rs:1692`); decode errors only kill the
  sender's own connection; duplicate login per account refused/tested (`registry.rs:1118-1191`).
- **Movement/coords:** dest is 16-bit by construction (`order.rs:159-163`) → `cell_of` /
  `.abs()` overflow impossible from client orders (`map_geometry.rs:102-115`); `signum` stepping
  bounded (`movement.rs:303-315`); move deadlines re-armed per step (`main.rs:675-678`).
- **Combat:** dead attacker blocked on all three swing arms; server-side range gate reads server
  coordinates (client names neither attacker tile nor distance, `combat.rs:428-453`); alternating
  240/267 cannot bypass the 200 ms gate — shared `last_attack_ms`, fail-closed, combined ≤5/s
  (`main.rs:1353-1356`, `:1450-1453`); 267's 200 ms-only gate is **legacy parity**
  (`Server-Person.cpp:2745-2771` checks clock + alive + range and nothing more); auto-cadence
  interval is server-table-driven with a 100 ms floor (tested, `combat.rs:728-769`), not
  client-controlled; `roll_power`/`reduced_by_ac`/`exp_for_hit` div-guards + tests
  (`combat.rs:663-702`).
- **Monster world:** corpse invisible to `position()` (live table only, `monsters.rs:759-764`) and
  lookup→hit share one world lock — dead-target hits impossible; respawn gets a fresh unique
  (test); corpse/respawn deadlines tick globally while AI is skipped on hibernating maps
  (`monsters.rs:717-718`) — consistent and tested; every dormancy boundary tested
  (`monsters.rs:2659-2753`); geometric idle roll pinned (tail + Poisson, `:2557-2653`); boss
  cooldowns tested.
- **Items/IDs:** drop rolls index-free; empty-table / zero-count / min>max clamps + tests
  (`drops.rs:259-286`, `:494-540`); RNG `below(0)`/`up_to(-5)` no-panic (`combat.rs:315-326`,
  tested `:589-603`); grant/merge/split/drop/pickup arithmetic bounds-checked inside transactions
  (`item.rs:707-771`, `:880-885`, `:1039-1056`, `:1223-1237`); ground-cell race closed by
  `ON CONFLICT` + DB UNIQUE (`item.rs:1258-1301`, `sql/0010`); `add_exp` concurrency-safe
  (`UPDATE … RETURNING`, `character.rs:668-679`); one-login-per-account with claim release on
  teardown.

## 5. Phase B — root causes

| RC | Root cause | Findings |
|----|-----------|----------|
| RC-1 | Single-task coupling with no backpressure — the read loop performs writes inline, on unbounded channels, with no deadlines; the legacy's sized send buffers were not carried over | A1 |
| RC-2 | Liveness modeled as recomputed state instead of absolute time — no fixed-start deadline, no post-establish liveness contract | A2 |
| RC-3 | Type permissiveness over a shared id space — bare integers, two allocators, one `i16` wire band, no range invariant | A3, A8 |
| RC-4 | Unbounded resources assumed finite — sequences/pools with hard cliffs and no designed exhaustion path | A4, A8 |
| RC-5 | One `Result` channel for fatal and transient failures — protocol death and storage blips are indistinguishable | A5 |
| RC-6 | Client authority where the port claims server authority — monster tile invariant exists; player path and death-state never got it | A6, A7 |
| RC-7 | Doc premises load-bearing as code — a stale sentence removed a guard | A7, A10-docs |

## 6. Hardening decisions & designs (owner-approved)

Decision record: ✅ approved as proposed / ❌ rejected (with rationale) — all decisions final per Cat.

### H1 — Network backpressure & zombie prevention — ✅ APPROVED (A1, RC-1)

1. Wrap `write_all` in a **10-second `tokio::time::timeout` inside `send_raw`** (`main.rs:1759-1763`).
   One line converts every stalled-writer zombie into a bounded, self-cleaning session (timeout →
   `Err` → `Registered::drop` → registry entry + login claim released).
2. **Bounded `mpsc::channel(256)` per session** replacing `unbounded_channel` (`main.rs:576`,
   `registry.rs:160`). Broadcast paths use `try_send`: a lagging subscriber **drops that frame**
   (legacy slow-consumer semantics); a full queue on a *direct reply* is treated as fatal for that
   session. Encapsulate in `struct Outbound { tx: Sender<..> }` so queue depth cannot be bypassed.
3. **Accept `Semaphore` (512)** in `serve` (`main.rs:388-419`): acquire before `tokio::spawn`, drop
   the socket immediately when exhausted (connection-flood guard).

### H2 — Liveness & AFK policy — ❌ AutoLogOut REJECTED / ✅ rest APPROVED (A2, RC-2)

- **Rejected: naive 3-minute inactivity kick.** GoonZu gameplay is legitimately AFK for hours
  (fishing, farming, mining, personal market stalls); an idle kick would break core gameplay.
- **Pre-handshake: strict 10-second absolute deadline.** Store `handshake_deadline: Instant` once
  at accept; the loop arms `deadline.saturating_duration_since(now)` — dribble-proof by
  construction. Fix the mismatched comment at `main.rs:610-613` to match shipped behavior.
- **Established sessions: NO idle/inactivity kick, ever.** Liveness is enforced only by
  **TCP keepalive / socket-error detection** (a half-open peer is reaped when the stack errors the
  parked socket; wire `socket2`-style keepalive on accept).
- **Roadmap note:** offline stalls / avatar-progression-while-offline is future product work and
  must not be conflated with this liveness policy.

### H3 — ID range invariant & stability — ✅ APPROVED + verification completed (A3, A8, RC-3)

- **Approved partition:** players allocate within **`1..=8499`** (wrapping), NPCs allocate from
  **`8500+`**. Enforce with checked constructors at both allocators (`allocate_char_unique`,
  `spawn_one`); `register`/`spawn_one` refuse foreign-band ids (capture-mode pinned ids stay behind
  their existing exception path).
- **Verification result (legacy `NPC.cpp` / `Char-Server.h`, completed during this audit):** NPC
  uniques are **NOT placement-stable**. Every spawn mints via `cltCharServer::FindEmptyCharUnique`
  (`NPC.cpp:976`, `:1110`), whose body (`Char-Server.h:1006-1037`) implements a **three-band,
  recycling partition**:
  - PC band: `i in [1, MAX_PERSON_NUMBER)` — first slot with `CharUniqueInfo[i] == 0`
  - instance-NPC band: `[MAX_PERSON_NUMBER, +MAX_INSTANCE_NPC_NUMBER)`
  - field-NPC band: `[MAX_PERSON_NUMBER + MAX_INSTANCE_NPC_NUMBER, MAX_CHAR_NUMBER)`
  - exhaustion returns `0` (spawn refused), and freed ids are reused on death/logout.
- **Consequences:** (a) the legacy **never overlapped** players and NPCs — Cat's partition is
  legacy-exact parity; the port's regression was the wrap-mod-20000 allocator crossing the band.
  (b) **A8 does not collapse** — instead the fix is **recycle-on-death** (a free-list on corpse
  removal), which matches legacy semantics and makes the pool bound *simultaneous* monsters rather
  than lifetime kills; exhaustion mirrors the legacy (`0` → refuse spawn + log).

### H4 — Ground-item lifecycle & handle cliff — ✅ APPROVED: TTL sweeper + recycling (A4, RC-4)

- Implement a **Ground Item Expiration / Sweeper**: periodic task deletes unclaimed dropped items
  after **1 hour** on the ground; the deleted handle returns to a recycle pool, so the `UI16`
  handle space is bounded by concurrent ground items — the 65,535 sequence cliff disappears and the
  DB stays clean.
- Wire the sweeper to the existing world tick; DB migration adds the expiry index + free-pool
  table (or `MIN(free)` reuse — implementation detail at coding time).
- Optional belt-and-braces during transition: map any residual sequence-exhaustion error to a soft
  "ground full" refusal instead of `?`→disconnect (never a session-kill for a resource limit).

### H5 — Full inventory & DB resilience — ✅ APPROVED: legacy parity refusal + error decoupling (A5, RC-5)

- **Inventory full → do NOT grant the item** — standard refusal/no-op, **no spill-to-ground**
  (legacy parity). Keep the existing log line; ensure no other state (kill, exp) is affected.
- **Decouple DB storage hiccups from session teardown:** storage errors in reward/persistence paths
  log-and-continue instead of `?`-propagating into disconnect (`combat.rs:527` add_exp,
  grant paths). `?` remains only for protocol/decode failures. One `Result` becomes two policies.

### H6 — Player movement & anti-noclip — ✅ APPROVED (A6, RC-6)

- **Server-authoritative tile checks:** after `move_target`, validate the dest tile against the
  gz-data **`is_walkable` mask** (the same machinery monsters use) and **cap per-order travel
  distance** (speed-scaled Manhattan bound) to stop both wall-hacking and teleport-style orders.
- Reject (no persist, no 241 fan) on violation. Deeper legacy `char-order.cpp` chain remains
  untraced **[R]**; the port's server-authoritative stepping model justifies the gate on its own.

### H7 — Death-state guards — ✅ APPROVED (A7, RC-6/RC-7)

- **`life == 0` freezes movement immediately:** `step_movement` re-reads the registry each committed
  step and clears `movement` on death (the registry parameter is already present).
- **Reject user orders and gate warps while dead:** `handle_userorder` early-returns when the
  session's registry life is 0 (revive path unaffected); the leave-request death guard dropped on
  the false premise is restored.
- **Correct the false doc premise** at `movement.rs:1144` (and re-derive the leave-request comment).

### H8 — Registry scalability — ✅ APPROVED: deferred (A9)

- Sharding / per-session `Arc` cells is **deferred until the >200-concurrent-player milestone**.
  Current single-`Mutex` model is correct and adequate; revisit with measurements at the milestone.

### H9 — Minor consistency cleanup — ✅ APPROVED (A10)

- **`saturating_mul` on critical damage** (`combat.rs:146`) and saturating/checked boundaries on the
  damage chain (data-trust stays, panic shapes go).
- **Unify slot-validation behavior** (pick one policy across 307/341/303/7223 — soft-skip preferred
  for client variance).
- **Fix citation nits** (`combat.rs:408` → cite the target-alive line), the `handle_revive` Gate
  literal → `warp_gate_to`, and stale docs (`DESIGN_MAP_DORMANCY.md` §3, `handoff.md`).

## 7. Regression test suite (locks in the decisions)

| Test | Invariant | Covers |
|------|-----------|--------|
| T1 | Dribbling pre-handshake client is rejected at absolute accept+10 s (paused-time tokio) | H2 |
| T2 | Peer that never reads → session reaped within the 10 s write timeout; registry entry **and** login claim gone | H1 |
| T3 | Queue over cap → broadcast frame dropped; direct-reply-full → session disconnect; memory bounded | H1 |
| T4 | Accept at semaphore cap → socket refused before spawn | H1 |
| T5 | An idle-but-alive established session survives past 3 minutes (**no idle kick** — locks the H2 decision); a half-open peer is reaped on keepalive/socket error | H2 |
| T6 | Player allocator wraps inside `1..=8499`; NPC allocator never crosses below 8500; `register`/`spawn_one` refuse foreign-band ids | H3 |
| T7 | NPC unique is recycled on death (free-list); pool exhaustion = refuse spawn + log at band size, never a lifetime-cliff | H3/A8 |
| T8 | Ground row older than 1 h is swept; its handle returns to the pool and is reusable; the 65,536th *distinct concurrent* handle works — cliff gone | H4 |
| T9 | Kill with unreachable DB → session survives, monster/respawn state consistent, blip logged | H5 |
| T10 | Full inventory → no grant, no spill, kill/exp unaffected, log line present (parity lock) | H5 |
| T11 | ORDER_MOVE into a wall or beyond the distance cap → position unchanged, no 241 fan | H6 |
| T12 | Kill mid-walk → step freezes; ORDER_MOVE and gate warp while dead → no-op; leave-request death guard active | H7 |
| T13 | `apply_critical` / damage chain saturate at i32 edges (no debug panic, no release wrap-to-negative) | H9 |

Already locked by existing suites (no new work): every dormancy boundary, geometric idle roll, boss
cooldown, corpse linger + fresh-unique respawn, drop-roll invariants, item move/split/pickup
arithmetic and transactions, the shared 200 ms throttle gate, RNG degenerate ranges.

## 8. Implementation sequencing

Each batch is gated on the standard checks (`cargo test -p game`, `cargo fmt --check`,
`cargo clippy -p game --all-targets`, `scripts/check.ps1`), one commit per batch, behavior tests
with the change — exactly the refactoring discipline used for `739ca5d`/`0bea4e0`.

1. **Batch 1 — network liveness & backpressure (P0):** H1 + H2. Self-contained (`send_raw`,
   channel type, accept loop, keepalive, comment fix). Tests T1–T5.
2. **Batch 2 — id space & ground lifecycle (P1):** H3 (banded allocators + NPC free-list) + H4
   (TTL sweeper, DB migration, handle pool). Tests T6–T8.
3. **Batch 3 — failure policy & death gates (P2):** H5 (refusal parity + storage-blip decoupling) +
   H7 (movement/order/warp death guards, doc correction). Tests T9, T10, T12.
4. **Batch 4 — movement authority (P2):** H6 (tile mask + distance cap). Test T11.
5. **Anytime:** H9 nit batch (T13) + stale-doc fixes. **Deferred:** H8 until >200 concurrent.

## 9. Open verifications & runtime items

- **[R]** Wall-clock dynamics of A1 (how fast `write_all` parks depends on `SO_SNDBUF` and peer
  behavior; the structural claim is verified).
- **[R]** Client behavior under a duplicate `char_unique` (H3 mitigations remove the server-side
  ambiguity; client rendering after collision was never reproduced).
- **[R]** Deeper legacy chains not traced: `LetOrder` move validation (`char-order.cpp`), the legacy
  death path's effect on in-progress moves.
- **[R]** TCP keepalive reap timing for NAT-dropped peers (Batch 1 should measure once wired).
- Prior parked items still valid: `RESPONSE_BOSSAPPEAR` countdown broadcast (legacy parity gap,
  out of scope here); full `.cae` boss-extras decode (needs a runtime dump of the packed consumer).

## 10. Evidence index (anchor citations)

- Network/session: `main.rs:388-419, 565-566, 576, 610-640, 649-660, 809-863, 1692, 1753-1765`;
  `registry.rs:160, 235-239, 273-308, 340-387, 436-446, 613-646, 790-851, 956-973`;
  `session.rs:49-53, 336-342`; `gz-net/lib.rs:169-180, 432-455, 478-522`.
- Ids: `main.rs:174-194`; `monsters.rs:513, 581-607`; legacy `Char-Server.h:1006-1037`,
  `ITZSessionFactory.h:15, 36-40, 64`, `NPC.cpp:976, 1110`.
- Items: `sql/0005_world_item_handle.sql:35-56`, `sql/0010_world_items_cell_unique.sql:23-24`;
  `gz-db/src/item.rs:534-607, 707-771, 1039-1056, 1223-1301`; `items.rs:191-199, 310-320, 552-570`;
  `combat.rs:527, 547-575`; `drops.rs:144-167, 259-286`.
- Combat/movement: `combat.rs:106-165, 282-286, 315-331, 395-577`; `main.rs:1330-1487, 703-794`;
  `movement.rs:303-315, 712-841, 1137-1158`; `order.rs:159-163`; `map_geometry.rs:102-115`;
  legacy `Server-Person.cpp:2698-2771`, `Server-System.cpp:1195/1239`, `Server-Attack.cpp:968-991, 4138`.
- Monsters: `monsters.rs:270-321, 689-725, 759-764, 781+, 820-933, 942-1014, 1018-1039, 1100-1201, 1284`;
  tests `:1936-2017, 2076-2167, 2557-2753`; legacy `NPC.cpp:223, 862-888, 928, 972, 1077`,
  `CharManager.cpp:257-277, 362-363`.
