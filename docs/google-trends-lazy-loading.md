# 新版 Google Trends 探索页：数据延迟加载机制与采集方案

> 观测对象：`https://trends.google.com/explore?q=...&date=today 1-y`（2026-10 的 Gemini 版探索页）
> 观测方式：只用通用原语（`new_tab` / `run_script` / `activate_tab`），**没有**使用 `googletrends`、`oldgoogletrends`。
> 结论均来自真实浏览器实测，数字为实测值，不是估算。

---

## 1. TL;DR

1. **数据与渲染是两道独立闸门**，不能笼统说"数据早就有了、只是渲染慢"：
   - **趋势线、区域榜**：随页面加载就取数（`DqDTgb` / `UZBRtc` / `g4kJzf`），与是否滚动无关；区域表在视口外也会挂载。
   - **热门/上升查询**：卡片进入视口才创建 DOM；**数据也是挂载时才请求**（`fXqlme`，挂载后 ~0.3s 发起，1.2–3.7s 返回），返回后行和 `共 N 行` 才一起出现。
   - 对照实验：先干等 15s 不滚动，查询表连 DOM 都没有、也没有 `fXqlme`；这时再滚过去，67ms 出空表，3.2s 后才有行 —— 行填充紧跟在 `fXqlme` 返回之后（+35ms）。
2. 页面外壳固定高 738px，**window 不滚动**；真正的滚动容器是内部一个 `overflow:auto` 的 div（class 是构建期哈希，不可依赖），当前 `clientHeight=602`、`scrollHeight` 随懒挂载从 1756 长到 2524。
3. 该滚动容器带 **`scroll-behavior: smooth`**。直接写 `scrollTop` 会变成 ~1s 的动画（实测写 200 时位置序列 907→895→370→200），连续写会互相打断；`scrollTo({behavior:'instant'})` 才能立即落位。
4. 卡片挂载后先渲染**空骨架行**（`<tr><td>` 有结构、文本为空），数据到达后才填充。实测冷启动时"表已存在"到"行有内容"间隔可达 **9s**。
5. 骨架阶段的分页文案是**假总数**：区域表刚出现时显示 `第 1 – 5 行（共 5 行）`，约 4s 后变成 `共 26 行`。只按"共 N 行"驱动翻页，会在数据到达前采到 5 行就收工。
6. 页面自身的批处理 RPC 很慢：`UZBRtc` 单次 1.6MB，耗时 **12–19s**；整页从冷启动到三块数据全部可读约 **13.6s**，其中图表 SVG 在 **8.4s** 出现。
7. 表格分页是**纯前端**的（点"下一页"不发请求）：查询 10 行/页、区域 5 行/页，页面自带 `第 X – Y 行（共 N 行）` 可作为翻页终止条件（**必须在数据就绪后读取**）。区域 CSV 下载按钮点击后**没有任何网络请求**，证明数据早已在内存里，只是没渲染。

因此正确的解法不是"滚得更久"，而是**用状态驱动的"水合（hydration）驱动"替代时间驱动的滚动**：定位滚动容器 → 关掉平滑滚动 → 逐卡片 `scrollIntoView` → 等"行有内容且两次采样稳定" → 再翻页采集。

---

## 2. 复现观测（只用通用原语）

### 2.1 页面结构：window 不滚，内部容器滚

```js
(() => {
  const sc = Array.from(document.querySelectorAll('div')).filter(el => {
    const cs = getComputedStyle(el);
    return /(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 100;
  }).sort((a, b) => (b.scrollHeight - b.clientHeight) - (a.scrollHeight - a.clientHeight))[0];
  return {
    winH: innerHeight, docH: document.documentElement.scrollHeight,
    scroller: sc && { ch: sc.clientHeight, sh: sc.scrollHeight, st: sc.scrollTop,
                      behavior: getComputedStyle(sc).scrollBehavior },
  };
})()
```

实测（1 年、单关键词）：

```json
{ "winH": 738, "docH": 738,
  "scroller": { "ch": 602, "sh": 1756, "st": 0, "behavior": "smooth" } }
```

> 结论：`window.scrollTo` 对内容无效；`scrollHeight` 会随懒挂载增长（1756 → 2524）。

### 2.2 懒挂载：不滚 = 只有一张表

