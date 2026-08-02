//! Google Trends 搜索配方：站点知识（选择器 + SVG 反解）集中在这里，
//! 通过通用原语 navigate + run_script 编排。

use serde_json::{json, Value};

use crate::transport::{urlencode, Bridge};

const DEFAULT_DATE: &str = "today 1-m";
const DEFAULT_GEO: &str = "Worldwide";

/// 页面内执行脚本：等图表和表格加载完，反解趋势曲线并读两张关键词表。
/// 日期由脚本端按 date 参数 + 点数推导（不依赖页面本地化文案）。
fn trends_script(date_spec: &str) -> String {
    let date_lit = serde_json::to_string(date_spec).unwrap_or_else(|_| "\"today 1-m\"".into());
    format!(
        r#"(async () => {{
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 20000;
  // Trends 是固定高度的应用壳，滚动要同时覆盖 window 和内部滚动容器，懒加载才触发
  const scrollAll = () => {{
    window.scrollTo(0, document.body.scrollHeight);
    Array.from(document.querySelectorAll('div')).forEach((el) => {{
      if (el.scrollHeight > el.clientHeight + 50) el.scrollTop = el.scrollHeight;
    }});
  }};
  let svg = null;
  let line = null;
  while (Date.now() < deadline) {{
    svg = Array.from(document.querySelectorAll('svg')).find((s) => s.getBoundingClientRect().width > 100);
    if (svg) {{
      const vbW = (svg.getAttribute('viewBox') || '0 0 1384 320').split(/[\s,]+/).map(Number)[2] || 1280;
      line = Array.from(svg.querySelectorAll('path'))
        .filter((p) => {{
          const d = p.getAttribute('d') || '';
          if (d.length < 1500) return false;
          const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
          // 曲线横跨整个绘图区（0~1280，viewBox 宽 1384，右侧留标签区），图标路径远小于此
          return nums.length > 4 && nums[0] <= 2 && nums[nums.length - 2] >= Math.min(vbW - 5, 1000);
        }})[0] || null;
    }}
    // 等表格行不是骨架：至少 20 个已填充内容的查询单元格
    const filledRows = Array.from(document.querySelectorAll('table tbody tr td:nth-child(2)'))
      .filter((td) => (td.textContent || '').trim().length > 0).length;
    if (line && filledRows >= 20) break;
    scrollAll();
    await sleep(300);
  }}
  if (!line) return {{ error: 'trend chart not loaded' }};

  // --- 趋势曲线：解析 path 坐标，再按 y 轴刻度校准成 0-100 ---
  const d = line.getAttribute('d');
  const cmds = d.match(/[MC]/g) || [];
  const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
  let ni = 0;
  const ys = [];
  for (const c of cmds) {{
    if (c === 'M') {{ ys.push(nums[ni + 1]); ni += 2; }}
    else {{ ys.push(nums[ni + 5]); ni += 6; }}
  }}
  const vb = (svg.getAttribute('viewBox') || '0 0 1384 320').split(/[\s,]+/).map(Number);
  const svgTop = svg.getBoundingClientRect().y;
  const labelY = {{}};
  Array.from(document.querySelectorAll('svg text')).forEach((t) => {{
    const v = t.textContent.trim();
    if (v === '0' && labelY['0'] == null) labelY['0'] = t.getBoundingClientRect().y - svgTop;
    if (v === '100' && labelY['100'] == null) labelY['100'] = t.getBoundingClientRect().y - svgTop;
  }});
  const y0 = labelY['0'] != null ? labelY['0'] : (vb[3] || 320);
  const y100 = labelY['100'] != null ? labelY['100'] : 0;
  const span = (y0 - y100) || 1;
  const values = ys.map((y) => Math.max(0, Math.min(100, Math.round(((y0 - y) / span) * 100))));

  // --- 日期：按 date 参数 + 点数推导粒度（日/周/月） ---
  const DATE_SPEC = {date_lit};
  const fmt = (dd) => dd.getFullYear() + '-' + String(dd.getMonth() + 1).padStart(2, '0') + '-' + String(dd.getDate()).padStart(2, '0');
  const buildDates = (spec, n) => {{
    const now = new Date();
    let end = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    let start = null;
    const m = spec.match(/^today\s+(\d+)-([my])$/);
    if (m) {{
      const k = parseInt(m[1], 10);
      start = new Date(end);
      if (m[2] === 'm') start.setMonth(end.getMonth() - k);
      else start.setFullYear(end.getFullYear() - k);
    }} else if (spec === 'all') {{
      start = new Date(2004, 0, 1);
    }} else {{
      const dm = spec.match(/^(\d{{4}}-\d{{2}}-\d{{2}})\s+(\d{{4}}-\d{{2}}-\d{{2}})$/);
      if (dm) {{ start = new Date(dm[1]); end = new Date(dm[2]); }}
    }}
    if (!start || n <= 0) return Array(n).fill(null);
    const days = Math.round((end - start) / 86400000);
    const back = (offset) => new Date(end.getFullYear(), end.getMonth(), end.getDate() - offset);
    // Google 的采样点以今天为终点往前排，日期从终点倒推而不是从起点正推
    if (Math.abs(n - days) <= 2) return Array.from({{ length: n }}, (_, i) => fmt(back(n - 1 - i)));
    const weekly = Math.round(days / 7);
    if (Math.abs(n - weekly) <= 2) return Array.from({{ length: n }}, (_, i) => fmt(back((n - 1 - i) * 7)));
    return Array.from({{ length: n }}, (_, i) => fmt(back(Math.round(((n - 1 - i) * days) / (n - 1 || 1)))));
  }};
  const dates = buildDates(DATE_SPEC, values.length);
  const trend = values.map((v, i) => ({{ date: dates[i] || null, value: v }}));

  // --- 关键词表：第一张=热门，第二张=上升；各表自动翻页（最多 10 页，实际一般 5 页 50 条） ---
  const parseTable = (table) => Array.from(table.querySelectorAll('tbody tr')).map((tr, i) => {{
    const cells = Array.from(tr.querySelectorAll('td'));
    if (cells.length < 3) return null;
    const rankCell = cells[0].textContent.trim();
    const query = (cells[1].querySelector('.Z9Uqw') || cells[1]).textContent.trim();
    const img = cells[2].querySelector('[role="img"]');
    const interest = img ? parseInt(img.getAttribute('title'), 10) : null;
    const chgCell = cells[3];
    const chgTxt = chgCell ? ((chgCell.querySelector('span') || chgCell).textContent || '').trim() : '';
    const pm = chgTxt.match(/([+-−])\s*([\d,.]+)\s*%/);
    const change = pm ? ((pm[1] === '-' || pm[1] === '−') ? '-' : '+') + pm[2] + '%'
      : /暴增|突破|breakout/i.test(chgTxt) ? 'breakout' : (chgTxt || null);
    const rankText = (cells[0].textContent || '').trim();
    const rank = /^\d+$/.test(rankText) ? parseInt(rankText, 10) : i + 1;
    return {{ rank, query, interest: Number.isFinite(interest) ? interest : null, change }};
  }}).filter(Boolean);
  // 表格和下一页按钮都按 x 排序一一对应（aria-label 本地化，不能写死文案）
  const tables = Array.from(document.querySelectorAll('table')).sort((a, b) => a.getBoundingClientRect().x - b.getBoundingClientRect().x);
  const nextBtns = Array.from(document.querySelectorAll('button')).filter((b) => {{
    const a = b.getAttribute('aria-label') || '';
    return /下一页|next page|next/i.test(a);
  }}).sort((a, b) => a.getBoundingClientRect().x - b.getBoundingClientRect().x);
  const isDisabled = (btn) => btn.disabled || btn.getAttribute('aria-disabled') === 'true' || btn.hasAttribute('disabled');
  const collect = async (table, nextBtn) => {{
    const seen = new Set();
    const rows = [];
    for (let p = 0; p < 10; p++) {{
      const pageRows = parseTable(table);
      for (const r of pageRows) {{
        if (!seen.has(r.rank)) {{ rows.push(r); seen.add(r.rank); }}
      }}
      if (isDisabled(nextBtn) || p === 9) break;
      nextBtn.click();
      // 等翻页完成：首行 rank 变为未见过，或按钮禁用，最多 5 秒
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {{
        const fr = parseTable(table)[0];
        if (fr && !seen.has(fr.rank)) break;
        if (isDisabled(nextBtn)) break;
        await sleep(300);
      }}
    }}
    return rows;
  }};
  const top = tables[0] ? await collect(tables[0], nextBtns[0]) : [];
  const rising = tables[1] ? await collect(tables[1], nextBtns[1]) : [];
  const tablesAvailable = top.length > 0 || rising.length > 0;
  return {{ trend, top, rising, tables_available: tablesAvailable }};
}})()"#
    )
}

