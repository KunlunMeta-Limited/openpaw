//! openpaw-goal-ledger — goal ledger ("passbook").
//!
//! **Landed** (M4). Same-source subset (registry S3) of the upstream goal
//! crate: profile storage, append-only JSONL ledger with per-write fsync,
//! `[GOAL_*]` marker parsing, and single-step resume decisions.
//!
//! # Modules
//!
//! - [`models`] — [`GoalFrontmatter`], [`GoalStatus`], [`RunSummary`],
//!   [`UserGate`], and the
//!   `[GOAL_COMPLETE]` / `[GOAL_FAILED]` / `[GOAL_PROGRESS:x]` /
//!   `[GOAL_BLOCKED_ON_USER:]` marker parsers (S3 same-source subset).
//! - [`ledger`] — [`GoalEventLedger`]: append-only JSONL, one fsync per
//!   write, goal-id validation blocking path injection (same-source port).
//! - [`store`] — [`FsGoalStore`]: `.md` + YAML frontmatter atomic writes
//!   and flat directory scan via the openpaw-store frontmatter codec.
//! - [`resume`] — pure single-step resume decision from the ledger's last
//!   run summary, plus ledger bookkeeping; execution stays with the CLI's
//!   `goal resume` command.
//! - [`error`] — [`GoalError`].
//!
//! # Hard red lines
//!
//! The four autonomous-driving facilities are deliberately **not** ported
//! from the upstream goal crate — none of them may enter this crate:
//!
//! 1. **quota** — turn-budget accounting, rolling-window reset, and
//!    throttling logic are excluded; only the passive [`GoalQuota`] data
//!    fields survive for document schema compatibility;
//! 2. **stall** — stall detection and re-planning injection are excluded;
//!    the upstream `StallInfo` type is not ported;
//! 3. **lock** — the cross-process run-lock facility is excluded;
//!    [`FsGoalStore`] keeps only an in-process write mutex;
//! 4. **auto-resume** — there is no polling daemon or guard loop; resume is
//!    a single decision, produced only on an explicit user invocation.
//!
//! Likewise, [`InteractionMode`] is reduced to its [`InteractionMode::UserGate`]
//! variant; the other ten autonomous-driving modes are not ported.

pub mod error;
pub mod ledger;
pub mod models;
pub mod resume;
pub mod store;

// ── Public exports ──

pub use error::{GoalError, Result};
pub use ledger::{GoalEvent, GoalEventLedger};
pub use models::{
    parse_user_gate_question, Confidence, ExecutionHint, GoalFrontmatter, GoalQuota, GoalStatus,
    InteractionMode, Priority, RunOutcome, RunSummary, Source, UserGate,
};
pub use resume::{
    decide_resume, last_next_action, record_resumed, record_run_summary, run_summary_event,
    ResumeDecision, RESUMED_EVENT, RUN_SUMMARY_EVENT,
};
pub use store::{FsGoalStore, GoalStore};
