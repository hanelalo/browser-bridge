#!/usr/bin/env bash
# 一键构建：Rust release 二进制（server + client）+ 浏览器扩展产物。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> 构建 Rust 二进制（release）"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> 构建浏览器扩展"
(cd "$ROOT/extension" && pnpm build)

echo "==> 完成"
echo "  server:    $ROOT/target/release/bridge-server"
echo "  client:    $ROOT/target/release/bridge-client"
echo "  mcp:       $ROOT/target/release/bridge-mcp"
echo "  extension: $ROOT/extension/dist/chrome-mv3"
