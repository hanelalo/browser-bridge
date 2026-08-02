//! Google 搜索配方：站点知识（选择器）集中在这里，
//! 通过通用原语 navigate + scrape 编排。

use serde_json::{json, Value};

use crate::transport::{urlencode, Bridge};

// 提取用容器：#rso 的直接子 div，每个即一条搜索结果
const GOOGLE_RESULT_ITEM: &str = "#rso > div";
const GOOGLE_RESULT_TITLE: &str = "h3";
const GOOGLE_RESULT_URL: &str = "a@href";
// 描述：每条结果下 data-sncf="1" 的子 div 里的文本容器（属性定位，不依赖混淆 class）
const GOOGLE_RESULT_DESC: &str = "div[data-sncf='1'] > div";
// 点击定位用（语义化：#rso 结果区里包含 h3 标题的链接，顺序与过滤后结果一致）
const GOOGLE_RESULT_TARGET: &str = "#rso a:has(h3)";

/// Google 搜索：导航到搜索结果页，提取 title / description / url。
/// 返回 `{ "tab_id": ..., "results": [...] }`，tab_id 供后续指令在同一标签页上链式操作。
pub async fn googlesearch(
    bridge: &mut Bridge,
    query: &str,
    tab: Option<i32>,
) -> Result<Value, String> {
    let url = format!("https://www.google.com/search?q={}", urlencode(query));
    let nav = bridge
        .request("gs1", "navigate", json!({ "url": url, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);

    let scraped = bridge
        .request(
            "gs2",
            "scrape",
            json!({
                "item": GOOGLE_RESULT_ITEM,
                "fields": {
                    "title": GOOGLE_RESULT_TITLE,
                    "url": GOOGLE_RESULT_URL,
                    "description": GOOGLE_RESULT_DESC,
                },
                "timeout": 10_000,
                "tab_id": tab,
            }),
        )
        .await?;

    let items = scraped
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut result_index = 0usize;
    let results: Vec<Value> = items
        .into_iter()
        .filter(|it| {
            it.get("title")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .map(|it| {
            // 定位：语义化的结果链接（#rso a:has(h3)），按过滤后的结果序号定位
            let target = json!({
                "by": "css",
                "value": GOOGLE_RESULT_TARGET,
                "index": result_index,
            });
            result_index += 1;
            json!({
                "title": it.get("title").cloned().unwrap_or(Value::Null),
                "description": it.get("description").cloned().unwrap_or(Value::Null),
                "url": it.get("url").cloned().unwrap_or(Value::Null),
                "target": target,
            })
        })
        .collect();

    Ok(json!({
        "tab_id": tab_id,
        "results": results,
    }))
}
