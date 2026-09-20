use crate::protocol::{
    CreateSession, PermissionDecision, PermissionReply, Prompt, ProviderList, Session, SessionQuery,
};
use crate::{approval::ManualApprovalGuardrail, events, runtime::RuntimeState};
use axum::{
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
        HeaderMap, StatusCode,
    },
    middleware::Next,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use base64::Engine;
use openpaw_core::{AutoApproveManager, Message as EngineMessage, Role, SessionEvent};
use openpaw_engine::{AgentEvent, AgentLoop};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Basic Authentication credentials resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicAuth {
    username: String,
    password: String,
}

impl BasicAuth {
    /// Credentials pair from `OPENCODE_SERVER_USERNAME` /
    /// `OPENCODE_SERVER_PASSWORD`.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }

    /// `true` only when the `Authorization` header carries a Basic scheme and
    /// exactly these credentials.
    pub fn accepts(&self, header: Option<&str>) -> bool {
        let Some(header) = header else {
            return false;
        };
        let mut parts = header.splitn(2, ' ');
        let scheme = parts.next().unwrap_or_default();
        let encoded = parts.next().unwrap_or_default().trim();
        if !scheme.eq_ignore_ascii_case("basic") || encoded.is_empty() {
            return false;
        }
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return false;
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return false;
        };
        match decoded.split_once(':') {
            Some((username, password)) => username == self.username && password == self.password,
            None => false,
        }
    }
}

/// Basic Authentication for every route, applied only when both credentials
/// are configured (§3.2). Missing or wrong credentials answer 401.
pub async fn require_auth(
    State(auth): State<Option<BasicAuth>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(auth) = auth else {
        return next.run(request).await;
    };
    let header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if auth.accepts(header) {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, "Basic realm=\"openpaw-engine-host\"")],
        Json(serde_json::json!({"error":"unauthorized"})),
    )
        .into_response()
}

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"healthy":true}))
}

/// `GET /config` and `GET /mcp`: capabilities outside this subset degrade to
/// an empty object instead of 404 (§3.2).
pub async fn empty_object() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

/// `GET /command` and `GET /project`: unimplemented listings degrade to an
/// empty array (§3.2).
pub async fn empty_array() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

