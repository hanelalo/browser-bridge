//! Browser Bridge MCP server：stdio 传输，把浏览器指令暴露为 MCP tools。
//! 通过 bridge-core 复用协议传输（含自动拉起 server、断线重连）。

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use bridge_core::recipes::googlesearch::googlesearch;
use bridge_core::recipes::googletrends::googletrends;
use bridge_core::recipes::googletrends::googletrends_compare;
use bridge_core::recipes::querydomains::querydomains;
use bridge_core::recipes::redditsearch::redditsearch;
use bridge_core::recipes::youtubeinfo::youtubeinfo;
use bridge_core::recipes::youtuberinfo::youtuberinfo;
use bridge_core::recipes::youtubesearch::youtubesearch;
use bridge_core::target;
use bridge_core::transport::{connect_bridge_no_spawn, Bridge};
use futures_util::StreamExt;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::tool_handler;
use rmcp::{ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const DEFAULT_SERVER: &str = "ws://127.0.0.1:9225";
const NO_EXTENSION_ERR: &str = "no extension connected";
/// 空闲多久关闭本进程拉起的 Chrome（可被 BRIDGE_CLOSE_CHROME_IDLE_SECS 覆盖，便于测试）
const IDLE_CLOSE_CHROME_SECS: u64 = 600;
const WATCHDOG_TICK_SECS: u64 = 30;

/// 本进程是否拉起了 Chrome（只在启动前 Chrome 未运行时置 true，之后只关自己拉起的）。
static LAUNCHED_CHROME: AtomicBool = AtomicBool::new(false);
static LAST_ACTIVITY: OnceLock<StdMutex<Instant>> = OnceLock::new();

fn last_activity() -> &'static StdMutex<Instant> {
    LAST_ACTIVITY.get_or_init(|| StdMutex::new(Instant::now()))
}

#[derive(Clone)]
struct BridgeMcp {
    bridge: Arc<Mutex<Bridge>>,
    client_id: String,
}

// ---------- 参数结构 ----------

