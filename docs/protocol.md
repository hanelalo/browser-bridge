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

点击匹配 CSS selector 的元素（默认当前激活标签页），最多等待 `timeout` 毫秒。

params：

```json
{ "selector": "#submit", "tab_id": 7, "timeout": 5000 }
```

`timeout` 可选，默认 5000。

result：

```json
{ "clicked": { "tag": "button", "id": "submit", "text": "提交" }, "tab_id": 7 }
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
