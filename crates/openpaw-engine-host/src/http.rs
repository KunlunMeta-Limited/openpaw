//! 功能：实现 Host 的 axum HTTP、SSE 路由和 Basic Authentication。
//! 输入输出边界：解析 HTTP 请求并调用 `runtime`，将状态转换为协议 JSON 或 SSE。
//! 禁止承担：不直接创建 AgentLoop，不把 Office 投影失败转换为回合失败。
//! 关联：`API-HOST-*`、`EVT-HOST-*`、`REQ-B-HOST-002`。

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{
    sse::{Event, KeepAlive, Sse},
    IntoResponse, Response,
};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::protocol::wire::SseEnvelope;
use crate::protocol::{
    CreateSessionRequest, ForkSessionRequest, HostConfig, MessageRequest, OpenCodeEvent,
    PermissionReply, RequestAdmission, RevertSessionRequest, TurnState, UpdateSessionRequest,
};
use crate::runtime::{
    self, AcceptTurnResult, AppState, SessionHandle, SessionLoadError, SessionMutationError,
};

const OPENPAW_TURN_ID: &str = "openpaw-turn-id";
const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_FORCE_HARVEST_MARGIN: Duration = Duration::from_millis(100);

/// 功能：根据共享 Host 状态构建全部已登记路由和认证中间件。
/// 参数：`state` 持有 session、审批、全局事件和配置。
/// 返回：可供 axum server 使用的 Router。
/// 错误：路由构建本身不执行 I/O；请求错误在各 handler 返回 4xx/5xx。
/// 状态：不登记桌面会话，不更改会话历史。
/// 并发与取消：请求由共享 runtime 协调；路由构建无 I/O。
/// 权限：所有路由统一经过 Basic Authentication 中间件。
/// 关联：`API-HOST-*`、`REQ-B-HOST-003`。
pub fn build_router(state: Arc<AppState>) -> Router {
    state.bind_delegation_runtime();
    state.main_office.start(state.shutdown_token());
    let router = Router::new()
        .route("/global/health", get(health))
        .route("/session", get(list_sessions).post(create_session))
        .route(
            "/session/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route(
            "/session/{id}/message",
            get(messages)
                .post(message)
                .layer(axum::extract::DefaultBodyLimit::max(
                    openpaw_core::media::MAX_REQUEST_BYTES,
                )),
        )
        .route("/session/{id}/message/{message_id}", get(messages))
        .route("/session/{id}/turns", get(turns))
        .route("/session/{id}/command", post(no_content))
        .route("/session/{id}/shell", post(no_content))
        .route("/session/{id}/fork", post(fork_session))
        .route("/session/{id}/revert", post(revert_session))
        .route("/session/{id}/unrevert", post(unrevert_session))
        .route("/session/{id}/summarize", post(no_content))
        .route(
            "/session/{id}/prompt_async",
            post(prompt_async).layer(axum::extract::DefaultBodyLimit::max(
                openpaw_core::media::MAX_REQUEST_BYTES,
            )),
        )
        .route("/session/{id}/abort", post(abort))
        .route(
            "/session/{id}/ai-pc",
            axum::routing::put(register_ai_pc)
                .delete(unregister_ai_pc)
                .layer(axum::extract::DefaultBodyLimit::max(8192)),
        )
        .route("/session/{id}/todo", get(todo))
        .route("/session/status", get(status))
        .route("/event", get(global_event_stream))
        .route("/global/event", get(global_event_stream))
        .route("/api/event", get(global_event_stream))
        .route("/provider", get(provider))
        .route("/permission", get(permissions))
        .route("/permission/{id}/reply", post(permission_reply))
        .route("/config", get(empty_object))
        .route("/mcp", get(empty_object))
        .route("/command", get(empty_array))
        .route("/project", get(empty_array))
        .route("/agent", get(empty_array))
        .route("/find/file", get(empty_array))
        .route(
            "/api/chat",
            post(main_chat).layer(axum::extract::DefaultBodyLimit::max(500_000)),
        )
        .route("/office", get(main_proxy))
        .route("/office/", get(main_proxy))
        .route("/office/{*path}", get(main_proxy))
        .route("/assets/{*path}", get(main_proxy))
        .route("/farm-assets/{*path}", get(main_proxy))
        .route("/brand/{*path}", get(main_proxy))
        .route("/characters/{*path}", get(main_proxy))
        .route("/pawoffice-config.js", get(main_proxy))
        .route("/api/pawoffice/events", get(main_proxy))
        .route("/api/pawoffice/reply", post(main_proxy))
        .route("/api/pawoffice/ui", post(main_proxy))
        .with_state(state.clone());
    router.layer(middleware::from_fn_with_state(state, auth_middleware))
}

/// 功能：在当前线程创建 Tokio runtime 并阻塞运行 Host。
/// 参数：`config` 为已解析的 Host 配置。
/// 返回：正常停止返回 0；runtime、绑定或服务异常返回非零码。
/// 状态：成功绑定后输出一条监听地址就绪信息。
/// 关联：`REQ-B-HOST-001`。
pub fn run_blocking(config: HostConfig) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(error) => {
            eprintln!("错误：无法启动 engine host runtime：{error}");
            return 1;
        }
    };
    runtime.block_on(async move {
        match run(config).await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("错误：engine host 已停止：{error}");
                1
            }
        }
    })
}

/// 功能：校验配置、绑定回环端口并异步运行 Host HTTP 服务。
/// 参数：`config` 指定监听、home、工作区、provider、认证和 Office 地址。
/// 返回：服务异常退出时返回中文错误；正常运行期间保持 pending future。
/// 错误：非回环、半配置凭据、未知 provider 或端口绑定失败会返回错误。
/// 状态：默认入口不自行生成 OS 信号，服务持续运行直至 future 被外部取消。
/// 并发与取消：由调用方取消 future；单个 session 的回合取消由 runtime 管理。
/// 权限：沿用 HostConfig 的回环监听、Basic Authentication 和工作区边界。
/// 关联：`API-HOST-HEALTH-001`、`REQ-B-HOST-001`。
pub async fn run(config: HostConfig) -> Result<(), String> {
    run_with_shutdown_result(config, async {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| format!("无法注册或等待 Ctrl+C 关闭信号：{error}"))
    })
    .await
}

/// 功能：运行 Host，直至调用方提供的关闭信号完成，并执行受控关闭。
/// 参数：`config` 为 Host 配置；`shutdown` 为调用方注入的进程或服务停止信号。
/// 返回：监听、服务和受控关闭均完成时返回 `Ok(())`。
/// 错误：配置、绑定、HTTP 服务或关闭超时返回脱敏中文错误。
/// 状态：关闭开始后停止接收新回合，取消活动回合并保留已持久化 session。
/// 并发与取消：Axum 先停止新连接并排空请求，Host 后台任务在五秒内收敛，超时强制 abort。
/// 权限：不改变监听、认证、Provider 和工作区权限边界。
/// 关联：`CHG-2026-0015`、`API-HOST-SHUTDOWN-004`。
pub async fn run_with_shutdown<F>(config: HostConfig, shutdown: F) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    run_with_shutdown_result(config, async move {
        shutdown.await;
        Ok(())
    })
    .await
}

async fn run_with_shutdown_result<F>(config: HostConfig, shutdown: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>> + Send + 'static,
{
    config.validate()?;
    let listener = tokio::net::TcpListener::bind((config.hostname.as_str(), config.port))
        .await
        .map_err(|e| format!("无法绑定引擎主机 {}:{}：{e}", config.hostname, config.port))?;
    let address = listener
        .local_addr()
        .map_err(|e| format!("无法读取监听地址：{e}"))?;
    let state = AppState::new(config);
    let recovered = state.recover_orphaned_turns().await;
    if recovered > 0 {
        tracing::info!(recovered, "Host 启动时已闭合孤儿回合");
    }
    // Kunlun 与 OpenCode 兼容客户端依赖此稳定就绪标记发现动态端口。
    println!("opencode server listening on http://{address}");
    let listener_shutdown = CancellationToken::new();
    let server_shutdown = listener_shutdown.clone();
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, build_router(state.clone())).with_graceful_shutdown(async move {
            server_shutdown.cancelled_owned().await;
        }),
    );
    tokio::pin!(server);
    tokio::pin!(shutdown);

    let shutdown_result = tokio::select! {
        result = &mut server => {
            return result.map_err(|error| format!("引擎主机服务异常退出：{error}"));
        }
        result = &mut shutdown => result,
    };

    // 先关闭 listener，再并行排空 HTTP 连接和 Host 后台回合。外层绝对截止时间
    // 保证 Axum drain 与任务 registry 合计不会突破五秒发布门禁。
    listener_shutdown.cancel();
    let deadline = tokio::time::Instant::now() + HOST_SHUTDOWN_TIMEOUT;
    let runtime_budget = HOST_SHUTDOWN_TIMEOUT.saturating_sub(HOST_FORCE_HARVEST_MARGIN);
    let drain = async {
        let (server_result, tasks_clean) =
            tokio::join!(&mut server, state.shutdown_gracefully(runtime_budget),);
        server_result.map_err(|error| format!("引擎主机服务异常退出：{error}"))?;
        if tasks_clean {
            shutdown_result
        } else {
            Err("Host 受控关闭超过五秒，已强制终止剩余回合".into())
        }
    };
    match tokio::time::timeout_at(deadline, drain).await {
        Ok(result) => result,
        Err(_) => Err("Host 受控关闭超过五秒，已强制终止剩余连接或回合".into()),
    }
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected_user) = state.config.username.as_deref() else {
        return next.run(request).await;
    };
    let Some(expected_password) = state.config.password.as_deref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"鉴权配置无效：用户名和密码必须同时设置"})),
        )
            .into_response();
    };
    let valid = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|pair| pair == format!("{expected_user}:{expected_password}"))
        .unwrap_or(false);
    if valid {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"openpaw\"")],
            Json(serde_json::json!({"error":"未通过鉴权"})),
        )
            .into_response()
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"healthy":true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MainChatRequest {
    version: String,
    team_id: String,
    conversation_id: String,
    messages: Vec<MainChatMessage>,
}
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MainChatMessage {
    role: String,
    content: String,
}

async fn main_proxy(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let Some(mut url) = state.main_office.url.clone() else {
        return bad(StatusCode::SERVICE_UNAVAILABLE, "main gateway 未配置");
    };
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site")
    {
        return bad(StatusCode::FORBIDDEN, "请使用同源入口");
    }
    if let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let expected = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if ![format!("http://{expected}"), format!("https://{expected}")]
            .contains(&origin.to_string())
        {
            return bad(StatusCode::FORBIDDEN, "请使用同源入口");
        }
    }
    url.set_path(request.uri().path());
    url.set_query(request.uri().query());
    let mut upstream = state
        .main_office
        .client
        .request(request.method().clone(), url.clone())
        .header(header::ORIGIN, url.origin().ascii_serialization());
    for name in [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
    ] {
        if let Some(value) = request.headers().get(&name) {
            upstream = upstream.header(name, value);
        }
    }
    let bytes = match axum::body::to_bytes(request.into_body(), 524_288).await {
        Ok(bytes) => bytes,
        Err(_) => return bad(StatusCode::PAYLOAD_TOO_LARGE, "请求体超出限制"),
    };
    let response =
        match tokio::time::timeout(Duration::from_secs(65), upstream.body(bytes).send()).await {
            Ok(Ok(response)) => response,
            _ => return bad(StatusCode::BAD_GATEWAY, "main gateway 暂不可用"),
        };
    let mut result = Response::builder().status(response.status());
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LAST_MODIFIED,
    ] {
        if let Some(value) = response.headers().get(&name) {
            result = result.header(name, value);
        }
    }
    result
        .body(Body::from_stream(response.bytes_stream()))
        .expect("upstream headers")
}

fn main_history(handle: &SessionHandle) -> Vec<MainChatMessage> {
    let messages = handle
        .message_cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut history = Vec::new();
    for outcome in handle
        .turn_outcomes()
        .iter()
        .filter(|outcome| outcome.state == TurnState::Completed)
    {
        let Some(user) = messages
            .iter()
            .find(|message| message.uuid.as_deref() == Some(&outcome.user_message_id))
        else {
            continue;
        };
        let prefix = format!("msg_{}_{}_assistant", handle.id, outcome.turn_id);
        let answer: String = messages
            .iter()
            .filter(|message| {
                message.role == openpaw_core::Role::Assistant
                    && message
                        .uuid
                        .as_deref()
                        .is_some_and(|id| id == prefix || id.starts_with(&format!("{prefix}_")))
            })
            .map(openpaw_core::Message::text)
            .collect();
        if !answer.is_empty() {
            history.push(MainChatMessage {
                role: "user".into(),
                content: user.text(),
            });
            history.push(MainChatMessage {
                role: "assistant".into(),
                content: answer,
            });
        }
    }
    while history.len() > 20
        || history
            .iter()
            .map(|message| message.content.encode_utf16().count())
            .sum::<usize>()
            > 100_000
    {
        history.drain(..2);
    }
    history
}

