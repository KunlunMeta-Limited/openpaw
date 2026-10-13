//! 功能：共享人工审批生命周期与 manual 策略。
//! 参数：中立通知和唯一请求 ID 生成器。
//! 返回：已有 Core ApprovalResponse。
//! 错误：取消、关闭和无效答复均拒绝。
//! 状态：pending 与授权仅驻留当前 session 内存。
//! 并发与取消：单锁线性化，future 丢弃清理。
//! 权限：批准不绕过工具自身权限。
//! 关联：CLI 与 Host adapter。
use openpaw_core::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// 功能：携带入口适配所需的中立审批通知。
#[derive(Clone)]
pub enum ApprovalNotice {
    /// 新 pending 的登记通知；入口只能非阻塞发送或排队。
    Asked {
        /// 当前 session 中不可复用的审批身份。
        id: String,
        /// 工具请求；终端不得原样展示 input 或 reason。
        request: ApprovalRequest,
    },
    /// pending 已被消费或清理的唯一结束通知。
    Replied {
        /// 已结束的审批身份。
        id: String,
    },
}
struct Pending {
    request: ApprovalRequest,
    tx: oneshot::Sender<ApprovalResponse>,
    generation: u64,
}
struct Lifecycle {
    pending: HashMap<String, Pending>,
    allowed: HashSet<String>,
    used_ids: HashSet<String>,
    cancelled: CancellationToken,
    aborted: bool,
    generation: u64,
}
/// 功能：统一 pending、回合取消与 session 工具授权。
pub struct ApprovalGate {
    lifecycle: Mutex<Lifecycle>,
    notify: Box<dyn Fn(ApprovalNotice) + Send + Sync>,
    next_id: Box<dyn Fn() -> String + Send + Sync>,
}
impl ApprovalGate {
    /// 功能：创建独立 session 审批 gate。
    /// 参数：cancelled 为外部取消令牌；next_id 返回唯一 ID；notify 仅非阻塞排队且不得回调 gate。
    /// 返回：Arc<ApprovalGate>。
    /// 错误：无。
    /// 状态：初始 pending/授权为空。
    /// 并发与取消：生命周期单锁；持锁通知不得等待 I/O。
    /// 权限：授权不持久化、不跨 session。
    /// 关联：ApprovalManager 与入口 adapter。
    pub fn new(
        cancelled: CancellationToken,
        next_id: impl Fn() -> String + Send + Sync + 'static,
        notify: impl Fn(ApprovalNotice) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new(Lifecycle {
                pending: HashMap::new(),
                allowed: HashSet::new(),
                used_ids: HashSet::new(),
                cancelled,
                aborted: false,
                generation: 0,
            }),
            notify: Box::new(notify),
            next_id: Box::new(next_id),
        })
    }
    /// 功能：拒绝旧等待并开启新审批代际。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：保留当前 session 同工具授权，清空 pending。
    /// 并发与取消：与 reply/abort/request 单锁竞争。
    /// 权限：不扩大授权作用域。
    /// 关联：Host 回合保留与 CLI 每轮开始。
    pub fn begin_turn(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        lifecycle.cancelled.cancel();
        self.drain(&mut lifecycle, "审批回合已结束");
        lifecycle.cancelled = CancellationToken::new();
        lifecycle.aborted = false;
        lifecycle.generation = lifecycle.generation.wrapping_add(1);
    }
    /// 功能：拒绝并清空当前回合审批。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：标记 aborted 并取消当前令牌。
    /// 并发与取消：与 reply 线性竞争，future 清理不会重复通知。
    /// 权限：取消不表示批准。
    /// 关联：AgentLoop 取消和回合结束。
    pub fn abort_all(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        lifecycle.aborted = true;
        lifecycle.cancelled.cancel();
        self.drain(&mut lifecycle, "回合已取消");
    }
    fn drain(&self, lifecycle: &mut Lifecycle, reason: &str) {
        for (id, pending) in lifecycle.pending.drain() {
            (self.notify)(ApprovalNotice::Replied { id });
            let _ = pending.tx.send(denied(reason));
        }
    }
    /// 功能：复制当前 pending 快照。
    /// 参数：无。
    /// 返回：请求 ID 与 ApprovalRequest 列表。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：单锁复制，返回后新增/回复不修改已返快照。
    /// 权限：调用方负责安全展示输入。
    /// 关联：Host GET permission。
    pub fn list(&self) -> Vec<(String, ApprovalRequest)> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .iter()
            .map(|(id, p)| (id.clone(), p.request.clone()))
            .collect()
    }
    /// 功能：消费一次审批请求并交付决定。
    /// 参数：id、approved、session 授权开关和安全原因。
    /// 返回：成功交付为 true，失效/取消/已消费为 false。
    /// 错误：通道关闭返回 false。
    /// 状态：交付成功且指定作用域时记住同工具授权。
    /// 并发与取消：与 abort/reset 线性竞争，通知至多一次。
    /// 权限：只在本 session 后续回合复用，不绕过工具限制。
    /// 关联：终端和 Host permission 回复。
    pub fn reply(
        &self,
        id: &str,
        approved: bool,
        remember_for_session: bool,
        reason: String,
    ) -> bool {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if lifecycle.aborted || lifecycle.cancelled.is_cancelled() {
            return false;
        }
        let Some(pending) = lifecycle.pending.remove(id) else {
            return false;
        };
        let name = pending.request.tool_name;
        let response = if approved {
            ApprovalResponse::Approved
        } else {
            denied(&reason)
        };
        let delivered = pending.tx.send(response).is_ok();
        if delivered && approved && remember_for_session {
            lifecycle.allowed.insert(name);
        }
        (self.notify)(ApprovalNotice::Replied { id: id.to_string() });
        delivered
    }
    fn remove_waiter(&self, id: &str, generation: u64) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if lifecycle
            .pending
            .get(id)
            .is_some_and(|p| p.generation == generation)
        {
            lifecycle.pending.remove(id);
            (self.notify)(ApprovalNotice::Replied { id: id.to_string() });
        }
    }
}
struct Waiter<'a> {
    gate: &'a ApprovalGate,
    id: String,
    generation: u64,
}
impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.gate.remove_waiter(&self.id, self.generation);
    }
}
#[async_trait::async_trait]
impl ApprovalManager for ApprovalGate {
    /// 功能：执行已有 Core request 接口。
    /// 参数：见既有 Core trait 请求。
    /// 返回：既有 Core 决定类型。
    /// 错误：取消/失效安全拒绝。
    /// 状态：审批等待由共享 gate 管理；guardrail 检查为纯计算。
    /// 并发与取消：取消或 future 丢弃清理 pending；通知不阻塞。
    /// 权限：不绕过工具权限，不展示凭据。
    /// 关联：CHG-2026-0023 共用审批。
    async fn request(&self, request: ApprovalRequest) -> ApprovalResponse {
        let id = (self.next_id)();
        let (tx, rx) = oneshot::channel();
        let (cancelled, generation) = {
            let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
            if lifecycle.aborted || lifecycle.cancelled.is_cancelled() {
                return denied("回合已取消");
            }
            if lifecycle.allowed.contains(&request.tool_name) {
                return ApprovalResponse::Approved;
            }
            if id.is_empty() || !lifecycle.used_ids.insert(id.clone()) {
                return denied("审批身份已失效");
            }
            let generation = lifecycle.generation;
            lifecycle.pending.insert(
                id.clone(),
                Pending {
                    request: request.clone(),
                    tx,
                    generation,
                },
            );
            // 通知只允许非阻塞排队，与 abort/reply 同锁排序。
            (self.notify)(ApprovalNotice::Asked {
                id: id.clone(),
                request,
            });
            (lifecycle.cancelled.clone(), generation)
        };
        let _waiter = Waiter {
            gate: self,
            id,
            generation,
        };
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => denied("回合已取消"),
            response = rx => response.unwrap_or_else(|_| denied("审批通道已关闭")),
        }
    }
}
fn denied(reason: &str) -> ApprovalResponse {
    ApprovalResponse::Denied {
        reason: reason.into(),
    }
}

