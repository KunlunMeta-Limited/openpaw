//! The agent loop — E1 skeleton.
//!
//! Semantics mirror the upstream `run_turn` (see `docs/spikes/S1-agent-loop.md`
//! for the call-tree outline and the E1/E2 split line).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use openpaw_core::{
    classify_api_error, ApprovalManager, ApprovalRequest, CheckContext, CompletionRequest,
    ContentBlock, Error, GuardrailDecision, IterationBudget, LlmProvider, Message, Result,
    RetryPolicy, StopReason, ToolCallOutcome, ToolGuardrail, ToolHost, ToolSpec,
};
use openpaw_plugins::{HookChain, HookDecision, HookPoint};
use tokio_util::sync::CancellationToken;

use crate::events::{AgentEvent, ClassifierStatus};

/// One model tool invocation extracted from a `ToolUse` block.
type ToolUseCall = (String, String, serde_json::Value);

/// The agent loop. Cheap to share (`Arc`); per-turn state lives in the
/// [`openpaw_core::Session`] passed to [`AgentLoop::run_turn`].
pub struct AgentLoop {
    provider: Arc<dyn LlmProvider>,
    tool_host: Arc<dyn ToolHost>,
    guardrail: Arc<dyn ToolGuardrail>,
    approval: Arc<dyn ApprovalManager>,
    budget: IterationBudget,
    model: String,
    max_tokens: u32,
    parallel_tools: bool,
    retry_policy: RetryPolicy,
    cancel_token: std::sync::Mutex<Option<CancellationToken>>,
    stream_timeout: Duration,
    compact_options: Option<crate::compaction::CompactOptions>,
    token_budget: std::sync::Mutex<Option<crate::token_budget::TokenBudgetTracker>>,
    hook_chain: Option<Arc<HookChain>>,
}

