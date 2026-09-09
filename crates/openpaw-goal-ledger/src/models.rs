//! Data models — same-source subset (registry S3): `GoalFrontmatter`,
//! `GoalStatus`, `RunSummary`, and the `[GOAL_*]` marker parsers.
//!
//! Ported from the upstream goal crate's `models.rs` with the following
//! subset decisions:
//! - `InteractionMode` is reduced to its `UserGate` variant only; the other
//!   ten autonomous-driving modes (quota throttling, autonomous re-planning,
//!   successor re-plan, monitor skip, ...) are NOT ported (hard red line).
//! - `GoalQuota` and `ExecutionHint` survive as passive data structures so
//!   goal profiles stay byte-compatible with upstream documents; all quota
//!   *logic* (budget accounting, window reset, throttling) is not ported.
//! - `StallInfo` (stall detection output) is not ported at all.
//! - The knowledge-domain enums (`Source` / `Confidence` / `Priority`) are
//!   re-declared locally with the same serialization shape as their
//!   upstream counterparts, because this crate deliberately does not depend
//!   on the memory crate (layering: goal-ledger -> store only).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Knowledge-domain enums (schema-compatible local declarations) ──────

/// Where a goal profile came from. Serialization shape matches the memory
/// crate's `Source` (lowercase), kept local to avoid a layering violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Distilled by the reflection pipeline.
    Reflection,
    /// Written or edited by a human.
    User,
    /// Imported from another project / agent.
    Imported,
    /// Written by the agent itself.
    Agent,
}

/// Confidence level. Serialization shape matches the memory crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Goal priority. Serialization shape matches the upstream task-domain enum
/// (`snake_case`; single words render identically).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

// ── ExecutionHint ───────────────────────────────────────────

/// Execution strategy hint. Kept as a passive profile field for schema
/// compatibility; this crate never drives an agent loop itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHint {
    /// Execute directly through the agent loop's run-turn entry point.
    #[default]
    Direct,
    /// Execute through a generated workflow plan.
    Workflow,
    /// Defer execution onto a task board.
    TaskBoard,
    /// Record only, do not execute (e.g. long-term vision).
    Record,
}

// ── GoalStatus ──────────────────────────────────────────────

/// Goal lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Created, waiting for the first run.
    #[default]
    Created,
    /// Active, ready to execute.
    Active,
    /// Paused manually by the user.
    Paused,
    /// Completed.
    Completed,
    /// Failed (`[GOAL_FAILED]` judged logically unreachable).
    Failed,
    /// Cancelled.
    Cancelled,
    /// Archived (kept for history, no longer active).
    Archived,
}

impl GoalStatus {
    /// Whether the goal may currently execute.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Whether the goal has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Archived
        )
    }
}

// ── GoalQuota ───────────────────────────────────────────────

/// Goal quota — passive data fields only, kept so goal profiles deserialize
/// identically to upstream documents. The quota *manager* (turn budgeting,
/// rolling-window reset, throttling decisions) is deliberately NOT ported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalQuota {
    /// Duty ratio (0.0 ~ 1.0); 1.0 = fully available.
    #[serde(default = "default_quota_compute")]
    pub compute: f64,

    /// Time window in hours, default 24.
    /// A `window_hours` of 0 would make the window a zero-length instant
    /// (always expired → quota bypass on the upstream side); deserialization
    /// clamps it back to the 24h default.
    #[serde(
        default = "default_quota_window_hours",
        deserialize_with = "clamp_window_hours"
    )]
    pub window_hours: u32,

    /// Allowed turns per window.
    #[serde(default = "default_quota_allowed_turns")]
    pub allowed_turns: u32,

    /// Turns already spent.
    #[serde(default)]
    pub spent_turns: u32,

    /// Window start (None = timer not started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,

    /// Quota state string ("eligible" / "throttled" / "blocked").
    #[serde(default = "default_quota_state")]
    pub state: String,
}

