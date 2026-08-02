# Browser Bridge

通过 WebSocket 把本地工具和真实浏览器连接起来的桥，不需要 CDP。

扩展安装在浏览器里，作为"手"；server 是本地的 WebSocket 枢纽；client 是发指令的入口（Rust CLI），也可通过 bridge-mcp 暴露给 Claude / Cursor 等 agent。

## 项目结构

| 目录 | 技术 | 职责 |
|------|------|------|
| `extension/` | Vue 3 + TypeScript（WXT / Manifest V3） | 安装在浏览器里，执行指令 |
| `server/` | Rust（tokio + tokio-tungstenite） | WebSocket 枢纽，路由指令与响应 |
| `client/` | Rust CLI（clap） | 发指令、打印结果 |
| `bridge-core/` | Rust 共享库 | 传输层（连接/自动拉起/重连）、元素定位、站点配方 |
| `bridge-mcp/` | Rust（rmcp，MCP server） | stdio 暴露全部指令为 MCP tools，供 Claude / Codex / Cursor 调用 |

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

client 连接失败时会**自动拉起 bridge-server**（需要已构建的二进制，可用 `BRIDGE_SERVER_BIN` 指定路径），自动拉起的 server 空闲 120 秒自动退出；插件断线后按 500ms→5s 退避自动重连。

```sh
cd client
cargo run -- list-tabs
cargo run -- navigate https://example.com
cargo run -- click '#submit'
cargo run -- set_value '#username' alice
cargo run -- scrape 'div.card' --fields 'name:.name,price:.price,img:img@src'
cargo run -- googlesearch 'Haze Seas'
cargo run -- redditsearch 'rust programming'
cargo run -- googletrends 'ai image' --date 'today 1-m' --geo Worldwide
cargo run -- googletrends-compare 'ai image' 'GPTs' --date 'today 1-m'
```

### 指令速查表

| 指令 | 作用 |
|------|------|
| `list-tabs` | 列出所有标签页 |
| `new-tab [url]` | 新建标签页（可指定 URL） |
| `activate-tab --tab <id>` | 切换标签页并聚焦窗口 |
| `close-tab [--tab <id>]` | 关闭标签页（默认当前激活页） |
| `close-auto-tabs` | 关闭 bridge 自动打开的全部标签页（不碰手动开的） |
| `navigate <url>` | 导航并等待页面加载完成 |
| `click <target> [--new-tab]` | 点击匹配定位的元素（锚点默认当前标签页打开） |
| `click-at <x> <y>` | 按坐标点击 |
| `press-key <key>` | 模拟按键（支持修饰键、`--wait-load`） |
| `scroll --dx --dy` | 滚动窗口或指定容器 |
| `set-value <target> <value>` | 设置 input/textarea/contenteditable 的值 |
| `check <target>` | 勾选/取消 checkbox、radio |
| `select-option <target> --text/--value/--option-index` | 选中下拉项 |
| `clear <target>` | 清空输入类元素 |
| `get-value <target>` | 读取元素当前值 |
| `scrape <item> --fields '...'` | 按选择器提取结构化数据 |
| `run-script '<js>'` | 页面里执行任意 JS，返回 JSON |
| `get-page-content` | 读取页面标题/URL/文本 |
| `googlesearch '<关键词>'` | Google 搜索，输出 `{ tab_id, results }` |
| `redditsearch '<关键词>'` | Reddit 搜索，输出 `{ tab_id, results }` |
| `googletrends '<关键词>' [--date] [--geo]` | Google Trends，输出 `{ trend[], top[], rising[] }` |
| `googletrends-compare <词1> <词2>... [--date] [--geo]` | Google Trends 多词对比，输出 `{ series[] }` |

多数指令支持 `--tab <id>` 指定标签页，默认操作当前激活页。

**标签页管理**：`click` 点击锚点链接默认在当前标签页打开（自动覆盖 `target="_blank"`），需要新开时用 `--new-tab`（由扩展创建标签页，响应会返回新标签页的 `tab_id`，便于链式操作）。`new-tab` 指令和 `click --new-tab` 打开的标签页都会被扩展记录，流程结束后可用 `close-auto-tabs` 一键清理，不会误关你手动打开的标签页。

### close-auto-tabs

