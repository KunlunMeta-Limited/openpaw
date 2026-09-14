use crate::protocol::{PermissionDecision, PermissionReply, Session};
use crate::{events, runtime::RuntimeState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use openpaw_core::Session as EngineSession;
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::MockProvider;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;
#[derive(serde::Deserialize)]
pub struct CreateSession {
    pub title: Option<String>,
    pub directory: Option<String>,
}
#[derive(serde::Deserialize)]
pub struct Prompt {
    pub parts: Vec<PromptPart>,
}
#[derive(serde::Deserialize)]
pub struct PromptPart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
}
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"healthy":true}))
}
pub async fn list(State(state): State<RuntimeState>) -> Json<Vec<Session>> {
    Json(state.sessions.lock().unwrap().clone())
}
pub async fn create(
    State(state): State<RuntimeState>,
    Json(body): Json<CreateSession>,
) -> Json<Session> {
    let id = format!("ses_{}", Uuid::new_v4().simple());
    let item = RuntimeState::new_session(
        id,
        body.directory.unwrap_or_else(|| ".".into()),
        body.title.unwrap_or_else(|| "New session".into()),
    );
    state.insert_session(item.clone());
    Json(item)
}
pub async fn get_session(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
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
pub async fn messages(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
) -> impl IntoResponse {
    if state.session_exists(&id) {
        Json(state.history(&id)).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
pub async fn prompt(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
    Json(body): Json<Prompt>,
) -> impl IntoResponse {
    if !state.session_exists(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let text: String = body
        .parts
        .into_iter()
        .filter(|p| p.kind == "text")
        .filter_map(|p| p.text)
        .collect();
    state.touch_title(&id, &text);
    if !text.is_empty() {
        state.append_history(&id, serde_json::json!({"role":"user","parts":[{"type":"text","text":text}]}));
    }
    let Some(tx) = state.event_sender(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let loop_ = Arc::new(
        AgentLoop::new(Arc::new(MockProvider::stream_text(
            if text.is_empty() { "ok".into() } else { text },
            4,
        )))
        .with_approval_manager(state.approvals.clone()),
    );
    state
        .loops
        .lock()
        .unwrap()
        .insert(id.clone(), loop_.clone());
    let session_id = id.clone();
    tokio::spawn(async move {
        let mut session = EngineSession::new("mock-1", "mock");
        let _ = tx.send(serde_json::json!({"type":"session.status","properties":{"sessionID":session_id,"status":{"type":"busy"}}}).to_string());
        let tx_events = tx.clone();
        let history_state = state.clone();
        let history_id = session_id.clone();
        let mut sink = move |event: AgentEvent| {
            if let Some(value) = events::to_wire(event) {
                history_state.append_history(&history_id, value.clone());
                let _ = tx_events.send(value.to_string());
            }
        };
        let _ = loop_.run_turn(&mut session, None, &[], &mut sink).await;
        let _ = tx.send(serde_json::json!({"type":"session.status","properties":{"sessionID":session_id,"status":{"type":"idle"}}}).to_string());
    });
    StatusCode::NO_CONTENT.into_response()
}
pub async fn abort(Path(id): Path<String>, State(state): State<RuntimeState>) -> impl IntoResponse {
    if !state.session_exists(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(loop_) = state.loops.lock().unwrap().get(&id) {
        loop_.cancel();
    }
    state.approvals.clear();
    Json(true).into_response()
}
pub async fn status() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

pub async fn todo(Path(id): Path<String>, State(state): State<RuntimeState>) -> impl IntoResponse {
    if state.session_exists(&id) { Json(Vec::<serde_json::Value>::new()).into_response() } else { StatusCode::NOT_FOUND.into_response() }
}

pub async fn providers() -> Json<Vec<crate::protocol::Provider>> {
    Json(vec![crate::protocol::Provider { id: "mock".into(), name: "Mock provider".into() }])
}
pub async fn permissions(State(state): State<RuntimeState>) -> Json<Vec<serde_json::Value>> {
    Json(state.approvals.list())
}
pub async fn permission_reply(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
    Json(body): Json<PermissionReply>,
) -> impl IntoResponse {
    let decision = match body.decision {
        PermissionDecision::Once => "once",
        PermissionDecision::Always => "always",
        PermissionDecision::Reject => "reject",
    };
    if state.approvals.reply(&id, decision) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
pub async fn events(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
) -> impl IntoResponse {
    let Some(tx) = state.event_sender(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rx = tx.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx: broadcast::Receiver<String>| async move {
        match rx.recv().await {
            Ok(value) => Some((
                Ok::<_, std::convert::Infallible>(SseEvent::default().data(value)),
                rx,
            )),
            Err(_) => None,
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
