//! 功能：管理 Host 侧的人工审批请求，并把审批结果回传给 AgentLoop。
//! 输入输出边界：输入来自引擎的 `ApprovalManager::request`，输出为
//! `permission.asked`/`permission.replied` 事件和单次审批响应。
//! 设计约束：待审批项只保存在当前 session 和当前回合内；`once` 与
//! `always` 在本实现中都只影响当前请求，不形成跨回合授权。
//! 关联：`API-HOST-PERMISSION-001`、`EVT-HOST-PERMISSION-001`、
//! `REQ-B-HOST-007`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openpaw_core::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::protocol::{OpenCodeEvent, PermissionInfo};

struct Pending {
    info: PermissionInfo,
    tx: oneshot::Sender<ApprovalResponse>,
}

/// 功能：提供一个 session 范围内的人工审批管理器。
/// 字段/状态：内部保存待审批请求、当前回合取消令牌和中止标记。
/// 数据来源：AgentLoop 发起的 `ApprovalRequest`。
/// 数据去向：Host SSE 订阅者、全局事件订阅者和等待中的工具调用。
/// 约束：审批请求必须在同一 session 内回复；回合中止后拒绝迟到回复。
/// 并发与取消：所有生命周期状态通过互斥锁线性化；`abort_all` 会唤醒全部等待者。
/// 权限：只传递审批结果，不记录 Basic Authentication 凭据。
/// 稳定性：Host 内部 L3 状态边界，修改必须建立 CHG。
/// 关联：`API-HOST-PERMISSION-001`、`EVT-HOST-PERMISSION-001`。
pub struct HostApprovalManager {
    session_id: String,
    lifecycle: Mutex<ApprovalLifecycle>,
    events: broadcast::Sender<OpenCodeEvent>,
    global_events: broadcast::Sender<OpenCodeEvent>,
}

struct ApprovalLifecycle {
    pending: HashMap<String, Pending>,
    cancelled: CancellationToken,
    aborted: bool,
}

impl HostApprovalManager {
    /// 向 session 和全局事件通道发布一次审批结束事件。
    /// 参数：`id` 为请求编号；`status` 为 approved/rejected；`reason` 为中文原因。
    /// 约束：调用方应已在线程安全的生命周期锁内移除 pending，避免重复结束通知。
    fn emit_replied(&self, id: &str, status: &str, reason: &str) {
        let event = OpenCodeEvent::new(
            "permission.replied",
            serde_json::json!({
                "sessionID": self.session_id,
                "requestID": id,
                "status": status,
                "reason": reason,
            }),
        );
        let _ = self.events.send(event.clone());
        let _ = self.global_events.send(event);
    }

