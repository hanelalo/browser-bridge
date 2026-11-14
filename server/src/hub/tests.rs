use super::*;

struct Fixture {
    hub: Hub,
    browser: mpsc::UnboundedReceiver<Message>,
    a: mpsc::UnboundedReceiver<Message>,
    b: mpsc::UnboundedReceiver<Message>,
}
fn recv(rx: &mut mpsc::UnboundedReceiver<Message>) -> WireMessage {
    let Message::Text(text) = rx.try_recv().expect("expected outgoing message") else {
        panic!("expected text")
    };
    serde_json::from_str(&text).unwrap()
}
impl Fixture {
    fn new() -> Self {
        let mut hub = Hub::new();
        let (tx, browser) = mpsc::unbounded_channel();
        hub.event(HubMsg::Register {
            conn_id: 1,
            role: Role::Extension,
            name: "test".into(),
            client_id: Some("instance".into()),
            tx,
        });
        let (tx, a) = mpsc::unbounded_channel();
        hub.event(HubMsg::Register {
            conn_id: 2,
            role: Role::Client,
            name: "a".into(),
            client_id: Some("mcp-a".into()),
            tx,
        });
        let (tx, b) = mpsc::unbounded_channel();
        hub.event(HubMsg::Register {
            conn_id: 3,
            role: Role::Client,
            name: "b".into(),
            client_id: Some("mcp-b".into()),
            tx,
        });
        let mut f = Self { hub, browser, a, b };
        let sync = recv(&mut f.browser);
        f.respond(&sync, json!({"executions":[]}));
        f
    }
    fn request(&mut self, conn: u64, id: &str, method: &str, params: Value) {
        self.hub.event(HubMsg::Forward {
            conn_id: conn,
            msg: WireMessage {
                id: Some(id.into()),
                method: Some(method.into()),
                params: Some(params),
                ..WireMessage::default()
            },
        });
    }
    /// 模拟扩展连接时上报 `_bridge_inventory`：强制拦截只放行 bridge 自建窗口。
    fn track(&mut self, window: i64) {
        self.hub.event(HubMsg::Forward {
            conn_id: self.hub.extension.expect("extension connected"),
            msg: WireMessage {
                id: None,
                method: Some("_bridge_inventory".into()),
                params: Some(json!({
                    "windows": [window],
                    "tabs": [{"tab_id": window * 10, "window_id": window}],
                })),
                ..WireMessage::default()
            },
        });
    }
    fn resolve(&mut self, window: i64) -> WireMessage {
        let resolve = recv(&mut self.browser);
        assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
        let mut params = resolve.params.as_ref().unwrap()["params"].clone();
        params["window_id"] = json!(window);
        if params["tab_id"].is_null() {
            params["tab_id"] = json!(window * 10);
        }
        self.track(window);
        self.respond(&resolve, json!({"window_ids":[window],"params":params}));
        resolve
    }
    fn submit(&mut self, conn: u64, id: &str, window: i64, task: Option<&str>) {
        self.request(
            conn,
            id,
            "run_script",
            json!({"window_id":window,"task_id":task}),
        );
        self.resolve(window);
    }
    fn respond(&mut self, msg: &WireMessage, result: Value) {
        self.hub.event(HubMsg::Forward {
            conn_id: self.hub.extension.expect("extension connected"),
            msg: WireMessage::ok(msg.id.as_deref().unwrap(), result),
        });
    }
    fn window_tasks(&mut self, window: i64) -> Value {
        self.hub.event(HubMsg::Forward {
            conn_id: 1,
            msg: WireMessage {
                id: Some(format!("snap-{window}")),
                method: Some("_window_tasks".into()),
                params: Some(json!({ "window_id": window })),
                ..WireMessage::default()
            },
        });
        let snap = recv(&mut self.browser);
        assert_eq!(snap.success, Some(true));
        snap.result.expect("snapshot")
    }
    fn expire_execution(&mut self, command: &WireMessage) {
        self.hub
            .executing
            .get_mut(command.id.as_deref().unwrap())
            .unwrap()
            .1
            .deadline = Instant::now() - Duration::from_millis(1);
        self.hub.tick();
    }
    /// 注销旧扩展并以新连接注册，返回扩展收到的第一条消息（sync 请求）。
    /// 后续发给扩展的消息走 f.browser 新 channel。
    fn reconnect_extension(&mut self, conn_id: u64) -> WireMessage {
        self.hub.event(HubMsg::Unregister { conn_id: 1 });
        let (tx, rx) = mpsc::unbounded_channel();
        self.hub.event(HubMsg::Register {
            conn_id,
            role: Role::Extension,
            name: "reconnect".into(),
            client_id: Some("instance".into()),
            tx,
        });
        self.browser = rx;
        recv(&mut self.browser)
    }
}

