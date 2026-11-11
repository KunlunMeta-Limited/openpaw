//! openpaw-engine — the agent loop.
//!
//! 职责：共享单轮推理、分阶段工具执行、审批、取消及可选压缩/预算。
//! 输入输出：原Session与Provider到流式事件及真实运行结果。
//! 依赖：core契约、llm与可选Hook装配。
//! 禁止职责：不内置Office协议，不默认加载外部插件或自动反思。
//! 压缩、fallback和预算已有实现，是否启用由CLI或Host装配决定。

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
