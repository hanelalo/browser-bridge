//! Query.Domains 域名批量查询配方：站点知识（选择器）集中在这里，
//! 通过通用原语 navigate + click + set_value + press_key + run_script 编排。
//! 按关键词同时查询多个 TLD 的注册情况与价格（基于 WHOIS，SSE 流式返回）。

use serde_json::{json, Value};

use crate::target;
use crate::transport::Bridge;

const HOME: &str = "https://query.domains/zh-hans";
/// 关键词输入框（placeholder「输入您的关键词」）
const KEYWORD_INPUT: &str = "input.bc-input__input";
/// TLD 自定义触发器：输入框右侧「14 个域名后缀，点击自定义」
const TLD_TRIGGER: &str = "div.flex.items-center.gap-3.mr-3 span.cursor-pointer";
/// TLD 编辑器：每行一个后缀的 textarea（模态框内）
const TLD_TEXTAREA: &str = "textarea.bc-textarea__input";
/// 默认 TLD 列表（与站点默认一致，最多 20 个）
pub const DEFAULT_TLDS: &[&str] = &[
    "com", "ai", "org", "net", "cn", "info", "app", "io", "xyz", "co", "run", "me", "pro", "top",
];

/// 等待首页关键词输入框出现（SSR 页面一般立即就绪，保险起见轮询）。
async fn wait_input_ready(bridge: &mut Bridge, tab_id: &Value) -> Result<(), String> {
    let script = r#"(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (document.querySelector('input.bc-input__input')) return { ready: true };
    await sleep(200);
  }
  return { ready: false };
})()"#;
    let resp = bridge
        .request(
            "qdw",
            "run_script",
            json!({ "code": script, "tab_id": tab_id }),
        )
        .await?;
    let ready = resp
        .get("result")
        .and_then(|r| r.get("ready"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ready {
        return Err("querydomains: 首页关键词输入框未就绪".to_string());
    }
    Ok(())
}

/// 页面内执行脚本：轮询等待结果行（SSE 流式返回），稳定后按行提取
/// 域名 / 状态（可用/不可用/不确定）/ 徽标（价格、注册年份等）。
fn extract_script(query: &str, expected: usize) -> String {
    let q = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(async () => {{
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 20000;
  const kw = {q};
  const want = {expected};
  const read = () => Array.from(document.querySelectorAll('div.group.relative'))
    .filter((r) => {{
      const d = r.querySelector('.font-medium');
      const t = (d && (d.title || d.textContent)) || '';
      return t.toLowerCase().startsWith(kw + '.');
    }})
    .map((r) => {{
      const d = r.querySelector('.font-medium');
      const dot = r.querySelector('.w-2.h-2.rounded-full');
      const cls = dot ? (dot.className || '') : '';
      let status = 'unavailable';
      if (cls.includes('green')) status = 'available';
      else if (cls.includes('amber') || cls.includes('yellow')) status = 'uncertain';
      const badgeBox = r.querySelector('div.flex.items-center.gap-1.flex-shrink-0');
      const spans = badgeBox
        ? Array.from(badgeBox.querySelectorAll('span'))
        : Array.from(r.querySelectorAll('span')).filter((s) => !s.closest('.font-medium'));
      const badges = spans
        .map((s) => (s.textContent || '').trim())
        .filter((t) => t.length > 0 && !t.includes('Domain Rating'));
      return {{
        domain: (d && (d.title || d.textContent.trim())) || r.textContent.trim().split(/\\s+/)[0],
        status,
        badges: [...new Set(badges)],
      }};
    }});
  let rows = [];
  let lastSig = '';
  let stableSince = 0;
  while (Date.now() < deadline) {{
    rows = read();
    const sig = rows.map((r) => r.status + '|' + r.badges.join(',')).join(';');
    const done = rows.length >= want && rows.every((r) => r.badges.length > 0 && r.status !== 'uncertain');
    if (done) break;
    if (sig !== lastSig) {{
      lastSig = sig;
      stableSince = Date.now();
    }} else if (rows.length > 0 && Date.now() - stableSince > 3000) {{
      break;
    }}
    await sleep(300);
  }}
  return {{
    rows,
    complete: rows.length >= want && rows.every((r) => r.badges.length > 0 && r.status !== 'uncertain'),
  }};
}})()"#
    )
}

