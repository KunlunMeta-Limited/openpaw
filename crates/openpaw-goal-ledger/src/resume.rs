//! Single-step resume — pure decision functions plus ledger bookkeeping.
//!
//! The resume flow is: read the last run summary from the event ledger,
//! then decide how (or whether) to resume. This crate only produces the
//! decision and appends ledger events; it never drives an agent loop —
//! execution belongs to the CLI's `goal resume` command. There is no
//! polling daemon and no guard loop: resume happens exactly once per
//! explicit user invocation (hard red line: no auto-resume).

use crate::error::Result;
use crate::ledger::{GoalEvent, GoalEventLedger};
use crate::models::{GoalFrontmatter, GoalStatus, RunSummary};

/// Ledger event type carrying a [`RunSummary`] payload.
pub const RUN_SUMMARY_EVENT: &str = "run_summary";

/// Ledger event type recording that a resume was granted.
pub const RESUMED_EVENT: &str = "resumed";

/// Outcome of a resume evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeDecision {
    /// Execute one bounded turn, continuing from the recorded next action.
    /// An empty `next_action` means the goal has no prior run summary and
    /// starts (or restarts) from its objective.
    Run {
        /// Suggested next action taken from the newest run summary.
        next_action: String,
    },
    /// An unresolved user gate blocks execution; surface the question and
    /// wait for the user's answer.
    WaitForUser {
        /// The pending question from the first unresolved gate.
        question: String,
    },
    /// The goal already reached a terminal status; nothing to resume.
    AlreadyFinished {
        /// The terminal status that ended the goal.
        status: GoalStatus,
    },
}

/// Build the ledger event for a finished turn ("run_summary").
///
/// The payload is the serialized [`RunSummary`], whose `next_action` field
/// is the resume seed.
pub fn run_summary_event(goal_id: &str, summary: &RunSummary) -> Result<GoalEvent> {
    Ok(GoalEvent::new(
        RUN_SUMMARY_EVENT,
        goal_id,
        serde_json::to_value(summary)?,
    ))
}

/// Append a finished turn's summary to the ledger (one fsync per write).
pub fn record_run_summary(
    ledger: &GoalEventLedger,
    goal_id: &str,
    summary: &RunSummary,
) -> Result<()> {
    ledger.append(&run_summary_event(goal_id, summary)?)
}

/// Append a "resumed" trail event (audit bookkeeping for the passbook).
pub fn record_resumed(ledger: &GoalEventLedger, goal_id: &str, next_action: &str) -> Result<()> {
    ledger.append(&GoalEvent::new(
        RESUMED_EVENT,
        goal_id,
        serde_json::json!({ "next_action": next_action }),
    ))
}

/// The next action recorded by the newest `run_summary` event, if any.
///
/// Skips malformed payloads so a corrupted entry cannot poison the lookup.
pub fn last_next_action(events: &[GoalEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .filter(|e| e.event_type == RUN_SUMMARY_EVENT)
        .find_map(|e| serde_json::from_value::<RunSummary>(e.payload.clone()).ok())
        .map(|s| s.next_action)
}

/// Pure resume decision: whether and how this goal may resume right now.
///
/// Precedence:
/// 1. terminal status wins — nothing resumes;
/// 2. an unresolved user gate wins — surface its question;
/// 3. otherwise run, seeded by the newest recorded next action.
pub fn decide_resume(fm: &GoalFrontmatter, events: &[GoalEvent]) -> ResumeDecision {
    if fm.status.is_terminal() {
        return ResumeDecision::AlreadyFinished { status: fm.status };
    }
    if let Some(gate) = fm.user_gates.iter().find(|g| !g.resolved) {
        return ResumeDecision::WaitForUser {
            question: gate.question.clone(),
        };
    }
    ResumeDecision::Run {
        next_action: last_next_action(events).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RunOutcome;

    fn summary(next_action: &str) -> RunSummary {
        RunSummary {
            session_id: "session_1".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            turns: 1,
            tokens_used: 100,
            outcome: RunOutcome::Progress,
            summary: "made progress".to_string(),
            next_action: next_action.to_string(),
        }
    }

    #[test]
    fn last_next_action_returns_newest() {
        let events = vec![
            run_summary_event("goal_cafe001", &summary("first")).unwrap(),
            run_summary_event("goal_cafe001", &summary("second")).unwrap(),
        ];
        assert_eq!(last_next_action(&events).as_deref(), Some("second"));
    }

    #[test]
    fn last_next_action_ignores_other_events_and_bad_payloads() {
        let mut events = vec![
            GoalEvent::new("created", "goal_cafe001", serde_json::json!({})),
            run_summary_event("goal_cafe001", &summary("real")).unwrap(),
            GoalEvent::new(
                RUN_SUMMARY_EVENT,
                "goal_cafe001",
                serde_json::json!({"broken": true}),
            ),
        ];
        // Newest run_summary has a malformed payload → older one wins.
        events.reverse();
        assert_eq!(last_next_action(&events).as_deref(), Some("real"));
    }

    #[test]
    fn decide_resume_run_with_recorded_action() {
        let fm = GoalFrontmatter::new("write the guide");
        let events = vec![run_summary_event("goal_cafe001", &summary("polish the draft")).unwrap()];
        assert_eq!(
            decide_resume(&fm, &events),
            ResumeDecision::Run {
                next_action: "polish the draft".to_string()
            }
        );
    }

    #[test]
    fn decide_resume_wait_for_user_gate() {
        let mut fm = GoalFrontmatter::new("needs a decision");
        fm.user_gates
            .push(crate::models::UserGate::new("Which database?"));
        let events = vec![run_summary_event("goal_cafe001", &summary("after the answer")).unwrap()];
        assert_eq!(
            decide_resume(&fm, &events),
            ResumeDecision::WaitForUser {
                question: "Which database?".to_string()
            }
        );
    }

    #[test]
    fn decide_resume_terminal_status_wins() {
        let mut fm = GoalFrontmatter::new("done deal");
        fm.status = GoalStatus::Completed;
        let events = vec![run_summary_event("goal_cafe001", &summary("anything")).unwrap()];
        assert_eq!(
            decide_resume(&fm, &events),
            ResumeDecision::AlreadyFinished {
                status: GoalStatus::Completed
            }
        );
    }

    #[test]
    fn decide_resume_no_history_starts_empty() {
        let fm = GoalFrontmatter::new("fresh goal");
        assert_eq!(
            decide_resume(&fm, &[]),
            ResumeDecision::Run {
                next_action: String::new()
            }
        );
    }

    #[test]
    fn record_run_summary_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = GoalEventLedger::new(tmp.path());
        record_run_summary(&ledger, "goal_cafe001", &summary("next step")).unwrap();
        record_resumed(&ledger, "goal_cafe001", "next step").unwrap();

        let events = ledger.query("goal_cafe001").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, RUN_SUMMARY_EVENT);
        assert_eq!(events[1].event_type, RESUMED_EVENT);
        assert_eq!(
            events[1].payload["next_action"], "next step",
            "resumed trail must carry the action it resumed from"
        );
    }
}
