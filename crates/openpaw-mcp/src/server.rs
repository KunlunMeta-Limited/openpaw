//! Live connection to one configured MCP server (rmcp thin wrapper).
//!
//! All rmcp usage on the client side concentrates here (the R9
//! single-point rule). The stdio transport is hand-rolled per the MC-0
//! decision: tokio `Command` with `kill_on_drop` + piped stdio feeds
//! rmcp's public `AsyncRwTransport`; the HTTP transport is rmcp's
//! Streamable HTTP client over plain http (no TLS in 1.0 — see the
//! crate card). Every call is bounded by the server's `timeout_secs`;
//! failures classify as `Transport` (retryable, feeds the breaker) or
//! `Timeout`. A tool-level `is_error` result is *not* a transport
//! failure — the server answered — so it never feeds the breaker.

use std::future::Future;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, JsonObject, ReadResourceResult, Resource, Tool,
};
use rmcp::service::{serve_client, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
use rmcp::RoleClient;
use tokio::process::Child;

use crate::config::ServerSpec;
use crate::error::{McpError, Result};

/// Grace period for the initialize handshake on connect.
const HANDSHAKE_CAP: Duration = Duration::from_secs(30);

/// One live session with an MCP server.
///
/// Stability: unstable.
#[derive(Debug)]
pub struct ServerSession {
    pub name: String,
    service: RunningService<RoleClient, ()>,
    child: Option<Child>,
}

impl ServerSession {
    /// Connect per the validated server spec (stdio spawns the child;
    /// http dials the endpoint). The handshake is bounded by `timeout`.
    ///
    /// Stability: unstable.
    pub async fn connect(name: &str, spec: &ServerSpec, timeout: Duration) -> Result<Self> {
        match spec {
            ServerSpec::Stdio { command, env, cwd } => {
                let mut cmd = tokio::process::Command::new(&command[0]);
                cmd.args(&command[1..])
                    .kill_on_drop(true)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit());
                if let Some(cwd) = cwd {
                    cmd.current_dir(cwd);
                }
                for (k, v) in env {
                    cmd.env(k, v);
                }
                let mut child = cmd
                    .spawn()
                    .map_err(|e| McpError::transport(format!("spawn failed: {e}")))?;
                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| McpError::transport("child stdin not piped"))?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| McpError::transport("child stdout not piped"))?;
                let transport = AsyncRwTransport::new_client(stdout, stdin);
                let service = handshake(name, transport, timeout).await?;
                Ok(Self {
                    name: name.to_string(),
                    service,
                    child: Some(child),
                })
            }
            ServerSpec::Http { url, .. } => {
                let transport = StreamableHttpClientTransport::from_uri(url.as_str());
                let service = handshake(name, transport, timeout).await?;
                Ok(Self {
                    name: name.to_string(),
                    service,
                    child: None,
                })
            }
        }
    }

    /// Connect over a caller-provided transport — the seam the in-process
    /// e2e drives (a duplex pair wired to an in-crate mock server), and
    /// the shape future embedding hosts would use.
    ///
    /// Stability: unstable.
    pub async fn connect_over_transport<T, E, A>(
        name: &str,
        transport: T,
        timeout: Duration,
    ) -> Result<Self>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A> + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = handshake(name, transport, timeout.min(HANDSHAKE_CAP)).await?;
        Ok(Self {
            name: name.to_string(),
            service,
            child: None,
        })
    }

    /// All tools the server exposes (paginated under the hood), bounded by
    /// `timeout`.
    ///
    /// Stability: unstable.
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<Tool>> {
        bounded(timeout, "list_tools", self.service.peer().list_all_tools()).await
    }

    /// One tool call, bounded by `timeout`. A tool-level error result
    /// (`is_error`) comes back as `Ok` — only transport/timeout faults
    /// surface as `Err`.
    ///
    /// Stability: unstable.
    pub async fn call_tool(
        &self,
        name: &str,
        args: JsonObject,
        timeout: Duration,
    ) -> Result<CallToolResult> {
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        bounded(timeout, name, self.service.peer().call_tool(params)).await
    }

    /// All resources the server exposes, bounded by `timeout`.
    ///
    /// Stability: unstable.
    pub async fn list_resources(&self, timeout: Duration) -> Result<Vec<Resource>> {
        bounded(
            timeout,
            "list_resources",
            self.service.peer().list_all_resources(),
        )
        .await
    }

    /// Read one resource by URI, bounded by `timeout`.
    ///
    /// Stability: unstable.
    pub async fn read_resource(&self, uri: &str, timeout: Duration) -> Result<ReadResourceResult> {
        let params = rmcp::model::ReadResourceRequestParams::new(uri);
        bounded(
            timeout,
            "read_resource",
            self.service.peer().read_resource(params),
        )
        .await
    }

    /// Graceful shutdown: stop the rmcp service loop, then reap the child
    /// (stdio) so no process outlives the session.
    ///
    /// Stability: unstable.
    pub async fn shutdown(self) -> Result<()> {
        let Self { service, child, .. } = self;
        service
            .cancel()
            .await
            .map_err(|e| McpError::transport(format!("service stop failed: {e}")))?;
        if let Some(mut child) = child {
            child
                .kill()
                .await
                .map_err(|e| McpError::transport(format!("child kill failed: {e}")))?;
        }
        Ok(())
    }
}

/// Handshake over any client transport, bounded by `timeout`.
async fn handshake<T, E, A>(
    name: &str,
    transport: T,
    timeout: Duration,
) -> Result<RunningService<RoleClient, ()>>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A> + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let bounded_timeout = timeout.min(HANDSHAKE_CAP);
    tokio::time::timeout(bounded_timeout, serve_client((), transport))
        .await
        .map_err(|_| {
            McpError::timeout(format!(
                "handshake with server '{name}' exceeded {bounded_timeout:?}"
            ))
        })?
        .map_err(|e| McpError::transport(format!("handshake with '{name}' failed: {e}")))
}

/// Bound any peer request by `timeout`; map elapsing to `Timeout`.
async fn bounded<F, T>(timeout: Duration, what: &str, fut: F) -> Result<T>
where
    F: Future<Output = std::result::Result<T, rmcp::service::ServiceError>>,
{
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| McpError::timeout(format!("'{what}' exceeded {timeout:?}")))?
        .map_err(|e| McpError::transport(format!("'{what}' failed: {e}")))
}
