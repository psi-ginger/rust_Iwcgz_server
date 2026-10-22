<#
.SYNOPSIS
    Catalog the Korean data tables still in the GoonZu pipeline, and splice
    English text into one of them without disturbing its structure.

.DESCRIPTION
    Catalog (the default parameter set) walks the data roots and reports one
    row per table: the encoding it is actually in, its line count, the column
    count of its first data row, and how many hangul characters it still
    carries. Nothing is written. Pass -OutFile to also get the whole census
    as CSV; the console only prints the files that still carry Korean, which
    is the list this script exists to produce.

    Splice rewrites one column of -Source with the matching rows of -English,
    joining on -KeyCol. It replaces whole cells only: row count, row order,
    delimiter, encoding and every other column are carried over untouched,
    and it refuses any replacement that would inject a delimiter or a line
    break into a cell -- that is the only way a cell edit can shift a later
    column or split a row.

    Encoding is detected per file rather than assumed:

      NCR\0    the NDataLoader container: 24-byte header, size at +8 (u32
               LE), key at +12, payload at +24, decrypted as
               pt[i] = ((ct[i] - key) & 0xFF) ^ ct[i+1]. The plaintext is
               NOT always CP949 -- Gounzu_kr_last\TextMgr\NPCMgr.txt is
               UTF-16LE -- so it is sniffed after decryption.
      FF FE    UTF-16LE with BOM (the EN pipeline tables).
      no BOM, NUL on every other byte    UTF-16LE.
      strict UTF-8, falling back to CP949 when it does not decode, and
               cross-checked because a CP949 pair can decode as UTF-8 in
               U+0080..U+00FF.

    Splice always writes to -Out and never over a source, so the only copy of
    a table is never the thing being rewritten. An NCR source is written as
    plaintext; re-encrypt the destination with goonzu-server\ncr-tool.ps1 if it
    is a client or deploy file.

.EXAMPLE
    pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/localize-table.ps1 -OutFile .\korean-tables.csv

