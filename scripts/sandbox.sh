#!/usr/bin/env bash
# 有头容器沙箱生命周期管理（单沙箱，profile 持久化在宿主机）
#
#   ./scripts/sandbox.sh start    # 构建镜像(有缓存) + 启动容器 + 打印 noVNC 地址
#   ./scripts/sandbox.sh stop     # 停止并移除容器（profile 在宿主机，登录态不丢）
#   ./scripts/sandbox.sh status   # 容器状态 + 扩展是否连上 hub
#   ./scripts/sandbox.sh logs     # 容器日志
#   ./scripts/sandbox.sh vnc      # 打印 noVNC 地址
#
# 首次使用：start 后打开 noVNC 地址，在沙箱 Chromium 里手动加载扩展
# （/opt/bridge-ext，只读挂载自宿主机 extension/dist/chrome-mv3），并开启
# 扩展详情页的「允许用户脚本」。之后 profile 持久化，无需重复安装。
#
# 注意：hub（bridge-server）当前只有一个扩展槽位。若日常 Chrome 里也启用了
# 同名扩展，两个实例会互相抢占——请只保留沙箱里的那个。
#
# 环境变量：BRIDGE_SANDBOX_HOME（默认 ~/.bridge/sandbox，放 profile 与补丁后的扩展副本）、
#           BRIDGE_SANDBOX_HOST_WS（宿主机在容器网络里的 WS 地址，默认 192.168.64.1）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NAME="bridge-sandbox"
IMAGE="bridge-sandbox"
SANDBOX_HOME="${BRIDGE_SANDBOX_HOME:-$HOME/.bridge/sandbox}"
PROFILE_DIR="$SANDBOX_HOME/profile"
EXT_SRC="$ROOT/extension/dist/chrome-mv3"
EXT_PATCHED="$SANDBOX_HOME/ext"
HOST_WS="${BRIDGE_SANDBOX_HOST_WS:-192.168.64.1}"
VNC_PORT=6080
CLIENT="$ROOT/target/debug/bridge-client"

die() { printf '错误: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

container_ip() {
  # container ls 的列数不固定（是否含 OS/ARCH 列因行而异），按 IP 形状匹配
  container ls 2>/dev/null | awk -v n="$NAME" '$1==n {for (i=2;i<=NF;i++) if ($i ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\//) { sub(/\/[0-9]+$/, "", $i); print $i; exit }}'
}

vnc_url() {
  # noVNC 固定发布到宿主机回环端口，IP 漂移不影响入口地址
  printf 'http://127.0.0.1:%s/vnc.html?path=websockify&autoconnect=true\n' "$VNC_PORT"
}

# hub 当前是否连着扩展（连着的理应是沙箱）
hub_connected() {
  [[ -x "$CLIENT" ]] || return 1
  "$CLIENT" list-windows >/dev/null 2>&1
}

cmd_start() {
  command -v container >/dev/null 2>&1 || die "需要 Apple container（先 container system start）"
  [[ -d "$EXT_SRC" ]] || die "扩展产物不存在: $EXT_SRC (先在 extension/ 里 pnpm build)"

  mkdir -p "$PROFILE_DIR"

  info "[1/4] 准备扩展副本 (WS 地址补丁为 ws://${HOST_WS}:9225, 仓库产物不动)"
  rm -rf "$EXT_PATCHED"
  cp -R "$EXT_SRC" "$EXT_PATCHED"
  LC_ALL=C grep -rlF 'ws://127.0.0.1:9225' "$EXT_PATCHED" | while IFS= read -r f; do
    LC_ALL=C sed -i '' "s|ws://127.0.0.1:9225|ws://$HOST_WS:9225|g" "$f"
  done

  info "[2/4] 构建镜像（首次较慢，之后走缓存）"
  container build -t "$IMAGE" "$ROOT/scripts/sandbox" 2>&1 | tail -2

  info "[3/4] 启动容器 (profile: $PROFILE_DIR)"
  container rm -f "$NAME" >/dev/null 2>&1 || true
  container run -d --name "$NAME" --cpus 2 --memory 4g \
    -v "$PROFILE_DIR":/data/profile \
    -v "$EXT_PATCHED":/opt/bridge-ext:ro \
    -p 127.0.0.1:"$VNC_PORT":6080 \
    "$IMAGE" >/dev/null
  sleep 3

  local url
  url="$(vnc_url)" || { container logs "$NAME" | tail -20; die "容器未正常运行"; }
  info "[3/4] 容器运行中"

  info "[4/4] 状态检查"
  if hub_connected; then
    info "ℹ️  hub 已连有扩展。注意 hub 只有一个槽位，无法区分连接者是谁："
    info "    - 若这是你日常 Chrome 的扩展且尚未在沙箱安装：请先在日常 Chrome 停用同名扩展，"
    info "      再按下面步骤在沙箱安装（否则两者会互相踢下线）。"
    info "    - 若你已完成沙箱安装：一切就绪，MCP/CLI 的操作目标就是沙箱。"
  else
    info "⏳ hub 无扩展连接。首次使用需要手动安装一次（约 1 分钟）："
    info "    1. 用浏览器打开:  $url"
    info "    2. 在沙箱 Chromium 里地址栏输入 chrome://extensions"
    info "    3. 打开右上角「开发者模式」→ 点「加载已解压的扩展程序」→ 选目录 /opt/bridge-ext"
    info "    4. 进入该扩展详情页，开启「允许用户脚本」开关（run_script 指令需要）"
    info ""
    info "  ⚠️  若日常 Chrome 里也启用了 Browser Bridge 扩展，请先停用，否则会互相踢下线。"
    info "  完成后运行 ./scripts/sandbox.sh status 确认连接。"
  fi
}

cmd_stop() {
  container rm -f "$NAME" >/dev/null 2>&1 || true
  info "已停止并移除容器 (profile 在 $PROFILE_DIR, 登录态/扩展安装状态不丢)"
}

cmd_status() {
  local ip
  ip="$(container_ip || true)"
  if [[ -z "$ip" ]]; then
    info "容器: 未运行（启动: ./scripts/sandbox.sh start）"
  else
    info "容器: 运行中 ($ip)"
    info "noVNC: $(vnc_url)"
  fi
  if hub_connected; then
    info "hub:   已连有扩展（槽位唯一，无法区分是沙箱还是日常 Chrome；"
    info "       若尚未完成沙箱安装，请先停用日常 Chrome 里的同名扩展再安装）"
  else
    info "hub:   ⏳ 无扩展连接（首次使用请按 start 输出的步骤经 noVNC 安装扩展）"
  fi
}

cmd_logs() {
  container logs "$NAME" 2>&1 | tail -n "${1:-100}"
}

cmd_vnc() {
  vnc_url || die "容器未运行"
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  logs) cmd_logs "${2:-100}" ;;
  vnc) cmd_vnc ;;
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
