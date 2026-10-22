//! Stats: the character status and stat-allocation family — the 230
//! `REQUEST_CHANGESTATUSINFO` auto-food echo, the derived-maxima formulas, the
//! server-authoritative 250 `REQUEST_USEBONUS` spend, and the `cltPBasicInfo`
//! builder shared with world entry.

use gz_cipher::Cipher;
use gz_db::{CharacterFull, CharacterRepository, PgCharacterRepository, StatsUpdate};
pub(crate) use gz_proto::game::{BasicInfo, UseBonus};
use gz_proto::game::{client_map_index, decode_use_bonus, encode_person_ip};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::statusinfo::{CHANGETYPE_AUTOFOOD, ChangeStatusInfo, decode_change_status_info};
use tokio::net::TcpStream;

use crate::registry::{Registry, SessionKey};
use crate::{AnyError, LiveFrames, logln, send_raw};
/// Answers `GAMEMSG_REQUEST_CHANGESTATUSINFO` (230) with
/// `GAMEMSG_RESPONSE_CHANGESTATUSINFO` (231) for the one change type the legacy
/// handles, and stays silent for every other (`Server-Person.cpp:2831-2860`).
///
/// The client sends 230 for the horse auto-food toggle (`NCharHorse.cpp:471`,
/// `NCharStatus-Horse.cpp:232`) and applies the reply in
/// `Client-Person.cpp:1655-1680`. The legacy branches on exactly
/// `CHANGETYPE_AUTOFOOD` (`:2841`); its `default:` is a bare `return`
/// (`:2857-2858`), so an unknown change type is answered with nothing rather
/// than with a guess.
///
/// The response's `siCharUnique` is the session's character — the only one this
/// server has. The client drops a reply whose unique it cannot resolve
/// (`Client-Person.cpp:1659-1660`), so echoing the session's own unique is what
/// lets the toggle take effect.
///
/// A change type other than auto-food is the debug tier: the shipped KR client
/// heartbeats a type the 2010 enum never had (33, every 30 s — verified live
/// 2026-10-18) on the same path, so that line repeats on a timer and stays out
/// of the console unless `GAME_DEBUG_LOG` asks for it.
///
/// # Errors
///
/// A malformed body ([`gz_proto::wire::WireError`]), a `char_unique` that does
/// not fit the `SI16` field, or a socket/cipher failure.
pub(crate) async fn handle_change_status_info(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    char_unique: i32,
    payload: &[u8],
    debug_log: bool,
) -> Result<(), AnyError> {
    let change = decode_change_status_info(payload)?;
    if change.change_type != CHANGETYPE_AUTOFOOD {
        if debug_log {
            logln!(
                "game: changestatusinfo type {} -> no reply (legacy answers only AUTOFOOD; \
                 Server-Person.cpp:2841-2858)",
                change.change_type
            );
        }
        return Ok(());
    }

    let own_unique = i16::try_from(char_unique).map_err(|_| {
        format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
    })?;
    // The legacy builds a fresh change block, so the amount slots go back out as
    // zero rather than echoing whatever the request carried (`:2851-2852`).
    let response = ChangeStatusInfo {
        change_type: CHANGETYPE_AUTOFOOD,
        change_switch: change.change_switch,
        amount1: 0,
        amount2: 0,
        amount3: 0,
    };
    let body = response.encode_response(own_unique);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponseChangestatusinfo as u16,
        &body,
    )
    .await?;
    Ok(())
}

/// The accepted post-state of a valid stat allocation.
///
/// Kept separate from [`CharacterFull`] so [`allocate_stat`] is a pure function
/// over the row plus the request — the server's row on one side, the client's
/// wish on the other — with no field it can forget to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Allocation {
    pub(crate) str_: i32,
    pub(crate) dex: i32,
    pub(crate) vit: i32,
    pub(crate) mag: i32,
    pub(crate) hnd: i32,
    pub(crate) luk: i32,
    pub(crate) wis: i32,
    /// Points still unspent, after the deduction.
    pub(crate) bonus: i16,
    pub(crate) max_life: i32,
    pub(crate) max_mana: i32,
}

