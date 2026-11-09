//! 站点配方：每个文件封装一个站点的搜索/提取逻辑，
//! 只依赖通用协议指令（navigate / scrape / click 等），扩展与协议保持通用。

use serde_json::{json, Value};

use crate::transport::Bridge;

pub mod googlesearch;
pub mod googletrends;
pub mod oldgoogletrends;
pub mod querydomains;
pub mod redditsearch;
pub mod youtubeinfo;
pub mod youtuberinfo;
pub mod youtubesearch;

/// 配方成功后调度延迟关闭：server 在随机 10~20 秒后关闭本次自动打开的标签页。
///
/// - 定时在 server 侧执行（`schedule_tab_close`），不占用任务租约与窗口锁，
///   CLI / MCP 调用方可立即拿到结果退出，进程不留挂。
/// - `tab_param` 非空表示调用方显式指定了已有标签页（可能是用户自己的页面），不关。
/// - 调度失败静默忽略，不影响配方结果；server 重启会丢失待关闭条目（标签页留着，可
///   用 `close_auto_tabs` 兜底清理）。
pub async fn schedule_auto_close(bridge: &Bridge, tab_param: Option<i32>, tab_id: &Value) {
    if tab_param.is_some() {
        return;
    }
    let Some(tab_id) = tab_id.as_i64() else {
        return;
    };
    // 伪随机延迟：取时钟纳秒低位映射到 10~20 秒，避免引入 rand 依赖
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let delay_ms = 10_000 + u64::from(nanos % 10_001);
    let _ = bridge
        .request(
            "auto-close",
            "schedule_tab_close",
            json!({ "tab_id": tab_id, "delay_ms": delay_ms }),
        )
        .await;
}
