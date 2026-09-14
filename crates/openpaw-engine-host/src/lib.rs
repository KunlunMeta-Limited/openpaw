//! OpenCode-compatible Engine Host, gated behind the `engine-host` feature.
#[cfg(feature = "engine-host")]
mod approval;
#[cfg(feature = "engine-host")]
mod events;
#[cfg(feature = "engine-host")]
mod http;
#[cfg(feature = "engine-host")]
pub mod protocol;
#[cfg(feature = "engine-host")]
mod runtime;

#[cfg(feature = "engine-host")]
pub async fn run(hostname: &str, port: u16) -> anyhow::Result<()> {
    use axum::{
        routing::{get, post},
        Router,
    };
    use std::net::SocketAddr;
    let state = runtime::RuntimeState::new();
    let app = Router::new()
        .route("/global/health", get(http::health))
        .route("/session", get(http::list).post(http::create))
        .route("/session/status", get(http::status))
        .route("/session/{id}", get(http::get_session))
        .route("/session/{id}/message", get(http::messages))
        .route("/session/{id}/prompt_async", post(http::prompt))
        .route("/session/{id}/abort", post(http::abort))
        .route("/session/{id}/event", get(http::events))
        .route("/permission", get(http::permissions))
        .route("/permission/{id}/reply", post(http::permission_reply))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind((hostname, port)).await?;
    let addr: SocketAddr = listener.local_addr()?;
    println!("openpaw server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
