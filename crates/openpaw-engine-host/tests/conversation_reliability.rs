//! 功能：验证 engine-host 多会话调度、取消、失败回滚、重启恢复和持久化可靠性。
//! 输入输出边界：通过真实 axum Router 和测试 provider 发起请求，只写临时 home/workspace。
//! 禁止职责：不得访问真实模型、生产端口、生产 OpenPaw home 或用户工作区。
//! 关联：`TEST-HOST-RELIABILITY-001`、`CHG-2026-0012`。
#![cfg(feature = "engine-host")]
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use openpaw_engine_host::{build_router, AppState, HostConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tempfile::TempDir;
use tokio::sync::{mpsc, Mutex, MutexGuard, Semaphore};
mod support;
use support::{
    abort, create_session, create_session_with_provider, get_json, prompt, prompt_with_request_id,
    response_json, wait_for_terminal,
};
static PROVIDER_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
#[derive(Clone)]
struct FakeProviderState {
    release: Arc<Semaphore>,
    entered_tx: mpsc::UnboundedSender<String>,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}
struct FakeProvider {
    address: std::net::SocketAddr,
    release: Arc<Semaphore>,
    entered_rx: mpsc::UnboundedReceiver<String>,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}
impl FakeProvider {
    async fn start() -> Self {
        async fn completion(
            State(state): State<FakeProviderState>,
            Json(body): Json<serde_json::Value>,
        ) -> Response {
            state.calls.fetch_add(1, Ordering::AcqRel);
            state.requests.lock().unwrap().push(body.clone());
            if body["stream"] == false {
                assert!(body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("Summarize"));
                assert!(!body.to_string().contains("base64,"));
                return Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"old context summarized"},"finish_reason":"stop"}]})).into_response();
            }
            let prompt = body["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let _ = state.entered_tx.send(prompt.clone());
            if prompt.contains("unsafe-error") {
                return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":{
                    "message":"Bearer UPSTREAM_SENTINEL test-key /tmp/private", "request":"PRIVATE_BODY_SENTINEL"
                }}))).into_response();
            }
            if prompt.contains("rate-limit") {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({"error":{"message":"quota exceeded"}})),
                )
                    .into_response();
            }
            if prompt.contains("stream-fault") {
                return (
                    [("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                )
                    .into_response();
            }
            if prompt.contains("blocked-auth-fail") {
                let permit = state.release.acquire().await.expect("release semaphore");
                permit.forget();
                return (StatusCode::UNAUTHORIZED, "invalid upstream credential").into_response();
            }
            if prompt.contains("auth-fail") {
                return (StatusCode::UNAUTHORIZED, "invalid upstream credential").into_response();
            }
            if prompt.contains("overload") {
                return (StatusCode::SERVICE_UNAVAILABLE, "service unavailable").into_response();
            }
            let permit = state.release.acquire().await.expect("release semaphore");
            permit.forget();
            if body["model"] == "media-model"
                && !body["messages"].to_string().contains("\"role\":\"tool\"")
            {
                return ([("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"image-read\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"first.gif\\\"}\"}},{\"index\":1,\"id\":\"audio-read\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"second.wav\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").into_response();
            }
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }
        let (entered_tx, entered_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(FakeProviderState {
                release: release.clone(),
                entered_tx,
                calls: calls.clone(),
                requests: requests.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake provider listener");
        let address = listener.local_addr().expect("fake provider address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            address,
            release,
            entered_rx,
            calls,
            requests,
            server,
        }
    }
    async fn next_prompt(&mut self) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.entered_rx.recv())
            .await
            .expect("provider call timeout")
            .expect("provider call channel")
    }
}
impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}
struct ProviderEnvironment {
    old_key: Option<std::ffi::OsString>,
    old_url: Option<std::ffi::OsString>,
    old_caps: Option<std::ffi::OsString>,
}
impl ProviderEnvironment {
    fn set(address: std::net::SocketAddr) -> Self {
        let old_key = std::env::var_os("OPENAI_API_KEY");
        let old_url = std::env::var_os("OPENPAW_OPENAI_BASE_URL");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"));
        let old_caps = std::env::var_os("OPENPAW_MODEL_CAPABILITIES");
        Self {
            old_key,
            old_url,
            old_caps,
        }
    }
}
impl Drop for ProviderEnvironment {
    fn drop(&mut self) {
        match self.old_key.take() {
            Some(value) => std::env::set_var("OPENAI_API_KEY", value),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        match self.old_url.take() {
            Some(value) => std::env::set_var("OPENPAW_OPENAI_BASE_URL", value),
            None => std::env::remove_var("OPENPAW_OPENAI_BASE_URL"),
        }
        match self.old_caps.take() {
            Some(value) => std::env::set_var("OPENPAW_MODEL_CAPABILITIES", value),
            None => std::env::remove_var("OPENPAW_MODEL_CAPABILITIES"),
        }
    }
}

async fn lock_provider_environment() -> MutexGuard<'static, ()> {
    PROVIDER_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .await
}

async fn provider_app(
    home: &TempDir,
    limits: Option<(usize, usize)>,
) -> (FakeProvider, ProviderEnvironment, Arc<AppState>, Router) {
    let provider = FakeProvider::start().await;
    let environment = ProviderEnvironment::set(provider.address);
    let config = HostConfig {
        home: home.path().into(),
        provider: "openai".into(),
        office_url: None,
        ..HostConfig::default()
    };
    let state = match limits {
        Some((running, queued)) => {
            AppState::new_with_turn_limits(config, running, queued).expect("scheduler config")
        }
        None => AppState::new(config),
    };
    let app = build_router(state.clone());
    (provider, environment, state, app)
}
#[tokio::test]
async fn global_scheduler_is_fifo_bounded_and_cancellable() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, _state, app) = provider_app(&home, Some((2, 2))).await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(create_session(&app).await);
    }
    assert_eq!(
        prompt(&app, &ids[0], "first").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        prompt(&app, &ids[1], "second").await.status(),
        StatusCode::NO_CONTENT
    );
    let mut entered = vec![provider.next_prompt().await, provider.next_prompt().await];
    entered.sort();
    assert_eq!(entered, vec!["first", "second"]);
    assert_eq!(
        prompt(&app, &ids[2], "third").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        prompt(&app, &ids[3], "fourth").await.status(),
        StatusCode::NO_CONTENT
    );
    let status = get_json(&app, "/session/status").await;
    assert_eq!(status[&ids[2]]["type"], "queued");
    assert_eq!(status[&ids[2]]["queuePosition"], 1);
    assert!(status[&ids[2]]["turnID"].is_string());
    assert!(status[&ids[2]]["userMessageID"].is_string());
    assert_eq!(status[&ids[3]]["queuePosition"], 2);
    let full = prompt(&app, &ids[4], "fifth").await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response_json(full).await["code"], "queue_full");
    assert_eq!(
        get_json(&app, format!("/session/{}/message", ids[4])).await,
        serde_json::json!([])
    );
    assert_eq!(
        get_json(&app, format!("/session/{}/turns", ids[4])).await,
        serde_json::json!([])
    );
    let same_session = prompt(&app, &ids[0], "duplicate").await;
    assert_eq!(same_session.status(), StatusCode::CONFLICT);
    assert_eq!(
        get_json(&app, format!("/session/{}/message", ids[0]))
            .await
            .as_array()
            .expect("messages")
            .len(),
        1
    );
    let aborted = abort(&app, &ids[2]).await;
    assert_eq!(aborted.status(), StatusCode::OK);
    let cancelled = wait_for_terminal(&app, &ids[2]).await;
    assert_eq!(cancelled[0]["state"], "cancelled");
    assert_eq!(cancelled[0]["error"]["code"], "cancelled");
    assert_eq!(provider.calls.load(Ordering::Acquire), 2);
    provider.release.add_permits(1);
    assert_eq!(provider.next_prompt().await, "fourth");
    provider.release.add_permits(2);
    for id in [&ids[0], &ids[1], &ids[3]] {
        let turns = wait_for_terminal(&app, id).await;
        assert_eq!(turns[0]["state"], "completed");
    }
}
#[tokio::test]
async fn failed_turn_is_sanitized_persisted_and_does_not_block_the_next_turn() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, Some((1, 2))).await;
    let id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &id, "auth-fail secret prompt").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "auth-fail secret prompt");
    let failed = wait_for_terminal(&app, &id).await;
    assert_eq!(failed[0]["state"], "failed");
    assert_eq!(failed[0]["error"]["code"], "provider_auth_failed");
    assert_eq!(failed[0]["error"]["retryable"], false);
    assert!(!failed[0]["error"]["message"]
        .as_str()
        .expect("safe message")
        .contains("secret"));
    let messages = get_json(&app, format!("/session/{id}/message")).await;
    assert_eq!(messages.as_array().expect("messages").len(), 2);
    assert_eq!(messages[0]["info"]["role"], "user");
    assert_eq!(messages[1]["info"]["error"]["data"]["statusCode"], 401);
    provider.release.add_permits(1);
    assert_eq!(
        prompt(&app, &id, "next succeeds").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "next succeeds");
    let turns = wait_for_terminal(&app, &id).await;
    assert_eq!(turns.as_array().expect("turns").len(), 2);
    assert_eq!(turns[0]["state"], "failed");
    assert_eq!(turns[1]["state"], "completed");
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    }));
    let restored = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(restored, turns);
}
#[tokio::test]
async fn accepted_input_and_api_failures_have_ordered_durable_opencode_projections() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, None).await;
    let id = create_session(&app).await;
    let mut events = state.global_events.subscribe();
    assert_eq!(
        prompt(&app, &id, "held first input").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "held first input");
    let admitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    let part = admitted
        .iter()
        .find(|event| {
            event.event_type == "message.part.updated"
                && event.properties["part"]["text"] == "held first input"
        })
        .expect("input before provider release");
    let busy = get_json(&app, format!("/session/{id}/message")).await;
    assert_eq!(part.properties["part"], busy[0]["parts"][0]);
    assert!(!admitted
        .iter()
        .any(|event| event.properties["info"]["role"] == "assistant"));
    provider.release.add_permits(1);
    wait_for_terminal(&app, &id).await;
    while events.try_recv().is_ok() {}

    for (input, status, description) in [
        ("second overload", Some(503), "service unavailable"),
        ("rate-limit", Some(429), "quota exceeded"),
        ("auth-fail", Some(401), "invalid upstream credential"),
        ("stream-fault", None, "不完整"),
        ("unsafe-error", Some(503), "HTTP 503"),
    ] {
        assert_eq!(
            prompt(&app, &id, input).await.status(),
            StatusCode::NO_CONTENT
        );
        let turns = wait_for_terminal(&app, &id).await;
        assert_eq!(turns.as_array().unwrap().last().unwrap()["state"], "failed");
        let emitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let error_index = emitted
            .iter()
            .position(|event| event.event_type == "session.error")
            .expect("explicit execution error");
        assert_eq!(
            emitted
                .iter()
                .filter(|event| event.event_type == "session.error")
                .count(),
            1
        );
        let error = &emitted[error_index].properties["error"];
        assert_eq!(error["data"]["providerID"], "openai");
        assert_eq!(
            error["data"]["statusCode"].as_u64(),
            status.map(|value| value as u64)
        );
        assert!(
            error["data"]["message"]
                .as_str()
                .unwrap()
                .contains(description),
            "{error}"
        );
        assert!(emitted[..error_index]
            .last()
            .is_some_and(|event| event.event_type == "message.updated"
                && event.properties["info"]["error"] == *error));
        assert!(
            emitted
                .iter()
                .position(|event| event.event_type == "session.idle")
                .unwrap()
                > error_index
        );
        let history = get_json(&app, format!("/session/{id}/message")).await;
        let last = history.as_array().unwrap().last().unwrap();
        assert_eq!(last["info"]["error"], *error);
        let restored = build_router(AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..Default::default()
        }));
        assert_eq!(
            get_json(&restored, format!("/session/{id}/message")).await,
            history
        );
        let projected = serde_json::to_string(&history).unwrap();
        let journal =
            std::fs::read_to_string(home.path().join("sessions").join(format!("{id}.jsonl")))
                .unwrap();
        for forbidden in [
            "UPSTREAM_SENTINEL",
            "PRIVATE_BODY_SENTINEL",
            "test-key",
            "/tmp/private",
        ] {
            assert!(!projected.contains(forbidden) && !journal.contains(forbidden));
            assert!(!serde_json::to_string(
                &emitted
                    .iter()
                    .map(|event| &event.properties)
                    .collect::<Vec<_>>()
            )
            .unwrap()
            .contains(forbidden));
        }
        // 占位仅用于 wire：下一次 provider 的请求不得携带错误说明。
        while provider.entered_rx.try_recv().is_ok() {}
    }
    let history = get_json(&app, format!("/session/{id}/message")).await;
    let failed_id = history.as_array().unwrap().last().unwrap()["info"]["id"]
        .as_str()
        .unwrap();
    let fork = support::request(
        &app,
        "POST",
        format!("/session/{id}/fork"),
        serde_json::json!({"messageID":failed_id}).to_string(),
    )
    .await;
    assert_eq!(fork.status(), StatusCode::OK);
    let fork_id = response_json(fork).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let fork_history = get_json(&app, format!("/session/{fork_id}/message")).await;
    assert!(fork_history.as_array().unwrap().iter().all(|message| {
        message["info"]["error"].is_null() && message["info"]["id"] != failed_id
    }));
    assert!(fork_history.to_string().contains("unsafe-error"));
    let nonexistent = support::request(
        &app,
        "POST",
        format!("/session/{id}/revert"),
        serde_json::json!({"messageID":format!("{failed_id}_999")}).to_string(),
    )
    .await;
    assert_eq!(nonexistent.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        get_json(&app, format!("/session/{id}/message")).await,
        history
    );
    let revert = support::request(
        &app,
        "POST",
        format!("/session/{id}/revert"),
        serde_json::json!({"messageID":failed_id}).to_string(),
    )
    .await;
    assert_eq!(revert.status(), StatusCode::OK);
    provider.release.add_permits(1);
    assert_eq!(
        prompt(&app, &id, "replacement succeeds").await.status(),
        StatusCode::NO_CONTENT
    );
    wait_for_terminal(&app, &id).await;
    let replacement_events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(replacement_events
        .iter()
        .any(|event| event.event_type == "message.removed"
            && event.properties["messageID"] == failed_id));
    let replaced = get_json(&app, format!("/session/{id}/message")).await;
    assert!(!replaced
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["info"]["id"] == failed_id));
    let restored = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..Default::default()
    }));
    assert_eq!(
        get_json(&restored, format!("/session/{id}/message")).await,
        replaced
    );
    for request in provider.requests.lock().unwrap().iter() {
        let context = request["messages"].to_string();
        assert!(
            !context.contains("HTTP 503")
                && !context.contains("service unavailable")
                && !context.contains("quota exceeded"),
            "display errors must not enter model context"
        );
    }
}

