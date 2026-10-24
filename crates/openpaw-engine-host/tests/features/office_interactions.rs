//! 职责：验证新增Office交互配置和共享只读数据边界。
//! 输入输出：真实配置及隔离数据到授权/只读断言。
//! 依赖：现有Host与原技能/目标/Todo查询。
//! 禁止职责：不将共享服务测试冒充真实Office浏览器验证。
use openpaw_engine_host::*;
use serde_json::json;
fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    serde_json::from_value(json!({"home":base.join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","office":{"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"]}})).unwrap()
}
#[test]
fn explicit_write_tools_are_allowed_but_default_remains_readonly_and_unsupported_tools_reject() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.validate().unwrap();
    assert!(!cfg.tools.iter().any(|s| s == "write_file"));
    for name in ["write_file", "edit_file", "todo_add", "todo_update", "bash"] {
        cfg.tools = vec![name.into()];
        cfg.validate().unwrap();
    }
    for name in ["notebook_edit", "sleep"] {
        cfg.tools = vec![name.into()];
        assert!(cfg.validate().is_err());
    }
}
#[tokio::test]
async fn readonly_categories_do_not_create_files_and_deleted_scope_cannot_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.office = None;
    let host = Host::start(cfg.clone()).await.unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "read".into(),
    };
    let b = host.open_session(scope.clone()).await.unwrap();
    for kind in [QueryKind::Skills, QueryKind::Goals, QueryKind::Todo] {
        let page = host
            .query(
                &scope,
                QueryRequest {
                    session_id: b.session_id.clone(),
                    kind,
                    offset: 0,
                    limit: 5,
                },
            )
            .await
            .unwrap();
        assert!(page.items.is_empty());
    }
    for path in [".openpaw/skills", ".openpaw/goals", ".openpaw/todo.json"] {
        assert!(
            !cfg.roots["r"].join(path).exists(),
            "只读查询不能创建{path}"
        );
    }
    host.delete_session(&scope).await.unwrap();
    assert!(host
        .query(
            &scope,
            QueryRequest {
                session_id: b.session_id,
                kind: QueryKind::History,
                offset: 0,
                limit: 5
            }
        )
        .await
        .is_err());
    host.shutdown().await.unwrap();
}