impl AgentLoop {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            provider,
            tool_host: Arc::new(openpaw_core::NullToolHost),
            guardrail: Arc::new(openpaw_core::PermissiveGuardrail),
            approval: Arc::new(openpaw_core::AutoApproveManager),
            budget: IterationBudget::new_elastic(8, 25, 1),
            model: "openpaw-default".to_string(),
            max_tokens: 4096,
            parallel_tools: true,
            retry_policy: RetryPolicy {
                max_retries: 3,
                // Tests and offline providers retry fast.
                base_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(200),
                backoff_multiplier: 2.0,
            },
            cancel_token: std::sync::Mutex::new(None),
            stream_timeout: Duration::from_secs(120),
            compact_options: None,
            token_budget: std::sync::Mutex::new(None),
            hook_chain: None,
        }
    }

    /// Enable per-iteration compaction checks with these options (E2).
    pub fn with_compact_options(mut self, opts: crate::compaction::CompactOptions) -> Self {
        self.compact_options = Some(opts);
        self
    }

    /// Enable the token budget: when the model stops but meaningful budget
    /// remains, the loop nudges it to continue (bounded by diminishing-
    /// returns detection and the usage threshold).
    pub fn with_token_budget(self, config: crate::token_budget::TokenBudgetConfig) -> Self {
        *self.token_budget_mut() = Some(crate::token_budget::TokenBudgetTracker::new(config));
        self
    }

    fn token_budget_mut(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<crate::token_budget::TokenBudgetTracker>> {
        self.token_budget.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn with_tool_host(mut self, host: Arc<dyn ToolHost>) -> Self {
        self.tool_host = host;
        self
    }

    pub fn with_guardrail(mut self, guardrail: Arc<dyn ToolGuardrail>) -> Self {
        self.guardrail = guardrail;
        self
    }

    pub fn with_approval_manager(mut self, mgr: Arc<dyn ApprovalManager>) -> Self {
        self.approval = mgr;
        self
    }

    /// Attach a plugin hook chain. `BeforeToolCall` hooks may block a tool
    /// call or rewrite its input; `AfterToolCall` hooks may rewrite the
    /// outcome. With no chain (or no hooks at those points), execution is
    /// unchanged.
    pub fn with_hook_chain(mut self, chain: Arc<HookChain>) -> Self {
        self.hook_chain = Some(chain);
        self
    }

    pub fn with_budget(mut self, budget: IterationBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = tokens;
        self
    }

    pub fn with_parallel_tools(mut self, enabled: bool) -> Self {
        self.parallel_tools = enabled;
        self
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    pub fn with_cancel_token(self, token: CancellationToken) -> Self {
        *self.cancel_token_mut() = Some(token);
        self
    }

    pub fn with_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_timeout = timeout;
        self
    }

    fn cancel_token_mut(&self) -> std::sync::MutexGuard<'_, Option<CancellationToken>> {
        self.cancel_token.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn current_cancel_token(&self) -> CancellationToken {
        self.cancel_token_mut().clone().unwrap_or_default()
    }

    fn is_cancelled(&self) -> bool {
        self.current_cancel_token().is_cancelled()
    }

    /// Request cancellation of the running turn.
    pub fn cancel(&self) {
        self.current_cancel_token().cancel();
    }

    /// Run one conversation turn: stream a model response, execute any
    /// requested tools, backfill the session, repeat until the model stops
    /// or the budget/cancel budget ends the turn. Returns the stop reason
    /// of the last model response.
    pub async fn run_turn(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<StopReason> {
        // Reset the cancellation token at the start of each turn — without
        // this, a cancel from a previous turn blocks all future turns.
        *self.cancel_token_mut() = Some(CancellationToken::new());
        self.budget.reset();

        loop {
            // ① cancel check
            if self.is_cancelled() {
                return Ok(StopReason::EndTurn);
            }

            // ③ budget consume (elastic; `expand` raises toward `max`)
            if !(self.budget.expand(1) && self.budget.try_consume()) {
                // Grace keeps one last iteration before giving up.
                if !self.budget.try_grace() {
                    return Ok(StopReason::EndTurn);
                }
            }

            // ④ compaction: estimate once, decide, compact with a single
            // summary call (E2; see docs/spikes/S1-agent-loop.md section 2 ④).
            if let Some(opts) = &self.compact_options {
                let tools_json_approx: usize =
                    tools.iter().map(|t| t.input_schema.to_string().len()).sum();
                let estimated = crate::compaction::estimate_session_tokens(
                    system.as_deref().unwrap_or(""),
                    session,
                    tools_json_approx,
                );
                let trigger = crate::compaction::should_compact_with_options_estimated(
                    session, opts, estimated,
                );
                if trigger != crate::compaction::CompactTrigger::None {
                    let compacted =
                        crate::compaction::compact_session(&*self.provider, session, opts).await?;
                    if compacted > 0 {
                        let after = crate::compaction::estimate_session_tokens(
                            system.as_deref().unwrap_or(""),
                            session,
                            tools_json_approx,
                        );
                        event_sink(AgentEvent::CompactionPerformed {
                            before_tokens: u32::try_from(estimated).unwrap_or(u32::MAX),
                            after_tokens: u32::try_from(after).unwrap_or(u32::MAX),
                        });
                    }
                }
            }

            // ⑤ build request
            let req = CompletionRequest {
                model: self.model.clone(),
                system: system.clone(),
                messages: session.messages.clone(),
                tools: tools.to_vec(),
                max_tokens: self.max_tokens,
                ..Default::default()
            };

            // ⑥ stream with retry
            let resp = match self.stream_with_retry(req, event_sink).await {
                Ok(resp) => resp,
                Err(e) => {
                    if self.is_cancelled() {
                        return Ok(StopReason::EndTurn);
                    }
                    return Err(e);
                }
            };

            // ⑦ usage backfill (saturating — see Usage docs) + token budget
            let usage = resp.usage;
            session.total_input_tokens = session
                .total_input_tokens
                .saturating_add(usage.input_tokens);
            session.total_output_tokens = session
                .total_output_tokens
                .saturating_add(usage.output_tokens);
            event_sink(AgentEvent::TurnUsage { usage });
            {
                let mut guard = self.token_budget_mut();
                if let Some(tracker) = guard.as_mut() {
                    tracker
                        .record_turn(usage.input_tokens.saturating_add(usage.output_tokens) as u64);
                }
            }

            // Assistant message backfill (may carry tool_use blocks).
            session.messages.push(Message {
                role: openpaw_core::Role::Assistant,
                content: resp.content.clone(),
                ..Default::default()
            });

            if resp.stop_reason != StopReason::ToolUse {
                // Token-budget continuation: the model stopped, but if
                // meaningful budget remains (and returns are not
                // diminishing), nudge it to keep working.
                let nudge = {
                    let mut guard = self.token_budget_mut();
                    match guard.as_mut() {
                        Some(tracker)
                            if resp.stop_reason == StopReason::EndTurn
                                && tracker.should_continue() =>
                        {
                            let msg = tracker.config().nudge_message.clone();
                            tracker.increment_continuation();
                            Some(msg)
                        }
                        _ => None,
                    }
                };
                if let Some(nudge) = nudge {
                    session.messages.push(Message::user_text(nudge));
                    continue;
                }
                return Ok(resp.stop_reason);
            }

            // ⑧ collect tool_use blocks and dispatch
            let tool_uses: Vec<ToolUseCall> = resp
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();
            for (id, name, _) in &tool_uses {
                event_sink(AgentEvent::ToolUseStart {
                    tool_use_id: id.clone(),
                    name: name.clone(),
                });
            }
            let results = self.execute_tools(tool_uses, event_sink).await;

            // ⑨ backfill tool results as the next user message
            // (tool_result blocks ride on the user role).
            session.messages.push(Message {
                role: openpaw_core::Role::User,
                content: results,
                ..Default::default()
            });
        }
    }

    /// Stream a completion with classified retry. Text deltas are forwarded
    /// to the sink; the assembled response is returned on `Final`.
    async fn stream_with_retry(
        &self,
        req: CompletionRequest,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<openpaw_core::CompletionResponse> {
        let mut attempts: u32 = 0;
        'outer: loop {
            let token = self.current_cancel_token();
            let stream = match self.provider.stream(req.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    let class = classify_api_error(&e);
                    if matches!(
                        class,
                        openpaw_core::ApiErrorClass::RetryableTransient
                            | openpaw_core::ApiErrorClass::RetryableOverloaded
                            | openpaw_core::ApiErrorClass::FallbackCandidate
                    ) && attempts < self.retry_policy.max_retries as u32
                    {
                        attempts += 1;
                        let retry_after = match &e {
                            Error::RateLimited {
                                retry_after_secs: Some(secs),
                                ..
                            } => Some(*secs),
                            _ => None,
                        };
                        tokio::time::sleep(
                            self.retry_policy
                                .delay_for_attempt_with_jitter(attempts as usize, retry_after),
                        )
                        .await;
                        continue;
                    }
                    return Err(e);
                }
            };

            futures::pin_mut!(stream);
            let mut final_resp: Option<openpaw_core::CompletionResponse> = None;
            // Whether this attempt already forwarded a `MessageStart` and any
            // text/thinking delta. Providers without native streaming — and
            // streams whose content rides only in `Final`, which the
            // `LlmProvider::stream` contract allows — emit a single `Final`
            // carrying the assembled response. Without the replay below that
            // content never reaches the sink and callers observe an empty turn.
            let mut announced = false;
            let mut streamed = false;

            loop {
                let ev = tokio::select! {
                    ev = stream.next() => ev,
                    _ = token.cancelled() => {
                        return Err(Error::Provider("stream cancelled by user".into()));
                    }
                };
                let Some(ev) = ev else { break };
                match ev? {
                    openpaw_core::StreamEvent::MessageStart => {
                        announced = true;
                        event_sink(AgentEvent::MessageStart);
                    }
                    openpaw_core::StreamEvent::TextDelta { text, .. } => {
                        streamed = true;
                        event_sink(AgentEvent::TextDelta { text });
                    }
                    openpaw_core::StreamEvent::ThinkingDelta { text, .. } => {
                        streamed = true;
                        event_sink(AgentEvent::ThinkingDelta { text });
                    }
                    // E1: buffered tool assembly — blocks arrive complete in
                    // `Final`. Early dispatch during streaming is the E2
                    // `streaming_tool_executor` evaluation.
                    openpaw_core::StreamEvent::ToolUseStart { .. }
                    | openpaw_core::StreamEvent::ToolUseInputDelta { .. }
                    | openpaw_core::StreamEvent::BlockStop { .. } => {}
                    openpaw_core::StreamEvent::Final(resp) => {
                        let resp = *resp;
                        if !streamed {
                            replay_final_content(&resp, announced, event_sink);
                        }
                        final_resp = Some(resp);
                        break;
                    }
                }
            }

            match final_resp {
                Some(resp) => return Ok(resp),
                None => {
                    // Stream ended without Final (torn stream): retry while
                    // budget remains, else fail.
                    if attempts < self.retry_policy.max_retries as u32 {
                        attempts += 1;
                        tokio::time::sleep(self.retry_policy.delay_for_attempt(attempts as usize))
                            .await;
                        continue 'outer;
                    }
                    return Err(Error::Provider("stream ended without Final event".into()));
                }
            }
        }
    }

    /// Tool dispatcher: >1 call with parallel_tools → phased three-stage
    /// path; otherwise sequential. (Upstream also routes orchestration and
    /// ctx_inspect tools to dedicated paths — none exist in E1.)
    async fn execute_tools(
        &self,
        tool_uses: Vec<ToolUseCall>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Vec<ContentBlock> {
        if tool_uses.is_empty() {
            return Vec::new();
        }
        if self.parallel_tools && tool_uses.len() > 1 {
            self.execute_tools_phased(tool_uses, event_sink).await
        } else {
            self.execute_tools_sequential(tool_uses, event_sink).await
        }
    }

    fn check_context(&self) -> CheckContext {
        CheckContext::default()
    }

    /// Three-stage phased execution:
    /// 1. classify all calls (guardrail) — Deny → error result immediately;
    /// 2. `Allow` calls run concurrently;
    /// 3. `Confirm` calls run serially through the approval manager.
    async fn execute_tools_phased(
        &self,
        tool_uses: Vec<ToolUseCall>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Vec<ContentBlock> {
        let ctx = self.check_context();

        // ── Phase 1: classify ──────────────────────────────────────────
        let mut denied: Vec<ContentBlock> = Vec::new();
        let mut safe: Vec<ToolUseCall> = Vec::new();
        let mut confirm: Vec<ToolUseCall> = Vec::new();

        for (id, name, input) in tool_uses {
            event_sink(AgentEvent::ClassifierStatus {
                tool_use_id: id.clone(),
                status: ClassifierStatus::Checking,
            });
            let decision = self.guardrail.check_with_context(&name, &input, &ctx).await;
            match decision {
                GuardrailDecision::Deny(reason) => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::AutoDenied {
                            reason: reason.clone(),
                        },
                    });
                    event_sink(AgentEvent::ToolResult {
                        tool_use_id: id.clone(),
                        content: format!("blocked by guardrail: {reason}"),
                        is_error: true,
                        name: Some(name.clone()),
                        duration_ms: None,
                    });
                    denied.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: format!("blocked by guardrail: {reason}"),
                        is_error: true,
                        name: Some(name),
                        protected: false,
                    });
                }
                GuardrailDecision::Allow => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::AutoApproved {
                            reason: "guardrail allowed".into(),
                        },
                    });
                    safe.push((id, name, input));
                }
                GuardrailDecision::Confirm(reason) => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::SuggestConfirm { reason },
                    });
                    confirm.push((id, name, input));
                }
            }
        }

        // ── Phase 2: run safe calls concurrently ───────────────────────
        let mut results: Vec<ContentBlock> = denied;
        if !safe.is_empty() {
            // tokio::spawn per call: real concurrency even for tools whose
            // hosts block their executor thread (upstream parity).
            let host = Arc::clone(&self.tool_host);
            let ctx = ctx.clone();
            let hook_chain = self.hook_chain.clone();
            let handles = safe.into_iter().map(|(id, name, input)| {
                let host = Arc::clone(&host);
                let ctx = ctx.clone();
                let hooks = hook_chain.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let outcome =
                        execute_with_hooks(hooks.as_deref(), &*host, &name, input, &ctx).await;
                    (id, name, started, outcome)
                })
            });
            let done = futures::future::join_all(handles).await;
            for joined in done {
                let (id, name, started, outcome) = joined.unwrap_or_else(|e| {
                    (
                        "unknown".to_string(),
                        "unknown".to_string(),
                        Instant::now(),
                        ToolCallOutcome::error(format!("tool task failed: {e}")),
                    )
                });
                let duration_ms =
                    Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                event_sink(AgentEvent::ToolResult {
                    tool_use_id: id.clone(),
                    content: outcome.content.clone(),
                    is_error: outcome.is_error,
                    name: Some(name.clone()),
                    duration_ms,
                });
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: outcome.content,
                    is_error: outcome.is_error,
                    name: Some(name),
                    protected: false,
                });
            }
        }

        // ── Phase 3: confirm calls, serially, behind the approval gate ──
        for (id, name, input) in confirm {
            let started = Instant::now();
            let response = self
                .approval
                .request(ApprovalRequest::new(
                    name.clone(),
                    input.clone(),
                    "guardrail confirm",
                ))
                .await;
            let outcome = match response {
                openpaw_core::ApprovalResponse::Approved => {
                    execute_with_hooks(
                        self.hook_chain.as_deref(),
                        &*self.tool_host,
                        &name,
                        input,
                        &ctx,
                    )
                    .await
                }
                openpaw_core::ApprovalResponse::Denied { reason } => {
                    ToolCallOutcome::error(format!("denied by user: {reason}"))
                }
            };
            let duration_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            event_sink(AgentEvent::ToolResult {
                tool_use_id: id.clone(),
                content: outcome.content.clone(),
                is_error: outcome.is_error,
                name: Some(name.clone()),
                duration_ms,
            });
            results.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content: outcome.content,
                is_error: outcome.is_error,
                name: Some(name),
                protected: false,
            });
        }

        results
    }

    /// Serial path: validate → classify → approve → call, one tool at a
    /// time. A failure produces an error result, never aborts the batch.
    async fn execute_tools_sequential(
        &self,
        tool_uses: Vec<ToolUseCall>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Vec<ContentBlock> {
        let ctx = self.check_context();
        let mut results = Vec::with_capacity(tool_uses.len());

        for (id, name, input) in tool_uses {
            let started = Instant::now();

            if let Err(reason) = self.tool_host.validate_input(&name, &input).await {
                let content = format!("invalid input: {reason}");
                event_sink(AgentEvent::ToolResult {
                    tool_use_id: id.clone(),
                    content: content.clone(),
                    is_error: true,
                    name: Some(name.clone()),
                    duration_ms: None,
                });
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content,
                    is_error: true,
                    name: Some(name),
                    protected: false,
                });
                continue;
            }

            event_sink(AgentEvent::ClassifierStatus {
                tool_use_id: id.clone(),
                status: ClassifierStatus::Checking,
            });
            let decision = self.guardrail.check_with_context(&name, &input, &ctx).await;

            let outcome = match decision {
                GuardrailDecision::Deny(reason) => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::AutoDenied {
                            reason: reason.clone(),
                        },
                    });
                    ToolCallOutcome::error(format!("blocked by guardrail: {reason}"))
                }
                GuardrailDecision::Confirm(reason) => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::SuggestConfirm {
                            reason: reason.clone(),
                        },
                    });
                    let response = self
                        .approval
                        .request(ApprovalRequest::new(name.clone(), input.clone(), reason))
                        .await;
                    match response {
                        openpaw_core::ApprovalResponse::Approved => {
                            execute_with_hooks(
                                self.hook_chain.as_deref(),
                                &*self.tool_host,
                                &name,
                                input,
                                &ctx,
                            )
                            .await
                        }
                        openpaw_core::ApprovalResponse::Denied { reason } => {
                            ToolCallOutcome::error(format!("denied by user: {reason}"))
                        }
                    }
                }
                GuardrailDecision::Allow => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::AutoApproved {
                            reason: "guardrail allowed".into(),
                        },
                    });
                    execute_with_hooks(
                        self.hook_chain.as_deref(),
                        &*self.tool_host,
                        &name,
                        input,
                        &ctx,
                    )
                    .await
                }
            };

            let duration_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            event_sink(AgentEvent::ToolResult {
                tool_use_id: id.clone(),
                content: outcome.content.clone(),
                is_error: outcome.is_error,
                name: Some(name.clone()),
                duration_ms,
            });
            results.push(ContentBlock::ToolResult {
                tool_use_id: id,
                content: outcome.content,
                is_error: outcome.is_error,
                name: Some(name),
                protected: false,
            });
        }

        results
    }
}

