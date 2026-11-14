# 窗口与 Tab 管理指南（CLI / MCP 调用方）

> 面向通过 `bridge-client` CLI 或 MCP 工具操作浏览器的人（含 agent）。只讲你会观察到的行为和应该采取的动作；协议级细节见 [protocol.md](protocol.md)。

## TL;DR

1. **不指定窗口/tab 是默认用法**：所有操作自动落在你自己的 bridge 专属窗口，不会碰用户窗口。
2. **只能操作 bridge 自建的窗口**，指向用户窗口会直接报错。
3. 自动打开的 tab 会被系统记账并自动清理；容量上限 20 tab / 5 窗口，超限按最旧逐出。
4. 会话收尾调用 `close_auto_tabs` + `close_agent_window`（MCP）/ `close-window`（CLI）。

## 1. 缺省行为：你通常不需要指定窗口和 tab

所有页面操作（`navigate` / `click` / `scrape` / `run_script` / `screenshot` …）都可以**不填 `window_id` / `tab_id`**：

- **专属窗口**：每个 client 一个专属 Chrome 窗口。MCP 每个会话、CLI 每个进程各有一个身份（`client_id`），首次操作时自动创建专属窗口（不抢屏幕焦点，也不影响用户已打开的窗口）。
- **缺省 tab**：不指定时使用专属窗口的当前激活页；窗口里没有 tab 会自动开一个。
- **显式指定**：需要多窗口/多 tab 并行时，先 `new_window` / `new_tab` 拿到 id，再在后续调用里带上。
- **CLI 全局选项**：`--window <id>`（或环境变量 `BRIDGE_WINDOW_ID`）等价于给本次所有操作带 `window_id`；省略时用当前 client 的默认窗口。

注意：一次性的工具调用之间不共享"当前 tab"记忆——每次调用都重新按上面的缺省规则定位。多步流程共享目标（`task_id` 租约）是自定义 WebSocket client 的能力，见第 6 节。

## 2. 硬性边界：只能操作 bridge 自己创建的窗口

这是隔离强制，不是约定：

- ❌ `navigate`、`click`、`close_tab`、`screenshot` 等任何指令指向**用户自己开的窗口**都会被拒绝：

  ```
  bridge may only operate on windows it created; omit window_id to use a bridge window, or create one with new_window
  ```

- ✅ 可操作的窗口只有两类：你的专属窗口、`new_window` 创建的窗口。`list_windows` 结果里 `bridge: true` 的就是。
- 指定 `tab_id` 时，该 tab 所在窗口也必须是 bridge 窗口；执行中 tab 被移到别的窗口会报错。
- 例外：`list_tabs` / `list_windows` 是只读的，能看到全部窗口和 tab（含用户的），但不能对它们操作。

## 3. Tab 的创建与归属

bridge 自动打开的 tab 都会被记账（记录创建者身份），便于清理：

| 动作 | 行为 |
|---|---|
| `new_tab` | 在指定窗口（缺省你的专属窗口）开新 tab，记在你名下；响应带 `tab_id` / `window_id` |
| `click` + `new_tab: true`（CLI `click --new-tab`） | 锚点链接由扩展精确开新 tab，新 tab id 精确可知、可直接链式操作，同样记账 |
| `click`（默认） | 锚点 `target="_blank"` 被**改写为当前 tab 打开**，防止流程越点 tab 越多 |
| 站点配方（`googlesearch` / `googletrends` / `youtubeinfo` 等） | 自动开临时 tab 抓取，**结果返回后 10~20 秒由服务端延迟关闭**——你拿到结果即可继续，不用等也不用善后 |

## 4. 容量上限与逐出（20 tab / 5 窗口）

bridge 为自建资源维护容量队列：

- tab 满 **20** 个：再开新 tab 前自动关掉**最旧的 5 个**；
- 窗口满 **5** 个：再建新窗口前自动关掉**最旧的 3 个**；
- "新旧"按**最近使用时间**排序——你一直在用的 tab/窗口会被刷新为"新"，长期不动的先被逐出；
- ⚠️ **没有"在用保护"**：如果长流程正在用某个 tab，而它恰好是最旧的，被逐出时该操作会直接失败（`target window was closed`）。长任务要么保持对 tab 的连续操作，要么显式 `new_tab` 开新的；
- tab 关闭可能连带关闭所在窗口（Chrome 行为），相关记录级联清理，不需要你善后。

