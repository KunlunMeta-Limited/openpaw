use crate::protocol::{
    CreateSession, PermissionDecision, PermissionReply, Prompt, ProviderList, Session, SessionQuery,
};
use crate::{approval::ManualApprovalGuardrail, events, runtime::RuntimeState};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use openpaw_core::{AutoApproveManager, Message as EngineMessage, Role, SessionEvent};
use openpaw_engine::{AgentEvent, AgentLoop};
use std::sync::Arc;
use tokio::sync::broadcast;

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"healthy":true}))
}

pub async fn list(State(state): State<RuntimeState>) -> Json<Vec<Session>> {
    Json(state.list_sessions())
}

pub async fn create(
    State(state): State<RuntimeState>,
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body = match parse_create_body(&body) {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };
    let directory = header_directory(&headers)
        .or(query.directory)
        .or(body.directory)
        .unwrap_or_else(|| ".".into());
    let session = match state.create_session(
        directory,
        body.title.unwrap_or_else(|| "New session".into()),
    ) {
        Ok(session) => session,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };
    state.emit(
        &session.id,
        events::session_event("session.created", &session),
    );
    Json(session).into_response()
}

pub async fn get_session(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
) -> impl IntoResponse {
    state
        .session(&id)
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub async fn messages(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
) -> impl IntoResponse {
    if state.session_exists(&id) {
        Json(state.messages(&id)).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn prompt(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
    Json(body): Json<Prompt>,
) -> impl IntoResponse {
    let Some(engine_session) = state.engine_session(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(wire_session) = state.session(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let text: String = body
        .parts
        .into_iter()
        .filter(|part| part.kind == "text")
        .filter_map(|part| part.text)
        .collect();
    let resolved = match state.resolve_provider(body.model.as_ref(), &text) {
        Ok(resolved) => resolved,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };

    let tool_host = openpaw_tools::arc_registry(wire_session.directory);
    let tool_specs = match tool_host.list_tools().await {
        Ok(specs) => specs,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":format!("cannot list OpenPaw tools: {error}")})),
            )
                .into_response();
        }
    };
    let mut agent = AgentLoop::new(resolved.provider)
        .with_model(resolved.model_id.clone())
        .with_tool_host(tool_host);
    if state.manual_permissions() {
        agent = agent
            .with_guardrail(Arc::new(ManualApprovalGuardrail))
            .with_approval_manager(state.approvals.scoped(id.clone()));
    } else {
        agent = agent.with_approval_manager(Arc::new(AutoApproveManager));
    }
    let loop_ = Arc::new(agent);
    if !state.try_start_turn(&id, loop_.clone()) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"session already has an active turn"})),
        )
            .into_response();
    }

    if let Some(session) = state.touch_title(&id, &text) {
        state.emit(&id, events::session_event("session.updated", &session));
    }
    let user_message = events::user_message(&id, body.message_id, text.clone());
    state.upsert_message(&id, user_message.clone());
    for event in events::message_events(&user_message) {
        state.emit(&id, event);
    }

    let session_id = id;
    let provider_id = resolved.provider_id;
    let model_id = resolved.model_id;
    tokio::spawn(async move {
        state.emit(&session_id, events::session_status(&session_id, "busy"));

        let mut mapper = events::TurnEventMapper::new(&session_id);
        let run_result = {
            let mut session = engine_session.lock().await;
            session.meta.provider = provider_id;
            session.meta.model = model_id;
            let engine_user = EngineMessage::user_text(text).with_uuid(user_message.info.id);
            session.messages.push(engine_user.clone());
            if let Err(error) =
                state.persist_session_event(&session_id, &SessionEvent::Message(engine_user))
            {
                emit_persistence_error(&state, &session_id, error);
            }
            let produced_from = session.messages.len();

            let result = {
                let mut sink = |event: AgentEvent| {
                    apply_wire_update(&state, &session_id, mapper.apply(event));
                };
                loop_
                    .run_turn(&mut session, None, &tool_specs, &mut sink)
                    .await
            };
            for message in &session.messages[produced_from..] {
                if message.role == Role::Assistant {
                    if let Err(error) = state
                        .persist_session_event(&session_id, &SessionEvent::Message(message.clone()))
                    {
                        emit_persistence_error(&state, &session_id, error);
                    }
                }
            }
            apply_wire_update(&state, &session_id, mapper.finish());
            result
        };

        if let Err(error) = run_result {
            state.emit(
                &session_id,
                serde_json::json!({
                    "type":"session.error",
                    "properties":{"sessionID":session_id,"error":{"name":"EngineError","data":{"message":error.to_string()}}}
                }),
            );
        }
        state.finish_turn(&session_id);
        state.emit(&session_id, events::session_status(&session_id, "idle"));
        state.emit(&session_id, events::session_idle(&session_id));
    });

    StatusCode::NO_CONTENT.into_response()
}

