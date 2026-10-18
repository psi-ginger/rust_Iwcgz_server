# generate-kr-canonical-tables.ps1 - regenerate the KR-baseline canonical monster tables.
#
# Emits three committed tables from the Korean baseline plus the active English data,
# and `-Check` fails when any has drifted:
#
#   * `crates/gz-data/data/CharAttackInfoCanonical.txt` - combat stats. EN 27-column
#     schema; every column the KR baseline carries takes the KR value, the 7 columns
#     only EN has (SortNUM, Luk, AttackE/R, DefenE/R, Wis) keep the EN value, and the
#     `Name` column keeps the English display name. The EN rows are followed by the
#     verified exact-code KR-only kinds - KR-only codes that already carry an EN wire
#     id, so the EN client can resolve them (`KIND_BOMBDRAGON`, `KIND_JANGBOGO`,
#     strategy doc 3.4). Human-confirmed rename pairs (Tier C) upgrade the EN-only row
#     of the paired EN code in place, so they add no row.
#   * `crates/gz-data/data/CharItemInfoCanonical.txt` - capture + drops. EN 6-column
#     schema, same merge rule; the `ItemInfo` drop string comes from KR verbatim. A
#     rename pair whose EN code has no EN row is appended.
#   * `crates/gz-data/data/KindReconciliation.txt` - roster audit. Every kind in either
#     roster or either spawn-group set, with its tier, EN wire id, spawning side and the
#     EN code a rename pair resolves it to. The spawn gate: a kind an EN group spawns
#     must carry a wire id - every one does but the 3 quest placeholders, which no roster
#     holds. The KR groups' extra kinds cannot resolve on an EN client and stay
#     quarantined, minus the rename pairs (strategy doc 3.4, 4).
#
# The decision record is docs/DESIGN_MONSTER_KR_BASELINE_STRATEGY.md (docs repo):
# the Korean baseline `Goonzu_kr_last` is the source of truth; English names and
# the EN wire ids (`Data\KindInfo_Unique.txt`, never modified) layer on top.
#   - shared kind codes are upgraded in place;
#   - EN-only codes keep their EN values (provisional - no KR truth exists), unless a
#     human confirmed them as a rename-pair target, which makes them KR-sourced;
#   - KR-only codes are quarantined: an EN client cannot resolve a kind id it does not
#     have, so they are reported but not emitted - unless a human confirmed a rename
#     pair to an EN code that carries the wire id (Tier C), which resolves the KR code.
#
# The KR `CharInfo\*.txt` files are NCR-encrypted (24-byte NCryptHeader, the read
# path `NLib\NDataLoader.cpp:104-114`); the decoder below is the legacy algorithm
# from `NLib\NUtil.cpp:300-331`. Plaintext is EUC-KR (code page 949).
#
# Output files are UTF-16LE with BOM in the NDataLoader layout (`<Data>` marker,
# column-name row, rows, `<End>`), so `gz_data::kindinfo::parse_char_attack` /
# `gz_data::drops::parse_char_drops` read them unchanged - and so could a legacy
# loader.
param(
    [string]$KRRoot = "C:\Data\04_Server_Goonzu\Goonzu_kr_last",
    [string]$ENRoot = "C:\Data\04_Server_Goonzu\New Era Live",
    [string]$OutDir = "",
    [switch]$Check
)
$ErrorActionPreference = "Stop"
if (-not $OutDir) { $OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) "crates\gz-data\data" }

$euc = [Text.Encoding]::GetEncoding(949)

# NCR = NCryptHeader (24 B) + NByteDecrypt chain, NUtil.cpp:300-331 / NUtil.h:9-22.
function Read-NcrText([string]$Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    if ([Text.Encoding]::ASCII.GetString($b[0..3]) -ne "NCR`0") { throw "$Path is not an NCR file" }
    $size = [BitConverter]::ToUInt32($b, 8)
    $key = $b[12]
    if (24 + $size -gt $b.Length) { throw "$Path size mismatch: hdr=$size file=$($b.Length)" }
    $ct = $b[24..(24 + $size - 1)]
    $pt = New-Object byte[] $size
    for ($i = 0; $i -lt $size - 1; $i++) { $pt[$i] = ((($ct[$i] - $key) -band 0xFF) -bxor $ct[$i + 1]) }
    $pt[$size - 1] = $ct[$size - 1] -bxor $key
    $euc.GetString($pt)
}

