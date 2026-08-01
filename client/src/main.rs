//! Browser Bridge CLI client（控制端）。

use std::time::Duration;

use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Parser)]
#[command(
    name = "bridge-client",
    version,
    about = "Browser Bridge CLI（控制端）"
)]
struct Cli {
    /// WebSocket 服务地址
    #[arg(long, default_value = "ws://127.0.0.1:9225", env = "BRIDGE_SERVER")]
    server: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出所有标签页
    ListTabs,
    /// 导航到指定 URL
    Navigate {
        url: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 点击匹配 CSS selector 的元素
    Click {
        selector: String,
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
    /// 读取页面文本内容
    GetPageContent {
        /// 指定标签页 id（默认当前激活标签页）
        #[arg(long)]
        tab: Option<i32>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let (method, params) = match cli.cmd {
        Cmd::ListTabs => ("list_tabs", json!({})),
        Cmd::Navigate { url, tab } => ("navigate", json!({ "url": url, "tab_id": tab })),
        Cmd::Click { selector, tab } => ("click", json!({ "selector": selector, "tab_id": tab })),
        Cmd::GetPageContent { tab } => ("get_page_content", json!({ "tab_id": tab })),
    };

    if let Err(err) = run(&cli.server, method, params).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run(server: &str, method: &str, params: Value) -> Result<(), String> {
    let (mut ws, _) = connect_async(server)
        .await
        .map_err(|e| format!("无法连接 {server}（server 是否在运行？）: {e}"))?;

    ws.send(Message::Text(
        json!({ "type": "hello", "role": "client", "name": "bridge-client" })
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| e.to_string())?;

    let id = "c1".to_string();
    ws.send(Message::Text(
        json!({ "id": id.as_str(), "method": method, "params": params })
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("等待响应超时".to_string());
        }
        let msg = tokio::time::timeout(remaining, ws.next())
            .await
            .map_err(|_| "等待响应超时".to_string())?
            .ok_or_else(|| "连接已关闭".to_string())?
            .map_err(|e| e.to_string())?;

        let Message::Text(text) = msg else { continue };

        let resp: Value = serde_json::from_str(&text).map_err(|e| format!("响应解析失败: {e}"))?;
        if resp.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            continue;
        }
        if resp.get("success").and_then(Value::as_bool) == Some(true) {
            let result = resp.get("result").cloned().unwrap_or(Value::Null);
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
            return Ok(());
        }
        let err = resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(err.to_string());
    }
}
