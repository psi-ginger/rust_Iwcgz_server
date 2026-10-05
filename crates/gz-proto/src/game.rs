//! Client-facing **game server** payload codecs (blueprint 01 Part B, 05 §S8–S13).
//!
//! Sources: `common\CommonLogic\MsgType.h` (opcodes + `cltGameMsgResponse_*`),
//! `common\CommonHeader.h` (`cltLogInInfo`, `cltPBasicInfo`), `common\Char\CharCommon\Char-Common.h`
//! (`cltCharListInfo`), `common\Char\ParametaBox\parametabox.h` (`cltPIPInfo`),
//! and the send sites in `Server\Server-Login.cpp` / `ServerDB-Person.cpp`.
//!
//! **Packing:** only `cltMsg` lives inside `#pragma pack(1)` (`MsgType.h:2938`);
//! every payload struct here is declared outside it and therefore uses MSVC
//! default (`/Zp8`). The explicit `pad`/zero-fill calls reproduce that.

use crate::item::{CLT_ITEM_SIZE, Item, ItemArrayError, MAX_ITEM_PER_PERSON, encode_item_array};
use crate::wire::{Reader, WireError, Writer};

/// `sizeof(cltGameMsgResponse_ClientCharUnique)`. `MsgType.h:3013-3027`.
pub const CHAR_UNIQUE_PAYLOAD_LEN: usize = 8;

/// `sizeof(cltLogInInfo)`, including its 3 trailing pad bytes.
/// `CommonHeader.h:1442-1452`.
pub const GAME_LOGIN_PAYLOAD_LEN: usize = 48;

/// The bare `SI32` slot index of `GAMEMSG_REQUEST_SELECTCHAR`.
pub const SELECT_CHAR_PAYLOAD_LEN: usize = 4;

/// `sizeof(cltCharListInfo)`. `Char-Common.h:453-471`.
///
/// **Binary-authoritative = 984.** The shipped `New Era.exe` indexes this array
/// with stride `0x3D8` (`imul ..., 0x3D8` at `0x48D000`, `0x48D05A`, `0x68E094`,
/// `0x68FAF6`, `0x73714F`, …) and `PutCharList` (`0x48D040`) copies `0xF6`
/// dwords = 984 bytes. The source snapshot derives 832 for `cltPBasicInfo`
/// (old revision); the binary's is 904, so this supersedes the old 896/912.
pub const CHAR_LIST_PAYLOAD_LEN: usize = 984;

/// `sizeof(cltGameMsgResponse_ServerInfo)`. `MsgType-System.h:1168-1233`.
pub const SERVER_INFO_PAYLOAD_LEN: usize = 48;

/// `sizeof(cltMakeCharInfo)`, the `GAMEMSG_RESPONSE_LOGIN` (opcode 5) body.
///
/// **Binary-authoritative = 592 (`0x250`).** The shipped client's `PushChar`
/// (`0x4697A0`) copies `0x94` dwords and `PopChar` (`0x469720`) walks the buffer
/// with stride `0x250`. The source snapshot's `cltMakeCharInfo` derives 524 from
/// an older revision, so it must not be trusted here.
pub const MAKE_CHAR_INFO_PAYLOAD_LEN: usize = 592;

/// `sizeof(cltGameMsgResponse_VillageStruct)`: `SI16 VillageUnique` +
/// `SI16 siVillageStructureInfo[MAX_CONSTRUCTION_NUM]` = `2 + 10*2`.
/// `MsgType.h:3085-3104`.
pub const VILLAGE_STRUCT_PAYLOAD_LEN: usize = 22;

/// `MAX_CONSTRUCTION_NUM` (`CommonHeader.h:590`) — building slots per village.
pub const MAX_CONSTRUCTION_NUM: usize = 10;

/// `MAX_VILLAGE_NUMBER` (`CommonHeader.h:583`). The legacy server rejects a
/// village-structure request unless `1 <= ref < MAX_VILLAGE_NUMBER`
/// (`Server-System.cpp:2998`).
pub const MAX_VILLAGE_NUMBER: i16 = 100;

/// `MAKECHARTYPE_PC` (`CommonHeader.h:1893`) — `cltMakeCharInfo.uiType` for a
/// player character.
pub const MAKECHARTYPE_PC: u32 = 1;

/// `GAMEMSG_RESPONSE_CLIENTCHARUNIQUE` (opcode 1). `char.cpp:5212-5219`.
///
/// The advertised version is `CLIENT_COMPAT_VERSION = 1`, **not** the build
/// number: the bundled client quits when the server advertises a higher build
/// than itself (`char.cpp:5180`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharUnique {
    /// `siCharUnique` — this connection's session character id.
    pub char_unique: i32,
    /// `uiKey1` — session cipher key byte.
    pub key1: u8,
    /// `uiKey2` — session cipher key byte.
    pub key2: u8,
    /// `siVersion`.
    pub version: i16,
}

