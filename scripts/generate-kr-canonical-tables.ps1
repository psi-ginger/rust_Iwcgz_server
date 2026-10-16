# generate-kr-canonical-tables.ps1 - regenerate the KR-baseline canonical monster tables.
#
# Emits two committed tables from the Korean baseline plus the active English data,
# and `-Check` fails when either has drifted:
#
#   * `crates/gz-data/data/CharAttackInfoCanonical.txt` - combat stats. EN 27-column
#     schema; every column the KR baseline carries takes the KR value, the 7 columns
#     only EN has (SortNUM, Luk, AttackE/R, DefenE/R, Wis) keep the EN value, and the
#     `Name` column keeps the English display name.
#   * `crates/gz-data/data/CharItemInfoCanonical.txt` - capture + drops. EN 6-column
#     schema, same merge rule; the `ItemInfo` drop string comes from KR verbatim.
#
# The decision record is docs/DESIGN_MONSTER_KR_BASELINE_STRATEGY.md (docs repo):
# the Korean baseline `Goonzu_kr_last` is the source of truth; English names and
# the EN wire ids (`Data\KindInfo_Unique.txt`, never modified) layer on top.
#   - shared kind codes are upgraded in place;
#   - EN-only codes keep their EN values (provisional - no KR truth exists);
#   - KR-only codes are quarantined: an EN client cannot resolve a kind id it does
#     not have, so they are reported but not emitted.
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
    }
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

# --- load -------------------------------------------------------------------
$krAttack = Read-KrTable (Join-Path $KRRoot "CharInfo\CharAttackInfo.txt")
$krItem = Read-KrTable (Join-Path $KRRoot "CharInfo\CharItemInfo.txt")
$enAttack = Read-EnTable (Join-Path $ENRoot "CharInfo\CharAttackInfoE.txt")
$enItem = Read-EnTable (Join-Path $ENRoot "CharInfo\chariteminfoE.txt")

$attack = Merge-Table $krAttack $enAttack
$item = Merge-Table $krItem $enItem

# --- wire-id gate (strategy doc step 3.4) -----------------------------------
# The wire carries the KindInfo_Unique numeric id; a kind without an id cannot be
# shown by a stock EN client. The 9 known misses (4 Crystal_State + 5 Koryu_*) are
# reported, not failed - they are the tracked work item.
$unique = @{}
foreach ($line in ([Text.Encoding]::Unicode.GetString([IO.File]::ReadAllBytes((Join-Path $ENRoot "Data\KindInfo_Unique.txt"))) -split "`r?`n")) {
    $c = $line -split "`t"
    if ($c.Count -ge 2 -and $c[1].Trim().StartsWith("KIND_")) { $unique[$c[1].Trim()] = [int]$c[0].Trim() }
}
$missingIds = @(($attack.Rows | ForEach-Object { $_[0].Trim() }) | Where-Object { -not $unique.ContainsKey($_) } | Sort-Object)

# --- emit -------------------------------------------------------------------
$prov = "KR baseline (Goonzu_kr_last) values, EN schema + English display names; docs/DESIGN_MONSTER_KR_BASELINE_STRATEGY.md"
Write-CanonTable (Join-Path $OutDir "CharAttackInfoCanonical.txt") "CharAttackInfoCanonical" $attack $prov
Write-CanonTable (Join-Path $OutDir "CharItemInfoCanonical.txt") "CharItemInfoCanonical" $item $prov

# --- report -----------------------------------------------------------------
Write-Host ""
Write-Host "attack: $($attack.Rows.Count) rows - $($attack.Upgraded) KR-upgraded, $($attack.Provisional) EN-provisional; $($attack.KrOnly.Count) KR-only quarantined"
if ($attack.Dups.Count) { Write-Host "  KR duplicate codes (last wins): $($attack.Dups -join ', ')" }
if ($attack.CaseOnly.Count) { Write-Host "  case-only pairs: $($attack.CaseOnly -join '; ')" }
Write-Host "drops : $($item.Rows.Count) rows - $($item.Upgraded) KR-upgraded, $($item.Provisional) EN-provisional; $($item.KrOnly.Count) KR-only quarantined"
if ($item.Dups.Count) { Write-Host "  KR duplicate codes (last wins): $($item.Dups -join ', ')" }
if ($item.CaseOnly.Count) { Write-Host "  case-only pairs: $($item.CaseOnly -join '; ')" }
Write-Host "wire-id gate: $($attack.Rows.Count - $missingIds.Count)/$($attack.Rows.Count) attack codes have an EN KindInfo_Unique id"
if ($missingIds.Count) { Write-Host "  missing ids: $($missingIds -join ', ')" -ForegroundColor Yellow }
