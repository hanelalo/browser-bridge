async () => {
  const scriptDeadline = Date.now() + 105000;
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const clean = (s) => String(s || '').replace(/[\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, '').trim();
  const text = (el) => clean(el?.textContent);
  const visible = (el) => !!el && el.getBoundingClientRect().height > 0;
  const card = (id) => document.querySelector(`trends-widget[widget-name="${id}"]`);
  const disabled = (el) => !el || el.disabled || el.hasAttribute('disabled') || el.getAttribute('aria-disabled') === 'true';
  const noData = (el) => /没有足够的(搜索量|相关数据|数据)|数据不足|没有相关(查询|数据)|无相关数据|not enough (search volume|data)|no (related queries|data)/i.test(el?.querySelector('.widget-error')?.innerText || '');
  const wait = async (fn, ms = 5000) => {
    const deadline = Math.min(Date.now() + ms, scriptDeadline);
    do {
      if (fn()) return true;
      await sleep(150);
    } while (Date.now() < deadline);
    return false;
  };
  const errors = {};
  // Only read rendered DOM and operate page controls. No fetch/XHR or API fallback.
  const ready = async (id, selector) => {
    let scrolled = null;
    return wait(() => {
      const el = card(id);
      if (el && el !== scrolled) { el.scrollIntoView({ block: 'center' }); scrolled = el; }
      return el && (el.querySelector(selector) || noData(el));
    }, id === 'TIMESERIES' ? 45000 : 15000);
  };
  const rows = (id, kind) => Array.from(card(id)?.querySelectorAll('.item') || [])
    .filter(visible).map((item) => {
      const rank = Number(text(item.querySelector('.label-line-number')));
      const name = text(item.querySelector('.label-text'));
      if (!rank || !name) throw new Error('榜单行尚未完整渲染');
      const value = text(item.querySelector(kind === 'rising' ? '.rising-value' : '.progress-value'));
      const interest = /^\d+$/.test(value) ? Number(value) : null;
      if (kind === 'region') {
        const href = item.querySelector('a[href]')?.getAttribute('href');
        return { rank, region: name, geo_code: href ? new URL(href, location.href).searchParams.get('geo') : null, interest };
      }
      const growth = value.replace(/[,，\s]/g, '').replace('−', '-');
      const change = /breakout|飙升|暴增|突破/i.test(value) ? 'breakout'
        : /^[+\-]?\d+(?:\.\d+)?%$/.test(growth) ? (growth.startsWith('-') || growth.startsWith('+') ? growth : '+' + growth) : null;
      return { rank, query: name, interest: kind === 'top' ? interest : null, change: kind === 'rising' ? change : null };
    });
  const button = (id, direction) => card(id)?.querySelector(`.pagination button[aria-label="${direction}"]`);
  const firstRank = (id) => text(card(id)?.querySelector('.item .label-line-number'));
  const step = async (id, direction) => {
    const before = firstRank(id);
    button(id, direction).click();
    if (!await wait(() => firstRank(id) && firstRank(id) !== before)) throw new Error('分页未更新，结果不完整');
    await sleep(150);
  };
  const collect = async (id, kind) => {
    const out = [];
    try {
      if (noData(card(id))) return { rows: [], available: true };
      // Support rerunning on an existing page that is not currently on page one.
      for (let i = 0; !disabled(button(id, 'Previous')); i++) {
        if (i >= 60) throw new Error('无法返回榜单第一页');
        await step(id, 'Previous');
      }
      for (let page = 0; page < 60; page++) {
        const current = rows(id, kind);
        if (!current.length) throw new Error('榜单没有行且未显示无数据状态');
        for (const row of current) {
          if (row.rank !== out.length + 1) throw new Error('榜单排名不连续，结果不完整');
          out.push(row);
        }
        const pagination = text(card(id)?.querySelector('.pagination'));
        const total = pagination.match(/共\s*(\d+)|of\s+(\d+)/i);
        const expected = total ? Number(total[1] || total[2]) : null;
        const next = button(id, 'Next');
        if (disabled(next)) {
          if (expected != null && expected !== out.length) throw new Error('榜单未完整采集');
          return { rows: out, available: true };
        }
        await step(id, 'Next');
      }
      throw new Error('榜单超过分页上限，结果不完整');
    } catch (error) {
      errors[kind] = error.message;
      return { rows: out, available: false };
    }
  };
  const collectQueries = async (kind) => {
    const id = 'RELATED_QUERIES';
    try {
      if (!await ready(id, '.item')) throw new Error('相关查询未加载或被限制');
      if (noData(card(id))) return { rows: [], available: true };
      const select = card(id).querySelector('md-select');
      if (!select) throw new Error('未找到热门/上升切换控件');
      const menu = () => document.getElementById(select.getAttribute('aria-owns'));
      const value = kind === 'top' ? 'bullets' : 'risingBullets';
      if (select.getAttribute('aria-expanded') !== 'true') select.click();
      if (!await wait(() => select.getAttribute('aria-expanded') === 'true' && menu()?.querySelector(`md-option[value="${value}"]`))) throw new Error('查询切换菜单未展开');
      const option = menu().querySelector(`md-option[value="${value}"]`);
      if (!option) throw new Error('未找到查询类型');
      if (disabled(option)) {
        // A disabled option is the page's explicit empty-list state.
        document.querySelector('md-backdrop._md-select-backdrop')?.click();
        return { rows: [], available: true };
      }
      option.click();
      if (!await wait(() => option.getAttribute('aria-selected') === 'true'
          && card(id)?.querySelector(kind === 'rising' ? '.item .rising-value' : '.item .progress-value'))) throw new Error('查询类型切换未完成');
      await sleep(200);
      return await collect(id, kind);
    } catch (error) {
      errors[kind] = error.message;
      return { rows: [], available: false };
    }
  };
  try {
    if (!await ready('TIMESERIES', 'table tbody tr')) throw new Error('经典版趋势表格未加载（可能被限制或需要验证）');
    const trend = noData(card('TIMESERIES')) ? [] : Array.from(card('TIMESERIES').querySelectorAll('table tbody tr')).map((row) => {
      const cells = row.querySelectorAll('td');
      const date = text(cells[0]);
      const valueText = text(cells[1]);
      if (!date || cells.length !== 2) throw new Error('趋势表格结构不正确');
      // Preserve labels exactly as displayed; do not infer missing years, timezone or partial flags.
      return { date, value: /^\d+(?:\.\d+)?$/.test(valueText) ? Number(valueText) : null,
        ...( /^\d+(?:\.\d+)?$/.test(valueText) ? {} : { value_text: valueText } ) };
    });
    const rising = await collectQueries('rising');
    const top = await collectQueries('top');
    let regions = { rows: [], available: false };
    if (await ready('GEO_MAP', '.item')) regions = await collect('GEO_MAP', 'region');
    else errors.region = '地区榜单未加载或被限制';
    return { source: 'page_dom', trend, top: top.rows, rising: rising.rows, regions: regions.rows,
      top_table_available: top.available, rising_table_available: rising.available,
      tables_available: top.available || rising.available, regions_available: regions.available,
      ...(Object.keys(errors).length ? { errors } : {}) };
  } catch (error) {
    return { error: error.message };
  }
}