impl CharUnique {
    /// Encodes the 8-byte body.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut w = Writer::with_capacity(CHAR_UNIQUE_PAYLOAD_LEN);
        w.i32(self.char_unique);
        w.u8(self.key1);
        w.u8(self.key2);
        w.i16(self.version);
        debug_assert_eq!(w.len(), CHAR_UNIQUE_PAYLOAD_LEN);
        w.finish()
    }
}

/// `cltLogInInfo` (opcode 2), decoded from the client. `CommonHeader.h:1442`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLogin {
    /// `siAuthKey` — the account `idnum` handed out by the front server.
    pub auth_key: i32,
    /// `szID[20]`.
    pub userid: String,
    /// `szPassword[20]`.
    pub password: String,
    /// `uiGameMode` (`0` windowed, `1`, `2`).
    pub game_mode: u8,
}

impl GameLogin {
    /// Decodes the 48-byte body (45 meaningful bytes + 3 pad).
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 45 bytes are present.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        let auth_key = r.i32()?;
        let userid = r.fixed_str(20)?;
        let password = r.fixed_str(20)?;
        let game_mode = r.u8()?;
        Ok(Self {
            auth_key,
            userid,
            password,
            game_mode,
        })
    }
}

/// Per-slot decode of a creation request body.
///
/// The client sends a complete 984-byte `cltCharListInfo`; only the
/// identity half-half of the struct's public fields are honored here
/// because `clIP` is always re-initialized server-side (`Server-Login.cpp:260`
/// resets clIP with default values before persisting). Offsets match
/// [`CharListEntry::encode`].
///
/// # Errors
///
/// [`WireError::Truncated`] when the payload is shorter than the identity block.
pub fn decode_make_new_char(payload: &[u8]) -> Result<(String, u8, u16, u8), WireError> {
    if payload.len() < CHAR_LIST_IDENTITY_END {
        return Err(WireError::Truncated {
            have: payload.len(),
            needed: CHAR_LIST_IDENTITY_END,
        });
    }
    let slot = payload[24];
    let sex = payload[25];
    let class_kind = u16::from_le_bytes([payload[26], payload[27]]);
    let name = decode_fixed_str(&payload[80..100])?;
    Ok((name, sex, class_kind, slot))
}

/// `cltCharListInfo` identity region ends: `clBI.clPerson.szName[20]` occupies
/// [80..100), so a truncated payload cannot legitimately cut inside it.
const CHAR_LIST_IDENTITY_END: usize = 100;

fn decode_fixed_str(bytes: &[u8]) -> Result<String, WireError> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    // The client feeds CP949 names here; on free development, UTF-8 is lossless
    // for ASCII names and lossy-replaces anything else instead of severing the
    // connection.
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// The 3-byte result body shared by opcode 10 and 84
/// (`cltGameMsgResponse_DelUserChar` == `cltGameMsgResponse_PreDelUserChar`).
#[must_use]
pub fn encode_del_result(result: i8, reason: i8, slot: i8) -> Vec<u8> {
    vec![result as u8, reason as u8, slot as u8]
}

/// One `cltCharListInfo` (opcode 3), sent once per slot (15 total).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharListEntry {
    /// `clBI.siIDNum` — the owning account's `idnum`.
    pub account_idnum: i32,
    /// `clBI.szAccountID[20]`.
    pub account_id: String,
    /// `clBI.uiIndex` — the character slot.
    pub slot: u8,
    /// `clBI.uiSex` (`1` male, `0` female).
    pub sex: u8,
    /// `clBI.uiKind` — the class/avatar id.
    pub class_kind: u16,
    /// `clBI.clPerson.siPersonID` — the character's own id.
    pub character_id: i32,
    /// `clBI.clPerson.szName[20]` — the character name.
    pub name: String,
    /// `clIP.siLevel`.
    pub level: i16,
    /// `clIP.Life`.
    pub life: i32,
    /// `clIP.Mana`.
    pub mana: i32,
    /// `clIP.siExp`.
    pub exp: i64,
}

