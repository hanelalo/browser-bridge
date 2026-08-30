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
cargo run -- querydomains 'browserbridge'
cargo run -- googlesearch 'Haze Seas'
cargo run -- redditsearch 'rust programming'
cargo run -- youtubesearch 'rust programming' --time week --sort popularity --max 10
cargo run -- youtubeinfo 'https://www.youtube.com/watch?v=rQ_J9WH6CGk'
cargo run -- youtuberinfo 'https://www.youtube.com/@xiaojunpodcast/videos'
cargo run -- youtuberinfo '@xiaojunpodcast' --max 20
cargo run -- googletrends 'ai image' --date 'today 1-m' --geo Worldwide
cargo run -- googletrends-compare 'ai image' 'GPTs' --date 'today 1-m'
cargo run -- get-page-markdown --url https://example.com
cargo run -- get-a11y-tree
cargo run -- screenshot --out shot.png
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
| `get-page-markdown [--url <url>] [--selector <css>] [--full]` | 把页面内容转换成标准 Markdown（默认自动提取正文，去掉导航/页脚等噪音） |
| `get-a11y-tree [--include-hidden] [--max-nodes <n>]` | 读取页面 a11y tree，可交互节点（按钮/链接/输入框等）带 `target` 可直接喂给 `click` / `set_value` |
| `screenshot [--tab <id>] [--format png\|jpeg] [--quality <0-100>] [--out <file>] [--foreground]` | 截取页面可见区域截图，返回 base64 data URL；`--out` 直接保存为文件 |
| `googlesearch '<关键词>'` | Google 搜索，输出 `{ tab_id, results }` |
| `redditsearch '<关键词>'` | Reddit 搜索，输出 `{ tab_id, results }` |
| `youtubesearch '<关键词>' [--time] [--sort] [--max]` | YouTube 搜索，支持上传日期 / 优先顺序筛选，最多返回 `--max` 条（默认 5），输出 `{ tab_id, results }` |
| `youtubeinfo '<视频URL或ID>'` | 获取指定 YouTube 视频详情：字幕全文、URL、作者、时长、点赞/评论/订阅数，输出 `{ tab_id, video }` |
| `youtuberinfo '<频道URL或handle>' [--max]` | 获取指定 YouTube 频道（youtuber）的视频列表：频道名、订阅数、视频名称/URL/观看数/时长/发布时间，最多返回 `--max` 条（默认 10），输出 `{ tab_id, channel, videos }` |
| `googletrends '<关键词>' [--date] [--geo]` | Google Trends，输出 `{ tab_id, trend[], top[], rising[], regions[] }` |
| `googletrends-compare <词1> <词2>... [--date] [--geo]` | Google Trends 多词对比，输出 `{ series[] }` |
| `querydomains '<关键词>' [--tlds 'com,ai,xyz']` | Query.Domains 批量查域名注册情况与价格，输出 `{ results[] }`（每项含 domain / tld / status / available / price / badges） |

多数指令支持 `--tab <id>` 指定标签页，默认操作当前激活页。

**标签页管理**：`click` 点击锚点链接默认在当前标签页打开（自动覆盖 `target="_blank"`），需要新开时用 `--new-tab`（由扩展创建标签页，响应会返回新标签页的 `tab_id`，便于链式操作）。`new-tab` 指令和 `click --new-tab` 打开的标签页都会被扩展记录，流程结束后可用 `close-auto-tabs` 一键清理，不会误关你手动打开的标签页。

### close-auto-tabs

清理"自动打开的标签页"，需要**单独执行**（CLI 手动调用，或 MCP 流程在收尾时调用一次），不会误关手动打开的标签页。支持**多 agent 隔离**：

- **MCP（`close_auto_tabs` 工具）**：每个 MCP 进程启动时生成独立身份（`mcp-<pid>-<nanos>`），只清理**本进程创建**的标签页，不会误关其他 agent 正在用的标签页；任务结束后可再调 `close_agent_window` 关闭自己的专用窗口（连同窗口内标签页一并释放）
- **CLI（`close-auto-tabs`）**：作为人工管理入口，清理全部自动标签页（不管是谁创建的）

**会被清理的**：`new-tab` 指令和 `click --new-tab` 创建的标签页（扩展记录在 `chrome.storage.session`，service worker 重启不丢）。例如 `googletrends` 每次查询都会新开一个标签页，跑完后清理效果最明显：

