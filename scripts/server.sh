#!/usr/bin/env bash
# bridge-server 后台常驻管理：start / stop / restart / status / logs
#
#   ./scripts/server.sh start      # 后台启动（已运行则直接报告）
#   ./scripts/server.sh stop       # 停止（含被 agent 自动拉起的实例）
#   ./scripts/server.sh restart    # 重启
#   ./scripts/server.sh status     # 看进程、端口、HTTP 健康
#   ./scripts/server.sh logs       # tail -f 日志
#
# 环境变量：BRIDGE_PORT（默认 9225）、BRIDGE_HTTP_PORT（默认 9226，0 关闭）、
#           BRIDGE_HTTP_BIND（默认 127.0.0.1）、BRIDGE_SERVER_BIN（指定二进制）、
#           BRIDGE_LOG（默认 /tmp/bridge-server.log）、BRIDGE_RUN_DIR（默认 <repo>/.run，放 pid）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="${BRIDGE_RUN_DIR:-$ROOT/.run}"
PID_FILE="$RUN_DIR/bridge-server.pid"
LOG_FILE="${BRIDGE_LOG:-/tmp/bridge-server.log}"
WS_PORT="${BRIDGE_PORT:-9225}"
HTTP_PORT="${BRIDGE_HTTP_PORT:-9226}"
HTTP_BIND="${BRIDGE_HTTP_BIND:-127.0.0.1}"
START_TIMEOUT="${BRIDGE_START_TIMEOUT:-10}"

# ---------- 工具函数 ----------

die() { printf '错误: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

find_bin() {
  if [[ -n "${BRIDGE_SERVER_BIN:-}" ]]; then
    [[ -x "$BRIDGE_SERVER_BIN" ]] || die "BRIDGE_SERVER_BIN=$BRIDGE_SERVER_BIN 不可执行"
    printf '%s' "$BRIDGE_SERVER_BIN"
    return
  fi
  local candidate
  for candidate in "$ROOT/target/release/bridge-server" "$ROOT/target/debug/bridge-server"; do
    if [[ -x "$candidate" ]]; then
      printf '%s' "$candidate"
      return
    fi
  done
  die "找不到 bridge-server 二进制，先执行 ./scripts/build.sh"
}

# 端口上的监听进程（优先 lsof，缺失时只做占用探测）
listener_pids() {
  local port="$1"
  command -v lsof >/dev/null 2>&1 || return 0
  lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | sort -u || true
}

port_busy() {
  local port="$1"
  local pids
  pids="$(listener_pids "$port")"
  if [[ -n "$pids" ]]; then
    return 0
  fi
  if command -v lsof >/dev/null 2>&1; then
    return 1
  fi
  # 没有 lsof 时的兜底：能连上就认为被占用
  (exec 3<>"/dev/tcp/127.0.0.1/$port") >/dev/null 2>&1 || return 1
  exec 3>&- 2>/dev/null || true
  return 0
}

http_healthy() {
  command -v curl >/dev/null 2>&1 || return 0
  curl -fsS --max-time 2 "http://$HTTP_BIND:$HTTP_PORT/health" >/dev/null 2>&1
}

read_pid() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid
  pid="$(tr -dc '0-9' <"$PID_FILE")"
  [[ -n "$pid" ]] || return 1
  printf '%s' "$pid"
}

# pid 存在且确实是 bridge-server（防止 pid 复用误杀）
alive() {
  local pid="$1" cmd
  kill -0 "$pid" 2>/dev/null || return 1
  cmd="$(ps -o command= -p "$pid" 2>/dev/null || true)"
  [[ "$cmd" == *bridge-server* ]]
}

managed_pid() {
  local pid
  pid="$(read_pid)" || return 1
  alive "$pid" || return 1
  printf '%s' "$pid"
}

endpoints() {
  info "  WS:    ws://127.0.0.1:$WS_PORT"
  if [[ "$HTTP_PORT" != "0" ]]; then
    info "  MCP:   http://$HTTP_BIND:$HTTP_PORT/mcp"
    info "  health http://$HTTP_BIND:$HTTP_PORT/health"
  else
    info "  MCP:   已关闭（BRIDGE_HTTP_PORT=0）"
  fi
  info "  log:   $LOG_FILE"
}

wait_ready() {
  local pid="$1" waited=0
  while (( waited < START_TIMEOUT * 10 )); do
    if ! alive "$pid"; then
      return 1
    fi
    if command -v lsof >/dev/null 2>&1; then
      if [[ -n "$(listener_pids "$WS_PORT")" ]]; then
        return 0
      fi
    else
      if port_busy "$WS_PORT"; then
        return 0
      fi
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
  return 1
}

# ---------- 动作 ----------

