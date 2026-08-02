# Browser Bridge MCP 使用指南

把本地浏览器变成 MCP 工具集：Claude Desktop / Cursor / Claude Code 等 agent 通过 stdio 调 `bridge-mcp`，即可操控真实 Chrome（搜索、点击、填表、读取页面、Google Trends 等），全程不需要 CDP。

## 架构

```text
Claude / Cursor / Codex 等 agent
        │  MCP stdio
        ▼
   bridge-mcp（Rust）          ← 本仓库新增的 MCP server
        │  WebSocket
        ▼
   bridge-server（Rust 枢纽，127.0.0.1:9225，空闲 120s 自动退出）
        │  WebSocket
        ▼
   Chrome 扩展（Browser Bridge，安装在浏览器里执行指令）
```

所有组件都是本地进程：扩展在浏览器里，其余跑在你自己机器上，浏览器页面数据不出本机。

## 从零开始

### 1. 前置要求

- macOS / Linux（Windows 未验证）
- Rust 工具链（`cargo`，用于构建 server / client / mcp）
- Node.js + pnpm（用于构建扩展）
- Chrome 或 Chromium（120+；`run_script` 建议 135+，138+ 需手动开开关，见下文）

### 2. 构建

在仓库根目录执行一键构建：

```sh
./scripts/build.sh
```

等价于分步执行：

```sh
# Rust release 二进制（bridge-server / bridge-client / bridge-mcp）
cargo build --release
# 浏览器扩展（产物在 extension/dist/chrome-mv3）
cd extension && pnpm install && pnpm build
```

产物：

| 产物 | 路径 |
|------|------|
| MCP server | `target/release/bridge-mcp` |
| WebSocket 枢纽 | `target/release/bridge-server` |
| 命令行入口 | `target/release/bridge-client` |
| 浏览器扩展 | `extension/dist/chrome-mv3` |

### 3. 加载扩展

1. 打开 `chrome://extensions`
2. 右上角打开「开发者模式」
3. 点「加载已解压的扩展程序」，选择 `extension/dist/chrome-mv3` 目录
4. **重要**：点扩展卡片上的「详细信息」，打开「**允许用户脚本**」开关（Chrome 138+ 默认关闭；不开的话 `run_script` 指令会报 `chrome.userScripts 不可用`）

扩展会尝试连接 `ws://127.0.0.1:9225`，连不上就按 500ms→5s 退避自动重连——server 还没起也没关系。

### 4. 验证链路（可选）

先确认 server 与扩展连通：

```sh
./target/release/bridge-client list-tabs
```

该命令会自动拉起 `bridge-server`（若未运行），返回浏览器标签页列表即表示扩展已连上。

### 5. 配置 MCP 客户端

把 MCP 服务指向 `bridge-mcp` 的**绝对路径**。`bridge-mcp` 首次收到工具调用时会自动拉起 server，无需手动启动任何进程。

**Claude Desktop**（`claude_desktop_config.json`）：

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

**Cursor**（项目下 `.cursor/mcp.json`）：

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

**Claude Code**：

```sh
claude mcp add browser-bridge -- /绝对路径/browser-bridge/target/release/bridge-mcp
```

配置好后在客户端里应该能看到 20+ 个工具（`list_tabs`、`navigate`、`click`、`scrape`、`googlesearch`、`redditsearch`、`youtubesearch`、`youtubeinfo`、`googletrends`、`googletrends_compare`、`close_auto_tabs` 等）。

## 可用工具

工具分两类：**协议原语**（通用指令，任何网站都能用）与**站点配方**（搜索/查询专用快捷指令，返回结构化 JSON）。

所有工具都通过 MCP 标准 JSON 传参，客户端按工具描述里的 schema 即可知道每个参数的类型、是否必填、默认值和可选值。下文中「必填」参数缺一不可；「可选」参数省略时使用默认值。

### 公共参数约定

| 参数 | 说明 |
|------|------|
| `tab_id` | 可选。指定操作哪个标签页；省略时操作当前激活页。配方返回的 `tab_id` 可直接用于后续链式操作（如把结果 `target` 喂给 `click`） |
| `target` / `by` / `index` | 元素定位三元组：`by` 取值 `css`（默认）/ `text` / `xpath`，`target` 是对应定位值，`index` 从 0 开始选第几个匹配 |
| `timeout` | 可选。等待元素出现或页面加载的最长毫秒数（如 `click` 默认 5000） |

### 站点配方

#### googlesearch

Google 搜索。复用当前标签页导航到搜索结果页，**不新建标签页**。

参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| `query` | string | ✔ | — | 搜索关键词 |
| `tab_id` | int | — | 当前激活页 | 目标标签页 |

