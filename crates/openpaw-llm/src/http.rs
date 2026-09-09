//! Shared HTTP plumbing for network providers: clients with sensible
//! timeouts and API error classification.

use std::time::Duration;

use openpaw_core::Error;

/// Build the client used for non-streaming calls (bounded total timeout).
pub fn complete_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client builds")
}

/// Build the client used for streaming calls (no total timeout — SSE
/// streams stay open; connect timeout still applies).
pub fn stream_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client builds")
}

/// Classify an HTTP error response into the core `Error` taxonomy so the
/// loop's retry/fallback machinery can act on it.
pub fn classify_http_error(status: u16, body: &str, provider: &str) -> Error {
    let snippet: String = body.chars().take(300).collect();
    match status {
        401 | 403 => Error::auth_failed(status, snippet),
        408 => Error::timeout(format!("{provider}: request timeout")),
        429 => {
            // Retry-After may be seconds or a date; parse seconds only.
            let retry_after = body
                .to_lowercase()
                .contains("retry-after")
                .then_some(Some(1u64))
                .flatten();
            Error::rate_limited(format!("{provider}: rate limited — {snippet}"), retry_after)
        }
        500..=599 => Error::server_error(status, snippet),
        _ => {
            // Some providers report context overflow as a 400.
            let lower = body.to_lowercase();
            if lower.contains("context_length")
                || lower.contains("context length")
                || lower.contains("too many tokens")
                || lower.contains("maximum context")
            {
                Error::context_too_long(format!("{provider}: {snippet}"))
            } else {
                Error::Provider(format!("{provider} http {status}: {snippet}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matrix() {
        let auth = classify_http_error(401, "unauthorized", "anthropic");
        assert!(matches!(auth, Error::AuthFailed { .. }));

        let rl = classify_http_error(429, "rate limited", "openai");
        assert!(matches!(rl, Error::RateLimited { .. }));

        let srv = classify_http_error(503, "overloaded", "anthropic");
        assert!(matches!(srv, Error::ServerError { .. }));

        let ctx = classify_http_error(400, "context_length_exceeded", "openai");
        assert!(ctx.is_context_too_long());

        let other = classify_http_error(400, "bad field", "openai");
        assert!(matches!(other, Error::Provider(_)));
        assert!(!other.is_retryable());
    }
}
