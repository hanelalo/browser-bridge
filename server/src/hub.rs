//! A single-owner scheduler: admission, deadlines and completion are all
//! handled in this actor, never in detached semaphore waiters.
use crate::{Role, WireMessage};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{sync::mpsc, time::Instant};
use tokio_tungstenite::tungstenite::Message;

const MAX_RUNNING: usize = 3;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const LEASE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// bridge 自建 tab 的容量上限；打满时新开 tab/窗口前逐出最旧的 5 个
const MAX_TRACKED_TABS: usize = 20;
const TAB_EVICT_COUNT: usize = 5;
/// bridge 自建窗口的容量上限；打满时新建窗口前逐出最旧的 3 个
const MAX_TRACKED_WINDOWS: usize = 5;
const WINDOW_EVICT_COUNT: usize = 3;

pub(super) enum HubMsg {
    Register {
        conn_id: u64,
        role: Role,
        name: String,
        client_id: Option<String>,
        tx: mpsc::UnboundedSender<Message>,
    },
    Unregister {
        conn_id: u64,
    },
    Forward {
        conn_id: u64,
        msg: WireMessage,
    },
}

struct Conn {
    role: Role,
    client_id: String,
    tx: mpsc::UnboundedSender<Message>,
}
struct Request {
    conn: u64,
    original: String,
    wire: String,
    msg: WireMessage,
    deadline: Instant,
    replied: bool,
    bootstrap: bool,
}
impl Request {
    fn task_key(&self) -> String {
        if self.bootstrap {
            return format!("bootstrap:{}", self.wire);
        }
        self.public_task_key()
    }
    fn public_task_key(&self) -> String {
        self.msg
            .params
            .as_ref()
            .and_then(|p| p["task_id"].as_str())
            .map(|id| format!("{}:{id}", self.conn))
            .unwrap_or_else(|| format!("request:{}", self.wire))
    }
}
struct Task {
    conn: u64,
    public_id: Option<String>,
    agent_id: Option<String>,
    windows: Vec<i64>,
    tab: Option<i64>,
    started: bool,
    closing: bool,
    inflight: Option<String>,
    queue: VecDeque<Request>,
    touched: Instant,
}