/// 对比模式脚本：解析多折线（每个关键词一条线，mask 序号对应查询顺序），返回各词趋势序列。
fn compare_script(terms: &[String], date_spec: &str) -> String {
    let date_lit = serde_json::to_string(date_spec).unwrap_or_else(|_| "\"today 1-m\"".into());
    let terms_json = serde_json::to_string(terms).unwrap_or_else(|_| "[]".into());
    format!(
        r#"(async () => {{
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const TERMS = {terms_json};
  const N = TERMS.length;
  const deadline = Date.now() + 20000;
  const scrollAll = () => {{
    window.scrollTo(0, document.body.scrollHeight);
    Array.from(document.querySelectorAll('div')).forEach((el) => {{
      if (el.scrollHeight > el.clientHeight + 50) el.scrollTop = el.scrollHeight;
    }});
  }};
  let svg = null;
  let lines = null;
  while (Date.now() < deadline) {{
    svg = Array.from(document.querySelectorAll('svg')).find((s) => s.getBoundingClientRect().width > 100);
    if (svg) {{
      // 每条线画了两遍（inverse mask 与普通 mask），取 inverse 的一套，mask 序号即线序
      lines = Array.from(svg.querySelectorAll('path')).filter((p) => {{
        const d = p.getAttribute('d') || '';
        const m = p.getAttribute('mask') || '';
        return d.length > 1500 && m.includes('inverse-mask');
      }});
      if (lines.length === N) break;
    }}
    scrollAll();
    await sleep(300);
  }}
  if (!lines || lines.length !== N) {{
    return {{ error: 'comparison chart not loaded (expected ' + N + ' lines, got ' + (lines ? lines.length : 0) + ')' }};
  }}
  const maskIdx = (p) => {{
    const m = (p.getAttribute('mask') || '').match(/timeline-inverse-mask-\d+-(\d+)/);
    return m ? parseInt(m[1], 10) : 0;
  }};
  lines.sort((a, b) => maskIdx(a) - maskIdx(b));

  const parseLine = (p) => {{
    const d = p.getAttribute('d');
    const cmds = d.match(/[MC]/g) || [];
    const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
    let ni = 0;
    const ys = [];
    for (const c of cmds) {{
      if (c === 'M') {{ ys.push(nums[ni + 1]); ni += 2; }}
      else {{ ys.push(nums[ni + 5]); ni += 6; }}
    }}
    return ys;
  }};
  const vb = (svg.getAttribute('viewBox') || '0 0 1384 320').split(/[\s,]+/).map(Number);
  const svgTop = svg.getBoundingClientRect().y;
  const labelY = {{}};
  Array.from(document.querySelectorAll('svg text')).forEach((t) => {{
    const v = t.textContent.trim();
    if (v === '0' && labelY['0'] == null) labelY['0'] = t.getBoundingClientRect().y - svgTop;
    if (v === '100' && labelY['100'] == null) labelY['100'] = t.getBoundingClientRect().y - svgTop;
  }});
  const y0 = labelY['0'] != null ? labelY['0'] : (vb[3] || 320);
  const y100 = labelY['100'] != null ? labelY['100'] : 0;
  const span = (y0 - y100) || 1;
  const toValue = (y) => Math.max(0, Math.min(100, Math.round(((y0 - y) / span) * 100)));

  // 日期：与单查询一致，从今天倒推
  const DATE_SPEC = {date_lit};
  const fmt = (dd) => dd.getFullYear() + '-' + String(dd.getMonth() + 1).padStart(2, '0') + '-' + String(dd.getDate()).padStart(2, '0');
  const buildDates = (spec, n) => {{
    const now = new Date();
    let end = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    let start = null;
    const m = spec.match(/^today\s+(\d+)-([my])$/);
    if (m) {{
      const k = parseInt(m[1], 10);
      start = new Date(end);
      if (m[2] === 'm') start.setMonth(end.getMonth() - k);
      else start.setFullYear(end.getFullYear() - k);
    }} else if (spec === 'all') {{
      start = new Date(2004, 0, 1);
    }} else {{
      const dm = spec.match(/^(\d{{4}}-\d{{2}}-\d{{2}})\s+(\d{{4}}-\d{{2}}-\d{{2}})$/);
      if (dm) {{ start = new Date(dm[1]); end = new Date(dm[2]); }}
    }}
    if (!start || n <= 0) return Array(n).fill(null);
    const days = Math.round((end - start) / 86400000);
    const back = (offset) => new Date(end.getFullYear(), end.getMonth(), end.getDate() - offset);
    if (Math.abs(n - days) <= 2) return Array.from({{ length: n }}, (_, i) => fmt(back(n - 1 - i)));
    const weekly = Math.round(days / 7);
    if (Math.abs(n - weekly) <= 2) return Array.from({{ length: n }}, (_, i) => fmt(back((n - 1 - i) * 7)));
    return Array.from({{ length: n }}, (_, i) => fmt(back(Math.round(((n - 1 - i) * days) / (n - 1 || 1)))));
  }};

  const counts = lines.map(parseLine);
  const n = counts[0].length;
  const dates = buildDates(DATE_SPEC, n);
  const series = TERMS.map((term, i) => {{
    const trend = counts[i].map((y, j) => ({{ date: dates[j] || null, value: toValue(y) }}));
    return {{ term, trend }};
  }});
  return {{ series }};
}})()"#
    )
}

