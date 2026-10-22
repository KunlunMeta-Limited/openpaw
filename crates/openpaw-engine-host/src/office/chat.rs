//! 职责：严格Office v1聊天、来源校验、有界NDJSON、订阅脱离与显式取消。
//! 输入输出：HTTP请求到共享Host Run及流式事件。
//! 依赖：原Host接受、订阅、取消；不自行驱动模型。
//! 禁止职责：不引入自定义Run URL或写工具或第二套会话。
use super::{
    dto::{self, Operation},
    turns, OfficeConfig,
};
use crate::{ClientScope, Host, HostError};
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
/// 并发与取消：Run由共享Host持有；订阅断开只detach，显式cancel等待清理。
/// 权限：回环监听和精确Host；浏览器Origin白名单，Node无Origin转发仍限本机。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub(crate) fn router(host: Host, config: OfficeConfig, address: SocketAddr) -> Router {
    Router::new()
        .route("/office/chat", post(handle).options(options))
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024))
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
        || (!source.is_empty() && !state.config.allowed_origins.iter().any(|o| o == source))
    {
        return Err(HostError::new("scope_denied", "聊天来源未获授权"));
    }
    Ok(source.to_owned())
}
fn cors(mut response: Response, source: &str) -> Response {
    let h = response.headers_mut();
    if !source.is_empty() {
        if let Ok(v) = source.parse() {
            h.insert("access-control-allow-origin", v);
        }
    }
    h.insert("vary", "Origin".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    response
}
fn error(e: HostError) -> Response {
    let code = match e.kind.as_str() {
        "scope_denied" => 403,
        "session_busy" | "slot_busy" | "history_conflict" | "request_conflict" => 409,
        "not_found" => 404,
        "capacity_exceeded" | "storage_unavailable" | "cancelled" => 503,
        "unsupported" => 422,
        _ => 400,
    };
    (
        StatusCode::from_u16(code).unwrap(),
        axum::Json(json!({"code":code,"msg":e.message,"data":{"reasonCode":e.kind,"details":{},"issues":[]}})),
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
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return error(HostError::new("invalid_input", "聊天JSON无效")),
        };
        if !dto::protocol_value(&value) {
            return error(HostError::new("invalid_input", "聊天不允许null或保留字段"));
        }
        let request: Operation = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => return error(HostError::new("invalid_input", "聊天JSON格式或字段无效")),
        };
        let scope = match request.scope() {
            Ok(s) => s,
            Err(e) => return error(e),
        };
        if !state.host.inner.config.slots.contains_key(&scope.slot_id) {
            return error(HostError::new("scope_denied", "办公室未授权"));
        }
        let request = match request {
            Operation::History(r) => {
                return match turns::history(&state.host, &scope, r).await {
                    Ok(page) => ok(page),
                    Err(e) => error(e),
                }
            }
            Operation::Cancel(r) => {
                // 取消任务归Host所有；HTTP连接消失不能使清理半途停止。
                let host = state.host.clone();
                return match tokio::spawn(async move {
                    let run = turns::find(&host, &scope, &r.turn_id).await?;
                    host.cancel_run(&scope, &run.run_id).await?;
                    loop {
                        let view = host.get_run(&scope, &run.run_id).await?;
                        if view.state.is_terminal() {
                            return Ok::<_, HostError>(());
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                {
                    Ok(Ok(())) => ok(json!({"stopped":true})),
                    Ok(Err(e)) => error(e),
                    Err(_) => error(HostError::new("interrupted", "取消任务异常")),
                };
            }
            other => other,
        };
        let (ready_tx, ready_rx) = oneshot::channel();
        let (tx, rx) = mpsc::channel::<Bytes>(1);
        let host = state.host.clone();
        // 接受与流归同一个宿主任务所有；请求future消失不能丢下已接受的Run。
        tokio::spawn(async move {
            let accepted = match request {
                Operation::Send(r) => turns::accept(&host, &scope, r).await,
                Operation::Resume(r) => turns::find(&host, &scope, &r.turn_id).await,
                _ => unreachable!("控制操作已分派"),
            };
            let run = match accepted {
                Ok(r) => r,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if ready_tx.send(Ok(())).is_ok() {
                // 流任务仅持有订阅；断线、刷新或慢消费者不取消原Run。
                let _ = stream_run(&host, &scope, &run.run_id, &tx).await;
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
    if dto::units(&line) > 350000 {
        return false;
    }
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(5), tx.send(Bytes::from(line)))
        .await
        .is_ok_and(|r| r.is_ok())
}
async fn stream_run(host: &Host, scope: &ClientScope, id: &str, tx: &mpsc::Sender<Bytes>) -> bool {
    let mut first = true;
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
        if first {
            let event = json!({"type":"snapshot","text":dto::bounded(&run.text,50000),"state":turns::state(&run),"message":turns::message(&run)});
            if !send(tx, event).await {
                return false;
            }
            if turns::state(&run) != "pending" {
                if !run.state.is_terminal() {
                    let _ = host.cancel_run(scope, id).await;
                }
                return true;
            }
            sent = run.text.clone();
            first = false;
            if !send(tx, json!({"type":"status","phase":"thinking"})).await {
                return false;
            }
        }
        if !run.text.starts_with(&sent) || dto::units(&run.text) > 50000 || run.text_truncated {
            let _ = host.cancel_run(scope, id).await;
            let _ = send(
                tx,
                json!({"type":"error","message":"回复超限或流内容不一致，已请求停止"}),
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
        sent = run.text.clone();
        if run.state.is_terminal() {
            return match turns::state(&run) {
                "complete" => send(tx, json!({"type":"done"})).await,
                "stopped" => {
                    send(
                        tx,
                        json!({"type":"snapshot","text":sent,"state":"stopped","message":""}),
                    )
                    .await
                }
                _ => send(tx, json!({"type":"error","message":turns::message(&run)})).await,
            };
        }
        tokio::select! {_=tx.closed()=>return false,_=notices.recv()=>{},_=tokio::time::sleep(Duration::from_millis(200))=>{}}
    }
}

fn ok(data: serde_json::Value) -> Response {
    axum::Json(json!({"code":200,"msg":"OK","data":data})).into_response()
}