#[test]
fn global_capacity_is_three_and_busy_windows_do_not_take_slots() {
    let mut f = Fixture::new();
    f.submit(2, "a", 1, None);
    let first = recv(&mut f.browser);
    f.submit(3, "busy", 1, None);
    assert!(f.browser.try_recv().is_err());
    f.submit(2, "b", 2, None);
    let _ = recv(&mut f.browser);
    f.submit(2, "c", 3, None);
    let _ = recv(&mut f.browser);
    f.submit(2, "d", 4, None);
    assert!(f.browser.try_recv().is_err());
    assert_eq!(f.hub.executing.len(), 3);
    f.respond(&first, json!({}));
    let _ = recv(&mut f.browser);
    assert_eq!(f.hub.executing.len(), 3);
}

#[test]
fn tab_only_and_window_requests_share_the_same_physical_lock() {
    let mut f = Fixture::new();
    f.submit(2, "by-window", 7, None);
    let first = recv(&mut f.browser);
    f.request(3, "by-tab", "click", json!({"tab_id":70}));
    f.resolve(7);
    assert!(f.browser.try_recv().is_err());
    f.respond(&first, json!({}));
    let _ = recv(&mut f.browser);
}

#[test]
fn commands_inside_one_task_are_serialized_and_lease_survives_replies() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, Some("t"));
    let first = recv(&mut f.browser);
    for id in ["second", "third", "fourth"] {
        f.submit(2, id, 7, Some("t"));
    }
    f.submit(3, "other-agent", 7, None);
    assert!(f.browser.try_recv().is_err());
    assert_eq!(f.hub.executing.len(), 1);
    f.respond(&first, json!({"tab_id":70}));
    let second = recv(&mut f.browser);
    assert_eq!(f.hub.executing.len(), 1);
    f.request(2, "end", "task_end", json!({"task_id":"t"}));
    assert!(f.browser.try_recv().is_err());
    f.respond(&second, json!({}));
    let _ = recv(&mut f.browser); // other agent now runs
    assert!(!f.hub.tasks.contains_key("2:t"));
}

#[test]
fn single_command_timeout_keeps_window_and_global_slot_until_actual_reply() {
    let mut f = Fixture::new();
    f.submit(2, "slow", 7, None);
    let first = recv(&mut f.browser);
    f.expire_execution(&first);
    assert_eq!(recv(&mut f.a).success, Some(false));
    f.submit(3, "next", 7, None);
    assert!(f.browser.try_recv().is_err());
    assert_eq!(f.hub.tasks.values().filter(|t| t.started).count(), 1);
    f.respond(&first, json!({}));
    let _ = recv(&mut f.browser);
    assert!(f.a.try_recv().is_err()); // no duplicate late response
}

#[test]
fn client_disconnect_drains_active_task_but_cancels_queued_work() {
    let mut f = Fixture::new();
    f.submit(2, "active", 7, Some("t"));
    let first = recv(&mut f.browser);
    f.submit(2, "queued", 8, None);
    let another = recv(&mut f.browser); // another active task, must drain too
    f.hub.event(HubMsg::Unregister { conn_id: 2 });
    f.submit(3, "next", 7, None);
    assert!(f.browser.try_recv().is_err());
    f.respond(&first, json!({}));
    let _ = recv(&mut f.browser);
    f.respond(&another, json!({}));
}

#[test]
fn expired_queued_request_is_never_executed_or_given_a_lease() {
    let mut f = Fixture::new();
    f.submit(2, "block", 7, None);
    let first = recv(&mut f.browser);
    f.submit(3, "expired", 7, Some("queued"));
    f.hub.tasks.get_mut("3:queued").unwrap().queue[0].deadline =
        Instant::now() - Duration::from_millis(1);
    f.hub.tick();
    assert_eq!(recv(&mut f.b).success, Some(false));
    f.respond(&first, json!({}));
    assert!(f.browser.try_recv().is_err());
    assert!(!f.hub.tasks.contains_key("3:queued"));
}