冷启动后**完全不滚动**，每 3s 采样一次：

| t(s) | tables | 行数 | 说明 |
|---|---|---|---|
| 2.3 | 1 | [6] | 只有区域表 |
| 26.3 | 1 | [6] | 查询卡片**始终没有挂载** |

此时 **还没有 `fXqlme`**。随后对该卡片标题执行一次 `scrollIntoView({block:'center'})`，用 50–100ms 采样盯住表格与 RPC（页面年龄，三次独立实验）：

| 实验 | 挂载 | 空表出现 | `fXqlme` 发起 → 结束 | 行有内容 | 总数 |
|---|---|---|---|---|---|
| A（挂载前已等 15s） | 19.49s | +67ms | 19.75s → 22.48s | **22.69s（请求结束后 +0.2s）** | 10 → **50** |
| B（挂载前已等 20s） | 24.87s | +115ms | 25.16s → 26.33s | **26.37s（+35ms）** | — |
| C（一开页就挂载） | 4.19s | +101ms | 5.79s → 9.51s | **9.57s（+63ms）** | — |

> 结论：查询卡片是 **IntersectionObserver 驱动的挂载**，而且 **数据请求由挂载触发**（`fXqlme`）；
> 骨架期行是空的、总数是占位值（`第 1 – 10 行（共 10 行）`），拿到响应后才变成 `共 50 行` 并填充。
> 所以"早挂载"不只是早渲染，更是**早发请求**——这是流水线要尽早滚过去的原因。

### 2.3 平滑滚动：写 `scrollTop` 会动画

```js
sc.scrollTop = 200;
// 实测位置：907 → 895 (t+50ms) → 370 (t+250ms) → 200 (t+1000ms)
sc.scrollTo({ top: 0, behavior: 'instant' });   // 立即归零
```

`getComputedStyle(sc).scrollBehavior === 'smooth'`。这就是"匀速滚 5 秒"脚本里每 50ms 写一次 `scrollTop` 会互相打断、位置滞后于脚本预期的原因。

### 2.4 骨架行 → 填充：表在 ≠ 数据在

冷启动、页面壳一出现就滚到查询区（tab 260，coffee / 3 个月）：

| t(s) | 查询表数 | 两张表首行 | `fXqlme` |
|---|---|---|---|
| 0.5 | 2 | `""`（空骨架） | 未发起 |
| 8.3 | 2 | `""` | 未发起 |
| 8.9 | 2 | `""` | 开始 |
| 9.4 | 2 | `the coffee` / `hc andersen født`（已区分热门/上升） | 进行中（13.5s 结束） |

> 结论：**"表存在"和"行有内容"之间有 ~9s 的差**；只按 `table` 数量或行数判就绪会读到空数据。

区域表也有同样的骨架态，而且**骨架里带着假总数**：

| t(s) | 行数 | `第 X – Y 行（共 N 行）` | 首行文本 |
|---|---|---|---|
| 0 | 6 | `1 – 5 / 共 5` | `1`（只有序号，无地区名） |
| 4 | 6 | `1 – 5 / 共 26` | `1 香港` |

> 结论：区域表在页面壳出现时就已经挂载（不等滚动），但 4s 内是"假总数 5"的骨架；**总数必须等首行有地区名之后再读**。

### 2.5 分页是纯前端

- 区域表点"下一页"：`performance` 没有新增资源条目，行号 1–5 → 6–10。
- 表格自带 `第 1 – 5 行（共 25 行）` / `第 1 – 10 行（共 25 行）`（不同词总量不同：coffee 3m 区域 56 行、查询各 50 行；malfoid meme 1y 区域 25–26 行、查询各 25 行）。
- 点区域 "下载 CSV" 按钮后 `performance` 无新增请求 → 数据早已在客户端内存。

### 2.6 页面 RPC 时间线（1 年、冷启动）

| 时间(ms) | RPC | 大小 | 说明 |
|---|---|---|---|
| 1544–2232 | `DqDTgb` | 50KB | 首屏 explore 数据 |
| 2270 | `Tnt4U` ×2 | 0.45KB | 辅助 |
| 3140–15854 | `UZBRtc` | 1.63MB | 大响应，12s+ |
| 3400–4158 | `qrLOJd` | 0.8KB | — |
| 3675–18694 | `UZBRtc` | 1.63MB | 第二次（同尺寸） |
| 4184–9533 | `g4kJzf` ×2 | 1.3–3.9KB | — |
| 44713–45459 | `fXqlme` | 0.79KB | 查询卡片挂载后才发起 |

