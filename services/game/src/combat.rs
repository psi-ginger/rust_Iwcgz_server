//! Combat resolution — Phase 2 of `docs\DESIGN_MONSTER_COMBAT_AND_AUTOHUNT.md` §10.3.
//!
//! This module holds the numbers: what an attacker rolls, what a defender
//! subtracts, and what a hit is worth in experience — and, since the combat
//! consolidation, the player-side flow that spends them ([`start_approach`],
//! [`player_swing`]) and the throttle the dispatcher keeps on the 267 arm.
//! The *wiring* — the 267/240 dispatch arms and the monster's own swings —
//! still lives in [`crate::main`] and [`crate::monsters`].
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
use std::sync::Mutex;
use std::time::Instant;

use gz_data::kindinfo::{CharAttackRow, battle_power, canonical_char_attack, load_kind_unique};
use gz_data::levelup::LevelUpTable;
use gz_db::{
    CharacterFull, CharacterRepository, ItemRepository, PgCharacterRepository, PgItemRepository,
};
use gz_proto::combat::{GetExpResponse, HIT_TARGET_MONSTER, HitResponse, LevelUpResponse};
use gz_proto::game::encode_person_ip;
use gz_proto::item::{Item, encode_person_item_gained};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::OrderData;

use crate::monsters;
use crate::movement::{Movement, next_move_deadline};
use crate::registry::{Registry, SessionKey};
use crate::stats::{basic_info_for, derived_max_life, derived_max_mana};
use crate::world::LiveFrames;
use crate::{AnyError, logln, logln_err, now_tick};

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

/// The STR term added to an attacker's minimum *and* maximum — Cat's §4.3.5
/// reformation (approved 2026-10-20), replacing the legacy
/// `siStrPHysicalAttackDmg` branch soup (`Parametabox.cpp:490-505`).
///
/// `StrDamage = ⌊STR × (0.20 + 0.01×(Level−1))⌋ + 5×⌊Level/10⌋`, in integer
/// math: the ratio is `(19 + level)/100`. One formula for every physical
/// weapon type — the old sword/spear/axe `/20000`, bow `/150000*6` and
/// staff-zero arms are gone by directive (this also retires the recorded
/// port-vs-legacy divergence where staff and bare-handed swung for zero).
///
/// Spec pins (reproduced exactly by this integer form): L1, 10 STR = +2;
/// L10, 35 STR = +15; L30, 120 STR = +73; L50, 220 STR = +176;
/// L100, 550 STR = +704.
fn str_damage(str_: i32, level: i32) -> i32 {
    str_ * (19 + level) / 100 + 5 * (level / 10)
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

/// One hit's critical, by Cat's §4.3.3 reformation (approved 2026-10-20):
/// **150% base** damage, scaled up by LUK at +0.05%/point to a +50% hard cap
/// (1000 LUK = ×2.25). The legacy flat doubling (`damage * 2`, roll gate
/// `rand()%1000` at `Server-Attack.cpp:801`) is replaced wholesale — the base
/// drops 200%→150% per the approved spec, with LUK buying it back and beyond.
///
/// `bonus` is basis-10000: `min(LUK * 5, 5000)` → 0..=5000, so the multiplier
/// is `3 * (10_000 + bonus) / 20_000` = 1.5..=2.25. Monster swings pass
/// `0` — the kind table's `siCriticalRate` gates the roll, and no LUK term
/// exists on the monster side of §4.3.3.
///
/// The *double* damage roll that follows a critical in the shipped client
/// (`SpecialAttackSwitch == 2`, `Client-Person.cpp:1258`) is **not** ported: it
/// sits inside the `GetDamage` tail the design doc (`§11`) marks as a
/// mechanical-port-only item.
#[must_use]
pub(crate) fn apply_critical(damage: i32, critical_rate: i32, luk: i32, roll: i32) -> i32 {
    if critical_rate > 0 && roll % 100 < critical_rate {
        let bonus = (luk * 5).clamp(0, 5000);
        (i64::from(damage) * 3 * (10_000 + i64::from(bonus)) / 20_000) as i32
    } else {
        damage
    }
}

/// The §4.3.2 hit term, basis-10000: **7000 base + 18 per AGI point**. The
/// 7000 is the legacy's own base (`7000 + rank*6` in `GetHitRate`,
/// `STAT_MECHANICS.md` §2) and 18 is that formula's `rank*6` per stat point
/// (`(AGI*3)*6`); the legacy's `(mine − theirs) * 50` level-difference term is
/// dropped by §4.7, which abolishes level-gap miss. AGI is the `dex` column
/// (the legacy dodge stat's home); `hnd` is crafting-only from now on
/// (§4.3.1).
pub(crate) const BASE_HIT_RATE_BP: i32 = 7000;
pub(crate) const AGI_HIT_RATE_BP: i32 = 18;

/// §4.3.2 dodge, basis-10000 — the same 18-per-point ladder the attacker's
/// hit climbs, so one AGI point is worth +hit and +dodge at once.
pub(crate) const AGI_DODGE_RATE_BP: i32 = 18;

/// Cat's Ghost Dodge fix (2026-10-20): `Actual Hit Rate = Attacker Hit Rate −
/// Defender Dodge Rate`, clamped to the 10%..=95% band. The legacy added the
/// defender's dodge *on top of* the attacker's hit as a failure band of
/// `dodge/2` (`Server-Attack.cpp:608-634`, the subtraction commented out at
/// `:596-599`) — dodge never actually subtracted from anyone's hit, hence
/// "Ghost Dodge". This is the subtraction the commented-out line intended,
/// for both `player_swing` and a monster's counter-swing.
///
/// Returns basis-10000 (1000..=9500); roll with [`Rng::below`]`(10_000)`.
#[must_use]
pub(crate) fn hit_chance(attacker_agi: i32, defender_agi: i32) -> i32 {
    let hit = BASE_HIT_RATE_BP + attacker_agi * AGI_HIT_RATE_BP;
    let dodge = defender_agi * AGI_DODGE_RATE_BP;
    (hit - dodge).clamp(1000, 9500)
}

/// What one swing's roll resolved to, so the 268 can carry the legacy's
/// dodge-vs-miss distinction with the corrected band widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwingOutcome {
    /// The roll landed inside the actual hit rate.
    Hit,
    /// The roll failed inside `[actual hit, attacker hit)` — the slice of
    /// success the defender's AGI carved out (`b_dodge_hit`).
    Dodge,
    /// The roll failed beyond the attacker's own hit rate — the attacker
    /// whiffed regardless of any dodge (`b_miss_hit`).
    Miss,
}

