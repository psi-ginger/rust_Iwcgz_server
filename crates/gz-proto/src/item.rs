//! Item records and the person's item array.
//!
//! # Sizes
//!
//! `sizeof(cltItem)` is **84 bytes, binary-authoritative.** The shipped server
//! sends `GAMEMSG_RESPONSE_ALLCHARINFOTWO` (249) as a 10960-byte body whose
//! first `80 * 84 = 6720` bytes are 80 fixed-size records with identical field
//! structure: records at offsets 0 and 84 in the live capture differ only in
//! `siUnique`, `siItemNum` and three payload dwords. `80` is
//! `MAX_ITEM_PER_PERSON` (`ItemUnit.h:326`) and the array is `cltPItemInfo`'s
//! `clItem[MAX_ITEM_PER_PERSON]` (`ItemUnit.h:1755`).
//!
//! The in-tree `cltItem` (`ItemUnit.h:1281`) is a later, much larger revision
//! (it grew a `cltCrystalItem` array and a bigger union), so its field offsets
//! are **not** trustworthy here — only the first two fields are confirmed by the
//! capture:
//!
//! | offset | size | field | evidence |
//! |---|---|---|---|
//! | `0x00` | `u16` | `siUnique` — item-table row id | values 1..23901 across the capture's occupied slots |
//! | `0x02` | `u16` | `siItemNum` — stack count | `1` on every distinct equipped item |
//!
//! Everything past `0x04` is kept verbatim in [`Item::raw`] rather than guessed.
//! Reconstructing those fields is the job of the `RESPONSE_ALLCHARINFOTWO`
//! encoder, and until then round-tripping the raw bytes is lossless.

use crate::wire::{Reader, WireError};

/// `sizeof(cltItem)` in the shipped wire format — see the module docs.
pub const CLT_ITEM_SIZE: usize = 84;

/// `cltItem.uiDateYear` / `.uiDateMonth` / `.uiDateDay` — three bytes at record
/// offset 4, right after `siUnique` and `siItemNum` (`ItemUnit.h:1196-1201`).
///
/// *Sealed* is exactly "all three zero" (`cltItem::IsSealingUp`,
/// `ItemUnit.cpp:3463-3467`).
pub const ITEM_SEAL_DATE_OFFSET: usize = 4;

/// `cltItem.uiDateUseDay` — the validity window in days. A `UI16` the compiler
/// aligns to offset 8 rather than packing at 7.
///
/// Pinned from the dev characters' own records: the necklaces in slots 32-34 carry
/// `14 00` there (`ItemUnit.h:1201`, filled by the lottery at
/// `Server-Item.cpp:3844-3855`).
pub const ITEM_USE_DAYS_OFFSET: usize = 8;

/// `MAX_ITEM_PER_PERSON` (`ItemUnit.h:326`) — the length of `cltPItemInfo::clItem`.
pub const MAX_ITEM_PER_PERSON: usize = 80;

/// The byte length of the item array at the head of
/// `GAMEMSG_RESPONSE_ALLCHARINFOTWO`.
pub const ITEM_ARRAY_LEN: usize = CLT_ITEM_SIZE * MAX_ITEM_PER_PERSON;

/// Slot-range anchors and sizes.
///
/// # Provenance
///
/// The four *base* indices below are quoted straight from `ItemUnit.h:388-402`
/// (`PERSONITEM_INV0 15`, `PERSONITEM_HORSE0 35`, `PERSONITEM_BAG0 55`,
/// `PERSONITEM_BAG1 67`). The *lengths* are **not** in the header — it declares
/// six bag slots (`PERSONITEM_INV0..INV5`) and no horse or sub-bag lengths at
/// all. That declaration is wrong for the shipped binary: the four bases only
/// form a consistent partition if the bag holds 20 slots and each sub-bag 12
/// (`15 + 20 = 35`, `35 + 20 = 55`, `55 + 12 = 67`).
///
/// The 20-slot bag is confirmed against the wire independently of the header:
/// the live capture's own `GAMEMSG_REQUEST_ITEMMOVE` pairs are `24 -> 20` and
/// `28 -> 24`, i.e. two adjacent 4-column bag cells three and four rows down,
/// which only fit a 4-wide grid rooted at `INV0`.
///
/// So the layout below is binary-derived, not header-derived. Treat it the same
/// way as [`CLT_ITEM_SIZE`]: the header is advisory.
///
/// | range | container | size | shape |
/// |---|---|---|---|
/// | `0..=14` | equipment | 15 | — |
/// | `15..=34` | main bag | 20 | 4 x 5 |
/// | `35..=54` | horse | 20 | 4 x 5 |
/// | `55..=66` | extra bag 1 | 12 | 4 x 3 |
/// | `67..=78` | extra bag 2 | 12 | 4 x 3 |
/// | `79` | unused | 1 | array is 80 long |
///
/// `PERSONITEM_NONE` is `25` (`ItemUnit.h:394`), which lands *inside* the main
/// bag. It is a per-slot sentinel meaning "no item", not a range boundary, so it
/// does not affect the partition above.
pub mod personitem {
    /// `PERSONITEM_HELMET`.
    pub const HELMET: u16 = 0;
    /// `PERSONITEM_ARMOUR`.
    pub const ARMOUR: u16 = 1;
    /// `PERSONITEM_WEAPON1`.
    pub const WEAPON1: u16 = 2;
    /// `PERSONITEM_NECK`.
    pub const NECK: u16 = 3;
    /// `PERSONITEM_RING1`.
    pub const RING1: u16 = 4;
    /// `PERSONITEM_RING2`.
    pub const RING2: u16 = 5;
    /// `PERSONITEM_BELT`.
    pub const BELT: u16 = 6;
    /// `PERSONITEM_SHOES`.
    pub const SHOES: u16 = 7;
    /// `PERSONITEM_WEAPON2`.
    pub const WEAPON2: u16 = 8;
    /// `PERSONITEM_MANTLE`.
    pub const MANTLE: u16 = 9;
    /// `PERSONITEM_BAG`.
    pub const BAG: u16 = 10;
    /// `PERSONITEM_HAT`.
    pub const HAT: u16 = 11;
    /// `PERSONITEM_DRESS`.
    pub const DRESS: u16 = 12;
    /// `PERSONITEM_BAG_SUB`.
    pub const BAG_SUB: u16 = 13;
    /// `PERSONITEM_RESERVED1` — the last equipment slot.
    pub const RESERVED1: u16 = 14;

