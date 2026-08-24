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
  // 打开后先等 2~3 秒让页面完成首屏渲染，再按 20%/50%/80%/100% 分段下滑，
  // 每段间隔 500ms，避免一打开页面就直接跳到底部
  const stagedScroll = async () => {{
    await sleep(2000 + Math.random() * 1000);
    for (const pct of [0.2, 0.5, 0.8, 1]) {{
      const maxY = document.body.scrollHeight - window.innerHeight;
      window.scrollTo(0, Math.max(0, maxY * pct));
      Array.from(document.querySelectorAll('div')).forEach((el) => {{
        if (el.scrollHeight > el.clientHeight + 50) el.scrollTop = el.scrollHeight * pct;
      }});
      await sleep(500);
    }}
  }};
  await stagedScroll();
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

  // --- 关键词表：按卡片标题分类（热门/上升/区域），各表自动翻页直到按钮禁用 ---
  // --- 表格分类：表头定大类，widget 卡片标题细分 top/rising/region，全走元素结构定位 ---
  const headerOf = (t) => Array.from(t.querySelectorAll('thead th')).map((th) => th.textContent.trim()).join(' ');
  const isRegionHeader = (t) => /指数和区域|region/i.test(headerOf(t));
  const isQueryHeader = (t) => !isRegionHeader(t) && /查询|query|term/i.test(headerOf(t));
  // widget 卡片标题：从表格向上爬，遇到含多张表的容器即视为已爬出卡片
  const cardTitleOf = (table) => {{
    let el = table;
    for (let i = 0; i < 12 && el; i++) {{
      el = el.parentElement;
      if (!el) break;
      if (el.querySelectorAll('table').length > 1) break;
      const h = Array.from(el.querySelectorAll('h1,h2,h3,h4,[role="heading"]')).map((x) => x.textContent.trim()).find(Boolean);
      if (h) return h;
    }}
    return '';
  }};
  const kindOf = (t) => {{
    if (isRegionHeader(t)) return 'region';
    if (!isQueryHeader(t)) return null;
    const title = cardTitleOf(t);
    if (/上升|rising/i.test(title)) return 'rising';
    if (/热门|top|most/i.test(title)) return 'top';
    return 'unknown';
  }};
  // 热度值：新版页面把数字放在 data-search-interest / aria-label 里，旧版在 title 上
  const interestOf = (el) => {{
    if (!el) return null;
    const dsi = el.getAttribute('data-search-interest');
    if (dsi != null && /^\d+$/.test(dsi.trim())) return parseInt(dsi, 10);
    const m = (el.getAttribute('aria-label') || '').match(/(\d+)\s*$/);
    return m ? parseInt(m[1], 10) : null;
  }};
  const parseQueryRow = (tr, i) => {{
    const cells = Array.from(tr.querySelectorAll('td'));
    if (cells.length < 4) return null;
    const query = (cells[1].querySelector('.Z9Uqw') || cells[1]).textContent.trim();
    if (!query) return null;
    const rankText = (cells[0].textContent || '').trim();
    const rank = /^\d+$/.test(rankText) ? parseInt(rankText, 10) : i + 1;
    const interest = interestOf(cells[2].querySelector('[role="img"]'));
    const chgCell = cells[3];
    const chgTxt = chgCell ? ((chgCell.querySelector('.VYi2zf') || chgCell).textContent || '').trim() : '';
    const pm = chgTxt.match(/([+-−])\s*([\d,.]+)\s*%/);
    let change = null;
    if (pm) change = ((pm[1] === '-' || pm[1] === '−') ? '-' : '+') + pm[2] + '%';
    else if (/暴增|突破|breakout/i.test(chgTxt)) change = 'breakout';
    else if (/没有变化|无变化|no change/i.test(chgTxt)) change = '+0%';
    return {{ rank, query, interest: Number.isFinite(interest) ? interest : null, change }};
  }};
  const parseRegionRow = (tr, i) => {{
    const cells = Array.from(tr.querySelectorAll('td'));
    if (cells.length < 3) return null;
    const nameEl = cells[1].querySelector('[role="button"]') || cells[1];
    const region = nameEl.textContent.trim();
    if (!region) return null;
    const bar = cells[2].querySelector('[role="img"]');
    const rankText = (cells[0].textContent || '').trim();
    const rank = /^\d+$/.test(rankText) ? parseInt(rankText, 10) : i + 1;
    return {{ rank, region, geo_code: (bar && bar.getAttribute('data-geo-code')) || null, interest: Number.isFinite(interestOf(bar)) ? interestOf(bar) : null }};
  }};
  // 翻页按钮定位：从表格向上找第一层恰好只有一颗翻页按钮的祖先容器；若该层有多颗，
  // 用元素归属判定——按钮向上找到的「单表容器」装的是不是当前这张表（不依赖坐标）
  const isNextBtn = (b) => /下一页|next page|next/i.test(b.getAttribute('aria-label') || '');
  const ownsTable = (btn, table) => {{
    let p = btn;
    for (let j = 0; j < 10 && p; j++) {{
      p = p.parentElement;
      if (!p) break;
      const inner = p.querySelectorAll('table');
      if (inner.length === 1) return inner[0] === table;
    }}
    return false;
  }};
  const nextBtnFor = (table) => {{
    let el = table;
    for (let i = 0; i < 10 && el; i++) {{
      el = el.parentElement;
      if (!el) break;
      const btns = Array.from(el.querySelectorAll('button')).filter(isNextBtn);
      if (btns.length === 1) return btns[0];
      if (btns.length > 1) return btns.find((b) => ownsTable(b, table)) || null;
    }}
    // 兜底：整页只有一颗翻页按钮时直接用
    const allBtns = Array.from(document.querySelectorAll('button')).filter(isNextBtn);
    return allBtns.length === 1 ? allBtns[0] : null;
  }};
  const isDisabled = (btn) => !btn || btn.disabled || btn.getAttribute('aria-disabled') === 'true' || btn.hasAttribute('disabled');
  // 点击翻页后 Google 会整体重渲染、替换 table 节点，所以每页都重新按结构解析表格引用
  const resolveTable = (kind) => Array.from(document.querySelectorAll('table')).find((t) => kindOf(t) === kind);
  const collect = async (getTable, parseRow) => {{
    const seen = new Set();
    const rows = [];
    // 页数上限只作保险（区域表实测可达 14+ 页），正常由「按钮禁用」终止翻页
    for (let p = 0; p < 30; p++) {{
      const table = getTable();
      if (!table) break;
      for (const r of Array.from(table.querySelectorAll('tbody tr')).map(parseRow)) {{
        if (r && !seen.has(r.rank)) {{ rows.push(r); seen.add(r.rank); }}
      }}
      const nextBtn = nextBtnFor(table);
      if (isDisabled(nextBtn)) break;
      nextBtn.click();
      // 等翻页完成：以「非空行数恢复到上一页水平」为准，避免新页面渲染到一半就被采走
      const filledRows = (t) => Array.from(t.querySelectorAll('tbody tr')).filter((tr) => /^\d+$/.test(((tr.querySelector('td') || {{}}).textContent || '').trim())).length;
      const prevCount = filledRows(table);
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {{
        const fresh = getTable();
        const fr = fresh ? Array.from(fresh.querySelectorAll('tbody tr')).map(parseRow).find(Boolean) : null;
        const ready = fresh && filledRows(fresh) >= prevCount && fr && !seen.has(fr.rank);
        if (ready || (nextBtn && !nextBtn.isConnected)) break;
        await sleep(300);
      }}
    }}
    return rows;
  }};
  let top = await collect(() => resolveTable('top'), parseQueryRow);
  let rising = await collect(() => resolveTable('rising'), parseQueryRow);
  const regions = await collect(() => resolveTable('region'), parseRegionRow);
  // 兜底：卡片标题匹配不到时（界面文案变化），未分类的查询表按文档顺序当 top/rising
  if (!top.length && !rising.length) {{
    const posTable = (idx) => {{
      let ts = Array.from(document.querySelectorAll('table')).filter((t) => kindOf(t) === 'unknown');
      if (!ts.length) ts = Array.from(document.querySelectorAll('table')).filter(isQueryHeader);
      return ts[idx]; // DOM 文档顺序兜底，不做坐标排序
    }};
    if (!top.length) top = await collect(() => posTable(0), parseQueryRow);
    if (!rising.length) rising = await collect(() => posTable(1), parseQueryRow);
  }}
  const tablesAvailable = top.length > 0 || rising.length > 0;
  return {{ trend, top, rising, regions, tables_available: tablesAvailable }};
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
  // 打开后先等 2~3 秒让页面完成首屏渲染，再按 20%/50%/80%/100% 分段下滑，
  // 每段间隔 500ms，避免一打开页面就直接跳到底部
  const stagedScroll = async () => {{
    await sleep(2000 + Math.random() * 1000);
    for (const pct of [0.2, 0.5, 0.8, 1]) {{
      const maxY = document.body.scrollHeight - window.innerHeight;
      window.scrollTo(0, Math.max(0, maxY * pct));
      Array.from(document.querySelectorAll('div')).forEach((el) => {{
        if (el.scrollHeight > el.clientHeight + 50) el.scrollTop = el.scrollHeight * pct;
      }});
      await sleep(500);
    }}
  }};
  await stagedScroll();
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
/// Google Trends：查询搜索趋势，返回趋势序列 + 热门/上升关键词 + 区域热度。
/// 返回 `{ "tab_id": ..., "query": ..., "date": ..., "geo": ..., "trend": [...], "top": [...], "rising": [...], "regions": [...] }`。
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
        "regions": data.get("regions").cloned().unwrap_or_else(|| json!([])),
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
