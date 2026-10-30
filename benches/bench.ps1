<#
.SYNOPSIS
  Times `ctx` on a large tree (Windows / PowerShell).

.DESCRIPTION
  Without -Path, generates a synthetic tree of -Files small files spread over
  nested directories, then times `ctx` on it with a warm file cache and prints
  the wall-clock time of several runs.

  With -Path, times ctx on a real checkout instead (for example a cloned Linux
  or Chromium subset).

  This is a script rather than a criterion benchmark on purpose: the thing worth
  measuring is a whole-process run over a real filesystem (walk + read + render),
  not an in-process microbenchmark.

.EXAMPLE
  pwsh benches/bench.ps1 -Files 100000 -Runs 5
  pwsh benches/bench.ps1 -Path C:\src\linux -Runs 5
#>
[CmdletBinding()]
param(
    [string]$Path = "",
    [string]$Ctx = "",
    [int]$Files = 100000,
    [int]$Runs = 5
)

$ErrorActionPreference = "Stop"

if (-not $Ctx) {
    $Ctx = Join-Path $PSScriptRoot "..\target\release\ctx.exe"
    if (-not (Test-Path $Ctx)) {
        $Ctx = Join-Path $PSScriptRoot "..\target\release\ctx"
    }
}
if (-not (Test-Path $Ctx)) {
    throw "ctx binary not found at '$Ctx'. Run: cargo build --release"
}
$Ctx = (Resolve-Path $Ctx).Path

$generated = $false
if (-not $Path) {
    $Path = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-bench-" + [Guid]::NewGuid().ToString("N"))
    $generated = $true
    Write-Host "generating $Files files under $Path ..."
    $body = "pub fn item() -> u32 {`n    let value = 41;`n    value + 1`n}`n"
    $encoding = [System.Text.UTF8Encoding]::new($false)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $dirCount = [Math]::Min($Files, 500)
    for ($i = 0; $i -lt $dirCount; $i++) {
        $dir = Join-Path $Path ("src\mod{0:D4}\sub{1:D3}" -f ($i % 500), ($i % 100))
        if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    }
    for ($i = 0; $i -lt $Files; $i++) {
        $dir = Join-Path $Path ("src\mod{0:D4}\sub{1:D3}" -f ($i % 500), ($i % 100))
        [System.IO.File]::WriteAllText((Join-Path $dir "file$i.rs"), $body, $encoding)
    }
    $sw.Stop()
    Write-Host ("generated in {0:N1}s" -f $sw.Elapsed.TotalSeconds)
}

$Path = (Resolve-Path $Path).Path
$fileCount = (Get-ChildItem -Path $Path -Recurse -File -ErrorAction SilentlyContinue).Count
Write-Host "tree: $Path ($fileCount files)"
Write-Host "ctx:  $Ctx"
Write-Host ""

Push-Location $Path
try {
    # Run from the corpus root so path prefixes do not affect output size.
    Write-Host "--- warm-up ---"
    & $Ctx --quiet . > $null
    Write-Host "--- $Runs timed runs ---"

    $times = @()
    for ($r = 1; $r -le $Runs; $r++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        & $Ctx --quiet --output (Join-Path ([System.IO.Path]::GetTempPath()) "ctx-bench-out.txt") .
        $sw.Stop()
        $times += $sw.Elapsed.TotalSeconds
        Write-Host ("run {0}: {1:N3}s" -f $r, $sw.Elapsed.TotalSeconds)
    }

    $sorted = $times | Sort-Object
    $median = $sorted[[int][Math]::Floor($sorted.Count / 2)]
    Write-Host ""
    Write-Host ("files:      {0}" -f $fileCount)
    Write-Host ("runs:       {0}" -f $Runs)
    Write-Host ("min:        {0:N3}s" -f $sorted[0])
    Write-Host ("median:     {0:N3}s" -f $median)
    Write-Host ("max:        {0:N3}s" -f $sorted[-1])
    if ($fileCount -gt 0) {
        Write-Host ("throughput: {0:N0} files/s (median)" -f ($fileCount / $median))
    }
}
finally {
    Pop-Location
}

if ($generated) {
    Write-Host "removing $Path"
    Remove-Item -Recurse -Force $Path
}