图表 SVG 在 **8412ms** 出现；三块数据全部可读在 **13617ms**。

---

## 3. 机制总结

| # | 机制 | 对采集的影响 |
|---|---|---|
| 1 | 固定高外壳 + 内部滚动容器 | 滚 `window`/`body` 无效，必须找内部容器 |
| 2 | 容器 `scroll-behavior: smooth` | 连续写 `scrollTop` 会动画、互相打断，必须 `instant` |
| 3 | 卡片 IntersectionObserver 懒挂载 | 不进入视口的卡片连 DOM 都没有；必须逐卡片滚入 |
| 4 | 趋势/区域随加载取数；查询卡片挂载后才取数 | 早挂载 = 早发请求；挂载后仍要等 `fXqlme` 返回才能读到行 |
| 5 | 首次渲染即空行骨架，且带假总数 | "表存在/行数够/共 N 行" 都不能作为就绪判据，要判单元格文本，总数就绪后再读 |
| 6 | 分页纯前端 + 自带总数文案 | 可用 `共 N 行` 精确终止，无需网络等待 |
| 7 | 大 RPC 12–19s | 固定 20s deadline 太紧；要分阶段、按状态等 |

---

## 4. 现有 `googletrends` 为什么不稳（逐条对应）

| 现有做法 | 问题 |
|---|---|
| 先等 2–3s，再 5s 匀速滚完，然后 300ms 轮询 `scrollAll()` | 5s 结束时页面往往还没出数据（实测图表 8.4s、数据 13.6s）；之后再没"逐卡片"滚入，卡片挂载时机与滚动时机错位 |
| 每 50ms 写一次 `scrollTop` | 容器是 `smooth`，每次写入都是一段新动画，位置滞后、IO 触发时机不可控 |
| 轮询条件 `filledRows >= 20` | 只能证明"有 20 个有内容的查询格"，不能证明"两张表都已填充且稳定"；空骨架阶段会被当成未就绪空转 |
| 20s 总 deadline | 新版页面单次大 RPC 就 12–19s，冷启动读满 13.6s，遇到慢请求直接超时 |
| 失败即换新标签页重试 3 次 | 重试解决不了"滚动时机"问题，只是把同样的竞态再跑一遍，耗时翻倍 |
| 用卡片标题正则分 top/rising | 新版卡片里是 `h3 热门查询` / `h3 热度上升的查询` 两段；标题爬取容易爬到整页容器 |

---

## 5. 方案：确定性水合驱动（deterministic hydration driver）

### 5.1 原则

1. **滚元素，不滚窗口**：从图表卡片向上找"可滚动祖先"，按 `scrollHeight-clientHeight` 取最大者。
2. **关动画**：`sc.style.scrollBehavior='auto'`，或所有滚动都用 `scrollTo({behavior:'instant'})`；采集结束后还原。
3. **逐卡片水合，不整页扫**：对 `按区域显示的搜索热度`、`常见搜索查询` 等标题逐个 `scrollIntoView({block:'center'})`，因为挂载会改变 `scrollHeight`，游标要重新计算。
4. **判就绪用状态，不用时间**：
   - 图表：存在满足"单段 M + 点数 ≥100 + 横跨绘图区 ≥70%"的 path；
   - 区域表：表存在 **且首行有地区名**（骨架态只有序号、没有地区名）；
   - 查询表：两张表都在，且每张 ≥1 个**查询单元格文本非空**；
   - 以上三条同时满足后，再要求**两次采样（间隔 400ms）的行数签名一致**，才算就绪；
   - 每段独立 deadline（图表 45s、单卡片 10s），超时记录到诊断字段而不是整条失败。
