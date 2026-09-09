use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures::FutureExt;
use serde_json::Value;

/// The 24 plugin hook points exposed by the OpenPaw event system.
///
/// The `Peer*` / `Collaboration*` variants and the workflow-related variants
/// (`BeforeWorkflow`, `AfterWorkflow`, `BeforeStep`, `AfterStep`,
/// `OnStepFailure`) keep their registration semantics, but the OpenPaw core
/// ships no built-in trigger source for them: they exist so future community
/// workflow / collaboration plugins can register against them and drive the
/// chain themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    BeforeAgentLoop,
    AfterAgentLoop,
    BeforeToolCall,
    AfterToolCall,
    BeforeSkillMatch,
    AfterSkillMatch,
    BeforeMemoryStore,
    AfterMemoryStore,
    BeforeReflect,
    AfterReflect,
    BeforeSessionStart,
    AfterSessionEnd,
    OnError,
    OnApproval,
    BeforeWorkflow,
    AfterWorkflow,
    BeforeStep,
    AfterStep,
    OnStepFailure,
    // Peer 协作 Hook Points：保留可注册语义；OpenPaw 内核不提供内置触发源，
    // 由未来的社区 workflow / 协作插件触发。
    PeerRegistered,
    PeerUnregistered,
    PeerMessageSent,
    CollaborationStarted,
    CollaborationCompleted,
}

