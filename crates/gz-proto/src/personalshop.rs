//! The personal-shop request/response pair: `GAMEMSG_REQUEST_PERSONALSHOPDATA`
//! (7002) → `GAMEMSG_RESPONSE_PERSONALSHOPDATA` (7003).
//!
//! Opening another character's personal shop (or the manufacture dialog) is a
//! two-message round trip. The client sends a 12-byte request and creates the
//! dialog **only** from the reply (`Client-PersonalShop.cpp:76-142`), so an
//! unanswered 7002 leaves the button dead — which is exactly what the log
//! showed before this module existed.
//!
//! # The request — 7002
//!
//! `cltGameMsgRequest_PersonalShopData` (`MsgType-PersonalShop.h:37-49`) is
//! `SI16 siCharUnique`, `BOOL bManufacture`, `BOOL bMaker`. `BOOL` is a 4-byte
//! `int`, so `/Zp8` inserts two pad bytes after the `SI16`:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siCharUnique` |
//! | `2` | 2 | *(pad)* |
//! | `4` | 4 | `bManufacture` |
//! | `8` | 4 | `bMaker` |
//!
//! The body is **12 bytes**, which the shipped client confirmed live: the
//! triage log recorded `unhandled opcode 7002` on exactly that length. That is
//! also the anchor for the packing of every struct below — under `/Zp1` the
//! same request would be 10 bytes.
//!
//! # The response — 7003
//!
//! `cltGameMsgResponse_PersonalShopData` (`MsgType-PersonalShop.h:51-61`) is
//! just `SI16 siCharUnique` (echoed from the request) and a `cltShopTradeData`
//! (`Char-Common.h:284-317`):
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 2 | `siCharUnique` |
//! | `2` | 6 | *(pad to the 8-byte `cltShopTradeData` alignment)* |
//! | `8` | | `clShopData` |
//!
//! `cltShopTradeData`'s own layout, with `sizeof(cltItem) = 84`
//! ([`crate::item::CLT_ITEM_SIZE`], binary-authoritative) and `GMONEY` an
//! 8-aligned `__int64`:
//!
//! | offset | size | field |
//! |---|---|---|
//! | `0` | 1 | `m_siShopMode` |
//! | `4` | 4 | `m_siPersonID` |
//! | `8` | 20 | `m_szAccountID[MAX_PLAYER_NAME]` |
//! | `28` | 420 | `m_clItem[MAX_SHOP_ITEM_NUMBER]` (5 × 84) |
//! | `448` | 10 | `m_siSelectedInventoryPos[5]` |
//! | `464` | 40 | `m_siPrice[5]` (6 pad after the `SI16`s) |
//! | `504` | 40 | `m_siCurrentPrice[5]` |
//! | `544` | 2 | `m_siHandPoint` |
//! | `546` | 2 | `m_siSkillLevel` |
//! | `548` | 21 | `m_szShopTitle[MAX_SHOP_TITLE_LENGTH]` |
//! | `569` | 1 | `m_bPremium` |
//! | `572` | 84 | `m_clSpacialItem` (2 pad) |
//! | `656` | 2 | `m_siSelectedSpacialInventoryPos` |
//! | `664` | | *(struct total; 8-aligned)* |
//!
//! The `_SAFE_MEMORY` variant of the header swaps each array for an
//! `NSafeTArray<T, N>`, which is a bare `T m_Array[N]` member plus bounds checks
//! (`NArray.h:123-166`) — the same bytes, so the layout is packing-identical.
//!
//! `sizeof(cltGameMsgResponse_PersonalShopData)` is therefore **672 bytes**.
//!
//! # What is verified and what is not
//!
//! The **offsets the client reads to open the dialog** — `m_siShopMode` at body
//! `8`, `m_siPersonID` at `12`, `m_szAccountID` at `16`
//! (`Client-PersonalShop.cpp:94,117,131` pass all three to `Show`) — do not
//! depend on the packing question, because the `SI16`+pad at the head is
//! identical under `/Zp4` and `/Zp8`.
//!
//! The **total length** does: the `GMONEY` arrays make `cltShopTradeData` 664
//! bytes under `/Zp8` and 656 under `/Zp4`. `/Zp8` is the established project
//! default (`crate::wire` documents it, and the only `/Zp1` in the tree is the
//! unrelated `NConvert` tool), so 672 is used. The reference capture carries
//! **no 7003 frame** — the reference player never opened a shop — so unlike the
//! 84-byte item record this length is derived rather than pinned. It is a
//! deliberate, recorded unknown: confirm it against a live client, and if the
//! client's struct is the `/Zp4` size the tail must shrink to 664.