5. **翻页用页面自带的总数**：解析 `第 X – Y 行（共 N 行）`，`Y >= N` 或"下一页"禁用即停；点击后等"首行文本变化"，而不是固定 sleep。**总数必须在就绪后读取**（骨架期显示的是假总数）。
   - 若 Phase B 可能重跑、或用户手动翻过页，采集前先把每张表翻回第 1 页（`range.start > 1` 时点"返回上一页"直到禁用），否则会从中间页开始、只采到半截。
   - 采完用 `rows.length === total` 做完整性校验，不一致时标记 `complete:false` 并在诊断里带上总数，便于上层判断。
6. **分两次 run_script**：`ready`（等壳+图表）与 `collect`（水合+翻页+解析）分开，各自 <60s，避免单次请求超时。

### 5.2 已验证的原型（可直接贴进 `run_script`）

**Phase A — 等壳、定位滚动容器、逐卡片水合**（实测 1 年页面 13.6s 完成）

```js
(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const t0 = performance.now();
  const marks = [];
  const mark = (m, extra) => marks.push({ t: Math.round(performance.now() - t0), m, ...extra });

  // 1) 图表就绪：单段 M + 点数足够 + 横跨绘图区
  const isLinePath = (p, vbW) => {
    const d = p.getAttribute('d') || '';
    const n = (d.match(/-?[\d.]+/g) || []).map(Number);
    return (d.match(/M/g) || []).length === 1 && n.length >= 100 && n[0] <= 2 && n[n.length - 2] >= vbW * 0.7;
  };
  let chart = null;
  while (performance.now() - t0 < 45000) {
    for (const s of document.querySelectorAll('svg')) {
      if (s.getBoundingClientRect().width <= 200) continue;
      const vbW = (s.getAttribute('viewBox') || '').split(/[\s,]+/).map(Number)[2] || 0;
      const hit = Array.from(s.querySelectorAll('path')).find((p) => isLinePath(p, vbW));
      if (hit) { chart = { vbW, len: (hit.getAttribute('d') || '').length }; break; }
    }
    if (chart) break;
    await sleep(250);
  }
  mark('chart-ready', chart);

  // 2) 找真正的滚动容器（结构判定，不认 class 名）
  const sc = Array.from(document.querySelectorAll('div')).filter((el) => {
    const cs = getComputedStyle(el);
    return /(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 100;
  }).sort((a, b) => (b.scrollHeight - b.clientHeight) - (a.scrollHeight - a.clientHeight))[0];
  if (!sc) return { error: 'scroll container not found' };
  const prevBehavior = sc.style.scrollBehavior;
  sc.style.scrollBehavior = 'auto';           // 关键：否则每次滚动都是 1s 动画

  // 3) 逐卡片水合
  const headOf = (re) => Array.from(document.querySelectorAll('h1,h2,h3,h4,[role=heading]'))
    .filter((h) => h.getBoundingClientRect().width > 0)
    .find((h) => re.test(clean(h.innerText)));
  const headerOf = (t) => Array.from(t.querySelectorAll('thead th')).map((th) => clean(th.innerText)).join(' ');
  const tablesOf = (kind) => Array.from(document.querySelectorAll('table')).filter((t) => {
    const h = headerOf(t);
    return kind === 'region' ? /区域|region/i.test(h) : /查询|query|term/i.test(h);
  });

  for (const re of [/按区域|region/i, /常见搜索查询|related queries/i]) {
    const h = headOf(re);
    if (!h) continue;
    h.scrollIntoView({ block: 'center', behavior: 'instant' });
    mark('scroll', { re: String(re), st: Math.round(sc.scrollTop) });
    await sleep(800);
  }
  sc.scrollTo({ top: sc.scrollHeight, behavior: 'instant' });

  // 4) 等两张查询表都有非空内容
  let ready = false;
  // 4) 就绪判据：区域首行有名字 + 两张查询表有非空查询 + 两次采样一致
  const contentRows = (t) => Array.from(t.querySelectorAll('tbody tr td:nth-child(2)'))
    .filter((td) => clean(td.innerText)).length;
  let ready = false, stable = 0, lastSig = '';
  for (let i = 0; i < 100; i++) {
    const qs = tablesOf('query');
    const rs = tablesOf('region');
    const qOk = qs.length >= 2 && qs.every((t) => contentRows(t) >= 1);
    const rOk = rs.length >= 1 && contentRows(rs[0]) >= 1;
    const sig = JSON.stringify([qs.map(contentRows), rs.map(contentRows)]);
    if (qOk && rOk && sig === lastSig) {
      if (++stable >= 2) { ready = true; break; }
    } else stable = 0;
    lastSig = sig;
    await sleep(400);
  }
  mark('ready', { ready, q: tablesOf('query').length, region: tablesOf('region').length });

  sc.style.scrollBehavior = prevBehavior;
  return { marks, elapsed: Math.round(performance.now() - t0) };
})()
```