# A table = header cell array + data-row cell arrays. Data rows are the rows whose
# first cell is a KIND_ code, which drops the preamble, the markers and any
# trailing junk in one rule (the shipped EN item file carries two Japanese rows
# after <End> that the legacy loader never reads).
function Read-Table([string[]]$Lines) {
    $header = $null
    $rows = @()
    foreach ($line in $Lines) {
        $cells = $line -split "`t"
        $first = $cells[0].Trim().TrimStart([char]0xFEFF)
        if ($null -eq $header) {
            # EN: the column-name row follows <Data>; the KR files carry no marker
            # and the column-name row is the row whose first cell is "Char".
            if ($first -eq "Char") { $header = @($cells | ForEach-Object { $_.Trim() }) }
            continue
        }
        if ($first.StartsWith("<End>")) { break }
        if ($first.StartsWith("KIND_")) { $rows += ,$cells }
    }
    if ($null -eq $header) { throw "no column-name row (Char) found" }
    @{ Header = $header; Rows = $rows }
}

function Read-EnTable([string]$Path) {
    Read-Table ([Text.Encoding]::Unicode.GetString([IO.File]::ReadAllBytes($Path)) -split "`r?`n")
}

function Read-KrTable([string]$Path) {
    Read-Table ((Read-NcrText $Path) -split "`r?`n")
}

# Group files are plain EUC-KR, but a few ship UTF-16 or NCR; sniff rather than assume.
function Read-LooseText([string]$Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    if ($b.Length -ge 24 -and [Text.Encoding]::ASCII.GetString($b[0..3]) -eq "NCR`0") {
        $size = [BitConverter]::ToUInt32($b, 8); $key = $b[12]
        $ct = $b[24..(24 + $size - 1)]; $pt = New-Object byte[] $size
        for ($i = 0; $i -lt $size - 1; $i++) { $pt[$i] = ((($ct[$i] - $key) -band 0xFF) -bxor $ct[$i + 1]) }
        $pt[$size - 1] = $ct[$size - 1] -bxor $key
        return $euc.GetString($pt)
    }
    if ($b.Length -ge 2 -and $b[0] -eq 0xFF -and $b[1] -eq 0xFE) { return [Text.Encoding]::Unicode.GetString($b, 2, $b.Length - 2) }
    $euc.GetString($b)
}

# Every KIND_ code a spawn-group set references (the codes are ASCII in every encoding).
function Get-ReferencedKinds([string]$Dir) {
    $rx = [regex]'KIND_[A-Za-z0-9_]+'
    $set = New-Object System.Collections.Generic.HashSet[string]
    foreach ($f in (Get-ChildItem $Dir -File -ErrorAction SilentlyContinue)) {
        foreach ($m in $rx.Matches((Read-LooseText $f.FullName))) { [void]$set.Add($m.Value) }
    }
    $set
}

function ColMap([string[]]$Header) {
    $m = @{}
    for ($i = 0; $i -lt $Header.Count; $i++) { if ($Header[$i]) { $m[$Header[$i]] = $i } }
    $m
}