/// `max_life = vit * (8000 + level*50) / 1000` — the client's `SetHitPoint`
/// base (`Parametabox.cpp:3374`) **as patched** by `SPEC_CLIENT_REBUILD_PATCH.md`
/// §11 (two same-width immediates, RVA `0084C6B4` + `0084C6C2`).
///
/// This **must** equal the client's own `clPB.GetMaxLife()` because that is
/// the denominator of every HP gauge: the HUD reads `clIP.GetLife() / clPB.
/// GetMaxLife()` (`NCharPerson.cpp:3943-3958`), and under the shipped
/// `Switch_HealthAddChange = 1` (`Data\CountrySwitch.txt`) `GetMaxLife()` is
/// `GetTotalHitpoint()` = `totalVit * (8000 + level*50) / 1000`
/// (`Parametabox.cpp:7268-7282`, `:3374`) — a value the **client computes
/// itself** from the `clIP` stats it is sent. No packet can override that
/// formula (both terms are compiled into `New Era.exe`), so any server-side HP
/// above it reads as a permanently-full bar: the ratio `life / client_max`
/// clamps to 1.0 while `life >= client_max`.
///
/// The §4.3.4 reformation (approved 2026-10-20) multiplied the level term
/// tenfold (`level*50`) to grant `8.0 + 0.05×Level` HP per VIT. Against the
/// **stock** client that desynced and pinned the player's own bar full — the
/// 2026-10-21 playtest's Bug 1 (L50, 100 VIT: server 1050 vs client 825, so
/// the first 225 damage was invisible) — which is why the term was first
/// reverted to the client's `×5`. The client is now patched to `×50` too, so
/// the balance intent is shippable again.
///
/// **Coupled:** the same `level*50` term lives in
/// `gz_data::kindinfo::MonsterStats::at` (monster `max_hp`), which is a
/// different function in a different crate. Patching the client moves *every*
/// entity's denominator — `SetHitPoint` ignores its `bnpc` flag and
/// `cltCharCommon::ParametaBoxAction()` writes the member `clPB` for NPCs and
/// monsters alike (`Char-Common.cpp:4712`, `Char-Draw.cpp:4651-4653`) — so
/// flipping only this function would desync every monster bar. Both flip in
/// one release, and stored rows move via `0015_recompute_max_life.sql`.
///
/// The client's total can only ever be *larger* than this base (equipment
/// `siEquipHitpoint%`, guild VIT via `clTotalBA`), never smaller — so the bar
/// errs low, never stuck-full, when a character wears HP% gear or is in a
/// guild. Level 1 is unchanged (vit 5 -> 40, the DDL default), so level-1 rows
/// stay valid.
///
/// Spec pins: L1, 5 VIT = 40; L50, 100 VIT = 1050; L100, 100 VIT = 1300.
pub(crate) fn derived_max_life(vit: i32, level: i16) -> i32 {
    (i64::from(vit) * (8000 + i64::from(level) * 50) / 1000) as i32
}

/// `max_mana = mag * (4000 + level*5) / 1000` — the base term of
/// `SetManaPoint` (`Parametabox.cpp:3545`), reproducing the DDL default at
/// level 1 (mag 5 -> 20).
pub(crate) fn derived_max_mana(mag: i32, level: i16) -> i32 {
    (i64::from(mag) * (4000 + i64::from(level) * 5) / 1000) as i32
}

/// Cat's §4.3.8 weight capacity (approved 2026-10-20), overriding both the
/// legacy `(STR * 200) + (Level * 800)` (`Parametabox.cpp:3641`) and the
/// baseline doc's own milestone variant:
/// `TotalWeight = 10_000 + Level * 2_000 + max(STR, INT) * 50`.
/// The stronger of STR and the `mag` column (the glossary's INT) buys
/// capacity, so a pure caster is not locked out of loot the way the
/// STR-only legacy keyed it.
///
/// Spec pins: L1, stats 5 = 12_250; L100, 500 in the governing stat =
/// 235_000 (the directive's ~210k-235k band).
///
/// No wire consumer yet: the shipped client computes its own display total
/// from the item array (`GetTotalWeight`, `Char-Common.cpp:2791`), so this
/// formula governs server-side capacity checks — the P3 equip/loot gates —
/// whenever those land.
#[allow(dead_code)] // consumed when the server-side capacity gates land
#[must_use]
pub(crate) fn weight_capacity(level: i16, str_: i32, mag: i32) -> i32 {
    (i64::from(level) * 2_000 + i64::from(str_.max(mag)) * 50 + 10_000) as i32
}

