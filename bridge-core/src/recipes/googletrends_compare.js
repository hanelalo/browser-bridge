// 新版 Google Trends 多词对比：只读首屏那张多折线图。
//
// 与单关键词不同，这里不需要滚动触发任何卡片：
//   - 对比图在首屏（卡片一进页面就可见），不依赖懒挂载；
//   - 也不需要热门/上升/区域数据。
// 所以老脚本里的"等 2~3s + 匀速滚 5s"是纯浪费：直接按状态等 N 条折线出现即可。
//
// 折线有两种渲染变体（实测）：
//   1. mask 变体：每条线画两遍（inverse-mask + 普通 mask），mask 序号即词条顺序；
//   2. 普通 path 变体：每条线一条无 mask 的 path，DOM 顺序即词条顺序。
// 两条都要兼容。

(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const TERMS = __TERMS__;
  const N = TERMS.length;
  const DATE_SPEC = __DATE_SPEC__;
  const age = () => Math.round(Date.now() - performance.timeOrigin);
  const startedAt = age();

  // 曲线判定不依赖 mask 和 d 长度：单段 M 起笔 + 点数足够 + 横跨绘图区七成以上，
  // 图标/装饰路径都过不了这一关
  const isLinePath = (p, vbW) => {
    const d = p.getAttribute('d') || '';
    const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
    return (d.match(/M/g) || []).length === 1 && nums.length >= 100
      && nums[0] <= 2 && nums[nums.length - 2] >= vbW * 0.7;
  };

  const collectLines = () => {
    let plain = null;
    for (const s of document.querySelectorAll('svg')) {
      if (s.getBoundingClientRect().width <= 100) continue;
      const vbW = (s.getAttribute('viewBox') || '0 0 1384 320').split(/[\s,]+/).map(Number)[2] || 1280;
      const paths = Array.from(s.querySelectorAll('path'));
      const masked = paths.filter((p) => (p.getAttribute('mask') || '').includes('inverse-mask') && isLinePath(p, vbW));
      if (masked.length === N) return { svg: s, paths: masked };
      const unmasked = paths.filter((p) => !p.getAttribute('mask') && isLinePath(p, vbW));
      if (unmasked.length === N) return { svg: s, paths: unmasked };
      if (unmasked.length > (plain ? plain.paths.length : 0)) plain = { svg: s, paths: unmasked };
    }
    return plain;
  };

  let got = null;
  let lastSeen = 0;
  let foundAt = null;
  const deadline = startedAt + 45000;
  while (age() < deadline) {
    got = collectLines();
    if (got && got.paths.length === N) { foundAt = age(); break; }
    if (got) lastSeen = Math.max(lastSeen, got.paths.length);
    await sleep(150);
  }
  if (!got || got.paths.length !== N) {
    return { error: 'comparison chart not loaded (expected ' + N + ' lines, got ' + (got ? got.paths.length : lastSeen) + ')' };
  }

  const { svg, paths: lines } = got;
  const maskIdx = (p) => {
    const m = (p.getAttribute('mask') || '').match(/timeline-inverse-mask-\d+-(\d+)/);
    return m ? parseInt(m[1], 10) : 0;
  };
  lines.sort((a, b) => maskIdx(a) - maskIdx(b));

  const parseLine = (p) => {
    const d = p.getAttribute('d');
    const cmds = d.match(/[MC]/g) || [];
    const nums = (d.match(/-?[\d.]+/g) || []).map(Number);
    let ni = 0;
    const ys = [];
    for (const c of cmds) {
      if (c === 'M') { ys.push(nums[ni + 1]); ni += 2; }
      else { ys.push(nums[ni + 5]); ni += 6; }
    }
    return ys;
  };

  // y 轴刻度校准：用页面上的 "0" / "100" 文本，比按 viewBox 硬算稳定
  const vb = (svg.getAttribute('viewBox') || '0 0 1384 320').split(/[\s,]+/).map(Number);
  const svgTop = svg.getBoundingClientRect().y;
  const labelY = {};
  Array.from(document.querySelectorAll('svg text')).forEach((t) => {
    const v = t.textContent.trim();
    if (v === '0' && labelY['0'] == null) labelY['0'] = t.getBoundingClientRect().y - svgTop;
    if (v === '100' && labelY['100'] == null) labelY['100'] = t.getBoundingClientRect().y - svgTop;
  });
  const y0 = labelY['0'] != null ? labelY['0'] : (vb[3] || 320);
  const y100 = labelY['100'] != null ? labelY['100'] : 0;
  const span = (y0 - y100) || 1;
  const toValue = (y) => Math.max(0, Math.min(100, Math.round(((y0 - y) / span) * 100)));

  // 日期：页面 x 轴只有月份，按 date 参数 + 点数从今天倒推（与单查询一致）
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

  const counts = lines.map(parseLine);
  const n = counts[0].length;
  const dates = buildDates(DATE_SPEC, n);
  const series = TERMS.map((term, i) => {
    const trend = counts[i].map((y, j) => ({ date: dates[j] || null, value: toValue(y) }));
    return { term, trend };
  });
  return { series, hydration: { found_at: foundAt, page_start: startedAt, points: n, lines: N, elapsed_ms: age() - startedAt } };
})()
