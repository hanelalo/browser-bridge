//! Browser Bridge MCP server：stdio 传输，把浏览器指令暴露为 MCP tools。
//! 通过 bridge-core 复用协议传输（含自动拉起 server、断线重连）。

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use bridge_core::recipes::googlesearch::googlesearch;
use bridge_core::recipes::googletrends::googletrends;
use bridge_core::recipes::googletrends::googletrends_compare;
use bridge_core::recipes::oldgoogletrends::oldgoogletrends;
use bridge_core::recipes::oldgoogletrends::oldgoogletrends_compare;
use bridge_core::recipes::querydomains::querydomains;
use bridge_core::recipes::redditsearch::redditsearch;
use bridge_core::recipes::youtubeinfo::youtubeinfo;
use bridge_core::recipes::youtuberinfo::youtuberinfo;
use bridge_core::recipes::youtubesearch::youtubesearch;
use bridge_core::target;
use bridge_core::transport::Bridge;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::tool_handler;
use rmcp::{tool, tool_router, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_SERVER: &str = "ws://127.0.0.1:9225";
const NO_EXTENSION_ERR: &str = "no extension connected";
#[derive(Clone)]
struct BridgeMcp {
    bridge: Bridge,
    client_id: String,
}

// ---------- 参数结构 ----------

#[derive(Serialize, Deserialize, JsonSchema)]
struct TabParams {
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NavigateParams {
    url: String,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ClickAtParams {
    x: f64,
    y: f64,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct RunScriptParams {
    code: String,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NewTabParams {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct GooglesearchParams {
    query: String,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct RedditsearchParams {
    query: String,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct YoutubeinfoParams {
    /// 视频 URL 或 11 位视频 ID（watch?v= / youtu.be / shorts / embed / live 均可）
    url: String,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct OldgoogletrendsParams {
    /// 单个搜索关键词
    query: String,
    /// 时间范围（默认 today 1-m；支持 now 7-d / now 1-d / today 3-m / all / YYYY-MM-DD YYYY-MM-DD）
    #[serde(default)]
    date: Option<String>,
    /// 地区代码（默认 Worldwide，空字符串也表示全球；如 US / CN）
    #[serde(default)]
    geo: Option<String>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct OldgoogletrendsCompareParams {
    /// 要对比的关键词列表（2 个及以上效果最好，最多 5 个）
    terms: Vec<String>,
    /// 时间范围（默认 today 1-m；支持 now 7-d / now 1-d / today 3-m / all / YYYY-MM-DD YYYY-MM-DD）
    #[serde(default)]
    date: Option<String>,
    /// 地区代码（默认 Worldwide，空字符串也表示全球；如 US / CN）
    #[serde(default)]
    geo: Option<String>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
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
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ScreenshotParams {
    #[serde(default)]
    tab_id: Option<i32>,
    /// 图片格式：png | jpeg（默认 png）
    #[serde(default)]
    format: Option<String>,
    /// JPEG 质量 0-100（默认 90；仅 jpeg 有效）
    #[serde(default)]
    quality: Option<u8>,
    /// 先把目标窗口拉到前台再截图（避免被其他窗口遮挡时截到别的内容）
    #[serde(default)]
    foreground: Option<bool>,
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct WindowParams {
    window_id: i32,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct NewWindowParams {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    focused: Option<bool>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct TaskParams {
    task_id: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct BeginTaskParams {
    #[serde(default)]
    window_id: Option<i32>,
    #[serde(default)]
    tab_id: Option<i32>,
    #[serde(default)]
    agent_id: Option<String>,
}

// ---------- 工具辅助 ----------

fn ok(value: Value) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// 发送请求并返回原始 JSON 结果（不包装成 MCP 内容块），自动拉起 Chrome 并重试。
async fn call_value(
    bridge: &Bridge,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, ErrorData> {
    match bridge.request(id, method, params.clone()).await {
        Ok(v) => return Ok(v),
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
        eprintln!(
            "[bridge-mcp] waiting {delay}ms for extension (attempt {})",
            attempt + 1
        );
        tokio::time::sleep(Duration::from_millis(*delay)).await;
        match bridge.request(id, method, params.clone()).await {
            Ok(v) => return Ok(v),
            Err(e) if e.contains(NO_EXTENSION_ERR) => continue,
            Err(e) => return Err(ErrorData::internal_error(e, None)),
        }
    }
    Err(ErrorData::internal_error(
        "no extension connected：已尝试拉起 Chrome，请确认浏览器里加载了 Browser Bridge 扩展",
        None,
    ))
}

async fn call(
    bridge: &Bridge,
    id: &str,
    method: &str,
    params: Value,
) -> Result<CallToolResult, ErrorData> {
    let value = call_value(bridge, id, method, params).await?;
    ok(value)
}

async fn finish_recipe(
    bridge: &Bridge,
    out: Result<Value, String>,
    owns_task: bool,
) -> Result<CallToolResult, ErrorData> {
    let cleanup = if owns_task {
        bridge.finish_task().await.map(|_| ())
    } else {
        Ok(())
    };
    match (out, cleanup) {
        (Ok(value), Ok(())) => ok(value),
        (Err(error), _) => Err(ErrorData::internal_error(error, None)),
        (Ok(_), Err(error)) => Err(ErrorData::internal_error(
            format!("recipe completed but task cleanup failed: {error}"),
            None,
        )),
    }
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
    [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ]
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
    for bin in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        if Command::new(bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("拉起 Chrome 失败：未在 PATH 中找到 chrome/chromium".to_string())
}

fn with_target(
    target_value: &str,
    by: Option<&str>,
    index: Option<usize>,
    tab_id: Option<i32>,
    window_id: Option<i32>,
) -> Value {
    let mut value = json!({
        "target": target::spec(by.unwrap_or("css"), target_value, index),
        "tab_id": tab_id,
    });
    if let Some(window_id) = window_id {
        value["window_id"] = json!(window_id);
    }
    value
}

// ---------- MCP tools ----------

#[tool_router]
impl BridgeMcp {
    #[tool(
        name = "begin_task",
        description = "申请窗口任务租约；等待 server 全局额度和窗口空闲后返回 task_id。后续工具传入该 task_id，结束时调用 end_task；空闲租约 60 秒后释放"
    )]
    pub async fn begin_task(
        &self,
        params: Parameters<BeginTaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "begin",
            "task_begin",
            serde_json::to_value(params.0)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        )
        .await
    }

    #[tool(
        name = "end_task",
        description = "结束任务；若浏览器仍在执行，进入 closing 状态，确认执行结束后自动释放额度和窗口"
    )]
    pub async fn end_task(
        &self,
        params: Parameters<TaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "end",
            "task_end",
            json!({"task_id":params.0.task_id}),
        )
        .await
    }

    #[tool(
        name = "cancel_task",
        description = "取消排队工作并停止接受新指令；在途浏览器操作确认结束后才释放窗口"
    )]
    pub async fn cancel_task(
        &self,
        params: Parameters<TaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "cancel",
            "task_cancel",
            json!({"task_id":params.0.task_id}),
        )
        .await
    }

    #[tool(
        name = "task_status",
        description = "查询任务的排队、运行、closing 或 finished 状态"
    )]
    pub async fn task_status(
        &self,
        params: Parameters<TaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "status",
            "task_status",
            json!({"task_id":params.0.task_id}),
        )
        .await
    }

    #[tool(name = "list_windows", description = "列出浏览器窗口及其标签页")]
    pub async fn list_windows(&self) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "lw", "list_windows", json!({})).await
    }

    #[tool(name = "new_window", description = "创建一个可供任务绑定的浏览器窗口")]
    pub async fn new_window(
        &self,
        params: Parameters<NewWindowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        call(
            &self.bridge,
            "nw",
            "new_window",
            json!({
                "url": p.url,
                "focused": p.focused.unwrap_or(false),
            }),
        )
        .await
    }

    #[tool(name = "close_window", description = "关闭指定浏览器窗口及其标签页")]
    pub async fn close_window(
        &self,
        params: Parameters<WindowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            Some(params.0.window_id),
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        call(
            &bridge,
            "cw",
            "close_window",
            json!({ "window_id": params.0.window_id }),
        )
        .await
    }

    #[tool(name = "list_tabs", description = "列出所有标签页")]
    pub async fn list_tabs(&self) -> Result<CallToolResult, ErrorData> {
        call(&self.bridge, "lt", "list_tabs", json!({})).await
    }

    #[tool(name = "close_tab", description = "关闭标签页（默认当前激活页）")]
    pub async fn close_tab(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        call(
            &bridge,
            "ct",
            "close_tab",
            json!({ "tab_id": params.0.tab_id, "window_id": params.0.window_id }),
        )
        .await
    }

    #[tool(
        name = "close_auto_tabs",
        description = "关闭本会话（当前 MCP 进程）自动打开的标签页（new_tab / click --new-tab / googletrends 创建的），不影响其他会话。流程结束时请务必调用本工具，清理本次任务创建的标签页，避免浏览器堆积"
    )]
    pub async fn close_auto_tabs(&self) -> Result<CallToolResult, ErrorData> {
        call(
            &self.bridge,
            "cat",
            "close_auto_tabs",
            json!({ "owner": self.client_id }),
        )
        .await
    }

    #[tool(
        name = "close_agent_window",
        description = "关闭本会话（当前 MCP 进程）的专用浏览器窗口（连同窗口内所有标签页），释放资源。任务执行完毕后请调用本工具关闭自己的窗口，避免窗口堆积；不影响其他会话的窗口"
    )]
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
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        call(
            &bridge,
            "nt",
            "new_tab",
            json!({ "url": params.0.url, "window_id": params.0.window_id }),
        )
        .await
    }

    #[tool(name = "activate_tab", description = "切换到指定标签页并聚焦窗口")]
    pub async fn activate_tab(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        call(
            &bridge,
            "at",
            "activate_tab",
            json!({ "tab_id": params.0.tab_id, "window_id": params.0.window_id }),
        )
        .await
    }

    #[tool(name = "navigate", description = "导航到指定 URL 并等待加载完成")]
    pub async fn navigate(
        &self,
        params: Parameters<NavigateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        call(
            &bridge,
            "nav",
            "navigate",
            json!({ "url": p.url, "tab_id": p.tab_id, "window_id": p.window_id }),
        )
        .await
    }

    #[tool(name = "click", description = "点击匹配定位的元素")]
    pub async fn click(
        &self,
        params: Parameters<ClickParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id);
        if let Some(t) = p.timeout {
            v["timeout"] = json!(t);
        }
        if p.new_tab.unwrap_or(false) {
            v["new_tab"] = json!(true);
        }
        call(&bridge, "clk", "click", v).await
    }

    #[tool(name = "click_at", description = "按页面坐标点击")]
    pub async fn click_at(
        &self,
        params: Parameters<ClickAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        call(
            &bridge,
            "cxy",
            "click_at",
            json!({ "x": p.x, "y": p.y, "tab_id": p.tab_id, "window_id": p.window_id }),
        )
        .await
    }

    #[tool(
        name = "press_key",
        description = "模拟按键（Enter/Escape/a/F5 等，支持修饰键）"
    )]
    pub async fn press_key(
        &self,
        params: Parameters<PressKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({ "key": p.key, "tab_id": p.tab_id, "window_id": p.window_id });
        if let Some(m) = p.modifiers {
            v["modifiers"] = json!(m);
        }
        if p.wait_load.unwrap_or(false) {
            v["wait_load"] = json!(true);
        }
        if let Some(t) = p.target {
            v["target"] = target::spec(p.by.as_deref().unwrap_or("css"), &t, p.index);
        }
        call(&bridge, "pk", "press_key", v).await
    }

    #[tool(name = "scroll", description = "滚动窗口或指定容器")]
    pub async fn scroll(
        &self,
        params: Parameters<ScrollParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({
            "dx": p.dx.unwrap_or(0.0),
            "dy": p.dy.unwrap_or(0.0),
            "smooth": p.smooth.unwrap_or(false),
            "tab_id": p.tab_id,
            "window_id": p.window_id,
        });
        if let Some(t) = p.target {
            v["target"] = target::spec(p.by.as_deref().unwrap_or("css"), &t, p.index);
        }
        call(&bridge, "scr", "scroll", v).await
    }

    #[tool(
        name = "set_value",
        description = "设置 input/textarea/contenteditable 的值"
    )]
    pub async fn set_value(
        &self,
        params: Parameters<SetValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id);
        v["value"] = json!(p.value);
        call(&bridge, "sv", "set_value", v).await
    }

    #[tool(name = "check", description = "勾选/取消 checkbox 或 radio")]
    pub async fn check(
        &self,
        params: Parameters<CheckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id);
        v["checked"] = json!(p.checked.unwrap_or(true));
        call(&bridge, "chk", "check", v).await
    }

    #[tool(name = "select_option", description = "选中 <select> 的某个选项")]
    pub async fn select_option(
        &self,
        params: Parameters<SelectOptionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id);
        if let Some(x) = p.option_value {
            v["value"] = json!(x);
        }
        if let Some(x) = p.option_text {
            v["text"] = json!(x);
        }
        if let Some(x) = p.option_index {
            v["index"] = json!(x);
        }
        call(&bridge, "so", "select_option", v).await
    }

    #[tool(name = "clear", description = "清空 input/textarea/contenteditable")]
    pub async fn clear(
        &self,
        params: Parameters<TargetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        call(
            &bridge,
            "clr",
            "clear",
            with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id),
        )
        .await
    }

    #[tool(name = "get_value", description = "读取元素当前值（用于验证）")]
    pub async fn get_value(
        &self,
        params: Parameters<TargetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        call(
            &bridge,
            "gv",
            "get_value",
            with_target(&p.target, p.by.as_deref(), p.index, p.tab_id, p.window_id),
        )
        .await
    }

    #[tool(
        name = "scrape",
        description = "按 CSS 选择器提取结构化数据（字段映射：字段名:选择器[@属性]）"
    )]
    pub async fn scrape(
        &self,
        params: Parameters<ScrapeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({ "item": p.item, "tab_id": p.tab_id, "window_id": p.window_id });
        if let Some(f) = p.fields {
            v["fields"] = serde_json::to_value(&f)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
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
        call(&bridge, "scp", "scrape", v).await
    }

    #[tool(
        name = "run_script",
        description = "在页面里执行任意 JS 表达式，返回 JSON 序列化结果"
    )]
    pub async fn run_script(
        &self,
        params: Parameters<RunScriptParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        call(
            &bridge,
            "rs",
            "run_script",
            json!({ "code": p.code, "tab_id": p.tab_id, "window_id": p.window_id }),
        )
        .await
    }

    #[tool(name = "get_page_content", description = "读取页面标题/URL/文本")]
    pub async fn get_page_content(
        &self,
        params: Parameters<TabParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        call(
            &bridge,
            "gpc",
            "get_page_content",
            json!({ "tab_id": params.0.tab_id, "window_id": params.0.window_id }),
        )
        .await
    }

    #[tool(
        name = "get_page_markdown",
        description = "把指定页面内容转换为标准 Markdown（标题/段落/列表/表格/代码块/链接/图片），返回 { tab_id, title, url, markdown }。默认用 Readability 自动提取正文去除导航/页脚等噪音，可传 url 先导航；selector 只转换某个容器（如 article / #content）；full=true 时跳过提取转换整个页面"
    )]
    pub async fn get_page_markdown(
        &self,
        params: Parameters<GetPageMarkdownParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({ "tab_id": p.tab_id, "window_id": p.window_id });
        if let Some(u) = p.url {
            v["url"] = json!(u);
        }
        if let Some(s) = p.selector {
            v["selector"] = json!(s);
        }
        if p.full.unwrap_or(false) {
            v["full"] = json!(true);
        }
        call(&bridge, "gpm", "get_page_markdown", v).await
    }

    #[tool(
        name = "get_a11y_tree",
        description = "读取页面 a11y tree（无障碍树），返回 { tab_id, title, url, count, nodes[] }。每个节点含 role（角色）/ name（可访问名称）/ value（当前值）/ states（状态：enabled/disabled/checked/unchecked/expanded/collapsed/required/readonly 等）/ depth（DOM 深度）/ tag；heading 额外带 level。可交互节点（button/link/textbox/searchbox/checkbox/radio/combobox/listbox/slider/select 等）带 target，可直接喂给 click / set_value / check / select_option / clear / get_value。适合与页面交互前先了解页面结构、找出可点击或可填写的元素"
    )]
    pub async fn get_a11y_tree(
        &self,
        params: Parameters<A11yTreeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({ "tab_id": p.tab_id, "window_id": p.window_id, "max_nodes": p.max_nodes.unwrap_or(500) });
        if p.include_hidden.unwrap_or(false) {
            v["include_hidden"] = json!(true);
        }
        call(&bridge, "a11y", "get_a11y_tree", v).await
    }

    #[tool(
        name = "screenshot",
        description = "截取页面可见区域截图，返回一个标准 MCP 图片块（agent 可直接查看，无需解码）+ 一个文本块（元信息 { tab_id, url, title, mime, format, width, height, size }）。窗口被其他应用完全遮挡时，截到的可能是遮挡内容，传 foreground=true 先把目标窗口拉到 OS 前台再截；format=jpeg 时可用 quality（0-100）控制质量"
    )]
    pub async fn screenshot(
        &self,
        params: Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let mut v = json!({ "tab_id": p.tab_id, "window_id": p.window_id });
        if let Some(f) = p.format {
            v["format"] = json!(f);
        }
        if let Some(q) = p.quality {
            v["quality"] = json!(q);
        }
        if p.foreground.unwrap_or(false) {
            v["foreground"] = json!(true);
        }
        let value = call_value(&bridge, "ss", "screenshot", v).await?;
        // 图片：从 data URL 中拆出纯 base64，作为标准 MCP image content block 返回
        let data_url = value
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorData::internal_error("screenshot 响应缺少 data 字段", None))?;
        let b64 = data_url
            .split_once(',')
            .map(|(_, b)| b.to_string())
            .filter(|b| !b.is_empty())
            .ok_or_else(|| {
                ErrorData::internal_error("screenshot data 不是合法的 data URL", None)
            })?;
        let mime = value
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string();
        // 元信息：去掉重复的 data 字段后作为文本块返回
        let mut meta = value.clone();
        if let Some(m) = meta.as_object_mut() {
            m.remove("data");
        }
        let meta_text = serde_json::to_string_pretty(&meta).unwrap_or_else(|_| "{}".to_string());
        Ok(CallToolResult::success(vec![
            ContentBlock::text(meta_text),
            ContentBlock::image(b64, mime),
        ]))
    }

    #[tool(
        name = "googlesearch",
        description = "Google 搜索，返回 { tab_id, results[] }（title/description/url/target）"
    )]
    pub async fn googlesearch_tool(
        &self,
        params: Parameters<GooglesearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = googlesearch(&mut bridge, &p.query, p.tab_id).await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "redditsearch",
        description = "Reddit 搜索，返回 { tab_id, results[] }（title/description/published/published_at/votes/comments/url/target；votes/comments 为整数 upvote/评论数量，published_at 为 ISO 时间戳）"
    )]
    pub async fn redditsearch_tool(
        &self,
        params: Parameters<RedditsearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = redditsearch(&mut bridge, &p.query, p.tab_id).await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "youtubesearch",
        description = "YouTube 搜索，返回 { tab_id, results[] }（title/channel/views/published/duration/url/target），支持上传日期（today/week/month/year）与优先顺序（relevance/popularity）筛选"
    )]
    pub async fn youtubesearch_tool(
        &self,
        params: Parameters<YoutubesearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = youtubesearch(
            &mut bridge,
            &p.query,
            p.time.as_deref().unwrap_or("any"),
            p.sort.as_deref().unwrap_or("relevance"),
            p.max.unwrap_or(5),
            p.tab_id,
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "youtubeinfo",
        description = "获取指定 YouTube 视频详情，返回 { tab_id, video }（url/title/author/author_url/duration/duration_seconds/like_count/comment_count/subscriber_count/captions[]，字幕为全文，各计数同时附 *_text 原始文本）"
    )]
    pub async fn youtubeinfo_tool(
        &self,
        params: Parameters<YoutubeinfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = youtubeinfo(&mut bridge, &p.url, p.tab_id).await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "youtuberinfo",
        description = "获取指定 YouTube 频道（youtuber）的视频列表，返回 { tab_id, channel, videos[] }（channel 含 name/url/subscriber_count/subscriber_count_text；videos 每项含 title/url/views/views_count/duration/published/target），最多返回 max 条（默认 10，至少 1）"
    )]
    pub async fn youtuberinfo_tool(
        &self,
        params: Parameters<YoutuberinfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = youtuberinfo(&mut bridge, &p.channel, p.max.unwrap_or(10), p.tab_id).await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "googletrends",
        description = "Google Trends 趋势查询，返回 { tab_id, trend[], top[], rising[], regions[], top_table_available, rising_table_available, tables_available }。available 表示表格已成功读取，即使 rows 为空也可能为 true；regions 为按地区的搜索热度（rank/region/geo_code/interest）"
    )]
    pub async fn googletrends_tool(
        &self,
        params: Parameters<GoogletrendsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = googletrends(
            &mut bridge,
            &p.query,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "oldgoogletrends",
        description = "经典版 Google Trends（legacy）单关键词查询，支持 now 7-d。返回 { tab_id, trend[], top[], rising[], regions[], top_table_available, rising_table_available, tables_available, regions_available }。从页面 DOM 读取趋势表格、切换热门/上升并翻页，trend.date 保留页面时间标签；rising.interest 为 null，change 为增长百分比或 breakout。空榜单 available=true，读取失败为 false 并附 errors。新建标签页，收尾用 close_auto_tabs 清理"
    )]
    pub async fn oldgoogletrends_tool(
        &self,
        params: Parameters<OldgoogletrendsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = oldgoogletrends(
            &mut bridge,
            &p.query,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "oldgoogletrends_compare",
        description = "经典版 Google Trends（legacy）关键词对比，支持 now 7-d。返回 { series[] }，每个词一条趋势序列（共享 0-100 刻度，无数据词整列为 0；trend.date 保留页面时间标签，不返回热门/上升表）。从经典页面 DOM 表格读取，新建标签页，收尾用 close_auto_tabs 清理"
    )]
    pub async fn oldgoogletrends_compare_tool(
        &self,
        params: Parameters<OldgoogletrendsCompareParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = oldgoogletrends_compare(
            &mut bridge,
            &p.terms,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "googletrends_compare",
        description = "Google Trends 关键词对比，返回 { series[] }，每个词一条趋势序列（共享 0-100 刻度，不返回热门/上升表）"
    )]
    pub async fn googletrends_compare_tool(
        &self,
        params: Parameters<GoogletrendsCompareParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = googletrends_compare(
            &mut bridge,
            &p.terms,
            p.date.as_deref().unwrap_or("today 1-m"),
            p.geo.as_deref().unwrap_or("Worldwide"),
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
    }

    #[tool(
        name = "querydomains",
        description = "用 Query.Domains 按关键词批量查询域名注册情况与价格，返回 { tab_id, query, tlds, complete, results[] }（每项含 domain/tld/status/available/price/badges；status: available|unavailable|uncertain）"
    )]
    pub async fn querydomains_tool(
        &self,
        params: Parameters<QuerydomainsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let bridge = self.bridge.with_context(
            params.0.window_id,
            params.0.task_id.clone(),
            params.0.agent_id.clone(),
        );
        let p = params.0;
        let owns_task = !bridge.has_task();
        let mut bridge = if owns_task {
            bridge.start_task(p.window_id)
        } else {
            bridge
        };
        let out = querydomains(
            &mut bridge,
            &p.query,
            p.tlds.as_deref().unwrap_or(&[]),
            p.tab_id,
        )
        .await;
        finish_recipe(&bridge, out, owns_task).await
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
    let mcp = BridgeMcp { bridge, client_id };

    // Chrome is shared by concurrent clients. A process ending must never
    // quit the browser used by other tasks; task leases drain on disconnect.
    let service = mcp.serve(rmcp::transport::stdio()).await?;
    let waiting = service.waiting().await;
    waiting?;
    Ok(())
}
