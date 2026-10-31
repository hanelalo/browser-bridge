//! Google Trends（新版探索页）：站点知识集中在 `googletrends.js`。
//!
//! 新版页面是"数据预取 + 卡片懒挂载"：查询卡片不进入视口时连 <table> 都不存在，
//! 滚动容器还带 scroll-behavior:smooth。脚本先滚到卡片、关掉动画，再按状态等待
//! 骨架填充，最后用页面自带的分页文案翻完榜单（纯前端）。
//! 编排侧只做两件事：等页面壳就绪，然后跑一次采集脚本（脚本内部 45s deadline）。

use serde_json::{json, Value};
use std::time::Duration;

use crate::transport::{urlencode, Bridge};

const DEFAULT_DATE: &str = "today 1-m";
const DEFAULT_GEO: &str = "Worldwide";
const SCRIPT: &str = include_str!("googletrends.js");
const COMPARE_SCRIPT: &str = include_str!("googletrends_compare.js");
/// 页面内的采集脚本自带 45s 状态 deadline，外层给足余量
const GOOGLE_TRENDS_SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);
/// 页面壳就绪的轮询次数（500ms/次，共 45s）
const READY_POLLS: usize = 90;

fn query_tables_available(top_table_found: bool, rising_table_found: bool) -> bool {
    top_table_found || rising_table_found
}

#[cfg(test)]
mod tests {
    use super::{
        compare_script, normalize_date, query_tables_available, trends_script,
        GOOGLE_TRENDS_SCRIPT_TIMEOUT,
    };

    #[test]
    fn query_tables_are_available_even_when_empty() {
        assert!(query_tables_available(true, true));
    }

    #[test]
    fn query_tables_are_unavailable_when_neither_table_was_found() {
        assert!(!query_tables_available(false, false));
    }

    #[test]
    fn trends_script_timeout_exceeds_default_bridge_request_timeout() {
        assert!(GOOGLE_TRENDS_SCRIPT_TIMEOUT > crate::transport::RESPONSE_TIMEOUT);
    }