清理"自动打开的标签页"，需要**单独执行**（CLI 手动调用，或 MCP 流程在收尾时调用一次），不会误关手动打开的标签页。支持**多 agent 隔离**：

- **MCP（`close_auto_tabs` 工具）**：每个 MCP 进程启动时生成独立身份（`mcp-<pid>-<nanos>`），只清理**本进程创建**的标签页，不会误关其他 agent 正在用的标签页
- **CLI（`close-auto-tabs`）**：作为人工管理入口，清理全部自动标签页（不管是谁创建的）

**会被清理的**：`new-tab` 指令和 `click --new-tab` 创建的标签页（扩展记录在 `chrome.storage.session`，service worker 重启不丢）。例如 `googletrends` 每次查询都会新开一个标签页，跑完后清理效果最明显：

```sh
cargo run -- googletrends 'ai image'
cargo run -- close-auto-tabs   # 关闭刚才 googletrends 开的标签页
```

**不会被清理的**：手动开的标签页（如 Sitemap Monitor）、以及 `navigate` / `googlesearch` / `redditsearch` 复用的当前标签页（这些不新开 tab，属于"工作标签页"，留着是正常的）。

### googlesearch

Google 搜索专用快捷指令，输出 `{ "tab_id": ..., "results": [...] }`，`tab_id` 是搜索所在标签页（供后续指令链式操作），`results` 每项含 `title` / `description` / `url` / `target`：

```sh
cargo run -- googlesearch 'Haze Seas'
```

`target` 是可直接喂给 `click` 的元素定位（`{ by, value, index }`），方便后续点击某个结果。实现是 client 侧的"站点配方"：用通用原语 `navigate` + `scrape` 编排，选择器作为常量集中在 client 里（`#rso > div` 容器、`data-sncf='1'` 描述等），扩展与协议保持通用。

### redditsearch

Reddit 搜索专用快捷指令，返回结构与 `googlesearch` 一致（`{ tab_id, results[] }`，每项 `title` / `description` / `url` / `target`）：

```sh
cargo run -- redditsearch 'rust programming'
```

结果页有两种渲染形态：`search-post-with-content-preview`（带正文预览）与 `search-sdui-post`（只有标题），配方同时收取；描述取自帖子正文预览，`search-sdui-post` 形态没有预览时为 `null`。Reddit 首页的搜索框藏在两层 shadow DOM 里，通用定位指令够不到，但配方直接导航到 `/search/?q=`，不依赖首页交互。

### googletrends

Google Trends 趋势查询，返回 `{ tab_id, trend[], top[], rising[] }`：

```sh
cargo run -- googletrends 'ai image' --date 'today 1-m' --geo Worldwide
```

- `trend`：时间序列 `[{ date, value }]`，`value` 为 0-100 相对热度（从图表 SVG 曲线坐标反解 + y 轴刻度校准）
- `top` / `rising`：热门查询与热度上升的查询（排名、关键词、热度、变化百分比），自动翻完所有分页（每表一般 5 页共 50 条，上限 10 页）
- `--date` 支持 `today 1-m`（默认）/ `today 3-m` / `today 12-m` / `today 5-y` / `all`，`--geo` 默认 `Worldwide`
- 关键词表是懒加载的，需要滚动到底部才渲染，配方会自动滚动内部容器等待表格数据
- 每次查询新开一个标签页（同标签页反复导航时图表偶发不加载，新标签页稳定），这些标签页会被扩展记录，可用 `close-auto-tabs` 清理

### googletrends-compare

多关键词走势对比，返回 `{ tab_id, terms[], date, geo, series[] }`，每个关键词一条趋势序列。**共享 0-100 刻度**（100 = 所有词中的最高峰值），便于直接比较；不返回热门/上升查询表：

```sh
cargo run -- googletrends-compare 'ai image' 'GPTs' --date 'today 1-m' --geo Worldwide
```

`terms` 也可用逗号分隔写成一个参数（`'ai image,GPTs'`）。`--date` / `--geo` 与 `googletrends` 一致。

### client 结构