# The merge: EN row order and EN row set, KR values for every shared value column
# (everything except the Char key and the Name display column), EN values kept for
# columns the KR baseline never carried. KR codes are joined exactly, with a
# case-insensitive fallback that is reported (strategy doc 7.4: case-only pairs are
# recorded, never silent).
function Merge-Table($Kr, $En) {
    $krMap = ColMap $Kr.Header
    $enMap = ColMap $En.Header
    $krByCode = @{}
    $dups = @()
    foreach ($r in $Kr.Rows) {
        $code = $r[$krMap["Char"]].Trim()
        if ($krByCode.ContainsKey($code)) { $dups += $code }
        $krByCode[$code] = $r # last wins (the documented duplicate rule)
    }
    $krLower = @{}
    foreach ($k in $krByCode.Keys) { $krLower[$k.ToLower()] = $k }
    $common = @($krMap.Keys | Where-Object { $enMap.ContainsKey($_) -and $_ -ne "Char" -and $_ -ne "Name" })
    $outRows = @()
    $upgraded = 0
    $provisional = 0
    $caseOnly = @()
    $enCodes = @()
    foreach ($r in $En.Rows) {
        $cells = @($r) + @("" * ($En.Header.Count - $r.Count))
        $code = $cells[$enMap["Char"]].Trim()
        $enCodes += $code.ToLower()
        $match = $null
        if ($krByCode.ContainsKey($code)) { $match = $code }
        elseif ($krLower.ContainsKey($code.ToLower())) {
            $match = $krLower[$code.ToLower()]
            $caseOnly += "$code <-> $match"
        }
        if ($match) {
            $krRow = $krByCode[$match]
            foreach ($c in $common) { $cells[$enMap[$c]] = ($krRow[$krMap[$c]]).Trim() }
            $upgraded++
        }
        else { $provisional++ }
        $outRows += ,$cells
    }
    $krOnly = @($krByCode.Keys | Where-Object { $enCodes -notcontains $_.ToLower() } | Sort-Object)
    @{
        Header    = $En.Header
        Rows      = $outRows
        Upgraded  = $upgraded
        Provisional = $provisional
        Dups      = @($dups | Sort-Object -Unique)
        CaseOnly  = $caseOnly
        KrOnly    = $krOnly
        KrByCode  = $krByCode
    }
}

# Tier C: human-confirmed rename pairs (strategy doc 4, docs/MONSTER_PAIRING_CANDIDATES.md).
# A KR-only code reviewed and paired to an EN-only code that carries the EN wire id: the
# EN code is the runtime identity - the EN client resolves its id - so the EN row takes
# the KR values (stats, attributes, capture numbers and the drop string) while keeping the
# EN code and the English display name. A pair whose EN code has no EN row gets one
# appended, as Tier B does. Only pairs a human has confirmed are listed; every unlisted
# KR-only kind stays quarantined.
function Apply-RenamePairs($Table, $KrTable, $Pairs) {
    $krMap = ColMap $KrTable.Header
    $enMap = ColMap $Table.Header
    $common = @($krMap.Keys | Where-Object { $enMap.ContainsKey($_) -and $_ -ne "Char" -and $_ -ne "Name" })
    $krByCode = @{}
    foreach ($r in $KrTable.Rows) { $krByCode[$r[$krMap["Char"]].Trim()] = $r }
    $rowAt = @{}
    for ($i = 0; $i -lt $Table.Rows.Count; $i++) { $rowAt[$Table.Rows[$i][$enMap["Char"]].Trim()] = $i }
    $upgraded = @()
    $appended = @()
    foreach ($krCode in @($Pairs.Keys)) {
        $enCode = $Pairs[$krCode]
        # A missing KR row means the table has no KR data for the pair (e.g. no drop
        # row): nothing to apply, the EN row stays as it is.
        if (-not $krByCode.ContainsKey($krCode)) { continue }
        $krRow = $krByCode[$krCode]
        if ($rowAt.ContainsKey($enCode)) {
            $cells = $Table.Rows[$rowAt[$enCode]]
            foreach ($c in $common) { if ($krMap[$c] -lt $krRow.Count) { $cells[$enMap[$c]] = $krRow[$krMap[$c]].Trim() } }
            $upgraded += "$krCode->$enCode"
        }
        else {
            $cells = @("") * $Table.Header.Count
            foreach ($c in $krMap.Keys) { if ($enMap.ContainsKey($c) -and $krMap[$c] -lt $krRow.Count) { $cells[$enMap[$c]] = $krRow[$krMap[$c]].Trim() } }
            $cells[$enMap["Char"]] = $enCode
            $Table.Rows += , $cells
            $appended += "$krCode->$enCode"
        }
    }
    @{ Upgraded = $upgraded; Appended = $appended }
}

