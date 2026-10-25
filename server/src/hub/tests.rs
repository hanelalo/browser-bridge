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
    fn resolve(&mut self, window: i64) -> WireMessage {
        let resolve = recv(&mut self.browser);
        assert_eq!(resolve.method.as_deref(), Some("_resolve_target"));
        let mut params = resolve.params.as_ref().unwrap()["params"].clone();
        params["window_id"] = json!(window);
        if params["tab_id"].is_null() {
            params["tab_id"] = json!(window * 10);
        }
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
