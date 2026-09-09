//! API error classification and retry policy (ported subset).
//!
//! Stability: stable — the classification enum is wire-visible in retry
//! decisions; new classes may be added, existing ones keep their meaning.

use std::time::Duration;

use crate::Error;

/// Classification driving retry/fallback decisions in the agent loop.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorClass {
    /// Transient network/timeout — retry the same provider.
    RetryableTransient,
    /// Overload/rate-limit with backoff — retry the same provider.
    RetryableOverloaded,
    /// Context window exceeded — trigger compaction, then retry.
    ContextTooLong,
    /// Generic server failure — try the fallback provider.
    FallbackCandidate,
    /// Unrecoverable (auth, bad request, ...).
    Fatal,
    /// Max output tokens exceeded — recoverable (escalate max_tokens).
    MaxOutputTokens,
    /// Media size exceeded — recoverable (reactive compaction).
    MediaSize,
}

/// Classify an error for the retry/fallback decision.
///
/// Stability: stable.
pub fn classify_api_error(error: &Error) -> ApiErrorClass {
    match error {
        Error::AuthFailed { .. } => ApiErrorClass::Fatal,
        Error::ContextTooLong(_) => ApiErrorClass::ContextTooLong,
        Error::RateLimited {
            retry_after_secs: Some(_),
            ..
        } => ApiErrorClass::RetryableOverloaded,
        Error::RateLimited {
            retry_after_secs: None,
            ..
        } => ApiErrorClass::RetryableTransient,
        Error::ServerError { message, .. } => classify_server_error(message),
        Error::Timeout(_) => ApiErrorClass::RetryableTransient,
        Error::Provider(msg) => classify_provider_string(msg),
        Error::Other(err) => classify_provider_string(&err.to_string()),
        _ => ApiErrorClass::Fatal,
    }
}

fn classify_server_error(message: &str) -> ApiErrorClass {
    let lower = message.to_lowercase();
    if lower.contains("too busy")
        || lower.contains("overloaded")
        || lower.contains("capacity")
        || lower.contains("try again later")
        || lower.contains("temporarily unavailable")
    {
        return ApiErrorClass::RetryableOverloaded;
    }
    if let Some(withheld) = classify_withholding_string(message) {
        return withheld;
    }
    ApiErrorClass::FallbackCandidate
}

fn classify_provider_string(msg: &str) -> ApiErrorClass {
    let lower = msg.to_lowercase();

    // Auth errors: HTTP status patterns, not bare digits.
    if lower.contains("http 401")
        || lower.contains("http 403")
        || lower.contains("status 401")
        || lower.contains("status 403")
        || lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
    {
        return ApiErrorClass::Fatal;
    }

    if lower.contains("context_length_exceeded")
        || lower.contains("context length")
        || lower.contains("too many tokens")
        || lower.contains("max_context_window")
        || lower.contains("maximum context")
    {
        return ApiErrorClass::ContextTooLong;
    }

    if lower.contains("max_output_tokens")
        || lower.contains("max output tokens")
        || lower.contains("maximum output tokens")
        || lower.contains("output too long")
        || lower.contains("response too long")
        || lower.contains("max_tokens exceeded")
    {
        return ApiErrorClass::MaxOutputTokens;
    }

    if lower.contains("media_size")
        || lower.contains("media size")
        || lower.contains("image too large")
        || lower.contains("image too big")
        || lower.contains("file too large")
        || lower.contains("payload too large")
        || lower.contains("request entity too large")
    {
        return ApiErrorClass::MediaSize;
    }

    if lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("429 too many requests")
        || lower.contains("rate limit")
    {
        if lower.contains("retry-after") || lower.contains("retry_after") {
            return ApiErrorClass::RetryableOverloaded;
        }
        return ApiErrorClass::RetryableTransient;
    }

    if lower.contains("http 500")
        || lower.contains("http 502")
        || lower.contains("http 503")
        || lower.contains("http 529")
        || lower.contains("status 500")
        || lower.contains("status 502")
        || lower.contains("status 503")
        || lower.contains("status 529")
        || lower.contains("internal server error")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
    {
        return ApiErrorClass::FallbackCandidate;
    }

    if lower.contains("overloaded") || lower.contains("capacity") {
        return ApiErrorClass::RetryableOverloaded;
    }

    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("resolve")
        || lower.contains("tcp connect")
        || lower.contains("network")
        || lower.contains("broken pipe")
        || lower.contains("reset")
        || lower.contains("unreachable")
        || lower.contains("refused")
        || lower.contains("eof")
        || lower.contains("channel closed")
        || lower.contains("error sending request")
        || lower.contains("request failed")
        // SSE chunk decode failures: mid-stream resets, truncated
        // gzip/br encoding over unstable links — re-establish the stream.
        || lower.contains("decoding response body")
        || lower.contains("error decoding")
        || lower.contains("decode error")
    {
        return ApiErrorClass::RetryableTransient;
    }

    ApiErrorClass::Fatal
}

fn classify_withholding_string(msg: &str) -> Option<ApiErrorClass> {
    let lower = msg.to_lowercase();
    if lower.contains("max_output_tokens")
        || lower.contains("max output tokens")
        || lower.contains("maximum output tokens")
        || lower.contains("output too long")
        || lower.contains("response too long")
    {
        return Some(ApiErrorClass::MaxOutputTokens);
    }
    if lower.contains("media_size")
        || lower.contains("media size")
        || lower.contains("image too large")
        || lower.contains("image too big")
        || lower.contains("file too large")
        || lower.contains("payload too large")
        || lower.contains("request entity too large")
    {
        return Some(ApiErrorClass::MediaSize);
    }
    None
}