function Write-CanonTable([string]$Path, [string]$Title, $Table, [string]$Provenance) {
    $sb = New-Object Text.StringBuilder
    [void]$sb.AppendLine($Title)
    [void]$sb.AppendLine("2026-10-16")
    [void]$sb.AppendLine($Provenance)
    [void]$sb.AppendLine("<Data>")
    [void]$sb.AppendLine(($Table.Header -join "`t"))
    foreach ($r in $Table.Rows) {
        $cells = @($r)
        if ($cells.Count -lt $Table.Header.Count) { $cells += @("" * ($Table.Header.Count - $cells.Count)) }
        [void]$sb.AppendLine((@($cells | Select-Object -First $Table.Header.Count) -join "`t"))
    }
    [void]$sb.AppendLine("<End>")
    $text = $sb.ToString()
    if ($Check) {
        if (-not (Test-Path -LiteralPath $Path)) { throw "missing committed table: $Path" }
        $have = [Text.Encoding]::Unicode.GetString([IO.File]::ReadAllBytes($Path)).TrimStart([char]0xFEFF)
        if ($have -ne $text) { throw "drift: $Path differs from the regenerated merge; re-run without -Check" }
        Write-Host "  OK $Path" -ForegroundColor Green
    }
    else {
        New-Item -ItemType Directory -Force -Path (Split-Path $Path) | Out-Null
        [IO.File]::WriteAllText($Path, $text, [Text.Encoding]::Unicode)
        Write-Host "  wrote $Path ($($Table.Rows.Count) rows)" -ForegroundColor Green
    }
}

# The reconciliation is a plain UTF-8 TSV, not an NDataLoader table: it is a build
# audit, not runtime data. `.gitattributes` pins eol=lf, so emit LF and compare
# newline-insensitively rather than the platform newline.
function Write-ReconTable([string]$Path, $Rows) {
    $sb = New-Object Text.StringBuilder
    [void]$sb.Append("code`ttier`ten_wire_id`tspawnable`tpairs_to`n")
    foreach ($r in $Rows) { [void]$sb.Append(($r -join "`t")); [void]$sb.Append("`n") }
    $text = $sb.ToString()
    if ($Check) {
        if (-not (Test-Path -LiteralPath $Path)) { throw "missing committed table: $Path" }
        $have = ([IO.File]::ReadAllText($Path) -replace "`r`n", "`n")
        if ($have -ne $text) { throw "drift: $Path differs from the regenerated reconciliation; re-run without -Check" }
        Write-Host "  OK $Path" -ForegroundColor Green
    }
    else {
        New-Item -ItemType Directory -Force -Path (Split-Path $Path) | Out-Null
        [IO.File]::WriteAllText($Path, $text, [Text.UTF8Encoding]::new($false))
        Write-Host "  wrote $Path ($($Rows.Count) rows)" -ForegroundColor Green
    }
}

# --- load -------------------------------------------------------------------
$krAttack = Read-KrTable (Join-Path $KRRoot "CharInfo\CharAttackInfo.txt")
$krItem = Read-KrTable (Join-Path $KRRoot "CharInfo\CharItemInfo.txt")
$enAttack = Read-EnTable (Join-Path $ENRoot "CharInfo\CharAttackInfoE.txt")
$enItem = Read-EnTable (Join-Path $ENRoot "CharInfo\chariteminfoE.txt")

$attack = Merge-Table $krAttack $enAttack
$item = Merge-Table $krItem $enItem

