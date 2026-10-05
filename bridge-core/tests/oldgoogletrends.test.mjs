// DOM interaction regression tests; no browser, network or npm dependencies required.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
const script = readFileSync(new URL('../src/recipes/oldgoogletrends.js', import.meta.url), 'utf8');

async function execute(config = {}) {
  let clock = 0, networkCalls = 0, mode = 'rising', expanded = false;
  const pages = { top: 0, rising: config.startOnLastPage ? 1 : 0, region: 0 };
  const clicks = [];
  const lists = {
    rising: [ ['makeup mania roblox', '+3,400%'], ['headless roblox', '+180%'], ['new game', '飙升'] ],
    top: [ ['roblox game', '100'], ['roblox codes', '55'] ],
    region: [ ['蒙古', '100'], ['菲律宾', '97'], ['美国', '0'] ],
  };
  const node = (content = '', selectors = {}, attrs = {}) => ({
    textContent: content, innerText: content, disabled: false,
    getBoundingClientRect: () => ({ height: 20 }), scrollIntoView() {},
    hasAttribute: (name) => Object.hasOwn(attrs, name),
    getAttribute: (name) => attrs[name] ?? null,
    querySelector: (s) => selectors[s] ?? null,
    querySelectorAll: (s) => selectors[s] ?? [],
  });
  const currentRows = (kind) => lists[kind].slice(pages[kind] * 2, pages[kind] * 2 + 2).map(([name, value], i) => node('', {
    '.label-line-number': node(String(pages[kind] * 2 + i + 1)), '.label-text': node(name),
    '.rising-value': kind === 'rising' ? node(value) : null,
    '.progress-value': kind !== 'rising' ? node(value) : null,
  }));
  const button = (kind, direction) => {
    const result = node();
    result.disabled = direction === 'Previous' ? pages[kind] === 0 : (pages[kind] + 1) * 2 >= lists[kind].length;
    result.click = () => {
      clicks.push([kind, direction]);
      if (!(config.stuckPagination && direction === 'Next')) pages[kind] += direction === 'Next' ? 1 : -1;
    };
    return result;
  };
  const option = (kind) => ({
    ...node(), disabled: config.emptyRising && kind === 'rising',
    getAttribute: (attr) => attr === 'aria-selected' ? String(mode === kind) : null,
    click() { clicks.push(['select', kind]); if (!config.stuckSwitch) { mode = kind; expanded = false; pages[kind] = 0; } },
  });
  const options = { rising: option('rising'), top: option('top') };
  const menu = node('', {
    'md-option[value="risingBullets"]': options.rising,
    'md-option[value="bullets"]': options.top,
  });
  const select = {
    ...node(),
    getAttribute: (name) => name === 'aria-owns' ? 'query-menu' : name === 'aria-expanded' ? String(expanded) : null,
    click() { expanded = true; },
  };
  const makeCard = (id) => {
    const empty = '抱歉，您的搜索没有足够的相关数据，因此无法显示在此处。';
    const card = node(config.noData ? empty : '');
    const kind = () => id === 'GEO_MAP' ? 'region' : mode;
    const timeline = [ ['\u202a9月28日 14:00\u202c', '47'], ['9月28日 15:00', '0'], ['9月28日 16:00', '<1'] ]
      .map(([date, value]) => node('', { td: [node(date), node(value)] }));
    card.querySelectorAll = (s) => {
      if (config.noData) return [];
      if (s === 'table tbody tr') return config.missingTimeline ? [] : timeline;
      if (s === '.item') return currentRows(kind());
      return [];
    };
    card.querySelector = (s) => {
      if (s === '.widget-error') return config.noData ? node(empty) : null;
      if (config.noData) return null;
      if (s === 'table tbody tr') return config.missingTimeline ? null : timeline[0];
      if (s === 'md-select') return select;
      if (s === '.item') return currentRows(kind())[0];
      if (s === '.item .label-line-number') return currentRows(kind())[0]?.querySelector('.label-line-number');
      if (s === '.item .rising-value' || s === '.item .progress-value') return currentRows(kind())[0]?.querySelector(s.replace('.item ', ''));
      if (s === '.pagination') return node(`当前显示的是第 1-2 个（共 ${lists[kind()].length} 个）`);
      if (s.includes('aria-label="Previous"')) return button(kind(), 'Previous');
      if (s.includes('aria-label="Next"')) return button(kind(), 'Next');
      return null;
    };
    return card;
  };
  const cards = Object.fromEntries(['TIMESERIES', 'RELATED_QUERIES', 'GEO_MAP'].map((id) => [id, makeCard(id)]));
  const document = {
    querySelector(s) {
      if (s.startsWith('md-backdrop')) return { click() { expanded = false; } };
      return cards[s.match(/widget-name="([^"]+)"/)?.[1]] || null;
    },
    getElementById: (id) => id === 'query-menu' ? menu : null,
  };
  class Clock extends Date { static now() { return clock; } }
  const result = await runInNewContext(`(${script})()`, {
    document, Date: Clock, URL, location: { href: 'https://trends.google.com/trends/explore' },
    setTimeout(fn, ms) { clock += ms; fn(); },
    fetch() { networkCalls++; throw Error('Unexpected API request'); },
    XMLHttpRequest: class { constructor() { networkCalls++; throw Error('Unexpected XHR'); } },
  });
  assert.equal(networkCalls, 0);
  return { result: JSON.parse(JSON.stringify(result)), clicks };
}