use crate::wire::{Reader, WireError};

/// `MAX_SHOP_ITEM_NUMBER` (`Char-Common.h:242`) — the length of
/// `cltShopTradeData::m_clItem`.
pub const SHOP_ITEM_NUMBER: usize = 5;

/// `MAX_SHOP_TITLE_LENGTH` (`Char-Common.h:244`).
pub const SHOP_TITLE_LENGTH: usize = 21;

/// `MAX_PLAYER_NAME` (`CommonHeader.h:575`) — the length of
/// `cltShopTradeData::m_szAccountID`.
pub const PLAYER_NAME_LENGTH: usize = 20;

/// `PSHOP_MODE_NONE` (`Char-Common.h:249`).
pub const PSHOP_MODE_NONE: i8 = 0;
/// `PSHOP_MODE_BUY` (`Char-Common.h:250`).
pub const PSHOP_MODE_BUY: i8 = 1;
/// `PSHOP_MODE_SELL` (`Char-Common.h:251`).
pub const PSHOP_MODE_SELL: i8 = 2;
/// `PSHOP_MODE_MAKEITEM` (`Char-Common.h:252`).
pub const PSHOP_MODE_MAKEITEM: i8 = 3;
/// `PSHOP_MODE_MAKEITEM_WANT` (`Char-Common.h:253`).
pub const PSHOP_MODE_MAKEITEM_WANT: i8 = 4;

/// `sizeof(cltGameMsgRequest_PersonalShopData)` — the 7002 body.
pub const REQUEST_PERSONALSHOPDATA_SIZE: usize = 12;

/// `sizeof(cltShopTradeData)` under `/Zp8`.
pub const CLT_SHOP_TRADE_DATA_SIZE: usize = 664;

/// `sizeof(cltGameMsgResponse_PersonalShopData)` — the 7003 body.
pub const RESPONSE_PERSONALSHOPDATA_SIZE: usize = 8 + CLT_SHOP_TRADE_DATA_SIZE;

/// `cltGameMsgRequest_PersonalShopData`, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalShopRequest {
    /// `siCharUnique` — the character whose shop is being opened.
    pub char_unique: i16,
    /// `bManufacture` — `TRUE` opens the manufacture flow rather than the shop.
    pub manufacture: bool,
    /// `bMaker` — with `bManufacture`, chooses *want-to-make* over *make*.
    pub maker: bool,
}

/// Decodes the 7002 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than
/// [`REQUEST_PERSONALSHOPDATA_SIZE`] bytes are supplied.
pub fn decode_personal_shop_request(payload: &[u8]) -> Result<PersonalShopRequest, WireError> {
    let mut r = Reader::new(payload);
    let char_unique = r.i16()?;
    // `/Zp8` put two pad bytes here; the reader skips them by reading the next
    // `SI32` at the right position.
    let _pad = r.u16()?;
    let manufacture = r.i32()? != 0;
    let maker = r.i32()? != 0;
    Ok(PersonalShopRequest {
        char_unique,
        manufacture,
        maker,
    })
}

/// The shop mode the legacy picks for a character whose shop is **not** open.
///
/// `Server-PersonalShop.cpp:54-68`: `bManufacture == FALSE` is an ordinary
/// seller shop; otherwise `bMaker` picks "wanted to make" versus "will make".
#[must_use]
pub fn shop_mode_for_request(manufacture: bool, maker: bool) -> i8 {
    if !manufacture {
        PSHOP_MODE_SELL
    } else if maker {
        PSHOP_MODE_MAKEITEM_WANT
    } else {
        PSHOP_MODE_MAKEITEM
    }
}