/// Resolves one swing against `roll` (basis-10000, [`Rng::below`]`10_000`).
///
/// The dodge band keeps the legacy's two-label display (`b_dodge_hit` vs
/// `b_miss_hit`) but with correct geometry: exactly the slice
/// `[actual hit, actual hit + defender dodge)` is a dodge — the success the
/// defender's AGI carved out — and everything past the attacker's own hit
/// rate is a plain miss. The band is empty for a defender with no dodge and
/// for a swing whose actual rate was pinned by the 95% ceiling, so those
/// failures are misses, as they should be.
#[must_use]
pub(crate) fn swing_outcome(attacker_agi: i32, defender_agi: i32, roll: i32) -> SwingOutcome {
    let actual = hit_chance(attacker_agi, defender_agi);
    let dodge = defender_agi * AGI_DODGE_RATE_BP;
    let dodge_end = (actual + dodge)
        .min(BASE_HIT_RATE_BP + attacker_agi * AGI_HIT_RATE_BP)
        .min(10_000);
    if roll < actual {
        SwingOutcome::Hit
    } else if roll < dodge_end {
        SwingOutcome::Dodge
    } else {
        SwingOutcome::Miss
    }
}

/// Cat's QoL (2026-10-21): the +5 flat base every character carries, on both
/// the physical and the magic side. Without it a level-1 character with 10 STR
/// deals 2–3 damage to the first monster; with it the opening hit lands in the
/// 7–8 range.
///
/// The physical half is added in [`PlayerAttacks::raw_power`] (players only —
/// monsters keep their own attack rows), the magic half in [`mag_damage`].
pub(crate) const BASE_ATTACK_BONUS: i32 = 5;

/// §4.3.4 natural flat defence from VIT: `⌊VIT × (0.15 + 0.01×(Level−1))⌋ +
/// 5×⌊Level/10⌋`, integer form `(14 + level)/100`. The VIT half of a
/// player's `GetTotalAC()` this port models (the legacy `(hnd+level)*60/100`
/// arm is retired per §4.3.1 — hnd is crafting-only). Added to
/// [`level_ac`] on the monster→player path.
///
/// Spec pins: L10, 35 VIT = +13; L100, 550 VIT = +677.
#[must_use]
pub(crate) fn natural_flat_def(vit: i32, level: i32) -> i32 {
    vit * (14 + level) / 100 + 5 * (level / 10)
}

/// §4.3.6 MagDamage: `⌊MAG × (0.28 + 0.01×(Level−1))⌋ + 5×⌊Level/10⌋`,
/// integer form `(27 + level)/100` — the Arcane Boost curve replacing the
/// legacy flat `MAG / 4` — plus the flat base every character carries
/// ([`BASE_ATTACK_BONUS`], Cat 2026-10-21: base +5 MATK).
///
/// No consumer yet: the skill/spell pipeline is a later milestone, so this
/// carries `dead_code` until then. The curve is written and pinned now per
/// the §4.3 directive.
///
/// Pins: L1, 28 MAG = +12; L10, 37 MAG = +23; L50, 220 MAG = +199;
/// L100, 550 MAG = +753 (the §4.3.6 curve plus the flat base).
#[allow(dead_code)] // consumed when the spell pipeline lands
#[must_use]
pub(crate) fn mag_damage(mag: i32, level: i32) -> i32 {
    mag * (27 + level) / 100 + 5 * (level / 10) + BASE_ATTACK_BONUS
}

/// §4.3.7 magical hit rank: `(WIS*2) + Level` — WIS is the AGI of casters,
///100% effective for magic. No consumer until the spell pipeline lands.
#[allow(dead_code)] // consumed when the spell pipeline lands
#[must_use]
pub(crate) fn wis_hit_rank(wis: i32, level: i32) -> i32 {
    wis * 2 + level
}

/// §4.3.7 magical dodge rank — the same WIS ladder, corrected from the
/// legacy's DEX-sourced dodge so casters actually evade boss magic. No
/// consumer until the spell pipeline lands.
#[allow(dead_code)] // consumed when the spell pipeline lands
#[must_use]
pub(crate) fn wis_dodge_rank(wis: i32, level: i32) -> i32 {
    wis * 2 + level
}

/// §4.3.7 natural magic defence: `(WIS + Level) * 0.90`, integer form
/// `* 9 / 10`. No consumer until the spell pipeline lands.
#[allow(dead_code)] // consumed when the spell pipeline lands
#[must_use]
pub(crate) fn wis_magic_def(wis: i32, level: i32) -> i32 {
    (wis + level) * 9 / 10
}

/// §4.3.3 LUK drop bonus as the `add_rate` percent [`gz_data::drops::roll_drop`]
/// takes: +0.05% per LUK (`luk * 5 / 100`), capped +50% at 1000 LUK. The
/// existing `add_rate` mechanism lifts every entry's weight by that percent,
/// which raises the chance a drop resolves at all on kinds whose weights sum
/// below 10000.
#[must_use]
pub(crate) fn drop_add_rate(luk: i32) -> i32 {
    (luk * 5 / 100).clamp(0, 50)
}

/// The weak-monster EXP ratio cut — **off by owner directive** (Cat,
/// 2026-10-20): full EXP regardless of level or battle-power difference. Flip
/// to `true` to re-enable the legacy bands in [`weak_monster_percent`]; the
/// ladder behind the switch is kept exact and tested, so nothing else needs to
/// change. Drops were never cut by level: legacy `GetRandDropItem` ignores its
/// `level` argument entirely (`kindinfo-Manager.cpp:68-107`) and the port's
/// [`gz_data::drops::roll_drop`] has no level input either.
const WEAK_MONSTER_PENALTY: bool = false;

/// `GetMyExp` + `GetExp`'s weak-monster share — the pool one hit is worth
/// before the damage split (`Server-Attack.cpp:4138`'s `exp`, seeded at
/// `Char-Common.cpp:1362`, optionally cut down at `:4003-4058`).
///
/// The seed is the enemy's kind `Exp` column plus `level - 1`, then — while
/// [`WEAK_MONSTER_PENALTY`] stands `false` — nothing else: the whole pool,
/// every time. No region, event or village multiplier is applied either: all
/// external bonus sources are cut for P1 (owner decision, 2026-10-19).
#[must_use]
pub(crate) fn exp_seed(
    kind_exp: i64,
    enemy_level: i32,
    killer_level: i16,
    attacker_battle: i32,
    enemy_battle: i32,
) -> i64 {
    let pool = kind_exp + i64::from(enemy_level - 1);
    if !WEAK_MONSTER_PENALTY {
        // No weak-monster penalty: a level-120 farmer takes the same pool
        // from a level-1 kill as the level-1 who farmed it.
        return pool;
    }
    pool * i64::from(weak_monster_percent(
        killer_level,
        attacker_battle,
        enemy_battle,
    )) / 100
}

