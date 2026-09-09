//! Tool guardrails — three-level decision semantics (same-source S7).
//!
//! `GuardrailDecision` semantics are frozen: `Allow` runs the tool,
//! `Confirm(reason)` requires user approval, `Deny(reason)` blocks with an
//! error result. Upstream implementations align to these variants.
//!
//! Stability: stable.

use async_trait::async_trait;

/// Lightweight check context. The upstream `ClassifierContext` maps onto
/// this shape; the full AI-classifier depth lands with E2.
///
/// Stability: stable.
#[derive(Debug, Clone, Default)]
pub struct CheckContext {
    /// Rendered transcript tail (most recent last).
    pub transcript_tail: Vec<String>,
    /// Workspace path for path-aware checks.
    pub workspace: std::path::PathBuf,
}

/// Three-level guardrail decision (same-source S7).
///
/// Not serializable by design — decisions are in-process signals; the
/// reason strings travel through events and tool results as plain text.
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailDecision {
    Allow,
    Confirm(String),
    Deny(String),
}

impl GuardrailDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny(_))
    }

    pub fn needs_confirmation(&self) -> bool {
        matches!(self, Self::Confirm(_))
    }
}

/// Stability: stable.
#[async_trait]
pub trait ToolGuardrail: Send + Sync {
    /// Primary guardrail check — implementors MUST override this method.
    ///
    /// The default implementation panics to prevent accidental infinite
    /// recursion: `check` must not delegate to `check_with_context` while
    /// the latter delegates back.
    async fn check(&self, _name: &str, _args: &serde_json::Value) -> GuardrailDecision {
        panic!(
            "ToolGuardrail::check must be overridden by implementor '{}'",
            std::any::type_name::<Self>()
        )
    }

    /// Context-aware check — defaults to `check`. Implementors that need
    /// the conversation context override this; `GuardrailChain` forwards
    /// the context to every child.
    async fn check_with_context(
        &self,
        name: &str,
        args: &serde_json::Value,
        _context: &CheckContext,
    ) -> GuardrailDecision {
        self.check(name, args).await
    }
}

/// A guardrail that allows everything. Default for tests and permissive
/// modes.
///
/// Stability: stable.
pub struct PermissiveGuardrail;

#[async_trait]
impl ToolGuardrail for PermissiveGuardrail {
    async fn check(&self, _name: &str, _args: &serde_json::Value) -> GuardrailDecision {
        GuardrailDecision::Allow
    }
}

/// A guardrail that denies everything except explicitly allow-listed tool
/// names. Useful for closed toolsets.
///
/// Stability: stable.
pub struct AllowlistGuardrail {
    pub allowed: Vec<String>,
}

#[async_trait]
impl ToolGuardrail for AllowlistGuardrail {
    async fn check(&self, name: &str, _args: &serde_json::Value) -> GuardrailDecision {
        if self.allowed.iter().any(|a| a == name) {
            GuardrailDecision::Allow
        } else {
            GuardrailDecision::Deny(format!("tool `{name}` is not on the allowlist"))
        }
    }
}

/// Ordered guardrail chain: the first non-`Allow` decision wins.
///
/// Stability: stable.
pub struct GuardrailChain {
    guardrails: Vec<Box<dyn ToolGuardrail>>,
}

impl GuardrailChain {
    pub fn new(guardrails: Vec<Box<dyn ToolGuardrail>>) -> Self {
        Self { guardrails }
    }

    pub fn is_empty(&self) -> bool {
        self.guardrails.is_empty()
    }

    pub fn len(&self) -> usize {
        self.guardrails.len()
    }
}

impl Default for GuardrailChain {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[async_trait]
impl ToolGuardrail for GuardrailChain {
    async fn check(&self, name: &str, args: &serde_json::Value) -> GuardrailDecision {
        for guardrail in &self.guardrails {
            match guardrail.check(name, args).await {
                GuardrailDecision::Allow => continue,
                non_allow => return non_allow,
            }
        }
        GuardrailDecision::Allow
    }

