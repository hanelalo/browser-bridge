//! Reddit 搜索配方：站点知识（选择器）集中在这里，
//! 通过通用原语 navigate + scrape 编排。

use serde_json::{json, Value};

use crate::transport::{urlencode, Bridge};

// 结果项两种渲染形态（preview 带正文预览，sdui 只有标题），内部结构一致
const REDDIT_RESULT_ITEM: &str =
    "[data-testid=\"search-post-with-content-preview\"], [data-testid=\"search-sdui-post\"]";
const REDDIT_RESULT_TITLE: &str = "a[data-testid=\"post-title\"]";
const REDDIT_RESULT_URL: &str = "a[data-testid=\"post-title-text\"]@href";
// 描述：结果项里 sdui-post-unit 的第三个直接子元素（search-telemetry-tracker）里的链接，
// 即帖子正文预览；sdui 形态没有预览，命中不到则为 null
const REDDIT_RESULT_DESC: &str =
    "div[data-testid=\"sdui-post-unit\"] > search-telemetry-tracker > a";
// 点击定位用（语义化：结果卡片上的覆盖链接，顺序与过滤后结果一致）
const REDDIT_RESULT_TARGET: &str = "a[data-testid=\"post-title\"]";

/// Reddit 搜索：导航到搜索结果页，提取 title / description / url。
/// 返回 `{ "tab_id": ..., "results": [...] }`，tab_id 供后续指令在同一标签页上链式操作。
pub async fn redditsearch(
    bridge: &mut Bridge,
    query: &str,
    tab: Option<i32>,
) -> Result<Value, String> {
    let url = format!("https://www.reddit.com/search/?q={}", urlencode(query));
    let nav = bridge
        .request("rs1", "navigate", json!({ "url": url, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);

    let scraped = bridge
        .request(
            "rs2",
            "scrape",
            json!({
                "item": REDDIT_RESULT_ITEM,
                "fields": {
                    "title": REDDIT_RESULT_TITLE,
                    "url": REDDIT_RESULT_URL,
                    "description": REDDIT_RESULT_DESC,
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
            // 定位：结果卡片上的覆盖链接，按过滤后的结果序号定位
            let target = json!({
                "by": "css",
                "value": REDDIT_RESULT_TARGET,
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
