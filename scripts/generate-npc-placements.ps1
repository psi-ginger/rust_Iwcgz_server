# generate-npc-placements.ps1 - regenerate the generated placement tables.
#
# Emits two committed tables from the client data, and `-Check` fails when either
# has drifted:
#
#   * `NPC_PLACEMENTS` (crates/gz-data/src/npc.rs) - the static town NPCs, from
#     `NPCGroup\*.txt` + `KindInfo_Unique.txt`.
#   * `GATES` (services/game/src/movement.rs) - the map-to-map gates, from
#     `Data\GateInfoE.txt`.
#
# (The file name predates the second table; the `-Table` switch picks one.)
#
# The NPC table is what the game service turns into the `cltMakeCharInfoCommon`
# records a joining client receives in a `GAMEMSG_RESPONSE_LOGINMANY` (6) body,
# so it is the difference between "Hanyang is empty" and "Sarah Spencer is
# standing at (16,82)". The gate table is what the order path checks a move
# destination against (`char-order.cpp:173`), so it is the difference between a
# city exit that works and a tile that does nothing.
#
# Recipe, straight from the client's own data so client and server cannot disagree:
#
#   * `KindInfo_Unique.txt` gives each `KIND_*` its numeric id - that number is
#     what goes on the wire as `uiMakeCharKind`, and the client resolves the
#     sprite and the display name from it.
#   * every `NPCGroup\*.txt` gives a group rectangle and the kinds inside it. For
#     the map under test the town-NPC files are `npcpalace*.txt`, `npcquest*.txt`,
#     `npcspringman*.txt` and `npcrecommend*.txt`.
#
# The generator refuses what it cannot place faithfully instead of guessing:
# a kind name with no id, a group on another map, a `Radius` other than 1 (the
# legacy picks a random tile inside the rectangle; we place at the centre, which
# is only the same thing for a 1-tile neighbourhood), a `MinNum` other than 1
# (several avatars need several distinct tiles), or two NPCs on one tile. The
# refusal is the check.
#
#   # print the const NPC_PLACEMENTS block for crates/gz-data/src/npc.rs:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\generate-npc-placements.ps1
#
#   # verify the committed table is current (exit 1 on drift):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\generate-npc-placements.ps1 -Check
#
# Override the inputs with -DataRoot / -MapIndex / -RustPath.

param(
    [string]$DataRoot = (Join-Path $PSScriptRoot '..\..\New Era Live'),
    [int]$MapIndex = 158,
    [string]$RustPath = (Join-Path $PSScriptRoot '..\crates\gz-data\src\npc.rs'),
    [string]$GateRustPath = (Join-Path $PSScriptRoot '..\services\game\src\movement.rs'),
    [string]$Table = 'all',
    [switch]$Check
)

$ErrorActionPreference = "Stop"

$kindFile = Join-Path $DataRoot 'Data\KindInfo_Unique.txt'
$groupDir = Join-Path $DataRoot 'NPCGroup'
if (-not (Test-Path $kindFile)) { throw "kind table not found: '$kindFile' (set -DataRoot)" }
if (-not (Test-Path $groupDir)) { throw "NPCGroup directory not found: '$groupDir' (set -DataRoot)" }

# --- kind ids -------------------------------------------------------------------
# Rows are "Unique<TAB>KindType<TAB>Explain"; headers and comments fail the parse.
$kindIds = @{}
foreach ($line in [System.IO.File]::ReadAllLines($kindFile, [System.Text.Encoding]::Unicode)) {
    $fields = $line -split "`t"
    if ($fields.Count -lt 2) { continue }
    $id = 0
    if (-not [int]::TryParse($fields[0].Trim(), [ref]$id)) { continue }
    $name = $fields[1].Trim()
    if ($name -notlike 'KIND_*') { continue }
    $kindIds[$name] = $id
}
if ($kindIds.Count -eq 0) { throw "no KIND_* rows parsed from '$kindFile'" }

# --- groups on this map ---------------------------------------------------------
$placements = New-Object 'System.Collections.Generic.List[object]'
$seenTiles = @{}
$skipped = New-Object 'System.Collections.Generic.List[string]'
$fileCount = 0