#[test]
fn late_response_finishes_timed_out_lease_without_needing_another_end() {
    let mut f = Fixture::new();
    f.submit(2, "slow", 7, Some("t"));
    let first = recv(&mut f.browser);
    f.expire_execution(&first);
    f.request(2, "end", "task_end", json!({"task_id":"t"}));
    assert!(f.hub.tasks.contains_key("2:t"));
    f.submit(3, "next", 7, None);
    assert!(f.browser.try_recv().is_err());
    f.respond(&first, json!({}));
    let _ = recv(&mut f.browser);
    assert!(!f.hub.tasks.contains_key("2:t"));
}

#[test]
fn cancellation_while_resolving_prevents_a_late_admission() {
    let mut f = Fixture::new();
    f.request(
        2,
        "pending",
        "run_script",
        json!({"window_id":7,"task_id":"t"}),
    );
    let resolution = recv(&mut f.browser);
    f.request(2, "cancel", "task_cancel", json!({"task_id":"t"}));
    f.respond(
        &resolution,
        json!({"window_ids":[7],"params":{"window_id":7,"task_id":"t"}}),
    );
    assert!(f.hub.tasks.is_empty());
    assert!(f.browser.try_recv().is_err());
}

#[test]
fn reconnect_releases_finished_execution_without_recovering_a_result() {
    let mut f = Fixture::new();
    f.submit(2, "old", 7, Some("t"));
    let first = recv(&mut f.browser);
    f.hub.event(HubMsg::Unregister { conn_id: 1 });
    assert!(f.hub.tasks.contains_key("2:t"));
    let (tx, rx) = mpsc::unbounded_channel();
    f.hub.event(HubMsg::Register {
        conn_id: 4,
        role: Role::Extension,
        name: "reconnect".into(),
        client_id: Some("instance".into()),
        tx,
    });
    f.browser = rx;
    let sync = recv(&mut f.browser);
    assert_eq!(sync.method.as_deref(), Some("_sync_executions"));
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok(
            sync.id.as_deref().unwrap(),
            json!({"executions":[{
                "id":first.id,"state":"finished"
            }]}),
        ),
    });
    assert!(f.hub.tasks.is_empty());
    assert!(f.hub.executing.is_empty());
    let failure = recv(&mut f.a);
    assert_eq!(failure.success, Some(false));
    assert!(failure.result.is_none());
    assert!(
        f.a.try_recv().is_err(),
        "reconnect must not reply a second time"
    );
    f.submit(3, "retry", 7, None);
    let retry = recv(&mut f.browser);
    assert_eq!(retry.method.as_deref(), Some("run_script"));
}

#[test]
fn server_restart_adopts_browser_executions_before_new_admissions() {
    let mut f = Fixture::new();
    f.hub.event(HubMsg::Unregister { conn_id: 1 });
    let (tx, mut rx) = mpsc::unbounded_channel();
    f.hub.event(HubMsg::Register {
        conn_id: 4,
        role: Role::Extension,
        name: "reconnect".into(),
        client_id: Some("instance".into()),
        tx,
    });
    let sync = recv(&mut rx);
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok(
            sync.id.as_deref().unwrap(),
            json!({"executions":[
                {"id":"orphan-a","state":"running","window_ids":[1]},
                {"id":"orphan-b","state":"unknown","window_ids":[2]},
                {"id":"orphan-c","state":"running","window_ids":[3]}
            ]}),
        ),
    });
    assert_eq!(f.hub.tasks.values().filter(|t| t.started).count(), 3);
    assert_eq!(f.hub.executing.len(), 3);
}

#[test]
fn task_cannot_switch_windows_and_other_clients_cannot_end_it() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, Some("t"));
    let _ = recv(&mut f.browser);
    f.submit(2, "wrong-window", 8, Some("t"));
    assert_eq!(recv(&mut f.a).success, Some(false));
    f.request(3, "foreign-end", "task_end", json!({"task_id":"t"}));
    assert!(f.hub.tasks.contains_key("2:t"));
    assert!(f.browser.try_recv().is_err());
}