impl CharListEntry {
    /// Encodes the 984-byte body. Unset fields are zero, which the client reads
    /// as "no deletion pending" (`DelCharTime == 0`) and "no wealth/guild".
    ///
    /// The first [`BASIC_INFO_LEN`] bytes come from [`BasicInfo`], the same
    /// encoder `ALLCHARINFOONE` uses, so the two frames cannot drift apart on an
    /// offset. Offsets are the shipped binary's, not the source's: `cltPBasicInfo`
    /// is 904 bytes so `clIP` begins at 904, which puts `siLevel` at 932 (the
    /// `%s(Lv.%d)` render reads `[&clBI + 0x3A4]` = `[clIP + 0x1C]`, `0x68F265`
    /// / `0x6905A9`) and `DelCharTime` at 968 (`[&clBI + 0x3C8]`, `0x68E15C`).
    ///
    /// The seven abilities and `siBonus` are deliberately left zero rather than
    /// seeded from `cltPIPInfoCommon::Init()` (`parametabox.h:95`, which sets
    /// `str..luk = 5` but not `siHnd`/`siWis`). The char list is a summary and the
    /// client does not render abilities from it; `ALLCHARINFOONE` is where they
    /// matter, and that frame carries the real values.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = BasicInfo {
            account_idnum: self.account_idnum,
            account_id: self.account_id.clone(),
            slot: self.slot,
            sex: self.sex,
            class_kind: self.class_kind,
            home_village: 0,
            map_index: 0,
            x: 0,
            y: 0,
            character_id: self.character_id,
            name: self.name.clone(),
            str_: 0,
            dex: 0,
            vit: 0,
            mag: 0,
            hnd: 0,
            luk: 0,
            wis: 0,
            level: self.level,
            bonus: 0,
            life: self.life,
            mana: self.mana,
            exp: self.exp,
        }
        .encode();

        debug_assert_eq!(out.len(), BASIC_INFO_LEN);
        // DelCharTime SYSTEMTIME (16 bytes; all-zero = not delete-pending).
        out.resize(CHAR_LIST_PAYLOAD_LEN, 0);
        debug_assert_eq!(out.len(), CHAR_LIST_PAYLOAD_LEN);
        out
    }
}

/// `cltPBasicInfo` (904) + `cltPIPInfo` (64): the identity-and-stats block that
/// both the char list and `ALLCHARINFOONE` open with.
///
/// `cltPBasicInfo` is `CommonHeader.h:2565` and `cltPIPInfo` is
/// `Char\ParametaBox\parametabox.h`; `cltPersonInfoOne` leads with the pair in
/// that order (`PersonInfo.h:289-290`), which is why one encoder serves both.
pub const BASIC_INFO_LEN: usize = 904 + 64;

/// The `cltPBasicInfo` + `cltPIPInfo` pair.
///
/// Every field here is read by the client: `clBI` is the sole thing
/// `DoMsg_GAMEMSG_RESPONSE_ALLCHARINFOONE` writes through `SetOne`
/// (`Client-Person.cpp:774-776` -> `PersonInfo.h:589`), and `clPos` is where the
/// client reads its own coordinates back out of (`Char-Common.cpp:1206` ->
/// `Common.cpp:2917`). `clIP` is `cltPIPInfoCommon`, whose `Init()` seeds
/// `str..luk = 5` but leaves `siHnd`/`siWis` at zero (`parametabox.h:95`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicInfo {
    /// `clBI.siIDNum` — the owning account's `idnum`.
    pub account_idnum: i32,
    /// `clBI.szAccountID[20]`.
    pub account_id: String,
    /// `clBI.uiIndex` — the character slot.
    pub slot: u8,
    /// `clBI.uiSex` (`1` male, `0` female).
    pub sex: u8,
    /// `clBI.uiKind` — the class/avatar id.
    pub class_kind: u16,
    /// `clBI.siHomeVillage`.
    pub home_village: i8,
    /// `clBI.clPos.siMapIndex` — the map index the client holds, not `map_id`.
    pub map_index: i16,
    /// `clBI.clPos.siX`.
    pub x: i16,
    /// `clBI.clPos.siY`.
    pub y: i16,
    /// `clBI.clPerson.siPersonID` — the character's own id.
    pub character_id: i32,
    /// `clBI.clPerson.szName[20]`.
    pub name: String,
    /// `clIP.siStr`.
    pub str_: i32,
    /// `clIP.siDex`.
    pub dex: i32,
    /// `clIP.siVit`.
    pub vit: i32,
    /// `clIP.siMag`.
    pub mag: i32,
    /// `clIP.siHnd`.
    pub hnd: i32,
    /// `clIP.siLuk`.
    pub luk: i32,
    /// `clIP.siWis`.
    pub wis: i32,
    /// `clIP.siLevel`.
    pub level: i16,
    /// `clIP.siBonus` — unspent stat points.
    pub bonus: i16,
    /// `clIP.Life`.
    pub life: i32,
    /// `clIP.Mana`.
    pub mana: i32,
    /// `clIP.siExp`.
    pub exp: i64,
}