# --- Tier C: confirmed rename pairs (strategy doc 4) ------------------------
# KR-only code -> EN-only code, each reviewed by a human against
# docs/MONSTER_PAIRING_CANDIDATES.md. The EN code carries the wire id, so it stays the
# runtime identity and the KR rows supply the values. Insertion order is the report order.
$renamePairs = [ordered]@{
    "KIND_BEAST06YOUNG"  = "KIND_HOR14YOUNG"
    "KIND_BLACKTIGER"    = "KIND_BLACKTIGER_MELEE"
    "KIND_BLOODDEACON"   = "KIND_DEACON"
    "KIND_BLOODPRIEST"   = "KIND_PRIEST"
    "KIND_Bum_I"         = "KIND_HUNTER_MELEE"
    "KIND_EMPERORWIZARD" = "KIND_EMPERORCAVALRY"
    "KIND_EMPERORWORKER" = "KIND_EMPERORPORTER"
    "KIND_FIREHAETAE"    = "KIND_HOR16YOUNG"
    "KIND_FULLMOON"      = "KIND_MOON"
    "KIND_GAGOIL"        = "KIND_GARGOYLE"
    "KIND_GRAVEROBBERY"  = "KIND_EMPERORROBBER"
    "KIND_Gum_I"         = "KIND_GALE_RANGE"
    "KIND_KRAKEN"        = "KIND_Kracken"
    "KIND_SASURI"        = "KIND_HOR18YOUNG"
    "KIND_SPAMONKEY"     = "KIND_MONKEYSON"
    "KIND_SPAPANDA"      = "KIND_NINJAPANDA"
    "KIND_SPARACCON"     = "KIND_RACCOONDOG"
    "KIND_SPATOAD"       = "KIND_TOAD"
    "KIND_VIRGINGHOST"   = "KIND_Ghost_NvGui1"
    "KIND_WERWOLF"       = "KIND_WEREWOLF"

    # Section 2.2 of docs/MONSTER_PAIRING_CANDIDATES.md, vetted by
    # docs/MONSTER_PAIRING_OVERNIGHT_AUDIT.md and confirmed 2026-10-18: the 7 stage-3
    # members of the numbered families (never n->1 by default) and 9 standalones.
    # Three targets are corrected from the candidate doc: the soul-guard ghosts
    # resolve to SOULGUARD_Ghostman / _Ghostwoman, and the kraken event variant to
    # KIND_Kracken_T -- not KIND_Ghost / KIND_Kracken, which are different kinds.
    "KIND_BLACKPRIEST_3"  = "KIND_BLACKPRIEST"
    "KIND_BLACKSOUL_3"    = "KIND_BLACKSOUL"
    "KIND_CLOWN_3"        = "KIND_CLOWN"
    "KIND_DUCHESS_3"      = "KIND_DUCHESS"
    "KIND_FOGBRIDE_3"     = "KIND_FOGBRIDE"
    "KIND_GENTLEMAN_3"    = "KIND_GENTLEMAN"
    "KIND_GOONZUDAY"      = "KIND_GOONZUDAYNPC"
    "KIND_KRAKEN_T"       = "KIND_Kracken_T"
    "KIND_MINIRABBIT_GIRL" = "KIND_RABBITGIRL"
    "KIND_PAPAGOBLIN_FB"  = "KIND_PAPAGOBLIN_2"
    "KIND_RUDOLF"         = "KIND_RUDOLPH"
    "KIND_SOULGUARD_GodEagle"   = "KIND_SOULGUARD_GoldEagle"
    "KIND_SOULGUARD_ManGhost"   = "KIND_SOULGUARD_Ghostman"
    "KIND_SOULGUARD_WomanGhost" = "KIND_SOULGUARD_Ghostwoman"
    "KIND_Soulguard_Miho" = "KIND_SOULGUARD_Miho_Elite"
    "KIND_THIRTEENFACE_3" = "KIND_THIRTEENFACE"
}
$attackPairs = Apply-RenamePairs $attack $krAttack $renamePairs
$itemPairs = Apply-RenamePairs $item $krItem $renamePairs
# Every pair must land on the attack master (the KR attack roster holds all 36); a
# shortfall means a typo in the pair table, not legitimate missing data.
if (($attackPairs.Upgraded.Count + $attackPairs.Appended.Count) -ne $renamePairs.Count) {
    throw "rename pairs applied to attack: $($attackPairs.Upgraded.Count + $attackPairs.Appended.Count) of $($renamePairs.Count)"
}

