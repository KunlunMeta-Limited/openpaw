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
}
struct FakeProvider {
    address: std::net::SocketAddr,
    release: Arc<Semaphore>,
    entered_rx: mpsc::UnboundedReceiver<String>,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}
impl FakeProvider {
    async fn start() -> Self {
        async fn completion(
            State(state): State<FakeProviderState>,
            Json(body): Json<serde_json::Value>,
        ) -> Response {
            state.calls.fetch_add(1, Ordering::AcqRel);
            let prompt = body["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let _ = state.entered_tx.send(prompt.clone());
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
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }
        let (entered_tx, entered_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(FakeProviderState {
                release: release.clone(),
                entered_tx,
                calls: calls.clone(),
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
}
impl ProviderEnvironment {
    fn set(address: std::net::SocketAddr) -> Self {
        let old_key = std::env::var_os("OPENAI_API_KEY");
        let old_url = std::env::var_os("OPENPAW_OPENAI_BASE_URL");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"));
        Self { old_key, old_url }
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
    assert_eq!(messages.as_array().expect("messages").len(), 1);
    assert_eq!(messages[0]["info"]["role"], "user");
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
    assert_eq!(messages.as_array().expect("messages").len(), 1);
    assert_eq!(messages[0]["info"]["role"], "user");
    assert_eq!(messages[0]["parts"][0]["text"], "atomic user");
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
