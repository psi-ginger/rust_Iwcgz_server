//! Combat resolution — Phase 2 of `docs\DESIGN_MONSTER_COMBAT_AND_AUTOHUNT.md` §10.3.
//!
//! This module holds the numbers: what an attacker rolls, what a defender
//! subtracts, and what a hit is worth in experience. The *wiring* — the 267
//! dispatch, the 268 broadcast, the death sequence, the monster's own swings —
//! lives in [`crate::main`] and [`crate::monsters`], because those need the
//! registry and the world and this deliberately does not.
//!
//! # The damage path, in legacy order
//!
//! `DoMsg_GAMEMSG_REQUEST_HIT` (`Server-Person.cpp:2745`) → `HitOperation`
//! (`Server-Attack.cpp:412`) → `GetDamage` (`:907`) → `DecreaseLife`. Only the
//! physical branch of `GetDamage` is ported; the defence, elemental, set-item
//! and enchant layers (`:993-2181`) are the design doc's explicitly
//! "do not hand-translate" zone, so they are absent rather than approximated.
//!
//! The attacker's own power comes from `CalcApower`
//! (`Parametabox.cpp:408-695`): the weapon's minimum plus a STR term plus a
//! level term, with a roll inside the min..max spread. Only the terms this port
//! can compute without an equipment model are here — see [`PlayerAttacks`].

use std::collections::HashMap;
use std::path::Path;

use gz_data::kindinfo::{CharAttackRow, canonical_char_attack, load_kind_unique};

/// The tier ladder `CalcLevelDamage` (`Parametabox.cpp:56-89`) and `CalcLevelAC`
/// (`:7325-7338`) share. Each arm takes what is above its threshold, divides by
/// that tier's step, and hands the remainder down — so the ladder is applied
/// top-down, exactly as the legacy's sequence of `if(level > N)` statements is.
///
/// AC halves the result afterwards ([`level_ac`]); damage does not.
fn level_tier_sum(level: i32) -> i32 {
    let mut level = level.max(0);
    let mut sum = 0;
    for (threshold, step) in [(90, 1), (70, 2), (60, 3), (50, 4), (40, 5)] {
        if level > threshold {
            sum += (level - threshold) / step;
            level = threshold;
        }
    }
    sum + level / 10
}

/// `CalcLevelDamage` (`Parametabox.cpp:56-89`) — the attack a level alone
/// grants, with no ability or equipment under it.
#[must_use]
pub(crate) fn level_damage(level: i32) -> i32 {
    level_tier_sum(level)
}

/// `CalcLevelAC` (`Parametabox.cpp:7325-7338`) — the defence a level alone
/// grants. This is the part of a player's `GetTotalAC()` that survives with no
/// equipment modelled, and it is what a monster's swing is reduced by.
#[must_use]
pub(crate) fn level_ac(level: i32) -> i32 {
    level_tier_sum(level) / 2
}

/// `siStrPHysicalAttackDmg` (`Parametabox.cpp:490-505`) — the STR term, which
/// is the same number added to an attacker's minimum *and* maximum.
///
/// The two arms differ only in the divisor and a trailing `* 6`, which is why a
/// bow user's STR buys much less than a sword user's at the same interval.
///
/// `interval` is the weapon's `dwAttackInterval`; the kind table carries it
/// ([`CharAttackRow::attack_interval`]).
fn str_damage(attack_type: &str, str_: i32, interval: i32, level: i32) -> i32 {
    let scaled = str_ * ((interval * 10) + (level * 50));
    if attack_type.starts_with("ATTACKTYPE_SWORD")
        || attack_type.starts_with("ATTACKTYPE_SPEAR")
        || attack_type.starts_with("ATTACKTYPE_AXE")
    {
        scaled / 20_000
    } else if attack_type.starts_with("ATTACKTYPE_BOW") || attack_type.starts_with("ATTACKTYPE_GUN")
    {
        // The division truncates before the `* 6`, as in the legacy.
        scaled / 150_000 * 6
    } else {
        // `ATTACKTYPE_STAFF` and the rest of the switch are skills-driven
        // (`Parametabox.cpp:505-`), which this port has no model for.
        0
    }
}

