//! YouTube 搜索配方：数据直接取自页面 HTML 内嵌的 ytInitialData（导航返回的
//! HTML 即包含完整首屏数据，与标签页是否可见无关），不足 max 条时用 InnerTube
//! continuation 续取——与 yt-dlp 同源，不需要页面渲染、滚动或窗口操作。
//!
//! 筛选 token 于 2026-08 在真实浏览器里逐一点击"过滤"面板实测：
//! - 上传日期：今天 `EgIIAg==` / 本周 `EgIIAw==` / 本月 `EgIIBA==` / 今年 `EgIIBQ==`
//! - 优先顺序：相关程度（默认，不加参数）/ 热门程度 `CAM=`
//! - 组合（上传日期 + 热门程度）：`CAMSBAgCEAE=`（今天）/ `CAMSBAgDEAE=`（本周）
//!   / `CAMSBAgEEAE=`（本月）/ `CAMSBAgFEAE=`（今年）

use serde_json::{json, Value};

use crate::transport::{urlencode, Bridge};

/// 点击定位用：结果卡片上的标题链接，按结果序号定位
const YT_RESULT_TARGET: &str = "a#video-title";

/// 上传日期 + 优先顺序 → YouTube 搜索 `sp` 参数（base64，拼 URL 时 percent-encode 一次）。
/// 相关程度是默认排序，不加参数；`--time any` + 热门程度时用 `CAM=`。
fn filter_sp(time: &str, sort: &str) -> Result<Option<&'static str>, String> {
    let date = match time {
        "any" | "" => None,
        "today" => Some(0),
        "week" => Some(1),
        "month" => Some(2),
        "year" => Some(3),
        other => {
            return Err(format!(
                "unknown upload date filter '{other}' (expected: any | today | week | month | year)"
            ))
        }
    };
    let sp = match sort {
        "relevance" | "" => match date {
            None => None,
            Some(0) => Some("EgIIAg=="),
            Some(1) => Some("EgIIAw=="),
            Some(2) => Some("EgIIBA=="),
            Some(3) => Some("EgIIBQ=="),
            _ => unreachable!(),
        },
        "popularity" => match date {
            None => Some("CAM="),
            Some(0) => Some("CAMSBAgCEAE="),
            Some(1) => Some("CAMSBAgDEAE="),
            Some(2) => Some("CAMSBAgEEAE="),
            Some(3) => Some("CAMSBAgFEAE="),
            _ => unreachable!(),
        },
        other => {
            return Err(format!(
                "unknown sort filter '{other}' (expected: relevance | popularity)"
            ))
        }
    };
    Ok(sp)
}

