// DOM interaction regression tests for the legacy compare collector; no browser, network or npm dependencies required.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
const script = readFileSync(new URL('../src/recipes/oldgoogletrends_compare.js', import.meta.url), 'utf8');

async function execute(config = {}) {
  let clock = 0, networkCalls = 0;
  const terms = config.terms ?? ['coffee', 'tea'];
  const dataTableRows = config.rows ?? [
    ['\u202a9月17日\u202c', '49', '24'],
    ['9月18日', '48', '<1'],
  ];
  const table = (headCells, rows) => ({
    querySelectorAll: (s) => {
      if (s === 'thead th') return headCells.map((c) => ({ textContent: c }));
      if (s === 'tbody tr') return rows.map((cells) => ({
        querySelectorAll: (sel) => sel === 'td' ? cells.map((c) => ({ textContent: c })) : [],
      }));
      return [];
    },
  });
  // 对比页 TIMESERIES 内还有一张均值摘要表（表头 x1 + 词名），必须被数据表选择排除
  const summary = table(['x1', ...terms], [['平均值', '44', '22']]);
  const data = table(['x', ...terms.map((_, i) => 'y' + (i + 1))], dataTableRows);
  const widget = {
    scrollIntoView() {},
    querySelector: (s) => s === '.widget-error' && config.noData
      ? { innerText: '抱歉，您的搜索没有足够的相关数据，因此无法显示在此处。' } : null,
    querySelectorAll: (s) => {
      if (s !== 'table' || config.noData || config.noDataTable) return [];
      return config.withoutSummary ? [data] : [summary, data];
    },
  };
  class Clock extends Date { static now() { return clock; } }
  const result = await runInNewContext(`(${script})(${JSON.stringify(terms)})`, {
    document: { querySelector: () => widget },
    Date: Clock,
    setTimeout(fn, ms) { clock += ms; fn(); },
    fetch() { networkCalls++; throw Error('Unexpected API request'); },
    XMLHttpRequest: class { constructor() { networkCalls++; throw Error('Unexpected XHR'); } },
  });
  assert.equal(networkCalls, 0);
  return { result: JSON.parse(JSON.stringify(result)) };
}

test('reads one shared-scale series per term and skips the average summary table', async () => {
  const { result } = await execute();
  assert.equal(result.source, 'page_dom');
  assert.equal(result.series.length, 2);
  assert.deepEqual(result.series[0], {
    term: 'coffee',
    trend: [{ date: '9月17日', value: 49 }, { date: '9月18日', value: 48 }],
  });
  assert.deepEqual(result.series[1], {
    term: 'tea',
    trend: [{ date: '9月17日', value: 24 }, { date: '9月18日', value: null, value_text: '<1' }],
  });
});

test('single term still matches the x + y1 data table', async () => {
  const { result } = await execute({ terms: ['coffee'], rows: [['9月17日', '49']], withoutSummary: true });
  assert.deepEqual(result.series, [{ term: 'coffee', trend: [{ date: '9月17日', value: 49 }] }]);
});

test('explicit no-data message returns empty trends with a note', async () => {
  const { result } = await execute({ noData: true });
  assert.deepEqual(result.series, [
    { term: 'coffee', trend: [] },
    { term: 'tea', trend: [] },
  ]);
  assert.match(result.note, /数据不足/);
});

test('missing data table is an error, not empty data', async () => {
  const { result } = await execute({ noDataTable: true });
  assert.match(result.error, /对比表格未加载/);
  assert.equal(result.series, undefined);
});

test('row with unexpected column count is a structure error', async () => {
  const { result } = await execute({ rows: [['9月17日', '49']] });
  assert.match(result.error, /结构不正确/);
});