test('reads displayed dates/values without inventing timestamps and paginates all lists', async () => {
  const { result, clicks } = await execute();
  assert.equal(result.source, 'page_dom');
  assert.deepEqual(result.trend, [
    { date: '9月28日 14:00', value: 47 }, { date: '9月28日 15:00', value: 0 },
    { date: '9月28日 16:00', value: null, value_text: '<1' },
  ]);
  assert.equal(result.rising.length, 3);
  assert.equal(result.rising[0].change, '+3400%');
  assert.equal(result.rising[0].interest, null);
  assert.equal(result.rising[2].change, 'breakout');
  assert.deepEqual(result.top[0], { rank: 1, query: 'roblox game', interest: 100, change: null });
  assert.equal(result.regions.length, 3);
  assert.equal(result.regions[0].geo_code, null);
  assert.equal(result.regions[2].interest, 0);
  assert.equal(result.errors, undefined);
  assert.ok(clicks.some(([kind, direction]) => kind === 'rising' && direction === 'Next'));
});

test('explicit no-data message is successful empty data', async () => {
  const { result, clicks } = await execute({ noData: true });
  assert.deepEqual(result.trend, []);
  assert.deepEqual(result.top, []);
  assert.equal(result.tables_available, true);
  assert.equal(result.regions_available, true);
  assert.equal(clicks.length, 0);
});

test('disabled rising option means empty rising, not failed loading', async () => {
  const { result } = await execute({ emptyRising: true });
  assert.deepEqual(result.rising, []);
  assert.equal(result.rising_table_available, true);
  assert.equal(result.top.length, 2);
});

test('pagination failure reports partial data as unavailable', async () => {
  const { result } = await execute({ stuckPagination: true });
  assert.equal(result.rising.length, 2);
  assert.equal(result.rising_table_available, false);
  assert.match(result.errors.rising, /分页/);
  assert.equal(result.top_table_available, true);
});

test('failed switch never reports the other list as top', async () => {
  const { result } = await execute({ stuckSwitch: true });
  assert.equal(result.top_table_available, false);
  assert.deepEqual(result.top, []);
  assert.match(result.errors.top, /切换/);
});

test('missing chart is an error, not empty data', async () => {
  const { result } = await execute({ missingTimeline: true });
  assert.match(result.error, /趋势表格未加载/);
  assert.equal(result.trend, undefined);
});