impl Default for GoalQuota {
    fn default() -> Self {
        Self {
            compute: default_quota_compute(),
            window_hours: default_quota_window_hours(),
            allowed_turns: default_quota_allowed_turns(),
            spent_turns: 0,
            window_start: None,
            state: default_quota_state(),
        }
    }
}

fn default_quota_compute() -> f64 {
    1.0
}

fn default_quota_window_hours() -> u32 {
    24
}

/// Clamp a deserialized `window_hours` of 0 back to the 24h default — a
/// zero-length window would make quota checks always report expiry,
/// effectively bypassing the budget.
fn clamp_window_hours<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(deserializer)?;
    Ok(if v == 0 {
        default_quota_window_hours()
    } else {
        v
    })
}

fn default_quota_allowed_turns() -> u32 {
    100
}

fn default_quota_state() -> String {
    "eligible".to_string()
}

// ── GoalFrontmatter ─────────────────────────────────────────

/// Core persisted goal structure — stored as YAML frontmatter so state
/// survives across sessions. Flat fields + a catch-all `extra` mapping, so
/// documents written by the upstream goal crate (or with additional keys)
/// round-trip without loss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalFrontmatter {
    // ── Identity ──────────────────────────────────────────
    /// Stable identifier, format `goal_<uuid_simple>`.
    pub id: String,

    /// Creation time.
    pub created: DateTime<Utc>,

    /// Origin of the goal.
    pub source: Source,

    /// Confidence in the goal.
    pub confidence: Confidence,

    // ── Content ───────────────────────────────────────────
    /// Goal description.
    pub objective: String,

    /// Measurable success criteria.
    #[serde(default)]
    pub success_criteria: String,

    /// Rationale / reasoning chain.
    #[serde(default)]
    pub rationale: String,

    // ── State & execution ─────────────────────────────────
    /// Current lifecycle state.
    #[serde(default)]
    pub status: GoalStatus,

    /// Priority.
    #[serde(default)]
    pub priority: Priority,

    /// Execution strategy hint (passive profile field).
    #[serde(default)]
    pub execution_hint: ExecutionHint,

    // ── Context ───────────────────────────────────────────
    /// Non-goals (what is out of scope).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,

    /// Current belief (the agent's understanding of the goal).
    #[serde(default)]
    pub current_belief: String,

    /// Suggested next action.
    #[serde(default)]
    pub next_action: String,

    /// Authoritative references (memories / skills backing the goal).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authority_sources: Vec<String>,

    // ── Associations ──────────────────────────────────────
    /// Associated task ids (meaningful only on the execution side).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,

    /// Associated session ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,

    // ── Quota (passive data, see GoalQuota) ───────────────
    /// Quota configuration and current consumption.
    #[serde(default)]
    pub quota: GoalQuota,

    // ── User gates ────────────────────────────────────────
    /// User gates (items awaiting a user decision).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_gates: Vec<UserGate>,

    // ── Time tracking ─────────────────────────────────────
    /// Last run time (None = never ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,

    /// Completion time (None = not completed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,

    // ── Extension ─────────────────────────────────────────
    /// Fallback storage for unknown frontmatter keys.
    #[serde(flatten)]
    pub extra: serde_yml::Mapping,
}

impl GoalFrontmatter {
    /// Create a new goal with an auto-assigned `goal_<uuid_simple>` id.
    ///
    /// Defaults:
    /// - `status` = `GoalStatus::Created`
    /// - `priority` = `Priority::Low`
    /// - `execution_hint` = `ExecutionHint::Direct`
    /// - `quota` = `GoalQuota::default()`
    pub fn new(objective: &str) -> Self {
        Self {
            id: format!("goal_{}", uuid::Uuid::new_v4().simple()),
            created: Utc::now(),
            source: Source::Reflection,
            confidence: Confidence::Medium,

            objective: objective.to_string(),
            success_criteria: String::new(),
            rationale: String::new(),

            status: GoalStatus::Created,
            priority: Priority::Low,
            execution_hint: ExecutionHint::Direct,

            scope: Vec::new(),
            current_belief: String::new(),
            next_action: String::new(),
            authority_sources: Vec::new(),

            task_ids: Vec::new(),
            session_ids: Vec::new(),

            quota: GoalQuota::default(),

            user_gates: Vec::new(),

            last_run_at: None,
            completed_at: None,

            extra: serde_yml::Mapping::new(),
        }
    }