foreach ($file in Get-ChildItem -LiteralPath $groupDir -Filter '*.txt' | Sort-Object Name) {
    $lines = [System.IO.File]::ReadAllLines($file.FullName)
    if ($lines.Count -eq 0) { continue }

    # Anchor on the two table headers, so a reordered or reformatted file fails
    # loudly instead of shifting every column by one.
    $groupHeader = -1
    $npcHeader = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $first = ($lines[$i] -split "`t")[0].Trim()
        if ($first -eq 'NPCGroupID') { $groupHeader = $i }
        if ($first -eq 'NPCName') { $npcHeader = $i }
    }
    if ($groupHeader -lt 0 -or $npcHeader -lt 0) { continue }

    $dataLine = $null
    for ($i = $groupHeader + 1; $i -lt $lines.Count; $i++) {
        if ($lines[$i].Trim().Length -gt 0) { $dataLine = $lines[$i]; break }
    }
    if ($null -eq $dataLine) { throw "$($file.Name): no group data row" }
    $g = $dataLine -split "`t"
    # Only the rows for *this* map have to be well formed: other maps' files use
    # older shapes (7-column rows exist) and are none of this table's business.
    if ($g.Count -lt 3) { continue }
    $groupMap = 0
    if (-not [int]::TryParse($g[2].Trim(), [ref]$groupMap)) { continue }
    if ($groupMap -ne $MapIndex) { continue }
    if ($g.Count -lt 8) { throw "$($file.Name): group row has $($g.Count) fields" }
    $centerX = [int]$g[3].Trim()
    $centerY = [int]$g[4].Trim()
    $radius = [int]$g[5].Trim()
    if ($radius -ne 1) {
        throw "$($file.Name): radius $radius on map $MapIndex - place these by hand, the centre rule assumes 1"
    }

    for ($i = $npcHeader + 1; $i -lt $lines.Count; $i++) {
        $fields = $lines[$i] -split "`t"
        $name = $fields[0].Trim()
        if ($name.Length -eq 0 -or $name -eq 'NULL') { break }
        $minNum = 1
        $level = 0
        if ($fields.Count -gt 1) { [void][int]::TryParse($fields[1].Trim(), [ref]$minNum) }
        if ($fields.Count -gt 2) { [void][int]::TryParse($fields[2].Trim(), [ref]$level) }
        if ($minNum -ne 1) {
            throw "$($file.Name): $name has MinNum $minNum - several avatars need distinct tiles"
        }
        if (-not $kindIds.ContainsKey($name)) {
            # The kind has no id anywhere in the shipped data, so the client could
            # not resolve a sprite or a name for it either (it reads the same
            # file). Skipped rather than guessed - and reported below, because the
            # skip is the interesting part of the roster.
            $skipped.Add("$($file.Name): $name")
            continue
        }
        $kind = $kindIds[$name]
        $tileKey = "$kind/$centerX/$centerY"
        if ($seenTiles.ContainsKey($tileKey)) {
            throw "$($file.Name): two NPCs of kind $kind on tile ($centerX,$centerY)"
        }
        $seenTiles[$tileKey] = $true
        $placements.Add([pscustomobject]@{
            Kind  = $kind
            MapId = $MapIndex
            X     = $centerX
            Y     = $centerY
            Level = $level
        })
    }
    $fileCount++
}

if ($placements.Count -eq 0) { throw "no NPC groups found for map $MapIndex" }

$sorted = @($placements | Sort-Object Kind, X, Y)

# Lay the body out the way rustfmt does. Struct literals wider than rustfmt's
# `struct_lit_width` (18 by default) get broken one field per line, so the
# single-line form would be rewritten on the next `cargo fmt` and every run of
# this script would look like drift.
$body = @($sorted | ForEach-Object {
    @(
        "    NpcPlacement {",
        "        kind: $($_.Kind),",
        "        map_id: $($_.MapId),",
        "        x: $($_.X),",
        "        y: $($_.Y),",
        "        level: $($_.Level),",
        "    },"
    )
})

# --- gates: every row, with the block its field object stamps -------------------
#
# `GateInfoE.txt` columns: Unique, TxtNum, FOUnique, ItemUnique, ItemNum, SrcMap,
# SrcX, SrcY, DstMap, DstX, DstY, PKAreaGate, then a comment. Every row in the file is
# emitted now, not just the ones between maps this port serves: the gate network is
# data, a lookup is keyed by `(map, x, y)`, and a row for a map nothing reaches yet
# costs a comparison and nothing else.
#
# The footprint is not in this file. A gate's `FOUnique` names a field object
# (`FieldObjectSetInfo.txt`), and the object's own detail file carries
# `TileXsize TileYsize TileCenterX TileCenterY` — the numbers
# `cltFieldObjectInfo::LoadDetailInfo` reads (`FieldObject.cpp:325-399`) and
# `cltMapCommon::PutFieldObject` (`Map-FieldObject.cpp:66-130`) stamps as
#     x = anchor + TileCenterX - TileXsize + 1 + j,  j in 0..TileXsize
#     y = anchor + TileCenterY - TileYsize + 1 + i,  i in 0..TileYsize
# The detail file sits either beside the archive as `<name>/<name>.txt` or inside
# `FieldObject/<zip>.mz` under that same path.
$servedMaps = @(0, $MapIndex)
$gateFile = Join-Path $DataRoot 'Data\GateInfoE.txt'
if (-not (Test-Path $gateFile)) { throw "gate table not found: '$gateFile' (set -DataRoot)" }
$foSetFile = Join-Path $DataRoot 'Data\FieldObjectSetInfo.txt'
if (-not (Test-Path $foSetFile)) { throw "field-object set not found: '$foSetFile' (set -DataRoot)" }

Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

