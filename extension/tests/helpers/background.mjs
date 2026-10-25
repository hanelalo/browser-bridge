import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import vm from 'node:vm';
import { webcrypto } from 'node:crypto';
// Run the actual background source with Chrome API doubles. This exercises the
// default route and journal, rather than reimplementing either in the tests.
export async function background(saved = {}, { quotaBytes = Infinity } = {}) {
  const tabs = new Map([[10, { id: 10, windowId: 5, active: true }], [20, { id: 20, windowId: 9, active: true }]]);
  const windows = new Map([[5, { id: 5 }], [9, { id: 9 }]]);
  const data = { ...saved };
  const storageWrites = [];
  const events = {};
  let nextId = 100;
  let creations = 0;
  const event = (name) => ({ addListener: (fn) => { events[name] = fn; } });
  const chrome = {
    storage: { session: {
      get: async (keys) => Object.fromEntries((Array.isArray(keys) ? keys : [keys]).map((key) => [key, data[key]])),
      set: async (value) => {
        const size = Buffer.byteLength(JSON.stringify({ ...data, ...value }));
        if (size > quotaBytes) throw new Error('Session storage quota bytes exceeded. Values were not stored.');
        storageWrites.push(size);
        Object.assign(data, structuredClone(value));
      },
    } },
    runtime: { onMessage: event('message'), sendMessage: async () => {} },
    tabs: {
      onCreated: event('created'), onRemoved: event('tabRemoved'),
      get: async (id) => { if (!tabs.has(id)) throw new Error('no tab'); return tabs.get(id); },
      query: async (q) => Array.from(tabs.values()).filter((t) => q.currentWindow ? t.id === 20 : q.windowId == null || t.windowId === q.windowId),
      create: async (p) => { const t = { id: nextId++, windowId: p.windowId, active: true }; tabs.set(t.id, t); return t; },
      remove: async (ids) => { for (const id of Array.isArray(ids) ? ids : [ids]) tabs.delete(id); },
    },
    windows: {
      onRemoved: event('windowRemoved'),
      get: async (id) => { if (!windows.has(id)) throw new Error('no window'); return windows.get(id); },
      getAll: async () => Array.from(windows.values()),
      create: async () => {
        creations++;
        await Promise.resolve();
        const win = { id: nextId++ }; windows.set(win.id, win);
        const tab = { id: nextId++, windowId: win.id, active: true }; tabs.set(tab.id, tab);
        return win;
      },
    },
    scripting: { executeScript: async () => [{ result: { title: 'agent' } }] },
  };
  const source = readFileSync(new URL('../../entrypoints/background.ts', import.meta.url), 'utf8')
    .replace(/^import .*#imports.*;\n/m, '')
    .replaceAll('import.meta.env', '({})')
    .replace('export default defineBackground', 'defineBackground');
  const context = vm.createContext({ chrome, crypto: webcrypto, console, setTimeout, clearTimeout, setInterval, clearInterval,
    WebSocket: { OPEN: 1 }, defineBackground: () => {} });
  vm.runInContext(stripTypeScriptTypes(source) + '\n globalThis.api = { init: () => (stateReady = restoreState()), resolveTab, resolveTarget, validateExecutionScope, handleMessage, syncExecutions, ensureAgentWindow, requestWindowTasks, setSocket: (socket) => { ws = socket; } };', context);
  await context.api.init();
  return { ...context.api, chrome, data, storageWrites, tabs, windows, events, creations: () => creations };
}