#[test]
fn cleanup_reserves_all_affected_windows_atomically() {
    let mut f = Fixture::new();
    f.submit(2, "active", 7, None);
    let first = recv(&mut f.browser);
    f.request(3, "cleanup", "close_auto_tabs", json!({}));
    let resolve = recv(&mut f.browser);
    f.respond(
        &resolve,
        json!({"window_ids":[7,8],"params":{"tab_ids":[70,80]}}),
    );
    assert!(f.browser.try_recv().is_err());
    f.submit(2, "other", 8, None);
    let other = recv(&mut f.browser); // cleanup waiting on 7 must not reserve 8
    f.respond(&first, json!({}));
    assert!(f.browser.try_recv().is_err());
    f.respond(&other, json!({}));
    let cleanup = recv(&mut f.browser);
    assert_eq!(cleanup.method.as_deref(), Some("close_auto_tabs"));
}

#[test]
fn three_timed_out_operations_still_block_a_fourth_window() {
    let mut f = Fixture::new();
    let mut active = Vec::new();
    for window in 1..=3 {
        f.submit(2, &format!("w{window}"), window, None);
        active.push(recv(&mut f.browser));
    }
    for command in &active {
        f.expire_execution(command);
    }
    f.submit(3, "fourth", 4, None);
    assert!(f.browser.try_recv().is_err());
    f.respond(&active[0], json!({}));
    let _ = recv(&mut f.browser);
    assert_eq!(f.hub.tasks.values().filter(|t| t.started).count(), 3);
}

#[test]
fn expired_absolute_deadlines_are_rejected_before_target_resolution() {
    let mut f = Fixture::new();
    f.hub.event(HubMsg::Forward {
        conn_id: 2,
        msg: WireMessage {
            id: Some("expired".into()),
            method: Some("click".into()),
            params: Some(json!({})),
            deadline_ms: Some(1),
            ..WireMessage::default()
        },
    });
    assert_eq!(recv(&mut f.a).success, Some(false));
    assert!(f.browser.try_recv().is_err());
}

#[test]
fn task_begin_returns_context_and_next_step_is_pinned_to_it() {
    let mut f = Fixture::new();
    f.request(2, "begin", "task_begin", json!({"window_id":7}));
    f.resolve(7);
    let response = recv(&mut f.a);
    let task_id = response.result.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(f.hub.executing.len(), 0);
    f.request(2, "step", "click", json!({"task_id":task_id}));
    let resolution = recv(&mut f.browser);
    let p = &resolution.params.as_ref().unwrap()["params"];
    assert_eq!(p["window_id"], 7);
    assert_eq!(p["tab_id"], 70);
}

#[test]
fn request_cancellation_removes_queued_work_without_releasing_active_work() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, None);
    let first = recv(&mut f.browser);
    f.submit(3, "queued", 7, None);
    f.request(
        3,
        "cancel",
        "request_cancel",
        json!({"request_id":"queued"}),
    );
    f.respond(&first, json!({}));
    assert!(f.browser.try_recv().is_err());
    assert!(f.hub.tasks.is_empty());
}

#[test]
fn identical_client_request_ids_do_not_overwrite_each_other() {
    let mut f = Fixture::new();
    f.submit(2, "same", 7, None);
    let first = recv(&mut f.browser);
    f.submit(3, "same", 8, None);
    let second = recv(&mut f.browser);
    assert_ne!(first.id, second.id);
    f.respond(&second, json!({"owner":"b"}));
    f.respond(&first, json!({"owner":"a"}));
    assert_eq!(recv(&mut f.a).result.unwrap()["owner"], "a");
    assert_eq!(recv(&mut f.b).result.unwrap()["owner"], "b");
}

#[test]
fn completion_arriving_before_a_running_sync_snapshot_cannot_create_a_ghost() {
    let mut f = Fixture::new();
    f.hub.event(HubMsg::Unregister { conn_id: 1 });
    let (tx, mut rx) = mpsc::unbounded_channel();
    f.hub.event(HubMsg::Register {
        conn_id: 4,
        role: Role::Extension,
        name: "reconnect".into(),
        client_id: Some("instance".into()),
        tx,
    });
    let sync = recv(&mut rx);
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok("already-completed", json!({})),
    });
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok(
            sync.id.as_deref().unwrap(),
            json!({"executions":[
                {"id":"already-completed","state":"running","window_ids":[7]}
            ]}),
        ),
    });
    assert!(f.hub.tasks.is_empty());
    assert!(f.hub.executing.is_empty());
}