# FOUnique -> @{ Name; Zip } from the set file (UTF-16, like every data file here).
$foNames = @{}
foreach ($line in [System.IO.File]::ReadAllLines($foSetFile, [System.Text.Encoding]::Unicode)) {
    $fields = $line -split "`t"
    if ($fields.Count -lt 8) { continue }
    $unique = 0
    if (-not [int]::TryParse($fields[0].Trim(), [ref]$unique)) { continue }
    $foNames[$unique] = @{ Name = $fields[3].Trim(); Zip = $fields[7].Trim() }
}
if ($foNames.Count -eq 0) { throw "$($foSetFile): no field-object rows parsed" }

$footprintCache = @{}
function Get-FieldObjectFootprint([int]$unique) {
    if ($footprintCache.ContainsKey($unique)) { return $footprintCache[$unique] }
    if (-not $foNames.ContainsKey($unique)) {
        throw "field object $unique is not in FieldObjectSetInfo.txt"
    }
    $name = $foNames[$unique].Name
    $zip = $foNames[$unique].Zip
    $text = $null
    $plain = Join-Path $DataRoot "FieldObject\$name\$name.txt"
    if (Test-Path $plain) {
        $text = [System.IO.File]::ReadAllText($plain, [System.Text.Encoding]::Default)
    }
    else {
        foreach ($archive in @($zip, $name)) {
            $mz = Join-Path $DataRoot "FieldObject\$archive.mz"
            if (-not (Test-Path $mz)) { continue }
            $zipFile = [System.IO.Compression.ZipFile]::OpenRead($mz)
            try {
                $entry = $zipFile.Entries | Where-Object { $_.FullName -ieq "$name/$name.txt" } | Select-Object -First 1
                if ($null -eq $entry) {
                    $entry = $zipFile.Entries | Where-Object { $_.FullName -ilike "$name*.txt" } | Select-Object -First 1
                }
                if ($null -ne $entry) {
                    $reader = New-Object System.IO.StreamReader($entry.Open(), [System.Text.Encoding]::Default)
                    try { $text = $reader.ReadToEnd() } finally { $reader.Dispose() }
                }
            }
            finally { $zipFile.Dispose() }
            if ($text) { break }
        }
    }
    if (-not $text) { throw "field object $unique ($name): no detail file in $name/ or $zip.mz" }

    # The data line is the first whose first four whitespace fields are all integers;
    # the header (`TileXsize TileYsize …`) and the model name never are.
    $footprint = $null
    foreach ($detail in $text -split "`r?`n") {
        $parts = @($detail -split '\s+' | Where-Object { $_ -ne '' })
        if ($parts.Count -lt 4) { continue }
        $nums = @()
        $ok = $true
        foreach ($part in $parts[0..3]) {
            $value = 0
            if (-not [int]::TryParse($part, [ref]$value)) { $ok = $false; break }
            $nums += $value
        }
        if ($ok -and $nums[0] -gt 0 -and $nums[1] -gt 0) { $footprint = $nums; break }
    }
    if ($null -eq $footprint) { throw "field object $unique ($name): no `TileXsize …` data line" }
    $footprintCache[$unique] = $footprint
    return $footprint
}

# --- map 0's stamped blocks, read from the map file --------------------------
#
# `GateInfoE.txt`'s `SrcX`/`SrcY` is the map editor's marker for the object, not the cell
# `PutFieldObject` stamped first: the marker sits inside the block, and the `ValidSwitch` bit in
# `FieldObjectMap` sits exactly on it (`Map-FieldObject.cpp:102-127`). The stamped block itself
# is in the map's field-object grid:
#
#   `MapCommon.cpp:55` builds the row offsets as `YOff[i] = i * MH.MapXsize`, so cell (x, y) is
#   index `x + y * Xsize` - no padding, no constant offset - and `Map-FieldObject.cpp:253` reads
#   the para grid as `pFieldObjectMapPara[x + YOff[y]]`. `map-save.cpp:44-55` writes the sections
#   in order: header, IDMap (2*W*H), CollMap (2*W*H), layer 0 (4*W*H + W*H), layer 1
#   (4*W*H + W*H), FieldObjectMap (4*W*H), **FieldObjectMapPara** (4*W*H), geometry
#   (8 + 16384 + W*H), every size driven by the header's own dimensions.
#
# The header revision decides the header's size: the shipped files are ANSI builds, where
# `TCHAR` is one byte, so `cltMapHeader` is 176 bytes and `MapXsize`/`MapYsize` sit at 140/142
# (the Unicode build the 2010 sources describe is 324 bytes with them at 268/270). Its para
# grid therefore starts at `headerSize + (2+2+5+5+4) * W * H`, and the total is checked against
# the file's length below, so a wrong layout is refused rather than read at the wrong offset.
#
# An earlier session derived this start by hand as `12631712 - 4*(291 + 350*800)`, taking the
# marker cell (291, 350) for the block's first cell. It is not - the marker is a cell inside the
# block - and that start is 9628 bytes (2407 cells, three rows and seven cells) early, which
# shifted every measured block by (7, 3) until the layout above replaced it.
$MapFile = Join-Path $DataRoot 'Map\MainMap\MainMap.map'
if (-not (Test-Path $MapFile)) { throw "map file not found: '$MapFile' (set -DataRoot)" }
$mapBytes = [System.IO.File]::ReadAllBytes($MapFile)
if ($mapBytes.Length -lt 324) { throw "$($MapFile): shorter than a map header" }
$ansiX = [System.BitConverter]::ToUInt16($mapBytes, 140)
$ansiY = [System.BitConverter]::ToUInt16($mapBytes, 142)
$uniX = [System.BitConverter]::ToUInt16($mapBytes, 268)
$uniY = [System.BitConverter]::ToUInt16($mapBytes, 270)
if ($ansiX -gt 0 -and $ansiY -gt 0) {
    $MapHeaderSize = 176
    $MapWidth = $ansiX
    $MapHeight = $ansiY
} elseif ($uniX -gt 0 -and $uniY -gt 0) {
    $MapHeaderSize = 324
    $MapWidth = $uniX
    $MapHeight = $uniY
} else {
    throw "$($MapFile): neither header revision carries usable dimensions"
}
$MapParaStart = $MapHeaderSize + 18 * $MapWidth * $MapHeight
$MapGeoStart = $MapParaStart + 4 * $MapWidth * $MapHeight
$MapExpectedLength = $MapGeoStart + 8 + 16384 + $MapWidth * $MapHeight
if ($mapBytes.Length -ne $MapExpectedLength) {
    throw "$($MapFile): $($mapBytes.Length) bytes, but the ${MapWidth}x${MapHeight} layout needs $MapExpectedLength"
}

