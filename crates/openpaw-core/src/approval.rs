//! Approval flow — the human gate for `Confirm` decisions.
//!
//! Stability: stable.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request for the user to approve (or deny) a guarded action.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input: Value,
    /// Why the approval is needed (guardrail `Confirm` reason).
    pub reason: String,
}

impl ApprovalRequest {
    pub fn new(tool_name: impl Into<String>, input: Value, reason: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            input,
            reason: reason.into(),
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
