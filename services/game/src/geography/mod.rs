//! World geometry: what sits on a cell, and where every warp out of it leads.
//!
//! `Gate`/`GATES` transcribe `New Era Live\Data\GateInfoE.txt` in file order (the
//! map-158 <-> map-0 family plus the lair rows); `Village`/`VILLAGES` pin the
//! world-map village entrances and the towns their gates lead to. The lookups
//! answer the one question every caller asks — *what object is on this cell* —
//! in the legacy's own order, village before gate (`char.cpp:8975-9028`).
//!
//! The tables sit beside their types in `gates` and `villages`, so a future
//! dungeon or sub-map table is one more file rather than another block in a
//! single growing module. This file keeps the lookups and the public surface,
//! re-exported so callers name one path: `crate::geography::{Gate, gate_at, ..}`.
//!
//! Split out of `movement.rs`, which keeps the stepped-move family and the
//! request handlers that consume these answers. Everything here is `&'static`
//! transcription: there is no state to hold, so the domain is modules of
//! tables and pure functions rather than a manager object.

mod gates;
mod villages;

pub(crate) use gates::{GATES, Gate};
pub(crate) use villages::{VILLAGES, Village};

/// The gate covering `(map_id, x, y)`, if any.
///
/// The gate is a **field object**, not a tile: `cltUnderMouseObject::FindGate`
/// (`UnderMouseObject.cpp:290-307`) resolves a click by the object on the cell, and
/// `PutFieldObject` (`Map-FieldObject.cpp:66-130`) stamps the object's whole block — so
/// every cell of the block is that gate. Each row carries its own object's numbers:
/// `TileXsize`/`TileYsize`/`TileCenterX`/`TileCenterY` from that object's detail file,
/// which `cltFieldObjectInfo::LoadDetailInfo` reads (`FieldObject.cpp:325-399`).
pub(crate) fn gate_at(map_id: i32, x: i32, y: i32) -> Option<&'static Gate> {
    GATES.iter().find(|g| g.src_map == map_id && g.covers(x, y))
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
pub(crate) fn village_at(map_id: i32, x: i32, y: i32) -> Option<&'static Village> {
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
pub(crate) fn village_exit2_block() -> (i32, i32, i32, i32) {
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
pub(crate) fn village_exit(map_id: i32, x: i32, y: i32) -> Option<(&'static Village, (i32, i32))> {
    let village = VILLAGES.iter().find(|v| v.dst_map == map_id)?;
    let (left2, top2, right2, bottom2) = village_exit2_block();
    let on_exit2 = (left2..=right2).contains(&x) && (top2..=bottom2).contains(&y);
    (village.covers_exit(x, y) || on_exit2).then(|| (village, entrance_landing(village)))
}

/// The village entrance nearest `(map_id, x, y)` within `radius`, and how far it is.
///
/// Diagnostic only, in the same spirit as [`gate_within`]: until the entrance block is
/// measured, a click that lands beside one must still be visible in the log.
pub(crate) fn village_within(
    map_id: i32,
    x: i32,
    y: i32,
    radius: i32,
) -> Option<(&'static Village, i32)> {
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
pub(crate) fn village_warp_gate(from_map: i32, village: &Village) -> Gate {
    warp_gate_to(from_map, village.dst_map, village.dst_x, village.dst_y)
}

/// A gate-shaped warp to anywhere: the village entry and exit and the revive path all end in
/// the same `SetWarpInfo`/`Warp` pair and differ only in how the destination is chosen.
pub(crate) fn warp_gate_to(from_map: i32, dst_map: i32, dst_x: i32, dst_y: i32) -> Gate {
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
pub(crate) fn gate_within(
    map_id: i32,
    x: i32,
    y: i32,
    radius: i32,
) -> Option<(&'static Gate, i32)> {
    GATES
        .iter()
        .filter(|g| g.src_map == map_id)
        .filter_map(|g| {
            let distance = (g.src_x - x).abs().max((g.src_y - y).abs());
            (distance <= radius).then_some((g, distance))
        })
        .min_by_key(|(_, distance)| *distance)
}