#[test]
fn default_window_creation_also_waits_for_global_capacity() {
    let mut f = Fixture::new();
    let mut active = Vec::new();
    for window in 1..=3 {
        f.submit(2, &format!("w{window}"), window, None);
        active.push(recv(&mut f.browser));
    }
    f.request(3, "needs-window", "click", json!({"task_id":"new-task"}));
    let resolution = recv(&mut f.browser);
    f.respond(
        &resolution,
        json!({"window_ids":[],"needs_default_window":true,"params":{"task_id":"new-task"}}),
    );
    assert!(f.browser.try_recv().is_err());
    f.respond(&active[0], json!({}));
    let setup = recv(&mut f.browser);
    assert_eq!(setup.method.as_deref(), Some("_ensure_default_window"));
    f.respond(&setup, json!({"window_id":8,"tab_id":80}));
    f.resolve(8);
    let actual = recv(&mut f.browser);
    assert_eq!(actual.method.as_deref(), Some("click"));
    assert_ne!(actual.id, setup.id);
    assert_eq!(f.hub.executing.len(), 3);
}

#[test]
fn window_task_snapshot_splits_running_and_pending_for_one_window() {
    let mut f = Fixture::new();
    f.submit(2, "run", 7, Some("lease"));
    let _running = recv(&mut f.browser);
    f.submit(2, "next", 7, Some("lease"));
    assert!(
        f.browser.try_recv().is_err(),
        "follow-up stays behind the inflight command"
    );
    f.submit(3, "wait", 7, None);
    assert!(
        f.browser.try_recv().is_err(),
        "a second task waits for the busy window"
    );
    f.submit(2, "other", 8, None);
    let _other = recv(&mut f.browser);

    let here = f.window_tasks(7);
    assert_eq!(here["window_id"], 7);
    assert_eq!(here["running"].as_array().unwrap().len(), 1);
    assert_eq!(here["pending"].as_array().unwrap().len(), 1);
    let running = &here["running"][0];
    assert_eq!(running["state"].as_str(), Some("running"));
    assert_eq!(running["task_id"].as_str(), Some("lease"));
    assert_eq!(running["client_id"].as_str(), Some("mcp-a"));
    assert_eq!(running["inflight"]["id"].as_str(), Some("run"));
    assert_eq!(running["inflight"]["method"].as_str(), Some("run_script"));
    assert!(running["inflight"]["detail"].is_null());
    assert_eq!(running["queued"][0]["id"].as_str(), Some("next"));
    let pending = &here["pending"][0];
    assert_eq!(pending["state"].as_str(), Some("queued"));
    assert!(pending["task_id"].is_null());
    assert_eq!(pending["client_id"].as_str(), Some("mcp-b"));
    assert!(pending["inflight"].is_null());
    assert_eq!(pending["queued"][0]["id"].as_str(), Some("wait"));

    let elsewhere = f.window_tasks(8);
    assert_eq!(elsewhere["running"].as_array().unwrap().len(), 1);
    assert_eq!(
        elsewhere["running"][0]["inflight"]["id"].as_str(),
        Some("other")
    );
    assert!(elsewhere["pending"].as_array().unwrap().is_empty());
    let empty = f.window_tasks(99);
    assert!(empty["running"].as_array().unwrap().is_empty());
    assert!(empty["pending"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn scheduled_tab_close_fires_after_delay_without_task_scheduling() {
    let mut f = Fixture::new();
    f.request(
        2,
        "s1",
        "schedule_tab_close",
        json!({"tab_id": 42, "delay_ms": 1000}),
    );
    let reply = recv(&mut f.a);
    assert_eq!(reply.success, Some(true));
    assert_eq!(reply.result.as_ref().unwrap()["scheduled"], json!(true));
    assert_eq!(reply.result.as_ref().unwrap()["tab_id"], json!(42));
    assert_eq!(reply.result.as_ref().unwrap()["delay_ms"], json!(1000));

    // 未到期：不向扩展发送任何指令
    f.hub.tick();
    assert!(f.browser.try_recv().is_err());

    // 到期后：直接向扩展发 close_tab（发射后不管，不走 _resolve_target / 任务队列）
    tokio::time::sleep(Duration::from_millis(1100)).await;
    f.hub.tick();
    let close = recv(&mut f.browser);
    assert_eq!(close.method.as_deref(), Some("close_tab"));
    assert_eq!(close.params.as_ref().unwrap()["tab_id"], json!(42));
}

#[tokio::test]
async fn scheduled_tab_close_waits_for_extension_reconnect() {
    let mut f = Fixture::new();
    f.request(2, "s1", "schedule_tab_close", json!({"tab_id": 7}));
    let reply = recv(&mut f.a);
    assert_eq!(reply.result.as_ref().unwrap()["delay_ms"], json!(15000));
    // 扩展离线：到期条目保留，等重连后再执行
    let ext = f.hub.extension.take();
    f.hub.tick();
    assert!(f.browser.try_recv().is_err());
    f.hub.extension = ext;
    // 把到期时刻拨到过去：直接改写队列条目模拟时间流逝
    let mut entries: VecDeque<(Instant, i64)> = std::mem::take(&mut f.hub.pending_closes);
    for (at, tab) in entries.iter_mut() {
        *at = Instant::now() - Duration::from_millis(1);
        assert_eq!(*tab, 7);
    }
    f.hub.pending_closes = entries;
    f.hub.tick();
    let close = recv(&mut f.browser);
    assert_eq!(close.method.as_deref(), Some("close_tab"));
    assert_eq!(close.params.as_ref().unwrap()["tab_id"], json!(7));
}

#[test]
fn requests_targeting_non_bridge_windows_are_rejected() {
    let mut f = Fixture::new();
    // 未先对账登记窗口 7：解析命中用户窗口 → 强制拦截
    f.request(2, "r1", "run_script", json!({}));
    let resolve = recv(&mut f.browser);
    assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
    let mut params = resolve.params.as_ref().unwrap()["params"].clone();
    params["window_id"] = json!(7);
    if params["tab_id"].is_null() {
        params["tab_id"] = json!(70);
    }
    f.respond(&resolve, json!({"window_ids":[7],"params":params}));
    let reply = recv(&mut f.a);
    assert_eq!(reply.success, Some(false));
    assert!(reply
        .error
        .as_deref()
        .unwrap()
        .contains("bridge may only operate on windows it created"));
    // 对账登记后同一窗口放行
    f.track(7);
    f.request(2, "r2", "run_script", json!({}));
    let resolve = recv(&mut f.browser);
    f.respond(
        &resolve,
        json!({"window_ids":[7],"params":{"tab_id":70,"window_id":7}}),
    );
    let admitted = recv(&mut f.browser);
    assert_eq!(admitted.method.as_deref(), Some("run_script"));
}

#[test]
fn new_tab_admission_evicts_five_oldest_tracked_tabs() {
    let mut f = Fixture::new();
    // 登记 20 个自建 tab（窗口 9），last_used 随 tab_id 递增变旧：115..=119 最旧
    let tabs: Vec<Value> = (100..120)
        .map(|t| json!({"tab_id": t, "window_id": 9}))
        .collect();
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_bridge_inventory".into()),
            params: Some(json!({"windows": [9], "tabs": tabs})),
            ..WireMessage::default()
        },
    });
    let base = Instant::now();
    for (&tab_id, entry) in f.hub.tracked_tabs.iter_mut() {
        entry.1 = base - Duration::from_millis((tab_id - 100) as u64);
    }

    f.request(2, "nt", "new_tab", json!({}));
    // 放行前先直发 5 个 close_tab（发射后不管，不带 _window_ids）
    let mut evicted = Vec::new();
    for _ in 0..5 {
        let close = recv(&mut f.browser);
        assert_eq!(close.method.as_deref(), Some("close_tab"));
        assert!(close.params.as_ref().unwrap()["_window_ids"].is_null());
        evicted.push(close.params.as_ref().unwrap()["tab_id"].as_i64().unwrap());
    }
    evicted.sort();
    assert_eq!(evicted, vec![115, 116, 117, 118, 119]);
    assert_eq!(f.hub.tracked_tabs.len(), 15);
    // 之后 new_tab 正常解析放行
    let resolve = recv(&mut f.browser);
    assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
    f.respond(
        &resolve,
        json!({"window_ids":[9],"params":{"tab_id":90,"window_id":9}}),
    );
    let admitted = recv(&mut f.browser);
    assert_eq!(admitted.method.as_deref(), Some("new_tab"));
}

