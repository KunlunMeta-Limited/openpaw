use crate::approval::HostApprovalManager;
use crate::protocol::{Session, Time};
use openpaw_engine::AgentLoop;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[derive(Clone)]
pub struct RuntimeState {
    pub sessions: Arc<Mutex<Vec<Session>>>,
    pub events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
    pub loops: Arc<Mutex<HashMap<String, Arc<AgentLoop>>>>,
    pub approvals: Arc<HostApprovalManager>,
    pub history: Arc<Mutex<HashMap<String, Vec<Value>>>>,
}
impl RuntimeState {
    pub fn new() -> Self {
        let events = Arc::new(Mutex::new(HashMap::new()));
        Self {
            sessions: Arc::new(Mutex::new(Vec::new())),
            events: events.clone(),
            loops: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(HostApprovalManager::new(events)),
            history: Arc::new(Mutex::new(HashMap::new())),
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
        let id = session.id.clone();
        self.sessions.lock().unwrap().push(session.clone());
        self.events.lock().unwrap().insert(id.clone(), tx);
        self.history.lock().unwrap().insert(id, Vec::new());
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

    pub fn append_history(&self, id: &str, value: Value) {
        self.history.lock().unwrap().entry(id.to_string()).or_default().push(value.clone());
        let Some(home) = std::env::var_os("OPENPAW_HOME") else { return; };
        let dir = std::path::PathBuf::from(home).join("engine-sessions");
        if fs::create_dir_all(&dir).is_err() { return; }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(dir.join(format!("{id}.jsonl"))) { let _ = writeln!(file, "{}", value); }
    }

    pub fn history(&self, id: &str) -> Vec<Value> { self.history.lock().unwrap().get(id).cloned().unwrap_or_default() }
}
