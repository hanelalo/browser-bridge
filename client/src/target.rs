//! 统一的元素定位参数（css / text / xpath + index）。

use clap::Args;
use serde_json::{json, Value};

/// 元素定位参数：css / text / xpath 三种方式，可指定第几个匹配。
#[derive(Args)]
pub struct Target {
    /// 定位值：CSS selector / 可见文本 / XPath
    #[arg(value_name = "TARGET", id = "target")]
    pub value: String,
    /// 定位方式：css | text | xpath
    #[arg(long, default_value = "css")]
    pub by: String,
    /// 第几个匹配（从 0 开始，默认 0）
    #[arg(long, id = "target_index", value_name = "INDEX")]
    pub index: Option<usize>,
}

impl Target {
    pub fn spec(&self) -> Value {
        let mut spec = json!({ "by": self.by, "value": self.value });
        if let Some(i) = self.index {
            spec["index"] = json!(i);
        }
        spec
    }
}

/// 构造可选的 target 定位（press_key / scroll 等可选目标元素时用）。
pub fn optional_target(value: Option<&str>, by: &str, index: Option<usize>) -> Option<Value> {
    value.map(|v| {
        let mut spec = json!({ "by": by, "value": v });
        if let Some(i) = index {
            spec["index"] = json!(i);
        }
        spec
    })
}
