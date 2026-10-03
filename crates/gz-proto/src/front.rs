//! Client-facing **front server** payload codecs (blueprint 01 Part A, 05 §S1–S6).
//!
//! Sources: `Server\FrontServer\FrontMsg.h` (structs, enum 43-115) and
//! `FrontClientProtocol.cpp` / `FrontServer.h` (send sites). All structs here
//! are declared **after** `FrontMsg.h:117`'s `#pragma pack(1)`, except the
//! world-list pieces (`GAMESERVER_BASEINFO`, `GAMESERVER_REFRESHINFO`,
//! `GAMESERVER_GAMESERVERINFO`, `WORLDINFO`, lines 6-41) which sit **above** it
//! and therefore use MSVC default (`/Zp8`) packing. The explicit `pad` calls in
//! [`WorldInfo::encode`] encode that padding; removing them makes every
//! world-list packet 7 bytes short per world.
//!
//! These are payload-only. `gz_net` owns the 5-byte `sPacketHeader`; every
//! `*_PAYLOAD_LEN` here is `sizeof(struct) - gz_net::HEADER_SIZE`.

use crate::wire::{Reader, WireError, Writer};

/// `sizeof(sResponse_SecretKey) - 5`. `FrontMsg.h:132-141`.
pub const SECRET_KEY_PAYLOAD_LEN: usize = 5;

/// `sizeof(sRequest_English_LogIn) - 5`. `FrontMsg.h:380-389`.
pub const ENGLISH_LOGIN_PAYLOAD_LEN: usize = 20 + 20 + 20 + 2048;

/// `sizeof(sResponse_LogIn) - 5`. `FrontMsg.h:419-442`.
pub const LOGIN_RESPONSE_PAYLOAD_LEN: usize = 108;

/// The notice packet's hard-coded body size: `usSize = 5 + 16`
/// (`FrontClientProtocol.cpp:760`), **not** `sizeof(text[8000])`.
pub const NOTICE_PAYLOAD_LEN: usize = 16;

/// `sizeof(WORLDINFO)` under default packing. `FrontMsg.h:36-41`.
pub const WORLDINFO_LEN: usize = 428;

/// The 30000 handshake payload. `ucTemp1/2/3` are indeterminate in the legacy
/// (`FrontServer.h:108-117` writes only the two key bytes); this port zeroes
/// them, which the client never reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretKey {
    /// The XOR key byte the client will use to encode later packets.
    pub key1: u8,
    /// The second XOR key byte.
    pub key2: u8,
}

impl SecretKey {
    /// Builds the 5-byte payload (`ucTemp1 ucTemp2 ucKey1 ucTemp3 ucKey2`).
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        vec![0, 0, self.key1, 0, self.key2]
    }
}

/// `sRequest_English_LogIn` (opcode 21), decoded from the client. `FrontMsg.h:380`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnglishLogin {
    /// `userid[20]`.
    pub userid: String,
    /// `userpw[20]`.
    pub password: String,
    /// `macaddress[20]`; unused by this port.
    pub macaddress: String,
    /// `Blackbox[2048]`, the IOVATION blob; unused by this port.
    pub blackbox: Vec<u8>,
}

impl EnglishLogin {
    /// Decodes the 2108-byte body. `FrontClientProtocol.cpp:1030` refuses a
    /// size mismatch, so callers should reject a payload of any other length
    /// before this point; a short buffer surfaces as [`WireError`].
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if the payload is shorter than the struct.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        let userid = r.fixed_str(20)?;
        let password = r.fixed_str(20)?;
        let macaddress = r.fixed_str(20)?;
        let blackbox = r.bytes(2048)?.to_vec();
        Ok(Self {
            userid,
            password,
            macaddress,
            blackbox,
        })
    }
}

/// `sResponse_LogIn` (opcode 1000). `FrontMsg.h:419-442`.
///
/// Result codes (`usResult`), from the comment at `FrontMsg.h:423`: `0` success,
/// `1` unknown id, `2` wrong password, `3` not an adult, `4` blocked, `5` DB
/// error. Negative values are the legacy's encoded failure code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResponse {
    /// `usResult`.
    pub result: u16,
    /// `siAuthKey` — the account's `idnum` (`AccountDBProcess.cpp:397`).
    pub auth_key: i32,
    /// `char userid[20]`.
    pub userid: String,
    /// `siGameMasterLevel`.
    pub gm_level: i16,
    /// `siBlockMode`.
    pub block_mode: i32,
    /// `siBlockYear/Month/Day/Hour/Minute`.
    pub block_time: [i32; 5],
    /// `szBlockReason[30]`.
    pub block_reason: String,
    /// `bAlreadyMakeCharSwitch` (a `bool`, 1 byte).
    pub already_made_char: bool,
    /// `szServerName[20]`.
    pub server_name: String,
    /// `siAccountStatus`.
    pub account_status: i32,
    /// `m_uiRealAge`.
    pub real_age: u8,
}

