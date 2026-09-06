<#
.SYNOPSIS
    Watch what ffmpeg, melt and whisper are doing to the machine, live.

.DESCRIPTION
    Samples every ffmpeg/ffprobe/melt/whisper-cli process once a second,
    walks each one's parent chain to say who spawned it (roughcut, the CLI,
    Shotcut, something else), shows a live table, and appends every sample to
    a CSV so a slowdown can be diagnosed after it has passed rather than only
    while staring at it.

    Needs nothing from Roughcut itself, so it works against any build,
    including one from before the in-app gauge existed. Stop with Ctrl-C; a
    summary of peaks prints on the way out.

.PARAMETER Csv
    Where samples accumulate. Defaults to the temp directory.

.EXAMPLE
    .\tools\watch-children.ps1
    .\tools\watch-children.ps1 -Csv D:\slowdown.csv
#>
[CmdletBinding()]
param(
    [string]$Csv = "$env:TEMP\roughcut-children.csv"
)

$names = 'ffmpeg.exe', 'ffprobe.exe', 'melt.exe', 'whisper-cli.exe'
$filter = ($names | ForEach-Object { "Name='$_'" }) -join ' OR '

# pid -> "who spawned this", cached because parents rarely change.
$owners = @{}

function Resolve-Owner([uint32]$ProcessId, [hashtable]$table) {
    if ($owners.ContainsKey($ProcessId)) { return $owners[$ProcessId] }
    $cursor = $ProcessId
    $owner = 'other'
    for ($hop = 0; $hop -lt 8; $hop++) {
        if (-not $table.ContainsKey($cursor)) { break }
        $row = $table[$cursor]
        $n = $row.Name.ToLower()
        if ($n -eq 'roughcut.exe')     { $owner = 'roughcut';     break }
        if ($n -eq 'roughcut-cli.exe') { $owner = 'roughcut-cli'; break }
        if ($n -eq 'shotcut.exe')      { $owner = 'shotcut';      break }
        if ($n -eq 'melt.exe' -and $cursor -ne $ProcessId) { $owner = 'melt'; break }
        $cursor = [uint32]$row.ParentProcessId
    }
    $owners[$ProcessId] = $owner
    return $owner
}

if (-not (Test-Path $Csv)) {
    'time,pid,name,owner,ws_mb,cmdline' | Set-Content $Csv -Encoding utf8
}

# Peaks survive Ctrl-C via the finally below.
$peakByPid = @{}
$peakTotal = 0
$mostAtOnce = 0

Write-Host "watching $($names -join ', ')  (Ctrl-C to stop)"
Write-Host "samples -> $Csv`n"

try {
    while ($true) {
        $now = Get-Date
        # The whole process table once, so parent chains resolve without a
        # query per hop.
        $all = @{}
        Get-CimInstance Win32_Process | ForEach-Object { $all[[uint32]$_.ProcessId] = $_ }
        $children = @($all.Values | Where-Object { $names -contains $_.Name.ToLower() })

        $rows = foreach ($c in $children) {
            $ws = [int]($c.WorkingSetSize / 1MB)
            $id = [uint32]$c.ProcessId
            if (-not $peakByPid.ContainsKey($id) -or $ws -gt $peakByPid[$id].ws) {
                $cmd = $c.CommandLine
                if ($cmd -and $cmd.Length -gt 100) { $cmd = $cmd.Substring(0, 100) }
                $peakByPid[$id] = @{ name = $c.Name; owner = (Resolve-Owner $id $all); ws = $ws; cmd = $cmd }
            }
            [pscustomobject]@{
                Pid    = $id
                Name   = $c.Name
                Owner  = Resolve-Owner $id $all
                WS_MB  = $ws
            }
        }

        $total = ($rows | Measure-Object WS_MB -Sum).Sum
        if ($null -eq $total) { $total = 0 }
        if ($total -gt $peakTotal) { $peakTotal = $total }
        if ($rows.Count -gt $mostAtOnce) { $mostAtOnce = $rows.Count }

        foreach ($r in $rows) {
            $cmd = $peakByPid[[uint32]$r.Pid].cmd -replace '"', "'" -replace ',', ';'
            Add-Content $Csv ('{0:yyyy-MM-dd HH:mm:ss},{1},{2},{3},{4},"{5}"' -f
                $now, $r.Pid, $r.Name, $r.Owner, $r.WS_MB, $cmd)
        }

        Clear-Host
        Write-Host ("{0:HH:mm:ss}   {1} children, {2:N0} MB together   (peak so far: {3:N0} MB, most at once: {4})" -f
            $now, $rows.Count, $total, $peakTotal, $mostAtOnce)
        if ($rows.Count) {
            $rows | Sort-Object WS_MB -Descending | Format-Table -AutoSize | Out-Host
        } else {
            Write-Host "`n  nothing running"
        }
        Start-Sleep -Seconds 1
    }
}
finally {
    Write-Host "`n=== peaks by process ==="
    $peakByPid.GetEnumerator() | Sort-Object { $_.Value.ws } -Descending |
        Select-Object -First 12 | ForEach-Object {
            "{0,6} MB  {1,-12} {2,-13} {3}" -f $_.Value.ws, $_.Value.name, $_.Value.owner, $_.Value.cmd
        } | Out-Host
    Write-Host "`nfull record: $Csv"
}
