async (TERMS) => {
  const scriptDeadline = Date.now() + 105000;
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const clean = (s) => String(s || '').replace(/[\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, '').trim();
  const text = (el) => clean(el?.textContent);
  const card = (id) => document.querySelector(`trends-widget[widget-name="${id}"]`);
  const noData = (el) => /没有足够的(搜索量|相关数据|数据)|数据不足|没有相关(查询|数据)|无相关数据|not enough (search volume|data)|no (related queries|data)/i.test(el?.querySelector('.widget-error')?.innerText || '');
  const wait = async (fn, ms = 5000) => {
    const deadline = Math.min(Date.now() + ms, scriptDeadline);
    do {
      if (fn()) return true;
      await sleep(150);
    } while (Date.now() < deadline);
    return false;
  };
  try {
    const N = TERMS.length;
    // 对比页 TIMESERIES 内有两张表：均值摘要表（表头 x1 + 词名）和逐日数据表（表头 x + y1..yN）。
    // 只取逐日数据表，列数 = 词条数 + 1，词序与 q 参数一致。
    let scrolled = null;
    const dataTable = () => {
      const widget = card('TIMESERIES');
      if (widget && widget !== scrolled) { widget.scrollIntoView({ block: 'center' }); scrolled = widget; }
      return Array.from(widget?.querySelectorAll('table') || []).find((t) => {
        const head = Array.from(t.querySelectorAll('thead th')).map(text);
        return head.length === N + 1 && head[0] === 'x' && head.slice(1).every((h, i) => h === 'y' + (i + 1));
      }) || null;
    };
    const rowCount = () => (dataTable()?.querySelectorAll('tbody tr') || []).length;
    if (!await wait(() => rowCount() > 0 || noData(card('TIMESERIES')), 45000)) {
      throw new Error('经典版对比表格未加载（可能被限制或需要验证）');
    }
    if (noData(card('TIMESERIES'))) {
      return { series: TERMS.map((term) => ({ term, trend: [] })), note: '页面提示数据不足' };
    }
    // 行数连续两次采样一致才算渲染稳定，避免 Angular 增量渲染时采到半张表
    let stable = -1;
    await wait(() => {
      const c = rowCount();
      const ok = c > 0 && c === stable;
      stable = c;
      return ok;
    }, 15000);
    const table = dataTable();
    const trend = TERMS.map(() => []);
    for (const row of Array.from(table?.querySelectorAll('tbody tr') || [])) {
      const cells = Array.from(row.querySelectorAll('td')).map(text);
      if (cells.length !== N + 1) throw new Error('对比表格结构不正确');
      const [date, ...values] = cells;
      if (!date) throw new Error('对比表格缺少时间标签');
      values.forEach((v, i) => {
        // 保留页面显示的时间标签与文本值，不做时区或年份推断
        trend[i].push(/^\d+(?:\.\d+)?$/.test(v) ? { date, value: Number(v) } : { date, value: null, value_text: v });
      });
    }
    if (!trend[0].length) throw new Error('对比表格没有数据行');
    return { source: 'page_dom', series: TERMS.map((term, i) => ({ term, trend: trend[i] })) };
  } catch (error) {
    return { error: error.message };
  }
}