/// Google Trends：查询搜索趋势，返回趋势序列 + 热门/上升关键词。
/// 返回 `{ "tab_id": ..., "query": ..., "date": ..., "geo": ..., "trend": [...], "top": [...], "rising": [...] }`。
pub async fn googletrends(
    bridge: &mut Bridge,
    query: &str,
    date: &str,
    geo: &str,
) -> Result<Value, String> {
    let date = if date.trim().is_empty() { DEFAULT_DATE } else { date };
    let geo = if geo.trim().is_empty() { DEFAULT_GEO } else { geo };
    let url = format!(
        "https://trends.google.com/explore?q={}&date={}&geo={}",
        urlencode(query),
        urlencode(date),
        urlencode(geo)
    );
    let script = trends_script(date);
    let mut tab_id = Value::Null;
    let mut data = Value::Null;
    // Trends 同标签页反复导航时图表偶发不加载，新标签页则稳定。
    // 每次查询新开标签页（扩展会记录，可用 close-auto-tabs 清理），失败则关掉重开。
    for attempt in 0..3 {
        let nav = bridge.request("gt1", "new_tab", json!({ "url": url })).await?;
        tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);
        let resp = bridge
            .request(
                "gt2",
                "run_script",
                json!({ "code": script, "tab_id": tab_id }),
            )
            .await?;
        let got = resp.get("result").cloned().unwrap_or(Value::Null);
        let is_chart_error = got
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.contains("trend chart not loaded"))
            .unwrap_or(false);
        if !is_chart_error {
            data = got;
            break;
        }
        // 图表没加载出来：关掉这次开的标签页，下轮换新标签页重试
        if tab_id.is_number() {
            let _ = bridge
                .request(
                    "gt3",
                    "close_tab",
                    json!({ "tab_id": tab_id }),
                )
                .await;
        }
        if attempt == 2 {
            data = got;
        }
    }
    if let Some(err) = data.get("error").and_then(Value::as_str) {
        return Err(format!("googletrends: {err}"));
    }
    let tables_available = data
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
    });
    if !tables_available {
        out["note"] = json!(
            "当前会话 Google 显示的是 Gemini 变体界面，热门/上升关键词表未渲染（趋势数据不受影响）；如在经典界面下运行则会返回完整表格"
        );
    }

    Ok(out)
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
    let date = if date.trim().is_empty() { DEFAULT_DATE } else { date };
    let geo = if geo.trim().is_empty() { DEFAULT_GEO } else { geo };
    let q = terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!(
        "https://trends.google.com/explore?q={}&date={}&geo={}",
        urlencode(&q),
        urlencode(date),
        urlencode(geo)
    );

    let script = compare_script(terms, date);
    let mut tab_id = Value::Null;
    let mut data = Value::Null;
    for attempt in 0..3 {
        let nav = bridge.request("gtc1", "new_tab", json!({ "url": url })).await?;
        tab_id = nav.get("tab_id").cloned().unwrap_or(Value::Null);
        let resp = bridge
            .request(
                "gtc2",
                "run_script",
                json!({ "code": script, "tab_id": tab_id }),
            )
            .await?;
        let got = resp.get("result").cloned().unwrap_or(Value::Null);
        let is_chart_error = got
            .get("error")
            .and_then(Value::as_str)
            .map(|s| s.contains("chart not loaded"))
            .unwrap_or(false);
        if !is_chart_error {
            data = got;
            break;
        }
        if tab_id.is_number() {
            let _ = bridge
                .request("gtc3", "close_tab", json!({ "tab_id": tab_id }))
                .await;
        }
        if attempt == 2 {
            data = got;
        }
    }
    if let Some(err) = data.get("error").and_then(Value::as_str) {
        return Err(format!("googletrends-compare: {err}"));
    }

    Ok(json!({
        "tab_id": tab_id,
        "terms": terms,
        "date": date,
        "geo": geo,
        "series": data.get("series").cloned().unwrap_or_else(|| json!([])),
    }))
}
