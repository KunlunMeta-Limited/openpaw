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
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 功能：将共享审批结果适配到原 Host 协议。
pub struct HostApprovalManager {
    session_id: String,
    gate: Arc<ApprovalGate>,
    metadata: serde_json::Value,
    events: broadcast::Sender<OpenCodeEvent>,
    children: Mutex<ChildApprovalScopes>,
}
struct ChildApprovalScopes {
    accepting: bool,
    scopes: HashMap<String, Arc<HostApprovalManager>>,
}
fn info(
    session_id: &str,
    id: String,
    req: ApprovalRequest,
    metadata: serde_json::Value,
) -> PermissionInfo {
    PermissionInfo {
        id,
        session_id: session_id.into(),
        permission: req.tool_name,
        patterns: vec![req.reason],
        metadata,
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
        Self::with_metadata(
            session_id.into(),
            global_events,
            cancelled,
            serde_json::json!({}),
        )
    }

    fn with_metadata(
        session_id: String,
        global_events: broadcast::Sender<OpenCodeEvent>,
        cancelled: CancellationToken,
        metadata: serde_json::Value,
    ) -> Arc<Self> {
        let notify_session = session_id.clone();
        let notify_metadata = metadata.clone();
        let events = global_events.clone();
        let gate = ApprovalGate::new(
            cancelled,
            || Uuid::new_v4().simple().to_string(),
            move |notice| {
                let event = match notice {
                    ApprovalNotice::Asked { id, request } => OpenCodeEvent::new(
                        "permission.asked",
                        info(&notify_session, id, request, notify_metadata.clone()),
                    ),
                    ApprovalNotice::Replied { id } => OpenCodeEvent::new(
                        "permission.replied",
                        serde_json::json!({"sessionID":notify_session,"requestID":id}),
                    ),
                };
                let _ = global_events.send(event);
            },
        );
        Arc::new(Self {
            session_id,
            gate,
            metadata,
            events,
            children: Mutex::new(ChildApprovalScopes {
                accepting: true,
                scopes: HashMap::new(),
            }),
        })
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
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        for (_, child) in children.scopes.drain() {
            child.abort_all();
        }
        children.accepting = true;
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
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        children.accepting = false;
        for (_, child) in children.scopes.drain() {
            child.abort_all();
        }
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
        self.snapshot()
    }
    fn snapshot(&self) -> Vec<PermissionInfo> {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = self
            .gate
            .list()
            .into_iter()
            .map(|(id, req)| info(&self.session_id, id, req, self.metadata.clone()))
            .collect();
        for child in children.scopes.values() {
            out.extend(child.snapshot());
        }
        out
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
        self.reply_scoped(id, approved, false, reason)
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
        self.reply_scoped(id, approved, remember_for_session, reason)
    }

    fn reply_scoped(&self, id: &str, approved: bool, remember: bool, reason: String) -> bool {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        self.gate.reply(id, approved, remember, reason.clone())
            || children
                .scopes
                .values()
                .any(|child| child.reply_scoped(id, approved, remember, reason.clone()))
    }

    /// 功能：为一次实际子任务登记独立审批范围。
    /// 参数：子空间身份、显示名和当前父回合取消令牌。
    /// 返回：持有期间有效的审批登记；父回合已终止则无。
    /// 错误：无。
    /// 状态：不继承父授权；登记在任务退出时移除。
    /// 并发与取消：与父 abort 单锁线性化，不允许迟到登记。
    /// 权限：子 always 只在本次执行内有效。
    /// 关联：ADR-006、GET/POST permission。
    pub(crate) fn child(
        self: &Arc<Self>,
        workspace: &str,
        name: &str,
        cancel: CancellationToken,
    ) -> Option<ChildApprovals> {
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if !children.accepting || cancel.is_cancelled() {
            return None;
        }
        let id = Uuid::new_v4().simple().to_string();
        let manager = Self::with_metadata(
            self.session_id.clone(),
            self.events.clone(),
            cancel.child_token(),
            serde_json::json!({"workspaceID":workspace,"workspaceName":name,"scopeID":id}),
        );
        children.scopes.insert(id.clone(), manager.clone());
        Some(ChildApprovals {
            parent: self.clone(),
            manager,
            id,
        })
    }
}

/// 功能：持有一次子执行的审批登记，离开作用域即拒绝遗留请求。
pub(crate) struct ChildApprovals {
    parent: Arc<HostApprovalManager>,
    /// 本次子执行独享的审批管理器；退出执行即清理，不复用至下次任务。
    pub(crate) manager: Arc<HostApprovalManager>,
    id: String,
}
impl Drop for ChildApprovals {
    fn drop(&mut self) {
        self.parent
            .children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scopes
            .remove(&self.id);
        self.manager.abort_all();
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
    async fn child_grants_are_execution_scoped_and_parent_routes_and_aborts_them() {
        let (events, mut rx) = broadcast::channel(32);
        let parent = HostApprovalManager::new("parent", events, CancellationToken::new());
        parent.begin_turn();
        let token = CancellationToken::new();
        let child = parent.child("space", "分析", token.clone()).unwrap();
        let waiter = {
            let manager = child.manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        let asked = rx.recv().await.unwrap();
        assert_eq!(asked.properties["sessionID"], "parent");
        assert_eq!(asked.properties["metadata"]["workspaceID"], "space");
        let id = parent.list().await.pop().unwrap().id;
        assert!(parent.reply_with_scope(&id, true, true, "ok".into()).await);
        assert!(waiter.await.unwrap().is_approved());
        assert!(child
            .manager
            .request(ApprovalRequest::new("write_file", json!({}), "write"))
            .await
            .is_approved());
        let scope = asked.properties["metadata"]["scopeID"].clone();
        drop(child);
        assert!(
            !token.is_cancelled(),
            "child completion must not cancel the parent turn"
        );
        let child = parent.child("space", "分析", token.clone()).unwrap();
        let waiter = {
            let manager = child.manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("write_file", json!({}), "write"))
                    .await
            })
        };
        loop {
            let event = rx.recv().await.unwrap();
            if event.event_type == "permission.asked" {
                assert_ne!(event.properties["metadata"]["scopeID"], scope);
                break;
            }
        }
        parent.abort_all();
        assert!(!waiter.await.unwrap().is_approved());
        assert!(parent.list().await.is_empty());
        assert!(parent.child("late", "late", token).is_none());
    }

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