.EXAMPLE
    pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/localize-table.ps1 `
        -Source 'C:\Data\04_Server_Goonzu\Goonzu_kr_last\Map\gate.txt' `
        -English 'C:\Data\04_Server_Goonzu\New Era Live\Data\GateInfoE.txt' `
        -KeyCol 0 -TextCol 1 -EnglishTextCol 1 -Out .\gate.en.txt
#>
[CmdletBinding(DefaultParameterSetName = 'Catalog')]
param(
    # Catalog: the data roots to walk. Defaults to the pipeline's own trees.
    [Parameter(ParameterSetName = 'Catalog')]
    [string[]]$DataRoot,

    # Catalog: write the full census here as CSV.
    [Parameter(ParameterSetName = 'Catalog')]
    [string]$OutFile,

    # Splice: the Korean table. Never written to.
    [Parameter(Mandatory, ParameterSetName = 'Splice')]
    [string]$Source,

    # Splice: the English table supplying the translation.
    [Parameter(Mandatory, ParameterSetName = 'Splice')]
    [string]$English,

    # Splice: column both tables are joined on (0-based).
    [Parameter(ParameterSetName = 'Splice')]
    [int]$KeyCol = 0,

    # Splice: column of -Source holding the Korean term (0-based).
    [Parameter(Mandatory, ParameterSetName = 'Splice')]
    [int]$TextCol,

    # Splice: column of -English holding the English term. Defaults to -TextCol.
    [Parameter(ParameterSetName = 'Splice')]
    [int]$EnglishTextCol = -1,

    # Splice: where to write the converted table.
    [Parameter(Mandatory, ParameterSetName = 'Splice')]
    [string]$Out
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Hangul syllables, choseong/jungseong and compatibility jamo. Fullwidth forms
# are deliberately excluded: they are punctuation, not a term to translate.
$script:HangulPattern = '[\u1100-\u11FF\u3130-\u318F\uAC00-\uD7A3]'

function Test-HasHangul {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) { return $false }
    return $Text -match $script:HangulPattern
}

# Decrypt an NCR container, or return $null when the file is not one.
function Get-NcrPlaintext {
    param([byte[]]$Bytes)

    if ($Bytes.Length -lt 25) { return $null }
    if (-not ($Bytes[0] -eq 0x4E -and $Bytes[1] -eq 0x43 -and $Bytes[2] -eq 0x52 -and $Bytes[3] -eq 0x00)) {
        return $null
    }

    [uint64]$size = [BitConverter]::ToUInt32($Bytes, 8)
    if ($size -lt 1 -or ($size + 24) -gt [uint64]$Bytes.Length) { return $null }

    $key = $Bytes[12]
    $length = [int]$size
    $plain = New-Object byte[] $length
    for ($i = 0; $i -lt $length - 1; $i++) {
        $cipher = $Bytes[24 + $i]
        $plain[$i] = ((($cipher - $key) -band 0xFF) -bxor $Bytes[24 + $i + 1])
    }
    $plain[$length - 1] = ($Bytes[24 + $length - 1] -bxor $key)
    return ,$plain
}

# Read a table, decode it, and remember how to write it back byte for byte.
function Read-Table {
    param([string]$Path)

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $container = 'plain'
    if ($bytes.Length -ge 4 -and $bytes[0] -eq 0x4E -and $bytes[1] -eq 0x43 -and $bytes[2] -eq 0x52 -and $bytes[3] -eq 0x00) {
        $plain = Get-NcrPlaintext $bytes
        if ($null -eq $plain) { throw "NCR container in '$Path' has an implausible size field" }
        $bytes = $plain
        $container = 'NCR'
    }

    $kind = $null
    $bom = $false
    $text = $null

    if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
        $kind = 'UTF16LE'
        $bom = $true
        $text = [Text.Encoding]::Unicode.GetString($bytes, 2, $bytes.Length - 2)
    }
    elseif ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        $kind = 'UTF8'
        $bom = $true
        $text = [Text.Encoding]::UTF8.GetString($bytes, 3, $bytes.Length - 3)
    }
    else {
        # UTF-16LE without a BOM puts a NUL on every odd byte.
        $sample = [Math]::Min(4096, $bytes.Length)
        $nuls = 0
        for ($i = 1; $i -lt $sample; $i += 2) { if ($bytes[$i] -eq 0) { $nuls++ } }

        if ($sample -ge 32 -and ($nuls * 2) -ge $sample) {
            $kind = 'UTF16LE'
            $text = [Text.Encoding]::Unicode.GetString($bytes)
        }
        else {
            $strict = New-Object Text.UTF8Encoding($false, $true)
            try {
                $text = $strict.GetString($bytes)
                $kind = 'UTF8'
                # A CP949 pair can decode as UTF-8 in U+0080..U+00FF. If strict
                # UTF-8 yields no hangul but CP949 does, the bytes are CP949.
                if (-not (Test-HasHangul $text)) {
                    $cp949 = [Text.Encoding]::GetEncoding(949).GetString($bytes)
                    if (Test-HasHangul $cp949) {
                        $text = $cp949
                        $kind = 'CP949'
                    }
                }
            }
            catch {
                $text = [Text.Encoding]::GetEncoding(949).GetString($bytes)
                $kind = 'CP949'
            }
        }
    }

    [pscustomobject]@{
        Path      = $Path
        Container = $container
        Kind      = $kind
        Bom       = $bom
        Text      = $text
    }
}

function Write-Table {
    param([string]$Path, [string]$Kind, [bool]$Bom, [string]$Text)

    $encoding = switch ($Kind) {
        'UTF16LE' { New-Object Text.UnicodeEncoding($false, $Bom) }
        'CP949'   { [Text.Encoding]::GetEncoding(949) }
        default   { New-Object Text.UTF8Encoding($Bom) }
    }

    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Split-Lines {
    param([string]$Text)
    # The leading comma keeps a one-line table from coming back as a scalar:
    # under Set-StrictMode a String has no .Count.
    return , ($Text -split "`r`n|`n|`r")
}

# The delimiter the table is really written with, decided by counting: the
# survey found tab-padded banners in comma-free tables and vice versa, so the
# first line is not trustworthy on its own.
function Get-Delimiter {
    param([string[]]$Lines)

    $tabs = 0
    $commas = 0
    foreach ($line in ($Lines | Select-Object -First 50)) {
        if ([string]::IsNullOrEmpty($line)) { continue }
        $tabs += [regex]::Matches($line, "`t").Count
        $commas += [regex]::Matches($line, ',').Count
    }
    if ($tabs -eq 0 -and $commas -eq 0) { return $null }
    if ($commas -gt $tabs) { return ',' }
    return "`t"
}

function Get-ColumnCount {
    param([string]$Line, [string]$Delimiter)
    if ([string]::IsNullOrEmpty($Delimiter)) { return 1 }
    return ($Line -split [regex]::Escape($Delimiter)).Count
}

function Get-Label {
    param($Table)
    $label = $Table.Kind
    if ($Table.Kind -eq 'UTF8' -and -not (Test-HasHangul $Table.Text) -and $Table.Text -notmatch '[\u0080-\uFFFF]') {
        $label = 'ASCII'
    }
    if ($Table.Container -ne 'plain') { $label = "$($Table.Container)/$label" }
    if ($Table.Bom -and $Table.Kind -eq 'UTF16LE') { $label = "$label-BOM" }
    return $label
}

