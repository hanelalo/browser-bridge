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

也可以 `pnpm build` 后，在 `chrome://extensions` 打开"开发者模式"，加载 `extension/dist/chrome-mv3` 目录。

### 3. 使用 client

```sh
cd client
cargo run -- list-tabs
cargo run -- new-tab https://example.com                # 新建标签页
cargo run -- activate-tab --tab 7                        # 切换到指定标签页
cargo run -- close-tab                                   # 关闭当前激活标签页
cargo run -- close-tab --tab 7                           # 关闭指定标签页
cargo run -- navigate https://example.com
cargo run -- click '#submit'                         # CSS 选择器
cargo run -- click '登录' --by text                  # 按文本定位
cargo run -- click_at 120 340                        # 按坐标点击
cargo run -- press_key Enter
cargo run -- press_key a --modifier ctrl             # Ctrl+A
cargo run -- press_key Enter --wait-load              # 回车触发导航后等页面加载
cargo run -- scroll --dy 800
cargo run -- set_value '#username' alice
cargo run -- check '#agree'
cargo run -- check '#agree' --uncheck
cargo run -- select_option '#city' --text 北京
cargo run -- get_value '#username'
cargo run -- run-script 'document.title'             # 在页面里执行任意 JS
cargo run -- scrape 'div.card' --fields 'name:.name,price:.price,img:img@src'
cargo run -- get-page-content
```

### googlesearch

Google 搜索专用快捷指令，输出 `{ "tab_id": ..., "results": [...] }`，`tab_id` 是搜索所在标签页（供后续指令链式操作），`results` 每项含 `title` / `description` / `url` / `target`：

```sh
cargo run -- googlesearch 'Haze Seas'
```

`target` 是可直接喂给 `click` 的元素定位（`{ by, value, index }`），方便后续点击某个结果。实现是 client 侧的"站点配方"：用通用原语 `navigate` + `scrape` 编排，选择器作为常量集中在 client 里（`#rso > div` 容器、`data-sncf='1'` 描述等），扩展与协议保持通用。

### 元素定位

统一支持三种方式，均可加 `--index` 指定第几个匹配：

| `--by` | 含义 | 示例 |
|--------|------|------|
| `css`（默认） | CSS 选择器 | `click '#submit'` |
| `text` | 元素自身可见文本（精确优先，退化为包含） | `click '登录' --by text` |
| `xpath` | XPath 表达式 | `click '//button[@id="x"]' --by xpath` |

### scrape 字段映射

`scrape` 按 CSS 选择器提取结构化数据（静态查询，CSP 安全）。字段语法：`字段名:选择器[@属性]`，默认取文本，`@属性` 取属性值（如 `a@href` 对链接返回绝对 URL）：

```sh
cargo run -- scrape 'div.card' --fields 'name:.name,price:.price,img:img@src'
```

兼容旧写法 `--title h3 --link a --desc .VwiC3b`（对应输出 `title`/`url`/`description`），同时传 `--fields` 时以 `fields` 为准。

### run_script

在页面里执行任意 JS 表达式（可返回 Promise），结果 JSON 序列化返回，用于探索页面结构、复杂提取等场景。实现基于 `chrome.userScripts`（USER_SCRIPT 世界豁免页面 CSP）：Chrome 135+ 走 `userScripts.execute`（当前页面即可用）；低版本退回注册式 user script（Chrome 120+，首次使用后需刷新一次目标标签页）。

完整协议见 [docs/protocol.md](docs/protocol.md)。

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
- `run_script` / `scrape` 可以读写页面，属于强能力；不要把它暴露给不可信来源。