impl LoginResponse {
    /// Encodes the 108-byte body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(LOGIN_RESPONSE_PAYLOAD_LEN);
        w.u16(self.result);
        w.i32(self.auth_key);
        w.fixed_str(&self.userid, 20);
        w.i16(self.gm_level);
        w.i32(self.block_mode);
        for v in self.block_time {
            w.i32(v);
        }
        w.fixed_str(&self.block_reason, 30);
        w.u8(self.already_made_char as u8);
        w.fixed_str(&self.server_name, 20);
        w.i32(self.account_status);
        w.u8(self.real_age);
        debug_assert_eq!(w.len(), LOGIN_RESPONSE_PAYLOAD_LEN);
        w.finish()
    }
}

/// The notice body: 16 zero bytes. `FrontClientProtocol.cpp:758-763`.
#[must_use]
pub fn notice_payload() -> Vec<u8> {
    vec![0u8; NOTICE_PAYLOAD_LEN]
}

/// `sRequest_WorldList` (opcode 3). `FrontMsg.h:486-491`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldListRequest {
    /// `szAccountID[20]`.
    pub account_id: String,
}

impl WorldListRequest {
    /// Decodes the 20-byte body.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than 20 bytes are present.
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        Ok(Self {
            account_id: r.fixed_str(20)?,
        })
    }
}

/// One `WORLDINFO` entry, flattened across its three nested structs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldInfo {
    /// `baseinfo.worldId` / `refreshinfo.worldId`.
    pub world_id: u16,
    /// `baseinfo.worldname[20]`.
    pub world_name: String,
    /// `baseinfo.siServerID`.
    pub server_id: i32,
    /// `baseinfo.gameserverip[16]` — where the client will dial next.
    pub game_server_ip: String,
    /// `baseinfo.gameserverport`.
    pub game_server_port: u16,
    /// `baseinfo.opendate[15]`.
    pub open_date: String,
    /// `baseinfo.shortmessage[64]`.
    pub short_message: String,
    /// `refreshinfo.state` (`0` normal, `2` maintenance).
    pub state: u16,
    /// `refreshinfo.currentConnections`.
    pub current_connections: u16,
    /// `gameserverinfo.goonzuname[20]`.
    pub goonzu_name: String,
    /// `gameserverinfo.goonzutext[256]`.
    pub goonzu_text: String,
    /// `gameserverinfo.population`.
    pub population: i32,
    /// `gameserverinfo.currentPrice`.
    pub current_price: i32,
    /// `gameserverinfo.npcrate`.
    pub npc_rate: i32,
    /// `gameserverinfo.fixationmoney`.
    pub fixation_money: i32,
}

impl WorldInfo {
    /// Encodes one 428-byte entry, reproducing the default-packing padding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(WORLDINFO_LEN);

        // GAMESERVER_BASEINFO (default packing: 123 raw + 5 pad).
        w.u16(self.world_id); // 0
        w.fixed_str(&self.world_name, 20); // 2..22
        w.pad(2); // 22..24  (align siServerID to 4)
        w.i32(self.server_id); // 24..28
        w.fixed_str(&self.game_server_ip, 16); // 28..44
        w.u16(self.game_server_port); // 44..46
        w.fixed_str(&self.open_date, 15); // 46..61
        w.fixed_str(&self.short_message, 64); // 61..125
        w.pad(3); // 125..128 (align WORLDINFO's next member)

        // GAMESERVER_REFRESHINFO (6 bytes).
        w.u16(self.world_id); // 128..130
        w.u16(self.state); // 130..132
        w.u16(self.current_connections); // 132..134
        w.pad(2); // 134..136 (align gameserverinfo to 4)

        // GAMESERVER_GAMESERVERINFO (292 bytes).
        w.fixed_str(&self.goonzu_name, 20); // 136..156
        w.fixed_str(&self.goonzu_text, 256); // 156..412
        w.i32(self.population); // 412..416
        w.i32(self.current_price); // 416..420
        w.i32(self.npc_rate); // 420..424
        w.i32(self.fixation_money); // 424..428

        debug_assert_eq!(w.len(), WORLDINFO_LEN);
        w.finish()
    }
}