impl BasicInfo {
    /// Encodes the [`BASIC_INFO_LEN`]-byte block.
    ///
    /// The 904/968 boundary is the shipped binary's, not the source's — the same
    /// evidence as [`CHAR_LIST_PAYLOAD_LEN`]: `siLevel` at 932 is read as
    /// `[&clBI + 0x3A4]` = `[clIP + 0x1C]` (`0x68F265`), and both captured
    /// frames that carry this block agree on it (`siLevel == 0x22` at 932 in the
    /// char list *and* in `ALLCHARINFOONE`).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(BASIC_INFO_LEN);

        // cltPBasicInfo (904 bytes, align 8).
        w.i32(self.account_idnum); // 0..4
        w.fixed_str(&self.account_id, 20); // 4..24
        w.u8(self.slot); // 24
        w.u8(self.sex); // 25
        w.u16(self.class_kind); // 26..28
        w.i8(self.home_village); // 28
        w.pad(3); // 29..32: uiGRank, uiAngelSwitch, uiPoliceSwitch
        w.i16(self.map_index); // 32..34 clPos.siMapIndex
        w.i16(self.x); // 34..36 clPos.siX
        w.i16(self.y); // 36..38 clPos.siY
        w.i16(0); // 38..40 clPos.siReserved
        w.pad(76 - 40); // 40..76: play seconds, last main map pos, school, wealth
        w.i32(self.character_id); // 76..80
        w.fixed_str(&self.name, 20); // 80..100
        w.pad(904 - 100); // 100..904: father/recommender, services, PVP, naming, attendance

        // cltPIPInfo (64 bytes, align 8) @ 904.
        w.i32(self.str_); // 904
        w.i32(self.dex); // 908
        w.i32(self.vit); // 912
        w.i32(self.mag); // 916
        w.i32(self.hnd); // 920
        w.i32(self.luk); // 924
        w.i32(self.wis); // 928
        w.i16(self.level); // 932..934
        w.i16(self.bonus); // 934..936
        w.i32(self.life); // 936..940
        w.i32(self.mana); // 940..944
        w.i64(self.exp); // 944..952
        w.pad(BASIC_INFO_LEN - 952); // 952..968: fame, skill bonus, fame exp, money

        debug_assert_eq!(w.len(), BASIC_INFO_LEN);
        w.finish()
    }
}

/// `sizeof(cltPersonInfoOne)` — the body of `ALLCHARINFOONE` before its four
/// trailing `SI16`.
///
/// `PersonInfo.h:285-313` declares 21 sub-structs. **1.2% of the captured 9,192
/// bytes below the identity block is non-zero**, and the rest is genuine data we
/// hold no table for: a skill list, a horse named `Foal744`, and a passport blob.
/// So this is emitted as a zero-fill and the identity block carries the
/// character. See [`encode_all_char_info_one`].
const PERSON_INFO_ONE_LEN: usize = 10_160;

/// `sizeof(cltGameMsgResponse_AllCharInfoOne)` — opcode 248's body.
/// `MsgType-Person.h:1928-1947`.
pub const ALL_CHAR_INFO_ONE_LEN: usize = PERSON_INFO_ONE_LEN + 8;

/// `sizeof(cltGameMsgResponse_AllCharInfoTwo)` — opcode 249's body.
///
/// `cltPersonInfoTwo` (`PersonInfo.h:414`) is `cltPItemInfo` + `cltSummonInfo`
/// (`PersonInfo.h:420-424`), so the item array leads at offset 0.
pub const ALL_CHAR_INFO_TWO_LEN: usize = MAX_ITEM_PER_PERSON * CLT_ITEM_SIZE + 4_240;

/// Builds `GAMEMSG_RESPONSE_ALLCHARINFOONE` (248), the frame the client's
/// `SetOne` applies wholesale to its `cltCharCommon`.
///
/// # Deviation from the legacy
///
/// The 19 sub-structs after `clIP` are zero-filled rather than echoed. That is
/// not laziness: they hold the skill list, the horse, the summon roster and the
/// passport, none of which `goonzu-rs` persists yet, so zero is the honest
/// encoding of "this character has none". The reference server sends them
/// uninitialised in part — `cltPersonInfoOne`'s constructor initialises only
/// `uiGachaCnt` (`PersonInfo.h:340`), and the captured body carries a four-byte
/// `f7 59` in a region it never writes, which is the same value the
/// `PERSONITEMINFO` padding holds (`MsgType.h:3356`, `siChangedNum`+2). Zeros are
/// strictly better than that.
///
/// [`encode_all_char_info_two`] is the other half of the same
/// `REQUEST_ALLCHARINFO` burst.
#[must_use]
pub fn encode_all_char_info_one(
    basic: &BasicInfo,
    set_item_type: i16,
    set_item_step: i16,
    set_item_type2: i16,
    set_item_step2: i16,
) -> Vec<u8> {
    let mut out = basic.encode();
    out.resize(PERSON_INFO_ONE_LEN, 0);
    let mut w = Writer::with_capacity(8);
    w.i16(set_item_type);
    w.i16(set_item_step);
    w.i16(set_item_type2);
    w.i16(set_item_step2);
    out.extend_from_slice(&w.finish());
    debug_assert_eq!(out.len(), ALL_CHAR_INFO_ONE_LEN);
    out
}