    /// Create a new goal with an explicit source and confidence.
    pub fn with_source_confidence(objective: &str, source: Source, confidence: Confidence) -> Self {
        let mut fm = Self::new(objective);
        fm.source = source;
        fm.confidence = confidence;
        fm
    }
}

// ── RunOutcome ──────────────────────────────────────────────

/// Classification of a single run's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Made progress (partial work completed).
    Progress,
    /// No progress (executed but produced nothing effective).
    NoProgress,
    /// Completed (goal achieved).
    Completed,
    /// Failed (execution errored).
    Failed,
    /// Blocked (a dependency is unmet, waiting).
    Blocked,
}

impl RunOutcome {
    /// Parse the run outcome and progress value from assistant reply text.
    ///
    /// Marker contract (written into the goal system prompt):
    /// - `[GOAL_COMPLETE]` — goal achieved (progress = 1.0)
    /// - `[GOAL_FAILED]` — goal failed (progress = 0.0)
    /// - `[GOAL_PROGRESS:x.x]` — this turn's progress (0.0–1.0; values > 1
    ///   are interpreted as percentages)
    /// - no marker — treated as no progress this turn
    pub fn parse_from_summary(summary: &str) -> (RunOutcome, Option<f64>) {
        if summary.contains("[GOAL_COMPLETE]") {
            return (RunOutcome::Completed, Some(1.0));
        }
        if summary.contains("[GOAL_FAILED]") {
            return (RunOutcome::Failed, Some(0.0));
        }
        if let Some(p) = parse_progress_marker(summary) {
            return (RunOutcome::Progress, Some(p));
        }
        (RunOutcome::NoProgress, None)
    }
}

/// Parse the progress value inside a `[GOAL_PROGRESS:x.x]` marker.
fn parse_progress_marker(summary: &str) -> Option<f64> {
    const MARKER: &str = "[GOAL_PROGRESS:";
    let start = summary.find(MARKER)? + MARKER.len();
    let rest = &summary[start..];
    let end = rest.find(']')?;
    let raw: f64 = rest[..end].trim().parse().ok()?;
    if !raw.is_finite() {
        return None;
    }
    let value = if raw > 1.0 { raw / 100.0 } else { raw };
    Some(value.clamp(0.0, 1.0))
}

// ── RunSummary ──────────────────────────────────────────────

/// Run history summary — recorded after each run turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    /// Associated session id.
    pub session_id: String,
    /// Run start time.
    pub started_at: DateTime<Utc>,
    /// Run end time.
    pub ended_at: DateTime<Utc>,
    /// Turns consumed.
    pub turns: u32,
    /// Tokens consumed.
    pub tokens_used: u64,
    /// Run outcome.
    pub outcome: RunOutcome,
    /// Natural-language summary of the run.
    #[serde(default)]
    pub summary: String,
    /// Suggested next action (the resume seed).
    #[serde(default)]
    pub next_action: String,
}

// ── UserGate ────────────────────────────────────────────────

/// User gate — a concrete item awaiting a user decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGate {
    /// Gate identifier.
    pub id: String,
    /// Question posed to the user.
    pub question: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Whether it has been resolved.
    #[serde(default)]
    pub resolved: bool,
    /// Resolution (the user's reply); None = unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Associated todo id, if any (meaningful only on the execution side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_id: Option<String>,
}

