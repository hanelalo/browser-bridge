<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';

type ConnStatus = 'connecting' | 'connected' | 'disconnected';
type TaskState = 'running' | 'closing' | 'queued';
type Step = { id: string; method: string; detail: string | null };
type TaskView = {
  key: string;
  task_id: string | null;
  agent_id: string | null;
  client_id: string | null;
  state: TaskState;
  window_ids: number[];
  tab_id: number | null;
  inflight: Step | null;
  queued: Step[];
};

const status = ref<ConnStatus>('disconnected');
const url = ref('');
const windowId = ref<number | null>(null);
const running = ref<TaskView[]>([]);
const pending = ref<TaskView[]>([]);
const error = ref('');
let timer: ReturnType<typeof setInterval> | null = null;
let inflightRefresh = false;

const statusText = computed(() =>
  status.value === 'connected' ? '已连接' : status.value === 'connecting' ? '连接中' : '未连接',
);
const subtitle = computed(() => {
  const windowLabel = windowId.value == null ? '当前窗口' : `窗口 ${windowId.value}`;
  return url.value ? `${windowLabel} · ${url.value}` : windowLabel;
});

function asStep(value: unknown): Step | null {
  if (!value || typeof value !== 'object') return null;
  const step = value as Partial<Step>;
  if (typeof step.method !== 'string' || !step.method) return null;
  return {
    id: typeof step.id === 'string' ? step.id : '',
    method: step.method,
    detail: typeof step.detail === 'string' && step.detail ? step.detail : null,
  };
}

function asTasks(value: unknown): TaskView[] {
  if (!Array.isArray(value)) return [];
  return value.map((item) => {
    const task = (item ?? {}) as Partial<TaskView>;
    const state: TaskState = task.state === 'closing' || task.state === 'queued' ? task.state : 'running';
    return {
      key: typeof task.key === 'string' ? task.key : '',
      task_id: typeof task.task_id === 'string' && task.task_id ? task.task_id : null,
      agent_id: typeof task.agent_id === 'string' && task.agent_id ? task.agent_id : null,
      client_id: typeof task.client_id === 'string' && task.client_id ? task.client_id : null,
      state,
      window_ids: Array.isArray(task.window_ids)
        ? task.window_ids.filter((id): id is number => typeof id === 'number')
        : [],
      tab_id: typeof task.tab_id === 'number' ? task.tab_id : null,
      inflight: asStep(task.inflight),
      queued: Array.isArray(task.queued) ? task.queued.flatMap((step) => {
        const parsed = asStep(step);
        return parsed ? [parsed] : [];
      }) : [],
    };
  });
}

function commandOf(task: TaskView): Step | null {
  return task.inflight ?? task.queued[0] ?? null;
}

function headline(task: TaskView): string {
  return commandOf(task)?.method || (task.state === 'queued' ? '等待准入' : '空闲租约');
}

function detailLine(task: TaskView): string {
  const detail = commandOf(task)?.detail;
  if (detail) return detail;
  if (!task.inflight && task.queued.length === 0 && task.state !== 'queued') {
    return '占着这个窗口，下一条指令进来或空闲超时前不会让出';
  }
  return '';
}

function upcoming(task: TaskView): Step[] {
  return task.inflight ? task.queued : task.queued.slice(1);
}

function shownUpcoming(task: TaskView): Step[] {
  return upcoming(task).slice(0, 8);
}

function hiddenUpcoming(task: TaskView): number {
  return Math.max(0, upcoming(task).length - 8);
}

function meta(task: TaskView): string {
  const parts: string[] = [];
  const who = task.agent_id || task.client_id;
  if (who) parts.push(who);
  if (task.tab_id != null) parts.push(`标签页 ${task.tab_id}`);
  if (task.window_ids.length > 1) parts.push(`同时锁定 ${task.window_ids.join('、')}`);
  if (task.task_id) parts.push(task.task_id);
  return parts.join(' · ');
}

function stateLabel(state: TaskState): string {
  if (state === 'closing') return '收尾中';
  if (state === 'queued') return '排队';
  return '执行中';
}

async function refresh(): Promise<void> {
  if (inflightRefresh) return;
  inflightRefresh = true;
  try {
    const current = await chrome.windows.getCurrent();
    windowId.value = current.id ?? null;
    if (current.id == null) {
      error.value = '无法确定当前窗口';
      return;
    }
    const resp = (await chrome.runtime.sendMessage({
      type: 'window_tasks',
      window_id: current.id,
    })) as {
      ok?: boolean;
      status?: ConnStatus;
      url?: string;
      error?: string;
      snapshot?: { running?: unknown; pending?: unknown };
    } | undefined;
    if (resp?.status) status.value = resp.status;
    if (resp?.url) url.value = resp.url;
    if (resp?.ok) {
      running.value = asTasks(resp.snapshot?.running);
      pending.value = asTasks(resp.snapshot?.pending);
      error.value = '';
      return;
    }
    error.value = resp?.error || '查询失败';
    if (resp?.status !== 'connected') {
      running.value = [];
      pending.value = [];
    }
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err);
  } finally {
    inflightRefresh = false;
  }
}

function startPolling(): void {
  if (timer) return;
  timer = setInterval(() => void refresh(), 1000);
}

function stopPolling(): void {
  if (!timer) return;
  clearInterval(timer);
  timer = null;
}