function Read-ParaCell([int]$x, [int]$y) {
    if ($x -lt 0 -or $y -lt 0 -or $x -ge $MapWidth -or $y -ge $MapHeight) { return -1 }
    return [System.BitConverter]::ToInt32($mapBytes, $MapParaStart + 4 * ($x + $y * $MapWidth))
}

function Get-StampedBlock([int]$value, [int]$nearX, [int]$nearY, [int]$xsize, [int]$ysize, [int]$centerX, [int]$centerY) {
    # Find a cell of the block near the editor's marker, then measure its extent.
    $found = $false
    $seedX = 0
    $seedY = 0
    for ($radius = 0; $radius -le 12 -and -not $found; $radius++) {
        for ($dy = -$radius; $dy -le $radius -and -not $found; $dy++) {
            for ($dx = -$radius; $dx -le $radius; $dx++) {
                if ([Math]::Abs($dx) -ne $radius -and [Math]::Abs($dy) -ne $radius) { continue }
                if ((Read-ParaCell ([int]$nearX + [int]$dx) ([int]$nearY + [int]$dy)) -eq $value) {
                    $seedX = [int]$nearX + [int]$dx
                    $seedY = [int]$nearY + [int]$dy
                    $found = $true
                    break
                }
            }
        }
    }
    if (-not $found) {
        # A row declared with a placeholder source (0,0) and others like it are not stamped
        # into map 0's grid at all - the object never existed on this map, so the row is
        # dead in-game. Report it and let the caller drop it.
        return $null
    }
    $left = $seedX; $right = $seedX; $top = $seedY; $bottom = $seedY
    while ((Read-ParaCell ($left - 1) $seedY) -eq $value) { $left-- }
    while ((Read-ParaCell ($right + 1) $seedY) -eq $value) { $right++ }
    while ((Read-ParaCell $seedX ($top - 1)) -eq $value) { $top-- }
    while ((Read-ParaCell $seedX ($bottom + 1)) -eq $value) { $bottom++ }
    # PutFieldObject stamps `x = anchor + center - size + 1 + j`, so the block ends at
    # `anchor + center`: invert that to the anchor the table stores, and check the result
    # reproduces the measured rectangle exactly.
    $anchorX = $right - $centerX
    $anchorY = $bottom - $centerY
    $checkLeft = $anchorX + $centerX - $xsize + 1
    $checkTop = $anchorY + $centerY - $ysize + 1
    if ($checkLeft -ne $left -or $checkTop -ne $top -or
        ($checkLeft + $xsize - 1) -ne $right -or ($checkTop + $ysize - 1) -ne $bottom) {
        # The shipped grid disagrees with the object's declared size for this row - some
        # rows in `GateInfoE.txt` are leftovers whose ids collide with something else. The
        # placement is the authority, so the row is reported and dropped rather than
        # emitted with a block that does not exist.
        return $null
    }
    return @($anchorX, $anchorY)
}

