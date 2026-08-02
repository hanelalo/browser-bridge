import { defineBackground } from '#imports';

const SERVER_URL =
  (import.meta.env as Record<string, string | undefined>).WXT_PUBLIC_BRIDGE_URL ??
  'ws://127.0.0.1:9225';

let ws: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let pingTimer: ReturnType<typeof setInterval> | null = null;
let reconnectDelayMs = 500;
let status: 'connecting' | 'connected' | 'disconnected' = 'disconnected';

// bridge 自动打开的标签页（new_tab），用于 close_auto_tabs 一键清理。
// 存 chrome.storage.session，service worker 重启也不丢；浏览器重启后自然失效。
let autoTabs: Set<number> = new Set();

async function saveAutoTabs(): Promise<void> {
  try {
    await chrome.storage.session.set({ autoTabs: Array.from(autoTabs) });
  } catch {
    // session 存储不可用时退回仅内存记录
  }
}

void (async () => {
  try {
    const got = await chrome.storage.session.get('autoTabs');
    if (Array.isArray(got.autoTabs)) autoTabs = new Set(got.autoTabs as number[]);
  } catch {
    // ignore
  }
  chrome.tabs.onRemoved.addListener((id) => {
    if (autoTabs.delete(id)) void saveAutoTabs();
  });
})();

function setStatus(next: typeof status): void {
  status = next;
  // 通知 popup（没有打开时静默失败）
  chrome.runtime.sendMessage({ type: 'status_changed', status, url: SERVER_URL }).catch(() => {});
}

function connect(): void {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  setStatus('connecting');
  const socket = new WebSocket(SERVER_URL);
  ws = socket;

  socket.onopen = () => {
    reconnectDelayMs = 1000;
    socket.send(JSON.stringify({ type: 'hello', role: 'extension', name: 'browser-bridge' }));
    setStatus('connected');
    startPing(socket);
  };

  socket.onmessage = (event) => {
    void handleMessage(socket, String(event.data));
  };

  socket.onclose = () => {
    if (ws === socket) ws = null;
    stopPing();
    setStatus('disconnected');
    scheduleReconnect();
  };

  socket.onerror = () => socket.close();
}

function scheduleReconnect(): void {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, 5_000);
    connect();
  }, reconnectDelayMs);
}

function startPing(socket: WebSocket): void {
  stopPing();
  pingTimer = setInterval(() => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ id: `ping-${Date.now()}`, method: 'ping', params: {} }));
    }
  }, 15_000);
}

function stopPing(): void {
  if (pingTimer) {
    clearInterval(pingTimer);
    pingTimer = null;
  }
}

