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
/// 功能：授权原件读取与有界模型上下文；不依赖Store或Host。
pub mod context;
pub mod events;
pub mod fallback;
pub mod token_budget;
/// 功能：中立用户问答协调及可信执行暂停，不依赖具体展示入口。
pub mod user_input;

pub use agent_loop::AgentLoop;
pub use compaction::{CompactOptions, CompactTrigger};
pub use events::{AgentEvent, ClassifierStatus};
pub use fallback::FallbackCompressProvider;
pub use token_budget::{TokenBudgetConfig, TokenBudgetTracker};
/// 功能：导出共享 approval 能力。
/// 参数：无。
/// 返回：模块接口。
/// 错误：由模块接口说明。
/// 状态：无独立状态。
/// 并发与取消：由模块接口说明。
/// 权限：保持既有边界。
/// 关联：CHG-2026-0023。
pub mod approval;

/// 功能：提供共享 scheduler 能力；边界及错误见模块说明。
pub mod scheduler;

/// 功能：提供共享 turn_execution 能力；边界及错误见模块说明。
pub mod turn_execution;