    /// `PERSONITEM_INV0` — first main-bag slot.
    pub const INV0: u16 = 15;
    /// Last main-bag slot (20 slots, 4 x 5).
    pub const INV_END: u16 = 34;

    /// `PERSONITEM_HORSE0` — first horse slot.
    pub const HORSE0: u16 = 35;
    /// Last horse slot (20 slots, 4 x 5).
    pub const HORSE_END: u16 = 54;

    /// `PERSONITEM_BAG0` — first extra-bag-1 slot.
    pub const BAG0: u16 = 55;
    /// Last extra-bag-1 slot (12 slots, 4 x 3).
    pub const BAG0_END: u16 = 66;

    /// `PERSONITEM_BAG1` — first extra-bag-2 slot.
    pub const BAG1: u16 = 67;
    /// Last extra-bag-2 slot (12 slots, 4 x 3).
    pub const BAG1_END: u16 = 78;
}

/// The base index of the container an absolute slot belongs to.
///
/// The ranges are the binary-derived partition documented on
/// [`personitem`]. A slot outside every range keeps its own value, so an
/// unexpected index stays visible instead of being folded into a neighbour.
#[must_use]
pub fn container_of(slot: u16) -> u16 {
    match slot {
        0..=personitem::RESERVED1 => personitem::HELMET,
        personitem::INV0..=personitem::INV_END => personitem::INV0,
        personitem::HORSE0..=personitem::HORSE_END => personitem::HORSE0,
        personitem::BAG0..=personitem::BAG0_END => personitem::BAG0,
        personitem::BAG1..=personitem::BAG1_END => personitem::BAG1,
        other => other,
    }
}

/// A single `clItem`: the 84 wire bytes, plus the two fields we have confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The complete record, byte for byte.
    pub raw: [u8; CLT_ITEM_SIZE],
}

impl Item {
    /// An empty slot (`cltItem::Init` is a `ZeroMemory`).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            raw: [0; CLT_ITEM_SIZE],
        }
    }

    /// Builds a record carrying only `siUnique` and `siItemNum`.
    ///
    /// The remaining fields stay zero, which is what the capture shows for the
    /// occupied bag slots of a freshly-seeded character.
    #[must_use]
    pub fn from_unique(unique: u16, count: u16) -> Self {
        let mut item = Self::empty();
        item.raw[0..2].copy_from_slice(&unique.to_le_bytes());
        item.raw[2..4].copy_from_slice(&count.to_le_bytes());
        item
    }

    /// Rebuilds a record from a stored slot's bytes.
    ///
    /// The repositories keep a slot's whole `cltItem` as `Vec<u8>`
    /// (`gz_db::item::CharacterItem::raw`); this is the way back to a wire
    /// [`Item`]. A wrong-length slice is a caller bug rather than a recoverable
    /// state, so anything short is padded out to an empty record and anything
    /// long is truncated — never a panic on the path that grants a drop.
    #[must_use]
    pub fn from_raw(raw: &[u8]) -> Self {
        let mut item = Self::empty();
        let n = raw.len().min(CLT_ITEM_SIZE);
        item.raw[..n].copy_from_slice(&raw[..n]);
        item
    }

    /// `cltItem.siUnique` — the item-table row id. `0` means the slot is empty.
    #[must_use]
    pub fn unique(&self) -> u16 {
        u16::from_le_bytes([self.raw[0], self.raw[1]])
    }

    /// `cltItem.siItemNum` — the stack count.
    #[must_use]
    pub fn count(&self) -> u16 {
        u16::from_le_bytes([self.raw[2], self.raw[3]])
    }

    /// Whether the slot holds nothing. The legacy treats `siUnique == 0` as the
    /// emptiness test everywhere (`ItemPerson.cpp:27`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unique() == 0
    }

    /// Overwrites `siItemNum`, leaving every other byte alone.
    pub fn set_count(&mut self, count: u16) {
        self.raw[2..4].copy_from_slice(&count.to_le_bytes());
    }

    /// `cltItem.uiDateYear` / `.uiDateMonth` / `.uiDateDay`, as `(y-2000, m, d)`.
    ///
    /// The three bytes sit at record offset 4, straight after `siUnique` and
    /// `siItemNum` (`ItemUnit.h:1196-1201`), and the dev characters' own records pin
    /// that: the slot holding item 705 reads `c1 02 01 00 1a 09 17 …`, i.e. 26/9/23.
    #[must_use]
    pub fn seal_date(&self) -> (u8, u8, u8) {
        (
            self.raw[ITEM_SEAL_DATE_OFFSET],
            self.raw[ITEM_SEAL_DATE_OFFSET + 1],
            self.raw[ITEM_SEAL_DATE_OFFSET + 2],
        )
    }

    /// Whether the slot is **sealed**.
    ///
    /// Exactly "the date trio is zero": `cltItem::IsSealingUp`
    /// (`ItemUnit.cpp:3463-3467`) returns true on `uiDateYear == 0 && uiDateMonth ==
    /// 0 && uiDateDay == 0`, and a freshly-built record starts zeroed — which is why
    /// an item a quest hands out arrives sealed until the player unseals it.
    #[must_use]
    pub fn is_sealed(&self) -> bool {
        self.seal_date() == (0, 0, 0)
    }

    /// Unseals by stamping the date — the write the legacy's sealing-up path makes
    /// (`Server-Item.cpp:3793-3795`, from its `SYSTEMTIME`, year relative to 2000).
    pub fn set_seal_date(&mut self, year: u8, month: u8, day: u8) {
        self.raw[ITEM_SEAL_DATE_OFFSET] = year;
        self.raw[ITEM_SEAL_DATE_OFFSET + 1] = month;
        self.raw[ITEM_SEAL_DATE_OFFSET + 2] = day;
    }

    /// `cltItem.uiDateUseDay` — the validity window in days.
    ///
    /// A `UI16` the compiler aligns to offset 8 rather than packing at 7
    /// (`ItemUnit.h:1201`); the dev characters' necklaces carry `14 00` there.
    #[must_use]
    pub fn use_days(&self) -> u16 {
        u16::from_le_bytes([
            self.raw[ITEM_USE_DAYS_OFFSET],
            self.raw[ITEM_USE_DAYS_OFFSET + 1],
        ])
    }

    /// Reads one record.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than [`CLT_ITEM_SIZE`] bytes remain.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut raw = [0u8; CLT_ITEM_SIZE];
        raw.copy_from_slice(Reader::new(bytes).bytes(CLT_ITEM_SIZE)?);
        Ok(Self { raw })
    }

    /// The record's wire bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; CLT_ITEM_SIZE] {
        self.raw
    }
}