    /// 功能：为一个 session 创建审批管理器并绑定事件通道与回合取消令牌。
    /// 参数：`session_id` 为 session 稳定编号；`events` 为 session 事件通道；
    /// `global_events` 为全局事件通道；`cancelled` 为当前回合取消令牌。
    /// 返回：可在线程间共享的审批管理器。
    /// 错误：不会主动返回错误；通道关闭时事件发送被忽略，等待者收到拒绝结果。
    /// 状态：初始没有待审批项，回合未中止。
    /// 并发与取消：管理器可由多个异步任务共享；取消令牌只服务于当前回合。
    /// 权限：不改变调用方权限，仅承载工具审批。
    /// 关联：`REQ-B-HOST-007`、`CHG-2026-0003`。
    pub fn new(
        session_id: impl Into<String>,
        events: broadcast::Sender<OpenCodeEvent>,
        global_events: broadcast::Sender<OpenCodeEvent>,
        cancelled: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.into(),
            lifecycle: Mutex::new(ApprovalLifecycle {
                pending: HashMap::new(),
                cancelled,
                aborted: false,
            }),
            events,
            global_events,
        })
    }

    /// 功能：为新保留的回合开启一套全新的审批生命周期。
    /// 参数：无；管理器使用构造时绑定的 session 和事件通道。
    /// 返回：无。
    /// 错误：无；上一回合仍在等待的审批会收到“审批回合已结束”拒绝结果。
    /// 状态：清空旧待审批项，替换已取消令牌，并将回合中止标记复位。
    /// 并发与取消：在同一互斥锁内完成令牌替换和请求清理，避免新旧回合交叉。
    /// 权限：不改变权限策略，只重置生命周期。
    /// 关联：`REQ-B-HOST-007`、`CHG-2026-0003`。
    pub fn begin_turn(&self) {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        lifecycle.cancelled.cancel();
        for (id, request) in lifecycle.pending.drain() {
            let _ = request.tx.send(ApprovalResponse::Denied {
                reason: "审批回合已结束".into(),
            });
            self.emit_replied(&id, "rejected", "审批回合已结束");
        }
        lifecycle.cancelled = CancellationToken::new();
        lifecycle.aborted = false;
    }

    /// 功能：列出当前 session 当前回合尚未完成的审批请求。
    /// 参数：无。
    /// 返回：待审批权限信息的快照；没有请求时返回空数组。
    /// 错误：无；锁中毒时沿用内部值继续读取。
    /// 状态：只读，不移除或改变待审批项。
    /// 并发与取消：返回值是复制快照，调用期间新增或回复的请求不修改已返回数组。
    /// 权限：调用方必须已通过 Host 鉴权，敏感输入仅按协议规定返回。
    /// 关联：`API-HOST-PERMISSION-001`。
    pub async fn list(&self) -> Vec<PermissionInfo> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    /// 功能：回复指定审批请求，并向 session 与全局事件通道发布结果事件。
    /// 参数：`id` 为待审批请求编号；`approved` 表示批准或拒绝；`reason` 为处理原因。
    /// 返回：请求存在且成功交付回复时返回 `true`；请求不存在、已中止或已被处理时返回 `false`。
    /// 错误：不会抛出错误；底层 oneshot 或广播通道关闭只表示对应接收方已离开。
    /// 状态：成功回复后请求从 pending 移除，并产生 `permission.replied`。
    /// 并发与取消：回复与 `abort_all` 在同一锁内竞争，至多有一个操作消费请求。
    /// 权限：鉴权由 HTTP 层负责；不得把凭据写入 `reason` 或事件。
    /// 关联：`API-HOST-PERMISSION-001`、`EVT-HOST-PERMISSION-001`。
    pub async fn reply(&self, id: &str, approved: bool, reason: String) -> bool {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if lifecycle.aborted {
            return false;
        }
        let pending = lifecycle.pending.remove(id);
        let Some(pending) = pending else { return false };
        let response = if approved {
            ApprovalResponse::Approved
        } else {
            ApprovalResponse::Denied {
                reason: reason.clone(),
            }
        };
        let _ = pending.tx.send(response);
        let status = if approved { "approved" } else { "rejected" };
        self.emit_replied(id, status, &reason);
        true
    }

    /// 功能：中止当前回合并唤醒所有等待审批的工具调用。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无；每个等待项都会收到“回合已取消”的拒绝响应。
    /// 状态：设置中止标记、取消当前令牌并清空 pending；后续请求直接拒绝。
    /// 并发与取消：与 `request`/`reply` 线性竞争，确保不存在迟到审批事件。
    /// 权限：不绕过权限检查，只把未完成操作安全终止。
    /// 关联：`API-HOST-SESSION-007`、`REQ-B-HOST-006`。
    pub fn abort_all(&self) {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        lifecycle.aborted = true;
        lifecycle.cancelled.cancel();
        for (id, request) in lifecycle.pending.drain() {
            let _ = request.tx.send(ApprovalResponse::Denied {
                reason: "回合已取消".into(),
            });
            self.emit_replied(&id, "rejected", "回合已取消");
        }
    }
}

