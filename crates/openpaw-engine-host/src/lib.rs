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
pub use http::BasicAuth;
#[cfg(feature = "engine-host")]
pub use runtime::RuntimeState;

#[cfg(feature = "engine-host")]
use axum::{
    middleware,
    routing::{get, post},
    Router,
};
#[cfg(feature = "engine-host")]
use std::path::PathBuf;

/// Everything one host process needs before it binds a socket.
///
/// Environment variables supply the defaults and the CLI layers on top, so
/// `--engine-host` shares `chat`'s resolution order (`--provider`,
/// `--workspace`, `--home`).
#[cfg(feature = "engine-host")]
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Bind address. The host is loopback-only by contract; containers pass
    /// `0.0.0.0` explicitly.
    pub hostname: String,
    /// Bind port. `0` picks a free port and the ready line reports it.
    pub port: u16,
    /// Default provider for prompts that omit `model`.
    pub provider: String,
    /// Root of the `<home>/sessions/<id>.jsonl` transcripts.
    pub home: PathBuf,
    /// Workspace used when neither the request header nor the query
    /// parameter names one.
    pub workspace: PathBuf,
    /// Basic Authentication username (`OPENCODE_SERVER_USERNAME`).
    pub username: Option<String>,
    /// Basic Authentication password (`OPENCODE_SERVER_PASSWORD`).
    pub password: Option<String>,
}

#[cfg(feature = "engine-host")]
impl HostConfig {
    /// Environment-derived defaults. `OPENPAW_ENGINE_PROVIDER` decides the
    /// default provider and `OPENPAW_HOME` the transcript root (falling back
    /// to `~/.openpaw`, like `chat`).
    pub fn from_env() -> Self {
        Self {
            hostname: "127.0.0.1".into(),
            port: 0,
            provider: env_value("OPENPAW_ENGINE_PROVIDER").unwrap_or_else(|| "mock".into()),
            home: openpaw_core::openpaw_home(),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            username: env_value("OPENCODE_SERVER_USERNAME"),
            password: env_value("OPENCODE_SERVER_PASSWORD"),
        }
    }

    /// Set the bind address and port.
    pub fn with_listener(mut self, hostname: impl Into<String>, port: u16) -> Self {
        self.hostname = hostname.into();
        self.port = port;
        self
    }

    /// Override the default provider when the CLI supplied one.
    pub fn with_provider(mut self, provider: Option<String>) -> Self {
        if let Some(provider) = provider {
            self.provider = provider;
        }
        self
    }

    /// Override the transcript root when the CLI supplied `--home`.
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        if let Some(home) = home {
            self.home = home;
        }
        self
    }

    /// Override the default workspace when the CLI supplied `--workspace`.
    pub fn with_workspace(mut self, workspace: Option<PathBuf>) -> Self {
        if let Some(workspace) = workspace {
            self.workspace = workspace;
        }
        self
    }

    /// Resolve §3.2's authentication rule: neither credential set disables
    /// authentication, setting exactly one is a startup failure, setting both
    /// requires Basic Authentication on every route.
    pub fn authentication(&self) -> anyhow::Result<Option<BasicAuth>> {
        match (&self.username, &self.password) {
            (None, None) => Ok(None),
            (Some(username), Some(password)) => {
                Ok(Some(BasicAuth::new(username.clone(), password.clone())))
            }
            (Some(_), None) => anyhow::bail!(
                "OPENCODE_SERVER_USERNAME is set but OPENCODE_SERVER_PASSWORD is missing; \
                 set both or neither"
            ),
            (None, Some(_)) => anyhow::bail!(
                "OPENCODE_SERVER_PASSWORD is set but OPENCODE_SERVER_USERNAME is missing; \
                 set both or neither"
            ),
        }
    }
}

#[cfg(feature = "engine-host")]
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Assemble the OpenCode v1 surface: the required routes, the explicitly
/// degraded routes, and the Basic Authentication layer. Exported so the
/// contract tests can drive it without binding a socket.
#[cfg(feature = "engine-host")]
pub fn router(state: RuntimeState, auth: Option<BasicAuth>) -> Router {
    Router::new()
        .route("/global/health", get(http::health))
        .route("/session", get(http::list).post(http::create))
        .route("/session/status", get(http::status))
        .route("/session/{id}", get(http::get_session))
        .route("/session/{id}/message", get(http::messages))
        .route("/session/{id}/todo", get(http::todo))
        .route("/session/{id}/prompt_async", post(http::prompt))
        .route("/session/{id}/abort", post(http::abort))
        .route("/event", get(http::global_events))
        .route("/global/event", get(http::global_events))
        .route("/api/event", get(http::global_events))
        .route("/permission", get(http::permissions))
        .route("/permission/{id}/reply", post(http::permission_reply))
        .route("/provider", get(http::providers))
        // Explicit degradations (§3.2): unimplemented v1 capabilities answer
        // with the documented empty shape instead of a 404.
        .route("/config", get(http::empty_object))
        .route("/mcp", get(http::empty_object))
        .route("/command", get(http::empty_array))
        .route("/project", get(http::empty_array))
        .route("/session/{id}/command", post(http::degraded))
        .route("/session/{id}/shell", post(http::degraded))
        .route("/session/{id}/fork", post(http::degraded))
        .route("/session/{id}/revert", post(http::degraded))
        .route("/session/{id}/summarize", post(http::degraded))
        .layer(middleware::from_fn_with_state(auth, http::require_auth))
        .with_state(state)
}

/// Bind and serve until the process is stopped.
///
/// The ready line is the only thing written to stdout, so launchers can read
/// the port of a `--engine-host-port 0` run.
#[cfg(feature = "engine-host")]
pub async fn run(config: HostConfig) -> anyhow::Result<()> {
    let auth = config.authentication()?;
    let state = RuntimeState::new(&config);
    let app = router(state, auth);
    let listener = tokio::net::TcpListener::bind((config.hostname.as_str(), config.port)).await?;
    let addr = listener.local_addr()?;
    println!("opencode server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
