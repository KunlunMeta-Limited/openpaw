//! openpaw-engine — the agent loop.
//!
//! E1 scope (this revision): `run_turn` skeleton (cancel check → budget
//! consume → compaction hook (no-op until E2) → streaming call → tool
//! execution → backfill), the three-path tool dispatcher, the phased
//! three-stage semantics (Deny → error / Allow → parallel / Confirm →
//! serial approval), and `stream_with_retry` (classified retry with
//! Retry-After). Compaction, fallback provider, and token budgeting land
//! with E2; checkpoint/nudge/stop-hook are stretch items (deferred in
//! order).
//!
//! Skeleton outline and the E1/E2 split line: `docs/spikes/S1-agent-loop.md`.

pub mod agent_loop;
pub mod compaction;
pub mod events;
pub mod fallback;
pub mod token_budget;

pub use agent_loop::AgentLoop;
pub use compaction::{CompactOptions, CompactTrigger};
pub use events::{AgentEvent, ClassifierStatus};
pub use fallback::FallbackCompressProvider;
pub use token_budget::{TokenBudgetConfig, TokenBudgetTracker};
