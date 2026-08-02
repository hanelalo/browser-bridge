//! 与 bridge server 的传输层：连接、自动拉起、请求-响应、可重连客户端。

use std::path::PathBuf;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);
/// 自动拉起 server 时的空闲退出时间（秒）
pub const AUTO_SPAWN_IDLE_TIMEOUT: &str = "120";

pub type BridgeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 可重连的 bridge 客户端：长连接 + 请求，断线自动重连（含自动拉起）后重试一次。
pub struct Bridge {
    server: String,
    client_id: String,
    ws: BridgeStream,
}

impl Bridge {
    pub async fn connect(server: &str) -> Result<Self, String> {
        let client_id = std::env::var("BRIDGE_CLIENT_ID").unwrap_or_else(|_| "cli".to_string());
        Self::connect_with_client_id(server, &client_id).await
    }

    /// 指定客户端身份（多 agent 场景下每个调用方用独立 id，用于标签页归属与清理隔离）
    pub async fn connect_with_client_id(server: &str, client_id: &str) -> Result<Self, String> {
        Ok(Self {
            server: server.to_string(),
            client_id: client_id.to_string(),
            ws: connect_bridge(server, client_id).await?,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// 发送一次请求；连接断开时自动重连（含自动拉起 server）后重试一次。
    pub async fn request(
        &mut self,
        id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        // 请求 id 只是语义标签，线上用唯一 id（进程 id + 自增序号），
        // 避免多 agent 并发时 server 按 id 路由响应串线
        static REQ_SEQ: AtomicU64 = AtomicU64::new(0);
        let wire_id = format!("{id}-{}-{}", std::process::id(), REQ_SEQ.fetch_add(1, Ordering::Relaxed));
        match request(&mut self.ws, &wire_id, method, params.clone()).await {
            Ok(value) => Ok(value),
            Err(_) => {
                self.ws = connect_bridge(&self.server, &self.client_id).await?;
                request(&mut self.ws, &wire_id, method, params).await
            }
        }
    }
}

/// 连接 server 并完成 hello 握手；连接失败时自动拉起 bridge-server。
pub async fn connect_bridge(server: &str, client_id: &str) -> Result<BridgeStream, String> {
    match connect_async(server).await {
        Ok((ws, _)) => finish_hello(ws, client_id).await,
        Err(first_err) => {
            // server 未运行：自动拉起一个，并等它就绪
            match spawn_bridge_server(server) {
                Ok(_) => {
                    eprintln!(
                        "已自动启动 bridge-server（空闲 {AUTO_SPAWN_IDLE_TIMEOUT}s 自动退出）"
                    );
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        match connect_async(server).await {
                            Ok((ws, _)) => return finish_hello(ws, client_id).await,
                            Err(e) => {
                                if tokio::time::Instant::now() >= deadline {
                                    return Err(format!(
                                        "自动启动 bridge-server 后仍无法连接 {server}: {e}"
                                    ));
                                }
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }
                }
                Err(spawn_err) => Err(format!(
                    "无法连接 {server}（server 是否在运行？）: {first_err}；自动启动失败: {spawn_err}"
                )),
            }
        }
    }
}

/// 完成 hello 握手。
async fn finish_hello(mut ws: BridgeStream, client_id: &str) -> Result<BridgeStream, String> {
    ws.send(Message::Text(
        json!({
            "type": "hello",
            "role": "client",
            "name": "bridge-client",
            "client_id": client_id,
        })
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    Ok(ws)
}

/// 自动拉起 bridge-server（与 client 同端口）。定位顺序：
/// 1. `BRIDGE_SERVER_BIN` 显式指定；2. 与 client 同目录；3. target/release|debug；4. PATH。
fn spawn_bridge_server(server: &str) -> Result<(), String> {
    let bin = server_binary_path()?;
    let port = server_port(server);
    let mut cmd = Command::new(&bin);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
        .env("BRIDGE_PORT", &port)
        .env("BRIDGE_IDLE_TIMEOUT", AUTO_SPAWN_IDLE_TIMEOUT)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("启动 {bin:?} 失败: {e}"))
}

fn server_binary_path() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("BRIDGE_SERVER_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("BRIDGE_SERVER_BIN 指向的文件不存在: {}", p.display()));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("bridge-server"));
        }
    }
    candidates.push(PathBuf::from("target/release/bridge-server"));
    candidates.push(PathBuf::from("target/debug/bridge-server"));
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    // 兜底：交给 PATH 解析
    Ok(PathBuf::from("bridge-server"))
}

/// 从 ws://host:port 中取出端口（自动拉起 server 时使用同一端口）。
fn server_port(server: &str) -> String {
    server
        .rsplit(':')
        .next()
        .unwrap_or("9225")
        .trim_end_matches('/')
        .to_string()
}

/// 在已有连接上发送一次请求并等待对应 id 的响应。
pub async fn request(
    ws: &mut BridgeStream,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    ws.send(Message::Text(
        json!({ "id": id, "method": method, "params": params })
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
        if resp.get("id").and_then(Value::as_str) != Some(id) {
            continue;
        }
        if resp.get("success").and_then(Value::as_bool) == Some(true) {
            return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
        }
        let err = resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(err.to_string());
    }
}

/// 简易 URL 编码：字母数字及 -_.~ 原样，空格转 +，其余按 UTF-8 字节转 %XX。
pub fn urlencode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}
