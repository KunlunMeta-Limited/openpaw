//! openpaw-mcp — MCP client host.
//!
//! Planned scope (M2): host (stdio + Streamable HTTP), circuit breaker,
//! `${VAR}` interpolation, minimal resource list/read. See the engineering
//! plan, crate card for this crate.
//!
//! Landed modules (MC-1): [`error`], [`interpolator`], [`config`].
//! Landed modules (MC-2): [`circuit_breaker`], [`server`].

pub mod circuit_breaker;
pub mod config;
pub mod error;
pub mod interpolator;
pub mod server;

pub use circuit_breaker::{CircuitBreaker, CircuitState, DEFAULT_COOLDOWN, FAILURE_THRESHOLD};
pub use config::{McpConfig, ServerEntry, ServerSpec};
pub use error::{McpError, Result};
pub use interpolator::{expand, expand_with, is_single_var_reference};
pub use server::ServerSession;

#[cfg(test)]
mod spike_probe {
    //! MC-0 verification spike (devlog session log has the conclusions).
    //!
    //! Offline-safe probes that pin the rmcp 1.8 client surface the
    //! MC-2/MC-3 implementation will drive:
    //! 1. hand-rolled stdio transport on `AsyncRwTransport` + tokio
    //!    `kill_on_drop` (a deliberate choice over rmcp's
    //!    `transport-child-process`, see Cargo.toml),
    //! 2. serve/handshake fails fast against a process that is not an MCP
    //!    server (immediate EOF),
    //! 3. Streamable HTTP client transport construction + fast-fail
    //!    handshake against a closed loopback port,
    //! 4. compile-only type-shape references for the request/result models.

    use std::future::Future;
    use std::time::Duration;

    use rmcp::model::{CallToolRequestParams, JsonObject, Tool};
    use rmcp::service::{serve_client, Peer, ServiceError};
    use rmcp::transport::async_rw::AsyncRwTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
    use rmcp::RoleClient;
    use tokio::process::Command;

    /// The peer surface host.rs will drive: list tools (paginated),
    /// call a tool, list + read resources.
    #[allow(dead_code, clippy::type_complexity)]
    fn client_surface_probe(
        peer: &Peer<RoleClient>,
    ) -> (
        impl Future<Output = Result<Vec<Tool>, ServiceError>> + '_,
        impl Future<Output = Result<rmcp::model::CallToolResult, ServiceError>> + '_,
        impl Future<Output = Result<Vec<rmcp::model::Resource>, ServiceError>> + '_,
        impl Future<Output = Result<rmcp::model::ReadResourceResult, ServiceError>> + '_,
    ) {
        (
            peer.list_all_tools(),
            peer.call_tool(CallToolRequestParams::new("probe")),
            peer.list_all_resources(),
            peer.read_resource(rmcp::model::ReadResourceRequestParams::new("probe:uri")),
        )
    }

    /// Argument-building shape pinned for host.rs (MC-3).
    #[allow(dead_code)]
    fn call_params_shape(args: JsonObject) -> CallToolRequestParams {
        CallToolRequestParams::new("probe").with_arguments(args)
    }

    /// The stdio spawn shape server.rs will use (MC-2): tokio Command with
    /// `kill_on_drop`, piped stdin/stdout, stderr inherited, then the
    /// rmcp `AsyncRwTransport` client over the child's pipes.
    #[tokio::test]
    async fn stdio_transport_spawn_shape() {
        let mut cmd = Command::new(if cfg!(windows) { "ping" } else { "sleep" });
        if cfg!(windows) {
            cmd.arg("-n").arg("30").arg("127.0.0.1");
        } else {
            cmd.arg("30");
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().expect("spawn child");
        assert!(child.id().is_some(), "child pid available after spawn");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let _transport: AsyncRwTransport<RoleClient, _, _> =
            AsyncRwTransport::new_client(stdout, stdin);

        child.kill().await.expect("explicit kill terminates child");
    }

    /// Probe 2: serve/handshake against a non-MCP process fails fast (the
    /// child prints one line and exits; no MCP server needed, fully
    /// offline).
    #[tokio::test]
    async fn handshake_fails_fast_on_non_mcp_process() {
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            cmd.arg("/c").arg("echo hi");
        } else {
            cmd.arg("-c").arg("echo hi");
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn echo process");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let transport: AsyncRwTransport<RoleClient, _, _> =
            AsyncRwTransport::new_client(stdout, stdin);
        let result = tokio::time::timeout(Duration::from_secs(10), serve_client((), transport))
            .await
            .expect("handshake attempt must not hang");
        assert!(result.is_err(), "non-MCP output must fail the handshake");
        // `child` dropped here: kill_on_drop reaps it even after serve
        // consumed the pipes.
        drop(child);
    }

    /// Probe 3: HTTP transport construction + handshake fails fast against
    /// a closed loopback port (offline; no server needed).
    #[tokio::test]
    async fn http_handshake_fails_fast_on_closed_port() {
        let transport = StreamableHttpClientTransport::from_uri("http://127.0.0.1:1/");
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(10), serve_client((), transport))
            .await
            .expect("handshake attempt must not hang");
        assert!(result.is_err(), "closed port must fail the handshake");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "connection refused surfaces quickly, not via timeout"
        );
    }
}
