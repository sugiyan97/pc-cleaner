#!/usr/bin/env bash
# macOS/Linux から Windows 向け実行ファイルをローカルでクロスビルドするスクリプト。
#
# 公式リリース（.github/workflows/release.yml）は windows-latest 上で
# x86_64-pc-windows-msvc 向けにビルドしており、これはそれとは別物（gnu ABI）。
# ローカルでの動作確認用のビルドとして使うことを想定している。
#
# 事前準備（初回のみ）:
#   brew install mingw-w64
#   rustup target add x86_64-pc-windows-gnu
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

TARGET="x86_64-pc-windows-gnu"

if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  echo "error: x86_64-w64-mingw32-gcc が見つかりません。'brew install mingw-w64' を実行してください。" >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "error: rustup target '$TARGET' が未インストールです。'rustup target add $TARGET' を実行してください。" >&2
  exit 1
fi

cargo build --release --target "$TARGET" -p pc-cleaner-cli -p pc-cleaner-gui

VERSION=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
OUT_DIR="dist/windows-local"
mkdir -p "$OUT_DIR"

cp "target/$TARGET/release/pc-cleaner.exe" "$OUT_DIR/pc-cleaner-v${VERSION}-windows-x86_64-gnu.exe"
cp "target/$TARGET/release/pc-cleaner-gui.exe" "$OUT_DIR/pc-cleaner-gui-v${VERSION}-windows-x86_64-gnu.exe"

echo "built:"
ls -la "$OUT_DIR"
