//! 与 bridge server 的传输层：连接、请求-响应、URL 编码。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);

pub type BridgeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 连接 server 并完成 hello 握手。
pub async fn connect_bridge(server: &str) -> Result<BridgeStream, String> {
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
    Ok(ws)
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
