//! Elastic iteration budget for the agent loop.
//!
//! Three-tier range: `min` (floor) → `current_max` (grows via `expand`) →
//! `max` (elastic ceiling, grows via `expand_max`) → `ceiling` (absolute
//! hard limit), plus a last-resort grace count.
//!
//! Stability: stable.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Elastic iteration budget for the agent loop: `min` → `current_max` →
/// `max` → `ceiling`, with a last-resort grace count.
///
/// Stability: stable.
pub struct IterationBudget {
    min: usize,
    max: AtomicUsize,
    ceiling: usize,
    current_max: AtomicUsize,
    consumed: AtomicUsize,
    grace_remaining: AtomicUsize,
}

impl IterationBudget {
    /// Fully elastic budget: `min` guaranteed, `expand()` grows toward
    /// `max`, `expand_max()` grows `max` toward `ceiling`, `grace` last
    /// resort.
    pub fn new_elastic_with_ceiling(min: usize, max: usize, ceiling: usize, grace: usize) -> Self {
        let max = max.min(ceiling);
        let min = min.min(max);
        Self {
            min,
            max: AtomicUsize::new(max),
            ceiling,
            current_max: AtomicUsize::new(min),
            consumed: AtomicUsize::new(0),
            grace_remaining: AtomicUsize::new(grace),
        }
    }

    /// Elastic range with `ceiling == max`.
    pub fn new_elastic(min: usize, max: usize, grace: usize) -> Self {
        Self::new_elastic_with_ceiling(min, max, max, grace)
    }

    /// Fixed budget (min == max == ceiling), 1 grace call.
    pub fn new(max: usize) -> Self {
        Self::new_elastic_with_ceiling(max, max, max, 1)
    }

    /// Consume one iteration. `false` = current ceiling reached; the caller
    /// may `expand()`, `expand_max()`, or `try_grace()`.
    pub fn try_consume(&self) -> bool {
        loop {
            let current = self.consumed.load(Ordering::Acquire);
            let cap = self.current_max.load(Ordering::Acquire);
            if current >= cap {
                return false;
            }
            if self
                .consumed
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Raise the effective ceiling by `n` (capped at `max`). Returns `true`
    /// if the ceiling was raised.
    pub fn expand(&self, n: usize) -> bool {
        loop {
            let current_cap = self.current_max.load(Ordering::Acquire);
            let max = self.max.load(Ordering::Acquire);
            if current_cap >= max {
                return false;
            }
            let new_cap = (current_cap + n).min(max);
            if self
                .current_max
                .compare_exchange_weak(current_cap, new_cap, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Raise the elastic `max` itself by `n` (capped at `ceiling`).
    pub fn expand_max(&self, n: usize) -> bool {
        loop {
            let current_max = self.max.load(Ordering::Acquire);
            if current_max >= self.ceiling {
                return false;
            }
            let new_max = (current_max + n).min(self.ceiling);
            if self
                .max
                .compare_exchange_weak(current_max, new_max, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Consume a grace call. `false` = no grace remains.
    pub fn try_grace(&self) -> bool {
        loop {
            let remaining = self.grace_remaining.load(Ordering::Acquire);
            if remaining == 0 {
                return false;
            }
            if self
                .grace_remaining
                .compare_exchange_weak(
                    remaining,
                    remaining - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn consumed(&self) -> usize {
        self.consumed.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> usize {
        self.current_max
            .load(Ordering::Acquire)
            .saturating_sub(self.consumed.load(Ordering::Acquire))
    }

    /// Reset consumption to zero (new turn with a fresh budget).
    pub fn reset(&self) {
        self.consumed.store(0, Ordering::Release);
        self.current_max.store(self.min, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fixed_budget_consumes_until_exhausted() {
        let b = IterationBudget::new(3);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume());
        // Fixed budget never expands.
        assert!(!b.expand(1));
        // One grace call by default.
        assert!(b.try_grace());
        assert!(!b.try_grace());
        assert_eq!(b.consumed(), 3);
    }

    #[test]
    fn elastic_budget_expands_in_tiers() {
        let b = IterationBudget::new_elastic_with_ceiling(2, 4, 6, 1);
        // First two come from the floor.
        assert!(b.try_consume());
        assert!(b.try_consume());
        // Floor exhausted → expand first tier.
        assert!(!b.try_consume());
        assert!(b.expand(1));
        assert!(b.try_consume());
        assert!(b.expand(1));
        assert!(b.try_consume());
        // current_max == max now; expand refuses, expand_max raises.
        assert!(!b.expand(1));
        assert!(b.expand_max(2));
        assert!(b.expand(1));
        assert!(b.try_consume());
        assert!(b.expand(1));
        assert!(b.try_consume());
        // Ceiling reached (6 consumed) — grace saves one.
        assert!(!b.try_consume() && !b.expand(1) && !b.expand_max(1));
        assert!(b.try_grace());
    }

    #[test]
    fn reset_restores_floor() {
        let b = IterationBudget::new_elastic(2, 5, 1);
        b.try_consume();
        b.try_consume();
        b.expand(2);
        b.try_consume();
        b.reset();
        assert_eq!(b.consumed(), 0);
        assert_eq!(b.remaining(), 2);
    }

    #[test]
    fn concurrent_consumption_never_overshoots() {
        let b = Arc::new(IterationBudget::new(100));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let b = Arc::clone(&b);
                std::thread::spawn(move || {
                    let mut got = 0;
                    while b.try_consume() {
                        got += 1;
                    }
                    got
                })
            })
            .collect();
        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 100, "CAS loop must distribute exactly the budget");
    }
}