/// `POST /session/:id/{command,shell,fork,revert,summarize}`: accepted and
/// ignored while the capability is unimplemented (§3.2).
pub async fn degraded() -> StatusCode {
    StatusCode::NO_CONTENT
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
    // §3.2 workspace priority: `x-opencode-directory`, the `directory` query
    // parameter, then the CLI default workspace.
    let directory = header_directory(&headers)
        .or(query.directory)
        .unwrap_or_else(|| state.default_workspace());
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
    body: Bytes,
) -> impl IntoResponse {
    let Some(engine_session) = state.engine_session(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(wire_session) = state.session(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Parsed by hand so every malformed body answers 400 (§3.2) instead of
    // axum's 422 for shape errors.
    let body: Prompt = match parse_prompt_body(&body) {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
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
        for event in events::turn_start_events(&session_id) {
            state.emit(&session_id, event);
        }

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
        for event in events::turn_finish_events(&session_id) {
            state.emit(&session_id, event);
        }
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
    body: Bytes,
) -> impl IntoResponse {
    let Ok(body) = serde_json::from_slice::<PermissionReply>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid permission reply JSON"})),
        )
            .into_response();
    };
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

/// SSE for the three agreed paths only (§3.2): `/event`, `/global/event` and
/// `/api/event`. Every other GET answers JSON, never an event stream.
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

fn parse_prompt_body(body: &[u8]) -> Result<Prompt, String> {
    serde_json::from_slice(body).map_err(|error| format!("invalid prompt JSON: {error}"))
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

    /// `Authorization: Basic <base64(user:pass)>` for one credential pair.
    fn credentials(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    #[test]
    fn decodes_directory_header() {
        assert_eq!(percent_decode("%2Fopt%2Fopenwork"), "/opt/openwork");
        assert_eq!(percent_decode("C%3A%5Cwork"), r"C:\work");
    }

    #[test]
    fn basic_auth_requires_the_configured_pair() {
        let auth = BasicAuth::new("dev", "dev");
        assert!(auth.accepts(Some(&credentials("dev", "dev"))));
        // The scheme is case-insensitive, the credentials are not.
        assert!(auth.accepts(Some(&credentials("dev", "dev").replace("Basic", "basic"))));
        assert!(!auth.accepts(None));
        assert!(!auth.accepts(Some("")));
        assert!(!auth.accepts(Some("Bearer ZGV2OmRldg==")));
        assert!(!auth.accepts(Some("Basic")));
        assert!(!auth.accepts(Some("Basic ")));
        assert!(!auth.accepts(Some(&credentials("dev", "wrong"))));
        assert!(!auth.accepts(Some(&credentials("other", "dev"))));
        assert!(!auth.accepts(Some("Basic !!!not-base64!!!")));
        // A payload without the `user:password` separator never matches.
        let no_separator = base64::engine::general_purpose::STANDARD.encode("devdev");
        assert!(!auth.accepts(Some(&format!("Basic {no_separator}"))));
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

/// §3.2 HTTP contract: authentication, the explicitly degraded routes, the
/// fixed error statuses and the workspace priority.
///
/// The tests drive the assembled `Router` through
/// `tower::ServiceExt::oneshot`, so they exercise the real route table, the
/// real middleware and the real handlers without binding a socket.
#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::{router, HostConfig, RuntimeState};
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use base64::Engine;
    use std::path::PathBuf;
    use tower::ServiceExt;

    /// Credentials every authenticated test uses.
    const USER: &str = "dev";
    const PASSWORD: &str = "dev";

    /// A writable directory for the transcript root and the default
    /// workspace.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("openpaw-engine-host-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    /// Host configuration with a real (existing) default workspace.
    fn config(tag: &str, auth: Option<(&str, &str)>) -> HostConfig {
        HostConfig {
            hostname: "127.0.0.1".into(),
            port: 0,
            provider: "mock".into(),
            home: scratch(&format!("{tag}-home")),
            workspace: scratch(&format!("{tag}-workspace")),
            username: auth.map(|(user, _)| user.to_string()),
            password: auth.map(|(_, password)| password.to_string()),
        }
    }

    fn app(tag: &str, auth: Option<(&str, &str)>) -> Router {
        let config = config(tag, auth);
        let auth = config.authentication().expect("credentials pair");
        router(RuntimeState::new(&config), auth)
    }

    /// `Authorization: Basic <base64(user:pass)>`.
    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
        let response = app.clone().oneshot(request).await.expect("router responds");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("response body");
        (status, body.to_vec())
    }

    async fn send_json(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let (status, body) = send(app, request).await;
        let value = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
        };
        (status, value)
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .expect("request")
    }

    fn post(path: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    async fn create_session(app: &Router) -> String {
        let (status, session) = send_json(app, post("/session", "{}")).await;
        assert_eq!(status, StatusCode::OK, "create session: {session}");
        session["id"].as_str().expect("session id").to_string()
    }

    #[tokio::test]
    async fn authentication_is_off_without_credentials() {
        let app = app("no-auth", None);
        let (status, body) = send_json(&app, get("/global/health")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["healthy"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn basic_authentication_gates_every_route() {
        let app = app("auth", Some((USER, PASSWORD)));

        // Missing credentials answer 401 on a required route and on a
        // degraded route alike.
        for request in [
            get("/global/health"),
            get("/config"),
            post("/session", "{}"),
        ] {
            let (status, _) = send(&app, request).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/global/health")
                    .header("authorization", basic(USER, "wrong"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("401 response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized
                .headers()
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok()),
            Some("Basic realm=\"openpaw-engine-host\"")
        );

        let authorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/global/health")
                    .header("authorization", basic(USER, PASSWORD))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("200 response");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn required_routes_answer_the_documented_shapes() {
        let app = app("required", None);

        let (status, body) = send_json(&app, get("/global/health")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"healthy": true}));

        let id = create_session(&app).await;

        let (status, sessions) = send_json(&app, get("/session")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sessions.as_array().map(Vec::len), Some(1));

        let (status, session) = send_json(&app, get(&format!("/session/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["id"], serde_json::json!(id));
        assert_eq!(session["slug"], serde_json::json!(id));

        let (status, messages) = send_json(&app, get(&format!("/session/{id}/message"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(messages, serde_json::json!([]));

        let (status, todos) = send_json(&app, get(&format!("/session/{id}/todo"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(todos, serde_json::json!([]));

        let (status, statuses) = send_json(&app, get("/session/status")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(statuses.as_object().is_some());

        let (status, providers) = send_json(&app, get("/provider")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(providers["all"].as_array().map(Vec::len), Some(4));
        let ids: Vec<&str> = providers["all"]
            .as_array()
            .expect("provider list")
            .iter()
            .filter_map(|provider| provider["id"].as_str())
            .collect();
        assert_eq!(ids, ["mock", "echo", "anthropic", "openai"]);

        let (status, permissions) = send_json(&app, get("/permission")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(permissions, serde_json::json!([]));

        // prompt_async admits the turn and answers 204 with an empty body;
        // abort answers `true`.
        let (status, body) = send(
            &app,
            post(
                &format!("/session/{id}/prompt_async"),
                r#"{"parts":[{"type":"text","text":"hello"}]}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());

        let (status, aborted) = send_json(&app, post(&format!("/session/{id}/abort"), "")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(aborted, serde_json::json!(true));
    }

    #[tokio::test]
    async fn degraded_routes_answer_instead_of_404() {
        let app = app("degraded", None);
        let id = create_session(&app).await;

        for path in ["/config", "/mcp"] {
            let (status, body) = send_json(&app, get(path)).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, serde_json::json!({}), "{path}");
        }
        for path in ["/command", "/project"] {
            let (status, body) = send_json(&app, get(path)).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, serde_json::json!([]), "{path}");
        }
        for action in ["command", "shell", "fork", "revert", "summarize"] {
            let path = format!("/session/{id}/{action}");
            let (status, body) = send(&app, post(&path, "")).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{path}");
            assert!(body.is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn v2_paths_stay_404() {
        let app = app("v2", None);
        for path in ["/api/health", "/api/session", "/v2/health"] {
            let (status, _) = send(&app, get(path)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn fixed_error_statuses() {
        let app = app("errors", None);

        // Malformed create body -> 400.
        let (status, _) = send(&app, post("/session", "not-json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Unknown session -> 404 on the session-scoped routes.
        for path in [
            "/session/missing",
            "/session/missing/message",
            "/session/missing/todo",
        ] {
            let (status, _) = send(&app, get(path)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        let (status, _) = send(
            &app,
            post("/session/missing/prompt_async", r#"{"parts":[]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(&app, post("/session/missing/abort", "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Unknown permission -> 404; malformed reply body -> 400.
        let (status, _) = send(
            &app,
            post("/permission/perm_missing/reply", r#"{"reply":"once"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(&app, post("/permission/perm_missing/reply", "not-json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Unsupported provider on an existing session -> 400, and every
        // malformed prompt body is a 400 rather than axum's 422.
        let id = create_session(&app).await;
        let (status, _) = send(
            &app,
            post(
                &format!("/session/{id}/prompt_async"),
                r#"{"model":{"providerID":"hologram","modelID":"h1"},"parts":[]}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        for body in ["not-json", "{}"] {
            let (status, _) = send(&app, post(&format!("/session/{id}/prompt_async"), body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
    }

    #[tokio::test]
    async fn workspace_priority_is_header_then_query_then_default() {
        let config = config("workspace", None);
        let default_workspace = config.workspace.to_string_lossy().into_owned();
        let app = router(RuntimeState::new(&config), None);

        let with_header = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session")
                    .header("content-type", "application/json")
                    .header("x-opencode-directory", "%2Fopt%2Fopenwork")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("header create");
        let (status, body) = (
            with_header.status(),
            axum::body::to_bytes(with_header.into_body(), 1 << 20)
                .await
                .expect("body"),
        );
        assert_eq!(status, StatusCode::OK);
        let session: serde_json::Value = serde_json::from_slice(&body).expect("session json");
        assert_eq!(session["directory"], serde_json::json!("/opt/openwork"));

        let (_, via_query) = send_json(&app, post("/session?directory=%2Fopt%2Fquery", "{}")).await;
        assert_eq!(via_query["directory"], serde_json::json!("/opt/query"));

        let (_, via_body) =
            send_json(&app, post("/session", r#"{"directory":"/from-body"}"#)).await;
        assert_eq!(
            via_body["directory"],
            serde_json::json!(default_workspace),
            "§3.2 has no body.directory tier, so the CLI default workspace wins"
        );

        let header_beats_query = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session?directory=%2Fopt%2Fquery")
                    .header("content-type", "application/json")
                    .header("x-opencode-directory", "%2Fopt%2Fheader")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("priority create");
        let body = axum::body::to_bytes(header_beats_query.into_body(), 1 << 20)
            .await
            .expect("body");
        let session: serde_json::Value = serde_json::from_slice(&body).expect("session json");
        assert_eq!(session["directory"], serde_json::json!("/opt/header"));
    }

    #[tokio::test]
    async fn only_the_agreed_paths_serve_sse() {
        let app = app("sse", None);

        for path in ["/event", "/global/event", "/api/event"] {
            let response = app.clone().oneshot(get(path)).await.expect("sse response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(
                content_type.starts_with("text/event-stream"),
                "{path} content-type: {content_type}"
            );
        }

        // §3.2 lists exactly three SSE paths: the retired per-session stream
        // must stay unrouted and every other GET (data routes included) must
        // answer JSON.
        let id = create_session(&app).await;
        let retired = app
            .clone()
            .oneshot(get(&format!("/session/{id}/event")))
            .await
            .expect("retired route");
        assert_eq!(
            retired.status(),
            StatusCode::NOT_FOUND,
            "/session/:id/event must not serve SSE any more"
        );

        for path in [
            "/global/health".to_string(),
            format!("/session/{id}/message"),
        ] {
            let response = app
                .clone()
                .oneshot(get(&path))
                .await
                .expect("json response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(
                content_type.starts_with("application/json"),
                "{path} content-type: {content_type}"
            );
        }
    }
}