async fn main_chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<MainChatRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(mut body)) = body else {
        return bad(StatusCode::BAD_REQUEST, "聊天请求格式错误");
    };
    let valid_id = |id: &str| {
        !id.is_empty()
            && id.len() <= 100
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    };
    if body.version != "1.0"
        || !valid_id(&body.team_id)
        || !valid_id(&body.conversation_id)
        || body.messages.is_empty()
        || body.messages.len() > 21
        || body.messages.len() % 2 != 1
        || body
            .messages
            .iter()
            .enumerate()
            .any(|(i, message)| message.role != if i % 2 == 0 { "user" } else { "assistant" })
    {
        return bad(StatusCode::BAD_REQUEST, "聊天请求字段错误");
    }
    let latest = body.messages.pop().expect("validated input");
    if latest.content.trim().is_empty()
        || latest.content.encode_utf16().count() > 4000
        || body
            .messages
            .iter()
            .map(|message| message.content.encode_utf16().count())
            .sum::<usize>()
            > 100_000
    {
        return bad(StatusCode::BAD_REQUEST, "聊天输入超出限制");
    }
    let _admission = state.main_office.admission.lock().await;
    let record = match state.main_office.record(&body.team_id) {
        Ok(record) => record,
        Err(error) => return bad(StatusCode::BAD_REQUEST, error),
    };
    if record
        .conversation
        .as_deref()
        .is_some_and(|id| id != body.conversation_id)
    {
        return bad(StatusCode::CONFLICT, "本轮暂不支持新对话");
    }
    let handle = if let Some(session) = record.session {
        match state.get_typed(&session).await {
            Ok(handle) => handle,
            Err(error) => return session_load_error(error),
        }
    } else {
        if !body.messages.is_empty() {
            return bad(StatusCode::CONFLICT, "对话历史与服务端不一致");
        }
        match state
            .create_session(None, None, Some(record.workspace), Some("Office main"))
            .await
        {
            Ok(handle) => {
                if let Err(error) =
                    state
                        .main_office
                        .bind(&body.team_id, &body.conversation_id, &handle.id)
                {
                    return bad(StatusCode::INTERNAL_SERVER_ERROR, error);
                }
                handle
            }
            Err(error) => return bad(StatusCode::INTERNAL_SERVER_ERROR, error),
        }
    };
    if handle.is_active() {
        return bad(StatusCode::CONFLICT, "会话已有正在执行的回合");
    }
    if main_history(&handle) != body.messages {
        return bad(StatusCode::CONFLICT, "对话历史与服务端不一致");
    }
    let receiver = state.global_events.subscribe();
    let permissions = state.global_events.subscribe();
    let response = schedule_turn(
        state.clone(),
        handle.id.clone(),
        MessageRequest {
            text: Some(latest.content),
            ..Default::default()
        },
        true,
    )
    .await;
    if response.status() != StatusCode::NO_CONTENT {
        return response;
    }
    let turn = response.headers()[OPENPAW_TURN_ID]
        .to_str()
        .expect("internal turn id")
        .to_string();
    let stop = CancellationToken::new();
    main_approval_bridge(
        state.main_office.clone(),
        body.team_id,
        handle.clone(),
        permissions,
        stop.clone(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let (timer_handle, timer_turn, timer_stop) = (handle.clone(), turn.clone(), stop.clone());
    tokio::spawn(async move {
        tokio::select! {
            _ = timer_stop.cancelled() => {},
            _ = tokio::time::sleep_until(deadline) => {timer_handle.cancel_main_turn(&timer_turn);timer_stop.cancel();}
        }
    });
    let stream = MainChatStream {
        receiver,
        handle,
        turn,
        deadline,
        stop,
        pending: std::collections::VecDeque::from([
            serde_json::json!({"type":"status","phase":"thinking"}),
        ]),
        terminal: false,
        characters: 0,
    };
    let stream = futures::stream::unfold(stream, |mut stream| async move {
        loop {
            if let Some(event) = stream.pending.pop_front() {
                let line = format!("{event}\n");
                return Some((Ok::<_, Infallible>(line), stream));
            }
            if stream.terminal {
                return None;
            }
            let received = tokio::time::timeout_at(stream.deadline, stream.receiver.recv()).await;
            match received {
                Ok(Ok(event))
                    if event.properties["sessionID"].as_str() == Some(&stream.handle.id) =>
                {
                    let prefix = format!("prt_{}_{}_text", stream.handle.id, stream.turn);
                    if event.event_type == "message.part.delta"
                        && event.properties["partID"]
                            .as_str()
                            .is_some_and(|id| id == prefix || id.starts_with(&format!("{prefix}_")))
                    {
                        if let Some(text) = event.properties["delta"].as_str() {
                            stream.characters += text.encode_utf16().count();
                            if stream.characters > 50_000 {
                                stream.fail("回复超出长度限制");
                                continue;
                            }
                            let mut chunk = String::new();
                            let mut units = 0;
                            for character in text.chars() {
                                if units + character.len_utf16() > 2000 {
                                    stream.pending.push_back(
                                        serde_json::json!({"type":"delta","text":chunk}),
                                    );
                                    chunk = String::new();
                                    units = 0;
                                }
                                chunk.push(character);
                                units += character.len_utf16();
                            }
                            if !chunk.is_empty() {
                                stream
                                    .pending
                                    .push_back(serde_json::json!({"type":"delta","text":chunk}));
                            }
                        }
                    }
                    if event.event_type == "session.idle" {
                        let outcome = stream
                            .handle
                            .turn_outcomes()
                            .into_iter()
                            .find(|outcome| outcome.turn_id == stream.turn);
                        if outcome.is_some_and(|outcome| outcome.state == TurnState::Completed)
                            && stream.characters > 0
                        {
                            stream.terminal = true;
                            stream.pending.push_back(serde_json::json!({"type":"done"}));
                        } else {
                            stream.fail("本轮执行未完成，请检查会话状态");
                        }
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => stream.fail("聊天事件连接中断"),
                Err(_) => stream.fail("聊天超过 180 秒，已取消"),
            }
        }
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

struct MainChatStream {
    receiver: broadcast::Receiver<OpenCodeEvent>,
    handle: Arc<SessionHandle>,
    turn: String,
    deadline: tokio::time::Instant,
    pending: std::collections::VecDeque<serde_json::Value>,
    terminal: bool,
    characters: usize,
    stop: CancellationToken,
}
impl MainChatStream {
    fn fail(&mut self, message: &str) {
        self.stop.cancel();
        self.handle.cancel_main_turn(&self.turn);
        self.terminal = true;
        self.pending
            .push_back(serde_json::json!({"type":"error","message":message}));
    }
}
impl Drop for MainChatStream {
    fn drop(&mut self) {
        self.stop.cancel();
        self.handle.cancel_main_turn(&self.turn);
    }
}

fn main_approval_bridge(
    office: Arc<crate::office::MainOffice>,
    team: String,
    handle: Arc<SessionHandle>,
    mut receiver: broadcast::Receiver<OpenCodeEvent>,
    stop: CancellationToken,
) {
    tokio::spawn(async move {
        let mut pending = std::collections::HashMap::<String, CancellationToken>::new();
        loop {
            let event = tokio::select! { _ = stop.cancelled() => break, event = receiver.recv() => match event {Ok(event)=>event,Err(_)=>break} };
            if !event.properties["sessionID"]
                .as_str()
                .is_some_and(|owner| handle.approvals.owns_session(owner))
            {
                continue;
            }
            if event.event_type == "permission.replied" {
                if let Some(id) = event.properties["requestID"].as_str() {
                    if let Some(cancel) = pending.remove(id) {
                        cancel.cancel();
                    }
                }
            }
            if event.event_type != "permission.asked" {
                continue;
            }
            let Some(id) = event.properties["id"].as_str().map(str::to_string) else {
                continue;
            };
            if pending.contains_key(&id) {
                continue;
            }
            let tool = event.properties["permission"]
                .as_str()
                .unwrap_or("工具")
                .chars()
                .take(80)
                .collect::<String>();
            let owner = event.properties["metadata"]["workspaceID"]
                .as_str()
                .unwrap_or(&handle.id);
            let employee = office.record(&team).ok().and_then(|record| {
                record.snapshot["employees"]
                    .as_array()
                    .and_then(|employees| employees.iter().find(|employee| employee["id"] == owner))
                    .cloned()
            });
            let actor = employee
                .as_ref()
                .and_then(|employee| employee["sceneActorId"].as_str())
                .filter(|actor| !actor.is_empty());
            let name = employee
                .as_ref()
                .and_then(|employee| employee["name"].as_str())
                .unwrap_or("执行 Agent");
            let summary = crate::office::tool_activity_summary(&tool, &event.properties["input"]);
            let mut decision = serde_json::json!({"id":id,"kind":"confirmation","title":format!("批准 {tool}？"),
                "description":format!("{name}：{summary}。仅授权本次工具调用。"),"confirmLabel":"允许本次","rejectLabel":"拒绝"});
            if let Some(actor) = actor {
                decision["actorId"] = serde_json::Value::from(actor);
            }
            let cancel = stop.child_token();
            pending.insert(id.clone(), cancel.clone());
            let (office, team, handle) = (office.clone(), team.clone(), handle.clone());
            tokio::spawn(async move {
                let request = serde_json::json!({"type":"decision.request","teamId":team,"timeoutMs":60000,"decision":decision});
                let result = tokio::select! { _ = cancel.cancelled() => return,
                result = office.message(request,Duration::from_secs(65)) => result };
                if cancel.is_cancelled() {
                    return;
                }
                let approved = result.is_ok_and(|(status, data)| {
                    status == 200
                        && data["teamId"] == team
                        && data["decisionId"] == id
                        && data["choiceId"] == "confirm"
                        && data["label"] == "允许本次"
                });
                handle
                    .approvals
                    .reply(
                        &id,
                        approved,
                        if approved {
                            "main 单次批准"
                        } else {
                            "main 拒绝或未取得批准"
                        }
                        .into(),
                    )
                    .await;
            });
        }
        for (_, cancel) in pending {
            cancel.cancel();
        }
    });
}

#[derive(Debug, Deserialize, Default)]
struct WorkspaceQuery {
    directory: Option<String>,
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    body: Option<Json<CreateSessionRequest>>,
) -> Response {
    // OpenWork's generated client omits both body and Content-Type for the
    // default session-create request. Optional JSON accepts that request while
    // still rejecting malformed JSON when a body is supplied.
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let workspace = headers
        .get("x-opencode-directory")
        .and_then(|v| v.to_str().ok())
        .map(std::path::PathBuf::from)
        .or_else(|| query.directory.map(std::path::PathBuf::from));
    match state
        .create_session(
            body.provider.as_deref(),
            body.model.as_deref(),
            workspace,
            body.title.as_deref(),
        )
        .await
    {
        Ok(handle) => {
            let wire = state.wire_session(&handle).await;
            handle.emit(crate::events::session_changed("session.created", &wire));
            // openwork 代理端到端验收断言 200（OpenCode v1 服务端亦为 200）。
            (StatusCode::OK, Json(wire)).into_response()
        }
        Err(error) => bad(StatusCode::BAD_REQUEST, error),
    }
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<crate::protocol::SessionSummary>> {
    Json(state.summaries().await)
}

async fn get_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let handle = match state.get_typed(&id).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    Json(state.wire_session(&handle).await).into_response()
}

async fn update_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateSessionRequest>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return bad(StatusCode::BAD_REQUEST, error.body_text()),
    };
    match state.update_session(&id, body).await {
        Ok(session) => Json(session).into_response(),
        Err(error) => session_mutation_error(error),
    }
}

async fn fork_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<ForkSessionRequest>>,
) -> Response {
    let message_id = body.and_then(|Json(body)| body.message_id);
    match state.fork_session(&id, message_id.as_deref()).await {
        Ok(handle) => Json(state.wire_session(&handle).await).into_response(),
        Err(error) => session_mutation_error(error),
    }
}

async fn revert_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<RevertSessionRequest>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return bad(StatusCode::BAD_REQUEST, error.body_text()),
    };
    if body.part_id.is_some() {
        return bad_with_code(
            StatusCode::BAD_REQUEST,
            "暂不支持按 partID 回退",
            "unsupported_part_revert",
            false,
        );
    }
    match state.revert_session(&id, &body.message_id).await {
        Ok(session) => Json(session).into_response(),
        Err(error) => session_mutation_error(error),
    }
}

async fn unrevert_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.unrevert_session(&id).await {
        Ok(session) => Json(session).into_response(),
        Err(error) => session_mutation_error(error),
    }
}

async fn delete_session(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.delete_session(&id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => bad(StatusCode::NOT_FOUND, "会话不存在"),
        Err(error) if error == "会话正在执行，无法删除" => {
            bad(StatusCode::CONFLICT, error)
        }
        Err(error) => bad(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MessageQuery {
    #[serde(rename = "turnID")]
    turn_id: Option<String>,
    text_only: Option<bool>,
}
async fn messages(
    State(state): State<Arc<AppState>>,
    Path(path): Path<std::collections::HashMap<String, String>>,
    Query(query): Query<MessageQuery>,
) -> Response {
    let handle = match state.get_typed(&path["id"]).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    // 读取独立快照，避免模型/工具回合持有 Session 写锁时让前端轮询等待。
    let mut result = handle.wire_messages();
    if query.text_only == Some(true) {
        let Some(turn) = query.turn_id.filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) else {
            return bad(StatusCode::BAD_REQUEST, "textOnly 需要合法 turnID");
        };
        let base = format!("msg_{}_{turn}_assistant", path["id"]);
        result.retain(|m| {
            m["info"]["role"] == "assistant"
                && m["info"]["id"].as_str().is_some_and(|id| {
                    id == base
                        || id
                            .strip_prefix(&(base.clone() + "_"))
                            .is_some_and(|s| s.parse::<u32>().is_ok())
                })
        });
        for message in &mut result {
            if let Some(parts) = message["parts"].as_array_mut() {
                parts.retain(|p| p["type"] == "text");
            }
        }
    } else if query.turn_id.is_some() {
        return bad(StatusCode::BAD_REQUEST, "turnID 仅用于 textOnly 读取");
    }
    match path.get("message_id") {
        Some(id) => match result
            .into_iter()
            .find(|message| message["info"]["id"] == *id)
        {
            Some(message) => Json(message).into_response(),
            None => bad(StatusCode::NOT_FOUND, "消息不存在"),
        },
        None => Json(result).into_response(),
    }
}

async fn turns(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let handle = match state.get_typed(&id).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    Json(handle.turn_outcomes()).into_response()
}

async fn message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<MessageRequest>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => {
            return bad(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "invalid JSON request body",
            )
        }
    };
    schedule_turn(state, id, body, false).await
}

async fn prompt_async(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<MessageRequest>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => {
            return bad(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "invalid JSON request body",
            )
        }
    };
    schedule_turn(state, id, body, true).await
}

async fn schedule_turn(
    state: Arc<AppState>,
    id: String,
    mut body: MessageRequest,
    asynchronous: bool,
) -> Response {
    let text = body.text();
    let (mut user, presentation) = match body.prepare() {
        Ok(input) => input,
        Err(error) => return media_error(error),
    };
    let input_media = openpaw_core::media::message_media(std::slice::from_ref(&user));
    if text.trim().is_empty() && input_media.is_empty() {
        return bad(StatusCode::BAD_REQUEST, "消息文本不能为空");
    }
    body.request_id = body.request_id.or_else(|| body.message_id.clone());
    for request_id in body.request_id.iter().chain(body.message_id.iter()) {
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return bad_with_code(
                StatusCode::BAD_REQUEST,
                "requestID/messageID 必须为 1..128 字节的可打印 ASCII",
                "invalid_request_id",
                false,
            );
        }
    }
    let host_admission = match state.begin_admission() {
        Ok(admission) => admission,
        Err(_) => {
            return bad_with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "Host 正在关闭，请稍后重试",
                "host_shutting_down",
                true,
            )
        }
    };
    let handle = match state.get_typed(&id).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    let (session_provider, session_model) = handle.provider_model();
    let (provider_name, requested_model) = match body.model.as_ref() {
        Some(model) => (
            model
                .provider_id
                .clone()
                .unwrap_or_else(|| state.config.provider.clone()),
            model.model_id.clone(),
        ),
        None => (session_provider, Some(session_model)),
    };
    let model_name = match requested_model {
        Some(model) => model.trim().to_string(),
        None => match runtime::default_model(&provider_name) {
            Ok(model) => model,
            Err(error) => return bad(StatusCode::BAD_REQUEST, error),
        },
    };
    if model_name.is_empty() {
        return bad(StatusCode::BAD_REQUEST, "modelID 不能为空");
    }

    // 先完成 requestID 重放判断，再构造 Provider；这样 Provider 凭据或地址暂时失效时，
    // 已成功接收的同一请求仍可稳定确认，不会被误报为新的模型配置错误。
    let fingerprint = body
        .request_id
        .as_ref()
        .map(|_| request_fingerprint(&body, &provider_name, &model_name));
    if let (Some(request_id), Some(fingerprint)) =
        (body.request_id.as_deref(), fingerprint.as_deref())
    {
        if let Some(existing) = handle.request_admission(request_id) {
            if existing.fingerprint == fingerprint {
                return replay_response(&state, &handle, asynchronous, &existing.turn_id).await;
            }
            return bad_with_code(
                StatusCode::CONFLICT,
                "相同 requestID 已用于不同请求",
                "idempotency_conflict",
                false,
            );
        }
    }
    let provider = match runtime::build_provider_for_model(&provider_name, Some(&model_name)) {
        Ok(provider) => provider,
        Err(error) => return bad(StatusCode::BAD_REQUEST, error),
    };
    if let Err(error) =
        openpaw_core::media::validate_media(&input_media, provider.model_capabilities(&model_name))
    {
        return media_error(error);
    }
    let turn_id = handle.next_turn_id();
    let user_message_id = body
        .message_id
        .clone()
        .unwrap_or_else(|| format!("msg_{id}_{turn_id}_user"));
    user.uuid = Some(user_message_id.clone());
    let input_content = user.content.clone();
    let request_admission =
        body.request_id
            .clone()
            .zip(fingerprint)
            .map(|(request_id, fingerprint)| RequestAdmission {
                request_id,
                fingerprint,
                turn_id: turn_id.clone(),
                user_message_id: user_message_id.clone(),
            });
    let accepted = match runtime::accept_turn(
        &state,
        &handle,
        user,
        turn_id,
        user_message_id,
        request_admission,
        presentation.clone(),
    ) {
        AcceptTurnResult::Accepted(accepted) => accepted,
        AcceptTurnResult::Replay(existing) => {
            return replay_response(&state, &handle, asynchronous, &existing.turn_id).await;
        }
        AcceptTurnResult::IdempotencyConflict => {
            return bad_with_code(
                StatusCode::CONFLICT,
                "相同 requestID 已用于不同请求",
                "idempotency_conflict",
                false,
            )
        }
        AcceptTurnResult::Busy => {
            return bad_with_code(
                StatusCode::CONFLICT,
                "会话已有正在执行的回合",
                "active_turn",
                true,
            )
        }
        AcceptTurnResult::Deleting => return bad(StatusCode::NOT_FOUND, "会话不存在"),
        AcceptTurnResult::Archived => return bad(StatusCode::CONFLICT, "子会话已归档"),
        AcceptTurnResult::QueueFull => {
            return bad_with_code(
                StatusCode::TOO_MANY_REQUESTS,
                "全局回合等待队列已满，请稍后重试",
                "queue_full",
                true,
            )
        }
        AcceptTurnResult::TemporarilyBusy => {
            return bad(StatusCode::SERVICE_UNAVAILABLE, "会话暂时繁忙，请稍后重试")
        }
        AcceptTurnResult::PersistenceFailed(error) => {
            return bad(StatusCode::INTERNAL_SERVER_ERROR, error)
        }
    };
    handle.updated_at.store(
        chrono::Utc::now().timestamp_millis(),
        std::sync::atomic::Ordering::Release,
    );
    handle.emit(crate::events::user_message_updated(
        &id,
        &accepted.user_message_id,
    ));
    for (index, block) in input_content.iter().enumerate() {
        if input_media.is_empty() && presentation.is_none() {
            handle.emit(crate::events::user_text_part(
                &id,
                &accepted.user_message_id,
                &text,
            ));
            break;
        }
        if let Some(part) = crate::protocol::projection::input_part(
            &id,
            &accepted.user_message_id,
            index,
            block,
            presentation.as_ref(),
        ) {
            handle.emit(OpenCodeEvent::new(
                "message.part.updated",
                serde_json::json!({"part":part}),
            ));
        }
    }
    let response_turn_id = accepted.turn_id.clone();
    let task = state.spawn_turn(
        host_admission,
        handle.clone(),
        runtime::ScheduledTurn {
            turn_id: accepted.turn_id,
            user_message_id: accepted.user_message_id,
            text: text.clone(),
            provider_tools: state.ai_pc_tools(&handle),
            provider,
            model: model_name,
            admission: accepted.admission,
        },
    );
    if asynchronous {
        with_turn_header(StatusCode::NO_CONTENT.into_response(), &response_turn_id)
    } else {
        // 同步端点保留 OpenCode 的阻塞调用语义；异步客户端使用 prompt_async。
        task.wait().await;
        with_turn_header(
            Json(state.wire_session(&handle).await).into_response(),
            &response_turn_id,
        )
    }
}