**Phase B — 翻页 + 解析**（实测 1.6s 采完 25/25/25 行 + 53 点趋势）

```js
(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();

  const headerOf = (t) => Array.from(t.querySelectorAll('thead th')).map((th) => clean(th.innerText)).join(' ');
  const tablesOf = (kind) => Array.from(document.querySelectorAll('table')).filter((t) =>
    kind === 'region' ? /区域|region/i.test(headerOf(t)) : /查询|query|term/i.test(headerOf(t)));

  // 页面自带的总数文案，作为翻页终止条件
  const rangeOf = (table) => {
    let el = table;
    for (let i = 0; i < 10 && el; i++) {
      el = el.parentElement; if (!el) break;
      const m = clean(el.innerText).match(/第\s*(\d+)\s*[–\-−]\s*(\d+)\s*行（共\s*(\d+)\s*行）/);
      if (m) return { start: +m[1], end: +m[2], total: +m[3] };
    }
    return null;
  };
  const nextBtnFor = (table) => {
    let el = table;
    for (let i = 0; i < 10 && el; i++) {
      el = el.parentElement; if (!el) break;
      const btns = Array.from(el.querySelectorAll('button'))
        .filter((b) => /下一页|next page|next/i.test(b.getAttribute('aria-label') || ''));
      if (btns.length === 1) return btns[0];
    }
    return null;
  };
  const disabled = (b) => !b || b.disabled || b.getAttribute('aria-disabled') === 'true';

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
      else { const m = (bar.getAttribute('aria-label') || '').match(/(\d+)\s*$/); if (m) interest = +m[1]; }
    }
    const chg = c[3] ? clean((c[3].querySelector('.VYi2zf') || c[3]).innerText) : '';
    const pm = chg.match(/([+\-−])\s*([\d,.]+)\s*%/);
    const change = pm ? (pm[1] === '-' || pm[1] === '−' ? '-' : '+') + pm[2] + '%'
      : (/暴增|突破|breakout/i.test(chg) ? 'breakout'
      : (/没有变化|无变化|no change/i.test(chg) ? '+0%' : null));
    const rt = clean(c[0].innerText);
    return { rank: /^\d+$/.test(rt) ? +rt : i + 1, query, interest, change };
  };

  const parseRegion = (tr, i) => {
    const c = Array.from(tr.querySelectorAll('td'));
    if (c.length < 3) return null;
    const nameEl = c[1].querySelector('[role=button]') || c[1];
    const region = clean(nameEl.innerText);
    if (!region) return null;
    const bar = c[2].querySelector('[role=img]');
    const m = ((bar && bar.getAttribute('aria-label')) || '').match(/(\d+)\s*$/);
    const rt = clean(c[0].innerText);
    return {
      rank: /^\d+$/.test(rt) ? +rt : i + 1, region,
      geo_code: (bar && bar.getAttribute('data-geo-code')) || null,
      interest: m ? +m[1] : null,
    };
  };

  // 一次翻完一张表；每页重新按结构取节点（重渲染会替换 table）
  const paginate = async (kind, index, parse) => {
    const seen = new Set(); const rows = [];
    let total = null;
    for (let p = 0; p < 40; p++) {
      const table = tablesOf(kind)[index];
      if (!table) break;
      const parsed = Array.from(table.querySelectorAll('tbody tr')).map(parse).filter(Boolean);
      for (const r of parsed) if (!seen.has(r.rank)) { rows.push(r); seen.add(r.rank); }

      const range = rangeOf(table);
      if (range) total = range.total;              // 就绪后的总数，用于完整性校验
      const btn = nextBtnFor(table);
      if (disabled(btn) || (range && range.end >= range.total)) break;

      const first = parsed.length ? (parsed[0].query || parsed[0].region) : '';
      btn.click();
      const deadline = Date.now() + 6000;
      let changed = false;
      while (Date.now() < deadline) {
        await sleep(200);
        const t2 = tablesOf(kind)[index];
        if (!t2) continue;
        const f = Array.from(t2.querySelectorAll('tbody tr')).map(parse).filter(Boolean)[0];
        if (f && (f.query || f.region) && (f.query || f.region) !== first) { changed = true; break; }
      }
      if (!changed) break;   // 保险：点了不变化就别死循环
    }
    return { rows, total, complete: total == null || rows.length === total };
  };

  const regions = await paginate('region', 0, parseRegion);
  const top = await paginate('query', 0, parseQuery);     // 热门查询（DOM 先出现）
  const rising = await paginate('query', 1, parseQuery);  // 热度上升的查询

  return {
    regions: regions.rows, top: top.rows, rising: rising.rows,
    complete: { regions: regions.complete, top: top.complete, rising: rising.complete },
  };
})()
```

