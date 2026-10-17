//! 经典版 Google Trends：读取真实浏览器中已渲染的表格，通过页面控件切换和翻页。
//! 独立于 Gemini 版 SVG 配方，保留页面显示的时间标签及完整榜单。

use serde_json::{json, Value};
use std::time::Duration;

use crate::transport::{urlencode, Bridge};

const DEFAULT_DATE: &str = "today 1-m";
const SCRIPT: &str = include_str!("oldgoogletrends.js");
const COMPARE_SCRIPT: &str = include_str!("oldgoogletrends_compare.js");
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);
/// 经典版 Explore 对比最多 5 个搜索字词（页面自身上限）。
const MAX_COMPARE_TERMS: usize = 5;

fn normalize_date(date: &str) -> Result<String, String> {
    let date = date.split_whitespace().collect::<Vec<_>>().join(" ");
    if date.is_empty() {
        return Ok(DEFAULT_DATE.into());
    }
    let relative = ["today ", "now "].iter().any(|prefix| {
        date.strip_prefix(prefix)
            .and_then(|s| s.split_once('-'))
            .map(|(n, unit)| {
                n.bytes().all(|b| b.is_ascii_digit())
                    && n.parse::<u32>().is_ok_and(|n| n > 0)
                    && if *prefix == "now " {
                        matches!(unit, "h" | "d")
                    } else {
                        matches!(unit, "d" | "m" | "y")
                    }
            })
            .unwrap_or(false)
    });
    let range = date
        .split_once(' ')
        .is_some_and(|(start, end)| valid_day(start) && valid_day(end) && start <= end);
    if date == "all" || relative || range {
        Ok(date)
    } else {
        Err(
            "oldgoogletrends: 无效 date；使用 now 7-d / today 1-m / all / YYYY-MM-DD YYYY-MM-DD"
                .into(),
        )
    }
}

fn valid_day(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = s[..4].parse().unwrap_or(0);
    let month: usize = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..].parse().unwrap_or(0);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        0,
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    year > 0 && month > 0 && month <= 12 && day > 0 && day <= days[month]
}

fn normalize_geo(geo: &str) -> String {
    let geo = geo.trim();
    if geo.is_empty() || geo.eq_ignore_ascii_case("worldwide") {
        String::new()
    } else {
        geo.to_ascii_uppercase()
    }
}

fn explore_url(query: &str, date: &str, geo: &str) -> String {
    let mut url = format!(
        "https://trends.google.com/trends/explore?q={}&date={}&legacy&hl=zh-CN",
        urlencode(query),
        urlencode(date)
    );
    // 经典版全球用空地区表示，不能传新版的 geo=Worldwide。
    if !geo.is_empty() {
        url.push_str(&format!("&geo={}", urlencode(geo)));
    }
    url
}

