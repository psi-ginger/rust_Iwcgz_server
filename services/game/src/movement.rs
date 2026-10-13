//! Movement: the stepped move family. `Movement` and its accumulator mirror
//! `cltCharCommon::Move` (`char-move.cpp:38-227`); `char_update_body` /
//! `send_char_update` / `fan_char_update` build and spread the 200
//! `CHARUPDATE`; and `handle_userorder` / `handle_leave_request` answer the 240
//! `REQUEST_USERORDER` and the 107/109 leave requests. The read loop feeds this
//! module's `step_movement` from its timer arm; the request handlers are called
//! from the dispatcher in `main.rs`.

use std::time::{Duration, Instant};

use gz_cipher::Cipher;
use gz_db::{CharacterFull, CharacterRepository, PgCharacterRepository};
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
/// | 306 | 49 | map 0 at (291,350) | map 158 at (27,87) |
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
    },
    Gate {
        src_map: 0,
        src_x: 0,
        src_y: 0,
        xsize: 9,
        ysize: 4,
        center_x: 1,
        center_y: 0,
        dst_map: 98,
        dst_x: 3,
        dst_y: 47,
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
    },
    Gate {
        src_map: 0,
        src_x: 212,
        src_y: 502,
        xsize: 4,
        ysize: 4,
        center_x: 0,
        center_y: 0,
        dst_map: 451,
        dst_x: 130,
        dst_y: 70,
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
    },
    Gate {
        src_map: 0,
        src_x: 296,
        src_y: 367,
        xsize: 5,
        ysize: 3,
        center_x: 0,
        center_y: 0,
        dst_map: 425,
        dst_x: 60,
        dst_y: 60,
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
/// actually stands), and ends the move on arrival. A no-op when no move is
/// active or the tile is not due yet.
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
    characters: &PgCharacterRepository,
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
    // Stepping *onto* a gate leaves the map too, not only ordering a move to one.
    //
    // The legacy checks the order's destination and nothing else
    // (`char-order.cpp:173` → `CharServerMoveByGate(x, y)`), but a walk that merely
    // passes across a gate cell never has it as a destination, and the client
    // resolves a gate by the object the player clicked rather than by our tile
    // table. Warping on the committed tile as well is a deliberate widening: it is
    // what makes "walk out through the gate" work, and a gate tile is by
    // definition somewhere the character was not meant to stay. The destination
    // check in `handle_userorder` stays first, so a direct order still leaves
    // without a step.
    let stepped_gate = gate_at(mv.map_id, x, y);
    let stepped_map = mv.map_id;
    if mv.arrived() || stepped_gate.is_some() {
        mv.deadline = None;
        *movement = None;
    }
    if let Some(gate) = stepped_gate {
        logln!(
            "game: stepped onto the gate at ({x}, {y}) on map {stepped_map} -> map {}",
            gate.dst_map
        );
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
        return Ok(());
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

    // The exit gates are checked on the **order's destination**, exactly where the
    // legacy checks them (`char-order.cpp:173` → `CharServerMoveByGate(x, y)`), and
    // a gate hit replaces the walk with a warp — so this has to run before the
    // destination is persisted as a position.
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
    use gz_db::RepositoryError;

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
    }

    /// The destinations are inside the 10-bit fields the record packs, and the
    /// client's map index for them is the map id itself
    /// (`gz_proto::game::client_map_index`).
    /// The step-onto path and the near-gate diagnostic both hang off these two
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
}
