//! Unified error type.
//!
//! Stability: stable — variants may be added; existing variants are not
//! removed or re-purposed (error strings surface in transcripts).

use thiserror::Error;

/// Unified error type for the whole agent stack.
///
/// Stability: stable.
#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("authentication failed: {status} {message}")]
    AuthFailed { status: u16, message: String },

    #[error("context too long: {0}")]
    ContextTooLong(String),

    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after_secs: Option<u64>,
    },

    #[error("server error: {status} {message}")]
    ServerError { status: u16, message: String },

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("tool host error: {0}")]
    ToolHost(String),

    #[error("store error: {0}")]
    Store(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("security violation: {0}")]
    Security(String),

    #[error("guardrail blocked: {tool} — {reason}")]
    GuardrailBlocked { tool: String, reason: String },

    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),

    #[error("session error: {0}")]
    Session(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn auth_failed(status: u16, message: impl Into<String>) -> Self {
        Self::AuthFailed {
            status,
            message: message.into(),
        }
    }

    pub fn context_too_long(message: impl Into<String>) -> Self {
        Self::ContextTooLong(message.into())
    }

    pub fn rate_limited(message: impl Into<String>, retry_after_secs: Option<u64>) -> Self {
        Self::RateLimited {
            message: message.into(),
            retry_after_secs,
        }
    }

    pub fn server_error(status: u16, message: impl Into<String>) -> Self {
        Self::ServerError {
            status,
            message: message.into(),
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    pub fn security(message: impl Into<String>) -> Self {
        Self::Security(message.into())
    }

    pub fn guardrail_blocked(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::GuardrailBlocked {
            tool: tool.into(),
            reason: reason.into(),
        }
    }

    pub fn session(message: impl Into<String>) -> Self {
        Self::Session(message.into())
    }

    pub fn is_context_too_long(&self) -> bool {
        matches!(self, Self::ContextTooLong(_))
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::ServerError { .. } | Self::Timeout(_)
        )
    }
}

/// Crate-wide result alias over [`Error`].
///
/// Stability: stable.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_three_segments() {
        // Error messages follow the "what happened" shape; the CLI layer is
        // responsible for the full three-segment rendering
        // (what / why / next step).
        let e = Error::Provider("connection reset".into());
        assert_eq!(e.to_string(), "provider error: connection reset");
    }

    #[test]
    fn retryable_classification() {
        assert!(Error::timeout("t").is_retryable());
        assert!(Error::rate_limited("rl", None).is_retryable());
        assert!(Error::server_error(500, "boom").is_retryable());
        assert!(!Error::auth_failed(401, "bad key").is_retryable());
        assert!(!Error::Provider("nope".into()).is_retryable());
    }

    #[test]
    fn context_too_long_detection() {
        let e = Error::context_too_long("140k > 128k");
        assert!(e.is_context_too_long());
    }
}
