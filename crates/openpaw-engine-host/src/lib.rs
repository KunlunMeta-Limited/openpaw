//! Minimal OpenCode-compatible engine host, gated behind `engine-host`.

#[cfg(feature = "engine-host")]
pub mod protocol;

#[cfg(feature = "engine-host")]
mod enabled {
    use axum::{
        extract::Path,
        http::StatusCode,
        response::{sse::{Event as SseEvent, KeepAlive, Sse}, IntoResponse},
        routing::{get, post},
        Json, Router,
    };
    use chrono::Utc;
    use serde::{Deserialize, Serialize};
    use std::{
        collections::HashMap,
        net::SocketAddr,
        sync::{Arc, Mutex},
    };
    use openpaw_core::Session as EngineSession;
    use openpaw_engine::{AgentEvent, AgentLoop};
    use openpaw_llm::MockProvider;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct State {
        sessions: Arc<Mutex<Vec<Session>>>,
        events: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
        loops: Arc<Mutex<HashMap<String, Arc<AgentLoop>>>>,
    }

    #[derive(Clone, Serialize, Deserialize)]
    pub struct Session {
        pub id: String,
        pub title: String,
        pub slug: String,
        pub directory: String,
        pub time: Time,
    }
    #[derive(Clone, Serialize, Deserialize)]
    pub struct Time {
        pub created: i64,
        pub updated: i64,
    }
    #[derive(Deserialize)]
    struct CreateSession {
        title: Option<String>,
        directory: Option<String>,
    }
    #[derive(Deserialize)]
    struct Prompt {
        parts: Vec<PromptPart>,
    }
    #[derive(Deserialize)]
    struct PromptPart {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }
    #[derive(Serialize)]
    struct Health {
        healthy: bool,
    }