#[tokio::test]
async fn second_turn_overload_is_bounded_and_third_turn_recovers() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, _state, app) = provider_app(&home, Some((1, 2))).await;
    let id = create_session(&app).await;

    provider.release.add_permits(1);
    assert_eq!(
        prompt(&app, &id, "first succeeds").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "first succeeds");
    assert_eq!(wait_for_terminal(&app, &id).await[0]["state"], "completed");

    assert_eq!(
        prompt(&app, &id, "second overload").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "second overload");
    assert_eq!(provider.next_prompt().await, "second overload");
    let failed = wait_for_terminal(&app, &id).await;
    assert_eq!(failed[1]["error"]["code"], "provider_overloaded");
    assert_eq!(provider.calls.load(Ordering::Acquire), 3);

    provider.release.add_permits(1);
    assert_eq!(
        prompt(&app, &id, "third succeeds").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "third succeeds");
    let turns = wait_for_terminal(&app, &id).await;
    assert_eq!(turns.as_array().expect("turns").len(), 3);
    assert_eq!(turns[2]["state"], "completed");
}
#[tokio::test]
async fn restart_converts_every_hanging_turn_once() {
    let home = TempDir::new().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        provider: "openai".into(),
        office_url: None,
        ..HostConfig::default()
    });
    let app = build_router(state.clone());
    let id = create_session_with_provider(&app, "mock").await;
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("session file");
    for outcome in [
        serde_json::json!({
            "type":"turn_outcome",
            "turnID":"7_deadbeef",
            "userMessageID":"msg_user_7",
            "state":"running",
            "startedAt":1
        }),
        serde_json::json!({
            "type":"turn_outcome",
            "turnID":"8_deadbeef",
            "userMessageID":"msg_user_8",
            "state":"queued",
            "queuedAt":2
        }),
        serde_json::json!({
            "type":"turn_outcome",
            "turnID":"9_deadbeef",
            "userMessageID":"msg_user_9",
            "state":"completed",
            "startedAt":3,
            "completedAt":4
        }),
    ] {
        writeln!(file, "{outcome}").expect("append turn outcome");
    }
    file.sync_all().expect("sync hanging turn");
    drop(file);
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    }));
    let turns = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(turns.as_array().unwrap().len(), 3);
    for index in [0, 1] {
        assert_eq!(turns[index]["state"], "failed");
        assert_eq!(turns[index]["error"]["code"], "host_restarted");
        assert!(turns[index]["completedAt"].is_number());
    }
    assert_eq!(turns[2]["state"], "completed");
    drop(restarted);
    let second_restart = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    }));
    assert_eq!(
        get_json(&second_restart, format!("/session/{id}/turns")).await,
        turns
    );
}

