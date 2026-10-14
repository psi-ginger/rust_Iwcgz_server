# Town exit gate — `Gate_Wood1ToMain` on maps 1..42

Why clicking the wooden town-exit gate stopped warping after `5525e56`, and what the
client executables actually do. Ground truth derived by decompiling the shipped
executables (REA + Ghidra), then tracing the legacy C++ engine source — no guessed
radii.

## Step 1 — what the client does (decompilation of all three binaries)

Binaries compared: `New Era Live`, `New_Era_Live_update`, `GoonZuNewHope`. All three
are **logic-identical**; their gate-related data files are byte-identical (only the
noted `ServiceArea`/text diffs).

- **There is no gate packet.** Clicking the gate sends an ordinary `ORDER_MOVE`
  (opcode 240, `usSize = 29`, `siPara1 = x | (y << 16)`) naming the *cell under the
  cursor*, built from `clMouseObject` (`MouseOperation.cpp:5408-5439`) and sent
  unchanged through `cltClient::OrderOperation` (`Client.cpp:3284-3304`). A hovered
  gate only sets `MOUSEMODE_MOVE` + the gate cursor (`MouseOperation.cpp:3333-3338`).
- **The clickable area is the stamped cell block**, not a radius: for FO 1840 that is
  x ∈ `[GateX−2, GateX+1]`, y ∈ `[GateY−1, GateY+1]` — 4×3 = 12 cells, the footprint of
  FO 1840 (`Gate_Wood1ToMain`, detail `4 3 1 1`) as `PutFieldObject` stamps it. The rule
  generalises to any hovered field object: its own stamped cells. The town's second
  gate, FO 292 (`Gate3_MainToBear1`, also `FOTYPE_GATE`, detail `4 3 2 2`), is stamped
  at the fixed (35,63) to its own 4×3 block x ∈ `[34,37]`, y ∈ `[63,65]` — same order,
  same cursor, same `IsEnterGate` path.
- `IsEnterGate` (`Client.cpp:8124`) gate-unique semantics: `para < 10000` indexes
  `clGate[para]`; the client only checks that the gate's destination map exists.

So the client never decides "this is an exit"; the *server* must recognise the ordered
cell.

## Step 2 — server audit of `handle_userorder` before `village_exit`

Audited (`services/game/src/movement.rs`): between the start of `handle_userorder` and
the `village_exit(...)` call there are **no early returns or validation that could
drop the order** — only `si_order != ORDER_MOVE`, a missing character, and send
errors. The break was data plus the match window, not control flow.

### Root cause

1. **The engine never loads `MapTypeInfo_Village.txt`.** `LoadMapTypeInfo_Village()`
   is commented out (`MapTypeManager.cpp:78`); centre/gate come only from the
   `SetMapType` hardcodes in the constructor (`MapTypeManager.cpp:116-153`):
   `SetMapType(24, "Village01", 25, 13, 25, 7, …)` (`:124`, default service area —
   `ServiceArea.txt` is `SERVICEAREA_ENGLISH` for all three clients). Every town map
   1..42 is map type 24, so the real gate anchor is **(25,7)** for all of them. The
   file's `(23,18)` (and `(16,8)`) values are dead data.
2. **The exit gate is stamped at map load, not shipped on disk.**
   `MapCommon.cpp:419-453` creates a runtime `cltGate` and stamps FO 1840 with
   `PutFieldObject(siGateX, siGateY, 1840, clGate.siUnique)`; `PutFieldObject`
   (`Map-FieldObject.cpp:66-130`) stamps `anchor + center − size + 1 + j` per axis, so
   with the anchor (25,7) the gate covers **x ∈ [23,26], y ∈ [6,8]** — 12 cells, all
   carrying the gate's para. (`Village01.map` on disk has no FO 1840; the wall gap at
   x24-26/y6-8 sits under it.)
3. **The generated table used the dead file**, so `VILLAGES.exit_x/exit_y` was
   (23,18) with `VILLAGE_EXIT_REACH = 6`: the exit window was x ∈ [17,29], y ∈
   [12,24]. Real gate clicks (y 6..8) missed it → walk armed, **no warp**.
4. **Why it appeared to work before `5525e56`:** the old step-seam checked every
   committed tile, and the walk from the town centre to the gate crossed y=12 —
   inside the over-wide window — so the warp fired *en route*. The same window also
   covered the spawn/centre ((24,22): dx 1, dy 4 ≤ 6), which is why steps in the
   stairs/spawn area warped out too. Once only the order destination is checked, both
   symptoms became visible: the gate never warps, and the fix must not widen the
   window again.

The legacy resolves the ordered cell with `GetFieldObject_Unique(x, y)`
(`char.cpp:8975`) → `FOTYPE_GATE` → `clGate[para]` (`CharServerMoveByGate`,
`char.cpp:8970-9100`): **exactly the stamped cells of either town gate exit**, nothing
else.

