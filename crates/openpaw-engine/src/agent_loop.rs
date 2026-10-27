//! The agent loop — E1 skeleton.
//!
//! Semantics mirror the upstream `run_turn` (see `docs/spikes/S1-agent-loop.md`
//! for the call-tree outline and the E1/E2 split line).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{FutureExt, StreamExt};
use openpaw_core::{
    classify_api_error, ApprovalManager, ApprovalRequest, CheckContext, CompletionRequest,
    ContentBlock, Error, GuardrailDecision, IterationBudget, LlmProvider, Message, Result,
    RetryPolicy, StopReason, ToolCallOutcome, ToolGuardrail, ToolHost, ToolSpec,
};
use openpaw_plugins::{HookChain, HookDecision, HookPoint};
use tokio_util::sync::CancellationToken;

use crate::events::{AgentEvent, ClassifierStatus};
use crate::run::{NoopObserver, RunContext, RunControl, RunObserver, RunOutcome, RunStatus};
use openpaw_core::{SessionEvent, Usage};

/// One model tool invocation extracted from a `ToolUse` block.
type ToolUseCall = (String, String, serde_json::Value);

/// 用途：复用现有推理循环
/// 字段：Provider、预算、工具和活动取消状态。
/// 来源去向：CLI/Host到Session与观察接口
/// 约束：同实例只允许一个Run，不共享预算并发。
/// 稳定性：保留旧入口并新增运行接口
/// 关联：需求/设计 REQ-OP-003；DES-OP-002。
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
    parent_cancel: Option<CancellationToken>,
    cancel_token: std::sync::Mutex<Option<CancellationToken>>,
    stream_timeout: Duration,
    compact_options: Option<crate::compaction::CompactOptions>,
    token_budget: std::sync::Mutex<Option<crate::token_budget::TokenBudgetTracker>>,
    hook_chain: Option<Arc<HookChain>>,
}

impl AgentLoop {
    /// 功能：用Provider真实默认模型创建引擎。
    /// 参数：provider为共享模型实现。
    /// 返回：未运行的独立引擎。
    /// 错误：无；空默认模型在执行前拒绝。
    /// 状态：初始化独立预算和取消状态。
    /// 并发与取消：一个实例不允许并发Run。
    /// 权限：不授予工具额外权限。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        let model = provider.default_model().to_owned();
        Self {
            provider,
            tool_host: Arc::new(openpaw_core::NullToolHost),
            guardrail: Arc::new(openpaw_core::PermissiveGuardrail),
            approval: Arc::new(openpaw_core::AutoApproveManager),
            budget: IterationBudget::new_elastic(8, 25, 1),
            model,
            max_tokens: 4096,
            parallel_tools: true,
            retry_policy: RetryPolicy {
                max_retries: 3,
                // Tests and offline providers retry fast.
                base_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(200),
                backoff_multiplier: 2.0,
            },
            parent_cancel: None,
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

    /// 功能：绑定外部父取消信号。
    /// 参数：token为调用方控制的父信号。
    /// 返回：配置后的引擎。
    /// 错误：无。
    /// 状态：保存父关系，不在运行开始覆盖。
    /// 并发与取消：已取消父token使后续运行均取消；cancel只取消当前子Run。
    /// 权限：无。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.parent_cancel = Some(token);
        self
    }