function Invoke-Catalog {
    if (-not $DataRoot -or $DataRoot.Count -eq 0) {
        $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
        $DataRoot = @(
            (Join-Path $repoRoot 'New Era Live'),
            (Join-Path $repoRoot 'Goonzu_kr_last'),
            (Join-Path $repoRoot 'GoonZuNewHope'),
            (Join-Path $repoRoot 'Goonzukr'),
            (Join-Path $repoRoot 'New_Era_Live_update'),
            (Join-Path $repoRoot 'goonzu-server\deploy\GameServer')
        )
    }

    $rows = New-Object System.Collections.Generic.List[object]
    $scanned = 0
    foreach ($root in $DataRoot) {
        if (-not (Test-Path -LiteralPath $root)) {
            Write-Warning "skipping missing root: $root"
            continue
        }
        $files = Get-ChildItem -LiteralPath $root -Recurse -File -Filter '*.txt' -ErrorAction SilentlyContinue
        foreach ($file in $files) {
            $scanned++
            try {
                $table = Read-Table $file.FullName
                $lines = Split-Lines $table.Text
                $delimiter = Get-Delimiter $lines
                $firstData = $lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1
                $hangul = [regex]::Matches($table.Text, $script:HangulPattern).Count
                $rows.Add([pscustomobject]@{
                        Path      = $file.FullName
                        Encoding  = Get-Label $table
                        Delimiter = if ($null -eq $delimiter) { 'none' } elseif ($delimiter -eq "`t") { 'TAB' } else { 'COMMA' }
                        Lines     = $lines.Count
                        Columns   = Get-ColumnCount -Line $firstData -Delimiter $delimiter
                        Hangul    = $hangul
                    })
            }
            catch {
                $rows.Add([pscustomobject]@{
                        Path      = $file.FullName
                        Encoding  = "UNREADABLE: $($_.Exception.Message)"
                        Delimiter = '?'
                        Lines     = 0
                        Columns   = 0
                        Hangul    = 0
                    })
            }
        }
    }

    if ($OutFile) {
        $dir = Split-Path -Parent $OutFile
        if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
        $rows | Export-Csv -LiteralPath $OutFile -NoTypeInformation -Encoding UTF8
    }

    $korean = @($rows | Where-Object { $_.Hangul -gt 0 } | Sort-Object -Property Hangul -Descending)
    $encodings = ($rows | Group-Object Encoding | Sort-Object Count -Descending |
        ForEach-Object { "$($_.Name) $($_.Count)" }) -join ', '

    Write-Host "roots        : $($DataRoot.Count)"
    Write-Host "tables read  : $($rows.Count) (scanned $scanned)"
    Write-Host "encodings    : $encodings"
    Write-Host "with hangul  : $($korean.Count) file(s), $((($rows | Measure-Object -Property Hangul -Sum).Sum)) character(s)"
    if ($OutFile) { Write-Host "full census  : $OutFile" }
    Write-Host ''

    if ($korean.Count -eq 0) {
        Write-Host 'No table in the pipeline still carries hangul.'
        return
    }
    $korean |
        Select-Object Encoding, Delimiter, Columns, Lines, Hangul, Path |
        Format-Table -AutoSize |
        Out-String -Width 260 |
        Write-Host
}

