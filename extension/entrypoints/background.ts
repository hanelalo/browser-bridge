import { defineBackground } from '#imports';

const SERVER_URL =
  (import.meta.env as Record<string, string | undefined>).WXT_PUBLIC_BRIDGE_URL ??
  'ws://127.0.0.1:9225';

let ws: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let pingTimer: ReturnType<typeof setInterval> | null = null;
let reconnectDelayMs = 500;
let status: 'connecting' | 'connected' | 'disconnected' = 'disconnected';

// bridge 自动打开的标签页（new_tab / click --new-tab），tabId -> 创建者 client_id（'' 表示未知/手动场景）。
// close_auto_tabs 支持按 owner 隔离：多 agent 场景下各自只能清理自己创建的标签页。
// 存 chrome.storage.session，service worker 重启也不丢；浏览器重启后自然失效。
let autoTabs: Map<number, string> = new Map();

// click 请求 new_tab 时的捕获窗口：预登记源标签页，onCreated 按 openerTabId 精确关联
let newTabClickUntil = 0;
let newTabClickSourceTab: number | null = null;
let newTabClickOwner = '';

async function saveAutoTabs(): Promise<void> {
  try {
    await chrome.storage.session.set({ autoTabs: Array.from(autoTabs.entries()) });
  } catch {
    // session 存储不可用时退回仅内存记录
  }
}

