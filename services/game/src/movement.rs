//! Movement: the stepped move family. `Movement` and its accumulator mirror
//! `cltCharCommon::Move` (`char-move.cpp:38-227`); `char_update_body` /
//! `send_char_update` / `fan_char_update` build and spread the 200
//! `CHARUPDATE`; and `handle_userorder` / `handle_leave_request` answer the 240
//! `REQUEST_USERORDER` and the 107/109 leave requests. The read loop feeds this
//! module's `step_movement` from its timer arm; the request handlers are called
//! from the dispatcher in `main.rs`.

use std::time::{Duration, Instant};

use gz_cipher::Cipher;
use gz_db::{CharacterFull, CharacterRepository, ItemRepository, PgCharacterRepository};
use gz_proto::game::encode_logout;
use gz_proto::map_geometry::cell_of;
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::{BasicCharUpdate, ORDER_MOVE, ORDER_SITDOWN, OrderData, SitDown};
use tokio::net::TcpStream;

use crate::registry::{Registry, SessionInfo, SessionKey};
use crate::{
    AnyError, LiveFrames, announce_map_npcs, announce_nearby_characters, logln, make_char_info_for,
    send_encoded,
};

/// One exit gate: the tile that triggers it and where it puts the character.
///
/// From `New Era Live\Data\GateInfoE.txt` (columns `Unique, TxtNum, FOUnique,
/// ItemUnique, ItemNum, SrcMap/SrcX/SrcY, DstMap/DstX/DstY, PKAreaGate`):
///
/// | rows | FOUnique | source | destination |
/// |---|---|---|---|
/// | 307, 308, 309 | 7023 | map 158 at (14,90), (28,101), (41,90) | map 0 at (288,354) |
/// | 306 | 49 | map 0 at x[284..292] y[347..350] | map 158 at (27,87) |
///
/// Hanyang's three gates are the map's exits — `FOTYPE_GATE` field objects whose
/// hover name is text 254, "Move outside" — and the reverse row lands on the tile
/// `Server-Login.cpp:375` spawns new characters on.
///
/// **A literal table is this slice's sanctioned cut** (card §4 slice 3): the real
/// loader is A9's field-object/map work, which needs `FieldObjectSetInfo.txt` and
/// the per-map tile arrays anyway, and the legacy reaches the same answer through
/// the field object's `para` field (`char.cpp:9031`). Every row below is copied
/// from the file above; nothing is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gate {
    /// `SrcMapIndex` — the server map id the gate stands on.
    src_map: i32,
    /// `SrcMapX`.
    src_x: i32,
    /// `SrcMapY`.
    src_y: i32,
    /// The gate object's `TileXsize` — the block is `xsize` cells wide.
    xsize: i32,
    /// The gate object's `TileYsize`.
    ysize: i32,
    /// The gate object's `TileCenterX` — 0 for every gate in this file.
    center_x: i32,
    /// The gate object's `TileCenterY`.
    center_y: i32,
    /// The item the gate requires, and how many — `GateInfoE.txt` columns 4 and 5.
    ///
    /// The legacy checks these before warping and **never consumes them**:
    /// `char.cpp:9123-9133` reads `ItemUnique`/`ItemNum` off the gate row (`:9077-9078`),
    /// and if `clCharItem.GetItemNum(itemunique) < itemnum` it answers
    /// `SRVAL_FAIL_WARP_NOITEM` and returns. Nothing in that function takes the item
    /// away, so a ticket is an entry *key*, not a fee. 0 means no requirement.
    item_unique: i32,
    /// See [`Gate::item_unique`].
    item_num: i32,
    /// `DstMapIndex`.
    dst_map: i32,
    /// `DstMapX`.
    dst_x: i32,
    /// `DstMapY`.
    dst_y: i32,
}

