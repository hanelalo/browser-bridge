import assert from 'node:assert/strict';
import test from 'node:test';
import { background } from './helpers/background.mjs';

test('every client falls back to its own bridge window, never the user active window', async () => {
  const bg = await background({ agentWindows: [['mcp-a', 5]] });
  for (let i = 0; i < 3; i++) assert.equal((await bg.resolveTab(undefined, 'mcp-a')).id, 10);
  // CLI 同样落到自己的专用窗口（不存在则新建，mock 里新建窗口 id=100、初始 tab id=101），
  // 绝不使用用户当前窗口的激活页（tab 20）
  const first = await bg.resolveTab(undefined, 'cli');
  const second = await bg.resolveTab(undefined, 'cli');
  assert.equal(first.id, 101);
  assert.equal(second.id, first.id);
  assert.equal(bg.creations(), 1);
});

test('server-fired close_tab without a permit executes directly', async () => {
  const bg = await background({ autoTabs: [[10, 'cli']] });
  const replies = [];
  const socket = { readyState: 1, send: (s) => replies.push(JSON.parse(s)) };
  // 无 _window_ids 的直发 close_tab（延迟关闭 / 容量逐出）：豁免准入直接执行
  await bg.handleMessage(socket, JSON.stringify({ id: 'd1', method: 'close_tab', params: { tab_id: 10 }, client_id: '' }));
  assert.equal(replies[0].success, true);
  assert.equal(replies[0].result.closed, true);
  assert.equal(bg.tabs.has(10), false);
});

test('list_windows marks bridge-created windows', async () => {
  const bg = await background();
  const replies = [];
  const socket = { readyState: 1, send: (s) => replies.push(JSON.parse(s)) };
  await bg.handleMessage(socket, JSON.stringify({ id: 'nw', method: 'new_window', params: { _window_ids: [] }, client_id: 'cli' }));
  await bg.handleMessage(socket, JSON.stringify({ id: 'lw', method: 'list_windows', params: {} }));
  const lw = replies.find((r) => r.id === 'lw');
  assert.ok(lw.success);
  const flags = lw.result.map((w) => w.bridge).sort();
  // 预置的 5/9 是用户窗口，new_window 新建的是 bridge 窗口
  assert.deepEqual(flags, [false, false, true]);
});

test('parallel first use creates exactly one default window', async () => {
  const bg = await background();
  const ids = await Promise.all(Array.from({ length: 5 }, () => bg.ensureAgentWindow('mcp-new')));
  assert.equal(new Set(ids).size, 1);
  assert.equal(bg.creations(), 1);
});

test('target discovery never creates a default window before server admission', async () => {
  const bg = await background();
  const route = await bg.resolveTarget('click', {}, 'mcp-new');
  assert.equal(route.needs_default_window, true);
  assert.equal(bg.creations(), 0);
});

test('tab-only and default routes resolve to physical windows and reject mismatches', async () => {
  const bg = await background({ agentWindows: [['mcp-a', 5]] });
  assert.equal((await bg.resolveTarget('click', { tab_id: 10 }, 'mcp-b')).window_ids[0], 5);
  assert.equal((await bg.resolveTarget('click', {}, 'mcp-a')).params.tab_id, 10);
  await assert.rejects(bg.resolveTarget('click', { tab_id: 10, window_id: 9 }, 'mcp-a'), /does not belong/);
  await assert.rejects(bg.resolveTarget('click', { window_id: '5' }, 'mcp-a'), /integer/);
});

test('moved tabs fail before an operation starts', async () => {
  const bg = await background();
  const scope = await bg.resolveTarget('click', { tab_id: 10 }, 'mcp-a');
  bg.tabs.set(10, { id: 10, windowId: 9 });
  await assert.rejects(bg.validateExecutionScope('click', scope.params, [5]), /moved/);
});

test('cleanup targets are frozen and reserve all affected windows', async () => {
  const bg = await background({ autoTabs: [[10, 'mcp-a'], [20, 'mcp-a']] });
  const scope = await bg.resolveTarget('close_auto_tabs', { owner: 'mcp-a', _tabs: [{ tab_id: 99 }] }, 'mcp-a');
  assert.equal(JSON.stringify(scope.window_ids), '[5,9]');
  assert.equal(scope.params._tabs.length, 2);
  bg.tabs.set(10, { id: 10, windowId: 99 });
  await assert.rejects(bg.validateExecutionScope('close_auto_tabs', scope.params, [5, 9]), /moved/);
});