# --- wire-id gate (strategy doc step 3.4) -----------------------------------
# The wire carries the KindInfo_Unique numeric id; a kind without an id cannot be
# shown by a stock EN client. The 9 known misses (4 Crystal_State + 5 Koryu_*) are
# reported, not failed - they are the tracked work item.
$unique = @{}
foreach ($line in ([Text.Encoding]::Unicode.GetString([IO.File]::ReadAllBytes((Join-Path $ENRoot "Data\KindInfo_Unique.txt"))) -split "`r?`n")) {
    $c = $line -split "`t"
    if ($c.Count -ge 2 -and $c[1].Trim().StartsWith("KIND_")) { $unique[$c[1].Trim()] = [int]$c[0].Trim() }
}
$missingIds = $null # computed after the Tier-B append below

# --- Tier B: verified exact-code KR-only kinds (strategy doc 3.4) ------------
# A KR-only code that already carries an EN wire id is the same kind under the same
# code - the EN client resolves the id - so its KR row joins the canonical master
# instead of being quarantined. Appended after the EN rows, ordinal order. These are
# the whole of Tier B today; they are dormant (no group spawns them), and the KR item
# table has no row, so the drop table is untouched.
$tierB = @()
$krMap = ColMap $krAttack.Header
$enMap = ColMap $attack.Header
foreach ($code in (@($attack.KrOnly) | Sort-Object)) {
    if (-not $unique.ContainsKey($code)) { continue }
    $krRow = $attack.KrByCode[$code]
    $cells = @("") * $attack.Header.Count
    foreach ($c in $krMap.Keys) {
        if ($enMap.ContainsKey($c) -and $krMap[$c] -lt $krRow.Count) { $cells[$enMap[$c]] = $krRow[$krMap[$c]].Trim() }
    }
    $cells[$enMap["Char"]] = $code
    $attack.Rows += , $cells
    $tierB += $code
}
$missingIds = @(($attack.Rows | ForEach-Object { $_[0].Trim() }) | Where-Object { -not $unique.ContainsKey($_) } | Sort-Object)

# --- roster reconciliation (strategy doc 3.4 / 4) ----------------------------
# One row per kind in either roster or either spawn-group set. The committed table
# is the audit the spawn gate reads. No safe automatic pairing key exists (strategy
# doc 4: the two id spaces are independent and the names diverge even for shared
# kinds), so KR-only kinds are quarantined unless a human confirmed a rename pair -
# those carry the EN code's wire id and name the EN code in `pairs_to`.
$krRef = Get-ReferencedKinds (Join-Path $KRRoot "NPCGroup")
$enRef = Get-ReferencedKinds (Join-Path $ENRoot "NPCGroup")
$krRoster = @{}
foreach ($r in $krAttack.Rows) { $krRoster[$r[0].Trim()] = $true }
$enRoster = @{}
foreach ($r in $enAttack.Rows) { $enRoster[$r[0].Trim()] = $true }
$codes = New-Object System.Collections.Generic.HashSet[string]
foreach ($k in $krRoster.Keys) { [void]$codes.Add($k) }
foreach ($k in $enRoster.Keys) { [void]$codes.Add($k) }
foreach ($k in $krRef) { [void]$codes.Add($k) }
foreach ($k in $enRef) { [void]$codes.Add($k) }
$recon = @()
$order = [string[]]@($codes)
# Ordinal, not culture-aware: equal-under-culture codes (KIND_KELAN / KIND_kelan) would
# otherwise fall back to HashSet order, and .NET Core randomises string hashing per
# process - the file would flip row order and `-Check` would fail at random.
[Array]::Sort($order, [StringComparer]::Ordinal)
$pairTo = @{}
foreach ($k in $renamePairs.Keys) { $pairTo[$k] = $renamePairs[$k] }
foreach ($code in $order) {
    if ($krRoster.ContainsKey($code) -and $enRoster.ContainsKey($code)) { $tier = "shared" }
    elseif ($enRoster.ContainsKey($code)) { $tier = "en_only" }
    elseif ($krRoster.ContainsKey($code)) { $tier = "kr_only" }
    else { $tier = "unrostered" }
    # A confirmed rename pair resolves its KR code to the EN code's wire id; the code
    # itself carries no id, so record the target rather than claiming the id as its own.
    if ($pairTo.ContainsKey($code)) {
        $target = $pairTo[$code]
        if (-not $unique.ContainsKey($target)) { throw "rename pair target $target has no EN wire id" }
        $id = [string]$unique[$target]
        $pairsTo = $target
    }
    elseif ($unique.ContainsKey($code)) { $id = [string]$unique[$code]; $pairsTo = "-" }
    else { $id = "-"; $pairsTo = "-" }
    $spawn = @()
    if ($krRef.Contains($code)) { $spawn += "kr" }
    if ($enRef.Contains($code)) { $spawn += "en" }
    if ($spawn.Count) { $spawnable = $spawn -join "+" } else { $spawnable = "none" }
    $recon += , @($code, $tier, $id, $spawnable, $pairsTo)
}

