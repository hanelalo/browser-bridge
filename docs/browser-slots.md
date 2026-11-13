# 浏览器槽位（Browser Slots）与沙箱设计

> 状态：设计定稿，未动代码。本文档是后续实现（hub 多浏览器、server 侧浏览器生命周期、沙箱容器）的依据。
> 背景：当前 hub 是单扩展模型（`Hub.extension: Option<u64>`），agent 操作无法避免打扰用户浏览器，也不支持多 profile / 多身份。

## 1. 目标

1. **焦点隔离**：agent 的浏览器操作永不把窗口弹到用户面前；页面兼容性不再靠"禁用焦点"换。
2. **多 profile**：支持多个浏览器身份（不同登录态）并存、按需启停、per-profile 网络出口。

## 2. 已确认的决策

| # | 决策 |
|---|------|
| D1 | 三档 slot：`daily` / `sandbox-gui` / `sandbox-headless`，**默认槽位为 `sandbox-gui`** |
| D2 | hub 多浏览器化：握手身份升格为 `browser_id`，按 slot 路由（`docs/protocol.md` 已预留该方向） |
| D3 | 浏览器生命周期（拉起/停止/装扩展）由 **bridge-server 统一管理**；MCP/CLI 保持无状态 |
| D4 | 一个沙箱 = 一个 profile = 一个独立 `--user-data-dir`；按需启停（停止即零内存），不做同进程多原生 profile |
| D5 | 焦点礼仪从"全局禁令"降级为 **per-slot 策略**：仅 `daily` 保留，沙箱槽位全部解除 |
| D6 | 扩展安装不依赖 `--load-extension`（新版本 Chrome 已移除）：沙箱用 Preferences 种入，容器用 CRX+策略，daily 维持手动 load unpacked |
| D7 | `sandbox-gui` 镜像内置 **Xvfb + noVNC**：人工登录/验证码接管在容器内完成，不碰用户浏览器 |
| D8 | headless 被降级为可选快路径：实测部分站点反爬直接拦截 headless，有头 + 虚拟显示是兼容性正解 |

## 2.1 实现范围修订（2026-11-14）

需求变更：**多 profile 暂缓**，沙箱保留，且**扩展由用户手动安装**（不再做自动安装机制）。

当前落地范围（阶段 0，单沙箱）：

- 只实现 `sandbox-gui` 单实例：Apple container + Debian Chromium + Xvfb + openbox + noVNC，镜像在 `scripts/sandbox/`，生命周期工具 `scripts/sandbox.sh`（start/stop/status/logs/vnc）。
- profile 持久化：`~/.bridge/sandbox/profile` bind mount 进容器 `/data/profile`，容器删除不丢扩展安装与登录态。
- **扩展手动安装**：首次 `start` 后打开 noVNC 地址，在沙箱 Chromium 里 `chrome://extensions` → 开发者模式 → 加载已解压 `/opt/bridge-ext`（宿主机 `extension/dist/chrome-mv3` 只读挂载，更新扩展只需重跑 `pnpm build` + `sandbox.sh stop && start`）→ 顺手开启「允许用户脚本」（解锁 run_script）。一劳永逸，全部安装机制的复杂度（§8、§14 的 CfT/策略/种入）**降级为历史记录与长期备选**。
- **hub 不改**（单扩展槽位）：沙箱扩展就是唯一连接的扩展；日常 Chrome 里的同名扩展须停用，否则互相抢占槽位。
- `server.sh` 支持 `BRIDGE_BIND`（沙箱回连需要非 loopback 绑定；设 `0.0.0.0` 会把未鉴权的 hub 暴露到 LAN，鉴权见 protocol.md 预留方向）。
- MCP/CLI 工具面零改动；`bridge-mcp` 无扩展连接时的 `open -a` 拉起逻辑**不再适用**（会拉起用户日常 Chrome），后续改为提示运行 `sandbox.sh start`。

暂缓项（设计保留，见 §5/§6）：hub 多浏览器路由、per-slot 配额、多 profile、`daily` 槽位。