/// The legacy weak-monster gate `GetExp` applies when the killer is a PC past
/// level 20 (`Server-Attack.cpp:4003-4058`): the enemy's battle power as a
/// share of the killer's, cut into the legacy's bands — 0/30/40/60/80/100
/// percent. Behind [`WEAK_MONSTER_PENALTY`] (`false`), so [`exp_seed`] never
/// consults it — kept exact and tested so flipping the switch restores the
/// legacy behaviour with no further edit.
///
/// A killer at level 20 or under is never cut (the gate is `level > 20`), an
/// enemy with no battle power falls on the legacy's `else percent = 0` arm and
/// earns nothing, and a negative killer power — impossible on a real row —
/// keeps the full grant behind the same `attackerbattle >= 0` that skips the
/// whole block.
#[must_use]
pub(crate) fn weak_monster_percent(
    killer_level: i16,
    attacker_battle: i32,
    enemy_battle: i32,
) -> i32 {
    if killer_level <= 20 || attacker_battle < 0 {
        return 100;
    }
    let percent = if attacker_battle > 0 {
        enemy_battle * 100 / attacker_battle
    } else {
        0
    };
    // The legacy's band ladder (`:4015-4055`). The `addHunger` steps riding
    // the same bands belong to tiredness, which is on the cut list with the
    // rest of the health system.
    if percent < 40 {
        0
    } else if percent < 50 {
        30
    } else if percent < 60 {
        40
    } else if percent < 70 {
        60
    } else if percent < 80 {
        80
    } else {
        100
    }
}

/// The living-target share of a pool: `exp = (exp * damage) / enemymaxdamage
/// / 3` (`Server-Attack.cpp:4131-4145`). A killing blow skips this — the same
/// `IsAlive(enemyid)` check is what makes it — and takes the whole pool, so
/// the caller's `hit.died` is the switch.
///
/// Both divisions truncate, in that order, so a chip hit on a huge monster
/// yields 0 — which is the legacy's behaviour, not a bug. A non-positive
/// result reaches the caller's `exp <= 0` gate (`:4209`) and is not granted.
#[must_use]
pub(crate) fn exp_for_hit(pool: i64, damage: i32, enemy_max_hp: i32) -> i64 {
    if enemy_max_hp <= 0 {
        return 0;
    }
    (pool * i64::from(damage)) / i64::from(enemy_max_hp) / 3
}

