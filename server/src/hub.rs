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
                Ok(_) if !closing && !request.replied && request.deadline > Instant::now() => {
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
pub(super) async fn hub_loop(
    mut rx: mpsc::UnboundedReceiver<HubMsg>,
    _tx: mpsc::UnboundedSender<HubMsg>,
    idle_timeout: Duration,
) {
    let mut hub = Hub::new();
    let mut last_activity = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            event=rx.recv()=>match event{Some(event)=>{last_activity=Instant::now();hub.event(event);},None=>break},
            _=ticker.tick()=>{
                hub.tick();
                if !idle_timeout.is_zero()&&hub.tasks.is_empty()&&hub.resolutions.is_empty()&&last_activity.elapsed()>=idle_timeout {
                    eprintln!("[hub] idle, exiting");std::process::exit(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
