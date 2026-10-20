//! Per-server circuit breaker: Closed → (N consecutive failures) → Open →
//! (cooldown) → HalfOpen → (probe) → Closed | Open.
//!
//! While Open, calls fail fast with [`McpError::CircuitOpen`] instead of
//! hammering a dead server — a tool failure must never abort the agent's
//! tool batch, and a dead server must not cost a network round trip per
//! call. Each configured MCP server owns an independent breaker instance.
//!
//! Single-threaded by design: the host (MC-3) drives one breaker per
//! server sequentially, so no interior locking.

use std::time::{Duration, Instant};

use crate::error::McpError;

/// Consecutive failures that trip a Closed circuit (crate default).
pub const FAILURE_THRESHOLD: u32 = 3;

/// How long an Open circuit waits before allowing one HalfOpen probe
/// (crate default).
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

/// Circuit breaker state machine.
///
/// Stability: unstable.
#[derive(Debug)]
pub struct CircuitBreaker {
    state: State,
    failure_threshold: u32,
    cooldown: Duration,
    consecutive_failures: u32,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum State {
    Closed,
    Open { until: Instant },
    HalfOpen,
}

impl CircuitBreaker {
    /// Build a breaker with explicit tuning (production uses
    /// [`FAILURE_THRESHOLD`] / [`DEFAULT_COOLDOWN`]).
    ///
    /// Stability: unstable.
    /// 功能：创建使用显式阈值/冷却时间的熔断器。
    /// 参数：failure_threshold至少按一处理；cooldown为重新探测等待。
    /// 返回：初始关闭的熔断器。
    /// 错误：无。
    /// 状态：初始化连续失败计数为零。
    /// 并发与取消：调用方管理实例所有权，无取消。
    /// 权限：无外部副作用或权限变化。
    /// 关联：熔断器状态机及精确时钟边界回归。
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: State::Closed,
            failure_threshold: failure_threshold.max(1),
            cooldown,
            consecutive_failures: 0,
        }
    }

    /// Crate-default tuning.
    ///
    /// Stability: unstable.
    pub fn with_defaults() -> Self {
        Self::new(FAILURE_THRESHOLD, DEFAULT_COOLDOWN)
    }

    /// Current state (for `doctor` reporting and tests).
    ///
    /// Stability: unstable.
    pub fn state(&self) -> CircuitState {
        match self.state {
            State::Closed => CircuitState::Closed,
            State::Open { .. } => CircuitState::Open,
            State::HalfOpen => CircuitState::HalfOpen,
        }
    }

    /// Whether a call may proceed now. An Open circuit whose cooldown has
    /// elapsed transitions to HalfOpen and admits exactly one probe.
    ///
    /// Stability: unstable.
    /// 功能：检查当前时刻是否允许调用；冷却到期只放行一次探测。
    /// 参数：可变self为本实例状态。
    /// 返回：可尝试时为真，否则为假。
    /// 错误：无。
    /// 状态：到期Open切换HalfOpen；后续探测被拒绝。
    /// 并发与取消：独占可变借用；无异步等待或取消。
    /// 权限：仅控制本地调用准入，不授予远端权限。
    /// 关联：冷却边界状态机。
    pub fn can_attempt(&mut self) -> bool {
        self.can_attempt_at(Instant::now())
    }

    // Keep transition time explicit so cooldown boundaries can be tested without
    // assuming the OS schedules a sleeping test within a particular time window.
    fn can_attempt_at(&mut self, now: Instant) -> bool {
        match self.state {
            State::Closed => true,
            State::Open { until } => {
                if now >= until {
                    self.state = State::HalfOpen;
                    true
                } else {
                    false
                }
            }
            State::HalfOpen => false,
        }
    }

    /// Record a successful call: HalfOpen → Closed (probe passed);
    /// Closed → reset the consecutive-failure count.
    ///
    /// Stability: unstable.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.state = State::Closed;
    }

    /// Record a failed call: HalfOpen → Open (fresh cooldown);
    /// Closed → count up, trip at the threshold.
    ///
    /// Stability: unstable.
    /// 功能：记录失败，达到阈值或半开失败时重新打开熔断器。
    /// 参数：可变self为本实例状态。
    /// 返回：无。
    /// 错误：无。
    /// 状态：累计失败并按真实当前时刻设置新冷却期限。
    /// 并发与取消：独占可变借用；无异步等待或取消。
    /// 权限：仅更新本地状态。
    /// 关联：熔断恢复与新冷却期限回归。
    pub fn record_failure(&mut self) {
        self.record_failure_at(Instant::now());
    }

    fn record_failure_at(&mut self, now: Instant) {
        match self.state {
            State::HalfOpen => {
                self.state = State::Open {
                    until: now + self.cooldown,
                };
            }
            State::Closed | State::Open { .. } => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= self.failure_threshold {
                    self.state = State::Open {
                        until: now + self.cooldown,
                    };
                }
            }
        }
    }

    /// The fail-fast error for a rejected attempt.
    ///
    /// Stability: unstable.
    pub fn rejection(&self, server: &str) -> McpError {
        McpError::circuit_open(server)
    }
}