function send(socket: WebSocket, msg: unknown): void {
  if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

async function handleMessage(socket: WebSocket, raw: string): Promise<void> {
  let msg: { id?: unknown; method?: unknown; params?: Record<string, unknown> };
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  if (!msg || typeof msg !== 'object' || typeof msg.id !== 'string') return;
  // 没有 method 的是服务端响应（如 pong），忽略
  if (typeof msg.method !== 'string') return;

  if (msg.method === 'ping') {
    send(socket, { id: msg.id, success: true, result: { pong: true } });
    return;
  }

  try {
    const result = await execute(msg.method, msg.params ?? {});
    send(socket, { id: msg.id, success: true, result });
  } catch (err) {
    send(socket, {
      id: msg.id,
      success: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}

async function execute(method: string, params: Record<string, unknown>): Promise<unknown> {
  switch (method) {
    case 'list_tabs':
      return listTabs();
    case 'close_tab':
      return closeTab(params);
    case 'new_tab':
      return newTab(params);
    case 'activate_tab':
      return activateTab(params);
    case 'close_auto_tabs':
      return closeAutoTabs();
    case 'navigate':
      return navigate(params);
    case 'click':
      return runPageOp('click', normalizeTarget(params));
    case 'press_key':
      return pressKey(params);
    case 'run_script':
      return runScript(params);
    case 'click_at':
    case 'scroll':
    case 'set_value':
    case 'check':
    case 'select_option':
    case 'clear':
    case 'get_value':
    case 'scrape':
      return runPageOp(method, params);
    case 'get_page_content':
      return getPageContent(params);
    default:
      throw new Error(`unknown method: ${method}`);
  }
}

/** 兼容旧协议：只传 selector 时补成统一的 target 结构。 */
function normalizeTarget(params: Record<string, unknown>): Record<string, unknown> {
  if (params.target == null && typeof params.selector === 'string') {
    return { ...params, target: { by: 'css', value: params.selector } };
  }
  return params;
}

/** 在指定标签页里跑页面级操作（统一入口）。 */
async function runPageOp(op: string, params: Record<string, unknown>): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: pageOp,
    args: [op, params],
  });
  const r = (result?.result ?? {}) as { __bridge_error?: string };
  if (r.__bridge_error) throw new Error(r.__bridge_error);
  return { ...r, tab_id: tab.id };
}

/** 模拟按键；可选 wait_load：按键触发导航后等页面加载完成。 */
async function pressKey(params: Record<string, unknown>): Promise<unknown> {
  const result = await runPageOp('press_key', params);
  if (params.wait_load === true) {
    const tab = await resolveTab(params.tab_id as number | undefined);
    if (tab.id != null) await waitTabComplete(tab.id);
  }
  return result;
}

/** 轮询等待标签页加载完成（比监听 onUpdated 更抗竞态）。 */
async function waitTabComplete(tabId: number, timeoutMs = 30_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const tab = await chrome.tabs.get(tabId);
    if (tab.status === 'complete') return;
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error('tab load timeout');
}

/**
 * 注册在 USER_SCRIPT 世界的求值监听器：收到 bridge_eval 消息后执行任意 JS。
 * 该世界豁免页面 CSP（配合 configureWorld 允许 unsafe-eval），
 * 因此 new Function 不会被页面 CSP 拦截。
 */
const BRIDGE_EVAL_SCRIPT = `
(() => {
  const serialize = (v, seen = new Set()) => {
    if (v === null || v === undefined) return v ?? null;
    const t = typeof v;
    if (t === 'string' || t === 'boolean') return v;
    if (t === 'number') return Number.isFinite(v) ? v : String(v);
    if (t === 'bigint') return v.toString();
    if (v instanceof Element) {
      return {
        __element: v.tagName.toLowerCase(),
        id: v.id || null,
        text: (v.textContent ?? '').trim().slice(0, 300),
      };
    }
    if (Array.isArray(v)) {
      if (seen.has(v)) return '[Circular]';
      seen.add(v);
      return v.map((x) => serialize(x, seen));
    }
    if (t === 'object') {
      if (seen.has(v)) return '[Circular]';
      seen.add(v);
      const out = {};
      for (const [k, x] of Object.entries(v)) out[k] = serialize(x, seen);
      return out;
    }
    return String(v);
  };
  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || msg.type !== 'bridge_eval') return;
    try {
      const fn = new Function('"use strict"; return (' + msg.code + ');');
      const value = fn();
      if (value && typeof value.then === 'function') {
        value.then(
          (r) => sendResponse({ ok: true, result: serialize(r) }),
          (e) => sendResponse({ ok: false, error: e && e.message ? e.message : String(e) }),
        );
        return true;
      }
      sendResponse({ ok: true, result: serialize(value) });
    } catch (e) {
      sendResponse({ ok: false, error: e && e.message ? e.message : String(e) });
    }
  });
})();
`;

let userScriptSetup: Promise<void> | null = null;

/** 确保 USER_SCRIPT 世界已配置 unsafe-eval 且监听器已注册。 */
async function ensureEvalUserScript(): Promise<void> {
  if (!chrome.userScripts?.register) {
    throw new Error(
      'chrome.userScripts 不可用：Chrome 138+ 需要在扩展详情页开启「允许用户脚本」开关',
    );
  }
  if (!userScriptSetup) {
    userScriptSetup = (async () => {
      try {
        await chrome.userScripts.configureWorld({
          csp: "script-src 'self' 'unsafe-eval'",
          messaging: true,
        });
      } catch {
        // 低版本没有 configureWorld，沿用默认世界
      }
      const existing = await chrome.userScripts.getScripts();
      if (!existing.some((s) => s.id === 'bridge-eval')) {
        await chrome.userScripts.register([
          {
            id: 'bridge-eval',
            matches: ['<all_urls>'],
            runAt: 'document_start',
            js: [{ code: BRIDGE_EVAL_SCRIPT }],
          },
        ]);
      }
    })();
  }
  return userScriptSetup;
}

/**
 * 在页面里执行任意 JS（返回 JSON 序列化结果）。
 * 优先用 chrome.userScripts.execute（Chrome 135+，无需刷新页面）；
 * 低版本退回注册式 userScript + messaging（Chrome 120+，页面需在注册后加载）。
 */
async function runScript(params: Record<string, unknown>): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  const code = String(params.code ?? '');
  if (!code) throw new Error('run_script requires params.code');

  // 优先：userScripts.execute 按需注入（当前页面无需刷新）
  if (chrome.userScripts?.execute) {
    await ensureEvalUserScript();
    const injected = `
      (async () => {
        const __serialize = (v, seen = new Set()) => {
          if (v === null || v === undefined) return v ?? null;
          const t = typeof v;
          if (t === 'string' || t === 'boolean') return v;
          if (t === 'number') return Number.isFinite(v) ? v : String(v);
          if (t === 'bigint') return v.toString();
          if (v instanceof Element) {
            return {
              __element: v.tagName.toLowerCase(),
              id: v.id || null,
              text: (v.textContent ?? '').trim().slice(0, 300),
            };
          }
          if (Array.isArray(v)) {
            if (seen.has(v)) return '[Circular]';
            seen.add(v);
            return v.map((x) => __serialize(x, seen));
          }
          if (t === 'object') {
            if (seen.has(v)) return '[Circular]';
            seen.add(v);
            const out = {};
            for (const [k, x] of Object.entries(v)) out[k] = __serialize(x, seen);
            return out;
          }
          return String(v);
        };
        try {
          const __code = ${JSON.stringify(code)};
          const fn = new Function('"use strict"; return (' + __code + ');');
          const value = await fn();
          return { __ok: true, value: __serialize(value) };
        } catch (err) {
          return { __ok: false, error: err && err.message ? err.message : String(err) };
        }
      })()
    `;
    const [result] = await chrome.userScripts.execute({
      target: { tabId: tab.id },
      js: [{ code: injected }],
    });
    const r = (result?.result ?? {}) as { __ok?: boolean; value?: unknown; error?: string };
    if (r.__ok) {
      return { result: r.value, tab_id: tab.id };
    }
    throw new Error(r.error ?? 'run_script failed');
  }

  // 兼容：注册式 userScript + messaging
  await ensureEvalUserScript();
  try {
    const resp = (await chrome.tabs.sendMessage(tab.id, {
      type: 'bridge_eval',
      code,
    })) as { ok?: boolean; result?: unknown; error?: string } | undefined;
    if (resp?.ok) return { result: resp.result, tab_id: tab.id };
    throw new Error(resp?.error ?? 'run_script failed');
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    if (msg.includes('Receiving end does not exist')) {
      throw new Error('run_script: 当前页面在 user script 注册前已加载，请刷新该标签页后重试');
    }
    throw err;
  }
}