function onVisibility(): void {
  if (document.visibilityState === 'visible') {
    void refresh();
    startPolling();
  } else {
    stopPolling();
  }
}

onMounted(() => {
  void refresh();
  startPolling();
  document.addEventListener('visibilitychange', onVisibility);
});

onUnmounted(() => {
  stopPolling();
  document.removeEventListener('visibilitychange', onVisibility);
});
</script>

<template>
  <div class="app">
    <header>
      <div>
        <h1>窗口任务</h1>
        <p class="url">{{ subtitle }}</p>
      </div>
      <p :class="['status', status]">{{ statusText }}</p>
    </header>

    <p v-if="error" class="banner">{{ error }}</p>

    <section>
      <div class="section-title">
        <h2>正在执行</h2>
        <span>{{ running.length }}</span>
      </div>
      <p v-if="running.length === 0" class="empty">这个窗口没有正在执行的任务</p>
      <article v-for="task in running" :key="task.key" :class="['card', task.state]">
        <div class="card-top">
          <span class="badge">{{ stateLabel(task.state) }}</span>
        </div>
        <h3>{{ headline(task) }}</h3>
        <p v-if="detailLine(task)" class="detail">{{ detailLine(task) }}</p>
        <p v-if="meta(task)" class="meta">{{ meta(task) }}</p>
        <ol v-if="shownUpcoming(task).length" class="upcoming">
          <li v-for="step in shownUpcoming(task)" :key="`${task.key}-${step.id}-${step.method}`">
            <span class="method">{{ step.method }}</span>
            <span v-if="step.detail" class="step-detail">{{ step.detail }}</span>
          </li>
        </ol>
        <p v-if="hiddenUpcoming(task)" class="more">还有 {{ hiddenUpcoming(task) }} 条</p>
      </article>
    </section>

    <section>
      <div class="section-title">
        <h2>待执行</h2>
        <span>{{ pending.length }}</span>
      </div>
      <p v-if="pending.length === 0" class="empty">没有排在这个窗口后面的任务</p>
      <article v-for="(task, index) in pending" :key="task.key" class="card queued">
        <div class="card-top">
          <span class="badge">{{ stateLabel(task.state) }}</span>
          <span class="place">第 {{ index + 1 }} 位</span>
        </div>
        <h3>{{ headline(task) }}</h3>
        <p v-if="detailLine(task)" class="detail">{{ detailLine(task) }}</p>
        <p v-if="meta(task)" class="meta">{{ meta(task) }}</p>
        <ol v-if="shownUpcoming(task).length" class="upcoming">
          <li v-for="step in shownUpcoming(task)" :key="`${task.key}-${step.id}-${step.method}`">
            <span class="method">{{ step.method }}</span>
            <span v-if="step.detail" class="step-detail">{{ step.detail }}</span>
          </li>
        </ol>
        <p v-if="hiddenUpcoming(task)" class="more">还有 {{ hiddenUpcoming(task) }} 条</p>
      </article>
    </section>
  </div>
</template>

<style>
body {
  margin: 0;
  background: #f4f5f7;
  color: #1c1d1f;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
}
.app {
  box-sizing: border-box;
  min-height: 100vh;
  padding: 16px;
}
header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 14px;
}
h1 {
  margin: 0;
  font-size: 16px;
  font-weight: 650;
}
h2, h3 {
  margin: 0;
  font-weight: 600;
}
h2 {
  font-size: 13px;
}
h3 {
  margin-top: 6px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 13px;
}
.url, .detail, .meta, .empty, .more, .step-detail {
  color: #667085;
}
.url {
  margin: 4px 0 0;
  font-size: 12px;
  word-break: break-all;
}
.status {
  flex: none;
  margin: 2px 0 0;
  font-size: 12px;
  font-weight: 650;
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
.banner {
  margin: 0 0 14px;
  padding: 8px 10px;
  border-radius: 8px;
  background: #fef2f2;
  color: #b91c1c;
  font-size: 12px;
}
section + section {
  margin-top: 18px;
}
.section-title {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 8px;
}
.section-title span, .place, .badge {
  font-size: 11px;
  font-weight: 650;
}
.section-title span {
  color: #667085;
}
.empty {
  margin: 0;
  font-size: 12px;
}
.card {
  margin-top: 8px;
  padding: 10px 12px;
  border: 1px solid #e6e8ec;
  border-left-width: 3px;
  border-radius: 8px;
  background: #fff;
}
.card.running {
  border-left-color: #16a34a;
}
.card.closing {
  border-left-color: #d97706;
}
.card.queued {
  border-left-color: #64748b;
}
.card-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.badge {
  padding: 1px 6px;
  border-radius: 999px;
}
.card.running .badge {
  background: #dcfce7;
  color: #166534;
}
.card.closing .badge {
  background: #ffedd5;
  color: #9a3412;
}
.card.queued .badge {
  background: #f1f5f9;
  color: #334155;
}
.place {
  color: #667085;
}
.detail, .meta, .more {
  margin: 4px 0 0;
  font-size: 12px;
}
.meta {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.upcoming {
  margin: 8px 0 0;
  padding: 0;
  list-style: none;
}
.upcoming li {
  display: flex;
  gap: 8px;
  padding: 4px 0;
  border-top: 1px solid #eef0f3;
  font-size: 12px;
}
.method {
  flex: none;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.step-detail {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