/// `sResponse_WorldList` (opcode 1002): `usCount` then N entries.
/// `FrontMsg.h:509-516`, builder `WorldList.cpp:215-256`.
#[must_use]
pub fn encode_world_list(worlds: &[WorldInfo]) -> Vec<u8> {
    let mut w = Writer::with_capacity(2 + worlds.len() * WORLDINFO_LEN);
    w.u16(worlds.len() as u16);
    for world in worlds {
        w.bytes(&world.encode());
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gz_net::HEADER_SIZE;

    #[test]
    fn secret_key_is_ten_bytes_on_the_wire() {
        let payload = SecretKey {
            key1: 0xAA,
            key2: 0xBB,
        }
        .encode();
        assert_eq!(payload.len(), SECRET_KEY_PAYLOAD_LEN);
        assert_eq!(payload, vec![0, 0, 0xAA, 0, 0xBB]);
        assert_eq!(payload.len() + HEADER_SIZE, 10);
    }

    #[test]
    fn english_login_is_2113_bytes_on_the_wire() {
        assert_eq!(ENGLISH_LOGIN_PAYLOAD_LEN + HEADER_SIZE, 2113);
    }

    #[test]
    fn english_login_round_trips_through_its_field_offsets() {
        let mut payload = vec![0u8; ENGLISH_LOGIN_PAYLOAD_LEN];
        payload[..3].copy_from_slice(b"bob");
        payload[20..23].copy_from_slice(b"pw!");
        payload[40..42].copy_from_slice(b"ok");
        let login = EnglishLogin::decode(&payload).unwrap();
        assert_eq!(login.userid, "bob");
        assert_eq!(login.password, "pw!");
        assert_eq!(login.macaddress, "ok");
        assert_eq!(login.blackbox.len(), 2048);
    }

    #[test]
    fn login_response_is_113_bytes_and_places_auth_key_at_offset_two() {
        let response = LoginResponse {
            result: 0,
            auth_key: 7,
            userid: "bob".into(),
            gm_level: 2,
            block_mode: 0,
            block_time: [0; 5],
            block_reason: String::new(),
            already_made_char: false,
            server_name: "Test".into(),
            account_status: 1,
            real_age: 0,
        };
        let payload = response.encode();
        assert_eq!(payload.len(), LOGIN_RESPONSE_PAYLOAD_LEN);
        assert_eq!(payload.len() + HEADER_SIZE, 113);
        assert_eq!(&payload[0..2], &[0, 0]); // result u16
        assert_eq!(&payload[2..6], &7i32.to_le_bytes()); // authKey unaligned at 2
    }

    #[test]
    fn notice_body_is_sixteen_bytes_not_eight_thousand() {
        assert_eq!(notice_payload().len(), NOTICE_PAYLOAD_LEN);
        assert_eq!(NOTICE_PAYLOAD_LEN + HEADER_SIZE, 21);
    }

    #[test]
    fn world_info_is_428_bytes_with_the_default_packing_pads() {
        let world = sample_world();
        let bytes = world.encode();
        assert_eq!(bytes.len(), WORLDINFO_LEN);
        // The two default-packing pads are the whole point of this test.
        assert_eq!(&bytes[22..24], &[0, 0]); // after worldname[20]
        assert_eq!(&bytes[125..128], &[0, 0, 0]); // after shortmessage[64]
        assert_eq!(&bytes[134..136], &[0, 0]); // before gameserverinfo
        // Field placement spot-checks.
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 7);
        assert_eq!(u16::from_le_bytes([bytes[44], bytes[45]]), 4010);
        assert_eq!(bytes[28..37], *b"127.0.0.1");
    }

    #[test]
    fn world_list_is_header_plus_count_plus_entries() {
        let payload = encode_world_list(&[sample_world(), sample_world()]);
        assert_eq!(payload.len(), 2 + 2 * WORLDINFO_LEN);
        assert_eq!(u16::from_le_bytes([payload[0], payload[1]]), 2);
        assert_eq!(payload.len() + HEADER_SIZE, 435 + WORLDINFO_LEN);
    }

    fn sample_world() -> WorldInfo {
        WorldInfo {
            world_id: 7,
            world_name: "Test".into(),
            server_id: 1,
            game_server_ip: "127.0.0.1".into(),
            game_server_port: 4010,
            open_date: "2026".into(),
            short_message: "hi".into(),
            state: 0,
            current_connections: 0,
            goonzu_name: "GoonZu".into(),
            goonzu_text: String::new(),
            population: 0,
            current_price: 0,
            npc_rate: 100,
            fixation_money: 0,
        }
    }
}