/// 判断徽标是否为注册价格（如 "3 USD" / "11 EUR" / "30 CNY"；
/// 排除 "29 days ago" / "2016" / "DR 30" 这类信息徽标）。
fn is_price_badge(s: &str) -> bool {
    let t = s.trim();
    let mut parts = t.split_whitespace();
    let num_ok = parts
        .next()
        .and_then(|x| x.replace(',', "").parse::<f64>().ok())
        .is_some();
    if !num_ok {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() == 1
        && (2..=4).contains(&rest[0].len())
        && rest[0].chars().all(|c| c.is_ascii_uppercase())
}

/// 按关键词批量查询域名：导航到 Query.Domains 首页（可选先自定义 TLD 列表），
/// 输入关键词回车，轮询提取每个 TLD 的注册状态与价格。
/// 返回 `{ "tab_id": ..., "query": ..., "tlds": [...], "complete": bool, "results": [...] }`。
pub async fn querydomains(
    bridge: &mut Bridge,
    query: &str,
    tlds: &[String],
    tab: Option<i32>,
) -> Result<Value, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("querydomains: 关键词不能为空".to_string());
    }
    let tlds: Vec<String> = if tlds.is_empty() {
        DEFAULT_TLDS.iter().map(|s| s.to_string()).collect()
    } else {
        tlds.iter()
            .map(|t| t.trim().trim_start_matches('.').to_lowercase())
            .filter(|t| !t.is_empty())
            .collect()
    };
    if tlds.is_empty() {
        return Err("querydomains: TLD 列表为空".to_string());
    }
    if tlds.len() > 20 {
        return Err("querydomains: 一次最多检查 20 个 TLD".to_string());
    }

    // 1. 打开首页（复用当前激活标签页，与 googlesearch / redditsearch 一致）
    let nav = bridge
        .request("qd1", "navigate", json!({ "url": HOME, "tab_id": tab }))
        .await?;
    let tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);
    wait_input_ready(bridge, &tab_id).await?;

    // 2. 自定义 TLD 列表（与默认不同才打开模态框）
    let same_as_default = tlds
        .iter()
        .map(String::as_str)
        .eq(DEFAULT_TLDS.iter().copied());
    if !same_as_default {
        bridge
            .request(
                "qd2",
                "click",
                json!({
                    "target": target::spec("css", TLD_TRIGGER, None),
                    "tab_id": tab_id,
                }),
            )
            .await?;
        bridge
            .request(
                "qd3",
                "set_value",
                json!({
                    "target": target::spec("css", TLD_TEXTAREA, None),
                    "value": tlds.join("\n"),
                    "tab_id": tab_id,
                }),
            )
            .await?;
        bridge
            .request(
                "qd4",
                "click",
                json!({
                    "target": target::spec("text", "Confirm", None),
                    "tab_id": tab_id,
                }),
            )
            .await?;
    }

    // 3. 输入关键词并回车触发查询
    bridge
        .request(
            "qd5",
            "set_value",
            json!({
                "target": target::spec("css", KEYWORD_INPUT, None),
                "value": query,
                "tab_id": tab_id,
            }),
        )
        .await?;
    bridge
        .request(
            "qd6",
            "press_key",
            json!({
                "key": "Enter",
                "target": target::spec("css", KEYWORD_INPUT, None),
                "tab_id": tab_id,
            }),
        )
        .await?;

    // 4. 轮询提取结果
    let resp = bridge
        .request(
            "qd7",
            "run_script",
            json!({
                "code": extract_script(query, tlds.len()),
                "tab_id": tab_id,
            }),
        )
        .await?;
    let data = resp.get("result").cloned().unwrap_or(Value::Null);
    let rows = data
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let complete = data
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // 状态转为可读中文，价格从徽标里单独拆出（如 "3 USD" / "11 EUR" / "30 CNY"）
    let results: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let domain = r.get("domain").cloned().unwrap_or(Value::Null);
            let status = match r.get("status").and_then(Value::as_str) {
                Some("available") => "available",
                Some("uncertain") => "uncertain",
                _ => "unavailable",
            };
            let badges = r.get("badges").cloned().unwrap_or_else(|| json!([]));
            let price = badges
                .as_array()
                .and_then(|arr| {
                    arr.iter().find(|b| {
                        b.as_str()
                            .map(is_price_badge)
                            .unwrap_or(false)
                    })
                })
                .cloned()
                .unwrap_or(Value::Null);
            json!({
                "domain": domain,
                "tld": domain.as_str().and_then(|d| d.split_once('.')).map(|(_, t)| t.to_string()).unwrap_or_default(),
                "status": status,
                "available": status == "available",
                "price": price,
                "badges": badges,
            })
        })
        .collect();

    Ok(json!({
        "tab_id": tab_id,
        "query": query,
        "tlds": tlds,
        "complete": complete,
        "results": results,
    }))
}