#[test]
fn office_approval_and_ui_use_real_http_with_shared_cas() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interaction_fixture_child", "--nocapture"])
        .env("OPENPAW_INTERACTION_CHILD", "1")
        .env("PAWOFFICE_TEST_KEY", "isolated-office-key")
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn interaction_fixture_child() {
    if std::env::var("OPENPAW_INTERACTION_CHILD").as_deref() != Ok("1") {
        return;
    }
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(interaction_scenario());
}
#[derive(Default)]
struct Fixture {
    snapshot: Option<serde_json::Value>,
    actions: std::collections::VecDeque<serde_json::Value>,
    responses: Vec<serde_json::Value>,
    decisions: Vec<serde_json::Value>,
    confirm: bool,
    wrong_scope: bool,
    custom_text: bool,
    expire_delete_reply: bool,
}
async fn interaction_scenario() {
    use axum::{extract::State, routing::post, Json, Router};
    use openpaw_llm::{MockMode, MockProvider};
    use serde_json::Value;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::Mutex;
    let shared = Arc::new(Mutex::new(Fixture {
        confirm: true,
        ..Default::default()
    }));
    async fn handle(
        State(shared): State<Arc<Mutex<Fixture>>>,
        Json(r): Json<Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let mut s = shared.lock().await;
        let actors: Vec<_> = ["office-lead", "file-agent", "data-agent", "code-agent"]
            .iter()
            .map(|id| json!({"id":id}))
            .collect();
        let ui = json!({"slots":[],"actions":[],"actionSchema":{},"presentationSchema":{},"whiteboardSchema":{}});
        let data = match r["type"].as_str().unwrap_or("") {
            "office.capabilities" => {
                json!({"scene":{"protocolVersion":"2.0","actors":actors,"capabilities":[]},"ui":ui,"requests":{},"responses":{"success":{},"error":{}}})
            }
            "office.read" if r.get("teamId").is_some() => {
                json!({"snapshot":s.snapshot.clone().unwrap(),"scene":{},"pendingDecisions":[]})
            }
            "office.read" | "offices.initialize" => {
                if r["type"] == "offices.initialize" {
                    if let Some(first) = r["offices"].as_array().and_then(|v| v.first()) {
                        s.snapshot = Some(first.clone());
                    }
                }
                json!({"offices":if s.snapshot.is_some(){vec![json!({"id":"office-1"})]}else{vec![]},"actors":[],"supportedMessages":[],"sceneCommandProtocol":"2.0","ui":ui})
            }
            "office.snapshot" => {
                s.snapshot = Some(r["snapshot"].clone());
                json!({"teamId":r["teamId"],"seq":r["snapshot"]["seq"],"status":"applied"})
            }
            "ui.next" => {
                let action = s.actions.pop_front();
                drop(s);
                if action.is_none() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                json!({"actions":action.into_iter().collect::<Vec<_>>()})
            }
            "ui.respond" => {
                s.responses.push(r.clone());
                if s.expire_delete_reply
                    && (r["interactionId"] == "delete-6"
                        || r["interactionId"] == "create-member-expired")
                {
                    return (axum::http::StatusCode::GONE,Json(json!({"code":410,"msg":"expired","data":{"reasonCode":"UI_REQUEST_EXPIRED"}}))).into_response();
                }
                json!({"interactionId":r["interactionId"],"delivered":true})
            }
            "decision.request" => {
                let confirm = s.confirm;
                let wrong_scope = s.wrong_scope;
                let custom_text = s.custom_text;
                s.decisions.push(r.clone());
                drop(s);
                tokio::time::sleep(Duration::from_millis(400)).await;
                let mut result = json!({"teamId":if wrong_scope {json!("other-office")}else{r["teamId"].clone()},"conversationId":r["conversationId"],"decisionId":r["decision"]["id"],"choiceId":if confirm{"confirm"}else{"reject"},"label":r["decision"][if confirm{"confirmLabel"}else{"rejectLabel"}]});
                if custom_text {
                    result["customText"] = json!("允许永久执行");
                }
                result
            }
            _ => panic!("unexpected request {r}"),
        };
        Json(json!({"code":200,"msg":"fixture","data":data})).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = Router::new()
        .route("/api/pawoffice/messages", post(handle))
        .with_state(shared.clone());
    let serving = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.office.as_mut().unwrap().base_url = base;
    cfg.tools = vec!["read_file".into(), "write_file".into()];
    cfg.approval_timeout_secs = 3;
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"产物.txt","content":"approved-output"}),
            text: "请求写入".into(),
        },
        MockMode::Fixed("done".into()),
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"denied.txt","content":"must not write"}),
            text: "request denied".into(),
        },
        MockMode::Fixed("denied".into()),
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"wrong-scope.txt","content":"must not write"}),
            text: "request invalid".into(),
        },
        MockMode::Fixed("expired".into()),
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"custom-bypass.txt","content":"must not write"}),
            text: "request custom injection".into(),
        },
        MockMode::Fixed("custom rejected".into()),
    ]));
    let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "ui-test".into(),
    };
    let b = host.open_session(scope.clone()).await.unwrap();
    let r = host
        .submit_run(&scope, &b.session_id, "write report".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !shared.lock().await.decisions.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!cfg.roots["r"].join("产物.txt").exists());
    tokio::time::timeout(Duration::from_secs(8), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cfg.roots["r"].join("产物.txt")).unwrap(),
        "approved-output"
    );
    assert_eq!(
        host.get_run(&scope, &r.run_id).await.unwrap().state,
        RunState::Completed
    );
    let decisions = shared.lock().await.decisions.clone();
    let approvals = host.snapshot_and_subscribe(&scope).await.unwrap().0;
    assert!(approvals.pending_approvals.is_empty());
    assert_eq!(decisions.len(), 1);
    assert_ne!(decisions[0]["decision"]["id"], r.run_id);
    async fn action(shared: &Arc<Mutex<Fixture>>, id: &str, kind: &str, extra: Value) -> Value {
        let mut a = json!({"id":id,"teamId":"office-1","conversationId":"ui-test","action":kind});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        shared.lock().await.actions.push_back(a);
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(r) = shared
                    .lock()
                    .await
                    .responses
                    .iter()
                    .find(|r| r["interactionId"] == id)
                {
                    return r.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    let sessions_before = host.snapshot_and_subscribe(&scope).await.unwrap().0;
    let files_before = std::fs::read_dir(&cfg.roots["r"]).unwrap().count();
    let member_create = action(
        &shared,
        "create-member-unsupported",
        "employee.create",
        json!({"name":"新成员","role":"","sceneActorId":"file-agent"}),
    )
    .await;
    assert_eq!(member_create["presentation"]["target"], "result");
    assert_eq!(member_create["presentation"]["success"], false);
    assert!(member_create["presentation"]["message"]
        .as_str()
        .unwrap()
        .contains("固定六个可信角色"));
    let sessions_after = host.snapshot_and_subscribe(&scope).await.unwrap().0;
    assert_eq!(
        serde_json::to_value(&sessions_before).unwrap(),
        serde_json::to_value(&sessions_after).unwrap()
    );
    assert_eq!(
        std::fs::read_dir(&cfg.roots["r"]).unwrap().count(),
        files_before
    );
    let cross_scope = action(
        &shared,
        "create-member-cross-scope",
        "employee.create",
        json!({"name":"新成员","role":"","sceneActorId":"file-agent","teamId":"not-authorized"}),
    )
    .await;
    assert_eq!(cross_scope["presentation"]["success"], false);
    assert!(cross_scope["presentation"]["message"]
        .as_str()
        .unwrap()
        .contains("scope_denied"));
    shared.lock().await.expire_delete_reply = true;
    let expired = action(
        &shared,
        "create-member-expired",
        "employee.create",
        json!({"name":"新成员","sceneActorId":"file-agent"}),
    )
    .await;
    assert_eq!(expired["presentation"]["success"], false);
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            let db=rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
            let status:String=db.query_row("SELECT status FROM host_office_actions WHERE action_id='create-member-expired'",[],|r|r.get(0)).unwrap();
            if status=="unconfirmed"{break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    shared.lock().await.expire_delete_reply = false;
    assert_eq!(
        shared
            .lock()
            .await
            .responses
            .iter()
            .filter(|r| r["interactionId"] == "create-member-expired")
            .count(),
        1
    );
    assert_eq!(
        serde_json::to_value(host.snapshot_and_subscribe(&scope).await.unwrap().0).unwrap(),
        serde_json::to_value(&sessions_before).unwrap()
    );
    let mut inspected = std::collections::BTreeMap::new();
    for (employee, actor, name) in [
        ("assistant", "office-lead", "王明"),
        ("command", "code-agent", "李研"),
        ("files", "file-agent", "周理"),
        ("draft", "app-agent", "陈书"),
        ("review", "review-agent", "刘市"),
        ("search", "data-agent", "赵审"),
    ] {
        let actor_reply = action(
            &shared,
            &format!("actor-{actor}"),
            "actor.inspect",
            json!({"actorId":actor}),
        )
        .await;
        let member_reply = action(
            &shared,
            &format!("member-{employee}"),
            "employee.inspect",
            json!({"employeeId":employee}),
        )
        .await;
        let a = &actor_reply["presentation"]["content"];
        let b = &member_reply["presentation"]["content"];
        assert_eq!(a, b, "同一人物和左侧成员必须对应相同目标");
        assert_eq!(a["title"], format!("{name} · 成员详情"));
        assert!(!a["fields"].as_array().unwrap().is_empty());
        inspected.insert(employee, a.clone());
        let list = action(
            &shared,
            &format!("history-{actor}"),
            "dialog.action",
            json!({"interactionId":format!("actor-{actor}"),"optionId":"history"}),
        )
        .await;
        assert_eq!(list["presentation"]["content"]["title"], "历史");
        let back = action(
            &shared,
            &format!("back-{actor}"),
            "dialog.action",
            json!({"interactionId":format!("history-{actor}"),"optionId":"back"}),
        )
        .await;
        assert_eq!(
            back["presentation"]["content"]["title"],
            format!("{name} · 成员详情"),
            "返回不能丢失目标身份"
        );
    }
    assert_ne!(
        inspected["assistant"]["fields"],
        inspected["draft"]["fields"]
    );
    assert!(inspected["assistant"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["label"] == "当前状态"
            && field["value"] == "本轮运行已完成，业务进度以任务与白板为准"));
    let unknown = action(
        &shared,
        "unknown-actor",
        "actor.inspect",
        json!({"actorId":"not-a-real-person"}),
    )
    .await;
    assert_eq!(unknown["presentation"]["success"], false);
    let menu = action(
        &shared,
        "inspect-1",
        "employee.inspect",
        json!({"employeeId":"assistant"}),
    )
    .await;
    assert_eq!(menu["presentation"]["target"], "dialog");
    assert_eq!(
        menu["presentation"]["content"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let tasks = action(
        &shared,
        "tasks-1",
        "dialog.action",
        json!({"interactionId":"inspect-1","optionId":"tasks"}),
    )
    .await;
    assert_eq!(tasks["presentation"]["content"]["title"], "任务进度与结果");
    assert!(!tasks["presentation"]["content"]["fields"]
        .as_array()
        .unwrap()
        .is_empty());
    let detail = action(
        &shared,
        "task-1",
        "dialog.action",
        json!({"interactionId":"tasks-1","optionId":"task-0"}),
    )
    .await;
    assert!(detail["presentation"]["content"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["label"] == "实际执行状态" && f["value"] == "执行已结束"));
    let task_back = action(
        &shared,
        "task-back",
        "dialog.action",
        json!({"interactionId":"task-1","optionId":"back"}),
    )
    .await;
    assert_eq!(
        task_back["presentation"]["content"]["title"],
        "任务进度与结果"
    );
    let files = action(
        &shared,
        "files-2",
        "dialog.action",
        json!({"interactionId":"inspect-1","optionId":"files"}),
    )
    .await;
    assert_eq!(files["presentation"]["content"]["title"], "真实文件产物");
    let file = action(
        &shared,
        "file-3",
        "dialog.action",
        json!({"interactionId":"files-2","optionId":"file-0"}),
    )
    .await;
    assert!(file.to_string().contains("approved-output"));
    let forged = action(
        &shared,
        "bad-4",
        "dialog.action",
        json!({"interactionId":"file-3","optionId":"../../etc/passwd"}),
    )
    .await;
    assert_eq!(forged["presentation"]["success"], false);
    for (i, kind) in ["task.start", "task.block", "task.complete", "task.assign"]
        .iter()
        .enumerate()
    {
        let mut extra = json!({"taskId":format!("run:{}",r.run_id)});
        if *kind == "task.assign" {
            extra["assigneeId"] = json!("assistant");
        }
        let unsupported = action(&shared, &format!("task-5-{i}"), kind, extra).await;
        assert_eq!(unsupported["presentation"]["success"], false);
        assert_eq!(
            host.get_run(&scope, &r.run_id).await.unwrap().state,
            RunState::Completed
        );
    }
    shared.lock().await.confirm = false;
    host.submit_run(&scope, &b.session_id, "reject second write".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), host.wait_idle())
        .await
        .unwrap();
    assert!(!cfg.roots["r"].join("denied.txt").exists());
    {
        let mut s = shared.lock().await;
        s.confirm = true;
        s.wrong_scope = true;
    }
    host.submit_run(
        &scope,
        &b.session_id,
        "cross scope response must not approve".into(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), host.wait_idle())
        .await
        .unwrap();
    assert!(!cfg.roots["r"].join("wrong-scope.txt").exists());
    {
        let mut s = shared.lock().await;
        s.wrong_scope = false;
        s.custom_text = true;
    }
    host.submit_run(
        &scope,
        &b.session_id,
        "customText must not authorize confirmation".into(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), host.wait_idle())
        .await
        .unwrap();
    assert!(!cfg.roots["r"].join("custom-bypass.txt").exists());
    shared.lock().await.custom_text = false;

    // 一次排队32个真实HTTP点击，超过处理并发16；每个ID只回应一次，不能无界派生任务。
    {
        let mut s = shared.lock().await;
        for i in 0..32 {
            s.actions.push_back(json!({"id":format!("pressure-{i}"),"teamId":"office-1","conversationId":"ui-test","action":"employee.inspect","employeeId":"assistant"}));
        }
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let n = shared
                .lock()
                .await
                .responses
                .iter()
                .filter(|r| {
                    r["interactionId"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("pressure-"))
                })
                .count();
            if n == 32 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    {
        let s = shared.lock().await;
        for i in 0..32 {
            let id = format!("pressure-{i}");
            let replies: Vec<_> = s
                .responses
                .iter()
                .filter(|r| r["interactionId"] == id)
                .collect();
            assert_eq!(replies.len(), 1);
            assert_eq!(
                replies[0]["presentation"]["content"]["template"],
                "detail-v1"
            );
        }
    }
    shared.lock().await.expire_delete_reply = true;
    let deleted = action(&shared, "delete-6", "conversation.delete", json!({})).await;
    assert_eq!(deleted["presentation"]["success"], true);
    assert!(
        shared.lock().await.snapshot.as_ref().unwrap()["conversations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(host.open_session(scope).await.is_err());
    server.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let (status, context): (String, String) = db
        .query_row(
            "SELECT status,context FROM host_office_actions WHERE action_id='delete-6'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), context.as_str()),
        ("deleted_ui_unconfirmed", "{}")
    );
    stop.cancel();
    serving.await.unwrap();
}

#[tokio::test]
async fn non_text_and_oversize_file_queries_reject_without_creating_data() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.office = None;
    std::fs::write(cfg.roots["r"].join("binary"), [0xff, 0x00, 0x81]).unwrap();
    std::fs::write(cfg.roots["r"].join("large"), vec![b'a'; 256 * 1024 + 1]).unwrap();
    let host = Host::start(cfg).await.unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "bounds".into(),
    };
    let b = host.open_session(scope.clone()).await.unwrap();
    for path in ["binary", "large", "../outside", "/etc/passwd"] {
        assert!(
            host.query(
                &scope,
                QueryRequest {
                    session_id: b.session_id.clone(),
                    kind: QueryKind::File { path: path.into() },
                    offset: 0,
                    limit: 5
                }
            )
            .await
            .is_err(),
            "必须拒绝{path}"
        );
    }
    host.shutdown().await.unwrap();
}

#[test]
fn reserved_creation_slots_must_be_unique_and_pre_authorized() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    assert!(cfg.office.as_ref().unwrap().creation_slots.is_empty());
    for slots in [
        vec!["other".into()],
        vec!["office-1".into(), "office-1".into()],
    ] {
        cfg.office.as_mut().unwrap().creation_slots = slots;
        assert!(cfg.validate().is_err());
    }
    cfg.office.as_mut().unwrap().creation_slots = vec!["office-1".into()];
    cfg.validate().unwrap();
}

#[test]
fn creation_http_recovers_410_serializes_allocations_and_restores_names() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "creation_http_fixture_child", "--nocapture"])
        .env("OPENPAW_CREATION_HTTP_CHILD", "1")
        .env("PAWOFFICE_TEST_KEY", "isolated-key")
        .status()
        .unwrap();
    assert!(result.success());
}
#[test]
fn creation_http_fixture_child() {
    if std::env::var("OPENPAW_CREATION_HTTP_CHILD").as_deref() != Ok("1") {
        return;
    }
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(creation_http_scenario());
}
async fn creation_http_scenario() {
    use axum::response::IntoResponse;
    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::Value;
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Arc,
        time::Duration,
    };
    use tokio::sync::Mutex;
    #[derive(Default)]
    struct Created {
        snapshots: BTreeMap<String, Value>,
        actions: VecDeque<Value>,
        responses: Vec<Value>,
        premature: bool,
        poll_failures: usize,
        lost_claims: usize,
        rejected_registrations: usize,
    }
    async fn handler(
        State(shared): State<Arc<Mutex<Created>>>,
        Json(r): Json<Value>,
    ) -> axum::response::Response {
        let mut s = shared.lock().await;
        let data = match r["type"].as_str().unwrap() {
            "office.capabilities" => {
                json!({"scene":{"protocolVersion":"2.0","actors":[{"id":"office-lead"},{"id":"file-agent"},{"id":"data-agent"}],"capabilities":[]},"ui":{"whiteboardSchema":{}},"requests":{},"responses":{"success":{},"error":{}}})
            }
            "office.read" | "offices.initialize" => {
                if let Some(items) = r["offices"].as_array() {
                    for item in items {
                        s.snapshots.insert(
                            item["workspace"]["id"].as_str().unwrap().into(),
                            item.clone(),
                        );
                    }
                }
                json!({"offices":s.snapshots.keys().map(|id|json!({"id":id})).collect::<Vec<_>>(),"actors":[],"supportedMessages":[],"sceneCommandProtocol":"2.0","ui":{}})
            }
            "office.snapshot" => {
                s.snapshots
                    .insert(r["teamId"].as_str().unwrap().into(), r["snapshot"].clone());
                json!({"teamId":r["teamId"],"seq":r["snapshot"]["seq"],"status":"applied"})
            }
            "ui.next" => {
                if s.poll_failures == 0 {
                    s.poll_failures += 1;
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"code":503,"msg":"controlled unavailable","data":{}})),
                    )
                        .into_response();
                }
                let a = s.actions.pop_front();
                if a.as_ref().is_some_and(|a| a["id"] == "claim-lost") {
                    s.lost_claims += 1;
                    // 真正在HTTP响应前消费动作再断链；无ACK协议不会重投递。
                    let body = axum::body::Body::from_stream(futures::stream::once(async {
                        Err::<axum::body::Bytes, _>(std::io::Error::other(
                            "controlled claimed response loss",
                        ))
                    }));
                    return axum::response::Response::new(body);
                }
                if a.as_ref().is_some_and(|a| a["id"] == "registration-fails") {
                    s.rejected_registrations += 1;
                }
                drop(s);
                if a.is_none() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                json!({"actions":a.into_iter().collect::<Vec<_>>()})
            }
            "ui.respond" => {
                s.responses.push(r.clone());
                if r["presentation"]["target"] == "office" {
                    let snapshot = &r["presentation"]["snapshot"];
                    let id = snapshot["workspace"]["id"].as_str().unwrap();
                    if s.snapshots.contains_key(id) {
                        s.premature = true;
                    }
                    if r["interactionId"] == "new-a" {
                        return (axum::http::StatusCode::GONE,Json(json!({"code":410,"msg":"expired","data":{"reasonCode":"UI_REQUEST_EXPIRED"}}))).into_response();
                    }
                    s.snapshots.insert(id.to_owned(), snapshot.clone());
                    if r["interactionId"] == "new-c" {
                        // 服务端已使用创建快照但确认失败；业务不能回滚或换槽重试。
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            Json(
                                json!({"code":503,"msg":"controlled lost confirmation","data":{}}),
                            ),
                        )
                            .into_response();
                    }
                }
                json!({"interactionId":r["interactionId"],"delivered":true})
            }
            _ => panic!("unexpected request {r}"),
        };
        Json(json!({"code":200,"msg":"fixture","data":data})).into_response()
    }
    let shared = Arc::new(Mutex::new(Created::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = Router::new()
        .route("/api/pawoffice/messages", post(handler))
        .with_state(shared.clone());
    let serving = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.slots.insert("reserved-a".into(), "r".into());
    cfg.slots.insert("reserved-b".into(), "r".into());
    cfg.slots.insert("reserved-c".into(), "r".into());
    cfg.office.as_mut().unwrap().base_url = base;
    cfg.office.as_mut().unwrap().creation_slots = vec![
        "reserved-a".into(),
        "reserved-b".into(),
        "reserved-c".into(),
    ];
    let host = Host::start(cfg.clone()).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!shared
        .lock()
        .await
        .snapshots
        .keys()
        .any(|s| s.starts_with("reserved")));
    let url = format!("http://{}/office/chat", server.address());
    let client = reqwest::Client::new();
    for kind in ["send", "history", "resume", "cancel"] {
        let mut request =
            json!({"version":"1.0","type":kind,"teamId":"reserved-a","conversationId":"guessed"});
        if kind == "send" {
            request["turnId"] = json!("before-create");
            request["messages"] = json!([{"role":"user","content":"不能预先建会话"}]);
        } else if kind != "history" {
            request["turnId"] = json!("before-create");
        }
        let response = client.post(&url).json(&request).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "{kind}");
    }
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let guesses: i64 = db
        .query_row(
            "SELECT count(*) FROM host_bindings WHERE conversation='guessed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(guesses, 0, "拒绝请求不能产生Session绑定");
    db.execute_batch("CREATE TRIGGER reject_action_registration BEFORE INSERT ON host_office_actions WHEN NEW.action_id='registration-fails' BEGIN SELECT RAISE(ABORT,'controlled action registration failure'); END;").unwrap();
    for id in ["claim-lost", "registration-fails"] {
        shared.lock().await.actions.push_back(json!({"id":id,"teamId":"","conversationId":"","action":"office.create","name":"不得创建"}));
    }
    for (id, name) in [
        ("new-a", "未确认办公室"),
        ("new-b", "正常办公室"),
        ("new-c", "确认丢失办公室"),
    ] {
        shared.lock().await.actions.push_back(
            json!({"id":id,"teamId":"","conversationId":"","action":"office.create","name":name}),
        );
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = shared.lock().await;
            if s.responses.len() >= 3 && s.snapshots.len() == 4 {
                break;
            }
            drop(s);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    {
        let s = shared.lock().await;
        assert!(!s.premature, "后台不能先于官方创建呈现初始化该槽");
        assert_eq!(
            (s.poll_failures, s.lost_claims, s.rejected_registrations),
            (1, 1, 1)
        );
        assert!(
            !s.responses
                .iter()
                .any(|r| ["claim-lost", "registration-fails"]
                    .iter()
                    .any(|id| r["interactionId"] == *id)),
            "未收到或未持久登记的动作不得执行业务"
        );
        assert_eq!(s.responses[0]["presentation"]["target"], "office");
        assert_eq!(s.responses[1]["presentation"]["target"], "office");
        assert_ne!(
            s.responses[0]["presentation"]["snapshot"]["workspace"]["id"],
            s.responses[1]["presentation"]["snapshot"]["workspace"]["id"]
        );
    }
    shared.lock().await.actions.push_back(json!({"id":"new-overflow","teamId":"","conversationId":"","action":"office.create","name":"容量满"}));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if shared.lock().await.responses.iter().any(|r| {
                r["interactionId"] == "new-overflow" && r["presentation"]["success"] == false
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    server.shutdown().await.unwrap();
    let state_of = |id: &str| {
        db.query_row(
            "SELECT status FROM host_office_actions WHERE action_id=?1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    assert_eq!(state_of("new-a"), "unconfirmed");
    assert_eq!(state_of("new-b"), "delivered");
    assert_eq!(state_of("new-c"), "unconfirmed");
    let failed: i64 = db.query_row("SELECT count(*) FROM host_office_actions WHERE action_id IN ('claim-lost','registration-fails')", [], |r| r.get(0)).unwrap();
    assert_eq!(failed, 0, "断链或登记失败不得留下伪造已领取/完成记录");
    db.execute_batch("DROP TRIGGER reject_action_registration;")
        .unwrap();
    drop(db);
    let read_facts = || {
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let mut stmt = db
            .prepare("SELECT reference FROM host_events WHERE kind='office_created'")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str::<Value>(&r.unwrap()).unwrap())
            .collect::<Vec<_>>()
    };
    let facts = read_facts();
    assert_eq!(facts.len(), 3);
    for fact in &facts {
        assert!(cfg
            .home
            .join("sessions")
            .join(format!("{}.jsonl", fact["session_id"].as_str().unwrap()))
            .exists());
    }
    shared.lock().await.snapshots.clear();
    let replies_before_restart = shared.lock().await.responses.len();
    for id in ["new-a", "new-b", "new-c"] {
        shared.lock().await.actions.push_back(json!({"id":id,"teamId":"","conversationId":"","action":"office.create","name":"重复不得换槽"}));
    }
    let host = Host::start(cfg.clone()).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let s = shared.lock().await;
            if s.snapshots.len() == 4 && s.actions.is_empty() {
                break;
            }
            drop(s);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for fact in &facts {
        assert_eq!(
            shared.lock().await.snapshots[fact["slot_id"].as_str().unwrap()]["workspace"]["name"],
            fact["name"]
        );
    }
    assert_eq!(read_facts(), facts, "重启不能重复分配或重放创建");
    server.shutdown().await.unwrap();
    assert_eq!(
        shared.lock().await.responses.len(),
        replies_before_restart,
        "未知/失败确认和重复动作均不能自动重发UI或重复分配"
    );
    eprintln!("ui.next 503恢复、领取后真实断链、SQLite登记拒绝、410/503确认未知及重启重复投递均通过；三个槽/Session只分配一次，未执行模型");
    stop.cancel();
    serving.await.unwrap();
}
