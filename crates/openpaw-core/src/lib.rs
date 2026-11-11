//! 职责：定义共享模型、消息、工具、审批和策略抽象。
//! 输入输出：运行层消费的类型与特征，不执行模型或工具。
//! 依赖：基础数据类型；不依赖UI、传输或存储实现。
//! 禁止职责：不装配宿主、网络服务或持久化后端。
//!
//! openpaw-core: foundation traits and types.
//!
//! This crate declares the project's central abstractions: the LLM provider
//! contract, message/session types, and the evolution policy traits. All
//! other crates implement or consume these traits. It must not depend on any
//! UI, transport, or storage crate (dependency layer L0).
//!
//! # Stability levels
//!
//! Every public item in this crate carries a `Stability:` doc tag:
//!
//! - `Stability: stable` — frozen at 1.0. Breaking changes require an RFC
//!   plus a deprecation window of two minor releases (see
//!   the root README, file formats and compatibility).
//! - `Stability: unstable` — allowed to iterate; promotion to stable follows
//!   an explicit compatibility review.
//!
//! The `xtask stability` gate asserts that every public item carries one of
//! these tags; an untagged public item fails CI.

pub mod approval;
pub mod budget;
pub mod error;
pub mod guardrail;
/// 职责：不可变的最终工具调用绑定。
/// 输入输出：审批调用到执行观察上下文。
/// 依赖：原core契约与取消token。
/// 禁止职责：不包含宿主调度和协议路由。
/// Stability: unstable.
pub mod invocation;
pub mod memory_provider;
pub mod message;
pub mod policy;
pub mod provider;
pub mod retry;
pub mod session;
pub mod tool_host;
pub mod workspace;

pub use approval::{
    ApprovalManager, ApprovalRequest, ApprovalResponse, AutoApproveManager, AutoDenyManager,
};
pub use budget::IterationBudget;
pub use error::{Error, Result};
pub use guardrail::{
    AllowlistGuardrail, CheckContext, GuardrailChain, GuardrailDecision, PermissiveGuardrail,
    ToolGuardrail,
};
pub use invocation::{ToolCallBinding, ToolCallContext, ToolExecutionObserver};
pub use memory_provider::{
    InMemoryProvider, MemoryEntry, MemoryProvider, MemorySearchQuery, MemorySearchResult,
};
pub use message::{CompactMetadata, CompactType, ContentBlock, Message, Role};
pub use policy::{
    CandidateKind, Confidence, EvolutionPolicy, GoalLedgerView, GoalStatusView, InboundSink,
    KnowledgeCandidate, LongHorizonDecision, LongHorizonPolicy, NullPolicy, PlatformAdapter,
    RunSummaryView, Transcript, TurnContext,
};
pub use provider::{
    is_thinking_model, Capabilities, CompletionRequest, CompletionResponse, DescriptionFn,
    LlmProvider, NullProvider, StopReason, StreamEvent, SystemPromptBlocks, TokenCounter,
    ToolAnnotations, ToolCategory, ToolConcurrency, ToolInterruptBehavior, ToolSpec, Usage,
};
pub use retry::{classify_api_error, ApiErrorClass, RetryPolicy};
pub use session::{InjectionRecord, Session, SessionEvent, SessionMeta};
pub use tool_host::{
    NullToolHost, RegistryToolHost, ToolCallOutcome, ToolExecutor, ToolHost, ToolHostValidator,
};
pub use workspace::{init_workspace_dirs, openpaw_home, validate_workspace_name};

/// The non-negotiable floor: every evolution policy must keep human
/// alignment enabled. This re-export exists so CI (Q9) can assert the
/// default with a named symbol instead of a stringly-typed path.
///
/// Stability: stable.
pub const REQUIRES_HUMAN_ALIGNMENT_DEFAULT: bool = true;

#[cfg(test)]
mod human_alignment_tests {
    use super::*;

    /// Q9: `NullPolicy::requires_human_alignment` must default to true.
    /// This is the physical evidence of the hard boundary "knowledge writes
    /// require user approval". Any future policy that overrides this to
    /// false fails this suite.
    #[test]
    fn null_policy_requires_human_alignment_defaults_true() {
        let policy = NullPolicy;
        assert!(policy.requires_human_alignment());
        assert_eq!(
            policy.requires_human_alignment(),
            REQUIRES_HUMAN_ALIGNMENT_DEFAULT
        );
    }
}