/// new_tab returns before navigation commits. Poll with separate short scripts so
/// the long-running collector never gets attached to the initial blank document.
async fn wait_explore_ready(bridge: &mut Bridge, tab_id: &Value) -> Result<(), String> {
    for _ in 0..60 {
        let probe = bridge.request("ogt-ready", "run_script", json!({
            "tab_id": tab_id,
            "code": "location.hostname === 'trends.google.com' && document.readyState === 'complete' && !!document.querySelector('[ng-app=trendsApp]')"
        })).await;
        if probe
            .ok()
            .and_then(|v| v.get("result").and_then(Value::as_bool))
            == Some(true)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("经典版页面未就绪（可能被限制或需要验证）".into())
}

/// 运行长耗时采集脚本并校验返回结构，页面级错误转为 Err。
async fn run_collector(bridge: &mut Bridge, tab_id: &Value, code: String) -> Result<Value, String> {
    let resp = bridge
        .request_with_timeout(
            "ogt2",
            "run_script",
            json!({ "code": code, "tab_id": tab_id }),
            SCRIPT_TIMEOUT,
        )
        .await?;
    let data = resp
        .get("result")
        .filter(|v| v.is_object())
        .ok_or("页面未返回有效数据")?;
    if let Some(error) = data.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(data.clone())
}

/// 新建经典版 Explore 标签页，通过 run_script 读取页面 DOM。
pub async fn oldgoogletrends(
    bridge: &mut Bridge,
    query: &str,
    date: &str,
    geo: &str,
) -> Result<Value, String> {
    let query = query.trim();
    if query.is_empty() || query.contains(',') {
        return Err("oldgoogletrends: 请提供一个非空关键词（不支持逗号分隔的多词对比）".into());
    }
    let date = normalize_date(date)?;
    let geo = normalize_geo(geo);
    let nav = bridge
        .request(
            "ogt1",
            "new_tab",
            json!({
                "url": explore_url(query, &date, &geo)
            }),
        )
        .await?;
    let tab_id = nav
        .get("tab_id")
        .filter(|v| v.is_number())
        .cloned()
        .ok_or("oldgoogletrends: new_tab 未返回 tab_id")?;
    if let Err(error) = wait_explore_ready(bridge, &tab_id).await {
        let _ = bridge
            .request("ogt-close", "close_tab", json!({ "tab_id": tab_id }))
            .await;
        return Err(format!("oldgoogletrends: {error}"));
    }
    let result = run_collector(bridge, &tab_id, format!("({SCRIPT})()"))
        .await
        .and_then(|data| {
            if !data.get("trend").is_some_and(Value::is_array) {
                return Err("页面未返回趋势序列".into());
            }
            Ok(data)
        });
    match result {
        Ok(mut data) => {
            data["tab_id"] = tab_id;
            data["query"] = json!(query);
            data["date"] = json!(date);
            data["geo"] = json!(if geo.is_empty() { "Worldwide" } else { &geo });
            Ok(data)
        }
        Err(error) => {
            // 只清理本次失败创建的页；不自动重试限流/验证错误。
            let _ = bridge
                .request("ogt3", "close_tab", json!({ "tab_id": tab_id }))
                .await;
            Err(format!("oldgoogletrends: {error}"))
        }
    }
}

/// 经典版 Google Trends 关键词对比：多词共享 0-100 刻度的趋势序列（legacy 页面 DOM 表格）。
/// 返回 `{ "tab_id": ..., "terms": [...], "date": ..., "geo": ..., "series": [{ "term": ..., "trend": [{ "date", "value" }] }] }`。
pub async fn oldgoogletrends_compare(
    bridge: &mut Bridge,
    terms: &[String],
    date: &str,
    geo: &str,
) -> Result<Value, String> {
    let terms = terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err("oldgoogletrends_compare: 至少需要一个关键词".into());
    }
    if let Some(term) = terms.iter().find(|t| t.contains(',')) {
        return Err(format!(
            "oldgoogletrends_compare: 关键词不能包含逗号：{term}"
        ));
    }
    if terms.len() > MAX_COMPARE_TERMS {
        return Err(format!(
            "oldgoogletrends_compare: 最多支持 {MAX_COMPARE_TERMS} 个关键词"
        ));
    }
    let date = normalize_date(date)?;
    let geo = normalize_geo(geo);
    let nav = bridge
        .request(
            "ogtc1",
            "new_tab",
            json!({
                "url": explore_url(&terms.join(","), &date, &geo)
            }),
        )
        .await?;
    let tab_id = nav
        .get("tab_id")
        .filter(|v| v.is_number())
        .cloned()
        .ok_or("oldgoogletrends_compare: new_tab 未返回 tab_id")?;
    if let Err(error) = wait_explore_ready(bridge, &tab_id).await {
        let _ = bridge
            .request("ogtc-close", "close_tab", json!({ "tab_id": tab_id }))
            .await;
        return Err(format!("oldgoogletrends_compare: {error}"));
    }
    let terms_json = serde_json::to_string(&terms).unwrap_or_else(|_| "[]".into());
    let result = run_collector(bridge, &tab_id, format!("({COMPARE_SCRIPT})({terms_json})"))
        .await
        .and_then(|data| {
            if !data.get("series").is_some_and(Value::is_array) {
                return Err("页面未返回对比序列".into());
            }
            Ok(data)
        });
    match result {
        Ok(mut data) => {
            data["tab_id"] = tab_id;
            data["terms"] = json!(terms);
            data["date"] = json!(date);
            data["geo"] = json!(if geo.is_empty() { "Worldwide" } else { &geo });
            Ok(data)
        }
        Err(error) => {
            let _ = bridge
                .request("ogtc3", "close_tab", json!({ "tab_id": tab_id }))
                .await;
            Err(format!("oldgoogletrends_compare: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_hourly_and_daily_ranges() {
        for date in [
            "now 7-d",
            "now 1-h",
            "today 1-m",
            "all",
            "2024-02-29 2024-03-01",
        ] {
            assert_eq!(normalize_date(date).unwrap(), date);
        }
        assert_eq!(normalize_date(" now   7-d ").unwrap(), "now 7-d");
        assert_eq!(normalize_date("").unwrap(), DEFAULT_DATE);
        for date in [
            "now 0-d",
            "now 1-y",
            "bogus",
            "2025-02-29 2025-03-01",
            "2026-10-05 2026-10-01",
        ] {
            assert!(normalize_date(date).is_err(), "{date}");
        }
    }

    #[test]
    fn builds_legacy_url_with_worldwide_and_encoded_query() {
        let geo = normalize_geo(" Worldwide ");
        assert_eq!(geo, "");
        assert_eq!(explore_url("ai image detector", "now 7-d", &geo),
            "https://trends.google.com/trends/explore?q=ai+image+detector&date=now+7-d&legacy&hl=zh-CN");
        assert!(explore_url("a&b #中文", "all", &normalize_geo(" us "))
            .ends_with("q=a%26b+%23%E4%B8%AD%E6%96%87&date=all&legacy&hl=zh-CN&geo=US"));
    }

    #[test]
    fn builds_legacy_compare_url_with_comma_joined_terms() {
        assert_eq!(
            explore_url(&["coffee", "black tea"].join(","), "today 1-m", &normalize_geo("worldwide")),
            "https://trends.google.com/trends/explore?q=coffee%2Cblack+tea&date=today+1-m&legacy&hl=zh-CN"
        );
        assert!(explore_url(&["a", "b"].join(","), "now 7-d", &normalize_geo("jp"))
            .ends_with("q=a%2Cb&date=now+7-d&legacy&hl=zh-CN&geo=JP"));
    }
}
