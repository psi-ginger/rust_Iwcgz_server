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

use gz_data::kindinfo::{CharAttackRow, canonical_char_attack, load_kind_unique};
use gz_db::{
    CharacterFull, CharacterRepository, ItemRepository, PgCharacterRepository, PgItemRepository,
};
use gz_proto::combat::{GetExpResponse, HIT_TARGET_MONSTER, HitResponse};
use gz_proto::item::{Item, encode_person_item_gained};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::OrderData;

use crate::monsters;
use crate::movement::{Movement, next_move_deadline};
use crate::registry::{Registry, SessionKey};
use crate::{AnyError, GameSession, logln, logln_err, now_tick};

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
    /// Damage went out and the target took it.
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
/// gate, the power and crit rolls, the 268 that carries the new life, and — on
/// a kill — the death frames, the EXP grant and the drop roll.
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
    login: &Option<GameSession>,
    character: &CharacterFull,
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
    let class_kind = u16::try_from(character.class_kind).unwrap_or(1);
    let range = player_attacks.range(class_kind);
    let (min, max) =
        player_attacks.raw_power(class_kind, character.str, i32::from(character.level));
    let raw = roll_power(min, max, combat_rng.up_to((max - min).max(0)));
    let crit = apply_critical(
        raw,
        player_attacks.critical_rate(class_kind),
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
            // Both sides of this comparison come from stores that are updated on
            // separate paths, and a player has a third copy in the registry — so
            // a failure is only actionable with every copy of "where I am" in
            // view next to the target's.
            let seen = registry
                .info(session_key)
                .map_or_else(|| "unknown".to_string(), |i| format!("({}, {})", i.x, i.y));
            logln!(
                "game: swing {target_unique} out of range: gate uses ({ax}, {ay}), registry \
                 says {seen}, monster at ({tx}, {ty}), range {range}, class {class_kind}"
            );
            return Ok(Swing::OutOfRange);
        }
        world.hit_by_player(
            registry,
            target_unique,
            me.char_unique.si16(),
            crit,
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
    // frame (`Server-Attack.cpp:2775`).
    registry.broadcast_near_point(
        monsters::FIELD_MAP,
        hit.x,
        hit.y,
        GameMsg::ResponseHit as u16,
        HitResponse {
            char_unique: hit.unique as i16,
            damage: hit.damage,
            double_damage: 0,
            life: hit.life as i32,
            mode: 0,
            special_attack_switch: 0,
            b_indure: false,
            b_miss_hit: false,
            b_dodge_hit: false,
            ui_type: HIT_TARGET_MONSTER,
        }
        .encode()
        .to_vec(),
    );

    if !hit.died {
        return Ok(Swing::Landed);
    }
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

    let Some(character_id) = login.as_ref().and_then(|s| s.character_id) else {
        // `GetExp`'s share, per hit and not per kill
        // (`Server-Attack.cpp:4138`) needs an owning row; without one the kill
        // still stands and only the reward is lost.
        return Ok(Swing::Landed);
    };
    let gained = exp_for_hit(
        hit.exp,
        hit.level,
        crit,
        i32::try_from(hit.max_life).unwrap_or(i32::MAX),
    );
    if gained > 0 {
        characters.add_exp(character_id, gained).await?;
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
}
