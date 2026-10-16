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

async fn messages(
    State(state): State<Arc<AppState>>,
    Path(path): Path<std::collections::HashMap<String, String>>,
) -> Response {
    let handle = match state.get_typed(&path["id"]).await {
        Ok(handle) => handle,
        Err(error) => return session_load_error(error),
    };
    // 读取独立快照，避免模型/工具回合持有 Session 写锁时让前端轮询等待。
    let result = handle.wire_messages();
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
    let (mut user, presentation) = match crate::input_presentation::prepare(&body) {
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
        if let Some(part) = crate::message_projection::input_part(
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
    body: Result<Json<crate::ai_pc::RegistrationInput>, JsonRejection>,
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
fn ai_pc_response(result: Result<(), crate::ai_pc::RegistrationError>) -> Response {
    use crate::ai_pc::RegistrationError;
    match result {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            [
                ("openpaw-desktop-protocol", "1"),
                ("openpaw-desktop-tools", "ai_pc_list_windows"),
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
    if let Ok((message, Some(presentation))) = crate::input_presentation::prepare(body) {
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

    #[tokio::test]
    async fn registered_visual_tool_reaches_model_with_trusted_turn_and_exact_parsed_ack() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _env = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for capture in [false, true] {
        let tool_name = if capture { "ai_pc_capture_target" } else { "ai_pc_list_windows" };
        let tool_args = if capture { r#"{"target_id":"native-v1"}"# } else { "{}" };
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLttAAAAABJRU5ErkJggg==";
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let model_seen = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let model_calls = Arc::new(AtomicUsize::new(0));
        let seen = model_seen.clone();
        let count = model_calls.clone();
        let model = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
            let (seen, count) = (seen.clone(), count.clone());
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
        let bridge = Router::new().route(if capture { "/ai-pc/capture" } else { "/ai-pc/windows" },post(move |Json(body): Json<serde_json::Value>| {
            let observed = observed.clone();
            async move { observed.lock().await.push(body.clone()); let mut value = serde_json::json!({"protocol_version":1,"invocation_id":body["invocation_id"],"delivery_id":"delivery-fixture","status":"ok","projection":{"kind":"untrusted_ui_data","target_id":"native-v1"}}); if capture { value["media"] = serde_json::json!([{"id":"att_fixture","mime":"image/png","data":png,"filename":"target.png"}]); } Json(value) }
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
        let bridge_url = format!("http://{}/ai-pc", listener.local_addr().unwrap());
        let bridge_task = tokio::spawn(async move {
            axum::serve(listener, bridge).await.unwrap();
        });
        let old_key = std::env::var_os("OPENAI_API_KEY");
        let old_url = std::env::var_os("OPENPAW_OPENAI_BASE_URL");
        let old_capabilities = std::env::var_os("OPENPAW_MODEL_CAPABILITIES");
        std::env::set_var("OPENPAW_MODEL_CAPABILITIES", r#"{"openai":{"fixture-model":{"input":["text","image"]}}}"#);
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
        state
            .register_ai_pc(
                &handle.id,
                Some(crate::ai_pc::RegistrationInput {
                    bridge_url,
                    token: "a".repeat(64),
                }),
            )
            .await
            .ok()
            .unwrap();
        let response = build_router(state).oneshot(Request::builder().method("POST").uri(format!("/session/{}/message",handle.id)).header("content-type","application/json").header("authorization","Basic Zml4dHVyZTpzZWNyZXQ=").body(Body::from(r#"{"requestID":"trusted-request","parts":[{"type":"text","text":"列出窗口"}]}"#)).unwrap()).await.unwrap();
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
        match old_capabilities { Some(v) => std::env::set_var("OPENPAW_MODEL_CAPABILITIES", v), None => std::env::remove_var("OPENPAW_MODEL_CAPABILITIES") }
        model_task.abort();
        bridge_task.abort();
        assert_eq!(status, StatusCode::OK);
        let calls = calls.lock().await;
        assert_eq!(
            calls.len(),
            2,
            "one native request followed by exact parsed ACK"
        );
        assert_eq!(calls[0]["session_id"], handle.id);
        assert_eq!(calls[0]["request_id"], "trusted-request");
        assert!(calls[0]["host_turn_id"].is_string());
        assert!(calls[0]["generation"].is_string());
        assert_eq!(calls[1]["call"], calls[0]);
        assert_eq!(calls[1]["delivery_id"], "delivery-fixture");
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
        assert!(tool["content"].as_str().unwrap().contains("native-v1"));
        assert!(!tool["content"].as_str().unwrap().contains(&"a".repeat(64)));
        if capture { assert!(requests[1]["messages"].to_string().contains(&format!("data:image/png;base64,{png}"))); assert!(!tool["content"].as_str().unwrap().contains(png)); }
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
