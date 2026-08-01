import { defineBackground } from '#imports';

const SERVER_URL =
  (import.meta.env as Record<string, string | undefined>).WXT_PUBLIC_BRIDGE_URL ??
  'ws://127.0.0.1:9225';

let ws: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let pingTimer: ReturnType<typeof setInterval> | null = null;
let reconnectDelayMs = 1000;
let status: 'connecting' | 'connected' | 'disconnected' = 'disconnected';

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
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, 30_000);
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
    case 'navigate':
      return navigate(params);
    case 'click':
      return click(params);
    case 'get_page_content':
      return getPageContent(params);
    default:
      throw new Error(`unknown method: ${method}`);
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

async function click(params: Record<string, unknown>): Promise<unknown> {
  const selector = String(params.selector ?? '');
  if (!selector) throw new Error('click requires params.selector');
  const tab = await resolveTab(params.tab_id as number | undefined);
  if (tab.id == null) throw new Error('tab has no id');
  const timeoutMs = typeof params.timeout === 'number' ? params.timeout : 5000;
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: async (sel: string, timeout: number) => {
      const deadline = Date.now() + timeout;
      let el: Element | null = null;
      while (Date.now() < deadline) {
        el = document.querySelector(sel);
        if (el) break;
        await new Promise((r) => setTimeout(r, 100));
      }
      if (!el) throw new Error(`element not found: ${sel}`);
      const htmlEl = el as HTMLElement;
      const info = {
        tag: el.tagName.toLowerCase(),
        id: el.id || null,
        class: typeof htmlEl.className === 'string' ? htmlEl.className : null,
        text: (el.textContent ?? '').trim().slice(0, 200),
        href: el instanceof HTMLAnchorElement ? el.href : null,
        visible: Boolean(htmlEl.offsetWidth || htmlEl.offsetHeight),
      };
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
      htmlEl.click();
      return info;
    },
    args: [selector, timeoutMs],
  });
  return { clicked: result?.result ?? null, tab_id: tab.id };
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