    #[test]
    fn trends_script_injects_date_spec() {
        let script = trends_script("today 3-m");
        assert!(script.contains(r#"const DATE_SPEC = "today 3-m";"#));
        assert!(!script.contains("__DATE_SPEC__"));
    }

    #[test]
    fn compare_script_injects_terms_and_date() {
        let script = compare_script(&["coffee".to_string(), "tea".to_string()], "today 3-m");
        assert!(script.contains(r#"const TERMS = ["coffee","tea"];"#));
        assert!(script.contains(r#"const DATE_SPEC = "today 3-m";"#));
        assert!(!script.contains("__TERMS__"));
        assert!(!script.contains("__DATE_SPEC__"));
    }

    #[test]
    fn normalize_date_keeps_supported_formats() {
        for date in [
            "today 1-m",
            "today 12-m",
            "today 1-y",
            "all",
            "2025-01-01 2025-06-30",
        ] {
            assert_eq!(normalize_date(date), date);
        }
    }

    #[test]
    fn normalize_date_trims_and_falls_back() {
        assert_eq!(normalize_date("  today 3-m  "), "today 3-m");
        assert_eq!(normalize_date(""), "today 1-m");
        assert_eq!(normalize_date("yesterday"), "today 1-m");
        assert_eq!(normalize_date("today x-m"), "today 1-m");
        assert_eq!(normalize_date("2025-01-01"), "today 1-m");
    }
}

/// 规范化 Google Trends 的 date 参数：去空白、识别合法格式、非法回落默认值。
/// 合法格式：today N-d / today N-m / today N-y / all / YYYY-MM-DD YYYY-MM-DD。
fn normalize_date(date: &str) -> String {
    let d = date.trim();
    if d.is_empty() {
        return DEFAULT_DATE.to_string();
    }
    let today_ok = regex_dmy(d);
    let range_ok = regex_range(d);
    if d == "all" || today_ok || range_ok {
        d.to_string()
    } else {
        DEFAULT_DATE.to_string()
    }
}

/// 匹配 today N-d / today N-m / today N-y
fn regex_dmy(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("today ") else {
        return false;
    };
    let Some(idx) = rest.find('-') else {
        return false;
    };
    let (num, unit) = (&rest[..idx], &rest[idx + 1..]);
    matches!(unit, "d" | "m" | "y") && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
}

/// 匹配 YYYY-MM-DD YYYY-MM-DD
fn regex_range(s: &str) -> bool {
    let mut it = s.split_whitespace();
    match (it.next(), it.next(), it.next()) {
        (Some(a), Some(b), None) => is_date(a) && is_date(b),
        _ => false,
    }
}

/// 匹配 YYYY-MM-DD
fn is_date(s: &str) -> bool {
    let mut it = s.splitn(3, '-');
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(y), Some(m), Some(d), None) => {
            y.len() == 4
                && m.len() == 2
                && d.len() == 2
                && y.chars().all(|c| c.is_ascii_digit())
                && m.chars().all(|c| c.is_ascii_digit())
                && d.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// 把日期写进采集脚本（脚本用它推导趋势点的日期）。
fn trends_script(date_spec: &str) -> String {
    let date_lit = serde_json::to_string(date_spec).unwrap_or_else(|_| "\"today 1-m\"".into());
    SCRIPT.replace("__DATE_SPEC__", &date_lit)
}

/// 等新标签页真正落到 trends.google.com 且应用壳起来。
/// new_tab 在导航提交前就返回，直接跑采集脚本可能注入到空白文档。
async fn wait_page_ready(bridge: &mut Bridge, tab_id: &Value) -> Result<(), String> {
    for _ in 0..READY_POLLS {
        let probe = bridge
            .request(
                "gt-ready",
                "run_script",
                json!({
                    "tab_id": tab_id,
                    "code": "location.hostname === 'trends.google.com' && !!(document.querySelector('main') || document.querySelector('c-wiz'))",
                }),
            )
            .await;
        if probe
            .ok()
            .and_then(|v| v.get("result").and_then(Value::as_bool))
            == Some(true)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("Google Trends 页面未就绪（可能被限制或需要验证）".into())
}

/// 关掉本次开的标签页；失败不影响主流程。
async fn close_tab_quietly(bridge: &mut Bridge, tab_id: &Value) {
    let _ = bridge
        .request("gt-close", "close_tab", json!({ "tab_id": tab_id }))
        .await;
}

fn shape_result(data: &Value, tab_id: Value, query: &str, date: &str, geo: &str) -> Value {
    let top_table_available = data
        .get("top_table_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rising_table_available = data
        .get("rising_table_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let regions_available = data
        .get("regions_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tables_available = query_tables_available(top_table_available, rising_table_available)
        || data
            .get("tables_available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let mut out = json!({
        "tab_id": tab_id,
        "query": query,
        "date": date,
        "geo": geo,
        "trend": data.get("trend").cloned().unwrap_or_else(|| json!([])),
        "top": data.get("top").cloned().unwrap_or_else(|| json!([])),
        "rising": data.get("rising").cloned().unwrap_or_else(|| json!([])),
        "regions": data.get("regions").cloned().unwrap_or_else(|| json!([])),
        "top_table_available": top_table_available,
        "rising_table_available": rising_table_available,
        "tables_available": tables_available,
        "regions_available": regions_available,
        "hydration": data.get("hydration").cloned().unwrap_or(Value::Null),
    });
    if !tables_available {
        out["note"] = json!(
            "当前会话 Google 显示的是 Gemini 变体界面，热门/上升关键词表未渲染（趋势数据不受影响）；如在经典界面下运行则会返回完整表格"
        );
    }
    out
}

/// 打开新版探索页并采集：趋势序列 + 热门/上升查询 + 地区热度。
/// 返回 `{ "tab_id": ..., "query": ..., "date": ..., "geo": ..., "trend": [...], "top": [...], "rising": [...], "regions": [...] }`。
/// 页面壳或图表始终不出现时换新标签页重试一次；表格缺失不算失败（无数据词本来就可能没有榜单）。
pub async fn googletrends(
    bridge: &mut Bridge,
    query: &str,
    date: &str,
    geo: &str,
) -> Result<Value, String> {
    let date = normalize_date(date);
    let geo = if geo.trim().is_empty() {
        DEFAULT_GEO
    } else {
        geo
    };
    let url = format!(
        "https://trends.google.com/explore?q={}&date={}&geo={}&hl=zh-CN",
        urlencode(query),
        urlencode(&date),
        urlencode(geo)
    );
    let script = trends_script(&date);
    let mut failure = String::from("未获取到趋势数据");

    for attempt in 0..2 {
        let nav = bridge
            .request("gt1", "new_tab", json!({ "url": url }))
            .await?;
        let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);
        if !tab_id.is_number() {
            failure = "new_tab 未返回 tab_id".into();
            continue;
        }

        if let Err(err) = wait_page_ready(bridge, &tab_id).await {
            close_tab_quietly(bridge, &tab_id).await;
            failure = err;
            continue;
        }

        let result = bridge
            .request_with_timeout(
                "gt3",
                "run_script",
                json!({ "code": script, "tab_id": tab_id }),
                GOOGLE_TRENDS_SCRIPT_TIMEOUT,
            )
            .await;
        let data = match result {
            Ok(resp) => resp.get("result").cloned().unwrap_or(Value::Null),
            Err(err) => {
                close_tab_quietly(bridge, &tab_id).await;
                failure = err;
                if attempt == 1 {
                    break;
                }
                continue;
            }
        };

        // 脚本只在致命场景返回 error（页面壳没找到 / 图表没出来），都值得换新标签页再试
        if let Some(err) = data.get("error").and_then(Value::as_str) {
            close_tab_quietly(bridge, &tab_id).await;
            failure = err.to_string();
            if attempt == 1 {
                break;
            }
            continue;
        }

        return Ok(shape_result(&data, tab_id, query, &date, geo));
    }

    Err(format!("googletrends: {failure}"))
}

/// 对比模式脚本：解析多折线（每个关键词一条线，mask 序号对应查询顺序），返回各词趋势序列。
/// 把关键词与日期写进对比脚本（脚本只读首屏多折线图，不需要滚动）。
fn compare_script(terms: &[String], date_spec: &str) -> String {
    let date_lit = serde_json::to_string(date_spec).unwrap_or_else(|_| "\"today 1-m\"".into());
    let terms_json = serde_json::to_string(terms).unwrap_or_else(|_| "[]".into());
    COMPARE_SCRIPT
        .replace("__TERMS__", &terms_json)
        .replace("__DATE_SPEC__", &date_lit)
}

/// Google Trends 关键词对比：多个关键词的走势对比（共享 0-100 刻度，不返回热门/上升表）。
/// 返回 `{ "tab_id": ..., "date": ..., "geo": ..., "series": [{ "term": ..., "trend": [...] }] }`。
pub async fn googletrends_compare(
    bridge: &mut Bridge,
    terms: &[String],
    date: &str,
    geo: &str,
) -> Result<Value, String> {
    if terms.is_empty() {
        return Err("googletrends-compare: 至少需要一个关键词".to_string());
    }
    let date = normalize_date(date);
    let geo = if geo.trim().is_empty() {
        DEFAULT_GEO
    } else {
        geo
    };
    let q = terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!(
        "https://trends.google.com/explore?q={}&date={}&geo={}&hl=zh-CN",
        urlencode(&q),
        urlencode(&date),
        urlencode(geo)
    );

    let script = compare_script(terms, &date);
    let mut failure = String::from("未获取到对比数据");

    for attempt in 0..2 {
        let nav = bridge
            .request("gtc1", "new_tab", json!({ "url": url }))
            .await?;
        let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);
        if !tab_id.is_number() {
            failure = "new_tab 未返回 tab_id".into();
            continue;
        }

        if let Err(err) = wait_page_ready(bridge, &tab_id).await {
            close_tab_quietly(bridge, &tab_id).await;
            failure = err;
            continue;
        }

        let result = bridge
            .request_with_timeout(
                "gtc2",
                "run_script",
                json!({ "code": script, "tab_id": tab_id }),
                GOOGLE_TRENDS_SCRIPT_TIMEOUT,
            )
            .await;
        let data = match result {
            Ok(resp) => resp.get("result").cloned().unwrap_or(Value::Null),
            Err(err) => {
                close_tab_quietly(bridge, &tab_id).await;
                failure = err;
                if attempt == 1 {
                    break;
                }
                continue;
            }
        };

        // 只在页面/图表始终不出现时重试（脚本 error），数据为空不算失败
        if let Some(err) = data.get("error").and_then(Value::as_str) {
            close_tab_quietly(bridge, &tab_id).await;
            failure = err.to_string();
            if attempt == 1 {
                break;
            }
            continue;
        }

        return Ok(json!({
            "tab_id": tab_id,
            "terms": terms,
            "date": date,
            "geo": geo,
            "series": data.get("series").cloned().unwrap_or_else(|| json!([])),
            "hydration": data.get("hydration").cloned().unwrap_or(Value::Null),
        }));
    }

    Err(format!("googletrends-compare: {failure}"))
}