/// Every gate this port honours, in `GateInfoE.txt` order — all 244 rows the file
/// yields, with each row's block from its own field object's detail file.
const GATES: &[Gate] = &[
    Gate {
        src_map: 0,
        src_x: 291,
        src_y: 350,
        xsize: 9,
        ysize: 4,
        center_x: 1,
        center_y: 0,
        dst_map: 158,
        dst_x: 27,
        dst_y: 87,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 158,
        src_x: 14,
        src_y: 90,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 158,
        src_x: 28,
        src_y: 101,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 158,
        src_x: 41,
        src_y: 90,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 254,
        src_y: 603,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 2,
        dst_map: 109,
        dst_x: 62,
        dst_y: 70,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 109,
        src_x: 67,
        src_y: 76,
        xsize: 9,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 260,
        dst_y: 603,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 109,
        src_x: 55,
        src_y: 3,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 94,
        dst_x: 6,
        dst_y: 69,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 94,
        src_x: 2,
        src_y: 69,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 109,
        dst_x: 61,
        dst_y: 13,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 317,
        src_y: 423,
        xsize: 5,
        ysize: 5,
        center_x: 3,
        center_y: 3,
        dst_map: 99,
        dst_x: 6,
        dst_y: 44,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 99,
        src_x: 6,
        src_y: 40,
        xsize: 5,
        ysize: 4,
        center_x: 1,
        center_y: 0,
        dst_map: 0,
        dst_x: 323,
        dst_y: 425,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 99,
        src_x: 62,
        src_y: 51,
        xsize: 5,
        ysize: 4,
        center_x: 1,
        center_y: 0,
        dst_map: 101,
        dst_x: 33,
        dst_y: 47,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 101,
        src_x: 29,
        src_y: 47,
        xsize: 6,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 99,
        dst_x: 62,
        dst_y: 55,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 101,
        src_x: 74,
        src_y: 62,
        xsize: 6,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 100,
        dst_x: 23,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 100,
        src_x: 19,
        src_y: 16,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 101,
        dst_x: 78,
        dst_y: 62,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 265,
        src_y: 360,
        xsize: 4,
        ysize: 4,
        center_x: 1,
        center_y: 1,
        dst_map: 108,
        dst_x: 40,
        dst_y: 48,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 108,
        src_x: 37,
        src_y: 44,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 0,
        dst_x: 265,
        dst_y: 360,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 402,
        src_y: 580,
        xsize: 4,
        ysize: 5,
        center_x: 1,
        center_y: 1,
        dst_map: 157,
        dst_x: 60,
        dst_y: 97,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 157,
        src_x: 59,
        src_y: 93,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 405,
        dst_y: 574,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 157,
        src_x: 32,
        src_y: 19,
        xsize: 6,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 168,
        dst_x: 69,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 168,
        src_x: 69,
        src_y: 12,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 157,
        dst_x: 32,
        dst_y: 23,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 522,
        src_y: 446,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 170,
        dst_x: 5,
        dst_y: 97,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 170,
        src_x: 5,
        src_y: 97,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 0,
        dst_x: 524,
        dst_y: 447,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 170,
        src_x: 86,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 171,
        dst_x: 20,
        dst_y: 44,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 171,
        src_x: 19,
        src_y: 42,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 170,
        dst_x: 86,
        dst_y: 12,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 166,
        src_y: 124,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 180,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 181,
        src_y: 322,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 181,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 240,
        src_y: 650,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 182,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 252,
        src_y: 697,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 183,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 477,
        src_y: 665,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 184,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 398,
        src_y: 707,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 185,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 203,
        src_y: 406,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 186,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 382,
        src_y: 85,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 187,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 418,
        src_y: 155,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 188,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 607,
        src_y: 219,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 189,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 258,
        src_y: 153,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 190,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 581,
        src_y: 592,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 191,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 516,
        src_y: 621,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 192,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 317,
        src_y: 585,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 193,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 663,
        src_y: 428,
        xsize: 4,
        ysize: 10,
        center_x: 0,
        center_y: 0,
        dst_map: 194,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 510,
        src_y: 23,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 120,
        dst_x: 38,
        dst_y: 58,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 120,
        src_x: 45,
        src_y: 75,
        xsize: 7,
        ysize: 9,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 507,
        dst_y: 26,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 120,
        src_x: 3,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 121,
        dst_x: 8,
        dst_y: 57,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 121,
        src_x: 5,
        src_y: 48,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 4,
        dst_map: 120,
        dst_x: 7,
        dst_y: 22,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 121,
        src_x: 59,
        src_y: 48,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 4,
        dst_map: 122,
        dst_x: 17,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 122,
        src_x: 11,
        src_y: 10,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 121,
        dst_x: 60,
        dst_y: 57,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 356,
        src_y: 700,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 179,
        dst_x: 56,
        dst_y: 54,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 179,
        src_x: 59,
        src_y: 54,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 356,
        dst_y: 700,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 289,
        src_y: 41,
        xsize: 6,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 154,
        dst_x: 40,
        dst_y: 81,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 154,
        src_x: 40,
        src_y: 77,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 286,
        dst_y: 43,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 154,
        src_x: 15,
        src_y: 103,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 169,
        dst_x: 53,
        dst_y: 61,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 169,
        src_x: 49,
        src_y: 61,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 154,
        dst_x: 19,
        dst_y: 103,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 154,
        src_x: 41,
        src_y: 2,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 155,
        dst_x: 34,
        dst_y: 42,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 155,
        src_x: 38,
        src_y: 49,
        xsize: 7,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 154,
        dst_x: 41,
        dst_y: 6,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 322,
        src_y: 213,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 111,
        dst_x: 51,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 111,
        src_x: 47,
        src_y: 16,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 321,
        dst_y: 216,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 111,
        src_x: 7,
        src_y: 56,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 112,
        dst_x: 52,
        dst_y: 45,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 112,
        src_x: 48,
        src_y: 45,
        xsize: 5,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 111,
        dst_x: 11,
        dst_y: 56,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 413,
        src_y: 217,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 104,
        dst_x: 77,
        dst_y: 60,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 104,
        src_x: 74,
        src_y: 60,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 417,
        dst_y: 216,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 104,
        src_x: 12,
        src_y: 35,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 105,
        dst_x: 105,
        dst_y: 23,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 105,
        src_x: 95,
        src_y: 20,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 104,
        dst_x: 6,
        dst_y: 35,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 419,
        src_y: 629,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 116,
        dst_x: 80,
        dst_y: 60,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 116,
        src_x: 76,
        src_y: 60,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 424,
        dst_y: 627,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 116,
        src_x: 12,
        src_y: 92,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 117,
        dst_x: 5,
        dst_y: 32,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 117,
        src_x: 1,
        src_y: 32,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 116,
        dst_x: 16,
        dst_y: 92,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 116,
        src_x: 61,
        src_y: 75,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 118,
        dst_x: 83,
        dst_y: 19,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 118,
        src_x: 83,
        src_y: 15,
        xsize: 7,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 116,
        dst_x: 61,
        dst_y: 79,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 602,
        src_y: 311,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 125,
        dst_x: 21,
        dst_y: 5,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 125,
        src_x: 21,
        src_y: 3,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 602,
        dst_y: 316,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 125,
        src_x: 33,
        src_y: 79,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 126,
        dst_x: 13,
        dst_y: 10,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 126,
        src_x: 13,
        src_y: 8,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 125,
        dst_x: 35,
        dst_y: 78,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 126,
        src_x: 5,
        src_y: 80,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 127,
        dst_x: 29,
        dst_y: 8,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 127,
        src_x: 29,
        src_y: 6,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 126,
        dst_x: 7,
        dst_y: 79,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 127,
        src_x: 9,
        src_y: 82,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 128,
        dst_x: 30,
        dst_y: 6,
        item_unique: 7917,
        item_num: 1,
    },
    Gate {
        src_map: 128,
        src_x: 30,
        src_y: 4,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 127,
        dst_x: 11,
        dst_y: 82,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 128,
        src_x: 74,
        src_y: 80,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 129,
        dst_x: 5,
        dst_y: 10,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 129,
        src_x: 2,
        src_y: 10,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 128,
        dst_x: 77,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 129,
        src_x: 5,
        src_y: 61,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 130,
        dst_x: 19,
        dst_y: 14,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 130,
        src_x: 19,
        src_y: 12,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 129,
        dst_x: 8,
        dst_y: 61,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 130,
        src_x: 47,
        src_y: 72,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 131,
        dst_x: 77,
        dst_y: 5,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 131,
        src_x: 78,
        src_y: 3,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 130,
        dst_x: 49,
        dst_y: 71,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 131,
        src_x: 54,
        src_y: 43,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 132,
        dst_x: 11,
        dst_y: 14,
        item_unique: 7917,
        item_num: 2,
    },
    Gate {
        src_map: 132,
        src_x: 9,
        src_y: 14,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 131,
        dst_x: 53,
        dst_y: 45,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 132,
        src_x: 81,
        src_y: 6,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 133,
        dst_x: 80,
        dst_y: 19,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 133,
        src_x: 81,
        src_y: 17,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 132,
        dst_x: 80,
        dst_y: 8,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 133,
        src_x: 9,
        src_y: 81,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 134,
        dst_x: 11,
        dst_y: 77,
        item_unique: 7917,
        item_num: 3,
    },
    Gate {
        src_map: 134,
        src_x: 9,
        src_y: 77,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 133,
        dst_x: 11,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 575,
        src_y: 172,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 135,
        dst_x: 15,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 135,
        src_x: 9,
        src_y: 80,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 1,
        dst_map: 0,
        dst_x: 579,
        dst_y: 177,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 135,
        src_x: 25,
        src_y: 9,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 136,
        dst_x: 15,
        dst_y: 69,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 136,
        src_x: 9,
        src_y: 69,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 1,
        dst_map: 135,
        dst_x: 25,
        dst_y: 15,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 136,
        src_x: 17,
        src_y: 6,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 137,
        dst_x: 48,
        dst_y: 130,
        item_unique: 7917,
        item_num: 1,
    },
    Gate {
        src_map: 137,
        src_x: 42,
        src_y: 130,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 136,
        dst_x: 17,
        dst_y: 12,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 137,
        src_x: 34,
        src_y: 2,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 138,
        dst_x: 26,
        dst_y: 82,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 138,
        src_x: 20,
        src_y: 82,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 137,
        dst_x: 34,
        dst_y: 8,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 137,
        src_x: 66,
        src_y: 2,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 139,
        dst_x: 17,
        dst_y: 82,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 139,
        src_x: 11,
        src_y: 82,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 137,
        dst_x: 66,
        dst_y: 8,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 138,
        src_x: 76,
        src_y: 34,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 139,
        dst_x: 49,
        dst_y: 58,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 139,
        src_x: 43,
        src_y: 58,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 138,
        dst_x: 76,
        dst_y: 40,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 138,
        src_x: 32,
        src_y: 6,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 140,
        dst_x: 15,
        dst_y: 130,
        item_unique: 7917,
        item_num: 2,
    },
    Gate {
        src_map: 140,
        src_x: 9,
        src_y: 131,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 138,
        dst_x: 32,
        dst_y: 12,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 139,
        src_x: 27,
        src_y: 10,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 140,
        dst_x: 67,
        dst_y: 126,
        item_unique: 7917,
        item_num: 2,
    },
    Gate {
        src_map: 140,
        src_x: 61,
        src_y: 127,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 139,
        dst_x: 27,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 140,
        src_x: 17,
        src_y: 11,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 141,
        dst_x: 50,
        dst_y: 133,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 141,
        src_x: 44,
        src_y: 132,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 1,
        dst_map: 140,
        dst_x: 17,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 140,
        src_x: 81,
        src_y: 11,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 142,
        dst_x: 15,
        dst_y: 132,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 142,
        src_x: 9,
        src_y: 132,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 140,
        dst_x: 81,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 140,
        src_x: 49,
        src_y: 11,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 143,
        dst_x: 48,
        dst_y: 126,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 143,
        src_x: 44,
        src_y: 131,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 140,
        dst_x: 49,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 143,
        src_x: 44,
        src_y: 11,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 144,
        dst_x: 40,
        dst_y: 130,
        item_unique: 7917,
        item_num: 3,
    },
    Gate {
        src_map: 144,
        src_x: 34,
        src_y: 130,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 1,
        dst_map: 143,
        dst_x: 44,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 253,
        src_y: 446,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 103,
        dst_x: 10,
        dst_y: 25,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 103,
        src_x: 2,
        src_y: 26,
        xsize: 3,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 257,
        dst_y: 446,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 300,
        src_x: 5,
        src_y: 97,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 97,
        dst_x: 60,
        dst_y: 130,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 300,
        src_x: 86,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 301,
        dst_x: 62,
        dst_y: 70,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 301,
        src_x: 67,
        src_y: 76,
        xsize: 9,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 300,
        dst_x: 86,
        dst_y: 12,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 301,
        src_x: 55,
        src_y: 3,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 302,
        dst_x: 105,
        dst_y: 23,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 302,
        src_x: 99,
        src_y: 25,
        xsize: 1,
        ysize: 1,
        center_x: 0,
        center_y: 0,
        dst_map: 301,
        dst_x: 61,
        dst_y: 13,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 302,
        src_x: 4,
        src_y: 83,
        xsize: 1,
        ysize: 1,
        center_x: 0,
        center_y: 0,
        dst_map: 303,
        dst_x: 80,
        dst_y: 60,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 303,
        src_x: 76,
        src_y: 60,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 302,
        dst_x: 10,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 303,
        src_x: 12,
        src_y: 92,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 304,
        dst_x: 8,
        dst_y: 57,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 304,
        src_x: 5,
        src_y: 48,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 4,
        dst_map: 303,
        dst_x: 16,
        dst_y: 92,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 304,
        src_x: 59,
        src_y: 48,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 4,
        dst_map: 305,
        dst_x: 73,
        dst_y: 12,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 305,
        src_x: 73,
        src_y: 9,
        xsize: 1,
        ysize: 1,
        center_x: 1,
        center_y: 1,
        dst_map: 304,
        dst_x: 60,
        dst_y: 57,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 305,
        src_x: 9,
        src_y: 81,
        xsize: 3,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 306,
        dst_x: 80,
        dst_y: 11,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 306,
        src_x: 82,
        src_y: 9,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 305,
        dst_x: 12,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 558,
        src_y: 548,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 163,
        dst_x: 103,
        dst_y: 84,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 163,
        src_x: 107,
        src_y: 85,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 552,
        dst_y: 546,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 163,
        src_x: 53,
        src_y: 80,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 164,
        dst_x: 22,
        dst_y: 8,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 164,
        src_x: 20,
        src_y: 5,
        xsize: 4,
        ysize: 4,
        center_x: 2,
        center_y: 0,
        dst_map: 163,
        dst_x: 56,
        dst_y: 80,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 224,
        src_y: 314,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 320,
        dst_x: 21,
        dst_y: 53,
        item_unique: 24107,
        item_num: 1,
    },
    Gate {
        src_map: 0,
        src_x: 266,
        src_y: 355,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 95,
        dst_x: 51,
        dst_y: 64,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 95,
        src_x: 51,
        src_y: 59,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 270,
        dst_y: 355,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 96,
        src_x: 26,
        src_y: 45,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 158,
        dst_x: 27,
        dst_y: 87,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 164,
        src_x: 40,
        src_y: 169,
        xsize: 4,
        ysize: 4,
        center_x: 2,
        center_y: 0,
        dst_map: 165,
        dst_x: 12,
        dst_y: 121,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 165,
        src_x: 8,
        src_y: 123,
        xsize: 6,
        ysize: 6,
        center_x: 1,
        center_y: 0,
        dst_map: 164,
        dst_x: 46,
        dst_y: 169,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 165,
        src_x: 39,
        src_y: 14,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 166,
        dst_x: 77,
        dst_y: 155,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 166,
        src_x: 77,
        src_y: 151,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 165,
        dst_x: 46,
        dst_y: 28,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 695,
        src_y: 240,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 172,
        dst_x: 126,
        dst_y: 28,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 172,
        src_x: 129,
        src_y: 39,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 690,
        dst_y: 245,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 172,
        src_x: 6,
        src_y: 34,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 173,
        dst_x: 33,
        dst_y: 44,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 173,
        src_x: 27,
        src_y: 43,
        xsize: 4,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 172,
        dst_x: 10,
        dst_y: 34,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 173,
        src_x: 82,
        src_y: 25,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 174,
        dst_x: 61,
        dst_y: 13,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 174,
        src_x: 61,
        src_y: 9,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 173,
        dst_x: 95,
        dst_y: 29,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 356,
        src_x: 3,
        src_y: 21,
        xsize: 3,
        ysize: 6,
        center_x: 2,
        center_y: 1,
        dst_map: 0,
        dst_x: 305,
        dst_y: 350,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 356,
        src_x: 9,
        src_y: 14,
        xsize: 3,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 357,
        dst_x: 50,
        dst_y: 60,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 356,
        src_x: 14,
        src_y: 13,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 358,
        dst_x: 55,
        dst_y: 50,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 356,
        src_x: 21,
        src_y: 13,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 359,
        dst_x: 68,
        dst_y: 50,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 356,
        src_x: 28,
        src_y: 13,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 360,
        dst_x: 51,
        dst_y: 61,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 361,
        src_x: 62,
        src_y: 51,
        xsize: 5,
        ysize: 4,
        center_x: 1,
        center_y: 0,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 362,
        src_x: 40,
        src_y: 77,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 363,
        src_x: 67,
        src_y: 76,
        xsize: 9,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 364,
        src_x: 83,
        src_y: 15,
        xsize: 7,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 365,
        src_x: 59,
        src_y: 48,
        xsize: 3,
        ysize: 4,
        center_x: 0,
        center_y: 4,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 314,
        src_y: 339,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 380,
        dst_x: 49,
        dst_y: 57,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 380,
        src_x: 23,
        src_y: 79,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 313,
        dst_y: 341,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 380,
        src_x: 23,
        src_y: 23,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 313,
        dst_y: 341,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 380,
        src_x: 79,
        src_y: 23,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 313,
        dst_y: 341,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 380,
        src_x: 79,
        src_y: 79,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 313,
        dst_y: 341,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 385,
        src_x: 56,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 292,
        dst_y: 351,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 385,
        src_x: 27,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 102,
        dst_x: 46,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 102,
        src_x: 46,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 385,
        dst_x: 32,
        dst_y: 24,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 386,
        src_x: 56,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 292,
        dst_y: 351,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 386,
        src_x: 27,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 123,
        dst_x: 46,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 123,
        src_x: 46,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 386,
        dst_x: 32,
        dst_y: 24,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 387,
        src_x: 56,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 292,
        dst_y: 351,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 387,
        src_x: 27,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 124,
        dst_x: 46,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 124,
        src_x: 46,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 387,
        dst_x: 32,
        dst_y: 24,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 782,
        src_y: 204,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 366,
        dst_x: 72,
        dst_y: 28,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 366,
        src_x: 67,
        src_y: 28,
        xsize: 4,
        ysize: 5,
        center_x: 1,
        center_y: 1,
        dst_map: 0,
        dst_x: 785,
        dst_y: 210,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 366,
        src_x: 82,
        src_y: 91,
        xsize: 6,
        ysize: 4,
        center_x: 1,
        center_y: 2,
        dst_map: 367,
        dst_x: 55,
        dst_y: 103,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 367,
        src_x: 55,
        src_y: 98,
        xsize: 6,
        ysize: 4,
        center_x: 1,
        center_y: 1,
        dst_map: 366,
        dst_x: 82,
        dst_y: 97,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 367,
        src_x: 95,
        src_y: 58,
        xsize: 4,
        ysize: 6,
        center_x: 1,
        center_y: 1,
        dst_map: 368,
        dst_x: 64,
        dst_y: 98,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 368,
        src_x: 58,
        src_y: 95,
        xsize: 4,
        ysize: 5,
        center_x: 1,
        center_y: 2,
        dst_map: 367,
        dst_x: 100,
        dst_y: 58,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 97,
        src_x: 135,
        src_y: 187,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 514,
        src_y: 264,
        xsize: 4,
        ysize: 4,
        center_x: 1,
        center_y: 1,
        dst_map: 373,
        dst_x: 6,
        dst_y: 30,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 321,
        src_y: 614,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 375,
        dst_x: 86,
        dst_y: 11,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 375,
        src_x: 32,
        src_y: 53,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 326,
        dst_y: 633,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 375,
        src_x: 29,
        src_y: 9,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 375,
        dst_x: 114,
        dst_y: 52,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 375,
        src_x: 119,
        src_y: 32,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 375,
        dst_x: 11,
        dst_y: 20,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 375,
        src_x: 75,
        src_y: 29,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 376,
        dst_x: 68,
        dst_y: 120,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 376,
        src_x: 74,
        src_y: 71,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 375,
        dst_x: 3,
        dst_y: 27,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 376,
        src_x: 31,
        src_y: 120,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 376,
        dst_x: 34,
        dst_y: 9,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 376,
        src_x: 54,
        src_y: 7,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 376,
        dst_x: 73,
        dst_y: 76,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 376,
        src_x: 6,
        src_y: 50,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 377,
        dst_x: 43,
        dst_y: 43,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 377,
        src_x: 40,
        src_y: 40,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 376,
        dst_x: 55,
        dst_y: 48,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 329,
        src_x: 14,
        src_y: 14,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 98,
        src_x: 3,
        src_y: 47,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 288,
        dst_y: 354,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 526,
        src_y: 144,
        xsize: 5,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 382,
        dst_x: 13,
        dst_y: 122,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 382,
        src_x: 10,
        src_y: 121,
        xsize: 4,
        ysize: 2,
        center_x: 0,
        center_y: 1,
        dst_map: 0,
        dst_x: 524,
        dst_y: 146,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 382,
        src_x: 53,
        src_y: 13,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 383,
        dst_x: 15,
        dst_y: 115,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 383,
        src_x: 9,
        src_y: 114,
        xsize: 5,
        ysize: 6,
        center_x: 2,
        center_y: 1,
        dst_map: 382,
        dst_x: 53,
        dst_y: 17,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 383,
        src_x: 7,
        src_y: 24,
        xsize: 5,
        ysize: 6,
        center_x: 2,
        center_y: 1,
        dst_map: 384,
        dst_x: 117,
        dst_y: 105,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 384,
        src_x: 114,
        src_y: 105,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 383,
        dst_x: 13,
        dst_y: 26,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 639,
        src_y: 531,
        xsize: 5,
        ysize: 5,
        center_x: 1,
        center_y: 1,
        dst_map: 400,
        dst_x: 6,
        dst_y: 33,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 400,
        src_x: 1,
        src_y: 32,
        xsize: 4,
        ysize: 2,
        center_x: 0,
        center_y: 1,
        dst_map: 0,
        dst_x: 641,
        dst_y: 533,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 400,
        src_x: 84,
        src_y: 20,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 401,
        dst_x: 6,
        dst_y: 49,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 401,
        src_x: 0,
        src_y: 50,
        xsize: 1,
        ysize: 1,
        center_x: 1,
        center_y: 1,
        dst_map: 400,
        dst_x: 78,
        dst_y: 21,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 401,
        src_x: 72,
        src_y: 16,
        xsize: 5,
        ysize: 5,
        center_x: 1,
        center_y: 1,
        dst_map: 402,
        dst_x: 20,
        dst_y: 22,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 402,
        src_x: 12,
        src_y: 28,
        xsize: 4,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 401,
        dst_x: 69,
        dst_y: 21,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 384,
        src_x: 177,
        src_y: 58,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 403,
        dst_x: 8,
        dst_y: 65,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 403,
        src_x: 8,
        src_y: 65,
        xsize: 5,
        ysize: 6,
        center_x: 2,
        center_y: 1,
        dst_map: 384,
        dst_x: 173,
        dst_y: 68,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 403,
        src_x: 95,
        src_y: 20,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 404,
        dst_x: 4,
        dst_y: 97,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 404,
        src_x: 4,
        src_y: 97,
        xsize: 5,
        ysize: 6,
        center_x: 2,
        center_y: 1,
        dst_map: 403,
        dst_x: 95,
        dst_y: 20,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 404,
        src_x: 53,
        src_y: 13,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 405,
        dst_x: 10,
        dst_y: 125,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 405,
        src_x: 10,
        src_y: 125,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 404,
        dst_x: 53,
        dst_y: 13,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 406,
        src_x: 100,
        src_y: 27,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 293,
        dst_y: 337,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 407,
        src_x: 125,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 293,
        dst_y: 337,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 408,
        src_x: 97,
        src_y: 107,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 293,
        dst_y: 337,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 409,
        src_x: 52,
        src_y: 14,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 293,
        dst_y: 337,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 412,
        src_x: 56,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 292,
        dst_y: 351,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 412,
        src_x: 27,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 410,
        dst_x: 46,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 410,
        src_x: 46,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 412,
        dst_x: 32,
        dst_y: 24,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 413,
        src_x: 56,
        src_y: 89,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 292,
        dst_y: 351,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 413,
        src_x: 27,
        src_y: 26,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 411,
        dst_x: 46,
        dst_y: 16,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 411,
        src_x: 46,
        src_y: 12,
        xsize: 4,
        ysize: 3,
        center_x: 2,
        center_y: 2,
        dst_map: 413,
        dst_x: 32,
        dst_y: 24,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 414,
        src_x: 27,
        src_y: 43,
        xsize: 4,
        ysize: 5,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 290,
        dst_y: 356,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 723,
        src_y: 574,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 415,
        dst_x: 12,
        dst_y: 22,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 415,
        src_x: 11,
        src_y: 20,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 0,
        dst_x: 727,
        dst_y: 579,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 415,
        src_x: 7,
        src_y: 56,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 417,
        dst_x: 38,
        dst_y: 9,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 417,
        src_x: 38,
        src_y: 9,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 417,
        dst_x: 38,
        dst_y: 9,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 417,
        src_x: 35,
        src_y: 76,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 418,
        dst_x: 37,
        dst_y: 4,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 418,
        src_x: 32,
        src_y: 78,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 419,
        dst_x: 64,
        dst_y: 4,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 780,
        src_y: 641,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 423,
        dst_x: 7,
        dst_y: 22,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 423,
        src_x: 8,
        src_y: 18,
        xsize: 5,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 780,
        dst_y: 644,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 423,
        src_x: 132,
        src_y: 70,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 424,
        dst_x: 65,
        dst_y: 27,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 424,
        src_x: 60,
        src_y: 27,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 423,
        dst_x: 133,
        dst_y: 70,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 0,
        src_x: 503,
        src_y: 710,
        xsize: 6,
        ysize: 6,
        center_x: 0,
        center_y: 0,
        dst_map: 428,
        dst_x: 104,
        dst_y: 120,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 428,
        src_x: 103,
        src_y: 123,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 503,
        dst_y: 710,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 428,
        src_x: 123,
        src_y: 95,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 429,
        dst_x: 3,
        dst_y: 132,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 429,
        src_x: 3,
        src_y: 132,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 428,
        dst_x: 123,
        dst_y: 95,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 94,
        src_x: 35,
        src_y: 72,
        xsize: 3,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 447,
        dst_x: 14,
        dst_y: 61,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 447,
        src_x: 14,
        src_y: 61,
        xsize: 9,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 94,
        dst_x: 35,
        dst_y: 72,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 451,
        src_x: 130,
        src_y: 70,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 0,
        dst_x: 212,
        dst_y: 502,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 451,
        src_x: 62,
        src_y: 11,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 452,
        dst_x: 55,
        dst_y: 93,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 452,
        src_x: 55,
        src_y: 93,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 451,
        dst_x: 62,
        dst_y: 11,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 453,
        src_x: 96,
        src_y: 81,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 452,
        dst_x: 45,
        dst_y: 9,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 452,
        src_x: 45,
        src_y: 9,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 453,
        dst_x: 96,
        dst_y: 81,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 428,
        src_x: 102,
        src_y: 41,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 454,
        dst_x: 22,
        dst_y: 36,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 418,
        src_x: 37,
        src_y: 15,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 419,
        dst_x: 7,
        dst_y: 79,
        item_unique: 0,
        item_num: 0,
    },
    Gate {
        src_map: 419,
        src_x: 7,
        src_y: 79,
        xsize: 4,
        ysize: 3,
        center_x: 1,
        center_y: 1,
        dst_map: 418,
        dst_x: 37,
        dst_y: 76,
        item_unique: 0,
        item_num: 0,
    },
];