    /// Forwards the context to every child guardrail so context-aware
    /// implementations receive the full tail.
    async fn check_with_context(
        &self,
        name: &str,
        args: &serde_json::Value,
        context: &CheckContext,
    ) -> GuardrailDecision {
        for guardrail in &self.guardrails {
            match guardrail.check_with_context(name, args, context).await {
                GuardrailDecision::Allow => continue,
                non_allow => return non_allow,
            }
        }
        GuardrailDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct DenyBash;

    #[async_trait]
    impl ToolGuardrail for DenyBash {
        async fn check(&self, name: &str, _args: &serde_json::Value) -> GuardrailDecision {
            if name == "bash" {
                GuardrailDecision::Deny("shell disabled".into())
            } else {
                GuardrailDecision::Allow
            }
        }
    }

    struct ConfirmBash;

    #[async_trait]
    impl ToolGuardrail for ConfirmBash {
        async fn check(&self, name: &str, _args: &serde_json::Value) -> GuardrailDecision {
            if name == "bash" {
                GuardrailDecision::Confirm("runs a shell".into())
            } else {
                GuardrailDecision::Allow
            }
        }
    }

    #[tokio::test]
    async fn chain_first_non_allow_wins() {
        let chain = GuardrailChain::new(vec![Box::new(ConfirmBash), Box::new(DenyBash)]);
        // Confirm comes first in the chain → Confirm wins over Deny.
        let d = chain.check("bash", &json!({})).await;
        assert_eq!(d, GuardrailDecision::Confirm("runs a shell".into()));
        // Non-matching tool falls through the whole chain.
        assert_eq!(
            chain.check("grep", &json!({})).await,
            GuardrailDecision::Allow
        );
    }

    #[tokio::test]
    async fn deny_short_circuits() {
        let chain = GuardrailChain::new(vec![Box::new(DenyBash), Box::new(ConfirmBash)]);
        assert!(chain.check("bash", &json!({})).await.is_denied());
    }

    #[tokio::test]
    async fn permissive_allows_and_helpers() {
        let d = PermissiveGuardrail.check("anything", &json!({})).await;
        assert!(d.is_allowed());
        assert!(!d.is_denied());
        assert!(!d.needs_confirmation());
    }

    #[tokio::test]
    async fn allowlist_guardrail_denies_unknown() {
        let g = AllowlistGuardrail {
            allowed: vec!["grep".into()],
        };
        assert!(g.check("grep", &json!({})).await.is_allowed());
        let d = g.check("bash", &json!({})).await;
        assert!(d.is_denied());
    }

    #[tokio::test]
    async fn context_forwarded_to_children() {
        struct ContextSpy;
        #[async_trait]
        impl ToolGuardrail for ContextSpy {
            async fn check(&self, _n: &str, _a: &serde_json::Value) -> GuardrailDecision {
                // Required override; context path uses check_with_context.
                GuardrailDecision::Allow
            }
            async fn check_with_context(
                &self,
                _n: &str,
                _a: &serde_json::Value,
                ctx: &CheckContext,
            ) -> GuardrailDecision {
                if ctx.workspace.as_os_str() == "/ws" {
                    GuardrailDecision::Confirm("saw workspace".into())
                } else {
                    GuardrailDecision::Deny("no workspace".into())
                }
            }
        }
        let chain = GuardrailChain::new(vec![Box::new(ContextSpy)]);
        let ctx = CheckContext {
            workspace: "/ws".into(),
            ..Default::default()
        };
        let d = chain.check_with_context("x", &json!({}), &ctx).await;
        assert_eq!(d, GuardrailDecision::Confirm("saw workspace".into()));
    }

    #[test]
    fn decision_helpers_cover_all_variants() {
        assert!(GuardrailDecision::Allow.is_allowed());
        assert!(GuardrailDecision::Confirm("why".into()).needs_confirmation());
        let d = GuardrailDecision::Deny("no".into());
        assert!(d.is_denied());
        assert_eq!(d, GuardrailDecision::Deny("no".into()));
    }

    #[test]
    #[should_panic(expected = "must be overridden")]
    fn default_check_panics_to_break_recursion() {
        struct Recursive;
        #[async_trait]
        impl ToolGuardrail for Recursive {}
        // The trait default panics — this is the recursion guard.
        futures::executor::block_on(Recursive.check("x", &json!({})));
    }
}