impl UserGate {
    /// Create an unresolved user gate.
    pub fn new(question: &str) -> Self {
        Self {
            id: format!("gate_{}", uuid::Uuid::new_v4().simple()),
            question: question.to_string(),
            created_at: Utc::now(),
            resolved: false,
            resolution: None,
            todo_id: None,
        }
    }
}

/// Parse the `[GOAL_BLOCKED_ON_USER:question]` marker from assistant text.
///
/// When goal execution is blocked on a user decision, the model emits this
/// marker in its reply; the caller creates a `UserGate` from the question
/// and pauses execution until the user answers.
pub fn parse_user_gate_question(summary: &str) -> Option<String> {
    const MARKER: &str = "[GOAL_BLOCKED_ON_USER:";
    let start = summary.find(MARKER)? + MARKER.len();
    let rest = &summary[start..];
    let end = rest.find(']')?;
    let question = rest[..end].trim();
    if question.is_empty() {
        return None;
    }
    Some(question.to_string())
}

// ── InteractionMode ─────────────────────────────────────────

/// Interaction mode — computed per turn to steer execution behaviour.
///
/// Subset decision: the upstream enum has eleven variants spanning the full
/// autonomous-driving stack. Only `UserGate` is ported; the other ten
/// variants (bounded delivery, fallback execution, re-planning, monitor
/// skip, quota throttling, ...) belong to the autonomous-driving facilities
/// that are deliberately excluded from this crate (hard red line: no quota
/// / stall / lock / auto-resume intelligence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionMode {
    /// Waiting for a user decision (`[GOAL_BLOCKED_ON_USER:]` path).
    UserGate,
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_serde_roundtrip() {
        let status = GoalStatus::Active;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"active\"");
        let de: GoalStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(de, status);
    }

    #[test]
    fn goal_frontmatter_new() {
        let fm = GoalFrontmatter::new("finish the unit tests");
        assert!(fm.id.starts_with("goal_"));
        assert_eq!(fm.objective, "finish the unit tests");
        assert_eq!(fm.status, GoalStatus::Created);
        assert_eq!(fm.quota.spent_turns, 0);
        assert_eq!(fm.quota.state, "eligible");
    }

    #[test]
    fn window_hours_zero_clamped_to_default() {
        let yaml = concat!(
            "id: goal_test\n",
            "created: 2026-08-29T00:00:00Z\n",
            "source: user\n",
            "confidence: high\n",
            "objective: test\n",
            "quota:\n",
            "  window_hours: 0\n",
        );
        let fm: GoalFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(
            fm.quota.window_hours, 24,
            "window_hours: 0 must be clamped to the 24h default"
        );
    }

    #[test]
    fn window_hours_explicit_value_preserved() {
        let yaml = concat!(
            "id: goal_test\n",
            "created: 2026-08-29T00:00:00Z\n",
            "source: user\n",
            "confidence: high\n",
            "objective: test\n",
            "quota:\n",
            "  window_hours: 6\n",
        );
        let fm: GoalFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.quota.window_hours, 6, "non-zero values must be kept");
    }

    #[test]
    fn goal_frontmatter_serde_roundtrip() {
        let fm = GoalFrontmatter::new("serialization test");
        let yaml = serde_yml::to_string(&fm).unwrap();
        let de: GoalFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(de.id, fm.id);
        assert_eq!(de.objective, fm.objective);
        assert_eq!(de.status, fm.status);
    }

    #[test]
    fn interaction_mode_user_gate_only() {
        // Subset check: only UserGate survives the port.
        let json = serde_json::to_string(&InteractionMode::UserGate).unwrap();
        assert_eq!(json, "\"user_gate\"");
    }

    #[test]
    fn run_outcome_serde() {
        let outcome = RunOutcome::Completed;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, "\"completed\"");
        let de: RunOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(de, outcome);
    }

    #[test]
    fn parse_summary_complete_marker() {
        let (outcome, progress) = RunOutcome::parse_from_summary("all done.\n[GOAL_COMPLETE]");
        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(progress, Some(1.0));
    }

    #[test]
    fn parse_summary_failed_marker() {
        let (outcome, progress) =
            RunOutcome::parse_from_summary("cannot continue. [GOAL_FAILED] missing dependency");
        assert_eq!(outcome, RunOutcome::Failed);
        assert_eq!(progress, Some(0.0));
    }

    #[test]
    fn parse_summary_progress_marker() {
        let (outcome, progress) =
            RunOutcome::parse_from_summary("first step done [GOAL_PROGRESS:0.4], continuing.");
        assert_eq!(outcome, RunOutcome::Progress);
        assert_eq!(progress, Some(0.4));
    }

    #[test]
    fn parse_summary_progress_percent() {
        // Values > 1 are interpreted as percentages.
        let (outcome, progress) =
            RunOutcome::parse_from_summary("[GOAL_PROGRESS:60] sixty percent");
        assert_eq!(outcome, RunOutcome::Progress);
        assert_eq!(progress, Some(0.6));
    }

    #[test]
    fn parse_summary_progress_clamped() {
        let (_, progress) = RunOutcome::parse_from_summary("[GOAL_PROGRESS:150]");
        assert_eq!(progress, Some(1.0));
        let (_, progress) = RunOutcome::parse_from_summary("[GOAL_PROGRESS:-5]");
        assert_eq!(progress, Some(0.0));
    }

    #[test]
    fn parse_summary_no_marker_is_no_progress() {
        let (outcome, progress) = RunOutcome::parse_from_summary("I checked the file contents.");
        assert_eq!(outcome, RunOutcome::NoProgress);
        assert_eq!(progress, None);
    }

    #[test]
    fn parse_summary_complete_takes_precedence() {
        let (outcome, progress) =
            RunOutcome::parse_from_summary("[GOAL_PROGRESS:0.9]\n[GOAL_COMPLETE]");
        assert_eq!(outcome, RunOutcome::Completed);
        assert_eq!(progress, Some(1.0));
    }

    #[test]
    fn parse_user_gate_marker() {
        let q = parse_user_gate_question("need a decision [GOAL_BLOCKED_ON_USER: Which database?]");
        assert_eq!(q.as_deref(), Some("Which database?"));

        // Empty question → None
        assert_eq!(parse_user_gate_question("[GOAL_BLOCKED_ON_USER:]"), None);
        // No marker → None
        assert_eq!(parse_user_gate_question("a plain reply"), None);
        // Unclosed marker → None
        assert_eq!(
            parse_user_gate_question("[GOAL_BLOCKED_ON_USER: unclosed"),
            None
        );
    }

    #[test]
    fn user_gate_serde_roundtrip() {
        let gate = UserGate::new("test question?");
        let json = serde_json::to_string(&gate).unwrap();
        let de: UserGate = serde_json::from_str(&json).unwrap();
        assert_eq!(de.question, "test question?");
        assert!(!de.resolved);
        assert!(de.resolution.is_none());
    }

    #[test]
    fn goal_frontmatter_user_gates_default() {
        // Legacy format (no user_gates field) deserializes to an empty list.
        let fm = GoalFrontmatter::new("compat test");
        let yaml = serde_yml::to_string(&fm).unwrap();
        let de: GoalFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert!(de.user_gates.is_empty());
    }

    #[test]
    fn upstream_frontmatter_extra_keys_land_in_extra() {
        // An upstream-style profile carrying keys this subset does not
        // declare must still deserialize; unknown keys land in `extra`.
        let yaml = concat!(
            "id: goal_test\n",
            "created: 2026-08-29T00:00:00Z\n",
            "source: user\n",
            "confidence: high\n",
            "objective: test\n",
            "custom_future_field: 42\n",
        );
        let fm: GoalFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.objective, "test");
        assert!(fm.extra.contains_key("custom_future_field"));
    }
}
