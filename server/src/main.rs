//! WebSocket transport. Scheduling and task ownership live in hub.
mod hub;

use futures_util::{SinkExt, StreamExt};
use hub::{hub_loop, HubMsg};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const DEFAULT_PORT: u16 = 9225;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    Extension,
    Client,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WireMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deadline_ms: Option<u64>,
}
impl WireMessage {
    fn ok(id: &str, result: Value) -> Self {
        Self {
            id: Some(id.into()),
            success: Some(true),
            result: Some(result),
            ..Self::default()
        }
    }
    fn err(id: &str, error: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            success: Some(false),
            error: Some(error.into()),
            ..Self::default()
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
