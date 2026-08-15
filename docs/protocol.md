# Browser Bridge 协议 v1

传输层为 WebSocket，默认地址 `ws://127.0.0.1:9225`。所有消息均为 JSON 文本帧。

## 连接握手

连接建立后，第一条消息必须是 hello：

```json
{ "type": "hello", "role": "extension", "name": "chrome-main" }
{ "type": "hello", "role": "client", "name": "bridge-client" }
```

- `role: "extension"`：浏览器插件连接。一个 server 同时只服务一个 extension，后连的会顶掉先连的。
- `role: "client"`：指令发起方，可以有多个。

## 请求

```json
{ "id": "abc-123", "method": "navigate", "params": { "url": "https://example.com" } }
```

- `id`：字符串，客户端自定义，用于配对响应。
- `method`：指令名。
- `params`：指令参数（可省略）。

## 响应

成功：

```json
{ "id": "abc-123", "success": true, "result": { "tab_id": 7, "url": "https://example.com", "title": "Example" } }
```

失败：

```json
{ "id": "abc-123", "success": false, "error": "no extension connected" }
```

## 指令表

### 元素定位 target

`click`、`press_key`、`scroll`、`set_value`、`check`、`select_option`、`clear`、`get_value` 都支持统一的元素定位，通过 `params.target` 指定：

```json
{ "by": "css", "value": "#submit", "index": 0 }
{ "by": "text", "value": "登录" }
{ "by": "xpath", "value": "//button[@id='submit']" }
```

- `by`：`css`（默认）/ `text` / `xpath`。
  - `text` 按元素自身的可见文本匹配，精确匹配优先，没有则退化为包含匹配（最深元素优先）。
  - `xpath` 使用 `document.evaluate`。
- `index`：第几个匹配（从 0 开始，默认 0）。
- 不穿透 iframe 与 shadow DOM。

### ping

心跳，任意端可发，server 直接回 pong。

```json
{ "id": "p1", "method": "ping", "params": {} }
```

### list_tabs

列出所有标签页。

params：`{}`

result：

```json
{
  "tabs": [
    { "tab_id": 7, "url": "https://example.com", "title": "Example", "active": true, "window_id": 1 }
  ]
}
```

### close_tab

关闭标签页（默认当前激活标签页）。

params：

```json
{ "tab_id": 7 }
```

`tab_id` 可选。

result：

```json
{ "closed": true, "tab_id": 7 }
```

### close_auto_tabs

关闭 bridge 自动打开（`new_tab` / `click --new-tab` 创建）的标签页，不碰手动开的标签页。

params：

```json
{ "owner": "mcp-1234-abcd" }
```

`owner` 可选：指定时只关闭该创建者的标签页（server 会为每个连接盖章 `client_id`，多 agent 场景各自隔离）；省略时关闭全部（CLI 手动清理入口）。

`new_tab` / `click --new-tab` 创建的标签页会记录创建者身份（请求的 `client_id`，由 server 在转发时盖章）。

result：

```json
{ "closed": [7, 9] }
```

### new_tab

新建标签页，可指定打开 URL。该标签页会被记录为"自动打开的标签页"，可用 `close_auto_tabs` 清理。

params：

```json
{ "url": "https://example.com" }
```

`url` 可选，省略为空白页。

result：

```json
{ "tab_id": 8, "url": "https://example.com", "title": "Example", "active": true }
```

### activate_tab

切换到指定标签页并聚焦所在窗口（默认当前激活标签页）。

params：

```json
{ "tab_id": 7 }
```

`tab_id` 可选。

result：

```json
{ "tab_id": 7, "url": "https://example.com", "title": "Example", "active": true }
```

### navigate

导航标签页（默认当前激活标签页），并等待页面加载完成。

params：

```json
{ "url": "https://example.com", "tab_id": 7 }
```

`tab_id` 可选。

result：

```json
{ "tab_id": 7, "url": "https://example.com", "title": "Example" }
```

### click

点击匹配定位的元素（默认当前激活标签页），最多等待 `timeout` 毫秒。

params：

```json
{ "target": { "by": "css", "value": "#submit" }, "tab_id": 7, "timeout": 5000, "new_tab": false }
```

`timeout` 可选，默认 5000。
`new_tab` 可选，默认 `false`：点击锚点链接时默认在当前标签页打开（覆盖 `target="_blank"`，避免流程开新标签页堆积）；设为 `true` 时由扩展创建新标签页打开（记录为自动打开的标签页，响应中的 `tab_id` 是新标签页的 id，可被 `close_auto_tabs` 清理）。

result：

```json
{ "clicked": { "tag": "button", "id": "submit", "text": "提交" }, "tab_id": 7 }
```

兼容旧协议：只传 `{ "selector": "#submit" }` 等价于 `{ "target": { "by": "css", "value": "#submit" } }`。

### click_at

按页面坐标点击（默认当前激活标签页）。用 `document.elementFromPoint(x, y)` 找到元素后走与 `click` 相同的点击逻辑。

params：

```json
{ "x": 120, "y": 340, "tab_id": 7 }
```

result：