$gates = New-Object 'System.Collections.Generic.List[object]'
$gateSkipped = New-Object 'System.Collections.Generic.List[string]'
$gateUnplaced = New-Object 'System.Collections.Generic.List[string]'
$gateSeenTiles = @{}
$gateSeenCells = @{}
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($gateFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 11) { continue }
    $nums = @()
    foreach ($field in $fields[0..10]) {
        $value = 0
        if (-not [int]::TryParse($field.Trim(), [ref]$value)) { $nums = @(); break }
        $nums += $value
    }
    if ($nums.Count -lt 11) { continue }
    $srcMap = $nums[5]
    $dstMap = $nums[8]

    $footprint = Get-FieldObjectFootprint $nums[2]
    $xsize = $footprint[0]
    $ysize = $footprint[1]
    $centerX = $footprint[2]
    $centerY = $footprint[3]

    # On map 0 the block is measured from the map file rather than derived from the
    # editor's marker (see the note above `Get-StampedBlock`). Every other source map keeps
    # the text anchor: this port has no reader for those map files yet, so their blocks stay
    # the formula's, and the card says so.
    $srcX = $nums[6]
    $srcY = $nums[7]
    if ($srcMap -eq 0) {
        $anchor = Get-StampedBlock $nums[0] $srcX $srcY $xsize $ysize $centerX $centerY
        if ($null -eq $anchor) {
            $gateUnplaced.Add("gate $($nums[0]) -> map ${dstMap}: declared at ($srcX,$srcY) but not stamped in MainMap.map")
            continue
        }
        $srcX = $anchor[0]
        $srcY = $anchor[1]
    }

    # `PutFieldObject` refuses a block that overlaps one already stamped
    # (`Map-FieldObject.cpp:88-97`), and the client's gate table is keyed by unique
    # (`IsEnterGate`, `Client.cpp:8128`), so two rows cannot both own a cell. The file
    # holds exactly one such tie - unique 670, map 418 (32,78), two destinations
    # ("Tower 3->4" and "Tower 4->3") - and the map builder's own order decides:
    # the first row wins, the rest are reported and skipped.
    $cells = @()
    $overlap = $null
    for ($i = 0; $i -lt $ysize; $i++) {
        for ($j = 0; $j -lt $xsize; $j++) {
            $x = $srcX + $centerX - $xsize + 1 + $j
            $y = $srcY + $centerY - $ysize + 1 + $i
            $cellKey = "$srcMap/$x/$y"
            if ($gateSeenCells.ContainsKey($cellKey)) { $overlap = "$($gateSeenCells[$cellKey]) at $cellKey"; break }
            $cells += $cellKey
        }
        if ($overlap) { break }
    }
    if ($overlap) {
        $gateSkipped.Add("gate $($nums[0]) at $srcMap/$srcX/$srcY -> $dstMap/$($nums[9])/$($nums[10]): block already claimed by $overlap")
        continue
    }
    $tileKey = "$srcMap/$($nums[6])/$($nums[7])"
    $gateSeenTiles[$tileKey] = $true
    foreach ($cellKey in $cells) { $gateSeenCells[$cellKey] = $nums[0] }

    $gates.Add([pscustomobject]@{
        SrcMap  = $srcMap
        SrcX    = $srcX
        SrcY    = $srcY
        Xsize   = $xsize
        Ysize   = $ysize
        CenterX = $centerX
        CenterY = $centerY
        DstMap  = $dstMap
        DstX    = $nums[9]
        DstY    = $nums[10]
        # Columns 4 and 5: the item the gate requires, and how many. The legacy checks
        # them before warping and never consumes them (`char.cpp:9123-9133`).
        ItemUnique = $nums[3]
        ItemNum    = $nums[4]
    })
}
if ($gates.Count -eq 0) { throw "no gate rows found in $gateFile" }
# Kept in `GateInfoE.txt` order (no sort): that order is the one the table's own
# doc documents, and it is deterministic by itself.
$gateSorted = $gates.ToArray()

$gateBody = @($gateSorted | ForEach-Object {
    @(
        "    Gate {",
        "        src_map: $($_.SrcMap),",
        "        src_x: $($_.SrcX),",
        "        src_y: $($_.SrcY),",
        "        xsize: $($_.Xsize),",
        "        ysize: $($_.Ysize),",
        "        center_x: $($_.CenterX),",
        "        center_y: $($_.CenterY),",
        "        dst_map: $($_.DstMap),",
        "        dst_x: $($_.DstX),",
        "        dst_y: $($_.DstY),",
        "        item_unique: $($_.ItemUnique),",
        "        item_num: $($_.ItemNum),",
        "    },"
    )
})
$gateGenerated = @($gateSorted | ForEach-Object { '{0}/{1}/{2}/{3}/{4}/{5}/{6}/{7}/{8}/{9}/{10}/{11}' -f $_.SrcMap, $_.SrcX, $_.SrcY, $_.Xsize, $_.Ysize, $_.CenterX, $_.CenterY, $_.DstMap, $_.DstX, $_.DstY, $_.ItemUnique, $_.ItemNum })