#[derive(Serialize, Deserialize, JsonSchema)]
struct TabParams {
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NavigateParams {
    url: String,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct TargetParams {
    /// 定位值：CSS selector / 可见文本 / XPath
    target: String,
    /// 定位方式：css | text | xpath（默认 css）
    #[serde(default)]
    by: Option<String>,
    /// 第几个匹配（从 0 开始）
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ClickParams {
    target: String,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    /// 等待元素出现的最长时间（毫秒，默认 5000）
    #[serde(default)]
    timeout: Option<u64>,
    /// 锚点链接在新标签页打开（默认当前标签页打开，避免流程开 tab 堆积）
    #[serde(default)]
    new_tab: Option<bool>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ClickAtParams {
    x: f64,
    y: f64,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct PressKeyParams {
    /// KeyboardEvent.key 值（如 Enter、Escape、a、F5）
    key: String,
    #[serde(default)]
    modifiers: Option<Vec<String>>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    /// 按键后等待页面加载完成
    #[serde(default)]
    wait_load: Option<bool>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ScrollParams {
    #[serde(default)]
    dx: Option<f64>,
    #[serde(default)]
    dy: Option<f64>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    smooth: Option<bool>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetValueParams {
    target: String,
    value: String,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CheckParams {
    target: String,
    /// 是否勾选（默认 true；false 表示取消）
    #[serde(default)]
    checked: Option<bool>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct SelectOptionParams {
    target: String,
    /// 按 option 的 value 匹配
    #[serde(default)]
    option_value: Option<String>,
    /// 按 option 的显示文本匹配
    #[serde(default)]
    option_text: Option<String>,
    /// 按 option 下标匹配
    #[serde(default)]
    option_index: Option<usize>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ScrapeParams {
    /// 每条结果的容器选择器
    item: String,
    /// 字段映射：{ 字段名: "选择器[@属性]" }
    #[serde(default)]
    fields: Option<HashMap<String, String>>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct RunScriptParams {
    code: String,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NewTabParams {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct GooglesearchParams {
    query: String,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct RedditsearchParams {
    query: String,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct YoutubesearchParams {
    query: String,
    /// 上传日期筛选（默认 any；可选 today / week / month / year）
    #[serde(default)]
    time: Option<String>,
    /// 优先顺序（默认 relevance；可选 popularity = 热门程度）
    #[serde(default)]
    sort: Option<String>,
    /// 最多返回的结果数（默认 5）
    #[serde(default)]
    max: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct YoutubeinfoParams {
    /// 视频 URL 或 11 位视频 ID（watch?v= / youtu.be / shorts / embed / live 均可）
    url: String,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct YoutuberinfoParams {
    /// 频道 URL（如 https://www.youtube.com/@handle/videos）或 handle（如 @handle）
    channel: String,
    /// 最多返回的视频条数（默认 10；至少 1）
    #[serde(default)]
    max: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct GoogletrendsParams {
    query: String,
    /// 时间范围（默认 today 1-m；如 today 3-m / today 12-m / today 5-y / all）
    #[serde(default)]
    date: Option<String>,
    /// 地区（默认 Worldwide）
    #[serde(default)]
    geo: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct GoogletrendsCompareParams {
    /// 要对比的关键词列表（2 个及以上效果最好）
    terms: Vec<String>,
    /// 时间范围（默认 today 1-m；如 today 3-m / today 12-m / today 5-y / all）
    #[serde(default)]
    date: Option<String>,
    /// 地区（默认 Worldwide）
    #[serde(default)]
    geo: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct QuerydomainsParams {
    /// 域名关键词（如 browserbridge）
    query: String,
    /// 要检查的 TLD 列表（默认 com,ai,org,net,cn,info,app,io,xyz,co,run,me,pro,top；最多 20 个）
    #[serde(default)]
    tlds: Option<Vec<String>>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct GetPageMarkdownParams {
    /// 可选：先导航到该 URL 再转换（省略则用当前/指定标签页）
    #[serde(default)]
    url: Option<String>,
    /// 可选：只转换匹配选择器的容器（如 article / #content）
    #[serde(default)]
    selector: Option<String>,
    /// 跳过正文自动提取，转换整个页面
    #[serde(default)]
    full: Option<bool>,
    #[serde(default)]
    tab_id: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct A11yTreeParams {
    /// 是否包含隐藏元素（hidden / display:none / visibility:hidden / aria-hidden，默认 false）
    #[serde(default)]
    include_hidden: Option<bool>,
    /// 最多返回节点数（默认 500，范围 10-5000）
    #[serde(default)]
    max_nodes: Option<usize>,
    #[serde(default)]
    tab_id: Option<i32>,
}

// ---------- 工具辅助 ----------

fn ok(value: Value) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

async fn call(
    bridge: &Arc<Mutex<Bridge>>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<CallToolResult, ErrorData> {
    *last_activity().lock().unwrap() = Instant::now();
    let mut b = bridge.lock().await;
    match b.request(id, method, params.clone()).await {
        Ok(v) => return ok(v),
        Err(e) if e.contains(NO_EXTENSION_ERR) => {
            // Chrome 没在运行（或扩展没连上）：拉起默认 Chrome，等扩展注册后重试
            eprintln!("[bridge-mcp] no extension connected, ensuring Chrome is running");
            if let Err(launch_err) = ensure_chrome_running() {
                return Err(ErrorData::internal_error(launch_err, None));
            }
        }
        Err(e) => return Err(ErrorData::internal_error(e, None)),
    }
    // 扩展连接 server 有 500ms→5s 退避重连；Chrome 冷启动可能较慢，留足 ~30s
    let delays_ms: [u64; 8] = [1500, 2000, 3000, 4000, 5000, 5000, 5000, 5000];
    for (attempt, delay) in delays_ms.iter().enumerate() {
        eprintln!("[bridge-mcp] waiting {delay}ms for extension (attempt {})", attempt + 1);
        tokio::time::sleep(Duration::from_millis(*delay)).await;
        match b.request(id, method, params.clone()).await {
            Ok(v) => return ok(v),
            Err(e) if e.contains(NO_EXTENSION_ERR) => continue,
            Err(e) => return Err(ErrorData::internal_error(e, None)),
        }
    }
    Err(ErrorData::internal_error(
        "no extension connected：已尝试拉起 Chrome，请确认浏览器里加载了 Browser Bridge 扩展",
        None,
    ))
}

/// 检查 Chrome 是否已在运行。
#[cfg(target_os = "macos")]
fn chrome_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Google Chrome"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn chrome_running() -> bool {
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]
        .iter()
        .any(|name| {
            Command::new("pgrep")
                .args(["-x", name])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
}

/// 确保 Chrome 在运行：没运行就拉起默认 Chrome（共享用户默认 profile，不另开隔离实例）。
#[cfg(target_os = "macos")]
fn ensure_chrome_running() -> Result<(), String> {
    if chrome_running() {
        eprintln!("[bridge-mcp] Chrome already running");
        return Ok(());
    }
    eprintln!("[bridge-mcp] launching Chrome via open -a");
    let status = Command::new("open")
        .args(["-a", "Google Chrome"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("拉起 Chrome 失败: {e}"))?;
    if status.success() {
        eprintln!("[bridge-mcp] open returned success");
        LAUNCHED_CHROME.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        eprintln!("[bridge-mcp] open returned failure");
        Err("拉起 Chrome 失败：未找到 Google Chrome".to_string())
    }
}

#[cfg(not(target_os = "macos"))]
fn ensure_chrome_running() -> Result<(), String> {
    if chrome_running() {
        return Ok(());
    }
    for bin in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"] {
        if Command::new(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            LAUNCHED_CHROME.store(true, Ordering::SeqCst);
            return Ok(());
        }
    }
    Err("拉起 Chrome 失败：未在 PATH 中找到 chrome/chromium".to_string())
}

/// 优雅退出 Chrome（会话保留，下次启动恢复）。只在确认是本进程拉起的 Chrome 时调用。
#[cfg(target_os = "macos")]
fn quit_chrome() {
    let _ = Command::new("osascript")
        .args(["-e", "quit app \"Google Chrome\""])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(target_os = "macos"))]
fn quit_chrome() {
    for bin in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"] {
        let _ = Command::new("pkill").args(["-x", bin]).status();
    }
}

fn with_target(
    target_value: &str,
    by: Option<&str>,
    index: Option<usize>,
    tab_id: Option<i32>,
) -> Value {
    json!({
        "target": target::spec(by.unwrap_or("css"), target_value, index),
        "tab_id": tab_id,
    })
}

// ---------- MCP tools ----------

#[tool_router]
impl BridgeMcp {
    #[tool(name = "list_tabs", description = "列出所有标签页")]
    pub async fn list_tabs(&self) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "lt", "list_tabs", json!({})).await
    }

    #[tool(name = "close_tab", description = "关闭标签页（默认当前激活页）")]
    pub async fn close_tab(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "ct", "close_tab", json!({ "tab_id": params.0.tab_id })).await
    }

    #[tool(name = "close_auto_tabs", description = "关闭本会话（当前 MCP 进程）自动打开的标签页（new_tab / click --new-tab / googletrends 创建的），不影响其他会话。流程结束时请务必调用本工具，清理本次任务创建的标签页，避免浏览器堆积")]
    pub async fn close_auto_tabs(&self) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "cat",
            "close_auto_tabs",
            json!({ "owner": self.client_id }),
        )
        .await
    }

    #[tool(name = "close_agent_window", description = "关闭本会话（当前 MCP 进程）的专用浏览器窗口（连同窗口内所有标签页），释放资源。任务执行完毕后请调用本工具关闭自己的窗口，避免窗口堆积；不影响其他会话的窗口")]
    pub async fn close_agent_window(&self) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "caw",
            "close_agent_window",
            json!({ "owner": self.client_id }),
        )
        .await
    }

    #[tool(name = "new_tab", description = "新建标签页（可选打开 URL）")]
    pub async fn new_tab(
        &self,
        params: Parameters<NewTabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "nt", "new_tab", json!({ "url": params.0.url })).await
    }

    #[tool(name = "activate_tab", description = "切换到指定标签页并聚焦窗口")]
    pub async fn activate_tab(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "at", "activate_tab", json!({ "tab_id": params.0.tab_id })).await
    }

    #[tool(name = "navigate", description = "导航到指定 URL 并等待加载完成")]
    pub async fn navigate(
        &self,
        params: Parameters<NavigateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(&self.bridge, "nav", "navigate", json!({ "url": p.url, "tab_id": p.tab_id })).await
    }

    #[tool(name = "click", description = "点击匹配定位的元素")]
    pub async fn click(
        &self,
        params: Parameters<ClickParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id);
        if let Some(t) = p.timeout {
            v["timeout"] = json!(t);
        }
        if p.new_tab.unwrap_or(false) {
            v["new_tab"] = json!(true);
        }
        call(&self.bridge, "clk", "click", v).await
    }

    #[tool(name = "click_at", description = "按页面坐标点击")]
    pub async fn click_at(
        &self,
        params: Parameters<ClickAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(
            &self.bridge,
            "cxy",
            "click_at",
            json!({ "x": p.x, "y": p.y, "tab_id": p.tab_id }),
        )
        .await
    }

    #[tool(name = "press_key", description = "模拟按键（Enter/Escape/a/F5 等，支持修饰键）")]
    pub async fn press_key(
        &self,
        params: Parameters<PressKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = json!({ "key": p.key, "tab_id": p.tab_id });
        if let Some(m) = p.modifiers {
            v["modifiers"] = json!(m);
        }
        if p.wait_load.unwrap_or(false) {
            v["wait_load"] = json!(true);
        }
        if let Some(t) = p.target {
            v["target"] = target::spec(p.by.as_deref().unwrap_or("css"), &t, p.index);
        }
        call(&self.bridge, "pk", "press_key", v).await
    }

    #[tool(name = "scroll", description = "滚动窗口或指定容器")]
    pub async fn scroll(
        &self,
        params: Parameters<ScrollParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = json!({
            "dx": p.dx.unwrap_or(0.0),
            "dy": p.dy.unwrap_or(0.0),
            "smooth": p.smooth.unwrap_or(false),
            "tab_id": p.tab_id,
        });
        if let Some(t) = p.target {
            v["target"] = target::spec(p.by.as_deref().unwrap_or("css"), &t, p.index);
        }
        call(&self.bridge, "scr", "scroll", v).await
    }

    #[tool(name = "set_value", description = "设置 input/textarea/contenteditable 的值")]
    pub async fn set_value(
        &self,
        params: Parameters<SetValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id);
        v["value"] = json!(p.value);
        call(&self.bridge, "sv", "set_value", v).await
    }

    #[tool(name = "check", description = "勾选/取消 checkbox 或 radio")]
    pub async fn check(
        &self,
        params: Parameters<CheckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id);
        v["checked"] = json!(p.checked.unwrap_or(true));
        call(&self.bridge, "chk", "check", v).await
    }

    #[tool(name = "select_option", description = "选中 <select> 的某个选项")]
    pub async fn select_option(
        &self,
        params: Parameters<SelectOptionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id);
        if let Some(x) = p.option_value {
            v["value"] = json!(x);
        }
        if let Some(x) = p.option_text {
            v["text"] = json!(x);
        }
        if let Some(x) = p.option_index {
            v["index"] = json!(x);
        }
        call(&self.bridge, "so", "select_option", v).await
    }

    #[tool(name = "clear", description = "清空 input/textarea/contenteditable")]
    pub async fn clear(
        &self,
        params: Parameters<TargetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(
            &self.bridge,
            "clr",
            "clear",
            with_target(&p.target, p.by.as_deref(), p.index, p.tab_id),
        )
        .await
    }

    #[tool(name = "get_value", description = "读取元素当前值（用于验证）")]
    pub async fn get_value(
        &self,
        params: Parameters<TargetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(
            &self.bridge,
            "gv",
            "get_value",
            with_target(&p.target, p.by.as_deref(), p.index, p.tab_id),
        )
        .await
    }

    #[tool(name = "scrape", description = "按 CSS 选择器提取结构化数据（字段映射：字段名:选择器[@属性]）")]
    pub async fn scrape(
        &self,
        params: Parameters<ScrapeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = json!({ "item": p.item, "tab_id": p.tab_id });
        if let Some(f) = p.fields {
            v["fields"] =
                serde_json::to_value(&f).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }
        if let Some(t) = p.title {
            v["title"] = json!(t);
        }
        if let Some(l) = p.link {
            v["link"] = json!(l);
        }
        if let Some(d) = p.desc {
            v["desc"] = json!(d);
        }
        if let Some(t) = p.timeout {
            v["timeout"] = json!(t);
        }
        call(&self.bridge, "scp", "scrape", v).await
    }

    #[tool(name = "run_script", description = "在页面里执行任意 JS 表达式，返回 JSON 序列化结果")]
    pub async fn run_script(
        &self,
        params: Parameters<RunScriptParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(&self.bridge, "rs", "run_script", json!({ "code": p.code, "tab_id": p.tab_id })).await
    }

    #[tool(name = "get_page_content", description = "读取页面标题/URL/文本")]
    pub async fn get_page_content(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "gpc", "get_page_content", json!({ "tab_id": params.0.tab_id })).await
    }

    #[tool(name = "get_page_markdown", description = "把指定页面内容转换为标准 Markdown（标题/段落/列表/表格/代码块/链接/图片），返回 { tab_id, title, url, markdown }。默认用 Readability 自动提取正文去除导航/页脚等噪音，可传 url 先导航；selector 只转换某个容器（如 article / #content）；full=true 时跳过提取转换整个页面")]
    pub async fn get_page_markdown(
        &self,
        params: Parameters<GetPageMarkdownParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = json!({ "tab_id": p.tab_id });
        if let Some(u) = p.url {
            v["url"] = json!(u);
        }
        if let Some(s) = p.selector {
            v["selector"] = json!(s);
        }
        if p.full.unwrap_or(false) {
            v["full"] = json!(true);
        }
        call(&self.bridge, "gpm", "get_page_markdown", v).await
    }

    #[tool(name = "get_a11y_tree", description = "读取页面 a11y tree（无障碍树），返回 { tab_id, title, url, count, nodes[] }。每个节点含 role（角色）/ name（可访问名称）/ value（当前值）/ states（状态：enabled/disabled/checked/unchecked/expanded/collapsed/required/readonly 等）/ depth（DOM 深度）/ tag；heading 额外带 level。可交互节点（button/link/textbox/searchbox/checkbox/radio/combobox/listbox/slider/select 等）带 target，可直接喂给 click / set_value / check / select_option / clear / get_value。适合与页面交互前先了解页面结构、找出可点击或可填写的元素")]
    pub async fn get_a11y_tree(
        &self,
        params: Parameters<A11yTreeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut v = json!({ "tab_id": p.tab_id, "max_nodes": p.max_nodes.unwrap_or(500) });
        if p.include_hidden.unwrap_or(false) {
            v["include_hidden"] = json!(true);
        }
        call(&self.bridge, "a11y", "get_a11y_tree", v).await
    }

    #[tool(name = "googlesearch", description = "Google 搜索，返回 { tab_id, results[] }（title/description/url/target）")]
    pub async fn googlesearch_tool(
        &self,
        params: Parameters<GooglesearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = googlesearch(&mut bridge, &p.query, p.tab_id)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "redditsearch", description = "Reddit 搜索，返回 { tab_id, results[] }（title/description/published/published_at/votes/comments/url/target；votes/comments 为整数 upvote/评论数量，published_at 为 ISO 时间戳）")]
    pub async fn redditsearch_tool(
        &self,
        params: Parameters<RedditsearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = redditsearch(&mut bridge, &p.query, p.tab_id)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "youtubesearch", description = "YouTube 搜索，返回 { tab_id, results[] }（title/channel/views/published/duration/url/target），支持上传日期（today/week/month/year）与优先顺序（relevance/popularity）筛选")]
    pub async fn youtubesearch_tool(
        &self,
        params: Parameters<YoutubesearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = youtubesearch(
            &mut bridge,
            &p.query,
            p.time.as_deref().unwrap_or("any"),
            p.sort.as_deref().unwrap_or("relevance"),
            p.max.unwrap_or(5),
            p.tab_id,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "youtubeinfo", description = "获取指定 YouTube 视频详情，返回 { tab_id, video }（url/title/author/author_url/duration/duration_seconds/like_count/comment_count/subscriber_count/captions[]，字幕为全文，各计数同时附 *_text 原始文本）")]
    pub async fn youtubeinfo_tool(
        &self,
        params: Parameters<YoutubeinfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = youtubeinfo(&mut bridge, &p.url, p.tab_id)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "youtuberinfo", description = "获取指定 YouTube 频道（youtuber）的视频列表，返回 { tab_id, channel, videos[] }（channel 含 name/url/subscriber_count/subscriber_count_text；videos 每项含 title/url/views/views_count/duration/published/target），最多返回 max 条（默认 10，至少 1）")]
    pub async fn youtuberinfo_tool(
        &self,
        params: Parameters<YoutuberinfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = youtuberinfo(&mut bridge, &p.channel, p.max.unwrap_or(10), p.tab_id)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "googletrends", description = "Google Trends 趋势查询，返回 { tab_id, trend[], top[], rising[], regions[] }；regions 为按地区的搜索热度（rank/region/geo_code/interest）")]
    pub async fn googletrends_tool(
        &self,
        params: Parameters<GoogletrendsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = googletrends(
            &mut bridge,
            &p.query,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "googletrends_compare", description = "Google Trends 关键词对比，返回 { series[] }，每个词一条趋势序列（共享 0-100 刻度，不返回热门/上升表）")]
    pub async fn googletrends_compare_tool(
        &self,
        params: Parameters<GoogletrendsCompareParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = googletrends_compare(
            &mut bridge,
            &p.terms,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }

    #[tool(name = "querydomains", description = "用 Query.Domains 按关键词批量查询域名注册情况与价格，返回 { tab_id, query, tlds, complete, results[] }（每项含 domain/tld/status/available/price/badges；status: available|unavailable|uncertain）")]
    pub async fn querydomains_tool(
        &self,
        params: Parameters<QuerydomainsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let mut bridge = self.bridge.lock().await;
        let out = querydomains(
            &mut bridge,
            &p.query,
            p.tlds.as_deref().unwrap_or(&[]),
            p.tab_id,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
        ok(out)
    }
}

#[tool_handler(router = Self::tool_router())]
impl rmcp::ServerHandler for BridgeMcp {
    /// opencode 1.18.x 的 MCP 客户端对 2026-07-28 协议（server/discover + resultType
    /// 包装）与 rmcp 3.1.0 的实现不兼容：探测成功后跳过 initialize 直接发 tools/list，
    /// 响应解析失败导致 "Failed to get tools"。
    /// 这里只声明到 2025-11-25，让客户端回退到经典的 initialize 握手流程。
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        use rmcp::model::ProtocolVersion;
        std::borrow::Cow::Borrowed(&[
            ProtocolVersion::V_2024_11_05,
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
        ])
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("BRIDGE_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());
    // 每个 MCP 进程一个稳定身份（多 agent 共享浏览器时用于标签页归属隔离）
    let client_id = format!(
        "mcp-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let bridge = Bridge::connect_with_client_id(&server, &client_id).await?;
    eprintln!("bridge-mcp connected to {server}");
    let mcp = BridgeMcp {
        bridge: Arc::new(Mutex::new(bridge)),
        client_id,
    };

    // Chrome 生命周期管理：只关本进程拉起的 Chrome。
    // 1) server 断开（server 退出/被杀）：本进程拉起的 Chrome 一并退出
    // 2) 空闲超时（默认 10 分钟）：退出本进程拉起的 Chrome
    // 3) MCP 会话结束（stdio 关闭）：main 末尾清理
    {
        let server_watch = server.clone();
        let watch_client = format!("mcp-watch-{}", std::process::id());
        tokio::spawn(async move {
            loop {
                match connect_bridge_no_spawn(&server_watch, &watch_client).await {
                    Ok(mut ws) => {
                        // 一直读到断开：server 退出/被杀时流结束
                        while let Some(_msg) = ws.next().await {}
                        eprintln!("[bridge-mcp] bridge server closed");
                        if LAUNCHED_CHROME.swap(false, Ordering::SeqCst) {
                            eprintln!("[bridge-mcp] closing Chrome launched by this MCP process");
                            quit_chrome();
                        }
                    }
                    Err(_) => {
                        // server 没在运行，无需处理
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(WATCHDOG_TICK_SECS)).await;
                if !LAUNCHED_CHROME.load(Ordering::SeqCst) {
                    continue;
                }
                let idle_secs = std::env::var("BRIDGE_CLOSE_CHROME_IDLE_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(IDLE_CLOSE_CHROME_SECS);
                if last_activity().lock().unwrap().elapsed() >= Duration::from_secs(idle_secs) {
                    eprintln!("[bridge-mcp] idle {idle_secs}s, closing Chrome launched by this MCP process");
                    quit_chrome();
                    LAUNCHED_CHROME.store(false, Ordering::SeqCst);
                }
            }
        });
    }

    let service = mcp.serve(rmcp::transport::stdio()).await?;
    let waiting = service.waiting().await;
    // MCP 会话结束（stdio 关闭）：关掉本进程拉起的 Chrome
    if LAUNCHED_CHROME.swap(false, Ordering::SeqCst) {
        eprintln!("[bridge-mcp] MCP session ended, closing Chrome launched by this MCP process");
        quit_chrome();
    }
    waiting?;
    Ok(())
}