async fn abort(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let handle = match state.get_typed(&id).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    handle.abort().await;
    Json(true).into_response()
}
// API-HOST-AI-PC-REGISTER-001：认证登记入口，由 runtime 协调身份和空闲闸门。
async fn register_ai_pc(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<crate::runtime::desktop::RegistrationInput>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = body else {
        return bad(StatusCode::BAD_REQUEST, "登记字段无效");
    };
    ai_pc_response(state.register_ai_pc(&id, Some(input)).await)
}
// API-HOST-AI-PC-UNREGISTER-001：认证撤销入口，与登记共享 runtime 生命周期规则。
async fn unregister_ai_pc(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    ai_pc_response(state.register_ai_pc(&id, None).await)
}
fn ai_pc_response(result: Result<(), crate::runtime::desktop::RegistrationError>) -> Response {
    use crate::runtime::desktop::RegistrationError;
    match result {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            [
                ("openpaw-desktop-protocol", "1".to_owned()),
                (
                    "openpaw-desktop-tools",
                    crate::runtime::desktop::advertised_tools(),
                ),
            ],
        )
            .into_response(),
        Err(RegistrationError::AuthenticationRequired) => bad(
            StatusCode::SERVICE_UNAVAILABLE,
            "桌面登记需要配置 Host Basic 鉴权",
        ),
        Err(RegistrationError::NotFound) => bad(StatusCode::NOT_FOUND, "会话不存在"),
        Err(RegistrationError::Busy) => bad(StatusCode::CONFLICT, "活动会话不能替换桌面登记"),
        Err(RegistrationError::Invalid) => bad(StatusCode::BAD_REQUEST, "桌面登记字段无效"),
    }
}

async fn todo(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if state.get(&id).await.is_none() {
        return bad(StatusCode::NOT_FOUND, "会话不存在");
    }
    Json(Vec::<crate::protocol::TodoItem>::new()).into_response()
}

async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let sessions = state.sessions.read().await;
    let mut result = serde_json::Map::new();
    for session in sessions.values() {
        if !session.is_active() {
            continue;
        }
        if let Some((turn, queue_position)) = session.current_turn_snapshot(&state.scheduler) {
            let mut value = serde_json::json!({
                "type": if turn.state == TurnState::Queued { "queued" } else { "busy" },
                "turnID": turn.turn_id,
                "userMessageID": turn.user_message_id
            });
            if let Some(position) = queue_position {
                value["queuePosition"] = serde_json::json!(position);
            }
            if turn.state == TurnState::Running {
                if let Some(activity) = session.activity_snapshot() {
                    if let Ok(activity_value) = serde_json::to_value(activity) {
                        value["activity"] = activity_value;
                    }
                }
            }
            result.insert(session.id.clone(), value);
        }
    }
    Json(serde_json::Value::Object(result))
}

/// §3.2：`/event`、`/global/event`、`/api/event` 三条路径提供同一个全局 SSE。
async fn global_event_stream(State(state): State<Arc<AppState>>) -> Response {
    let stream = sse_stream(state.global_events.subscribe(), state.shutdown_token());
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn provider(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = openpaw_llm::config::ConfigSnapshot::from_env();
    let all: Vec<_> = openpaw_llm::config::PROVIDERS
        .iter()
        .map(|(id, _, name)| {
            let models: serde_json::Map<_, _> = config
                .models(id)
                .into_iter()
                .map(|model| {
                    let caps = config.capabilities(id, &model).unwrap_or_default();
                    (model.clone(), serde_json::json!({"id":model,"name":model,"capabilities":{"input":{"text":true,"image":caps.image_input,"audio":caps.audio_input}}}))
                })
                .collect();
            serde_json::json!({"id":id,"name":name,"models":models})
        })
        .collect();
    let connected: Vec<_> = openpaw_llm::config::PROVIDERS
        .iter()
        .filter(|(id, _, _)| config.resolve(id, Default::default()).is_ok())
        .map(|(id, _, _)| *id)
        .collect();
    Json(serde_json::json!({"all":all,"connected":connected,"default":state.config.provider}))
}

async fn permissions(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<crate::protocol::PermissionInfo>> {
    let mut out = Vec::new();
    for session in state.sessions.read().await.values() {
        out.extend(session.approvals.list().await);
    }
    out.sort_by(|left, right| left.id.cmp(&right.id));
    out.dedup_by(|left, right| left.id == right.id);
    Json(out)
}

async fn permission_reply(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<PermissionReply>, JsonRejection>,
) -> Response {
    // §3.2：permission 不存在返回 404，先于请求体校验（openwork 验收用未知 id 断言 404）。
    let mut known = false;
    for session in state.sessions.read().await.values() {
        known |= session.approvals.list().await.iter().any(|p| p.id == id);
    }
    if !known {
        return bad(StatusCode::NOT_FOUND, "审批请求不存在");
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return bad(StatusCode::BAD_REQUEST, error.body_text()),
    };
    let Some(reply) = body.reply.as_deref() else {
        return bad(
            StatusCode::BAD_REQUEST,
            "reply 必须是 once、always 或 reject",
        );
    };
    let (approved, remember_for_session) = match reply {
        "once" => (true, false),
        "always" => (true, true),
        "reject" => (false, false),
        _ => {
            return bad(
                StatusCode::BAD_REQUEST,
                "reply 必须是 once、always 或 reject",
            )
        }
    };
    let reason = body.reason.clone().unwrap_or_else(|| {
        if approved {
            "用户已批准".into()
        } else {
            "用户拒绝".into()
        }
    });
    for session in state.sessions.read().await.values() {
        if session
            .approvals
            .reply_with_scope(&id, approved, remember_for_session, reason.clone())
            .await
        {
            return StatusCode::NO_CONTENT.into_response();
        }
    }
    bad(StatusCode::NOT_FOUND, "审批请求不存在")
}

async fn empty_object() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}
async fn empty_array() -> Json<serde_json::Value> {
    Json(serde_json::json!([]))
}
async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

fn bad(status: StatusCode, error: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({"error":error.into()}))).into_response()
}

fn session_mutation_error(error: SessionMutationError) -> Response {
    match error {
        SessionMutationError::BadRequest(error) => bad(StatusCode::BAD_REQUEST, error),
        SessionMutationError::NotFound => bad(StatusCode::NOT_FOUND, "会话不存在"),
        SessionMutationError::Conflict(error) => bad(StatusCode::CONFLICT, error),
        SessionMutationError::Persistence(error) => bad(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

fn bad_with_code(
    status: StatusCode,
    error: impl Into<String>,
    code: &'static str,
    retryable: bool,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": error.into(),
            "code": code,
            "retryable": retryable
        })),
    )
        .into_response()
}

fn session_load_error(error: SessionLoadError) -> Response {
    match error {
        SessionLoadError::NotFound => bad(StatusCode::NOT_FOUND, "会话不存在"),
        SessionLoadError::Corrupt => bad_with_code(
            StatusCode::CONFLICT,
            "会话数据损坏，无法加载",
            "session_corrupt",
            false,
        ),
        SessionLoadError::PermissionDenied => bad_with_code(
            StatusCode::FORBIDDEN,
            "没有权限读取会话数据",
            "session_permission_denied",
            false,
        ),
        SessionLoadError::Unavailable => bad_with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "会话暂时无法加载，请稍后重试",
            "session_unavailable",
            true,
        ),
    }
}

async fn replay_response(
    state: &AppState,
    handle: &SessionHandle,
    asynchronous: bool,
    turn_id: &str,
) -> Response {
    let response = if asynchronous {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(state.wire_session(handle).await).into_response()
    };
    with_turn_header(response, turn_id)
}

fn with_turn_header(mut response: Response, turn_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(turn_id) {
        response.headers_mut().insert(OPENPAW_TURN_ID, value);
    }
    response
}

