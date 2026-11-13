#!/bin/sh
# 有头容器（sandbox-gui 形态）可行性验证（可复现）：
#   用法: verify_container.sh
# 前置: Apple container 运行中（container system start）、target/debug/bridge-server 已构建
# 步骤: 补丁扩展(指向宿主机 vmnet 地址 192.168.64.1) → 构建镜像 → 起容器
#       → 等扩展回连 → sannysoft 截图对照
set -e
ROOT=/private/tmp/bridge-feasibility
HERE="$(cd "$(dirname "$0")" && pwd)"
HOST_IP=192.168.64.1   # Apple container vmnet 网段中宿主机的地址

mkdir -p "$ROOT/shots"

echo "[*] 隔离 server（幂等）"
if ! nc -z 127.0.0.1 9325 2>/dev/null; then
  (cd "$HERE/../.." && BRIDGE_BIND=0.0.0.0 BRIDGE_PORT=9325 BRIDGE_HTTP_PORT=0 \
    nohup ./target/debug/bridge-server > "$ROOT/server9325.log" 2>&1 &)
  sleep 1
fi

echo "[*] 生成容器用扩展副本（ws://$HOST_IP:9325）"
node "$HERE/seed_profile.mjs" \
  --ext-src "$HERE/../../extension/dist/chrome-mv3" \
  --profile "$ROOT/profiles/container" \
  --ws-url "ws://$HOST_IP:9325" \
  --ext-dst "$HERE/container/chrome-mv3" > /dev/null

echo "[*] 构建镜像（apt 层有缓存时很快）"
container build -t bridge-feasibility "$HERE/container" 2>&1 | tail -2

echo "[*] 重启容器"
container rm -f bridge-feasibility >/dev/null 2>&1 || true
container run -d --name bridge-feasibility --cpus 2 --memory 4g bridge-feasibility > /dev/null

echo "[*] 等待扩展回连（最多 30s）..."
BEFORE=$(grep -c "registered as Extension" "$ROOT/server9325.log" || echo 0)
for i in $(seq 1 30); do
  NOW=$(grep -c "registered as Extension" "$ROOT/server9325.log" || echo 0)
  [ "$NOW" -gt "$BEFORE" ] && break
  sleep 1
done
[ "$NOW" -gt "$BEFORE" ] && echo "✅ 容器内扩展已回连" || { echo "❌ 未回连，查 container logs bridge-feasibility"; exit 2; }

export BRIDGE_SERVER=ws://127.0.0.1:9325
CLIENT="$HERE/../../target/debug/bridge-client"
echo "[*] sannysoft 对照截图"
"$CLIENT" navigate https://bot.sannysoft.com
sleep 3
"$CLIENT" screenshot --out "$ROOT/shots/sannysoft-container.png"
echo "完成。对照 headless 结果：UA 不再暴露 HeadlessChrome 即通过。"