/// One hit's variance — `CalcApower`'s `siTotalPHysicalMinAttackDmg +
/// frame % (diff + 1)` (`Parametabox.cpp:408-`), where `diff` is the attacker's
/// max minus its min. `roll` is the `frame` value the caller supplies, so this
/// stays pure.
#[must_use]
pub(crate) fn roll_power(min: i32, max: i32, roll: i32) -> i32 {
    let spread = (max - min).max(0);
    min + roll % (spread + 1)
}

/// `GetDamage`'s physical branch (`Server-Attack.cpp:968-991`) — what `power`
/// becomes after the **defender's** AC.
///
/// `ac` is `GetTotalAC()` of the thing being hit, which is why a monster's own
/// AC column reduces a player's swing and a player's [`level_ac`] reduces a
/// monster's.
#[must_use]
pub(crate) fn reduced_by_ac(power: i32, ac: i32) -> i32 {
    let denominator = (power * 2) + (ac * 8 / 10);
    // The legacy guards on `damage + ac`, not on the denominator — keep that
    // guard verbatim, and add one for the denominator the legacy would divide
    // by zero on (it is reachable for `power == 0` with a tiny AC, a shape the
    // legacy's own callers always avoided).
    let decpower = if power + ac == 0 || denominator == 0 {
        0
    } else {
        // The C++ divides `SI32`s, so the quotient truncates; the `* 1.3` then
        // runs in `double` and truncates again on assignment.
        (((power * ac) / denominator) as f64 * 1.3) as i32
    };
    let valid = power - decpower;
    if valid <= 0 { 1 } else { valid }
}

/// One hit's critical. The legacy's gate is the attacker's `siCriticalRate` —
/// a percent (`kindinfo-Manager.cpp:705`) — and a critical doubles the applied
/// power.
///
/// The *double* damage roll that follows a critical in the shipped client
/// (`SpecialAttackSwitch == 2`, `Client-Person.cpp:1258`) is **not** ported: it
/// sits inside the `GetDamage` tail the design doc (`§11`) marks as a
/// mechanical-port-only item. So a critical doubles, and nothing else does.
#[must_use]
pub(crate) fn apply_critical(damage: i32, critical_rate: i32, roll: i32) -> i32 {
    if critical_rate > 0 && roll % 100 < critical_rate {
        damage * 2
    } else {
        damage
    }
}

/// One hit's experience — `GetExp`'s core (`Server-Attack.cpp:4138`):
/// `exp = (exp * damage) / enemymaxdamage / 3`, where `exp` is the enemy's
/// `GetMyExp()` (`Char-Common.cpp:1362`), i.e. its kind's `Exp` column plus
/// `level - 1`.
///
/// Both divisions truncate, in that order, so a chip hit on a huge monster
/// yields 0 — which is the legacy's behaviour, not a bug.
#[must_use]
pub(crate) fn exp_for_hit(kind_exp: i64, enemy_level: i32, damage: i32, enemy_max_hp: i32) -> i64 {
    let my_exp = kind_exp + i64::from(enemy_level - 1);
    if enemy_max_hp <= 0 || damage <= 0 {
        return 0;
    }
    (my_exp * i64::from(damage)) / i64::from(enemy_max_hp) / 3
}

/// A player's default-weapon attack rows, resolved once at boot by joining the
/// live `KindInfo_Unique.txt` (`KindInfo_Unique` id → `KIND_*` code) with the
/// committed [`canonical_char_attack`] table (code → row).
///
/// # Why this exists
///
/// `CalcApower`'s `siTotalPHysicalMinAttackDmg` (`Parametabox.cpp:695`) starts
/// from `defaultweaponmindamage`, and that is the character's kind's own
/// `CharAttackInfo` row — the same column monsters read. Resolving it here is
/// what makes a player's damage come from committed data rather than a guess.
///
/// # What it leaves out
///
/// Equipment. A geared character hits like an unarmed one, because there is no
/// item model to add `siEquipPHysical*` from. That is the same shortcut the
/// design doc (`§10.3`) takes for Phase 2.
#[derive(Debug, Default)]
pub(crate) struct PlayerAttacks {
    by_kind: HashMap<u16, &'static CharAttackRow>,
}