/// Builds `GAMEMSG_RESPONSE_ALLCHARINFOTWO` (249): the whole 80-slot item array,
/// then `cltSummonInfo` zero-filled.
///
/// `items` is indexed by slot; a shorter slice leaves the remaining slots empty,
/// which is how the client's `GetTotalWeight()` (`Char-Common.cpp:2791`) sees a
/// partially-filled inventory. `clSummonInfo` is zero for the reason given on
/// [`encode_all_char_info_one`].
///
/// # Errors
///
/// [`ItemArrayError`] if `items` is longer than [`MAX_ITEM_PER_PERSON`].
pub fn encode_all_char_info_two(items: &[Item]) -> Result<Vec<u8>, ItemArrayError> {
    let mut out = vec![0u8; ALL_CHAR_INFO_TWO_LEN];
    encode_item_array(&mut out, items)?;
    Ok(out)
}

/// `GAMEMSG_REQUEST_SELECTCHAR` (opcode 4): a bare `SI32` slot index.
/// `NSelectChar.cpp:1353`, handler `Server-Login.cpp:184-187`.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than 4 bytes are present.
pub fn decode_select_char(payload: &[u8]) -> Result<i32, WireError> {
    Reader::new(payload).i32()
}

/// `cltGameMsgResponse_ServerInfo` (opcode 106). `MsgType-System.h:1168-1233`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// `siServiceArea`.
    pub service_area: i16,
    /// `siServerUnique`.
    pub server_unique: i16,
    /// `bItemMallUseSwitch`.
    pub item_mall_use: bool,
    /// `bVillageWarSwitch`.
    pub village_war: bool,
    /// `bBonusTimeSwitch`.
    pub bonus_time: bool,
    /// `bBonus2TimeSwitch`.
    pub bonus2_time: bool,
    /// `bSpecialQuestBonusTimeSwitch`.
    pub special_quest_bonus_time: bool,
    /// `bBroadcastSwitch`.
    pub broadcast: bool,
    /// `siBestVillageLevel`.
    pub best_village_level: i8,
    /// `siNPCPriceRate`.
    pub npc_price_rate: i32,
    /// `siItemPriceIndex`.
    pub item_price_index: i64,
    /// `bEventSwitch[8]`.
    pub event_switch: [bool; 8],
    /// `siMinimumTeacherLevel`.
    pub minimum_teacher_level: i32,
    /// `siServiceAge`.
    pub service_age: i16,
    /// `siEnablePVP`.
    pub enable_pvp: i8,
    /// `bAutoUnblockMode`.
    pub auto_unblock_mode: bool,
    /// `bVoteCampaign`.
    pub vote_campaign: bool,
    /// `bEventToggleSwitch[2]`.
    pub event_toggle_switch: [bool; 2],
}

impl ServerInfo {
    /// Encodes the 48-byte body. `Server.cpp:6470-6487`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(SERVER_INFO_PAYLOAD_LEN);
        w.i16(self.service_area); // 0..2
        w.i16(self.server_unique); // 2..4
        w.u8(self.item_mall_use as u8); // 4
        w.u8(self.village_war as u8); // 5
        w.u8(self.bonus_time as u8); // 6
        w.u8(self.bonus2_time as u8); // 7
        w.u8(self.special_quest_bonus_time as u8); // 8
        w.u8(self.broadcast as u8); // 9
        w.i8(self.best_village_level); // 10
        w.pad(1); // 11: align siNPCPriceRate to 4
        w.i32(self.npc_price_rate); // 12..16
        w.i64(self.item_price_index); // 16..24
        for v in self.event_switch {
            w.u8(v as u8); // 24..32
        }
        w.i32(self.minimum_teacher_level); // 32..36
        w.i16(self.service_age); // 36..38
        w.i8(self.enable_pvp); // 38
        w.u8(self.auto_unblock_mode as u8); // 39
        w.u8(self.vote_campaign as u8); // 40
        for v in self.event_toggle_switch {
            w.u8(v as u8); // 41..43
        }
        w.pad(5); // 43..48: trailing struct pad
        debug_assert_eq!(w.len(), SERVER_INFO_PAYLOAD_LEN);
        w.finish()
    }
}