test('completed results are discarded and a new dispatch executes again', async () => {
  const bg = await background();
  const replies = [];
  const socket = { readyState: 1, send: (s) => replies.push(JSON.parse(s)) };
  const raw = JSON.stringify({ id: 'once', method: 'new_window', params: { _window_ids: [] }, client_id: 'mcp-a' });
  await bg.handleMessage(socket, raw);
  await bg.handleMessage(socket, raw);
  assert.equal(bg.creations(), 2);
  assert.equal(replies.length, 2);
  assert.equal(bg.data.bridgeExecutions.length, 0);
  const restarted = await background(bg.data);
  const result = await restarted.syncExecutions(['once']);
  assert.equal(result.executions[0].state, 'finished');
  assert.equal(result.executions[0].response, undefined);
});

test('large results never enter session storage or reconnect replies', async () => {
  const bg = await background({}, { quotaBytes: 10 * 1024 * 1024 });
  const large = 'x'.repeat(12 * 1024 * 1024);
  bg.chrome.scripting.executeScript = async () => [{ result: { text: large } }];
  let response;
  const socket = { readyState: 1, send: (s) => { response = JSON.parse(s); } };
  await bg.handleMessage(socket, JSON.stringify({ id: 'large', method: 'get_page_content',
    params: { tab_id: 10, window_id: 5, _window_ids: [5] } }));
  assert.equal(response.success, true);
  assert.equal(response.result.text, large);
  assert.equal(bg.data.bridgeExecutions.length, 0);
  assert.ok(bg.storageWrites.every((bytes) => bytes < 1024));
  const sync = await bg.syncExecutions(['large']);
  assert.equal(JSON.stringify(sync), '{"executions":[{"id":"large","state":"finished"}]}');
});

test('startup removes legacy result cache before writing, preserving unfinished locks', async () => {
  const bg = await background({ bridgeExecutions: [
    ['large-old', { state: 'completed', window_ids: [9], response: { result: 'x'.repeat(10 * 1024 * 1024) } }],
    ['active-old', { state: 'running', window_ids: [5] }],
  ] }, { quotaBytes: 10 * 1024 * 1024 });
  assert.ok(bg.storageWrites.every((bytes) => bytes < 1024));
  assert.equal(JSON.stringify(bg.data.bridgeExecutions), '[["active-old",{"state":"unknown","window_ids":[5]}]]');
  const sync = await bg.syncExecutions(['large-old', 'active-old']);
  assert.equal(sync.executions.find((r) => r.id === 'large-old').state, 'finished');
  assert.equal(sync.executions.find((r) => r.id === 'active-old').state, 'unknown');
});

test('running journal entries become unknown after worker restart, not finished', async () => {
  const bg = await background({ bridgeExecutions: [['old', { state: 'running', window_ids: [5] }]] });
  assert.equal((await bg.syncExecutions(['old'])).executions[0].state, 'unknown');
  bg.windows.delete(5);
  assert.equal((await bg.syncExecutions(['old'])).executions[0].state, 'closed');
  assert.equal(bg.data.bridgeExecutions.length, 0);
});

test('background rejects overlapping operations even if a server sends invalid permits', async () => {
  const bg = await background();
  let finish;
  bg.chrome.scripting.executeScript = () => new Promise((resolve) => { finish = () => resolve([{ result: {} }]); });
  const replies = [];
  const socket = { readyState: 1, send: (s) => replies.push(JSON.parse(s)) };
  const command = (id) => JSON.stringify({ id, method: 'get_page_content', params: { tab_id: 10, window_id: 5, _window_ids: [5] } });
  const first = bg.handleMessage(socket, command('first'));
  while (!finish) await new Promise((r) => setImmediate(r));
  assert.equal(bg.data.bridgeExecutions.length, 1);
  await bg.handleMessage(socket, command('first'));
  assert.equal(replies.length, 0, 'an unfinished duplicate must wait for the first execution');
  await bg.handleMessage(socket, command('second'));
  assert.match(replies.find((r) => r.id === 'second').error, /still has/);
  finish(); await first;
});

test('window task query returns the scheduler snapshot for one window', async () => {
  const bg = await background();
  await assert.rejects(bg.requestWindowTasks(5), /未连接/);
  const snapshot = { window_id: 5, running: [{ state: 'running' }], pending: [{ state: 'queued' }] };
  const socket = {
    readyState: 1,
    send(raw) {
      const msg = JSON.parse(raw);
      assert.equal(msg.method, '_window_tasks');
      assert.equal(msg.params.window_id, 5);
      void bg.handleMessage(socket, JSON.stringify({ id: msg.id, success: true, result: snapshot }));
    },
  };
  bg.setSocket(socket);
  assert.equal(JSON.stringify(await bg.requestWindowTasks(5)), JSON.stringify(snapshot));
});
