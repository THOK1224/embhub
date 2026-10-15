@echo off
rem embhub build wrapper -- run cargo with the workspace-local toolchain.
rem
rem Usage:  scripts\cargo.cmd build -p embhub-app
rem         .\scripts\cargo.cmd test -p embhub-core
rem
rem Why this exists:
rem   1. The DSH sandbox is workspace-write: writes outside the workspace are denied.
rem      RUSTUP_HOME / CARGO_HOME therefore live INSIDE the workspace, so cargo is not
rem      on PATH and the toolchain is not in the default location.
rem   2. link.exe writes temp files to the system TMP (outside the workspace) and then
rem      fails with LNK1104, so TMP/TEMP must point inside the workspace too.
rem   3. Dot-sourcing scripts\env.ps1 is blocked in sandboxed shells
rem      ("AuthorizationManager check failed"), and `pwsh -File` would not persist the
rem      variables anyway. A .cmd child process is not subject to the PowerShell
rem      execution policy, so this works everywhere.
rem
rem This file is intentionally ASCII-only.

for %%I in ("%~dp0..") do set "WS=%%~fI"
set "RUSTUP_HOME=%WS%\.rustup"
set "CARGO_HOME=%WS%\.cargo"
set "TMP=%WS%\target\tmp"
set "TEMP=%TMP%"
if not exist "%TMP%" mkdir "%TMP%" >nul 2>&1

if not exist "%CARGO_HOME%\bin\cargo.exe" (
  echo [embhub] cargo.exe not found at "%CARGO_HOME%\bin\cargo.exe" 1>&2
  echo [embhub] Rust toolchain is missing; see README.md section "How to run". 1>&2
  exit /b 127
)

"%CARGO_HOME%\bin\cargo.exe" %*
exit /b %ERRORLEVEL%
