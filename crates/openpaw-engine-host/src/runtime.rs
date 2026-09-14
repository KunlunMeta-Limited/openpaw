use crate::approval::HostApprovalManager;
use crate::protocol::{Session, Time};
use openpaw_engine::AgentLoop;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
#[derive(Clone)]
pub struct RuntimeState {
    pub sessions: Arc<Mutex<Vec<Session>>>,
    pub events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
    pub loops: Arc<Mutex<HashMap<String, Arc<AgentLoop>>>>,
    pub approvals: Arc<HostApprovalManager>,
}
impl RuntimeState {
    pub fn new() -> Self {
        let events = Arc::new(Mutex::new(HashMap::new()));
        Self {
            sessions: Arc::new(Mutex::new(Vec::new())),
            events: events.clone(),
            loops: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(HostApprovalManager::new(events)),
        }
    }
    pub fn session_exists(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().iter().any(|s| s.id == id)
    }
    pub fn touch_title(&self, id: &str, text: &str) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            if s.title == "New session" && !text.is_empty() {
                s.title = text.chars().take(80).collect();
            }
            s.time.updated = chrono::Utc::now().timestamp_millis();
        }
    }
    pub fn insert_session(&self, session: Session) {
        let (tx, _) = broadcast::channel(256);
        self.sessions.lock().unwrap().push(session.clone());
        self.events.lock().unwrap().insert(session.id, tx);
    }
    pub fn event_sender(&self, id: &str) -> Option<broadcast::Sender<String>> {
        self.events.lock().unwrap().get(id).cloned()
    }
    pub fn new_session(id: String, directory: String, title: String) -> Session {
        let now = chrono::Utc::now().timestamp_millis();
        Session {
            id: id.clone(),
            title,
            slug: id,
            directory,
            time: Time {
                created: now,
                updated: now,
            },
        }
    }
}
