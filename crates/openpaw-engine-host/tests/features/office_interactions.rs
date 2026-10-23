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
                if s.expire_delete_reply && r["interactionId"] == "delete-6" {
                    return (axum::http::StatusCode::GONE,Json(json!({"code":410,"msg":"expired","data":{"reasonCode":"UI_REQUEST_EXPIRED"}}))).into_response();
                }
                json!({"interactionId":r["interactionId"],"delivered":true})
            }
            "decision.request" => {
                let confirm = s.confirm;
                let wrong_scope = s.wrong_scope;
                s.decisions.push(r.clone());
                drop(s);
                tokio::time::sleep(Duration::from_millis(400)).await;
                json!({"teamId":if wrong_scope {json!("other-office")}else{r["teamId"].clone()},"conversationId":r["conversationId"],"decisionId":r["decision"]["id"],"choiceId":if confirm{"confirm"}else{"reject"},"label":r["decision"][if confirm{"confirmLabel"}else{"rejectLabel"}]})
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
        6
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