## 3. 三档 Slot 模型

| slot 类型 | 形态 | 焦点礼仪 | 反爬表现 | 用途 |
|-----------|------|----------|----------|------|
| `daily` | 用户日常浏览器（手动 load unpacked 扩展） | **保留**（`focused:false`、不聚焦 activate_tab、截图不拉前台） | 最好（真实用户指纹） | 人机协作：登录风控、验证码、顶级 Cloudflare |
| `sandbox-gui` | Linux 容器：有头 Chrome + Xvfb + openbox + noVNC | **解除** | 接近真浏览器（有头指纹 + 家宽出口 IP） | **默认槽位**，日常 agent 任务主体 |
| `sandbox-headless` | 本机 `--headless=new` Chrome，无容器 | **解除** | 差，部分站点被拦 | 无反爬站点的快速采集，省容器开销 |

架构上三者同构：一条扩展 WebSocket 连接 = 一个 `browser_id`。headless 与容器只是 slot 的形态参数，不是架构分支。

## 4. 目标架构

```
                MCP (stdio/HTTP) / CLI
                        │  请求带 params.browser_id（或会话级默认槽位）
                        ▼
                bridge-server (hub)
        ├── 路由表：HashMap<browser_id, 扩展连接>
        ├── per-slot 配额（并发/窗口锁/tracked 资源）
        └── 浏览器生命周期管理器（slot 配置 → spawn/stop/list）
          │              │                    │
   [daily] 手动    [sandbox-gui] 容器     [sandbox-headless] 本机
   日常 Chrome     Chrome + Xvfb + openbox   --headless=new
   扩展手动装      noVNC(127.0.0.1)          独立 user-data-dir
   礼仪保留        CRX 策略装扩展            Preferences 种入装扩展
   礼仪解除/解除   可配 per-profile 代理      Preferences 种入装扩展
          │              │                    │
          └────── ws://127.0.0.1:9225（容器内走 host.docker.internal）──────┘
```

## 5. 协议与 hub 改造

- **握手**：扩展 hello 的 `client_id`（extensionInstanceId UUID，每 profile 天然不同）升格为 `browser_id`；hub 对同/异实例的连接从"踢旧/拒新"（`hub.rs:196-198`、`hub.rs:186-195`）改为**并存注册**。
- **路由**：请求增加 `params.browser_id`；未指定时用该 client 会话的默认槽位（MCP/CLI 增加 `browser` 参数与 `use_browser` 会话级默认）。
- **配额 per-slot 化**：`MAX_RUNNING=3`、tracked windows 5 / tabs 20（`hub.rs:10-18`）从全局改为每 slot 独立。
- **窗口锁**：key 从 `window_id` 改为 `(browser_id, window_id)`——Chrome 窗口 id 跨实例会撞号。
- **`_window_tasks` / sidepanel**：扩展带自身 instance id 查询，server 过滤只返回该实例的数据（sidepanel 语义天然正确）。
- **资源清理**：`close_agent_window` / `close_auto_tabs` / `schedule_tab_close` 的 owner 语义叠加 browser 维度。
- **文档**：`docs/protocol.md` 同步更新（多 extension 节从"预留"转正）。
- **三处同步**（AGENTS.md 约定）：`bridge-mcp/src/lib.rs`、`client/src/main.rs`、必要时 `extension/entrypoints/background.ts`。

## 6. 浏览器生命周期与资源管理（server 侧新增）

### 6.1 Slot 与容器的两层模型

- **Slot（身份层）**：配置 + profile 目录（`~/.bridge/profiles/<name>/`，bind mount 进容器）。可以有任意多个，唯一约束是磁盘；默认**永不自动删除**（删除等于删身份），`slot remove` 显式删，`slot list` 报告磁盘占用，超期未用仅标记提醒。
- **容器（运行层）**：slot 的运行态实例。**每 slot 至多 1 个**（Chrome user-data-dir SingletonLock 硬约束）；**全局运行中沙箱容器有上限**（`max_running_sandboxes`，默认 2，按内存预算定）；**每容器 cgroup 限额**（默认内存 2GB / CPU 1.5 核），防单个失控页面拖垮宿主机。

