#!/bin/sh
# headless 沙箱可行性验证（可复现）：
#   用法: verify_headless.sh <Chrome二进制路径> [标签]
#   例:   verify_headless.sh "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" branded157
#
# 步骤: 种子 profile(WS 补丁) → 隔离 server(BRIDGE_PORT=9325, 已在跑则复用)
#       → --load-extension headless 启动 → 等扩展注册 → navigate/screenshot 冒烟
# 结论判定看 server9325.log 里的 "registered as Extension"。
set -e
BIN="$1"; TAG="${2:-test}"
[ -x "$BIN" ] || { echo "usage: $0 <chrome-binary> [tag]"; exit 1; }

ROOT=/private/tmp/bridge-feasibility
PROFILE="$ROOT/profiles/headless-$TAG"
mkdir -p "$ROOT/profiles" "$ROOT/shots"

# 隔离 server（幂等：9325 已监听则复用）
if ! nc -z 127.0.0.1 9325 2>/dev/null; then
  echo "[*] 启动隔离 server (9325)"
  (cd "$(dirname "$0")/../.." && BRIDGE_BIND=0.0.0.0 BRIDGE_PORT=9325 BRIDGE_HTTP_PORT=0 \
    nohup ./target/debug/bridge-server > "$ROOT/server9325.log" 2>&1 &)
  sleep 1
fi

echo "[*] 种子 profile: $PROFILE"
node "$(dirname "$0")/seed_profile.mjs" \
  --ext-src "$(dirname "$0")/../../extension/dist/chrome-mv3" \
  --profile "$PROFILE" \
  --ws-url ws://127.0.0.1:9325 \
  --ext-dst "$ROOT/ext-9325"

echo "[*] headless 启动: $BIN"
"$BIN" --headless=new --user-data-dir="$PROFILE" --no-first-run --no-default-browser-check \
  --disable-background-timer-throttling --disable-backgrounding-occluded-windows \
  --disable-renderer-backgrounding --disable-features=IntensiveWakeUpThrottling \
  --load-extension="$ROOT/ext-9325" > "$ROOT/chrome-$TAG.log" 2>&1 &

echo "[*] 等待扩展注册（最多 15s）..."
BEFORE=$(grep -c "registered as Extension" "$ROOT/server9325.log" 2>/dev/null || echo 0)
CONNECTED=0
for i in $(seq 1 15); do
  NOW=$(grep -c "registered as Extension" "$ROOT/server9325.log" 2>/dev/null || echo 0)
  if [ "$NOW" -gt "$BEFORE" ]; then CONNECTED=1; break; fi
  sleep 1
done
if [ "$CONNECTED" = "1" ]; then
  echo "✅ 扩展已连上 hub"
else
  echo "❌ 扩展未连上（--load-extension 被忽略或加载失败），详见 $ROOT/chrome-$TAG.log"
  exit 2
fi

export BRIDGE_SERVER=ws://127.0.0.1:9325
CLIENT="$(dirname "$0")/../../target/debug/bridge-client"
echo "[*] navigate example.com"
"$CLIENT" navigate https://example.com
echo "[*] screenshot"
"$CLIENT" screenshot --out "$ROOT/shots/smoke-$TAG.png" || true
echo "完成。截图: $ROOT/shots/smoke-$TAG.png"
