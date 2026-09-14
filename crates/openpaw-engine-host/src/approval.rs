use async_trait::async_trait;
use openpaw_core::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;

pub struct PendingApproval {
    pub request: ApprovalRequest,
    reply: oneshot::Sender<ApprovalResponse>,
}
pub struct HostApprovalManager {
    pending: Mutex<HashMap<String, PendingApproval>>,
    always: Mutex<HashSet<String>>,
    events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
}
impl HostApprovalManager {
    pub fn new(events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            always: Mutex::new(HashSet::new()),
            events,
        }
    }
    pub fn list(&self) -> Vec<serde_json::Value> {
        self.pending.lock().unwrap().iter().map(|(id, item)| serde_json::json!({"id":id,"tool":item.request.tool_name,"input":item.request.input,"reason":item.request.reason})).collect()
    }
    pub fn reply(&self, id: &str, decision: &str) -> bool {
        let item = self.pending.lock().unwrap().remove(id);
        let Some(item) = item else {
            return false;
        };
        if decision == "always" {
            self.always
                .lock()
                .unwrap()
                .insert(item.request.tool_name.clone());
        }
        let response = if matches!(decision, "once" | "always") {
            ApprovalResponse::Approved
        } else {
            ApprovalResponse::Denied {
                reason: "rejected by user".into(),
            }
        };
        let _ = item.reply.send(response);
        let event = serde_json::json!({"type":"permission.replied","properties":{"id":id,"decision":decision}}).to_string();
        for tx in self.events.lock().unwrap().values() {
            let _ = tx.send(event.clone());
        }
        true
    }
    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}
#[async_trait]
impl ApprovalManager for HostApprovalManager {
    async fn request(&self, request: ApprovalRequest) -> ApprovalResponse {
        if self.always.lock().unwrap().contains(&request.tool_name) {
            return ApprovalResponse::Approved;
        }
        let id = format!("perm_{}", Uuid::new_v4().simple());
        let (reply, wait) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id.clone(),
            PendingApproval {
                request: request.clone(),
                reply,
            },
        );
        let event = serde_json::json!({"type":"permission.asked","properties":{"id":id,"permission":{"tool":request.tool_name,"input":request.input,"reason":request.reason}}}).to_string();
        for tx in self.events.lock().unwrap().values() {
            let _ = tx.send(event.clone());
        }
        wait.await.unwrap_or(ApprovalResponse::Denied {
            reason: "approval cancelled".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::{sleep, Duration};

    fn manager() -> HostApprovalManager {
        HostApprovalManager::new(Arc::new(Mutex::new(HashMap::new())))
    }

    #[tokio::test]
    async fn reply_once_resolves_pending_request() {
        let manager = Arc::new(manager());
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new(
                        "read_file",
                        json!({"path":"a"}),
                        "confirm",
                    ))
                    .await
            })
        };
        for _ in 0..20 {
            if let Some(item) = manager.list().first().cloned() {
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
    async fn always_skips_follow_up_request() {
        let manager = manager();
        let manager = Arc::new(manager);
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("read_file", json!({}), "confirm"))
                    .await
            })
        };
        for _ in 0..20 {
            if let Some(item) = manager.list().first().cloned() {
                let id = item["id"].as_str().unwrap().to_string();
                manager.reply(&id, "always");
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        assert!(task.await.unwrap().is_approved());
        assert!(manager
            .request(ApprovalRequest::new("read_file", json!({}), "confirm"))
            .await
            .is_approved());
    }

    #[tokio::test]
    async fn clear_cancels_pending_request() {
        let manager = Arc::new(manager());
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .request(ApprovalRequest::new("bash", json!({}), "confirm"))
                    .await
            })
        };
        for _ in 0..20 {
            if !manager.list().is_empty() {
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
        manager.clear();
        assert!(!task.await.unwrap().is_approved());
        assert!(manager.list().is_empty());
    }
}
