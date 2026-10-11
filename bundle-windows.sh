#!/usr/bin/env bash
# Cross-compiles the plugin for Windows from Linux and packages VST3 + CLAP
# bundles into target/bundled-windows/. (On Windows itself, use
# `cargo xtask bundle hushwire --release` instead.)
set -euo pipefail
cd "$(dirname "$0")"

TARGET=x86_64-pc-windows-gnullvm
NAME="Hushwire"
OUT=target/bundled-windows

cargo build --release -p hushwire --target "$TARGET"
DLL="target/$TARGET/release/hushwire.dll"

rm -rf "$OUT"
mkdir -p "$OUT/$NAME.vst3/Contents/x86_64-win"
cp "$DLL" "$OUT/$NAME.vst3/Contents/x86_64-win/$NAME.vst3"
cp "$DLL" "$OUT/$NAME.clap"
echo "Created Windows bundles in $OUT:"
ls -1 "$OUT"
