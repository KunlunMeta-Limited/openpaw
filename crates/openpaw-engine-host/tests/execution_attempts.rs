//! 功能：通过公开 HTTP 观察失败回合中实际发生的工具事实。
//! 边界：临时 home/workspace、受控外部模型；不使用生产凭据。
#![cfg(feature = "engine-host")]
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use openpaw_engine_host::{build_router, AppState, HostConfig};
use std::sync::{Arc, OnceLock};
#[allow(dead_code)]
mod support;
use support::{create_session, get_json, request};

#[tokio::test]
async fn completed_tool_survives_later_provider_failure_without_polluting_model_history() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    async fn provider(Json(body): Json<serde_json::Value>) -> axum::response::Response {
        if body["messages"].as_array().unwrap().last().unwrap()["content"] == "next clean" {
            assert!(
                !body["messages"].to_string().contains("durable-write"),
                "failed tool pairs must not reenter model context"
            );
            return ([("content-type","text/event-stream")],"data: {\"choices\":[{\"delta\":{\"content\":\"clean\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").into_response();
        }
        if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
        {
            return (StatusCode::UNAUTHORIZED, "fixture model failure").into_response();
        }
        ([ ("content-type", "text/event-stream") ], concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"saving actual file\",\"tool_calls\":[{\"index\":0,\"id\":\"durable-write\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"evidence.txt\\\",\\\"content\\\":\\\"saved-before-failure\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n")).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(provider)),
        )
        .await
    });
    std::env::set_var("OPENAI_API_KEY", "local-fixture");
    std::env::set_var("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"));
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let config = HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        provider: "openai".into(),
        office_url: None,
        ..Default::default()
    };
    let app = build_router(AppState::new(config.clone()));
    let id = create_session(&app).await;
    let response = request(
        &app,
        "POST",
        format!("/session/{id}/message"),
        r#"{"text":"write evidence"}"#,
    )
    .await;
    assert!(response.status().is_success());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("evidence.txt")).unwrap(),
        "saved-before-failure"
    );
    let history = get_json(&app, format!("/session/{id}/message")).await;
    let tool = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .find(|p| p["callID"] == "durable-write")
        .expect("actual tool result must survive failed turn");
    assert_eq!(tool["state"]["status"], "completed");
    assert_eq!(tool["state"]["input"]["content"], "saved-before-failure");
    assert_eq!(tool["state"]["disposition"], "completed");
    let read = openpaw_store::journal::read_repaired(
        &home.path().join("sessions").join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert_eq!(
        openpaw_store::transcript::replay(&id, &read.events)
            .unwrap()
            .messages
            .len(),
        1,
        "failed attempt is not committed model history"
    );
    let restarted = build_router(AppState::new(config));
    assert_eq!(
        get_json(&restarted, format!("/session/{id}/message")).await,
        history
    );
    assert_eq!(
        request(
            &restarted,
            "POST",
            format!("/session/{id}/message"),
            r#"{"text":"next clean"}"#
        )
        .await
        .status(),
        StatusCode::OK
    );
    server.abort();
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("OPENPAW_OPENAI_BASE_URL");
}

static ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct Fixture {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
    calls: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    old: Vec<(&'static str, Option<std::ffi::OsString>)>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}
impl Fixture {
    async fn new(manual: bool) -> Self {
        async fn provider(
            axum::extract::State(calls): axum::extract::State<
                Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
            >,
            Json(body): Json<serde_json::Value>,
        ) -> axum::response::Response {
            calls.lock().unwrap().push(body.clone());
            let messages = body["messages"].as_array().unwrap();
            let prompt = messages
                .iter()
                .rev()
                .find(|m| m["role"] == "user" && m["content"].is_string())
                .and_then(|m| m["content"].as_str())
                .unwrap_or_default();
            if prompt == "next clean" {
                return text("clean follow-up");
            }
            if prompt == "partial" {
                return ([("content-type","text/event-stream")], "data: {\"choices\":[{\"delta\":{\"content\":\"visible unfinished answer\"}}]}\n\n").into_response();
            }
            let has_tool = messages.last().is_some_and(|m| m["role"] == "tool");
            if has_tool {
                if prompt == "cancel" {
                    return std::future::pending().await;
                }
                return text("tool round finished");
            }
            let (name, input) = match prompt {
                "delegate-child" => (
                    "delegate_task",
                    serde_json::json!({"task":"partial","name":"recorder-child"}),
                ),
                "tool-error" => ("read_file", serde_json::json!({"path":"not-present.txt"})),
                "result-fail" | "cancel-tool" => (
                    "bash",
                    serde_json::json!({"command":"printf actual-side-effect > actual.txt; cat release"}),
                ),
                _ => (
                    "write_file",
                    serde_json::json!({"path":"actual.txt","content":"actual-side-effect"}),
                ),
            };
            let frame = serde_json::json!({"choices":[{"delta":{"content":"working","tool_calls":[{"index":0,"id":"attempt-call","function":{"name":name,"arguments":input.to_string()}}]},"finish_reason":"tool_calls"}]});
            (
                [("content-type", "text/event-stream")],
                format!("data: {frame}\n\ndata: [DONE]\n\n"),
            )
                .into_response()
        }
        fn text(value: &str) -> axum::response::Response {
            let frame =
                serde_json::json!({"choices":[{"delta":{"content":value},"finish_reason":"stop"}]});
            (
                [("content-type", "text/event-stream")],
                format!("data: {frame}\n\ndata: [DONE]\n\n"),
            )
                .into_response()
        }
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/v1/chat/completions", post(provider))
            .route("/actions", post(|| async { StatusCode::NO_CONTENT }))
            .with_state(calls.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let old = [
            "OPENAI_API_KEY",
            "OPENPAW_OPENAI_BASE_URL",
            "OPENPAW_ENGINE_PERMISSIONS",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        std::env::set_var("OPENAI_API_KEY", "fixture");
        std::env::set_var("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"));
        std::env::set_var(
            "OPENPAW_ENGINE_PERMISSIONS",
            if manual { "manual" } else { "auto" },
        );
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        assert!(std::process::Command::new("mkfifo")
            .arg(workspace.path().join("release"))
            .status()
            .unwrap()
            .success());
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            provider: "openai".into(),
            office_url: Some(format!("http://{address}/actions")),
            ..Default::default()
        });
        let app = build_router(state.clone());
        Self {
            home,
            workspace,
            state,
            app,
            calls,
            old,
            server,
        }
    }
    async fn start(&self, prompt: &str) -> String {
        let id = create_session(&self.app).await;
        assert_eq!(
            support::prompt(&self.app, &id, prompt).await.status(),
            StatusCode::NO_CONTENT
        );
        id
    }
    async fn pending(&self) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pending = get_json(&self.app, "/permission").await;
                if let Some(id) = pending[0]["id"].as_str() {
                    return id.to_string();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    async fn wait_file(&self, id: &str) {
        let wait = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !self.workspace.path().join("actual.txt").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await
            }
        })
        .await;
        assert!(
            wait.is_ok(),
            "file not produced: {}",
            get_json(&self.app, format!("/session/{id}/message")).await
        );
    }
    fn transcript(&self, id: &str) -> openpaw_store::transcript::Transcript {
        let read = openpaw_store::SessionReader::read(
            self.home
                .path()
                .join("sessions")
                .join(format!("{id}.jsonl")),
        )
        .unwrap();
        openpaw_store::transcript::replay(id, &read.events).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        for (key, value) in self.old.drain(..) {
            if let Some(value) = value {
                std::env::set_var(key, value)
            } else {
                std::env::remove_var(key)
            }
        }
    }
}
fn tool(history: &serde_json::Value) -> &serde_json::Value {
    history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .find(|p| p["callID"] == "attempt-call")
        .expect("saved real tool call")
}

#[tokio::test]
async fn partial_child_messages_remain_owned_by_child_in_public_history() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let fixture = Fixture::new(false).await;
    let parent = fixture.start("delegate-child").await;
    support::wait_for_terminal(&fixture.app, &parent).await;
    let sessions = get_json(&fixture.app, "/session").await;
    let child = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["parentID"] == parent)
        .expect("real child session")["id"]
        .as_str()
        .unwrap();
    let history = get_json(&fixture.app, format!("/session/{child}/message")).await;
    assert_eq!(history[1]["parts"][0]["text"], "visible unfinished answer");
    assert_eq!(history[1]["info"]["sessionID"], child);
    let parent_history = get_json(&fixture.app, format!("/session/{parent}/message")).await;
    assert_eq!(tool(&parent_history)["tool"], "delegate_task");
    assert!(!parent_history
        .to_string()
        .contains("visible unfinished answer"));
    assert_eq!(fixture.transcript(child).messages.len(), 1);
}