```json
{ "clicked": { "tag": "button", "text": "提交" }, "x": 120, "y": 340, "tab_id": 7 }
```

### press_key

模拟按键（默认当前激活标签页），派发 keydown → keypress（仅单字符/Enter）→ keyup。只能触发页面 JS 按键处理，不能触发浏览器级快捷键。

params：

```json
{
  "key": "Enter",
  "modifiers": ["ctrl", "shift"],
  "target": { "by": "css", "value": "#search" },
  "tab_id": 7
}
```

- `key`：KeyboardEvent.key 规范值，如 `"Enter"`、`"Escape"`、`"a"`、`"F5"`、`"ArrowDown"`。
- `modifiers`：可选数组，取值 `alt` / `ctrl` / `shift` / `meta`。
- `target`：可选；指定则先 focus 再派发，省略则派发到当前聚焦元素（没有则 body）。
- `wait_load`：可选；为 `true` 时按键派发后等待标签页加载完成（适用于 Enter 回车触发导航的场景）。

事件同时携带 `keyCode`/`which`（兼容依赖旧字段的页面处理函数）。注意：合成事件 `isTrusted` 为 false，浏览器级快捷键与原生表单提交不保证触发。

result：

```json
{ "key": "Enter", "modifiers": [], "element": { "tag": "input" }, "tab_id": 7 }
```

### run_script

在页面里执行一段 JS 表达式（可返回 Promise），并把结果 JSON 序列化返回。用于探索页面结构、提取结构化数据等通用场景。

实现上通过 `chrome.userScripts` 注入到 USER_SCRIPT 世界，该世界豁免页面 CSP，因此不会像 `executeScript` 那样被 `script-src 'unsafe-eval'` 拦截。Chrome 135+ 走 `userScripts.execute`（当前页面即可用）；低版本退回注册式 user script + messaging（Chrome 120+，首次使用后需刷新一次目标标签页）。

params：

```json
{
  "code": "Array.from(document.querySelectorAll('a')).slice(0, 3).map(a => ({ text: a.textContent.trim(), href: a.href }))",
  "tab_id": 7
}
```

result：

```json
{ "result": [ { "text": "Example", "href": "https://example.com/" } ], "tab_id": 7 }
```

序列化规则：`null`/字符串/布尔原样，数字转字符串（若超精度），`BigInt` 转字符串，DOM 元素转 `{ __element, id, class, text }`，循环引用标记为 `[Circular]`。

### scrape

按 CSS 选择器提取结构化数据。与 `run_script` 不同，它不执行任意代码（页面内只有静态选择器查询），天然 CSP 安全。

params：

```json
{
  "item": "div.g",
  "fields": {
    "title": "h3",
    "url": "a@href",
    "description": ".VwiC3b"
  },
  "timeout": 5000,
  "tab_id": 7
}
```

- `item`：必填，每条结果的容器选择器。
- `fields`：可选，任意字段映射 `{ 字段名: "选择器[@属性]" }`。字段值默认取匹配元素文本；`@属性`（如 `a@href`、`img@src`）取该属性值，其中 `a@href` 对 `<a>` 返回绝对 URL。
- `timeout`：等待结果出现的最长时间，默认 5000。
- 兼容旧写法：`title` / `link` / `desc`（对应输出 `title` / `url` / `description`）仍然可用；同时传 `fields` 时以 `fields` 为准。

result：

```json
{
  "count": 10,
  "items": [
    { "title": "Example", "url": "https://example.com/", "description": "..." }
  ],
  "tab_id": 7
}
```

### scroll

滚动窗口或指定滚动容器（默认当前激活标签页）。

params：

```json
{ "dx": 0, "dy": 800, "target": { "by": "css", "value": "#list" }, "smooth": true, "tab_id": 7 }
```

- `dx` / `dy`：滚动量，默认 0。
- `target`：可选；省略则滚动整个窗口，指定则滚动该容器元素。
- `smooth`：可选，默认 `false`（瞬间滚动）。

result：

```json
{ "scrolled": { "dx": 0, "dy": 800 }, "element": { "tag": "div", "id": "list" }, "tab_id": 7 }
```

### set_value

设置 input / textarea / contenteditable 的值并派发 input、change 事件（React 受控组件可用）。

params：

```json
{ "target": { "by": "css", "value": "#username" }, "value": "alice", "tab_id": 7 }
```

result：

```json
{ "element": { "tag": "input", "type": "text" }, "value": "alice", "tab_id": 7 }
```

### check

勾选/取消勾选 checkbox 或 radio（默认勾选）。选中 radio 时会取消同组其他选项。

params：

```json
{ "target": { "by": "css", "value": "#agree" }, "checked": true, "tab_id": 7 }
```

result：

```json
{ "element": { "tag": "input", "type": "checkbox" }, "checked": true, "tab_id": 7 }
```

### select_option

选中 `<select>` 的某个选项，按 `value` / `text` / `index` 三选一匹配。

params：

```json
{ "target": { "by": "css", "value": "#city" }, "text": "北京", "tab_id": 7 }
```

result：

```json
{ "element": { "tag": "select" }, "value": "beijing", "text": "北京", "tab_id": 7 }
```

