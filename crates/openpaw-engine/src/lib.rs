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

/// 职责：共享运行与装配支持。
/// 输入输出：配置、生命周期到有效运行结果。
/// 依赖：复用核心类型和现有实现。
/// 禁止职责：不处理Office协议或复制存储。
pub mod run;
pub use run::{RunContext, RunObserver, RunOutcome, RunStatus};