impl PlayerAttacks {
    /// Joins the data root's unique-id table against the committed attack
    /// master. A kind with no attack row is simply absent, and
    /// [`PlayerAttacks::raw_power`] degrades to the level term for it — the
    /// legacy's `clKI == NULL` guard (`Server-Attack.cpp:586`) refuses the hit
    /// outright, but a PC that cannot attack at all is worse than a weak one.
    ///
    /// # Errors
    ///
    /// When the data root cannot be read.
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
        let attack_by_code: HashMap<&str, &CharAttackRow> = canonical_char_attack()
            .iter()
            .map(|row| (row.code.as_str(), row))
            .collect();
        let mut by_kind = HashMap::new();
        for unique in load_kind_unique(root).map_err(|e| format!("KindInfo_Unique: {e}"))? {
            if let Some(&row) = attack_by_code.get(unique.code.as_str()) {
                by_kind.entry(unique.id).or_insert(row);
            }
        }
        Ok(Self { by_kind })
    }

    /// The attacker's raw damage before the defender's AC and before the
    /// [`roll_power`] spread is picked: the weapon's minimum and maximum, each
    /// plus the STR and level terms [`str_damage`] and [`level_damage`] add.
    ///
    /// `class_kind` is the `KindInfo_Unique` id the character was created with
    /// (`SessionInfo::class_kind`).
    #[must_use]
    pub(crate) fn raw_power(&self, class_kind: u16, str_: i32, level: i32) -> (i32, i32) {
        let term = level_damage(level);
        match self.by_kind.get(&class_kind) {
            Some(row) => (
                row.min_dmg + str_damage(&row.attack_type, str_, row.attack_interval, level) + term,
                row.max_dmg + str_damage(&row.attack_type, str_, row.attack_interval, level) + term,
            ),
            None => (term, term),
        }
    }

    /// The critical rate for `class_kind`, in percent, for [`apply_critical`].
    /// `0` when the kind has no attack row, so an unknown class cannot crit.
    #[must_use]
    pub(crate) fn critical_rate(&self, class_kind: u16) -> i32 {
        self.by_kind
            .get(&class_kind)
            .map_or(0, |row| row.critical_rate)
    }

    /// The attack range for `class_kind`, in tiles — the legacy's
    /// `IsInRange` operand (`Server-Person.cpp:2765`), which reads the weapon's
    /// `range` column. A kind with no row gets 1, so it can still land a swing
    /// on a tile it shares with its target.
    #[must_use]
    pub(crate) fn range(&self, class_kind: u16) -> i32 {
        self.by_kind
            .get(&class_kind)
            .map_or(1, |row| row.range.max(1))
    }

    /// The swing cadence for `class_kind`, in milliseconds — the column the
    /// legacy's attack animation, and therefore its `HitOperation`, runs on
    /// (`dwAttackInterval`, `:703`).
    ///
    /// This is what makes one click start an ongoing fight: the client queues a
    /// single `ORDER_ATTACK` (`cltOrder::MakeOrder` pushes one entry and the
    /// mouse handler then clears `siSelectedOrder`, `order.cpp:182`,
    /// `MouseOperation.cpp:771`) and never re-sends it, so the server has to
    /// keep swinging on its own.
    ///
    /// A kind with no row gets 1000 ms, the most common shipped value. A row
    /// reading 0 would re-arm the read loop with a zero sleep and spin it, so
    /// the result is floored at 100 ms.
    #[must_use]
    pub(crate) fn interval(&self, class_kind: u16) -> u64 {
        self.by_kind.get(&class_kind).map_or(1_000, |row| {
            u64::try_from(row.attack_interval)
                .unwrap_or(1_000)
                .clamp(100, 60_000)
        })
    }
}

