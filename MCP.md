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

配置好后在客户端里应该能看到 20+ 个工具（`list_tabs`、`navigate`、`click`、`scrape`、`googlesearch`、`redditsearch`、`youtubesearch`、`googletrends`、`googletrends_compare`、`close_auto_tabs` 等）。

## 可用工具

协议原语（通用，任何网站都能用）：

- 标签页：`list_tabs` / `new_tab` / `activate_tab` / `close_tab` / `close_auto_tabs`
- 导航与读取：`navigate` / `get_page_content` / `scrape`（CSS 字段映射，CSP 安全）/ `run_script`（任意 JS）
- 交互：`click`（支持 css/text/xpath 三种定位，锚点默认同标签页打开）/ `click_at` / `press_key` / `scroll`
- 表单：`set_value` / `check` / `select_option` / `clear` / `get_value`

站点配方（专用快捷指令，返回结构化 JSON）：

- `googlesearch`：Google 搜索，每条结果含 title / description / url / target（target 可直接喂给 click）
- `redditsearch`：Reddit 搜索，结构同上
- `youtubesearch`：YouTube 搜索，每条结果含 title / channel / views / published / duration / url / target，支持上传日期（today/week/month/year）与优先顺序（relevance/popularity）筛选，max 控制最多返回条数（默认 5）；直接解析页面 HTML 里的 ytInitialData + InnerTube continuation 翻页（隐藏/被遮挡标签页照常拿满，不弹窗不抢焦点）
- `googletrends`：Google Trends 单关键词，返回趋势序列 + 热门/上升查询（自动翻页）
- `googletrends_compare`：多关键词走势对比（共享 0-100 刻度）

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