async function activeTab(): Promise<chrome.tabs.Tab> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) throw new Error('no active tab');
  return tab;
}

async function resolveTab(tabId?: number): Promise<chrome.tabs.Tab> {
  if (typeof tabId === 'number') return chrome.tabs.get(tabId);
  return activeTab();
}

function waitForComplete(tabId: number, timeoutMs = 30_000): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('tab load timeout'));
    }, timeoutMs);
    const onUpdated = (id: number, info: chrome.tabs.OnUpdatedInfo): void => {
      if (id === tabId && info.status === 'complete') {
        cleanup();
        resolve();
      }
    };
    function cleanup(): void {
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(onUpdated);
    }
    chrome.tabs.onUpdated.addListener(onUpdated);
  });
}

async function listTabs(): Promise<unknown> {
  const tabs = await chrome.tabs.query({});
  return tabs.map((t) => ({
    tab_id: t.id ?? null,
    url: t.url ?? null,
    title: t.title ?? null,
    active: t.active,
    window_id: t.windowId,
  }));
}

async function navigate(params: Record<string, unknown>): Promise<unknown> {
  const url = String(params.url ?? '');
  if (!url) throw new Error('navigate requires params.url');
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  const done = waitForComplete(tab.id);
  await chrome.tabs.update(tab.id, { url });
  await done;
  const updated = await chrome.tabs.get(tab.id);
  return { tab_id: updated.id, url: updated.url, title: updated.title };
}

