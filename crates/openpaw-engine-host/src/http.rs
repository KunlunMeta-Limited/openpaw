use std::net::SocketAddr;

use axum::{routing::get, Json, Router};
use tokio::net::TcpListener;

use crate::HostConfig;

pub async fn bind(config: HostConfig) -> Result<TcpListener, String> {
    TcpListener::bind(config.bind_address())
        .await
        .map_err(|error| format!("cannot bind engine host: {error}"))
}

pub fn router() -> Router {
    Router::new().route(
        "/global/health",
        get(|| async { Json(serde_json::json!({ "healthy": true })) }),
    )
}

pub async fn serve(listener: TcpListener) -> Result<(), String> {
    axum::serve(listener, router())
        .await
        .map_err(|error| format!("engine host stopped: {error}"))
}

pub fn run_blocking(config: HostConfig) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start engine host runtime: {error}"))?;
    runtime.block_on(async {
        let listener = bind(config).await?;
        let address: SocketAddr = listener
            .local_addr()
            .map_err(|error| format!("cannot read engine host address: {error}"))?;
        println!("engine host listening on http://{address}");
        serve(listener).await
    })
}