/// Reads the 80-record item array that opens `RESPONSE_ALLCHARINFOTWO` (249).
///
/// The array is the first member of `cltPersonInfoTwo` (`PersonInfo.h:414`); the
/// remaining 4240 bytes of that 10960-byte body are other person fields this
/// module does not decode.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than [`ITEM_ARRAY_LEN`].
pub fn decode_item_array(payload: &[u8]) -> Result<Vec<Item>, WireError> {
    let array = Reader::new(payload).bytes(ITEM_ARRAY_LEN)?;
    (0..MAX_ITEM_PER_PERSON)
        .map(|i| Item::decode(&array[i * CLT_ITEM_SIZE..]))
        .collect()
}

/// Why an item array could not be written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemArrayError {
    /// The caller passed a different number of records than the array holds.
    SlotCount {
        /// How many records were supplied.
        got: usize,
    },
    /// The destination buffer is shorter than the array.
    TooShort {
        /// The buffer's length.
        have: usize,
    },
}

impl std::fmt::Display for ItemArrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlotCount { got } => {
                write!(f, "expected {MAX_ITEM_PER_PERSON} items, got {got}")
            }
            Self::TooShort { have } => {
                write!(f, "buffer of {have} bytes is shorter than {ITEM_ARRAY_LEN}")
            }
        }
    }
}

impl std::error::Error for ItemArrayError {}

/// Writes `items` back into the head of a `RESPONSE_ALLCHARINFOTWO` body,
/// leaving the trailing person fields untouched. This is how the replayed
/// capture frame becomes PostgreSQL-driven without disturbing the rest of it.
///
/// # Errors
///
/// [`ItemArrayError::SlotCount`] unless `items` is exactly
/// [`MAX_ITEM_PER_PERSON`] long, or [`ItemArrayError::TooShort`] if `payload` is
/// shorter than [`ITEM_ARRAY_LEN`].
pub fn encode_item_array(payload: &mut [u8], items: &[Item]) -> Result<(), ItemArrayError> {
    if items.len() != MAX_ITEM_PER_PERSON {
        return Err(ItemArrayError::SlotCount { got: items.len() });
    }
    if payload.len() < ITEM_ARRAY_LEN {
        return Err(ItemArrayError::TooShort {
            have: payload.len(),
        });
    }
    for (i, item) in items.iter().enumerate() {
        payload[i * CLT_ITEM_SIZE..(i + 1) * CLT_ITEM_SIZE].copy_from_slice(&item.raw);
    }
    Ok(())
}

/// `cltMoveItemInfo` — the `GAMEMSG_REQUEST_ITEMMOVE` (303) body.
///
/// Two `UI16`s and nothing else (`cltItem.h:458-475`). The client packs the
/// pair into one `SI32` first (`MouseOperation.cpp:2241-2242`: `LOWORD` is the
/// source, `HIWORD` the destination) but sends the struct itself.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than 4 bytes.
pub fn decode_move_item(payload: &[u8]) -> Result<(u16, u16), WireError> {
    let mut r = Reader::new(payload);
    let from = r.u16()?;
    let to = r.u16()?;
    Ok((from, to))
}

/// `cltSplitItemInfo` — the `GAMEMSG_REQUEST_ITEMSPLIT` (341) body.
///
/// Two `UI16`s then a signed count of how many items to move out of the source
/// stack (`cltItem.h:437-455`; `siCount` is `SI32`). The client only sends this
/// when the destination is empty, so a merge destination never arises in
/// practice — but like the legacy we accept one, for the same reason the legacy
/// does: a merge is just a partial move with a shared-unique destination.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than 8 bytes.
pub fn decode_split_item(payload: &[u8]) -> Result<(u16, u16, i32), WireError> {
    let mut r = Reader::new(payload);
    let from = r.u16()?;
    let to = r.u16()?;
    let count = r.i32()?;
    Ok((from, to, count))
}

/// The `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210) `siOption` values this server
/// emits. The header defines 45 of them (`MsgType.h:3320-3354`); the port uses
/// two — `NONE` for a bare slot write, `SHOW` for an item *arriving*, which is
/// the only difference between "keep the bag in step" and "tell the player they
/// got something".
pub mod personiteminfo {
    /// `PERSONITEMINFO_OPTION_NONE` — a bare slot update, no extra meaning.
    pub const OPTION_NONE: u16 = 0;