/// 统一把原始条目（title / url / channel / views / published / duration）整理成输出结构：
/// 按 url 去重、补上可点击的 target、截取前 max 条。
fn build_results(items: Vec<Value>, max: usize) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut result_index = 0usize;
    items
        .into_iter()
        .filter(|it| {
            it.get("title")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        // 续载可能出现重复项，按 url 去重
        .filter(|it| {
            let url = it.get("url").and_then(Value::as_str).unwrap_or("");
            seen.insert(url.to_string())
        })
        .take(max)
        .map(|it| {
            // 定位：结果卡片上的标题链接，按过滤后的结果序号定位
            let target = json!({
                "by": "css",
                "value": YT_RESULT_TARGET,
                "index": result_index,
            });
            result_index += 1;
            json!({
                "title": it.get("title").cloned().unwrap_or(Value::Null),
                "channel": it.get("channel").cloned().unwrap_or(Value::Null),
                "views": it.get("views").cloned().unwrap_or(Value::Null),
                "published": it.get("published").cloned().unwrap_or(Value::Null),
                "duration": it.get("duration").cloned().unwrap_or(Value::Null),
                "url": it.get("url").cloned().unwrap_or(Value::Null),
                "target": target,
            })
        })
        .collect()
}

/// YouTube 搜索：导航到搜索结果页（可选上传日期 / 优先顺序筛选），直接解析页面
/// HTML 内嵌的 ytInitialData（首屏约 20 条），不足 `max` 条时用 InnerTube
/// continuation 续取。数据在 HTML 里即齐全，与标签页是否可见/渲染无关，因此
/// 后台标签页也能拿满结果，无需弹窗或窗口操作。
/// `max` 控制最多返回多少条（至少 1）。返回 `{ "tab_id": ..., "results": [...] }`，
/// tab_id 供后续指令在同一标签页上链式操作。
pub async fn youtubesearch(
    bridge: &mut Bridge,
    query: &str,
    time: &str,
    sort: &str,
    max: usize,
    tab: Option<i32>,
) -> Result<Value, String> {
    let max = max.max(1);
    let sp = filter_sp(time, sort)?;
    let mut url = format!(
        "https://www.youtube.com/results?search_query={}",
        urlencode(query)
    );
    if let Some(token) = sp {
        url.push_str("&sp=");
        url.push_str(&urlencode(token));
    }

    let nav = bridge
        .request("yt1", "navigate", json!({ "url": url, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);

    // 解析 ytInitialData / InnerTube（yt-dlp 同款数据源）：
    // 导航返回的 HTML 里就内嵌了完整首屏数据；翻页用 continuation token 调
    // /youtubei/v1/search，不需要滚动和窗口可见。
    let fetch_script = format!(
        r#"(async () => {{
  const MAX = {max};
  const html = document.documentElement.outerHTML;
  const parseInitial = (text) => {{
    const m = text.match(/var ytInitialData = (\{{.*?\}});<\/script>/s);
    if (!m) return null;
    try {{ return JSON.parse(m[1]); }} catch (e) {{ return null; }}
  }};
  const extract = (data) => {{
    const items = [];
    let token = null;
    const walk = (o) => {{
      if (!o || typeof o !== "object") return;
      if (o.videoRenderer) {{
        const v = o.videoRenderer;
        const title = v.title && v.title.runs ? v.title.runs.map((x) => x.text || "").join("") : "";
        let url = v.navigationEndpoint && v.navigationEndpoint.commandMetadata && v.navigationEndpoint.commandMetadata.webCommandMetadata ? v.navigationEndpoint.commandMetadata.webCommandMetadata.url : null;
        if (url && !url.startsWith("http")) url = "https://www.youtube.com" + url;
        items.push({{
          title,
          url,
          channel: v.ownerText && v.ownerText.runs ? v.ownerText.runs.map((x) => x.text || "").join("") : null,
          views: v.viewCountText ? v.viewCountText.simpleText : null,
          published: v.publishedTimeText ? v.publishedTimeText.simpleText : null,
          duration: v.lengthText ? v.lengthText.simpleText : null,
        }});
      }} else if (!token && o.continuationItemRenderer && o.continuationItemRenderer.continuationEndpoint && o.continuationItemRenderer.continuationEndpoint.continuationCommand) {{
        token = o.continuationItemRenderer.continuationEndpoint.continuationCommand.token;
      }}
      for (const val of Object.values(o)) walk(val);
    }};
    walk(data);
    return {{ items, token }};
  }};
  const data = parseInitial(html);
  if (!data) return {{ ok: false, reason: "no ytInitialData" }};
  const first = extract(data);
  const items = first.items;
  let token = first.token;
  const keyM = html.match(/"INNERTUBE_API_KEY":"([^"]+)"/);
  let context = null;
  const ctxStart = html.indexOf("\"INNERTUBE_CONTEXT\":");
  if (ctxStart >= 0) {{
    const start = html.indexOf("{{", ctxStart);
    let depth = 0, inStr = false, esc = false, end = -1;
    for (let j = start; j < html.length; j++) {{
      const ch = html[j];
      if (inStr) {{
        if (esc) esc = false;
        else if (ch === "\\") esc = true;
        else if (ch === "\"") inStr = false;
        continue;
      }}
      if (ch === "\"") inStr = true;
      else if (ch === "{{") depth++;
      else if (ch === "}}") {{ depth--; if (depth === 0) {{ end = j; break; }} }}
    }}
    if (end > 0) {{ try {{ context = JSON.parse(html.slice(start, end + 1)); }} catch (e) {{}} }}
  }}
  const api = async (body) => {{
    if (!keyM || !context) return null;
    try {{
      const r = await fetch("/youtubei/v1/search?key=" + encodeURIComponent(keyM[1]), {{ method: "POST", headers: {{ "content-type": "application/json" }}, body: JSON.stringify(body) }});
      const j = await r.json();
      if (!j || j.error) return null;
      return j;
    }} catch (e) {{ return null; }}
  }};
  const seen = new Set(items.map((x) => x.url));
  let rounds = 0;
  while (items.length < MAX && token && rounds < 12) {{
    const j = await api({{ context, continuation: token }});
    if (!j) break;
    const next = extract(j);
    for (const it of next.items) {{
      if (it.url && !seen.has(it.url)) {{ seen.add(it.url); items.push(it); }}
    }}
    token = next.token;
    rounds++;
  }}
  return {{ ok: true, items: items.slice(0, MAX), total: items.length, rounds }};
}})()"#,
        max = max,
    );
    let fetched = bridge
        .request("yt2", "run_script", json!({ "code": fetch_script, "tab_id": tab_id }))
        .await?;
    let res = fetched.get("result").cloned().unwrap_or(Value::Null);
    if !res.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let reason = res
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!(
            "YouTube 页面数据缺失（{reason}），可能是验证/consent 墙；确认浏览器登录状态正常后重试"
        ));
    }
    let items = res
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = build_results(items, max);
    Ok(json!({
        "tab_id": tab_id,
        "results": results,
    }))
}
