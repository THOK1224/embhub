# embhub acceptance script
#
# Usage (from the repository root, in your own terminal):
#     .\scripts\accept.ps1
# If the execution policy blocks it:
#     Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
#     .\scripts\accept.ps1
#
# What it does: runs the whole acceptance checklist and prints PASS/FAIL per item.
# Exit code 0 means every hard criterion passed.
#
# This file is intentionally ASCII-only: a .ps1 without a BOM is decoded as ANSI by
# Windows PowerShell 5.1, which corrupts non-ASCII text and can break parsing.
# The Chinese explanation of every item lives in docs/ACCEPTANCE.md.
#
# Design note: this script is a convenience. Every check below can be run by hand -
# docs/ACCEPTANCE.md lists the raw command, the expected output and the pass
# criterion for each item, so you never have to trust this script.

if ($PSScriptRoot) { $root = Split-Path -Parent $PSScriptRoot } else { $root = (Get-Location).Path }
$cargo = Join-Path $root 'scripts\cargo.cmd'
Push-Location $root

$script:fails = 0
$script:passes = 0

function Report([string]$id, [string]$desc, [bool]$ok, [string]$detail) {
  if ($ok) {
    Write-Host ("  [PASS] {0}  {1}" -f $id, $desc) -ForegroundColor Green
    $script:passes++
  } else {
    Write-Host ("  [FAIL] {0}  {1}" -f $id, $desc) -ForegroundColor Red
    if ($detail) { Write-Host ("         detail: {0}" -f $detail) -ForegroundColor DarkGray }
    $script:fails++
  }
}

# Run cargo and capture BOTH the text and the real exit code.
# Never truncate the pipeline here: doing that kills the child process early and
# yields a bogus exit code (-1) that looks like a failure.
function Invoke-Cargo([string[]]$cargoArgs) {
  $text = (& $cargo @cargoArgs 2>&1 | Out-String)
  $code = $LASTEXITCODE
  return [pscustomobject]@{ Text = $text; Code = $code }
}

Write-Host ""
Write-Host "=== embhub acceptance ===" -ForegroundColor Cyan
Write-Host ("root: {0}" -f $root)
Write-Host ""

# ---------------------------------------------------------------- B: builds/tests
Write-Host "[B] build, tests, static checks" -ForegroundColor Yellow

$r = Invoke-Cargo @('test','-p','embhub-core')
$m = [regex]::Match($r.Text, '(\d+) passed; (\d+) failed')
$seen = if ($m.Success) { $m.Value } else { 'no "N passed" line' }
Report 'B1' 'cargo test -p embhub-core  (expect 89 passed / 0 failed)' `
       ($r.Code -eq 0 -and $m.Success -and [int]$m.Groups[2].Value -eq 0) `
       ("exit={0}  {1}" -f $r.Code, $seen)

$r = Invoke-Cargo @('test','-p','embhub-cli')
$m = [regex]::Match($r.Text, '(\d+) passed; (\d+) failed')
$seen = if ($m.Success) { $m.Value } else { 'no "N passed" line' }
Report 'B2' 'cargo test -p embhub-cli   (expect 34 passed / 0 failed)' `
       ($r.Code -eq 0 -and $m.Success -and [int]$m.Groups[2].Value -eq 0) `
       ("exit={0}  {1}" -f $r.Code, $seen)

$r = Invoke-Cargo @('clippy','--all-targets','--','-D','warnings')
Report 'B3' 'cargo clippy -D warnings   (expect exit 0, no warning)' ($r.Code -eq 0) ("exit={0}" -f $r.Code)

$r = Invoke-Cargo @('fmt','--all','--check')
Report 'B4' 'cargo fmt --check          (expect exit 0, no diff)' ($r.Code -eq 0) ("exit={0}" -f $r.Code)

$r = Invoke-Cargo @('build','-p','embhub-app')
Report 'B5' 'cargo build -p embhub-app   (GUI must stay buildable)' ($r.Code -eq 0) ("exit={0}" -f $r.Code)