const VILLAGES: &[Village] = &[
    Village {
        unique: 1,
        src_x: 269,
        src_y: 325,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 1,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 2,
        src_x: 214,
        src_y: 346,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 2,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 3,
        src_x: 241,
        src_y: 303,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 3,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 4,
        src_x: 277,
        src_y: 619,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 4,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 5,
        src_x: 476,
        src_y: 132,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 5,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 6,
        src_x: 280,
        src_y: 405,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 6,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 7,
        src_x: 290,
        src_y: 214,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 7,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 8,
        src_x: 377,
        src_y: 661,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 8,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 9,
        src_x: 425,
        src_y: 52,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 9,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 10,
        src_x: 448,
        src_y: 498,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 10,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 11,
        src_x: 563,
        src_y: 238,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 11,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 12,
        src_x: 591,
        src_y: 362,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 12,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 13,
        src_x: 475,
        src_y: 413,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 13,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 14,
        src_x: 437,
        src_y: 240,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 14,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 15,
        src_x: 280,
        src_y: 694,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 15,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 16,
        src_x: 241,
        src_y: 409,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 16,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 17,
        src_x: 464,
        src_y: 722,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 17,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 18,
        src_x: 252,
        src_y: 92,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 18,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 19,
        src_x: 650,
        src_y: 376,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 19,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 20,
        src_x: 641,
        src_y: 553,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 20,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 21,
        src_x: 24,
        src_y: 629,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 21,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 22,
        src_x: 754,
        src_y: 130,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 22,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 23,
        src_x: 458,
        src_y: 572,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 23,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 24,
        src_x: 473,
        src_y: 593,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 24,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 25,
        src_x: 412,
        src_y: 655,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 25,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 26,
        src_x: 458,
        src_y: 660,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 26,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 27,
        src_x: 396,
        src_y: 591,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 27,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 28,
        src_x: 415,
        src_y: 550,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 28,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 29,
        src_x: 524,
        src_y: 514,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 29,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 30,
        src_x: 616,
        src_y: 500,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 30,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 31,
        src_x: 425,
        src_y: 438,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 31,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 32,
        src_x: 449,
        src_y: 411,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 32,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 33,
        src_x: 640,
        src_y: 281,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 33,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 34,
        src_x: 188,
        src_y: 179,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 34,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 35,
        src_x: 269,
        src_y: 243,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 35,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 36,
        src_x: 298,
        src_y: 299,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 36,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 37,
        src_x: 252,
        src_y: 478,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 37,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 38,
        src_x: 302,
        src_y: 447,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 38,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 39,
        src_x: 337,
        src_y: 482,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 39,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 40,
        src_x: 357,
        src_y: 410,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 40,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 41,
        src_x: 272,
        src_y: 522,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 41,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
    Village {
        unique: 42,
        src_x: 299,
        src_y: 660,
        xsize: 8,
        ysize: 8,
        center_x: 1,
        center_y: 1,
        dst_map: 42,
        dst_x: 25,
        dst_y: 14,
        exit_x: 25,
        exit_y: 7,
    },
];

/// The tile block one gate covers, `PutFieldObject`'s formula made explicit.
impl Gate {
    /// Whether `(x, y)` is one of the cells this gate's object occupies.
    ///
    /// `Map-FieldObject.cpp:102-107`: the object is stamped for `j` in
    /// `0..TileXsize` at `x = anchor + TileCenterX - TileXsize + 1 + j`, and likewise
    /// for `y`.
    pub(crate) const fn covers(&self, x: i32, y: i32) -> bool {
        let (left, top, _, _) = self.block();
        x >= left && x < left + self.xsize && y >= top && y < top + self.ysize
    }

    /// The stamped block as `(left, top, right, bottom)`, inclusive.
    ///
    /// The row's `src_x`/`src_y` is the map editor's own marker for the gate and, for
    /// objects whose `TileCenter` is not zero, can sit outside the block this formula
    /// produces — map 121 at (5,48) is one. That is harmless here because the legacy
    /// never looks a gate up by the row's coordinates: it reads the field object of the
    /// *cell* the player ordered to (`char.cpp:8975`, `:9031`) and takes the gate id
    /// from that cell's `para`. So the block is the authority, and this is it.
    pub(crate) const fn block(&self) -> (i32, i32, i32, i32) {
        let left = self.src_x + self.center_x - self.xsize + 1;
        let top = self.src_y + self.center_y - self.ysize + 1;
        (left, top, left + self.xsize - 1, top + self.ysize - 1)
    }
}

/// The gate covering `(map_id, x, y)`, if any.
///
/// The gate is a **field object**, not a tile: `cltUnderMouseObject::FindGate`
/// (`UnderMouseObject.cpp:290-307`) resolves a click by the object on the cell, and
/// `PutFieldObject` (`Map-FieldObject.cpp:66-130`) stamps the object's whole block — so
/// every cell of the block is that gate. Each row carries its own object's numbers:
/// `TileXsize`/`TileYsize`/`TileCenterX`/`TileCenterY` from that object's detail file,
/// which `cltFieldObjectInfo::LoadDetailInfo` reads (`FieldObject.cpp:325-399`).
fn gate_at(map_id: i32, x: i32, y: i32) -> Option<&'static Gate> {
    GATES.iter().find(|g| g.src_map == map_id && g.covers(x, y))
}

/// `INIT_LEVEL` — the level below which a revive always returns to the palace
/// (`CommonHeader.h:672`, `#define INIT_LEVEL 10`).
pub(crate) const INIT_LEVEL: i16 = 10;

/// `MAPINDEX_PALACE` at the tile the login spawn uses — where `Revive()` sends every
/// branch that has no better answer (`char.cpp:4923`, `:5000`, `:5009`, `:5019`,
/// `:5045`, `:5061`, all `cltWarpInfo(1, MAPINDEX_PALACE, false, false, 27, 87, …)`).
pub(crate) const REVIVE_PALACE: (i32, i32, i32) = (158, 27, 87);

/// The maps this port treats as towns, so a revive inside one stays there rather than
/// being sent to the map that gate-enters it.
fn is_town(map_id: i32) -> bool {
    map_id == 0 || map_id == REVIVE_PALACE.0
}

/// Where a revive sends a character, in `cltCharServer::Revive()`'s own order
/// (`char.cpp:4887-5100`).
///
/// Kept:
///
/// 1. **`level < INIT_LEVEL` → palace (158) at (27,87)** — `:5027-5046`, and it runs
///    *before* the village branch, so a beginner always lands in Hanyang even when a
///    nearer village resolved (`:5069-5085`).
/// 2. **A dungeon returns to the map it is entered from** — the hunt-map rule at
///    `:4914-4917` sets the parent map as the destination. This port has no map table,
///    so the parent is read off the gate table: the row that enters the map names it
///    (for the Raccoon Lair, row 561's source map 0 at (224,314) — its mouth).
/// 3. The palace fallback (`:5007-5013`).
///
/// Not modelled, and why: the instance-map branches (`:4955-5015`) need instance maps;
/// `KILLEDMODE_GOTOSAFEZONE` (`:5017-5025`) needs death modes; the PK-map switch
/// (`:5057-5068`) is off in this port's SERVERINFO; and the nearest-village branch
/// (`:5069-5085`, which warps to the village map's **centre** from the map type) needs
/// the village table (`Data\villageinfoE.txt`) and the map-type table, neither of which
/// is loaded here.
#[must_use]
pub(crate) fn revive_target(level: i16, map_id: i32) -> (i32, i32, i32) {
    if level < INIT_LEVEL {
        return REVIVE_PALACE;
    }
    if !is_town(map_id)
        && let Some(entrance) = GATES.iter().find(|g| g.dst_map == map_id)
    {
        return (entrance.src_map, entrance.src_x, entrance.src_y);
    }
    REVIVE_PALACE
}

/// `GAMEMSG_REQUEST_REVIVE` (7225) — the client's revive prompt.
///
/// The body is `cltGameMsgRequest_Revive` (`MsgType-Person.h:1448-1460`): three
/// `SI16`s — `siCharUnique`, `siParam`, `siDieMode`. The client fills it from its death
/// dialog (`NCommonMsgDlg.cpp:172-178`, `(charid, 1, NORMAL)`), and the handler is a
/// thin shell around `Revive()`: `Server-Person.cpp:3330-3345` validates the id, applies
/// a one-second throttle, and calls it — there is **no HP check**, so the packet is
/// honoured whatever state the character is in.
///
/// The warp itself reuses the gate path with a synthesised one-cell gate: a revive and a
/// gate differ only in how the destination is chosen, and both end in the same
/// `SetWarpInfo`/`Warp` pair the client is told about
/// (`char.cpp:5080-5084`). The gate path's own entry-item check sees 0/0 and stays out
/// of the way.
///
/// # Errors
///
/// A short body is logged and ignored, like the rest of this family; repository and
/// socket failures propagate.
pub(crate) async fn handle_revive(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    payload: &[u8],
) -> Result<(), AnyError> {
    const REVIVE_REQUEST_SIZE: usize = 6;
    if payload.len() < REVIVE_REQUEST_SIZE {
        logln!("game: malformed 7225 ({} bytes) — ignored", payload.len());
        return Ok(());
    }
    let named = i16::from_le_bytes([payload[0], payload[1]]);
    let die_mode = i16::from_le_bytes([payload[4], payload[5]]);
    // `:3341` — a request that does not name this session's character is dropped.
    if i32::from(named) != live.char_unique {
        logln!(
            "game: revive for char {named} ignored: this session is {}",
            live.char_unique
        );
        return Ok(());
    }
    let Some(character) = live.character.as_ref() else {
        logln!("game: revive before the character row is loaded — ignored");
        return Ok(());
    };
    let (from_map, level) = (character.map_id, character.level);
    let (dst_map, x, y) = revive_target(level, from_map);
    logln!(
        "game: revive: char {} (level {level}, mode {die_mode}) on map {from_map} -> map {dst_map} at ({x}, {y})",
        character.character_id
    );
    let gate = Gate {
        src_map: from_map,
        src_x: 0,
        src_y: 0,
        xsize: 1,
        ysize: 1,
        center_x: 0,
        center_y: 0,
        item_unique: 0,
        item_num: 0,
        dst_map,
        dst_x: x,
        dst_y: y,
    };
    warp_through_gate(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        &gate,
    )
    .await
}

/// One village entrance on the world map — the `FOTYPE_VILLAGE` branch's data.
///
/// `cltCharServer::CharServerMoveByGate`'s village half (`char.cpp:8983-9027`) takes the
/// village unique from the cell's field-object para (`:8986`) and warps to the village's
/// own map at that map type's centre (`:9008-9015`). The shape comes from text data —
/// `villageinfoE.txt` (`Unique`, `PosX`, `PosY`, `MapIndex`) → `mapinfoE.txt` (the map's
/// type unique) → `MapTypeInfo.txt` (type name) — but the landing `dst_x`/`dst_y` is the
/// port's own value, **(25,14)**: the live-tested town entry spawn, deliberately off both
/// legacy numbers. The engine never loads `MapTypeInfo_Village.txt` (`MapTypeManager.cpp:78`,
/// so its (24,22) is dead data) and would warp to its own `SetMapType` centre instead —
/// (25,13) for type 24 (`char.cpp:9011-9012`). See `docs/town-exit-gate.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Village {
    /// `villageinfoE.txt`'s `Unique` — the value the client's click carries in the
    /// field object's para.
    unique: i32,
    /// `PosX` on map 0.
    src_x: i32,
    /// `PosY` on map 0.
    src_y: i32,
    /// The entrance object's `TileXsize` — 8
    /// (`KoreanVillageSmall` / `KoreanVillageSmallUp`, both `8 8 1 1`).
    xsize: i32,
    /// See [`Village::xsize`].
    ysize: i32,
    /// The entrance object's `TileCenterX`.
    center_x: i32,
    /// See [`Village::center_x`].
    center_y: i32,
    /// `MapIndex` — the village's own map (`siVillageUnique`, `char.cpp:9008`).
    dst_map: i32,
    /// The town entry spawn, x — port-set to (25,14), the live-tested landing on entering
    /// a town (`village_warp_gate`). Neither legacy number ships: the engine reads its own
    /// `SetMapType` centre (`char.cpp:9011`, (25,13) for type 24) and the never-loaded
    /// `MapTypeInfo_Village.txt` says (24,22).
    dst_x: i32,
    /// The town entry spawn, y. See [`Village::dst_x`].
    dst_y: i32,
    /// The map type's engine gate anchor — `SetMapType`'s hardcoded `GateX`
    /// (`MapTypeManager.cpp:124` for every town; `MapTypeInfo_Village.txt` is dead data,
    /// its loader is commented out at `:78`). At map load `MapCommon.cpp:436-453` stamps
    /// `Gate_Wood1ToMain` (FO 1840, 4x3, centre (1,1)) over
    /// `[exit_x-2, exit_x+1] x [exit_y-1, exit_y+1]`. The town's *second* exit (FO 292
    /// at the engine's fixed (35,63)) is engine-constant, so it lives in
    /// [`village_exit2_block`] instead of this table; `GateInfoE.txt` has no row with a
    /// source in 1..42, so these two stamped blocks are the only exits the data names.
    exit_x: i32,
    /// See [`Village::exit_x`].
    exit_y: i32,
}

/// The village whose entrance is `(map_id, x, y)`, if any.
///
/// The entrance's *footprint* is the one number no text file carries — it lives in the
/// map's field-object grid, and unlike a gate row a village row names no object to read a
/// size from. So this matches the entrance tile itself for now, and
/// [`village_within`] reports the cells a player actually clicks so the block can be
/// measured rather than guessed.
impl Village {
    /// The stamped block as `(left, top, right, bottom)`, inclusive.
    pub(crate) const fn block(&self) -> (i32, i32, i32, i32) {
        let left = self.src_x + self.center_x - self.xsize + 1;
        let top = self.src_y + self.center_y - self.ysize + 1;
        (left, top, left + self.xsize - 1, top + self.ysize - 1)
    }