/// Breaker state for reporting (crate-external view).
///
/// Stability: unstable.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker() -> CircuitBreaker {
        CircuitBreaker::new(3, Duration::from_millis(50))
    }

    #[test]
    fn closed_admits_until_threshold_trips() {
        let mut b = breaker();
        let now = Instant::now();
        assert_eq!(b.state(), CircuitState::Closed);
        assert!(b.can_attempt_at(now));
        b.record_failure_at(now);
        b.record_failure_at(now);
        assert_eq!(b.state(), CircuitState::Closed, "below threshold");
        b.record_failure_at(now);
        assert_eq!(b.state(), CircuitState::Open, "tripped at threshold");
        assert!(!b.can_attempt_at(now), "open rejects");
    }

    #[test]
    fn success_resets_consecutive_count() {
        let mut b = breaker();
        let now = Instant::now();
        b.record_failure_at(now);
        b.record_failure_at(now);
        b.record_success();
        assert_eq!(b.state(), CircuitState::Closed);
        b.record_failure_at(now);
        b.record_failure_at(now);
        assert_eq!(
            b.state(),
            CircuitState::Closed,
            "two fresh failures after a success stay below threshold 3"
        );
    }

    #[test]
    fn open_transitions_to_half_open_after_cooldown() {
        let mut b = breaker();
        let now = Instant::now();
        for _ in 0..3 {
            b.record_failure_at(now);
        }
        assert_eq!(b.state(), CircuitState::Open);
        assert!(!b.can_attempt_at(now + Duration::from_millis(49)));
        assert!(
            b.can_attempt_at(now + b.cooldown),
            "cooldown boundary admits one probe"
        );
        assert_eq!(b.state(), CircuitState::HalfOpen);
        assert!(
            !b.can_attempt_at(now + b.cooldown),
            "half open admits exactly one probe"
        );
    }

    #[test]
    fn half_open_probe_success_closes() {
        let mut b = breaker();
        let now = Instant::now();
        for _ in 0..3 {
            b.record_failure_at(now);
        }
        let now = now + b.cooldown;
        assert!(b.can_attempt_at(now));
        b.record_success();
        assert_eq!(b.state(), CircuitState::Closed);
        assert!(b.can_attempt_at(now), "closed admits again");
        assert_eq!(b.consecutive_failures, 0);
    }

    #[test]
    fn half_open_probe_failure_reopens_with_fresh_cooldown() {
        let mut b = breaker();
        let now = Instant::now();
        for _ in 0..3 {
            b.record_failure_at(now);
        }
        let now = now + b.cooldown;
        assert!(b.can_attempt_at(now));
        b.record_failure_at(now);
        assert_eq!(b.state(), CircuitState::Open, "failed probe reopens");
        assert!(!b.can_attempt_at(now), "fresh cooldown starts");
        assert_eq!(
            b.state,
            State::Open {
                until: now + b.cooldown
            },
            "failed probe establishes a fresh deadline"
        );
        // The original deadline has elapsed; only the new deadline admits a probe.
        assert!(!b.can_attempt_at(now + Duration::from_millis(49)));
        assert!(b.can_attempt_at(now + b.cooldown));
        assert!(!b.can_attempt_at(now + b.cooldown), "one probe only");
    }

    #[test]
    fn rejection_names_the_server_without_details() {
        let b = breaker();
        let err = b.rejection("files");
        assert_eq!(err.to_string(), "mcp circuit open for server 'files'");
        assert!(!err.is_retryable(), "open circuit fails fast by design");
    }

    #[test]
    fn threshold_floor_is_one() {
        let mut b = CircuitBreaker::new(0, Duration::from_millis(10));
        b.record_failure();
        assert_eq!(b.state(), CircuitState::Open, "0 threshold clamps to 1");
    }
}