#[test]
fn new_window_admission_evicts_three_oldest_tracked_windows() {
    let mut f = Fixture::new();
    let windows: Vec<i64> = (0..5).map(|i| 50 + i).collect();
    let tabs: Vec<Value> = windows
        .iter()
        .map(|w| json!({"tab_id": w * 10, "window_id": w}))
        .collect();
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_bridge_inventory".into()),
            params: Some(json!({"windows": windows, "tabs": tabs})),
            ..WireMessage::default()
        },
    });
    let base = Instant::now();
    for (&window_id, last_used) in f.hub.tracked_windows.iter_mut() {
        // 50 最旧、54 最新
        *last_used = base - Duration::from_millis((60 - window_id) as u64);
    }

    f.request(2, "nw", "new_window", json!({}));
    // 窗口队列满 5：先直发 3 个 close_window，逐出最旧的 50/51/52
    let mut evicted = Vec::new();
    for _ in 0..3 {
        let close = recv(&mut f.browser);
        assert_eq!(close.method.as_deref(), Some("close_window"));
        evicted.push(
            close.params.as_ref().unwrap()["window_id"]
                .as_i64()
                .unwrap(),
        );
    }
    evicted.sort();
    assert_eq!(evicted, vec![50, 51, 52]);
    assert_eq!(f.hub.tracked_windows.len(), 2);
    // 被逐出窗口的标签页记录级联移除
    assert!(!f.hub.tracked_tabs.contains_key(&500));
    assert!(!f.hub.tracked_tabs.contains_key(&520));
    assert!(f.hub.tracked_tabs.contains_key(&530));
    // 之后 new_window 正常解析放行（new_window 解析豁免窗口范围，直接执行）
    let resolve = recv(&mut f.browser);
    assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
    f.respond(&resolve, json!({"window_ids":[],"params":{}}));
    let admitted = recv(&mut f.browser);
    assert_eq!(admitted.method.as_deref(), Some("new_window"));
    // 执行成功后新窗口 60 登记为 bridge 自建
    f.respond(
        &admitted,
        json!({"window_id":60,"tab_id":600,"focused":false}),
    );
    assert!(f.hub.tracked_windows.contains_key(&60));
    assert!(f.hub.tracked_tabs.contains_key(&600));
}