/// The dialog-opening fields of a `cltShopTradeData`.
///
/// An empty shop still carries a mode, the owner's person id and account name;
/// every item/price/skill slot is left zero, which is what the legacy's
/// `clShopData.Init()` produces for a shop that was never opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopTradeData {
    /// `m_siShopMode` — one of the `PSHOP_MODE_*` values.
    pub shop_mode: i8,
    /// `m_siPersonID` — the owner's `clPerson.siPersonID`.
    pub person_id: i32,
    /// `m_szAccountID` — the owner's account name.
    pub account_id: String,
}

impl ShopTradeData {
    /// Encodes the 7003 body, echoing `char_unique` into the response's
    /// `siCharUnique`.
    #[must_use]
    pub fn encode_response(&self, char_unique: i16) -> [u8; RESPONSE_PERSONALSHOPDATA_SIZE] {
        let mut out = [0u8; RESPONSE_PERSONALSHOPDATA_SIZE];
        out[0..2].copy_from_slice(&char_unique.to_le_bytes());
        // 2..8: pad to cltShopTradeData's 8-byte alignment.
        out[OFFSET_SHOP_MODE] = self.shop_mode as u8;
        // 9..12: pad to m_siPersonID's 4-byte alignment.
        out[OFFSET_PERSON_ID..OFFSET_PERSON_ID + 4].copy_from_slice(&self.person_id.to_le_bytes());
        // `StringCchCopy(..., MAX_PLAYER_NAME, ...)` copies at most 19 bytes and
        // always writes a terminator, so the 20th byte stays zero.
        let bytes = self.account_id.as_bytes();
        let take = bytes.len().min(PLAYER_NAME_LENGTH - 1);
        out[OFFSET_ACCOUNT_ID..OFFSET_ACCOUNT_ID + take].copy_from_slice(&bytes[..take]);
        out
    }
}

