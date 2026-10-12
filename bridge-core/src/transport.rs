//! 与 bridge server 的传输层：连接、自动拉起、请求-响应、可重连客户端。

use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);
/// 自动拉起 server 时的空闲退出时间（秒）
pub const AUTO_SPAWN_IDLE_TIMEOUT: &str = "120";

pub type BridgeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 可重连的 bridge 客户端：长连接 + 请求，断线自动重连（含自动拉起）后重试一次。
#[derive(Clone)]
pub struct Bridge {
    server: String,
    client_id: String,
    window_id: Option<i32>,
    task_id: Option<String>,
    requests: mpsc::UnboundedSender<RequestCommand>,
}

struct RequestCommand {
    id: String,
    method: String,
    params: Value,
    timeout: Duration,
    enqueued_at: tokio::time::Instant,
    reply: oneshot::Sender<Result<Value, String>>,
}

struct Pending {
    reply: oneshot::Sender<Result<Value, String>>,
    deadline: tokio::time::Instant,
}

impl Bridge {
    pub async fn connect(server: &str) -> Result<Self, String> {
        let client_id = std::env::var("BRIDGE_CLIENT_ID").unwrap_or_else(|_| "cli".to_string());
        let mut bridge = Self::connect_with_client_id(server, &client_id).await?;
        bridge.window_id = std::env::var("BRIDGE_WINDOW_ID")
            .ok()
            .and_then(|value| value.parse::<i32>().ok());
        Ok(bridge)
    }