返回：`{ "tab_id": int, "results": [ { "title", "description", "url", "target" } ] }`。每条的 `target` 可直接喂给 `click`。

示例：

```json
{ "query": "Haze Seas" }
```

#### redditsearch

Reddit 搜索。直接导航到 Reddit 的 `/search/?q=` 结果页，不依赖首页交互，复用当前标签页、**不新建标签页**。参数与 `googlesearch` 一致（`query` 必填，`tab_id` 可选）。

返回：`{ "tab_id": int, "results": [ { "title", "description", "published", "published_at", "votes", "comments", "url", "target" } ] }`。`published` 为相对时间文本（如 `1mo ago`），`published_at` 为 ISO 时间戳；`votes` / `comments` 为整数（upvote / 评论数，取页面里的原始数值而非格式化文本）。结果页有两种渲染形态（带正文预览 / 仅标题），配方同时收取；无预览时 `description` 为 `null`。

#### youtubesearch

YouTube 搜索，支持上传日期与优先顺序筛选。直接解析搜索结果页 HTML 内嵌的 `ytInitialData`（首屏约 20 条），不足 `max` 条时用页面里的 InnerTube API key/context 通过 continuation 续取（与 yt-dlp 同源）。**不依赖页面渲染与窗口可见性**：标签页在后台或被全屏应用遮挡时也照常拿满，不弹窗、不抢焦点、不用手动切过去。`duration` 取自接口的 `lengthText`，不会缺失。

参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| `query` | string | ✔ | — | 搜索关键词 |
| `time` | string | — | `any` | 上传日期筛选：`any` / `today` / `week` / `month` / `year` |
| `sort` | string | — | `relevance` | 优先顺序：`relevance`（相关程度）/ `popularity`（热门程度） |
| `max` | int | — | `5` | 最多返回条数（至少 1；实测 `max=40` 约 3 秒返回） |
| `tab_id` | int | — | 当前激活页 | 目标标签页 |

返回：`{ "tab_id": int, "results": [ { "title", "channel", "views", "published", "duration", "url", "target" } ] }`。`published` / `duration` 为页面原始文本（如 `1 week ago` / `12:34`），按 URL 去重后截取前 `max` 条。

示例：

```json
{ "query": "rust programming", "time": "week", "sort": "popularity", "max": 10 }
```

注意：

- `time` / `sort` 传入不支持的值会**直接报错**（不静默忽略），错误信息会列出合法取值。
- 若页面数据缺失（如遇到验证墙 / consent 页）会返回明确错误提示。

#### youtubeinfo

获取指定 YouTube 视频的详情（字幕全文、URL、作者、时长、点赞/评论/订阅数）。直接解析视频页 HTML 内嵌的 `ytInitialPlayerResponse` / `ytInitialData`，评论数用 InnerTube `next` continuation 接口获取（不依赖滚动评论区）。**不依赖页面渲染与窗口可见性**，标签页在后台也能取到。

参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| `url` | string | ✔ | — | 视频 URL 或 11 位视频 ID（`watch?v=` / `youtu.be` / `shorts` / `embed` / `live` 均可） |
| `tab_id` | int | — | 当前激活页 | 目标标签页 |

返回：`{ "tab_id": int, "video": { "url", "title", "author", "author_url", "duration", "duration_seconds", "like_count", "like_count_text", "comment_count", "comment_count_text", "subscriber_count", "subscriber_count_text", "captions": [...] } }`。各 `*_count` 为解析后的整数（`万`/`亿`/`K`/`M` 缩写会换算），`*_text` 为页面原始文本；`captions[]` 每项含 `language_code` / `name` / `kind` / `text`（字幕全文）/ `error`（单轨道失败原因，成功为 `null`）。

字幕说明：优先用页面内嵌的 `captionTracks`（timedtext json3）；若返回空（YouTube 对 `exp=xpe` 的轨道要求 PO token，页面内无法生成），按 yt-dlp 的做法改用 **android_vr 客户端**调 player API 取无 pot 要求的轨道。视频无字幕时 `captions` 为空数组，不报错。

示例：

```json
{ "url": "https://www.youtube.com/watch?v=rQ_J9WH6CGk" }
```

#### googletrends

单关键词 Google Trends 查询，自动翻页拿完整趋势序列 + 热门/上升查询。**每次查询会新建一个标签页**，流程收尾时请调用 `close_auto_tabs` 清理。

参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| `query` | string | ✔ | — | 关键词 |
| `date` | string | — | `today 1-m` | 时间范围：`today 1-m` / `today 3-m` / `today 12-m` / `today 5-y` / `all` |
| `geo` | string | — | `Worldwide` | 地区代码（如 `US`、`CN`），不区分大小写 |

