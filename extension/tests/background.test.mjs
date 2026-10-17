import assert from 'node:assert/strict';
import test from 'node:test';
import { background } from './helpers/background.mjs';

test('existing agent window is used repeatedly, never the user active window', async () => {
  const bg = await background({ agentWindows: [['mcp-a', 5]] });
  for (let i = 0; i < 3; i++) assert.equal((await bg.resolveTab(undefined, 'mcp-a')).id, 10);
  assert.equal((await bg.resolveTab(undefined, 'cli')).id, 20);
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