/// `m_siShopMode` inside the 7003 body (`clShopData` starts at 8).
const OFFSET_SHOP_MODE: usize = 8;
/// `m_siPersonID` inside the 7003 body.
const OFFSET_PERSON_ID: usize = 12;
/// `m_szAccountID` inside the 7003 body.
const OFFSET_ACCOUNT_ID: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::CLT_ITEM_SIZE;

    /// Rounds `off` up to `align`, as MSVC `/Zp8` does between fields.
    fn align_to(off: usize, align: usize) -> usize {
        off.div_ceil(align) * align
    }

    /// Walks `cltShopTradeData` field by field and checks each offset against
    /// the module table, ending on the 664/672 totals.
    #[test]
    fn the_trade_data_layout_sums_to_the_zp8_size() {
        let mut off = 0usize;

        off = align_to(off, 1); // m_siShopMode (SI08)
        assert_eq!(off, 0);
        off += 1;

        off = align_to(off, 4); // m_siPersonID (SI32)
        assert_eq!(off, 4);
        off += 4;

        off = align_to(off, 1); // m_szAccountID (TCHAR[20])
        assert_eq!(off, 8);
        off += PLAYER_NAME_LENGTH;

        off = align_to(off, 4); // m_clItem (cltItem[5], align 4)
        assert_eq!(off, 28);
        off += SHOP_ITEM_NUMBER * CLT_ITEM_SIZE;

        off = align_to(off, 2); // m_siSelectedInventoryPos (SI16[5])
        assert_eq!(off, 448);
        off += 2 * SHOP_ITEM_NUMBER;

        off = align_to(off, 8); // m_siPrice (GMONEY[5], align 8)
        assert_eq!(off, 464);
        off += 8 * SHOP_ITEM_NUMBER;

        off = align_to(off, 8); // m_siCurrentPrice (GMONEY[5])
        assert_eq!(off, 504);
        off += 8 * SHOP_ITEM_NUMBER;

        off = align_to(off, 2); // m_siHandPoint + m_siSkillLevel
        assert_eq!(off, 544);
        off += 2 + 2;

        off = align_to(off, 1); // m_szShopTitle (TCHAR[21])
        assert_eq!(off, 548);
        off += SHOP_TITLE_LENGTH;
        assert_eq!(off, 569);

        off += 1; // m_bPremium (bool)
        off = align_to(off, 4); // m_clSpacialItem (cltItem, align 4)
        assert_eq!(off, 572);
        off += CLT_ITEM_SIZE;
        assert_eq!(off, 656);

        off += 2; // m_siSelectedSpacialInventoryPos (SI16)
        off = align_to(off, 8); // struct alignment = max member alignment
        assert_eq!(off, CLT_SHOP_TRADE_DATA_SIZE);
        assert_eq!(CLT_SHOP_TRADE_DATA_SIZE, 664);
        assert_eq!(RESPONSE_PERSONALSHOPDATA_SIZE, 672);
    }

    #[test]
    fn request_is_twelve_bytes_and_reads_both_flags() {
        let body = [
            0xBF, 0x05, // siCharUnique = 1471
            0x00, 0x00, // /Zp8 padding
            0x01, 0x00, 0x00, 0x00, // bManufacture = TRUE
            0x01, 0x00, 0x00, 0x00, // bMaker = TRUE
        ];
        assert_eq!(body.len(), REQUEST_PERSONALSHOPDATA_SIZE);
        assert_eq!(
            decode_personal_shop_request(&body).unwrap(),
            PersonalShopRequest {
                char_unique: 1471,
                manufacture: true,
                maker: true,
            }
        );
        assert!(matches!(
            decode_personal_shop_request(&body[..11]),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn shop_mode_matches_the_legacy_branch() {
        assert_eq!(shop_mode_for_request(false, false), PSHOP_MODE_SELL);
        assert_eq!(shop_mode_for_request(true, false), PSHOP_MODE_MAKEITEM);
        assert_eq!(shop_mode_for_request(true, true), PSHOP_MODE_MAKEITEM_WANT);
    }

    #[test]
    fn the_response_opens_the_dialog_at_the_expected_offsets() {
        let shop = ShopTradeData {
            shop_mode: PSHOP_MODE_SELL,
            person_id: 7,
            account_id: "Tester".to_owned(),
        };
        let body = shop.encode_response(0x05BF);
        assert_eq!(body.len(), RESPONSE_PERSONALSHOPDATA_SIZE);
        assert_eq!(&body[0..2], &0x05BFu16.to_le_bytes());
        assert_eq!(body[2..8], [0u8; 6], "pad to clShopData");
        assert_eq!(body[OFFSET_SHOP_MODE], PSHOP_MODE_SELL as u8);
        assert_eq!(body[9..12], [0u8; 3], "pad to m_siPersonID");
        assert_eq!(
            &body[OFFSET_PERSON_ID..OFFSET_PERSON_ID + 4],
            &7i32.to_le_bytes()
        );
        assert_eq!(&body[OFFSET_ACCOUNT_ID..OFFSET_ACCOUNT_ID + 6], b"Tester");
        // Everything past the account name is an empty shop.
        assert!(
            body[OFFSET_ACCOUNT_ID + PLAYER_NAME_LENGTH..]
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn the_account_name_is_nul_padded_and_never_overruns_the_field() {
        let shop = ShopTradeData {
            shop_mode: PSHOP_MODE_MAKEITEM,
            person_id: 1,
            account_id: "ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_owned(),
        };
        let body = shop.encode_response(1);
        // `StringCchCopy` keeps a terminator, so only 19 bytes go in.
        assert_eq!(
            &body[OFFSET_ACCOUNT_ID..OFFSET_ACCOUNT_ID + 19],
            b"ABCDEFGHIJKLMNOPQRS"
        );
        assert_eq!(body[OFFSET_ACCOUNT_ID + 19], 0);
    }
}