    /// 指定客户端身份（多 agent 场景下每个调用方用独立 id，用于标签页归属与清理隔离）
    pub async fn connect_with_client_id(server: &str, client_id: &str) -> Result<Self, String> {
        let ws = connect_bridge(server, client_id).await?;
        let (requests, request_rx) = mpsc::unbounded_channel();
        let server_name = server.to_string();
        let client_name = client_id.to_string();
        tokio::spawn(connection_manager(server_name, client_name, ws, request_rx));
        Ok(Self {
            server: server.to_string(),
            client_id: client_id.to_string(),
            window_id: None,
            task_id: None,
            requests,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Clone this transport with a default browser window. Recipe-level
    /// requests inherit the value automatically, so every step of a recipe
    /// stays bound to the same window without changing every recipe signature.
    pub fn with_window(&self, window_id: Option<i32>) -> Self {
        let mut next = self.clone();
        next.window_id = window_id;
        next
    }

    /// Start a lease that keeps one window occupied across every request in a
    /// multi-step recipe. The server releases it only after `finish_task`.
    pub fn start_task(&self, window_id: Option<i32>) -> Self {
        static TASK_SEQ: AtomicU64 = AtomicU64::new(0);
        let mut next = self.clone();
        next.window_id = window_id;
        next.task_id = Some(format!(
            "task-{}-{}",
            std::process::id(),
            TASK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        next
    }

    pub async fn finish_task(&self) -> Result<Value, String> {
        let Some(task_id) = self.task_id.as_deref() else {
            return Ok(Value::Null);
        };
        self.request("task-end", "task_end", json!({ "task_id": task_id }))
            .await
    }

    /// 发送一次请求；连接断开时自动重连（含自动拉起 server）后重试一次。
    pub async fn request(&self, id: &str, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(id, method, params, RESPONSE_TIMEOUT)
            .await
    }

    /// Send one request with a caller-selected response timeout.
    pub async fn request_with_timeout(
        &self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        // 请求 id 只是语义标签，线上用唯一 id（进程 id + 自增序号），
        // 避免多 agent 并发时 server 按 id 路由响应串线
        static REQ_SEQ: AtomicU64 = AtomicU64::new(0);
        let wire_id = format!(
            "{id}-{}-{}",
            std::process::id(),
            REQ_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let mut params = params;
        if let (Some(window_id), Some(object)) = (self.window_id, params.as_object_mut()) {
            object
                .entry("window_id")
                .or_insert_with(|| json!(window_id));
        }
        if let (Some(task_id), Some(object)) = (self.task_id.as_deref(), params.as_object_mut()) {
            object.entry("task_id").or_insert_with(|| json!(task_id));
        }
        let (reply, response) = oneshot::channel();
        self.requests
            .send(RequestCommand {
                id: wire_id,
                method: method.to_string(),
                params,
                timeout,
                enqueued_at: tokio::time::Instant::now(),
                reply,
            })
            .map_err(|_| format!("bridge connection manager stopped for {}", self.server))?;
        response
            .await
            .map_err(|_| "bridge connection manager stopped".to_string())?
    }
}

/// Owns the WebSocket and is the only task allowed to read from it. Requests
/// therefore remain multiplexed even when one MCP session has several tools in
/// flight at once; each caller waits on its own oneshot response.
async fn connection_manager(
    server: String,
    client_id: String,
    mut ws: BridgeStream,
    mut rx: mpsc::UnboundedReceiver<RequestCommand>,
) {
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut queued: VecDeque<RequestCommand> = VecDeque::new();
    let mut reconnect_at: Option<tokio::time::Instant> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.tick().await;

    loop {
        if reconnect_at.is_some() {
            // Keep accepting calls while the server is unavailable. They are
            // sent after reconnect, or fail at their own deadline.
            tokio::select! {
                command = rx.recv() => match command {
                    Some(command) => queued.push_back(command),
                    None => return,
                },
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    match connect_bridge_no_spawn(&server, &client_id).await {
                        Ok(new_ws) => {
                            ws = new_ws;
                            reconnect_at = None;
                        }
                        Err(_) => {}
                    }
                }
                _ = ticker.tick() => {
                    expire_queued(&mut queued);
                }
            }
            continue;
        }

        while let Some(command) = queued.pop_front() {
            if command.reply.is_closed() {
                continue;
            }
            if let Err(err) = send_command(&mut ws, &command).await {
                let _ = command.reply.send(Err(err));
                reconnect_at = Some(tokio::time::Instant::now());
                break;
            }
            pending.insert(
                command.id,
                Pending {
                    reply: command.reply,
                    deadline: tokio::time::Instant::now() + command.timeout,
                },
            );
        }
        if reconnect_at.is_some() {
            fail_pending(&mut pending, "bridge connection lost");
            continue;
        }

        tokio::select! {
            command = rx.recv() => match command {
                Some(command) => {
                    if let Err(err) = send_command(&mut ws, &command).await {
                        let _ = command.reply.send(Err(err));
                        fail_pending(&mut pending, "bridge connection lost");
                        reconnect_at = Some(tokio::time::Instant::now());
                    } else {
                        pending.insert(command.id, Pending {
                            reply: command.reply,
                            deadline: tokio::time::Instant::now() + command.timeout,
                        });
                    }
                }
                None => return,
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(text))) => handle_response(&mut pending, text.to_string()),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    fail_pending(&mut pending, "bridge connection lost");
                    reconnect_at = Some(tokio::time::Instant::now());
                }
            },
            _ = ticker.tick() => expire_pending(&mut pending),
        }
    }
}

async fn send_command(ws: &mut BridgeStream, command: &RequestCommand) -> Result<(), String> {
    ws.send(Message::Text(
        json!({
            "id": command.id,
            "method": command.method,
            "params": command.params,
            "timeout_ms": command.timeout.as_millis().min(u64::MAX as u128) as u64,
        })
        .to_string()
        .into(),
    ))
    .await
    .map_err(|e| e.to_string())
}

fn handle_response(pending: &mut HashMap<String, Pending>, text: String) {
    let Ok(resp) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(id) = resp.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(request) = pending.remove(id) else {
        return;
    };
    let result = if resp.get("success").and_then(Value::as_bool) == Some(true) {
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string())
    };
    let _ = request.reply.send(result);
}

fn expire_pending(pending: &mut HashMap<String, Pending>) {
    let now = tokio::time::Instant::now();
    let expired: Vec<String> = pending
        .iter()
        .filter(|(_, request)| request.deadline <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        if let Some(request) = pending.remove(&id) {
            let _ = request.reply.send(Err("等待响应超时".to_string()));
        }
    }
}

fn expire_queued(queued: &mut VecDeque<RequestCommand>) {
    let now = tokio::time::Instant::now();
    let mut kept = VecDeque::with_capacity(queued.len());
    while let Some(command) = queued.pop_front() {
        if command.reply.is_closed() {
            continue;
        }
        if command.enqueued_at + command.timeout > now {
            kept.push_back(command);
        } else {
            let _ = command.reply.send(Err("等待响应超时".to_string()));
        }
    }
    *queued = kept;
}

fn fail_pending(pending: &mut HashMap<String, Pending>, error: &str) {
    for (_, request) in pending.drain() {
        let _ = request.reply.send(Err(error.to_string()));
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

/// 连接 server 并完成 hello 握手；**不自动拉起 server**。
/// 用于后台监听类连接（如检测 server 退出），避免把已退出的 server 又拉起来。
pub async fn connect_bridge_no_spawn(
    server: &str,
    client_id: &str,
) -> Result<BridgeStream, String> {
    let (ws, _) = connect_async(server)
        .await
        .map_err(|e| format!("无法连接 {server}: {e}"))?;
    finish_hello(ws, client_id).await
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
    cmd.env("BRIDGE_PORT", &port)
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
        return Err(format!(
            "BRIDGE_SERVER_BIN 指向的文件不存在: {}",
            p.display()
        ));
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
    request_with_timeout(ws, id, method, params, RESPONSE_TIMEOUT).await
}

/// Send one request and wait up to `timeout` for its matching response.
pub async fn request_with_timeout(
    ws: &mut BridgeStream,
    id: &str,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    ws.send(Message::Text(
        json!({ "id": id, "method": method, "params": params })
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + timeout;
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
