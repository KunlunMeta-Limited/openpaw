//! Approval flow — the human gate for `Confirm` decisions.
//!
//! Stability: stable.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 用途：用户批准或拒绝最终工具输入的请求。
/// 字段：工具、输入、原因和可选精确绑定。
/// 来源去向：引擎到既有ApprovalManager。
/// 约束：旧构造不带绑定，新调用必须与执行上下文一致。
/// 稳定性：保留旧构造，struct literal调用者需补binding。
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input: Value,
    /// Why the approval is needed (guardrail `Confirm` reason).
    pub reason: String,
    /// 用途：可选的精确调用关联
    /// 字段：Run/call/最终输入
    /// 来源去向：引擎到审批器
    /// 约束：旧构造为空；不能信任客户端自行发送批准
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<crate::ToolCallBinding>,
}

impl ApprovalRequest {
    /// 功能：创建兼容旧调用方的审批请求
    /// 参数：tool_name/input/reason为最终工具内容和原因
    /// 返回：无绑定请求，新的引擎会附精确绑定
    /// 错误：无
    /// 状态：不执行工具，也不授予权限
    /// 并发与取消：无等待
    /// 权限：实际审批仍由ApprovalManager决定
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn new(tool_name: impl Into<String>, input: Value, reason: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            input,
            reason: reason.into(),
            binding: None,
        }
    }
}

/// User's decision on an [`ApprovalRequest`], tagged for event logging.
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum ApprovalResponse {
    /// Approve (variants for remember-once / remember-always land with the
    /// approval-whitelist work in E2).
    Approved,
    Denied {
        reason: String,
    },
}

impl ApprovalResponse {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// The approval sink. UI layers implement this; the engine awaits it
/// during the `Confirm` phase.
///
/// Stability: stable.
#[async_trait]
pub trait ApprovalManager: Send + Sync {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse;
}

/// Approves everything — headless/CI default.
///
/// Stability: stable.
pub struct AutoApproveManager;

#[async_trait]
impl ApprovalManager for AutoApproveManager {
    async fn request(&self, _req: ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse::Approved
    }
}

/// Denies everything — safe default when no approver is wired.
///
/// Stability: stable.
pub struct AutoDenyManager;

#[async_trait]
impl ApprovalManager for AutoDenyManager {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse::Denied {
            reason: format!("no approval manager configured; denied `{}`", req.tool_name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn auto_approve_and_deny() {
        let req = ApprovalRequest::new("bash", json!({"cmd": "ls"}), "runs shell");
        assert!(AutoApproveManager.request(req.clone()).await.is_approved());
        let denied = AutoDenyManager.request(req).await;
        assert!(!denied.is_approved());
        match denied {
            ApprovalResponse::Denied { reason } => assert!(reason.contains("bash")),
            _ => panic!("expected Denied"),
        }
    }
}