function Invoke-Splice {
    if ($EnglishTextCol -lt 0) { $EnglishTextCol = $TextCol }

    # $srcTable/$engTable rather than $source/$english: PowerShell variable
    # names are case-insensitive, so a local named $source would shadow the
    # -Source parameter for every later reference in this function.
    $srcTable = Read-Table $Source
    $engTable = Read-Table $English

    # Split-Lines returns ONE array object (the leading comma is what keeps a
    # one-line table from coming back as a scalar, which has no .Count under
    # strict mode). Assigning first gives foreach the array itself; writing
    # `foreach ($x in Split-Lines ...)` wraps that single object in an outer
    # array, so the loop runs once with $x bound to every row at once.
    $sourceLines = Split-Lines $srcTable.Text
    $sourceDelimiter = Get-Delimiter $sourceLines
    if ($null -eq $sourceDelimiter) {
        throw "'$Source' has no tab or comma delimiter to split on"
    }
    $englishLines = Split-Lines $engTable.Text
    $englishDelimiter = Get-Delimiter $englishLines
    if ($null -eq $englishDelimiter) {
        throw "'$English' has no tab or comma delimiter to split on"
    }

    # Key -> English term. First row wins, so a duplicate key cannot silently
    # translate a later row differently from an earlier one.
    $lookup = @{}
    foreach ($line in $englishLines) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $cells = @($line -split [regex]::Escape($englishDelimiter))
        $widest = [Math]::Max($KeyCol, $EnglishTextCol)
        if ($cells.Count -le $widest) { continue }
        $key = $cells[$KeyCol].Trim()
        if ($key.Length -eq 0 -or $lookup.ContainsKey($key)) { continue }
        $lookup[$key] = $cells[$EnglishTextCol]
    }
    if ($lookup.Count -eq 0) {
        throw "'$English' yielded no joinable rows at key column $KeyCol"
    }

    # The source's own line ending, so the rewrite does not restamp them all.
    $newline = if ($srcTable.Text.Contains("`r`n")) { "`r`n" }
    elseif ($srcTable.Text.Contains("`n")) { "`n" }
    elseif ($srcTable.Text.Contains("`r")) { "`r" }
    else { "`r`n" }

    $outLines = New-Object System.Collections.Generic.List[string]
    $replaced = 0
    $unmatched = 0
    $koreanTerms = 0
    $koreanKeys = New-Object System.Collections.Generic.List[string]
    $widest = [Math]::Max($KeyCol, $TextCol)

    foreach ($line in $sourceLines) {
        if ([string]::IsNullOrWhiteSpace($line)) { $outLines.Add($line); continue }

        $cells = @($line -split [regex]::Escape($sourceDelimiter))
        # Banners and short rows have no key column to join on; leave them be.
        if ($cells.Count -le $widest) { $outLines.Add($line); continue }

        $key = $cells[$KeyCol].Trim()
        if (-not $lookup.ContainsKey($key)) {
            $unmatched++
            $outLines.Add($line)
            continue
        }

        $term = $lookup[$key]
        # The English source is not always English: the NDOORS export still
        # carries Korean for terms it never translated. Copying them verbatim
        # is honest; calling that done would not be, so they are counted.
        if ($term -match $script:HangulPattern) {
            $koreanTerms++
            if ($koreanKeys.Count -lt 5) { $koreanKeys.Add($key) }
        }
        # The only way a cell edit can change the row: a delimiter re-splits it
        # into more columns, and a line break creates a row that was never
        # there. Both shift every column after this one.
        if ($term -match "[`t`r`n]") { throw "key $key at '$Source':$($outLines.Count) has a tab or line break in its English term" }
        if ($sourceDelimiter -eq ',' -and $term.Contains(',')) {
            throw "key $key at '$Source':$($outLines.Count) has a comma in its English term, and this table is comma-delimited"
        }

        $cells[$TextCol] = $term
        if ($cells.Count -ne (@($line -split [regex]::Escape($sourceDelimiter))).Count) {
            throw "key ${key}: column count changed, refusing to write"
        }
        $outLines.Add([string]::Join($sourceDelimiter, $cells))
        $replaced++
    }

    # The structural promise: same rows, same order, same columns. Cells are
    # re-joined from the array they were split into, so only a delimiter or a
    # line break smuggled into a term could break it -- and those are refused
    # above. Say so rather than trust it.
    if ($outLines.Count -ne $sourceLines.Count) {
        throw "row count changed ($($sourceLines.Count) -> $($outLines.Count)); refusing to write"
    }

    Write-Table -Path $Out -Kind $srcTable.Kind -Bom $srcTable.Bom -Text ([string]::Join($newline, $outLines))

    Write-Host "source       : $Source ($(Get-Label $srcTable), delimiter '$sourceDelimiter')"
    Write-Host "english      : $English ($($lookup.Count) joinable key(s))"
    Write-Host "rows         : $($sourceLines.Count) -> $($outLines.Count)"
    Write-Host "replaced     : $replaced"
    Write-Host "no english   : $unmatched (left as they were)"
    if ($koreanTerms -gt 0) {
        Write-Host "still korean : $koreanTerms English term(s) themselves still carry hangul (keys: $($koreanKeys -join ', '))"
        Write-Host "               -- those rows are untranslated in $English, not unmapped here"
    }
    Write-Host "wrote        : $Out ($(Get-Label $srcTable))"
    if ($srcTable.Container -eq 'NCR') {
        Write-Host 'note         : the source was an NCR container; this output is plaintext.'
        Write-Host '               Re-encrypt it with goonzu-server\ncr-tool.ps1 if it ships as a client/deploy file.'
    }
}

if ($PSCmdlet.ParameterSetName -eq 'Splice') { Invoke-Splice } else { Invoke-Catalog }