### 6.2 Slot 配置文件

```toml
# ~/.bridge/slots.toml
max_running_sandboxes = 2       # 全局运行中沙箱容器上限

[slots.work]
type = "sandbox-gui"            # daily 不需要配置（手动存在）
proxy = ""                      # 可选 per-profile 出口
default = true                  # MCP 未指定 browser 时的默认槽位
idle_timeout_ms = 900000        # 空闲回收阈值，默认 15 分钟

[slots.scraper]
type = "sandbox-headless"
```

### 6.3 状态机

```
stopped ──任务触发/手动 start──▶ starting ──扩展 ws 连上──▶ ready
   ▲                                                        │
   │              无在途任务租约 + 无 VNC 连接，idle 超时
   └── stopping ◀─────────────────────────────────────── idle ◀──┘（任务结束回到 idle）
   crashed：崩溃重试（指数退避）N 次失败后停在此态，等待下次触发或手动处理
```

- **懒启动**：第一个指向该 slot 的任务触发 spawn；冷启动 2-5s 的等待由 hub 现有任务排队语义覆盖（排队不占全局名额）。运行中容器数达到上限时，spawn 请求在 server 侧 FIFO 排队等 permit，不报错。
- **空闲回收**：无在途任务 + 无活跃 VNC 连接（VNC 接管期间 pin 住不回收）→ idle 超时后自动 stop。登录态在磁盘 profile 里，重启不丢。
- **显式控制**：CLI/MCP `slot start / stop / pin`，手动 pin 可锁住不回收。
- **不做 per-task 一次性容器**：每任务冷启动太贵、丢预热缓存，且"每次全新会话"的指纹反而更可疑。容器是长驻 + 空闲回收。

### 6.4 spawn → 连接认领

server 记录 `pending_spawn`（目标 slot 名 + 时间），串行化；spawn 后**下一个未知 UUID 的 hello 认领为该 slot**，已知 UUID 视为重连。手动接入的浏览器走 CLI 别名注册（`bridge slot register <uuid> --name daily`）。

### 6.5 孤儿与对账

- 所有托管容器打 label（`bridge.slot=<name>`、`bridge.managed=true`）。
- **server 重启**：按 label 对存量容器全部 stop，干净重来（懒启动会再拉，状态都在磁盘）。
- **容器 crash**：指数退避重试 N 次，仍失败标 `crashed` 并上报；扩展 ws 断线重连由 hub 既有机制处理（UUID 持久化后跨重启稳定）。

### 6.6 持久身份

扩展身份从 `chrome.storage.session`（重启即变）迁到 `chrome.storage.local`，UUID 跨重启稳定；slot 别名表存 server 侧。

## 7. 焦点策略与页面兼容性

礼仪代码不是删除，而是按 slot 类型分叉：

| 行为 | daily | sandbox-gui / headless |
|------|-------|------------------------|
| agent 窗口创建 | `focused:false`（`background.ts:870-874`） | `focused:true` |
| `activate_tab` | 不聚焦窗口（`background.ts:1532-1539`） | 直接聚焦 |
| `screenshot` | 激活前奏 + foreground 才拉前台（`background.ts:1409-1420`） | 无前奏，永远"可见" |
| 焦点限制解除后的收益 | — | 后台降速类页面加载失败问题随 occlusion/throttling 消失 |

沙箱实例启动 flag（方向性列举，组合需实测）：

```
--disable-background-timer-throttling
--disable-backgrounding-occluded-windows
--disable-renderer-backgrounding
--disable-features=IntensiveWakeUpThrottling
--no-first-run --no-default-browser-check
```

`--disable-session-crashed-bubble` 等防弹窗 flag 一并纳入。headless 形态额外加 `--headless=new`。

## 8. 扩展安装（不依赖 --load-extension）