#[async_trait::async_trait]
impl ApprovalManager for HostApprovalManager {
    /// 功能：登记一次工具审批请求，并等待人工回复或回合取消。
    /// 参数：`req` 包含工具名称、工具输入和需要审批的原因。
    /// 返回：人工批准返回 `Approved`；拒绝、取消、通道关闭或已中止回合返回 `Denied`。
    /// 错误：审批协议不抛出错误，失败原因编码在 `ApprovalResponse::Denied` 中。
    /// 状态：先发布 `permission.asked`，完成、回合重置或取消后移除 pending 并发布 `permission.replied`。
    /// 并发与取消：等待同时监听 oneshot 回复和当前回合取消令牌；取消优先终止等待。
    /// 权限：输入只供受控工具执行，不能因审批事件而扩大工具权限。
    /// 关联：`EVT-HOST-PERMISSION-001`、`CHG-2026-0003`。
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        let id = Uuid::new_v4().simple().to_string();
        let info = PermissionInfo {
            id: id.clone(),
            session_id: self.session_id.clone(),
            permission: req.tool_name.clone(),
            patterns: vec![req.reason.clone()],
            metadata: serde_json::json!({}),
            always: Vec::new(),
            input: req.input,
        };
        let (tx, rx) = oneshot::channel();
        let cancelled = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if lifecycle.aborted {
                return ApprovalResponse::Denied {
                    reason: "回合已取消".into(),
                };
            }
            let cancelled = lifecycle.cancelled.clone();
            lifecycle.pending.insert(
                id.clone(),
                Pending {
                    info: info.clone(),
                    tx,
                },
            );
            // 在生命周期锁内发布事件，使 asked 与 abort 线性化：abort_all 返回后，
            // 当前回合不会再产生迟到的 asked 事件。
            let event = OpenCodeEvent::new("permission.asked", info.clone());
            let _ = self.events.send(event.clone());
            let _ = self.global_events.send(event);
            cancelled
        };
        if cancelled.is_cancelled() {
            self.lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pending
                .remove(&id);
            return ApprovalResponse::Denied {
                reason: "回合已取消".into(),
            };
        }
        tokio::select! {
            response = rx => response.unwrap_or(ApprovalResponse::Denied { reason: "审批通道已关闭".into() }),
            _ = cancelled.cancelled() => {
                self.lifecycle
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pending
                    .remove(&id);
                ApprovalResponse::Denied { reason: "回合已取消".into() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::ApprovalManager;
    use serde_json::json;

    #[tokio::test]
    async fn pending_permission_roundtrip_and_reply_event() {
        let (events, mut rx) = broadcast::channel(8);
        let (global, _global_rx) = broadcast::channel(8);
        let manager =
            HostApprovalManager::new("session-1", events, global, CancellationToken::new());
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
        assert!(waiting.await.unwrap().is_approved());
        let replied = rx.recv().await.expect("permission replied");
        assert_eq!(replied.event_type, "permission.replied");
        assert_eq!(replied.properties["requestID"], permission_id);
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn abort_after_request_wakes_waiter_and_next_turn_resets_state() {
        let (events, mut rx) = broadcast::channel(8);
        let (global, _global_rx) = broadcast::channel(8);
        let manager =
            HostApprovalManager::new("session-1", events, global, CancellationToken::new());
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
        assert_eq!(aborted_event.properties["status"], "rejected");
        assert_eq!(aborted_event.properties["reason"], "回合已取消");
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
        let (events, mut rx) = broadcast::channel(8);
        let (global, _global_rx) = broadcast::channel(8);
        let manager =
            HostApprovalManager::new("session-1", events, global, CancellationToken::new());
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
        assert_eq!(replied.properties["reason"], "审批回合已结束");
        assert!(matches!(
            waiting.await.unwrap(),
            ApprovalResponse::Denied { .. }
        ));
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn request_after_abort_is_denied_without_a_late_asked_event() {
        let (events, mut rx) = broadcast::channel(8);
        let (global, _global_rx) = broadcast::channel(8);
        let manager =
            HostApprovalManager::new("session-1", events, global, CancellationToken::new());
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
}