返回：`{ "tab_id": int, "trend": [...], "top": [...], "rising": [...] }`。

#### googletrends_compare

多关键词走势对比，共享 0-100 刻度。同样会新建标签页，收尾时用 `close_auto_tabs` 清理。

参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| `terms` | string[] | ✔ | — | 要对比的关键词列表（2 个及以上效果最好） |
| `date` | string | — | `today 1-m` | 同 `googletrends` |
| `geo` | string | — | `Worldwide` | 同 `googletrends` |

返回：`{ "tab_id": int, "series": [...] }`。

### 协议原语

每个工具的参数同样通过 MCP schema 暴露，速查如下（`tab_id`、`target`/`by`/`index` 见上方公共约定，不重复列出）：

| 工具 | 用途 | 主要参数 |
|------|------|----------|
| `list_tabs` | 列出所有标签页 | 无 |
| `new_tab` | 新建标签页 | `url` |
| `activate_tab` | 切换到指定标签页并聚焦窗口 | `tab_id` |
| `close_tab` | 关闭标签页（默认当前激活页） | `tab_id` |
| `close_auto_tabs` | 关闭本会话（当前 MCP 进程）自动打开的标签页（`new_tab` / `click --new-tab` / `googletrends` 创建的），不影响其他会话 | 无 |
| `navigate` | 导航到指定 URL 并等待加载完成 | `url`（必填）、`tab_id` |
| `get_page_content` | 读取页面标题 / URL / 文本 | `tab_id` |
| `scrape` | 按 CSS 选择器提取结构化数据 | `item`（必填，结果容器选择器）、`fields`（字段映射：`字段名: "选择器[@属性]"`）、`title` / `link` / `desc`、`timeout`、`tab_id` |
| `run_script` | 在页面执行任意 JS 表达式，返回 JSON 序列化结果 | `code`（必填）、`tab_id` |
| `click` | 点击匹配定位的元素 | `target`（必填）、`by`、`index`、`timeout`（默认 5000ms）、`new_tab`（锚点在新标签页打开，默认 false）、`tab_id` |
| `click_at` | 按页面坐标点击 | `x`、`y`（必填）、`tab_id` |
| `press_key` | 模拟按键（Enter / Escape / a / F5 等，支持修饰键） | `key`（必填，KeyboardEvent.key 值）、`modifiers`、`target`/`by`/`index`、`wait_load`（按键后等待加载）、`tab_id` |
| `scroll` | 滚动窗口或指定容器 | `dx`、`dy`、`target`/`by`/`index`、`smooth`、`tab_id` |
| `set_value` | 设置 input / textarea / contenteditable 的值 | `target`（必填）、`value`（必填）、`by`、`index`、`tab_id` |
| `check` | 勾选 / 取消 checkbox 或 radio | `target`（必填）、`checked`（默认 true）、`by`、`index`、`tab_id` |
| `select_option` | 选中 `<select>` 的某个选项 | `target`（必填）、`option_value` / `option_text` / `option_index`（三选一）、`by`、`index`、`tab_id` |
| `clear` | 清空 input / textarea / contenteditable | `target`（必填）、`by`、`index`、`tab_id` |
| `get_value` | 读取元素当前值（用于验证） | `target`（必填）、`by`、`index`、`tab_id` |

## 多 agent / 多客户端使用

- 每个 `bridge-mcp` 进程启动时生成独立身份（`mcp-<pid>-<nanos>`）
- `new_tab` / `click --new-tab` 创建的标签页会记录创建者；`close_auto_tabs` **只清理本进程创建的标签页**，不会误关其他 agent 正在用的
- CLI 的 `close-auto-tabs` 是人工管理入口，清理全部自动标签页
- 多个 agent 可以同时连同一个 server（请求 id 唯一，不会串线）

## 常见问题

**`run_script` 报 "chrome.userScripts 不可用"**

Chrome 138+ 需要在扩展详情页打开「允许用户脚本」开关（见上文第 3 步）。

**改了扩展代码不生效**

重新 `pnpm build` 后，到 `chrome://extensions` 点扩展卡片上的刷新图标；`run_script` 相关改动可能还需要刷新一次已打开的页面。

**agent 说连不上 / 工具调用超时**

- 确认扩展已加载且「允许用户脚本」已开
- 浏览器保持打开（agent 操控的就是你正在看的这个浏览器）
- 首次调用会自动拉起 server，稍等几秒重试
- 换端口：`BRIDGE_SERVER=ws://127.0.0.1:9226` 环境变量（server 端用 `BRIDGE_PORT`）

**标签页堆积**

配方（如 googletrends）每次查询会新开标签页，让 agent 在流程收尾时调用 `close_auto_tabs`；也可以手动用 CLI 执行 `close-auto-tabs` 清场。
