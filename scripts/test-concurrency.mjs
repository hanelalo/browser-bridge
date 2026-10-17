// Full protocol regression: real server + real MCP + actual background source
// with in-memory Chrome API doubles. Never touches the user's browser.
// Run after: cargo build --workspace
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import assert from 'node:assert/strict';
import { background } from '../extension/tests/helpers/background.mjs';

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(predicate, label) {
  const end = Date.now() + 5000;
  while (!predicate()) { if (Date.now() >= end) throw new Error(`timed out: ${label}`); await delay(5); }
}
const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening');
const port = probe.address().port; await new Promise((resolve) => probe.close(resolve));
const url = `ws://127.0.0.1:${port}`;
const children = []; const sockets = []; let logs = '';
function child(binary, env) {
  const p = spawn(new URL(`../target/debug/${binary}`, import.meta.url).pathname, [], { env: { ...process.env, ...env }, stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(p); p.stderr.on('data', (s) => { logs += s; }); return p;
}
async function socket() {
  const ws = new WebSocket(url); sockets.push(ws);
  await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
  return ws;
}
let seq = 0;
function rpc(send, onMessage) {
  const pending = new Map();
  onMessage((message) => { const waiter = pending.get(message.id); if (waiter) { pending.delete(message.id); clearTimeout(waiter.timer); waiter.resolve(message); } });
  return (method, params) => new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`RPC timed out: ${method}`)); }, 5000);
    timer.unref();
    pending.set(id, { resolve, timer }); send({ jsonrpc: '2.0', id, method, params });
  });
}
async function rawClient() {
  const ws = await socket(); ws.send(JSON.stringify({ type: 'hello', role: 'client', client_id: 'mcp-probe-b' }));
  const pending = new Map();
  ws.addEventListener('message', (e) => { const r = JSON.parse(e.data); const done = pending.get(r.id); if (done) { pending.delete(r.id); done(r); } });
  return (method, params, timeout_ms = 4000) => new Promise((resolve) => {
    const id = `raw-${++seq}`; pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params, timeout_ms, deadline_ms: Date.now() + timeout_ms }));
  });
}
const bg = await background();
for (const window of [11, 12]) { bg.windows.set(window, { id: window }); bg.tabs.set(window * 10, { id: window * 10, windowId: window, active: true }); }
const calls = [];
bg.chrome.scripting.executeScript = ({ target }) => new Promise((resolve) => {
  const tab = bg.tabs.get(target.tabId);
  const call = { window: tab.windowId, done: false, finish: () => { call.done = true; resolve([{ result: { title: `window-${tab.windowId}` } }]); } };
  calls.push(call);
  assert.ok(calls.filter((c) => !c.done).length <= 3, 'actual executions exceed global limit');
  assert.equal(calls.filter((c) => !c.done && c.window === call.window).length, 1, 'same window executed concurrently');
});
async function browser() {
  const ws = await socket(); bg.setSocket(ws);
  await new Promise((resolve, reject) => {
    ws.addEventListener('message', async (event) => {
      try {
        const message = JSON.parse(event.data);
        await bg.handleMessage(ws, event.data);
        if (message.method === '_sync_executions') resolve();
      } catch (error) { reject(error); }
    });
    ws.send(JSON.stringify({ type: 'hello', role: 'extension', client_id: bg.data.bridgeInstanceId }));
  });
  await delay(15); return ws;
}
function content(response) {
  assert.ok(!response.error, JSON.stringify(response));
  assert.ok(!response.result.isError, JSON.stringify(response));
  return JSON.parse(response.result.content[0].text);
}
try {
  const server = child('bridge-server', { BRIDGE_PORT: `${port}`, BRIDGE_IDLE_TIMEOUT: '0' });
  await until(() => logs.includes('listening'), 'server startup');
  let extension = await browser();
  const mcp = child('bridge-mcp', { BRIDGE_SERVER: url });
  const lines = createInterface({ input: mcp.stdout });
  const request = rpc((r) => mcp.stdin.write(`${JSON.stringify(r)}\n`), (fn) => lines.on('line', (line) => fn(JSON.parse(line))));
  const init = await request('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'regression', version: '1' } });
  assert.ok(init.result);
  mcp.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`);
  const tools = await request('tools/list', {});
  for (const name of ['begin_task', 'end_task', 'cancel_task', 'task_status']) assert.ok(tools.result.tools.some((t) => t.name === name));
  assert.ok(tools.result.tools.find((t) => t.name === 'click').inputSchema.properties.task_id);
  const tool = (name, args) => request('tools/call', { name, arguments: args });

  // The default-window regression must be tested after the first creation too.
  const firstDefault = tool('get_page_content', {});
  await until(() => calls.length === 1, 'default window setup');
  const dedicatedWindow = calls[0].window;
  assert.notEqual(dedicatedWindow, 9, 'MCP used the user active window');
  calls[0].finish(); content(await firstDefault);
  const secondDefault = tool('get_page_content', {});
  await until(() => calls.length === 2, 'default window reuse');
  assert.equal(calls[1].window, dedicatedWindow);
  calls[1].finish(); content(await secondDefault);
  console.log('PASS: default agent window is created under admission and reused');

  // Four simultaneous tools in one MCP connection, at most three running.
  const start = calls.length;
  const batch = [5, 9, 11, 12].map((window_id) => tool('get_page_content', { window_id }));
  await until(() => calls.length === start + 3, 'three parallel tools'); await delay(40);
  assert.equal(calls.length, start + 3);
  calls[start].finish(); await until(() => calls.length === start + 4, 'fourth tool admission');
  for (const call of calls.slice(start + 1)) call.finish();
  (await Promise.all(batch)).forEach(content);
  console.log('PASS: one MCP connection, global maximum 3');

  // Several commands using one explicit lease still execute one at a time.
  const lease = content(await tool('begin_task', { window_id: 5, agent_id: 'agent-a' }));
  const scopedStart = calls.length;
  const scoped = [0, 1, 2, 3].map(() => tool('get_page_content', { task_id: lease.task_id }));
  for (let i = 0; i < 4; i++) {
    await until(() => calls.length === scopedStart + i + 1, 'serialized lease step');
    await delay(15); assert.equal(calls.length, scopedStart + i + 1); calls[scopedStart + i].finish();
  }
  (await Promise.all(scoped)).forEach(content);
  content(await tool('end_task', { task_id: lease.task_id }));
  console.log('PASS: same-task commands serialized, task context pins window');

  const raw = await rawClient();
  const timeoutStart = calls.length;
  const slow = raw('get_page_content', { tab_id: 10 }, 100);
  await until(() => calls.length === timeoutStart + 1, 'slow command begins');
  assert.equal((await slow).success, false);
  const next = raw('get_page_content', { window_id: 5 });
  await delay(40); assert.equal(calls.length, timeoutStart + 1);
  calls[timeoutStart].finish(); await until(() => calls.length === timeoutStart + 2, 'drain release');
  calls[timeoutStart + 1].finish(); assert.equal((await next).success, true);
  console.log('PASS: timed-out browser operation retains lock until completion');

  const queuedStart = calls.length;
  const blocker = raw('get_page_content', { window_id: 5 });
  await until(() => calls.length === queuedStart + 1, 'queue blocker');
  assert.equal((await raw('get_page_content', { window_id: 5, task_id: 'expires-in-queue' }, 80)).success, false);
  calls[queuedStart].finish(); await blocker; await delay(50);
  assert.equal(calls.length, queuedStart + 1);
  console.log('PASS: expired queued task never executes');

  const reconnectStart = calls.length;
  const old = raw('get_page_content', { window_id: 5 });
  await until(() => calls.length === reconnectStart + 1, 'operation before extension disconnect');
  const closed = once(extension, 'close'); extension.close(); await closed;
  assert.equal((await old).success, false);
  extension = await browser();
  const afterReconnect = raw('get_page_content', { window_id: 5 });
  await delay(40); assert.equal(calls.length, reconnectStart + 1);
  calls[reconnectStart].finish(); await until(() => calls.length === reconnectStart + 2, 'reconciled operation completion');
  calls[reconnectStart + 1].finish(); assert.equal((await afterReconnect).success, true);
  console.log('PASS: extension reconnect preserves and reconciles in-flight locks');

  const offlineStart = calls.length;
  const offline = raw('get_page_content', { window_id: 5 });
  await until(() => calls.length === offlineStart + 1, 'operation before offline completion');
  const offlineClosed = once(extension, 'close'); extension.close(); await offlineClosed;
  assert.equal((await offline).success, false);
  calls[offlineStart].finish();
  await until(() => bg.data.bridgeExecutions.length === 0, 'offline operation removes its journal entry');
  extension = await browser();
  const retry = raw('get_page_content', { window_id: 5 });
  await until(() => calls.length === offlineStart + 2, 'fresh retry after offline completion');
  calls[offlineStart + 1].finish(); assert.equal((await retry).success, true);
  console.log('PASS: completion while disconnected retains no result and allows a fresh retry');

  const disconnectStart = calls.length;
  const beforeExit = tool('get_page_content', { window_id: 5 });
  void beforeExit.catch(() => {});
  await until(() => calls.length === disconnectStart + 1, 'operation before MCP exit');
  const exited = once(mcp, 'exit'); mcp.kill('SIGTERM'); await exited;
  const afterExit = raw('get_page_content', { window_id: 5 });
  await delay(40); assert.equal(calls.length, disconnectStart + 1);
  calls[disconnectStart].finish(); await until(() => calls.length === disconnectStart + 2, 'client drain release');
  calls[disconnectStart + 1].finish(); assert.equal((await afterExit).success, true);
  console.log('PASS: MCP exit drains browser operation without releasing early');
  assert.equal(server.exitCode, null);
} catch (error) {
  console.error(logs); throw error;
} finally {
  for (const ws of sockets) ws.close();
  for (const p of children) if (p.exitCode == null) p.kill('SIGTERM');
}
