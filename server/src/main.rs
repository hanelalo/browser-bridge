//! Browser Bridge WebSocket hub。
//!
//! 职责：接收一个 extension 连接和多个 client 连接，
//! 把 client 的请求转发给 extension，再把结果路由回对应的 client。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_PORT: u16 = 9225;
// run_script 的最长往返：googletrends 脚本含首屏等待 + 5s 匀速滚动 + 表格翻页采集，
// 数据多的查询实测可超 30s，给到 60s 余量
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    Extension,
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// 来源客户端身份（server 转发 client 请求给 extension 时盖章）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Optional caller deadline in milliseconds; old clients omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

impl WireMessage {
    fn ok(id: &str, result: Value) -> Self {
        Self {
            id: Some(id.to_string()),
            success: Some(true),
            result: Some(result),
            ..Self::empty()
        }
    }

    fn err(id: &str, error: impl Into<String>) -> Self {
        Self {
            id: Some(id.to_string()),
            success: Some(false),
            error: Some(error.into()),
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        Self {
            id: None,
            type_: None,
            role: None,
            name: None,
            client_id: None,
            method: None,
            params: None,
            success: None,
            result: None,
            error: None,
            timeout_ms: None,
        }
    }

    fn to_text(&self) -> Message {
        Message::Text(
            serde_json::to_string(self)
                .expect("wire message serializes")
                .into(),
        )
    }
}

#[cfg(test)]
fn text_of(msg: Message) -> Option<String> {
    match msg {
        Message::Text(text) => Some(text.to_string()),
        _ => None,
    }
}

/// 中枢事件：注册/注销连接、转发消息、请求超时。
enum HubMsg {
    Register {
        conn_id: u64,
        role: Role,
        name: String,
        client_id: Option<String>,
        tx: mpsc::UnboundedSender<Message>,
    },
    Unregister {
        conn_id: u64,
    },
    Forward {
        conn_id: u64,
        msg: WireMessage,
    },
    TimedOut {
        id: String,
    },
    /// A request that waited for both the global and window admission gates.
    Admit {
        conn_id: u64,
        extension_id: u64,
        msg: WireMessage,
        task_id: Option<String>,
        window_key: String,
        timeout: Duration,
        global_permit: OwnedSemaphorePermit,
        window_permit: OwnedSemaphorePermit,
    },
}

struct Conn {
    role: Role,
    client_id: Option<String>,
    tx: mpsc::UnboundedSender<Message>,
}

/// id -> (发起请求的 client 连接, 取消超时任务的信号)
struct PendingRequest {
    client_conn_id: u64,
    task_id: Option<String>,
    timed_out: bool,
    done_tx: oneshot::Sender<()>,
    // Keeping these permits in Pending makes the resource lifetime explicit:
    // a slot is released only after a response or timeout is observed.
    _permits: Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)>,
}

type Pending = HashMap<String, PendingRequest>;

struct TaskLease {
    client_conn_id: u64,
    window_key: String,
    uncertain: bool,
    _global_permit: OwnedSemaphorePermit,
    _window_permit: OwnedSemaphorePermit,
}