# --- village entrances: the FOTYPE_VILLAGE branch's data ---------------------
#
# `cltCharServer::CharServerMoveByGate`'s village branch (`char.cpp:8983-9027`) takes the
# village unique from the cell's field-object para (`:8986`), bounds it against
# `MAX_VILLAGE_NUMBER` (`:8988`), and warps to
#     `pclVillageInfo[villageunique]->siVillageUnique`         the village's own map (`:9008`)
#     at `pclMapTypeInfo[maptypeunique]->siCenterX/siCenterY`  that map type's centre (`:9010-9012`)
#     `cltWarpInfo(1, mapindex, false, false, centerx, centery, x, y); SetWarpInfo(..., 30)` (`:9014-9015`)
# with `IsEnterLevel(villageunique, level)` as a gate this port does not model yet (`:9006`),
# an enemy-castle refusal (`:8992`) and a prison silent-deny (`:8998`).
#
# Every number is text data:
#   `villageinfoE.txt`        Unique, Province, TownName, PosX, PosY (the entrance on map 0),
#                             MapIndex (the village's own map)
#   `mapinfoE.txt`            that map row's third column is the map type unique
#   `MapTypeInfo.txt`         map type unique -> MapUniqueString
#   `MapTypeInfo_Village.txt` MapUniqueString -> CenterX, CenterY
#
# The entrance's *footprint* is the one number no text file carries: it lives in the map's
# field-object grid, exactly as a gate's does, but a gate row names its object (`FOUnique`)
# and a village row does not. So the table matches the entrance tile itself, and the runtime
# logs any order landing within three tiles of it (`village_within`) - that is how the block
# gets measured, from the first click.
$villageFile = Join-Path $DataRoot 'Data\villageinfoE.txt'
if (-not (Test-Path $villageFile)) { throw "village table not found: '$villageFile' (set -DataRoot)" }
$mapInfoFile = Join-Path $DataRoot 'Data\mapinfoE.txt'
$mapTypeFile = Join-Path $DataRoot 'Data\MapTypeInfo.txt'
$villageTypeFile = Join-Path $DataRoot 'Data\MapTypeInfo_Village.txt'
foreach ($needed in @($mapInfoFile, $mapTypeFile, $villageTypeFile)) {
    if (-not (Test-Path $needed)) { throw "map data not found: '$needed' (set -DataRoot)" }
}

# map index -> map type unique (mapinfoE.txt column 3)
$mapTypeOfMap = @{}
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($mapInfoFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 3) { continue }
    $mapId = 0
    $typeUnique = 0
    if (-not [int]::TryParse($fields[0].Trim(), [ref]$mapId)) { continue }
    if (-not [int]::TryParse($fields[2].Trim(), [ref]$typeUnique)) { continue }
    $mapTypeOfMap[$mapId] = $typeUnique
}

# map type unique -> name (MapTypeInfo.txt)
$typeNameOfUnique = @{}
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($mapTypeFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 2) { continue }
    $unique = 0
    if (-not [int]::TryParse($fields[0].Trim(), [ref]$unique)) { continue }
    $typeNameOfUnique[$unique] = $fields[1].Trim()
}

# map type name -> centre and gate (MapTypeInfo_Village.txt:
#   MapUniqueString, MapAttb, CenterX, CenterY, GateX, GateY, Cost, Level, Slots...)
$centerOfTypeName = @{}
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($villageTypeFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 6) { continue }
    $values = @()
    $ok = $true
    foreach ($index in @(2, 3, 4, 5)) {
        $value = 0
        if (-not [int]::TryParse($fields[$index].Trim(), [ref]$value)) { $ok = $false; break }
        $values += $value
    }
    if (-not $ok) { continue }
    $centerOfTypeName[$fields[0].Trim()] = $values
}

