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
//!   docs/compatibility.md).
//! - `Stability: unstable` — allowed to iterate; promotion to stable follows
//!   the 1.0 freeze process (design plan section 7.8).
//!
//! The `xtask stability` gate asserts that every public item carries one of
//! these tags; an untagged public item fails CI.

pub mod approval;
pub mod budget;
pub mod error;
pub mod guardrail;
/// 功能：共享媒体内容与校验。Stability: unstable.
pub mod media;
pub mod memory_provider;
pub mod message;
pub mod policy;
pub mod provider;
pub mod retry;
pub mod session;
pub mod tool_host;
/// 功能：导出中立单题问答值类型。Stability: unstable.
pub mod user_input;
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
pub use media::InlineMedia;
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
/// 功能：供应商中立的模型上下文检查点数据。
pub mod context;
pub use context::ModelContextCheckpoint;
pub use tool_host::{
    NullToolHost, RegistryToolHost, ToolCallOutcome, ToolExecutor, ToolHost, ToolHostValidator,
    ToolInvocationContext,
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