### clear

清空 input / textarea / contenteditable 并派发 input、change 事件。

params：

```json
{ "target": { "by": "css", "value": "#keyword" }, "tab_id": 7 }
```

result：

```json
{ "element": { "tag": "input", "type": "search" }, "tab_id": 7 }
```

### get_value

读取元素当前值，用于验证操作结果。

params：

```json
{ "target": { "by": "css", "value": "#username" }, "tab_id": 7 }
```

result（input 类型会附带 `checked`，select 会附带选中文本）：

```json
{ "element": { "tag": "input", "type": "text" }, "value": "alice", "tab_id": 7 }
```

### get_page_content

读取页面文本、标题和 URL（默认当前激活标签页）。

params：

```json
{ "tab_id": 7 }
```

result：

```json
{ "title": "Example", "url": "https://example.com", "text": "..." }
```

### get_page_markdown

把指定页面内容转换成标准 Markdown（GFM：标题 / 段落 / 列表 / 表格 / 代码块 / 引用块 / 链接 / 图片 / 加粗 / 斜体 / 删除线 / 行内代码），默认当前激活标签页。转换在页面内直接遍历渲染后的 DOM，因此 SPA 动态渲染的内容也会包含在内。默认用 Readability 自动提取主内容（去掉导航 / 页脚 / 相关文章等噪音），提取不到或内容过少时退回整页转换。

自动跳过：`script` / `style` / `noscript` / `template`、隐藏元素（`hidden` / `aria-hidden="true"` / `display:none` / `visibility:hidden`）、表单控件、iframe 等非正文内容。

params：

```json
{
  "tab_id": 7,
  "url": "https://example.com/docs",
  "selector": "article",
  "full": false
}
```

- `tab_id`：可选，目标标签页（默认当前激活页）。
- `url`：可选，先导航到该 URL 并等待加载完成，再转换。
- `selector`：可选，只转换匹配该 CSS 选择器的容器（如 `article` / `#content`），优先级最高。
- `full`：可选，默认 `false`；为 `true` 时跳过正文自动提取，转换整个页面。

result：

```json
{
  "tab_id": 7,
  "title": "Example",
  "url": "https://example.com/docs",
  "markdown": "# Example\n\n正文..."
}
```

图片与链接会转成绝对 URL；代码块语言从 `language-*` / `lang-*` / `highlight-*` class 或 `data-lang` 识别。

### get_a11y_tree

读取页面 a11y tree（无障碍树），返回扁平节点列表。适合需要与页面交互（点击 / 填表 / 选择 / 勾选）前先了解页面结构、找出可交互元素的场景：每个可交互节点带 `target`，可直接喂给 `click` / `set_value` / `check` / `select_option` / `clear` / `get_value`。

params：

```json
{
  "tab_id": 7,
  "include_hidden": false,
  "max_nodes": 500
}
```

- `tab_id`：可选，目标标签页（默认当前激活页）。
- `include_hidden`：可选，默认 `false`；为 `true` 时包含隐藏元素（`hidden` / `display:none` / `visibility:hidden` / `aria-hidden`）。
- `max_nodes`：可选，最多返回节点数，默认 500（范围 10-5000），防止大页面输出过大。

result：

```json
{
  "tab_id": 7,
  "title": "Example",
  "url": "https://example.com",
  "count": 42,
  "nodes": [
    { "role": "button", "name": "提交", "value": null, "states": ["enabled"], "depth": 3, "tag": "button", "target": { "by": "css", "value": "#submit", "index": 0 } },
    { "role": "textbox", "name": "用户名", "value": "alice", "states": ["enabled"], "depth": 2, "tag": "input", "target": { "by": "css", "value": "#username", "index": 0 } },
    { "role": "heading", "name": "文档", "value": null, "states": ["enabled"], "depth": 1, "level": 1, "tag": "h1" }
  ]
}
```

- `role` / `name`：无障碍角色与可访问名称，优先用 Chrome 的 `computedRole` / `computedName`，低版本回退到标签/属性推断。
- `value`：输入类元素的当前值（select 为选中项文本），无则为 `null`。
- `states`：状态数组（`enabled` / `disabled` / `checked` / `unchecked` / `expanded` / `collapsed` / `required` / `readonly` / `selected` / `pressed` 等）。
- `level`：仅 heading 角色，标题层级。
- `depth`：DOM 深度（可据此还原树形结构）。
- `target`：仅可交互节点（button / link / textbox / checkbox / radio / combobox / select / slider 等）带定位，可直接喂给 `click` 等指令。

与元素定位行为一致：只遍历 light DOM，不穿透 iframe 与 shadow DOM。

## 服务端错误

| error | 场景 |
|-------|------|
| `missing id` | 请求没有 id |
| `no extension connected` | 没有插件连接时收到 client 请求 |
| `timeout: extension did not respond in 30s` | 插件 30 秒未响应 |
| `expected hello message` / `hello must declare role` / `invalid hello` | 握手不合法 |

## 扩展方向

- 多 extension：握手增加 `browser_id`，请求通过 `params.browser_id` 指定。
- 鉴权：握手增加 `token`。