$villages = New-Object 'System.Collections.Generic.List[object]'
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($villageFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 6) { continue }
    $nums = @()
    $ok = $true
    foreach ($index in @(0, 3, 4, 5)) {
        $value = 0
        if (-not [int]::TryParse($fields[$index].Trim(), [ref]$value)) { $ok = $false; break }
        $nums += $value
    }
    if (-not $ok) { continue }
    $unique = $nums[0]
    $srcX = $nums[1]
    $srcY = $nums[2]
    $dstMap = $nums[3]
    if (-not $mapTypeOfMap.ContainsKey($dstMap)) { throw "village ${unique}: map $dstMap is not in mapinfoE.txt" }
    $typeName = $typeNameOfUnique[$mapTypeOfMap[$dstMap]]
    if (-not $typeName -or -not $centerOfTypeName.ContainsKey($typeName)) {
        throw "village ${unique}: no centre for map $dstMap (type '$typeName')"
    }
    $centre = $centerOfTypeName[$typeName]
    # The entrance object is `KoreanVillageSmall` (FOUnique 1) or `KoreanVillageSmallUp`
    # (FOUnique 5); both detail files read `8 8 1 1 19 1 9 2`
    # (`FieldObject\Korean.mz -> KoreanVillageSmall/KoreanVillageSmall.txt`), so the block is
    # 8x8 with centre (1,1). `Get-StampedBlock` then measures the shipped block and derives
    # the anchor, throwing if the size does not reproduce it.
    $anchor = Get-StampedBlock $unique $srcX $srcY 8 8 1 1
    if ($null -eq $anchor) {
        throw "village ${unique}: declared at ($srcX,$srcY) but not stamped in MainMap.map"
    }
    $villages.Add([pscustomobject]@{
        Unique  = $unique
        SrcX    = $anchor[0]
        SrcY    = $anchor[1]
        Xsize   = 8
        Ysize   = 8
        CenterX = 1
        CenterY = 1
        DstMap  = $dstMap
        DstX    = $centre[0]
        DstY    = $centre[1]
        # `GateX`/`GateY` of the village's map type: the gate *inside* the town, which is
        # what a character walks to in order to leave it again.
        ExitX   = $centre[2]
        ExitY   = $centre[3]
    })
}
if ($villages.Count -eq 0) { throw "no village rows found in $villageFile" }
$villageSorted = $villages.ToArray()
$villageBody = @($villageSorted | ForEach-Object {
    @(
        "    Village {",
        "        unique: $($_.Unique),",
        "        src_x: $($_.SrcX),",
        "        src_y: $($_.SrcY),",
        "        xsize: $($_.Xsize),",
        "        ysize: $($_.Ysize),",
        "        center_x: $($_.CenterX),",
        "        center_y: $($_.CenterY),",
        "        dst_map: $($_.DstMap),",
        "        dst_x: $($_.DstX),",
        "        dst_y: $($_.DstY),",
        "        exit_x: $($_.ExitX),",
        "        exit_y: $($_.ExitY),",
        "    },"
    )
})
$villageGenerated = @($villageSorted | ForEach-Object { '{0}/{1}/{2}/{3}/{4}/{5}/{6}/{7}/{8}/{9}/{10}/{11}' -f $_.Unique, $_.SrcX, $_.SrcY, $_.Xsize, $_.Ysize, $_.CenterX, $_.CenterY, $_.DstMap, $_.DstX, $_.DstY, $_.ExitX, $_.ExitY })