/// A deterministic PRNG for the combat rolls, so a test can replay a fight
/// exactly and the game crate does not need a `rand` dependency for four
/// modulo operations. xorshift64\* — not cryptographic, and it does not need to
/// be: every number it makes here is a damage, a crit or a drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rng(u64);

impl Rng {
    /// Seeds the generator. A zero seed would be stuck at zero, so it is nudged
    /// to 1 — the standard xorshift fix.
    #[must_use]
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform value in `0..n`, like the legacy's `rand() % n`. `n == 0`
    /// returns 0 rather than dividing by zero, because a caller passing a flat
    /// range should not crash the server.
    pub(crate) fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(n)) as u32
    }

    /// A uniform value in `0..=lo`, the legacy's `rand() % (lo + 1)` where the
    /// caller wants an inclusive top.
    pub(crate) fn up_to(&mut self, lo: i32) -> i32 {
        self.below((lo.max(0) + 1) as u32) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rng_is_reproducible_and_stays_in_range() {
        let mut a = Rng::new(20_251_016);
        let mut b = Rng::new(20_251_016);
        for _ in 0..64 {
            let (x, y) = (a.below(100), b.below(100));
            assert_eq!(x, y, "the same seed and order must give the same value");
            assert!(x < 100);
        }
        // A different seed gives a different stream.
        let mut c = Rng::new(1);
        assert_ne!(Rng::new(2).below(1_000_000), c.below(1_000_000));
        // Degenerate ranges cannot panic.
        assert_eq!(Rng::new(1).below(0), 0);
        assert_eq!(Rng::new(1).up_to(-5), 0);
    }

    #[test]
    fn the_level_ladder_consumes_each_tier_top_down() {
        // Below 40 there is only the base `level / 10`.
        assert_eq!(level_damage(9), 0);
        assert_eq!(level_damage(10), 1);
        assert_eq!(level_damage(39), 3);
        // 40-50 adds a fifth of the excess above 40.
        assert_eq!(level_damage(40), 4);
        assert_eq!(level_damage(45), 5);
        // 50-60 adds a quarter, and so on up the ladder. Every tier the level
        // passes contributes, so the sum climbs fast.
        assert_eq!(level_damage(50), 6);
        assert_eq!(level_damage(54), 7);
        assert_eq!(level_damage(60), 8);
        assert_eq!(level_damage(63), 9);
        assert_eq!(level_damage(70), 11);
        assert_eq!(level_damage(74), 13);
        assert_eq!(level_damage(90), 21);
        assert_eq!(level_damage(95), 26);
        assert_eq!(level_damage(100), 31);
    }

    /// AC is the same ladder halved — pinned so a "tidy" edit to one formula
    /// cannot silently leave the other behind.
    #[test]
    fn ac_is_the_damage_ladder_divided_by_two() {
        for level in [1, 10, 39, 40, 50, 60, 70, 90, 95, 100] {
            assert_eq!(level_ac(level), level_damage(level) / 2, "level {level}");
        }
    }

    #[test]
    fn the_str_term_follows_the_melee_and_ranged_branches() {
        // Melee: `str * ((interval*10) + (level*50)) / 20000`.
        assert_eq!(str_damage("ATTACKTYPE_SWORD", 30, 1000, 10), 15);
        assert_eq!(str_damage("ATTACKTYPE_AXE", 30, 1000, 10), 15);
        // Ranged: the same numerator over 150000, then * 6 — the truncation
        // happens before the multiply, which is what makes a bow's STR so much
        // weaker.
        assert_eq!(str_damage("ATTACKTYPE_BOW", 30, 1000, 10), 12);
        // A type outside the two arms contributes nothing.
        assert_eq!(str_damage("ATTACKTYPE_STAFF", 30, 1000, 10), 0);
        assert_eq!(str_damage("ATTACKTYPE_UNKNOWN", 30, 1000, 10), 0);
    }

    #[test]
    fn the_power_spread_walks_the_whole_inclusive_range() {
        assert_eq!(roll_power(10, 19, 0), 10);
        assert_eq!(roll_power(10, 19, 9), 19);
        // Wraps rather than saturating, as the legacy's `%` does.
        assert_eq!(roll_power(10, 19, 10), 10);
        // A flat attacker always hits its one number.
        assert_eq!(roll_power(7, 7, 4), 7);
    }

    /// `decpower = ((dmg*ac)/((dmg*2)+ac*8/10)) * 1.3`, truncated at both
    /// divisions, and never able to take a hit below 1.
    #[test]
    fn ac_reduces_damage_by_the_legacy_formula() {
        // dmg 100, ac 50: 100*50 / (200 + 40) = 5000/240 = 20 (truncated),
        // * 1.3 = 26 (truncated) -> 100 - 26 = 74.
        assert_eq!(reduced_by_ac(100, 50), 74);
        // No defence: no reduction.
        assert_eq!(reduced_by_ac(37, 0), 37);
        // A huge AC still cannot take a hit to nothing.
        assert_eq!(reduced_by_ac(5, 10_000), 1);
        // The legacy's `(dmg + ac) == 0` guard.
        assert_eq!(reduced_by_ac(0, 0), 1, "floors at 1");
        // A tiny AC with zero damage would divide by zero; the added guard
        // keeps that from panicking.
        assert_eq!(reduced_by_ac(0, 1), 1);
    }

    #[test]
    fn a_critical_doubles_and_rate_zero_never_crits() {
        assert_eq!(apply_critical(40, 30, 0), 80);
        assert_eq!(apply_critical(40, 30, 29), 80);
        assert_eq!(apply_critical(40, 30, 30), 40);
        assert_eq!(apply_critical(40, 30, 99), 40);
        assert_eq!(apply_critical(40, 0, 0), 40);
    }

    #[test]
    fn experience_is_the_legacy_share_of_a_kill() {
        // `GetMyExp()` = kind exp + (level - 1); the share is
        // `(exp * damage) / maxhp / 3`.
        // kind exp 90, level 1 -> 90; damage 60 of 60 max -> 90*60/60/3 = 30.
        assert_eq!(exp_for_hit(90, 1, 60, 60), 30);
        // A chip hit on the same monster: 90 * 3 / 60 / 3 = 1 (truncated).
        assert_eq!(exp_for_hit(90, 1, 3, 60), 1);
        // Level 10 raises the pool by 9.
        assert_eq!(exp_for_hit(90, 10, 60, 60), 33);
        // A hit that kills a much larger monster still shares by the same rule.
        assert_eq!(exp_for_hit(300, 20, 10, 1000), 1);
        // Degenerate inputs cannot divide by zero.
        assert_eq!(exp_for_hit(90, 1, 0, 60), 0);
        assert_eq!(exp_for_hit(90, 1, 60, 0), 0);
    }

    /// The join resolves the shipped data end to end: a real PC class kind must
    /// come back with its own committed weapon row, not the level-only
    /// fallback.
    #[test]
    fn the_join_resolves_a_shipped_player_class() {
        let root = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../New Era Live"
        ));
        let attacks = PlayerAttacks::load(root).expect("the data root is readable");
        let (min, max) = attacks.raw_power(1, 20, 10);
        assert!(
            max > min || min > level_damage(10),
            "kind 1 must resolve to a real weapon row, got ({min}, {max})"
        );
        // The spread has to contain the minimum.
        assert!(roll_power(min, max, 0) >= min);
    }

    /// The cadence the read loop arms a next swing with. A kind with no row
    /// must fall back rather than arm a zero-length sleep, and a shipped class
    /// must land inside the clamp — both are what keep the auto-attack loop
    /// from spinning.
    #[test]
    fn the_swing_interval_is_always_arming_distance() {
        let root = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../New Era Live"
        ));
        let attacks = PlayerAttacks::load(root).expect("the data root is readable");
        assert_eq!(
            attacks.interval(u16::MAX),
            1_000,
            "an unjoined kind defaults"
        );
        assert!(attacks.interval(1) >= 100, "never re-arms immediately");
        assert!(
            attacks.interval(1) <= 60_000,
            "never parks for a quarter hour"
        );
    }
}
