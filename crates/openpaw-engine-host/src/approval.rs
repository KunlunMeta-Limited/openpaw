//! 功能：将共享审批 gate 适配到既有 Host permission/SSE 协议。
//! 参数：session 和事件广播通道。
//! 返回：原 PermissionInfo 与 ApprovalResponse。
//! 错误：取消或失效请求拒绝。
//! 状态：pending 与 session 授权由 Engine gate 管理。
//! 并发与取消：通知同步非阻塞广播。
//! 权限：HTTP 层仍负责鉴权。
//! 关联：API-HOST-PERMISSION-001。
use crate::protocol::{OpenCodeEvent, PermissionInfo};
use openpaw_core::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use openpaw_engine::approval::{ApprovalGate, ApprovalNotice};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 功能：将共享审批结果适配到原 Host 协议。
pub struct HostApprovalManager {
    session_id: String,
    gate: Arc<ApprovalGate>,
}
fn info(session_id: &str, id: String, req: ApprovalRequest) -> PermissionInfo {
    PermissionInfo {
        id,
        session_id: session_id.into(),
        permission: req.tool_name,
        patterns: vec![req.reason],
        metadata: serde_json::json!({}),
        always: Vec::new(),
        input: req.input,
    }
}
impl HostApprovalManager {
    /// 功能：构造 Host permission 协议 adapter。
    /// 参数：session_id、事件广播和当前取消令牌。
    /// 返回：Arc<HostApprovalManager>。
    /// 错误：广播无人接收时忽略通知。
    /// 状态：pending 与授权委托 Engine gate。
    /// 并发与取消：UUID 身份；通知仅非阻塞广播。
    /// 权限：沿用 HTTP 鉴权和原字段。
    /// 关联：API-HOST-PERMISSION-001。
    pub fn new(
        session_id: impl Into<String>,
        global_events: broadcast::Sender<OpenCodeEvent>,
        cancelled: CancellationToken,
    ) -> Arc<Self> {
        let session_id = session_id.into();
        let notify_session = session_id.clone();
        let gate = ApprovalGate::new(
            cancelled,
            || Uuid::new_v4().simple().to_string(),
            move |notice| {
                let event = match notice {
                    ApprovalNotice::Asked { id, request } => {
                        OpenCodeEvent::new("permission.asked", info(&notify_session, id, request))
                    }
                    ApprovalNotice::Replied { id } => OpenCodeEvent::new(
                        "permission.replied",
                        serde_json::json!({"sessionID":notify_session,"requestID":id}),
                    ),
                };
                let _ = global_events.send(event);
            },
        );
        Arc::new(Self { session_id, gate })
    }
    /// 功能：开启新审批代际。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：清旧 pending，保留本 session 同工具授权。
    /// 并发与取消：委托 gate 单锁线性化。
    /// 权限：不跨 session 或重启。
    /// 关联：Host reserve_turn。
    pub fn begin_turn(&self) {
        self.gate.begin_turn();
    }
    /// 功能：终止并清理当前审批。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：所有失效卡片发 replied。
    /// 并发与取消：委托 gate 唤醒等待，阻止迟到 asked。
    /// 权限：取消不授权。
    /// 关联：Host abort/关闭。
    pub fn abort_all(&self) {
        self.gate.abort_all();
    }
    /// 功能：适配当前 pending 为原 PermissionInfo。
    /// 参数：无。
    /// 返回：权限卡片快照。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：gate 单锁复制。
    /// 权限：HTTP 层先鉴权。
    /// 关联：GET /permission。
    pub async fn list(&self) -> Vec<PermissionInfo> {
        self.gate
            .list()
            .into_iter()
            .map(|(id, req)| info(&self.session_id, id, req))
            .collect()
    }
    /// 功能：一次性回复权限卡片。
    /// 参数：id、决定和安全原因。
    /// 返回：成功交付为 true，失效为 false。
    /// 错误：无。
    /// 状态：委托 gate 消费请求。
    /// 并发与取消：与取消竞争至多结束一次。
    /// 权限：只批准本次。
    /// 关联：POST /permission/:id/reply。
    pub async fn reply(&self, id: &str, approved: bool, reason: String) -> bool {
        self.gate.reply(id, approved, false, reason)
    }
    /// 功能：按一次或 session 作用域回复审批。
    /// 参数：id、决定、session 授权开关、安全原因。
    /// 返回：成功交付为 true，失效为 false。
    /// 错误：无。
    /// 状态：同工具授权可跨当前 session 回合；重启失效。
    /// 并发与取消：委托 gate 线性化。
    /// 权限：不扩大到其他 session 或工具。
    /// 关联：原 once/always/reject 协议。
    pub async fn reply_with_scope(
        &self,
        id: &str,
        approved: bool,
        remember_for_session: bool,
        reason: String,
    ) -> bool {
        self.gate.reply(id, approved, remember_for_session, reason)
    }
}
#[async_trait::async_trait]
impl ApprovalManager for HostApprovalManager {
    /// 功能：执行已有 Core request 接口。
    /// 参数：见既有 Core trait 请求。
    /// 返回：既有 Core 决定类型。
    /// 错误：取消/失效安全拒绝。
    /// 状态：审批等待由共享 gate 管理；guardrail 检查为纯计算。
    /// 并发与取消：取消或 future 丢弃清理 pending；通知不阻塞。
    /// 权限：不绕过工具权限，不展示凭据。
    /// 关联：CHG-2026-0023 共用审批。
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        self.gate.request(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::ApprovalManager;
    use serde_json::json;

    #[tokio::test]
    async fn dropped_approval_waiter_removes_permission_and_closes_card() {
        let (global, mut events) = broadcast::channel(8);
        let manager = HostApprovalManager::new("session-drop", global, CancellationToken::new());
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let asked = events.recv().await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(
            manager.list().await.is_empty(),
            "cancelled waiter leaves a stale permission card"
        );
        let replied = events.try_recv().expect("close cancelled card");
        assert_eq!(replied.event_type, "permission.replied");
        assert_eq!(replied.properties["requestID"], asked.properties["id"]);
    }

    #[tokio::test]
    async fn pending_permission_roundtrip_and_reply_event() {
        let (global, mut rx) = broadcast::channel(8);
        let manager = HostApprovalManager::new("session-1", global, CancellationToken::new());
        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new(
                        "write_file",
                        json!({"path":"a"}),
                        "write",
                    ))
                    .await
            })
        };
        let asked = rx.recv().await.expect("permission asked");
        assert_eq!(asked.event_type, "permission.asked");
        let permission_id = asked.properties["id"].as_str().unwrap().to_string();
        assert_eq!(manager.list().await.len(), 1);
        assert!(manager.reply(&permission_id, true, String::new()).await);
        let replied = rx.recv().await.expect("permission replied");
        assert_eq!(replied.event_type, "permission.replied");
        assert_eq!(
            replied.properties,
            json!({"sessionID":"session-1","requestID":permission_id})
        );
        assert!(waiting.await.unwrap().is_approved());
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn abort_after_request_wakes_waiter_and_next_turn_resets_state() {
        let (global, mut rx) = broadcast::channel(8);
        let manager = HostApprovalManager::new("session-1", global, CancellationToken::new());
        manager.begin_turn();
        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let _asked = rx.recv().await.expect("permission asked");
        manager.abort_all();
        let aborted_event = rx.recv().await.expect("permission replied after abort");
        assert_eq!(aborted_event.event_type, "permission.replied");
        assert_eq!(
            aborted_event
                .properties
                .as_object()
                .map(|value| value.len()),
            Some(2)
        );
        let response = tokio::time::timeout(std::time::Duration::from_millis(100), waiting)
            .await
            .expect("abort must wake request")
            .expect("request task");
        assert!(matches!(
            response,
            ApprovalResponse::Denied { reason } if reason == "回合已取消"
        ));
        assert!(manager.list().await.is_empty());

        manager.begin_turn();
        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let _asked = rx.recv().await.expect("next turn permission asked");
        assert!(manager.list().await.len() == 1);
        manager.abort_all();
        let _ = rx
            .recv()
            .await
            .expect("next permission replied after abort");
        let _ = waiting.await.expect("next request task");
    }

    #[tokio::test]
    async fn begin_turn_closes_stale_permission_with_replied_event() {
        let (global, mut rx) = broadcast::channel(8);
        let manager = HostApprovalManager::new("session-1", global, CancellationToken::new());
        manager.begin_turn();
        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let asked = rx.recv().await.expect("permission asked");
        let permission_id = asked.properties["id"].as_str().unwrap().to_string();
        manager.begin_turn();
        let replied = rx.recv().await.expect("permission replied after reset");
        assert_eq!(replied.event_type, "permission.replied");
        assert_eq!(replied.properties["requestID"], permission_id);
        assert_eq!(
            replied.properties.as_object().map(|value| value.len()),
            Some(2)
        );
        assert!(matches!(
            waiting.await.unwrap(),
            ApprovalResponse::Denied { .. }
        ));
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn request_after_abort_is_denied_without_a_late_asked_event() {
        let (global, mut rx) = broadcast::channel(8);
        let manager = HostApprovalManager::new("session-1", global, CancellationToken::new());
        manager.begin_turn();
        manager.abort_all();

        let response = manager
            .request(ApprovalRequest::new("write_file", json!({}), "write"))
            .await;

        assert!(matches!(
            response,
            ApprovalResponse::Denied { reason } if reason == "回合已取消"
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn always_remembers_tool_for_current_session_only() {
        let (global, mut rx) = broadcast::channel(16);
        let manager = HostApprovalManager::new("session-1", global, CancellationToken::new());
        manager.begin_turn();

        let first = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("read_file", json!({}), "read"))
                    .await
            })
        };
        let asked = rx.recv().await.expect("first permission asked");
        let permission_id = asked.properties["id"].as_str().unwrap().to_string();
        assert!(
            manager
                .reply_with_scope(&permission_id, true, true, String::new())
                .await
        );
        assert!(first.await.unwrap().is_approved());
        let _ = rx.recv().await.expect("first permission replied");

        manager.begin_turn();
        let second = manager
            .request(ApprovalRequest::new("read_file", json!({}), "read again"))
            .await;
        assert!(second.is_approved());
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        let third = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let asked = rx.recv().await.expect("different tool still asks");
        let different_id = asked.properties["id"].as_str().unwrap().to_string();
        assert!(manager.reply(&different_id, false, "拒绝".into()).await);
        assert!(matches!(
            third.await.unwrap(),
            ApprovalResponse::Denied { .. }
        ));

        let (other_global, mut other_rx) = broadcast::channel(4);
        let other = HostApprovalManager::new("session-2", other_global, CancellationToken::new());
        let other_waiting = {
            let other = other.clone();
            tokio::spawn(async move {
                other
                    .request(ApprovalRequest::new("read_file", json!({}), "new session"))
                    .await
            })
        };
        let _ = other_rx.recv().await.expect("new session asks again");
        assert_eq!(other.list().await.len(), 1);
        other.abort_all();
        let _ = other_waiting.await;
    }
}
