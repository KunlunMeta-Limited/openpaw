//! 功能：验证 engine-host HTTP、SSE、认证、会话、审批和 Office 投影的线协议行为。
//! 输入输出边界：直接调用测试 Router，所有文件写入临时目录，不绑定生产网络端口。
//! 禁止职责：不得使用真实 provider 凭据或依赖外部 Gateway 可用性。
//! 关联：`TEST-HOST-HTTP-001`、`CHG-2026-0003`、`CHG-2026-0012`。
#![cfg(feature = "engine-host")]
use axum::body::Body;
use axum::http::StatusCode;
use openpaw_engine_host::{build_router, run_with_shutdown, AppState, HostConfig};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
mod support;
use support::{
    create_session, get_json, prompt_with_request_id, request, request_with_headers,
    response_bytes, response_json,
};
fn unused_loopback_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind temporary listener")
        .local_addr()
        .expect("temporary listener address")
        .port()
}
async fn wait_until_listening(address: SocketAddr) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if TcpStream::connect(address).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Host should start listening");
}
#[tokio::test]
async fn health_and_session_wire_contract() {
    let home = tempdir().expect("home");
    let workspace = tempdir().expect("workspace");
    let config = HostConfig {
        home: home.path().into(),
        workspace: workspace.path().into(),
        ..Default::default()
    };
    let app = build_router(AppState::new(config));
    assert_eq!(get_json(&app, "/session").await, serde_json::json!([]));
    let response = request(&app, "GET", "/global/health", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await,
        serde_json::json!({"healthy":true})
    );
    let response = request(&app, "POST", "/session", r#"{"title":"demo"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    let session = response_json(response).await;
    assert_eq!(session["title"], "demo");
    assert!(session["time"]["created"].is_number());
    let id = session["id"].as_str().unwrap();
    let listed = get_json(&app, "/session").await;
    for field in ["id", "slug", "title", "directory"] {
        assert_eq!(listed[0][field], session[field]);
    }
    assert_eq!(listed[0]["time"]["created"], session["time"]["created"]);
    assert_eq!(listed[0]["time"]["updated"], listed[0]["updatedAt"]);
    assert_eq!(listed[0]["time"]["created"], listed[0]["createdAt"]);
    assert!(get_json(&app, "/session/status").await.is_object());
    assert_eq!(
        get_json(&app, format!("/session/{id}/message")).await,
        serde_json::json!([])
    );
    assert_eq!(
        get_json(&app, format!("/session/{id}/turns")).await,
        serde_json::json!([])
    );
    let providers = get_json(&app, "/provider").await;
    assert_eq!(providers["connected"].as_array().unwrap().len(), 4);
    let response = request(&app, "DELETE", format!("/session/{id}"), Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = request(&app, "GET", format!("/session/{id}"), Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
#[tokio::test]
async fn malformed_json_is_a_bad_request() {
    let home = tempdir().expect("home");
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    }));
    let response = request(&app, "POST", "/session", "{").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn basic_auth_is_required_when_configured() {
    let home = tempdir().expect("home");
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        username: Some("alice".into()),
        password: Some("secret".into()),
        ..Default::default()
    }));
    let response = request(&app, "GET", "/global/health", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = request_with_headers(
        &app,
        "GET",
        "/global/health",
        &[("authorization", "Basic YWxpY2U6c2VjcmV0")],
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
#[tokio::test]
async fn global_event_accepts_subscription_without_session() {
    let home = tempdir().expect("home");
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    }));
    let response = request(&app, "GET", "/global/event", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
}
#[tokio::test]
async fn active_turn_reservation_is_atomic() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    assert!(session.reserve_turn());
    assert!(!session.reserve_turn());
    session.release_turn();
    assert!(session.reserve_turn());
    session.release_turn();
}
#[tokio::test]
async fn reserved_turn_rollback_clears_busy_state() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    assert!(session.reserve_turn());
    session.rollback_reserved_turn();
    assert!(!session.is_active());
    assert!(session.reserve_turn());
    session.release_turn();
}
#[test]
fn partial_auth_configuration_is_rejected() {
    let config = HostConfig {
        username: Some("alice".into()),
        password: None,
        ..Default::default()
    };
    assert!(config.validate().is_err());
}
#[tokio::test]
async fn prompt_async_returns_204_and_unknown_session_404() {
    let home = tempdir().expect("home");
    let config = HostConfig {
        home: home.path().into(),
        ..Default::default()
    };
    let app = build_router(AppState::new(config));
    let id = create_session(&app).await;
    let response = request(
        &app,
        "POST",
        format!("/session/{id}/prompt_async"),
        r#"{"parts":[{"type":"text","text":"hi"},{"type":"image","text":"ignored"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let response = request(
        &app,
        "POST",
        "/session/missing/prompt_async",
        r#"{"parts":[{"type":"text","text":"hi"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = request(&app, "GET", "/api/health", Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
#[tokio::test]
async fn request_id_replay_is_stable_and_conflict_is_rejected_after_restart() {
    let home = tempdir().expect("home");
    let config = HostConfig {
        home: home.path().into(),
        provider: "echo".into(),
        office_url: None,
        ..Default::default()
    };
    let state = AppState::new(config.clone());
    let app = build_router(state.clone());
    let id = create_session(&app).await;
    let first = prompt_with_request_id(&app, &id, "stable-key", "hello").await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let turn_id = first.headers()["openpaw-turn-id"]
        .to_str()
        .unwrap()
        .to_string();
    let replay = prompt_with_request_id(&app, &id, "stable-key", "hello").await;
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert_eq!(replay.headers()["openpaw-turn-id"], turn_id);
    let conflict = prompt_with_request_id(&app, &id, "stable-key", "different").await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(conflict).await["code"],
        "idempotency_conflict"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.get(&id).await.unwrap().is_active() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("turn completion");
    drop(app);
    drop(state);
    let restarted = build_router(AppState::new(config));
    let replay = prompt_with_request_id(&restarted, &id, "stable-key", "hello").await;
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert_eq!(replay.headers()["openpaw-turn-id"], turn_id);
    let messages = get_json(&restarted, format!("/session/{id}/message")).await;
    assert_eq!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["info"]["role"] == "user")
            .count(),
        1
    );
}
#[tokio::test]
async fn corrupt_session_returns_typed_safe_error() {
    let home = tempdir().expect("home");
    let sessions = home.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    std::fs::write(sessions.join(format!("{id}.jsonl")), "{not-json}\n").unwrap();
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    }));
    let response = request(&app, "GET", format!("/session/{id}"), Body::empty()).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response_bytes(response).await;
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["code"], "session_corrupt");
    assert!(!String::from_utf8_lossy(&body).contains(home.path().to_string_lossy().as_ref()));
}
#[tokio::test]
async fn workspace_uses_header_then_query_then_cli_default() {
    let home = tempdir().expect("home");
    let default_workspace = tempdir().expect("default workspace");
    let header_workspace = tempdir().expect("header workspace");
    let body_workspace = tempdir().expect("body workspace");
    let query_workspace = std::env::current_dir()
        .expect("cwd")
        .join("query-workspace");
    let default_path = default_workspace.path().to_string_lossy().into_owned();
    let query_path = query_workspace.to_string_lossy().into_owned();
    let header_path = header_workspace.path().to_string_lossy().into_owned();
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        workspace: default_workspace.path().into(),
        ..Default::default()
    }));
    let body = serde_json::json!({"directory": body_workspace.path()}).to_string();
    let response = request(&app, "POST", "/session", body.clone()).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response_json(response).await["directory"], default_path);
    let response = request(
        &app,
        "POST",
        "/session?directory=query-workspace",
        body.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response_json(response).await["directory"], query_path);
    let response = request_with_headers(
        &app,
        "POST",
        "/session?directory=query-workspace",
        &[("x-opencode-directory", header_path.as_ref())],
        body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response_json(response).await["directory"], header_path);
}
#[tokio::test]
async fn wrong_basic_auth_credentials_are_rejected() {
    let home = tempdir().expect("home");
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        username: Some("alice".into()),
        password: Some("secret".into()),
        ..Default::default()
    }));
    let response = request_with_headers(
        &app,
        "GET",
        "/provider",
        &[("authorization", "Basic YWxpY2U6d3Jvbmc=")],
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn active_turn_requests_return_conflict_without_waiting_for_session_lock() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let app = build_router(state.clone());
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    let id = session.id.clone();
    session
        .active
        .store(true, std::sync::atomic::Ordering::Release);
    let guard = session.session.lock().await;
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        request(
            &app,
            "POST",
            format!("/session/{id}/prompt_async"),
            r#"{"parts":[{"type":"text","text":"第二轮"}]}"#,
        ),
    )
    .await
    .expect("busy request must not wait for the full turn");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(started.elapsed() < std::time::Duration::from_millis(450));
    let messages = session
        .message_cache
        .read()
        .unwrap_or_else(|error| error.into_inner());
    assert!(messages.is_empty(), "409 前不得新增 user 消息");
    drop(guard);
    session.release_turn();
}
#[tokio::test]
async fn turns_endpoint_uses_fixed_camel_case_contract() {
    let home = tempdir().expect("home");
    let app = build_router(AppState::new(HostConfig {
        home: home.path().into(),
        provider: "echo".into(),
        office_url: None,
        ..Default::default()
    }));
    let id = create_session(&app).await;
    let response = request(
        &app,
        "POST",
        format!("/session/{id}/message"),
        r#"{"text":"字段契约"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let turns = get_json(&app, format!("/session/{id}/turns")).await;
    let turn = &turns[0];
    assert_eq!(turn["state"], "completed");
    assert!(turn["turnID"].is_string());
    assert!(turn["userMessageID"].is_string());
    assert!(turn["startedAt"].is_number());
    assert!(turn["completedAt"].is_number());
    assert!(turn.get("turnId").is_none());
    assert!(turn.get("userMessageId").is_none());
    assert!(turn.get("error").is_none());

    let messages = get_json(&app, format!("/session/{id}/message")).await;
    assert!(messages.as_array().unwrap().iter().any(|message| {
        message["info"]["role"] == "assistant" && message["parts"][0]["text"] == "Echo: 字段契约"
    }));
}
#[tokio::test]
async fn locked_session_rejects_prompt_without_leaking_reservation() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let app = build_router(state.clone());
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    let id = session.id.clone();
    // provider/model 使用非阻塞快照；首状态写入拿不到 Session 锁时必须快速失败。
    let held = session.session.lock().await;
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        request(
            &app,
            "POST",
            format!("/session/{id}/prompt_async"),
            r#"{"parts":[{"type":"text","text":"取消测试"}]}"#,
        ),
    )
    .await
    .expect("locked session request must return promptly");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!session.is_active(), "失败接收不得遗留 busy 状态");
    assert!(session.message_cache.read().unwrap().is_empty());
    drop(held);
}
#[tokio::test]
async fn session_metadata_reads_use_snapshot_during_active_turn() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let app = build_router(state.clone());
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    let id = session.id.clone();
    session
        .active
        .store(true, std::sync::atomic::Ordering::Release);
    let guard = session.session.lock().await;
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        request(&app, "GET", format!("/session/{id}"), Body::empty()),
    )
    .await
    .expect("metadata must remain readable during a turn");
    assert_eq!(response.status(), StatusCode::OK);
    drop(guard);
    session.release_turn();
}
#[tokio::test]
async fn message_reads_snapshot_during_active_turn() {
    let home = tempdir().expect("home");
    let state = AppState::new(HostConfig {
        home: home.path().into(),
        ..Default::default()
    });
    let app = build_router(state.clone());
    let session = state
        .create_session(None, None, None, None)
        .await
        .expect("create session");
    let id = session.id.clone();
    session
        .message_cache
        .write()
        .unwrap()
        .push(openpaw_core::Message::user_text("快照消息"));
    session
        .active
        .store(true, std::sync::atomic::Ordering::Release);
    let guard = session.session.lock().await;
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        request(&app, "GET", format!("/session/{id}/message"), Body::empty()),
    )
    .await
    .expect("message snapshot must remain readable during a turn");
    assert_eq!(response.status(), StatusCode::OK);
    let messages = response_json(response).await;
    assert_eq!(messages[0]["info"]["role"], "user");
    drop(guard);
    session.release_turn();
}
#[tokio::test]
async fn real_listener_stops_accepting_after_injected_shutdown() {
    let home = tempdir().expect("home");
    let port = unused_loopback_port();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(run_with_shutdown(
        HostConfig {
            home: home.path().into(),
            port,
            ..Default::default()
        },
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    wait_until_listening(address).await;
    let mut connection = TcpStream::connect(address).await.expect("connect to Host");
    connection
        .write_all(b"GET /global/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .expect("write health request");
    let mut response = Vec::new();
    connection
        .read_to_end(&mut response)
        .await
        .expect("read health response");
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    shutdown_tx.send(()).expect("trigger shutdown");
    let mut late_connection_received_ok = false;
    if let Ok(Ok(mut late_connection)) =
        tokio::time::timeout(Duration::from_millis(250), TcpStream::connect(address)).await
    {
        let _ = late_connection
            .write_all(
                b"GET /global/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
            .await;
        let mut late_response = Vec::new();
        if let Ok(Ok(_)) = tokio::time::timeout(
            Duration::from_millis(500),
            late_connection.read_to_end(&mut late_response),
        )
        .await
        {
            late_connection_received_ok = late_response.starts_with(b"HTTP/1.1 200 OK");
        }
    }
    assert!(
        !late_connection_received_ok,
        "关闭开始后新连接不得获得成功 HTTP 响应"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("Host should stop within the shutdown deadline")
        .expect("server task");
    assert_eq!(result, Ok(()));
    let rebound = std::net::TcpListener::bind(address).expect("shutdown should release the port");
    drop(rebound);
}
#[tokio::test]
async fn incomplete_request_body_cannot_exceed_total_shutdown_deadline() {
    let home = tempdir().expect("home");
    let port = unused_loopback_port();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(run_with_shutdown(
        HostConfig {
            home: home.path().into(),
            port,
            ..Default::default()
        },
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    wait_until_listening(address).await;
    let mut incomplete = TcpStream::connect(address)
        .await
        .expect("connect incomplete request");
    incomplete
        .write_all(
            b"POST /session HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 1000000\r\n\r\n{",
        )
        .await
        .expect("write incomplete request");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    shutdown_tx.send(()).expect("trigger shutdown");
    let result = tokio::time::timeout(Duration::from_millis(5600), server)
        .await
        .expect("Host must enforce its own five second deadline")
        .expect("server task");
    let elapsed = started.elapsed();
    let error = result.expect_err("incomplete HTTP drain should hit hard deadline");
    assert!(error.contains("超过五秒"));
    assert!(elapsed >= Duration::from_millis(4500));
    assert!(elapsed <= Duration::from_millis(5500));
    drop(incomplete);
    assert!(TcpStream::connect(address).await.is_err());
}