#[tokio::test]
async fn visible_truncated_reply_is_saved_as_incomplete_and_followup_has_valid_history() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let fixture = Fixture::new(false).await;
    let id = fixture.start("partial").await;
    assert_eq!(
        support::wait_for_terminal(&fixture.app, &id).await[0]["state"],
        "failed"
    );
    let history = get_json(&fixture.app, format!("/session/{id}/message")).await;
    assert_eq!(history[1]["parts"][0]["text"], "visible unfinished answer");
    assert_eq!(history[1]["info"]["execution"]["incomplete"], true);
    assert!(history[1]["info"]["time"].get("completed").is_none());
    assert_eq!(
        fixture.calls.lock().unwrap().len(),
        1,
        "visible output must not be retried"
    );
    assert_eq!(
        support::prompt(&fixture.app, &id, "next clean")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    support::wait_for_terminal(&fixture.app, &id).await;
    assert!(!fixture
        .calls
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .to_string()
        .contains("visible unfinished answer"));
    assert_eq!(fixture.transcript(&id).messages.len(), 3);
}

#[tokio::test]
async fn permission_denial_and_tool_error_remain_distinct_from_successful_turn() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let fixture = Fixture::new(true).await;
    let id = fixture.start("denied").await;
    let permission = fixture.pending().await;
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            format!("/permission/{permission}/reply"),
            r#"{"reply":"reject"}"#
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        support::wait_for_terminal(&fixture.app, &id).await[0]["state"],
        "completed"
    );
    assert!(!fixture.workspace.path().join("actual.txt").exists());
    assert_eq!(
        tool(&get_json(&fixture.app, format!("/session/{id}/message")).await)["state"]
            ["disposition"],
        "denied"
    );
    assert!(!fixture
        .transcript(&id)
        .execution_attempts
        .iter()
        .any(|r| r.kind == "tool_start"));
    drop(fixture);
    let fixture = Fixture::new(false).await;
    let id = fixture.start("tool-error").await;
    assert_eq!(
        support::wait_for_terminal(&fixture.app, &id).await[0]["state"],
        "completed"
    );
    let history = get_json(&fixture.app, format!("/session/{id}/message")).await;
    assert_eq!(tool(&history)["state"]["disposition"], "error");
    assert_eq!(
        history.as_array().unwrap().len(),
        3,
        "successful attempt is not duplicated"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn cancellation_preserves_completed_effect_and_unknown_inflight_effect() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    for prompt in ["cancel", "cancel-tool"] {
        let fixture = Fixture::new(false).await;
        let id = fixture.start(prompt).await;
        fixture.wait_file(&id).await;
        if prompt == "cancel" {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while fixture.calls.lock().unwrap().len() < 2 {
                    tokio::task::yield_now().await
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(
            support::abort(&fixture.app, &id).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            support::wait_for_terminal(&fixture.app, &id).await[0]["state"],
            "cancelled"
        );
        let history = get_json(&fixture.app, format!("/session/{id}/message")).await;
        assert_eq!(
            tool(&history)["state"]["disposition"],
            if prompt == "cancel" {
                "completed"
            } else {
                "unknown"
            }
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("actual.txt")).unwrap(),
            "actual-side-effect"
        );
        assert_eq!(fixture.transcript(&id).messages.len(), 1);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn start_persistence_failure_forbids_effect_and_result_failure_forbids_completed_event() {
    let _guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let fixture = Fixture::new(true).await;
    let id = fixture.start("start-fail").await;
    let permission = fixture.pending().await;
    fixture
        .state
        .get(&id)
        .await
        .unwrap()
        .writer
        .lock()
        .unwrap()
        .take();
    request(
        &fixture.app,
        "POST",
        format!("/permission/{permission}/reply"),
        r#"{"reply":"once"}"#,
    )
    .await;
    let turns = support::wait_for_terminal(&fixture.app, &id).await;
    assert_eq!(turns[0]["error"]["code"], "persistence_failed");
    assert!(!fixture.workspace.path().join("actual.txt").exists());
    assert!(!fixture
        .transcript(&id)
        .execution_attempts
        .iter()
        .any(|r| r.kind == "tool_start"));
    drop(fixture);
    let fixture = Fixture::new(false).await;
    let mut events = fixture.state.global_events.subscribe();
    let id = fixture.start("result-fail").await;
    fixture.wait_file(&id).await;
    fixture
        .state
        .get(&id)
        .await
        .unwrap()
        .writer
        .lock()
        .unwrap()
        .take();
    std::fs::write(fixture.workspace.path().join("release"), "").unwrap();
    assert_eq!(
        support::wait_for_terminal(&fixture.app, &id).await[0]["error"]["code"],
        "persistence_failed"
    );
    let history = get_json(&fixture.app, format!("/session/{id}/message")).await;
    assert_eq!(tool(&history)["state"]["disposition"], "unknown");
    assert!(!std::iter::from_fn(|| events.try_recv().ok())
        .any(|e| e.properties["part"]["callID"] == "attempt-call"
            && e.properties["part"]["state"]["status"] == "completed"));
    assert_eq!(fixture.transcript(&id).messages.len(), 1);
}