/// `cltMakeCharInfo` (opcode 5 `GAMEMSG_RESPONSE_LOGIN`) — the full character
/// record that makes the client create its avatar and enter the world.
///
/// **Field offsets are the shipped binary's, not the source snapshot's.** The
/// struct is 592 bytes; `PushChar` (`0x4697A0`) copies `0x94` dwords and
/// `PopChar` (`0x469720`) strides `0x250`. Confirmed offsets:
///
/// | offset | field |
/// |--------|-------|
/// | `0x00` | `uiType:1` (bit 0), `uiMakeCharKind:16` (bits 1–16) |
/// | `0x04` | `uiMakeCharUnique:16`, `uiLevel:8` (bits 16–23) |
/// | `0x08` | `uiLife:20` (bits 0–19), `uiX:10` (bits 20–29) |
/// | `0x0C` | `uiY:10` (bits 0–9), `uiMapIndex:10` (bits 10–19), `uiTotalMoveSpeed:6` (bits 20–25) |
/// | `0x10` | `siHomeVillageUnique` |
/// | `0x12` | `siJimCharUnique` |
/// | `0x50` | `siCurrentShowHorse` |
/// | `0x51` | `siGameMasterLevel` |
/// | `0x52` | `siMapUnique` |
/// | `0x54` | `clShapeEquip` |
/// | `0x84` | `clRank` |
/// | `0x92` | `siMakeAniType` |
/// | `0x94` | `clGuildRank` |
///
/// Unset fields are zero. All-zero `clShapeEquip` means "nothing equipped"; the
/// avatar appearance comes from `uiMakeCharKind` (the character's class kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakeCharInfo {
    /// `uiMakeCharUnique` — must equal the session `char_unique` the client got
    /// in opcode 1, and be `< MAX_SETCHAR_RESERVE_NUMBER` (the client indexes
    /// `CharUniqueInfo[uiMakeCharUnique]`).
    pub make_char_unique: u16,
    /// `uiMakeCharKind` — the class/kind id (same as the char-list `uiKind`).
    pub make_char_kind: u16,
    /// `uiLevel`.
    pub level: u8,
    /// `uiLife` — current life (20 bits).
    pub life: u32,
    /// `uiX` — map tile x (10 bits).
    pub x: u16,
    /// `uiY` — map tile y (10 bits).
    pub y: u16,
    /// `uiMapIndex` — the `MapData` index the client loads (10 bits). Must
    /// match the client's `clUnitMapInfo` row or `SetCharReservedChar` skips it.
    ///
    /// Build this with [`client_map_index`] rather than from `map_id` directly.
    pub map_index: u16,
    /// `uiTotalMoveSpeed` — base movement speed (6 bits).
    pub total_move_speed: u8,
    /// `siHomeVillageUnique`.
    pub home_village_unique: u8,
    /// `siGameMasterLevel`.
    pub game_master_level: u8,
    /// `siMapUnique` — 0 lets the client resolve it from its own `MapData`
    /// (`MapManager.cpp:958`).
    pub map_unique: i16,
    /// `siMakeAniType` — personal make-item animation type.
    pub make_ani_type: i16,
}

/// The `uiMapIndex` this client will be holding for a character on `map_id`.
///
/// `MakeCharInfo` packs the field into 10 bits and `Client-Login.cpp:245` copies
/// it straight into `cltMapManager::CurrentMapIndex`. That makes it a value the
/// *server* has to agree with on every frame that names a map to this client —
/// most sharply `GAMEMSG_RESPONSE_ITEMDROP` (309), whose handler returns without
/// drawing anything if `clPos.siMapIndex != CurrentMapIndex`
/// (`Client-SystemNPC.cpp:2280`).
///
/// Both the login frame and the 309 encoder go through here so the two cannot
/// drift apart silently.
#[must_use]
pub const fn client_map_index(map_id: i32) -> u16 {
    (map_id as u32 & 0x3FF) as u16
}

impl MakeCharInfo {
    /// Encodes the 592-byte body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(MAKE_CHAR_INFO_PAYLOAD_LEN);

        // 0x00: uiType:1 | uiMakeCharKind:16 (declared order = low bits first).
        let type_kind = (MAKECHARTYPE_PC & 0x1) | ((u32::from(self.make_char_kind) & 0xFFFF) << 1);
        w.u32(type_kind);

        // 0x04: uiMakeCharUnique:16 | uiLevel:8.
        w.u16(self.make_char_unique);
        w.u8(self.level);
        w.pad(1); // 0x07

        // 0x08: uiLife:20 | uiX:10.
        let life_x = (self.life & 0xF_FFFF) | ((u32::from(self.x) & 0x3FF) << 20);
        w.u32(life_x);

