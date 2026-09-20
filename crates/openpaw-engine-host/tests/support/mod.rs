#![cfg(feature = "engine-host")]

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use tower::ServiceExt;

pub async fn request(
    app: &Router,
    method: &str,
    uri: impl Into<String>,
    body: impl Into<Body>,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri.into());
    if method.eq_ignore_ascii_case("POST") {
        request = request.header("content-type", "application/json");
    }
    app.clone()
        .oneshot(request.body(body.into()).expect("request"))
        .await
        .expect("response")
}

#[allow(dead_code)]
pub async fn request_with_headers(
    app: &Router,
    method: &str,
    uri: impl Into<String>,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri.into());
    if method.eq_ignore_ascii_case("POST") {
        request = request.header("content-type", "application/json");
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(body.into()).expect("request"))
        .await
        .expect("response")
}

pub async fn response_bytes(response: Response) -> axum::body::Bytes {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body")
}

pub async fn response_json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&response_bytes(response).await).expect("response json")
}

pub async fn get_json(app: &Router, uri: impl AsRef<str>) -> serde_json::Value {
    let uri = uri.as_ref().to_string();
    let response = request(app, "GET", uri.clone(), Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
    )
    .expect("json")
}

pub async fn create_session(app: &Router) -> String {
    create_session_with(app, serde_json::json!({})).await
}

pub async fn create_session_with(app: &Router, config: serde_json::Value) -> String {
    let response = request(app, "POST", "/session", config.to_string()).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await["id"]
        .as_str()
        .expect("session id")
        .to_string()
}

#[allow(dead_code)]
pub async fn create_session_with_provider(app: &Router, provider: &str) -> String {
    create_session_with(app, serde_json::json!({"provider": provider})).await
}

#[allow(dead_code)]
pub async fn abort(app: &Router, id: &str) -> Response {
    request(app, "POST", format!("/session/{id}/abort"), Body::empty()).await
}

#[allow(dead_code)]
pub async fn prompt(app: &Router, id: &str, text: &str) -> Response {
    request(
        app,
        "POST",
        format!("/session/{id}/prompt_async"),
        serde_json::json!({"parts": [{"type": "text", "text": text}]}).to_string(),
    )
    .await
}

pub async fn prompt_with_request_id(
    app: &Router,
    id: &str,
    request_id: &str,
    text: &str,
) -> Response {
    request(
        app,
        "POST",
        format!("/session/{id}/prompt_async"),
        serde_json::json!({"requestID": request_id, "text": text}).to_string(),
    )
    .await
}

#[allow(dead_code)]
pub async fn wait_for_terminal(app: &Router, id: &str) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let turns = get_json(app, format!("/session/{id}/turns")).await;
            if turns
                .as_array()
                .and_then(|items| items.last())
                .and_then(|turn| turn["state"].as_str())
                .is_some_and(|state| matches!(state, "completed" | "failed" | "cancelled"))
            {
                return turns;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("turn terminal timeout")
}