/// Replay a `Final` response's assembled content as the event sequence a
/// streaming provider would have produced.
///
/// `LlmProvider::stream` promises that `Final` "carries the fully assembled
/// `CompletionResponse`", so a provider without native streaming (or a stream
/// that only delivers content in `Final`) is fully conformant while emitting
/// no delta at all. This is called only when the attempt forwarded no
/// text/thinking delta, which keeps streaming providers on their exact
/// existing event order; `announced` avoids a duplicate `MessageStart` when
/// one was already forwarded.
fn replay_final_content(
    resp: &openpaw_core::CompletionResponse,
    announced: bool,
    event_sink: &mut (dyn FnMut(AgentEvent) + Send),
) {
    let has_content = resp.content.iter().any(|block| {
        block.as_text().is_some_and(|text| !text.is_empty())
            || block
                .as_thinking()
                .is_some_and(|thinking| !thinking.is_empty())
    });
    if !has_content {
        return;
    }
    if !announced {
        event_sink(AgentEvent::MessageStart);
    }
    for block in &resp.content {
        if let Some(text) = block.as_text() {
            if !text.is_empty() {
                event_sink(AgentEvent::TextDelta {
                    text: text.to_string(),
                });
            }
        } else if let Some(thinking) = block.as_thinking() {
            if !thinking.is_empty() {
                event_sink(AgentEvent::ThinkingDelta {
                    text: thinking.to_string(),
                });
            }
        }
    }
}

