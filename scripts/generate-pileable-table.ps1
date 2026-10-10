# generate-pileable-table.ps1 - regenerate the gz-data `PILEABLE` table.
#
# `is_pileable` (crates/gz-data/src/lib.rs) backs the item-stacking decisions with
# a sorted table of the uniques the legacy `cltItem::CanPile` returns true for.
# The table is the union, deduplicated and sorted, of the first column of the
# deployed English `ItemType*E.txt` files whose type lacks `ITEMATB_OBJECT`
# (`cltItemManagerCommon::SetItemType`, `cltItem-SetItemData.Cpp`).
#
# This applies that recipe and can verify the committed table, so a game-data
# change cannot silently desync it: a missed regenerate is a silent no-stack
# regression, not a crash (docs/MUST_FIX.md, scrutiny remark 3).
#
#   # print the const PILEABLE block for crates/gz-data/src/lib.rs:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\generate-pileable-table.ps1
#
#   # verify the committed table is current (exit 1 on drift):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\generate-pileable-table.ps1 -Check
#
# Override the inputs with -DataRoot / -RustPath.

param(
    [string]$DataRoot = (Join-Path $PSScriptRoot '..\..\goonzu-server\deploy\GameServer\Item\Data'),
    [string]$RustPath = (Join-Path $PSScriptRoot '..\crates\gz-data\src\lib.rs'),
    [switch]$Check
)

$ErrorActionPreference = "Stop"

# The English item files whose type lacks `ITEMATB_OBJECT`, i.e. the pileable
# ones. The order is not significant: the union is sorted below.
$categories = @(
    "Heal", "Food", "Book", "Mineral", "Animal", "Tree", "Cloth", "Seafood",
    "Etc", "Scroll", "Doll", "Ticket", "Eventitem", "Quest", "Box", "Premium",
    "CompositionMaterial"
)

if (-not (Test-Path $DataRoot)) { throw "item data root not found: '$DataRoot' (set -DataRoot)" }

# First column of every line, as a positive integer. Header and comment lines
# ("ItemTypeHeal", "2007-09-05", "// ...") fail the parse and drop out.
$uniques = New-Object 'System.Collections.Generic.HashSet[int]'
foreach ($category in $categories) {
    $file = Join-Path $DataRoot "ItemType${category}E.txt"
    if (-not (Test-Path $file)) { throw "missing item file: $file" }
    foreach ($line in [System.IO.File]::ReadAllLines($file, [System.Text.Encoding]::Unicode)) {
        $value = 0
        if ([int]::TryParse((($line -split "`t")[0]).Trim(), [ref]$value) -and $value -gt 0 -and $value -le 65535) {
            [void]$uniques.Add($value)
        }
    }
}

$sorted = @($uniques | Sort-Object)

# Lay the body out the way rustfmt does (greedy fill to width 100, trailing
# comma), so the printed block is a byte-for-byte drop-in for the committed one.
$body = New-Object 'System.Collections.Generic.List[string]'
$current = '    '
foreach ($n in $sorted) {
    $piece = "$n,"
    $candidate = if ($current.Trim().Length -eq 0) { $current + $piece } else { "$current $piece" }
    if ($candidate.Length -le 100) {
        $current = $candidate
    } else {
        $body.Add($current)
        $current = "    $piece"
    }
}
$body.Add($current)

if ($Check) {
    if (-not (Test-Path $RustPath)) { throw "Rust source not found: '$RustPath' (set -RustPath)" }
    $text = Get-Content $RustPath -Raw
    $match = [regex]::Match($text, 'const PILEABLE: &\[u16\] = &\[(.*?)\];', [System.Text.RegularExpressions.RegexOptions]::Singleline)
    if (-not $match.Success) { throw "no PILEABLE table found in $RustPath" }
    $committed = @([regex]::Matches($match.Groups[1].Value, '\d+') | ForEach-Object { [int]$_.Value })
    # Both sides are sorted ascending, so a joined comparison is exact and
    # type-stable (Compare-Object would trip on uint16-vs-int boxing).
    if (($sorted -join ',') -ne ($committed -join ',')) {
        throw "PILEABLE is stale: $($sorted.Count) uniques generated vs $($committed.Count) committed. Re-run without -Check and paste the block."
    }
    Write-Host "PILEABLE is current: $($sorted.Count) uniques"
    exit 0
}

@("const PILEABLE: &[u16] = &[") + $body + @("];")