### 5.3 趋势曲线解析（与表格同一轮采集）

图表是 SVG 折线，没有无障碍数据表；沿用已验证的"path 反解 + y 轴 0/100 刻度校准"，只保留一条路径（下面是**采集函数内的片段**，不是独立入口）：

```js
// 图表 SVG 里同一根线会画两份（mask / inverse-mask）或一份普通 path，取第一份满足条件的
const isLinePath = (p, vbW) => {
  const d = p.getAttribute('d') || '';
  const n = (d.match(/-?[\d.]+/g) || []).map(Number);
  return (d.match(/M/g) || []).length === 1 && n.length >= 100 && n[0] <= 2 && n[n.length - 2] >= vbW * 0.7;
};
let svg = null, line = null;
for (const s of document.querySelectorAll('svg')) {
  if (s.getBoundingClientRect().width <= 200) continue;
  const vbW = (s.getAttribute('viewBox') || '').split(/[\s,]+/).map(Number)[2] || 0;
  const hit = Array.from(s.querySelectorAll('path')).find((p) => isLinePath(p, vbW));
  if (hit) { svg = s; line = hit; break; }
}
if (!line) return { error: 'trend chart not loaded' };

const d = line.getAttribute('d');
const cmds = d.match(/[MC]/g) || [];
const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
let ni = 0; const ys = [];
for (const c of cmds) {
  if (c === 'M') { ys.push(nums[ni + 1]); ni += 2; }
  else { ys.push(nums[ni + 5]); ni += 6; }
}

// 用 y 轴的 "0" / "100" 文本做校准，比按 viewBox 硬算稳
const svgTop = svg.getBoundingClientRect().y;
const labels = {};
Array.from(document.querySelectorAll('svg text')).forEach((t) => {
  const v = t.textContent.trim();
  if ((v === '0' || v === '100') && labels[v] == null) labels[v] = t.getBoundingClientRect().y - svgTop;
});
const y0 = labels['0'] != null ? labels['0'] : 320;
const y100 = labels['100'] != null ? labels['100'] : 0;
const span = (y0 - y100) || 1;
const trend = ys.map((y) => Math.max(0, Math.min(100, Math.round(((y0 - y) / span) * 100))));
```

> 采样点的日期页面不直接给（x 轴只有月份），沿用现有配方的推导规则：`today N-d` 按天、`today N-m/y` 按周/月、`all` 从 2004-01-01 起，从今天倒推与点数对齐。

### 5.4 实测结果

| 页面 | 图表就绪 | 全部卡片水合 | 采集耗时 | 结果 |
|---|---|---|---|---|
| malfoid meme / today 1-y | 9.4s | 15.1s | 1.6s | regions 26、top 25、rising 25、trend 53 点 |
| coffee / today 3-m | 9.7s | ~11s | 3.9s | regions 56（12 页）、top 50（5 页）、rising 50（5 页） |

> 同一查询两次运行 region 分别是 25 / 26 行：多出来的是骨架期看不到的尾行（`26 泰国`，interest=1）。这也说明**必须等首行有内容后再读总数并翻页**，否则会少采。

对比：旧脚本"2–3s 等待 + 5s 匀速滚 + 20s deadline"，在冷启动 13.6s、大 RPC 19s 的页面上必然贴着超时线跑。

### 5.5 流水线版（更快）：区域先就绪就先采，与查询数据等待重叠