## 5. 收尾清理

| 工具 | 平台 | 行为 |
|---|---|---|
| `close_auto_tabs`（CLI `close-auto-tabs`） | MCP / CLI | 只关"自动打开的 tab"，不碰手动开的。MCP 指定 `owner` 只关自己的（多 agent 隔离），省略关全部；CLI 一律全关（手动兜底入口） |
| `close_agent_window` | MCP | 关掉自己的专属窗口（连同所有 tab），释放资源。`owner` 参数可清理其他 agent 的窗口。窗口不存在返回 `{closed: false, window_id: null}`，幂等安全 |
| `close_window`（CLI `close-window`） | MCP / CLI | 按显式 `window_id` 关闭窗口，只能关 bridge 自建窗口 |

**推荐的会话收尾顺序**：

1. `close_auto_tabs`（清临时 tab，兜底配方延迟关闭的遗漏）；
2. `close_agent_window`（MCP，关掉自己的专属窗口）。

## 6. 并发、排队与任务

- 全局最多 **3 个任务同时在浏览器里执行**；**同一个窗口同时只能跑 1 个任务**。指令指向忙窗口时自动排队，排队不占全局名额——一个忙窗口不会阻塞其他窗口。
- 多 agent 共享同一浏览器时，指向同一窗口的请求共用同一把窗口锁；各自的专属窗口互不阻塞。
- 自定义 client 的多步流程（`task_begin` / `task_id`）绑定当前连接：连接断开任务作废，重连后需重新发起，不会静默落到别的窗口。租约空闲 60 秒自动回收。
- 已进入浏览器的操作即使超时/取消也会继续占用窗口直到真正结束（`closing` 状态）——这是防止资源被提前误放的保护，稍等即可，不需要重试补偿。

## 7. 焦点策略：不会打扰用户

- 专属窗口创建时不抢 OS 焦点；`activate_tab` 切换专属窗口内的 tab 也不会把窗口拉到前台。
- `new_window` 默认不聚焦，显式 `focused: true` 才前置。
- `screenshot` 截非激活 tab 时会先在窗口内激活它（不抢 OS 焦点）；窗口被其他应用完全遮挡时才需要 `foreground: true` 把窗口拉到前台。

## 8. 常见报错速查

| 报错 | 含义 / 处理 |
|---|---|
| `bridge may only operate on windows it created...` | 指向了用户窗口。省略 `window_id` 用专属窗口，或先 `new_window` |
| `task already finished` / `task is closing` | 旧 `task_id` 已结束（含连接断开、空闲超时）。重新发起流程 |
| `target window was closed` | 目标窗口没了（可能被逐出、被手动关闭或 tab 关闭连带）。重新 `new_tab` / `new_window` |
| `target tab moved to another window; resolve again` / `cleanup target moved...` | 目标 tab 执行中被移窗。重新调用 |
| `browser execution capacity is still reserved...` | 上一操作还在浏览器里收尾（closing）。稍候重试 |
| `window still has an executing or unresolved browser operation` | 同窗口有操作未结束。等它完成或换窗口 |

## 9. 实践清单

1. **常规单窗口流程**：什么都不指定，`navigate` → `click` → `scrape`，结束 `close_agent_window`。
2. **多页面对照**：`new_tab` 几个（总量 ≤20），用返回的 `tab_id` 分别操作。
3. **独立窗口布局**：`new_window`，后续调用带返回的 `window_id`。
4. **开新页面的链接**用 `click --new-tab`（`new_tab: true`），不要依赖页面自己开——默认行为已被改写为当前页打开。
5. **长流程**保持对 tab 的连续操作（会刷新"最近使用"），避免被 LRU 逐出打断。
6. **多 agent 并行**：各自用缺省专属窗口即可天然隔离；需要共享一个窗口时注意窗口锁会串行化操作。