async fn hub_loop(
    mut rx: mpsc::UnboundedReceiver<HubMsg>,
    hub_tx: mpsc::UnboundedSender<HubMsg>,
    idle_timeout: Duration,
) {
    let mut conns: HashMap<u64, Conn> = HashMap::new();
    let mut extension_id: Option<u64> = None;
    let mut pending: Pending = HashMap::new();
    let mut tasks: HashMap<String, TaskLease> = HashMap::new();
    // All clients share this limit. A request must also acquire a per-window
    // gate below, so one busy window cannot consume the other two slots.
    let global_slots = std::sync::Arc::new(Semaphore::new(3));
    let mut window_slots: HashMap<String, std::sync::Arc<Semaphore>> = HashMap::new();
    let mut last_activity = tokio::time::Instant::now();
    // 每秒检查一次空闲状态；丢弃 interval 的首次立即触发
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.tick().await;

    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break };
                last_activity = tokio::time::Instant::now();
                match event {
                    HubMsg::Register {
                        conn_id,
                        role,
                        name,
                        client_id,
                        tx,
                    } => {
                        if role == Role::Extension {
                            if let Some(old) = extension_id.replace(conn_id) {
                                // 新 extension 顶掉旧的：清掉所有未完成请求
                                conns.remove(&old);
                                pending.clear();
                                tasks.clear();
                            }
                        }
                        eprintln!(
                            "[hub] connection #{conn_id} registered as {:?} \"{name}\"",
                            role
                        );
                        conns.insert(conn_id, Conn { role, client_id, tx });
                    }
                    HubMsg::Unregister { conn_id } => {
                        if extension_id == Some(conn_id) {
                            extension_id = None;
                            pending.clear();
                            tasks.clear();
                        }
                        if conns.remove(&conn_id).is_some() {
                            eprintln!("[hub] connection #{conn_id} disconnected");
                        }
                        tasks.retain(|_, task| task.client_conn_id != conn_id);
                    }
                    HubMsg::Forward { conn_id, msg } => {
                        match conns.get(&conn_id).map(|c| c.role.clone()) {
                            Some(Role::Client) => {
                                handle_client(
                                    &hub_tx,
                                    &conns,
                                    extension_id,
                                    &mut pending,
                                    &global_slots,
                                    &mut window_slots,
                                    &mut tasks,
                                    conn_id,
                                    msg,
                                )
                            }
                            Some(Role::Extension) => {
                                handle_extension(&conns, &mut pending, &mut tasks, conn_id, msg)
                            }
                            None => {}
                        }
                    }
                    HubMsg::TimedOut { id } => {
                        let task_id = pending.get(&id).and_then(|request| request.task_id.clone());
                        if let Some(task_id) = task_id {
                            let client_conn_id = pending
                                .get(&id)
                                .map(|request| request.client_conn_id);
                            if let Some(request) = pending.get_mut(&id) {
                                request.timed_out = true;
                            }
                            if let Some(task) = tasks.get_mut(&task_id) {
                                // Keep the pending entry until the extension
                                // replies, so task_end cannot release the window
                                // while the browser may still be executing.
                                task.uncertain = true;
                            }
                            if let Some(client_conn_id) = client_conn_id {
                                send(
                                    conns.get(&client_conn_id),
                                    WireMessage::err(
                                        &id,
                                        &format!(
                                            "timeout: extension did not respond in {}s",
                                            REQUEST_TIMEOUT.as_secs()
                                        ),
                                    ),
                                );
                            }
                        } else if let Some(request) = pending.remove(&id) {
                            send(
                                conns.get(&request.client_conn_id),
                                WireMessage::err(
                                    &id,
                                    &format!(
                                        "timeout: extension did not respond in {}s",
                                        REQUEST_TIMEOUT.as_secs()
                                    ),
                                ),
                            );
                        }
                    }
                    HubMsg::Admit { conn_id, extension_id: admitted_ext, msg, task_id, window_key, timeout, global_permit, window_permit } => {
                        // The extension or client may have disconnected while this
                        // request was queued. Dropping the permits here returns the
                        // capacity without sending a stale command.
                        if extension_id != Some(admitted_ext) || !conns.contains_key(&conn_id) {
                            send(conns.get(&conn_id), WireMessage::err(
                                msg.id.as_deref().unwrap_or(""),
                                "request cancelled while waiting for execution slot",
                            ));
                            drop((global_permit, window_permit));
                            continue;
                        }
                        let Some(id) = msg.id.clone() else { continue };
                        let Some(ext_tx) = conns.get(&admitted_ext).map(|c| c.tx.clone()) else {
                            drop((global_permit, window_permit));
                            continue;
                        };
                        let (done_tx, done_rx) = oneshot::channel();
                        pending.insert(id.clone(), PendingRequest {
                            client_conn_id: conn_id,
                            task_id: task_id.clone(),
                            timed_out: false,
                            done_tx,
                            _permits: if let Some(task_id) = task_id.clone() {
                                tasks.insert(task_id, TaskLease {
                                    client_conn_id: conn_id,
                                    window_key,
                                    uncertain: false,
                                    _global_permit: global_permit,
                                    _window_permit: window_permit,
                                });
                                None
                            } else {
                                Some((global_permit, window_permit))
                            },
                        });
                        let _ = ext_tx.send(msg.to_text());
                        let timeout_tx = hub_tx.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = done_rx => {}
                                _ = tokio::time::sleep(timeout) => {
                                    let _ = timeout_tx.send(HubMsg::TimedOut { id });
                                }
                            }
                        });
                    }
                }
            }
            _ = ticker.tick() => {
                if !idle_timeout.is_zero() && last_activity.elapsed() >= idle_timeout {
                    eprintln!(
                        "[hub] idle for {}s, exiting",
                        idle_timeout.as_secs()
                    );
                    std::process::exit(0);
                }
            }
        }
    }
}