/// Validates a `GAMEMSG_REQUEST_USEBONUS` (250) against the server's own row and
/// computes the accepted allocation.
///
/// This is the owner anti-cheat policy in one place: the client's `siAmount` is
/// only a wish, `character` is the server's authoritative row, and a request that
/// would break the unspent-point vault, the Hnd per-level cap, or the level
/// allowance is refused. The allowance is checked on the *post-state*
/// (`Σabilities + bonus_points <= 40 + (level-1)*5`), which is what defeats CE:
/// points cannot be conjured, and a row already over the ceiling (the unrepaired
/// level-10 seed) cannot spend either.
///
/// # Errors
///
/// A human-readable reason; the caller logs it and soft-fails. The legacy's
/// `CloseSocket()` for a mismatch (`Server-Person.cpp:2476-2493`) is deliberately
/// not ported.
pub(crate) fn allocate_stat(
    character: &CharacterFull,
    request: &UseBonus,
) -> Result<Allocation, String> {
    let pos = request.pos;
    let amount = request.amount;

    // Reset path (`siAmount < 0`, `IDC_BUTTON_*2` = 1115..1120 with no WIS2
    // button). Per `docs/rust-rewrite/ROADMAP.md` D3 this is **cut, not
    // deferred**: item 24035 is dropped entirely — no cash- or item-based
    // reset exists — and a reset may only come from the native free milestones
    // at Level 10 and Level 30, which is a different path, not this allocate
    // request. A zero or negative amount is therefore always refused here,
    // never misinterpreted as an allocation. (Was "deferred, see T-018".)
    if amount <= 0 {
        return Err(format!(
            "siAmount {amount} is not a positive allocation (the reset path is not wired)"
        ));
    }
    if !(0..=6).contains(&pos) {
        return Err(format!("siPos {pos} is outside the 0..=6 allocate space"));
    }

    if i64::from(amount) > i64::from(character.bonus_points) {
        return Err(format!(
            "requested {amount} exceeds the {} unspent point(s)",
            character.bonus_points
        ));
    }

    // Hnd is the one ability with a per-level cap (`Server-Person.cpp:2518-2529`);
    // the others have none.
    let level = i64::from(character.level);
    if pos == 4 {
        let hnd = i64::from(character.hnd);
        if hnd + i64::from(amount) > level * 2 {
            return Err(format!(
                "Hnd {hnd} + {amount} would exceed the level-{level} cap {}",
                level * 2
            ));
        }
    }

    // The ability being raised; `_` covers 6 (Wis) after the 0..=6 guard.
    let current = match pos {
        0 => character.str,
        1 => character.dex,
        2 => character.mag,
        3 => character.vit,
        4 => character.hnd,
        5 => character.luk,
        _ => character.wis,
    };
    let raised = current
        .checked_add(i32::from(amount))
        .ok_or_else(|| format!("{current} + {amount} overflows the stored SI32 ability"))?;

    let mut out = Allocation {
        str_: character.str,
        dex: character.dex,
        vit: character.vit,
        mag: character.mag,
        hnd: character.hnd,
        luk: character.luk,
        wis: character.wis,
        bonus: character.bonus_points - amount,
        max_life: 0,
        max_mana: 0,
    };
    match pos {
        0 => out.str_ = raised,
        1 => out.dex = raised,
        2 => out.mag = raised,
        3 => out.vit = raised,
        4 => out.hnd = raised,
        5 => out.luk = raised,
        _ => out.wis = raised,
    }

    // Level allowance on the post-state. An allocation moves points from bonus to
    // a stat so the total is invariant, but the check is still on the result —
    // that literal reading is what the owner asked for and what the tests lock.
    let allowance = 40 + (level - 1) * 5;
    let total = i64::from(out.bonus)
        + i64::from(out.str_)
        + i64::from(out.dex)
        + i64::from(out.vit)
        + i64::from(out.mag)
        + i64::from(out.hnd)
        + i64::from(out.luk)
        + i64::from(out.wis);
    if total > allowance {
        return Err(format!(
            "post-allocation total {total} exceeds the level-{level} allowance {allowance}"
        ));
    }

    // Derived maxima, recomputed from the post-state — `SetParametaBoxAction`'s
    // job on the legacy, done here per accepted spend.
    out.max_life = derived_max_life(out.vit, character.level);
    out.max_mana = derived_max_mana(out.mag, character.level);
    Ok(out)
}

