# Future Concept: Client-Side Combat Simulation with Server Sanity/Validation (Zero-overhead Server Model for 20k CCU)

Status: vision note — Cat's direction, recorded so it is not lost. Not scheduled
work. Today's server stays fully authoritative (`combat.rs`, `monsters.rs`);
nothing in this repo depends on this note.

## The idea

Stop simulating every swing on the server. The client rolls hits, damage, crits
and effects for its own combat; the server validates instead of computes:

- **Rate** — a client cannot swing faster than the per-hit throttle allows
  (`HIT_THROTTLE_MS` today).
- **Range** — the claimed attacker/target positions sit within the server's
  authoritative ones.
- **Plausibility** — reported damage lies inside the band the attacker's stats
  and the target's AC can produce.
- **Outcomes that matter stay server-side** — death, loot, exp, respawn and
  inventory changes are granted by the server after validation, never taken on
  the client's word.

## Why this reaches 20k CCU

Server cost today scales with *fights happening*: every swing crosses the wire,
takes locks, rolls RNG and fans frames. Under client-side simulation the
marginal cost per swing becomes a few cheap checks — or a sampled audit instead
of a per-event one — so cost scales with *players connected*, and a
connected-but-idle player costs almost nothing. That inversion is the
"zero-overhead" in the title.

## What it is not

- Not anti-cheat by itself: validation bounds what a modified client can claim;
  sampling plus per-account enforcement carries the other half.
- Not a reason to weaken the current server: the client rebuild
  (`SPEC_CLIENT_REBUILD_PATCH.md`) and the id architecture
  (`DESIGN_THREE_TIER_ID_ARCHITECTURE.md`) land first, and the authoritative
  path must keep working as the fallback for anything that fails validation.

## Where it would attach later

`player_swing` and `MonsterWorld::hit_by_player` already return structured
outcomes (`Swing`, `MonsterHit`) instead of burying side effects in the loop —
that seam is where a validating path could consume a client's reported result
instead of computing one.