    /// 功能：设置Provider建连及流事件空闲时限。
    /// 参数：timeout为每次等待的最大空闲时长。
    /// 返回：配置后的引擎。
    /// 错误：无；到期由运行返回Timeout。
    /// 状态：不改变Run总预算或工具时限。
    /// 并发与取消：每个流事件重置空闲计时，取消优先。
    /// 权限：无。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    pub fn with_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_timeout = timeout;
        self
    }

    fn cancel_token_mut(&self) -> std::sync::MutexGuard<'_, Option<CancellationToken>> {
        self.cancel_token.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 功能：请求取消当前运行。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：仅取消当前子token，不影响后续Run和外部父token。
    /// 并发与取消：工具实际清理完成后运行才返回。
    /// 权限：无。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    pub fn cancel(&self) {
        if let Some(token) = self.cancel_token_mut().as_ref() {
            token.cancel();
        }
    }

    /// 功能：兼容旧调用方，运行一次对话。
    /// 参数：session为模型上下文，system/tools为输入，event_sink只接收展示事件。
    /// 返回：正常停止原因；没有持久化观察器。
    /// 错误：失败、取消和超时分别返回明确错误。
    /// 状态：修改Session；可靠保存应使用run_with_context。
    /// 并发与取消：同实例拒绝并发，继承with_cancel_token父关系。
    /// 权限：沿用现有工具检查。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    pub async fn run_turn(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<StopReason> {
        let context = RunContext::default();
        let outcome = self
            .run_with_context(
                session,
                system,
                tools,
                &context,
                &mut NoopObserver,
                event_sink,
            )
            .await;
        match outcome.error {
            Some(e) => Err(e),
            None => outcome
                .stop_reason
                .ok_or_else(|| Error::Session("missing stop reason".into())),
        }
    }

    /// 功能：用独立上下文和可等待观察接口运行现有推理循环。
    /// 参数：session/system/tools为模型输入，context为运行标识与预算，observer保存关键事实，event_sink显示增量。
    /// 返回：已完成清理和终态保存的RunOutcome。
    /// 错误：错误包含在outcome中；持久化失败强制Failed，不重试副作用。
    /// 状态：保存接受、消息、工具关联、用量、压缩上下文与终态。
    /// 并发与取消：同引擎实例拒绝并发；旧工具未实现及时取消时仍等待真实完成。
    /// 权限：不扩大工具权限，观察器负责数据目录授权。
    /// 关联：需求/设计 REQ-OP-003、REQ-OP-004；DES-OP-002、DES-OP-003。
    pub async fn run_with_context(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        context: &RunContext,
        observer: &mut dyn RunObserver,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> RunOutcome {
        // 观察器的任何错误都是事实保存失败，不能因实现返回Timeout而误报运行预算耗尽。
        struct Reliable<'a>(&'a mut dyn RunObserver);
        #[async_trait::async_trait]
        impl RunObserver for Reliable<'_> {
            async fn record(&mut self, event: SessionEvent) -> Result<()> {
                self.0
                    .record(event)
                    .await
                    .map_err(|e| Error::Store(format!("lifecycle persistence failed: {e}")))
            }
        }
        let mut observer = Reliable(observer);
        let token = context.cancel.child_token();
        {
            let mut active = self.cancel_token_mut();
            if active.is_some() {
                return RunOutcome {
                    status: RunStatus::Failed,
                    stop_reason: None,
                    usage: Usage::default(),
                    error: Some(Error::Session("engine already running".into())),
                };
            }
            *active = Some(token.clone());
        }
        // 即使future被调用方丢弃，也恢复同实例活动标记；调用方仍不得drop带工具副作用的Run。
        struct Active<'a>(&'a std::sync::Mutex<Option<CancellationToken>>);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        let _active = Active(&self.cancel_token);
        let control = RunControl {
            run_id: context.run_id.clone(),
            parent: self.parent_cancel.clone(),
            cancel: token.clone(),
            deadline: context
                .total_timeout
                .map(|d| tokio::time::Instant::now() + d),
            tool_timeout: context.tool_timeout,
            expired: CancellationToken::new(),
        };
        let mut usage = Usage::default();
        let mut execute = Box::pin(async {
            if context.run_id.trim().is_empty() || self.model.trim().is_empty() {
                return Err(Error::Config("run id and model must not be empty".into()));
            }
            observer
                .record(SessionEvent::RunStarted {
                    run_id: context.run_id.clone(),
                })
                .await?;
            self.budget.reset();
            if let Some(tracker) = self.token_budget_mut().as_mut() {
                tracker.reset();
            }
            session.cancelled = false;
            self.run_inner(
                session,
                system,
                tools,
                &control,
                &mut observer,
                event_sink,
                &mut usage,
            )
            .await
        });
        let result = (&mut execute).await;
        // 释放对observer/session/usage的借用后保存终态；不因取消丢弃持久化future。
        drop(execute);
        let (mut status, stop_reason, mut error) = match result {
            Ok(stop) => (RunStatus::Completed, Some(stop), None),
            Err(e) => {
                let status = match e {
                    Error::Cancelled => RunStatus::Cancelled,
                    Error::Timeout(_) => RunStatus::TimedOut,
                    _ => RunStatus::Failed,
                };
                (status, None, Some(e))
            }
        };
        session.cancelled = status == RunStatus::Cancelled;
        if let Err(e) = observer
            .record(SessionEvent::RunFinished {
                run_id: context.run_id.clone(),
                status: status.as_str().into(),
                error: error.as_ref().map(ToString::to_string),
            })
            .await
        {
            status = RunStatus::Failed;
            error = Some(Error::Store(format!("cannot persist run terminal: {e}")));
        }
        RunOutcome {
            status,
            stop_reason: if error.is_some() { None } else { stop_reason },
            usage,
            error,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_inner(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        control: &RunControl,
        observer: &mut dyn RunObserver,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        run_usage: &mut Usage,
    ) -> Result<StopReason> {
        loop {
            control.check()?;
            // 先消费既有额度，耗尽才扩容；固定额度不能因无法扩容而被跳过。
            // 扩容到上限后仍可消费剩余额度，最后才使用原有宽限，不提高预算。
            if !(self.budget.try_consume() || (self.budget.expand(1) && self.budget.try_consume()))
            {
                // Grace keeps one last iteration before giving up.
                if !self.budget.try_grace() {
                    return Err(Error::BudgetExhausted("iteration budget exhausted".into()));
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
                    let (compacted, summary_usage) = control
                        .wait(tokio::time::timeout(
                            self.stream_timeout,
                            crate::compaction::compact_session_report(
                                &*self.provider,
                                session,
                                opts,
                                &self.model,
                            ),
                        ))
                        .await?
                        .map_err(|_| Error::Timeout("compaction summary idle".into()))??;
                    if compacted > 0 {
                        account_usage(session, run_usage, summary_usage, observer).await?;
                        observer
                            .record(SessionEvent::ContextSnapshot {
                                messages: session.messages.clone(),
                            })
                            .await?;
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
            let mut partial = String::new();
            let resp = match self
                .stream_with_retry(req, event_sink, control, &mut partial)
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if !partial.is_empty() {
                        let msg = Message::assistant_text(partial);
                        observer.record(SessionEvent::Message(msg.clone())).await?;
                        session.messages.push(msg);
                    }
                    return Err(e);
                }
            };

            // ⑦ usage backfill (saturating — see Usage docs) + token budget
            let usage = resp.usage;
            account_usage(session, run_usage, usage, observer).await?;
            event_sink(AgentEvent::TurnUsage { usage });
            {
                let mut guard = self.token_budget_mut();
                if let Some(tracker) = guard.as_mut() {
                    tracker
                        .record_turn(usage.input_tokens.saturating_add(usage.output_tokens) as u64);
                }
            }

            // Assistant message backfill (may carry tool_use blocks).
            let message = Message {
                role: openpaw_core::Role::Assistant,
                content: resp.content.clone(),
                ..Default::default()
            };
            observer
                .record(SessionEvent::Message(message.clone()))
                .await?;
            session.messages.push(message);

            if resp.stop_reason != StopReason::ToolUse {
                // 额度耗尽/不完整响应只是停止生成，不是交付完成；只思考
                // 而无正文也不能报成功。先保留usage和已输出内容，再可靠写失败
                // 终态，不自动重试，以免重复文字或重放已经发生的工具副作用。
                if matches!(
                    resp.stop_reason,
                    StopReason::MaxTokens | StopReason::Incomplete
                ) {
                    return Err(Error::Provider(
                        if resp.stop_reason == StopReason::MaxTokens {
                            "model output limit reached before complete delivery".into()
                        } else {
                            "model response incomplete before delivery".into()
                        },
                    ));
                }
                if !resp.content.iter().any(|block| match block {
                    ContentBlock::Text { text } => !text.trim().is_empty(),
                    ContentBlock::Image { .. } | ContentBlock::Document { .. } => true,
                    _ => false,
                }) {
                    return Err(Error::Provider("model completed without an answer".into()));
                }
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
                    let msg = Message::user_text(nudge);
                    observer.record(SessionEvent::Message(msg.clone())).await?;
                    session.messages.push(msg);
                    continue;
                }
                control.check()?;
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
                observer
                    .record(SessionEvent::ToolCallStart {
                        call_id: id.clone(),
                        tool_name: name.clone(),
                    })
                    .await?;
                event_sink(AgentEvent::ToolUseStart {
                    tool_use_id: id.clone(),
                    name: name.clone(),
                });
            }
            let mut tool_events = Vec::new();
            let (results, execution_failure) = self
                .execute_tools(
                    tool_uses,
                    &mut |event| {
                        if let AgentEvent::ToolResult {
                            tool_use_id,
                            name,
                            is_error,
                            duration_ms,
                            ..
                        } = &event
                        {
                            tool_events.push(SessionEvent::ToolCallComplete {
                                call_id: tool_use_id.clone(),
                                tool_name: name.clone().unwrap_or_default(),
                                success: !is_error,
                                duration_ms: *duration_ms,
                            });
                        }
                        event_sink(event);
                    },
                    control,
                )
                .await;
            for event in tool_events {
                observer.record(event).await?;
            }

            // ⑨ backfill tool results as the next user message
            // (tool_result blocks ride on the user role).
            let message = Message {
                role: openpaw_core::Role::User,
                content: results,
                ..Default::default()
            };
            observer
                .record(SessionEvent::Message(message.clone()))
                .await?;
            session.messages.push(message);
            if let Some(error) = execution_failure {
                return Err(error);
            }
            control.check()?;
        }
    }

    /// 无外部输出前允许重试；一旦输出文本/思考/工具流片段，禁止从头重拼。
    async fn stream_with_retry(
        &self,
        req: CompletionRequest,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        control: &RunControl,
        partial: &mut String,
    ) -> Result<openpaw_core::CompletionResponse> {
        let mut attempts = 0;
        'attempt: loop {
            let stream = match control
                .wait(tokio::time::timeout(
                    self.stream_timeout,
                    self.provider.stream(req.clone()),
                ))
                .await?
            {
                Ok(Ok(stream)) => stream,
                other => {
                    let error = match other {
                        Ok(Err(e)) => e,
                        _ => Error::Timeout("provider connection idle".into()),
                    };
                    let class = classify_api_error(&error);
                    if matches!(
                        class,
                        openpaw_core::ApiErrorClass::RetryableTransient
                            | openpaw_core::ApiErrorClass::RetryableOverloaded
                            | openpaw_core::ApiErrorClass::FallbackCandidate
                    ) && attempts < self.retry_policy.max_retries
                    {
                        attempts += 1;
                        let retry_after = match error {
                            Error::RateLimited {
                                retry_after_secs, ..
                            } => retry_after_secs,
                            _ => None,
                        };
                        control
                            .wait(tokio::time::sleep(
                                self.retry_policy
                                    .delay_for_attempt_with_jitter(attempts, retry_after),
                            ))
                            .await?;
                        continue;
                    }
                    return Err(error);
                }
            };
            futures::pin_mut!(stream);
            let mut emitted = false;
            loop {
                let ev = control
                    .wait(tokio::time::timeout(self.stream_timeout, stream.next()))
                    .await?
                    .map_err(|_| Error::Timeout("provider stream idle".into()))?;
                let Some(ev) = ev else { break };
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(Error::Provider(ref reason))
                        if reason.starts_with("invalid OpenAI streamed tool arguments")
                            && !emitted
                            && attempts < self.retry_policy.max_retries.min(1) =>
                    {
                        // 本次调用尚未发布文字/工具且未执行，重发相同上下文至多一次。
                        // 已完成工具保留在请求历史里；取消、期限与真实请求预算仍生效。
                        attempts += 1;
                        control
                            .wait(tokio::time::sleep(
                                self.retry_policy.delay_for_attempt(attempts),
                            ))
                            .await?;
                        continue 'attempt;
                    }
                    Err(error) => return Err(error),
                };
                match ev {
                    openpaw_core::StreamEvent::MessageStart => event_sink(AgentEvent::MessageStart),
                    openpaw_core::StreamEvent::TextDelta { text, .. } => {
                        emitted |= !text.is_empty();
                        partial.push_str(&text);
                        event_sink(AgentEvent::TextDelta { text });
                    }
                    openpaw_core::StreamEvent::ThinkingDelta { text, .. } => {
                        emitted |= !text.is_empty();
                        event_sink(AgentEvent::ThinkingDelta { text });
                    }
                    openpaw_core::StreamEvent::Final(resp) => return Ok(*resp),
                    _ => {
                        emitted = true;
                    }
                }
            }
            if !emitted && attempts < self.retry_policy.max_retries {
                attempts += 1;
                control
                    .wait(tokio::time::sleep(
                        self.retry_policy.delay_for_attempt(attempts),
                    ))
                    .await?;
            } else {
                return Err(Error::Provider("stream ended without Final event".into()));
            }
        }
    }

    // D05：串行和并行都先经过同一准备步骤；未知/写入工具保守作为并发屏障。
    async fn execute_tools(
        &self,
        tool_uses: Vec<ToolUseCall>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        control: &RunControl,
    ) -> (Vec<ContentBlock>, Option<Error>) {
        let specs = match control.wait(self.tool_host.list_tools()).await {
            Ok(Ok(specs)) => specs,
            error => {
                return (
                    tool_uses
                        .into_iter()
                        .map(|(id, name, _)| {
                            emit_tool_result(
                                id,
                                name,
                                ToolCallOutcome::error(format!(
                                    "cannot load tool metadata: {error:?}"
                                )),
                                None,
                                event_sink,
                            )
                        })
                        .collect(),
                    None,
                )
            }
        };
        let mut failure = None;
        let mut results = Vec::with_capacity(tool_uses.len());
        let mut parallel = Vec::new();
        for (id, name, input) in tool_uses {
            let spec = specs.iter().find(|s| s.name == name);
            let parallel_safe = self.parallel_tools
                && spec.is_some_and(|s| {
                    s.concurrency == Some(openpaw_core::ToolConcurrency::ParallelSafe)
                });
            // NeverParallel、PathScoped和未知声明均不与任何前序调用重叠。
            if !parallel_safe {
                self.finish_parallel(
                    &mut parallel,
                    &mut results,
                    &mut failure,
                    event_sink,
                    control,
                )
                .await;
            }
            match self
                .prepare_tool(&id, &name, input, spec, event_sink, control)
                .await
            {
                Ok(call) if parallel_safe => parallel.push(call),
                Ok(call) => {
                    let started = Instant::now();
                    let outcome = std::panic::AssertUnwindSafe(execute_prepared(
                        self.hook_chain.as_deref(),
                        &*self.tool_host,
                        &call,
                        control,
                    ))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Ok(ToolCallOutcome::error("tool task panicked")));
                    let outcome = capture_execution_failure(outcome, &mut failure, control);
                    let duration =
                        Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
                    let cascade = self.tool_host.should_cascade_on_error(&call.name, &outcome);
                    results.push(emit_tool_result(
                        call.id, call.name, outcome, duration, event_sink,
                    ));
                    if cascade {
                        control.cancel.cancel();
                    }
                }
                Err(outcome) => results.push(emit_tool_result(id, name, outcome, None, event_sink)),
            }
        }
        self.finish_parallel(
            &mut parallel,
            &mut results,
            &mut failure,
            event_sink,
            control,
        )
        .await;
        (results, failure)
    }

    async fn prepare_tool(
        &self,
        id: &str,
        name: &str,
        mut input: serde_json::Value,
        spec: Option<&ToolSpec>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        control: &RunControl,
    ) -> std::result::Result<PreparedTool, ToolCallOutcome> {
        if let Some(chain) = &self.hook_chain {
            let payload = serde_json::json!({"tool_name":name,"input":input});
            match control
                .wait(chain.run(HookPoint::BeforeToolCall, payload))
                .await
            {
                Ok(HookDecision::Continue) => {}
                Ok(HookDecision::Skip) => return Err(ToolCallOutcome::error("blocked by hook")),
                Ok(HookDecision::Replace(value)) => {
                    // Hook不能偷偷改工具名；改输入必须重新走完整安全链。
                    if value
                        .get("tool_name")
                        .is_some_and(|n| n.as_str() != Some(name))
                    {
                        return Err(ToolCallOutcome::error("hook cannot change tool name"));
                    }
                    input = value
                        .get("input")
                        .cloned()
                        .ok_or_else(|| ToolCallOutcome::error("hook replacement missing input"))?;
                }
                Err(e) => return Err(ToolCallOutcome::error(e.to_string())),
            }
        }
        control
            .wait(self.tool_host.validate_input(name, &input))
            .await
            .map_err(|e| ToolCallOutcome::error(e.to_string()))?
            .map_err(|e| ToolCallOutcome::error(format!("invalid input: {e}")))?;
        event_sink(AgentEvent::ClassifierStatus {
            tool_use_id: id.into(),
            status: ClassifierStatus::Checking,
        });
        let decision = control
            .wait(
                self.guardrail
                    .check_with_context(name, &input, &CheckContext::default()),
            )
            .await
            .map_err(|e| ToolCallOutcome::error(e.to_string()))?;
        let decision = match decision {
            GuardrailDecision::Allow if spec.is_some_and(|s| s.requires_approval) => {
                GuardrailDecision::Confirm("tool metadata requires approval".into())
            }
            other => other,
        };
        let mut context = openpaw_core::ToolCallContext::new(
            &control.run_id,
            id,
            name,
            &input,
            control.cancel.child_token(),
        );
        match decision {
            GuardrailDecision::Deny(reason) => {
                event_sink(AgentEvent::ClassifierStatus {
                    tool_use_id: id.into(),
                    status: ClassifierStatus::AutoDenied {
                        reason: reason.clone(),
                    },
                });
                return Err(ToolCallOutcome::error(format!(
                    "blocked by guardrail: {reason}"
                )));
            }
            GuardrailDecision::Allow => event_sink(AgentEvent::ClassifierStatus {
                tool_use_id: id.into(),
                status: ClassifierStatus::AutoApproved {
                    reason: "guardrail allowed".into(),
                },
            }),
            GuardrailDecision::Confirm(reason) => {
                event_sink(AgentEvent::ClassifierStatus {
                    tool_use_id: id.into(),
                    status: ClassifierStatus::SuggestConfirm {
                        reason: reason.clone(),
                    },
                });
                let mut request = ApprovalRequest::new(name, input.clone(), reason);
                request.binding = Some(context.binding().clone());
                let response = control
                    .wait(self.approval.request(request))
                    .await
                    .map_err(|e| ToolCallOutcome::error(e.to_string()))?;
                match response {
                    openpaw_core::ApprovalResponse::Approved => context = context.approve(),
                    openpaw_core::ApprovalResponse::Denied { reason } => {
                        return Err(ToolCallOutcome::error(format!("denied by user: {reason}")))
                    }
                }
            }
        }
        control
            .check()
            .map_err(|e| ToolCallOutcome::error(e.to_string()))?;
        Ok(PreparedTool {
            id: id.into(),
            name: name.into(),
            input,
            context,
        })
    }

    async fn finish_parallel(
        &self,
        calls: &mut Vec<PreparedTool>,
        results: &mut Vec<ContentBlock>,
        failure: &mut Option<Error>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        control: &RunControl,
    ) {
        let mut pending = futures::stream::FuturesUnordered::new();
        for call in calls.drain(..) {
            let host = Arc::clone(&self.tool_host);
            let hooks = self.hook_chain.clone();
            let control = control.clone();
            // 关联ID留在JoinHandle外，panic不能把结果关联变成unknown。
            let id = call.id.clone();
            let name = call.name.clone();
            let task = tokio::spawn(async move {
                let started = Instant::now();
                let outcome = execute_prepared(hooks.as_deref(), &*host, &call, &control).await;
                (
                    outcome,
                    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                )
            });
            pending.push(async move {
                let (outcome, duration) = match task.await {
                    Ok((outcome, duration)) => (outcome, Some(duration)),
                    Err(error) => (
                        Ok(ToolCallOutcome::error(format!("tool task failed: {error}"))),
                        None,
                    ),
                };
                (id, name, outcome, duration)
            });
        }
        // D06：每个完成即报告；取消后继续join，直到所有实际清理结束。
        while let Some((id, name, outcome, duration)) = pending.next().await {
            let outcome = capture_execution_failure(outcome, failure, control);
            let cascade = self.tool_host.should_cascade_on_error(&name, &outcome);
            results.push(emit_tool_result(id, name, outcome, duration, event_sink));
            if cascade {
                control.cancel.cancel();
            }
        }
    }
}

