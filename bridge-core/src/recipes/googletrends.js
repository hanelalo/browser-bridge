// 新版 Google Trends 探索页（Gemini 版）采集脚本。
//
// 页面机制（实测，见 docs/google-trends-lazy-loading.md）：
//   1. 外壳固定高，window 不滚动；真正的滚动容器是内部 overflow:auto 的 div，
//      且带 scroll-behavior:smooth —— 必须先关掉动画，否则每次 scrollTop 都是 1s 动画。
//   2. 卡片按 IntersectionObserver 懒挂载：不进入视口时连 <table> 都没有。
//   3. 趋势线/区域的数据随页面加载取数；热门/上升查询是挂载后才发的小请求（fXqlme），
//      并且先渲染空骨架行（总数也是占位值）再填充。
//   4. 分页是纯前端：点击"下一页"不发请求，页面自带 `第 X – Y 行（共 N 行）`。
//
// 策略：尽早滚到查询卡片（不等图表）→ 区域先就绪先翻区域（藏在等查询数据的窗口里）
//       → 查询两张表 + 图表就绪后翻页并解析 → 返回结构化结果。
// 所有等待都是状态判据（内容非空 + 两次采样稳定），没有固定 sleep。

(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const DATE_SPEC = __DATE_SPEC__;
  // 用页面年龄计时（相对导航），不受脚本注入时机影响
  const now = () => Math.round(Date.now() - performance.timeOrigin);
  const startedAt = now();
  const diag = {
    mounted_at: null,
    regions_at: null,
    queries_at: null,
    chart_at: null,
    region_pages: 0,
    top_pages: 0,
    rising_pages: 0,
    complete: { regions: null, top: null, rising: null },
    elapsed_ms: 0,
  };

  // ---------- 定位滚动容器 / 卡片标题 ----------
  const scrollRoot = () =>
    Array.from(document.querySelectorAll('div'))
      .filter((el) => {
        const cs = getComputedStyle(el);
        return /(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 100;
      })
      .sort((a, b) => (b.scrollHeight - b.clientHeight) - (a.scrollHeight - a.clientHeight))[0];

  const headOf = (re) =>
    Array.from(document.querySelectorAll('h1,h2,h3,h4,[role=heading]'))
      .filter((h) => h.getBoundingClientRect().width > 0)
      .find((h) => re.test(clean(h.innerText)));

  // 等页面壳：滚动容器 + 查询卡片标题（最多 20s）
  let sc = null;
  let qHead = null;
  const shellDeadline = Date.now() + 20000;
  while (Date.now() < shellDeadline) {
    sc = scrollRoot();
    qHead = headOf(/常见搜索查询|related queries/i);
    if (sc && qHead) break;
    await sleep(100);
  }
  if (!sc) return { error: 'trend page shell not found' };

  const prevBehavior = sc.style.scrollBehavior;
  sc.style.scrollBehavior = 'auto'; // 关掉平滑滚动
  if (qHead) {
    qHead.scrollIntoView({ block: 'center', behavior: 'instant' });
    diag.mounted_at = now();
  }

  // ---------- 表格解析 ----------
  const headerOf = (t) => Array.from(t.querySelectorAll('thead th')).map((th) => clean(th.innerText)).join(' ');
  const tablesOf = (kind) =>
    Array.from(document.querySelectorAll('table')).filter((t) =>
      kind === 'region' ? /区域|region/i.test(headerOf(t)) : /查询|query|term/i.test(headerOf(t)));
  // 数据单元格是否已有内容：骨架期 td 文本为空
  const contentRows = (t) => Array.from(t.querySelectorAll('tbody tr td:nth-child(2)')).filter((td) => clean(td.innerText)).length;

  const rangeOf = (table) => {
    let el = table;
    for (let i = 0; i < 10 && el; i++) {
      el = el.parentElement;
      if (!el) break;
      const m = clean(el.innerText).match(/第\s*(\d+)\s*[–\-−]\s*(\d+)\s*行（共\s*(\d+)\s*行）/);
      if (m) return { start: +m[1], end: +m[2], total: +m[3] };
    }
    return null;
  };

  // 从表格向上找"只有一颗翻页按钮"的祖先；查询卡片里有两张表时靠这一层区分
  const navBtn = (table, re) => {
    let el = table;
    for (let i = 0; i < 10 && el; i++) {
      el = el.parentElement;
      if (!el) break;
      const btns = Array.from(el.querySelectorAll('button')).filter((b) => re.test(b.getAttribute('aria-label') || ''));
      if (btns.length === 1) return btns[0];
    }
    return null;
  };
  const disabled = (b) => !b || b.disabled || b.getAttribute('aria-disabled') === 'true';

  const rankOf = (cells, i) => {
    const t = clean((cells[0] || {}).innerText || '');
    return /^\d+$/.test(t) ? +t : i + 1;
  };

  const parseQuery = (tr, i) => {
    const c = Array.from(tr.querySelectorAll('td'));
    if (c.length < 3) return null;
    const query = clean((c[1].querySelector('.Z9Uqw') || c[1]).innerText);
    if (!query) return null;
    const bar = c[2].querySelector('[role=img]');
    let interest = null;
    if (bar) {
      const dsi = bar.getAttribute('data-search-interest');
      if (dsi && /^\d+$/.test(dsi.trim())) interest = +dsi;
      else {
        const m = (bar.getAttribute('aria-label') || '').match(/(\d+)\s*$/);
        if (m) interest = +m[1];
      }
    }
    const chg = c[3] ? clean((c[3].querySelector('.VYi2zf') || c[3]).innerText) : '';
    const pm = chg.match(/([+\-−])\s*([\d,.]+)\s*%/);
    const change = pm
      ? (pm[1] === '-' || pm[1] === '−' ? '-' : '+') + pm[2] + '%'
      : (/暴增|突破|breakout/i.test(chg) ? 'breakout' : (/没有变化|无变化|no change/i.test(chg) ? '+0%' : null));
    return { rank: rankOf(c, i), query, interest, change };
  };

  const parseRegion = (tr, i) => {
    const c = Array.from(tr.querySelectorAll('td'));
    if (c.length < 3) return null;
    const region = clean((c[1].querySelector('[role=button]') || c[1]).innerText);
    if (!region) return null;
    const bar = c[2].querySelector('[role=img]');
    const m = ((bar && bar.getAttribute('aria-label')) || '').match(/(\d+)\s*$/);
    return {
      rank: rankOf(c, i),
      region,
      geo_code: (bar && bar.getAttribute('data-geo-code')) || null,
      interest: m ? +m[1] : null,
    };
  };

  // 翻完一张表：每页重新按结构取节点（重渲染会替换 table），按 rank 去重
  const collect = async (kind, index, parse) => {
    const seen = new Set();
    const rows = [];
    let total = null;
    let pages = 0;
    for (let p = 0; p < 40; p++) {
      const table = tablesOf(kind)[index];
      if (!table) break;
      const parsed = Array.from(table.querySelectorAll('tbody tr')).map(parse).filter(Boolean);
      for (const r of parsed) if (!seen.has(r.rank)) { rows.push(r); seen.add(r.rank); }
      pages++;

      const range = rangeOf(table);
      if (range) total = range.total;
      const btn = navBtn(table, /下一页|next page|next/i);
      if (disabled(btn) || (range && range.end >= range.total)) break;

      const first = clean((table.querySelectorAll('tbody tr')[1] || {}).innerText || '');
      btn.click();
      const deadline = Date.now() + 6000;
      let changed = false;
      while (Date.now() < deadline) {
        await sleep(150);
        const fresh = tablesOf(kind)[index];
        if (!fresh) continue;
        const f = clean((fresh.querySelectorAll('tbody tr')[1] || {}).innerText || '');
        if (f && f !== first) { changed = true; break; }
      }
      if (!changed) break; // 点了不变化就别死循环
    }
    return { rows, total, pages, complete: total == null || rows.length === total };
  };

  // ---------- 趋势曲线（SVG 反解 + y 轴刻度校准） ----------
  const isLinePath = (p, vbW) => {
    const d = p.getAttribute('d') || '';
    const n = (d.match(/-?[\d.]+/g) || []).map(Number);
    return (d.match(/M/g) || []).length === 1 && n.length >= 100 && n[0] <= 2 && n[n.length - 2] >= vbW * 0.7;
  };
  const findChart = () => {
    for (const s of document.querySelectorAll('svg')) {
      if (s.getBoundingClientRect().width <= 200) continue;
      const vbW = (s.getAttribute('viewBox') || '').split(/[\s,]+/).map(Number)[2] || 0;
      const hit = Array.from(s.querySelectorAll('path')).find((p) => isLinePath(p, vbW));
      if (hit) return { svg: s, line: hit };
    }
    return null;
  };
  const parseTrend = (chart) => {
    const d = chart.line.getAttribute('d');
    const cmds = d.match(/[MC]/g) || [];
    const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
    let ni = 0;
    const ys = [];
    for (const c of cmds) {
      if (c === 'M') { ys.push(nums[ni + 1]); ni += 2; }
      else { ys.push(nums[ni + 5]); ni += 6; }
    }
    const svgTop = chart.svg.getBoundingClientRect().y;
    const labels = {};
    Array.from(document.querySelectorAll('svg text')).forEach((t) => {
      const v = t.textContent.trim();
      if ((v === '0' || v === '100') && labels[v] == null) labels[v] = t.getBoundingClientRect().y - svgTop;
    });
    const y0 = labels['0'] != null ? labels['0'] : 320;
    const y100 = labels['100'] != null ? labels['100'] : 0;
    const span = (y0 - y100) || 1;
    return ys.map((y) => Math.max(0, Math.min(100, Math.round(((y0 - y) / span) * 100))));
  };

  // 日期：页面 x 轴只有月份，按 date 参数 + 点数从今天倒推
  const fmt = (dd) => dd.getFullYear() + '-' + String(dd.getMonth() + 1).padStart(2, '0') + '-' + String(dd.getDate()).padStart(2, '0');
  const buildDates = (spec, n) => {
    const today = new Date();
    let end = new Date(today.getFullYear(), today.getMonth(), today.getDate());
    let start = null;
    const s = String(spec).trim();
    const m = s.match(/^today\s+(\d+)-([dmy])$/);
    if (m) {
      const k = parseInt(m[1], 10);
      start = new Date(end);
      if (m[2] === 'd') start.setDate(end.getDate() - k);
      else if (m[2] === 'm') start.setMonth(end.getMonth() - k);
      else start.setFullYear(end.getFullYear() - k);
    } else if (s === 'all') {
      start = new Date(2004, 0, 1);
    } else {
      const dm = s.match(/^(\d{4}-\d{2}-\d{2})\s+(\d{4}-\d{2}-\d{2})$/);
      if (dm) { start = new Date(dm[1]); end = new Date(dm[2]); }
    }
    if (!start) { start = new Date(end); start.setMonth(end.getMonth() - 1); }
    if (n <= 0) return [];
    const days = Math.round((end - start) / 86400000);
    const back = (offset) => new Date(end.getFullYear(), end.getMonth(), end.getDate() - offset);
    if (Math.abs(n - days) <= 2) return Array.from({ length: n }, (_, i) => fmt(back(n - 1 - i)));
    const weekly = Math.round(days / 7);
    if (Math.abs(n - weekly) <= 2) return Array.from({ length: n }, (_, i) => fmt(back((n - 1 - i) * 7)));
    return Array.from({ length: n }, (_, i) => fmt(back(Math.round(((n - 1 - i) * days) / (n - 1 || 1)))));
  };

  // ---------- 流水线 ----------
  let region = null;
  let top = null;
  let rising = null;
  let chart = null;
  let regionSig = '';
  let regionStable = 0;
  let querySig = '';
  let queryStable = 0;
  const deadline = startedAt + 45000;

  while (now() < deadline) {
    if (!chart) {
      const c = findChart();
      if (c) { chart = c; diag.chart_at = now(); }
    }

    // 区域：先就绪先采（通常 4–8s），翻页完全藏在等查询数据的窗口里
    const rTable = tablesOf('region')[0];
    if (!region && rTable) {
      const cr = contentRows(rTable);
      const rg = rangeOf(rTable);
      const sig = cr + ':' + (rg ? rg.total : '-');
      if (cr > 0 && rg && rg.total >= cr) {
        if (sig === regionSig) regionStable++;
        else { regionSig = sig; regionStable = 0; }
        if (regionStable >= 1) {
          region = await collect('region', 0, parseRegion);
          diag.regions_at = now();
          diag.region_pages = region.pages;
          diag.complete.regions = region.complete;
        }
      } else {
        regionStable = 0;
        regionSig = sig;
      }
    }

    // 查询：两张表都要有内容；单表变体则只采一张
    const qTables = tablesOf('query');
    if (!top && qTables.length >= 1) {
      const need = Math.min(2, qTables.length);
      const filled = qTables.slice(0, need).every((t) => contentRows(t) >= 1);
      if (filled) {
        const sig = qTables.slice(0, need).map(contentRows).join(',');
        if (sig === querySig) queryStable++;
        else { querySig = sig; queryStable = 0; }
        if (queryStable >= 1) {
          top = await collect('query', 0, parseQuery);
          diag.top_pages = top.pages;
          diag.complete.top = top.complete;
          if (qTables.length >= 2) {
            rising = await collect('query', 1, parseQuery);
            diag.rising_pages = rising.pages;
            diag.complete.rising = rising.complete;
          }
          diag.queries_at = now();
        }
      } else {
        queryStable = 0;
        querySig = '';
      }
    }

    if (region && top && chart) break;
    await sleep(250);
  }

  // 收尾：到点仍没采的，若已有内容补采一次（不阻塞太久）
  if (!region) {
    const t = tablesOf('region')[0];
    if (t && contentRows(t) > 0) {
      region = await collect('region', 0, parseRegion);
      diag.complete.regions = region.complete;
    }
  }
  if (!top) {
    const qs = tablesOf('query');
    if (qs.length && contentRows(qs[0]) > 0) {
      top = await collect('query', 0, parseQuery);
      diag.complete.top = top.complete;
      if (qs.length >= 2 && contentRows(tablesOf('query')[1]) > 0) {
        rising = await collect('query', 1, parseQuery);
        diag.complete.rising = rising.complete;
      }
    }
  }

  sc.style.scrollBehavior = prevBehavior;
  diag.elapsed_ms = now() - startedAt;

  if (!chart) return { error: 'trend chart not loaded' };

  const values = parseTrend(chart);
  const dates = buildDates(DATE_SPEC, values.length);

  return {
    trend: values.map((v, i) => ({ date: dates[i] || null, value: v })),
    top: top ? top.rows : [],
    rising: rising ? rising.rows : [],
    regions: region ? region.rows : [],
    top_table_available: !!top,
    rising_table_available: !!rising,
    regions_available: !!region,
    tables_available: !!(top || rising),
    hydration: {
      ...diag,
      region_total: region ? region.total : null,
      top_total: top ? top.total : null,
      rising_total: rising ? rising.total : null,
    },
  };
})()