fn emit_persistence_error(state: &RuntimeState, session_id: &str, error: String) {
    state.emit(
        session_id,
        serde_json::json!({
            "type":"session.error",
            "properties":{"sessionID":session_id,"error":{"name":"PersistenceError","data":{"message":error}}}
        }),
    );
}

fn apply_wire_update(state: &RuntimeState, session_id: &str, update: events::WireUpdate) {
    for message in update.messages {
        state.upsert_message(session_id, message);
    }
    for event in update.events {
        state.emit(session_id, event);
    }
}

pub async fn abort(Path(id): Path<String>, State(state): State<RuntimeState>) -> impl IntoResponse {
    if !state.session_exists(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    state.cancel_turn(&id);
    Json(true).into_response()
}

pub async fn status(State(state): State<RuntimeState>) -> Json<serde_json::Value> {
    Json(state.statuses())
}

pub async fn todo(Path(id): Path<String>, State(state): State<RuntimeState>) -> impl IntoResponse {
    if state.session_exists(&id) {
        Json(Vec::<serde_json::Value>::new()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub async fn providers(State(state): State<RuntimeState>) -> Json<ProviderList> {
    Json(state.provider_list())
}

pub async fn permissions(State(state): State<RuntimeState>) -> Json<Vec<serde_json::Value>> {
    Json(state.approvals.list())
}

pub async fn permission_reply(
    Path(id): Path<String>,
    State(state): State<RuntimeState>,
    Json(body): Json<PermissionReply>,
) -> impl IntoResponse {
    let decision = match body.reply {
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
    sse(tx.subscribe()).into_response()
}

pub async fn global_events(State(state): State<RuntimeState>) -> impl IntoResponse {
    sse(state.subscribe_global())
}

fn sse(
    rx: broadcast::Receiver<String>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(value) => {
                    return Some((
                        Ok::<_, std::convert::Infallible>(SseEvent::default().data(value)),
                        rx,
                    ));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn header_directory(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-opencode-directory")?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(percent_decode(value))
}

fn parse_create_body(body: &[u8]) -> Result<CreateSession, String> {
    if body.is_empty() {
        return Ok(CreateSession::default());
    }
    serde_json::from_slice(body).map_err(|error| format!("invalid create session JSON: {error}"))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                output.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_directory_header() {
        assert_eq!(percent_decode("%2Fopt%2Fopenwork"), "/opt/openwork");
        assert_eq!(percent_decode("C%3A%5Cwork"), r"C:\work");
    }

    #[test]
    fn empty_create_body_is_valid_regardless_of_content_type() {
        assert_eq!(parse_create_body(&[]).unwrap(), CreateSession::default());
        assert_eq!(
            parse_create_body(br#"{"title":"named"}"#)
                .unwrap()
                .title
                .as_deref(),
            Some("named")
        );
        assert!(parse_create_body(b"not-json").is_err());
    }
}
