//! Evolution policy traits — the OpenPaw-side new abstractions.
//!
//! These traits are the stable seam between the agent loop and the
//! reflection/goal subsystems. Upstream-side implementations align to
//! these signatures (same-source contract S4/S5, see the registry doc).
//!
//! Layering rule: this module must not reference engine, cli, or any
//! concrete storage/LLM implementation. LLM calls go through an injected
//! `dyn LlmProvider`; knowledge writes go through policy-implemented
//! persisters assembled at the CLI layer.
//!
//! Stability: stable.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::session::SessionId;
use crate::Result;

/// Context for one conversation turn, handed to policies.
///
/// Stability: stable.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub session_id: SessionId,
    pub turn_index: u64,
    pub error_count: u32,
    pub tool_calls_this_turn: u32,
    /// Tail of the current transcript (bounded by the caller).
    pub transcript_tail: Vec<Message>,
}

/// A conversation transcript to reflect on.
///
/// Stability: stable.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub messages: Vec<Message>,
}

/// Kind of knowledge a reflection produced.
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// `MemoryType` is carried as its string form to avoid a dependency on
    /// the memory crate from L0. The memory crate owns the enum mapping.
    Memory(String),
    Skill,
    ThinkingPath,
}

/// Confidence level attached to a knowledge candidate.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// A candidate knowledge artifact produced by reflection.
///
/// The body is Markdown; frontmatter is synthesized at persist time (the
/// persist path forces `unverified: true` for Lite-generated artifacts).
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeCandidate {
    pub kind: CandidateKind,
    pub title: String,
    pub body_markdown: String,
    pub confidence: Confidence,
    #[serde(default)]
    pub frontmatter_extra: serde_json::Value,
}

/// Read-only view of a goal ledger. Core defines the minimal field set;
/// the goal-ledger crate maps its richer same-source types onto this view
/// (one-way mapping, no dependency inversion).
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalLedgerView {
    pub goal_id: String,
    pub status: GoalStatusView,
    pub last_run: Option<RunSummaryView>,
    pub total_events: u64,
}

/// Lifecycle status of a goal, as seen through [`GoalLedgerView`].
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatusView {
    Draft,
    Active,
    BlockedOnUser,
    Complete,
    Failed,
    Archived,
}

/// One long-horizon run summary, as seen through [`GoalLedgerView`].
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummaryView {
    pub run_index: u64,
    pub next_action: String,
    pub progress: f64,
}

/// Decision returned by [`LongHorizonPolicy::next_action`]. Lite built-in
/// policies only return `Continue | Pause | AskUser`.
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LongHorizonDecision {
    Continue,
    Pause { reason: String },
    AskUser { question: String },
    Stop { reason: String },
}

/// Inbound message delivery handle for platform adapters (M5). The struct
/// is intentionally opaque at L0; the runtime finalizes its contents.
///
/// Stability: stable.
#[derive(Debug, Clone, Default)]
pub struct InboundSink {
    _private: (),
}

/// Frequency/content policy for reflection ("what is worth learning").
///
/// # Non-negotiable
/// [`EvolutionPolicy::requires_human_alignment`] defaults to `true` and any
/// override must still return `true`. CI asserts this (quality gate Q9).
///
/// Stability: stable.
#[async_trait]
pub trait EvolutionPolicy: Send + Sync {
    fn name(&self) -> &str;

    /// After each turn: is this turn worth reflecting on? (Frequency is
    /// up to the policy.)
    async fn should_reflect(&self, ctx: &TurnContext) -> bool;

    /// Extract candidate knowledge (memories/skills/thinking paths) from a
    /// session transcript.
    async fn extract(&self, transcript: &Transcript) -> Vec<KnowledgeCandidate>;

    /// Resolve candidates: merge, drop, deduplicate, route to user approval.
    async fn resolve(&self, candidates: Vec<KnowledgeCandidate>) -> Vec<KnowledgeCandidate>;

    /// Non-negotiable floor: writes must go through user approval.
    /// Defaults to `true`; overriding must preserve `true`.
    fn requires_human_alignment(&self) -> bool {
        crate::REQUIRES_HUMAN_ALIGNMENT_DEFAULT
    }
}

/// Long-horizon execution policy ("continue, pause, or hand over").
///
/// Stability: stable.
#[async_trait]
pub trait LongHorizonPolicy: Send + Sync {
    fn name(&self) -> &str;

    /// Before the next run: continue / pause / hand over to the user.
    async fn next_action(&self, ledger: &GoalLedgerView) -> LongHorizonDecision;
}

/// Platform adapter trait (M5): inbound entry points such as a sidecar or
/// messaging platform. Implementations drive `InboundSink`.
///
/// Stability: stable.
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self, sink: InboundSink) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}

/// The default policy: reflect never, continue always, human alignment on.
/// Ships with core (M1) so the engine can always hold a policy.
///
/// Stability: stable.
pub struct NullPolicy;

#[async_trait]
impl EvolutionPolicy for NullPolicy {
    fn name(&self) -> &str {
        "null"
    }

    async fn should_reflect(&self, _ctx: &TurnContext) -> bool {
        false
    }

    async fn extract(&self, _transcript: &Transcript) -> Vec<KnowledgeCandidate> {
        Vec::new()
    }

    async fn resolve(&self, candidates: Vec<KnowledgeCandidate>) -> Vec<KnowledgeCandidate> {
        candidates
    }
}

#[async_trait]
impl LongHorizonPolicy for NullPolicy {
    fn name(&self) -> &str {
        "null"
    }

    async fn next_action(&self, _ledger: &GoalLedgerView) -> LongHorizonDecision {
        LongHorizonDecision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TurnContext {
        TurnContext {
            session_id: SessionId::new(),
            turn_index: 3,
            error_count: 1,
            tool_calls_this_turn: 2,
            transcript_tail: vec![Message::user_text("hi")],
        }
    }

    /// Q9 re-check at the policy level: NullPolicy must never reflect and
    /// must always require human alignment.
    #[tokio::test]
    async fn null_policy_never_reflects_and_keeps_alignment() {
        let p = NullPolicy;
        assert_eq!(EvolutionPolicy::name(&p), "null");
        assert!(!p.should_reflect(&ctx()).await);
        let out = p.extract(&Transcript::default()).await;
        assert!(out.is_empty());
        assert!(p.requires_human_alignment());
    }

    #[tokio::test]
    async fn null_policy_long_horizon_continues() {
        let p = NullPolicy;
        let view = GoalLedgerView {
            goal_id: "g1".into(),
            status: GoalStatusView::Active,
            last_run: None,
            total_events: 0,
        };
        assert_eq!(
            LongHorizonPolicy::next_action(&p, &view).await,
            LongHorizonDecision::Continue
        );
        assert_eq!(LongHorizonPolicy::name(&p), "null");
    }

    #[test]
    fn knowledge_candidate_serde_roundtrip() {
        let c = KnowledgeCandidate {
            kind: CandidateKind::Memory("fact".into()),
            title: "Deploy uses pnpm".into(),
            body_markdown: "The repo uses pnpm workspaces.".into(),
            confidence: Confidence::Medium,
            frontmatter_extra: serde_json::json!({}),
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: KnowledgeCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn long_horizon_decision_serde_tags() {
        let d = LongHorizonDecision::AskUser {
            question: "Proceed?".into(),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""decision":"ask_user""#));
    }
}
