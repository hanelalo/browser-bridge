# AGENTS.md — Browser Bridge

通过 WebSocket 把本地工具与真实 Chrome 连接的桥（不依赖 CDP）。架构：

```
agent / CLI ──MCP(stdio 或 HTTP)──> bridge-mcp / bridge-server:9226
        │ WebSocket (127.0.0.1:9225)
        ▼
   bridge-server (hub 调度) ──WebSocket──> Chrome 扩展 (MV3, 执行指令)
```

## 目录

| 目录 | 说明 |
|------|------|
| `server/` | Rust WebSocket 枢纽（tokio + tokio-tungstenite）。`hub.rs` 是调度核心（并发额度、窗口锁、任务租约）；`mcp_http.rs` 在 9226 提供 Streamable HTTP MCP，内部以普通 client 回连同一 hub |
| `client/` | Rust CLI（clap），发指令并打印结果；连不上 server 会自动拉起 `bridge-server` |
| `bridge-core/` | 共享库：`transport.rs`（连接/自动拉起/重连）、`target.rs`（元素定位）、`recipes/`（各站点配方） |
| `bridge-mcp/` | rmcp MCP server（stdio）。`lib.rs` 集中定义全部 MCP tools；`bridge-server` 复用同一套工具实现 HTTP 传输，两处工具集必须一致 |
| `extension/` | Vue 3 + WXT (Manifest V3)。`background.ts` 执行指令，`sidepanel/` 显示当前窗口任务面板 |
| `docs/protocol.md` | **WebSocket 协议规范（607 行），改动协议/调度前必读** |
| `docs/google-trends-lazy-loading.md` | 新版 Trends 页懒挂载/水合机制的实测分析，改 googletrends 配方前读 |

## 常用命令

```sh
cargo build                                    # Rust 全 workspace
cargo test -p bridge-server hub                # hub 测试（server/src/hub/tests.rs）
cargo clippy --workspace && cargo fmt --all    # lint（Rust edition 2021）
./scripts/build.sh                             # release 二进制 + 扩展产物
./scripts/server.sh start|stop|restart|status|logs   # server 后台常驻（pid 在 .run/，日志 /tmp/bridge-server.log）

cd extension
pnpm install && pnpm dev        # WXT 开发模式，自动开 Chrome 加载扩展
pnpm test                       # node --test tests/*.test.mjs
pnpm build                      # 产物在 extension/dist/chrome-mv3
```

## 架构规则（改代码前注意）

- **配方只依赖通用原语**：`bridge-core/src/recipes/` 每个文件封装一个站点，只能用 `navigate` / `scrape` / `click` / `run_script` 等通用协议指令；扩展与协议保持站点无关。
- **页面脚本用 JS 文件嵌入**：复杂采集逻辑写在配方同目录的 `.js` 文件，Rust 侧 `include_str!` 引入（如 `googletrends.rs` + `googletrends.js`）。改 `.js` 后必须 `cargo build`（不是增量热更新）。
- **协议兼容**：`docs/protocol.md` 明确说明 WebSocket 协议与端口不因 MCP HTTP 而改变。改握手、消息格式或调度语义时同步更新该文档。
- **调度约束**（hub.rs）：全局最多并发 3 个任务；同窗口同时只执行 1 个；排队任务不占全局名额；`task_id` 租约绑定发起连接。`_resolve_target` / `_sync_executions` / `_window_tasks` 是 server 内部方法，不能加进客户端工具集。
- **server 常驻共享**：server 无空闲退出，多个 agent 共用同一进程；改了 server 代码后用 `./scripts/server.sh restart` 才会生效。
- **工具新增三处同步**：`bridge-mcp/src/lib.rs`（MCP tool 定义）、`client/src/main.rs`（CLI 子命令）、必要时 `extension/entrypoints/background.ts`（执行端）。MCP 与 CLI 的站点配方逻辑放在 `bridge-core` 共享。
- **server 与扩展需一起升级**：任务面板排队数据（`_window_tasks`）在 bridge-server 上。

## 环境变量 / 端口

- `BRIDGE_PORT`（默认 9225，WebSocket）、`BRIDGE_HTTP_PORT`（默认 9226，MCP HTTP；设 0 关闭）、`BRIDGE_HTTP_BIND`、`BRIDGE_SERVER_BIN`（client 自动拉起时用的 server 二进制路径）。
- 所有组件都是本地进程；页面数据不出本机。MCP 身份（`mcp-`）的 client 首次使用会自动创建专用浏览器窗口，不带 `tab_id` 的操作落在本窗口，不得抢用户窗口。

## 约定

- 仓库文档（README/MCP.md/protocol.md）与代码注释为中文，commit message 为英文祈使句。
- 扩展用 pnpm（`packageManager` 已固定版本），Rust 侧遵循 `cargo fmt` 默认风格。
- Windows 未验证，主要目标平台是 macOS / Linux。
