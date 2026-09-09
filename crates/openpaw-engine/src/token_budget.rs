//! Token budget — usage tracking with continuation nudges and diminishing
//! returns detection (same-source port of the upstream tracker; the
//! parser-side budget syntax lands with the CLI config work).

/// Token budget configuration.
#[derive(Debug, Clone)]
pub struct TokenBudgetConfig {
    /// Total token budget (`None` = budget disabled).
    pub total_budget: Option<u64>,
    /// Continue threshold (default 0.9 = 90%).
    pub continue_threshold: f64,
    /// Diminishing-returns detection window (default 3).
    pub diminishing_window: usize,
    /// Diminishing delta threshold (default 500 tokens).
    pub diminishing_delta_threshold: u64,
    /// Nudge message template.
    pub nudge_message: String,
}

impl Default for TokenBudgetConfig {
    fn default() -> Self {
        Self {
            total_budget: None,
            continue_threshold: 0.9,
            diminishing_window: 3,
            diminishing_delta_threshold: 500,
            nudge_message: "You have remaining token budget. Continue working on the task."
                .to_string(),
        }
    }
}

impl TokenBudgetConfig {
    pub fn with_budget(mut self, budget: u64) -> Self {
        self.total_budget = Some(budget);
        self
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.continue_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    pub fn with_diminishing_window(mut self, window: usize) -> Self {
        self.diminishing_window = window.max(2);
        self
    }

    pub fn with_diminishing_delta(mut self, delta: u64) -> Self {
        self.diminishing_delta_threshold = delta;
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.total_budget.is_some()
    }
}

/// Token budget tracker: accumulates per-turn usage, decides whether the
/// loop should nudge a stopping model to continue, and detects
/// diminishing returns.
#[derive(Debug, Clone)]
pub struct TokenBudgetTracker {
    config: TokenBudgetConfig,
    /// Tokens used (accumulated over the current run).
    pub used_tokens: u64,
    /// Per-turn token usage (for diminishing-returns detection).
    pub turn_tokens_history: Vec<u64>,
    /// Continuations triggered by the token budget.
    pub continuation_count: usize,
    /// Context tokens consumed before a compaction (cross-compaction
    /// accounting).
    pub pre_compact_context_tokens: u64,
    pub stopped_due_to_diminishing: bool,
}

impl TokenBudgetTracker {
    pub fn new(config: TokenBudgetConfig) -> Self {
        Self {
            config,
            used_tokens: 0,
            turn_tokens_history: Vec::new(),
            continuation_count: 0,
            pre_compact_context_tokens: 0,
            stopped_due_to_diminishing: false,
        }
    }

    /// Tracker with budget disabled (never continues).
    pub fn disabled() -> Self {
        Self::new(TokenBudgetConfig::default())
    }

    /// Record one turn's token usage.
    pub fn record_turn(&mut self, turn_tokens: u64) {
        self.used_tokens += turn_tokens;
        self.turn_tokens_history.push(turn_tokens);

        let window = self.config.diminishing_window.max(2);
        if self.turn_tokens_history.len() > window {
            self.turn_tokens_history.remove(0);
        }
    }

    /// Record context tokens consumed before a compaction.
    pub fn record_pre_compact(&mut self, context_tokens: u64) {
        self.pre_compact_context_tokens += context_tokens;
    }

    /// Effective usage including compacted-away context.
    pub fn effective_used_tokens(&self) -> u64 {
        self.used_tokens + self.pre_compact_context_tokens
    }

    /// Usage as a fraction (0.0–1.0). Without a budget: 1.0 (never
    /// continues).
    pub fn usage_percentage(&self) -> f64 {
        match self.config.total_budget {
            Some(budget) if budget > 0 => self.effective_used_tokens() as f64 / budget as f64,
            _ => 1.0,
        }
    }

