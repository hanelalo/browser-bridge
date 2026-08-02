//! Browser Bridge MCP server：stdio 传输，把浏览器指令暴露为 MCP tools。
//! 通过 bridge-core 复用协议传输（含自动拉起 server、断线重连）。

use std::collections::HashMap;
use std::sync::Arc;

use bridge_core::recipes::googlesearch::googlesearch;
use bridge_core::recipes::redditsearch::redditsearch;
use bridge_core::target;
use bridge_core::transport::Bridge;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const DEFAULT_SERVER: &str = "ws://127.0.0.1:9225";

#[derive(Clone)]
struct BridgeMcp {
    bridge: Arc<Mutex<Bridge>>,
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
    let mut b = bridge.lock().await;
    let result = b
        .request(id, method, params)
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
    ok(result)
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

#[tool_router(server_handler)]
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

    #[tool(name = "redditsearch", description = "Reddit 搜索，返回 { tab_id, results[] }（title/description/url/target）")]
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("BRIDGE_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());
    let bridge = Bridge::connect(&server).await?;
    eprintln!("bridge-mcp connected to {server}");
    let mcp = BridgeMcp {
        bridge: Arc::new(Mutex::new(bridge)),
    };
    let service = mcp.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