async function getPageContent(params: Record<string, unknown>): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: () => ({
      title: document.title,
      url: location.href,
      text: document.body ? document.body.innerText : '',
    }),
  });
  return { ...(result?.result ?? {}), tab_id: tab.id };
}

/** 关闭标签页（默认当前激活标签页）。 */
async function closeTab(params: Record<string, unknown>): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  await chrome.tabs.remove(tab.id);
  return { closed: true, tab_id: tab.id };
}

/** 关闭 bridge 自动打开（new_tab）的全部标签页，不碰手动开的。 */
async function closeAutoTabs(): Promise<unknown> {
  const ids = Array.from(autoTabs);
  const existing = (
    await Promise.all(ids.map((id) => chrome.tabs.get(id).catch(() => null)))
  ).filter((t): t is chrome.tabs.Tab => t !== null);
  const closed = existing.map((t) => t.id ?? 0).filter((id) => id > 0);
  if (closed.length > 0) {
    await chrome.tabs.remove(closed);
  }
  autoTabs.clear();
  void saveAutoTabs();
  return { closed };
}

/** 新建标签页（可指定 URL）。 */
async function newTab(params: Record<string, unknown>): Promise<unknown> {
  const url = typeof params.url === 'string' && params.url ? params.url : undefined;
  const tab = await chrome.tabs.create({ url });
  if (tab.id != null) {
    autoTabs.add(tab.id);
    void saveAutoTabs();
  }
  return { tab_id: tab.id, url: tab.url ?? null, title: tab.title ?? null, active: tab.active };
}

/** 切换到指定标签页并聚焦所在窗口（默认当前激活标签页）。 */
async function activateTab(params: Record<string, unknown>): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  await chrome.tabs.update(tab.id, { active: true });
  if (tab.windowId != null) {
    await chrome.windows.update(tab.windowId, { focused: true });
  }
  const updated = await chrome.tabs.get(tab.id);
  return {
    tab_id: updated.id,
    url: updated.url ?? null,
    title: updated.title ?? null,
    active: updated.active,
  };
}

/**
 * 页面内执行器：注入到目标标签页运行。
 * 注意：chrome.scripting 只序列化本函数自身，所有辅助逻辑必须内联在这里。
 */
