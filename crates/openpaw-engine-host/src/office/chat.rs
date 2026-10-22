//! 职责：严格Office v1聊天、来源校验、有界NDJSON与断开取消。
//! 输入输出：HTTP请求到共享Host Run及流式事件。
//! 依赖：原Host接受、订阅、取消；不自行驱动模型。
//! 禁止职责：不引入Run ID扩展、写工具或第二套会话。
use super::{
    dto::{self, ChatRequest},
    OfficeConfig,
};
use crate::{ClientScope, Host, HostError, RunState};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde_json::json;
use std::{convert::Infallible, net::SocketAddr, time::Duration};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
struct ChatState {
    host: Host,
    config: OfficeConfig,
    address: SocketAddr,
}
/// 功能：在已有监听器上装配可选Office聊天路由。
/// 参数：host为共享宿主，config为已校验Office配置，address为实际监听地址。
/// 返回：仅含官方POST及OPTIONS的Router。
/// 错误：请求错误在处理入口明确返回。
/// 状态：不创建独立运行服务。
/// 并发与取消：独立任务持有Run生命周期；断开后仍等待清理。
/// 权限：精确Host/Origin及只读配置校验。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub(crate) fn router(host: Host, config: OfficeConfig, address: SocketAddr) -> Router {
    Router::new()
        .route("/office/chat", post(handle).options(options))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .with_state(ChatState {
            host,
            config,
            address,
        })
}
fn origin(state: &ChatState, headers: &HeaderMap) -> Result<String, HostError> {
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let source = headers
        .get("origin")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if ![
        state.address.to_string(),
        format!("localhost:{}", state.address.port()),
    ]
    .contains(&host.to_owned())
        || !state.config.allowed_origins.iter().any(|o| o == source)
    {
        return Err(HostError::new("scope_denied", "聊天来源未获授权"));
    }
    Ok(source.to_owned())
}
fn cors(mut response: Response, source: &str) -> Response {
    let h = response.headers_mut();
    if let Ok(v) = source.parse() {
        h.insert("access-control-allow-origin", v);
    }
    h.insert("vary", "Origin".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    response
}
fn error(e: HostError) -> Response {
    let code = match e.kind.as_str() {
        "scope_denied" => 403,
        "session_busy" | "slot_busy" | "history_conflict" => 409,
        "capacity_exceeded" | "storage_unavailable" | "cancelled" => 503,
        "unsupported" => 422,
        _ => 400,
    };
    (
        StatusCode::from_u16(code).unwrap(),
        axum::Json(json!({"error":e.kind,"message":e.message})),
    )
        .into_response()
}
async fn options(State(state): State<ChatState>, headers: HeaderMap) -> Response {
    let source = match origin(&state, &headers) {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    if headers
        .get("access-control-request-method")
        .and_then(|h| h.to_str().ok())
        != Some("POST")
    {
        return cors(
            error(HostError::new("invalid_input", "只允许POST预检")),
            &source,
        );
    }
    if headers
        .get("access-control-request-headers")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|h| {
                !matches!(
                    h.trim().to_ascii_lowercase().as_str(),
                    "content-type" | "accept"
                )
            })
        })
    {
        return cors(
            error(HostError::new("scope_denied", "不允许的请求头")),
            &source,
        );
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert("access-control-allow-methods", "POST".parse().unwrap());
    response.headers_mut().insert(
        "access-control-allow-headers",
        "content-type, accept".parse().unwrap(),
    );
    cors(response, &source)
}
async fn handle(
    State(state): State<ChatState>,
    headers: HeaderMap,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    let source = match origin(&state, &headers) {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    let response = async {
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !content_type.eq_ignore_ascii_case("application/json") {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "需要application/json").into_response();
        }
        let body = match body {
            Ok(b) => b,
            Err(e) => return e.into_response(),
        };
        let request: ChatRequest = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => return error(HostError::new("invalid_input", "聊天JSON格式或字段无效")),
        };
        if let Err(e) = request.validate() {
            return error(e);
        }
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: request.team_id,
            conversation_id: request.conversation_id,
        };
        let (ready_tx, ready_rx) = oneshot::channel();
        let (tx, rx) = mpsc::channel::<Bytes>(1);
        let host = state.host.clone();
        // 接受与流归同一个宿主任务所有；请求future消失不能丢下已接受的Run。
        tokio::spawn(async move {
            let accepted = async {
                let binding = host.open_session(scope.clone()).await?;
                let mut messages = request.messages;
                let input = messages.pop().unwrap().content;
                let history = messages
                    .into_iter()
                    .map(|m| {
                        if m.role == "user" {
                            openpaw_core::Message::user_text(m.content)
                        } else {
                            openpaw_core::Message::assistant_text(m.content)
                        }
                    })
                    .collect();
                host.submit_with_history(&scope, &binding.session_id, input, Some(history))
                    .await
            }
            .await;
            let run = match accepted {
                Ok(r) => r,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                let _ = host.cancel_run(&scope, &run.run_id).await;
                return;
            }
            let result = stream_run(&host, &scope, &run.run_id, &tx).await;
            if !result {
                let _ = host.cancel_run(&scope, &run.run_id).await;
                // 共享执行任务仍持有资源，清理完成才进入终态；不drop工具future。
                loop {
                    match host.get_run(&scope, &run.run_id).await {
                        Ok(v) if !v.state.is_terminal() => {
                            tokio::time::sleep(Duration::from_millis(20)).await
                        }
                        _ => break,
                    }
                }
            }
        });
        match ready_rx.await {
            Ok(Ok(())) => {
                let stream = futures::stream::unfold(rx, |mut rx| async move {
                    rx.recv()
                        .await
                        .map(|bytes| (Ok::<_, Infallible>(bytes), rx))
                });
                let mut response = Body::from_stream(stream).into_response();
                response.headers_mut().insert(
                    "content-type",
                    "application/x-ndjson; charset=utf-8".parse().unwrap(),
                );
                response
                    .headers_mut()
                    .insert("x-accel-buffering", "no".parse().unwrap());
                response
            }
            Ok(Err(e)) => error(e),
            Err(_) => error(HostError::new("interrupted", "聊天接受任务异常")),
        }
    }
    .await;
    cors(response, &source)
}
async fn send(tx: &mpsc::Sender<Bytes>, event: serde_json::Value) -> bool {
    let mut line = event.to_string();
    if dto::units(&line) > 32000 {
        return false;
    }
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(5), tx.send(Bytes::from(line)))
        .await
        .is_ok_and(|r| r.is_ok())
}
async fn stream_run(host: &Host, scope: &ClientScope, id: &str, tx: &mpsc::Sender<Bytes>) -> bool {
    if !send(tx, json!({"type":"status","phase":"thinking"})).await {
        return false;
    }
    let mut sent = String::new();
    let mut streaming = false;
    loop {
        let (_, mut notices) = match host.snapshot_and_subscribe(scope).await {
            Ok(s) => s,
            Err(_) => {
                let _ = send(tx, json!({"type":"error","message":"运行状态保存失败"})).await;
                return false;
            }
        };
        let run = match host.get_run(scope, id).await {
            Ok(r) => r,
            Err(_) => {
                let _ = send(tx, json!({"type":"error","message":"无法读取可靠运行状态"})).await;
                return false;
            }
        };
        if !run.text.starts_with(&sent) || dto::units(&run.text) > 50000 || run.text_truncated {
            let _ = send(
                tx,
                json!({"type":"error","message":"回复超限或流内容不一致，运行已停止"}),
            )
            .await;
            return false;
        }
        let mut tail = &run.text[sent.len()..];
        if !tail.is_empty() && !streaming {
            if !send(tx, json!({"type":"status","phase":"streaming"})).await {
                return false;
            }
            streaming = true;
        }
        while !tail.is_empty() {
            // 4000 UTF-16单元即使全部JSON转义也小于32000，且delta不超过16000。
            let chunk = dto::bounded(tail, 4000);
            if !send(tx, json!({"type":"delta","text":chunk})).await {
                return false;
            }
            tail = &tail[chunk.len()..];
        }
        sent = run.text;
        if run.state.is_terminal() {
            if run.state == RunState::Completed && !sent.trim().is_empty() && run.error.is_none() {
                return send(tx, json!({"type":"done"})).await;
            }
            let message = if sent.trim().is_empty() && run.state == RunState::Completed {
                "模型没有返回可显示文本"
            } else {
                match run.state {
                    RunState::Cancelled => "运行已取消",
                    RunState::TimedOut => "运行超时",
                    RunState::Interrupted => "运行已中断",
                    _ => "运行失败，未正常完成",
                }
            };
            let _ = send(tx, json!({"type":"error","message":message})).await;
            return true;
        }
        tokio::select! {_=tx.closed()=>return false,_=notices.recv()=>{},_=tokio::time::sleep(Duration::from_millis(200))=>{}}
    }
}