#[tokio::test]
async fn persisted_request_replay_does_not_require_provider_credentials() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, _state, app) = provider_app(&home, None).await;
    let id = create_session(&app).await;

    let first =
        prompt_with_request_id(&app, &id, "replay-before-provider", "blocked-auth-fail").await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let turn_id = first.headers()["openpaw-turn-id"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(provider.next_prompt().await, "blocked-auth-fail");

    // 已接收事实已写入 session；即使 Provider 凭据在重试时不可用，也应返回同一回合身份。
    std::env::remove_var("OPENAI_API_KEY");
    let replay =
        prompt_with_request_id(&app, &id, "replay-before-provider", "blocked-auth-fail").await;
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert_eq!(replay.headers()["openpaw-turn-id"], turn_id);

    provider.release.add_permits(1);
    wait_for_terminal(&app, &id).await;
}

#[tokio::test]
async fn graceful_shutdown_stops_admission_and_harvests_registered_turns() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, None).await;
    let mut events = state.global_events.subscribe();
    let running_id = create_session(&app).await;
    let rejected_id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &running_id, "shutdown-blocked").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "shutdown-blocked");
    let shutdown_state = state.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_state
            .shutdown_gracefully(std::time::Duration::from_secs(2))
            .await
    });
    tokio::task::yield_now().await;
    let rejected = prompt(&app, &rejected_id, "must-not-start").await;
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response_json(rejected).await["code"], "host_shutting_down");
    assert_eq!(
        get_json(&app, format!("/session/{rejected_id}/message")).await,
        serde_json::json!([])
    );
    assert!(shutdown.await.expect("shutdown task"));
    let turns = wait_for_terminal(&app, &running_id).await;
    assert_eq!(turns[0]["state"], "cancelled");
    assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        event.event_type == "session.error" && event.properties["sessionID"] == running_id
    }));
    let history = get_json(&app, format!("/session/{running_id}/message")).await;
    assert!(history
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["info"]["error"].is_null()));
}