#[test]
fn window_removed_notification_cascades_tab_records() {
    let mut f = Fixture::new();
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_bridge_inventory".into()),
            params: Some(json!({
                "windows": [9, 8],
                "tabs": [
                    {"tab_id": 60, "window_id": 9},
                    {"tab_id": 61, "window_id": 9},
                    {"tab_id": 80, "window_id": 8},
                ],
            })),
            ..WireMessage::default()
        },
    });
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_window_removed".into()),
            params: Some(json!({"window_id": 9})),
            ..WireMessage::default()
        },
    });
    assert!(!f.hub.tracked_windows.contains_key(&9));
    assert!(f.hub.tracked_windows.contains_key(&8));
    assert!(!f.hub.tracked_tabs.contains_key(&60));
    assert!(!f.hub.tracked_tabs.contains_key(&61));
    assert!(f.hub.tracked_tabs.contains_key(&80));
}

#[test]
fn execution_expired_notification_releases_window_and_capacity() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, None);
    let command = recv(&mut f.browser);
    let wire = command.id.clone().unwrap();
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_execution_expired".into()),
            params: Some(json!({"id": wire})),
            ..WireMessage::default()
        },
    });
    // 扩展宣告丢失：在途执行按失败完成，额度与窗口锁立即释放
    let failure = recv(&mut f.a);
    assert_eq!(failure.success, Some(false));
    assert!(f.hub.executing.is_empty());
    assert!(f.hub.tasks.is_empty());
    f.submit(2, "second", 7, None);
    assert_eq!(recv(&mut f.browser).method.as_deref(), Some("run_script"));
}

#[test]
fn sync_finished_report_releases_drained_capacity() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, None);
    let command = recv(&mut f.browser);
    let wire = command.id.clone().unwrap();
    // 客户端超时进入 draining：额度仍被保留，直到拿到完成证明
    f.expire_execution(&command);
    assert_eq!(recv(&mut f.a).success, Some(false));
    assert_eq!(f.hub.executing.len(), 1);
    // 扩展重连后上报 finished（SW 重启恢复的 unknown 条目按此上报）：
    // 释放额度，窗口可立即复用
    let sync = f.reconnect_extension(4);
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok(
            sync.id.as_deref().unwrap(),
            json!({"executions":[{"id":wire,"state":"finished"}]}),
        ),
    });
    assert!(f.hub.executing.is_empty());
    assert!(f.hub.tasks.is_empty());
    f.submit(2, "second", 7, None);
    assert_eq!(recv(&mut f.browser).method.as_deref(), Some("run_script"));
}