    /// `PERSONITEMINFO_OPTION_SHOW` — "display the item on screen"
    /// (`MsgType.h:3321`).
    ///
    /// The client's `siOption` switch (`Client-Person-BigInfo.cpp:1054-1096`)
    /// sends this case two places, and **both only fire while `siChangedNum` is
    /// non-zero** (`:1073`):
    ///
    /// - `CNChatBarDlg::ItemGetChatRecv(itemname, itemCnt, price)`
    ///   (`NChatBarDlg.cpp:950`) renders the loot line from text resource 2394
    ///   — *"Obtained &lt;name&gt; x&lt;count&gt;"* — in `CHATEDIT_COLOR_GREEN`;
    /// - `CNShowGetItemDlg::SetItemData` raises the item-acquisition popup
    ///   (`DLG_NShowGetItem.ddf`).
    ///
    /// So one 7210 carries both the slot write and the loot notice. Sending
    /// `NONE` with a zero count gets neither.
    pub const OPTION_SHOW: u16 = 1;
}

/// Size of `cltGameMsgResponse_PersonItemInfo` (`MsgType.h:3356`) on the wire.
///
/// The struct is `SI16 siItemPos`, `SI16 siOption`, `SI16 siChangedNum`, 2 bytes
/// of `/Zp8` padding, `SI32 siExp`, `cltItem clItem`, `GMONEY siPrice`. `GMONEY`
/// is `__int64` (`NDataTypes.h:19`), so the record is `12 + 84 + 8`.
///
/// The capture contains exactly one 7210 frame and it is **104 bytes**, which
/// fixes both the struct size and [`CLT_ITEM_SIZE`] a third time.
pub const PERSON_ITEM_INFO_SIZE: usize = 12 + CLT_ITEM_SIZE + 8;

/// The `GAMEMSG_REQUEST_INVDROP` (307) body — `cltGameMsgRequest_InvDrop`
/// (`MsgType-Item.h:66`).
///
/// `SI32 siItemPos` followed by `cltItem clItem`, i.e. `4 + 84 = 88` bytes. The
/// live capture's 307 is 88 bytes, matching.
///
/// Unlike [`decode_move_item`] the client does **not** pack the position here:
/// `MouseOperation.cpp:1997` passes `si32lParam` straight through as the single
/// `SI32` slot, with no `LOWORD`/`HIWORD` split.
///
/// The slot is returned as the raw `SI32` so the caller can apply the same range
/// test the legacy does (`Server-Item.cpp:2462`) rather than having the decoder
/// silently clamp it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvDropRequest {
    /// `siItemPos` — the absolute slot the client is dropping from.
    pub slot: i32,
    /// `clItem` — the client's own copy of the record it wants removed.
    pub item: Item,
}

/// Reads a 307 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than
/// `4 + CLT_ITEM_SIZE`.
pub fn decode_inv_drop(payload: &[u8]) -> Result<InvDropRequest, WireError> {
    let mut r = Reader::new(payload);
    let slot = r.i32()?;
    let item = Item::decode(r.bytes(CLT_ITEM_SIZE)?)?;
    Ok(InvDropRequest { slot, item })
}

/// `GAMEMSG_REQUEST_INVPICKUP` (308) body — `cltGameMsgRequest_InvPickUp`
/// (`MsgType-Item.h:389`).
///
/// Five `SI16` — `siItemUnique`, `siItemIndex`, `siMapIndex`, `siX`, `siY` — so
/// `10` bytes with no padding.
///
/// Every field is a *claim* about which ground object the player clicked, and the
/// legacy checks all of them against its own record before believing any
/// (`Server-Item.cpp:2358-2370`): the handle has to resolve to a live item, and
/// then the item's unique, map and both coordinates have to match too. None of it
/// is redundant. The client fills the fields straight out of the per-cell pair it
/// stored when [`GroundItem`] arrived (`MouseOperation.cpp:5316` passes
/// `GetItemUnique(), GetItemIndex(), CurrentMapIndex, GetItemX(), GetItemY()`), so
/// in practice they agree — but they are five separately-checked numbers on the
/// wire, and validating the tuple rather than trusting the handle is what the
/// legacy does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvPickupRequest {
    /// `siItemUnique` — `cltItem.siUnique` of the ground item.
    pub item_unique: i16,
    /// `siItemIndex` — the ground handle, the `UI16` that 309 carried as
    /// `uiItemListIndex` and the client stored per cell. Opaque to the client, so
    /// only the server interprets it; it comes back signed.
    pub item_index: i16,
    /// `siMapIndex` — the map the client believes it is on, i.e. its
    /// `CurrentMapIndex`.
    pub map_index: i16,
    /// `siX` — map cell column.
    pub x: i16,
    /// `siY` — map cell row.
    pub y: i16,
}

/// Exclusive upper bound on `cltItem.siUnique` — `MAX_ITEM_UNIQUE`
/// (`cltItem.h:99`).
///
/// The legacy range-checks a pickup against it before looking anything up
/// (`Server-Item.cpp:2358`), so a request outside the range is rejected without
/// touching the item list.
pub const MAX_ITEM_UNIQUE: i32 = 30000;

/// Reads a 308 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than [`INV_PICKUP_SIZE`].
/// The 7223 body — `cltGameMsgRequest_Inv_CancellationSealingUp`
/// (`MsgType.h:3531-3541`): the slot to unseal, plus the record the client believes
/// is in it.
///
/// 88 bytes: `SI32 siItemPos` then the 84-byte `clItem`. The reply struct
/// (`MsgType.h:3543-3553`) has the same shape, but the server never sends it — see
/// [`decode_inv_cancellationsealingup`]'s neighbour `encode_person_item_info`.
pub const REQUEST_INV_CANCELLATIONSEALINGUP_SIZE: usize = 4 + CLT_ITEM_SIZE;

/// `GAMEMSG_REQUEST_INV_CANCELLATIONSEALINGUP` (7223) — "unseal this slot".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvCancellationSealingUp {
    /// `siItemPos` — an absolute slot of the character's item array.
    pub slot: i32,
    /// `clItem` — the client's copy.
    ///
    /// The server does **not** trust it: the legacy reads the live array by
    /// `siItemPos` (`Server-Item.cpp:3781`), and so does this port. It is decoded so
    /// the frame parses whole, and a future reader can compare the two.
    pub item: Item,
}