> **2026-11 可行性验证更新**：机制 1 在品牌版 Chrome 157 (macOS) 上实测失败——种入 `Secure Preferences` 的 `extensions.settings` 条目在启动时被完整性校验**直接清除**（macOS 同样强制 tracked-prefs 校验，历史资料"HMAC 仅 Windows 强制"已过时）。品牌版的免 flag 安装只剩机制 2（策略）。而 **Chrome for Testing 构建的 `--load-extension` 仍可用**（151 实测 ✓，Debian Chromium 亦可用），沙箱二进制首选 CfT 可完全绕开安装问题。

| 机制 | 适用 slot | 说明 | 验证状态 |
|------|-----------|------|----------|
| `--load-extension` + **旧版 CfT / Debian Chromium** | server 拉起的沙箱（短期） | 版本锁定（CfT ≤151 或 Debian Chromium）；**CfT 157 起已被移除，不可依赖** | ✅ CfT 151、Debian Chromium 156 实测可用；❌ CfT 157 |
| Preferences 种入 | ~~全部~~ 不可行 | macOS 品牌 Chrome 157 强制清除未签名 MAC 的种入条目；计算合法 MAC 需逆向 tracked-prefs 哈希机制，不值得 | ❌ 品牌 Chrome 157 实测条目被清除 |
| **CRX + 策略安装** | **长期主机制** | 固定 PEM 打包 CRX → 稳定扩展 ID；bridge-server 托管 CRX + update manifest（复用 9226）；容器内放 `/etc/opt/chrome/policies/managed/*.json` | 未测（策略会波及同 OS 用户全部 Chrome profile，影响日常浏览器，需单独评估） |
| 手动 load unpacked | daily | 维持现状 | ✅ 现状即工作 |

## 9. sandbox-gui 容器镜像

- 内容：Linux Chrome（品牌版 Linux 构建，指纹比 Chromium 主流；版本锁定）+ Xvfb + openbox（极小 WM，保证焦点行为正常）+ **noVNC** + 策略 JSON。
- **noVNC 用途**：人工登录、验证码、过盾的接管入口。用户在浏览器里开 `http://127.0.0.1:<port>` 看到沙箱桌面亲手操作一次，登录态留在沙箱 profile，之后全程 agent 自动。
- **安全**：VNC/noVNC 端口只绑 `127.0.0.1`，按 profile 动态分配端口，不暴露局域网。
- **网络**：扩展经 `ws://host.docker.internal:9225` 回连 hub；`proxy` 配置在容器网络层生效（per-profile 独立出口）。
- **底座**：macOS 用 colima（比 Docker Desktop 轻），底座 VM ~1-2GB + 每实例 0.5-1GB。
- **可迁移性**：协议即 WebSocket，slot 可整体挪到局域网 Linux 主机，架构零改动。

## 10. Profile / 登录态模型

- **一个沙箱一个身份**：user-data-dir 即身份，备份/迁移/删除都是目录操作。
- **登录态来源**：首选经 noVNC 人工登录一次（每个 profile 一次性成本）；**不做** daily profile 克隆（指纹变化会触发 Google 等重新验证，收益低）。
- **1 进程多原生 profile**（同 user-data-dir 多 `--profile-directory`）暂不做；若未来需要"同进程多身份并发在线"，对 hub 只是"一个 slot 多条连接"，协议不变，随时可加。

## 11. 分阶段路线

| 阶段 | 内容 | 依赖 |
|------|------|------|
| 1 | hub 多浏览器：路由表、per-slot 配额与窗口锁、`params.browser_id`、MCP/CLI `browser` 参数、协议文档更新 | 无 |
| 2 | server 生命周期管理 + `sandbox-headless` 打通端到端（用 headless 验证 spawn/认领/种入机制最便宜） | 阶段 1 |
| 3 | `sandbox-gui` 容器镜像（Chrome + Xvfb + openbox + noVNC + 策略装扩展）成为默认槽位 | 阶段 1、2 的生命周期框架；镜像构建 |
| 持续 | `daily` 槽位维持现状可用（手动装扩展 + 礼仪保留），无需开发 | — |

## 12. 动代码前的验证清单（2026-11-13 已执行，详见 §14）