fn request_fingerprint(body: &MessageRequest, provider: &str, model: &str) -> String {
    let text = body.text();
    let mut canonical = Vec::new();
    for field in ["openpaw-message-v1", text.as_str(), provider, model] {
        canonical.extend_from_slice(&(field.len() as u64).to_be_bytes());
        canonical.extend_from_slice(field.as_bytes());
    }
    if let Some(message_id) = &body.message_id {
        canonical.extend_from_slice(message_id.as_bytes());
    }
    if body
        .parts
        .iter()
        .any(|part| part.kind.as_deref() == Some("file"))
    {
        if let Ok(message) = body.message() {
            let content: Vec<_> = message.content.iter().map(|block| {
                match openpaw_core::media::block_media(block) {
                    Ok(Some(media)) => serde_json::json!({"mime":media.mime,"data":media.data,"filename":media.filename}),
                    _ => serde_json::to_value(block).unwrap_or_default(),
                }
            }).collect();
            canonical.extend_from_slice(
                serde_json::to_string(&content)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
    }
    if let Ok((message, Some(presentation))) = body.prepare() {
        let content: Vec<_> = message.content.iter().map(|block| match openpaw_core::media::block_media(block) {
            Ok(Some(media)) => serde_json::json!({"mime":media.mime,"data":media.data,"filename":media.filename}),
            _ => serde_json::to_value(block).unwrap_or_default(),
        }).collect();
        canonical.extend_from_slice(
            serde_json::to_string(&(content, presentation))
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    sha256_hex(&canonical)
}

fn media_error(error: openpaw_core::Error) -> Response {
    let detail = error.to_string();
    bad(
        if detail.contains("byte limit") {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        },
        detail,
    )
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = INITIAL;
    for chunk in padded.as_slice().as_chunks::<64>().0 {
        let mut words = [0u32; 64];
        for (index, bytes) in chunk.as_slice().as_chunks::<4>().0.iter().enumerate() {
            words[index] = u32::from_be_bytes(*bytes);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (target, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

fn sse_stream(
    receiver: broadcast::Receiver<SseEnvelope>,
    shutdown: CancellationToken,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    futures::stream::unfold(Some((receiver, shutdown)), |state| async move {
        let (mut receiver, shutdown) = state?;
        let received = tokio::select! {
            _ = shutdown.cancelled() => return None,
            received = receiver.recv() => received,
        };
        match received {
            Ok(event) => Some((
                Ok(Event::default()
                    .json_data(event)
                    .unwrap_or_else(|_| Event::default())),
                Some((receiver, shutdown)),
            )),
            Err(broadcast::error::RecvError::Closed) => None,
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                let gap = OpenCodeEvent::new(
                    "stream.gap",
                    serde_json::json!({
                        "scope": "global",
                        "dropped": dropped.max(1),
                        "recoverVia": ["status", "turns", "message"]
                    }),
                );
                Some((
                    Ok(Event::default()
                        .json_data(gap)
                        .unwrap_or_else(|_| Event::default())),
                    None,
                ))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use futures::StreamExt;
    use std::sync::{Arc, OnceLock};
    use tempfile::tempdir;
    use tower::ServiceExt;

    static PROVIDER_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    struct MainFixture {
        app: Router,
        state: Arc<AppState>,
        team: String,
        home: tempfile::TempDir,
        workspace: tempfile::TempDir,
        seen: Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
        model_seen: Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
        servers: Vec<tokio::task::JoinHandle<()>>,
        env: Vec<(&'static str, Option<std::ffi::OsString>)>,
        release_model: Arc<tokio::sync::Notify>,
        ui_actions: Arc<tokio::sync::Mutex<std::collections::VecDeque<serde_json::Value>>>,
    }
    impl Drop for MainFixture {
        fn drop(&mut self) {
            for server in &self.servers {
                server.abort();
            }
            for (key, value) in &self.env {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
    impl MainFixture {
        async fn child_employee(&self) -> serde_json::Value {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(employee) = self.seen.lock().await.iter().rev().find_map(|item| {
                        item["snapshot"]["employees"]
                            .as_array()
                            .and_then(|items| items.get(1))
                            .cloned()
                    }) {
                        return employee;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("真实子员工投影已到达 gateway")
        }

        async fn ui(&self, action: serde_json::Value) -> serde_json::Value {
            let id = action["id"].as_str().unwrap().to_string();
            self.ui_actions.lock().await.push_back(action);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(response) = self
                        .seen
                        .lock()
                        .await
                        .iter()
                        .find(|message| {
                            message["type"] == "ui.respond" && message["interactionId"] == id
                        })
                        .cloned()
                    {
                        return response["presentation"]["content"].clone();
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("现有 UI 桥未回复当前请求 {id}"))
        }

        async fn button(
            &self,
            current: &str,
            id: &str,
            content: &serde_json::Value,
            label: &str,
        ) -> serde_json::Value {
            let option = content["actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|option| option["label"] == label)
                .unwrap();
            self.ui(serde_json::json!({"id":id,"teamId":self.team,"action":"dialog.action","interactionId":current,"optionId":option["id"]})).await
        }

        fn chat(&self, text: &str) -> Request<Body> {
            Request::builder().method("POST").uri("/api/chat").header("content-type","application/json")
                .body(Body::from(serde_json::json!({"version":"1.0","teamId":self.team,"conversationId":"fixture",
                    "messages":[{"role":"user","content":text}]}).to_string())).unwrap()
        }
    }
    async fn main_fixture(scenario: &'static str, choice: &'static str) -> MainFixture {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let model_seen = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let gateway_seen = seen.clone();
        let models = model_seen.clone();
        let release_model = Arc::new(tokio::sync::Notify::new());
        let model_gate = release_model.clone();
        let ui_actions = Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new()));
        let gateway_actions = ui_actions.clone();
        let server = Router::new().route("/api/pawoffice/messages",post(move |headers:HeaderMap,Json(body):Json<serde_json::Value>| {
            let seen = gateway_seen.clone();
            let actions = gateway_actions.clone();
            async move {
                assert_eq!(headers[header::AUTHORIZATION],"Bearer fixture-gateway-key");
                seen.lock().await.push(body.clone());
                let data = match body["type"].as_str() {
                    Some("office.capabilities") => serde_json::json!({"scene":{"actors":[]},"ui":{},"requests":{},"responses":{"success":{},"error":{}}}),
                    Some("office.read") if body["teamId"].is_string() => {
                        let messages = seen.lock().await;
                        let snapshot = messages.iter().rev().find(|message| message["type"] == "office.snapshot" && message["teamId"] == body["teamId"]).unwrap()["snapshot"].clone();
                        let actors = ["office-lead","code-agent","file-agent","app-agent","review-agent","data-agent"].into_iter().map(|id|
                            serde_json::json!({"id":id,"presentation":{"sourceRevision":0}})).collect::<Vec<_>>();
                        serde_json::json!({"snapshot":snapshot,"scene":{"actors":actors},"pendingDecisions":[]})
                    },
                    Some("offices.initialize"|"office.read") => serde_json::json!({"offices":[],"actors":[],"supportedMessages":[],"sceneCommandProtocol":"2.0","ui":{}}),
                    Some("office.snapshot") => serde_json::json!({"teamId":body["teamId"],"seq":body["snapshot"]["seq"],"status":"applied"}),
                    Some("ui.next") => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        let seen = seen.lock().await;
                        if let Some(action) = actions.lock().await.pop_front() {
                            return Json(serde_json::json!({"code":200,"msg":"OK","data":{"actions":[action]}}));
                        }
                        let employee = seen.iter().rev().find_map(|message| {
                            message["snapshot"]["employees"].as_array().and_then(|employees|employees.get(1)).map(|employee|(message["teamId"].clone(),employee["id"].clone()))
                        });
                        if scenario == "delegate-inspect" && !seen.iter().any(|message|message["type"] == "ui.respond") {
                            if let Some((team, employee)) = employee { serde_json::json!({"actions":[{"id":"inspect-real-child","teamId":team,"action":"employee.inspect","employeeId":employee}]}) }
                            else { serde_json::json!({"actions":[]}) }
                        } else { serde_json::json!({"actions":[]}) }
                    },
                    Some("decision.request") => {
                        if choice == "pending" || choice == "child-pending" && !body["decision"]["title"].as_str().unwrap_or("").contains("delegate_task") { std::future::pending::<()>().await; }
                        let chosen = if matches!(choice,"child-pending"|"child-timeout") {"confirm"}else{choice};
                        serde_json::json!({"teamId":body["teamId"],"decisionId":body["decision"]["id"],"choiceId":chosen,
                            "label":if chosen == "confirm" {"允许本次"} else {"拒绝"}})
                    },
                    _ => serde_json::json!({"forwarded":1}),
                };
                Json(serde_json::json!({"code":if choice=="child-timeout" && body["type"]=="decision.request" && !body["decision"]["title"].as_str().unwrap_or("").contains("delegate_task") {408}else{200},"msg":"OK","data":data}))
            }
        })).route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
            let seen = models.clone();
            let gate = model_gate.clone();
            async move {
                seen.lock().await.push(body.clone());
                if body["stream"] != true {
                    return Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"Compacted fixture conversation."},"finish_reason":"stop"}]})).into_response();
                }
                if !body["tools"].is_array() {
                    let chunk = serde_json::json!({"choices":[{"delta":{"content":"Compacted fixture conversation."},"finish_reason":"stop"}]});
                    return ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response();
                }
                let messages = body["messages"].as_array().unwrap();
                let latest = messages.iter().rposition(|message|message["role"] == "user").unwrap_or(0);
                let tools = messages[latest..].iter().filter(|message| message["role"] == "tool").count();
                let parent = body["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == "delegate_task");
                if scenario == "delegate-concurrent" || scenario == "delegate-cancel" {
                    let task = messages[latest]["content"].as_str().unwrap_or("");
                    if !parent && tools == 0 && task == "hold-model" { gate.notified().await; }
                    let calls = if parent && tools == 0 {
                        serde_json::from_str::<Vec<serde_json::Value>>(task).unwrap().into_iter().enumerate().map(|(index,args)|serde_json::json!({"index":index,"id":format!("delegate-{index}"),"type":"function","function":{"name":"delegate_task","arguments":args.to_string()}})).collect::<Vec<_>>()
                    } else if !parent && tools == 0 && matches!(task,"alpha"|"beta") {
                        let command = format!("python3 lock-test.py {task}");
                        vec![serde_json::json!({"index":0,"id":"locked-command","type":"function","function":{"name":"bash","arguments":serde_json::json!({"command":command}).to_string()}})]
                    } else { Vec::new() };
                    let chunk = if calls.is_empty() {serde_json::json!({"choices":[{"delta":{"content":"concurrent turn completed"},"finish_reason":"stop"}]})}
                    else {serde_json::json!({"choices":[{"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]})};
                    return ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response();
                }
                if scenario == "delegate-single-approval" {
                    let task = messages[latest]["content"].as_str().unwrap_or("");
                    let call = if parent && tools == 0 {
                        Some(("delegate_task",serde_json::from_str::<serde_json::Value>(task).unwrap()))
                    } else if !parent && tools < 2 {
                        Some(("write_file",serde_json::json!({"path":format!("{task}-{tools}.txt"),"content":"single-call-authorized"})))
                    } else { None };
                    let chunk = if let Some((name,args)) = call {
                        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("call-{tools}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
                    } else {serde_json::json!({"choices":[{"delta":{"content":if parent {"parent integrated"} else {"child handled decisions"}},"finish_reason":"stop"}]})};
                    return ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response();
                }
                if scenario == "delegate-child-fail" && !parent && tools > 0 {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                if scenario == "delegate-resume" {
                    let task = messages[latest]["content"].as_str().unwrap_or("");
                    let call = if parent && tools == 0 {
                        Some(("delegate_task",serde_json::from_str::<serde_json::Value>(task).unwrap()))
                    } else if !parent && tools == 0 && task == "读取首轮秘密" {
                        Some(("read_file",serde_json::json!({"path":"README.md"})))
                    } else if !parent && tools == 0 && task == "继续，使用上轮工具信息写入文件" {
                        let previous = messages[..latest].iter().find(|message|message["role"] == "tool")
                            .expect("续办必须收到自己的上轮工具结果");
                        Some(("write_file",serde_json::json!({"path":"continued.txt","content":previous["content"].as_str().unwrap()})))
                    } else { None };
                    let chunk = if let Some((name,args)) = call {
                        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("call-{tools}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
                    } else {serde_json::json!({"choices":[{"delta":{"content":if parent {"parent integrated"} else {"本轮任务已完成"}},"finish_reason":"stop"}]})};
                    return ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response();
                }
                if scenario == "recover" && parent && tools == 0 { gate.notified().await; }
                let call = if (scenario.starts_with("delegate") || scenario == "recover") && parent && tools == 0 {
                    Some(("delegate_task",serde_json::json!({"name":"资料检索","task":if scenario == "delegate-repeat" {
                        format!("读取 README.md 并汇报：{}", messages[latest]["content"].as_str().unwrap_or(""))
                    }else{"读取 README.md 并汇报".into()}})))
                } else if tools < if scenario == "tools" {2} else {1} {
                    Some(if scenario == "delegate-long" {("sleep",serde_json::json!({"duration_ms":300000}))}
                        else if scenario == "write" || scenario == "delegate-write" {("write_file",serde_json::json!({"path":"approved.txt","content":"approved once"}))}
                        else {("read_file",serde_json::json!({"path":"README.md"}))})
                } else { None };
                let chunk = if let Some((name,args)) = call {
                    serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("call-{tools}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
                } else { serde_json::json!({"choices":[{"delta":{"content":if scenario == "emoji" {"😀".repeat(20_000)}else if parent {"parent integrated".into()}else if scenario == "delegate-details" {format!("子结果开始|{}|子结果结束","🙂任务".repeat(12_000))}else{"tool results integrated".into()}},"finish_reason":"stop"}]}) };
                ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let servers = vec![tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        })];
        let values = [
            ("OPENAI_API_KEY", "fixture-model-key".into()),
            ("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1")),
            ("OPENPAW_OPENAI_MODEL", "fixture-model".into()),
            ("OPENPAW_MAIN_API_KEY", "fixture-gateway-key".into()),
            (
                "OPENPAW_ENGINE_MAX_MODEL_REQUESTS",
                if matches!(scenario, "delegate-concurrent" | "delegate-cancel") {
                    "2"
                } else {
                    "1"
                }
                .into(),
            ),
            (
                "OPENPAW_ENGINE_PERMISSIONS",
                if scenario == "write"
                    || scenario == "delegate-write"
                    || scenario == "delegate-single-approval"
                {
                    "manual"
                } else {
                    "auto"
                }
                .into(),
            ),
        ];
        let env = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        std::fs::write(workspace.path().join("README.md"), "actual tool content").unwrap();
        let config = HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            provider: "openai".into(),
            ..Default::default()
        };
        let mut state = AppState::new_with_turn_limits(
            config.clone(),
            if scenario == "delegate-cancel" { 2 } else { 1 },
            0,
        )
        .unwrap();
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            &config.home,
            &config.workspace,
            Some(format!("http://{address}")),
        );
        state.office.attach_main(&state.main_office);
        let team = state.main_office.directory().unwrap()[0]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let app = build_router(state.clone());
        MainFixture {
            app,
            state,
            team,
            home,
            workspace,
            seen,
            model_seen,
            servers,
            env,
            release_model,
            ui_actions,
        }
    }

    #[tokio::test]
    async fn main_broken_registry_does_not_block_existing_openwork_session_entry() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let config = HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            provider: "echo".into(),
            ..Default::default()
        };
        let mut state = AppState::new(config.clone());
        std::fs::write(
            home.path().join("offices/registry.json"),
            b"broken registry",
        )
        .unwrap();
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            &config.home,
            &config.workspace,
            Some("http://127.0.0.1:1".into()),
        );
        let session = state
            .create_session(Some("echo"), None, None, None)
            .await
            .unwrap();
        assert!(session.message_cache.read().unwrap().is_empty());
        assert!(state.main_office.directory().is_err());
        assert_eq!(
            std::fs::read(home.path().join("offices/registry.json")).unwrap(),
            b"broken registry"
        );
    }

    #[tokio::test]
    async fn main_unicode_replies_follow_original_browser_history_and_delta_limits() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("emoji", "confirm").await;
        let mut history = Vec::<serde_json::Value>::new();
        for turn in 0..4 {
            if turn == 3 {
                history.drain(..2);
            }
            history.push(serde_json::json!({"role":"user","content":format!("question {turn}")}));
            let request = Request::builder().method("POST").uri("/api/chat").header("content-type","application/json")
                .body(Body::from(serde_json::json!({"version":"1.0","teamId":fixture.team,"conversationId":"unicode","messages":history}).to_string())).unwrap();
            let response = fixture.app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 200_000).await.unwrap();
            let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let session_id = fixture
                .state
                .main_office
                .record(&fixture.team)
                .unwrap()
                .session
                .unwrap();
            let outcomes = fixture
                .state
                .get_typed(&session_id)
                .await
                .unwrap()
                .turn_outcomes();
            assert_eq!(
                events.last().unwrap()["type"],
                "done",
                "turn {turn}, outcomes {outcomes:?}"
            );
            let mut answer = String::new();
            for event in events.iter().filter(|event| event["type"] == "delta") {
                let text = event["text"].as_str().unwrap();
                assert!(text.encode_utf16().count() <= 2000);
                answer.push_str(text);
            }
            assert_eq!(answer.encode_utf16().count(), 40_000);
            history.push(serde_json::json!({"role":"assistant","content":answer}));
        }
    }

    #[tokio::test]
    async fn main_tools_reenter_model_without_creating_fake_employees() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("tools", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("read twice"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"type\":\"done\""));
        let requests = fixture.model_seen.lock().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .unwrap()
                    .contains("actual tool content")));
        let record = fixture.state.main_office.record(&fixture.team).unwrap();
        assert_eq!(record.snapshot["employees"].as_array().unwrap().len(), 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let seen = fixture.seen.lock().await;
        assert!(seen
            .iter()
            .any(|message| message["command"]["actorId"] == "office-lead"
                && message["command"]["status"] == "working"));
        assert!(!seen
            .iter()
            .any(|message| message["command"]["capability"] == "office.visit"));
    }

    #[tokio::test]
    async fn delegated_child_has_public_history_and_parent_relation_after_reload() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-write", "confirm").await;
        let mut public_events = fixture.state.global_events.subscribe();
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派子 Agent 写文件"))
            .await
            .unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(8),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("parent integrated"));
        let parent = fixture
            .state
            .main_office
            .record(&fixture.team)
            .unwrap()
            .session
            .unwrap();
        let read = |app: Router, path: String| async move {
            let response = app
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice::<serde_json::Value>(
                &to_bytes(response.into_body(), 200_000).await.unwrap(),
            )
            .unwrap()
        };
        let sessions = read(fixture.app.clone(), "/session".into()).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parent)
            .expect("委派必须创建可公开查询的真实子会话");
        let child_id = child["id"].as_str().unwrap();
        let history = read(fixture.app.clone(), format!("/session/{child_id}/message")).await;
        assert!(history
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["parts"].as_array().unwrap())
            .any(|part| part["tool"] == "write_file"));
        assert!(history.to_string().contains("tool results integrated"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("approved.txt")).unwrap(),
            "approved once"
        );
        let parent_history = read(fixture.app.clone(), format!("/session/{parent}/message")).await;
        assert!(
            parent_history.to_string().contains(child_id),
            "真实子 ID 必须对父模型和历史可见"
        );
        assert!(!parent_history
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["parts"].as_array().unwrap())
            .any(|part| part["tool"] == "write_file"));
        let mut child_tool_events = 0;
        while let Ok(event) = public_events.try_recv() {
            if event.event_type == "message.part.updated"
                && event.properties["part"]["tool"] == "write_file"
            {
                assert_eq!(event.properties["part"]["sessionID"], child_id);
                child_tool_events += 1;
            }
        }
        assert!(
            child_tool_events > 0,
            "实际子工具必须通过公开事件报告子 owner"
        );
        let requests = fixture.model_seen.lock().await;
        let child_request = requests
            .iter()
            .find(|request| {
                !request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["function"]["name"] == "delegate_task")
            })
            .unwrap();
        assert!(
            !child_request["messages"]
                .to_string()
                .contains("委派子 Agent 写文件"),
            "新子不得复制父历史"
        );
        assert!(!child_request["tools"].to_string().contains("desktop"));
        drop(requests);
        let reloaded = build_router(AppState::new(fixture.state.config.clone()));
        assert_eq!(
            read(reloaded.clone(), format!("/session/{child_id}")).await["parentID"],
            parent
        );
        assert_eq!(
            read(reloaded, format!("/session/{child_id}/message")).await,
            history
        );
    }

    #[tokio::test]
    async fn explicit_child_id_continues_own_tool_history_after_normal_reload() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-resume", "confirm").await;
        let secret = format!("secret-{}", uuid::Uuid::new_v4());
        std::fs::write(fixture.workspace.path().join("README.md"), &secret).unwrap();
        let json = |app: Router, method: &'static str, path: String, body: serde_json::Value| async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice::<serde_json::Value>(
                &to_bytes(response.into_body(), 500_000).await.unwrap(),
            )
            .unwrap()
        };
        let parent = json(
            fixture.app.clone(),
            "POST",
            "/session".into(),
            serde_json::json!({}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let first = serde_json::json!({"task":"读取首轮秘密","name":"李研"}).to_string();
        json(
            fixture.app.clone(),
            "POST",
            format!("/session/{parent}/message"),
            serde_json::json!({"text":first,"requestID":"first"}),
        )
        .await;
        let children = json(
            fixture.app.clone(),
            "GET",
            "/session".into(),
            serde_json::json!({}),
        )
        .await;
        let child = children
            .as_array()
            .unwrap()
            .iter()
            .find(|child| child["parentID"] == parent)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        // 已提交工具结果中的随机信息不在第二轮任务里；文件随后删除，不能再次读取获得。
        std::fs::remove_file(fixture.workspace.path().join("README.md")).unwrap();
        assert!(
            fixture
                .state
                .shutdown_gracefully(Duration::from_secs(2))
                .await
        );
        let mut reloaded_state = AppState::new(fixture.state.config.clone());
        Arc::get_mut(&mut reloaded_state).unwrap().main_office = crate::office::MainOffice::new(
            &fixture.state.config.home,
            &fixture.state.config.workspace,
            Some(
                std::env::var("OPENPAW_OPENAI_BASE_URL")
                    .unwrap()
                    .trim_end_matches("/v1")
                    .to_string(),
            ),
        );
        reloaded_state
            .office
            .attach_main(&reloaded_state.main_office);
        let reloaded = build_router(reloaded_state.clone());
        let second = serde_json::json!({"task":"继续，使用上轮工具信息写入文件","task_id":child})
            .to_string();
        json(
            reloaded.clone(),
            "POST",
            format!("/session/{parent}/message"),
            serde_json::json!({"text":second,"requestID":"second"}),
        )
        .await;
        let history = json(
            reloaded.clone(),
            "GET",
            format!("/session/{child}/message"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            history
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["info"]["role"] == "user")
                .count(),
            2,
            "明确 ID 必须继续原子会话"
        );
        assert!(
            std::fs::read_to_string(fixture.workspace.path().join("continued.txt"))
                .unwrap()
                .contains(&secret)
        );
        assert_eq!(
            json(
                reloaded.clone(),
                "GET",
                "/session".into(),
                serde_json::json!({})
            )
            .await
            .as_array()
            .unwrap()
            .iter()
            .filter(|session| session["parentID"] == parent)
            .count(),
            1
        );
        let seen = fixture.model_seen.lock().await;
        let continuation = seen
            .iter()
            .find(|request| {
                !request["tools"].to_string().contains("delegate_task")
                    && request["messages"]
                        .to_string()
                        .contains("继续，使用上轮工具信息写入文件")
            })
            .unwrap();
        assert!(continuation["messages"].to_string().contains(&secret));
        assert!(!continuation["messages"].to_string().contains("requestID"));
        eprintln!(
            "T02 continuation evidence: {}",
            serde_json::json!({"parentID":parent,"childID":child,"messages":history,"prior_tool_secret_preserved":true,"original_file_removed":true})
        );
        drop(seen);
        assert!(
            reloaded_state
                .shutdown_gracefully(Duration::from_secs(2))
                .await
        );
    }

    async fn child_test_json(
        app: &Router,
        method: &'static str,
        path: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{} {}: {:?}",
            method,
            path,
            response.status()
        );
        let bytes = to_bytes(response.into_body(), 500_000).await.unwrap();
        if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
    }

    async fn next_child_permission(app: &Router, owner: Option<&str>) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let pending =
                    child_test_json(app, "GET", "/permission", serde_json::json!({})).await;
                if let Some(permission) = pending.as_array().unwrap().iter().find(|permission| {
                    permission["metadata"]["parentSessionID"].is_string()
                        && owner.is_none_or(|owner| permission["sessionID"] == owner)
                }) {
                    return permission.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("子工具必须逐次请求授权，不能复用 always")
    }

    #[tokio::test]
    async fn child_turns_parallelize_but_share_write_lock_and_reject_same_child_races() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-concurrent", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("lock-test.py"),
            r#"from pathlib import Path
import sys, time
name = sys.argv[1]
try:
    Path('exclusive').mkdir()
except FileExistsError:
    Path('collision').touch()
    raise