/// Reads a 7223 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if fewer than
/// [`REQUEST_INV_CANCELLATIONSEALINGUP_SIZE`] bytes are available.
pub fn decode_inv_cancellationsealingup(
    payload: &[u8],
) -> Result<InvCancellationSealingUp, WireError> {
    let mut r = Reader::new(payload);
    Ok(InvCancellationSealingUp {
        slot: r.i32()?,
        item: Item::decode(r.bytes(CLT_ITEM_SIZE)?)?,
    })
}

/// The server's answer to a 7223: `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210) carrying
/// the slot's new record.
///
/// The sealing-up family *has* a `GAMEMSG_RESPONSE_INV_CANCELLATIONSEALINGUP`
/// (7224) in the opcode table, but the legacy never sends it on this path — it calls
/// `cltGameMsgResponse_PersonItemInfo` instead (`Server-Item.cpp:3868-3873`), and the
/// shipped client has no 7224 handler anywhere. So the reply is
/// [`encode_person_item_info`].
///
/// # Errors
///
/// [`WireError`] only if `payload` is short, which the fixed-size callers cannot
/// hit.
pub fn decode_inv_pickup(payload: &[u8]) -> Result<InvPickupRequest, WireError> {
    let mut r = Reader::new(payload);
    Ok(InvPickupRequest {
        item_unique: r.i16()?,
        item_index: r.i16()?,
        map_index: r.i16()?,
        x: r.i16()?,
        y: r.i16()?,
    })
}

/// Builds a `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210) body for one slot.
///
/// This is the frame the client actually uses to redraw its inventory: its
/// handler writes `clCharItem.clItem[siItemPos].Set(&clItem)` and then refreshes
/// every open inventory dialog (`Client-Person-BigInfo.cpp:616`). `siOption`,
/// `siChangedNum`, `siExp` and `siPrice` are always zero here, matching both the
/// drop path (`ServerDB-Item.cpp:2833`) and the capture's single frame.
#[must_use]
pub fn encode_person_item_info(slot: u16, item: &Item) -> [u8; PERSON_ITEM_INFO_SIZE] {
    person_item_info(slot, personiteminfo::OPTION_NONE, 0, item)
}

/// The 7210 for an item *arriving* in the bag — the same 104 bytes as
/// [`encode_person_item_info`], but carrying [`personiteminfo::OPTION_SHOW`]
/// and the count in `siChangedNum`.
///
/// Those two fields are the whole difference between "keep the client's copy of
/// the slot in step" and "tell the player they got something". The client's
/// `siOption` switch renders the loot line and the acquisition popup only for
/// that case, and only while `siChangedNum` is non-zero
/// (`Client-Person-BigInfo.cpp:1054-1093`). See [`personiteminfo::OPTION_SHOW`]
/// for what the player ends up seeing.
#[must_use]
pub fn encode_person_item_gained(
    slot: u16,
    count: i32,
    item: &Item,
) -> [u8; PERSON_ITEM_INFO_SIZE] {
    // `siChangedNum` is `SI16` (`MsgType.h:3361`). A count past `i16::MAX` means
    // the caller is broken rather than that the bag holds a fortune, so clamp —
    // a truncated negative would be read by the client as "say nothing at all".
    person_item_info(
        slot,
        personiteminfo::OPTION_SHOW,
        i16::try_from(count).unwrap_or(i16::MAX),
        item,
    )
}

fn person_item_info(
    slot: u16,
    option: u16,
    changed_num: i16,
    item: &Item,
) -> [u8; PERSON_ITEM_INFO_SIZE] {
    let mut out = [0u8; PERSON_ITEM_INFO_SIZE];
    out[0..2].copy_from_slice(&slot.to_le_bytes());
    out[2..4].copy_from_slice(&option.to_le_bytes());
    out[4..6].copy_from_slice(&changed_num.to_le_bytes());
    // 6..8 is the /Zp8 pad and 8..12 is siExp; both stay zero.
    out[12..12 + CLT_ITEM_SIZE].copy_from_slice(&item.raw);
    // 96..104 siPrice stays zero.
    out
}

/// Size of `cltGameMsgResponse_ItemDrop` (`MsgType-Item.h:413`) on the wire.
///
/// The struct is `cltCharPos clPos`, `UI16 uiItemUnique`, `UI16 uiItemListIndex`.
/// `cltCharPos` (`CommonHeader.h:1096`) is four `SI16` and `UI16` has alignment 2,
/// so the record is `8 + 2 + 2 = 12` with no padding. Here the size follows from
/// the field types rather than from a declared `sizeof`, which matters because
/// **no frame in the capture carries opcode 309** — there is no sample to check
/// it against. What the capture-free evidence does fix is the field order: the
/// client's reader takes `clPos.siMapIndex`, `clPos.siX`, `clPos.siY`,
/// `uiItemUnique` then `uiItemListIndex`, and the four `SI16` of `clPos` are
/// declared in that order.
/// Size of `cltGameMsgRequest_InvPickUp` (`MsgType-Item.h:389`) on the wire.
///
/// Five `SI16`, so `10` bytes with no padding — the layout follows from the field
/// types, as with [`ITEM_DROP_SIZE`]. The capture contains no 308 frame, so there
/// is again no recorded sample; what pins the field *order* is the client's send
/// site (`MouseOperation.cpp:5316`), which fills the five arguments in exactly
/// this sequence, and the legacy's reads in the same sequence.
pub const INV_PICKUP_SIZE: usize = 10;

pub const ITEM_DROP_SIZE: usize = 12;

/// `uiItemUnique` value that means "this cell is now empty", not "an item whose
/// unique happens to be zero".
///
/// The legacy clears a cell by sending the item's coordinates with both handles
/// zeroed (`ServerMap.cpp:241`, `// 지워진 아이템이다` — "this is a deleted
/// item"), reusing the same frame. `siUnique` is unsigned on the wire but signed
/// in the record, so zero is free to repurpose as a sentinel.
pub const ITEM_DROP_CLEAR_UNIQUE: u16 = 0;