1. **headless 扩展兼容性** ✅：CfT 151 `--headless=new` 下 MV3 service worker + WebSocket 存活、`captureVisibleTab` 可用（导航后立即调用有 undefined 瞬态，扩展端需重试）；`userScripts.execute` ❌（"允许用户脚本"开关需 UI，pref 种入无效）。
2. **Preferences 种入** ✅ 结论为不可行：品牌版 macOS Chrome 157 强制清除种入条目 → 沙箱二进制改用 CfT（`--load-extension` 可用），品牌版兜底方案只剩 CRX+策略。
3. **降速 flag** ⏳ 部分：flag 集已进入沙箱启动参数与容器镜像，但"后台不加载"页面的专项验证未做（阶段 2 实现时补）。
4. **反爬对照实验** ✅：headless UA 暴露 `HeadlessChrome`；容器有头 UA/WebDriver/Phantom 全过，WebGL 经 SwiftShader flag 修复可用（renderer 字符串暴露为已知残余）。
5. **noVNC 流程** ⏳ 未验证：可行性镜像未含 noVNC（x11vnc + websockify + noVNC 属成熟方案），人工登录全流程留待阶段 3 实现时验证。

## 13. 开放问题 / 风险

- **Preferences 版本漂移**：Chrome 改 `extensions.settings` 结构则机制 1 失效 → 自检 + CRX 策略兜底（已列入 D6）。
- **SwiftShader WebGL**：无 GPU 容器的 WebGL 渲染器可被顶级风控检测 → 这类站点归 `daily` 兜底（分工即设计意图）。
- **daily 若未来改策略安装**：平台策略会波及日常 Chrome 全部 profile（"由组织管理"提示）→ 维持手动装即可，不迁移。
- **二进制选型**：已验证——短期锁 CfT 151 或 Debian Chromium（`--load-extension` 可用），长期机制为 CRX+策略安装（§14 CfT 157 实测 flag 已移除）。

## 14. 可行性验证记录（2026-11-13，分支 `sandbox-feasibility`）

环境：macOS arm64 / 品牌 Chrome 157.0.8103.51 / Chrome for Testing 151.0.7922.34 / Apple container 1.1.0 / 隔离 server `BRIDGE_PORT=9325 BRIDGE_BIND=0.0.0.0`（不触碰日常 9225 环境，扩展产物拷贝后补丁 WS 地址）。

### 分支上的代码改动

- `server/src/main.rs`：WS 监听新增 `BRIDGE_BIND` 环境变量（默认仍 127.0.0.1，容器回连宿主机需要）。
- `client/src/main.rs:783`：修复 `screenshot --out` 摘要打印的 serde_json Map index panic（改用 `insert()`），文件落盘本身一直是成功的。
- `scripts/feasibility/`：`seed_profile.mjs`（扩展拷贝 + WS 地址补丁 + unpacked ID 计算 + Secure Preferences 种入）、`container/{Dockerfile,entrypoint.sh}`（Debian + Chromium + Xvfb + openbox 有头沙箱镜像）。

### V0：--load-extension 可用性

| 二进制 | 结果 |
|--------|------|
| 品牌 Chrome 157（macOS） | ❌ flag 被忽略，扩展未加载（10s 无注册） |
| Chrome for Testing 151 | ✅ `[hub] #N registered as Extension`，链路通 |
| Debian Chromium（容器内） | 详见 V4 |

### V1：Preferences 种入（品牌 Chrome 157）

1. 种入 `Preferences` → 无效：macOS 上扩展设置实际存放在 **`Secure Preferences`**，`Preferences` 里的条目启动时被剥离。
2. 种入 `Secure Preferences` → **条目被完整性校验直接清除**。unpacked ID 算法（SHA256(路径) 前 16 字节 nibble→a-p）已用 CfT 真实安装条目验证正确（`kbplmnofcaklimedkpfpjmjhcphpijlp`），问题出在校验而非 ID。
3. 结论：品牌版的 Preferences 种入不可行（反劫持加固），免 flag 只剩 CRX+策略；沙箱走 CfT + `--load-extension` 绕开。