cmd_start() {
  local pid
  if pid="$(managed_pid)"; then
    info "bridge-server 已在运行（pid ${pid}，由本脚本管理）"
    endpoints
    return 0
  fi
  [[ -f "$PID_FILE" ]] && rm -f "$PID_FILE"

  local orphan
  orphan="$(listener_pids "$WS_PORT")"
  orphan="${orphan%%$'\n'*}"
  if [[ -n "$orphan" ]]; then
    die "端口 $WS_PORT 已被 pid $orphan 占用（可能是 agent 自动拉起的实例）；先执行 ./scripts/server.sh stop"
  fi

  local bin
  bin="$(find_bin)"
  mkdir -p "$RUN_DIR"
  mkdir -p "$(dirname "$LOG_FILE")"
  info "启动 $bin"
  BRIDGE_PORT="$WS_PORT" BRIDGE_HTTP_PORT="$HTTP_PORT" BRIDGE_HTTP_BIND="$HTTP_BIND" \
    nohup "$bin" >>"$LOG_FILE" 2>&1 &
  pid=$!
  printf '%s\n' "$pid" >"$PID_FILE"

  if ! wait_ready "$pid"; then
    printf '启动失败，最近日志：\n' >&2
    tail -n 20 "$LOG_FILE" >&2 || true
    rm -f "$PID_FILE"
    exit 1
  fi
  info "bridge-server 已启动（pid ${pid}）"
  endpoints
  if [[ "$HTTP_PORT" != "0" ]] && ! http_healthy; then
    info "  提示: HTTP 健康检查未通过，稍后再试或看日志"
  fi
}

stop_pid() {
  local pid="$1" waited=0
  kill -TERM "$pid" 2>/dev/null || return 0
  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= 100 )); then
      info "等待超时，强制结束 pid $pid"
      kill -KILL "$pid" 2>/dev/null || true
      break
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
}

cmd_stop() {
  local pid stopped=0
  if pid="$(managed_pid)"; then
    info "停止 bridge-server（pid ${pid}）"
    stop_pid "$pid"
    stopped=1
  fi
  rm -f "$PID_FILE"

  # 兜底：端口上还有监听者（agent 自动拉起的、或 pidfile 丢了的实例）
  local port_pids
  port_pids="$(listener_pids "$WS_PORT")"
  if [[ -n "$port_pids" ]]; then
    local p pcmd
    for p in $port_pids; do
      pcmd="$(ps -o command= -p "$p" 2>/dev/null || true)"
      if [[ "$pcmd" == *bridge-server* ]]; then
        info "停止未托管的 bridge-server（pid ${p}，端口 ${WS_PORT}）"
        stop_pid "$p"
        stopped=1
      fi
    done
  fi

  if (( stopped == 0 )); then
    info "bridge-server 未在运行"
  else
    info "已停止"
  fi
}

cmd_status() {
  local pid
  if pid="$(managed_pid)"; then
    local etime rss
    etime="$(ps -o etime= -p "$pid" 2>/dev/null | tr -d ' ')"
    rss="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')"
    info "状态: 运行中（由本脚本管理）"
    info "  pid:   $pid"
    info "  运行:  ${etime:-?}   内存: $(( ${rss:-0} / 1024 )) MB"
    endpoints
    if [[ "$HTTP_PORT" != "0" ]]; then
      if http_healthy; then
        info "  health: OK"
      else
        info "  health: 无响应"
      fi
    fi
    return 0
  fi

  local port_pids
  port_pids="$(listener_pids "$WS_PORT")"
  if [[ -n "$port_pids" ]]; then
    info "状态: 运行中（未托管，可能是 agent 自动拉起的实例）"
    local p pcmd
    for p in $port_pids; do
      pcmd="$(ps -o command= -p "$p" 2>/dev/null || true)"
      info "  pid:   $p  $pcmd"
    done
    endpoints
    return 0
  fi

  info "状态: 未运行"
  info "  启动: ./scripts/server.sh start"
  return 3
}

cmd_logs() {
  [[ -f "$LOG_FILE" ]] || die "还没有日志：${LOG_FILE}"
  # 只有交互终端才 tail -f；在脚本/管道里只打印尾部，避免阻塞调用方
  if [[ -t 1 ]]; then
    tail -n "${1:-50}" -f "$LOG_FILE"
  else
    tail -n "${1:-50}" "$LOG_FILE"
  fi
}

usage() {
  cat <<'USAGE'
用法: ./scripts/server.sh <start|stop|restart|status|logs [行数]>

  start     后台启动 bridge-server（已在运行则直接报告，不重复启动）
  stop      停止（pidfile 管理的实例 + 端口上未托管的 bridge-server）
  restart   stop + start
  status    进程 / 端口 / HTTP 健康（未运行时退出码 3）
  logs [n]  跟随日志（默认先看最后 50 行）

环境变量: BRIDGE_PORT(9225) BRIDGE_HTTP_PORT(9226,0=关闭)
          BRIDGE_HTTP_BIND(127.0.0.1) BRIDGE_SERVER_BIN BRIDGE_LOG(/tmp/bridge-server.log)
          BRIDGE_RUN_DIR(<repo>/.run, 放 pid) BRIDGE_START_TIMEOUT(10)
USAGE
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  logs) cmd_logs "${2:-50}" ;;
  *) usage; exit 1 ;;
esac