/// `uiItemListIndex` the legacy sends alongside [`ITEM_DROP_CLEAR_UNIQUE`].
pub const ITEM_DROP_CLEAR_LIST_INDEX: u16 = 0;

/// `GAMEMSG_RESPONSE_ITEMDROP` (309): put one item on the map, or clear a cell.
///
/// This is the frame that makes a dropped item visible, and it is the whole of
/// what the client does with it (`Client-SystemNPC.cpp:2275`):
///
/// ```text
/// if(pclinfo->clPos.siMapIndex != pclMapManager->CurrentMapIndex) return;
/// if(pclMapManager->pclMapArray[mapindex] == NULL) return;
/// pclMapArray[mapindex]->SetItemMap(x, y, pclinfo->uiItemUnique, pclinfo->uiItemListIndex);
/// ```
///
/// `SetItemMap` (`map.cpp:289`) is a bounds-checked per-cell write into the
/// client's *own* map grid, so the map layer lives in the client, not here:
/// emitting this frame needs no map model on the server.
///
/// Three consequences worth stating, all of them silent failures in the client:
///
/// - `map_index` must equal the client's `CurrentMapIndex`, which it took from
///   `RESPONSE_LOGIN`'s `uiMapIndex` (`Client-Login.cpp:245`). Build it with
///   [`crate::game::client_map_index`] so it cannot drift from the login frame.
/// - `x`/`y` are bounds-checked against the client's `MH.MapXsize`/`MapYsize`,
///   which we have no model of. A cell outside them drops the frame silently, so
///   the caller should log what it sent.
/// - The frame carries no stack count. `SetItemMap` stores only the unique pair,
///   so the client cannot know a dropped stack was 16 rather than 1.
///
/// `list_index` is opaque to the client: it stores it per cell and hands it back
/// in `GAMEMSG_REQUEST_INVPICKUP` (308) as `siItemIndex`. Its value is therefore
/// ours to choose, and only has to round-trip through `UI16`/`SI16` unchanged and
/// stay unique among live ground objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroundItem {
    /// `clPos.siMapIndex` — must equal the client's `CurrentMapIndex`.
    pub map_index: i16,
    /// `clPos.siX` — map cell column.
    pub x: i16,
    /// `clPos.siY` — map cell row.
    pub y: i16,
    /// `uiItemUnique` — `cltItem.siUnique` of the item on the ground, or
    /// [`ITEM_DROP_CLEAR_UNIQUE`] to clear the cell.
    pub item_unique: u16,
    /// `uiItemListIndex` — the server's handle for this ground object.
    pub list_index: u16,
}

impl GroundItem {
    /// The frame that clears `map_index`/`x`/`y` of a ground object.
    #[must_use]
    pub fn removal(map_index: i16, x: i16, y: i16) -> Self {
        Self {
            map_index,
            x,
            y,
            item_unique: ITEM_DROP_CLEAR_UNIQUE,
            list_index: ITEM_DROP_CLEAR_LIST_INDEX,
        }
    }

    /// Whether this frame clears a cell rather than placing an item.
    #[must_use]
    pub fn is_removal(&self) -> bool {
        self.item_unique == ITEM_DROP_CLEAR_UNIQUE
    }

    /// Encodes the 12-byte body.
    #[must_use]
    pub fn encode(&self) -> [u8; ITEM_DROP_SIZE] {
        let mut out = [0u8; ITEM_DROP_SIZE];
        // `clPos` is four SI16; `cltCharPos` never initialises `siReserved` and the
        // client never reads it, so it goes out zero rather than as garbage.
        out[0..2].copy_from_slice(&self.map_index.to_le_bytes());
        out[2..4].copy_from_slice(&self.x.to_le_bytes());
        out[4..6].copy_from_slice(&self.y.to_le_bytes());
        // 6..8 siReserved stays zero.
        out[8..10].copy_from_slice(&self.item_unique.to_le_bytes());
        out[10..12].copy_from_slice(&self.list_index.to_le_bytes());
        out
    }
}

