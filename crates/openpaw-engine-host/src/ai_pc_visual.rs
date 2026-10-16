//! 功能：提供 Host 私有实时视觉调用上下文和解析回执。
//! 边界：身份由已接收且实际运行的回合安装，模型只能提交工具参数。
//! 禁止承担：不改变 Core/AgentLoop，不重试原生操作，不把 ACK 称为模型理解。
//! 关联：CHG-2026-0029、API-HOST-AI-PC-REGISTER-001。
use crate::ai_pc::Registration;
use openpaw_core::{InlineMedia, ToolCallOutcome};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
/// 功能：每个可信登记独有的活动回合与有界取消队列，不向模型公开。
pub(super) struct VisualContext {
    active: Arc<Mutex<Option<Active>>>,
    controls: Arc<OnceLock<tokio::sync::mpsc::Sender<Value>>>,
}
#[derive(Clone)]
struct Active {
    request_id: String,
    host_turn_id: String,
    generation: String,
    cancel: CancellationToken,
    image: bool,
}
/// 功能：退出执行时仅清理自身代次，防止旧执行清理新回合。
pub(super) struct ExecutionGuard {
    context: VisualContext,
    generation: String,
}
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        let mut active = self
            .context
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|a| a.generation == self.generation)
        {
            active.take();
        }
    }
}
impl VisualContext {
    /// 功能：在真实回合进入 running 后安装可信桌面调用身份。
    /// 参数：request_id/host_turn_id 来自已接收回合，cancel 是原执行取消，image 是实际模型能力。
    /// 返回：只清理自身代次的执行守卫。
    /// 错误：无；中毒锁取回已有状态。
    /// 状态：替换本登记的活动代次，不持久化凭据。
    /// 并发与取消：短同步锁，不跨 await，取消由原 token 传播。
    /// 权限：仅 runtime 调用，模型不能填写身份。
    /// 关联：CHG-2026-0029、API-HOST-AI-PC-REGISTER-001。
    pub(super) fn enter(
        &self,
        request_id: String,
        host_turn_id: String,
        cancel: CancellationToken,
        image: bool,
    ) -> ExecutionGuard {
        let generation = uuid::Uuid::new_v4().to_string();
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = Some(Active {
            request_id,
            host_turn_id,
            generation: generation.clone(),
            cancel,
            image,
        });
        ExecutionGuard {
            context: self.clone(),
            generation,
        }
    }
    fn snapshot(&self) -> Result<Active, String> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|a| !a.cancel.is_cancelled())
            .ok_or_else(|| "视觉工具没有已接收的活动回合。".into())
    }
}
struct CancelOnDrop {
    sender: tokio::sync::mpsc::Sender<Value>,
    call: Option<Value>,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(call) = self.call.take() {
            // 独立有界队列；析构不等待网络。桌面 Stop 原回合取消是主路径，
            // 运行时关闭或队列已满时，此补充通知不保证送达，不伪称原生已停止。
            let _ = self.sender.try_send(call);
        }
    }
}
fn control_sender(r: &Registration) -> tokio::sync::mpsc::Sender<Value> {
    r.visual
        .controls
        .get_or_init(|| {
            let (send, mut receive) = tokio::sync::mpsc::channel::<Value>(64);
            let (client, mut url, token) = (r.client.clone(), r.url.clone(), r.token.clone());
            url.set_path("/ai-pc/cancel");
            tokio::spawn(async move {
                while let Some(call) = receive.recv().await {
                    let _ = client
                        .post(url.clone())
                        .bearer_auth(&token)
                        .timeout(Duration::from_secs(2))
                        .json(&call)
                        .send()
                        .await;
                }
            });
            send
        })
        .clone()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultEnvelope {
    protocol_version: u32,
    invocation_id: String,
    delivery_id: String,
    status: String,
    projection: Value,
    #[serde(default)]
    media: Vec<InlineMedia>,
}
/// 功能：按已实现工具选择桌面路由，执行一次实时调用并回执精确解析结果。
/// 参数：name 为已验证工具名，session_id 为可信登记会话，args 为模型参数，r 为内存登记。
/// 返回：不含凭据的不可变投影工具结果。
/// 错误：身份、网络、超时、容量、格式或 ACK 失败时明确返回；不重试。
/// 状态：每次分配唯一 invocation，保留原活动代次。
/// 并发与取消：网络等待可取消；Drop 仅尝试发送独立有界取消通知。
/// 权限：仅已接收且仍活动回合，经原桌面授权链交付。
/// 关联：CHG-2026-0029、API-HOST-AI-PC-REGISTER-001。
pub(super) async fn call(
    name: &str,
    session_id: &str,
    args: Value,
    r: Registration,
) -> Result<ToolCallOutcome, String> {
    let path = match name {
        "ai_pc_list_windows" => "/ai-pc/windows",
        "ai_pc_query_structure" => "/ai-pc/structure",
        "ai_pc_read_text" => "/ai-pc/text",
        "ai_pc_action_batch" => "/ai-pc/actions",
        "ai_pc_capture_target" => "/ai-pc/capture",
        _ => return Err("此实时桌面工具尚未实现。".into()),
    };
    let active = r.visual.snapshot()?;
    let capture = name == "ai_pc_capture_target";
    if capture && !active.image {
        return Err(
            "实际所选主模型未声明 image 输入，不能取得或发送目标 PNG；未转换文字或更换模型。"
                .into(),
        );
    }
    let invocation = uuid::Uuid::new_v4().to_string();
    let call = json!({"session_id":session_id,"request_id":active.request_id,"host_turn_id":active.host_turn_id,"generation":active.generation,"invocation_id":invocation,"args":args});
    let mut guard = CancelOnDrop {
        sender: control_sender(&r),
        call: Some(call.clone()),
    };
    let mut url = r.url.clone();
    url.set_path(path);
    // 20 秒是 Host 工具等待期限，不是 HTTP 收包期限，也不表示同步原生动作可中断。
    let response = tokio::select! { biased; _ = active.cancel.cancelled() => return Err("原回合已取消。".into()), result = r.client.post(url).bearer_auth(&r.token).timeout(Duration::from_secs(20)).json(&call).send() => result.map_err(|_|"视觉工具连接失败；没有重发。")? };
    if !response.status().is_success() {
        return Err(format!("视觉工具拒绝 HTTP {}", response.status().as_u16()));
    }
    let mut response = response;
    let mut bytes = vec![];
    while let Some(chunk) = tokio::select! { biased; _=active.cancel.cancelled()=>return Err("原回合已取消。".into()), chunk=response.chunk()=>chunk.map_err(|_|"视觉结果传输失败。")? }
    {
        if bytes.len().saturating_add(chunk.len()) > (if capture { 7 } else { 1 }) * 1024 * 1024 {
            return Err("视觉结果超过本工具传输容量。".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let result: ResultEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| "视觉结果格式无效。")?;
    if result.protocol_version != 1
        || result.invocation_id != invocation
        || result.delivery_id.is_empty()
        || result.delivery_id.len() > 128
        || result.status != "ok"
        || !result.projection.is_object()
    {
        return Err("视觉结果绑定或状态无效。".into());
    }
    let current = r.visual.snapshot()?;
    if current.generation != active.generation {
        return Err("视觉结果属于旧回合。".into());
    }
    if capture {
        if result.media.len() != 1 || result.media[0].mime != "image/png" {
            return Err("目标截图必须交付单张原始 PNG。".into());
        }
        result.media[0]
            .bytes()
            .map_err(|_| "PNG 媒体无效或超过既有原件容量。")?;
    } else if !result.media.is_empty() {
        return Err("此视觉工具不允许附带媒体。".into());
    }
    let mut ack = r.url.clone();
    ack.set_path("/ai-pc/ack");
    let accepted = tokio::select! { biased; _=active.cancel.cancelled()=>return Err("原回合已取消。".into()), result=r.client.post(ack).bearer_auth(&r.token).timeout(Duration::from_secs(2)).json(&json!({"call":call,"delivery_id":result.delivery_id})).send()=>result.map_err(|_|"Host 解析回执未知；没有重发工具。")? };
    if !accepted.status().is_success() || active.cancel.is_cancelled() {
        return Err("交付回执被拒绝或原回合已取消。".into());
    }
    // Response size is bounded before parsing. A generic 2xx is not proof that
    // the desktop accepted this exact nonce under its still-current policy.
    if accepted.content_length().is_some_and(|length| length > 128) {
        return Err("交付回执格式无效。".into());
    }
    let mut accepted = accepted;
    let mut ack_bytes = vec![];
    while let Some(chunk) = accepted.chunk().await.map_err(|_| "交付回执传输失败。")? {
        if ack_bytes.len() + chunk.len() > 128 {
            return Err("交付回执容量无效。".into());
        }
        ack_bytes.extend_from_slice(&chunk);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ack {
        status: String,
    }
    let ack: Ack = serde_json::from_slice(&ack_bytes).map_err(|_| "交付回执格式无效。")?;
    if ack.status != "accepted" || active.cancel.is_cancelled() {
        return Err("交付回执未接受或已取消。".into());
    }
    guard.call = None;
    let structured =
        json!({"invocation_id":invocation,"status":result.status,"result":result.projection});
    Ok(ToolCallOutcome {
        content: structured.to_string(),
        structured: Some(structured),
        is_error: matches!(
            result.projection["state"].as_str(),
            Some("error" | "unknown" | "cancelled")
        ),
        media: result.media,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::post, Json, Router};
    #[derive(Clone, Default)]
    struct Fixture {
        seen: Arc<tokio::sync::Mutex<Vec<Value>>>,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        cancelled: Arc<tokio::sync::Notify>,
    }
    #[tokio::test]
    async fn cancel_and_old_generation_never_ack_or_replay_the_native_call() {
        for replace in [false, true] {
            let fixture = Fixture::default();
            let router=Router::new().route("/ai-pc/windows",post(|State(f):State<Fixture>,Json(body):Json<Value>| async move {
                f.seen.lock().await.push(body.clone()); f.started.notify_one(); f.release.notified().await;
                Json(json!({"protocol_version":1,"invocation_id":body["invocation_id"],"delivery_id":"delivery","status":"ok","projection":{"windows":[]}}))
            })).route("/ai-pc/cancel",post(|State(f):State<Fixture>,Json(body):Json<Value>|async move { f.seen.lock().await.push(body); f.cancelled.notify_one(); Json(json!({"status":"accepted"})) })).with_state(fixture.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/ai-pc", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let registration = crate::ai_pc::RegistrationInput {
                bridge_url: url,
                token: "a".repeat(64),
            }
            .checked()
            .ok()
            .unwrap();
            let cancel = CancellationToken::new();
            let old = registration.visual.enter(
                "request".into(),
                "host-turn".into(),
                cancel.clone(),
                false,
            );
            let task = tokio::spawn(call(
                "ai_pc_list_windows",
                "session",
                json!({}),
                registration.clone(),
            ));
            tokio::time::timeout(Duration::from_secs(2), fixture.started.notified())
                .await
                .unwrap();
            let newer = if replace {
                Some(registration.visual.enter(
                    "next".into(),
                    "host-next".into(),
                    CancellationToken::new(),
                    false,
                ))
            } else {
                cancel.cancel();
                None
            };
            fixture.release.notify_one();
            assert!(tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .is_err());
            tokio::time::timeout(Duration::from_secs(2), fixture.cancelled.notified())
                .await
                .unwrap();
            let seen = fixture.seen.lock().await;
            assert_eq!(
                seen.len(),
                2,
                "one request, one best-effort cancel, no ACK or retry"
            );
            assert_eq!(seen[0], seen[1]);
            drop(old);
            if newer.is_some() {
                assert_eq!(registration.visual.snapshot().unwrap().request_id, "next");
            }
            drop(newer);
            server.abort();
        }
    }
}