#[test]
fn orphan_lease_expiry_fails_open_after_deadline() {
    let mut f = Fixture::new();
    let sync = f.reconnect_extension(4);
    f.hub.event(HubMsg::Forward {
        conn_id: 4,
        msg: WireMessage::ok(
            sync.id.as_deref().unwrap(),
            json!({"executions":[{"id":"orphan-a","state":"running","window_ids":[7]}]}),
        ),
    });
    assert_eq!(f.hub.executing.len(), 1);
    // 租期未到：占用保持
    f.hub.tick();
    assert_eq!(f.hub.executing.len(), 1);
    // 租期到期且始终没有完成证明：fail-open 释放，窗口可复用
    f.hub.executing.get_mut("orphan-a").unwrap().1.deadline =
        Instant::now() - Duration::from_millis(1);
    f.hub.tick();
    assert!(f.hub.executing.is_empty());
    assert!(f.hub.tasks.is_empty());
    f.submit(2, "retry", 7, None);
    assert_eq!(recv(&mut f.browser).method.as_deref(), Some("run_script"));
}

#[test]
fn fire_and_forget_closes_reclaim_tracked_records_immediately() {
    let mut f = Fixture::new();
    f.track(7);
    assert!(f.hub.tracked_tabs.contains_key(&70));
    f.hub.send_close_tab(70);
    assert!(!f.hub.tracked_tabs.contains_key(&70));
    assert!(f.hub.tracked_windows.contains_key(&7));
    f.hub.send_close_window(7);
    assert!(!f.hub.tracked_windows.contains_key(&7));
    assert!(f.hub.tracked_tabs.is_empty());
}

#[test]
fn inventory_reconcile_replaces_stale_records_instead_of_accumulating() {
    let mut f = Fixture::new();
    f.track(7);
    assert!(f.hub.tracked_tabs.contains_key(&70));
    f.hub.event(HubMsg::Forward {
        conn_id: 1,
        msg: WireMessage {
            id: None,
            method: Some("_bridge_inventory".into()),
            params: Some(json!({
                "windows": [8],
                "tabs": [{"tab_id": 80, "window_id": 8}],
            })),
            ..WireMessage::default()
        },
    });
    // 全量对账：已失效的窗口/标签页记录被清掉，而不是永久堆积
    assert!(f.hub.tracked_windows.contains_key(&8));
    assert!(!f.hub.tracked_windows.contains_key(&7));
    assert!(f.hub.tracked_tabs.contains_key(&80));
    assert!(!f.hub.tracked_tabs.contains_key(&70));
}

#[test]
fn extension_instance_change_completes_stale_executions_instead_of_rejecting() {
    let mut f = Fixture::new();
    f.submit(2, "first", 7, None);
    let command = recv(&mut f.browser);
    let wire = command.id.clone().unwrap();
    // 扩展重连后该执行被采纳为孤儿（无完成证明，占窗口与全局额度）
    let sync = f.reconnect_extension(4);
    f.respond(
        &sync,
        json!({"executions":[{"id":wire,"state":"running","window_ids":[7]}]}),
    );
    assert_eq!(f.hub.executing.len(), 1);
    // 换实例注册（扩展重载）：旧实例的孤儿按丢失完成，新实例被放行，
    // 而不是被拒绝后陷入无限重连循环
    let (tx, rx) = mpsc::unbounded_channel();
    f.hub.event(HubMsg::Register {
        conn_id: 5,
        role: Role::Extension,
        name: "reloaded".into(),
        client_id: Some("new-instance".into()),
        tx,
    });
    f.browser = rx;
    let sync2 = recv(&mut f.browser);
    assert_eq!(sync2.method.as_deref(), Some("_sync_executions"));
    f.respond(&sync2, json!({"executions":[]}));
    assert!(f.hub.executing.is_empty());
    assert!(f.hub.tasks.is_empty());
    // 等待结果的客户端收到明确错误，而不是一直挂到超时
    let failure = recv(&mut f.a);
    assert_eq!(failure.success, Some(false));
    // 新实例接续调度
    f.request(2, "second", "run_script", json!({"window_id": 7}));
    let resolve = recv(&mut f.browser);
    assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
    f.track(7);
    f.respond(
        &resolve,
        json!({"window_ids":[7],"params":{"tab_id":70,"window_id":7}}),
    );
    assert_eq!(recv(&mut f.browser).method.as_deref(), Some("run_script"));
}