```sh
cargo run -- googletrends 'ai image'
cargo run -- close-auto-tabs   # 关闭刚才 googletrends 开的标签页
```

**不会被清理的**：手动开的标签页（如 Sitemap Monitor）、以及 `navigate` / `googlesearch` / `redditsearch` 复用的当前标签页（这些不新开 tab，属于"工作标签页"，留着是正常的）。

### get-page-markdown

把页面内容转换成标准 Markdown，输出 `{ tab_id, title, url, markdown }`。转换在页面内直接遍历渲染后的 DOM，SPA 动态渲染的内容也会包含；自动跳过脚本、隐藏元素与表单控件，链接/图片转成绝对 URL。转换核心基于开源 [Turndown](https://github.com/mixmark-io/turndown) + [@joplin/turndown-plugin-gfm](https://github.com/laurent22/joplin/tree/dev/packages/turndown-plugin-gfm)（GFM 表格 / 删除线 / 任务列表），正文提取用 [@mozilla/readability](https://github.com/mozilla/readability)（Firefox 阅读模式同款）。

```sh
cargo run -- get-page-markdown                                  # 当前标签页（自动提取正文）
cargo run -- get-page-markdown --url https://example.com/docs   # 先导航再转换
cargo run -- get-page-markdown --selector article               # 只转换 article 容器
cargo run -- get-page-markdown --full                           # 跳过提取，转换整页
cargo run -- get-page-markdown --selector '#content' --tab 7    # 指定标签页 + 指定容器
```

- `--url`：可选，先导航到该 URL 并等待加载完成，再转换。
- 默认行为：用 Readability 自动提取主内容（去掉导航 / 页脚 / 相关文章等噪音），提取不到或内容过少时退回整页转换。
- `--selector`：可选，只转换匹配该 CSS 选择器的容器（如 `article` / `#content`），优先级最高。
- `--full`：可选，跳过正文自动提取，转换整个页面。

### get-a11y-tree

读取页面 a11y tree（无障碍树），返回 `{ tab_id, title, url, count, nodes[] }`。适合需要与页面交互（点击 / 填表 / 选择 / 勾选）前先了解页面结构、找出可交互元素的场景——比 `get-page-content` 的纯文本更能回答"页面上有什么按钮、输入框、下拉框"：

```sh
cargo run -- get-a11y-tree
cargo run -- get-a11y-tree --max-nodes 1000   # 大页面放宽上限
cargo run -- get-a11y-tree --include-hidden    # 连隐藏元素一起返回
```

`nodes` 是扁平节点列表，每项含 `role`（无障碍角色）/ `name`（可访问名称）/ `value`（当前值）/ `states`（enabled / disabled / checked / expanded 等）/ `depth`（DOM 深度）/ `tag`；可交互节点额外带 `target`，可直接喂给 `click` / `set_value` / `check` / `select_option` / `clear` / `get_value`：

```sh
cargo run -- click '#submit'              # target 直接可用
```

- `--include-hidden`：可选，默认只返回可见元素；开启后包含 `hidden` / `display:none` / `visibility:hidden` / `aria-hidden` 的元素。
- `--max-nodes`：可选，最多返回节点数（默认 500，范围 10-5000），防止大页面输出过大。
- 角色与名称优先用 Chrome 的 `computedRole` / `computedName`（Chrome 135+），低版本自动回退到标签/属性推断；只遍历 light DOM，不穿透 iframe 与 shadow DOM（与元素定位行为一致）。

### screenshot

截取页面可见区域截图，协议输出 `{ tab_id, url, title, mime, format, width, height, size, data }`，`data` 是完整 base64 data URL（`data:image/png;base64,...` / `data:image/jpeg;base64,...`）。MCP 的 `screenshot` 工具会把图片作为**标准 MCP 图片块**返回（另附文本元信息块），agent 可直接查看、无需自己解码：

```sh
cargo run -- screenshot                                # 当前激活页，PNG
cargo run -- screenshot --format jpeg --quality 80     # JPEG，质量 80
cargo run -- screenshot --out shot.png                 # 解码保存为文件（--out 时打印摘要，不输出大段 base64）
cargo run -- screenshot --tab 7 --out shot.jpg         # 指定标签页
cargo run -- screenshot --foreground --out shot.png    # 先把目标窗口拉到前台再截（避免被遮挡时截到别的内容）
```

- 基于 `chrome.tabs.captureVisibleTab`，捕获的是目标标签页所在窗口的**可见区域**（viewport），不含视口外的内容——需要看页面其他部分时先 `scroll` 再截。
- 目标标签页若不是其窗口的激活页会先激活（不抢 OS 焦点，agent 专用窗口照常工作）。
- `--format`：`png`（默认）/ `jpeg`；`--quality`：JPEG 质量 0-100（默认 90，仅 jpeg 有效）。
- `--foreground`：默认关闭。窗口被其他应用完全遮挡时，`captureVisibleTab` 截到的可能是遮挡内容，加 `--foreground` 会把目标窗口拉到 OS 前台再截。
- `--out`：把 data URL 解码写入指定文件（自动创建父目录）；省略时直接打印完整 data URL。
- 只截可见区域、不滚动拼接整页：这是 `captureVisibleTab` 的能力边界（本项目不依赖 CDP）。

### googlesearch

Google 搜索专用快捷指令，输出 `{ "tab_id": ..., "results": [...] }`，`tab_id` 是搜索所在标签页（供后续指令链式操作），`results` 每项含 `title` / `description` / `url` / `target`：

```sh
cargo run -- googlesearch 'Haze Seas'
```

`target` 是可直接喂给 `click` 的元素定位（`{ by, value, index }`），方便后续点击某个结果。实现是 client 侧的"站点配方"：用通用原语 `navigate` + `scrape` 编排，选择器作为常量集中在 client 里（`#rso > div` 容器、`data-sncf='1'` 描述等），扩展与协议保持通用。

### querydomains

Query.Domains 域名批量查询，按关键词同时检查多个 TLD 的注册情况与注册价格，输出 `{ tab_id, query, tlds, complete, results[] }`：

```sh
cargo run -- querydomains 'browserbridge'
cargo run -- querydomains 'browserbridge' --tlds 'dev,cloud,blog'   # 自定义 TLD（默认 14 个，最多 20 个）
```

`results` 每项含 `domain` / `tld` / `status`（`available` / `unavailable` / `uncertain`）/ `available`（布尔）/ `price`（可用时的注册价，如 `3 USD`，不可用时为 `null`）/ `badges`（原始徽标：价格、注册年份、`29 days ago` 等）。实现是导航到首页 → 每次都打开 TLD 自定义模态框显式写入后缀（站点会持久化自定义列表，不重置就不是默认 14 个）→ 输入关键词回车 → 用 `run_script` 等到批量检查流（`/api/upstream/check` 的 resource entry 只在请求完成后出现）真正结束后逐行提取（圆点颜色判状态、徽标容器取价格），选择器集中在 `bridge-core/src/recipes/querydomains.rs`。个别 TLD 可能没有价格徽标（上游未返回定价），此时 `price` 为 `null`，属站点数据问题而非超时。

### redditsearch

Reddit 搜索专用快捷指令，返回 `{ tab_id, results[] }`，每项含 `title` / `description` / `published`（相对时间，如 `1mo ago`）/ `published_at`（ISO 时间戳）/ `votes`（整数）/ `comments`（整数）/ `url` / `target`：

```sh
cargo run -- redditsearch 'rust programming'
```

结果页有两种渲染形态：`search-post-with-content-preview`（带正文预览）与 `search-sdui-post`（只有标题），配方同时收取；描述取自帖子正文预览，`search-sdui-post` 形态没有预览时为 `null`。Reddit 首页的搜索框藏在两层 shadow DOM 里，通用定位指令够不到，但配方直接导航到 `/search/?q=`，不依赖首页交互。

### youtubesearch

YouTube 搜索专用快捷指令，返回 `{ tab_id, results[] }`，每项含 `title` / `channel` / `views` / `published` / `duration` / `url` / `target`（`target` 可直接喂给 `click` 打开视频）：

```sh
cargo run -- youtubesearch 'rust programming'
cargo run -- youtubesearch 'rust programming' --time week        # 本周上传
cargo run -- youtubesearch 'rust programming' --sort popularity  # 热门程度优先
cargo run -- youtubesearch 'rust programming' --time month --sort popularity
cargo run -- youtubesearch 'rust programming' --time week --max 10   # 最多返回 10 条
```

- `--time`：上传日期筛选，`any`（默认）/ `today` / `week` / `month` / `year`
- `--sort`：优先顺序，`relevance`（默认）/ `popularity`（热门程度）
- `--max`：最多返回多少条结果（默认 5，至少 1）
- 日期与排序可组合（如 `--time month --sort popularity`）

筛选不是靠点击页面 UI，而是直接构造 YouTube 搜索 URL 的 `sp` 参数（今天 `EgIIAg==`、本周 `EgIIAw==`、本月 `EgIIBA==`、今年 `EgIIBQ==`；热门程度 `CAM=`；组合 token 实测自真实浏览器 2026 年的"过滤"面板）。**不依赖页面渲染**：导航返回的 HTML 里就内嵌了完整首屏数据（`ytInitialData`，约 20 条），配方直接解析它；不够 `--max` 时再取页面里的 InnerTube API key/context，用 continuation token 调 `/youtubei/v1/search` 续取（yt-dlp 同款数据源）。数据在 HTML 里就齐全，所以**标签页在后台/被全屏应用遮挡也照常拿满，不弹窗、不抢焦点、不用切过去**，实测 `--max 40` 约 3 秒返回。duration 直接取接口的 lengthText，不会缺失。若页面数据缺失（如验证墙/consent 页）会返回明确错误提示。

### youtubeinfo

获取指定 YouTube 视频的详情，返回 `{ tab_id, video }`，`video` 含 `url` / `title` / `author` / `author_url` / `duration`（`HH:MM:SS`）/ `duration_seconds` / `like_count` / `comment_count` / `subscriber_count`（均为解析后的整数，`万`/`亿`/`K`/`M` 等缩写会换算）/ 对应的 `*_text` 原始文本 / `captions[]`（每个字幕轨道含 `language_code` / `name` / `kind` / `text` 全文）：

```sh
cargo run -- youtubeinfo 'https://www.youtube.com/watch?v=rQ_J9WH6CGk'
cargo run -- youtubeinfo 'rQ_J9WH6CGk'                  # 直接传 11 位视频 ID
cargo run -- youtubeinfo 'https://youtu.be/rQ_J9WH6CGk' # youtu.be 短链 / shorts / embed / live 均可
```

- 输入支持 11 位视频 ID、`watch?v=`、`youtu.be/`、`/shorts/`、`/embed/`、`/live/` 链接
- 点赞数取自页面点赞按钮（`LIKE` 的 `title`，如 `1.2万`）；评论数用 InnerTube `next` continuation 接口（yt-dlp 同款数据源，不依赖滚动评论区）；订阅数取自 `videoOwnerRenderer`
- 字幕优先用页面内嵌的 `captionTracks`（timedtext json3）；若返回空（YouTube 对 `exp=xpe` 的轨道要求 PO token，页面内无法生成），按 yt-dlp 的做法改用 **android_vr 客户端**调 player API 取无 pot 要求的轨道
- 同样不依赖页面渲染：数据来自 HTML 内嵌 JSON + InnerTube 接口，标签页在后台也能取到

### youtuberinfo

获取指定 YouTube 频道（youtuber）的视频列表，返回 `{ tab_id, channel, videos[] }`。`channel` 含 `name` / `url` / `subscriber_count`（解析后的整数，`万`/`亿`/`K`/`M` 缩写会换算）/ `subscriber_count_text`（原始文本）；`videos` 每项含 `title` / `url` / `views`（原始文本）/ `views_count`（整数）/ `duration` / `published` / `target`（可直接喂给 `click` 打开视频）：

```sh
cargo run -- youtuberinfo 'https://www.youtube.com/@xiaojunpodcast/videos'
cargo run -- youtuberinfo '@xiaojunpodcast'            # 直接传 handle
cargo run -- youtuberinfo '@xiaojunpodcast' --max 20   # 最多返回 20 条
```

- 输入支持完整频道 URL（`/@handle`、`/c/`、`/user/`、`/channel/UC...` 均可，末尾带不带 `/videos` 都行）或 handle（`@handle` 或裸 handle）
- `--max`：最多返回多少条视频（默认 10，至少 1）
- 数据来自频道页 HTML 内嵌的 `ytInitialData` + InnerTube `browse` continuation（yt-dlp 同款数据源）：首屏直接解析，不足 `--max` 条时自动续取；不依赖页面渲染、滚动或窗口可见性，标签页在后台也能拿满
- 订阅数同时兼容新旧版频道页结构（`c4TabbedHeaderRenderer` 与新版 `contentMetadataViewModel`）
- 视频条目同样兼容新旧版结构：旧版 `videoRenderer` / `gridVideoRenderer` 与 2026 年新版频道页的 `lockupViewModel`（`richItemRenderer` + `lockupViewModel`）都能解析，实测新版已全面切换
- 标题完整性兜底：频道页数据在部分会话/变体下会把超长标题截断（实测截到 100 字符甚至更短），配方会用 YouTube 官方 oEmbed 接口校验，发现疑似截断的标题自动替换成完整标题

### googletrends

Google Trends 趋势查询，返回 `{ tab_id, trend[], top[], rising[] }`：
Google Trends 趋势查询，返回 `{ tab_id, trend[], top[], rising[], regions[] }`：

```sh
cargo run -- googletrends 'ai image' --date 'today 1-m' --geo Worldwide
```

- `trend`：时间序列 `[{ date, value }]`，`value` 为 0-100 相对热度（从图表 SVG 曲线坐标反解 + y 轴刻度校准）
- `top` / `rising`：热门查询与热度上升的查询（排名、关键词、热度、变化百分比），自动翻完所有分页
- `regions`：按地区显示的搜索热度 `[{ rank, region, geo_code, interest }]`，`geo_code` 为 ISO 地区码，同样自动翻完分页（实测可达 66 个地区）
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
    ├── youtubesearch.rs  # YouTube 搜索（解析 ytInitialData + InnerTube 翻页 + sp 筛选）
    ├── youtubeinfo.rs    # YouTube 视频详情（字幕全文 + 点赞/评论/订阅数，InnerTube 接口）
    ├── youtuberinfo.rs   # YouTube 频道视频列表（频道名/订阅数/视频列表，InnerTube 翻页）
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
- Chrome 没在运行会自动拉起默认 Chrome（共享 profile），等扩展连上后重试（最长约 30 秒）；
- 每个 agent（`mcp-` 身份）自动拥有一个**专用浏览器窗口**：标签页默认开在那里，不占你正在看的窗口、不抢焦点；
- 本进程拉起的 Chrome 会在空闲 10 分钟（`BRIDGE_CLOSE_CHROME_IDLE_SECS` 可覆盖）或 server/会话结束时自动退出，自己开的 Chrome 不受影响；
- 工具列表：`list_tabs` / `close_tab` / `close_auto_tabs` / `close_agent_window` / `new_tab` / `activate_tab` / `navigate` / `click` / `click_at` / `press_key` / `scroll` / `set_value` / `check` / `select_option` / `clear` / `get_value` / `scrape` / `run_script` / `get_page_content` / `get_page_markdown` / `get_a11y_tree` / `screenshot` / `googlesearch` / `redditsearch` / `youtubesearch` / `youtubeinfo` / `youtuberinfo` / `googletrends` / `googletrends_compare`。

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

前提：Chrome 里已加载扩展（`chrome://extensions` 加载 `extension/dist/chrome-mv3`）。Chrome 没开也没关系——MCP 首次调用会自动拉起默认 Chrome 和 bridge-server，扩展会自动连上，无需手动启动任何进程。

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
| MCP 关闭拉起的 Chrome 空闲时间 | 600（10 分钟） | `BRIDGE_CLOSE_CHROME_IDLE_SECS`（秒）；server 断开或 MCP 会话结束时也会关闭自己拉起的 Chrome |
| Chrome 版本 | 120+ | `run_script` 需要 `chrome.userScripts`（135+ 体验最佳） |

## 安全说明

- server 只监听 `127.0.0.1`，不要让公网访问。
- 插件声明了 `host_permissions: <all_urls>` 才能操作任意页面——这是个人工具的便利；如果要对外分发，应改成按站点授权。
- 服务端目前接受任意角色的连接；如需更强隔离，可在握手阶段加 token。
- `run_script` / `scrape` 可以读写页面，属于强能力；不要把它暴露给不可信来源。

## License

本项目使用 [MIT License](LICENSE) 开源。Copyright (c) 2026 hanelalo。

你可以自由使用、修改、分发本项目（含商用），但需保留版权声明和许可协议原文。本项目按 "AS IS" 提供，不附带任何明示或默示的担保。