#[tokio::test]
async fn forced_shutdown_reports_success_after_registered_tasks_are_harvested() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, None).await;
    let id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &id, "force-harvest").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "force-harvest");

    assert!(state.shutdown_gracefully(std::time::Duration::ZERO).await);
}

#[tokio::test]
async fn turn_accept_is_atomic_and_torn_tail_is_repaired_permanently() {
    let _env_guard = lock_provider_environment().await;
    let mut provider = FakeProvider::start().await;
    let _environment = ProviderEnvironment::set(provider.address);
    let home = TempDir::new().expect("home");
    let config = HostConfig {
        home: home.path().into(),
        provider: "openai".into(),
        office_url: None,
        ..HostConfig::default()
    };
    let state = AppState::new(config.clone());
    let app = build_router(state.clone());
    let id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &id, "ordered persistence").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "ordered persistence");
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect();
    let accepts: Vec<_> = lines
        .iter()
        .filter(|event| event["type"] == "turn_accept_v1")
        .collect();
    assert_eq!(accepts.len(), 1);
    assert_eq!(accepts[0]["message"]["role"], "user");
    assert_eq!(
        accepts[0]["message"]["content"][0]["text"],
        "ordered persistence"
    );
    assert!(matches!(
        accepts[0]["outcome"]["state"].as_str(),
        Some("queued" | "running")
    ));
    assert!(!lines
        .iter()
        .any(|event| event["type"] == "message" && event["role"] == "user"));
    provider.release.add_permits(1);
    wait_for_terminal(&app, &id).await;
    let messages = get_json(&app, format!("/session/{id}/message")).await;
    drop(app);
    drop(state);
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("session file");
    write!(file, "{{\"type\":\"mess").expect("torn tail");
    file.sync_all().expect("sync torn tail");
    drop(file);
    let restarted_state = AppState::new(config.clone());
    let restarted = build_router(restarted_state.clone());
    assert_eq!(
        get_json(&restarted, format!("/session/{id}/message")).await,
        messages
    );
    drop(restarted);
    drop(restarted_state);
    let repaired = std::fs::read_to_string(&path).expect("repaired session file");
    assert!(repaired
        .lines()
        .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
    let second_restart = build_router(AppState::new(config));
    assert_eq!(
        get_json(&second_restart, format!("/session/{id}/message")).await,
        messages
    );
}
#[tokio::test]
async fn restart_ignores_uncommitted_generated_messages() {
    let home = TempDir::new().expect("home");
    let config = HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    };
    let app = build_router(AppState::new(config.clone()));
    let id = create_session_with_provider(&app, "mock").await;
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    drop(app);
    use std::io::Write;
    let turn_id = "41_deadbeef";
    let user_message_id = "msg_user_41";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("session file");
    for event in [
        serde_json::json!({
            "type":"turn_accept_v1",
            "message":{
                "role":"user",
                "content":[{"type":"text","text":"atomic user"}],
                "uuid":user_message_id
            },
            "outcome":{
                "turnID":turn_id,
                "userMessageID":user_message_id,
                "state":"running",
                "startedAt":1
            }
        }),
        serde_json::json!({
            "type":"turn_finish_prepare_v1",
            "turnID":turn_id
        }),
        serde_json::json!({
            "type":"message",
            "role":"assistant",
            "content":[{"type":"text","text":"must stay invisible"}],
            "uuid":"msg_assistant_uncommitted"
        }),
    ] {
        writeln!(file, "{event}").expect("append recovery fixture");
    }
    file.sync_all().expect("sync recovery fixture");
    drop(file);
    let restarted = build_router(AppState::new(config));
    let messages = get_json(&restarted, format!("/session/{id}/message")).await;
    assert_eq!(messages.as_array().expect("messages").len(), 2);
    assert_eq!(messages[0]["info"]["role"], "user");
    assert_eq!(messages[0]["parts"][0]["text"], "atomic user");
    assert_eq!(messages[1]["parts"], serde_json::json!([]));
    assert_eq!(
        messages[1]["info"]["error"]["data"]["code"],
        "host_restarted"
    );
    assert!(
        !messages.to_string().contains("must stay invisible"),
        "uncommitted assistant remains excluded"
    );
    let turns = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(turns[0]["state"], "failed");
    assert_eq!(turns[0]["error"]["code"], "host_restarted");
}
#[tokio::test]
async fn restart_restores_committed_generated_messages_and_terminal() {
    let home = TempDir::new().expect("home");
    let config = HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    };
    let app = build_router(AppState::new(config.clone()));
    let id = create_session_with_provider(&app, "mock").await;
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    drop(app);
    use std::io::Write;
    let turn_id = "42_deadbeef";
    let user_message_id = "msg_user_42";
    let assistant_message_id = "msg_assistant_42";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("session file");
    for event in [
        serde_json::json!({
            "type":"turn_accept_v1",
            "message":{
                "role":"user",
                "content":[{"type":"text","text":"committed user"}],
                "uuid":user_message_id
            },
            "outcome":{
                "turnID":turn_id,
                "userMessageID":user_message_id,
                "state":"running",
                "startedAt":1
            }
        }),
        serde_json::json!({
            "type":"turn_finish_prepare_v1",
            "turnID":turn_id
        }),
        serde_json::json!({
            "type":"message",
            "role":"assistant",
            "content":[{"type":"text","text":"committed assistant"}],
            "uuid":assistant_message_id
        }),
        serde_json::json!({
            "type":"turn_finish_commit_v1",
            "turnID":turn_id,
            "generatedMessageIDs":[assistant_message_id],
            "terminal":{
                "turnID":turn_id,
                "userMessageID":user_message_id,
                "state":"completed",
                "startedAt":1,
                "completedAt":2
            }
        }),
    ] {
        writeln!(file, "{event}").expect("append recovery fixture");
    }
    file.sync_all().expect("sync recovery fixture");
    drop(file);
    let restarted = build_router(AppState::new(config));
    let messages = get_json(&restarted, format!("/session/{id}/message")).await;
    assert_eq!(messages.as_array().expect("messages").len(), 2);
    assert_eq!(messages[0]["info"]["role"], "user");
    assert_eq!(messages[1]["info"]["role"], "assistant");
    assert_eq!(messages[1]["parts"][0]["text"], "committed assistant");
    let turns = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(turns.as_array().expect("turns").len(), 1);
    assert_eq!(turns[0]["state"], "completed");
}
#[tokio::test]
async fn abort_and_terminal_race_never_persists_a_mixed_result() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, _state, app) = provider_app(&home, None).await;
    for index in 0..8 {
        let id = create_session(&app).await;
        let text = format!("race-{index}");
        assert_eq!(
            prompt(&app, &id, &text).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(provider.next_prompt().await, text);
        let abort_app = app.clone();
        let abort_id = id.clone();
        let abort = tokio::spawn(async move { abort(&abort_app, &abort_id).await });
        tokio::task::yield_now().await;
        provider.release.add_permits(1);
        assert_eq!(abort.await.expect("abort task").status(), StatusCode::OK);
        let turns = wait_for_terminal(&app, &id).await;
        let state = turns[0]["state"].as_str().expect("terminal state");
        let messages = get_json(&app, format!("/session/{id}/message")).await;
        let assistant_count = messages
            .as_array()
            .expect("messages")
            .iter()
            .filter(|message| message["info"]["role"] == "assistant")
            .count();
        match state {
            "cancelled" => assert_eq!(assistant_count, 0),
            "completed" => assert_eq!(assistant_count, 1),
            other => panic!("unexpected terminal state: {other}"),
        }
    }
}
#[tokio::test]
async fn running_state_persistence_failure_closes_turn_and_releases_capacity() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, Some((1, 1))).await;
    let running_id = create_session(&app).await;
    let queued_id = create_session(&app).await;
    let recovery_id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &running_id, "holds-capacity").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "holds-capacity");
    assert_eq!(
        prompt(&app, &queued_id, "running-persist-fails")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let queued_handle = state.get(&queued_id).await.expect("queued handle");
    queued_handle
        .writer
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    provider.release.add_permits(1);
    let failed = wait_for_terminal(&app, &queued_id).await;
    assert_eq!(failed[0]["state"], "failed");
    assert_eq!(failed[0]["error"]["code"], "persistence_failed");
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    let status = get_json(&app, "/session/status").await;
    assert!(status.get(&queued_id).is_none());
    assert_eq!(
        prompt(&app, &recovery_id, "capacity-recovered")
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "capacity-recovered");
    provider.release.add_permits(1);
    let recovered = wait_for_terminal(&app, &recovery_id).await;
    assert_eq!(recovered[0]["state"], "completed");
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    }));
    let restored = get_json(&restarted, format!("/session/{queued_id}/turns")).await;
    assert_eq!(restored[0]["state"], "failed");
    assert_eq!(restored[0]["error"]["code"], "persistence_failed");
}
#[tokio::test]
async fn terminal_persistence_failure_replaces_running_snapshot_in_turns() {
    let _env_guard = lock_provider_environment().await;
    let home = TempDir::new().expect("home");
    let (mut provider, _environment, state, app) = provider_app(&home, Some((1, 1))).await;
    let id = create_session(&app).await;
    assert_eq!(
        prompt(&app, &id, "blocked-auth-fail").await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.next_prompt().await, "blocked-auth-fail");
    let handle = state.get(&id).await.expect("session handle");
    let active = get_json(&app, "/session/status").await;
    assert_eq!(active[&id]["type"], "busy");
    handle
        .writer
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    provider.release.add_permits(1);
    let turns = wait_for_terminal(&app, &id).await;
    assert_eq!(turns[0]["state"], "failed");
    assert_eq!(turns[0]["error"]["code"], "persistence_failed");
    assert!(turns[0]["completedAt"].is_number());
    let status = get_json(&app, "/session/status").await;
    assert!(status.get(&id).is_none());
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        office_url: None,
        ..HostConfig::default()
    }));
    let restored = get_json(&restarted, format!("/session/{id}/turns")).await;
    assert_eq!(restored[0]["state"], "failed");
    assert_eq!(restored[0]["error"]["code"], "persistence_failed");
}

