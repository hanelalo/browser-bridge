<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';

type ConnStatus = 'connecting' | 'connected' | 'disconnected';

const status = ref<ConnStatus>('disconnected');
const url = ref('ws://127.0.0.1:9225');
const tabCount = ref<number | null>(null);
let timer: ReturnType<typeof setInterval> | null = null;

const statusText = computed(() =>
  status.value === 'connected' ? '已连接' : status.value === 'connecting' ? '连接中' : '未连接',
);

async function refresh(): Promise<void> {
  try {
    const resp = (await chrome.runtime.sendMessage({ type: 'get_status' })) as
      | { status?: ConnStatus; url?: string }
      | undefined;
    if (resp?.status) status.value = resp.status;
    if (resp?.url) url.value = resp.url;
  } catch {
    // popup 关闭或消息通道断开，忽略
  }
}

async function reconnect(): Promise<void> {
  try {
    await chrome.runtime.sendMessage({ type: 'reconnect' });
    await refresh();
  } catch {
    // ignore
  }
}

async function countTabs(): Promise<void> {
  try {
    const resp = (await chrome.runtime.sendMessage({ type: 'tab_count' })) as
      | { count?: number | null }
      | undefined;
    tabCount.value = resp?.count ?? null;
  } catch {
    tabCount.value = null;
  }
}

chrome.runtime.onMessage.addListener((msg: unknown) => {
  const m = msg as { type?: string; status?: ConnStatus; url?: string };
  if (m.type === 'status_changed') {
    if (m.status) status.value = m.status;
    if (m.url) url.value = m.url;
  }
});

onMounted(() => {
  void refresh();
  timer = setInterval(() => void refresh(), 2000);
});

onUnmounted(() => {
  if (timer) clearInterval(timer);
});
</script>

<template>
  <div class="app">
    <h1>Browser Bridge</h1>
    <p class="url">{{ url }}</p>
    <p :class="['status', status]">{{ statusText }}</p>
    <div class="actions">
      <button @click="reconnect">重新连接</button>
      <button @click="countTabs">{{ tabCount === null ? '统计标签页' : `标签页：${tabCount}` }}</button>
    </div>
  </div>
</template>

<style>
body {
  margin: 0;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
}
.app {
  width: 260px;
  padding: 12px 14px;
}
h1 {
  margin: 0 0 4px;
  font-size: 15px;
}
.url {
  margin: 0 0 6px;
  color: #666;
  font-size: 12px;
  word-break: break-all;
}
.status {
  margin: 0 0 10px;
  font-size: 13px;
  font-weight: 600;
}
.status.connected {
  color: #16a34a;
}
.status.connecting {
  color: #d97706;
}
.status.disconnected {
  color: #dc2626;
}
.actions {
  display: flex;
  gap: 8px;
}
button {
  padding: 4px 10px;
  font-size: 12px;
  border: 1px solid #ccc;
  border-radius: 6px;
  background: #fff;
  cursor: pointer;
}
button:hover {
  background: #f3f4f6;
}
</style>
