//! SM-2 (SuperMemo 2) spaced repetition algorithm.
//!
//! Pure functions with no side effects. The caller is responsible for
//! persistence and wiring the algorithm into the event/recording pipeline.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::stats::MemoryEvent;

/// Maximum interval cap in days. Prevents SM-2 intervals from growing
/// unboundedly as the ease factor increases over long-running memories.
/// Without this cap, memories may never be reviewed again.
const MAX_INTERVAL_DAYS: u32 = 365;

/// Output of a single SM-2 calculation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sm2Result {
    pub repetition: u32,
    pub ease_factor: f64,
    pub interval_days: u32,
    pub next_review_at: DateTime<Utc>,
}

impl Sm2Result {
    /// Clamp all fields to sane bounds (defensive sanitization).
    pub fn sanitize(self) -> Self {
        Self {
            repetition: self.repetition,
            ease_factor: self.ease_factor.max(1.3),
            interval_days: self.interval_days.clamp(1, MAX_INTERVAL_DAYS),
            next_review_at: self.next_review_at.max(Utc::now()),
        }
    }
}

/// SM-2 core calculation — pure function, no side effects.
///
/// - `quality` is the recall quality in [0, 5]:
///   - 0–2: lapse (resets repetition)
///   - 3–5: successful recall (advances interval)
/// - `now` is the reference timestamp for computing `next_review_at`.
pub fn sm2_calculate(
    repetition: u32,
    ease_factor: f64,
    interval_days: u32,
    quality: u8,
    now: DateTime<Utc>,
) -> Sm2Result {
    let quality = quality.min(5);

    let (new_rep, new_interval, new_ease) = if quality >= 3 {
        // Successful recall
        let new_rep = repetition + 1;
        let new_interval = match new_rep {
            1 => 1u32,
            2 => 6u32,
            _ => {
                let raw = (interval_days as f64 * ease_factor).ceil() as u32;
                // Cap the interval to prevent unbounded growth.
                raw.min(MAX_INTERVAL_DAYS)
            }
        };
        let delta = 0.1 - (5 - quality) as f64 * (0.08 + (5 - quality) as f64 * 0.02);
        let new_ease = (ease_factor + delta).max(1.3);
        (new_rep, new_interval, new_ease)
    } else {
        // Lapse: reset repetition but keep ease_factor unchanged
        (0u32, 1u32, ease_factor)
    };

    Sm2Result {
        repetition: new_rep,
        ease_factor: new_ease,
        interval_days: new_interval,
        next_review_at: now + Duration::days(new_interval as i64),
    }
}

/// Map a [`MemoryEvent`] to an SM-2 recall-quality value.
///
/// | Event            | q  | Rationale                          |
/// |------------------|----|------------------------------------|
/// | `Accessed`       | 5  | Deep use = perfect recall          |
/// | `Referenced`     | 4  | Active recall                      |
/// | `Loaded`         | 3  | Passive recall (barely)            |
/// | `ReviewFailed`   | 2  | Nudge triggered but unused         |
/// | `ContextInjected`| 2  | Injected by focus (not self-recall)|
pub fn event_to_quality(event: MemoryEvent) -> u8 {
    match event {
        MemoryEvent::Accessed => 5,
        MemoryEvent::Referenced => 4,
        MemoryEvent::Loaded => 3,
        MemoryEvent::ReviewFailed => 2,
        MemoryEvent::ContextInjected => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 7, 0, 0, 0).unwrap()
    }

    #[test]
    fn sm2_first_success() {
        let result = sm2_calculate(0, 2.5, 0, 4, fixed_now());
        assert_eq!(result.repetition, 1);
        assert_eq!(result.interval_days, 1);
        assert!(result.ease_factor > 2.4);
        // next_review_at should be 1 day later
        assert_eq!(result.next_review_at, fixed_now() + Duration::days(1));
    }

    #[test]
    fn sm2_second_success() {
        let r1 = sm2_calculate(0, 2.5, 0, 4, fixed_now());
        let r2 = sm2_calculate(
            r1.repetition,
            r1.ease_factor,
            r1.interval_days,
            4,
            fixed_now(),
        );
        assert_eq!(r2.repetition, 2);
        assert_eq!(r2.interval_days, 6);
    }

    #[test]
    fn sm2_failure_resets() {
        let result = sm2_calculate(3, 2.5, 15, 2, fixed_now());
        assert_eq!(result.repetition, 0);
        assert_eq!(result.interval_days, 1);
        // ease_factor unchanged on failure
        assert!((result.ease_factor - 2.5).abs() < 0.01);
    }

    #[test]
    fn sm2_ease_floor() {
        // Repeated low-quality recalls should not drop ease below 1.3
        let mut ef = 2.5;
        for _ in 0..20 {
            let result = sm2_calculate(0, ef, 1, 0, fixed_now());
            ef = result.ease_factor;
        }
        assert!(ef >= 1.3);
    }

    #[test]
    fn sm2_quality_clamped() {
        // Quality > 5 should be clamped to 5
        let result = sm2_calculate(0, 2.5, 0, 10, fixed_now());
        assert_eq!(result.repetition, 1);
        assert_eq!(result.interval_days, 1);
    }

    #[test]
    fn sm2_interval_grows_with_repetition() {
        let now = fixed_now();
        let mut rep = 0u32;
        let mut ef = 2.5;
        let mut interval = 0u32;
        let mut intervals = Vec::new();
        for _ in 0..5 {
            let r = sm2_calculate(rep, ef, interval, 4, now);
            intervals.push(r.interval_days);
            rep = r.repetition;
            ef = r.ease_factor;
            interval = r.interval_days;
        }
        // Intervals should be monotonically non-decreasing
        for w in intervals.windows(2) {
            assert!(w[1] >= w[0], "interval did not grow: {:?}", intervals);
        }
    }

    #[test]
    fn event_quality_mapping() {
        assert_eq!(event_to_quality(MemoryEvent::Accessed), 5);
        assert_eq!(event_to_quality(MemoryEvent::Referenced), 4);
        assert_eq!(event_to_quality(MemoryEvent::Loaded), 3);
        assert_eq!(event_to_quality(MemoryEvent::ReviewFailed), 2);
        assert_eq!(event_to_quality(MemoryEvent::ContextInjected), 2);
    }

    #[test]
    fn sanitize_clamps() {
        let result = Sm2Result {
            repetition: 0,
            ease_factor: 0.5,
            interval_days: 0,
            next_review_at: fixed_now() - Duration::days(10),
        }
        .sanitize();
        assert!((result.ease_factor - 1.3).abs() < 0.01);
        assert_eq!(result.interval_days, 1);
        assert!(result.next_review_at >= fixed_now());
    }

    #[test]
    fn interval_capped_at_365_days() {
        // After many successful recalls with a high ease factor,
        // the interval should never exceed 365 days.
        let now = fixed_now();
        let mut rep = 0u32;
        let mut ef = 2.5;
        let mut interval = 0u32;

        for _ in 0..100 {
            let r = sm2_calculate(rep, ef, interval, 5, now);
            rep = r.repetition;
            ef = r.ease_factor;
            interval = r.interval_days;
        }

        assert!(interval <= 365, "interval {interval} exceeds 365-day cap");
    }

    #[test]
    fn sanitize_caps_interval() {
        let result = Sm2Result {
            repetition: 50,
            ease_factor: 3.0,
            interval_days: 999,
            next_review_at: fixed_now(),
        }
        .sanitize();
        assert_eq!(result.interval_days, 365);
    }
}