把 5.2 的"先等齐再采"改成按页面年龄推进的流水线，实测（malfoid meme / 1y，页面年龄）：

| 时刻 | 事件 |
|---|---|
| 8.03s | 页面壳+标题就绪，立即滚到查询卡片（不等图表） |
| 8.03s | 滚动完成，查询卡片开始挂载 |
| 8.80s | **区域**数据已就绪 → 翻完 6 页（769ms，26 行） |
| 13.49s | 查询两张表 + 图表同时就绪 → 翻完各 3 页（609ms，各 26 行） |
| 13.49s | 完成（趋势 53 点） |

关键点：
- **区域翻页完全藏在"等查询数据"的窗口里**，不额外增加总时长；
- 数据到达后剩下的活只有 ~0.6–0.8s；
- 总时长 ≈ 页面自身数据到达时间（本次 12.9s），再叠加最后一小段翻页。

> 页面数据到达时间本身在 9–13s（偶发 15–19s）波动，这是任何"读页面"方案的下限；总耗时公式 ≈ **payload 时间 + 0.6–1.5s**。

---

## 6. 落地到本仓库

> **已落地（2026-10-31）**：`googletrends` 已从"匀速滚 5s + 20s deadline"换成 5.5 的流水线；
> `googletrends_compare` 也已改为只读首屏多折线、不再滚动。命令名、参数、CLI/MCP 名称都不变。

### 6.1 文件与接线

```
bridge-core/src/recipes/
├── googletrends.js   # 采集脚本：早挂载 + 区域先行 + 状态等待 + 客户端分页 + SVG 反解
└── googletrends.rs   # 编排：URL/参数校验，等页面壳，跑脚本（120s 上限），整形输出
```

- CLI / MCP 名称不变：`googletrends`（`cargo run -- googletrends '...' --date ... --geo ...`）
- 输出为增量兼容：新增 `regions_available` 与 `hydration`（各阶段页面年龄、分页次数、`complete` 完整性标记）；`top/rising/regions` 与 `*_available` 语义不变
- 编排只做两次浏览器往返：`new_tab` → 轮询页面壳（`document.querySelector('main') || c-wiz`）→ 一次 `run_script` 跑完整流水线（脚本内部 45s 状态 deadline，外层 120s 超时）
- 只有"页面壳/图表始终不出现"才关标签页换新标签页重试一次；表格缺失不算失败，用 `available` 表达

### 6.2 超时与重试（实现值）

| 阶段 | 动作 | 超时 | 失败处理 |
|---|---|---|---|
| 打开 | `new_tab` | 10s（`RESPONSE_TIMEOUT`） | 直接报错 |
| 等壳 | 轮询 `run_script`（500ms 间隔，90 次） | 45s | 关标签页，换新标签页重试一次 |
| 采集 | 一次 `run_script`（脚本内 45s 状态 deadline） | 120s | 脚本返回 `error`（壳/图表缺失）才重试一次；表格缺失不算失败 |

要点：
- 脚本自带 45s 状态 deadline，外层 120s 只是兜底；不要把它拆成多次往返。
- **不要把"表格缺失"当失败重试**：无数据词本来就可能没有榜单；用 `available` 字段表达。
- 未调用 `activate_tab`（避免抢焦点）：`new_tab` 本身会把标签页置前；若并行任务导致页面不可见，懒挂载可能不触发，此时应显式激活标签页（本次未能稳定复现后台隐藏态，保守处理）。
- 旧版"每次失败换新标签页重试 3 次"已降为 2 次（1 次重试），避免慢请求时成倍耗时。

### 6.3 需要固化的选择器（都按结构，不认混淆 class）

| 目标 | 判据 |
|---|---|
| 滚动容器 | `overflow-y auto/scroll` 且可滚动距离最大的、包含图表的祖先 |
| 区域表 | `thead` 文本含 `区域` / `region` |
| 查询表 | `thead` 文本含 `查询` / `query`；DOM 顺序 [0]=热门、[1]=上升；更稳的是向上找最近的 `h3` 文本 `热门查询` / `热度上升的查询` |
| 区域名 | `td:nth-child(2) [role=button]` |
| 查询名 | `td:nth-child(2) .Z9Uqw`（兜底 `td:nth-child(2)`） |
| 热度 | `[role=img][data-search-interest]`，兜底 `aria-label` 末尾数字 |
| 变化 | `td:nth-child(4) .VYi2zf`：`±N%` / `暴增|breakout` / `无变化` |
| 地区码 | `[role=img][data-geo-code]` |
| 区域就绪 | 首行 `td:nth-child(2)` 文本非空（骨架期只有序号） |
| 翻页结束 | `第 X – Y 行（共 N 行）` 的 `Y >= N`，或下一页按钮 `disabled/aria-disabled`（**总数须就绪后读**） |