    /// Diminishing returns: enough continuations have happened and the
    /// latest per-turn delta is below the threshold.
    pub fn is_diminishing(&self) -> bool {
        if self.continuation_count < self.config.diminishing_window {
            return false;
        }
        if self.turn_tokens_history.len() < 2 {
            return false;
        }
        let recent: Vec<u64> = self
            .turn_tokens_history
            .iter()
            .rev()
            .take(2)
            .copied()
            .collect();
        let delta = recent[0].abs_diff(recent[1]);
        delta < self.config.diminishing_delta_threshold
    }

    /// Whether the loop should nudge the model to continue: budget enabled,
    /// not diminishing, usage below the continue threshold.
    pub fn should_continue(&self) -> bool {
        if !self.config.is_enabled() {
            return false;
        }
        if self.is_diminishing() {
            return false;
        }
        self.usage_percentage() < self.config.continue_threshold
    }

    pub fn mark_diminishing_stop(&mut self) {
        self.stopped_due_to_diminishing = self.config.is_enabled() && self.is_diminishing();
    }

    pub fn increment_continuation(&mut self) {
        self.continuation_count += 1;
    }

    /// Reset for a new task.
    pub fn reset(&mut self) {
        self.used_tokens = 0;
        self.turn_tokens_history.clear();
        self.continuation_count = 0;
        self.pre_compact_context_tokens = 0;
        self.stopped_due_to_diminishing = false;
    }

    pub fn config(&self) -> &TokenBudgetConfig {
        &self.config
    }

    /// Remaining budget, if enabled.
    pub fn remaining_budget(&self) -> Option<u64> {
        self.config
            .total_budget
            .map(|budget| budget.saturating_sub(self.effective_used_tokens()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_tracker_never_continues() {
        let t = TokenBudgetTracker::disabled();
        assert!(!t.config().is_enabled());
        assert!(!t.should_continue());
        assert_eq!(t.usage_percentage(), 1.0);
    }

    #[test]
    fn continues_below_threshold_and_stops_above() {
        let mut t = TokenBudgetTracker::new(TokenBudgetConfig::default().with_budget(1_000));
        t.record_turn(500); // 50%
        assert!(t.should_continue());
        t.record_turn(400); // 90% → not below threshold
        assert!(!t.should_continue());
        assert_eq!(t.remaining_budget(), Some(100));
    }

    #[test]
    fn diminishing_returns_detected_after_window() {
        let mut t = TokenBudgetTracker::new(
            TokenBudgetConfig::default()
                .with_budget(1_000_000)
                .with_diminishing_window(2)
                .with_diminishing_delta(500),
        );
        t.record_turn(2_000);
        t.record_turn(2_000);
        // Not enough continuations yet.
        assert!(!t.is_diminishing());
        t.increment_continuation();
        t.increment_continuation();
        // Two flat turns after ≥ window continuations → diminishing.
        assert!(t.is_diminishing());
        assert!(!t.should_continue());
    }

    #[test]
    fn pre_compact_tokens_count_toward_usage() {
        let mut t = TokenBudgetTracker::new(TokenBudgetConfig::default().with_budget(1_000));
        t.record_pre_compact(800);
        t.record_turn(90);
        assert_eq!(t.effective_used_tokens(), 890);
        assert!(t.should_continue()); // 89% < 90%
        t.record_turn(20);
        assert!(!t.should_continue()); // 91% ≥ 90%
    }

    #[test]
    fn reset_clears_everything() {
        let mut t = TokenBudgetTracker::new(TokenBudgetConfig::default().with_budget(1_000));
        t.record_turn(500);
        t.increment_continuation();
        t.reset();
        assert_eq!(t.used_tokens, 0);
        assert_eq!(t.continuation_count, 0);
        assert!(t.turn_tokens_history.is_empty());
    }

    #[test]
    fn zero_budget_never_continues() {
        let t = TokenBudgetTracker::new(TokenBudgetConfig::default().with_budget(0));
        assert_eq!(t.usage_percentage(), 1.0);
        assert!(!t.should_continue());
    }
}
