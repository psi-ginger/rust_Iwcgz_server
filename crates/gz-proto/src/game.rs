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
    /// Offsets are the shipped binary's, not the source's: `cltPBasicInfo` is
    /// 904 bytes so `clIP` begins at 904, which puts `siLevel` at 932 (the
    /// `%s(Lv.%d)` render reads `[&clBI + 0x3A4]` = `[clIP + 0x1C]`, `0x68F265`
    /// / `0x6905A9`) and `DelCharTime` at 968 (`[&clBI + 0x3C8]`, `0x68E15C`).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(CHAR_LIST_PAYLOAD_LEN);

        // cltPBasicInfo (904 bytes, align 8).
        w.i32(self.account_idnum); // 0..4
        w.fixed_str(&self.account_id, 20); // 4..24
        w.u8(self.slot); // 24
        w.u8(self.sex); // 25
        w.u16(self.class_kind); // 26..28
        w.pad(76 - 28); // 28..76: home village, rank, switches, clPos, school, wealth
        w.i32(self.character_id); // 76..80
        w.fixed_str(&self.name, 20); // 80..100
        w.pad(904 - 100); // 100..904: father/recommender, services, PVP, naming, attendance, empire...

        // cltPIPInfo (64 bytes, align 8) @ 904.
        w.pad(28); // 904..932: seven basic abilities
        w.i16(self.level); // 932..934
        w.i16(0); // 934: siBonus
        w.i32(self.life); // 936..940
        w.i32(self.mana); // 940..944
        w.i64(self.exp); // 944..952
        w.i16(0); // 952: siFameLevel
        w.i16(0); // 954: siSkillBonus
        w.i32(0); // 956..960: siFameExp
        w.i64(0); // 960..968: siMoney

        // DelCharTime SYSTEMTIME (16 bytes; all-zero = not delete-pending).
        w.pad(16); // 968..984

        debug_assert_eq!(w.len(), CHAR_LIST_PAYLOAD_LEN);
        w.finish()
    }
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