struct Hub {
    conns: HashMap<u64, Conn>,
    extension: Option<u64>,
    extension_instance: Option<String>,
    sync_id: Option<String>,
    completed_during_sync: HashSet<String>,
    resolutions: HashMap<String, Request>,
    inventory: HashMap<String, Request>,
    executing: HashMap<String, (String, Request)>,
    tasks: HashMap<String, Task>,
    order: VecDeque<String>,
    finished: VecDeque<String>,
    /// 延迟关闭队列：(到期时刻, tab_id)。配方任务结束后由 server 在 10~20 秒后
    /// 关闭其自动打开的标签页；在 tick 里触发，不走任务调度（不占窗口锁与并发名额）。
    pending_closes: VecDeque<(Instant, i64)>,
    /// bridge 自建的标签页：tab_id -> (所属窗口, 最近一次被使用时刻)。
    /// 来源包括 new_tab / new_window 首个 tab / agent 默认窗口 / click --new-tab。
    tracked_tabs: HashMap<i64, (Option<i64>, Instant)>,
    /// bridge 自建的窗口：window_id -> 最近一次被使用时刻。同时是
    /// "bridge 只能操作自建窗口"强制拦截的判定依据。
    tracked_windows: HashMap<i64, Instant>,
    sequence: u64,
    epoch: u128,
}
impl Hub {
    fn new() -> Self {
        Self {
            conns: HashMap::new(),
            extension: None,
            extension_instance: None,
            sync_id: None,
            completed_during_sync: HashSet::new(),
            resolutions: HashMap::new(),
            inventory: HashMap::new(),
            executing: HashMap::new(),
            tasks: HashMap::new(),
            order: VecDeque::new(),
            finished: VecDeque::new(),
            pending_closes: VecDeque::new(),
            tracked_tabs: HashMap::new(),
            tracked_windows: HashMap::new(),
            sequence: 0,
            epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        }
    }
    fn id(&mut self, kind: &str) -> String {
        self.sequence += 1;
        format!("{kind}-{}-{}", self.epoch, self.sequence)
    }
    fn send(&self, conn: u64, msg: WireMessage) {
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.tx.send(msg.to_text());
        }
    }
    fn reply(&self, request: &mut Request, result: Result<Value, String>) {
        if request.replied {
            return;
        }
        request.replied = true;
        self.send(
            request.conn,
            match result {
                Ok(value) => WireMessage::ok(&request.original, value),
                Err(error) => WireMessage::err(&request.original, error),
            },
        );
    }
    fn remove_task(&mut self, key: &str) {
        if let Some(task) = self.tasks.remove(key) {
            if task.public_id.is_some() {
                self.finished.push_back(key.into());
                if self.finished.len() > 4096 {
                    self.finished.pop_front();
                }
            }
        }
        self.order.retain(|id| id != key);
    }
    fn close_task(&mut self, key: &str, reason: &str) {
        if let Some(task) = self.tasks.get_mut(key) {
            task.closing = true;
            let requests = std::mem::take(&mut task.queue);
            let idle = task.inflight.is_none();
            for mut request in requests {
                self.reply(&mut request, Err(reason.into()));
            }
            if idle {
                self.remove_task(key);
            }
        }
    }
    fn event(&mut self, event: HubMsg) {
        match event {
            HubMsg::Register {
                conn_id,
                role,
                name,
                client_id,
                tx,
            } => {
                let identity = client_id.unwrap_or_else(|| format!("conn-{conn_id}"));
                if role == Role::Extension {
                    // Do not let another profile's window IDs stand in for an
                    // unresolved execution in the original browser instance.
                    if !self.executing.is_empty()
                        && self
                            .extension_instance
                            .as_ref()
                            .is_some_and(|id| id != &identity)
                    {
                        let _ = tx.send(Message::Close(None));
                        return;
                    }
                    if let Some(old) = self.extension.take() {
                        self.disconnect(old);
                    }
                    self.extension = Some(conn_id);
                    self.extension_instance = Some(identity.clone());
                }
                eprintln!("[hub] #{conn_id} registered as {role:?} {name:?}");
                self.conns.insert(
                    conn_id,
                    Conn {
                        role: role.clone(),
                        client_id: identity,
                        tx,
                    },
                );
                if role == Role::Extension {
                    let id = self.id("sync");
                    self.completed_during_sync.clear();
                    self.sync_id = Some(id.clone());
                    self.send(
                        conn_id,
                        WireMessage {
                            id: Some(id),
                            method: Some("_sync_executions".into()),
                            params: Some(json!({"ids": self.executing.keys().collect::<Vec<_>>()})),
                            ..WireMessage::default()
                        },
                    );
                }
            }
            HubMsg::Unregister { conn_id } => self.disconnect(conn_id),
            HubMsg::Forward { conn_id, msg } => {
                match self.conns.get(&conn_id).map(|c| c.role.clone()) {
                    Some(Role::Client) => self.client(conn_id, msg),
                    Some(Role::Extension) if self.extension == Some(conn_id) => self.browser(msg),
                    _ => {}
                }
            }
        }
        self.schedule();
    }
    fn disconnect(&mut self, conn: u64) {
        let is_extension = self.extension == Some(conn);
        self.conns.remove(&conn);
        if is_extension {
            self.extension = None;
            self.sync_id = None;
            let requests = std::mem::take(&mut self.resolutions)
                .into_values()
                .chain(std::mem::take(&mut self.inventory).into_values());
            for mut request in requests {
                self.reply(
                    &mut request,
                    Err("extension disconnected before execution".into()),
                );
            }
            // Browser scripts may outlive the WebSocket or service worker.
            // Closing is a drain state, not permission to release capacity.
            let keys: Vec<_> = self.tasks.keys().cloned().collect();
            for key in keys {
                self.close_task(&key, "extension disconnected; execution result unknown");
            }
            let executions = std::mem::take(&mut self.executing);
            for (id, (key, mut request)) in executions {
                self.reply(
                    &mut request,
                    Err("extension disconnected; execution result unknown".into()),
                );
                self.executing.insert(id, (key, request));
            }
        } else {
            self.resolutions.retain(|_, r| r.conn != conn);
            self.inventory.retain(|_, r| r.conn != conn);
            let keys: Vec<_> = self
                .tasks
                .iter()
                .filter(|(_, t)| t.conn == conn)
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                self.close_task(&key, "client disconnected");
            }
        }
    }
    fn client(&mut self, conn: u64, mut msg: WireMessage) {
        let Some(original) = msg.id.clone().filter(|id| !id.is_empty()) else {
            self.send(conn, WireMessage::err("", "missing id"));
            return;
        };
        let method = msg.method.as_deref().unwrap_or("").to_string();
        if method == "ping" {
            self.send(conn, WireMessage::ok(&original, json!({"pong":true})));
            return;
        }
        if method == "request_cancel" {
            let target = msg
                .params
                .as_ref()
                .and_then(|p| p["request_id"].as_str())
                .unwrap_or("");
            let ids: Vec<_> = self
                .resolutions
                .iter()
                .filter(|(_, r)| r.conn == conn && r.original == target)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(mut r) = self.resolutions.remove(&id) {
                    self.reply(&mut r, Err("request cancelled".into()));
                }
            }
            let keys: Vec<_> = self.tasks.keys().cloned().collect();
            for key in keys {
                let mut cancelled = Vec::new();
                if let Some(task) = self.tasks.get_mut(&key) {
                    let mut live = VecDeque::new();
                    for r in std::mem::take(&mut task.queue) {
                        if r.conn == conn && r.original == target {
                            cancelled.push(r);
                        } else {
                            live.push_back(r);
                        }
                    }
                    task.queue = live;
                }
                for mut r in cancelled {
                    self.reply(&mut r, Err("request cancelled".into()));
                }
                if self.tasks.get(&key).is_some_and(|t| {
                    t.conn == conn && t.queue.is_empty() && t.inflight.is_none() && !t.started
                }) {
                    self.remove_task(&key);
                }
            }
            let ids: Vec<_> = self
                .executing
                .iter()
                .filter(|(_, (_, r))| r.conn == conn && r.original == target)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some((key, mut r)) = self.executing.remove(&id) {
                    self.reply(
                        &mut r,
                        Err("request cancelled; browser execution is draining".into()),
                    );
                    self.close_task(&key, "request cancelled");
                    self.executing.insert(id, (key, r));
                }
            }
            self.send(conn, WireMessage::ok(&original, json!({"cancelled":true})));
            return;
        }
        if matches!(method.as_str(), "task_end" | "task_cancel" | "task_status") {
            let task_id = msg
                .params
                .as_ref()
                .and_then(|p| p["task_id"].as_str())
                .unwrap_or("");
            if task_id.is_empty() {
                self.send(conn, WireMessage::err(&original, "task_id is required"));
                return;
            }
            let key = format!("{conn}:{task_id}");
            if method == "task_status" {
                let task = self.tasks.get(&key).or_else(|| {
                    self.tasks.values().find(|t| {
                        t.queue.iter().any(|r| r.public_task_key() == key)
                            || t.inflight
                                .as_ref()
                                .and_then(|id| self.executing.get(id))
                                .is_some_and(|(_, r)| r.public_task_key() == key)
                    })
                });
                let value = task.map(|t| json!({"task_id":task_id,
                    "state":if t.closing {"closing"} else if t.started {"running"} else {"queued"},
                    "window_ids":t.windows, "agent_id":t.agent_id, "inflight":t.inflight.is_some()}))
                    .unwrap_or_else(|| json!({"task_id":task_id,"state":if self.resolutions.values().any(|r|r.task_key()==key) {"resolving"} else {"finished"}}));
                self.send(conn, WireMessage::ok(&original, value));
            } else {
                let ids: Vec<_> = self
                    .resolutions
                    .iter()
                    .filter(|(_, r)| r.task_key() == key)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    if let Some(mut r) = self.resolutions.remove(&id) {
                        self.reply(&mut r, Err("task cancelled before execution".into()));
                    }
                }
                self.close_task(&key, "task is closing");
                let preparing: Vec<_> = self
                    .tasks
                    .iter()
                    .filter(|(_, t)| {
                        t.queue.iter().any(|r| r.public_task_key() == key)
                            || t.inflight
                                .as_ref()
                                .and_then(|id| self.executing.get(id))
                                .is_some_and(|(_, r)| r.public_task_key() == key)
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in preparing {
                    self.close_task(&id, "task is closing");
                }
                if !self.finished.contains(&key) {
                    self.finished.push_back(key.clone());
                }
                if self.finished.len() > 4096 {
                    self.finished.pop_front();
                }
                let draining = self.tasks.contains_key(&key)
                    || self
                        .executing
                        .values()
                        .any(|(_, r)| r.public_task_key() == key);
                self.send(
                    conn,
                    WireMessage::ok(
                        &original,
                        json!({"task_id":task_id,"ended":!draining,
                    "state":if draining{"closing"}else{"finished"}}),
                    ),
                );
            }
            return;
        }
        // 延迟关闭调度：只登记定时器并立即应答，真正关闭在 tick 里触发。
        // 不依赖扩展在线（离线时条目等到重连后再执行），也不绑定调用方连接的生命周期。
        if method == "schedule_tab_close" {
            let params = msg.params.clone().unwrap_or(Value::Null);
            let Some(tab_id) = params["tab_id"].as_i64() else {
                self.send(
                    conn,
                    WireMessage::err(&original, "tab_id (number) is required"),
                );
                return;
            };
            let delay_ms = params["delay_ms"]
                .as_u64()
                .unwrap_or(15_000)
                .clamp(1_000, 120_000);
            // 上限保护：异常调用方不能无限堆积待关闭条目
            if self.pending_closes.len() >= 1024 {
                self.pending_closes.pop_front();
            }
            self.pending_closes
                .push_back((Instant::now() + Duration::from_millis(delay_ms), tab_id));
            self.send(
                conn,
                WireMessage::ok(
                    &original,
                    json!({"scheduled":true,"tab_id":tab_id,"delay_ms":delay_ms}),
                ),
            );
            return;
        }
        let Some(ext) = self.extension else {
            self.send(conn, WireMessage::err(&original, "no extension connected"));
            return;
        };
        if self.sync_id.is_some() {
            self.send(
                conn,
                WireMessage::err(
                    &original,
                    "extension is reconciling executions; retry when ready",
                ),
            );
            return;
        }
        if method.starts_with('_') {
            self.send(
                conn,
                WireMessage::err(&original, "internal method is not callable by clients"),
            );
            return;
        }
        if msg.params.as_ref().is_some_and(|p| !p.is_object()) {
            self.send(
                conn,
                WireMessage::err(&original, "params must be an object"),
            );
            return;
        }
        let wire = self.id("request");
        if msg.params.is_none() {
            msg.params = Some(json!({}));
        }
        if msg
            .params
            .as_ref()
            .and_then(|p| p.get("task_id"))
            .is_some_and(|id| !id.is_null() && id.as_str().is_none_or(str::is_empty))
        {
            self.send(
                conn,
                WireMessage::err(&original, "task_id must be a non-empty string"),
            );
            return;
        }
        if method == "task_begin"
            && msg
                .params
                .as_ref()
                .and_then(|p| p["task_id"].as_str())
                .is_none()
        {
            msg.params.as_mut().unwrap()["task_id"] = json!(wire);
        }
        msg.client_id = self.conns.get(&conn).map(|c| c.client_id.clone());
        let remaining = msg.deadline_ms.map(|deadline| {
            deadline.saturating_sub(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            )
        });
        let timeout = Duration::from_millis(
            msg.timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT.as_millis() as u64)
                .clamp(1, 600_000)
                .min(remaining.unwrap_or(u64::MAX)),
        );
        if timeout.is_zero() {
            self.send(
                conn,
                WireMessage::err(&original, "request expired before receipt"),
            );
            return;
        }
        let mut request = Request {
            conn,
            original,
            wire: wire.clone(),
            msg,
            deadline: Instant::now() + timeout,
            replied: false,
            bootstrap: false,
        };
        if matches!(method.as_str(), "list_tabs" | "list_windows") {
            let mut outgoing = request.msg.clone();
            outgoing.id = Some(wire.clone());
            self.inventory.insert(wire, request);
            self.send(ext, outgoing);
            return;
        }
        let key = request.task_key();
        if self.finished.contains(&key) {
            self.reply(&mut request, Err("task already finished".into()));
            return;
        }
        if let Some(task) = self.tasks.get(&key) {
            if task.closing {
                self.reply(&mut request, Err("task is closing".into()));
                return;
            }
            let params = request.msg.params.as_mut().unwrap();
            if params["window_id"].is_null() && task.windows.len() == 1 {
                params["window_id"] = json!(task.windows[0]);
            }
            if params["tab_id"].is_null() && method != "new_tab" {
                if let Some(tab) = task.tab {
                    params["tab_id"] = json!(tab);
                }
            }
        }
        // 资源容量管理：新开 tab / 窗口前先看 bridge 的 tab/window 队列是否打满，
        // 打满则先逐出最旧的若干条目（关闭指令发射后不管，不阻塞本次创建）。
        let creates_window = method == "new_window";
        if creates_window
            || method == "new_tab"
            || (method == "click"
                && request
                    .msg
                    .params
                    .as_ref()
                    .is_some_and(|p| p["new_tab"].as_bool() == Some(true)))
        {
            self.evict_before_create(creates_window);
        }
        self.resolve(request);
    }
    fn resolve(&mut self, mut request: Request) {
        let Some(ext) = self.extension else {
            self.reply(&mut request, Err("no extension connected".into()));
            return;
        };
        let method = request.msg.method.clone();
        let resolve_id = self.id("resolve");
        self.send(
            ext,
            WireMessage {
                id: Some(resolve_id.clone()),
                method: Some("_resolve_target".into()),
                client_id: request.msg.client_id.clone(),
                params: Some(json!({"method":method,"params":request.msg.params})),
                ..WireMessage::default()
            },
        );
        self.resolutions.insert(resolve_id, request);
    }
    fn browser(&mut self, msg: WireMessage) {
        if msg.method.as_deref() == Some("ping") {
            if let (Some(ext), Some(id)) = (self.extension, msg.id.as_deref()) {
                self.send(ext, WireMessage::ok(id, json!({"pong":true})));
            }
            return;
        }
        if msg.method.as_deref() == Some("_window_tasks") {
            self.reply_window_tasks(&msg);
            return;
        }
        if msg.method.as_deref() == Some("_bridge_inventory") {
            if let Some(params) = msg.params.as_ref() {
                self.reconcile_inventory(params);
            }
            return;
        }
        if msg.method.as_deref() == Some("_window_removed") {
            if let Some(window) = msg.params.as_ref().and_then(|p| p["window_id"].as_i64()) {
                let ids: Vec<_> = self
                    .executing
                    .iter()
                    .filter(|(_, (key, _))| {
                        self.tasks.get(key).is_some_and(|t| t.windows == [window])
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.complete(&id, Err("target window was closed".into()));
                }
                let keys: Vec<_> = self
                    .tasks
                    .iter()
                    .filter(|(_, t)| t.windows.contains(&window) && t.inflight.is_none())
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in keys {
                    self.close_task(&key, "target window was closed");
                }
                // 级联清理：窗口没了，bridge 记录里的窗口与其标签页一并移除
                //（覆盖逐出连带的窗口自关、用户手动关闭 bridge 窗口等场景）
                self.remove_tracked_window(window);
            }
            return;
        }
        let Some(id) = msg.id.as_deref() else {
            return;
        };
        if self.sync_id.as_deref() == Some(id) {
            if msg.success != Some(true) {
                return;
            } // Fail closed with an old extension.
            self.sync_id = None;
            for entry in msg
                .result
                .as_ref()
                .and_then(|r| r["executions"].as_array())
                .into_iter()
                .flatten()
            {
                let Some(wire) = entry["id"].as_str() else {
                    continue;
                };
                if matches!(entry["state"].as_str(), Some("finished" | "completed")) {
                    self.complete(
                        wire,
                        Err(
                            "execution finished without a retained result; submit a new request"
                                .into(),
                        ),
                    );
                } else if entry["state"] == "closed" {
                    self.complete(wire, Err("target window was closed".into()));
                } else if matches!(entry["state"].as_str(), Some("running" | "unknown"))
                    && !self.executing.contains_key(wire)
                    && !self.completed_during_sync.contains(wire)
                {
                    // Adopt orphaned executions after a server restart. Their
                    // capacity stays reserved until a completion/closed proof.
                    let windows = window_ids(&entry["window_ids"]).unwrap_or_default();
                    let key = format!("orphan:{wire}");
                    self.tasks.insert(
                        key.clone(),
                        Task {
                            conn: 0,
                            public_id: None,
                            agent_id: None,
                            windows,
                            tab: None,
                            started: true,
                            closing: true,
                            inflight: Some(wire.into()),
                            queue: VecDeque::new(),
                            touched: Instant::now(),
                        },
                    );
                    self.executing.insert(
                        wire.into(),
                        (
                            key,
                            Request {
                                conn: 0,
                                original: wire.into(),
                                wire: wire.into(),
                                msg: WireMessage::default(),
                                deadline: Instant::now() + DEFAULT_TIMEOUT,
                                replied: true,
                                bootstrap: false,
                            },
                        ),
                    );
                }
            }
            self.completed_during_sync.clear();
            return;
        }
        if let Some(mut request) = self.inventory.remove(id) {
            self.reply(&mut request, wire_result(&msg));
            return;
        }
        if let Some(mut request) = self.resolutions.remove(id) {
            if request.deadline <= Instant::now() {
                self.reply(&mut request, Err("request expired before execution".into()));
                return;
            }
            let result = match wire_result(&msg) {
                Ok(v) => v,
                Err(e) => {
                    self.reply(&mut request, Err(e));
                    return;
                }
            };
            let Some(windows) = window_ids(&result["window_ids"]) else {
                self.reply(
                    &mut request,
                    Err("extension returned invalid window scope".into()),
                );
                return;
            };
            if !result["params"].is_object() {
                self.reply(
                    &mut request,
                    Err("extension returned invalid target params".into()),
                );
                return;
            }
            request.bootstrap = result["needs_default_window"] == true;
            // 默认窗口引导即将新建 window+tab：放行前先做容量逐出
            if request.bootstrap {
                self.evict_before_create(true);
            }
            let method = request.msg.method.as_deref().unwrap_or("");
            if windows.is_empty()
                && !request.bootstrap
                && !matches!(
                    method,
                    "new_window" | "close_auto_tabs" | "close_agent_window"
                )
            {
                self.reply(
                    &mut request,
                    Err("extension did not resolve a browser window".into()),
                );
                return;
            }
            request.msg.params = Some(result["params"].clone());
            // 隔离强制：bridge 只能操作自己创建的窗口。引导流程（默认窗口尚未
            // 落地登记）与本身即创建/清理入口的指令豁免；list_tabs/list_windows
            // 走 inventory 路径，到不了这里。
            let exempt = (request.bootstrap && windows.is_empty())
                || matches!(
                    method,
                    "new_window" | "close_auto_tabs" | "close_agent_window"
                );
            if !exempt
                && windows
                    .iter()
                    .any(|w| !self.tracked_windows.contains_key(w))
            {
                self.reply(
                    &mut request,
                    Err(
                        "bridge may only operate on windows it created; omit window_id to use a bridge window, or create one with new_window".into(),
                    ),
                );
                return;
            }
            // 命中自建窗口/标签页：刷新最近使用时间（逐出按此排序）
            let now = Instant::now();
            for w in &windows {
                if let Some(last_used) = self.tracked_windows.get_mut(w) {
                    *last_used = now;
                }
            }
            if let Some(tab_id) = request
                .msg
                .params
                .as_ref()
                .and_then(|p| p["tab_id"].as_i64())
            {
                if let Some(entry) = self.tracked_tabs.get_mut(&tab_id) {
                    entry.1 = now;
                }
            }
            let key = request.task_key();
            if self.finished.contains(&key) {
                self.reply(&mut request, Err("task already finished".into()));
                return;
            }
            if let Some(task) = self.tasks.get(&key) {
                if task.closing || task.windows != windows {
                    self.reply(
                        &mut request,
                        Err("task is closing or target window differs from task scope".into()),
                    );
                    return;
                }
            } else {
                let public_id = request
                    .msg
                    .params
                    .as_ref()
                    .and_then(|p| p["task_id"].as_str())
                    .filter(|_| !request.bootstrap)
                    .map(str::to_string);
                let agent_id = request
                    .msg
                    .params
                    .as_ref()
                    .and_then(|p| p["agent_id"].as_str())
                    .map(str::to_string);
                self.tasks.insert(
                    key.clone(),
                    Task {
                        conn: request.conn,
                        public_id,
                        agent_id,
                        windows,
                        tab: result["params"]["tab_id"].as_i64(),
                        started: false,
                        closing: false,
                        inflight: None,
                        queue: VecDeque::new(),
                        touched: Instant::now(),
                    },
                );
                self.order.push_back(key.clone());
            }
            let task = self.tasks.get_mut(&key).unwrap();
            task.touched = Instant::now();
            task.queue.push_back(request);
            return;
        }
        self.complete(id, wire_result(&msg));
    }
    fn complete(&mut self, wire: &str, result: Result<Value, String>) {
        if self.sync_id.is_some() {
            self.completed_during_sync.insert(wire.to_string());
        }
        let Some((key, mut request)) = self.executing.remove(wire) else {
            return;
        };
        if request.bootstrap {
            let closing = self.tasks.get(&key).is_none_or(|task| task.closing);
            self.remove_task(&key);
            match result {
                Ok(value) if !closing && !request.replied && request.deadline > Instant::now() => {
                    // 默认窗口落地：登记 bridge 自建窗口与其初始标签页，
                    // 后续对该窗口的强制拦截放行依赖这条记录
                    if let Some(w) = value["window_id"].as_i64() {
                        let now = Instant::now();
                        self.tracked_windows.insert(w, now);
                        if let Some(t) = value["tab_id"].as_i64() {
                            self.tracked_tabs.insert(t, (Some(w), now));
                        }
                    }
                    request.bootstrap = false;
                    request.wire = self.id("request");
                    self.resolve(request);
                }
                Err(error) => self.reply(&mut request, Err(error)),
                _ => self.reply(
                    &mut request,
                    Err("request expired or cancelled during window setup".into()),
                ),
            }
            return;
        }
        if let Some(task) = self.tasks.get_mut(&key) {
            task.inflight = None;
            task.touched = Instant::now();
            if let Ok(value) = &result {
                if let Some(tab) = value["tab_id"].as_i64() {
                    task.tab = Some(tab);
                }
            }
        }
        // bridge 自建资源登记/回收：创建类指令成功后登记，关闭类指令成功后回收。
        // 延迟关闭/逐出等 server 直发的 close 不经过这里（发射后不管）。
        let method = request.msg.method.as_deref().unwrap_or("").to_string();
        if let Ok(value) = &result {
            match method.as_str() {
                "new_window" => {
                    if let Some(w) = value["window_id"].as_i64() {
                        let now = Instant::now();
                        self.tracked_windows.insert(w, now);
                        if let Some(t) = value["tab_id"].as_i64() {
                            self.tracked_tabs.insert(t, (Some(w), now));
                        }
                    }
                }
                "new_tab" => {
                    if let Some(t) = value["tab_id"].as_i64() {
                        let w = value["window_id"].as_i64().or_else(|| {
                            request
                                .msg
                                .params
                                .as_ref()
                                .and_then(|p| p["window_id"].as_i64())
                        });
                        self.tracked_tabs.insert(t, (w, Instant::now()));
                    }
                }
                // click/click_at 的 --new-tab 流程由扩展代开标签页，响应带
                // window_id 表示本次新开了 tab（普通 click 响应无 window_id 字段）
                "click" | "click_at" => {
                    if let (Some(t), Some(w)) =
                        (value["tab_id"].as_i64(), value["window_id"].as_i64())
                    {
                        self.tracked_tabs.insert(t, (Some(w), Instant::now()));
                    }
                }
                "close_tab" if value["closed"].as_bool() == Some(true) => {
                    if let Some(t) = value["tab_id"].as_i64() {
                        self.tracked_tabs.remove(&t);
                    }
                }
                "close_window" if value["closed"].as_bool() == Some(true) => {
                    if let Some(w) = value["window_id"].as_i64() {
                        self.remove_tracked_window(w);
                    }
                }
                "close_auto_tabs" => {
                    if let Some(closed) = value["closed"].as_array() {
                        for tab_id in closed.iter().filter_map(Value::as_i64) {
                            self.tracked_tabs.remove(&tab_id);
                        }
                    }
                }
                _ => {}
            }
        }
        self.reply(&mut request, result);
        if self
            .tasks
            .get(&key)
            .is_some_and(|t| t.closing || t.public_id.is_none())
        {
            self.close_task(&key, "task finished");
        }
    }
    fn tick(&mut self) {
        let now = Instant::now();
        // 到期的延迟关闭：直接发给扩展执行。发射后不管——tab 已被手动关闭时
        // 扩展会返回错误，响应 id 不匹配任何在途请求，complete() 会自然忽略。
        if !self.pending_closes.is_empty() && self.sync_id.is_none() && self.extension.is_some() {
            let mut due = VecDeque::new();
            let mut rest = VecDeque::new();
            while let Some(entry) = self.pending_closes.pop_front() {
                if entry.0 <= now {
                    due.push_back(entry);
                } else {
                    rest.push_back(entry);
                }
            }
            self.pending_closes = rest;
            for (_, tab_id) in due {
                self.send_close_tab(tab_id);
            }
        }
        let ids: Vec<_> = self
            .resolutions
            .iter()
            .filter(|(_, r)| r.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut r) = self.resolutions.remove(&id) {
                self.reply(&mut r, Err("request expired resolving target".into()));
            }
        }
        let ids: Vec<_> = self
            .inventory
            .iter()
            .filter(|(_, r)| r.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut r) = self.inventory.remove(&id) {
                self.reply(&mut r, Err("request timed out".into()));
            }
        }
        let ids: Vec<_> = self
            .executing
            .iter()
            .filter(|(_, (_, r))| !r.replied && r.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some((key, mut r)) = self.executing.remove(&id) {
                self.reply(
                    &mut r,
                    Err("request timed out; browser execution is draining".into()),
                );
                self.close_task(&key, "task timed out");
                self.executing.insert(id, (key, r));
            }
        }
        let keys: Vec<_> = self.tasks.keys().cloned().collect();
        for key in keys {
            let mut expired = Vec::new();
            if let Some(task) = self.tasks.get_mut(&key) {
                let mut live = VecDeque::new();
                for r in std::mem::take(&mut task.queue) {
                    if r.deadline <= now {
                        expired.push(r);
                    } else {
                        live.push_back(r);
                    }
                }
                task.queue = live;
            }
            let had_expired = !expired.is_empty();
            for mut r in expired {
                self.reply(&mut r, Err("request expired in queue; not executed".into()));
            }
            if self.tasks.get(&key).is_some_and(|t| {
                t.inflight.is_none()
                    && t.queue.is_empty()
                    && (!t.started
                        || had_expired
                        || now.duration_since(t.touched) >= LEASE_IDLE_TIMEOUT)
            }) {
                self.close_task(&key, "task lease expired");
            }
        }
        self.schedule();
    }
    /// Read-only snapshot for the extension side panel. Does not admit work.
    fn reply_window_tasks(&self, msg: &WireMessage) {
        let Some(ext) = self.extension else {
            return;
        };
        let Some(id) = msg.id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        match msg
            .params
            .as_ref()
            .and_then(|params| params["window_id"].as_i64())
        {
            Some(window_id) if window_id >= 0 => {
                self.send(ext, WireMessage::ok(id, self.window_tasks(window_id)));
            }
            _ => self.send(
                ext,
                WireMessage::err(id, "window_id must be a non-negative integer"),
            ),
        }
    }
    fn window_tasks(&self, window_id: i64) -> Value {
        let mut keys: Vec<String> = self.order.iter().cloned().collect();
        for key in self.tasks.keys() {
            if !keys.iter().any(|existing| existing == key) {
                keys.push(key.clone());
            }
        }
        let mut running = Vec::new();
        let mut pending = Vec::new();
        for key in keys {
            let Some(task) = self.tasks.get(&key) else {
                continue;
            };
            if !task.windows.contains(&window_id) {
                continue;
            }
            let view = self.task_view(&key, task);
            if task.started {
                running.push(view);
            } else {
                pending.push(view);
            }
        }
        json!({ "window_id": window_id, "running": running, "pending": pending })
    }
    fn task_view(&self, key: &str, task: &Task) -> Value {
        let inflight = task
            .inflight
            .as_deref()
            .and_then(|id| self.executing.get(id))
            .map(|(_, request)| step_view(request));
        let queued: Vec<_> = task.queue.iter().map(step_view).collect();
        json!({
            "key": key,
            "task_id": task.public_id,
            "agent_id": task.agent_id,
            "client_id": self.conns.get(&task.conn).map(|conn| conn.client_id.clone()),
            "state": if task.closing { "closing" } else if task.started { "running" } else { "queued" },
            "window_ids": task.windows,
            "tab_id": task.tab,
            "inflight": inflight,
            "queued": queued,
        })
    }
    fn schedule(&mut self) {
        let Some(ext) = self.extension else {
            return;
        };
        if self.sync_id.is_some() {
            return;
        }
        let mut running = self.tasks.values().filter(|t| t.started).count();
        let mut occupied: HashSet<i64> = self
            .tasks
            .values()
            .filter(|t| t.started)
            .flat_map(|t| t.windows.iter().copied())
            .collect();
        for key in self.order.clone() {
            let Some(task) = self.tasks.get_mut(&key) else {
                continue;
            };
            if task.closing || task.inflight.is_some() || task.queue.is_empty() {
                continue;
            }
            // Check expiration at admission too, not just on the periodic tick.
            if task
                .queue
                .front()
                .is_some_and(|r| r.deadline <= Instant::now())
            {
                continue;
            }
            if !task.started {
                if running >= MAX_RUNNING || task.windows.iter().any(|w| occupied.contains(w)) {
                    continue;
                }
                task.started = true;
                running += 1;
                occupied.extend(task.windows.iter().copied());
            }
            let mut request = task.queue.pop_front().unwrap();
            task.touched = Instant::now();
            if request.msg.method.as_deref() == Some("task_begin") {
                let value = json!({"task_id":task.public_id,"state":"running","window_ids":task.windows,"window_id":task.windows.first(),"tab_id":task.tab});
                self.reply(&mut request, Ok(value));
                continue;
            }
            task.inflight = Some(request.wire.clone());
            let mut outgoing = request.msg.clone();
            outgoing.id = Some(request.wire.clone());
            if request.bootstrap {
                outgoing.method = Some("_ensure_default_window".into());
            }
            outgoing.params.as_mut().unwrap()["_window_ids"] = json!(task.windows);
            self.executing.insert(request.wire.clone(), (key, request));
            self.send(ext, outgoing);
        }
    }

    /// 向扩展发射后不管地关闭一个标签页（延迟关闭与逐出共用）。
    /// 扩展对直发的 close_tab 豁免 `_window_ids` 准入检查；标签页已不存在时
    /// 扩展返回错误，响应 id 不匹配任何在途请求，complete() 会自然忽略。
    fn send_close_tab(&mut self, tab_id: i64) {
        let Some(ext) = self.extension else {
            return;
        };
        let wire = self.id("close");
        self.send(
            ext,
            WireMessage {
                id: Some(wire),
                method: Some("close_tab".into()),
                params: Some(json!({ "tab_id": tab_id })),
                ..WireMessage::default()
            },
        );
    }

    /// 向扩展发射后不管地关闭一个窗口（窗口逐出用）。
    fn send_close_window(&mut self, window_id: i64) {
        let Some(ext) = self.extension else {
            return;
        };
        let wire = self.id("close");
        self.send(
            ext,
            WireMessage {
                id: Some(wire),
                method: Some("close_window".into()),
                params: Some(json!({ "window_id": window_id })),
                ..WireMessage::default()
            },
        );
    }

    /// 移除一个 bridge 窗口的全部记录；窗口内标签页记录级联移除
    ///（窗口关闭时 Chrome 会连带关闭其中所有标签页）。
    fn remove_tracked_window(&mut self, window_id: i64) {
        self.tracked_windows.remove(&window_id);
        self.tracked_tabs.retain(|_, (w, _)| *w != Some(window_id));
    }

    /// tab 队列打满时，逐出最近使用时间最旧的 TAB_EVICT_COUNT 个。
    /// 严格按最旧逐出，不做"在用保护"；正在使用被逐出 tab 的任务会收到失败。
    fn evict_tabs_if_full(&mut self) {
        if self.tracked_tabs.len() < MAX_TRACKED_TABS {
            return;
        }
        let mut oldest: Vec<(Instant, i64)> = self
            .tracked_tabs
            .iter()
            .map(|(tab_id, (_, last_used))| (*last_used, *tab_id))
            .collect();
        oldest.sort_by_key(|(last_used, _)| *last_used);
        oldest.truncate(TAB_EVICT_COUNT);
        for (_, tab_id) in oldest {
            self.tracked_tabs.remove(&tab_id);
            self.send_close_tab(tab_id);
        }
    }

    /// 窗口队列打满时，逐出最近使用时间最旧的 WINDOW_EVICT_COUNT 个。
    fn evict_windows_if_full(&mut self) {
        if self.tracked_windows.len() < MAX_TRACKED_WINDOWS {
            return;
        }
        let mut oldest: Vec<(Instant, i64)> = self
            .tracked_windows
            .iter()
            .map(|(window_id, last_used)| (*last_used, *window_id))
            .collect();
        oldest.sort_by_key(|(last_used, _)| *last_used);
        oldest.truncate(WINDOW_EVICT_COUNT);
        for (_, window_id) in oldest {
            self.remove_tracked_window(window_id);
            self.send_close_window(window_id);
        }
    }

    /// 扩展连接时上报现存 bridge 资源清单：server 重启后据此重建队列，
    /// 避免把已存在的 agent 窗口误判为用户窗口而拒绝服务。
    fn reconcile_inventory(&mut self, params: &Value) {
        let now = Instant::now();
        if let Some(windows) = params["windows"].as_array() {
            for window_id in windows.iter().filter_map(Value::as_i64) {
                self.tracked_windows.insert(window_id, now);
            }
        }
        if let Some(tabs) = params["tabs"].as_array() {
            for tab in tabs {
                if let Some(tab_id) = tab["tab_id"].as_i64() {
                    self.tracked_tabs
                        .insert(tab_id, (tab["window_id"].as_i64(), now));
                }
            }
        }
    }

    /// 创建入口（new_tab / new_window / click --new-tab / 默认窗口引导）在放行前
    /// 检查容量并逐出。逐出的关闭指令发射后不管，不阻塞本次创建。
    fn evict_before_create(&mut self, creates_window: bool) {
        self.evict_tabs_if_full();
        if creates_window {
            self.evict_windows_if_full();
        }
    }
}