        // 0x0C: uiY:10 | uiMapIndex:10 | uiTotalMoveSpeed:6.
        let y_map_speed = (u32::from(self.y) & 0x3FF)
            | ((u32::from(self.map_index) & 0x3FF) << 10)
            | ((u32::from(self.total_move_speed) & 0x3F) << 20);
        w.u32(y_map_speed);

        w.u8(self.home_village_unique); // 0x10
        w.pad(1); // 0x11
        w.u16(0); // 0x12: siJimCharUnique
        // 0x14: `cltOrderDataInfo clOrder` — `siOrder` + `siHostMode`. Leaving
        // this zero hits the `default:` case in `cltCommonLogic::SetChar`, which
        // not only fires a debug MsgBox but also skips `OrderStop`, leaving the
        // char's order struct uninitialized; the client's frame loop then
        // dereferences a null pointer (APPCRASH at RVA 0x921fa).
        // Legacy sends `clOrder.Make(ORDER_STOP=2, BY_PLAYER=0, ...)`.
        w.u8(2); // 0x14: siOrder = ORDER_STOP
        w.u8(0); // 0x15: siHostMode = BY_PLAYER
        w.pad(0x50 - 0x16); // 0x16..0x50: clOrder.params/dwPushedTime + tail

        w.u8(0); // 0x50: siCurrentShowHorse
        w.u8(self.game_master_level); // 0x51
        w.i16(self.map_unique); // 0x52
        w.pad(0x92 - 0x54); // 0x54..0x92: clShapeEquip + clRank

        w.i16(self.make_ani_type); // 0x92
        w.pad(MAKE_CHAR_INFO_PAYLOAD_LEN - 0x94); // 0x94..0x250: clGuildRank + tail

        debug_assert_eq!(w.len(), MAKE_CHAR_INFO_PAYLOAD_LEN);
        w.finish()
    }
}

/// `cltGameMsgResponse_VillageStruct` (opcode 1102). Sent in answer to
/// `GAMEMSG_REQUEST_VILLAGESTRUCT` (1101) so the client stops waiting
/// (`SetCharReservedChar` refuses to pop while `bWaitVillageStructureInfoSwitch`
/// is set, `Client-SetChar.cpp:66`). All-zero structure info renders an empty
/// village. `MsgType.h:3085-3104`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VillageStruct {
    /// `VillageUnique` — the requested village ref.
    pub village_unique: i16,
    /// `siVillageStructureInfo[MAX_CONSTRUCTION_NUM]`.
    pub structure_info: [i16; MAX_CONSTRUCTION_NUM],
}

impl VillageStruct {
    /// Encodes the 22-byte body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(VILLAGE_STRUCT_PAYLOAD_LEN);
        w.i16(self.village_unique);
        for slot in self.structure_info {
            w.i16(slot);
        }
        debug_assert_eq!(w.len(), VILLAGE_STRUCT_PAYLOAD_LEN);
        w.finish()
    }
}