impl HookPoint {
    pub fn name(&self) -> &str {
        match self {
            HookPoint::BeforeAgentLoop => "before_agent_loop",
            HookPoint::AfterAgentLoop => "after_agent_loop",
            HookPoint::BeforeToolCall => "before_tool_call",
            HookPoint::AfterToolCall => "after_tool_call",
            HookPoint::BeforeSkillMatch => "before_skill_match",
            HookPoint::AfterSkillMatch => "after_skill_match",
            HookPoint::BeforeMemoryStore => "before_memory_store",
            HookPoint::AfterMemoryStore => "after_memory_store",
            HookPoint::BeforeReflect => "before_reflect",
            HookPoint::AfterReflect => "after_reflect",
            HookPoint::BeforeSessionStart => "before_session_start",
            HookPoint::AfterSessionEnd => "after_session_end",
            HookPoint::OnError => "on_error",
            HookPoint::OnApproval => "on_approval",
            HookPoint::BeforeWorkflow => "before_workflow",
            HookPoint::AfterWorkflow => "after_workflow",
            HookPoint::BeforeStep => "before_step",
            HookPoint::AfterStep => "after_step",
            HookPoint::OnStepFailure => "on_step_failure",
            HookPoint::PeerRegistered => "peer_registered",
            HookPoint::PeerUnregistered => "peer_unregistered",
            HookPoint::PeerMessageSent => "peer_message_sent",
            HookPoint::CollaborationStarted => "collaboration_started",
            HookPoint::CollaborationCompleted => "collaboration_completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Continue,
    Skip,
    /// Replace the payload with an arbitrary JSON value.
    ///
    /// The replaced payload is NOT schema-validated by the hook chain.
    /// Callers that consume the returned `HookDecision::Replace(value)` MUST
    /// validate the value against their expected schema before using it,
    /// as a malicious or buggy plugin could inject arbitrary JSON.
    Replace(Value),
}

#[derive(Debug, Clone)]
pub struct HookContext {
    pub point: HookPoint,
    pub payload: Value,
    pub metadata: HashMap<String, String>,
}

pub type HookFn =
    Box<dyn Fn(HookContext) -> Pin<Box<dyn Future<Output = HookResult> + Send>> + Send + Sync>;

#[derive(Debug)]
pub struct HookResult {
    pub decision: HookDecision,
    pub modified_payload: Option<Value>,
}

pub struct HookChain {
    hooks: HashMap<HookPoint, Vec<(String, HookFn)>>,
}

impl HookChain {
    pub fn new() -> Self {
        Self {
            hooks: HashMap::new(),
        }
    }

    pub fn register<F, Fut>(&mut self, point: HookPoint, name: &str, handler: F)
    where
        F: Fn(HookContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookResult> + Send + 'static,
    {
        let wrapped: HookFn = Box::new(move |ctx| Box::pin(handler(ctx)));
        self.hooks
            .entry(point)
            .or_default()
            .push((name.to_string(), wrapped));
    }

    /// Remove every hook registered under `name` at `point`.
    ///
    /// Returns the number of removed handlers. Registering the same name
    /// more than once is allowed; unregistering removes all of them.
    pub fn unregister(&mut self, point: HookPoint, name: &str) -> usize {
        let Some(list) = self.hooks.get_mut(&point) else {
            return 0;
        };
        let before = list.len();
        list.retain(|(n, _)| n != name);
        let removed = before - list.len();
        if list.is_empty() {
            self.hooks.remove(&point);
        }
        removed
    }

    pub async fn run(&self, point: HookPoint, payload: Value) -> HookDecision {
        let mut current = payload;
        let mut replaced = false;
        let hooks = self.hooks.get(&point);

        if let Some(hooks) = hooks {
            for (name, handler) in hooks {
                let ctx = HookContext {
                    point,
                    payload: current.clone(),
                    metadata: HashMap::new(),
                };
                tracing::trace!(hook=%name, point=%point.name(), "running hook");
                // Add timeout and panic recovery so a single misbehaving hook
                // can neither hang nor crash the entire chain.
                let hook_future = std::panic::AssertUnwindSafe(handler(ctx));
                let result = match tokio::time::timeout(
                    Duration::from_secs(10),
                    hook_future.catch_unwind(),
                )
                .await
                {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => {
                        tracing::warn!(hook=%name, "hook panicked, recovering");
                        continue;
                    }
                    Err(_) => {
                        tracing::warn!(hook=%name, timeout_secs = 10, "hook timed out, skipping");
                        continue;
                    }
                };

                match result.decision {
                    HookDecision::Skip => {
                        tracing::debug!(hook=%name, "hook requested skip");
                        return HookDecision::Skip;
                    }
                    HookDecision::Replace(val) => {
                        tracing::debug!(hook=%name, "hook replaced payload");
                        current = val;
                        replaced = true;
                    }
                    HookDecision::Continue => {
                        if let Some(modified) = result.modified_payload {
                            current = modified;
                            replaced = true;
                        }
                    }
                }
            }
        }

        if replaced {
            HookDecision::Replace(current)
        } else {
            HookDecision::Continue
        }
    }

    pub fn hook_count(&self, point: HookPoint) -> usize {
        self.hooks.get(&point).map(|v| v.len()).unwrap_or(0)
    }

    pub fn registered_points(&self) -> Vec<HookPoint> {
        self.hooks.keys().copied().collect()
    }
}

impl Default for HookChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hook_continue() {
        let mut chain = HookChain::new();
        chain.register(HookPoint::BeforeToolCall, "logger", |_ctx| async move {
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: None,
            }
        });

        let decision = chain
            .run(
                HookPoint::BeforeToolCall,
                serde_json::json!({"tool": "bash"}),
            )
            .await;
        assert_eq!(decision, HookDecision::Continue);
    }

    #[tokio::test]
    async fn test_hook_skip() {
        let mut chain = HookChain::new();
        chain.register(HookPoint::AfterToolCall, "blocker", |_ctx| async move {
            HookResult {
                decision: HookDecision::Skip,
                modified_payload: None,
            }
        });

        let decision = chain
            .run(HookPoint::AfterToolCall, serde_json::json!({}))
            .await;
        assert_eq!(decision, HookDecision::Skip);
    }

    #[tokio::test]
    async fn test_hook_replace() {
        let mut chain = HookChain::new();
        chain.register(HookPoint::BeforeSkillMatch, "rewriter", |_ctx| async move {
            HookResult {
                decision: HookDecision::Replace(serde_json::json!({"query": "modified"})),
                modified_payload: None,
            }
        });

        let decision = chain
            .run(
                HookPoint::BeforeSkillMatch,
                serde_json::json!({"query": "original"}),
            )
            .await;
        assert!(matches!(decision, HookDecision::Replace(_)));
    }

    #[tokio::test]
    async fn test_hook_chain_order() {
        let mut chain = HookChain::new();
        chain.register(HookPoint::OnApproval, "first", |ctx| async move {
            let val = ctx.payload["step"].as_u64().unwrap_or(0);
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: Some(serde_json::json!({"step": val + 1})),
            }
        });
        chain.register(HookPoint::OnApproval, "second", |ctx| async move {
            let val = ctx.payload["step"].as_u64().unwrap_or(0);
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: Some(serde_json::json!({"step": val + 10})),
            }
        });