with open('sequence','a') as log: log.write(name+':start\n')
Path(name+'.started').touch()
while not Path('release-'+name).exists(): time.sleep(0.01)
with open('sequence','a') as log: log.write(name+':end\n')
Path('exclusive').rmdir()
"#,
        )
        .unwrap();
        let parent = child_test_json(&fixture.app, "POST", "/session", serde_json::json!({})).await
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let app = fixture.app.clone();
        let path = format!("/session/{parent}/message");
        let task = tokio::spawn(async move {
            child_test_json(&app,"POST",&path,serde_json::json!({"text":serde_json::json!([{"task":"alpha","name":"甲"},{"task":"beta","name":"乙"}]).to_string()})).await
        });
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let status = child_test_json(
                    &fixture.app,
                    "GET",
                    "/session/status",
                    serde_json::json!({}),
                )
                .await;
                let active_tools = status
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|value| value["activity"]["tool"] == "bash")
                    .count();
                if active_tools == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("不同子应同时进入实际工具执行，父只占一份全局额度");
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let children: Vec<_> = sessions
            .as_array()
            .unwrap()
            .iter()
            .filter(|child| child["parentID"] == parent)
            .cloned()
            .collect();
        assert_eq!(children.len(), 2);
        let started = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let started: Vec<_> = ["alpha", "beta"]
                    .into_iter()
                    .filter(|name| {
                        fixture
                            .workspace
                            .path()
                            .join(format!("{name}.started"))
                            .exists()
                    })
                    .collect();
                if !started.is_empty() {
                    break started;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            started.len(),
            1,
            "两个 ToolExecutionStart 之后仍只有一个 bash 能进入写锁"
        );
        for name in ["alpha", "beta"] {
            std::fs::write(fixture.workspace.path().join(format!("release-{name}")), "").unwrap();
        }
        tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!fixture.workspace.path().join("collision").exists());
        let sequence = std::fs::read_to_string(fixture.workspace.path().join("sequence")).unwrap();
        assert!(
            sequence == "alpha:start\nalpha:end\nbeta:start\nbeta:end\n"
                || sequence == "beta:start\nbeta:end\nalpha:start\nalpha:end\n"
        );
        let child = children[0]["id"].as_str().unwrap();
        let app = fixture.app.clone();
        let path = format!("/session/{parent}/message");
        let target = child.to_string();
        let task = tokio::spawn(async move {
            child_test_json(&app,"POST",&path,serde_json::json!({"text":serde_json::json!([{"task":"hold-model","task_id":target},{"task":"hold-model","task_id":target}]).to_string()})).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                let started = requests.iter().any(|request| {
                    !request["tools"].to_string().contains("delegate_task")
                        && request["messages"].to_string().contains("hold-model")
                });
                drop(requests);
                if started {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let public = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{child}/prompt_async"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"must be busy"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(public.status(), StatusCode::CONFLICT);
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .unwrap()
            .unwrap();
        let turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(turns.as_array().unwrap().len(), 2, "两次并发续办只接受一次");
        assert!(turns
            .as_array()
            .unwrap()
            .iter()
            .all(|turn| turn["state"] == "completed"));
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{parent}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(history.to_string().contains("正在执行"));
        assert_eq!(
            child_test_json(
                &fixture.app,
                "GET",
                "/session/status",
                serde_json::json!({})
            )
            .await,
            serde_json::json!({})
        );
        eprintln!(
            "T05 concurrency evidence: {}",
            serde_json::json!({"parentID":parent,"children":children,"serializedWrites":sequence,"sameChildTurns":turns,"publicBusy":409,"globalParentLimit":1,"modelLimit":2})
        );
    }

    #[tokio::test]
    async fn parent_abort_joins_own_child_terminal_and_leaves_other_parent_running() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-cancel", "confirm").await;
        let mut parents = Vec::new();
        let mut tasks = Vec::new();
        for name in ["first", "other"] {
            let parent = child_test_json(&fixture.app, "POST", "/session", serde_json::json!({}))
                .await["id"]
                .as_str()
                .unwrap()
                .to_string();
            let app = fixture.app.clone();
            let path = format!("/session/{parent}/message");
            tasks.push(tokio::spawn(async move {child_test_json(&app,"POST",&path,serde_json::json!({"text":serde_json::json!([{"task":"hold-model","name":name}]).to_string()})).await}));
            parents.push(parent);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                let held = requests
                    .iter()
                    .filter(|request| {
                        !request["tools"].to_string().contains("delegate_task")
                            && request["messages"].to_string().contains("hold-model")
                    })
                    .count();
                drop(requests);
                if held == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parents[0])
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let other_child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parents[1])
            .unwrap()["id"]
            .as_str()
            .unwrap();
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{}/abort", parents[0]),
            serde_json::json!({}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(3), tasks.remove(0))
            .await
            .unwrap()
            .unwrap();
        let stopped = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(stopped[0]["state"], "cancelled");
        assert!(stopped[0]["completedAt"].is_number());
        let active = child_test_json(
            &fixture.app,
            "GET",
            "/session/status",
            serde_json::json!({}),
        )
        .await;
        assert!(active.get(child).is_none());
        assert_eq!(active[&parents[1]]["type"], "busy");
        assert_eq!(active[other_child]["type"], "busy");
        fixture.release_model.notify_waiters();
        tokio::time::timeout(Duration::from_secs(3), tasks.remove(0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{other_child}/turns"),
                serde_json::json!({})
            )
            .await[0]["state"],
            "completed"
        );
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{}/message", parents[0]),
            serde_json::json!({"text":"[]"}),
        )
        .await;
        assert_eq!(
            child_test_json(
                &fixture.app,
                "GET",
                "/session/status",
                serde_json::json!({})
            )
            .await,
            serde_json::json!({})
        );
        eprintln!(
            "T05 cancellation evidence: {}",
            serde_json::json!({"stoppedParent":parents[0],"stoppedChild":child,"childTerminal":stopped,"unrelatedParent":parents[1],"unrelatedChild":other_child,"otherFinished":true,"cancelledParentCanRunAgain":true})
        );
    }

    #[tokio::test]
    async fn persistent_child_approvals_remain_single_call_for_old_always_clients() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-single-approval", "child-pending").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat(&serde_json::json!({"name":"李研","task":"first"}).to_string()))
            .await
            .unwrap();
        let first = next_child_permission(&fixture.app, None).await;
        let child = first["sessionID"].as_str().unwrap();
        let reply = |id: &str| format!("/permission/{id}/reply");
        child_test_json(
            &fixture.app,
            "POST",
            &reply(first["id"].as_str().unwrap()),
            serde_json::json!({"reply":"always"}),
        )
        .await;
        let second = next_child_permission(&fixture.app, Some(child)).await;
        assert_ne!(first["id"], second["id"]);
        assert!(fixture.workspace.path().join("first-0.txt").exists());
        assert!(!fixture.workspace.path().join("first-1.txt").exists());
        child_test_json(
            &fixture.app,
            "POST",
            &reply(second["id"].as_str().unwrap()),
            serde_json::json!({"reply":"reject"}),
        )
        .await;
        tokio::time::timeout(
            Duration::from_secs(3),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!fixture.workspace.path().join("first-1.txt").exists());
        // 同一真实 child 经公开入口继续执行，仍必须重新批准，不能继承先前 always。
        let app = fixture.app.clone();
        let child_path = format!("/session/{child}/message");
        let running = tokio::spawn(async move {
            child_test_json(
                &app,
                "POST",
                &child_path,
                serde_json::json!({"text":"public"}),
            )
            .await
        });
        let third = next_child_permission(&fixture.app, Some(child)).await;
        child_test_json(
            &fixture.app,
            "POST",
            &reply(third["id"].as_str().unwrap()),
            serde_json::json!({"reply":"once"}),
        )
        .await;
        let fourth = next_child_permission(&fixture.app, Some(child)).await;
        child_test_json(
            &fixture.app,
            "POST",
            &reply(fourth["id"].as_str().unwrap()),
            serde_json::json!({"reply":"reject"}),
        )
        .await;
        running.await.unwrap();
        assert!(fixture.workspace.path().join("public-0.txt").exists());
        assert!(!fixture.workspace.path().join("public-1.txt").exists());
        let parent = first["metadata"]["parentSessionID"].as_str().unwrap();
        let app = fixture.app.clone();
        let path = format!("/session/{parent}/message");
        let target = child.to_string();
        let delegated = tokio::spawn(async move {
            child_test_json(&app,"POST",&path,serde_json::json!({"text":serde_json::json!({"task":"resumed","task_id":target}).to_string()})).await
        });
        let parent_permission = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let items =
                    child_test_json(&fixture.app, "GET", "/permission", serde_json::json!({}))
                        .await;
                if let Some(item) = items
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["sessionID"] == parent)
                {
                    break item.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        child_test_json(
            &fixture.app,
            "POST",
            &reply(parent_permission["id"].as_str().unwrap()),
            serde_json::json!({"reply":"always"}),
        )
        .await;
        let fifth = next_child_permission(&fixture.app, Some(child)).await;
        child_test_json(
            &fixture.app,
            "POST",
            &reply(fifth["id"].as_str().unwrap()),
            serde_json::json!({"reply":"once"}),
        )
        .await;
        let sixth = next_child_permission(&fixture.app, Some(child)).await;
        child_test_json(
            &fixture.app,
            "POST",
            &reply(sixth["id"].as_str().unwrap()),
            serde_json::json!({"reply":"reject"}),
        )
        .await;
        delegated.await.unwrap();
        assert!(fixture.workspace.path().join("resumed-0.txt").exists());
        assert!(!fixture.workspace.path().join("resumed-1.txt").exists());
        for used in [&first, &second, &third, &fourth, &fifth, &sixth] {
            let stale = fixture
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(reply(used["id"].as_str().unwrap()))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"reply":"once"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(stale.status(), StatusCode::NOT_FOUND);
        }
        eprintln!(
            "T05 single-call evidence: {}",
            serde_json::json!({"childID":child,"cards":[first,second,third,fourth,fifth,sixth],"allowed":["first-0.txt","public-0.txt","resumed-0.txt"],"rejected":["first-1.txt","public-1.txt","resumed-1.txt"],"lateReplies":404})
        );
    }

    #[tokio::test]
    async fn child_gateway_timeout_denies_the_actual_tool_without_side_effects() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-write", "child-timeout").await;
        // 父 delegate 本次批准，子卡片由外部 gateway 明确返回超时。
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("请写入文件"))
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(4),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!fixture.workspace.path().join("approved.txt").exists());
        let decisions = fixture
            .seen
            .lock()
            .await
            .iter()
            .filter(|message| message["type"] == "decision.request")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(decisions.len(), 2, "必须实际进入子工具的超时审批");
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"].is_string())
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(history.to_string().contains("denied by user"));
        assert_eq!(
            child_test_json(&fixture.app, "GET", "/permission", serde_json::json!({})).await,
            serde_json::json!([])
        );
    }

    #[tokio::test]
    #[ignore = "实际等待 main 既有 180 秒期限；独立集成验收运行"]
    async fn original_main_deadline_cancels_sleeping_child_and_joins_durable_terminal() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-long", "confirm").await;
        let started = std::time::Instant::now();
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("安排子 Agent 等待，验证原始超时取消"))
            .await
            .unwrap();
        let body = tokio::time::timeout(
            Duration::from_secs(185),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("error"));
        assert!(started.elapsed() >= Duration::from_secs(179));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let status = child_test_json(
                    &fixture.app,
                    "GET",
                    "/session/status",
                    serde_json::json!({}),
                )
                .await;
                if status == serde_json::json!({}) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let parent = fixture
            .state
            .main_office
            .record(&fixture.team)
            .unwrap()
            .session
            .unwrap();
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parent)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let child_turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child}/turns"),
            serde_json::json!({}),
        )
        .await;
        let parent_turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{parent}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(child_turns[0]["state"], "cancelled");
        assert_eq!(parent_turns[0]["state"], "cancelled");
        assert!(
            child_turns[0]["completedAt"].as_i64().unwrap()
                <= parent_turns[0]["completedAt"].as_i64().unwrap()
        );
        eprintln!(
            "T05 original timeout evidence: {}",
            serde_json::json!({"elapsedSeconds":started.elapsed().as_secs(),"parentID":parent,"childID":child,"parentTurns":parent_turns,"childTurns":child_turns,"originalDeadlineSeconds":180})
        );
    }

    #[tokio::test]
    async fn explicit_child_retry_is_idempotent_but_same_text_new_request_executes_again() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-resume", "confirm").await;
        let parent = child_test_json(&fixture.app, "POST", "/session", serde_json::json!({})).await
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let initial = serde_json::json!({"task":"读取首轮秘密","name":"李研"}).to_string();
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{parent}/message"),
            serde_json::json!({"text":initial,"requestID":"initial"}),
        )
        .await;
        let list = child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = list
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parent)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        for (key, expected) in [("again", 2), ("again", 2), ("new-request", 3)] {
            let text = serde_json::json!({"task":"读取首轮秘密","task_id":child}).to_string();
            child_test_json(
                &fixture.app,
                "POST",
                &format!("/session/{parent}/message"),
                serde_json::json!({"text":text,"requestID":key}),
            )
            .await;
            let history = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{child}/message"),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(
                history
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|message| message["info"]["role"] == "user")
                    .count(),
                expected
            );
            assert_eq!(
                fixture.model_seen.lock().await.len(),
                expected * 4,
                "原请求重试不得增加真实执行"
            );
        }
        let seen = fixture.model_seen.lock().await;
        assert!(seen
            .iter()
            .filter(|request| request["tools"].to_string().contains("delegate_task"))
            .flat_map(|request| request["messages"].as_array().unwrap())
            .any(|message| message["role"] == "system"
                && message["content"].as_str().is_some_and(|text| text
                    .contains("可继续工作的真实子员工")
                    && text.contains(&child))));
        drop(seen);
        // 无 ID 的同名请求是真正的新身份，不能合并到已经工作三轮的员工。
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{parent}/message"),
            serde_json::json!({"text":initial,"requestID":"new-identity"}),
        )
        .await;
        let list = child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        assert_eq!(
            list.as_array()
                .unwrap()
                .iter()
                .filter(|session| session["parentID"] == parent)
                .count(),
            2
        );
        eprintln!(
            "T02 retry evidence: {}",
            serde_json::json!({"parentID":parent,"continuedChildID":child,"same_text_new_requests":3,"retried_request_added_turns":0,"same_name_new_identity":list})
        );
    }

    #[tokio::test]
    async fn explicit_child_rejects_unknown_cross_parent_archived_deleted_and_display_ids() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-resume", "confirm").await;
        let parent = child_test_json(&fixture.app, "POST", "/session", serde_json::json!({})).await
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        child_test_json(&fixture.app,"POST",&format!("/session/{parent}/message"),serde_json::json!({"text":serde_json::json!({"task":"读取首轮秘密","name":"李研"}).to_string()})).await;
        let list = child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = list
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"] == parent)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let elsewhere = tempfile::tempdir().unwrap();
        let mut other_url = reqwest::Url::parse("http://fixture/session").unwrap();
        other_url
            .query_pairs_mut()
            .append_pair("directory", elsewhere.path().to_str().unwrap());
        let other_path = format!("/session?{}", other_url.query().unwrap());
        let other = child_test_json(&fixture.app, "POST", &other_path, serde_json::json!({})).await
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut targets = vec![
            (parent.clone(), uuid::Uuid::new_v4().to_string(), "不存在"),
            (parent.clone(), parent.clone(), "不是可续办"),
            (other.clone(), child.clone(), "不属于当前父会话"),
            (parent.clone(), format!("{parent}:agent:old"), "不存在"),
        ];
        for (caller, target, error) in targets.drain(..) {
            child_test_json(&fixture.app,"POST",&format!("/session/{caller}/message"),serde_json::json!({"text":serde_json::json!({"task":"拒绝错误身份","task_id":target}).to_string()})).await;
            let history = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{caller}/message"),
                serde_json::json!({}),
            )
            .await;
            assert!(history.to_string().contains(error), "{history}");
        }
        child_test_json(
            &fixture.app,
            "PATCH",
            &format!("/session/{child}"),
            serde_json::json!({"time":{"archived":1}}),
        )
        .await;
        for (error, delete) in [("已归档", false), ("不存在", true)] {
            if delete {
                child_test_json(
                    &fixture.app,
                    "DELETE",
                    &format!("/session/{child}"),
                    serde_json::json!({}),
                )
                .await;
            }
            child_test_json(&fixture.app,"POST",&format!("/session/{parent}/message"),serde_json::json!({"text":serde_json::json!({"task":"拒绝失效身份","task_id":child}).to_string()})).await;
            let history = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{parent}/message"),
                serde_json::json!({}),
            )
            .await;
            assert!(history.to_string().contains(error), "{history}");
            let list =
                child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
            assert_eq!(
                list.as_array().unwrap().len(),
                if delete { 2 } else { 3 },
                "不能静默创建替代身份"
            );
        }
        assert_eq!(
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .filter(|request| !request["tools"].to_string().contains("delegate_task"))
                .count(),
            2,
            "拒绝路径不得调用子模型"
        );
        eprintln!(
            "T02 rejection evidence: {}",
            serde_json::json!({"parentID":parent,"childID":child,"otherParentID":other,"rejected":["unknown","root","cross_parent_and_directory","display_id","archived","deleted"],"child_model_requests":2})
        );
    }

    #[tokio::test]
    async fn cold_employee_four_views_do_not_require_a_prior_session_read() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let mut fixture = main_fixture("delegate-inspect", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派读取 README 并汇报"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let employee = fixture.child_employee().await;
        assert!(
            fixture
                .state
                .shutdown_gracefully(Duration::from_secs(2))
                .await
        );
        let calls = fixture.model_seen.lock().await.len();
        let mut state = AppState::new(fixture.state.config.clone());
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            &fixture.state.config.home,
            &fixture.state.config.workspace,
            Some(
                std::env::var("OPENPAW_OPENAI_BASE_URL")
                    .unwrap()
                    .trim_end_matches("/v1")
                    .into(),
            ),
        );
        state.office.attach_main(&state.main_office);
        fixture.app = build_router(state.clone());
        fixture.state = state;
        let mut current = "cold-open".to_string();
        let mut content=fixture.ui(serde_json::json!({"id":current,"teamId":fixture.team,"action":"employee.inspect","employeeId":employee["id"]})).await;
        assert!(content.to_string().contains("会话编号"), "{content}");
        for label in ["消息", "工具", "状态", "刷新"] {
            let next = format!("cold-{label}");
            content = fixture.button(&current, &next, &content, label).await;
            assert!(content.to_string().contains("会话编号"), "{content}");
            current = next;
        }
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
    }

    #[tokio::test]
    async fn reverted_child_origin_cannot_resume_and_fork_never_inherits_child_authority() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-resume", "confirm").await;
        async fn json(
            app: &Router,
            method: &str,
            path: String,
            body: serde_json::Value,
        ) -> serde_json::Value {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(response.status().is_success());
            serde_json::from_slice(&to_bytes(response.into_body(), 500_000).await.unwrap()).unwrap()
        }
        let parent = json(
            &fixture.app,
            "POST",
            "/session".into(),
            serde_json::json!({}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        json(&fixture.app,"POST",format!("/session/{parent}/message"),serde_json::json!({"text":serde_json::json!({"task":"读取首轮秘密","name":"worker"}).to_string()})).await;
        let sessions = json(
            &fixture.app,
            "GET",
            "/session".into(),
            serde_json::json!({}),
        )
        .await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"] == parent)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let history = json(
            &fixture.app,
            "GET",
            format!("/session/{parent}/message"),
            serde_json::json!({}),
        )
        .await;
        let origin = history[0]["info"]["id"].as_str().unwrap();
        json(
            &fixture.app,
            "POST",
            format!("/session/{parent}/revert"),
            serde_json::json!({"messageID":origin}),
        )
        .await;
        let count = fixture.model_seen.lock().await.len();
        json(&fixture.app,"POST",format!("/session/{parent}/message"),serde_json::json!({"text":serde_json::json!({"task":"continue","task_id":child}).to_string()})).await;
        let requests = fixture.model_seen.lock().await;
        assert!(
            requests[count..].iter().all(|request| request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "delegate_task")),
            "hidden child must not execute"
        );
        assert!(
            requests[count..]
                .iter()
                .any(|request| request["messages"].to_string().contains("父会话可见历史")),
            "must explain detached relation"
        );
        drop(requests);
        let fork = json(
            &fixture.app,
            "POST",
            format!("/session/{parent}/fork"),
            serde_json::json!({}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let count = fixture.model_seen.lock().await.len();
        json(&fixture.app,"POST",format!("/session/{fork}/message"),serde_json::json!({"text":serde_json::json!({"task":"continue","task_id":child}).to_string()})).await;
        assert!(fixture.model_seen.lock().await[count..]
            .iter()
            .all(|request| request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "delegate_task")));
        assert!(
            json(
                &fixture.app,
                "GET",
                format!("/session/{child}/message"),
                serde_json::json!({})
            )
            .await
            .as_array()
            .unwrap()
            .len()
                > 1
        );
    }

    #[tokio::test]
    async fn legacy_snapshot_remains_readonly_with_explicit_missing_history_and_unchanged_bytes() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let mut fixture = main_fixture("tools", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("读取 README"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let sessions = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let sessions: serde_json::Value =
            serde_json::from_slice(&to_bytes(sessions.into_body(), 200_000).await.unwrap())
                .unwrap();
        let parent = sessions[0]["id"].as_str().unwrap();
        let employee = format!("{parent}:agent:legacy");
        let directory = fixture
            .workspace
            .path()
            .join(".openpaw/engine-host-delegations");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{parent}.json"));
        let bytes=serde_json::to_vec(&serde_json::json!({"version":2,"records":[{"logical_key":"legacy","workspace_id":employee,"workspace_name":"历史员工","task_fingerprint":"old","task":"旧任务已完成","completed":{"content":"真实旧结果","structured":{"path":"legacy.txt"}}}]})).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(fixture.workspace.path().join("legacy.txt"), "kept artifact").unwrap();
        assert!(
            fixture
                .state
                .shutdown_gracefully(Duration::from_secs(2))
                .await
        );
        let calls = fixture.model_seen.lock().await.len();
        let mut state = AppState::new(fixture.state.config.clone());
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            &fixture.state.config.home,
            &fixture.state.config.workspace,
            Some(
                std::env::var("OPENPAW_OPENAI_BASE_URL")
                    .unwrap()
                    .trim_end_matches("/v1")
                    .into(),
            ),
        );
        state.office.attach_main(&state.main_office);
        fixture.app = build_router(state.clone());
        fixture.state = state;
        let mut current = "legacy-open".to_string();
        let mut content=fixture.ui(serde_json::json!({"id":current,"teamId":fixture.team,"action":"employee.inspect","employeeId":employee})).await;
        assert!(content.to_string().contains("完整历史未保存"), "{content}");
        assert!(content.to_string().contains("旧任务已完成"));
        for label in ["消息", "工具", "状态", "刷新"] {
            let next = format!("legacy-{label}");
            content = fixture.button(&current, &next, &content, label).await;
            assert!(content.to_string().contains("完整历史未保存"), "{content}");
            if label == "消息" {
                assert!(content.to_string().contains("真实旧结果"));
            }
            current = next;
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("legacy.txt")).unwrap(),
            "kept artifact"
        );
    }

    #[tokio::test]
    async fn employee_inspect_returns_real_child_readonly_overview_without_execution() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-inspect", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派读取 README 并汇报"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let overview = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(value) = fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .find(|message| message["type"] == "ui.respond")
                    .cloned()
                {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(overview["interactionId"], "inspect-real-child");
        assert_eq!(overview["presentation"]["content"]["template"], "detail-v1");
        assert!(overview.to_string().contains("只读"));
        let labels = overview["presentation"]["content"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["label"].as_str().unwrap())
            .collect::<Vec<_>>();
        for expected in ["任务", "消息", "工具", "状态", "刷新"] {
            assert!(
                labels.contains(&expected),
                "员工详情缺少只读视图：{expected}"
            );
        }
        let fields = overview["presentation"]["content"]["fields"]
            .as_array()
            .unwrap();
        let child_id = fields[0]["value"].as_str().unwrap();
        let child = fixture
            .state
            .get(child_id)
            .await
            .expect("概览指向真实子会话");
        assert!(child.child.is_some());
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            4,
            "只读查看不得调用模型或工具"
        );
    }

    #[tokio::test]
    async fn employee_details_keep_failed_child_tool_facts_without_execution() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-child-fail", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派读取文件，失败时保留真实过程"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let child = fixture.child_employee().await;
        let models = fixture.model_seen.lock().await.len();
        assert_eq!(models, 4, "子模型读完文件后失败，父模型接收失败汇总");
        let overview = fixture.ui(serde_json::json!({"id":"failed-open","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        let session = overview["fields"][0]["value"].as_str().unwrap();
        let tools = fixture
            .button("failed-open", "failed-tools", &overview, "工具")
            .await;
        assert_eq!(tools["fields"][0]["value"], session);
        let text = tools["fields"].to_string();
        for expected in [
            "callID=call-0",
            "read_file",
            "README.md",
            "actual tool content",
            "completed",
        ] {
            assert!(
                text.contains(expected),
                "员工详情缺少已发生事实：{expected}"
            );
        }
        assert_eq!(
            text.matches("callID=call-0").count(),
            1,
            "真实调用只展示一次"
        );
        let status = fixture
            .button("failed-tools", "failed-status", &tools, "状态")
            .await;
        assert!(
            status["fields"].to_string().contains("failed"),
            "不能把已完成工具当作成功回合"
        );
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            models,
            "只读查看不产生模型或工具调用"
        );
        eprintln!(
            "T06 employee evidence: {}",
            serde_json::json!({"employeeID":child["id"],"sessionID":session,"tool":"read_file","callID":"call-0","turnState":"failed","toolDisposition":"completed","readonlyModelRequests":0})
        );
    }

    #[tokio::test]
    async fn employee_task_view_keeps_delegation_result_and_turn_identity() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派真实子任务"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let child = fixture.child_employee().await;
        let content = fixture.ui(serde_json::json!({"id":"task-details","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        let text = content["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|field| field["value"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("读取 README.md 并汇报"));
        assert!(text.contains("turnID"));
        assert!(
            text.contains("tool results integrated"),
            "任务详情必须包含这次实际委派结果"
        );
    }

    #[tokio::test]
    async fn employee_details_page_complete_frozen_history_and_refresh_without_execution() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-details", "confirm").await;
        let long_tool = format!("工具原文开始|{}|工具原文结束", "段落🙂".repeat(12_000));
        std::fs::write(fixture.workspace.path().join("README.md"), &long_tool).unwrap();
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派读取长文并汇报"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let child = fixture.child_employee().await;
        let mut page = fixture.ui(serde_json::json!({"id":"long-open","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        page = fixture
            .button("long-open", "long-messages", &page, "消息")
            .await;
        let session = page["fields"][0]["value"].as_str().unwrap().to_string();
        let mut current = "long-messages".to_string();
        let initial_snapshot = page["description"]
            .as_str()
            .unwrap()
            .split(" · 第")
            .next()
            .unwrap()
            .to_string();
        let first_fields = page["fields"].clone();
        let second = fixture
            .button(&current, "long-peek-next", &page, "下一页")
            .await;
        page = fixture
            .button("long-peek-next", "long-return-first", &second, "上一页")
            .await;
        current = "long-return-first".into();
        assert_eq!(page["fields"], first_fields, "上一页还原相同冻结内容");
        // 使用公开会话入口追加新回合，验证已打开快照的后续页仍是原来的历史边界。
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{session}/message"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"parts":[{"type":"text","text":"LATER-ONLY-MARKER"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 500_000).await.unwrap();
        let model_count = fixture.model_seen.lock().await.len();
        let mut reconstructed = String::new();
        for number in 0..20 {
            assert!(page["fields"].as_array().unwrap().len() <= 30);
            assert!(page["actions"].as_array().unwrap().len() <= 8);
            assert!(page["description"]
                .as_str()
                .unwrap()
                .starts_with(&initial_snapshot));
            for field in page["fields"].as_array().unwrap().iter().skip(3) {
                let value = field["value"].as_str().unwrap();
                assert!(value.encode_utf16().count() <= 2000);
                reconstructed.push_str(value);
            }
            if !page["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["label"] == "下一页")
            {
                break;
            }
            let next = format!("long-next-{number}");
            page = fixture.button(&current, &next, &page, "下一页").await;
            current = next;
        }
        let long_answer = format!("子结果开始|{}|子结果结束", "🙂任务".repeat(12_000));
        assert_eq!(
            reconstructed.matches(&long_answer).count(),
            1,
            "完整长消息仅出现一次，无漏页或重复"
        );
        assert!(!reconstructed.contains("LATER-ONLY-MARKER"));
        let mut refreshed = fixture
            .button(&current, "long-refresh", &page, "刷新")
            .await;
        assert!(refreshed["description"].as_str().unwrap().contains("第 1/"));
        current = "long-refresh".into();
        let mut fresh_text = String::new();
        for number in 0..30 {
            for field in refreshed["fields"].as_array().unwrap().iter().skip(3) {
                fresh_text.push_str(field["value"].as_str().unwrap());
            }
            if !refreshed["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["label"] == "下一页")
            {
                break;
            }
            let next = format!("fresh-next-{number}");
            refreshed = fixture.button(&current, &next, &refreshed, "下一页").await;
            current = next;
        }
        assert_eq!(
            fresh_text.matches("LATER-ONLY-MARKER").count(),
            1,
            "手动刷新必须看到真实新增记录"
        );
        let mut tools = fixture
            .button(&current, "long-tools", &refreshed, "工具")
            .await;
        let mut tool_text = String::new();
        current = "long-tools".into();
        for number in 0..30 {
            for field in tools["fields"].as_array().unwrap().iter().skip(3) {
                tool_text.push_str(field["value"].as_str().unwrap());
            }
            if !tools["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["label"] == "下一页")
            {
                break;
            }
            let next = format!("tool-next-{number}");
            tools = fixture.button(&current, &next, &tools, "下一页").await;
            current = next;
        }
        assert!(tool_text.contains(&long_tool), "工具结果不被字段上限截断");
        assert!(
            tool_text.contains("callID=call-0")
                && tool_text.contains("read_file")
                && tool_text.contains("completed")
        );
        assert!(tool_text.contains("输入：") && tool_text.contains("README.md"));
        let status = fixture
            .button(&current, "long-status", &tools, "状态")
            .await;
        assert!(status.to_string().contains("更新时间"));
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            model_count,
            "视图/翻页/刷新不得发起模型或工具"
        );
    }

    #[tokio::test]
    async fn employee_details_reject_cross_office_employee_and_replayed_buttons() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派任务并返回"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let child = fixture.child_employee().await;
        let child_view = fixture.ui(serde_json::json!({"id":"secure-child","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        let child_session = child_view["fields"][0]["value"].clone();
        let actor_view = fixture.ui(serde_json::json!({"id":"secure-actor","teamId":fixture.team,"action":"actor.inspect","actorId":child["sceneActorId"]})).await;
        assert_eq!(
            actor_view["fields"][0]["value"], child_session,
            "员工/人物定位同一真实子会话"
        );
        let parent = fixture.ui(serde_json::json!({"id":"secure-parent","teamId":fixture.team,"action":"actor.inspect","actorId":"office-lead"})).await;
        assert_ne!(
            parent["fields"][0]["value"], child_session,
            "主员工不冒用子会话"
        );
        let token = child_view["actions"][0]["id"].clone();
        let invalid = fixture.ui(serde_json::json!({"id":"wrong-employee","teamId":fixture.team,"action":"dialog.action","interactionId":"secure-parent","optionId":token})).await;
        assert!(invalid["description"].as_str().unwrap().contains("跨员工"));
        let invalid = fixture.ui(serde_json::json!({"id":"wrong-office","teamId":"office-other","action":"dialog.action","interactionId":"secure-child","optionId":token})).await;
        assert!(invalid["description"]
            .as_str()
            .unwrap()
            .contains("办公室不匹配"));
        let valid = fixture
            .button("secure-child", "secure-next", &child_view, "消息")
            .await;
        assert_eq!(valid["fields"][0]["value"], child_session);
        let replay = fixture
            .button("secure-child", "secure-replay", &child_view, "消息")
            .await;
        assert!(replay["description"].as_str().unwrap().contains("过期"));
        let forged = fixture.ui(serde_json::json!({"id":"forged","teamId":fixture.team,"action":"dialog.action","interactionId":"never-issued","optionId":"detail-fake"})).await;
        assert!(forged["actions"].as_array().unwrap().is_empty());
        let unbound = fixture.ui(serde_json::json!({"id":"unbound-actor","teamId":fixture.team,"action":"actor.inspect","actorId":"data-agent"})).await;
        assert!(unbound["description"]
            .as_str()
            .unwrap()
            .contains("尚未绑定"));
        assert_eq!(fixture.model_seen.lock().await.len(), 4);
    }

    #[tokio::test]
    async fn employee_details_expire_and_evict_old_associations_without_work() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派任务并返回"))
            .await
            .unwrap();
        to_bytes(response.into_body(), 200_000).await.unwrap();
        let child = fixture.child_employee().await;
        let first = fixture.ui(serde_json::json!({"id":"capacity-0","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        for index in 1..=64 {
            fixture.ui(serde_json::json!({"id":format!("capacity-{index}"),"teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        }
        let evicted = fixture
            .button("capacity-0", "capacity-evicted", &first, "消息")
            .await;
        assert!(evicted["description"].as_str().unwrap().contains("过期"));
        let fresh = fixture.ui(serde_json::json!({"id":"expiry-open","teamId":fixture.team,"action":"employee.inspect","employeeId":child["id"]})).await;
        // 沿公开边界验证真实60秒能力期限，不读取私有缓存或改动运行时时钟。
        tokio::time::sleep(Duration::from_secs(61)).await;
        let expired = fixture
            .button("expiry-open", "expiry-after", &fresh, "消息")
            .await;
        assert!(expired["description"].as_str().unwrap().contains("过期"));
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            4,
            "查看/回收无执行副作用"
        );
    }

    #[tokio::test]
    async fn main_alone_delegates_real_child_and_projects_one_assignment_and_return() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("delegate actual reading"))
            .await
            .unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("parent integrated"));
        let record = fixture.state.main_office.record(&fixture.team).unwrap();
        assert_eq!(record.snapshot["employees"].as_array().unwrap().len(), 2);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let seen = fixture.seen.lock().await;
        let assignments: Vec<_> = seen
            .iter()
            .flat_map(|message| {
                message["snapshot"]["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter(|event| event["kind"] == "task.assigned")
            .collect();
        assert_eq!(assignments.len(), 1);
        assert!(seen
            .iter()
            .any(|message| message["command"]["capability"] == "office.visit"
                && message["command"]["participants"][1]["entityId"] == "office-lead"));
        assert_eq!(fixture.model_seen.lock().await.len(), 4);
    }

    #[tokio::test]
    async fn main_new_delegations_each_execute_and_handoff_even_for_identical_tasks() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-repeat", "confirm").await;
        let mut history = Vec::<serde_json::Value>::new();
        for text in ["task-a", "task-b", "task-b"] {
            history.push(serde_json::json!({"role":"user","content":text}));
            let request = Request::builder().method("POST").uri("/api/chat").header("content-type","application/json")
                .body(Body::from(serde_json::json!({"version":"1.0","teamId":fixture.team,"conversationId":"fixture","messages":history}).to_string())).unwrap();
            let response = fixture.app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 200_000).await.unwrap();
            let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events.last().unwrap()["type"], "done");
            let answer: String = events
                .iter()
                .filter(|event| event["type"] == "delta")
                .map(|event| event["text"].as_str().unwrap())
                .collect();
            history.push(serde_json::json!({"role":"assistant","content":answer}));
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        let record = fixture.state.main_office.record(&fixture.team).unwrap();
        assert_eq!(record.snapshot["tasks"].as_array().unwrap().len(), 3);
        assert_eq!(record.snapshot["employees"].as_array().unwrap().len(), 4);
        let seen = fixture.seen.lock().await;
        let events: Vec<_> = seen
            .iter()
            .flat_map(|message| {
                message["snapshot"]["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["kind"] == "task.assigned")
                .count(),
            3
        );
        assert_eq!(
            seen.iter()
                .filter(|message| message["command"]["capability"] == "office.visit")
                .count(),
            3
        );
        assert_eq!(fixture.model_seen.lock().await.len(), 12);
    }

    #[tokio::test]
    async fn main_single_tool_decision_approves_once_and_http_200_reject_never_writes() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for choice in ["confirm", "reject"] {
            let fixture = main_fixture("write", choice).await;
            let response = fixture
                .app
                .clone()
                .oneshot(fixture.chat("write with approval"))
                .await
                .unwrap();
            let bytes = tokio::time::timeout(
                Duration::from_secs(3),
                to_bytes(response.into_body(), 200_000),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(std::str::from_utf8(&bytes)
                .unwrap()
                .contains("\"type\":\"done\""));
            assert_eq!(
                fixture.workspace.path().join("approved.txt").exists(),
                choice == "confirm"
            );
            let seen = fixture.seen.lock().await;
            let decisions: Vec<_> = seen
                .iter()
                .filter(|message| message["type"] == "decision.request")
                .collect();
            assert_eq!(decisions.len(), 1);
            assert_eq!(decisions[0]["decision"]["confirmLabel"], "允许本次");
            assert_eq!(decisions[0]["decision"]["rejectLabel"], "拒绝");
            let requests = fixture.model_seen.lock().await;
            assert_eq!(requests.len(), 2);
            let result = requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "tool")
                .unwrap();
            assert!(!result["content"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn main_parent_approval_does_not_authorize_child_and_disconnect_cancels_both() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-write", "child-pending").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("delegate controlled write"))
            .await
            .unwrap();
        let decision = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let seen = fixture.seen.lock().await;
                let decisions: Vec<_> = seen
                    .iter()
                    .filter(|message| message["type"] == "decision.request")
                    .cloned()
                    .collect();
                if decisions.len() == 2 {
                    break decisions;
                }
                drop(seen);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_ne!(decision[0]["decision"]["id"], decision[1]["decision"]["id"]);
        assert_eq!(decision[0]["decision"]["actorId"], "office-lead");
        assert_ne!(decision[1]["decision"]["actorId"], "office-lead");
        assert!(!fixture.workspace.path().join("approved.txt").exists());
        let pending = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/permission")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let pending: serde_json::Value =
            serde_json::from_slice(&to_bytes(pending.into_body(), 200_000).await.unwrap()).unwrap();
        let items = pending.as_array().unwrap();
        assert_eq!(items.len(), 1, "父聚合及子会话读取不能重复返回相同审批");
        let owner = items[0]["sessionID"].as_str().unwrap();
        let owner_handle = fixture
            .state
            .get(owner)
            .await
            .expect("审批实际 owner 必须是可读子会话");
        assert!(owner_handle.child.is_some());
        assert_ne!(
            Some(owner),
            fixture
                .state
                .main_office
                .record(&fixture.team)
                .unwrap()
                .session
                .as_deref()
        );
        drop(response);
        let record = fixture.state.main_office.record(&fixture.team).unwrap();
        let id = record.session.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = fixture
                    .app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/session/status")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let value: serde_json::Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 200_000).await.unwrap())
                        .unwrap();
                if value[&id]["type"] != "busy" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let child_turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{owner}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            child_turns[0]["state"], "cancelled",
            "父终态发布前，子执行必须保存取消终态"
        );
        assert!(child_turns[0]["completedAt"].is_number());
        assert!(!owner_handle.is_active(), "子不是仅被清掉人物状态");
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/permission")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let permissions: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 200_000).await.unwrap())
                .unwrap();
        assert_eq!(permissions, serde_json::json!([]));
        let late = decision[1]["decision"]["id"].as_str().unwrap();
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/permission/{late}/reply"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"reply":"once"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!fixture.workspace.path().join("approved.txt").exists());
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/session/{id}/turns"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let turns: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 200_000).await.unwrap())
                .unwrap();
        assert!(turns.to_string().contains("cancelled"));
        assert!(fixture.home.path().join("offices/registry.json").exists());
    }

    #[tokio::test]
    async fn main_restart_rebuilds_current_snapshot_from_committed_session_facts_without_actions() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("recover", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("delegate before projection crash"))
            .await
            .unwrap();
        let registry = fixture.home.path().join("offices/registry.json");
        let checkpoint = std::fs::read(&registry).unwrap();
        // 模拟事实已提交而业务投影发布失败。保留绑定检查点，不改会话历史。
        std::fs::remove_file(&registry).unwrap();
        std::fs::create_dir(&registry).unwrap();
        fixture.release_model.notify_one();
        let bytes = tokio::time::timeout(
            Duration::from_secs(3),
            to_bytes(response.into_body(), 200_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"type\":\"done\""));
        std::fs::remove_dir(&registry).unwrap();
        std::fs::write(&registry, checkpoint).unwrap();
        let config = fixture.state.config.clone();
        let recovered = AppState::new(config);
        let record = recovered.main_office.record(&fixture.team).unwrap();
        assert_eq!(record.snapshot["employees"].as_array().unwrap().len(), 2);
        assert!(record.snapshot["employees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|employee| employee["online"] == false));
        assert!(record.snapshot["events"].as_array().unwrap().is_empty());
        assert_eq!(record.snapshot["tasks"][0]["status"], "completed");
        assert!(record.snapshot["seq"].as_u64().unwrap() > 1);
        assert_eq!(fixture.model_seen.lock().await.len(), 4);
    }

    #[tokio::test]
    async fn main_gateway_proxy_preserves_stream_and_separates_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let gateway = Router::new()
            .route(
                "/office/",
                get(|headers: HeaderMap| async move {
                    assert!(headers.get(header::AUTHORIZATION).is_none());
                    ([(header::CONTENT_TYPE, "text/html")], "original main")
                }),
            )
            .route(
                "/api/pawoffice/events",
                get(|headers: HeaderMap| async move {
                    assert!(headers.get(header::AUTHORIZATION).is_none());
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"type\":\"office.sync.start\"}\n\n",
                    )
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, gateway).await.unwrap();
        });
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let config = HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            username: Some("user".into()),
            password: Some("password".into()),
            ..Default::default()
        };
        let mut state = AppState::new(config.clone());
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            &config.home,
            &config.workspace,
            Some(format!("http://{address}")),
        );
        let app = build_router(state);
        assert_eq!(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri("/office/")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        for path in ["/office/", "/api/pawoffice/events?workspace=1"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let content = to_bytes(response.into_body(), 1000).await.unwrap();
            assert!(!content.is_empty());
        }
        task.abort();
    }

    #[tokio::test]
    async fn main_wire_distinguishes_cached_ignored_timeout_and_invalid_identity() {
        use std::collections::VecDeque;
        let cases = vec![
            (
                serde_json::json!({"type":"office.snapshot","teamId":"a","snapshot":{"seq":4}}),
                202,
                serde_json::json!({"teamId":"a","seq":4,"status":"cached"}),
                true,
            ),
            (
                serde_json::json!({"type":"office.snapshot","teamId":"a","snapshot":{"seq":4}}),
                200,
                serde_json::json!({"teamId":"a","seq":5,"status":"ignored","reasonCode":"STALE_SEQ"}),
                true,
            ),
            (
                serde_json::json!({"type":"office.snapshot","teamId":"a","snapshot":{"seq":4}}),
                200,
                serde_json::json!({"teamId":"b","seq":4,"status":"applied"}),
                false,
            ),
            (
                serde_json::json!({"type":"ui.next","timeoutMs":1000}),
                200,
                serde_json::json!({"actions":[]}),
                true,
            ),
            (
                serde_json::json!({"type":"ui.respond","interactionId":"ui-1"}),
                200,
                serde_json::json!({"interactionId":"ui-1","delivered":true}),
                true,
            ),
            (
                serde_json::json!({"type":"decision.request","teamId":"a","decision":{"id":"d"}}),
                408,
                serde_json::json!({"reasonCode":"REQUEST_TIMEOUT","details":{},"issues":[]}),
                true,
            ),
            (
                serde_json::json!({"type":"decision.request","teamId":"a","decision":{"id":"d"}}),
                200,
                serde_json::json!({"teamId":"a","decisionId":"other","choiceId":"confirm","label":"允许本次"}),
                false,
            ),
            (
                serde_json::json!({"type":"ui.next"}),
                200,
                serde_json::json!({"actions":null}),
                false,
            ),
        ];
        let replies = Arc::new(tokio::sync::Mutex::new(
            cases
                .iter()
                .map(|(_, code, data, _)| (*code, data.clone()))
                .collect::<VecDeque<_>>(),
        ));
        let gateway = Router::new().route(
            "/api/pawoffice/messages",
            post(move || {
                let replies = replies.clone();
                async move {
                    let (code, data) = replies.lock().await.pop_front().unwrap();
                    (
                        StatusCode::from_u16(code).unwrap(),
                        Json(serde_json::json!({"code":code,"msg":"fixture","data":data})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, gateway).await.unwrap() });
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let office = crate::office::MainOffice::new(
            home.path(),
            workspace.path(),
            Some(format!("http://{address}")),
        );
        for (message, code, data, valid) in cases {
            let result = office.message(message, Duration::from_secs(1)).await;
            if valid {
                assert_eq!(result.unwrap(), (code, data));
            } else {
                assert!(result.is_err());
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn main_first_binding_race_rejects_extra_conversations_and_isolates_workspaces() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("recover", "confirm").await;
        let make_request = |conversation: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                "version":"1.0","teamId":fixture.team,"conversationId":conversation,
                "messages":[{"role":"user","content":"work"}]})
                    .to_string(),
                ))
                .unwrap()
        };
        let (left, right) = tokio::join!(
            fixture.app.clone().oneshot(make_request("left")),
            fixture.app.clone().oneshot(make_request("right"))
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(
            [left.status(), right.status()]
                .into_iter()
                .filter(|status| *status == StatusCode::OK)
                .count(),
            1
        );
        let (winner, loser) = if left.status() == StatusCode::OK {
            (left, right)
        } else {
            (right, left)
        };
        assert_eq!(loser.status(), StatusCode::CONFLICT);
        let binding = fixture.state.main_office.record(&fixture.team).unwrap();
        assert_eq!(
            std::fs::read_dir(fixture.home.path().join("sessions"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl"))
                .count(),
            1
        );
        let duplicate = fixture
            .app
            .clone()
            .oneshot(make_request(binding.conversation.as_ref().unwrap()))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        let other = tempdir().unwrap();
        fixture.state.main_office.register(other.path()).unwrap();
        let second = fixture
            .state
            .main_office
            .directory()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot["workspace"]["id"] != fixture.team)
            .unwrap();
        let second_team = second["workspace"]["id"].as_str().unwrap();
        // 使用同一 AppState 的公开 OpenWork 会话入口，确认未绑定会话不会继承 main 历史。
        let independent = fixture
            .state
            .create_session(
                Some("echo"),
                None,
                Some(other.path().into()),
                Some("OpenWork"),
            )
            .await
            .unwrap();
        assert!(main_history(&independent).is_empty());
        assert!(fixture
            .state
            .main_office
            .record(second_team)
            .unwrap()
            .conversation
            .is_none());
        assert_ne!(Some(independent.id.clone()), binding.session);
        fixture.release_model.notify_one();
        let bytes = to_bytes(winner.into_body(), 200_000).await.unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"type\":\"done\""));
        assert!(fixture
            .state
            .main_office
            .record(second_team)
            .unwrap()
            .snapshot["employees"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn main_chat_keeps_context_and_binding_after_restart() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let config = HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            provider: "echo".into(),
            ..Default::default()
        };
        let mut state = AppState::new(config.clone());
        Arc::get_mut(&mut state).unwrap().main_office =
            crate::office::MainOffice::new(&config.home, &config.workspace, None);
        let team = state.main_office.directory().unwrap()[0]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let app = build_router(state);
        let chat = |conversation: &str, messages: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"version":"1.0", "teamId":team,
                    "conversationId":conversation,"messages":messages})
                    .to_string(),
                ))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(chat(
                "first",
                serde_json::json!([
            {"role":"user","content":"hello"}]),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-ndjson"
        );
        let bytes = to_bytes(response.into_body(), 200_000).await.unwrap();
        let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.last().unwrap()["type"], "done");
        let answer: String = events
            .iter()
            .filter(|e| e["type"] == "delta")
            .map(|e| e["text"].as_str().unwrap())
            .collect();
        assert!(!answer.is_empty());
        let restarted = build_router(AppState::new(config));
        let response = restarted
            .clone()
            .oneshot(chat(
                "other",
                serde_json::json!([
            {"role":"user","content":"new"}]),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(String::from_utf8(
            to_bytes(response.into_body(), 200_000)
                .await
                .unwrap()
                .to_vec()
        )
        .unwrap()
        .contains("本轮暂不支持新对话"));
        let response = restarted
            .clone()
            .oneshot(chat(
                "first",
                serde_json::json!([
            {"role":"user","content":"hello"},{"role":"assistant","content":answer},
            {"role":"user","content":"hello"}]),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 200_000).await.unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"type\":\"done\""));
        let response = restarted
            .clone()
            .oneshot(chat(
                "first",
                serde_json::json!([
            {"role":"user","content":"changed"},{"role":"assistant","content":"wrong"},
            {"role":"user","content":"next"}]),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response = restarted
            .oneshot(
                Request::builder()
                    .uri("/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let sessions: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 200_000).await.unwrap())
                .unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn client_message_identity_survives_replay_and_restart() {
        let home = tempdir().unwrap();
        let config = HostConfig {
            home: home.path().into(),
            provider: "echo".into(),
            ..Default::default()
        };
        let app = build_router(AppState::new(config.clone()));
        let id = create_session_id(&app).await;
        let body = r#"{"messageID":"msg_client_test","text":"synthetic identity check"}"#;
        let request = |method: &str, path: &str, body: &str| {
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(request("POST", &format!("/session/{id}/message"), body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let turn = response.headers()[OPENPAW_TURN_ID].clone();
        for router in [app.clone(), build_router(AppState::new(config))] {
            let response = router
                .clone()
                .oneshot(request(
                    "POST",
                    &format!("/session/{id}/prompt_async"),
                    body,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(response.headers()[OPENPAW_TURN_ID], turn);
            let response = router
                .clone()
                .oneshot(request(
                    "GET",
                    &format!("/session/{id}/message/msg_client_test"),
                    "",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let value: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(value["info"]["id"], "msg_client_test");
            assert!(value["info"]["time"]["created"].as_i64().unwrap() > 0);
            let response = router
                .clone()
                .oneshot(request(
                    "POST",
                    &format!("/session/{id}/message"),
                    r#"{"messageID":"msg_client_test","text":"different"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let response = router
                .oneshot(request(
                    "GET",
                    &format!("/session/{id}/message/missing"),
                    "",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let event = crate::events::user_message_updated(&id, "msg_client_test");
        assert_eq!(event.properties["info"]["id"], "msg_client_test");
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn sse_lag_emits_gap_once_then_closes() {
        let (sender, receiver) = broadcast::channel(1);
        let shutdown = CancellationToken::new();
        let _ = sender.send(OpenCodeEvent::new("first", serde_json::json!({})));
        let _ = sender.send(OpenCodeEvent::new("second", serde_json::json!({})));
        let stream = sse_stream(receiver, shutdown);
        futures::pin_mut!(stream);

        let gap = stream.next().await.expect("gap event").expect("sse event");
        let rendered = format!("{gap:?}");
        assert!(rendered.contains("stream.gap"));
        assert!(rendered.contains("global"));
        assert!(stream.next().await.is_none());
    }

    async fn create_session_id(app: &Router) -> String {
        create_session_id_with_body(app, r#"{}"#).await
    }

    async fn create_session_id_with_body(app: &Router, body: &'static str) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn create_session_accepts_empty_sdk_post_without_content_type() {
        let home = tempdir().expect("home");
        let app = build_router(AppState::new(HostConfig {
            home: home.path().into(),
            provider: "mock".into(),
            ..Default::default()
        }));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(session["id"].as_str().is_some());
    }

    #[tokio::test]
    async fn prompt_model_provider_is_used_and_text_parts_are_concatenated() {
        let home = tempdir().expect("home");
        let app = build_router(AppState::new(HostConfig {
            home: home.path().into(),
            provider: "mock".into(),
            ..Default::default()
        }));
        let id = create_session_id(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{id}/message"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":{"providerID":"echo","modelID":"echo-custom"},"parts":[{"type":"text","text":"你"},{"type":"image","text":"ignored"},{"type":"text","text":"好"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/session/{id}/message"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let messages: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let assistant = messages
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["info"]["role"] == "assistant")
            .expect("assistant response");
        assert_eq!(assistant["parts"][0]["text"], "Echo: 你好");

        // 单回合模型覆盖不得修改会话持久化的 provider/model，也不得影响后续摘要。
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let summaries: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let summary = summaries
            .as_array()
            .unwrap()
            .iter()
            .find(|summary| summary["id"] == id)
            .expect("session summary");
        assert_eq!(summary["provider"], "mock");
        assert_eq!(summary["model"], "mock-1");
    }

    #[tokio::test]
    async fn required_downgrade_and_v2_route_matrix_is_explicit() {
        let home = tempdir().expect("home");
        let app = build_router(AppState::new(HostConfig {
            home: home.path().into(),
            ..Default::default()
        }));
        let id = create_session_id(&app).await;
        for route in [
            "/global/health",
            "/session",
            "/session/status",
            "/provider",
            "/permission",
            "/config",
            "/mcp",
            "/command",
            "/project",
            "/agent",
            "/find/file",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "GET {route}");
        }
        for route in [
            format!("/session/{id}"),
            format!("/session/{id}/message"),
            format!("/session/{id}/todo"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(&route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "GET {route}");
        }
        // 降级路由只在 session 作用域；fork 已升级为真实分支。
        for route in [
            format!("/session/{id}/command"),
            format!("/session/{id}/shell"),
            format!("/session/{id}/fork"),
            format!("/session/{id}/summarize"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&route)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if route.ends_with("/fork") {
                assert_eq!(response.status(), StatusCode::OK, "POST {route}");
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_ne!(session["id"], id);
                assert!(session.get("parentID").is_none());
            } else {
                assert_eq!(response.status(), StatusCode::NO_CONTENT, "POST {route}");
            }
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{id}/revert"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{id}/unrevert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // §3.2 的 `/command` 只有 GET → []；根路径不存在会话作用域操作。
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/command")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "POST /command"
        );
        for route in ["/shell", "/fork", "/revert", "/unrevert", "/summarize"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(route)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "POST {route}");
        }
        for route in [
            "/api/health",
            "/opencode2/session",
            "/session/execution/status",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {route}");
        }
        // 三条路径提供同一个全局 SSE；不存在第四条（含 session 作用域）。
        for route in ["/event", "/global/event", "/api/event"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "GET {route}");
            assert_eq!(response.headers()["content-type"], "text/event-stream");
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/session/{id}/event"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "GET /session/{{id}}/event"
        );
    }

    // 场景同时表达能力、媒体以及取得/ACK预期，避免数字范围隐藏分支含义。
    #[derive(Clone, Copy, Debug)]
    enum VisualScenario {
        Windows,
        Png,
        MissingImageCapability,
        TextOnly,
        WrongMime,
        InvalidPng,
        Structure,
    }
    impl VisualScenario {
        fn capture(self) -> bool {
            !matches!(self, Self::Windows | Self::Structure)
        }
        fn expected_calls(self) -> (usize, usize) {
            match self {
                Self::MissingImageCapability | Self::TextOnly => (0, 0),
                Self::WrongMime | Self::InvalidPng => (1, 0),
                Self::Windows | Self::Png | Self::Structure => (1, 1),
            }
        }
        fn expected_error(self) -> Option<&'static str> {
            match self {
                Self::MissingImageCapability | Self::TextOnly => Some("image"),
                Self::WrongMime | Self::InvalidPng => Some("PNG"),
                Self::Windows | Self::Png | Self::Structure => None,
            }
        }
    }

    #[tokio::test]
    async fn registered_visual_tool_reaches_model_with_trusted_turn_and_exact_parsed_ack() {
        visual_roundtrip("ai_pc_list_windows",serde_json::json!({}),"windows",serde_json::json!({"kind":"untrusted_ui_data","windows":[{"id":"native-v1","app":"fixture.exe","process_id":42,"title":"本次窗口"}]}), "127.0.0.1").await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn registered_native_windows_host_uses_desktop_docker_alias_without_a_proxy() {
        visual_roundtrip("ai_pc_list_windows",serde_json::json!({}),"windows",serde_json::json!({"kind":"untrusted_ui_data","windows":[{"id":"native-v1","app":"fixture.exe","process_id":42,"title":"本次窗口"}]}), "host.docker.internal").await;
    }

    #[tokio::test]
    async fn registered_structure_tool_reaches_model_with_trusted_turn_and_exact_parsed_ack() {
        visual_roundtrip("ai_pc_query_structure",serde_json::json!({"generation":"native","target_id":"native-v1","directions":"children,parent","offset":2,"limit":5000,"boxes":true}),"structure",serde_json::json!({"kind":"untrusted_ui_data","observation":"live_structure","controls":[{"id":"native-v1","name":"本次控件","box":[-10,20,40,50]}]}), "127.0.0.1").await;
    }

    async fn visual_roundtrip(
        name: &'static str,
        args: serde_json::Value,
        path: &str,
        projection: serde_json::Value,
        callback_host: &str,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _env = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let scenarios: &[VisualScenario] = if name == "ai_pc_query_structure" {
            &[VisualScenario::Structure]
        } else {
            &[
                VisualScenario::Windows,
                VisualScenario::Png,
                VisualScenario::MissingImageCapability,
                VisualScenario::TextOnly,
                VisualScenario::WrongMime,
                VisualScenario::InvalidPng,
            ]
        };
        for &scenario in scenarios {
            let capture = scenario.capture();
            let tool_name = if capture {
                "ai_pc_capture_target"
            } else {
                name
            };
            let tool_args = if capture {
                r#"{"target_id":"native-v1"}"#.to_string()
            } else {
                args.to_string()
            };
            let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLttAAAAABJRU5ErkJggg==";
            let calls = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let model_seen = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let model_calls = Arc::new(AtomicUsize::new(0));
            let seen = model_seen.clone();
            let count = model_calls.clone();
            let model = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
            let (seen, count, tool_args) = (seen.clone(), count.clone(), tool_args.clone());
            async move {
                seen.lock().await.push(body);
                let chunk = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"provider-list","type":"function","function":{"name":tool_name,"arguments":tool_args}}]},"finish_reason":"tool_calls"}]})
                } else { serde_json::json!({"choices":[{"delta":{"content":"已取得窗口"},"finish_reason":"stop"}]}) };
                ([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            }
        }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
            let model_task = tokio::spawn(async move {
                axum::serve(listener, model).await.unwrap();
            });
            let observed = calls.clone();
            let saved_projection = if capture {
                serde_json::json!({"kind":"untrusted_ui_data","target_id":"native-v1"})
            } else {
                projection.clone()
            };
            let bridge_projection = saved_projection.clone();
            let bridge = Router::new().route(&format!("/ai-pc/{}", if capture { "capture" } else { path }),post(move |Json(body): Json<serde_json::Value>| {
            let (observed,projection) = (observed.clone(), bridge_projection.clone());
            async move { observed.lock().await.push(body.clone()); let mut value = serde_json::json!({"protocol_version":1,"invocation_id":body["invocation_id"],"delivery_id":"delivery-fixture","status":"ok","projection":projection}); if capture { value["media"] = serde_json::json!([{"id":"att_fixture","mime":if matches!(scenario,VisualScenario::WrongMime){"image/jpeg"}else{"image/png"},"data":if matches!(scenario,VisualScenario::InvalidPng){"AAAA"}else{png},"filename":"target.png"}]); } Json(value) }
        }));
            let observed = calls.clone();
            let bridge = bridge.route(
                "/ai-pc/ack",
                post(move |Json(body): Json<serde_json::Value>| {
                    let observed = observed.clone();
                    async move {
                        observed.lock().await.push(body);
                        Json(serde_json::json!({"status":"accepted"}))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bridge_url = format!(
                "http://{callback_host}:{}/ai-pc",
                listener.local_addr().unwrap().port()
            );
            let bridge_task = tokio::spawn(async move {
                axum::serve(listener, bridge).await.unwrap();
            });
            let old_key = std::env::var_os("OPENAI_API_KEY");
            let old_url = std::env::var_os("OPENPAW_OPENAI_BASE_URL");
            let old_capabilities = std::env::var_os("OPENPAW_MODEL_CAPABILITIES");
            if matches!(scenario, VisualScenario::MissingImageCapability) {
                std::env::remove_var("OPENPAW_MODEL_CAPABILITIES");
            } else {
                std::env::set_var(
                    "OPENPAW_MODEL_CAPABILITIES",
                    if matches!(scenario, VisualScenario::TextOnly) {
                        r#"{"openai":{"fixture-model":{"input":["text"]}}}"#
                    } else {
                        r#"{"openai":{"fixture-model":{"input":["text","image"]}}}"#
                    },
                );
            }
            std::env::set_var("OPENAI_API_KEY", "test-key");
            std::env::set_var("OPENPAW_OPENAI_BASE_URL", model_url);
            let home = tempfile::tempdir().unwrap();
            let state = AppState::new(HostConfig {
                home: home.path().into(),
                provider: "openai".into(),
                username: Some("fixture".into()),
                password: Some("secret".into()),
                ..Default::default()
            });
            let handle = state
                .create_session(None, Some("fixture-model"), None, None)
                .await
                .unwrap();
            let router = build_router(state);
            let registered = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!("/session/{}/ai-pc", handle.id))
                        .header("content-type", "application/json")
                        .header("authorization", "Basic Zml4dHVyZTpzZWNyZXQ=")
                        .body(Body::from(
                            serde_json::json!({"bridge_url":bridge_url,"token":"a".repeat(64)})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(registered.status(), StatusCode::NO_CONTENT);
            let response = router.clone().oneshot(Request::builder().method("POST").uri(format!("/session/{}/message",handle.id)).header("content-type","application/json").header("authorization","Basic Zml4dHVyZTpzZWNyZXQ=").body(Body::from(r#"{"requestID":"trusted-request","parts":[{"type":"text","text":"列出窗口"}]}"#)).unwrap()).await.unwrap();
            let status = response.status();
            let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            match old_key {
                Some(v) => std::env::set_var("OPENAI_API_KEY", v),
                None => std::env::remove_var("OPENAI_API_KEY"),
            }
            match old_url {
                Some(v) => std::env::set_var("OPENPAW_OPENAI_BASE_URL", v),
                None => std::env::remove_var("OPENPAW_OPENAI_BASE_URL"),
            }
            match old_capabilities {
                Some(v) => std::env::set_var("OPENPAW_MODEL_CAPABILITIES", v),
                None => std::env::remove_var("OPENPAW_MODEL_CAPABILITIES"),
            }
            model_task.abort();
            bridge_task.abort();
            assert_eq!(status, StatusCode::OK);
            let calls = calls.lock().await;
            let (acquired, acknowledged) = scenario.expected_calls();
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.get("args").is_some())
                    .count(),
                acquired,
                "{scenario:?}: native acquisitions"
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.get("delivery_id").is_some())
                    .count(),
                acknowledged,
                "{scenario:?}: exact ACKs"
            );
            assert_eq!(
                calls.len(),
                acquired + acknowledged,
                "{scenario:?}: no extra requests"
            );
            if let Some(error) = scenario.expected_error() {
                let requests = model_seen.lock().await;
                assert!(requests[1]["messages"].to_string().contains(error));
                assert!(!requests[1]["messages"].to_string().contains("data:image"));
                continue;
            }
            assert_eq!(calls[0]["session_id"], handle.id);
            assert_eq!(calls[0]["request_id"], "trusted-request");
            assert_eq!(
                calls[0]["args"],
                if capture {
                    serde_json::json!({"target_id":"native-v1"})
                } else {
                    args.clone()
                }
            );
            assert!(calls[0]["host_turn_id"].is_string());
            assert!(calls[0]["generation"].is_string());
            assert_eq!(calls[1]["call"], calls[0]);
            assert_eq!(calls[1]["delivery_id"], "delivery-fixture");
            if capture {
                let turn = calls[0]["host_turn_id"].as_str().unwrap();
                for (query, expected_status) in [
                    (String::new(), StatusCode::OK),
                    (format!("?turnID={turn}&textOnly=true"), StatusCode::OK),
                    ("?textOnly=true".into(), StatusCode::BAD_REQUEST),
                    (format!("?turnID={turn}"), StatusCode::BAD_REQUEST),
                ] {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::builder()
                                .uri(format!("/session/{}/message{query}", handle.id))
                                .header("authorization", "Basic Zml4dHVyZTpzZWNyZXQ=")
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), expected_status);
                    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                    if query.is_empty() {
                        assert!(String::from_utf8_lossy(&body).contains(png));
                    } else if expected_status == StatusCode::OK {
                        let messages: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert!(!String::from_utf8_lossy(&body).contains(png));
                        assert!(messages
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|m| m["info"]["role"] == "assistant"
                                && m["parts"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .all(|p| p["type"] == "text")));
                        assert!(String::from_utf8_lossy(&body).contains("已取得窗口"));
                    }
                }
            }
            let requests = model_seen.lock().await;
            assert!(requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == tool_name));
            let tool = requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "tool")
                .unwrap();
            assert_eq!(tool["tool_call_id"], "provider-list");
            let model_result =
                serde_json::from_str::<serde_json::Value>(tool["content"].as_str().unwrap())
                    .unwrap();
            assert_eq!(model_result["result"], saved_projection);
            assert_eq!(model_result["invocation_id"], calls[0]["invocation_id"]);
            assert!(tool["content"].as_str().unwrap().contains("native-v1"));
            assert!(!tool["content"].as_str().unwrap().contains(&"a".repeat(64)));
            if capture {
                assert!(requests[1]["messages"]
                    .to_string()
                    .contains(&format!("data:image/png;base64,{png}")));
                assert!(!tool["content"].as_str().unwrap().contains(png));
            }
        }
    }

    #[tokio::test]
    async fn openai_prompt_overrides_and_session_defaults_reach_provider() {
        async fn completion(
            State(seen): State<Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>>,
            Json(body): Json<serde_json::Value>,
        ) -> impl IntoResponse {
            seen.lock().await.push(body);
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"fake\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
        }

        let _env = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let fake = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake provider listener");
        let address = listener.local_addr().expect("fake provider address");
        let server = tokio::spawn(async move { axum::serve(listener, fake).await });

        let old_key = std::env::var_os("OPENAI_API_KEY");
        let old_url = std::env::var_os("OPENPAW_OPENAI_BASE_URL");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"));
        let home = tempdir().expect("home");
        let app = build_router(AppState::new(HostConfig {
            home: home.path().into(),
            provider: "mock".into(),
            ..Default::default()
        }));
        let mock_id = create_session_id(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{mock_id}/message"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":{"providerID":"openai","modelID":"requested-model"},"parts":[{"type":"text","text":"hello"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let openai_id =
            create_session_id_with_body(&app, r#"{"provider":"openai","model":"session-model"}"#)
                .await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/session/{openai_id}/message"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"parts":[{"type":"text","text":"session default"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let summaries: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let openai_summary = summaries
            .as_array()
            .unwrap()
            .iter()
            .find(|summary| summary["id"] == openai_id)
            .expect("OpenAI session summary");
        assert_eq!(openai_summary["provider"], "openai");
        assert_eq!(openai_summary["model"], "session-model");

        match old_key {
            Some(value) => std::env::set_var("OPENAI_API_KEY", value),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        match old_url {
            Some(value) => std::env::set_var("OPENPAW_OPENAI_BASE_URL", value),
            None => std::env::remove_var("OPENPAW_OPENAI_BASE_URL"),
        }
        server.abort();

        let requests = seen.lock().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "requested-model");
        assert_eq!(requests[0]["stream"], true);
        assert_eq!(requests[1]["model"], "session-model");
        assert_eq!(requests[1]["stream"], true);
    }
}
