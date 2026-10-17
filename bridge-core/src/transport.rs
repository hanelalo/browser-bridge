//! 与 bridge server 的传输层：连接、自动拉起、请求-响应、可重连客户端。

use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Multiplexed transport. Reconnect future calls without replaying sent operations.
#[derive(Clone)]
pub struct Bridge {
    server: String,
    client_id: String,
    window_id: Option<i32>,
    task_id: Option<String>,
    task_guard: Option<Arc<TaskGuard>>,
    agent_id: Option<String>,
    requests: mpsc::UnboundedSender<RequestCommand>,
}

struct RequestCommand {
    id: String,
    method: String,
    params: Value,
    timeout: Duration,
    enqueued_at: tokio::time::Instant,
    deadline_ms: u64,
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
            task_guard: None,
            agent_id: None,
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
        next.window_id = window_id.or(self.window_id);
        next.task_id = Some(format!(
            "task-{}-{}",
            std::process::id(),
            TASK_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        next.task_guard = Some(Arc::new(TaskGuard {
            id: next.task_id.clone().unwrap(),
            requests: self.requests.clone(),
            finished: AtomicBool::new(false),
        }));
        next
    }

    pub fn with_context(
        &self,
        window_id: Option<i32>,
        task_id: Option<String>,
        agent_id: Option<String>,
    ) -> Self {
        let mut next = self.with_window(window_id.or(self.window_id));
        next.task_id = task_id;
        next.task_guard = None;
        next.agent_id = agent_id;
        next
    }

    pub fn has_task(&self) -> bool {
        self.task_id.is_some()
    }

    pub async fn finish_task(&self) -> Result<Value, String> {
        let Some(task_id) = self.task_id.as_deref() else {
            return Ok(Value::Null);
        };
        let result = self
            .request("task-end", "task_end", json!({ "task_id": task_id }))
            .await;
        if result.is_ok() {
            if let Some(guard) = &self.task_guard {
                guard.finished.store(true, Ordering::Relaxed);
            }
        }
        result
    }

    /// Send once; the result may be unknown on disconnect and is never replayed.
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
            if object.get("window_id").is_none_or(Value::is_null) {
                object.insert("window_id".into(), json!(window_id));
            }
        }
        if let (Some(task_id), Some(object)) = (self.task_id.as_deref(), params.as_object_mut()) {
            object.entry("task_id").or_insert_with(|| json!(task_id));
        }
        if let (Some(agent_id), Some(object)) = (self.agent_id.as_deref(), params.as_object_mut()) {
            object.entry("agent_id").or_insert_with(|| json!(agent_id));
        }
        let (reply, response) = oneshot::channel();
        self.requests
            .send(RequestCommand {
                id: wire_id,
                method: method.to_string(),
                params,
                timeout,
                enqueued_at: tokio::time::Instant::now(),
                deadline_ms: unix_ms()
                    .saturating_add(timeout.as_millis().min(u64::MAX as u128) as u64),
                reply,
            })
            .map_err(|_| format!("bridge connection manager stopped for {}", self.server))?;
        response
            .await
            .map_err(|_| "bridge connection manager stopped".to_string())?
    }
}