async function pageOp(op: string, params: Record<string, unknown>): Promise<unknown> {
  {
    const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));
    const isVisible = (el: Element): boolean =>
      el instanceof HTMLElement &&
      (el.offsetWidth > 0 || el.offsetHeight > 0 || el.getClientRects().length > 0);
    const depth = (el: Element): number => {
      let d = 0;
      let p = el.parentElement;
      while (p) {
        d += 1;
        p = p.parentElement;
      }
      return d;
    };
    const ownText = (el: Element): string =>
      Array.from(el.childNodes)
        .filter((n) => n.nodeType === Node.TEXT_NODE)
        .map((n) => n.textContent ?? '')
        .join('')
        .trim();
    const matchElement = (spec: { by?: string; value?: unknown; index?: unknown }): Element | null => {
      const by = typeof spec?.by === 'string' ? spec.by : 'css';
      const value = String(spec?.value ?? '');
      const index = typeof spec?.index === 'number' ? spec.index : 0;
      let els: Element[] = [];
      if (by === 'css') {
        els = Array.from(document.querySelectorAll(value));
      } else if (by === 'xpath') {
        const snap = document.evaluate(
          value,
          document,
          null,
          XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,
          null,
        );
        for (let i = 0; i < snap.snapshotLength; i += 1) {
          const n = snap.snapshotItem(i);
          if (n instanceof Element) els.push(n);
        }
      } else if (by === 'text') {
        const all = Array.from(document.querySelectorAll('body *')) as Element[];
        const exact = all.filter((el) => isVisible(el) && ownText(el) === value);
        // 没有精确匹配时退化为包含匹配，最深的元素优先（避免点到父容器）
        const pool =
          exact.length > 0
            ? exact
            : all.filter((el) => isVisible(el) && ownText(el).includes(value));
        els = pool.sort((a, b) => depth(b) - depth(a));
      }
      return els[index] ?? null;
    };
    const waitForElement = async (spec: unknown, timeoutMs: number): Promise<Element> => {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        const el = matchElement(spec as { by?: string; value?: unknown; index?: unknown });
        if (el) return el;
        await sleep(100);
      }
      throw new Error(`element not found: ${JSON.stringify(spec ?? {})}`);
    };
    const describe = (el: Element): Record<string, unknown> => {
      const htmlEl = el as HTMLElement;
      const inputEl = el as HTMLInputElement;
      return {
        tag: el.tagName.toLowerCase(),
        id: el.id || null,
        class: typeof htmlEl.className === 'string' ? htmlEl.className : null,
        type: el instanceof HTMLInputElement ? inputEl.type : null,
        name: typeof inputEl.name === 'string' ? inputEl.name : null,
        text: (el.textContent ?? '').trim().slice(0, 200),
        href: el instanceof HTMLAnchorElement ? el.href : null,
        visible: Boolean(htmlEl.offsetWidth || htmlEl.offsetHeight),
      };
    };
  const clickElement = (el: Element, newTab = false): Record<string, unknown> => {
    const info = describe(el);
    el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
    el.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, cancelable: true }));
    if (el instanceof HTMLAnchorElement) {
      // 锚点：只走 el.click() 的默认激活，避免合成 click 先触发页面 handler；
      // 默认覆盖 target=_blank 在当前标签页打开，防止流程开新 tab 堆积，new_tab=true 时保留原行为
      if (!newTab && el.target === '_blank') {
        el.target = '_self';
        el.click();
        el.target = '_blank';
      } else {
        el.click();
      }
    } else {
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
      if (el instanceof HTMLElement) el.click();
    }
    return info;
  };
    const setNativeValue = (el: HTMLInputElement | HTMLTextAreaElement, value: string): void => {
      const proto =
        el instanceof HTMLTextAreaElement
          ? HTMLTextAreaElement.prototype
          : HTMLInputElement.prototype;
      const descriptor = Object.getOwnPropertyDescriptor(proto, 'value');
      if (descriptor?.set) descriptor.set.call(el, value);
      else el.value = value;
    };
    const keyToCode = (key: string): string | undefined => {
      const map: Record<string, string> = {
        Enter: 'Enter',
        Escape: 'Escape',
        Tab: 'Tab',
        Backspace: 'Backspace',
        Delete: 'Delete',
        ArrowUp: 'ArrowUp',
        ArrowDown: 'ArrowDown',
        ArrowLeft: 'ArrowLeft',
        ArrowRight: 'ArrowRight',
        Home: 'Home',
        End: 'End',
        PageUp: 'PageUp',
        PageDown: 'PageDown',
        ' ': 'Space',
        Shift: 'ShiftLeft',
        Control: 'ControlLeft',
        Alt: 'AltLeft',
        Meta: 'MetaLeft',
      };
      if (map[key]) return map[key];
      if (/^[a-zA-Z]$/.test(key)) return `Key${key.toUpperCase()}`;
      if (/^[0-9]$/.test(key)) return `Digit${key}`;
      if (/^F\d{1,2}$/.test(key)) return key;
      return undefined;
    };
    const keyCodeOf = (key: string): number | undefined => {
      const map: Record<string, number> = {
        Enter: 13,
        Escape: 27,
        Tab: 9,
        Backspace: 8,
        Delete: 46,
        ArrowUp: 38,
        ArrowDown: 40,
        ArrowLeft: 37,
        ArrowRight: 39,
        Home: 36,
        End: 35,
        PageUp: 33,
        PageDown: 34,
        ' ': 32,
        Shift: 16,
        Control: 17,
        Alt: 18,
        Meta: 91,
      };
      if (map[key] !== undefined) return map[key];
      if (/^[a-zA-Z]$/.test(key)) return key.toUpperCase().charCodeAt(0);
      if (/^[0-9]$/.test(key)) return key.charCodeAt(0);
      if (/^F\d{1,2}$/.test(key)) return 111 + parseInt(key.slice(1), 10);
      return undefined;
    };
    const dispatchKey = (el: Element, key: string, modifiers: string[]): void => {
      const init: KeyboardEventInit & { keyCode?: number; which?: number } = {
        key,
        bubbles: true,
        cancelable: true,
        altKey: modifiers.includes('alt'),
        ctrlKey: modifiers.includes('ctrl') || modifiers.includes('control'),
        metaKey: modifiers.includes('meta'),
        shiftKey: modifiers.includes('shift'),
      };
      const code = keyToCode(key);
      if (code) init.code = code;
      const keyCode = keyCodeOf(key);
      if (keyCode !== undefined) {
        init.keyCode = keyCode;
        init.which = keyCode;
      }
      el.dispatchEvent(new KeyboardEvent('keydown', init));
      // keypress 已废弃，但部分老站点仍监听，仅对单字符和 Enter 派发
      if (key.length === 1 || key === 'Enter') {
        el.dispatchEvent(new KeyboardEvent('keypress', init));
      }
      el.dispatchEvent(new KeyboardEvent('keyup', init));
    };

    try {
      const timeoutMs = typeof params.timeout === 'number' ? params.timeout : 5000;

      if (op === 'click_at') {
        const x = Number(params.x);
        const y = Number(params.y);
        if (!Number.isFinite(x) || !Number.isFinite(y)) {
          throw new Error('click_at requires numeric params.x / params.y');
        }
        const el = document.elementFromPoint(x, y);
        if (!el) throw new Error(`no element at (${x}, ${y})`);
        return { clicked: clickElement(el, params.new_tab === true), x, y };
      }

      if (op === 'scroll') {
        const dx = Number(params.dx ?? 0);
        const dy = Number(params.dy ?? 0);
        const behavior: ScrollBehavior = params.smooth ? 'smooth' : 'auto';
        if (params.target) {
          const el = await waitForElement(params.target, timeoutMs);
          el.scrollBy({ left: dx, top: dy, behavior });
          return { scrolled: { dx, dy }, element: describe(el) };
        }
        window.scrollBy({ left: dx, top: dy, behavior });
        return { scrolled: { dx, dy } };
      }

      if (op === 'press_key') {
        const key = String(params.key ?? '');
        if (!key) throw new Error('press_key requires params.key');
        const modifiers = Array.isArray(params.modifiers)
          ? params.modifiers.filter((m): m is string => typeof m === 'string')
          : [];
        let el: Element | null = null;
        if (params.target) el = await waitForElement(params.target, timeoutMs);
        const target =
          el ?? (document.activeElement instanceof Element ? document.activeElement : document.body);
        if (el && target instanceof HTMLElement) target.focus();
        dispatchKey(target, key, modifiers);
        return { key, modifiers, element: describe(target) };
      }

      if (op === 'scrape') {
        const itemSel = String(params.item ?? '');
        if (!itemSel) throw new Error('scrape requires params.item');
        type FieldSpec = { key: string; selector: string; attr: string | null };
        const parseSpec = (raw: string): { selector: string; attr: string | null } => {
          const at = raw.indexOf('@');
          if (at > 0) {
            return { selector: raw.slice(0, at), attr: raw.slice(at + 1) || null };
          }
          return { selector: raw, attr: null };
        };
        let fields: FieldSpec[] = [];
        const rawFields = params.fields;
        if (rawFields && typeof rawFields === 'object' && !Array.isArray(rawFields)) {
          // 任意字段映射：{ "name": ".name", "img": "img@src" }
          for (const [key, spec] of Object.entries(rawFields as Record<string, unknown>)) {
            const s = String(spec ?? '');
            if (!key || !s) continue;
            fields.push({ key, ...parseSpec(s) });
          }
        } else {
          // 兼容旧的三字段写法：title/link/desc
          const legacy: Array<[string, string | null, string | null]> = [
            [
              'title',
              typeof params.title === 'string' && params.title ? String(params.title) : null,
              null,
            ],
            [
              'url',
              typeof params.link === 'string' && params.link ? String(params.link) : null,
              'href',
            ],
            [
              'description',
              typeof params.desc === 'string' && params.desc ? String(params.desc) : null,
              null,
            ],
          ];
          for (const [key, selector, attr] of legacy) {
            if (selector) fields.push({ key, selector, attr });
          }
        }
        if (fields.length === 0) {
          throw new Error('scrape requires params.fields 或 title/link/desc 至少一个字段');
        }
        const deadline = Date.now() + timeoutMs;
        let items: Element[] = [];
        while (Date.now() < deadline) {
          items = Array.from(document.querySelectorAll(itemSel));
          if (items.length > 0) break;
          await sleep(100);
        }
        const out = items.map((node) => {
          const row: Record<string, unknown> = {};
          for (const f of fields) {
            const el = node.querySelector(f.selector);
            if (!el) {
              row[f.key] = null;
              continue;
            }
            if (f.attr) {
              row[f.key] =
                el instanceof HTMLAnchorElement && f.attr === 'href'
                  ? el.href
                  : el.getAttribute(f.attr);
            } else {
              row[f.key] = (el.textContent ?? '').trim();
            }
          }
          return row;
        });
        return { count: out.length, items: out };
      }

      if (params.target == null) throw new Error(`${op} requires params.target`);
      const el = await waitForElement(params.target, timeoutMs);

      switch (op) {
        case 'click':
          return { clicked: clickElement(el, params.new_tab === true) };
        case 'set_value': {
          const value = String(params.value ?? '');
          if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
            setNativeValue(el, value);
          } else if (el instanceof HTMLElement && el.isContentEditable) {
            el.textContent = value;
          } else {
            throw new Error(`element is not input/textarea/contenteditable: ${describe(el).tag}`);
          }
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
          const current =
            el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement
              ? el.value
              : el.textContent ?? '';
          return { element: describe(el), value: current };
        }
        case 'check': {
          if (!(el instanceof HTMLInputElement) || (el.type !== 'checkbox' && el.type !== 'radio')) {
            throw new Error('check requires a checkbox or radio input');
          }
          const checked = params.checked !== false;
          // 模拟浏览器行为：选中 radio 时取消同组其他项
          if (el.type === 'radio' && checked && typeof el.name === 'string' && el.name) {
            document
              .querySelectorAll(`input[type=radio][name="${CSS.escape(el.name)}"]`)
              .forEach((r) => {
                (r as HTMLInputElement).checked = false;
              });
          }
          el.checked = checked;
          el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
          return { element: describe(el), checked: el.checked };
        }
        case 'select_option': {
          if (!(el instanceof HTMLSelectElement)) {
            throw new Error('select_option requires a <select> element');
          }
          let opt: HTMLOptionElement | null = null;
          if (typeof params.index === 'number') {
            opt = el.options[params.index] ?? null;
          } else if (params.value !== undefined) {
            opt = Array.from(el.options).find((o) => o.value === String(params.value)) ?? null;
          } else if (params.text !== undefined) {
            opt = Array.from(el.options).find((o) => o.text.trim() === String(params.text)) ?? null;
          }
          if (!opt) throw new Error('option not found in <select>');
          el.value = opt.value;
          el.dispatchEvent(new Event('change', { bubbles: true }));
          return { element: describe(el), value: el.value, text: opt.text.trim() };
        }
        case 'clear': {
          if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
            setNativeValue(el, '');
          } else if (el instanceof HTMLElement && el.isContentEditable) {
            el.textContent = '';
          } else {
            throw new Error(`element is not input/textarea/contenteditable: ${describe(el).tag}`);
          }
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
          return { element: describe(el) };
        }
        case 'get_value': {
          if (el instanceof HTMLInputElement) {
            return { element: describe(el), value: el.value, checked: el.checked };
          }
          if (el instanceof HTMLTextAreaElement) {
            return { element: describe(el), value: el.value };
          }
          if (el instanceof HTMLSelectElement) {
            return {
              element: describe(el),
              value: el.value,
              text: el.options[el.selectedIndex]?.text.trim() ?? null,
            };
          }
          return { element: describe(el), value: el.textContent ?? '' };
        }
        default:
          throw new Error(`unknown method: ${op}`);
      }
    } catch (err) {
      // 把页面内错误转成干净的字符串，避免被 executeScript 的包装错误吞掉细节
      return { __bridge_error: err instanceof Error ? err.message : String(err) };
    }
  }
}

chrome.runtime.onMessage.addListener(
  (msg: unknown, _sender: chrome.runtime.MessageSender, sendResponse: (resp: unknown) => void) => {
    const m = msg as { type?: string } | undefined;
    if (m?.type === 'get_status') {
      sendResponse({ status, url: SERVER_URL });
    } else if (m?.type === 'reconnect') {
      connect();
      sendResponse({ status });
    } else if (m?.type === 'tab_count') {
      chrome.tabs
        .query({})
        .then((tabs) => sendResponse({ count: tabs.length }))
        .catch(() => sendResponse({ count: null }));
      return true; // 异步响应
    }
  },
);

export default defineBackground(() => {
  connect();
});