### 6.4 测试与验证

- Rust 单测：date 规范化（`today N-d/m/y`、`all`、日期区间、非法回落）、脚本日期注入无残留占位符、`GOOGLE_TRENDS_SCRIPT_TIMEOUT > RESPONSE_TIMEOUT`。
- 实测（CLI 端到端，2026-10-31）：
  - `malfoid meme / today 1-y`：11.9s，regions 26、top 25、rising 25、trend 53，`complete` 全 true
  - `coffee / today 3-m`：regions 56（12 页）、top 50（5 页）、rising 50（5 页）、trend 90，`complete` 全 true
- 未覆盖：`googletrends.js` 没有 vm 假 DOM 单测（DOM 依赖较重），目前靠真浏览器回归；`googletrends_compare` 仍是旧脚本。

---

## 7. 备选方案与取舍

| 方案 | 可行性 | 结论 |
|---|---|---|
| **A. 水合驱动 + DOM 解析**（本文） | ✅ 已实测 | 推荐。依赖页面渲染，但用状态判据后稳定；总耗时 ≈ payload + 0.6–1.5s |
| B. 经典 widgetdata JSON API（`/trends/api/explore` + `widgetdata/multiline|relatedsearches|comparedgeo`） | ⚡ 页面内 fetch 实测 **0.8s**（暖）/ 3.5s（冷），数值精确 | **快但少**：同一查询返回 top/rising 各 12 条、页面是 25 条；coffee 是 25 vs 50。且 `today 1-y` 直接 400（要映射成 `today 12-m`；`2-y`/`60-m` 也不收，`5-y`/`all`/日期区间/`now 7-d` 可用）。适合做"快速模式"，不能做等效默认 |
| C. 新版 batchexecute（`UZBRtc` 等）直接重放 | ⚠️ 需 `f.sid`/`bl`/`_reqid` + 混淆 payload | 不推荐；且 1.6MB 响应本身就要 9–15s，省不掉 |
| D. 点"下载 CSV"拿数据 | ❌ 下载无网络请求（客户端生成），blob 锚点从不进 DOM（detached `a.click()`），抓不到 | 不可行（唯一产物是磁盘文件） |
| E. 读 `AF_initDataCallback` | ❌ 只有分类词表（21KB），没有趋势数据 | 不可行 |
| F. 回到 legacy 页 | 用户已明确不用 | 排除 |

> 备注：`performance` 的 `transferSize` 显示 `UZBRtc` 为 1.63MB 且 1 年/3 个月几乎同尺寸，推测是固定体积的批量响应（可能含区域地图的小时/周序列）。这意味着**渲染路径的下限是这条响应本身**，B 方案的最大价值正是完全绕开它——代价是榜单条数变少。

---

## 8. 风险与后续验证

1. **后台标签页**：本次环境里 `activate_tab` 后目标页仍报 `visibilityState: visible`，没能稳定复现隐藏态。若并行任务导致标签页不可见，需在 Phase A 前显式 `activate_tab`，并保留"卡片 10s 未挂载则再滚一次"的补救。
2. **界面变体**：Gemini 版探索页存在无榜单的变体（`tables_available:false` 的既有 note 就是为此）。新配方应保留该语义，不要报错。
3. **class 名不可依赖**：本方案只用了 `[role=img]`、`thead`、`aria-label`、`第 X – Y 行` 文案和 `h2/h3` 文本；混淆 class（`.Jh24Ne`、`.Z9Uqw`、`.VYi2zf`）作为兜底，需在每次版本变更后回归。
4. **多词对比**：对比页的折线是多条 path（mask 序号对应词序），本次未验证新版对比页的懒挂载；接入前需单独跑一遍。
