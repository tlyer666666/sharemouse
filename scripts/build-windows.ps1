$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $projectRoot "src-tauri\Cargo.toml"
$outputDirectory = Join-Path $projectRoot "dist\windows"
$binary = Join-Path $projectRoot "src-tauri\target\release\deskbridge.exe"
$destination = Join-Path $outputDirectory "DeskBridge.exe"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "找不到 cargo。请先从 https://rustup.rs 安装 Rust stable。"
}

Write-Host "[1/3] 运行核心测试..." -ForegroundColor Cyan
cargo test --manifest-path $manifest --release --locked

Write-Host "[2/3] 构建 Windows release..." -ForegroundColor Cyan
cargo build --manifest-path $manifest --release --locked

Write-Host "[3/3] 整理输出..." -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
Copy-Item -LiteralPath $binary -Destination $destination -Force

Write-Host "完成：$destination" -ForegroundColor Green
Write-Host "首次运行时，请只允许 Windows 防火墙的专用网络访问。"
