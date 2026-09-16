use async_trait::async_trait;
use openpaw_core::{
    ApprovalManager, ApprovalRequest, ApprovalResponse, GuardrailDecision, ToolGuardrail,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;

pub struct PendingApproval {
    pub session_id: String,
    pub request: ApprovalRequest,
    reply: oneshot::Sender<ApprovalResponse>,
}

pub struct HostApprovalManager {
    pending: Mutex<HashMap<String, PendingApproval>>,
    events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
    global_events: broadcast::Sender<String>,
}

struct SessionApprovalManager {
    host: Arc<HostApprovalManager>,
    session_id: String,
}

impl HostApprovalManager {
    pub fn new(
        events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
        global_events: broadcast::Sender<String>,
    ) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
            global_events,
        }
    }

    pub fn scoped(self: &Arc<Self>, session_id: impl Into<String>) -> Arc<dyn ApprovalManager> {
        Arc::new(SessionApprovalManager {
            host: self.clone(),
            session_id: session_id.into(),
        })
    }

    pub fn list(&self) -> Vec<serde_json::Value> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|(id, item)| {
                serde_json::json!({
                    "id": id,
                    "sessionID": item.session_id,
                    "permission": item.request.tool_name,
                    "patterns": [item.request.reason],
                    "metadata": {"input": item.request.input},
                    "always": []
                })
            })
            .collect()
    }

    pub fn reply(&self, id: &str, decision: &str) -> bool {
        let item = self.pending.lock().unwrap().remove(id);
        let Some(item) = item else {
            return false;
        };
        let response = if matches!(decision, "once" | "always") {
            ApprovalResponse::Approved
        } else {
            ApprovalResponse::Denied {
                reason: "rejected by user".into(),
            }
        };
        let _ = item.reply.send(response);
        self.emit(
            &item.session_id,
            serde_json::json!({
                "type":"permission.replied",
                "properties":{"sessionID":item.session_id,"requestID":id}
            }),
        );
        true
    }

    pub fn clear_session(&self, session_id: &str) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, item| item.session_id != session_id);
    }

    async fn request_for(&self, session_id: String, request: ApprovalRequest) -> ApprovalResponse {
        let id = format!("prm_{}", Uuid::new_v4().simple());
        let (reply, wait) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id.clone(),
            PendingApproval {
                session_id: session_id.clone(),
                request: request.clone(),
                reply,
            },
        );
        self.emit(
            &session_id,
            serde_json::json!({
                "type":"permission.asked",
                "properties": {
                    "id":id,
                    "sessionID":session_id,
                    "permission":request.tool_name,
                    "patterns":[request.reason],
                    "metadata":{"input":request.input},
                    "always":[]
                }
            }),
        );
        wait.await.unwrap_or(ApprovalResponse::Denied {
            reason: "approval cancelled".into(),
        })
    }

    fn emit(&self, session_id: &str, event: serde_json::Value) {
        let serialized = event.to_string();
        if let Some(tx) = self.events.lock().unwrap().get(session_id) {
            let _ = tx.send(serialized.clone());
        }
        let _ = self.global_events.send(serialized);
    }
}

/// Manual mode asks the attached UI to approve every tool call. The engine's
/// existing guardrail/approval seams still own classification and execution;
/// this adapter only selects the interactive policy required by the host.
pub struct ManualApprovalGuardrail;

#[async_trait]
impl ToolGuardrail for ManualApprovalGuardrail {
    async fn check(&self, name: &str, _args: &serde_json::Value) -> GuardrailDecision {
        GuardrailDecision::Confirm(format!("manual approval required for `{name}`"))
    }
}

#[async_trait]
impl ApprovalManager for SessionApprovalManager {
    async fn request(&self, request: ApprovalRequest) -> ApprovalResponse {
        self.host
            .request_for(self.session_id.clone(), request)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::{sleep, Duration};

    fn manager() -> Arc<HostApprovalManager> {
        let (global, _) = broadcast::channel(16);
        Arc::new(HostApprovalManager::new(
            Arc::new(Mutex::new(HashMap::new())),
            global,
        ))
    }

    #[tokio::test]
    async fn reply_once_resolves_pending_request() {
        let manager = manager();
        let scoped = manager.scoped("ses_1");
        let task = tokio::spawn(async move {
            scoped
                .request(ApprovalRequest::new(
                    "read_file",
                    json!({"path":"a"}),
                    "confirm",
                ))
                .await
        });
        for _ in 0..20 {
            if let Some(item) = manager.list().first().cloned() {
                assert_eq!(item["sessionID"], "ses_1");
                let id = item["id"].as_str().unwrap().to_string();
                assert!(manager.reply(&id, "once"));
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        assert!(task.await.unwrap().is_approved());
        assert!(manager.list().is_empty());
    }

    #[tokio::test]
    async fn always_only_approves_current_request() {
        let manager = manager();
        let scoped = manager.scoped("ses_1");
        let first = scoped.clone();
        let task = tokio::spawn(async move {
            first
                .request(ApprovalRequest::new("read_file", json!({}), "confirm"))
                .await
        });
        for _ in 0..20 {
            if let Some(item) = manager.list().first().cloned() {
                manager.reply(item["id"].as_str().unwrap(), "always");
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        assert!(task.await.unwrap().is_approved());

        let second = scoped.clone();
        let second_task = tokio::spawn(async move {
            second
                .request(ApprovalRequest::new("read_file", json!({}), "confirm"))
                .await
        });
        for _ in 0..20 {
            if let Some(item) = manager.list().first().cloned() {
                manager.reply(item["id"].as_str().unwrap(), "once");
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        assert!(second_task.await.unwrap().is_approved());
        assert!(manager.list().is_empty());
    }

    #[tokio::test]
    async fn manual_guardrail_requests_confirmation() {
        let decision = ManualApprovalGuardrail
            .check("read_file", &json!({"path":"README.md"}))
            .await;
        assert!(decision.needs_confirmation());
    }

    #[tokio::test]
    async fn clear_session_only_cancels_that_sessions_request() {
        let manager = manager();
        let scoped = manager.scoped("ses_1");
        let task = tokio::spawn(async move {
            scoped
                .request(ApprovalRequest::new("bash", json!({}), "confirm"))
                .await
        });
        for _ in 0..20 {
            if !manager.list().is_empty() {
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        manager.clear_session("ses_1");
        assert!(!task.await.unwrap().is_approved());
        assert!(manager.list().is_empty());
    }
}