/// Hook-aware wrapper around [`execute_one`].
///
/// Decision mapping — payload shape `{"tool_name", "input"}` (the
/// AfterToolCall payload adds `"result"`):
/// - `BeforeToolCall`: `Skip` → the host is never invoked and an error
///   outcome (`"blocked by hook"`) is returned; `Continue` → run as-is;
///   `Replace(val)` → run with `val["input"]` as the input.
/// - `AfterToolCall`: `Replace(val)` rewrites `result.content` /
///   `result.is_error` when present; any other shape is ignored.
///
/// Zero-overhead short-circuit: with no chain attached, or when no hooks
/// are registered at either point, this is exactly [`execute_one`].
async fn execute_with_hooks(
    hook_chain: Option<&HookChain>,
    host: &dyn ToolHost,
    name: &str,
    input: serde_json::Value,
    ctx: &CheckContext,
) -> ToolCallOutcome {
    let Some(chain) = hook_chain else {
        return execute_one(host, name, input, ctx).await;
    };
    let has_before = chain.hook_count(HookPoint::BeforeToolCall) > 0;
    let has_after = chain.hook_count(HookPoint::AfterToolCall) > 0;
    if !has_before && !has_after {
        return execute_one(host, name, input, ctx).await;
    }

    // BeforeToolCall: block, rewrite the input, or proceed unchanged.
    let input = if has_before {
        let payload = serde_json::json!({ "tool_name": name, "input": input.clone() });
        match chain.run(HookPoint::BeforeToolCall, payload).await {
            HookDecision::Skip => return ToolCallOutcome::error("blocked by hook"),
            HookDecision::Continue => input,
            HookDecision::Replace(val) => val["input"].clone(),
        }
    } else {
        input
    };

    let mut outcome = execute_one(host, name, input.clone(), ctx).await;

    // AfterToolCall: observers may rewrite the outcome.
    if has_after {
        let payload = serde_json::json!({
            "tool_name": name,
            "input": input,
            "result": {
                "content": outcome.content.clone(),
                "is_error": outcome.is_error,
            },
        });
        if let HookDecision::Replace(val) = chain.run(HookPoint::AfterToolCall, payload).await {
            if let Some(content) = val.pointer("/result/content").and_then(|c| c.as_str()) {
                outcome.content = content.to_string();
            }
            if let Some(is_error) = val.pointer("/result/is_error").and_then(|e| e.as_bool()) {
                outcome.is_error = is_error;
            }
        }
    }
    outcome
}

/// Validate → call, converting transport errors into error outcomes so a
/// single failing tool never aborts the batch.
async fn execute_one(
    host: &dyn ToolHost,
    name: &str,
    input: serde_json::Value,
    ctx: &CheckContext,
) -> ToolCallOutcome {
    let _ = ctx; // context-aware hosts arrive with E2 depth
    match host.call(name, input).await {
        Ok(outcome) => outcome,
        Err(e) => ToolCallOutcome::error(e.to_string()),
    }
}