/// `GAMEMSG_REQUEST_VILLAGESTRUCT` (opcode 1101) — a bare `SI32` village ref
/// (`Server-System.cpp:2997` reads `siData32[0]`).
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than 4 bytes are present.
pub fn decode_village_struct_request(payload: &[u8]) -> Result<i32, WireError> {
    Reader::new(payload).i32()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gz_net::HEADER_SIZE;

    #[test]
    fn char_unique_is_13_bytes_on_the_wire() {
        let payload = CharUnique {
            char_unique: 42,
            key1: 0x11,
            key2: 0x22,
            version: 1,
        }
        .encode();
        assert_eq!(payload.len(), CHAR_UNIQUE_PAYLOAD_LEN);
        assert_eq!(payload.len() + HEADER_SIZE, 13);
        assert_eq!(&payload[..4], &42i32.to_le_bytes());
        assert_eq!(&payload[4..8], &[0x11, 0x22, 0x01, 0x00]);
    }

    #[test]
    fn game_login_is_48_bytes_of_which_45_are_read() {
        let mut payload = vec![0u8; GAME_LOGIN_PAYLOAD_LEN];
        payload[..4].copy_from_slice(&9i32.to_le_bytes());
        payload[4..7].copy_from_slice(b"bob");
        payload[24..26].copy_from_slice(b"pw");
        payload[44] = 2;
        let login = GameLogin::decode(&payload).unwrap();
        assert_eq!(login.auth_key, 9);
        assert_eq!(login.userid, "bob");
        assert_eq!(login.password, "pw");
        assert_eq!(login.game_mode, 2);
        assert_eq!(GAME_LOGIN_PAYLOAD_LEN + HEADER_SIZE, 53);
    }

    #[test]
    fn char_list_entry_is_984_bytes_with_name_and_level_placed() {
        let entry = sample_entry();
        let bytes = entry.encode();
        assert_eq!(bytes.len(), CHAR_LIST_PAYLOAD_LEN);
        assert_eq!(bytes.len() + HEADER_SIZE, 989);
        assert_eq!(&bytes[4..13], b"test_acct");
        assert_eq!(bytes[24], 3); // slot
        assert_eq!(u16::from_le_bytes([bytes[26], bytes[27]]), 7); // kind
        assert_eq!(&bytes[80..91], b"Hero Person");
        assert_eq!(i16::from_le_bytes([bytes[932], bytes[933]]), 12); // level
        assert_eq!(
            i32::from_le_bytes([bytes[936], bytes[937], bytes[938], bytes[939]]),
            40
        );
        assert_eq!(
            i64::from_le_bytes(bytes[944..952].try_into().unwrap()),
            1234
        );
    }

    #[test]
    fn select_char_is_a_bare_i32() {
        assert_eq!(decode_select_char(&7i32.to_le_bytes()).unwrap(), 7);
        assert_eq!(SELECT_CHAR_PAYLOAD_LEN + HEADER_SIZE, 9);
    }

    #[test]
    fn server_info_is_53_bytes_on_the_wire() {
        let info = ServerInfo {
            service_area: 1,
            server_unique: 7,
            item_mall_use: true,
            village_war: false,
            bonus_time: false,
            bonus2_time: false,
            special_quest_bonus_time: false,
            broadcast: true,
            best_village_level: 5,
            npc_price_rate: 100,
            item_price_index: 1000,
            event_switch: [false; 8],
            minimum_teacher_level: 1,
            service_age: 1,
            enable_pvp: 1,
            auto_unblock_mode: false,
            vote_campaign: false,
            event_toggle_switch: [false; 2],
        };
        let payload = info.encode();
        assert_eq!(payload.len(), SERVER_INFO_PAYLOAD_LEN);
        assert_eq!(payload.len() + HEADER_SIZE, 53);
        assert_eq!(
            i32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]),
            100
        );
        assert_eq!(
            i64::from_le_bytes(payload[16..24].try_into().unwrap()),
            1000
        );
    }

    #[test]
    fn make_char_info_places_binary_confirmed_offsets() {
        let info = MakeCharInfo {
            make_char_unique: 1234,
            make_char_kind: 1,
            level: 10,
            life: 40,
            x: 25,
            y: 13,
            map_index: 1,
            total_move_speed: 8,
            home_village_unique: 0,
            game_master_level: 0,
            map_unique: 0,
            make_ani_type: 0,
        };
        let payload = info.encode();
        assert_eq!(payload.len(), MAKE_CHAR_INFO_PAYLOAD_LEN);
        assert_eq!(payload.len() + HEADER_SIZE, 597);

        // 0x00: uiType=1 (bit0) | uiMakeCharKind=1 (bits1-16).
        let type_kind = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        assert_eq!(type_kind & 0x1, MAKECHARTYPE_PC);
        assert_eq!(type_kind >> 1, 1);

        // 0x04: uiMakeCharUnique:16 | uiLevel:8.
        assert_eq!(u16::from_le_bytes([payload[4], payload[5]]), 1234);
        assert_eq!(payload[6], 10);

        // 0x08: uiLife:20 | uiX:10.
        let life_x = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        assert_eq!(life_x & 0xF_FFFF, 40);
        assert_eq!(life_x >> 20, 25);

        // 0x0C: uiY:10 | uiMapIndex:10 | uiTotalMoveSpeed:6.
        let y_map_speed = u32::from_le_bytes(payload[12..16].try_into().unwrap());
        assert_eq!(y_map_speed & 0x3FF, 13);
        assert_eq!((y_map_speed >> 10) & 0x3FF, 1);
        assert_eq!((y_map_speed >> 20) & 0x3F, 8);
    }

    #[test]
    fn village_struct_is_27_bytes_on_the_wire() {
        let body = VillageStruct {
            village_unique: 1,
            structure_info: [0; MAX_CONSTRUCTION_NUM],
        }
        .encode();
        assert_eq!(body.len(), VILLAGE_STRUCT_PAYLOAD_LEN);
        assert_eq!(body.len() + HEADER_SIZE, 27);
        assert_eq!(i16::from_le_bytes([body[0], body[1]]), 1);
        assert_eq!(
            decode_village_struct_request(&3i32.to_le_bytes()).unwrap(),
            3
        );
    }

    fn sample_entry() -> CharListEntry {
        CharListEntry {
            account_idnum: 1,
            account_id: "test_acct".into(),
            slot: 3,
            sex: 0,
            class_kind: 7,
            character_id: 55,
            name: "Hero Person".into(),
            level: 12,
            life: 40,
            mana: 20,
            exp: 1234,
        }
    }
}
