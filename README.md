# Browser Bridge

通过 WebSocket 把本地工具和真实浏览器连接起来的桥，不需要 CDP。

扩展安装在浏览器里，作为"手"；server 是本地的 WebSocket 枢纽；client 是发指令的入口（当前为 Rust CLI，后续可加 TS / Python client）。

## 三个子项目

| 目录 | 技术 | 职责 |
|------|------|------|
| `extension/` | Vue 3 + TypeScript（WXT / Manifest V3） | 安装在浏览器里，执行指令 |
| `server/` | Rust（tokio + tokio-tungstenite） | WebSocket 枢纽，路由指令与响应 |
| `client/` | Rust CLI（clap） | 发指令、打印结果 |

## 快速开始

### 1. 启动 server

```sh
cd server
cargo run            # 默认监听 ws://127.0.0.1:9225
# 换端口：BRIDGE_PORT=9226 cargo run
```

### 2. 加载插件

```sh
cd extension
pnpm install
pnpm dev             # 会自动打开 Chrome 并加载开发版插件
```

也可以 `pnpm build` 后，在 `chrome://extensions` 打开"开发者模式"，加载 `extension/.output/chrome-mv3` 目录。

### 3. 使用 client

```sh
cd client
cargo run -- list-tabs
cargo run -- navigate https://example.com
cargo run -- click '#submit'
cargo run -- get-page-content
```

## 协议

见 [docs/protocol.md](docs/protocol.md)。client 只要按协议走 WebSocket，语言不限（后续 TS / Python client 直接实现同一协议即可）。

## 配置

| 项 | 默认 | 说明 |
|----|------|------|
| server 端口 | 9225 | 环境变量 `BRIDGE_PORT` |
| 插件连接地址 | `ws://127.0.0.1:9225` | 构建时 `WXT_PUBLIC_BRIDGE_URL=ws://... pnpm build` |
| client 服务地址 | `ws://127.0.0.1:9225` | `--server` 或环境变量 `BRIDGE_SERVER` |

## 安全说明

- server 只监听 `127.0.0.1`，不要让公网访问。
- 插件声明了 `host_permissions: <all_urls>` 才能操作任意页面——这是个人工具的便利；如果要对外分发，应改成按站点授权。
- 服务端目前接受任意角色的连接；如需更强隔离，可在握手阶段加 token。