/// 功能：要求每个工具通过人工审批。
pub struct ManualGuardrail;
#[async_trait::async_trait]
impl openpaw_core::ToolGuardrail for ManualGuardrail {
    /// 功能：执行已有 Core check 接口。
    /// 参数：见既有 Core trait 请求。
    /// 返回：既有 Core 决定类型。
    /// 错误：取消/失效安全拒绝。
    /// 状态：审批等待由共享 gate 管理；guardrail 检查为纯计算。
    /// 并发与取消：取消或 future 丢弃清理 pending；通知不阻塞。
    /// 权限：不绕过工具权限，不展示凭据。
    /// 关联：CHG-2026-0023 共用审批。
    async fn check(
        &self,
        name: &str,
        _args: &serde_json::Value,
    ) -> openpaw_core::GuardrailDecision {
        openpaw_core::GuardrailDecision::Confirm(format!("manual approval required for `{name}`"))
    }
}
/// 功能：组装已有 AgentLoop 的审批接口。
/// 参数：agent、manual 开关和已有 Core ApprovalManager。
/// 返回：配置后的 AgentLoop。
/// 错误：无。
/// 状态：仅使用现有 builder。
/// 并发与取消：取消算法仍由 AgentLoop 管理。
/// 权限：manual 要求确认；工具自身限制仍执行。
/// 关联：CLI chat 与 Host configured_agent。
pub fn configure(
    agent: crate::AgentLoop,
    manual: bool,
    manager: Arc<dyn ApprovalManager>,
) -> crate::AgentLoop {
    let agent = agent.with_approval_manager(manager);
    if manual {
        agent.with_guardrail(Arc::new(ManualGuardrail))
    } else {
        agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn replies_racing_abort_or_reset_close_each_request_exactly_once() {
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        let next = std::sync::atomic::AtomicU64::new(0);
        let gate = ApprovalGate::new(
            Default::default(),
            move || {
                next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .to_string()
            },
            move |notice| {
                let _ = tx.send(notice);
            },
        );
        for i in 0..100 {
            gate.begin_turn();
            let task = {
                let gate = gate.clone();
                tokio::spawn(async move {
                    gate.request(ApprovalRequest::new(
                        "write_file",
                        serde_json::json!({}),
                        "write",
                    ))
                    .await
                })
            };
            let ApprovalNotice::Asked { id, .. } = notices.recv().await.unwrap() else {
                panic!("asked first")
            };
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    gate.reply(&id, true, false, String::new());
                });
                scope.spawn(|| {
                    if i % 2 == 0 {
                        gate.abort_all();
                    } else {
                        gate.begin_turn();
                    }
                });
            });
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();
            assert!(gate.list().is_empty());
            assert!(!gate.reply(&id, true, true, String::new()));
            let ApprovalNotice::Replied { id: closed } = notices.try_recv().unwrap() else {
                panic!("replied")
            };
            assert_eq!(closed, id);
            assert!(notices.try_recv().is_err());
        }
    }
}
