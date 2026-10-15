# embhub dev environment
#
# Usage (MUST be dot-sourced in the CURRENT shell, so the variables survive):
#     . .\scripts\env.ps1
# If the execution policy blocks it, relax it for this process only (no system change):
#     Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
#     . .\scripts\env.ps1
#
# Do NOT use `pwsh -File .\scripts\env.ps1`: that runs in a child process and the
# variables are lost when it exits.
#
# This file is intentionally ASCII-only: a .ps1 without a BOM is decoded as ANSI by
# Windows PowerShell 5.1, which corrupts non-ASCII comments and can break parsing.
# Rationale and measurements live in docs/PLAN.md 5.4 and LOG.md (rounds 2 and 3).
#
# Why these variables are needed:
#   1. The DSH sandbox is workspace-write, so writes outside the workspace are denied.
#      RUSTUP_HOME / CARGO_HOME must therefore live inside the workspace.
#   2. link.exe writes temp files to the system TMP (outside the workspace) and fails
#      with LNK1104, so TMP/TEMP must point inside the workspace too.
#   3. The first dependency download needs TLS credentials, which fail under the
#      sandbox (schannel SEC_E_NO_CREDENTIALS); that one step needs escalation or a
#      plain terminal. Once the registry cache is populated, builds need no network.

# Works both when dot-sourced ($PSScriptRoot is set) and when piped through iex.
if ($PSScriptRoot) {
    $ws = Split-Path -Parent $PSScriptRoot
} else {
    $ws = (Get-Location).Path
}

$env:RUSTUP_HOME = Join-Path $ws '.rustup'
$env:CARGO_HOME = Join-Path $ws '.cargo'

$tmp = Join-Path $ws 'target\tmp'
if (-not (Test-Path $tmp)) { New-Item -ItemType Directory -Force -Path $tmp | Out-Null }
$env:TMP = $tmp
$env:TEMP = $tmp

$env:PATH = "$env:CARGO_HOME\bin;$env:PATH"

Write-Host "[embhub] RUSTUP_HOME = $env:RUSTUP_HOME"
Write-Host "[embhub] CARGO_HOME  = $env:CARGO_HOME"
Write-Host "[embhub] TMP/TEMP    = $env:TMP"
