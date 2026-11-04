//! MCP over HTTP (Streamable HTTP transport) 内置在 bridge-server 里。
//!
//! 设计要点：
//! - 复用 `bridge-mcp` 的同一套工具实现（`BridgeMcp`），不复制任何 tool 代码；
//! - 工具调用走本机回环 WebSocket（client 角色）回到同一个 hub，
//!   因此 hub 的调度/限额/任务语义完全不变，原有 WS 接口一字未改；
//! - 监听独立端口（默认 127.0.0.1:9226），原有 9225 上的 WS 协议不受影响。

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use bridge_mcp::BridgeMcp;
use http::{header, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde_json::json;
use tokio::net::TcpListener;
use tower::Service;

type HttpResponse = Response<BoxBody<Bytes, Infallible>>;

/// MCP 端点路径（客户端配置成 `http://127.0.0.1:9226/mcp`）
pub(crate) const MCP_PATH: &str = "/mcp";

/// 在 `addr` 上启动 MCP over HTTP。绑定失败只打印告警，不影响 WS hub。
pub(crate) async fn serve(addr: SocketAddr, ws_url: String) {
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("[mcp-http] 无法监听 http://{addr}{MCP_PATH}: {error}（WS hub 不受影响）");
            return;
        }
    };

    let client_id = BridgeMcp::new_client_id("mcp-http");
    let mcp = match connect_bridge(&ws_url, &client_id).await {
        Ok(mcp) => mcp,
        Err(error) => {
            eprintln!("[mcp-http] 无法连接本机 hub {ws_url}: {error}；HTTP MCP 不可用");
            return;
        }
    };

    let service: StreamableHttpService<BridgeMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(mcp.clone()),
            Arc::new(LocalSessionManager::default()),
            // 无状态：不签发 Mcp-Session-Id，也不校验它。
            // 否则 rmcp 的会话默认 5 分钟无活动就被回收，客户端拿着旧 session id
            // 再来请求就是 404 "Session not found"（server 重启同样会命中）。
            // 本服务的工具状态都在 hub / 工具参数里，不依赖 MCP 会话，
            // 所以逐请求直接服务即可。
            StreamableHttpServerConfig::default().with_legacy_session_mode(false),
        );
    eprintln!("[mcp-http] MCP over HTTP listening on http://{addr}{MCP_PATH}");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                eprintln!("[mcp-http] accept failed: {error}");
                continue;
            }
        };
        let service = service.clone();
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |request: Request<Incoming>| {
                let mut service = service.clone();
                async move { Ok::<HttpResponse, Infallible>(dispatch(&mut service, request).await) }
            });
            if let Err(error) = auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, handler)
                .await
            {
                // 浏览器/客户端正常断开会走到这里，不是错误现场。
                eprintln!("[mcp-http] {peer} connection closed: {error}");
            }
        });
    }
}

/// 回环连接本机 hub（绝不自动拉起新的 bridge-server 进程）。
async fn connect_bridge(ws_url: &str, client_id: &str) -> Result<BridgeMcp, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match BridgeMcp::connect_no_spawn(ws_url, client_id).await {
            Ok(mcp) => return Ok(mcp),
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn dispatch(
    service: &mut StreamableHttpService<BridgeMcp, LocalSessionManager>,
    request: Request<Incoming>,
) -> HttpResponse {
    let path = request.uri().path().to_string();
    if path == "/health" {
        return json_response(
            StatusCode::OK,
            json!({"ok": true, "mcp": MCP_PATH}),
        );
    }
    if path != MCP_PATH && !path.starts_with(&format!("{MCP_PATH}/")) {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({"error": format!("unknown path {path}; MCP endpoint is {MCP_PATH}")}),
        );
    }
    let method = request.method().clone();
    let response = match service.call(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    // 只有异常状态才打日志：正常调用不污染日志，出问题时能直接看出端点行为。
    if response.status().is_client_error() || response.status().is_server_error() {
        eprintln!(
            "[mcp-http] {method} {path} -> {} {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or("")
        );
    }
    response
}

fn json_response(status: StatusCode, value: serde_json::Value) -> HttpResponse {
    let body = Full::new(Bytes::from(value.to_string()))
        .map_err(|never| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .expect("static response builds")
}