struct PreparedTool {
    id: String,
    name: String,
    input: serde_json::Value,
    context: openpaw_core::ToolCallContext,
}

fn emit_tool_result(
    id: String,
    name: String,
    outcome: ToolCallOutcome,
    duration_ms: Option<u64>,
    event_sink: &mut (dyn FnMut(AgentEvent) + Send),
) -> ContentBlock {
    event_sink(AgentEvent::ToolResult {
        tool_use_id: id.clone(),
        content: outcome.content.clone(),
        is_error: outcome.is_error,
        name: Some(name.clone()),
        duration_ms,
    });
    ContentBlock::ToolResult {
        tool_use_id: id,
        content: outcome.content,
        is_error: outcome.is_error,
        name: Some(name),
        protected: false,
    }
}

// 普通工具错误可反馈模型；可靠开始记录失败则停止整Run，仍收齐每个call的结果。
fn capture_execution_failure(
    result: Result<ToolCallOutcome>,
    failure: &mut Option<Error>,
    control: &RunControl,
) -> ToolCallOutcome {
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let content = error.to_string();
            if failure.is_none() {
                *failure = Some(error);
            }
            control.cancel.cancel();
            ToolCallOutcome::error(content)
        }
    }
}

async fn execute_prepared(
    hooks: Option<&HookChain>,
    host: &dyn ToolHost,
    call: &PreparedTool,
    control: &RunControl,
) -> Result<ToolCallOutcome> {
    // 审批等待中外部文件可能改变，不能用旧的前置校验替代这里的重验。
    if let Err(e) = control
        .wait(host.validate_input(&call.name, &call.input))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    {
        return Ok(ToolCallOutcome::error(format!(
            "invalid input before execution: {e}"
        )));
    }
    let mut outcome = execute_one(host, call, control).await?;
    if let Some(chain) = hooks {
        if chain.hook_count(HookPoint::AfterToolCall) > 0 {
            let payload = serde_json::json!({"tool_name":call.name,"input":call.input,"result":{"content":outcome.content,"is_error":outcome.is_error}});
            if let Ok(HookDecision::Replace(value)) = control
                .wait(chain.run(HookPoint::AfterToolCall, payload))
                .await
            {
                if let Some(text) = value.pointer("/result/content").and_then(|v| v.as_str()) {
                    outcome.content = text.into();
                }
                // 已发生失败不能被展示Hook改成成功；允许把成功降为错误。
                if value.pointer("/result/is_error").and_then(|v| v.as_bool()) == Some(true) {
                    outcome.is_error = true;
                }
            }
        }
    }
    Ok(outcome)
}

