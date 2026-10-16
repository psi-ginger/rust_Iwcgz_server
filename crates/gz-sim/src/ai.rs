//! Monster AI decisions: what to attack, and where to stand.
//!
//! Two pure rules. The tick that will own monsters
//! (`services/game/src/monsters.rs`, design doc §10.2) passes facts in and gets
//! a decision out, so both rules are testable without a world.
//!
//! # Target acquisition ([`acquires_target`])
//!
//! Legacy monsters call `SearchEnemy(range)` with their kind's `siAggressive`
//! every wander tick (`common\Char\CharWait\char-wait.cpp:22`, called at
//! `:363`): an aggressive kind acquires whatever walks into that radius, a
//! passive kind never proactively acquires anything. The port keeps that gate
//! and adds a second, independent one: a player idle for
//! [`AFK_FORCE_AGGRO_SECS`] becomes an immediate hostile target for anything
//! *nearby* — including passive kinds. That punishment is a port rule, not a
//! legacy one: the legacy has no idle tracking at all.
//!
//! Both range questions stay with the caller. `in_aggressive_range` is the
//! legacy `siAggressive` test; `in_detection_range` is the "nearby" the AFK
//! rule needs (the spawn group's leash square is the natural choice —
//! `gz_data::spawn::SpawnGroup::contains`). No range constant is invented
//! here — the numbers arrive with the tick.
//!
//! # No flee ([`next_state`])
//!
//! A monster has three movement states and no fourth: [`MonsterState::Idle`]
//! (wander the leash), [`MonsterState::Engage`] (fight the target),
//! [`MonsterState::ReturnHome`] (walk back to the group centre). Hit points are
//! not an input to [`next_state`] — there is no branch that can see them, so a
//! monster at 1 life with a target in range stays engaged. "Fight to the death
//! inside the leash" is structural, not a rule someone can forget to apply.
//!
//! That is also a legacy truth, discovered rather than assumed: the legacy
//! flee routine `cltCharCommon::Escape()` (`common\Char\CharCommon\
//! char-attack.cpp:1565-1599`, which sets `CHARWORKSTEP_RETREAT` and steps the
//! character three tiles *away* from its attacker, `Char-Common.h:170`) has
//! **zero call sites** in the whole server tree — the shipped game never fled
//! either. What the legacy does have is `dwAttackGiveUpDelay`
//! (`char-attack.cpp:1550`, checked at `:1807`): five seconds without a
//! *reachable* path gives up the *target*. That is target bookkeeping, not
//! flight — the caller drops `has_target`, and the monster still never runs.
//!
//! The leash outranks the fight: outside the group square
//! (`cltNPCGroup::IsInNPCGroup`, `common\NPCManager\NPC.cpp:109-120`) the
//! monster goes home even mid-chase, which is the legacy's return-to-center
//! walk (`char-wait.cpp:219-236`) with the same dominance.

/// A player idle this many seconds becomes immediate monster bait, whatever
/// the monster's own passivity.
///
/// A port rule (the legacy has no idle detection); 30 minutes is long enough
/// that any deliberate play session resets it long before the gate opens.
pub const AFK_FORCE_AGGRO_SECS: u64 = 1800;

/// Whether an idle of `idle_secs` opens the force-aggro gate.
#[must_use]
pub fn force_aggro(idle_secs: u64) -> bool {
    idle_secs >= AFK_FORCE_AGGRO_SECS
}

/// Whether this monster acquires the player as a target on this tick.
///
/// * `in_aggressive_range` — inside the kind's `siAggressive` radius: the
///   legacy gate, true only where an aggressive kind would search
///   (`char-wait.cpp:363`).
/// * `in_detection_range` — inside the range the AFK rule scans ("nearby").
/// * `player_idle_secs` — how long the player has done nothing
///   ([`force_aggro`]).
#[must_use]
pub fn acquires_target(
    in_aggressive_range: bool,
    in_detection_range: bool,
    player_idle_secs: u64,
) -> bool {
    in_aggressive_range || (in_detection_range && force_aggro(player_idle_secs))
}

/// A monster's movement decision for one tick.
///
/// Deliberately no fleeing variant — see the module docs. Death is not a
/// state either: the loop that calls [`next_state`] stops when life hits 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonsterState {
    /// Wander the leash; nothing to fight.
    Idle,
    /// Fight the current target, life permitting.
    Engage,
    /// Walk back to the group centre — taken whenever the leash is breached,
    /// combat or not.
    ReturnHome,
}

/// The entire transition table: the leash outranks the target, the target
/// outranks idling, and hit points are never consulted.
#[must_use]
pub fn next_state(has_target: bool, in_leash: bool) -> MonsterState {
    if !in_leash {
        MonsterState::ReturnHome
    } else if has_target {
        MonsterState::Engage
    } else {
        MonsterState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Damage never causes flight. All four input combinations of
    /// [`next_state`] are pinned below and none of them names life, so "low
    /// HP" cannot select a state — and `MonsterState` has no fleeing variant
    /// to select. (Legacy `Escape()` had no call sites either; module docs.)
    #[test]
    fn damage_never_causes_flight() {
        // Target inside the leash: fight on. At 1 HP this is the identical
        // call — there is no HP parameter to change the answer with.
        assert_eq!(next_state(true, true), MonsterState::Engage);
        // Leash breached mid-fight: home, toward the group centre — never
        // away from the attacker.
        assert_eq!(next_state(true, false), MonsterState::ReturnHome);
        // Nothing to fight: wander inside the leash, go home outside it.
        assert_eq!(next_state(false, true), MonsterState::Idle);
        assert_eq!(next_state(false, false), MonsterState::ReturnHome);
    }

    /// Under the threshold a passive monster keeps ignoring the player, no
    /// matter how long they idle *below* 30 minutes — and nobody is
    /// force-aggroed from outside detection range at all.
    #[test]
    fn under_the_threshold_a_passive_monster_keeps_ignoring_the_player() {
        assert!(!force_aggro(0));
        assert!(!force_aggro(AFK_FORCE_AGGRO_SECS - 1));
        assert!(!acquires_target(false, true, AFK_FORCE_AGGRO_SECS - 1));
        // Out of detection range: idle as long as they like.
        assert!(!acquires_target(false, false, u64::MAX));
    }

    /// At the threshold everything nearby comes for the player — the passive
    /// monster included. Posture (standing/sitting/vending) never reaches this
    /// function: the clock that feeds it is the registry's, tested there.
    #[test]
    fn at_thirty_minutes_everything_nearby_comes_for_the_player() {
        assert!(force_aggro(AFK_FORCE_AGGRO_SECS));
        assert!(acquires_target(false, true, AFK_FORCE_AGGRO_SECS));
        // An aggressive kind's legacy behaviour is unchanged: it attacks
        // regardless of the clock.
        assert!(acquires_target(true, false, 0));
    }
}