```text
bridge-core/              # 共享库（CLI 与 MCP 复用）
├── transport.rs          # 连接 / 自动拉起 server / 请求 / 可重连 Bridge
├── target.rs             # 元素定位参数（css / text / xpath）
└── recipes/              # 站点配方
    ├── googlesearch.rs   # Google 搜索（选择器 + 编排）
    ├── redditsearch.rs   # Reddit 搜索（选择器 + 编排）
    └── googletrends.rs   # Google Trends（SVG 反解 + 表格解析 + 多词对比）
client/                   # CLI（薄壳：子命令 + 分发）
bridge-mcp/               # MCP server（stdio，每个指令一个 tool）
```

加新站点搜索只需在 `recipes/` 里加一个文件，并在 `main.rs` 注册子命令，协议与扩展无需改动。

### MCP

`bridge-mcp` 把全部浏览器指令暴露为 MCP tools（stdio 传输），供 Claude / Codex / Cursor 等客户端直接调用。运行：

> 从零构建、加载扩展、配置各客户端（Claude Desktop / Cursor / Claude Code）的完整步骤见 [MCP.md](./MCP.md)。

```sh
./target/release/bridge-mcp        # 或 cargo run -p bridge-mcp
```

- 默认连 `ws://127.0.0.1:9225`，可用 `BRIDGE_SERVER` 覆盖；
- 连接失败会自动拉起 `bridge-server`（空闲 120s 自动退出），断线自动重连；
- 工具列表：`list_tabs` / `close_tab` / `close_auto_tabs` / `new_tab` / `activate_tab` / `navigate` / `click` / `click_at` / `press_key` / `scroll` / `set_value` / `check` / `select_option` / `clear` / `get_value` / `scrape` / `run_script` / `get_page_content` / `googlesearch` / `redditsearch` / `googletrends` / `googletrends_compare`。

#### 配置示例

先构建一次 `./scripts/build.sh`，然后把 MCP 服务指向 `target/release/bridge-mcp`（绝对路径）。

Claude Desktop（`claude_desktop_config.json`）：

```json
{
  "mcpServers": {
    "browser-bridge": {
      "command": "/绝对路径/browser-bridge/target/release/bridge-mcp",
      "args": []
    }
  }
}
```

Cursor（`.cursor/mcp.json`）：

```json
{
  "mcpServers": {
    "browser-bridge": {
      "command": "/绝对路径/browser-bridge/target/release/bridge-mcp",
      "args": []
    }
  }
}
```

前提：浏览器已加载扩展（`chrome://extensions` 加载 `extension/dist/chrome-mv3`）。MCP 首次调用会自动拉起 bridge-server，扩展会在几秒内自动重连，无需手动启动任何进程。

## 构建产物（发布）

开发时用 `cargo run` / `pnpm dev`；正式使用前执行一键构建：

```sh
./scripts/build.sh
```

产物：

| 产物 | 路径 | 用途 |
|------|------|------|
| `bridge-server` | `target/release/bridge-server` | 常驻 WebSocket 枢纽，直接运行 |
| `bridge-client` | `target/release/bridge-client` | 发指令的 CLI，直接运行 |
| `bridge-mcp` | `target/release/bridge-mcp` | MCP server，配给 Claude Desktop / Cursor 等 |
| 扩展 | `extension/dist/chrome-mv3` | `chrome://extensions` 加载已解压目录 |

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
| server 空闲退出 | 0（不退出） | 环境变量 `BRIDGE_IDLE_TIMEOUT`（秒）；客户端自动拉起时默认 120 |
| 插件连接地址 | `ws://127.0.0.1:9225` | 构建时 `WXT_PUBLIC_BRIDGE_URL=ws://... pnpm build` |
| client 服务地址 | `ws://127.0.0.1:9225` | `--server` 或环境变量 `BRIDGE_SERVER` |
| client 自动拉起 | 已构建的 `bridge-server` | `BRIDGE_SERVER_BIN` 指定路径，否则按同目录 / target / PATH 查找 |
| Chrome 版本 | 120+ | `run_script` 需要 `chrome.userScripts`（135+ 体验最佳） |

## 安全说明

- server 只监听 `127.0.0.1`，不要让公网访问。
- 插件声明了 `host_permissions: <all_urls>` 才能操作任意页面——这是个人工具的便利；如果要对外分发，应改成按站点授权。
- 服务端目前接受任意角色的连接；如需更强隔离，可在握手阶段加 token。
- `run_script` / `scrape` 可以读写页面，属于强能力；不要把它暴露给不可信来源。