fn send(conn: Option<&Conn>, msg: WireMessage) {
    if let Some(conn) = conn {
        let _ = conn.tx.send(msg.to_text());
    }
}

fn request_timeout(msg: &WireMessage) -> Duration {
    msg.timeout_ms
        .map(|ms| Duration::from_millis(ms.clamp(1_000, 10 * 60 * 1_000)))
        .unwrap_or(REQUEST_TIMEOUT)
}

fn handle_client(
    hub_tx: &mpsc::UnboundedSender<HubMsg>,
    conns: &HashMap<u64, Conn>,
    extension_id: Option<u64>,
    pending: &mut Pending,
    global_slots: &std::sync::Arc<Semaphore>,
    window_slots: &mut HashMap<String, std::sync::Arc<Semaphore>>,
    tasks: &mut HashMap<String, TaskLease>,
    client_id: u64,
    msg: WireMessage,
) {
    let Some(id) = msg.id.clone().filter(|id| !id.is_empty()) else {
        send(conns.get(&client_id), WireMessage::err("", "missing id"));
        return;
    };

    // 心跳由 server 直接回应，不经过 extension
    if msg.method.as_deref() == Some("ping") {
        send(
            conns.get(&client_id),
            WireMessage::ok(&id, json!({ "pong": true })),
        );
        return;
    }

    // Read-only inventory calls remain available while all three execution
    // slots are busy, so a client can discover or inspect windows to decide
    // where its next task should go.
    if matches!(
        msg.method.as_deref(),
        Some("list_tabs") | Some("list_windows")
    ) {
        let Some(ext_id) = extension_id else {
            send(
                conns.get(&client_id),
                WireMessage::err(&id, "no extension connected"),
            );
            return;
        };
        let Some(ext_tx) = conns.get(&ext_id).map(|c| c.tx.clone()) else {
            return;
        };
        let mut msg = msg;
        msg.client_id = conns
            .get(&client_id)
            .and_then(|conn| conn.client_id.clone());
        let timeout = request_timeout(&msg);
        let (done_tx, done_rx) = oneshot::channel();
        pending.insert(
            id.clone(),
            PendingRequest {
                client_conn_id: client_id,
                task_id: None,
                timed_out: false,
                done_tx,
                _permits: None,
            },
        );
        let timeout_tx = hub_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = done_rx => {}
                _ = tokio::time::sleep(timeout) => {
                    let _ = timeout_tx.send(HubMsg::TimedOut { id });
                }
            }
        });
        let _ = ext_tx.send(msg.to_text());
        return;
    }

    if msg.method.as_deref() == Some("task_end") {
        let task_id = msg
            .params
            .as_ref()
            .and_then(|params| params.get("task_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if task_id.is_empty() {
            send(
                conns.get(&client_id),
                WireMessage::err(&id, "task_end requires task_id"),
            );
        } else {
            let owned = tasks
                .get(task_id)
                .map(|task| task.client_conn_id == client_id)
                .unwrap_or(false);
            let active_request = pending.values().any(|request| {
                request.client_conn_id == client_id && request.task_id.as_deref() == Some(task_id)
            });
            let uncertain = tasks
                .get(task_id)
                .map(|task| task.uncertain)
                .unwrap_or(false);
            if owned && !active_request && !uncertain {
                tasks.remove(task_id);
                send(
                    conns.get(&client_id),
                    WireMessage::ok(&id, json!({ "ended": true, "task_id": task_id })),
                );
            } else {
                send(
                    conns.get(&client_id),
                    WireMessage::err(&id, "unknown task_id"),
                );
            }
        }
        return;
    }

    let Some(ext_id) = extension_id else {
        send(
            conns.get(&client_id),
            WireMessage::err(&id, "no extension connected"),
        );
        return;
    };

    let Some(conn) = conns.get(&client_id) else {
        return;
    };

    // 盖章来源客户端身份，extension 据此记录标签页归属（close_auto_tabs 按 owner 隔离）
    let mut msg = msg;
    msg.client_id = conn.client_id.clone();
    // window_id is an optional routing hint. Legacy callers without it are
    // isolated by client identity, preserving the old per-agent window rule.
    let window_key = msg
        .params
        .as_ref()
        .and_then(|p| p.get("window_id"))
        .and_then(Value::as_i64)
        .map(|wid| format!("window:{wid}"))
        .unwrap_or_else(|| format!("client:{}", conn.client_id.as_deref().unwrap_or("")));
    let timeout = request_timeout(&msg);
    let window_slot = window_slots
        .entry(window_key.clone())
        .or_insert_with(|| std::sync::Arc::new(Semaphore::new(1)))
        .clone();
    let task_id = msg
        .params
        .as_ref()
        .and_then(|params| params.get("task_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(task_id) = task_id.as_ref() {
        if let Some(task) = tasks.get(task_id) {
            if task.client_conn_id != client_id || task.window_key != window_key {
                send(
                    conns.get(&client_id),
                    WireMessage::err(&id, "task_id is bound to another client or window"),
                );
                return;
            }
            if task.uncertain {
                send(
                    conns.get(&client_id),
                    WireMessage::err(&id, "task is in an unknown execution state after timeout"),
                );
                return;
            }
            let Some(ext_tx) = conns.get(&ext_id).map(|c| c.tx.clone()) else {
                return;
            };
            let (done_tx, done_rx) = oneshot::channel();
            pending.insert(
                id.clone(),
                PendingRequest {
                    client_conn_id: client_id,
                    task_id: Some(task_id.clone()),
                    timed_out: false,
                    done_tx,
                    _permits: None,
                },
            );
            let timeout_tx = hub_tx.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = done_rx => {}
                _ = tokio::time::sleep(timeout) => {
                        let _ = timeout_tx.send(HubMsg::TimedOut { id });
                    }
                }
            });
            let _ = ext_tx.send(msg.to_text());
            return;
        }
    }
    let global_slot = global_slots.clone();
    let hub_tx = hub_tx.clone();
    tokio::spawn(async move {
        let Ok(window_permit) = window_slot.acquire_owned().await else {
            return;
        };
        // Acquire the window first. A task waiting on a busy window must not
        // consume one of the three global execution slots.
        let Ok(global_permit) = global_slot.acquire_owned().await else {
            return;
        };
        let _ = hub_tx.send(HubMsg::Admit {
            conn_id: client_id,
            extension_id: ext_id,
            msg,
            task_id,
            window_key,
            timeout,
            global_permit,
            window_permit,
        });
    });
}