/// Exponential backoff with ±25% jitter; honors the server's Retry-After.
///
/// Stability: stable.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: usize,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    pub fn new(
        max_retries: usize,
        base_delay_ms: u64,
        max_delay_ms: u64,
        backoff_multiplier: f64,
    ) -> Self {
        Self {
            max_retries,
            base_delay: Duration::from_millis(base_delay_ms),
            max_delay: Duration::from_millis(max_delay_ms),
            backoff_multiplier,
        }
    }

    pub fn delay_for_attempt(&self, attempt: usize) -> Duration {
        self.delay_for_attempt_with_jitter(attempt, None)
    }

    /// Delay with jitter; `retry_after_secs` (from a RateLimited error)
    /// takes precedence over computed backoff.
    pub fn delay_for_attempt_with_jitter(
        &self,
        attempt: usize,
        retry_after_secs: Option<u64>,
    ) -> Duration {
        if let Some(secs) = retry_after_secs {
            let server_delay = Duration::from_secs(secs);
            let jitter = Self::compute_jitter(server_delay);
            return Self::apply_jitter(server_delay, jitter);
        }

        let multiplier = self.backoff_multiplier.powi(attempt as i32);
        let delay = self.base_delay.as_secs_f64() * multiplier;
        let capped = delay.min(self.max_delay.as_secs_f64());
        let base = Duration::from_secs_f64(capped);

        let jitter = Self::compute_jitter(base);
        Self::apply_jitter(base, jitter)
    }

    fn apply_jitter(base: Duration, jitter_ms: i64) -> Duration {
        if jitter_ms >= 0 {
            base + Duration::from_millis(jitter_ms as u64)
        } else {
            base.saturating_sub(Duration::from_millis(jitter_ms.unsigned_abs()))
        }
    }

    /// Signed jitter in [-25%, +25%] of the base delay, xorshift PRNG
    /// seeded from the clock (no external dependency).
    fn compute_jitter(base: Duration) -> i64 {
        let base_ms = base.as_millis() as i64;
        if base_ms == 0 {
            return 0;
        }
        use std::cell::Cell;
        thread_local! {
            static RNG_STATE: Cell<u64> = Cell::new({
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64)
                    .unwrap_or(1);
                nanos | 1
            });
        }
        RNG_STATE.with(|state| {
            let mut x = state.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            state.set(x);
            let range = (base_ms / 4).max(1);
            (x % (range as u64 * 2 + 1)) as i64 - range
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matrix() {
        assert_eq!(
            classify_api_error(&Error::AuthFailed {
                status: 401,
                message: "x".into()
            }),
            ApiErrorClass::Fatal
        );
        assert_eq!(
            classify_api_error(&Error::context_too_long("140k")),
            ApiErrorClass::ContextTooLong
        );
        assert_eq!(
            classify_api_error(&Error::rate_limited("rl", Some(30))),
            ApiErrorClass::RetryableOverloaded
        );
        assert_eq!(
            classify_api_error(&Error::rate_limited("rl", None)),
            ApiErrorClass::RetryableTransient
        );
        assert_eq!(
            classify_api_error(&Error::timeout("t")),
            ApiErrorClass::RetryableTransient
        );
        assert_eq!(
            classify_api_error(&Error::server_error(503, "try again later")),
            ApiErrorClass::RetryableOverloaded
        );
        assert_eq!(
            classify_api_error(&Error::server_error(500, "boom")),
            ApiErrorClass::FallbackCandidate
        );
        assert_eq!(
            classify_api_error(&Error::Provider("HTTP 401 unauthorized".into())),
            ApiErrorClass::Fatal
        );
        assert_eq!(
            classify_api_error(&Error::Provider("context_length_exceeded".into())),
            ApiErrorClass::ContextTooLong
        );
        assert_eq!(
            classify_api_error(&Error::Provider("connection reset by peer".into())),
            ApiErrorClass::RetryableTransient
        );
        assert_eq!(
            classify_api_error(&Error::Provider("something odd".into())),
            ApiErrorClass::Fatal
        );
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        let p = RetryPolicy::new(10, 1000, 5000, 2.0);
        assert!(p.delay_for_attempt(1).as_millis() <= 5000 * 5 / 4 + 1);
        let d = p.delay_for_attempt(20).as_millis();
        assert!(d <= 5000 + 5000 / 4, "must cap at max_delay, got {d}ms");
    }

    #[test]
    fn retry_after_overrides_backoff() {
        let p = RetryPolicy::default();
        let d = p.delay_for_attempt_with_jitter(1, Some(7));
        assert!(d.as_secs() >= 5 && d.as_secs() <= 9, "≈7s ±25%, got {d:?}");
    }

    #[test]
    fn jitter_stays_in_bounds() {
        let p = RetryPolicy::new(5, 2000, 10_000, 2.0);
        for attempt in 1usize..=5 {
            let base = p.base_delay.as_millis() as f64 * 2f64.powi(attempt as i32);
            let capped = base.min(10_000.0);
            let d = p.delay_for_attempt(attempt).as_millis() as f64;
            assert!(
                d >= capped * 0.75 - 1.0 && d <= capped * 1.25 + 1.0,
                "attempt {attempt}: {d} outside ±25% of {capped}"
            );
        }
    }
}
