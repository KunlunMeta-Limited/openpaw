use crate::approval::HostApprovalManager;
use crate::protocol::{Message, ModelRef, Provider, ProviderList, ProviderModel, Session, Time};
use crate::HostConfig;
use openpaw_core::{LlmProvider, Session as EngineSession, SessionEvent};
use openpaw_engine::AgentLoop;
use openpaw_llm::{AnthropicProvider, EchoProvider, MockMode, MockProvider, OpenAiProvider};
use openpaw_store::SessionWriter;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

#[derive(Clone)]
struct ProviderConfig {
    default_provider: String,
    mock_mode: String,
    openai_api_key: Option<String>,
    openai_base_url: String,
    openai_model: String,
    anthropic_api_key: Option<String>,
    anthropic_base_url: String,
    anthropic_model: String,
}

impl ProviderConfig {
    /// `default_provider` comes from the CLI/env (`--provider` >
    /// `OPENPAW_ENGINE_PROVIDER` > `mock`); the rest stays environment-driven.
    fn new(default_provider: String) -> Self {
        Self {
            default_provider,
            mock_mode: env_value("OPENPAW_ENGINE_MOCK").unwrap_or_else(|| "text".into()),
            openai_api_key: env_value("OPENAI_API_KEY"),
            openai_base_url: env_value("OPENAI_BASE_URL")
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
            openai_model: env_value("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o".into()),
            anthropic_api_key: env_value("ANTHROPIC_API_KEY"),
            anthropic_base_url: env_value("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            anthropic_model: env_value("ANTHROPIC_MODEL")
                .unwrap_or_else(|| "claude-sonnet-4".into()),
        }
    }

    fn catalog(&self) -> ProviderList {
        let providers = [
            provider("mock", "OpenPaw mock", "mock-1"),
            provider("echo", "OpenPaw echo", "echo-1"),
            provider("anthropic", "Anthropic", &self.anthropic_model),
            provider("openai", "OpenAI", &self.openai_model),
        ];
        let mut connected = vec!["mock".into(), "echo".into()];
        if self.anthropic_api_key.is_some() {
            connected.push("anthropic".into());
        }
        if self.openai_api_key.is_some() {
            connected.push("openai".into());
        }
        ProviderList {
            all: providers.into(),
            connected,
        }
    }

    fn default_identity(&self) -> (String, String) {
        match self.default_provider.as_str() {
            "echo" => ("echo".into(), "echo-1".into()),
            "openai" => ("openai".into(), self.openai_model.clone()),
            "anthropic" => ("anthropic".into(), self.anthropic_model.clone()),
            _ => ("mock".into(), "mock-1".into()),
        }
    }

    fn resolve(
        &self,
        requested: Option<&ModelRef>,
        mock_text: &str,
    ) -> Result<ResolvedProvider, String> {
        let provider_id = requested
            .map(|model| model.provider_id.as_str())
            .unwrap_or(&self.default_provider);
        let requested_model = requested.map(|model| model.model_id.as_str());
        match provider_id {
            "mock" => {
                validate_model("mock", requested_model, "mock-1")?;
                let text = if mock_text.is_empty() {
                    "ok"
                } else {
                    mock_text
                };
                let provider = match self.mock_mode.as_str() {
                    "tools" => MockProvider::scripted(vec![
                        MockMode::ToolUse {
                            tool_name: "read_file".into(),
                            input: serde_json::json!({"path":"README.md"}),
                            text: "reading workspace file".into(),
                        },
                        MockMode::Fixed("tool call completed".into()),
                    ]),
                    "stream" => MockProvider::stream_text(
                        format!("{} {text}", "streaming mock response".repeat(64)),
                        64,
                    ),
                    _ => {
                        let chunks = text.chars().count().clamp(1, 4);
                        MockProvider::stream_text(text, chunks)
                    }
                };
                Ok(ResolvedProvider::new(Arc::new(provider), "mock", "mock-1"))
            }
            "echo" => {
                validate_model("echo", requested_model, "echo-1")?;
                Ok(ResolvedProvider::new(
                    Arc::new(EchoProvider::new()),
                    "echo",
                    "echo-1",
                ))
            }
            "openai" => {
                validate_model("openai", requested_model, &self.openai_model)?;
                let key = self.openai_api_key.clone().ok_or_else(|| {
                    "provider `openai` is not configured: OPENAI_API_KEY is missing".to_string()
                })?;
                let model = requested_model.unwrap_or(&self.openai_model).to_string();
                let provider =
                    OpenAiProvider::new(key, &model).with_base_url(self.openai_base_url.clone());
                Ok(ResolvedProvider::new(Arc::new(provider), "openai", model))
            }
            "anthropic" => {
                validate_model("anthropic", requested_model, &self.anthropic_model)?;
                let key = self.anthropic_api_key.clone().ok_or_else(|| {
                    "provider `anthropic` is not configured: ANTHROPIC_API_KEY is missing"
                        .to_string()
                })?;
                let model = requested_model.unwrap_or(&self.anthropic_model).to_string();
                let provider = AnthropicProvider::new(key, &model)
                    .with_base_url(self.anthropic_base_url.clone());
                Ok(ResolvedProvider::new(
                    Arc::new(provider),
                    "anthropic",
                    model,
                ))
            }
            other => Err(format!(
                "unsupported provider `{other}`; expected mock, echo, anthropic, or openai"
            )),
        }
    }
}

pub struct ResolvedProvider {
    pub provider: Arc<dyn LlmProvider>,
    pub provider_id: String,
    pub model_id: String,
}

impl ResolvedProvider {
    fn new(
        provider: Arc<dyn LlmProvider>,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn provider(id: &str, name: &str, model_id: &str) -> Provider {
    let model = ProviderModel {
        id: model_id.into(),
        name: model_id.into(),
    };
    Provider {
        id: id.into(),
        name: name.into(),
        models: BTreeMap::from([(model_id.into(), model)]),
    }
}

fn validate_model(provider: &str, requested: Option<&str>, expected: &str) -> Result<(), String> {
    if let Some(model) = requested {
        if model != expected {
            return Err(format!(
                "unsupported model `{model}` for provider `{provider}`; expected `{expected}`"
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct RuntimeState {
    sessions: Arc<Mutex<Vec<Session>>>,
    events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
    global_events: broadcast::Sender<String>,
    engine_sessions: Arc<Mutex<HashMap<String, Arc<AsyncMutex<EngineSession>>>>>,
    messages: Arc<Mutex<HashMap<String, Vec<Message>>>>,
    loops: Arc<Mutex<HashMap<String, Arc<AgentLoop>>>>,
    writers: Arc<Mutex<HashMap<String, SessionWriter>>>,
    pub approvals: Arc<HostApprovalManager>,
    providers: Arc<ProviderConfig>,
    default_workspace: String,
    home: PathBuf,
    manual_permissions: bool,
}
impl RuntimeState {
    pub fn new(config: &HostConfig) -> Self {
        Self::with_provider_config(
            ProviderConfig::new(config.provider.clone()),
            config.workspace.clone(),
            config.home.clone(),
        )
    }

    fn with_provider_config(providers: ProviderConfig, workspace: PathBuf, home: PathBuf) -> Self {
        let events = Arc::new(Mutex::new(HashMap::new()));
        let (global_events, _) = broadcast::channel(1024);
        Self {
            sessions: Arc::new(Mutex::new(Vec::new())),
            events: events.clone(),
            global_events: global_events.clone(),
            engine_sessions: Arc::new(Mutex::new(HashMap::new())),
            messages: Arc::new(Mutex::new(HashMap::new())),
            loops: Arc::new(Mutex::new(HashMap::new())),
            writers: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(HostApprovalManager::new(events, global_events)),
            providers: Arc::new(providers),
            default_workspace: workspace.to_string_lossy().into_owned(),
            home,
            manual_permissions: env_value("OPENPAW_ENGINE_PERMISSIONS").as_deref()
                == Some("manual"),
        }
    }

    /// Workspace used when a request carries neither `x-opencode-directory`
    /// nor `?directory=` (§3.2 third tier).
    pub fn default_workspace(&self) -> String {
        self.default_workspace.clone()
    }

    pub fn provider_list(&self) -> ProviderList {
        self.providers.catalog()
    }

    pub fn resolve_provider(
        &self,
        requested: Option<&ModelRef>,
        mock_text: &str,
    ) -> Result<ResolvedProvider, String> {
        self.providers.resolve(requested, mock_text)
    }

    pub fn list_sessions(&self) -> Vec<Session> {
        self.sessions.lock().unwrap().clone()
    }

    pub fn session_exists(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().iter().any(|s| s.id == id)
    }

    pub fn session(&self, id: &str) -> Option<Session> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    pub fn touch_title(&self, id: &str, text: &str) -> Option<Session> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.iter_mut().find(|session| session.id == id)?;
        if session.title == "New session" && !text.is_empty() {
            session.title = text.chars().take(80).collect();
        }
        session.time.updated = chrono::Utc::now().timestamp_millis();
        Some(session.clone())
    }

    pub fn create_session(&self, directory: String, title: String) -> Result<Session, String> {
        let (provider, model) = self.providers.default_identity();
        let engine_session = EngineSession::new(model, provider);
        let id = engine_session.meta.id.clone();
        let now = chrono::Utc::now().timestamp_millis();
        let session = Session {
            id: id.clone(),
            title,
            slug: id.clone(),
            directory,
            time: Time {
                created: now,
                updated: now,
            },
        };
        // Transcript: <home>/sessions/<id>.jsonl. `home` comes from
        // `--home` / `OPENPAW_HOME` / `~/.openpaw`, so every session is
        // persisted regardless of how the host was started.
        let path = self.home.join("sessions").join(format!("{id}.jsonl"));
        let mut writer = SessionWriter::create(path)
            .map_err(|error| format!("cannot create session transcript: {error}"))?;
        writer
            .append(&SessionEvent::Meta(engine_session.meta.clone()))
            .map_err(|error| format!("cannot write session metadata: {error}"))?;
        writer
            .append(&SessionEvent::Metadata {
                title: Some(session.title.clone()),
                mode: Some("engine-host".into()),
                model_override: None,
                cwd: Some(session.directory.clone()),
                workspace_id: None,
                workspace_name: None,
                task_id: None,
            })
            .map_err(|error| format!("cannot write session workspace metadata: {error}"))?;
        let writer = Some(writer);
        let (tx, _) = broadcast::channel(256);
        self.sessions.lock().unwrap().push(session.clone());
        self.events.lock().unwrap().insert(id.clone(), tx);
        self.engine_sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(AsyncMutex::new(engine_session)));
        self.messages.lock().unwrap().insert(id, Vec::new());
        if let Some(writer) = writer {
            self.writers
                .lock()
                .unwrap()
                .insert(session.id.clone(), writer);
        }
        Ok(session)
    }

    pub fn manual_permissions(&self) -> bool {
        self.manual_permissions
    }

    pub fn persist_session_event(&self, id: &str, event: &SessionEvent) -> Result<(), String> {
        let mut writers = self.writers.lock().unwrap();
        let Some(writer) = writers.get_mut(id) else {
            return Ok(());
        };
        writer
            .append(event)
            .map_err(|error| format!("cannot append session transcript: {error}"))
    }

    pub fn engine_session(&self, id: &str) -> Option<Arc<AsyncMutex<EngineSession>>> {
        self.engine_sessions.lock().unwrap().get(id).cloned()
    }

    pub fn event_sender(&self, id: &str) -> Option<broadcast::Sender<String>> {
        self.events.lock().unwrap().get(id).cloned()
    }

    pub fn subscribe_global(&self) -> broadcast::Receiver<String> {
        self.global_events.subscribe()
    }

    pub fn emit(&self, id: &str, value: Value) {
        let serialized = value.to_string();
        if let Some(tx) = self.event_sender(id) {
            let _ = tx.send(serialized.clone());
        }
        let _ = self.global_events.send(serialized);
    }

    pub fn upsert_message(&self, id: &str, message: Message) {
        let mut messages = self.messages.lock().unwrap();
        let list = messages.entry(id.to_string()).or_default();
        if let Some(existing) = list
            .iter_mut()
            .find(|existing| existing.info.id == message.info.id)
        {
            *existing = message;
        } else {
            list.push(message);
        }
    }

    pub fn messages(&self, id: &str) -> Vec<Message> {
        self.messages
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn try_start_turn(&self, id: &str, loop_: Arc<AgentLoop>) -> bool {
        let mut loops = self.loops.lock().unwrap();
        if loops.contains_key(id) {
            return false;
        }
        loops.insert(id.to_string(), loop_);
        true
    }

    pub fn finish_turn(&self, id: &str) {
        self.loops.lock().unwrap().remove(id);
    }

    pub fn cancel_turn(&self, id: &str) {
        if let Some(loop_) = self.loops.lock().unwrap().get(id) {
            loop_.cancel();
        }
        self.approvals.clear_session(id);
    }

    pub fn statuses(&self) -> Value {
        let loops = self.loops.lock().unwrap();
        Value::Object(
            loops
                .keys()
                .map(|id| (id.clone(), serde_json::json!({"type":"busy"})))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ProviderConfig {
        ProviderConfig {
            default_provider: "mock".into(),
            mock_mode: "text".into(),
            openai_api_key: None,
            openai_base_url: "https://example.invalid/v1".into(),
            openai_model: "gpt-test".into(),
            anthropic_api_key: None,
            anthropic_base_url: "https://example.invalid".into(),
            anthropic_model: "claude-test".into(),
        }
    }

    #[test]
    fn catalog_only_connects_configured_network_providers() {
        let mut config = config();
        config.openai_api_key = Some("secret".into());
        let catalog = config.catalog();
        assert_eq!(catalog.all.len(), 4);
        assert_eq!(catalog.connected, ["mock", "echo", "openai"]);
        assert!(catalog.all[3].models.contains_key("gpt-test"));
    }

    #[test]
    fn resolve_rejects_unconfigured_and_unknown_providers() {
        let config = config();
        let openai = ModelRef {
            provider_id: "openai".into(),
            model_id: "gpt-test".into(),
        };
        assert!(config
            .resolve(Some(&openai), "text")
            .err()
            .unwrap()
            .contains("not configured"));

        let unknown = ModelRef {
            provider_id: "unknown".into(),
            model_id: "unknown-1".into(),
        };
        assert!(config
            .resolve(Some(&unknown), "text")
            .err()
            .unwrap()
            .contains("unsupported provider"));

        let wrong_mock_model = ModelRef {
            provider_id: "mock".into(),
            model_id: "other".into(),
        };
        assert!(config
            .resolve(Some(&wrong_mock_model), "text")
            .err()
            .unwrap()
            .contains("unsupported model"));
    }

    #[test]
    fn resolve_uses_requested_openai_model() {
        let mut config = config();
        config.openai_api_key = Some("secret".into());
        let requested = ModelRef {
            provider_id: "openai".into(),
            model_id: "gpt-test".into(),
        };
        let resolved = config.resolve(Some(&requested), "text").unwrap();
        assert_eq!(resolved.provider_id, "openai");
        assert_eq!(resolved.model_id, "gpt-test");
        assert_eq!(resolved.provider.default_model(), "gpt-test");
    }
}
