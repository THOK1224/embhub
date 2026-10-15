# embhub 开发环境（用法：. .\scripts\env.ps1）
#
# 为什么需要这个脚本（全部是实测结论，见 docs/PLAN.md 5.4 与 LOG.md 第 2 轮）：
#   1. 当前 DSH 沙箱是 workspace-write，工作区外写入被拒。
#      因此 RUSTUP_HOME / CARGO_HOME 都必须落在工作区内。
#   2. link.exe 用系统 TMP（工作区外）建临时文件会 LNK1104，必须把 TMP/TEMP 也指进工作区。
#   3. 首次下载依赖需要 TLS 凭据，沙箱下 schannel 会失败，需提权一次；
#      依赖进了工作区内的 CARGO_HOME 之后，日常 cargo build 不再需要提权。

$ws = Split-Path -Parent $PSScriptRoot

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
