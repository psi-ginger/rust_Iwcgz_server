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

$gates = New-Object 'System.Collections.Generic.List[object]'
$gateSkipped = New-Object 'System.Collections.Generic.List[string]'
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
            $x = $nums[6] + $centerX - $xsize + 1 + $j
            $y = $nums[7] + $centerY - $ysize + 1 + $i
            $cellKey = "$srcMap/$x/$y"
            if ($gateSeenCells.ContainsKey($cellKey)) { $overlap = "$($gateSeenCells[$cellKey]) at $cellKey"; break }
            $cells += $cellKey
        }
        if ($overlap) { break }
    }
    if ($overlap) {
        $gateSkipped.Add("gate $($nums[0]) at $srcMap/$($nums[6])/$($nums[7]) -> $dstMap/$($nums[9])/$($nums[10]): block already claimed by $overlap")
        continue
    }
    $tileKey = "$srcMap/$($nums[6])/$($nums[7])"
    $gateSeenTiles[$tileKey] = $true
    foreach ($cellKey in $cells) { $gateSeenCells[$cellKey] = $nums[0] }

    $gates.Add([pscustomobject]@{
        SrcMap  = $srcMap
        SrcX    = $nums[6]
        SrcY    = $nums[7]
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

# map type name -> centre (MapTypeInfo_Village.txt: MapUniqueString, MapAttb, CenterX, CenterY, ...)
$centerOfTypeName = @{}
$inData = $false
foreach ($line in [System.IO.File]::ReadAllLines($villageTypeFile, [System.Text.Encoding]::Unicode)) {
    if ($line -match '^\s*<Data>') { $inData = $true; continue }
    if (-not $inData) { continue }
    $fields = $line -split "`t"
    if ($fields.Count -lt 4) { continue }
    $cx = 0
    $cy = 0
    if (-not [int]::TryParse($fields[2].Trim(), [ref]$cx)) { continue }
    if (-not [int]::TryParse($fields[3].Trim(), [ref]$cy)) { continue }
    $centerOfTypeName[$fields[0].Trim()] = @($cx, $cy)
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
    $villages.Add([pscustomobject]@{
        Unique = $unique
        SrcX   = $srcX
        SrcY   = $srcY
        DstMap = $dstMap
        DstX   = $centre[0]
        DstY   = $centre[1]
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
        "        dst_map: $($_.DstMap),",
        "        dst_x: $($_.DstX),",
        "        dst_y: $($_.DstY),",
        "    },"
    )
})
$villageGenerated = @($villageSorted | ForEach-Object { '{0}/{1}/{2}/{3}/{4}/{5}' -f $_.Unique, $_.SrcX, $_.SrcY, $_.DstMap, $_.DstX, $_.DstY })

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
        $pattern = 'Village\s*\{\s*unique:\s*(\d+)\s*,\s*src_x:\s*(\d+)\s*,\s*src_y:\s*(\d+)\s*,\s*dst_map:\s*(\d+)\s*,\s*dst_x:\s*(\d+)\s*,\s*dst_y:\s*(\d+)\s*,?\s*\}'
        $committed = @([regex]::Matches(
                $tableMatch.Groups[1].Value,
                $pattern,
                [System.Text.RegularExpressions.RegexOptions]::Singleline) | ForEach-Object {
            '{0}/{1}/{2}/{3}/{4}/{5}' -f $_.Groups[1].Value, $_.Groups[2].Value, $_.Groups[3].Value,
                $_.Groups[4].Value, $_.Groups[5].Value, $_.Groups[6].Value
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
