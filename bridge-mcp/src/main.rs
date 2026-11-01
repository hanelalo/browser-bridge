//! Browser Bridge MCP server：stdio 传输入口。
//! 工具实现与 HTTP 传输共用 `bridge-mcp` 的 lib（见 `src/lib.rs`）。

use bridge_mcp::BridgeMcp;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("BRIDGE_SERVER")
        .unwrap_or_else(|_| bridge_mcp::DEFAULT_SERVER.to_string());
    // 每个 MCP 进程一个稳定身份（多 agent 共享浏览器时用于标签页归属隔离）
    let client_id = BridgeMcp::new_client_id("mcp");
    let mcp = BridgeMcp::connect(&server, &client_id)
        .await
        .map_err(|error| format!("无法连接 {server}: {error}"))?;
    eprintln!("bridge-mcp connected to {server}");

    // Chrome is shared by concurrent clients. A process ending must never
    // quit the browser used by other tasks; task leases drain on disconnect.
    let service = mcp.serve(rmcp::transport::stdio()).await?;
    let waiting = service.waiting().await;
    waiting?;
    Ok(())
}