// Owned by all clones of a scoped Bridge. Dropping a cancelled recipe still
// sends an end request; the server drains an in-flight operation before release.
struct TaskGuard {
    id: String,
    requests: mpsc::UnboundedSender<RequestCommand>,
    finished: AtomicBool,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if self.finished.load(Ordering::Relaxed) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let sender = self.requests.clone();
        let id = self.id.clone();
        runtime.spawn(async move {
            let (reply, response) = oneshot::channel();
            let command = RequestCommand {
                id: format!("drop-{id}"),
                method: "task_end".into(),
                params: json!({"task_id":id}),
                timeout: RESPONSE_TIMEOUT,
                enqueued_at: tokio::time::Instant::now(),
                deadline_ms: unix_ms() + RESPONSE_TIMEOUT.as_millis() as u64,
                reply,
            };
            if sender.send(command).is_ok() {
                let _ = response.await;
            }
        });
    }
}
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// One reader distributes responses by ID. Retry timing is an absolute Instant,
/// so the 100ms expiry ticker cannot cancel/reset the reconnect delay.
async fn connection_manager(
    server: String,
    client_id: String,
    mut ws: BridgeStream,
    mut rx: mpsc::UnboundedReceiver<RequestCommand>,
) {
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut queued: VecDeque<RequestCommand> = VecDeque::new();
    let mut connected = true;
    let mut known_tasks = HashSet::<String>::new();
    let mut invalid_tasks = HashSet::<String>::new();
    let mut retry_at = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    loop {
        expire_queued(&mut queued);
        if connected {
            while let Some(command) = queued.pop_front() {
                if command.reply.is_closed()
                    || command.enqueued_at + command.timeout <= tokio::time::Instant::now()
                {
                    continue;
                }
                if let Some(task_id) = command.params["task_id"].as_str() {
                    if invalid_tasks.contains(task_id)
                        && !matches!(
                            command.method.as_str(),
                            "task_end" | "task_cancel" | "task_status"
                        )
                    {
                        let _ = command.reply.send(Err(
                            "task lease was invalidated by reconnect; begin a new task".into(),
                        ));
                        continue;
                    }
                    known_tasks.insert(task_id.to_string());
                }
                if let Err(error) = send_command(&mut ws, &command).await {
                    let _ = command.reply.send(Err(format!(
                        "connection lost; execution result unknown: {error}"
                    )));
                    fail_pending(&mut pending, "connection lost; execution result unknown");
                    connected = false;
                    invalid_tasks.extend(known_tasks.drain());
                    retry_at = tokio::time::Instant::now() + Duration::from_millis(500);
                    break;
                }
                pending.insert(
                    command.id,
                    Pending {
                        reply: command.reply,
                        deadline: command.enqueued_at + command.timeout,
                    },
                );
            }
        }
        tokio::select! {
            command = rx.recv() => match command { Some(command)=>queued.push_back(command), None=>return },
            incoming = ws.next(), if connected => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(response)=serde_json::from_str::<Value>(&text) {
                        if response["success"]==true {
                            if let Some(task)=response["result"]["task_id"].as_str() {
                                if response["result"]["state"]=="finished" {known_tasks.remove(task);}
                                else {known_tasks.insert(task.to_string());}
                            }
                        }
                    }
                    handle_response(&mut pending, text.to_string());
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                    fail_pending(&mut pending, "connection lost; execution result unknown");
                    connected=false; invalid_tasks.extend(known_tasks.drain());
                    retry_at=tokio::time::Instant::now()+Duration::from_millis(500);
                }
                Some(Ok(_)) => {}
            },
            _ = ticker.tick() => {
                // A cancelled tool future must not leave queued browser work
                // behind. Cancellation is advisory for an already running
                // operation; the server keeps its resources until completion.
                if connected {
                    let cancelled: Vec<_> = pending.iter().filter(|(_, r)|r.reply.is_closed()).map(|(id,_)|id.clone()).collect();
                    for id in cancelled {
                        pending.remove(&id);
                        let _ = ws.send(Message::Text(json!({"id":format!("cancel-{id}"),"method":"request_cancel","params":{"request_id":id}}).to_string().into())).await;
                    }
                }
                expire_pending(&mut pending);
                if !connected && tokio::time::Instant::now() >= retry_at {
                    if let Ok(Ok(new_ws)) = tokio::time::timeout(Duration::from_secs(2), connect_bridge_no_spawn(&server, &client_id)).await {
                        ws = new_ws; connected = true;
                    }
                    retry_at = tokio::time::Instant::now()+Duration::from_millis(500);
                }
            }
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
            "deadline_ms": command.deadline_ms,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    async fn read(ws: &mut BridgeStream) -> Value {
        loop {
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    async fn accept(listener: &TcpListener) -> BridgeStream {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(MaybeTlsStream::Plain(stream)).await.unwrap();
        let hello = read(&mut ws).await;
        assert_eq!(hello["type"], "hello");
        ws
    }
    async fn respond(ws: &mut BridgeStream, request: &Value, result: Value) {
        ws.send(Message::Text(
            json!({"id":request["id"],"success":true,"result":result})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn concurrent_responses_are_matched_even_when_out_of_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut ws = accept(&listener).await;
            let first = read(&mut ws).await;
            let second = read(&mut ws).await;
            respond(&mut ws, &second, second["params"].clone()).await;
            respond(&mut ws, &first, first["params"].clone()).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            bridge.request("a", "echo", json!({"value":1})),
            bridge.request("b", "echo", json!({"value":2}))
        );
        assert_eq!(first.unwrap()["value"], 1);
        assert_eq!(second.unwrap()["value"], 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_is_not_starved_by_expiry_ticker_and_does_not_replay_sent_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            let request = read(&mut first).await;
            assert_eq!(request["method"], "click");
            first.close(None).await.unwrap(); // execution outcome unknown
            let mut second = accept(&listener).await;
            let request = read(&mut second).await;
            assert_eq!(request["method"], "ping");
            respond(&mut second, &request, json!({"pong":true})).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap();
        assert!(bridge
            .request("first", "click", json!({}))
            .await
            .unwrap_err()
            .contains("unknown"));
        let reply = bridge
            .request_with_timeout("second", "ping", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(reply["pong"], true);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn requests_expired_during_reconnect_are_never_sent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            let _ = read(&mut first).await;
            first.close(None).await.unwrap();
            let mut second = accept(&listener).await;
            let request = read(&mut second).await;
            assert_eq!(request["method"], "live");
            respond(&mut second, &request, json!({"ok":true})).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap();
        assert!(bridge.request("first", "click", json!({})).await.is_err());
        assert!(bridge
            .request_with_timeout(
                "stale",
                "must-not-execute",
                json!({}),
                Duration::from_millis(80)
            )
            .await
            .is_err());
        assert!(bridge
            .request_with_timeout("live", "live", json!({}), Duration::from_secs(2))
            .await
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_a_recipe_scope_sends_end_without_waiting_for_connection_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut ws = accept(&listener).await;
            let request = read(&mut ws).await;
            let task_id = request["params"]["task_id"].clone();
            respond(&mut ws, &request, json!({})).await;
            let end = read(&mut ws).await;
            assert_eq!(end["method"], "task_end");
            assert_eq!(end["params"]["task_id"], task_id);
            respond(&mut ws, &end, json!({"ended":true})).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap();
        let task = bridge.start_task(Some(7));
        task.request("work", "click", json!({})).await.unwrap();
        drop(task);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn window_context_replaces_null_and_deadline_is_sent_to_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut ws = accept(&listener).await;
            let request = read(&mut ws).await;
            assert_eq!(request["params"]["window_id"], 7);
            assert!(request["deadline_ms"].as_u64().unwrap() > unix_ms());
            respond(&mut ws, &request, json!({})).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap()
            .with_window(Some(7));
        bridge
            .request("work", "click", json!({"window_id":null}))
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_cannot_silently_recreate_an_old_task_in_a_default_window() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            let begin = read(&mut first).await;
            respond(
                &mut first,
                &begin,
                json!({"task_id":"issued-task","state":"running"}),
            )
            .await;
            first.close(None).await.unwrap();
            let mut second = accept(&listener).await;
            let request = read(&mut second).await;
            assert_eq!(request["method"], "live");
            respond(&mut second, &request, json!({})).await;
        });
        let bridge = Bridge::connect_with_client_id(&format!("ws://{addr}"), "test")
            .await
            .unwrap();
        bridge
            .request("begin", "task_begin", json!({}))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let old = bridge.with_context(None, Some("issued-task".into()), None);
        assert!(old
            .request("unsafe", "click", json!({}))
            .await
            .unwrap_err()
            .contains("invalidated"));
        bridge.request("live", "live", json!({})).await.unwrap();
        server.await.unwrap();
    }
}