void (async () => {
  try {
    const got = await chrome.storage.session.get('autoTabs');
    if (Array.isArray(got.autoTabs)) {
      autoTabs = new Map(
        (got.autoTabs as Array<[number, string]>).map(([id, owner]) => [Number(id), String(owner ?? '')]),
      );
    }
  } catch {
    // ignore
  }
  chrome.tabs.onCreated.addListener((tab) => {
    if (
      Date.now() <= newTabClickUntil &&
      tab.openerTabId != null &&
      tab.openerTabId === newTabClickSourceTab &&
      tab.id != null
    ) {
      autoTabs.set(tab.id, newTabClickOwner);
      void saveAutoTabs();
    }
  });
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
    void closeOffscreen();
  };

  socket.onmessage = (event) => {
    void handleMessage(socket, String(event.data));
  };

  socket.onclose = () => {
    if (ws === socket) ws = null;
    stopPing();
    setStatus('disconnected');
    scheduleReconnect();
    void ensureOffscreen();
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

/** 断开期间创建 offscreen 唤醒源：它每 5 秒发消息把休眠的 worker 叫醒重连。
 *  已连接/连接中不需要它，直接跳过。 */
async function ensureOffscreen(): Promise<void> {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  try {
    if (await chrome.offscreen.hasDocument()) return;
  } catch {
    // 旧版 Chrome 没有 hasDocument：直接尝试创建，失败即视为已存在
  }
  try {
    await chrome.offscreen.createDocument({
      url: 'offscreen.html',
      reasons: ['DOM_SCRAPING'],
      justification: '断开期间每 5 秒唤醒 service worker 重连 bridge server',
    });
  } catch {
    // 已存在或创建失败：下一次 onclose 会再尝试
  }
}

/** 连上后关闭 offscreen 唤醒源，平时零开销。 */
async function closeOffscreen(): Promise<void> {
  try {
    await chrome.offscreen.closeDocument();
  } catch {
    // 没有文档时忽略
  }
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
  let msg: {
    id?: unknown;
    method?: unknown;
    params?: Record<string, unknown>;
    client_id?: unknown;
  };
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
    const clientId = typeof msg.client_id === 'string' ? msg.client_id : '';
    const result = await execute(msg.method, msg.params ?? {}, clientId);
    send(socket, { id: msg.id, success: true, result });
  } catch (err) {
    send(socket, {
      id: msg.id,
      success: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}

async function execute(
  method: string,
  params: Record<string, unknown>,
  clientId: string,
): Promise<unknown> {
  switch (method) {
    case 'list_tabs':
      return listTabs();
    case 'close_tab':
      return closeTab(params);
    case 'new_tab':
      return newTab(params, clientId);
    case 'activate_tab':
      return activateTab(params);
    case 'close_auto_tabs':
      return closeAutoTabs(params);
    case 'navigate':
      return navigate(params);
    case 'click':
      return runPageOp('click', normalizeTarget(params), clientId);
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
      return runPageOp(method, params, clientId);
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
async function runPageOp(
  op: string,
  params: Record<string, unknown>,
  clientId: string,
): Promise<unknown> {
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  // 预登记：本次点击要求新开标签页时，开 3 秒窗口等 onCreated 按 openerTabId 捕获
  if ((op === 'click' || op === 'click_at') && params.new_tab === true) {
    newTabClickUntil = Date.now() + 3000;
    newTabClickSourceTab = tab.id;
    newTabClickOwner = clientId;
  }
  try {
    const [result] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: pageOp,
      args: [op, params],
    });
    const r = (result?.result ?? {}) as { __bridge_error?: string };
    if (r.__bridge_error) throw new Error(r.__bridge_error);
    // new_tab 锚点：由扩展创建标签页，精确记录并返回新 tab id，便于后续链式操作
    if (typeof r.open_url === 'string' && r.open_url) {
      const nt = await chrome.tabs.create({ url: r.open_url });
      if (nt.id != null) {
        autoTabs.set(nt.id, clientId);
        void saveAutoTabs();
      }
      const { open_url: _openUrl, ...rest } = r;
      return { ...rest, tab_id: nt.id };
    }
    return { ...r, tab_id: tab.id };
  } catch (err) {
    // 操作失败则作废捕获窗口，避免误记
    newTabClickUntil = 0;
    throw err;
  }
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

/** 关闭 bridge 自动打开的标签页；params.owner 指定时只关该创建者的（多 agent 隔离）。 */
async function closeAutoTabs(params: Record<string, unknown>): Promise<unknown> {
  const owner = typeof params?.owner === 'string' && params.owner ? params.owner : null;
  const entries = Array.from(autoTabs.entries()).filter(([, o]) => owner === null || o === owner);
  const ids = entries.map(([id]) => id);
  const existing = (
    await Promise.all(ids.map((id) => chrome.tabs.get(id).catch(() => null)))
  ).filter((t): t is chrome.tabs.Tab => t !== null);
  const closed = existing.map((t) => t.id ?? 0).filter((id) => id > 0);
  if (closed.length > 0) {
    await chrome.tabs.remove(closed);
  }
  entries.forEach(([id]) => {
    autoTabs.delete(id);
  });
  void saveAutoTabs();
  return { closed };
}

/** 新建标签页（可指定 URL）。 */
async function newTab(params: Record<string, unknown>, clientId: string): Promise<unknown> {
  const url = typeof params.url === 'string' && params.url ? params.url : undefined;
  const tab = await chrome.tabs.create({ url });
  if (tab.id != null) {
    autoTabs.set(tab.id, clientId);
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
  const clickElement = (
    el: Element,
    newTab = false,
  ): { info: Record<string, unknown>; open_url?: string } => {
    const info = describe(el);
    el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
    el.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, cancelable: true }));
    if (el instanceof HTMLAnchorElement) {
      if (newTab) {
        // new_tab：不派发 click，href 交给 background 用 tabs.create 打开，
        // 这样新标签页 id 精确可知，可直接记入 autoTabs 并返回给调用方
        return { info, open_url: el.href };
      }
      // 默认：覆盖 target=_blank 在当前标签页打开，防止流程开新 tab 堆积
      if (el.target === '_blank') {
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
    return { info };
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
        const { info, open_url } = clickElement(el, params.new_tab === true);
        return open_url ? { clicked: info, x, y, open_url } : { clicked: info, x, y };
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
        case 'click': {
          const { info, open_url } = clickElement(el, params.new_tab === true);
          return open_url ? { clicked: info, open_url } : { clicked: info };
        }
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
    } else if (m?.type === 'wake') {
      // offscreen 唤醒源：worker 可能刚被拉起，直接尝试重连
      connect();
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
  // worker 每次启动都确保唤醒源存在（若已连上，onopen 会立刻关掉它）
  void ensureOffscreen();
});