    fn now() -> i64 {
        Utc::now().timestamp_millis()
    }
    fn session(id: &str, directory: String, title: String) -> Session {
        let time = now();
        Session {
            id: id.into(),
            title,
            slug: id.into(),
            directory,
            time: Time {
                created: time,
                updated: time,
            },
        }
    }
    async fn health() -> Json<Health> {
        Json(Health { healthy: true })
    }
    async fn list(state: axum::extract::State<State>) -> Json<Vec<Session>> {
        Json(state.sessions.lock().unwrap().clone())
    }
    async fn create(
        axum::extract::State(state): axum::extract::State<State>,
        Json(body): Json<CreateSession>,
    ) -> Json<Session> {
        let id = format!("ses_{}", Uuid::new_v4().simple());
        let item = session(
            &id,
            body.directory.unwrap_or_else(|| ".".into()),
            body.title.unwrap_or_else(|| "New session".into()),
        );
        state.sessions.lock().unwrap().push(item.clone());
        let (tx, _) = broadcast::channel(128);
        state.events.lock().unwrap().insert(id.clone(), tx);
        Json(item)
    }
    async fn get_session(
        Path(id): Path<String>,
        axum::extract::State(state): axum::extract::State<State>,
    ) -> impl IntoResponse {
        state
            .sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .map(Json)
            .map(IntoResponse::into_response)
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
    }
    async fn messages(
        Path(id): Path<String>,
        axum::extract::State(state): axum::extract::State<State>,
    ) -> impl IntoResponse {
        if state.sessions.lock().unwrap().iter().any(|s| s.id == id) {
            Json(Vec::<serde_json::Value>::new()).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }
    async fn prompt(
        Path(id): Path<String>,
        axum::extract::State(state): axum::extract::State<State>,
        Json(body): Json<Prompt>,
    ) -> impl IntoResponse {
        if let Some(s) = state
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            let text: String = body
                .parts
                .into_iter()
                .filter(|p| p.kind == "text")
                .filter_map(|p| p.text)
                .collect();
            if !text.is_empty() && s.title == "New session" {
                s.title = text.chars().take(80).collect();
                s.time.updated = now();
            }
            let tx = state.events.lock().unwrap().get(&id).cloned();
            let loop_ = Arc::new(AgentLoop::new(Arc::new(MockProvider::stream_text(
                if text.is_empty() { "ok".to_string() } else { text.clone() },
                4,
            ))));
            state.loops.lock().unwrap().insert(id.clone(), loop_.clone());
            if let Some(tx) = tx {
                let session_id = id.clone();
                tokio::spawn(async move {
                    let mut session = EngineSession::new("mock-1", "mock");
                    let _ = tx.send(serde_json::json!({"type":"session.status","properties":{"sessionID":session_id,"status":{"type":"busy"}}}).to_string());
                    let tx_events = tx.clone();
                    let mut sink = move |event: AgentEvent| {
                        let (kind, props) = match event {
                            AgentEvent::MessageStart => ("message.updated", serde_json::json!({"role":"assistant"})),
                            AgentEvent::TextDelta { text } => ("message.part.delta", serde_json::json!({"field":"text","delta":text})),
                            AgentEvent::ThinkingDelta { text } => ("message.part.delta", serde_json::json!({"field":"reasoning","delta":text})),
                            AgentEvent::ToolUseStart { tool_use_id, name } => ("message.part.updated", serde_json::json!({"callID":tool_use_id,"tool":name,"status":"running"})),
                            AgentEvent::ToolResult { tool_use_id, content, is_error, .. } => ("message.part.updated", serde_json::json!({"callID":tool_use_id,"output":content,"status":if is_error {"error"} else {"completed"}})),
                            AgentEvent::ClassifierStatus { .. } => ("permission.asked", serde_json::json!({})),
                            AgentEvent::TurnUsage { .. } => ("turn.usage", serde_json::json!({})),
                            AgentEvent::CompactionPerformed { .. } => return,
                        };
                        let _ = tx_events.send(serde_json::json!({"type":kind,"properties":props}).to_string());
                    };
                    let _ = loop_.run_turn(&mut session, None, &[], &mut sink).await;
                    let _ = tx.send(serde_json::json!({"type":"session.status","properties":{"sessionID":session_id,"status":{"type":"idle"}}}).to_string());
                });
            }
            StatusCode::NO_CONTENT.into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }
    async fn abort(
        Path(id): Path<String>,
        axum::extract::State(state): axum::extract::State<State>,
    ) -> impl IntoResponse {
        if state.sessions.lock().unwrap().iter().any(|s| s.id == id) {
            if let Some(loop_) = state.loops.lock().unwrap().get(&id) { loop_.cancel(); }
            Json(true).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }
    async fn status() -> Json<serde_json::Value> {
        Json(serde_json::json!({}))
    }

    async fn events(
        Path(id): Path<String>,
        axum::extract::State(state): axum::extract::State<State>,
    ) -> impl IntoResponse {
        let Some(tx) = state.events.lock().unwrap().get(&id).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let rx = tx.subscribe();
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            match rx.recv().await {
                Ok(value) => Some((Ok::<_, std::convert::Infallible>(SseEvent::default().data(value)), rx)),
                Err(_) => None,
            }
        });
        Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
    }

    pub async fn run(hostname: &str, port: u16) -> anyhow::Result<()> {
        let state = State::default();
        let app = Router::new()
            .route("/global/health", get(health))
            .route("/session", get(list).post(create))
            .route("/session/status", get(status))
            .route("/session/{id}", get(get_session))
            .route("/session/{id}/message", get(messages))
            .route("/session/{id}/prompt_async", post(prompt))
            .route("/session/{id}/abort", post(abort))
            .route("/session/{id}/event", get(events))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind((hostname, port)).await?;
        let addr: SocketAddr = listener.local_addr()?;
        println!("openpaw server listening on http://{addr}");
        axum::serve(listener, app).await?;
        Ok(())
    }
}

#[cfg(feature = "engine-host")]
pub use enabled::run;