    /// Whether `(x, y)` is one of the entrance's own cells.
    pub(crate) const fn covers(&self, x: i32, y: i32) -> bool {
        let (left, top, _, _) = self.block();
        x >= left && x < left + self.xsize && y >= top && y < top + self.ysize
    }

    /// The exit gate's stamped block as `(left, top, right, bottom)`, inclusive.
    ///
    /// The exit object is always `Gate_Wood1ToMain` (FO 1840, hardcoded at
    /// `MapCommon.cpp:419`), 4x3 with centre (1,1) — `PutFieldObject`'s stamping rule
    /// then gives `x in [exit_x-2, exit_x+1]`, `y in [exit_y-1, exit_y+1]`.
    pub(crate) const fn exit_block(&self) -> (i32, i32, i32, i32) {
        (
            self.exit_x - 2,
            self.exit_y - 1,
            self.exit_x + 1,
            self.exit_y + 1,
        )
    }

    /// Whether `(x, y)` is one of the north gate's own stamped cells — exactly the cells
    /// the legacy resolves the field object on (`char.cpp:8975`).
    pub(crate) const fn covers_exit(&self, x: i32, y: i32) -> bool {
        let (left, top, right, bottom) = self.exit_block();
        x >= left && x <= right && y >= top && y <= bottom
    }
}

/// The village whose entrance `(map_id, x, y)` covers, if any.
///
/// This is the legacy's rule, cell for cell: the client sends the *cell under the cursor* as
/// an ordinary `ORDER_MOVE` — a hovered village only sets `MOUSEMODE_MOVE` and the gate
/// cursor (`MouseOperation.cpp:3333-3338`), the move path builds the order from
/// `clMouseObject` (`:5408-5439`), and every order leaves through
/// `cltClient::OrderOperation` (`Client.cpp:3284-3304`) with that same cell, whether the
/// click was adjacent or across the field. The server then resolves the field object on that
/// cell (`char.cpp:8975-8986`). There is no range anywhere in the client — a click from far
/// away already names the entrance's own cell — so none is invented here.
fn village_at(map_id: i32, x: i32, y: i32) -> Option<&'static Village> {
    (map_id == 0).then_some(())?;
    VILLAGES.iter().find(|v| v.covers(x, y))
}

/// Where a character leaving `village` arrives back on map 0: the first cell south of the
/// entrance block, on the road the player walked in from (the block itself is impassable).
fn entrance_landing(village: &Village) -> (i32, i32) {
    let (left, _, right, bottom) = village.block();
    ((left + right) / 2, bottom + 1)
}

/// The town maps' *second* exit gate's stamped block as `(left, top, right, bottom)`,
/// inclusive — `PutFieldObject`'s formula, the same way [`Village::exit_block`] reads it
/// for the north gate.
///
/// A town load stamps two gates: FO 292 `Gate3_MainToBear1` (`FOTYPE_GATE`,
/// `FieldObjectSetInfo.txt` row 292) on the engine's fixed anchor (35,63)
/// (`MapCommon.cpp:463,474-490`; Japan ships FO 7176 instead, `:457-464`). Its footprint
/// is its own detail file — `4 3 2 2`, centre (2,2), unlike FO 1840's `4 3 1 1`
/// (`FieldObject\Gate3_MainToBear1\Gate3_MainToBear1.txt`) — which stamps
/// `x in [34,37]`, `y in [63,65]`. The anchor fits `Village01.map` (63x68), the file
/// every town loads (all 42 are map type 24, `MapManager.cpp:851,865`); on a smaller
/// town file `PutFieldObject` would refuse the out-of-map anchor
/// (`Map-FieldObject.cpp:68`) and this block would not exist.
///
/// shortcut: engine-constant, not table data — if a build ever stamps a different
/// second gate per town, move the anchor into `VILLAGES`.
fn village_exit2_block() -> (i32, i32, i32, i32) {
    // (anchor x, anchor y, TileXsize, TileYsize, TileCenterX, TileCenterY)
    const GATE2: (i32, i32, i32, i32, i32, i32) = (35, 63, 4, 3, 2, 2);
    let (ax, ay, sx, sy, cx, cy) = GATE2;
    let left = ax + cx - sx + 1;
    let top = ay + cy - sy + 1;
    (left, top, left + sx - 1, top + sy - 1)
}

/// The exit a character on a village map orders to, and where it lands — either of the
/// town's two stamped gates.
///
/// Exact stamped cells, no reach: at map load the engine stamps `Gate_Wood1ToMain` over
/// the map type's anchor and FO 292 over (35,63) (`MapCommon.cpp:436-453`, `:474-490`),
/// and the legacy warps only when the ordered cell carries one of those field objects
/// (`char.cpp:8975`) — the 12 cells of [`Village::exit_block`] or of
/// [`village_exit2_block`]. The former six-tile reach around the anchor taken from the
/// never-loaded `MapTypeInfo_Village.txt` (23,18) both missed the real block at (25,7)
/// and swallowed the town centre, so a step in the stairs/spawn area warped too. Both
/// gates share one landing because the engine builds both from the same destination
/// (`MapCommon.cpp:431-432` and `:469-470`: `cltGate(..., 0, mapposx, mapposy, ...)`).
fn village_exit(map_id: i32, x: i32, y: i32) -> Option<(&'static Village, (i32, i32))> {
    let village = VILLAGES.iter().find(|v| v.dst_map == map_id)?;
    let (left2, top2, right2, bottom2) = village_exit2_block();
    let on_exit2 = (left2..=right2).contains(&x) && (top2..=bottom2).contains(&y);
    (village.covers_exit(x, y) || on_exit2).then(|| (village, entrance_landing(village)))
}

/// The village entrance nearest `(map_id, x, y)` within `radius`, and how far it is.
///
/// Diagnostic only, in the same spirit as [`gate_within`]: until the entrance block is
/// measured, a click that lands beside one must still be visible in the log.
fn village_within(map_id: i32, x: i32, y: i32, radius: i32) -> Option<(&'static Village, i32)> {
    if map_id != 0 {
        return None;
    }
    VILLAGES
        .iter()
        .filter_map(|v| {
            let (left, top, right, bottom) = v.block();
            let dx = (left - x).max(x - right).max(0);
            let dy = (top - y).max(y - bottom).max(0);
            let distance = dx.max(dy);
            (distance <= radius).then_some((v, distance))
        })
        .min_by_key(|(_, distance)| *distance)
}

/// The gate-shaped warp a village entry becomes.
///
/// A village and a gate differ only in how the destination is chosen; both end in the
/// same `SetWarpInfo`/`Warp` pair the client is told about (`char.cpp:9014-9015` versus
/// `:9014` for the gate family). The synthesised row carries no entry item, so the gate
/// path's own item check stays out of the way.
fn village_warp_gate(from_map: i32, village: &Village) -> Gate {
    warp_gate_to(from_map, village.dst_map, village.dst_x, village.dst_y)
}

/// A gate-shaped warp to anywhere: the village entry and exit and the revive path all end in
/// the same `SetWarpInfo`/`Warp` pair and differ only in how the destination is chosen.
fn warp_gate_to(from_map: i32, dst_map: i32, dst_x: i32, dst_y: i32) -> Gate {
    Gate {
        src_map: from_map,
        src_x: 0,
        src_y: 0,
        xsize: 1,
        ysize: 1,
        center_x: 0,
        center_y: 0,
        item_unique: 0,
        item_num: 0,
        dst_map,
        dst_x,
        dst_y,
    }
}

/// The gate nearest `(map_id, x, y)` within `radius` tiles, and how far it is.
///
/// Diagnostic only. The live run of 2026-10-13 had the player clicking (26,100),
/// (27,100), (26,101) and (28,99) while walking around the map-158 gate recorded at
/// (28,101) — adjacent cells, never the recorded cell, so no order destination ever
/// matched a gate. The client resolves a gate by the **field object** the player
/// clicked (`IsEnterGate(siGateUnique)`, `MouseOperation.cpp:5517`) and sends that
/// cell, so a gate object wider than one tile would explain it — but the object's
/// footprint lives in the binary map data, which is not pinned here. This lets the
/// next live run print the cell the client actually sent for a gate click, so the
/// footprint is measured instead of guessed.
fn gate_within(map_id: i32, x: i32, y: i32, radius: i32) -> Option<(&'static Gate, i32)> {
    GATES
        .iter()
        .filter(|g| g.src_map == map_id)
        .filter_map(|g| {
            let distance = (g.src_x - x).abs().max((g.src_y - y).abs());
            (distance <= radius).then_some((g, distance))
        })
        .min_by_key(|(_, distance)| *distance)
}
/// `MOVE_PERCENT_STEP` — the accumulator level at which the legacy commits a
/// tile (`clMoveInfo.IncMovePercent(movedlength) >= 50`, `char-move.cpp:159`).
const MOVE_PERCENT_STEP: i32 = 50;

/// `MOVE_PERCENT_COST` — what a committed tile subtracts from the accumulator
/// (`siMovePercent - 100`, `char-move.cpp:164`).
const MOVE_PERCENT_COST: i32 = 100;

/// `(CHAR_ACTION_INTERVAL_FRAME + 1) * 50` — the cap on one advance's elapsed
/// time, so a long stall cannot bank more than one tile (`char-move.cpp:145`;
/// `CHAR_ACTION_INTERVAL_FRAME` is 6 at `Char-Common.h`).
const MOVE_DELAY_CAP_MS: u32 = 350;

/// The direction bits the 7231 sit request carries (`CommonHeader.h:388-394`).
const NORTH: i32 = 16;
/// See [`NORTH`].
const EAST: i32 = 64;
/// See [`NORTH`].
const SOUTH: i32 = 1;
/// See [`NORTH`].
const WEST: i32 = 4;

/// The 8-way facings `DoMsg_GAMEMSG_REQUEST_SITDOWN` accepts
/// (`Server-Person.cpp:3299-3319`) — the `SOUTH|EAST`..`EAST` cases. Any other
/// value (0 included) is dropped rather than guessed at.
const SIT_DIRECTIONS: [i32; 8] = [
    SOUTH | EAST,
    SOUTH,
    SOUTH | WEST,
    WEST,
    WEST | NORTH,
    NORTH,
    NORTH | EAST,
    EAST,
];

/// The exact `Instant` when the next movement step will be ready.
///
/// `now` is the caller's clock, so the deadline stays a pure function of the
/// accumulator and the production path is testable; `main.rs` passes
/// `Instant::now()`.
///
/// # Why the result sits inside [`MOVE_DELAY_CAP_MS`]
///
/// [`Movement::advance`] credits **at most 350 ms** of elapsed time per call —
/// the legacy's `min((CHAR_ACTION_INTERVAL_FRAME + 1) * 50, ...)` per frame
/// (`char-move.cpp:145`) — so a wake further out than the cap *discards* the
/// excess instead of banking it, and the step lands late.
///
/// Measured 2026-10-10 with an uncapped deadline: at speed 10 the deadline
/// armed for the full 500 ms, `advance` credited only 350 ms (70 points, not
/// the 100 needed), so the loop re-armed for the missing 150 ms and the tile
/// committed at **650 ms** instead of 500 ms — a 30 % slowdown, visible in
/// `target/m1-logs/game.log` as `fanned 200` every ~650 ms. The observing
/// client animates one single-tile order per 200 and that cycle is exactly
/// `5000 / movespeed` ms, so any slower cadence leaves the avatar idle for
/// the difference: **one step, a pause, repeat.**
///
/// Capping keeps every gap between two [`Movement::advance`] calls inside the
/// credit window, so nothing is lost: the wake at the cap is a no-op and the
/// follow-up wake lands on the exact legacy cadence of `5000 / movespeed` ms
/// per tile.
pub(crate) fn next_move_deadline(movement: &Movement, now: Instant) -> Instant {
    now + movement.next_delay()
}

/// A move in progress, stepped the way `cltCharCommon::Move` steps it
/// (`char-move.cpp:38-227`): every tick the accumulator grows by
/// `movespeed * elapsed / 50`, a tile is committed once it reaches 50, and each
/// committed tile is broadcast as a 200 `CHARUPDATE`. The legacy server keeps
/// one of these per character and calls it once per frame; the read loop's timer
/// arm is the frame here.
///
/// Modelling the step rather than teleporting matters for authority: an item
/// dropped mid-walk has to land on the tile the character is actually on, and
/// the captured reference session shows the server broadcasting the start cell
/// and then each stepped cell (the captured run was 241, 200, 200).
pub(crate) struct Movement {
    /// Destination from the 240's `siPara1` (`x | (y << 16)`).
    pub(crate) dest: (i32, i32),
    /// The authoritative tile, advanced one step at a time.
    pub(crate) pos: (i32, i32),
    /// Map the move is on, for the drop cell.
    pub(crate) map_id: i32,
    /// The mover's effective `GetTotalMoveSpeed()` for the whole walk. Captured
    /// once when the order is answered, so gear changes mid-walk do not rescale
    /// an in-flight step.
    pub(crate) move_speed: i32,
    /// `clMoveInfo.siMovePercent`.
    pub(crate) percent: i32,
    /// When [`Movement::advance`] last ran, i.e. `clMoveInfo.dwLastMoveClock`.
    pub(crate) last: Instant,
    /// Absolute deadline for the next step. `None` when the move has arrived.
    /// Stored here so `step_movement` can update it after each step.
    pub(crate) deadline: Option<Instant>,
}

impl Movement {
    pub(crate) fn new(dest: (i32, i32), pos: (i32, i32), map_id: i32, move_speed: i32) -> Self {
        Self {
            dest,
            pos,
            map_id,
            move_speed,
            // Start at -MOVE_PERCENT_STEP so the first step also needs 100 points
            // (steady-state cadence 5000/speed ms). The legacy starts at 0, giving
            // a half-interval first step, but its frame-by-frame accumulation hides
            // the alternation. Our timer fires once per step, so the asymmetry
            // would produce visible step-and-pause (short/long/short/long intervals).
            percent: -MOVE_PERCENT_STEP,
            last: Instant::now(),
            deadline: None,
        }
    }

    /// Advances the accumulator and commits at most one tile
    /// (`char-move.cpp:145-171`). The step is limited by
    /// [`MOVE_DELAY_CAP_MS`], so a stalled tick banks no more than one tile.
    /// Returns the new cell when a tile was committed.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<(i32, i32)> {
        let cap = i64::from(MOVE_DELAY_CAP_MS);
        let delay = (now - self.last).as_millis().min(cap as u128) as i64;
        self.last = now;
        self.percent += (i64::from(self.move_speed) * delay / 50) as i32;
        if self.percent < MOVE_PERCENT_STEP {
            return None;
        }
        self.percent -= MOVE_PERCENT_COST;
        // One tile toward the destination; moving both axes at once is the
        // diagonal the legacy's `siNX++`/`siNY++` pair produces
        // (`char-move.cpp:107-126`).
        self.pos = (
            self.pos.0 + (self.dest.0 - self.pos.0).signum(),
            self.pos.1 + (self.dest.1 - self.pos.1).signum(),
        );
        Some(self.pos)
    }

    /// Whether the character has reached the destination.
    pub(crate) fn arrived(&self) -> bool {
        self.pos == self.dest
    }

    /// How long until the accumulator should next reach [`MOVE_PERCENT_STEP`]:
    /// `ceil(needed * 50 / movespeed)` ms, **capped at**
    /// [`MOVE_DELAY_CAP_MS`] exactly as one legacy tick is capped
    /// (`char-move.cpp:145`).
    ///
    /// The cap is load-bearing rather than cosmetic — see
    /// [`next_move_deadline`]: `advance` credits at most that many
    /// milliseconds per call, so the deadline must never be further out or
    /// the difference is silently dropped and every tile lands late.
    pub(crate) fn next_delay(&self) -> Duration {
        let needed = i64::from((MOVE_PERCENT_STEP - self.percent).max(1));
        let speed = i64::from(self.move_speed).max(1);
        let ms = ((needed * 50 + speed - 1) / speed)
            .min(i64::from(MOVE_DELAY_CAP_MS))
            .max(1);
        Duration::from_millis(ms as u64)
    }
}

