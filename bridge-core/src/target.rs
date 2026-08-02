//! 元素定位参数构造（与 clap 解耦，CLI 与 MCP 共用）。

use serde_json::{json, Value};

/// 构造统一的元素定位：`{ "by": ..., "value": ..., "index": ... }`。
pub fn spec(by: &str, value: &str, index: Option<usize>) -> Value {
    let mut spec = json!({ "by": by, "value": value });
    if let Some(i) = index {
        spec["index"] = json!(i);
    }
    spec
}

/// 构造可选的 target 定位（press_key / scroll 等可选目标元素时用）。
pub fn optional_target(value: Option<&str>, by: &str, index: Option<usize>) -> Option<Value> {
    value.map(|v| spec(by, v, index))
}