/// `IncreaseExp`'s threshold check (`Parametabox.cpp:43-53`): the grant took
/// the total to or past the next level's requirement, and the level it lands
/// on is `level + 1` — the legacy's `IncreaseLevel(1)` raises exactly one per
/// grant, however far the overshoot reaches, and re-checks only on the next
/// grant. `None` at the cap, where [`LevelUpTable::next_threshold`] has no row
/// and `GetLevel() < MAX_CHARACTER_LEVEL` (`:49`) refuses at the same point.
#[must_use]
pub(crate) fn leveled_to(table: &LevelUpTable, level: i16, exp_after_grant: i64) -> Option<i16> {
    table
        .next_threshold(level)
        .filter(|need| exp_after_grant >= *need)
        .map(|_| level + 1)
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
    /// plus the STR and level terms [`str_damage`] and [`level_damage`] add,
    /// and the flat base every character carries ([`BASE_ATTACK_BONUS`], Cat
    /// 2026-10-21: base +5 ATK). Players only — monsters fight from their own
    /// attack rows.
    ///
    /// `class_kind` is the `KindInfo_Unique` id the character was created with
    /// (`SessionInfo::class_kind`).
    #[must_use]
    pub(crate) fn raw_power(&self, class_kind: u16, str_: i32, level: i32) -> (i32, i32) {
        let term = level_damage(level) + str_damage(str_, level) + BASE_ATTACK_BONUS;
        match self.by_kind.get(&class_kind) {
            Some(row) => (row.min_dmg + term, row.max_dmg + term),
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
    #[must_use]
    pub(crate) fn interval(&self, class_kind: u16) -> u64 {
        self.by_kind
            .get(&class_kind)
            .map_or(1_000, |row| attack_interval_ms(row.attack_interval))
    }
}

/// One kind's swing cadence, in milliseconds: its `CharAttackInfo`
/// `Attack_Interval` (`CharAttackRow::attack_interval`).
///
/// Both halves of a fight run on this clock and nowhere else. The **client**
/// starts the attacker's swing animation from it — it receives the order once
/// and then repeats on `dwAttackInterval`
/// (`MouseOperation.cpp:771`) — so a server that paces its hits on any other
/// timer lands damage several animations late and the swing reads as a miss.
///
/// A kind with no row, or one whose `Attack_Interval` does not read as a
/// non-negative integer, gets 1000 ms — the most common shipped value. A row
/// reading 0 would re-arm a timer with a zero sleep and spin it, so a readable
/// value is floored at 100 ms and capped at 60 s for a corrupt one.
#[must_use]
pub(crate) fn attack_interval_ms(attack_interval: i32) -> u64 {
    u64::try_from(attack_interval)
        .unwrap_or(1_000)
        .clamp(100, 60_000)
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

/// `CheckLastMsgClock(200)` — the legacy's per-character gate on the whole
/// `DoMsg_GAMEMSG_REQUEST_HIT` handler (`Server-Person.cpp:2760`).
pub(crate) const HIT_THROTTLE_MS: u64 = 200;

/// What one swing did, so a caller holding an engagement can tell "wait for a
/// better moment" from "there is nobody there".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Swing {
    /// The swing was made and resolved against a live in-range target: it
    /// landed its damage, or the target dodged or missed it (§4.3.2). Either
    /// way the engagement holds and the client keeps swinging.
    Landed,
    /// The target is a monster, just not close enough yet.
    OutOfRange,
    /// The swing could not be thrown at all: the attacker is dead, or no
    /// monster stands on that tile. Either way there is nothing to keep
    /// swinging at.
    NoTarget,
}

/// Whether an `OutOfRange` swing may (re)start the approach walk.
///
/// # Why a walk in flight does not block the click
///
/// The legacy's `ORDER_ATTACK` replaces the order in flight outright
/// (`clCurOrder.Set(&ReserveOrder)`, `char-order.cpp:226-263`) and the client
/// applies it locally before sending (`Client.cpp:3297`). The port used to wait
/// for the player's own ordered walk to arrive and then for the next weapon
/// interval before planning the approach, so a click that landed mid-walk left
/// the avatar finishing the old destination — and standing still for up to one
/// interval afterwards — while the client's own copy was already closing on the
/// monster. That desync is the first-step hesitation on a distance attack.
///
/// An approach already running keeps its walk: the client re-decides
/// `CHARWORKSTEP_DECIDELENGTH` only when the walk arrives, so re-planning
/// mid-tile on every swing would put this server ahead of the avatar it tracks.
/// Only a click naming a *different* target — or the first click of a fight —
/// takes the walk over, exactly as a new order does on the client.
pub(crate) fn may_start_approach(
    outcome: Swing,
    movement_running: bool,
    approach: Option<u16>,
    target: u16,
) -> bool {
    outcome == Swing::OutOfRange && (!movement_running || approach != Some(target))
}

/// Walks toward `target` if some standable tile is in range of it, replacing any
/// walk already running — the half of `ORDER_ATTACK` the client does for itself
/// (`Client.cpp:3297` `LetOrder` sets `CHARWORKSTEP_DECIDELENGTH`) and this
/// server has to do on its own tile.
///
/// Returns the tile it started for, or `None` when there is nothing to walk to.
/// `None` is not always a wall: asked from inside range, `approach_tile` refuses
/// the target's own tile as an answer and returns nothing to do, which is what
/// a finished approach wants.
pub(crate) fn start_approach(
    monsters: &Mutex<monsters::MonsterWorld>,
    player_attacks: &PlayerAttacks,
    movement: &mut Option<Movement>,
    target: u16,
    (ax, ay): (i32, i32),
    character: &CharacterFull,
) -> Option<(i32, i32)> {
    let class_kind = u16::try_from(character.class_kind).unwrap_or(1);
    let range = player_attacks.range(class_kind);
    let dest = monsters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .approach_tile(target, (ax, ay), range, class_kind)?;
    // No 241 goes with this walk: no new order arrived, so the legacy sends only
    // the per-tile 200s `step_movement` already fans — and the client is walking
    // its own copy of the approach already.
    let mut mv = Movement::new(
        dest,
        (ax, ay),
        character.map_id,
        i32::from(character.move_speed),
    );
    mv.deadline = Some(next_move_deadline(&mv, Instant::now()));
    *movement = Some(mv);
    Some(dest)
}

/// Resolves one player swing against a field monster, end to end: the range
/// gate, the power and crit rolls, the 268 that carries the new life, the EXP
/// grant every landed hit earns, and — on a kill — the death frames and the
/// drop roll. A grant that crosses the next threshold levels the character
/// (215 + 7221 to the neighbourhood, and no 270) instead of answering with
/// the plain grant frame.
///
/// One body serves both wire shapes that reach it. `REQUEST_HIT` (267) is what
/// the design doc assumed, but the shipped client only ever built it inside a
/// commented-out block (`char-attack.cpp:2100-2109`); the attack it really
/// sends is `ORDER_ATTACK` (5) inside `REQUEST_USERORDER` (240)
/// (`MouseOperation.cpp:5631`, `Client.cpp:3299`), and that is what makes this
/// worth sharing rather than duplicating.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn player_swing(
    registry: &Registry,
    monsters: &Mutex<monsters::MonsterWorld>,
    player_attacks: &PlayerAttacks,
    combat_rng: &mut Rng,
    characters: &PgCharacterRepository,
    items: &PgItemRepository,
    session_key: SessionKey,
    level_table: &LevelUpTable,
    live: &mut LiveFrames<'_>,
    (ax, ay): (i32, i32),
    target_unique: u16,
) -> Result<Swing, AnyError> {
    // A dead character cannot swing (the legacy's `IsAlive` guard,
    // `Server-Person.cpp:2758`). `me` also carries this session's own unique,
    // which is the only attacker id the world needs.
    let Some(me) = registry.info(session_key) else {
        return Ok(Swing::NoTarget);
    };
    if me.life == 0 {
        return Ok(Swing::NoTarget);
    }
    // Everything this swing reads out of the session row, taken before the
    // level-up below writes back through `&mut live`: one borrow, done with,
    // and no character in the world means nothing to swing at.
    let Some((
        character_id,
        class_kind,
        attacker_level,
        attacker_str,
        attacker_dex,
        attacker_mag,
        attacker_vit,
        attacker_luk,
    )) = live.character.as_ref().map(|character| {
        (
            character.character_id,
            u16::try_from(character.class_kind).unwrap_or(1),
            character.level,
            character.str,
            character.dex,
            character.mag,
            character.vit,
            character.luk,
        )
    })
    else {
        return Ok(Swing::NoTarget);
    };
    let range = player_attacks.range(class_kind);
    let (min, max) = player_attacks.raw_power(class_kind, attacker_str, i32::from(attacker_level));
    let raw = roll_power(min, max, combat_rng.up_to((max - min).max(0)));
    let crit = apply_critical(
        raw,
        player_attacks.critical_rate(class_kind),
        attacker_luk,
        combat_rng.up_to(99),
    );

    let hit = {
        let mut world = monsters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The gate reads the monster's own tile rather than one the client
        // claimed (`IsInRange` reads `pcltarget->clPos`,
        // `Server-Person.cpp:2765`): the claim adds nothing a server-side
        // lookup does not already have, and a client does not get to name the
        // distance.
        let Some((tx, ty)) = world.position(target_unique) else {
            return Ok(Swing::NoTarget);
        };
        if (tx - ax).abs() > range || (ty - ay).abs() > range {
            return Ok(Swing::OutOfRange);
        }
        // Cat's §4.3.2 Ghost Dodge fix, player→monster half: roll the swing
        // against this kind's AGI (`unit_agi` — the `dex` column). The
        // client-supplied `si_hit_rate` / `si_dodge_rate` in the 267 are
        // ignored: the shipped client computes them from the retired HND
        // formulas, so reading them would leave the whole reformation
        // decorative. A dodge or a miss still reaches `hit_by_player` so the
        // monster turns on the attacker the way it does on any swing — the
        // legacy's `SetBeAttackedSwitch` runs before the dodge check
        // (`Server-Attack.cpp:585-591`) — but it lands no damage, rolls no
        // drop and owes no EXP.
        let outcome = swing_outcome(
            attacker_dex,
            world.unit_agi(target_unique).unwrap_or(0),
            combat_rng.below(10_000) as i32,
        );
        world.hit_by_player(
            registry,
            target_unique,
            me.char_unique.si16(),
            crit,
            outcome,
            drop_add_rate(attacker_luk),
            now_tick(),
            combat_rng,
        )
    };
    // A target the world does not hold — a player, a static NPC, or a monster
    // that just died between the lookup and the hit. The legacy's null `CR[id]`
    // guard drops the hit the same way (`Server-Attack.cpp:586`).
    let Some(hit) = hit else {
        return Ok(Swing::NoTarget);
    };

    // The 268 answers the *target*, so everyone near the monster sees the same
    // frame (`Server-Attack.cpp:2775`). A dodge or a miss sends the same
    // zeroed body with its flag that the legacy did
    // (`Server-Attack.cpp:608-634`): the client reads the flag and plays the
    // text without applying any life.
    let (damage, life) = if hit.outcome == SwingOutcome::Hit {
        (hit.damage, hit.life as i32)
    } else {
        (0, 0)
    };
    registry.broadcast_near_point(
        monsters::FIELD_MAP,
        hit.x,
        hit.y,
        GameMsg::ResponseHit as u16,
        HitResponse {
            char_unique: hit.unique as i16,
            damage,
            double_damage: 0,
            life,
            mode: 0,
            special_attack_switch: 0,
            b_indure: false,
            b_miss_hit: hit.outcome == SwingOutcome::Miss,
            b_dodge_hit: hit.outcome == SwingOutcome::Dodge,
            ui_type: HIT_TARGET_MONSTER,
        }
        .encode()
        .to_vec(),
    );
    // Cat's Bug-5 id trace (2026-10-21): which entity the swing resolved,
    // from which tile, and what life the frame carried — the line that
    // answers "did the attacked entity id change when the bar bounced" —
    // with the internal log tag (Cat 2026-10-21). Frame values are logged
    // deliberately: they are what the client saw.
    let tag = monsters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .log_tag(hit.unique);
    logln!(
        "game: 268: {session_key:?}: monster {} {} at ({}, {}) life {life} \
         (-{damage}) {:?}",
        hit.unique,
        tag,
        hit.x,
        hit.y,
        hit.outcome
    );
    if hit.outcome != SwingOutcome::Hit {
        // A dodge or miss owes nothing: no EXP, no death frame, no life
        // movement — the legacy's handler returns right after the flagged
        // frame (`Server-Attack.cpp:610-634`). The swing itself was made, so
        // the engagement holds and the client keeps swinging.
        return Ok(Swing::Landed);
    }

    // What this hit is worth, decided before any frame goes out so both arms
    // of the legacy's `IsAlive(enemyid)` (`Server-Attack.cpp:4131`) share one
    // grant point. `ExpOperate` runs at `Hit`'s tail (`:2921-2928`), i.e.
    // after the damage frames — which the single grant below honours.
    let gained = {
        let pool = exp_seed(
            hit.exp,
            hit.level,
            attacker_level,
            battle_power(attacker_str, attacker_dex, attacker_mag, attacker_vit),
            hit.battle_power,
        );
        if hit.died {
            // Not alive, so the scaling is skipped and the killing blow takes
            // the pool whole.
            pool
        } else {
            // Alive: share by the damage actually dealt — `clinfo.siDamage =
            // realdamage` (`:2926`), which is `hit.damage`, not the
            // pre-defence roll the 268 reports.
            exp_for_hit(
                pool,
                hit.damage,
                i32::try_from(hit.max_life).unwrap_or(i32::MAX),
            )
        }
    };

    if hit.died {
        // Death: no dedicated opcode. `Life = 0` in the 268 is the first half and
        // `ORDER_DIE` in a 241 is the animation, which is what this frame is. The
        // 11 that removes the avatar is deliberately *not* sent here — a client
        // that receives both in one pass deletes the avatar before the animation
        // can draw, so the world holds the corpse and `despawn_corpses` sends the
        // removal once [`monsters::CORPSE_MS`] has run. The legacy reaches the same
        // place by holding the dead char until its action loop finds it invalid
        // (`CharManager.cpp:257-277`).
        registry.broadcast_near_point(
            monsters::FIELD_MAP,
            hit.x,
            hit.y,
            GameMsg::ResponseUserorder as u16,
            OrderData::die_order(i32::from(hit.unique))
                .encode()
                .to_vec(),
        );
    }

    // The grant (`ExpOperate`, `Server-Attack.cpp:4172`): persist first, then
    // either level up — 215 to self, 7221 to the neighbourhood including the
    // leveler (`char.cpp:5943`), and no 270 because `IncreaseExp` returned
    // TRUE instead of the grant frame — or answer with the plain 270.
    if gained > 0 {
        let new_exp = characters.add_exp(character_id, gained).await?;
        if let Some(new_level) = leveled_to(level_table, attacker_level, new_exp) {
            let row = characters
                .level_up(
                    character_id,
                    derived_max_life(attacker_vit, new_level),
                    derived_max_mana(attacker_mag, new_level),
                )
                .await?;
            // Mirror into the session row — the `handle_use_bonus` pattern
            // (`stats.rs`): the next 215/248, this swing's own power roll and
            // the next 250 allowance all read it.
            if let Some(character) = live.character.as_mut() {
                character.level = row.level;
                character.exp = row.exp;
                character.bonus_points = row.bonus_points;
                character.max_life = row.max_life;
                character.max_mana = row.max_mana;
            }
            // The full-heal half of `LevelUpAction` (`SetLife(GetMaxLife())`,
            // `char.cpp:5794`) — session life is a session value with no DB
            // write (`registry::restore_life`) — plus the session's cached
            // level that the 119 chat delivery echoes
            // (`Server-System.cpp:591`).
            registry.update_level(session_key, i32::from(row.level));
            registry.restore_life(session_key, u32::try_from(row.max_life).unwrap_or(0));
            // The `UPDATE_IP` flush (`char.cpp:5968`) as a 215, built the way
            // `handle_use_bonus` builds it so the two frames cannot disagree.
            let body = {
                let character = live
                    .character
                    .as_ref()
                    .ok_or("level-up row disappeared after the write")?;
                // The full-heal above restored the session to this same total, so
                // reading it back is the only way the 215 can never claim more life
                // than the session actually holds (Bug 2).
                let life = registry
                    .info(session_key)
                    .map_or(row.max_life as i32, |s| s.life as i32);
                let basic = basic_info_for(live.account_id, &live.account_name, character, life)?;
                // Re-capture the block the 214 inspect reads.
                registry.set_basic_info(session_key, basic.clone());
                encode_person_ip(me.char_unique.si16(), &basic)
            };
            registry.send_to(session_key, GameMsg::ResponsePersonIp as u16, body);
            // `cltGameMsgResponse_LevelUp` to the 3×3 cell block around the
            // kill, self included — `SendNeighbourMsg(&clMsg, true)` fans from
            // the leveler's own position (`char.cpp:5945`).
            registry.broadcast_near_point(
                monsters::FIELD_MAP,
                hit.x,
                hit.y,
                GameMsg::ResponseLevelup as u16,
                LevelUpResponse {
                    si_char_unique: me.char_unique.si16(),
                    si_level: i8::try_from(row.level).map_err(|_| {
                        format!("level {} does not fit the SI08 siLevel", row.level)
                    })?,
                    si_fame_level: 0,
                }
                .encode()
                .to_vec(),
            );
            let name = live
                .character
                .as_ref()
                .map(|character| character.name.clone())
                .unwrap_or_default();
            logln!(
                "game: level up: {name} reached level {} (exp {}, bonus {}) — 215 + 7221",
                row.level,
                row.exp,
                row.bonus_points
            );
        } else {
            registry.send_to(
                session_key,
                GameMsg::ResponseGetexp as u16,
                GetExpResponse {
                    si_char_unique: me.char_unique.si16(),
                    si_reason: 0,
                    si_exp: gained.min(i32::MAX as i64) as i32,
                    si_fame_exp: 0,
                }
                .encode()
                .to_vec(),
            );
        }
    }

    if !hit.died {
        return Ok(Swing::Landed);
    }
    // `GetRandDropItem` rolled at most one entry at death
    // (`kindinfo-Manager.cpp:68-107`); grant it into the bag.
    for (item_unique, count) in hit.drops {
        let Ok(item_unique) = u16::try_from(item_unique) else {
            continue;
        };
        match items.grant(character_id, item_unique, count).await {
            // One 7210 does both halves of "the drop landed": it writes the
            // slot, so the item is in the bag without a relog, and because it
            // carries `OPTION_SHOW` with a non-zero `siChangedNum` the client
            // also *says* so — `ItemGetChatRecv` puts "Obtained <name> x<n>" in
            // the loot line and `SetItemData` raises the acquisition popup
            // (`Client-Person-BigInfo.cpp:1054-1093`).
            Ok(Some(granted)) => {
                let gained = encode_person_item_gained(
                    granted.slot,
                    granted.count,
                    &Item::from_raw(&granted.raw),
                );
                registry.send_to(
                    session_key,
                    GameMsg::ResponsePersoniteminfo as u16,
                    gained.to_vec(),
                );
                logln!(
                    "game: character {character_id} received item {item_unique} x{} (slot {})",
                    granted.count,
                    granted.slot
                );
            }
            Ok(None) => logln_err!(
                "game: drop {item_unique} found no free slot for character {character_id}"
            ),
            Err(e) => logln_err!("game: drop grant failed: {e}"),
        }
    }
    Ok(Swing::Landed)
}

// The player-side flow over those numbers, moved here from `main.rs` so the
// combat family has one home: the approach step, one swing end to end, and
// the throttle the dispatcher keeps on the 267 arm. Unlike the numbers above,
// these touch the registry and the world.
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
    fn the_str_term_walks_the_4_3_curve() {
        // Cat's §4.3.5: `⌊STR × (0.20 + 0.01×(Level−1))⌋ + 5×⌊Level/10⌋`,
        // integer form `STR × (19 + level) / 100 + 5 × (level / 10)` — one
        // curve for every physical weapon; the legacy's sword/bow/staff arms
        // are retired by the directive.
        assert_eq!(str_damage(10, 1), 2, "L1: 10 STR = +2");
        assert_eq!(str_damage(35, 10), 15, "L10: 35 STR = +15");
        assert_eq!(str_damage(120, 30), 73, "L30: 120 STR = +73");
        assert_eq!(str_damage(220, 50), 176, "L50: 220 STR = +176");
        assert_eq!(str_damage(550, 100), 704, "L100: 550 STR = +704");
        // The level term is the milestone bonus alone at L1–L9.
        assert_eq!(str_damage(0, 1), 0);
        assert_eq!(str_damage(0, 10), 5, "the L10 milestone, no STR under it");
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
    fn a_critical_pays_150_percent_and_luk_buys_up_to_225() {
        // Cat's §4.3.3: 150% base (the legacy's flat doubling is retired with
        // the directive), `×3 × (10_000 + min(LUK×5, 5000)) / 20_000`.
        assert_eq!(apply_critical(40, 30, 0, 0), 60, "150% base");
        assert_eq!(apply_critical(40, 30, 0, 29), 60, "roll 29 < rate 30");
        assert_eq!(apply_critical(40, 30, 0, 30), 40, "roll 30 >= rate 30");
        assert_eq!(apply_critical(40, 30, 0, 99), 40);
        assert_eq!(apply_critical(40, 0, 0, 0), 40, "rate 0 never crits");
        // LUK scales the multiplier: +0.05%/point → 1000 LUK = ×2.25.
        assert_eq!(apply_critical(40, 30, 1000, 0), 90, "225% at cap");
        // Past the cap is flat — 2000 LUK buys no more than 1000.
        assert_eq!(apply_critical(40, 30, 2000, 0), 90, "the +50% cap");
        // Mid curve: 100 LUK = +5% → ×1.575.
        assert_eq!(apply_critical(200, 30, 100, 0), 315);
        // The rate gate still holds with LUK under it.
        assert_eq!(apply_critical(40, 30, 1000, 50), 40, "no crit, no scaling");
    }

    #[test]
    fn experience_pool_is_whole_while_the_weak_monster_penalty_is_off() {
        // `GetMyExp()` = kind exp + (level - 1) — and nothing else, per the
        // owner directive behind `WEAK_MONSTER_PENALTY`: every ratio that the
        // legacy band ladder would have cut down still pays the whole pool.
        assert_eq!(exp_seed(90, 1, 1, 100, 100), 90);
        assert_eq!(exp_seed(90, 10, 20, 100, 100), 99, "kind exp + (level - 1)");
        assert_eq!(exp_seed(90, 10, 21, 100, 40), 99, "40% ratio: no 30% band");
        assert_eq!(exp_seed(90, 10, 21, 100, 90), 99, "90% ratio stays whole");
        assert_eq!(exp_seed(90, 10, 21, 100, 20), 99, "20% ratio: not zeroed");
        assert_eq!(
            exp_seed(90, 10, 21, 100, 0),
            99,
            "an enemy with no battle power still pays — the legacy's `else 0` is off"
        );
        assert_eq!(
            exp_seed(90, 10, 120, i32::MAX, 0),
            99,
            "the level-120 farmer case the directive is about"
        );
    }

    /// The ladder **behind** `WEAK_MONSTER_PENALTY` — `exp_seed` no longer
    /// consults it, but it stays exact so flipping the switch back on needs
    /// no re-verification.
    #[test]
    fn the_disabled_weak_monster_ladder_still_walks_the_legacy_bands() {
        // The enemy's battle power as a share of the killer's, past the gate.
        assert_eq!(weak_monster_percent(21, 100, 39), 0, "below 40%");
        assert_eq!(weak_monster_percent(21, 100, 40), 30);
        assert_eq!(weak_monster_percent(21, 100, 49), 30, "the band is 40..50");
        assert_eq!(weak_monster_percent(21, 100, 50), 40);
        assert_eq!(weak_monster_percent(21, 100, 59), 40);
        assert_eq!(weak_monster_percent(21, 100, 60), 60);
        assert_eq!(weak_monster_percent(21, 100, 69), 60);
        assert_eq!(weak_monster_percent(21, 100, 70), 80);
        assert_eq!(weak_monster_percent(21, 100, 79), 80);
        assert_eq!(
            weak_monster_percent(21, 100, 80),
            100,
            "80% and up is whole"
        );
        assert_eq!(weak_monster_percent(21, 100, 500), 100, "stronger than us");
        // The gate: a killer at or under level 20 is never cut...
        assert_eq!(weak_monster_percent(20, 100, 0), 100);
        assert_eq!(weak_monster_percent(1, 100, 0), 100);
        // ...while an enemy with no battle power earns nothing on the legacy's
        // `else percent = 0` arm, and the share truncates like its `* 100 /`.
        assert_eq!(weak_monster_percent(21, 100, 0), 0);
        assert_eq!(weak_monster_percent(21, 3, 1), 0, "1*100/3 -> 33");
        // A negative killer power is left alone by `attackerbattle >= 0`.
        assert_eq!(weak_monster_percent(21, -1, 500), 100);
    }

    #[test]
    fn experience_is_the_legacy_share_of_a_living_hit() {
        // The pool from `exp_seed`, split by `(pool * damage) / maxhp / 3`
        // (`Server-Attack.cpp:4138`).
        // pool 90, damage 60 of 60 max -> 90*60/60/3 = 30.
        assert_eq!(exp_for_hit(90, 60, 60), 30);
        // A chip hit on the same monster: 90 * 3 / 60 / 3 = 1 (truncated).
        assert_eq!(exp_for_hit(90, 3, 60), 1);
        // The level-10 pool of 99: 99 * 60 / 60 / 3 = 33.
        assert_eq!(exp_for_hit(99, 60, 60), 33);
        // A hit on a much larger monster shares by the same rule.
        assert_eq!(exp_for_hit(319, 10, 1000), 1);
        // Degenerate inputs cannot divide by zero.
        assert_eq!(exp_for_hit(90, 0, 60), 0);
        assert_eq!(exp_for_hit(90, 60, 0), 0);
    }

    /// The threshold check `IncreaseExp` runs (`Parametabox.cpp:49`): the
    /// exact boundary, one level per grant, nothing at the cap.
    #[test]
    fn a_grant_levels_once_at_the_threshold_and_never_past_120() {
        // 120 rows shaped like the shipped file, `NeedExp` = 75 * (level - 1),
        // so a level-1 character needs 75 for level 2.
        let mut file = String::new();
        for level in 1..=120 {
            file.push_str(&format!(
                "{level}\t{}\t0\t0\t0\n",
                75 * i64::from(level - 1)
            ));
        }
        let table = LevelUpTable::parse(file.as_bytes()).expect("120 rows");
        assert_eq!(
            leveled_to(&table, 1, 74),
            None,
            "one below the 75 threshold"
        );
        assert_eq!(leveled_to(&table, 1, 75), Some(2), "exactly at it");
        // One level per grant — the legacy's `IncreaseLevel(1)`: even a 9.9M
        // overshoot lands on 2, not 3, and the next grant re-checks from there.
        assert_eq!(leveled_to(&table, 1, 9_999_999), Some(2));
        assert_eq!(leveled_to(&table, 2, 149), None, "level 2 needs 150");
        assert_eq!(leveled_to(&table, 2, 150), Some(3));
        assert_eq!(
            leveled_to(&table, 120, i64::MAX),
            None,
            "the cap has no next level"
        );
        assert_eq!(
            leveled_to(&LevelUpTable::empty(), 1, i64::MAX),
            None,
            "no table, no level-ups"
        );
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
        // Even an unjoined kind carries the flat base (shared with
        // `mag_damage`, pinned there).
        let (min, max) = attacks.raw_power(u16::MAX, 0, 1);
        assert!(min >= BASE_ATTACK_BONUS && max >= BASE_ATTACK_BONUS);
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

    /// The same clamp, for the field monsters: a `SpawnUnit` reads its cadence
    /// straight from its `CharAttackInfo` row, so this is the rule that decides
    /// both a raccoon's 2 000 ms and a corrupt row's floor. A row reading 0
    /// would re-arm a timer with a zero sleep and spin it.
    #[test]
    fn attack_interval_ms_never_arms_a_zero_sleep() {
        assert_eq!(
            attack_interval_ms(2_000),
            2_000,
            "the shipped value passes through untouched"
        );
        assert_eq!(attack_interval_ms(0), 100, "a zero row would spin the loop");
        assert_eq!(
            attack_interval_ms(-5),
            1_000,
            "a negative interval is a malformed row, so it takes the no-row default"
        );
        assert_eq!(attack_interval_ms(1), 100, "the floor is 100 ms");
        assert_eq!(
            attack_interval_ms(10_000_000),
            60_000,
            "and the ceiling is a minute"
        );
    }

    /// Cat's §4.3.2 Ghost Dodge fix (2026-10-20): the actual hit rate is the
    /// attacker's minus the defender's, clamped 10%..=95% — the subtraction
    /// the legacy left commented out (`Server-Attack.cpp:596-599`). One AGI
    /// point is 18 basis points both ways.
    #[test]
    fn the_actual_hit_rate_is_attacker_minus_defender_in_a_10_to_95_band() {
        // Parity cancels: 7000 + AGI*18 − AGI*18 = the 70% legacy base.
        assert_eq!(hit_chance(5, 5), 7000, "level-1 parity is the legacy base");
        assert_eq!(hit_chance(200, 200), 7000, "parity at any AGI");
        // One point of AGI is worth 18 bp of hit — and 18 bp of dodge.
        assert_eq!(hit_chance(10, 0), 7180);
        assert_eq!(hit_chance(0, 10), 6820);
        // The floor: a swing far behind on AGI still lands 10% of the time.
        assert_eq!(hit_chance(0, 1_000), 1000);
        // The ceiling: no swing ever becomes certain, not even maxed out.
        assert_eq!(hit_chance(1_000, 0), 9500);
        // The floor beats the ceiling when both AGIs are extreme.
        assert_eq!(hit_chance(1_000, 1_000), 7000, "parity even at the cap");
    }

    /// The Ghost Dodge regression itself: over the whole roll space, raising
    /// the defender's AGI strictly lowers the number of hits — the legacy bug
    /// was that dodge relabelled misses without moving this count at all.
    /// Enumerating all 10 000 rolls makes the mitigation exact, not
    /// statistical.
    #[test]
    fn defender_agi_genuinely_causes_misses_over_the_whole_roll_space() {
        let hit_count = |att: i32, def: i32| {
            (0..10_000)
                .filter(|&roll| swing_outcome(att, def, roll) == SwingOutcome::Hit)
                .count() as i32
        };
        // The hit count IS the actual rate — the clamp and the bands cannot
        // drift apart from it.
        assert_eq!(hit_count(50, 0), hit_chance(50, 0));
        // Mitigation: a defender's AGI moves the count down, every step.
        assert_eq!(hit_count(50, 0), 7900);
        assert_eq!(hit_count(50, 100), 6100);
        assert_eq!(hit_count(50, 200), 4300);
        assert!(hit_count(50, 200) < hit_count(50, 100));
        assert!(hit_count(50, 100) < hit_count(50, 0));
        // And the attacker's AGI moves it back up, both ways.
        assert_eq!(hit_count(200, 50), 9500, "the 95% ceiling");
        assert_eq!(hit_count(0, 50), 6100, "an unskilled attacker vs 50 AGI");
    }

    /// The two failure labels keep the legacy's display semantics with the
    /// corrected geometry: a dodge is exactly the slice the defender's AGI
    /// carved out of success, and everything else a swing fails is a miss.
    #[test]
    fn the_dodge_band_is_exactly_the_slice_the_defender_carved_out() {
        let band = |att: i32, def: i32| {
            let mut dodge = 0;
            let mut miss = 0;
            for roll in 0..10_000 {
                match swing_outcome(att, def, roll) {
                    SwingOutcome::Hit => {}
                    SwingOutcome::Dodge => dodge += 1,
                    SwingOutcome::Miss => miss += 1,
                }
            }
            (dodge, miss)
        };
        // No dodge under it: the failure band is all miss (parity at 1 AGI —
        // the 18 bp carved out is truncation-visible only in the rate).
        assert_eq!(band(50, 0), (0, 2100), "no dodge, no dodge label");
        // 100 AGI of defence at 50 AGI of attack: the carved slice is the
        // defender's full dodge (1800 bp), the rest is miss.
        assert_eq!(band(50, 100), (1800, 2100));
        // A ceiling-pinned swing with nothing to dodge is still 5% miss — the
        // 95% cap keeps every swing fallible.
        assert_eq!(band(500, 0), (0, 500));
    }

    /// Cat's §4.3.4 natural flat defence: the VIT half of a player's AC that
    /// joins the level term on the monster→player path.
    #[test]
    fn natural_flat_def_walks_the_4_3_curve() {
        assert_eq!(natural_flat_def(10, 1), 1, "L1: 10 VIT = +1");
        assert_eq!(natural_flat_def(35, 10), 13, "L10: 35 VIT = +13");
        assert_eq!(natural_flat_def(120, 30), 67, "L30: 120 VIT = +67");
        assert_eq!(natural_flat_def(220, 50), 165, "L50: 220 VIT = +165");
        assert_eq!(natural_flat_def(550, 100), 677, "L100: 550 VIT = +677");
    }

    /// Cat's §4.3.6 MagDamage curve (Arcane Boost) plus the flat base every
    /// character carries (Cat 2026-10-21: base +5 MATK). No combat consumer
    /// until the spell pipeline lands; pinned now per the directives.
    #[test]
    fn mag_damage_walks_the_4_3_curve() {
        assert_eq!(mag_damage(28, 1), 12, "L1: 28 MAG = +12");
        assert_eq!(mag_damage(37, 10), 23, "L10: 37 MAG = +23");
        assert_eq!(mag_damage(220, 50), 199, "L50: 220 MAG = +199");
        assert_eq!(mag_damage(550, 100), 753, "L100: 550 MAG = +753");
        assert_eq!(
            mag_damage(0, 1),
            BASE_ATTACK_BONUS,
            "zero MAG still carries the flat base"
        );
    }

    /// Cat's §4.3.7 WIS ranks — the caster's half of the AGI reformation.
    /// No consumer until the spell pipeline lands; pinned now per the
    /// directive.
    #[test]
    fn wis_carries_the_magic_hit_dodge_and_defense_ranks() {
        assert_eq!(wis_hit_rank(50, 10), 110, "(WIS*2) + Level");
        assert_eq!(wis_dodge_rank(50, 10), 110, "the same ladder, corrected");
        assert_eq!(wis_magic_def(50, 10), 54, "(WIS + Level) * 0.90");
        assert_eq!(wis_magic_def(100, 100), 180);
    }

    /// Cat's §4.3.3 LUK drop bonus: +0.05% per point, capped +50%.
    #[test]
    fn luk_drop_rate_walks_to_the_50_percent_cap() {
        assert_eq!(drop_add_rate(0), 0);
        assert_eq!(drop_add_rate(20), 1, "20 LUK = +1%");
        assert_eq!(drop_add_rate(100), 5, "+0.05%/point");
        assert_eq!(drop_add_rate(1000), 50, "the cap");
        assert_eq!(drop_add_rate(2000), 50, "past the cap is flat");
    }
}