/// Sends one 200 `CHARUPDATE` for the given cell (`cltServer::SendCharUpdate`,
/// `Server.cpp:1654-1689`). Life, mana and move speed come from the cached row,
/// because stats are not modelled yet.
/// Sends one 200 `CHARUPDATE` for the given cell.
///
/// `urgent=true` for teleports/long jumps (>3 tiles); `false` for normal steps.
pub(crate) async fn send_char_update(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &LiveFrames<'_>,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<(), AnyError> {
    let character = live
        .character
        .as_ref()
        .ok_or("char update before the character row is loaded")?;
    let body = char_update_body(live.char_unique, character, x, y, urgent)?;
    send_encoded(stream, outbound, GameMsg::ResponseCharupdate, &body).await
}

/// Builds the 200 `CHARUPDATE` body for one cell, as
/// `cltServer::SendCharUpdate` builds `cltBasicCharUpdateInfo`
/// (`Server.cpp:1654-1698`).
///
/// Factored out of [`send_char_update`] so the observer fan ([`fan_char_update`])
/// can queue byte-identical bodies without a second encoder that could drift, and
/// taken as a row rather than a `LiveFrames` so it is testable without a database.
///
/// # Errors
///
/// A `char_unique` or tile that does not fit the struct's narrower fields.
/// Builds the 200 `CHARUPDATE` body for one cell.
///
/// `urgent` controls `bUrgentMode`:
/// - `true`  -> teleports / long jumps (>3 tiles) where the client must snap and animate via `MoveTo`+`MoveInit`.
/// - `false` -> normal single-tile steps. The 241 ORDER_MOVE already told the client the destination;
///   200 packets are just position confirmations. Sending `b_urgent_mode=true` every tile would
///   call `MoveTo`+`MoveInit` repeatedly, resetting the animation and causing step-and-pause.
pub(crate) fn char_update_body(
    char_unique: i32,
    character: &CharacterFull,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<Vec<u8>, AnyError> {
    let update = BasicCharUpdate {
        si_char_unique: i16::try_from(char_unique).map_err(|_| {
            format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
        })?,
        // The character's effective `GetTotalMoveSpeed()`; the client uses it for
        // the avatar's step animation rate.
        si_move_speed: i8::try_from(character.move_speed.clamp(0, i16::from(i8::MAX))).unwrap_or(0),
        // `bUrgentMode`:
        // - `true`  -> teleport / long jump (>3 tiles): client calls `MoveTo` + `MoveInit`.
        // - `false` -> normal step: 241 ORDER_MOVE already gave destination; 200 is just confirmation.
        b_urgent_mode: urgent,
        si_life: character.max_life,
        si_reserved_life: 0,
        si_mana: character.max_mana,
        si_x: i16::try_from(x).map_err(|_| format!("x {x} does not fit SI16"))?,
        si_y: i16::try_from(y).map_err(|_| format!("y {y} does not fit SI16"))?,
        ui_gm_mode: 0,
        b_pc_room: false,
        si_make_ani_type: 0,
    };
    Ok(update.encode().to_vec())
}

/// Queues the 200 `CHARUPDATE` for every character in the mover's 3×3 near cell,
/// self included — T-018 slice 6 B1.
///
/// The legacy fans exactly this frame: `SendCharUpdate` builds one
/// `cltBasicCharUpdateInfo` and hands it to
/// `pclchar->SendNeighbourMsg(&clMsg, true)` (`Server.cpp:1689`), which lands on
/// `pCell->SendPacketNearAll` over the `m_pNearCell[9]` block — the 3×3 cells
/// around the sender's, self included (`Char-Neighbour.cpp:29-78`,
/// `ITZMap.cpp:71-87`). The `true` is the self-inclusion flag.
///
/// # What this fan is for: position and walk animation
///
/// Not HP. The client never draws a bystander's health bar — the floating gauge
/// is `cltNPersonStatus::DrawCharStatus`, which hardcodes `pclCM->CR[1]`
/// (`NCharPerson.cpp:3920`) and `CR[1]` is the local player
/// (`Client-Winsock.cpp:130`, and the client's own
/// `charunique == MyCharUnique → charid = 1` at
/// `Client-Person-BigInfo.cpp:1160-1163`). Life and mana do ride along in the
/// struct and are written to the remote object
/// (`SetLife`/`SetMana`, `:1228-1238`); they just stay unrendered for a bystander.
///
/// The fields an observer actually acts on are `siX`/`siY`/`siMoveSpeed`, in the
/// branch the client runs *only* for other characters
/// (`Client-Person-BigInfo.cpp:1187-1222`):
///
/// ```text
/// SI32 length = max( TABS(x - CR[charid]->GetX()), TABS(y - CR[charid]->GetY()));
/// if(length && bselfswitch == false) { ... OrderMove(charid, x, y, BY_COMPUTER); ... }
/// CR[charid]->MoveTo(x, y);
/// CR[charid]->MoveInit();
/// ```
///
/// `bselfswitch == false` is why the mover's own client animates itself off the
/// 241 and needs nothing from a 200 (slice 3), and its absence on the position
/// branch is why observers need this fan. Without it a second client's copy of
/// another avatar is stranded on whatever tile it was created at.
///
/// A send to a session that just disconnected is dropped, not an error — the same
/// tolerance `Registry::send_to` documents for the chat paths.
///
/// Self is **excluded** ([`Registry::broadcast_near_others`]) because every caller
/// also writes the frame to its own socket directly, and one writer per connection
/// keeps a queued copy from overtaking a later direct one.
///
/// `urgent=true` for teleports/long jumps (>3 tiles); `false` for normal steps.
///
/// # Errors
///
/// No character row yet, or a tile/`char_unique` that does not fit the struct.
pub(crate) fn fan_char_update(
    registry: &Registry,
    session_key: SessionKey,
    char_unique: i32,
    character: &CharacterFull,
    x: i32,
    y: i32,
    urgent: bool,
) -> Result<usize, AnyError> {
    let body = char_update_body(char_unique, character, x, y, urgent)?;
    let origin = registry
        .info(session_key)
        .ok_or("char update fan before the session is in the registry")?;
    Ok(registry.broadcast_near_others(
        &origin,
        session_key,
        GameMsg::ResponseCharupdate as u16,
        body,
    ))
}

/// Sends `GAMEMSG_RESPONSE_LOGOUT` (11) to every character that could see the
/// mover at `was` and cannot see it at `now` — the counterpart of the 200 fan
/// and of [`announce_nearby_characters`]. T-018 slice 6 B3a.
///
/// Both directions of a cell crossing have to exist, and they are the same event
/// seen from two ends: someone arriving is an *appearance* (a 6 naming them) and
/// someone leaving is a *disappearance* (an 11 removing them). Shipping only the
/// first half is what left avatars frozen on screen when their owner walked off
/// — the client kept the object it had created and simply stopped being told
/// where it was, until its own screen culling deleted it.
///
/// `was` is the mover's registry entry **before** the tile step and `now` the one
/// after, which is the whole input: the recipient set is `near(was) - near(now)`
/// ([`Registry::no_longer_near`]). The legacy carries that difference in
/// `m_pRemoveCell` (`ITZMap.cpp:73-150`) and hands it to `SendPacket`
/// (`ServerMap.cpp:58-61`); recomputing it is the same answer, because the
/// neighbourhood is a pure function of the tile.
///
/// Returns how many connections took the frame.
pub(crate) fn logout_departed(
    registry: &Registry,
    was: &SessionInfo,
    now: &SessionInfo,
    mover_key: SessionKey,
    char_unique: i32,
) -> usize {
    let payload = encode_logout(char_unique);
    let mut sent = 0;
    for (key, _) in registry.no_longer_near(was, now, mover_key) {
        if registry.send_to(key, GameMsg::ResponseLogout as u16, payload.clone()) {
            sent += 1;
        }
    }
    sent
}

/// Advances an in-flight move by one tick. When the tick commits a tile it sends
/// the 200 `CHARUPDATE` for the new cell, updates the session's drop cell and
/// the registry's copy of the position (so the near fan sees where the avatar
/// actually stands), and ends the move on arrival. A committed step never warps:
/// the legacy arms a warp only from an order's destination (`char-order.cpp:173`), so
/// stepping onto an entrance block or a gate cell keeps walking. A no-op when no
/// move is active or the tile is not due yet.
///
/// # Errors
///
/// A socket or cipher failure while sending the update.
pub(crate) async fn step_movement(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    live: &mut LiveFrames<'_>,
    movement: &mut Option<Movement>,
    registry: &Registry,
    session_key: SessionKey,
) -> Result<(), AnyError> {
    let Some(mv) = movement.as_mut() else {
        return Ok(());
    };
    let Some((x, y)) = mv.advance(Instant::now()) else {
        return Ok(());
    };
    // A movement belongs to one map. A warp clears it (the gate path returns no
    // movement), so a movement whose map no longer matches the row's is stale —
    // stepping it would write a tile on the map the character left.
    if live
        .character
        .as_ref()
        .is_some_and(|c| c.map_id != mv.map_id)
    {
        mv.deadline = None;
        *movement = None;
        return Ok(());
    }
    // A committed step never leaves the map.
    //
    // The legacy arms a warp in exactly one place: the order handler, over the
    // order's *destination* (`char-order.cpp:173` is `CharServerMoveByGate`'s only
    // caller). Warping on the committed tile as well was this port's own widening,
    // and it misfired the way it was always going to: an entrance block is the
    // town's whole stamped footprint, so standing on the open ground beside
    // Ankaran's wall and taking one step pulled the character into town (owner
    // report, Test12). Walking past a town or across a gate is now just a walk;
    // entering, leaving and gates fire only from an explicit order, which
    // `handle_userorder` checks on the destination.
    if mv.arrived() {
        mv.deadline = None;
        *movement = None;
    }
    // The stepped tile is now the authoritative cell, and it has to become so
    // *before* anything is built from the row: `announce_nearby_characters`
    // stamps `live.character`'s position into the `LOGINMANY` record it hands
    // to the neighbours. `handle_userorder` wrote the **destination** there
    // when the walk was ordered, so announcing from a stale row placed the
    // avatar at the far end of the path (or one tile behind) and the client,
    // which deletes and re-creates on a repeat record, warped to it.
    //
    // A 248 resync mid-walk and a re-issued 240 have to see this tile too, not
    // the destination the row was set to.
    if let Some(character) = live.character.as_mut() {
        character.pos_x = x;
        character.pos_y = y;
    }
    // The legacy fans this frame to the 3×3 near block (`Server.cpp:1689`), so a
    // bystander's copy of this avatar steps with ours instead of standing still
    // (slice 6 B1). `update_pos` runs *before* the fan so the observer set is
    // computed from the cell we have just reached, not the one we left.
    let was = registry.info(session_key);
    let was_cell = was.as_ref().map(|i| cell_of(i.x, i.y, i.map_id));
    registry.update_pos(session_key, x, y);
    let observers = {
        let character = live
            .character
            .as_ref()
            .ok_or("char update fan before the character row is loaded")?;
        fan_char_update(
            registry,
            session_key,
            live.char_unique,
            character,
            x,
            y,
            false,
        )?
    };
    let now = registry.info(session_key);
    if let (Some(was), Some(now)) = (was.as_ref(), now.as_ref())
        && was_cell.is_some_and(|was| was != cell_of(x, y, now.map_id))
    {
        // Crossing a cell boundary is an *appearance* event, not just a movement
        // one.
        //
        // The legacy maintains a `m_pNewCell` set per character and runs
        // `SendNewOthersInfoToPerson` for it on cell entry, not only on world entry
        // (`ServerMap.cpp:73-150`, and the `m_uiNewCellCount` walk). Without this, a
        // player who walks up to somebody is never told they exist: no LOGINMANY, so no
        // avatar, so the 200 fan above is discarded on arrival.
        if let Some(character) = live.character.as_ref() {
            announce_nearby_characters(
                registry,
                session_key,
                character,
                live.char_unique,
                Some(was),
            )?;
        }
        // ...and the mirror image, which was missing entirely until now: the people
        // who *had* us and no longer do have to be told the avatar is gone.
        //
        // `SendPersonLeaveMsgToOthers` (`ServerMap.cpp:51-63`) sends
        // `GAMEMSG_RESPONSE_LOGOUT` to `m_pRemoveCell` — the cells the character is
        // leaving — and the client answers it by deleting the object
        // (`Client-Login.cpp:396-417`). Without the frame a departing avatar simply
        // stops receiving 200s and lingers on screen until the client's own screen
        // culling drops it, which is the abrupt pop at the screen edge.
        let left = logout_departed(registry, was, now, session_key, live.char_unique);
        if left > 0 {
            logln!("game: char {session_key} sent 11 to {left} observer(s) that lost sight of it");
        }
    }

    send_char_update(stream, outbound, live, x, y, false).await?;
    if observers > 0 {
        logln!("game: char {session_key} fanned 200 to {observers} observer(s) at ({x}, {y})");
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_USERORDER` (240). The only order acted on is
/// `ORDER_MOVE`; every other order type is logged and dropped.
///
/// `siPara1` carries the destination as `x | (y << 16)`: the client packs it that
/// way (`cltOrder::MakeOrder`, `order.cpp:148`) and the `ORDER_MOVE` handler
/// unpacks it with `LOWORD`/`HIWORD` (`char-order.cpp:139-140`).
///
/// The reply mirrors the legacy order handler: a 241 order ack built the way
/// `MakeOrder_MOVE` builds it (`order.cpp:257-262`), then a 200 `CHARUPDATE` of
/// the cell the move starts from. The legacy then steps the character a tile at a
/// time and broadcasts the new cell on each step (`char-move.cpp:159-182`); the
/// returned [`Movement`] drives the same behaviour from the read loop, so the
/// avatar lands where it was sent and a mid-walk drop lands underfoot.
///
/// The destination is persisted to `characters.pos_x`/`pos_y` and mirrored into
/// the cached row up front, so a later 248/258 re-generates at the new cell and
/// a re-login resumes there even if the connection drops mid-walk.
///
/// Returns the move to drive; `None` for an order that is not modelled or a
/// click on the current cell.
///
/// # Errors
///
/// A malformed body, a database failure, or a socket/cipher failure.
/// The exit branch of [`handle_userorder`], run on the **order's destination** before the
/// walk is persisted — exactly where the legacy checks it (`char-order.cpp:173` →
/// `CharServerMoveByGate(x, y)`). `true` when a stamped exit gate was hit: the warp to
/// map 0 has replaced the walk and the order ends. `false` for any other tile, which
/// then walks on.
///
/// Both town exits lead the same way. At every town's load `MapCommon.cpp` stamps the
/// north `Gate_Wood1ToMain` on the map type's anchor (`:436-453`) *and* FO 292
/// `Gate3_MainToBear1` on the fixed (35,63) (`:463,474-490`), each from the same
/// `cltGate(siVillageUnique, ..., 0, mapposx, mapposy, false)` (`:431-432`, `:469-470`) —
/// so whichever gate the destination names, the landing is the road just south of the
/// entrance block on map 0, and `GateInfoE.txt` has no row that leaves a village.
#[allow(clippy::too_many_arguments)]
async fn warp_out_of_village(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    map_id: i32,
    dest: (i32, i32),
) -> Result<bool, AnyError> {
    let Some((village, landing)) = village_exit(map_id, dest.0, dest.1) else {
        return Ok(false);
    };
    logln!(
        "game: village {} exit: map {map_id} ordered {dest:?} -> map 0 at {landing:?}",
        village.unique
    );
    let gate = warp_gate_to(map_id, 0, landing.0, landing.1);
    warp_through_gate(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        &gate,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn handle_userorder(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    order: OrderData,
) -> Result<Option<Movement>, AnyError> {
    if order.si_order != ORDER_MOVE {
        logln!(
            "game: ignoring USERORDER opcode {} (only ORDER_MOVE={ORDER_MOVE} is modelled)",
            order.si_order
        );
        return Ok(None);
    }

    let Some(character) = live.character.as_ref() else {
        return Err("userorder before the character row is loaded".into());
    };
    let character_id = character.character_id;
    let map_id = character.map_id;
    let start = (character.pos_x, character.pos_y);
    let move_speed = i32::from(character.move_speed);

    let (x16, y16) = OrderData::move_target(order.si_para1);
    let dest = (i32::from(x16), i32::from(y16));

    // The legacy answers the order in the handler itself (`char-order.cpp:169`);
    // the position reaches the database on a separate flush, so the client's
    // local walk is not held up by a round trip here.
    //
    // One 241 goes to the mover and the *same* bytes fan to the observers: the
    // legacy's `SendNeighbourMsg(&clMsg, true)` (`char-order.cpp:170`) writes a
    // single frame to the whole 3×3 block, the mover included, so there is no
    // second self-copy to send here.
    let order_241_body: Vec<u8> = OrderData::move_order(live.char_unique, x16, y16)
        .encode()
        .to_vec();
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseUserorder,
        &order_241_body,
    )
    .await?;
    // The observer's client turns this 241 into the walk to the destination
    // (`DoMsg_GAMEMSG_RESPONSE_USERORDER` → `LetOrder` → `PushOrder`,
    // `Client-SystemNPC.cpp:538/689`); without it a bystander only sees the
    // per-tile 200s and animates each tile as a discrete jump.
    //
    // Build a temporary SessionInfo for the mover to fan the 241.
    let mover_session_info = session_info_for(live);
    let fanned_241 = registry.broadcast_near_others(
        &mover_session_info,
        session_key,
        GameMsg::ResponseUserorder as u16,
        order_241_body.clone(),
    );
    if fanned_241 > 0 {
        logln!(
            "game: char {session_key} fanned 241 to {fanned_241} observer(s) at ({}, {})",
            start.0,
            start.1
        );
    }
    // The reference server broadcasts the cell the move starts from, then one
    // 200 per committed tile (the captured run was 241, 200, 200). The start-cell
    // 200 is fanned too (slice 6 B1): the legacy's `SendCharUpdate` is one call for
    // both, so an observer that missed the origin tile would otherwise start its
    // walk from a stale one. Self still gets it directly, before the fan, so the
    // ordering the capture shows (241, 200, 200…) is preserved for this client.
    send_char_update(stream, outbound, live, start.0, start.1, false).await?;
    fan_char_update(
        registry,
        session_key,
        live.char_unique,
        live.character
            .as_ref()
            .ok_or("userorder before the row is loaded")?,
        start.0,
        start.1,
        false,
    )?;

    // Leaving a town runs on the **order's destination** before the walk is persisted,
    // where the legacy checks it (`char-order.cpp:173`) — a hit on either of the town's
    // two stamped gates replaces the walk with a warp; see `warp_out_of_village`.
    if warp_out_of_village(
        stream,
        outbound,
        characters,
        registry,
        session_key,
        live,
        map_id,
        dest,
    )
    .await?
    {
        return Ok(None);
    }
    // The village entrances are checked on the same seam, and before the gates: the
    // legacy runs one object lookup over the ordered cell and dispatches on its type
    // (`char.cpp:8975-8983` village, `:9028` gate).
    if let Some(village) = village_at(map_id, dest.0, dest.1) {
        logln!(
            "game: village {} entered: map {map_id} ({}, {}) -> map {} at ({}, {})",
            village.unique,
            village.src_x,
            village.src_y,
            village.dst_map,
            village.dst_x,
            village.dst_y
        );
        let gate = village_warp_gate(map_id, village);
        warp_through_gate(
            stream,
            outbound,
            characters,
            registry,
            session_key,
            live,
            &gate,
        )
        .await?;
        return Ok(None);
    }
    if let Some((village, distance)) = village_within(map_id, dest.0, dest.1, 3) {
        logln!(
            "game: move order to {dest:?} on map {map_id} is {distance} tile(s) from the village \
             {} entrance at ({}, {}) — if the client sent this cell for a village click, the \
             entrance's block is wider than the table",
            village.unique,
            village.src_x,
            village.src_y
        );
    }
    if let Some(gate) = gate_at(map_id, dest.0, dest.1) {
        warp_through_gate(
            stream,
            outbound,
            characters,
            registry,
            session_key,
            live,
            gate,
        )
        .await?;
        // No step is armed: the character is not walking to that tile, it left the
        // map. The read loop drops the previous movement when this returns `None`.
        return Ok(None);
    }
    // Not a gate. If the order landed *near* one, say so: the client resolves a gate
    // by the field object the player clicked and sends that cell, so a click on a
    // gate we do not recognise is how a wider-than-one-tile gate would show up. See
    // [`gate_within`].
    if let Some((gate, distance)) = gate_within(map_id, dest.0, dest.1, 2) {
        logln!(
            "game: move order to {dest:?} on map {map_id} is {distance} tile(s) from the gate \
             recorded at ({}, {}) -> map {} — if the client sent this cell for a gate click, the \
             gate's footprint is wider than the table",
            gate.src_x,
            gate.src_y,
            gate.dst_map
        );
    }

    // Persist the destination, then mirror: a later 248 (resync) or a re-login
    // reads the row. The stepped cell is tracked in memory for drops.
    characters
        .update_position(character_id, dest.0, dest.1)
        .await?;
    if let Some(character) = live.character.as_mut() {
        character.pos_x = dest.0;
        character.pos_y = dest.1;
    }

    logln!("game: move char {character_id} from {start:?} to {dest:?} on map {map_id}");

    if start == dest {
        return Ok(None);
    }
    Ok(Some(Movement::new(dest, start, map_id, move_speed)))
}

/// Moves a character through `gate`: the map change, the frames that carry it, and
/// every store that has to agree afterwards.
///
/// The legacy's shape is `clCharServer::CharServerMoveByGate` → `SetWarpInfo` →
/// `Warp(true)` (`char.cpp:9317-9345`, `:4043`), and a warp is literally "re-send
/// the login packet": `char.cpp:4574` builds the character's own
/// `cltMakeCharInfo` for the new map and sends it as `GAMEMSG_RESPONSE_LOGIN` (5).
/// The client then loads the destination map from its own files (`map/MainMap/
/// MainMap.map`) — terrain never rides the wire — and the ordinary cell-join path
/// tells it who (and which NPCs) are already there.
///
/// Order matters: the row, the cached character and the registry entry move
/// first, because every frame below is built from them and a stale map on any of
/// the three would aim the next fan at the map the character just left.
///
/// # Errors
///
/// Propagates database, encoder and socket failures.
async fn warp_through_gate(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    live: &mut LiveFrames<'_>,
    gate: &Gate,
) -> Result<(), AnyError> {
    // The entry requirement first, exactly where the legacy checks it
    // (`char.cpp:9123-9133`) — before anything moves, and without consuming the item.
    if gate.item_unique > 0 && gate.item_num > 0 {
        let character_id = live
            .character
            .as_ref()
            .ok_or("gate entry before the character row is loaded")?
            .character_id;
        let have: i64 = live
            .items
            .list(character_id)
            .await?
            .iter()
            .filter(|row| row.item_unique == gate.item_unique)
            .map(|row| i64::from(row.count))
            .sum();
        if have < i64::from(gate.item_num) {
            logln!(
                "game: gate {} -> map {} refused: needs item {} x{}, character {character_id} has {have}",
                gate.src_map,
                gate.dst_map,
                gate.item_unique,
                gate.item_num
            );
            return Ok(());
        }
    }
    let warp = apply_gate_move(
        characters,
        registry,
        session_key,
        live.char_unique,
        &mut live.character,
        gate,
    )
    .await?;
    let character_id = warp.moved.character_id;

    // The map change itself: opcode 5 with this character's record.
    let info = make_char_info_for(&warp.moved, live.char_unique)?;
    send_encoded(stream, outbound, GameMsg::ResponseLogin, &info.encode()).await?;

    // Both ends of the move. The departure fan is computed against the *new* cell
    // in the *new* map, so every session still near the old tile gets the 11 and
    // nobody on the destination gets a spurious one.
    let left = logout_departed(
        registry,
        &warp.was,
        &warp.now,
        session_key,
        live.char_unique,
    );
    announce_nearby_characters(registry, session_key, &warp.moved, live.char_unique, None)?;
    // The destination's own NPCs, which the client has never been told about.
    announce_map_npcs(registry, session_key, warp.now.map_id);

    logln!(
        "game: char {character_id} took the gate {} -> map {} at ({}, {}); {left} observer(s) \
         on the old map were told",
        warp.was.map_id,
        gate.dst_map,
        gate.dst_x,
        gate.dst_y
    );
    Ok(())
}

/// What a gate move changed, before any frame is built from it.
struct GateWarp {
    /// The character row, already moved to the destination.
    moved: CharacterFull,
    /// The near-fan view *before* the move — the set the departure 11 is diffed
    /// against (`logout_departed`).
    was: SessionInfo,
    /// The same view after it.
    now: SessionInfo,
}

/// Applies a gate move to every store that names the character's place: the row,
/// the cached copy and the registry entry.
///
/// Separated from the frames so the decision is testable without a socket or a
/// database — the same split the quest handlers use (T-019 slice 3 review, F2).
///
/// # Errors
///
/// [`gz_db::RepositoryError`] from the map write, which the caller propagates.
async fn apply_gate_move(
    characters: &dyn CharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    char_unique: i32,
    character: &mut Option<CharacterFull>,
    gate: &Gate,
) -> Result<GateWarp, AnyError> {
    let was = session_info_from(character, char_unique);
    let row = character
        .as_mut()
        .ok_or("a gate move before the character row is loaded")?;

    characters
        .update_map_position(row.character_id, gate.dst_map, gate.dst_x, gate.dst_y)
        .await?;
    row.map_id = gate.dst_map;
    row.pos_x = gate.dst_x;
    row.pos_y = gate.dst_y;
    let moved = row.clone();
    registry.update_map_pos(session_key, gate.dst_map, gate.dst_x, gate.dst_y);

    let now = SessionInfo {
        map_id: moved.map_id,
        x: moved.pos_x,
        y: moved.pos_y,
        ..was.clone()
    };
    Ok(GateWarp { moved, was, now })
}

/// The [`SessionInfo`] the near fan keys off, built from the connection's live
/// row. Shared by the move 241 fan and the sit/stand 241 fan so both compute the
/// observer set from the one character state.
fn session_info_for(live: &LiveFrames<'_>) -> SessionInfo {
    session_info_from(&live.character, live.char_unique)
}

/// [`session_info_for`] over a bare row, so a test can build the same view
/// without a connection.
fn session_info_from(character: &Option<CharacterFull>, char_unique: i32) -> SessionInfo {
    let character = character.as_ref();
    SessionInfo {
        char_unique: char_unique as i16,
        name: character.map(|c| c.name.clone()).unwrap_or_default(),
        level: character.map(|c| c.level as i32).unwrap_or(1),
        map_id: character.map(|c| c.map_id).unwrap_or(0),
        x: character.map(|c| c.pos_x).unwrap_or(0),
        y: character.map(|c| c.pos_y).unwrap_or(0),
        school_id: character.map(|c| c.school_id).unwrap_or(0),
        class_kind: character.map(|c| c.class_kind as u16).unwrap_or(1),
        life: character.map(|c| c.max_life as u32).unwrap_or(161),
        move_speed: character
            .map(|c| c.move_speed.clamp(0, 63) as u8)
            .unwrap_or(12),
        sex: character.map(|c| c.gender as u8).unwrap_or(1),
        gm_level: 0,
    }
}

/// Applies `GAMEMSG_REQUEST_SITDOWN` (7231) — the sit/stand toggle, T-018
/// slice 6 B4's sit/stand leg.
///
/// The client sends this opcode, **not** a 240 `ORDER_SITDOWN`
/// (`MouseOperation.cpp:4132`): the 240/241 order in [`handle_userorder`] is a
/// different flow. Here the server's answer is the 241 `RESPONSE_USERORDER` that
/// tells the sitter's own client and every observer which stance to draw.
///
/// `bSitDown` picks `ORDER_SITDOWN` (the 8-way facing rides in `siPara1`) or
/// `ORDER_FORCESTOP` (`Server-Person.cpp:3297-3324`). A sit whose facing is not
/// one of the eight the legacy accepts is dropped, as the legacy's `default:`
/// returns (`:3317`). The combat leg of B4 (the 268 hit fan) is deliberately not
/// here.
///
/// # Errors
///
/// A malformed body, or a socket/cipher failure.
pub(crate) async fn handle_sit_down(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    registry: &Registry,
    session_key: SessionKey,
    live: &LiveFrames<'_>,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = SitDown::decode(payload)?;
    let order = if request.b_sit_down {
        if !SIT_DIRECTIONS.contains(&request.direction) {
            logln!(
                "game: sitdown facing {} -> no reply (legacy drops a non-8-way \
                 direction; Server-Person.cpp:3317)",
                request.direction
            );
            return Ok(());
        }
        OrderData::sit_order(live.char_unique, request.direction)
    } else {
        OrderData::force_stop_order(live.char_unique)
    };
    let body = order.encode().to_vec();
    // Self gets the frame directly — its client only adopts the stance once the
    // 241 lands, since `OperateAutoLeftOrder` just sends the request — then the
    // same bytes fan to the near block, self excluded, exactly as the move 241
    // does (`char-order.cpp:526` `SendNeighbourMsg(..., true)`).
    send_encoded(stream, outbound, GameMsg::ResponseUserorder, &body).await?;
    let origin = session_info_for(live);
    let fanned = registry.broadcast_near_others(
        &origin,
        session_key,
        GameMsg::ResponseUserorder as u16,
        body,
    );
    logln!(
        "game: sitdown char {} {} (order {}) fanned 241 to {fanned} observer(s)",
        live.char_unique,
        if order.si_order == ORDER_SITDOWN {
            "sit"
        } else {
            "stand"
        },
        order.si_order
    );
    Ok(())
}

/// Persists the character and acknowledges a client leave request (107 quit or
/// 109 return-to-front).
///
/// The legacy handles both leave requests the same way:
/// `DoMsg_GAMEMSG_REQUEST_HOPETOQUIT` (`Server-System.cpp:1195`) and
/// `DoMsg_GAMEMSG_REQUEST_HOPETOFRONT` (`:1239`) guard against auto-hunt/PVP and
/// death, set `bHopeToQuitSwitch`, and answer with the matching empty 108/110
/// frame. The port has no auto-hunt, no PVP flag, and `life` never reaches zero,
/// so those guards cannot fire; what remains is the save and the ack.
///
/// The client does the rest: 108 makes it quit
/// (`Client-System.cpp:3002`) and 110 makes it close the game socket and return
/// to front (`:3025`). The handler deliberately does **not** close the session:
/// the client's own close is gated on the socket still being open (see the
/// caller), so the server has to leave it for the peer.
///
/// The character cell is written before the ack. Every `ORDER_MOVE` persists its
/// destination already, so this only matters when the character did not move
/// this session; it mirrors `SendDB_PosLifeMana` on the legacy
/// `cltCharServer::Close` path (`CharServer-Session.cpp:687`). No character is
/// selected only if the client leaves before `REQUEST_SELECTCHAR`, in which case
/// there is nothing to save and the ack is still sent.
///
/// # Errors
///
/// A database failure while saving, or a socket/cipher failure while sending.
pub(crate) async fn handle_leave_request(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    character: Option<&CharacterFull>,
    ack: GameMsg,
) -> Result<(), AnyError> {
    // Where the character stands is the row's business, not a session copy's:
    // `step_movement` re-seats the row per committed tile and a gate warp moves
    // it with the map, so this save follows a warp instead of undoing it.
    if let Some(character) = character {
        characters
            .update_position(character.character_id, character.pos_x, character.pos_y)
            .await?;
    }
    send_encoded(stream, outbound, ack, &[]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gz_db::{PgItemRepository, RepositoryError};

    /// The gate table is a transcription of `Data\GateInfoE.txt`, so the test
    /// states the file's own facts: Hanyang's three exits land on the outdoor map
    /// at its gate tile, and the outdoor map's way back lands on the palace tile
    /// the login spawn uses.
    #[test]
    fn the_gate_table_matches_the_shipped_gate_rows() {
        assert_eq!(gate_at(158, 14, 90).unwrap().dst_map, 0);
        assert_eq!(gate_at(158, 28, 101).unwrap().dst_map, 0);
        assert_eq!(gate_at(158, 41, 90).unwrap().dst_map, 0);
        for (x, y) in [(14, 90), (28, 101), (41, 90)] {
            let gate = gate_at(158, x, y).unwrap();
            assert_eq!(
                (gate.dst_x, gate.dst_y),
                (288, 354),
                "one shared landing tile"
            );
        }
        // Row 306, the reverse: the destination is the (27,87) the spawn uses
        // (`Server-Login.cpp:375`).
        let back = gate_at(0, 291, 350).unwrap();
        assert_eq!((back.dst_map, back.dst_x, back.dst_y), (158, 27, 87));

        // A tile that is not a gate is not one, on either map.
        assert!(
            gate_at(158, 27, 87).is_none(),
            "the spawn tile is not a gate"
        );
        assert!(
            gate_at(158, 288, 354).is_none(),
            "the landing tile is on map 0"
        );
        assert!(gate_at(1, 14, 90).is_none(), "gates are per-map");
        // The whole file is loaded now, so the table is no longer only Hanyang's pair:
        // the outdoor map alone carries dozens of gates (46 rows start on map 0).
        let outdoor = GATES.iter().filter(|g| g.src_map == 0).count();
        assert!(outdoor >= 40, "only {outdoor} gate(s) start on map 0");
        // `PutFieldObject` refuses a block that overlaps one already stamped
        // (`Map-FieldObject.cpp:88-97`), and the client's gate table is keyed by unique,
        // so two rows can never own the same cell. `gate_at` returns the first match, so
        // an overlap would also make which gate wins depend on row order.
        for (i, gate) in GATES.iter().enumerate() {
            for other in &GATES[i + 1..] {
                if gate.src_map != other.src_map {
                    continue;
                }
                let (l1, t1, r1, b1) = gate.block();
                let (l2, t2, r2, b2) = other.block();
                assert!(
                    r1 < l2 || r2 < l1 || b1 < t2 || b2 < t1,
                    "gate blocks overlap on map {}: ({l1},{t1})-({r1},{b1}) and ({l2},{t2})-({r2},{b2})",
                    gate.src_map
                );
            }
        }
        // No two rows share an anchor either: the file's one duplicate (unique 670 on
        // map 418) is dropped by the generator with a warning.
        for (i, gate) in GATES.iter().enumerate() {
            for other in &GATES[i + 1..] {
                assert!(
                    !(gate.src_map == other.src_map
                        && gate.src_x == other.src_x
                        && gate.src_y == other.src_y),
                    "two gate rows share the anchor ({}, {}) on map {}",
                    gate.src_x,
                    gate.src_y,
                    gate.src_map
                );
            }
        }
        // The entry requirement, straight off the gate rows: exactly eight gates in the
        // file carry one, and the Raccoon Lair (row 561, anchor map 0 (224,314)) is the
        // one a player meets first — item 24107, one of them (`char.cpp:9123-9133`).
        let lair = GATES
            .iter()
            .find(|g| g.dst_map == 320)
            .expect("the Raccoon Lair gate");
        assert_eq!(
            (lair.dst_map, lair.item_unique, lair.item_num),
            (320, 24107, 1)
        );
        let gated = GATES
            .iter()
            .filter(|g| g.item_unique > 0 && g.item_num > 0)
            .count();
        assert_eq!(gated, 8, "the file's gated rows");
        // Hanyang's exits and the mine take nothing. The mine is looked up in its stamped
        // block, which holds the editor's marker (402,580).
        for (map, x, y) in [(158, 14, 90), (158, 28, 101), (158, 41, 90), (0, 402, 580)] {
            let gate = gate_at(map, x, y).unwrap_or_else(|| panic!("({map}, {x}, {y})"));
            assert_eq!(
                (gate.item_unique, gate.item_num),
                (0, 0),
                "({map}, {x}, {y}) must be free"
            );
        }
    }
    /// The destinations are inside the 10-bit fields the record packs, and the
    /// client's map index for them is the map id itself
    /// (`gz_proto::game::client_map_index`).
    /// The order path and the near-gate diagnostic both hang off these two
    /// lookups, so their semantics are pinned here.
    ///
    /// The gate is a **4×4 block ending at the anchor**: `PutFieldObject`
    /// (`Map-FieldObject.cpp:102-107`) stamps `x = six + centerx - xsize + 1 + j` and
    /// `y = siy + centery - ysize + 1 + i` with `Gate_Warp01`'s own `4 4 0 0`.
    #[test]
    fn every_cell_of_the_gate_block_is_the_gate() {
        // Map 158's centre exit is row 308 of `GateInfoE.txt`: (28, 101) -> map 0 (288, 354).
        assert!(gate_at(158, 28, 101).is_some_and(|g| g.dst_map == 0));
        // Once more for the anchor binding, then the block: x in [25, 28], y in [98, 101].
        // The cells the live run actually ordered to are inside it, which is the finding.
        for (x, y) in [
            (26, 98),
            (26, 100),
            (27, 100),
            (26, 101),
            (28, 99),
            (25, 98),
        ] {
            assert!(
                gate_at(158, x, y).is_some(),
                "({x}, {y}) is inside the circle"
            );
        }
        // One cell outside the block on each side is not a gate.
        assert!(gate_at(158, 24, 101).is_none());
        assert!(gate_at(158, 28, 97).is_none());
        assert!(gate_at(158, 29, 101).is_none());
        assert!(gate_at(158, 28, 102).is_none());
        // The two side exits of the same map, and the outward gate on map 0.
        assert!(gate_at(158, 14, 90).is_some());
        assert!(gate_at(158, 41, 90).is_some());
        assert!(gate_at(0, 291, 350).is_some_and(|g| g.dst_map == 158));
        // A gate block is not honoured on another map.
        assert!(gate_at(109, 28, 101).is_none());
    }

    /// `Revive()`'s destination rules, and the two facts that decide them: `INIT_LEVEL`
    /// is 10 (`CommonHeader.h:672`) and the palace spawn is (158, 27, 87)
    /// (`char.cpp:5045`).
    #[test]
    fn a_revive_sends_beginners_to_hanyang_and_dungeon_dwellers_back_to_the_mouth() {
        // Below 10: always Hanyang, whatever map they died on (`char.cpp:5027-5046`).
        assert_eq!(revive_target(1, 320), REVIVE_PALACE);
        assert_eq!(revive_target(9, 0), REVIVE_PALACE);
        assert_eq!(revive_target(9, 158), REVIVE_PALACE);
        // At or above it, a dungeon returns to the map that gate-enters it
        // (`char.cpp:4914-4917`): the Lair to its mouth on map 0, the mine likewise.
        // Read the row rather than hard-coding: the anchor is the *stamped* block's, measured
        // from the map file — the cell `GateInfoE.txt` names is the same one, the block's
        // `ValidSwitch` cell.
        for map_id in [320, 157] {
            let entrance = GATES
                .iter()
                .find(|g| g.dst_map == map_id)
                .unwrap_or_else(|| panic!("no entrance into map {map_id}"));
            assert_eq!(
                revive_target(30, map_id),
                (entrance.src_map, entrance.src_x, entrance.src_y)
            );
        }
        // A town keeps you in a town: the palace is entered by a gate row too, and
        // sending a Hanyang character outside to that gate would not be "the nearest
        // town".
        assert_eq!(revive_target(30, 158), REVIVE_PALACE);
        assert_eq!(revive_target(30, 0), REVIVE_PALACE);
        // A map nothing enters falls back to the palace (`char.cpp:5007-5013`).
        assert_eq!(revive_target(30, 999), REVIVE_PALACE);
    }

    /// The village table: village 1 (Ankaran) is stamped at x[263..270] y[319..326] around the
    /// marker (269,325) and village 3 (Talasha) at x[235..242] y[297..304] around (241,303)
    /// — `villageinfoE.txt:12,14` — and both land on their own map at the port's town entry
    /// spawn (25,14), live-tested and deliberately off both legacy numbers (engine centre
    /// (25,13), dead `MapTypeInfo_Village.txt` (24,22)).
    #[test]
    fn a_village_entrance_warps_to_the_town_entry_spawn() {
        let ankaran = village_at(0, 263, 319).expect("Ankaran's entrance");
        assert_eq!(
            (
                ankaran.unique,
                ankaran.dst_map,
                ankaran.dst_x,
                ankaran.dst_y
            ),
            (1, 1, 25, 14)
        );
        let talasha = village_at(0, 235, 297).expect("Talasha's entrance");
        assert_eq!(
            (
                talasha.unique,
                talasha.dst_map,
                talasha.dst_x,
                talasha.dst_y
            ),
            (3, 3, 25, 14)
        );
        // Every row is an entrance on map 0 with a destination that has a centre.
        for village in VILLAGES {
            assert!(village.src_x > 0 && village.src_y > 0, "{village:?}");
            assert!(village.dst_map > 0, "{village:?}");
            assert!(village.dst_x > 0 && village.dst_y > 0, "{village:?}");
        }
        assert!(VILLAGES.len() >= 4, "only {} villages", VILLAGES.len());
        // Villages live on the world map, so nothing else answers.
        assert!(village_at(158, 269, 325).is_none());
        // The blocks the shipped `MainMap.map` stamps: Ankaran's entrance is x[263..270]
        // y[319..326] with the editor's marker (269,325) inside it, Talasha's x[235..242]
        // y[297..304].
        let (left, top, right, bottom) = ankaran.block();
        assert_eq!((left, top, right, bottom), (263, 319, 270, 326));
        let (left, top, right, bottom) = talasha.block();
        assert_eq!((left, top, right, bottom), (235, 297, 242, 304));
        // The ordered-cell rule stays exact: only the block's own cells.
        assert!(village_at(0, 263, 319).is_some());
        assert!(village_at(0, 270, 326).is_some());
        assert!(
            village_at(0, 262, 322).is_none(),
            "one cell outside the block"
        );
        assert!(village_at(0, 271, 327).is_none(), "and past the far corner");
        // No range is invented on top: the client sends the cell under the cursor and nothing
        // else (see `village_at`'s doc for the file:line trail), so the rule is the block, cell
        // for cell, however far away the character stood when the click happened.
        assert!(
            village_at(0, 259, 315).is_none(),
            "four tiles off is not the entrance"
        );
        assert!(village_at(0, 257, 313).is_none(), "nor six");
        assert!(
            village_at(158, 269, 325).is_none(),
            "villages live on map 0"
        );
        // Leaving: at map load the engine stamps Ankaran's `Gate_Wood1ToMain` over
        // [23..26]x[6..8] (4x3, anchor (25,7)), and the exit lands on the road just south
        // of the entrance block, at (266, 327).
        let (village, landing) = village_exit(1, 25, 7).expect("the village gate");
        assert_eq!(village.unique, 1);
        assert_eq!(landing, (266, 327));
        // All 12 stamped cells exit, far corner included.
        for (x, y) in [(23, 6), (26, 6), (23, 8), (26, 8)] {
            assert!(village_exit(1, x, y).is_some(), "stamped cell ({x},{y})");
        }
        // One tile outside does not — and neither do the stairs/spawn: the spawn (25,14)
        // and both legacy centres (24,22)/(25,13) sit far from the stamped block.
        for (x, y) in [
            (22, 7),
            (27, 7),
            (25, 5),
            (25, 9),
            (24, 22),
            (25, 13),
            (25, 14),
        ] {
            assert!(
                village_exit(1, x, y).is_none(),
                "outside the block ({x},{y})"
            );
        }
        assert!(
            village_exit(0, 25, 7).is_none(),
            "map 0 is not a village map"
        );
        // The diagnostic measures to the block: (261,322) is two tiles left of it.
        let (near, distance) = village_within(0, 261, 322, 3).expect("two tiles away");
        assert_eq!((near.unique, distance), (1, 2));
        assert!(village_within(158, 270, 322, 3).is_none());
    }

    /// The mine's gate: `GateInfoE.txt:19` names the source cell (402,580), and the shipped
    /// `MainMap.map` stamps the object's block at x[400..403] y[577..581] — the named cell is
    /// the block's `ValidSwitch` cell, inside it. The table carries the measured anchor, so a
    /// click anywhere in the block is the gate.
    #[test]
    fn the_mine_gate_is_the_block_the_map_stamps() {
        let gate = gate_at(0, 402, 580).expect("the stamp block");
        assert_eq!((gate.dst_map, gate.dst_x, gate.dst_y), (157, 60, 97));
        let (left, top, right, bottom) = gate.block();
        assert_eq!((left, top, right, bottom), (400, 577, 403, 581));
        assert!(
            gate_at(0, 400, 577).is_some(),
            "the block's other corner is a gate too"
        );
        assert!(
            gate_at(0, 403, 582).is_none(),
            "one cell past the far corner is not"
        );
    }

    #[test]
    fn the_near_gate_diagnostic_reports_the_closest_row_within_radius() {
        // Distance is measured to the anchor cell, and the diagnostic is only about
        // telling an unrecognised order apart from a gate hit — which the block check in
        // [`every_cell_of_the_gate_block_is_the_gate`] now covers.
        let (gate, distance) = gate_within(158, 28, 101, 0).expect("the anchor itself");
        assert_eq!((gate.src_x, gate.src_y), (28, 101));
        assert_eq!(distance, 0);
        // Five tiles north of the anchor is out of a two-tile radius and in a six-tile one.
        assert!(gate_within(158, 28, 96, 2).is_none());
        assert_eq!(gate_within(158, 28, 96, 6).map(|(_, d)| d), Some(5));
        assert!(gate_within(109, 28, 101, 2).is_none());
    }

    #[test]
    fn every_gate_destination_fits_the_record() {
        for gate in GATES {
            assert!((0..1024).contains(&gate.dst_x), "uiX is 10 bits");
            assert!((0..1024).contains(&gate.dst_y), "uiY is 10 bits");
            assert!(
                gz_proto::game::client_map_index(gate.dst_map) == gate.dst_map as u16,
                "the frame carries the client's map index"
            );
        }
    }

    /// One tile can only be one gate: `gate_at` returns the first match, so a
    /// duplicate row would silently shadow the other one.
    #[test]
    fn no_two_gate_rows_share_a_source_tile() {
        let mut seen = std::collections::HashSet::new();
        for gate in GATES {
            assert!(
                seen.insert((gate.src_map, gate.src_x, gate.src_y)),
                "two gate rows claim ({}, {}):{}",
                gate.src_map,
                gate.src_x,
                gate.src_y
            );
        }
    }

    /// A character repository that records the two position writes and nothing
    /// else — the seam `apply_gate_move` takes so the warp's decision is testable
    /// without PostgreSQL (T-019 slice 3 review, F2).
    #[derive(Default)]
    struct MemoryCharacters {
        map_writes: std::sync::Mutex<Vec<(i32, i32, i32, i32)>>,
    }

    #[async_trait::async_trait]
    impl CharacterRepository for MemoryCharacters {
        async fn list_for_account(
            &self,
            _account_id: i32,
        ) -> Result<Vec<gz_db::CharacterSummary>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn find_by_slot(
            &self,
            _account_id: i32,
            _slot: i16,
        ) -> Result<Option<CharacterFull>, RepositoryError> {
            Ok(None)
        }

        async fn find_by_name(
            &self,
            _name: &str,
        ) -> Result<Option<CharacterFull>, RepositoryError> {
            Ok(None)
        }

        async fn create_character(
            &self,
            _account_id: i32,
            _name: &str,
            _class_kind: i16,
            _gender: i16,
            _slot: i16,
        ) -> Result<i32, RepositoryError> {
            Err(RepositoryError::NotFound)
        }

        async fn delete_character(
            &self,
            _account_id: i32,
            _slot: i16,
        ) -> Result<Option<i32>, RepositoryError> {
            Ok(None)
        }

        async fn set_delete_pending(
            &self,
            _account_id: i32,
            _slot: i16,
            _pending: bool,
        ) -> Result<Option<i32>, RepositoryError> {
            Ok(None)
        }

        async fn resolve_game_unique(&self, _character_id: i32) -> Result<i32, RepositoryError> {
            Err(RepositoryError::NotFound)
        }

        async fn update_position(
            &self,
            _character_id: i32,
            _x: i32,
            _y: i32,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn update_map_position(
            &self,
            character_id: i32,
            map_id: i32,
            x: i32,
            y: i32,
        ) -> Result<(), RepositoryError> {
            self.map_writes
                .lock()
                .unwrap()
                .push((character_id, map_id, x, y));
            Ok(())
        }

        /// The quest reward path pays through this; the movement tests do not, so it
        /// only has to be honest about doing nothing.
        async fn add_money(&self, _character_id: i32, _amount: i64) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn update_stats(
            &self,
            _character_id: i32,
            _stats: &gz_db::StatsUpdate,
        ) -> Result<(), RepositoryError> {
            Ok(())
        }
    }

    /// A row standing on Hanyang's east gate, as the character loader would hand
    /// it over.
    fn row_on_the_east_gate() -> CharacterFull {
        CharacterFull {
            character_id: 1,
            name: "Tester".to_owned(),
            class_kind: 1,
            gender: 1,
            level: 13,
            max_life: 161,
            max_mana: 40,
            move_speed: 12,
            exp: 0,
            money: 1000,
            map_id: 158,
            pos_x: 41,
            pos_y: 90,
            slot: 0,
            school_id: 0,
            game_unique: None,
            str: 20,
            dex: 20,
            vit: 5,
            mag: 5,
            hnd: 10,
            luk: 10,
            wis: 10,
            bonus_points: 5,
        }
    }

    /// The warp's decision, end to end and without a socket: the row, the cached
    /// copy and the registry all move together, and `was`/`now` describe the two
    /// ends the departure 11 is diffed against.
    #[tokio::test]
    async fn a_gate_move_moves_every_store_that_names_the_map() {
        let characters = MemoryCharacters::default();
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(
            key,
            tx,
            session_info_from(&Some(row_on_the_east_gate()), 1473),
        );

        let mut character = Some(row_on_the_east_gate());
        let gate = gate_at(158, 41, 90).expect("the east gate");
        let warp = apply_gate_move(&characters, &registry, key, 1473, &mut character, gate)
            .await
            .expect("the gate move applies");

        // 1. The durable row.
        assert_eq!(
            characters.map_writes.lock().unwrap().clone(),
            vec![(1, 0, 288, 354)],
            "the map must move with the position, or a re-login returns to Hanyang"
        );
        // 2. The cached copy every frame is built from.
        let cached = character.as_ref().expect("still loaded");
        assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (0, 288, 354));
        assert_eq!(warp.moved.map_id, 0);
        // 3. The registry entry the near fans key off.
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 288, 354));
        // 4. The two ends the departure 11 diffs.
        assert_eq!((warp.was.map_id, warp.was.x, warp.was.y), (158, 41, 90));
        assert_eq!((warp.now.map_id, warp.now.x, warp.now.y), (0, 288, 354));
        assert!(
            !gz_proto::map_geometry::cells_adjacent(
                cell_of(warp.was.x, warp.was.y, warp.was.map_id),
                cell_of(warp.now.x, warp.now.y, warp.now.map_id),
            ),
            "crossing maps must never look like a step within one"
        );
    }

    /// A gate move with no character row is refused rather than writing a
    /// position for nobody.
    #[tokio::test]
    async fn a_gate_move_without_a_row_is_refused() {
        let characters = MemoryCharacters::default();
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, session_info_from(&None, 1473));

        let mut character = None;
        let gate = gate_at(158, 41, 90).expect("the east gate");
        assert!(
            apply_gate_move(&characters, &registry, key, 1473, &mut character, gate)
                .await
                .is_err()
        );
        assert!(characters.map_writes.lock().unwrap().is_empty());
    }

    /// A connected socket pair: `step_movement` writes its 200s into one end
    /// and the test holds the other open, so nothing blocks on a full buffer.
    async fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let writer = TcpStream::connect(addr).await.expect("the client end");
        let (reader, _) = listener.accept().await.expect("the server end");
        (writer, reader)
    }

    /// A movement that is due on the next tick: the accumulator already holds a
    /// full step, so `step_movement`'s own `advance` commits the first tile.
    fn due_movement(dest: (i32, i32), pos: (i32, i32), map_id: i32) -> Movement {
        let mut movement = Movement::new(dest, pos, map_id, 12);
        movement.percent = MOVE_PERCENT_STEP;
        movement
    }

    /// A row standing at `(x, y)` on map 0.
    fn row_on_map0(x: i32, y: i32) -> CharacterFull {
        let mut row = row_on_the_east_gate();
        row.map_id = 0;
        row.pos_x = x;
        row.pos_y = y;
        row
    }

    /// The item repository `LiveFrames` carries: a pool that never connects,
    /// because the step path reads no items.
    fn lazy_items() -> PgItemRepository {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://none:none@127.0.0.1:1/none")
            .expect("a parseable URL, connected only on first use");
        PgItemRepository::new(pool)
    }

    /// The owner's live report, as a test: Test12 stood on the open ground
    /// beside Ankaran's wall — the entrance block's footprint is eight tiles
    /// deep around the marker — and one step pulled it into town. A committed
    /// step is **never** a warp: the legacy arms one only from an order's
    /// destination (`char-order.cpp:173` is `CharServerMoveByGate`'s only
    /// caller), so walking beside the wall is just walking.
    #[tokio::test]
    async fn a_step_across_an_entrance_block_does_not_warp() {
        // (263, 322) is Ankaran's leftmost entrance cell and (262, 322) is one
        // tile of open ground west of it, so the walk crosses the boundary.
        assert!(village_at(0, 263, 322).is_some(), "the block starts here");
        assert!(village_at(0, 262, 322).is_none(), "and this is off it");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let row = row_on_map0(262, 322);
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, session_info_from(&Some(row), 1473));

        let mut movement = Some(due_movement((265, 322), (262, 322), 0));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (0, 263, 322),
            "the step lands on the entrance block and stays on map 0"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 263, 322));
        assert!(movement.is_some(), "the walk continues past the block");
    }

    /// The same rule for a gate: the palace gate's block on map 0 is nine by
    /// four (`x[284..292] y[347..350]`), and stepping onto it must not move the
    /// character to Hanyang. Only an order that names such a cell does that.
    #[tokio::test]
    async fn a_step_across_a_gate_block_does_not_warp() {
        assert!(gate_at(0, 284, 350).is_some(), "the block starts here");
        assert!(gate_at(0, 283, 350).is_none(), "and this is off it");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let row = row_on_map0(283, 350);
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, session_info_from(&Some(row), 1473));

        let mut movement = Some(due_movement((286, 350), (283, 350), 0));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (0, 284, 350),
            "the step lands on the gate block and stays on map 0"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (0, 284, 350));
    }

    /// And for leaving: the village map's exit is the stamped 4x3 gate block at anchor
    /// (25,7) — x[23..26] y[6..8] — on Ankaran's own map, and a step onto one of its
    /// cells is a step too.
    #[tokio::test]
    async fn a_step_across_a_village_exit_does_not_warp() {
        assert!(village_exit(1, 24, 8).is_some(), "the exit tile");

        let (mut stream, _peer) = socket_pair().await;
        let mut outbound = Cipher::new();
        let items = lazy_items();
        let mut row = row_on_map0(24, 9);
        row.map_id = 1;
        let mut live = LiveFrames {
            items: &items,
            account_id: 0,
            account_name: String::new(),
            character: Some(row.clone()),
            char_unique: 1473,
            friend_list: Vec::new(),
            quest: gz_proto::quest::PersonQuest::new(),
        };
        let registry = Registry::new();
        let key = Registry::next_key();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register(key, tx, session_info_from(&Some(row), 1473));

        let mut movement = Some(due_movement((24, 8), (24, 9), 1));
        step_movement(
            &mut stream,
            &mut outbound,
            &mut live,
            &mut movement,
            &registry,
            key,
        )
        .await
        .expect("the step commits");

        let character = live.character.as_ref().expect("still loaded");
        assert_eq!(
            (character.map_id, character.pos_x, character.pos_y),
            (1, 24, 8),
            "the step lands on the exit tile and stays in the village"
        );
        let stored = registry.info(key).expect("still registered");
        assert_eq!((stored.map_id, stored.x, stored.y), (1, 24, 8));
    }

    /// A town map has exactly two exits, both stamped by the engine at load: the north
    /// `Gate_Wood1ToMain` block and the south `Gate3_MainToBear1` block. Every other
    /// cell of the town — spawn, stairs, centre — is not an exit, on any of the 42
    /// towns, and both gates share one map-0 landing.
    #[test]
    fn a_town_has_exactly_two_exit_gates() {
        let north = (23, 6, 26, 8); // anchor (25,7), FO 1840 `4 3 1 1`
        let south = village_exit2_block();
        assert_eq!(south, (34, 63, 37, 65)); // anchor (35,63), FO 292 `4 3 2 2`

        for map in 1..=42 {
            let mut landing = None;
            // the whole town: `Village01.map` is 63x68, the file every town loads
            for y in 0..68 {
                for x in 0..63 {
                    let is_exit = ((north.0..=north.2).contains(&x)
                        && (north.1..=north.3).contains(&y))
                        || ((south.0..=south.2).contains(&x) && (south.1..=south.3).contains(&y));
                    match village_exit(map, x, y) {
                        Some((village, at)) => {
                            assert!(is_exit, "map {map}: ({x},{y}) is not on either gate");
                            assert_eq!(village.dst_map, map);
                            match landing {
                                None => landing = Some(at),
                                Some(first) => {
                                    assert_eq!(first, at, "map {map}: one landing for both gates")
                                }
                            }
                        }
                        None => assert!(!is_exit, "map {map}: gate cell ({x},{y}) missed"),
                    }
                }
            }
            assert!(landing.is_some(), "map {map}: both gates exit");
        }

        // Neither block leaves a non-town map, and one tile outside either block is a
        // step: corners of the south block plus the spawn (25,14) and both legacy
        // centres the old reach-6 window used to swallow.
        assert!(village_exit(0, 25, 7).is_none());
        assert!(village_exit(0, 35, 63).is_none());
        for (x, y) in [
            (33, 63),
            (38, 63),
            (35, 62),
            (35, 66),
            (34, 66),
            (37, 62),
            (24, 22),
            (25, 13),
            (25, 14),
        ] {
            assert!(village_exit(1, x, y).is_none(), "not an exit: ({x},{y})");
        }
    }

    /// Both gates through the production branch: `warp_out_of_village` moves the durable
    /// row, the cached copy and the registry entry to map 0's road, while any other tile
    /// returns `false` and writes nothing.
    #[tokio::test]
    async fn both_town_exit_gates_warp_to_map_zero_and_other_tiles_walk() {
        for (dest, expected_warp) in [((25, 7), true), ((35, 63), true), ((30, 40), false)] {
            let characters = MemoryCharacters::default();
            let registry = Registry::new();
            let key = Registry::next_key();
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut row = row_on_map0(25, 9);
            row.map_id = 1;
            let character_id = row.character_id;
            registry.register(key, tx, session_info_from(&Some(row.clone()), 1473));

            let (mut stream, _peer) = socket_pair().await;
            let mut outbound = Cipher::new();
            let items = lazy_items();
            let mut live = LiveFrames {
                items: &items,
                account_id: 0,
                account_name: String::new(),
                character: Some(row.clone()),
                char_unique: 1473,
                friend_list: Vec::new(),
                quest: gz_proto::quest::PersonQuest::new(),
            };

            let warped = warp_out_of_village(
                &mut stream,
                &mut outbound,
                &characters,
                &registry,
                key,
                &mut live,
                1,
                dest,
            )
            .await
            .expect("the order path completes");
            assert_eq!(warped, expected_warp, "order to {dest:?}");

            let cached = live.character.as_ref().expect("still loaded");
            let stored = registry.info(key).expect("still registered");
            if expected_warp {
                // (266, 327): the road just south of Ankaran's entrance block — the
                // one landing both gates share, on map 0.
                assert_eq!(
                    characters.map_writes.lock().unwrap().clone(),
                    vec![(character_id, 0, 266, 327)],
                    "{dest:?}: the map must move with the position"
                );
                assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (0, 266, 327));
                assert_eq!((stored.map_id, stored.x, stored.y), (0, 266, 327));
            } else {
                assert!(characters.map_writes.lock().unwrap().is_empty());
                assert_eq!((cached.map_id, cached.pos_x, cached.pos_y), (1, 25, 9));
                assert_eq!((stored.map_id, stored.x, stored.y), (1, 25, 9));
            }
        }
    }
}
