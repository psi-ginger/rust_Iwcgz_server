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
    [ValidateSet('all', 'npc', 'gate')]
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

# --- gates on the served maps ---------------------------------------------------
#
# `GateInfoE.txt` columns: Unique, TxtNum, FOUnique, ItemUnique, ItemNum, SrcMap,
# SrcX, SrcY, DstMap, DstX, DstY, PKAreaGate, then a comment. A gate is emitted
# only when **both** ends are maps this port serves: the file also carries every
# other city's field-to-dungeon links, and pulling those in would add hundreds of
# rows for maps nothing can reach yet. The served set grows with the maps we
# serve, one line here.
$servedMaps = @(0, $MapIndex)
$gateFile = Join-Path $DataRoot 'Data\GateInfoE.txt'
if (-not (Test-Path $gateFile)) { throw "gate table not found: '$gateFile' (set -DataRoot)" }

$gates = New-Object 'System.Collections.Generic.List[object]'
$gateSeenTiles = @{}
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
    if (($servedMaps -notcontains $srcMap) -or ($servedMaps -notcontains $dstMap)) { continue }
    $tileKey = "$srcMap/$($nums[6])/$($nums[7])"
    if ($gateSeenTiles.ContainsKey($tileKey)) {
        throw "$($gateFile): two gate rows claim source tile $tileKey"
    }
    $gateSeenTiles[$tileKey] = $true
    $gates.Add([pscustomobject]@{
        SrcMap = $srcMap
        SrcX   = $nums[6]
        SrcY   = $nums[7]
        DstMap = $dstMap
        DstX   = $nums[9]
        DstY   = $nums[10]
    })
}
if ($gates.Count -eq 0) { throw "no gate rows found between maps $($servedMaps -join ', ')" }
# Kept in `GateInfoE.txt` order (no sort): that order is the one the table's own
# doc documents, and it is deterministic by itself.
$gateSorted = $gates.ToArray()

$gateBody = @($gateSorted | ForEach-Object {
    @(
        "    Gate {",
        "        src_map: $($_.SrcMap),",
        "        src_x: $($_.SrcX),",
        "        src_y: $($_.SrcY),",
        "        dst_map: $($_.DstMap),",
        "        dst_x: $($_.DstX),",
        "        dst_y: $($_.DstY),",
        "    },"
    )
})
$gateGenerated = @($gateSorted | ForEach-Object { '{0}/{1}/{2}/{3}/{4}/{5}' -f $_.SrcMap, $_.SrcX, $_.SrcY, $_.DstMap, $_.DstX, $_.DstY })

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
        $pattern = 'Gate\s*\{\s*src_map:\s*(-?\d+)\s*,\s*src_x:\s*(\d+)\s*,\s*src_y:\s*(\d+)\s*,\s*dst_map:\s*(-?\d+)\s*,\s*dst_x:\s*(\d+)\s*,\s*dst_y:\s*(\d+)\s*,?\s*\}'
        $committed = @([regex]::Matches(
                $tableMatch.Groups[1].Value,
                $pattern,
                [System.Text.RegularExpressions.RegexOptions]::Singleline) | ForEach-Object {
            '{0}/{1}/{2}/{3}/{4}/{5}' -f $_.Groups[1].Value, $_.Groups[2].Value, $_.Groups[3].Value,
                $_.Groups[4].Value, $_.Groups[5].Value, $_.Groups[6].Value
        })
        if (($gateGenerated -join ';') -ne ($committed -join ';')) {
            throw "GATES is stale: $($gateGenerated.Count) generated vs $($committed.Count) committed. Re-run without -Check and paste the block."
        }
        Write-Host "GATES is current: $($gateGenerated.Count) row(s) between maps $($servedMaps -join ', ')"
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
    Write-Host "# $($gateSorted.Count) gate row(s) between maps $($servedMaps -join ', ')" -ForegroundColor Cyan
    @("const GATES: &[Gate] = &[") + $gateBody + @("];")
}