/// Reads a 309 body.
///
/// # Errors
///
/// [`WireError::Truncated`] if the body is shorter than [`ITEM_DROP_SIZE`].
pub fn decode_item_drop(payload: &[u8]) -> Result<GroundItem, WireError> {
    let mut r = Reader::new(payload);
    let map_index = r.i16()?;
    let x = r.i16()?;
    let y = r.i16()?;
    let _si_reserved = r.i16()?;
    let item_unique = r.u16()?;
    let list_index = r.u16()?;
    Ok(GroundItem {
        map_index,
        x,
        y,
        item_unique,
        list_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_record_is_eighty_four_bytes() {
        assert_eq!(ITEM_ARRAY_LEN, 6720);
        assert_eq!(Item::empty().encode().len(), CLT_ITEM_SIZE);
    }

    #[test]
    fn unique_and_count_sit_at_the_first_two_words() {
        let item = Item::from_unique(0x02c1, 1);
        let bytes = item.encode();
        assert_eq!(&bytes[..4], &[0xc1, 0x02, 0x01, 0x00]);
        assert_eq!(item.unique(), 705);
        assert_eq!(item.count(), 1);
        assert!(!item.is_empty());
        assert!(Item::empty().is_empty());
    }

    #[test]
    fn count_can_be_rewritten_without_touching_the_rest() {
        let mut item = Item::decode(&Item::from_unique(7, 1).encode()).unwrap();
        item.set_count(9);
        assert_eq!(item.count(), 9);
        assert_eq!(item.unique(), 7);
    }

    #[test]
    fn item_array_round_trips_in_place() {
        let mut payload = vec![0xAB; ITEM_ARRAY_LEN + 4240];
        let mut items = vec![Item::empty(); MAX_ITEM_PER_PERSON];
        items[2] = Item::from_unique(1, 1);
        items[17] = Item::from_unique(200, 1);
        encode_item_array(&mut payload, &items).unwrap();

        assert_eq!(&payload[ITEM_ARRAY_LEN..], &[0xAB; 4240]);
        let read = decode_item_array(&payload).unwrap();
        assert_eq!(read, items);
        assert_eq!(read[2].unique(), 1);
        assert_eq!(read[17].unique(), 200);
    }

    #[test]
    fn a_mis_sized_array_is_refused_with_the_reason() {
        let mut payload = vec![0u8; ITEM_ARRAY_LEN];
        assert_eq!(
            encode_item_array(&mut payload, &[Item::empty(); 79]),
            Err(ItemArrayError::SlotCount { got: 79 })
        );
        assert_eq!(
            encode_item_array(&mut payload[..10], &[Item::empty(); 80]),
            Err(ItemArrayError::TooShort { have: 10 })
        );
    }

    #[test]
    fn move_item_is_two_little_endian_words() {
        assert_eq!(
            decode_move_item(&[0x0f, 0x00, 0x03, 0x00]).unwrap(),
            (15, 3)
        );
        assert_eq!(
            decode_move_item(&[0x03]),
            Err(WireError::Truncated { needed: 2, have: 1 })
        );
        assert_eq!(
            decode_move_item(&[0x03, 0x00, 0x00]),
            Err(WireError::Truncated { needed: 2, have: 1 })
        );
    }

    #[test]
    fn containers_match_the_personitem_ranges() {
        use personitem::*;
        // The whole equipment block collapses onto one container id.
        for slot in HELMET..=RESERVED1 {
            assert_eq!(container_of(slot), HELMET, "slot {slot}");
        }
        assert_eq!(container_of(INV0), INV0);
        assert_eq!(container_of(INV_END), INV0);
        assert_eq!(container_of(HORSE0), HORSE0);
        assert_eq!(container_of(HORSE_END), HORSE0);
        assert_eq!(container_of(BAG0), BAG0);
        assert_eq!(container_of(BAG0_END), BAG0);
        assert_eq!(container_of(BAG1), BAG1);
        assert_eq!(container_of(BAG1_END), BAG1);
    }

    #[test]
    fn the_four_range_bases_tile_the_array_without_gaps() {
        use personitem::*;
        // The layout is only credible if each container starts exactly where the
        // previous one ends. This is what makes the header's `INV0/ HORSE0 /
        // BAG0 / BAG1` anchors consistent with 20/20/12/12.
        assert_eq!(INV0, RESERVED1 + 1);
        assert_eq!(HORSE0, INV_END + 1);
        assert_eq!(BAG0, HORSE_END + 1);
        assert_eq!(BAG1, BAG0_END + 1);
        assert_eq!(INV_END - INV0 + 1, 20, "main bag is 4x5");
        assert_eq!(HORSE_END - HORSE0 + 1, 20, "horse is 4x5");
        assert_eq!(BAG0_END - BAG0 + 1, 12, "extra bag 1 is 4x3");
        assert_eq!(BAG1_END - BAG1 + 1, 12, "extra bag 2 is 4x3");
        // The 80th slot is outside every container and keeps its own id.
        assert_eq!(container_of(79), 79);
    }

    #[test]
    fn the_captured_drag_slots_are_bag_cells() {
        use personitem::*;
        // The live capture's two 303 pairs were `24 -> 20` and `28 -> 24`. In a
        // 4-wide grid rooted at INV0 that is rows 2, 3 and 4, i.e. ordinary bag
        // cells - which is what pinned the bag width at 4.
        for slot in [20u16, 24, 28] {
            assert!((INV0..=INV_END).contains(&slot), "slot {slot} is bag");
            assert_eq!(container_of(slot), INV0);
        }
    }

    #[test]
    fn inv_drop_is_a_position_then_a_full_record() {
        let item = Item::from_unique(2004, 5);
        let mut body = Vec::new();
        body.extend_from_slice(&24i32.to_le_bytes());
        body.extend_from_slice(&item.encode());
        assert_eq!(body.len(), 88, "4 + sizeof(cltItem)");

        let req = decode_inv_drop(&body).unwrap();
        assert_eq!(req.slot, 24);
        assert_eq!(req.item, item);

        assert_eq!(
            decode_inv_drop(&body[..87]),
            Err(WireError::Truncated {
                needed: 84,
                have: 83
            })
        );
    }

    #[test]
    fn inv_drop_keeps_an_out_of_range_position_intact() {
        // The legacy range test lives in the handler, not the decoder
        // (`Server-Item.cpp:2462`), so the raw SI32 must survive decoding.
        let mut body = Vec::new();
        body.extend_from_slice(&9999i32.to_le_bytes());
        body.extend_from_slice(&Item::from_unique(1, 1).encode());
        assert_eq!(decode_inv_drop(&body).unwrap().slot, 9999);
    }

    #[test]
    fn person_item_info_is_the_capture_s_104_bytes() {
        assert_eq!(PERSON_ITEM_INFO_SIZE, 104);
        let bytes = encode_person_item_info(8, &Item::empty());
        // siItemPos = 8 (PERSONITEM_WEAPON2), everything else zero.
        assert_eq!(bytes[0..2], [0x08, 0x00]);
        assert_eq!(bytes[2..4], [0x00, 0x00]);
        assert_eq!(&bytes[12..96], &[0u8; CLT_ITEM_SIZE]);
        assert_eq!(&bytes[96..104], &[0u8; 8]);
    }

    #[test]
    fn person_item_info_carries_the_record_verbatim() {
        let item = Item::from_unique(1500, 1);
        let bytes = encode_person_item_info(9, &item);
        assert_eq!(&bytes[12..96], &item.encode());
        assert_eq!(Item::decode(&bytes[12..]).unwrap(), item);
    }

    /// A grant goes out as `OPTION_SHOW` with the count in `siChangedNum`, and
    /// those four bytes are the *only* difference from a bare slot write.
    ///
    /// They are worth pinning because the client gates everything the owner
    /// asked for on them: `Client-Person-BigInfo.cpp:1054` switches on
    /// `siOption`, and `:1073` refuses to render anything while
    /// `siChangedNum` is 0. With `OPTION_NONE` the slot still updates — the
    /// drop was always reaching the bag — but the player is never told.
    #[test]
    fn a_gained_item_differs_from_a_bare_slot_write_only_in_the_notice_fields() {
        let item = Item::from_unique(1500, 3);
        let bare = encode_person_item_info(9, &item);
        let gained = encode_person_item_gained(9, 3, &item);

        assert_eq!(bare[2..4], [0, 0], "a bare write carries OPTION_NONE");
        assert_eq!(gained[2..4], [1, 0], "a grant carries OPTION_SHOW");
        assert_eq!(bare[4..6], [0, 0], "a bare write carries no changed count");
        assert_eq!(
            gained[4..6],
            [3, 0],
            "and a grant carries the granted count"
        );

        // Position, padding, siExp, the record and siPrice are byte for byte
        // the same frame.
        assert_eq!(bare[..2], gained[..2]);
        assert_eq!(bare[6..], gained[6..]);

        // A count past SI16 clamps rather than truncating into a negative,
        // which the client would read as "say nothing at all".
        assert_eq!(
            encode_person_item_gained(9, i32::MAX, &item)[4..6],
            [0xff, 0x7f]
        );
    }

    /// `from_raw` is the way back from a stored slot's `Vec<u8>` to a wire
    /// record, and it sits on the path that grants a drop — so it pads and
    /// truncates rather than panicking on a caller's bad slice.
    #[test]
    fn from_raw_rebuilds_a_record_and_survives_a_bad_slice() {
        let item = Item::from_unique(1500, 3);
        assert_eq!(Item::from_raw(&item.encode()), item);

        // Short: the prefix given survives, the rest is empty.
        let short = Item::from_raw(&item.encode()[..4]);
        assert_eq!(short.unique(), 1500);
        assert_eq!(short.raw[4..], [0u8; CLT_ITEM_SIZE - 4]);

        // Long: the extra bytes are ignored.
        let mut over = item.encode().to_vec();
        over.extend_from_slice(&[0xAA; 8]);
        assert_eq!(Item::from_raw(&over), item);
    }

    /// The 309 body is `cltCharPos` + two `UI16`, in the order the client reads
    /// them.
    ///
    /// No captured frame carries opcode 309, so there is no sample to diff
    /// against. What this pins is the field order — which the client's handler
    /// fixes — and the zeroed `siReserved`.
    #[test]
    fn item_drop_is_a_char_pos_and_two_handles() {
        let placed = GroundItem {
            map_index: 1,
            x: 25,
            y: 13,
            item_unique: 5075,
            list_index: 7,
        };

        assert_eq!(ITEM_DROP_SIZE, 12);
        assert_eq!(
            placed.encode(),
            [
                1, 0, // siMapIndex
                25, 0, // siX
                13, 0, // siY
                0, 0, // siReserved — never written by the legacy, never read
                0xD3, 0x13, // uiItemUnique 5075
                7, 0, // uiItemListIndex
            ]
        );
        assert_eq!(decode_item_drop(&placed.encode()).unwrap(), placed);
    }

    /// A cleared cell has to be expressible, because that is how the legacy takes
    /// an item back off the map (`ServerMap.cpp:241`).
    #[test]
    fn item_drop_can_clear_a_cell() {
        let clear = GroundItem::removal(1, 25, 13);
        assert!(clear.is_removal());
        assert_eq!(clear.encode(), [1, 0, 25, 0, 13, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode_item_drop(&clear.encode()).unwrap(), clear);
    }

    /// `siUnique` is signed in the record, so a placed item is never unique zero —
    /// which is exactly what makes zero usable as the clear sentinel.
    #[test]
    fn a_placed_item_is_not_mistaken_for_a_clear() {
        let placed = GroundItem {
            map_index: 1,
            x: 0,
            y: 0,
            item_unique: 1,
            list_index: 1,
        };
        assert!(!placed.is_removal());
        assert!(
            !GroundItem::removal(0, 0, 0).encode()[8..10]
                .iter()
                .any(|&b| b != 0)
        );
    }

    #[test]
    fn a_short_item_drop_is_refused_rather_than_read_past() {
        assert!(matches!(
            decode_item_drop(&[0u8; 11]),
            Err(WireError::Truncated { .. })
        ));
    }

    /// The 308 body is five little-endian `SI16` in the order the client fills
    /// them at `MouseOperation.cpp:5316`.
    ///
    /// No captured frame carries opcode 308, so this pins the field order rather
    /// than diffing against a sample. The values are chosen so a field-order slip
    /// cannot accidentally pass: swapping any adjacent pair changes the bytes.
    #[test]
    fn inv_pickup_is_five_signed_words() {
        let body = [
            0xD3, 0x13, // siItemUnique  5075
            0x07, 0x00, // siItemIndex   7  (the ground handle)
            0x01, 0x00, // siMapIndex    1
            0x19, 0x00, // siX           25
            0x0D, 0x00, // siY           13
        ];
        assert_eq!(body.len(), INV_PICKUP_SIZE);
        assert_eq!(body.len(), 10);
        assert_eq!(
            decode_inv_pickup(&body).unwrap(),
            InvPickupRequest {
                item_unique: 5075,
                item_index: 7,
                map_index: 1,
                x: 25,
                y: 13,
            }
        );
    }

    /// The handle travels as `UI16` in 309 but comes back as `SI16`, so a handle
    /// above `SI16::MAX` would arrive negative and has to be rejected rather than
    /// reinterpreted.
    #[test]
    fn a_handle_that_does_not_survive_the_signed_return_is_visible_as_negative() {
        let decoded = decode_inv_pickup(&[0x01, 0x00, 0x00, 0x80, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(decoded.item_index, i16::MIN);
    }

    #[test]
    fn a_short_inv_pickup_is_refused_rather_than_read_past() {
        assert!(matches!(
            decode_inv_pickup(&[0u8; 9]),
            Err(WireError::Truncated { .. })
        ));
    }
}