fn step_view(request: &Request) -> Value {
    json!({
        "id": request.original,
        "method": request.msg.method.clone().unwrap_or_default(),
        "detail": step_detail(request),
    })
}

fn step_detail(request: &Request) -> Option<String> {
    let params = request.msg.params.as_ref()?;
    let method = request.msg.method.as_deref().unwrap_or("");
    let raw = match method {
        "navigate" | "new_tab" => params["url"].as_str(),
        "press_key" => params["key"].as_str(),
        "scrape" => params["item"].as_str(),
        "run_script" | "screenshot" | "get_page_content" | "get_page_markdown"
        | "get_a11y_tree" | "task_begin" | "close_tab" | "close_auto_tabs" | "close_window"
        | "close_agent_window" => None,
        _ => params
            .get("target")
            .and_then(|target| target.get("value"))
            .and_then(Value::as_str)
            .or_else(|| params.get("url").and_then(Value::as_str)),
    }?;
    let text = raw.trim();
    if text.is_empty() {
        None
    } else {
        Some(truncate_chars(text, 96))
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn window_ids(value: &Value) -> Option<Vec<i64>> {
    let mut ids = value
        .as_array()?
        .iter()
        .map(Value::as_i64)
        .collect::<Option<Vec<_>>>()?;
    if ids.iter().any(|id| *id < 0) {
        return None;
    }
    ids.sort_unstable();
    ids.dedup();
    Some(ids)
}
fn wire_result(msg: &WireMessage) -> Result<Value, String> {
    if msg.success == Some(true) {
        Ok(msg.result.clone().unwrap_or(Value::Null))
    } else {
        Err(msg
            .error
            .clone()
            .unwrap_or_else(|| "extension request failed".into()))
    }
}
pub(super) async fn hub_loop(mut rx: mpsc::UnboundedReceiver<HubMsg>) {
    let mut hub = Hub::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            event=rx.recv()=>match event{Some(event)=>hub.event(event),None=>break},
            _=ticker.tick()=>hub.tick(),
        }
    }
}

#[cfg(test)]
mod tests;