### V2：headless（CfT 151 `--headless=new`）能力清单

| 能力 | 结果 |
|------|------|
| navigate / get-page-content / get-page-markdown 域 | ✅ |
| get-a11y-tree | ✅（50 节点） |
| scrape | ✅（sannysoft 134 个 td） |
| screenshot（captureVisibleTab） | ✅ 可用；**导航后立即调用曾出现 `data` 缺失的瞬态**（返回 undefined→JSON 丢键→client panic 报错），扩展端需加 undefined 重试/等待 |
| run_script | ❌ `chrome.userScripts` 不可用——"允许用户脚本"开关需要 UI，pref 种入（`extensions.user_scripts.enabled` / per-ext `user_scripts_enabled`）均无效。**依赖 JS 注入的配方（如 googletrends.js）在 headless 不可用**；正式解法：sandbox-gui 里经 noVNC 人工开一次开关 |

### 反爬对照（bot.sannysoft.com，headless 侧）

headless 截图实证（`scripts/feasibility/evidence/sannysoft-headless.png`）：UA = `HeadlessChrome/151.0.0.0`（红行暴露），WebDriver 新检测 passed（新 headless 的进步）。→ **headless 的 UA 指纹缺口依旧存在，`sandbox-gui` 作为默认槽位的决策成立**；headless 槽位定位为"纯采集快路径"（无 run_script、指纹暴露）。

### V4：Apple container 有头对照 ✅

镜像：Debian bookworm + Debian Chromium 156 + Xvfb + openbox（`scripts/feasibility/container/`），扩展经 `--load-extension` 装载（Debian 非品牌构建可用），WS 回连 `ws://192.168.64.1:9325`（宿主机在 vmnet 的地址；server 需 `BRIDGE_BIND=0.0.0.0`）。全链路（容器 → vmnet → hub → 指令 → 截图回传）实测通过。

bot.sannysoft.com 对照结论：

| 检测项 | headless (CfT 151) | 容器有头 (Chromium 156 + Xvfb) |
|--------|--------------------|-------------------------------|
| User Agent | ❌ `HeadlessChrome/151`（结构性暴露） | ✅ `Chrome/156.0.0.0` 正常 |
| WebDriver / Phantom 系列 | 部分通过 | ✅ 全部通过 |
| WebGL | context 创建失败 | ✅ 可用（flag：`--enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader`） |
| WebGL Renderer 指纹 | — | ⚠️ 暴露 `SwiftShader` 字符串（软件渲染可检测；功能无损。顶级风控场景归 `daily` 兜底，或后续评估 WebGL renderer 注入伪装） |

> 截图证据：`scripts/feasibility/evidence/`（`sannysoft-headless.png` = headless UA 暴露；`sannysoft-headed-webgl-off.png` = 未开 SwiftShader 时 WebGL 红；`sannysoft-headed-final.png` = 最终 flag 集下的通过状态）。

**结论：`sandbox-gui` 作为默认槽位在反爬维度成立**；`sandbox-headless` 定位为纯采集快路径（UA 暴露 + run_script 不可用）。

### CfT 157：--load-extension 跨版本验证 ❌

CfT 157.0.8103.78（与品牌版同版本号）实测：headless 与 **headed 均不加载**——`--load-extension` 已在 CfT 新版本中被移除（可用边界在 152~157 之间，151 实测可用）。非品牌 FOSS 构建不受影响：Debian Chromium 156 实测仍接受该 flag。

**策略结论**：
- 短期：沙箱二进制锁定 **CfT 151**（已验证）或 **Debian Chromium**（已验证，156 仍可用），版本锁定 + 启动自检（扩展连上 hub 即成功）。
- 长期：`--load-extension` 是死路（品牌版与 CfT 新版都已移除），**CRX + `ExtensionInstallForcelist` 策略安装是唯一可持续机制**，优先级上调——阶段 2/3 实现时应直接落地策略安装（bridge-server 托管 CRX + update manifest），flag 加载只作为旧版本的兼容路径。