### Fix

- `scripts/generate-npc-placements.ps1`: exit anchor now comes from
  `$engineGateOfType`, the `SetMapType` hardcode table keyed by the map's type unique
  (24 → (25,7); 25/26/27 → (23,18); 31/33/34/35 → (16,8); throws if a type is
  missing). All 42 rows regenerate to exit (25,7).
- `services/game/src/movement.rs`: `village_exit` now matches the stamped 4×3 block
  exactly via `Village::covers_exit` — x ∈ `[exit_x−2, exit_x+1]`, y ∈
  `[exit_y−1, exit_y+1]` — instead of a Chebyshev reach of 6 around a dead anchor.
  `VILLAGE_EXIT_REACH` is gone. Exact cells are inherently legacy-faithful and leave
  the stairs/spawn/centre untouched: both centres ((24,22) and (25,13)) are far
  outside the block.
- Tests: every one of the 12 cells warps (corners included); one tile outside, both
  centres, and map 0 do not; a step *onto* a stamped cell still commits the position
  (only an order destination warps).

Verified with `scripts/generate-npc-placements.ps1 -Check` and `scripts/check.ps1`.

## Second exit gate — FO 292 `Gate3_MainToBear1`

Towns have two exits; the second is stamped in the same map-load pass:

- **Data**: `FieldObjectSetInfo.txt` row 292 = `FOTYPE_GATE` (same type as FO 1840),
  zip `Gate3`; detail file `FieldObject\Gate3_MainToBear1\Gate3_MainToBear1.txt` =
  `4 3 2 2 0 -1 1 1` → `TileXsize=4, TileYsize=3, CenterX=2, CenterY=2`. Note the centre
  is **(2,2)** — unlike FO 1840's `4 3 1 1`, centre (1,1).
- **Anchor**: `MapCommon.cpp:457-490` — `founique = 292` (Japan ships FO 7176 instead,
  `:457-464`), then for any map with a type `gatex = 35; gatey = 63`, hardcoded.
- **Stamp**: `PutFieldObject(35, 63, 292, clGate.siUnique)` with the same formula →
  x = 35+2−4+1+j, y = 63+2−3+1+i → **x ∈ [34,37], y ∈ [63,65]** — 12 cells,
  `ValidSwitch` at (35,63), the village unique as para on every cell
  (`Map-FieldObject.cpp:105-106,119`). `PutFieldObject` refuses out-of-map anchors and
  blocks that already hold a field object (`:68,96`): both blocks are free in
  `Village01.map` (63×68), verified by dumping `FieldObjectMap` over all eight shipped
  `Village0*.map` files. That covers every town, because the file comes from the map's
  **type** (`map\<type>\<type>.Map`, `MapManager.cpp:851,865`) and all 42 towns are type
  24 → `Village01`; the other `Village02..08` files (48×48, 29×66 — the (35,63) anchor
  would fall out of bounds there) belong to map types no town index uses.
- **Destination**: built from the same constructor call as the north gate —
  `cltGate(siVillageUnique, ..., 0, mapposx, mapposy, false)` at `MapCommon.cpp:469-470`
  vs `:431-432` — so both gates leave to map 0 at the village's PosX/PosY, i.e. the same
  landing; in goonzu-rs both go through `entrance_landing(village)`.
- **Collision is irrelevant**: the gate check runs on the order's destination before
  the walk (`char-order.cpp:173`), and both blocks sit on a mix of open (coll 16) and
  wall (coll 48) cells in `Village01.map` — same as the north gate.

Implementation: `village_exit2_block()` + the `warp_out_of_village` branch in
`movement.rs`. Tests sweep every cell of the 63×68 town on all 42 maps — exactly the
two blocks exit, everything else is a step — and drive the production branch for both
anchors plus a non-gate tile: both gates land row, cached copy and registry on map 0
(266,327); the non-gate tile writes nothing.

## Town entry spawn — (25, 13)

Entering a town (maps 1..42) lands the character at **(25,13)** — the engine's own
`SetMapType` centre for type 24, read exactly as the legacy entry warp reads it
(`char.cpp:9011-9012`). The only number it does *not* use is `MapTypeInfo_Village.txt`'s
(24,22): that file's loader is commented out (`MapTypeManager.cpp:78`), so it is dead
data. The generator emits `DstX/DstY = 25/13` (its `$centerOfTypeName` parse survives
only as a type-name check), and the `VILLAGES` table carries the same 42 times.

## Known gaps (deliberately untouched)
- **Japan/China service area** would move the anchor to (26,8)
  (`MapTypeManager.cpp:121`); no shipped client uses it.
