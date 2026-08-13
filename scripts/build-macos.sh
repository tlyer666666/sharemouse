#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MANIFEST="$PROJECT_ROOT/src-tauri/Cargo.toml"
TARGET_DIR="$PROJECT_ROOT/src-tauri/target/release"
APP_DIR="$TARGET_DIR/bundle/macos/DeskBridge.app"
CONTENTS="$APP_DIR/Contents"

if ! command -v cargo >/dev/null 2>&1; then
  echo "找不到 cargo。请先安装 Rust stable：https://rustup.rs" >&2
  exit 1
fi

if ! xcode-select -p >/dev/null 2>&1; then
  echo "找不到 Xcode Command Line Tools。请先运行：xcode-select --install" >&2
  exit 1
fi

echo "[1/4] 运行核心测试..."
cargo test --manifest-path "$MANIFEST" --release --locked

echo "[2/4] 构建 macOS release..."
cargo build --manifest-path "$MANIFEST" --release --locked

echo "[3/4] 创建 DeskBridge.app..."
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$TARGET_DIR/deskbridge" "$CONTENTS/MacOS/deskbridge"
cp "$PROJECT_ROOT/src-tauri/macos/Info.plist" "$CONTENTS/Info.plist"
chmod 755 "$CONTENTS/MacOS/deskbridge"

echo "[4/4] 添加本地 ad-hoc 签名..."
codesign --force --deep --sign - "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR"

echo "完成：$APP_DIR"
echo "运行：open \"$APP_DIR\""
echo "然后在系统设置的‘输入监控’和‘辅助功能’中允许 DeskBridge。"