async fn execute_one(
    host: &dyn ToolHost,
    call: &PreparedTool,
    control: &RunControl,
) -> Result<ToolCallOutcome> {
    if let Err(e) = control.check() {
        return Ok(ToolCallOutcome::error(e.to_string()));
    }
    let call_cancel = call.context.cancel();
    let future = host.call_with_context(&call.name, call.input.clone(), call.context.clone());
    tokio::pin!(future);
    let timeout = async {
        match control.tool_timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => futures::future::pending().await,
        }
    };
    let result = tokio::select! {
        biased;
        _=control.interrupted()=> { call_cancel.cancel(); future.await },
        _=timeout=> { call_cancel.cancel();control.expired.cancel();future.await },
        result=&mut future=>result,
    };
    match result {
        // 观察器将记录失败统一为Store；不能作为普通模型工具错误吞掉。
        Err(error @ Error::Store(_)) => Err(error),
        other => Ok(other.unwrap_or_else(|e| ToolCallOutcome::error(e.to_string()))),
    }
}

async fn account_usage(
    session: &mut openpaw_core::Session,
    run_usage: &mut Usage,
    usage: Usage,
    observer: &mut dyn RunObserver,
) -> Result<()> {
    run_usage.input_tokens = run_usage.input_tokens.saturating_add(usage.input_tokens);
    run_usage.output_tokens = run_usage.output_tokens.saturating_add(usage.output_tokens);
    run_usage.cache_read_tokens = run_usage
        .cache_read_tokens
        .saturating_add(usage.cache_read_tokens);
    run_usage.cache_creation_tokens = run_usage
        .cache_creation_tokens
        .saturating_add(usage.cache_creation_tokens);
    observer.record(SessionEvent::Usage(usage)).await?;
    session.total_input_tokens = session
        .total_input_tokens
        .saturating_add(usage.input_tokens);
    session.total_output_tokens = session
        .total_output_tokens
        .saturating_add(usage.output_tokens);
    Ok(())
}