$r = Invoke-Cargo @('test','-p','embhub-app')
$m = [regex]::Match($r.Text, '(\d+) passed; (\d+) failed')
$seen = if ($m.Success) { $m.Value } else { 'no "N passed" line' }
Report 'B6' 'cargo test -p embhub-app    (expect 31 passed / 0 failed)' `
       ($r.Code -eq 0 -and $m.Success -and [int]$m.Groups[2].Value -eq 0) `
       ("exit={0}  {1}" -f $r.Code, $seen)

# B7: the layout solver is pure JS, so its tests run under plain Node (no browser).
$nodeExe = (Get-Command node -ErrorAction SilentlyContinue).Source
if (-not $nodeExe) {
  $cand = Join-Path $env:USERPROFILE '.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\node\bin\node.exe'
  if (Test-Path $cand) { $nodeExe = $cand }
}
if (-not $nodeExe) {
  Report 'B7' 'node ui/layout.test.js      (layout solver tests)' $false 'node not found on PATH nor in the DSH bundled runtime'
} else {
  $out = & $nodeExe (Join-Path $Root 'crates\embhub-app\ui\layout.test.js') 2>&1
  $code = $LASTEXITCODE
  # NOTE: keep this file ASCII-only. PS 5.1 reads a BOM-less .ps1 as ANSI, so one
  # non-ASCII literal breaks string termination and corrupts the WHOLE file's parse.
  # The Chinese summary word is therefore built from char codes, not typed literally.
  $passWord = [string][char]0x901A + [string][char]0x8FC7
  $sum = ($out | Select-String -Pattern ($passWord + ' \d+ ') | Select-Object -First 1).Line
  if (-not $sum) { $sum = ($out | Select-Object -Last 1) }
  Report 'B7' 'node ui/layout.test.js      (layout solver: pure, no browser)' `
         ($code -eq 0) ("exit={0}  {1}" -f $code, $sum)
}

# ---------------------------------------------------------------- A: zero-loss loops
Write-Host ""
Write-Host "[A] zero-loss closed loop (no hardware needed)" -ForegroundColor Yellow

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','selftest','--bytes','67108864')
Report 'A1' 'selftest 64 MiB            (injected == logged == replayed, exit 0)' `
       ($r.Code -eq 0 -and $r.Text -match 'EMBHLOG') ("exit={0}" -f $r.Code)

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','selftest','--bytes','67108864','--repo','.','--bin','docs\SPEC.md')
Report 'A2' 'selftest + fingerprint     (header v2, 8/8 fields read back from disk)' `
       ($r.Code -eq 0 -and $r.Text -match 'EMBHLOG') ("exit={0}" -f $r.Code)

# ---------------------------------------------------------------- C: pipeline/backpressure
Write-Host ""
Write-Host "[C] streaming pipeline + backpressure (SPEC 4.3)" -ForegroundColor Yellow

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','stream','--bytes','67108864','--consume','fast')
Report 'C1' 'stream --consume fast       (display drops == 0, disk complete, exit 0)' `
       ($r.Code -eq 0 -and $r.Text -match 'batches_dropped\s*:\s*0') `
       ("exit={0}  dropped-line: {1}" -f $r.Code, ([regex]::Match($r.Text,'batches_dropped\s*:\s*\d+').Value))

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','stream','--bytes','67108864','--consume','stalled')
Report 'C2' 'stream --consume stalled    (display drops > 0 BUT disk still complete, exit 0)' `
       ($r.Code -eq 0 -and $r.Text -match 'batches_dropped\s*:\s*[1-9]') `
       ("exit={0}  dropped-line: {1}" -f $r.Code, ([regex]::Match($r.Text,'batches_dropped\s*:\s*\d+').Value))

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','stream','--bytes','67108864','--consume','stalled','--display-capacity','0')
$emittedZero = $r.Text -match 'batches_emitted\s*:\s*0'
Report 'C3' 'stream --display-capacity 0 (display 100% dropped, disk STILL complete, exit 0)' `
       ($r.Code -eq 0 -and $emittedZero) `
       ("exit={0}  batches_emitted: {1}" -f $r.Code, ([regex]::Match($r.Text,'batches_emitted\s*:\s*\d+').Value))

# ---------------------------------------------------------------- D: fingerprint vs external
Write-Host ""
Write-Host "[D] firmware fingerprint (cross-checked against an external implementation)" -ForegroundColor Yellow

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','fingerprint','--repo','.','--bin','docs\SPEC.md')
$m = [regex]::Match($r.Text, 'sha256:[0-9a-f]{64}')
$external = 'sha256:' + (Get-FileHash (Join-Path $root 'docs\SPEC.md') -Algorithm SHA256).Hash.ToLower()
Report 'D1' 'fingerprint bin_hash == Windows Get-FileHash SHA256' `
       ($r.Code -eq 0 -and $m.Success -and $m.Value -eq $external) `
       ("embhub={0}  Get-FileHash={1}" -f $m.Value, $external)

$commitOk = $r.Text -match 'commit\s*:\s*[0-9a-f]{40}'
Report 'D2' 'fingerprint commit is a full 40-hex git revision' ($r.Code -eq 0 -and $commitOk) 'no 40-hex commit found'

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','list')
Report 'D3' 'list still enumerates serial ports (exit 0)' ($r.Code -eq 0) ("exit={0}" -f $r.Code)

# ---------------------------------------------------------------- E: no scope/dependency drift
Write-Host ""
Write-Host "[E] scope and dependency drift" -ForegroundColor Yellow

& git diff --quiet -- Cargo.lock 2>$null
Report 'E1' 'Cargo.lock unchanged (zero new downloaded crates)' ($LASTEXITCODE -eq 0) ("git diff exit={0}" -f $LASTEXITCODE)

$tauriInCore = [bool](Select-String -Path (Join-Path $root 'crates\embhub-core\Cargo.toml') -Pattern 'tauri' -Quiet)
Report 'E2' 'embhub-core manifest has no tauri dependency' (-not $tauriInCore) 'tauri found in core manifest'

# ---------------------------------------------------------------- F: send capability
Write-Host ""
Write-Host "[F] send capability (ByteSink / Session) - no hardware needed" -ForegroundColor Yellow

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','loopback','--bytes','1000000')
Report 'F1' 'loopback 1 MiB             (written == read back == logged == replayed, exit 0)' `
       ($r.Code -eq 0) ("exit={0}" -f $r.Code)

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','send','--port','NOPE','--data','hex:AA55')
Report 'F2' 'send to a missing port    (must exit nonzero, must not panic)' `
       ($r.Code -ne 0) ("exit={0} (expected nonzero)" -f $r.Code)

$r = Invoke-Cargo @('run','--quiet','-p','embhub-cli','--','send','--port','NOPE','--data','hex:')
Report 'F3' 'send with an empty payload (must be refused before touching a port)' `
       ($r.Code -ne 0) ("exit={0} (expected nonzero)" -f $r.Code)

# ---------------------------------------------------------------- summary
Write-Host ""
Write-Host ("=== summary: {0} passed, {1} failed ===" -f $script:passes, $script:fails) -ForegroundColor Cyan
Write-Host ""
Write-Host "Items this script CANNOT check (need your eyes / your hardware):" -ForegroundColor Yellow
Write-Host "  G1  cargo run -p embhub-app -> window opens, port list is populated"
Write-Host "  G2  plug/unplug a USB-serial device, press Refresh -> list changes"
Write-Host "  G3  Open session on your board's port at 115200 -> status shows running"
Write-Host "  G4  Receive area: text <-> HEX toggle, Clear, auto-scroll switch all work"
Write-Host "  G5  Send hex AA5501FF -> the board replies PONG fw=2.0.0 ..."
Write-Host "  G6  RX/TX counters match what the board actually sent/received"
Write-Host "  G7  under heavy traffic the 'display dropped' counter is visible (disk is unaffected)"
Write-Host "  G8  serial params: data bits / parity / stop bits / flow control take effect;"
Write-Host "      controls lock while running; illegal combos show the exact core reason"
Write-Host "  G9  receive-area wall-clock timestamp toggle works and matches real time"
Write-Host "  G10 timed send: interval works; stopping really stops; closing the session stops it"
Write-Host ""

Pop-Location
if ($script:fails -gt 0) { exit 1 } else { exit 0 }