fn handle_extension(
    conns: &HashMap<u64, Conn>,
    pending: &mut Pending,
    tasks: &mut HashMap<String, TaskLease>,
    conn_id: u64,
    msg: WireMessage,
) {
    let Some(id) = msg.id.clone().filter(|id| !id.is_empty()) else {
        return;
    };

    // extension 发来的心跳，server 直接回 pong
    if msg.method.as_deref() == Some("ping") {
        send(
            conns.get(&conn_id),
            WireMessage::ok(&id, json!({ "pong": true })),
        );
        return;
    }

    if let Some(request) = pending.remove(&id) {
        drop(request.done_tx); // 通知超时任务退出
        if let Some(task_id) = request.task_id.as_deref() {
            if let Some(task) = tasks.get_mut(task_id) {
                task.uncertain = false;
            }
        }
        if !request.timed_out {
            send(conns.get(&request.client_conn_id), msg);
        }
    }
}

async fn handle_conn(
    conn_id: u64,
    stream: TcpStream,
    addr: SocketAddr,
    hub_tx: mpsc::UnboundedSender<HubMsg>,
) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(err) => {
            eprintln!("[{addr}] websocket handshake failed: {err}");
            return;
        }
    };
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    // 第一条消息必须是 hello
    let hello = match ws_rx.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<WireMessage>(&text) {
            Ok(msg) => msg,
            Err(err) => {
                let _ = ws_tx
                    .send(Message::Text(
                        json!({ "type": "error", "error": format!("invalid hello: {err}") })
                            .to_string()
                            .into(),
                    ))
                    .await;
                return;
            }
        },
        _ => {
            let _ = ws_tx
                .send(Message::Text(
                    json!({ "type": "error", "error": "expected hello message" })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };

    let role = match hello.role.as_deref() {
        Some("extension") => Role::Extension,
        Some("client") => Role::Client,
        _ => {
            let _ = ws_tx
                .send(Message::Text(
                    json!({ "type": "error", "error": "hello must declare role" })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let name = hello.name.clone().unwrap_or_else(|| "unnamed".to_string());
    let client_id = hello.client_id.clone();
    let _ = hub_tx.send(HubMsg::Register {
        conn_id,
        role,
        name,
        client_id,
        tx: out_tx,
    });

    loop {
        tokio::select! {
            incoming = ws_rx.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = serde_json::from_str::<WireMessage>(&text) {
                            let _ = hub_tx.send(HubMsg::Forward { conn_id, msg });
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
            outgoing = out_rx.recv() => match outgoing {
                Some(msg) => {
                    if ws_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    let _ = hub_tx.send(HubMsg::Unregister { conn_id });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("BRIDGE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let idle_timeout = std::env::var("BRIDGE_IDLE_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::ZERO);
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).await?;
    let idle_note = if idle_timeout.is_zero() {
        String::new()
    } else {
        format!("（空闲 {}s 自动退出）", idle_timeout.as_secs())
    };
    eprintln!("browser-bridge server listening on ws://{addr} {idle_note}");

    let (hub_tx, hub_rx) = mpsc::unbounded_channel();
    tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), idle_timeout));

    let mut next_id: u64 = 0;
    loop {
        let (stream, addr) = listener.accept().await?;
        next_id += 1;
        let hub_tx = hub_tx.clone();
        tokio::spawn(async move { handle_conn(next_id, stream, addr, hub_tx).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_json() {
        let msg: WireMessage = serde_json::from_str(
            r#"{"id":"a1","method":"navigate","params":{"url":"https://example.com"}}"#,
        )
        .unwrap();
        assert_eq!(msg.id.as_deref(), Some("a1"));
        assert_eq!(msg.method.as_deref(), Some("navigate"));
        assert_eq!(
            msg.params
                .as_ref()
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str()),
            Some("https://example.com")
        );
    }

    #[test]
    fn parses_hello_and_response() {
        let hello: WireMessage =
            serde_json::from_str(r#"{"type":"hello","role":"extension","name":"chrome"}"#).unwrap();
        assert_eq!(hello.role.as_deref(), Some("extension"));
        assert_eq!(hello.type_.as_deref(), Some("hello"));

        let resp: WireMessage =
            serde_json::from_str(r#"{"id":"a1","success":true,"result":{"title":"Example"}}"#)
                .unwrap();
        assert_eq!(resp.success, Some(true));
        assert!(resp.error.is_none());
        assert!(resp.method.is_none());
    }

    #[tokio::test]
    async fn routes_request_to_extension_and_response_back_to_client() {
        let (hub_tx, hub_rx) = mpsc::unbounded_channel();
        tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), Duration::ZERO));

        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 1,
                role: Role::Extension,
                name: "chrome".into(),
                client_id: None,
                tx: ext_tx,
            })
            .unwrap();
        let (cli_tx, mut cli_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 2,
                role: Role::Client,
                name: "cli".into(),
                client_id: Some("cli".into()),
                tx: cli_tx,
            })
            .unwrap();

        let req = WireMessage {
            id: Some("r1".into()),
            method: Some("navigate".into()),
            params: Some(json!({ "url": "https://example.com" })),
            ..WireMessage::empty()
        };
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: req,
            })
            .unwrap();

        let forwarded = text_of(ext_rx.recv().await.unwrap()).unwrap();
        let forwarded: WireMessage = serde_json::from_str(&forwarded).unwrap();
        assert_eq!(forwarded.id.as_deref(), Some("r1"));
        assert_eq!(forwarded.method.as_deref(), Some("navigate"));
        // server 应盖章来源客户端身份，供扩展记录标签页归属
        assert_eq!(forwarded.client_id.as_deref(), Some("cli"));

        hub_tx
            .send(HubMsg::Forward {
                conn_id: 1,
                msg: WireMessage::ok("r1", json!({ "title": "Example" })),
            })
            .unwrap();

        let resp = text_of(cli_rx.recv().await.unwrap()).unwrap();
        let resp: WireMessage = serde_json::from_str(&resp).unwrap();
        assert_eq!(resp.id.as_deref(), Some("r1"));
        assert_eq!(resp.success, Some(true));
        assert_eq!(
            resp.result
                .as_ref()
                .and_then(|r| r.get("title"))
                .and_then(|t| t.as_str()),
            Some("Example")
        );
    }

    #[tokio::test]
    async fn errors_when_no_extension_connected() {
        let (hub_tx, hub_rx) = mpsc::unbounded_channel();
        tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), Duration::ZERO));

        let (cli_tx, mut cli_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 2,
                role: Role::Client,
                name: "cli".into(),
                client_id: Some("cli".into()),
                tx: cli_tx,
            })
            .unwrap();

        let req = WireMessage {
            id: Some("r1".into()),
            method: Some("list_tabs".into()),
            params: Some(json!({})),
            ..WireMessage::empty()
        };
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: req,
            })
            .unwrap();

        let resp = text_of(cli_rx.recv().await.unwrap()).unwrap();
        let resp: WireMessage = serde_json::from_str(&resp).unwrap();
        assert_eq!(resp.success, Some(false));
        assert_eq!(resp.error.as_deref(), Some("no extension connected"));
    }

    #[tokio::test]
    async fn limits_global_execution_to_three_tasks() {
        let (hub_tx, hub_rx) = mpsc::unbounded_channel();
        tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), Duration::ZERO));
        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 1,
                role: Role::Extension,
                name: "chrome".into(),
                client_id: None,
                tx: ext_tx,
            })
            .unwrap();
        let (cli_tx, _cli_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 2,
                role: Role::Client,
                name: "cli".into(),
                client_id: Some("cli".into()),
                tx: cli_tx,
            })
            .unwrap();

        for i in 0..4 {
            hub_tx
                .send(HubMsg::Forward {
                    conn_id: 2,
                    msg: WireMessage {
                        id: Some(format!("r{i}")),
                        method: Some("run_script".into()),
                        params: Some(json!({ "window_id": i })),
                        ..WireMessage::empty()
                    },
                })
                .unwrap();
        }

        let mut admitted = Vec::new();
        for _ in 0..3 {
            let msg = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
                .await
                .expect("three tasks should be admitted")
                .expect("extension channel should stay open");
            admitted.push(serde_json::from_str::<WireMessage>(&text_of(msg).unwrap()).unwrap());
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), ext_rx.recv())
                .await
                .is_err()
        );
        for msg in admitted {
            hub_tx
                .send(HubMsg::Forward {
                    conn_id: 1,
                    msg: WireMessage::ok(msg.id.as_deref().unwrap(), json!({})),
                })
                .unwrap();
        }
        let fourth = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
            .await
            .expect("fourth task should run after a slot is released")
            .expect("extension channel should stay open");
        let fourth: WireMessage = serde_json::from_str(&text_of(fourth).unwrap()).unwrap();
        assert!(fourth
            .id
            .as_deref()
            .is_some_and(|id| ["r0", "r1", "r2", "r3"].contains(&id)));
    }

    #[tokio::test]
    async fn serializes_tasks_on_one_window() {
        let (hub_tx, hub_rx) = mpsc::unbounded_channel();
        tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), Duration::ZERO));
        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 1,
                role: Role::Extension,
                name: "chrome".into(),
                client_id: None,
                tx: ext_tx,
            })
            .unwrap();
        let (cli_tx, _cli_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 2,
                role: Role::Client,
                name: "cli".into(),
                client_id: Some("cli".into()),
                tx: cli_tx,
            })
            .unwrap();
        for id in ["a", "b"] {
            hub_tx
                .send(HubMsg::Forward {
                    conn_id: 2,
                    msg: WireMessage {
                        id: Some(id.into()),
                        method: Some("navigate".into()),
                        params: Some(json!({ "window_id": 7 })),
                        ..WireMessage::empty()
                    },
                })
                .unwrap();
        }
        let first = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let first: WireMessage = serde_json::from_str(&text_of(first).unwrap()).unwrap();
        assert!(matches!(first.id.as_deref(), Some("a" | "b")));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), ext_rx.recv())
                .await
                .is_err()
        );
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 1,
                msg: WireMessage::ok("a", json!({})),
            })
            .unwrap();
        let second = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let second: WireMessage = serde_json::from_str(&text_of(second).unwrap()).unwrap();
        assert!(second
            .id
            .as_deref()
            .is_some_and(|id| ["a", "b"].contains(&id)));
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn keeps_a_task_lease_across_multiple_requests() {
        let (hub_tx, hub_rx) = mpsc::unbounded_channel();
        tokio::spawn(hub_loop(hub_rx, hub_tx.clone(), Duration::ZERO));
        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 1,
                role: Role::Extension,
                name: "chrome".into(),
                client_id: None,
                tx: ext_tx,
            })
            .unwrap();
        let (cli_tx, mut cli_rx) = mpsc::unbounded_channel();
        hub_tx
            .send(HubMsg::Register {
                conn_id: 2,
                role: Role::Client,
                name: "cli".into(),
                client_id: Some("cli".into()),
                tx: cli_tx,
            })
            .unwrap();

        let task_request = |id: &str| WireMessage {
            id: Some(id.into()),
            method: Some("run_script".into()),
            params: Some(json!({ "window_id": 4, "task_id": "task-a" })),
            ..WireMessage::empty()
        };
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: task_request("one"),
            })
            .unwrap();
        let first = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<WireMessage>(&text_of(first).unwrap())
                .unwrap()
                .id
                .as_deref(),
            Some("one")
        );
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 1,
                msg: WireMessage::ok("one", json!({})),
            })
            .unwrap();
        let _ = cli_rx.recv().await;

        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: task_request("two"),
            })
            .unwrap();
        let second = tokio::time::timeout(Duration::from_millis(500), ext_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<WireMessage>(&text_of(second).unwrap())
                .unwrap()
                .id
                .as_deref(),
            Some("two")
        );

        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: WireMessage {
                    id: Some("early-end".into()),
                    method: Some("task_end".into()),
                    params: Some(json!({ "task_id": "task-a" })),
                    ..WireMessage::empty()
                },
            })
            .unwrap();
        let early = text_of(cli_rx.recv().await.unwrap()).unwrap();
        assert_eq!(
            serde_json::from_str::<WireMessage>(&early).unwrap().success,
            Some(false)
        );
        hub_tx
            .send(HubMsg::Forward {
                conn_id: 1,
                msg: WireMessage::ok("two", json!({})),
            })
            .unwrap();
        let _ = cli_rx.recv().await;

        hub_tx
            .send(HubMsg::Forward {
                conn_id: 2,
                msg: WireMessage {
                    id: Some("end".into()),
                    method: Some("task_end".into()),
                    params: Some(json!({ "task_id": "task-a" })),
                    ..WireMessage::empty()
                },
            })
            .unwrap();
        let ended = text_of(cli_rx.recv().await.unwrap()).unwrap();
        assert!(serde_json::from_str::<WireMessage>(&ended).unwrap().success == Some(true));
    }
}