        let decision = chain
            .run(HookPoint::OnApproval, serde_json::json!({"step": 0}))
            .await;
        assert!(matches!(decision, HookDecision::Replace(_)));
        if let HookDecision::Replace(val) = decision {
            assert_eq!(val["step"].as_u64(), Some(11));
        }
    }

    #[test]
    fn test_hook_count() {
        let mut chain = HookChain::new();
        assert_eq!(chain.hook_count(HookPoint::BeforeToolCall), 0);
        chain.register(HookPoint::BeforeToolCall, "a", |_ctx| async move {
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: None,
            }
        });
        assert_eq!(chain.hook_count(HookPoint::BeforeToolCall), 1);
    }

    #[tokio::test]
    async fn test_replace_takes_precedence_over_modified_payload() {
        // When a hook returns both Replace(val) and modified_payload,
        // the Replace value should win (modified_payload ignored).
        let mut chain = HookChain::new();
        chain.register(
            HookPoint::BeforeToolCall,
            "conflicting",
            |_ctx| async move {
                HookResult {
                    decision: HookDecision::Replace(serde_json::json!({"winner": "replace"})),
                    modified_payload: Some(serde_json::json!({"winner": "modified"})),
                }
            },
        );

        let decision = chain
            .run(HookPoint::BeforeToolCall, serde_json::json!({}))
            .await;
        match decision {
            HookDecision::Replace(val) => {
                assert_eq!(val["winner"].as_str(), Some("replace"));
            }
            _ => panic!("expected Replace decision"),
        }
    }

    #[tokio::test]
    async fn test_modified_payload_works_with_continue() {
        // modified_payload should still work when decision is Continue
        let mut chain = HookChain::new();
        chain.register(HookPoint::AfterSkillMatch, "modifier", |_ctx| async move {
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: Some(serde_json::json!({"modified": true})),
            }
        });

        let decision = chain
            .run(
                HookPoint::AfterSkillMatch,
                serde_json::json!({"modified": false}),
            )
            .await;
        match decision {
            HookDecision::Replace(val) => {
                assert_eq!(val["modified"].as_bool(), Some(true));
            }
            _ => panic!("expected Replace decision due to modified_payload"),
        }
    }

    #[tokio::test]
    async fn test_hook_panic_recovery() {
        // A panicking hook should be recovered, not crash the chain.
        let mut chain = HookChain::new();
        chain.register(HookPoint::BeforeToolCall, "panicker", |_ctx| async move {
            panic!("hook panicked!");
        });

        let decision = chain
            .run(HookPoint::BeforeToolCall, serde_json::json!({}))
            .await;
        // The panicked hook is skipped; no modification → Continue
        assert_eq!(decision, HookDecision::Continue);
    }

    #[tokio::test]
    async fn test_hook_panic_does_not_stop_chain() {
        // A panicked hook should not prevent subsequent hooks from running.
        let mut chain = HookChain::new();
        chain.register(HookPoint::BeforeToolCall, "panicker", |_ctx| async move {
            panic!("hook panicked!");
        });
        chain.register(HookPoint::BeforeToolCall, "normal", |_ctx| async move {
            HookResult {
                decision: HookDecision::Replace(serde_json::json!({"recovered": true})),
                modified_payload: None,
            }
        });

        let decision = chain
            .run(HookPoint::BeforeToolCall, serde_json::json!({}))
            .await;
        match decision {
            HookDecision::Replace(val) => {
                assert_eq!(val["recovered"].as_bool(), Some(true));
            }
            _ => panic!("expected Replace decision from second hook after panic recovery"),
        }
    }

    /// Every one of the 24 hook points must support the full lifecycle:
    /// register → trigger → unregister. The 5 `Peer*` / `Collaboration*`
    /// points have no built-in trigger source, so the test acts as the
    /// trigger itself (exactly what a community plugin would do).
    #[tokio::test]
    async fn test_all_24_hook_points_register_trigger_unregister() {
        let all_points = [
            HookPoint::BeforeAgentLoop,
            HookPoint::AfterAgentLoop,
            HookPoint::BeforeToolCall,
            HookPoint::AfterToolCall,
            HookPoint::BeforeSkillMatch,
            HookPoint::AfterSkillMatch,
            HookPoint::BeforeMemoryStore,
            HookPoint::AfterMemoryStore,
            HookPoint::BeforeReflect,
            HookPoint::AfterReflect,
            HookPoint::BeforeSessionStart,
            HookPoint::AfterSessionEnd,
            HookPoint::OnError,
            HookPoint::OnApproval,
            HookPoint::BeforeWorkflow,
            HookPoint::AfterWorkflow,
            HookPoint::BeforeStep,
            HookPoint::AfterStep,
            HookPoint::OnStepFailure,
            HookPoint::PeerRegistered,
            HookPoint::PeerUnregistered,
            HookPoint::PeerMessageSent,
            HookPoint::CollaborationStarted,
            HookPoint::CollaborationCompleted,
        ];
        assert_eq!(all_points.len(), 24, "hook point count must stay at 24");
        // Names must be unique and snake_case so manifests can reference them.
        let mut names: Vec<&str> = all_points.iter().map(|p| p.name()).collect();
        names.sort();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "hook point names must be unique");

        for point in all_points {
            let mut chain = HookChain::new();

            // 1) register
            chain.register(point, "probe", |_ctx| async move {
                HookResult {
                    decision: HookDecision::Continue,
                    modified_payload: Some(serde_json::json!({"touched": true})),
                }
            });
            assert_eq!(
                chain.hook_count(point),
                1,
                "register failed for {}",
                point.name()
            );
            assert!(chain.registered_points().contains(&point));

            // 2) trigger
            let decision = chain.run(point, serde_json::json!({})).await;
            match decision {
                HookDecision::Replace(v) => {
                    assert_eq!(
                        v["touched"],
                        serde_json::json!(true),
                        "trigger failed for {}",
                        point.name()
                    );
                }
                other => panic!(
                    "expected Replace from probe hook on {}: got {:?}",
                    point.name(),
                    other
                ),
            }

            // 3) unregister
            assert_eq!(
                chain.unregister(point, "probe"),
                1,
                "unregister failed for {}",
                point.name()
            );
            assert_eq!(
                chain.hook_count(point),
                0,
                "count after unregister on {}",
                point.name()
            );
            assert!(chain.registered_points().is_empty());
            let decision = chain.run(point, serde_json::json!({})).await;
            assert_eq!(
                decision,
                HookDecision::Continue,
                "hook still fired after unregister on {}",
                point.name()
            );
        }
    }

    #[test]
    fn test_unregister_missing_name_or_point() {
        let mut chain = HookChain::new();
        // Nothing registered at all.
        assert_eq!(chain.unregister(HookPoint::OnError, "nope"), 0);

        chain.register(HookPoint::OnError, "a", |_ctx| async move {
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: None,
            }
        });
        // Same name, different point → untouched.
        assert_eq!(chain.unregister(HookPoint::AfterAgentLoop, "a"), 0);
        assert_eq!(chain.hook_count(HookPoint::OnError), 1);
        assert_eq!(chain.unregister(HookPoint::OnError, "a"), 1);
        assert_eq!(chain.unregister(HookPoint::OnError, "a"), 0);
    }
}