/// Applies `GAMEMSG_REQUEST_USEBONUS` (250): spend unspent points into one
/// ability.
///
/// Server-authoritative end to end, per the owner policy: the request's numbers
/// are a wish, the accepted state comes out of the server's own row
/// ([`allocate_stat`]), is persisted, and is reflected back with a 215
/// `RESPONSE_PERSON_IP` carrying the refreshed `cltPIPInfo` — the `UPDATE_IP`
/// flush of `Char-UpdateChangedData.cpp:74-83`. A refused request is logged and
/// otherwise ignored; the session stays up, the legacy `CloseSocket()` is not
/// ported.
///
/// The row write happens before the 215 so the client can never apply a state
/// the server has not stored, and a mid-flight crash leaves the DB (the durable
/// truth) past the spend.
///
/// # Errors
///
/// A malformed body ([`gz_proto::wire::WireError`]), a database failure, or a
/// socket/cipher failure. A *rejected* request is not an error.
pub(crate) async fn handle_use_bonus(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_use_bonus(payload)?;
    let (character_id, updated) = {
        let character = live
            .character
            .as_ref()
            .ok_or("usebonus before the character row is loaded")?;
        match allocate_stat(character, &request) {
            Ok(updated) => (character.character_id, updated),
            Err(reason) => {
                logln!(
                    "game: usebonus pos {} amount {} refused: {reason}",
                    request.pos,
                    request.amount
                );
                return Ok(());
            }
        }
    };

    characters
        .update_stats(
            character_id,
            &StatsUpdate {
                str_: updated.str_,
                dex: updated.dex,
                vit: updated.vit,
                mag: updated.mag,
                hnd: updated.hnd,
                luk: updated.luk,
                wis: updated.wis,
                bonus_points: updated.bonus,
                max_life: updated.max_life,
                max_mana: updated.max_mana,
            },
        )
        .await?;

    // Mirror into the cached row so a later 200 (movement) and a 248 resync send
    // the new maxima, exactly like `handle_userorder` mirrors a new cell.
    if let Some(row) = live.character.as_mut() {
        row.str = updated.str_;
        row.dex = updated.dex;
        row.vit = updated.vit;
        row.mag = updated.mag;
        row.hnd = updated.hnd;
        row.luk = updated.luk;
        row.wis = updated.wis;
        row.bonus_points = updated.bonus;
        row.max_life = updated.max_life;
        row.max_mana = updated.max_mana;
    }

    let body = {
        let character = live
            .character
            .as_ref()
            .ok_or("usebonus row disappeared after the write")?;
        // The session's current total, not the row's max: a spend does not heal,
        // and reporting the max is Bug 2's second sender — the bar snapping to
        // full with no movement and no kill. Fail-closed, like `send_char_update`
        // and `fan_char_update`: a fallback to the max would reintroduce that bug
        // on exactly the path where the session is missing.
        let life = registry
            .info(session_key)
            .ok_or("usebonus before the session is in the registry")?
            .life as i32;
        let basic = basic_info_for(live.account_id, &live.account_name, character, life)?;
        // Re-capture the block the 214 inspect reads, so the next pull of this
        // character's stats is not stale.
        registry.set_basic_info(session_key, basic.clone());
        encode_person_ip(
            i16::try_from(live.char_unique).map_err(|_| {
                format!(
                    "session char_unique {} does not fit the SI16 siCharUnique",
                    live.char_unique
                )
            })?,
            &basic,
        )
    };
    send_raw(stream, outbound, GameMsg::ResponsePersonIp as u16, &body).await?;
    logln!(
        "game: usebonus pos {} amount {} accepted: bonus {}, life {}, mana {} (215, {} bytes)",
        request.pos,
        request.amount,
        updated.bonus,
        updated.max_life,
        updated.max_mana,
        body.len()
    );
    Ok(())
}

/// Builds the `cltPBasicInfo` + `cltPIPInfo` pair from a character row.
///
/// One source of truth for the identity-and-stats block:
/// [`LiveFrames::regenerate`] emits 248 from it and `handle_use_bonus` emits the
/// 215 `clIP` from it, so the two frames can never disagree about a stat.
///
/// `life` is the caller's **current** total, passed in because the row carries only
/// the max and the client overwrites its whole `clIP` from this frame
/// (`Client-Person-BigInfo.cpp:236-243`). A max here reports a hurt character as
/// full — Bug 2's second sender.
///
/// # Errors
///
/// A map id that does not fit the `SI16 clPos.siMapIndex`.
pub(crate) fn basic_info_for(
    account_id: i32,
    account_name: &str,
    character: &CharacterFull,
    life: i32,
) -> Result<BasicInfo, AnyError> {
    let map_index = i16::try_from(client_map_index(character.map_id)).map_err(|_| {
        format!(
            "map {} does not fit the SI16 clPos.siMapIndex",
            character.map_id
        )
    })?;
    Ok(BasicInfo {
        account_idnum: account_id,
        account_id: account_name.to_owned(),
        slot: character.slot as u8,
        sex: character.gender as u8,
        class_kind: character.class_kind as u16,
        home_village: character.school_id as i8,
        map_index,
        x: character.pos_x as i16,
        y: character.pos_y as i16,
        character_id: character.character_id,
        name: character.name.clone(),
        str_: character.str,
        dex: character.dex,
        vit: character.vit,
        mag: character.mag,
        hnd: character.hnd,
        luk: character.luk,
        wis: character.wis,
        level: character.level,
        bonus: character.bonus_points,
        life,
        // shortcut: no current MP is modelled, so this is always the max.
        mana: character.max_mana,
        exp: character.exp,
        money: character.money,
    })
}