if ($Check) {
    if ($Table -ne 'gate') {
        if (-not (Test-Path $RustPath)) { throw "Rust source not found: '$RustPath' (set -RustPath)" }
        $text = Get-Content $RustPath -Raw
        # Scope the scan to the committed table block: the module's own tests build
        # `NpcPlacement` values too, and counting those would make this check lie.
        $tableMatch = [regex]::Match(
            $text,
            'pub const NPC_PLACEMENTS: &\[NpcPlacement\] = &\[(.*?)\];',
            [System.Text.RegularExpressions.RegexOptions]::Singleline)
        if (-not $tableMatch.Success) { throw "no NPC_PLACEMENTS table found in $RustPath" }
        $pattern = 'NpcPlacement\s*\{\s*kind:\s*(\d+)\s*,\s*map_id:\s*(-?\d+)\s*,\s*x:\s*(\d+)\s*,\s*y:\s*(\d+)\s*,\s*level:\s*(\d+)\s*,?\s*\}'
        $committed = @([regex]::Matches(
                $tableMatch.Groups[1].Value,
                $pattern,
                [System.Text.RegularExpressions.RegexOptions]::Singleline) | ForEach-Object {
            '{0}/{1}/{2}/{3}/{4}' -f $_.Groups[1].Value, $_.Groups[2].Value, $_.Groups[3].Value,
                $_.Groups[4].Value, $_.Groups[5].Value
        })
        $generated = @($sorted | ForEach-Object { '{0}/{1}/{2}/{3}/{4}' -f $_.Kind, $_.MapId, $_.X, $_.Y, $_.Level })
        if (($generated -join ';') -ne ($committed -join ';')) {
            throw "NPC_PLACEMENTS is stale: $($generated.Count) generated vs $($committed.Count) committed. Re-run without -Check and paste the block."
        }
        Write-Host "NPC_PLACEMENTS is current: $($generated.Count) placements from $fileCount group file(s) on map $MapIndex"
    }
    if ($Table -ne 'npc') {
        if (-not (Test-Path $GateRustPath)) { throw "Rust source not found: '$GateRustPath' (set -GateRustPath)" }
        $text = Get-Content $GateRustPath -Raw
        $tableMatch = [regex]::Match(
            $text,
            'const GATES: &\[Gate\] = &\[(.*?)\];',
            [System.Text.RegularExpressions.RegexOptions]::Singleline)
        if (-not $tableMatch.Success) { throw "no GATES table found in $GateRustPath" }
        $pattern = 'Gate\s*\{\s*src_map:\s*(-?\d+)\s*,\s*src_x:\s*(\d+)\s*,\s*src_y:\s*(\d+)\s*,\s*xsize:\s*(\d+)\s*,\s*ysize:\s*(\d+)\s*,\s*center_x:\s*(-?\d+)\s*,\s*center_y:\s*(-?\d+)\s*,\s*dst_map:\s*(-?\d+)\s*,\s*dst_x:\s*(\d+)\s*,\s*dst_y:\s*(\d+)\s*,\s*item_unique:\s*(\d+)\s*,\s*item_num:\s*(\d+)\s*,?\s*\}'
        $committed = @([regex]::Matches(
                $tableMatch.Groups[1].Value,
                $pattern,
                [System.Text.RegularExpressions.RegexOptions]::Singleline) | ForEach-Object {
            '{0}/{1}/{2}/{3}/{4}/{5}/{6}/{7}/{8}/{9}/{10}/{11}' -f $_.Groups[1].Value, $_.Groups[2].Value, $_.Groups[3].Value,
                $_.Groups[4].Value, $_.Groups[5].Value, $_.Groups[6].Value, $_.Groups[7].Value,
                $_.Groups[8].Value, $_.Groups[9].Value, $_.Groups[10].Value, $_.Groups[11].Value, $_.Groups[12].Value
        })
        if (($gateGenerated -join ';') -ne ($committed -join ';')) {
            throw "GATES is stale: $($gateGenerated.Count) generated vs $($committed.Count) committed. Re-run without -Check and paste the block."
        }
        Write-Host "GATES is current: $($gateGenerated.Count) row(s), $($gateSeenCells.Count) covered cell(s), $($footprintCache.Count) field object(s)"
    }
    if ($Table -ne 'npc' -and $Table -ne 'gate') {
        if (-not (Test-Path $GateRustPath)) { throw "Rust source not found: '$GateRustPath' (set -GateRustPath)" }
        $text = Get-Content $GateRustPath -Raw
        $tableMatch = [regex]::Match(
            $text,
            'const VILLAGES: &\[Village\] = &\[(.*?)\];',
            [System.Text.RegularExpressions.RegexOptions]::Singleline)
        if (-not $tableMatch.Success) { throw "no VILLAGES table found in $GateRustPath" }
        $pattern = 'Village\s*\{\s*unique:\s*(\d+)\s*,\s*src_x:\s*(\d+)\s*,\s*src_y:\s*(\d+)\s*,\s*xsize:\s*(\d+)\s*,\s*ysize:\s*(\d+)\s*,\s*center_x:\s*(-?\d+)\s*,\s*center_y:\s*(-?\d+)\s*,\s*dst_map:\s*(\d+)\s*,\s*dst_x:\s*(\d+)\s*,\s*dst_y:\s*(\d+)\s*,\s*exit_x:\s*(\d+)\s*,\s*exit_y:\s*(\d+)\s*,?\s*\}'
        $committed = @([regex]::Matches(
                $tableMatch.Groups[1].Value,
                $pattern,
                [System.Text.RegularExpressions.RegexOptions]::Singleline) | ForEach-Object {
            '{0}/{1}/{2}/{3}/{4}/{5}/{6}/{7}/{8}/{9}/{10}/{11}' -f $_.Groups[1].Value, $_.Groups[2].Value, $_.Groups[3].Value,
                $_.Groups[4].Value, $_.Groups[5].Value, $_.Groups[6].Value, $_.Groups[7].Value,
                $_.Groups[8].Value, $_.Groups[9].Value, $_.Groups[10].Value, $_.Groups[11].Value, $_.Groups[12].Value
        })
        if (($villageGenerated -join ';') -ne ($committed -join ';')) {
            throw "VILLAGES is stale: $($villageGenerated.Count) generated vs $($committed.Count) committed. Re-run without -Check and paste the block."
        }
        Write-Host "VILLAGES is current: $($villageGenerated.Count) entrance(s) on map 0"
    }
    exit 0
}

if ($Table -ne 'gate') {
    Write-Host "# $($sorted.Count) placement(s) from $fileCount group file(s) on map $MapIndex" -ForegroundColor Cyan
    if ($skipped.Count -gt 0) {
        Write-Host "# skipped $($skipped.Count) kind(s) with no id in KindInfo_Unique.txt:" -ForegroundColor Yellow
        foreach ($entry in $skipped) { Write-Host "#   $entry" -ForegroundColor Yellow }
    }
    @("pub const NPC_PLACEMENTS: &[NpcPlacement] = &[") + $body + @("];")
}
if ($Table -ne 'npc') {
    Write-Host "# $($gateSorted.Count) gate row(s) covering $($gateSeenCells.Count) cell(s), $($footprintCache.Count) field object(s)" -ForegroundColor Cyan
    if ($gateSkipped.Count -gt 0) {
        Write-Host "# skipped $($gateSkipped.Count) gate row(s) whose block was already claimed:" -ForegroundColor Yellow
        foreach ($entry in $gateSkipped) { Write-Host "#   $entry" -ForegroundColor Yellow }
    }
    @("const GATES: &[Gate] = &[") + $gateBody + @("];")
}
if ($Table -ne 'npc' -and $Table -ne 'gate') {
    Write-Host "# $($villageSorted.Count) village entrance(s) on map 0" -ForegroundColor Cyan
    @("const VILLAGES: &[Village] = &[") + $villageBody + @("];")
}