# --- emit -------------------------------------------------------------------
$prov = "KR baseline (Goonzu_kr_last) values, EN schema + English display names; docs/DESIGN_MONSTER_KR_BASELINE_STRATEGY.md"
Write-CanonTable (Join-Path $OutDir "CharAttackInfoCanonical.txt") "CharAttackInfoCanonical" $attack $prov
Write-CanonTable (Join-Path $OutDir "CharItemInfoCanonical.txt") "CharItemInfoCanonical" $item $prov
Write-ReconTable (Join-Path $OutDir "KindReconciliation.txt") $recon

# --- report -----------------------------------------------------------------
Write-Host ""
Write-Host "attack: $($attack.Rows.Count) rows - $($attack.Upgraded + $attackPairs.Upgraded.Count) KR-upgraded, $($attack.Provisional - $attackPairs.Upgraded.Count) EN-provisional, $($tierB.Count) Tier-B exact-code KR-only appended; $($attack.KrOnly.Count - $tierB.Count - $attackPairs.Upgraded.Count) KR-only quarantined"
if ($tierB.Count) { Write-Host "  Tier B appended: $($tierB -join ', ')" }
if ($attack.Dups.Count) { Write-Host "  KR duplicate codes (last wins): $($attack.Dups -join ', ')" }
if ($attack.CaseOnly.Count) { Write-Host "  case-only pairs: $($attack.CaseOnly -join '; ')" }
Write-Host "drops : $($item.Rows.Count) rows - $($item.Upgraded + $itemPairs.Upgraded.Count) KR-upgraded, $($item.Provisional - $itemPairs.Upgraded.Count) EN-provisional; $($item.KrOnly.Count - $itemPairs.Upgraded.Count - $itemPairs.Appended.Count) KR-only quarantined"
if ($item.Dups.Count) { Write-Host "  KR duplicate codes (last wins): $($item.Dups -join ', ')" }
if ($item.CaseOnly.Count) { Write-Host "  case-only pairs: $($item.CaseOnly -join '; ')" }
Write-Host "Tier C rename pairs: attack $($attackPairs.Upgraded.Count) upgraded / $($attackPairs.Appended.Count) appended; drops $($itemPairs.Upgraded.Count) upgraded / $($itemPairs.Appended.Count) appended"
if ($itemPairs.Appended.Count) { Write-Host "  drops appended: $($itemPairs.Appended -join ', ')" }
Write-Host "wire-id gate: $($attack.Rows.Count - $missingIds.Count)/$($attack.Rows.Count) attack codes have an EN KindInfo_Unique id"
if ($missingIds.Count) { Write-Host "  missing ids: $($missingIds -join ', ')" -ForegroundColor Yellow }
$spawnNoId = @($recon | Where-Object { $_[3] -ne "none" -and $_[2] -eq "-" })
$enNoId = @($recon | Where-Object { ($_[3] -split '\+') -contains "en" -and $_[2] -eq "-" })
Write-Host "reconcile: $($recon.Count) kinds across both rosters + both group sets"
Write-Host "  spawned without an EN wire id: $($spawnNoId.Count) (of which an EN group spawns: $($enNoId.Count))"
if ($enNoId.Count) { Write-Host "  EN-group misses: $((@($enNoId | ForEach-Object { $_[0] })) -join ', ')" -ForegroundColor Yellow }
