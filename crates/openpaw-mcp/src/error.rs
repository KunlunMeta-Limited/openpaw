//! Error types for the MCP client host.
//!
//! Messages follow the repo-wide "what happened" shape; the CLI layer
//! renders the full three-segment form (what / why / next step). Config
//! errors carry the next step inline because `config doctor` surfaces
//! them verbatim.

use thiserror::Error;

/// Unified error type for the MCP client host.
///
/// Stability: unstable — variants may be added; existing variants are not
/// removed or re-purposed (error strings surface in transcripts).
#[derive(Debug, Error)]
pub enum McpError {
    /// Configuration could not be parsed or failed validation. Fail-closed:
    /// the server is never started from a doubtful config.
    #[error("mcp config error: {0}")]
    Config(String),

    /// A `${VAR}` reference could not be expanded. The message names the
    /// variable but never its value (values may be credentials).
    #[error("mcp interpolation error: {0}")]
    Interpolation(String),

    /// A server connection failed (spawn, handshake, or send). Retryable
    /// depending on the failure class; the circuit breaker consumes this.
    #[error("mcp transport error: {0}")]
    Transport(String),

    /// The server's circuit breaker is open: calls fail fast until the
    /// cooldown elapses. Never blocks a tool batch.
    #[error("mcp circuit open for server '{0}'")]
    CircuitOpen(String),

    /// A call exceeded its per-call timeout.
    #[error("mcp call timeout: {0}")]
    Timeout(String),

    /// A remote tool call or resource read returned an error result.
    #[error("mcp tool error: {0}")]
    Tool(String),
}

impl McpError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub fn interpolation(message: impl Into<String>) -> Self {
        Self::Interpolation(message.into())
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }

    pub fn circuit_open(server: impl Into<String>) -> Self {
        Self::CircuitOpen(server.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    pub fn tool(message: impl Into<String>) -> Self {
        Self::Tool(message.into())
    }

    /// True for failures where retrying the same call may succeed.
    /// Config/interpolation faults are deterministic — never retryable;
    /// an open circuit fails fast by design.
    ///
    /// Stability: unstable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Timeout(_))
    }

    /// True when the fault is in configuration, not runtime. `openpaw
    /// doctor` and chat startup use this to decide between "fix the file"
    /// guidance and degrade-and-warn.
    ///
    /// Stability: unstable.
    pub fn is_config(&self) -> bool {
        matches!(self, Self::Config(_) | Self::Interpolation(_))
    }
}

/// Crate-wide result alias over [`McpError`].
///
/// Stability: unstable.
pub type Result<T> = std::result::Result<T, McpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_shapes() {
        assert_eq!(
            McpError::circuit_open("files").to_string(),
            "mcp circuit open for server 'files'"
        );
        assert_eq!(
            McpError::config("missing command").to_string(),
            "mcp config error: missing command"
        );
    }

    #[test]
    fn retryable_classification() {
        assert!(McpError::timeout("60s").is_retryable());
        assert!(McpError::transport("connection reset").is_retryable());
        assert!(!McpError::config("bad url").is_retryable());
        assert!(!McpError::circuit_open("files").is_retryable());
    }

    #[test]
    fn config_classification_covers_interpolation() {
        assert!(McpError::interpolation("no such variable X").is_config());
        assert!(McpError::config("bad url").is_config());
        assert!(!McpError::tool("remote said no").is_config());
    }
}