#[tokio::test]
async fn media_only_titles_match_after_restart_with_or_without_helper() {
    use openpaw_core::InlineMedia;
    use support::request;
    let _guard = lock_provider_environment().await;
    let home = TempDir::new().unwrap();
    let (mut provider, _environment, state, app) = provider_app(&home, None).await;
    std::env::set_var(
        "OPENPAW_MODEL_CAPABILITIES",
        r#"{"openai":{"attachment-title":{"input":["text","image","audio"]}}}"#,
    );
    let image = InlineMedia::from_bytes(b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;", "image/gif", None).unwrap();
    let audio = InlineMedia::from_bytes(b"RIFF0000WAVEfixture", "audio/wav", None).unwrap();
    for (media, title) in [(&image, "图片附件"), (&audio, "音频附件")] {
        for helper in [false, true] {
            let id = support::create_session_with(
                &app,
                serde_json::json!({"provider":"openai","model":"attachment-title"}),
            )
            .await;
            let mut parts =
                vec![serde_json::json!({"type":"file","mime":media.mime,"url":media.data_url()})];
            if helper {
                parts.push(
                    serde_json::json!({"type":"text","text":"attachment helper","synthetic":true}),
                );
            }
            let response = request(
                &app,
                "POST",
                format!("/session/{id}/prompt_async"),
                serde_json::json!({"parts":parts}).to_string(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            provider.next_prompt().await;
            provider.release.add_permits(1);
            assert_eq!(wait_for_terminal(&app, &id).await[0]["state"], "completed");
            let restarted = build_router(AppState::new(state.config.clone()));
            for reader in [&app, &restarted] {
                assert_eq!(
                    get_json(reader, format!("/session/{id}")).await["title"],
                    title
                );
            }
        }
    }
}

#[tokio::test]
async fn inline_media_compaction_tools_restart_fork_revert_and_idempotency() {
    use openpaw_core::{InlineMedia, Message, SessionEvent};
    use support::{request, request_with_headers};
    let _guard = lock_provider_environment().await;
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let (mut provider, _environment, state, app) = provider_app(&home, None).await;
    std::env::set_var(
        "OPENPAW_MODEL_CAPABILITIES",
        r#"{"openai":{"media-model":{"input":["text","image","audio"],"context_window":90000}}}"#,
    );
    let image = InlineMedia::from_bytes(b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;", "image/gif", None).unwrap();
    let audio = InlineMedia::from_bytes(b"RIFF0000WAVEfixture", "audio/wav", None).unwrap();
    std::fs::write(workspace.path().join("first.gif"), image.bytes().unwrap()).unwrap();
    std::fs::write(workspace.path().join("second.wav"), audio.bytes().unwrap()).unwrap();
    let response = request_with_headers(
        &app,
        "POST",
        "/session",
        &[("x-opencode-directory", workspace.path().to_str().unwrap())],
        r#"{"provider":"openai","model":"media-model"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = response_json(response).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // 构造可重启的旧历史，迫使下一回合压缩；媒体原件必须仍能公开读回。
    let handle = state.get(&id).await.unwrap();
    let mut old = Message::user_text("x".repeat(80_000)).with_uuid("old-image");
    old.content.push(image.block());
    {
        let mut writer = handle.writer.lock().unwrap();
        for message in [
            old,
            Message::assistant_text("x".repeat(80_000)).with_uuid("old-answer"),
            Message::user_text("x".repeat(80_000)).with_uuid("old-question-2"),
            Message::assistant_text("x".repeat(80_000)).with_uuid("old-answer-2"),
        ] {
            writer
                .as_mut()
                .unwrap()
                .append(&SessionEvent::Message(message))
                .unwrap();
        }
    }
    drop(handle);
    drop(app);
    drop(state);
    let config = HostConfig {
        home: home.path().into(),
        provider: "openai".into(),
        office_url: None,
        ..Default::default()
    };
    let state = AppState::new(config.clone());
    let mut events = state.global_events.subscribe();
    let app = build_router(state.clone());
    let catalog = get_json(&app, "/provider").await;
    assert_eq!(
        catalog["all"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "openai")
            .unwrap()["models"]["media-model"]["capabilities"]["input"]["audio"],
        true
    );
    let part = |media: &InlineMedia| serde_json::json!({"type":"file","mime":media.mime,"url":media.data_url()});
    let body = serde_json::json!({"messageID":"media-input","parts":[part(&image),part(&audio),
        {"type":"text","text":"attachment helper","synthetic":true,"metadata":{"fixture":"presentation-only"}},
        {"type":"text","text":"分析附件"}
    ]});
    let uri = format!("/session/{id}/prompt_async");
    assert_eq!(
        request(&app, "POST", &uri, body.to_string()).await.status(),
        StatusCode::NO_CONTENT
    );
    provider.next_prompt().await;
    let immediate = get_json(&app, format!("/session/{id}/message/media-input")).await;
    assert_eq!(immediate["parts"][0]["url"], image.data_url());
    assert_eq!(immediate["parts"][1]["url"], audio.data_url());
    assert_eq!(immediate["parts"][2]["synthetic"], true);
    assert_eq!(
        immediate["parts"][2]["metadata"]["fixture"],
        "presentation-only"
    );
    assert_eq!(immediate["parts"][3]["text"], "分析附件");
    let mut input_events = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.event_type == "message.part.updated"
            && event.properties["part"]["messageID"] == "media-input"
        {
            input_events.push(event.properties["part"].clone());
        }
    }
    assert_eq!(input_events, immediate["parts"].as_array().unwrap().clone());
    provider.release.add_permits(2);
    let turns = wait_for_terminal(&app, &id).await;
    assert_eq!(turns[0]["state"], "completed", "{turns}");
    let history = get_json(&app, format!("/session/{id}/message")).await;
    assert_eq!(history.as_array().unwrap().len(), 7);
    assert_eq!(
        history[0]["parts"][0]["text"].as_str().unwrap().len(),
        80_000
    );
    let tool_parts: Vec<_> = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["parts"].as_array().unwrap())
        .filter(|part| part["type"] == "tool")
        .collect();
    assert_eq!(tool_parts.len(), 2);
    for part in &tool_parts {
        assert_eq!(
            part["attachments"][0]["mime"],
            if part["callID"] == "image-read" {
                "image/gif"
            } else {
                "audio/wav"
            }
        );
    }
    assert!(
        tool_parts
            .iter()
            .all(|part| part["state"]["status"] == "completed"
                && part["attachments"].as_array().unwrap().len() == 1),
        "{tool_parts:?}"
    );
    let handle = state.get(&id).await.unwrap();
    assert!(handle.session.lock().await.messages.len() < 8);
    drop(handle);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["model"], "media-model");
    assert_eq!(requests[0]["stream"], false);
    assert!(requests[1]["messages"].to_string().contains(&image.id));
    assert!(requests[1]["messages"].to_string().contains("input_audio"));
    assert!(requests[1]["messages"]
        .to_string()
        .contains("attachment helper"));
    assert!(!requests[1]["messages"]
        .to_string()
        .contains("presentation-only"));
    assert!(requests[2]["messages"].to_string().contains("audio-read"));
    let replay = request(&app, "POST", &uri, body.to_string()).await;
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    let mut conflict = body.clone();
    conflict["parts"][0]["filename"] = "different.gif".into();
    assert_eq!(
        request(&app, "POST", &uri, conflict.to_string())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    for (parts, status) in [
        (
            serde_json::json!([{"type":"file","mime":"image/gif","url":"https://invalid/image"}]),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!([
                part(&image),
                part(&image),
                part(&image),
                part(&image),
                part(&image)
            ]),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            request(
                &app,
                "POST",
                &uri,
                serde_json::json!({"parts":parts}).to_string()
            )
            .await
            .status(),
            status
        );
    }
    let oversized = format!("{{\"text\":\"{}\"}}", "x".repeat(32 * 1024 * 1024));
    assert_eq!(
        request(&app, "POST", &uri, oversized).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        get_json(&app, format!("/session/{id}/message")).await,
        history
    );
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(config));
    assert_eq!(
        get_json(&restarted, format!("/session/{id}/message")).await,
        history
    );
    assert_eq!(
        request(&restarted, "POST", &uri, body.to_string())
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(provider.calls.load(Ordering::Acquire), 3);
    let fork =
        response_json(request(&restarted, "POST", format!("/session/{id}/fork"), "{}").await).await;
    let forked = get_json(
        &restarted,
        format!("/session/{}/message", fork["id"].as_str().unwrap()),
    )
    .await;
    let fork_input = forked
        .as_array()
        .unwrap()
        .iter()
        .find(|message| {
            message["parts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|part| part["mime"] == "audio/wav")
        })
        .unwrap();
    assert_eq!(fork_input["parts"][1]["url"], audio.data_url());
    assert_eq!(fork_input["parts"][2]["synthetic"], true);
    assert_eq!(
        fork_input["parts"][2]["metadata"],
        immediate["parts"][2]["metadata"]
    );
    assert_eq!(
        fork_input["parts"][1]["attachmentID"],
        immediate["parts"][1]["attachmentID"]
    );
    assert_eq!(
        request(
            &restarted,
            "POST",
            format!("/session/{id}/revert"),
            r#"{"messageID":"media-input"}"#
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        get_json(&restarted, format!("/session/{id}")).await["revert"]["messageID"],
        "media-input"
    );
    // 暂存回退保留完整 REST 记录，模型/分支使用可见前缀。
    let visible_fork =
        response_json(request(&restarted, "POST", format!("/session/{id}/fork"), "{}").await).await;
    assert_eq!(
        get_json(
            &restarted,
            format!("/session/{}/message", visible_fork["id"].as_str().unwrap())
        )
        .await
        .as_array()
        .unwrap()
        .len(),
        4
    );
    assert_eq!(
        request(&restarted, "POST", format!("/session/{id}/unrevert"), "{}")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get_json(&restarted, format!("/session/{id}/message")).await,
        history
    );
}
