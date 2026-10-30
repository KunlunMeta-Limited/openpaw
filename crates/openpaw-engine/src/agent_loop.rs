//! 功能：驱动模型请求、工具执行与安全的上下文整理。
//! 输入输出边界：接收中立会话、模型和工具，发布事件并独立收集完整产出。
//! 禁止承担：不写会话文件，不创建入口权限或重置压缩过程中的预算。
//! 关联：docs/spikes/S1-agent-loop.md、CHG-2026-0051。

use std::future::Future;
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

/// One model tool invocation extracted from a `ToolUse` block.
type ToolUseCall = (String, String, serde_json::Value);

// 完整产出与模型投影分开收集；入口上下文只服务本回合，不进入执行器共享状态。
struct TurnCollection<'a> {
    tool_batch_coordination: Option<crate::turn_execution::ToolBatchCoordination>,
    generated: &'a mut Vec<Message>,
    attachments: Vec<openpaw_core::InlineMedia>,
    recorder: Option<crate::turn_execution::ExecutionRecorder>,
    context_turn_id: Option<String>,
    context_progress: Option<crate::turn_execution::ContextProgress>,
    end_turn_coordination: Option<crate::turn_execution::EndTurnCoordination>,
    pause_check: Option<crate::user_input::PauseCheck>,
}

impl<'a> TurnCollection<'a> {
    fn new(generated: &'a mut Vec<Message>) -> Self {
        Self {
            tool_batch_coordination: None,
            generated,
            attachments: Vec::new(),
            recorder: None,
            context_turn_id: None,
            context_progress: None,
            end_turn_coordination: None,
            pause_check: None,
        }
    }
}

#[derive(Default)]
struct TurnRuntime {
    active: bool,
    cancel_token: Option<CancellationToken>,
}

struct ActiveTurnGuard<'a> {
    runtime: &'a std::sync::Mutex<TurnRuntime>,
    token: CancellationToken,
    tool_host: &'a dyn ToolHost,
}

impl Drop for ActiveTurnGuard<'_> {
    fn drop(&mut self) {
        // 父回合 future 被 Host 强制 abort 时也必须先取消内部回合令牌，
        // 让审批、Provider 和工具子任务统一停止，再释放实例级活动闸门。
        self.token.cancel();
        self.tool_host.set_media_context(Vec::new());
        let mut runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        runtime.cancel_token = None;
        runtime.active = false;
    }
}

enum PreparedToolInput {
    Classified {
        input: serde_json::Value,
        decision: GuardrailDecision,
    },
    Rejected(ToolCallOutcome),
    Cancelled,
}

/// 对明确限流或过载只允许一次重试，避免上游拥塞时放大请求。
fn retry_limit_for_error(
    error: &Error,
    class: openpaw_core::ApiErrorClass,
    configured_max: u32,
) -> u32 {
    if matches!(
        error,
        Error::RateLimited { .. }
            | Error::ServerError {
                status: 503 | 529,
                ..
            }
    ) || class == openpaw_core::ApiErrorClass::RetryableOverloaded
    {
        configured_max.min(1)
    } else {
        configured_max
    }
}

/// 识别已建立流中可通过重新请求恢复的协议截断；不修改 Core 的通用错误分类。
fn retryable_stream_protocol_error(error: &Error) -> bool {
    let detail = error.to_string().to_lowercase();
    detail.contains("stream ended without final")
        || detail.contains("utf-8 码点中间结束")
        || detail.contains("sse data")
        || detail.contains("finish_reason")
        || detail.contains("[done]")
        || detail.contains("arguments json")
        || detail.contains("tool_use json")
}

fn response_contains_dsml_tool_call(response: &openpaw_core::CompletionResponse) -> bool {
    response.content.iter().any(|block| {
        let ContentBlock::Text { text } = block else {
            return false;
        };
        let compact = text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .replace('\u{ff5c}', "|")
            .to_ascii_lowercase();
        compact.contains("<||dsml||tool_calls>")
            || compact.contains("<||dsml||invoke")
            || compact.contains("</||dsml||tool_calls>")
    })
}

fn add_usage(left: openpaw_core::Usage, right: openpaw_core::Usage) -> openpaw_core::Usage {
    openpaw_core::Usage {
        input_tokens: left.input_tokens.saturating_add(right.input_tokens),
        output_tokens: left.output_tokens.saturating_add(right.output_tokens),
        cache_read_tokens: left
            .cache_read_tokens
            .saturating_add(right.cache_read_tokens),
        cache_creation_tokens: left
            .cache_creation_tokens
            .saturating_add(right.cache_creation_tokens),
    }
}

/// 功能：驱动模型与工具的有界回合；会话事实仍由调用方持有。
/// 数据边界：可选正常结束续行回调只返回提示，不启动新回合或增加预算。
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
    turn_runtime: std::sync::Mutex<TurnRuntime>,
    stream_timeout: Duration,
    compact_options: Option<crate::compaction::CompactOptions>,
    token_budget: std::sync::Mutex<Option<crate::token_budget::TokenBudgetTracker>>,
    hook_chain: Option<Arc<HookChain>>,
    end_turn_continuation: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl AgentLoop {
    /// 功能：创建默认有界模型与工具回合驱动器。
    /// 参数：provider 为模型供应商边界。
    /// 返回：尚未安装可选续行策略的 AgentLoop。
    /// 错误：无。
    /// 状态：创建实例级预算与取消状态，不启动请求。
    /// 并发与取消：每个实例同一时间只允许一个活动回合。
    /// 权限：使用默认工具与审批配置，可由 builder 明确替换。
    /// 关联：run_turn、with_end_turn_continuation。
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
                // 保持 OpenPaw 原有快速退避；限流和过载另由错误分类限制为最多一次重试。
                base_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(200),
                backoff_multiplier: 2.0,
            },
            turn_runtime: std::sync::Mutex::new(TurnRuntime::default()),
            stream_timeout: Duration::from_secs(120),
            compact_options: None,
            token_budget: std::sync::Mutex::new(None),
            hook_chain: None,
            end_turn_continuation: None,
        }
    }

    /// 功能：安装正常 EndTurn 时的可选续行判断。
    /// 参数：continuation 返回下一提示，或 None 表示允许结束。
    /// 返回：具有该续行策略的 AgentLoop。
    /// 错误：无。
    /// 状态：原回合内续行，沿用同一迭代预算和截止时间。
    /// 并发与取消：同步回调只在正常结束且未取消时调用。
    /// 权限：不扩展工具权限；预算终答、错误和 MaxTokens 不续行。
    /// 关联：Host Goal 前台协调；未配置时保持原行为。
    pub fn with_end_turn_continuation(
        mut self,
        continuation: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        self.end_turn_continuation = Some(continuation);
        self
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
        self.turn_runtime_mut().cancel_token = Some(token);
        self
    }

    pub fn with_stream_timeout(mut self, timeout: Duration) -> Self {
        self.stream_timeout = timeout;
        self
    }

    fn turn_runtime_mut(&self) -> std::sync::MutexGuard<'_, TurnRuntime> {
        self.turn_runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn current_cancel_token(&self) -> CancellationToken {
        self.turn_runtime_mut()
            .cancel_token
            .clone()
            .unwrap_or_default()
    }

    fn begin_turn(&self, token: CancellationToken) -> Result<ActiveTurnGuard<'_>> {
        {
            let mut runtime = self.turn_runtime_mut();
            if runtime.active {
                return Err(Error::session("agent loop already has an active turn"));
            }
            runtime.active = true;
            runtime.cancel_token = Some(token.clone());
        }
        let guard = ActiveTurnGuard {
            runtime: &self.turn_runtime,
            token,
            tool_host: &*self.tool_host,
        };
        self.budget.reset();
        if let Some(tracker) = self.token_budget_mut().as_mut() {
            tracker.reset();
        }
        Ok(guard)
    }

    fn record_summary_cost(&self, session: &openpaw_core::Session, before: (u32, u32)) {
        if let Some(tracker) = self.token_budget_mut().as_mut() {
            tracker.record_turn(
                u64::from(session.total_input_tokens.saturating_sub(before.0))
                    + u64::from(session.total_output_tokens.saturating_sub(before.1)),
            );
        }
    }

    fn is_cancelled(&self) -> bool {
        self.current_cancel_token().is_cancelled()
    }

    /// Request cancellation of the running turn.
    pub fn cancel(&self) {
        self.current_cancel_token().cancel();
    }

    /// 功能：执行一个完整回合，按需调用工具、整理上下文并接续模型。
    /// 参数：session为独占模型视图，system/tools为实际请求配置，event_sink接收执行事件。
    /// 返回：最后模型响应的停止原因。
    /// 错误：模型、工具、上下文和预算错误向调用方传播。
    /// 状态：更新会话消息和用量；不自行提交原始记录。
    /// 并发与取消：同一执行器仅允许一个活动回合，cancel可中断请求。
    /// 权限：工具调用沿用已配置护栏和审批。
    /// 关联：turn_execution::run_turn及持久入口。
    pub async fn run_turn(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<StopReason> {
        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        self.run_turn_inner(
            session,
            system,
            tools,
            event_sink,
            CancellationToken::new(),
            (provider, model),
            TurnCollection::new(&mut Vec::new()),
        )
        .await
    }

    /// 功能：使用调用方拥有的取消令牌执行一个回合。
    /// 参数：参数含义与 [`run_turn`] 相同，`token` 为该回合的精确取消令牌。
    /// 返回：最后一次模型响应的停止原因或引擎错误。
    /// 错误：取消、provider、工具和审批错误按 `Result` 返回。
    /// 状态：不会在入口替换令牌，因此调度空档内取消仍能命中本回合。
    /// 并发与取消：令牌同时控制模型流、工具、审批和退避等待。
    /// 权限：沿用执行器已配置的工具和审批策略。
    /// 关联：`API-HOST-SESSION-007`、`REQ-B-HOST-006`。
    pub async fn run_turn_with_cancel_token(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        token: CancellationToken,
    ) -> Result<StopReason> {
        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        self.run_turn_inner(
            session,
            system,
            tools,
            event_sink,
            token,
            (provider, model),
            TurnCollection::new(&mut Vec::new()),
        )
        .await
    }

    /// 功能：按单次请求选择 provider 和 model 执行回合，同时保留执行器的工具、护栏和审批状态。
    /// 参数：前五个参数与 [`run_turn_with_cancel_token`] 相同；`provider_and_model` 为本次 provider 与模型标识。
    /// 返回：最后一次模型响应的停止原因或引擎错误。
    /// 错误：provider、工具、审批、取消和预算错误均不被吞掉。
    /// 状态：只覆盖本次回合的模型来源，不修改执行器默认 provider/model。
    /// 并发与取消：使用传入令牌控制精确回合，工具和审批仍共享执行器策略。
    /// 权限：请求选择 provider 不得绕过 Host 的 provider 白名单和鉴权边界。
    /// 关联：`API-HOST-SESSION-006`、`REQ-B-HOST-003`。
    pub async fn run_turn_with_provider_and_cancel_token(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        token: CancellationToken,
        provider_and_model: (Arc<dyn LlmProvider>, String),
    ) -> Result<StopReason> {
        self.run_turn_inner(
            session,
            system,
            tools,
            event_sink,
            token,
            provider_and_model,
            TurnCollection::new(&mut Vec::new()),
        )
        .await
    }

    /// 功能：执行回合并收集每一条真实生成消息，独立于压缩后的上下文。
    /// 参数：session、tools、sink 沿用回合输入，options 提供媒体快照和模型，generated 接收产出。
    /// 返回：停止原因；完整消息交由入口提交。
    /// 错误：模型、预算或工具媒体非法时返回既有错误。
    /// 状态：更新模型上下文与本轮收集器；不写文件。
    /// 并发与取消：每个执行器仅一个活动回合，取消由 options 令牌控制。
    /// 权限：沿用工具分类、审批和工作区规则。
    /// 关联：turn_execution::run_turn、CLI/Host 完整历史。
    pub(crate) async fn run_turn_collect(
        &self,
        session: &mut openpaw_core::Session,
        tools: &[ToolSpec],
        sink: &mut (dyn FnMut(AgentEvent) + Send),
        options: crate::turn_execution::TurnOptions,
        generated: &mut Vec<Message>,
    ) -> Result<StopReason> {
        let selection = options
            .provider_and_model
            .unwrap_or_else(|| (self.provider.clone(), self.model.clone()));
        if options.compact_only {
            let _active_turn = self.begin_turn(options.cancel.child_token())?;
            if let Some(progress) = &options.context_progress {
                progress();
            }
            let mut opts = self.compact_options.clone().unwrap_or_default();
            opts.model_limit = opts
                .model_limit
                .min(selection.0.model_capabilities(&selection.1).context_window as usize);
            opts.protect_head_pairs = 0;
            opts.keep_recent_pairs = 1;
            let intent = session.messages.last().cloned();
            let before = (session.total_input_tokens, session.total_output_tokens);
            let summary_result = cancel_or(
                &options.cancel,
                crate::compaction::compact_session_for_turn(
                    &*selection.0,
                    &selection.1,
                    session,
                    &opts,
                    intent.as_ref(),
                    &Default::default(),
                ),
            )
            .await;
            self.record_summary_cost(session, before);
            let count = match summary_result {
                Some(result) => result?,
                None => return Ok(StopReason::EndTurn),
            };
            let message = Message::assistant_text(if count == 0 {
                "无需整理：当前没有足够的旧历史。"
            } else {
                "已整理上下文，原始聊天和工具记录继续保留。"
            })
            .identified();
            session.messages.push(message.clone());
            generated.push(message);
            return Ok(StopReason::EndTurn);
        }
        self.run_turn_inner(
            session,
            options.system,
            tools,
            sink,
            options.cancel,
            selection,
            TurnCollection {
                tool_batch_coordination: options.tool_batch_coordination,
                generated,
                attachments: options.attachments,
                recorder: options.recorder,
                context_turn_id: options.context_turn_id,
                context_progress: options.context_progress,
                end_turn_coordination: options.end_turn_coordination,
                pause_check: options.pause_check,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // 中立入口保留原有参数；额外收集记录独立于上下文。
    async fn run_turn_inner(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<String>,
        tools: &[ToolSpec],
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        token: CancellationToken,
        provider_and_model: (Arc<dyn LlmProvider>, String),
        collection: TurnCollection<'_>,
    ) -> Result<StopReason> {
        let TurnCollection {
            tool_batch_coordination,
            generated,
            attachments,
            recorder,
            context_turn_id,
            context_progress,
            end_turn_coordination,
            pause_check,
        } = collection;
        if context_turn_id.is_some() {
            crate::context::project_history(
                &mut session.messages,
                &session.meta.id,
                provider_and_model
                    .0
                    .model_capabilities(&provider_and_model.1)
                    .context_window as usize,
            );
        }
        let current_intent = session
            .messages
            .last()
            .filter(|m| m.role == openpaw_core::Role::User)
            .cloned();
        let mut original_references = std::collections::HashMap::<String, String>::new();
        let mut response_index = 0usize;
        let mut known_media = std::collections::HashMap::<String, openpaw_core::InlineMedia>::new();
        for media in attachments
            .iter()
            .cloned()
            .chain(openpaw_core::media::message_media(&session.messages))
        {
            if let Some(original) = known_media.get(&media.id) {
                if original.mime != media.mime || original.data != media.data {
                    return Err(Error::session(
                        "attachment ID conflicts with original content",
                    ));
                }
            }
            known_media.insert(media.id.clone(), media);
        }
        let (provider, model) = provider_and_model;
        // Engine 只持有调用方令牌的子令牌：外部取消会向下传播，但正常完成、
        // panic 或父 future 被丢弃时的内部清理不能反向污染 Host 的终态判断。
        let token = token.child_token();
        let _active_turn = self.begin_turn(token.clone())?;
        self.tool_host.set_media_context(attachments);
        let mut turn_media = openpaw_core::media::message_media(
            session
                .messages
                .last()
                .map(std::slice::from_ref)
                .unwrap_or(&[]),
        );
        self.tool_host
            .remember_media(openpaw_core::media::message_media(&session.messages));
        openpaw_core::media::validate_media(&turn_media, provider.model_capabilities(&model))?;

        let mut system = system;
        let mut refreshed_tools: Option<Vec<ToolSpec>> = None;
        loop {
            let tools = refreshed_tools.as_deref().unwrap_or(tools);
            // ① 检查取消
            if self.is_cancelled() {
                return Ok(StopReason::EndTurn);
            }

            // ③ 先消费当前预算，再按需扩展；耗尽后仅允许一次禁用工具的最终答复。
            // 固定预算不能调用 expand，因此必须先 try_consume，否则 `new(n)` 实际只会
            // 运行 grace 那一次。最终答复不占工具迭代预算，也不能再次请求工具。
            let normal_iteration =
                self.budget.try_consume() || (self.budget.expand(1) && self.budget.try_consume());
            let grace_iteration = !normal_iteration && self.budget.try_grace();
            if !(normal_iteration || grace_iteration) {
                return self
                    .finish_after_iteration_budget(
                        session,
                        system.as_deref(),
                        (&provider, &model),
                        event_sink,
                        generated,
                        (recorder.as_ref(), response_index),
                    )
                    .await;
            }

            // ④ 上下文压缩：只估算一次，根据结果决定是否通过单次摘要调用压缩。
            // （E2 语义见 docs/spikes/S1-agent-loop.md 第 2 节 ④。）
            if let Some(opts) = &self.compact_options {
                let mut opts = opts.clone();
                // 生产入口允许短历史压缩；仍以可保留的当前回合决定实际切点。
                opts.min_messages_before_compact = opts.min_messages_before_compact.min(2);
                opts.model_limit = opts
                    .model_limit
                    .min(provider.model_capabilities(&model).context_window as usize);
                let tools_json_approx = crate::context::tools_tokens(tools);
                let estimated = crate::compaction::estimate_session_tokens(
                    system.as_deref().unwrap_or(""),
                    session,
                    tools_json_approx,
                );
                let trigger = if opts.use_context_aware_buffer {
                    let available = crate::context::input_limit(opts.model_limit, self.max_tokens);
                    if estimated > available {
                        crate::compaction::CompactTrigger::HardLimit
                    } else if opts.trigger_proactive && estimated > available * 4 / 5 {
                        crate::compaction::CompactTrigger::Proactive
                    } else {
                        crate::compaction::CompactTrigger::None
                    }
                } else {
                    crate::compaction::should_compact_with_options_estimated(
                        session,
                        &opts,
                        estimated.saturating_add(self.max_tokens as usize),
                    )
                };
                let media_bytes: usize = openpaw_core::media::message_media(&session.messages)
                    .iter()
                    .map(|item| item.data.len())
                    .sum();
                if trigger != crate::compaction::CompactTrigger::None
                    || media_bytes > 24 * 1024 * 1024
                {
                    let before_usage = (session.total_input_tokens, session.total_output_tokens);
                    if let Some(progress) = &context_progress {
                        progress();
                    }
                    let summary_result = cancel_or(
                        &token,
                        crate::compaction::compact_session_for_turn(
                            &*provider,
                            &model,
                            session,
                            &opts,
                            current_intent.as_ref(),
                            &original_references,
                        ),
                    )
                    .await;
                    self.record_summary_cost(session, before_usage);
                    let compacted = match summary_result {
                        Some(result) => result?,
                        None => return Ok(StopReason::EndTurn),
                    };
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

            // ⑤ 构造模型请求
            let req = CompletionRequest {
                model: model.clone(),
                system: system.clone(),
                messages: session.messages.clone(),
                tools: tools.to_vec(),
                max_tokens: self.max_tokens,
                ..Default::default()
            };
            crate::context::validate_request(
                &req,
                provider.model_capabilities(&model).context_window as usize,
            )?;

            // ⑥ 带重试地读取模型流
            let resp = match self
                .stream_with_context(
                    &provider,
                    req,
                    session,
                    event_sink,
                    (recorder.as_ref(), response_index),
                    (
                        current_intent.as_ref(),
                        &original_references,
                        context_progress.as_ref(),
                    ),
                )
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if self.is_cancelled() {
                        return Ok(StopReason::EndTurn);
                    }
                    return Err(e);
                }
            };

            crate::turn_execution::observe(
                recorder.as_ref(),
                response_index,
                "response",
                serde_json::json!({"content":resp.content}),
            )?;
            // ⑦ 累加用量（饱和加法，见 Usage 文档）并更新 token 预算
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

            // 回填 assistant 消息（可能包含 tool_use 内容块）。
            let assistant = Message {
                role: openpaw_core::Role::Assistant,
                content: resp.content.clone(),
                ..Default::default()
            };
            let assistant = assistant.identified();
            generated.push(assistant.clone());
            session.messages.push(assistant);

            if resp.stop_reason != StopReason::ToolUse {
                if resp.stop_reason == StopReason::EndTurn
                    && !self.is_cancelled()
                    && !grace_iteration
                {
                    if let Some(coordinate) = &end_turn_coordination {
                        match cancel_or(&token, coordinate()).await {
                            Some(Some(fact)) => {
                                let fact = Message::user_text(fact).identified();
                                generated.push(fact.clone());
                                session.messages.push(fact);
                                response_index += 1;
                                continue;
                            }
                            None => return Ok(StopReason::EndTurn),
                            Some(None) => {}
                        }
                    }
                }
                // Token 预算继续：模型已停止，但仍有有效预算且收益未递减时，提示其继续工作。
                let nudge = if resp.stop_reason == StopReason::EndTurn
                    && !self.is_cancelled()
                    && self.end_turn_continuation.is_some()
                {
                    self.end_turn_continuation
                        .as_ref()
                        .and_then(|continuation| continuation())
                } else {
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
                    let nudge = Message::user_text(nudge).identified();
                    generated.push(nudge.clone());
                    session.messages.push(nudge);
                    response_index += 1;
                    continue;
                }
                return Ok(resp.stop_reason);
            }

            // ⑧ 收集 tool_use 内容块并分派工具
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
            let results = self
                .execute_tools(
                    tool_uses,
                    event_sink,
                    (recorder.as_ref(), response_index),
                    context_turn_id.as_deref(),
                )
                .await?;
            response_index += 1;
            let media: Vec<_> = results
                .iter()
                .flat_map(|block| {
                    if let ContentBlock::ToolResult { media, .. } = block {
                        media.clone()
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            for item in &media {
                if let Some(original) = known_media.get(&item.id) {
                    if original.mime != item.mime || original.data != item.data {
                        return Err(Error::session(
                            "attachment ID conflicts with original content",
                        ));
                    }
                } else {
                    known_media.insert(item.id.clone(), item.clone());
                }
                if !turn_media.iter().any(|previous| previous.id == item.id) {
                    turn_media.push(item.clone());
                }
            }
            openpaw_core::media::validate_media(&turn_media, provider.model_capabilities(&model))?;
            self.tool_host.remember_media(media);

            // ⑨ 将工具结果作为下一条 user 消息回填（tool_result 内容块使用 user 角色承载）。
            let tool_message = Message {
                role: openpaw_core::Role::User,
                content: results,
                ..Default::default()
            };
            let tool_message = tool_message.identified();
            if let Some(turn) = &context_turn_id {
                for block in &tool_message.content {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                        original_references.insert(
                            format!(
                                "{}:{tool_use_id}",
                                tool_message.uuid.as_deref().unwrap_or("")
                            ),
                            crate::context::record_reference(
                                &session.meta.id,
                                turn,
                                response_index - 1,
                                tool_use_id,
                            ),
                        );
                    }
                }
            }
            generated.push(tool_message.clone());
            let mut model_message = tool_message;
            if let Some(turn) = &context_turn_id {
                let window = provider.model_capabilities(&model).context_window as usize;
                let fixed = crate::compaction::estimate_messages_tokens(
                    system.as_deref().unwrap_or(""),
                    &session.messages,
                    crate::context::tools_tokens(tools),
                );
                let available =
                    crate::context::input_limit(window, self.max_tokens).saturating_sub(fixed);
                let mut preview_window = window;
                loop {
                    // 新增协作声明也占固定输入；只缩小模型副本，原件与引用保持完整。
                    crate::context::project_results(
                        &mut model_message.content,
                        &session.meta.id,
                        turn,
                        response_index - 1,
                        preview_window,
                    );
                    let cost = crate::compaction::estimate_messages_tokens(
                        "",
                        std::slice::from_ref(&model_message),
                        0,
                    );
                    if cost <= available || preview_window <= 1024 {
                        break;
                    }
                    preview_window = (preview_window / 2).max(1024);
                }
            }
            session.messages.push(model_message);
            if let Some(interaction) = tool_batch_coordination
                .as_ref()
                .and_then(|prepare| prepare())
                .filter(|_| {
                    !self.is_cancelled()
                        && (self.budget.remaining() > 0 || self.budget.expand(1))
                        && !self
                            .token_budget_mut()
                            .as_ref()
                            .is_some_and(|tracker| tracker.remaining_budget() == Some(0))
                })
            {
                self.publish_interaction(
                    session,
                    generated,
                    event_sink,
                    (recorder.as_ref(), response_index),
                    interaction.presentation,
                )?;
                response_index += 1;
                let Some(resolution) = cancel_or(&token, interaction.resolve).await else {
                    return Ok(StopReason::EndTurn);
                };
                match resolution? {
                    crate::turn_execution::TurnInteractionResolution::Continue {
                        instruction,
                        tools,
                        system: latest_system,
                    } => {
                        let fact = Message::user_text(instruction).identified();
                        generated.push(fact.clone());
                        session.messages.push(fact);
                        refreshed_tools = Some(tools);
                        system = latest_system;
                        continue;
                    }
                    crate::turn_execution::TurnInteractionResolution::Pause(reason) => {
                        self.publish_interaction(
                            session,
                            generated,
                            event_sink,
                            (recorder.as_ref(), response_index),
                            reason,
                        )?;
                        return Ok(StopReason::EndTurn);
                    }
                }
            }
            if let Some(reason) = pause_check.as_ref().and_then(|check| check()) {
                // 暂停只阻断依赖模型；已经启动的独立分支仍归本轮，先处理真实结果。
                let mut child_facts = Vec::new();
                if let Some(coordinate) = &end_turn_coordination {
                    loop {
                        match cancel_or(&token, coordinate()).await {
                            Some(Some(fact)) => {
                                let message = Message::user_text(&fact).identified();
                                generated.push(message.clone());
                                session.messages.push(message);
                                child_facts.push(fact);
                            }
                            Some(None) => break,
                            None => return Ok(StopReason::EndTurn),
                        }
                    }
                }
                let mut text = format!("执行已暂停：{reason}。请在原聊天明确继续。");
                if !child_facts.is_empty() {
                    text.push_str("\n已启动的独立子任务已收尾，实际完成、失败或暂停情况如下：\n");
                    text.push_str(&child_facts.join("\n\n"));
                }
                let message = Message::assistant_text(text);
                event_sink(AgentEvent::TextDelta {
                    text: message.text(),
                });
                generated.push(message.clone());
                session.messages.push(message);
                return Ok(StopReason::EndTurn);
            }
        }
    }

    fn publish_interaction(
        &self,
        session: &mut openpaw_core::Session,
        generated: &mut Vec<Message>,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
        text: String,
    ) -> Result<()> {
        let message = Message::assistant_text(&text).identified();
        for (kind, payload) in [
            ("response_start", serde_json::json!({})),
            ("text", serde_json::json!({"text":text})),
            ("response", serde_json::json!({"content":message.content})),
        ] {
            crate::turn_execution::observe(recording.0, recording.1, kind, payload)?;
        }
        generated.push(message.clone());
        session.messages.push(message);
        sink(AgentEvent::MessageStart);
        sink(AgentEvent::TextDelta { text });
        Ok(())
    }

    async fn finish_after_iteration_budget(
        &self,
        session: &mut openpaw_core::Session,
        system: Option<&str>,
        provider_and_model: (&Arc<dyn LlmProvider>, &str),
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        generated: &mut Vec<Message>,
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
    ) -> Result<StopReason> {
        let (provider, model) = provider_and_model;
        let instruction = "工具调用轮次已达到上限。工具现已禁用。请仅依据已有消息和工具结果给出有效最终答复，遵循用户指定的语言和输出格式，如实说明必要的限制；不得再请求或声称调用任何工具。";
        let final_system = match system {
            Some(value) if !value.trim().is_empty() => format!("{value}\n\n{instruction}"),
            _ => instruction.to_string(),
        };
        let req = CompletionRequest {
            model: model.to_string(),
            system: Some(final_system),
            messages: session.messages.clone(),
            tools: Vec::new(),
            max_tokens: self.max_tokens,
            ..Default::default()
        };
        crate::context::validate_request(
            &req,
            provider.model_capabilities(model).context_window as usize,
        )?;
        // 预算耗尽后的候选答复先缓冲，校验通过后才发布给 Host。部分 OpenAI
        // 兼容模型会把工具调用退化为 DSML 正文；若边生成边转发，协议标记会被
        // UI 当成最终答复永久保存。
        let mut buffered_events = Vec::new();
        let mut resp = {
            let mut buffered_sink = |event| buffered_events.push(event);
            self.stream_with_context(
                provider,
                req.clone(),
                session,
                &mut buffered_sink,
                (None, 0),
                (None, &Default::default(), None),
            )
            .await?
        };
        let mut total_usage = resp.usage;

        if response_contains_dsml_tool_call(&resp) {
            let mut repair_messages = req.messages.clone();
            repair_messages.push(Message {
                role: openpaw_core::Role::Assistant,
                content: resp.content,
                ..Default::default()
            });
            repair_messages.push(Message::user_text(
                "上一份候选答复错误地输出了 DSML/工具调用协议。工具已经禁用，请不要更新任务状态或输出DSML工具调用协议。现在依据已有结果给出有效最终答复，保持用户要求的格式；合法JSON、XML或代码内容可以正常输出。",
            ));
            let repair_req = CompletionRequest {
                messages: repair_messages,
                ..req
            };
            crate::context::validate_request(
                &repair_req,
                provider.model_capabilities(model).context_window as usize,
            )?;
            buffered_events.clear();
            resp = {
                let mut buffered_sink = |event| buffered_events.push(event);
                self.stream_with_context(
                    provider,
                    repair_req,
                    session,
                    &mut buffered_sink,
                    (None, 0),
                    (None, &Default::default(), None),
                )
                .await?
            };
            total_usage = add_usage(total_usage, resp.usage);
        }

        let has_final_text = resp
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()));
        if resp.stop_reason == StopReason::ToolUse
            || !has_final_text
            || response_contains_dsml_tool_call(&resp)
        {
            return Err(Error::Provider(
                "工具循环达到迭代上限后未返回可展示的最终答复".into(),
            ));
        }

        crate::turn_execution::observe(
            recording.0,
            recording.1,
            "response_start",
            serde_json::json!({}),
        )?;
        crate::turn_execution::observe(
            recording.0,
            recording.1,
            "response",
            serde_json::json!({"content":resp.content}),
        )?;
        for event in buffered_events {
            event_sink(event);
        }

        let usage = total_usage;
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
                tracker.record_turn(usage.input_tokens.saturating_add(usage.output_tokens) as u64);
            }
        }
        let final_message = Message {
            role: openpaw_core::Role::Assistant,
            content: resp.content,
            ..Default::default()
        }
        .identified();
        generated.push(final_message.clone());
        session.messages.push(final_message);
        Ok(resp.stop_reason)
    }

    /// 功能：以分类重试策略读取一次 completion 流。
    /// 状态：文本增量实时转发到回调，在 `Final` 到达时返回完整响应。
    #[allow(clippy::too_many_arguments)] // 模型重试沿用同一会话、记录器和当前回合原件引用。
    async fn stream_with_context(
        &self,
        provider: &Arc<dyn LlmProvider>,
        mut request: CompletionRequest,
        session: &mut openpaw_core::Session,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
        continuation: (
            Option<&Message>,
            &std::collections::HashMap<String, String>,
            Option<&crate::turn_execution::ContextProgress>,
        ),
    ) -> Result<openpaw_core::CompletionResponse> {
        match self
            .stream_with_retry(provider, request.clone(), sink, recording)
            .await
        {
            Err(error) if error.is_context_too_long() => {
                let Some(mut options) = self.compact_options.clone() else {
                    return Err(error);
                };
                options.model_limit = options
                    .model_limit
                    .min(provider.model_capabilities(&request.model).context_window as usize);
                let suffix = request
                    .messages
                    .get(session.messages.len()..)
                    .unwrap_or_default()
                    .to_vec();
                let before = (session.total_input_tokens, session.total_output_tokens);
                if let Some(progress) = continuation.2 {
                    progress();
                }
                let token = self.current_cancel_token();
                let summary_result = cancel_or(
                    &token,
                    crate::compaction::compact_session_for_turn(
                        &**provider,
                        &request.model,
                        session,
                        &options,
                        continuation.0,
                        continuation.1,
                    ),
                )
                .await;
                self.record_summary_cost(session, before);
                let count = match summary_result {
                    Some(result) => result?,
                    None => return Err(Error::Provider("整理期间已取消".into())),
                };
                if count == 0 {
                    return Err(error);
                }
                request.messages = session.messages.clone();
                request.messages.extend(suffix);
                crate::context::validate_request(&request, options.model_limit)?;
                // 只对同一逻辑模型请求恢复一次；内部网络退避不能重新进入此分支。
                self.stream_with_retry(provider, request, sink, recording)
                    .await
            }
            result => result,
        }
    }

    async fn stream_with_retry(
        &self,
        provider: &Arc<dyn LlmProvider>,
        req: CompletionRequest,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
    ) -> Result<openpaw_core::CompletionResponse> {
        let has_media = !openpaw_core::media::message_media(&req.messages).is_empty();
        let mut attempts: u32 = 0;
        'outer: loop {
            let token = self.current_cancel_token();
            let stream_result = tokio::select! {
                biased;
                _ = token.cancelled() => {
                    return Err(Error::Provider("stream cancelled by user".into()));
                }
                result = tokio::time::timeout(self.stream_timeout, provider.stream(req.clone())) => {
                    result.unwrap_or_else(|_| Err(Error::timeout("provider stream connection timed out")))
                }
            };
            let stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    let e = if has_media {
                        openpaw_core::media::redact_error(e)
                    } else {
                        e
                    };
                    let class = classify_api_error(&e);
                    let retry_limit =
                        retry_limit_for_error(&e, class, self.retry_policy.max_retries as u32);
                    if matches!(
                        class,
                        openpaw_core::ApiErrorClass::RetryableTransient
                            | openpaw_core::ApiErrorClass::RetryableOverloaded
                            | openpaw_core::ApiErrorClass::FallbackCandidate
                    ) && attempts < retry_limit
                    {
                        attempts += 1;
                        let retry_after = match &e {
                            Error::RateLimited {
                                retry_after_secs: Some(secs),
                                ..
                            } => Some(*secs),
                            _ => None,
                        };
                        let delay = self
                            .retry_policy
                            .delay_for_attempt_with_jitter(attempts as usize, retry_after);
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = token.cancelled() => {
                                return Err(Error::Provider("stream cancelled by user".into()));
                            }
                        }
                        continue;
                    }
                    return Err(e);
                }
            };

            futures::pin_mut!(stream);
            let mut final_resp: Option<openpaw_core::CompletionResponse> = None;
            let mut message_start_emitted = false;
            let mut saw_text_delta = false;
            let mut saw_thinking_delta = false;
            let mut emitted_visible_output = false;
            let mut pending_thinking = String::new();

            loop {
                let next = tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        return Err(Error::Provider("stream cancelled by user".into()));
                    }
                    result = tokio::time::timeout(self.stream_timeout, stream.next()) => result,
                };
                let ev = match next {
                    Ok(Some(Ok(ev))) => ev,
                    Ok(Some(Err(error))) => {
                        let error = if has_media {
                            openpaw_core::media::redact_error(error)
                        } else {
                            error
                        };
                        if emitted_visible_output && error.is_context_too_long() {
                            return Err(Error::Provider(
                                "已输出部分内容后发生上下文超限，拒绝重放请求".into(),
                            ));
                        }
                        let class = classify_api_error(&error);
                        let retry_limit = retry_limit_for_error(
                            &error,
                            class,
                            self.retry_policy.max_retries as u32,
                        );
                        if !emitted_visible_output
                            && (matches!(
                                class,
                                openpaw_core::ApiErrorClass::RetryableTransient
                                    | openpaw_core::ApiErrorClass::RetryableOverloaded
                                    | openpaw_core::ApiErrorClass::FallbackCandidate
                            ) || retryable_stream_protocol_error(&error))
                            && attempts < retry_limit
                        {
                            attempts += 1;
                            let retry_after = match &error {
                                Error::RateLimited {
                                    retry_after_secs: Some(secs),
                                    ..
                                } => Some(*secs),
                                _ => None,
                            };
                            let delay = self
                                .retry_policy
                                .delay_for_attempt_with_jitter(attempts as usize, retry_after);
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => {
                                    return Err(Error::Provider("stream cancelled by user".into()));
                                }
                                _ = tokio::time::sleep(delay) => {}
                            }
                            continue 'outer;
                        }
                        return Err(error);
                    }
                    Ok(None) => break,
                    Err(_) => {
                        let timeout = Error::timeout("provider stream frame timed out");
                        if !emitted_visible_output
                            && attempts < self.retry_policy.max_retries as u32
                        {
                            attempts += 1;
                            let delay = self.retry_policy.delay_for_attempt(attempts as usize);
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => {
                                    return Err(Error::Provider("stream cancelled by user".into()));
                                }
                                _ = tokio::time::sleep(delay) => {}
                            }
                            continue 'outer;
                        }
                        return Err(timeout);
                    }
                };
                match ev {
                    // OpenAI 兼容网关可能先发空角色帧，再通过 SSE 返回可重试错误。
                    // 暂存 MessageStart，直到出现可见内容或 Final，避免空帧阻断安全重试。
                    openpaw_core::StreamEvent::MessageStart => {}
                    openpaw_core::StreamEvent::TextDelta { text, .. } => {
                        if text.is_empty() {
                            continue;
                        }
                        if !message_start_emitted {
                            message_start_emitted = true;
                            crate::turn_execution::observe(
                                recording.0,
                                recording.1,
                                "response_start",
                                serde_json::json!({}),
                            )?;
                            event_sink(AgentEvent::MessageStart);
                        }
                        if !pending_thinking.is_empty() {
                            crate::turn_execution::observe(
                                recording.0,
                                recording.1,
                                "reasoning",
                                serde_json::json!({"text":pending_thinking}),
                            )?;
                            event_sink(AgentEvent::ThinkingDelta {
                                text: std::mem::take(&mut pending_thinking),
                            });
                        }
                        saw_text_delta = true;
                        emitted_visible_output = true;
                        crate::turn_execution::observe(
                            recording.0,
                            recording.1,
                            "text",
                            serde_json::json!({"text":text}),
                        )?;
                        event_sink(AgentEvent::TextDelta { text });
                    }
                    openpaw_core::StreamEvent::ThinkingDelta { text, .. } => {
                        if text.is_empty() {
                            continue;
                        }
                        saw_thinking_delta = true;
                        pending_thinking.push_str(&text);
                    }
                    // E1：工具内容块在 `Final` 中完整到达后再组装；流式期间提前分派属于 E2
                    // 的 `streaming_tool_executor` 评估范围。
                    openpaw_core::StreamEvent::ToolUseStart { .. }
                    | openpaw_core::StreamEvent::ToolUseInputDelta { .. }
                    | openpaw_core::StreamEvent::BlockStop { .. } => {}
                    openpaw_core::StreamEvent::Final(resp) => {
                        if resp.stop_reason == StopReason::MaxTokens {
                            return Err(Error::Provider(format!(
                                "max_output_tokens exceeded before a complete response（limit={}）",
                                req.max_tokens
                            )));
                        }
                        // provider 可能返回合法的 Final/finish_reason，但 content 为空（或只有
                        // thinking）。这不是可完成的用户回合：继续写入会产生“completed + 空
                        // assistant”，让 Host 与 Gateway 都无法投影有效回复。无可见输出时
                        // 按瞬时协议故障重试；已经发出可见增量后则直接失败，避免重复显示。
                        let has_actionable_content = resp.content.iter().any(|block| {
                            matches!(block, ContentBlock::ToolUse { .. })
                                || matches!(block, ContentBlock::Text { text } if !text.trim().is_empty())
                        });
                        if !has_actionable_content {
                            if !emitted_visible_output
                                && attempts < self.retry_policy.max_retries as u32
                            {
                                attempts += 1;
                                let delay = self.retry_policy.delay_for_attempt(attempts as usize);
                                tokio::select! {
                                    biased;
                                    _ = token.cancelled() => {
                                        return Err(Error::Provider("stream cancelled by user".into()));
                                    }
                                    _ = tokio::time::sleep(delay) => {}
                                }
                                continue 'outer;
                            }
                            return Err(Error::Provider(
                                "OpenAI 流返回空响应，拒绝生成成功响应".into(),
                            ));
                        }
                        // 某些 provider 只为非流式响应发送 Final；保持 AgentEvent 契约一致，
                        // 使 Host 适配器能为每个模型响应分配独立消息编号。
                        if !message_start_emitted {
                            crate::turn_execution::observe(
                                recording.0,
                                recording.1,
                                "response_start",
                                serde_json::json!({}),
                            )?;
                            event_sink(AgentEvent::MessageStart);
                        }
                        if !pending_thinking.is_empty() {
                            crate::turn_execution::observe(
                                recording.0,
                                recording.1,
                                "reasoning",
                                serde_json::json!({"text":pending_thinking}),
                            )?;
                            event_sink(AgentEvent::ThinkingDelta {
                                text: std::mem::take(&mut pending_thinking),
                            });
                        }
                        // 只发送 Final 的 provider 也要产生与真实流式 provider 相同的可观察文本/思考流，
                        // 并以随后的 TurnUsage 事件作为响应结束边界。
                        for block in &resp.content {
                            match block {
                                ContentBlock::Text { text } if !saw_text_delta => {
                                    crate::turn_execution::observe(
                                        recording.0,
                                        recording.1,
                                        "text",
                                        serde_json::json!({"text":text}),
                                    )?;
                                    event_sink(AgentEvent::TextDelta { text: text.clone() });
                                }
                                ContentBlock::Thinking { thinking, .. } if !saw_thinking_delta => {
                                    crate::turn_execution::observe(
                                        recording.0,
                                        recording.1,
                                        "reasoning",
                                        serde_json::json!({"text":thinking}),
                                    )?;
                                    event_sink(AgentEvent::ThinkingDelta {
                                        text: thinking.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                        final_resp = Some(*resp);
                        break;
                    }
                }
            }

            match final_resp {
                Some(resp) => return Ok(resp),
                None => {
                    // 流在没有 Final 的情况下结束（断流）：预算允许时重试，否则返回失败。
                    if !emitted_visible_output && attempts < self.retry_policy.max_retries as u32 {
                        attempts += 1;
                        let delay = self.retry_policy.delay_for_attempt(attempts as usize);
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                return Err(Error::Provider("stream cancelled by user".into()));
                            }
                            _ = tokio::time::sleep(delay) => {}
                        }
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
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
        turn_id: Option<&str>,
    ) -> Result<Vec<ContentBlock>> {
        if tool_uses.is_empty() {
            return Ok(Vec::new());
        }
        // 提问批次按模型顺序结算，后续调用才能检查原问题的可信暂停事实。
        let asks_user = tool_uses
            .iter()
            .any(|(_, name, _)| name == "request_user_input");
        if self.parallel_tools && tool_uses.len() > 1 && !asks_user {
            self.execute_tools_phased(tool_uses, event_sink, recording, turn_id)
                .await
        } else {
            self.execute_tools_sequential(tool_uses, event_sink, recording, turn_id)
                .await
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
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
        turn_id: Option<&str>,
    ) -> Result<Vec<ContentBlock>> {
        let ctx = self.check_context();
        let cancel = self.current_cancel_token();

        // ── Phase 1: classify ──────────────────────────────────────────
        let mut denied: Vec<ContentBlock> = Vec::new();
        let mut safe: Vec<ToolUseCall> = Vec::new();
        let mut confirm: Vec<(String, String, serde_json::Value, String)> = Vec::new();

        for (id, name, input) in tool_uses {
            event_sink(AgentEvent::ClassifierStatus {
                tool_use_id: id.clone(),
                status: ClassifierStatus::Checking,
            });
            let prepared = self.prepare_tool_input(&name, input, &ctx, &cancel).await;
            let (input, decision) = match prepared {
                PreparedToolInput::Classified { input, decision } => (input, decision),
                PreparedToolInput::Rejected(outcome) => {
                    denied.push(tool_result_block(&id, &name, &outcome));
                    record_tool_result(recording, &id, &name, &outcome, None, false, false)?;
                    event_sink(tool_result_event(&id, &name, &outcome, None));
                    continue;
                }
                PreparedToolInput::Cancelled => {
                    let outcome = ToolCallOutcome::error("cancelled by user");
                    denied.push(tool_result_block(&id, &name, &outcome));
                    record_tool_result(recording, &id, &name, &outcome, None, false, true)?;
                    event_sink(tool_result_event(&id, &name, &outcome, None));
                    continue;
                }
            };

            match decision {
                GuardrailDecision::Deny(reason) => {
                    event_sink(AgentEvent::ClassifierStatus {
                        tool_use_id: id.clone(),
                        status: ClassifierStatus::AutoDenied {
                            reason: reason.clone(),
                        },
                    });
                    let outcome = ToolCallOutcome::error(format!("blocked by guardrail: {reason}"));
                    record_tool_result(recording, &id, &name, &outcome, None, false, false)?;
                    event_sink(tool_result_event(&id, &name, &outcome, None));
                    denied.push(tool_result_block(&id, &name, &outcome));
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
                        status: ClassifierStatus::SuggestConfirm {
                            reason: reason.clone(),
                        },
                    });
                    confirm.push((id, name, input, reason));
                }
            }
        }

        // ── Phase 2: run safe calls concurrently ───────────────────────
        let mut results: Vec<ContentBlock> = denied;
        if !safe.is_empty() {
            // tokio::spawn per call: real concurrency even for tools whose
            // hosts block their executor thread (upstream parity).
            let host = Arc::clone(&self.tool_host);
            let hook_chain = self.hook_chain.clone();
            let mut tasks = tokio::task::JoinSet::new();
            let mut metadata = Vec::with_capacity(safe.len());
            for (index, (id, name, input)) in safe.into_iter().enumerate() {
                crate::turn_execution::observe(
                    recording.0,
                    recording.1,
                    "tool_start",
                    serde_json::json!({"call_id":id,"name":name,"input":input,"authorization":"approved"}),
                )?;
                event_sink(AgentEvent::ToolExecutionStart {
                    tool_use_id: id.clone(),
                    name: name.clone(),
                    input: observable_tool_input(&name, &input),
                });
                let started = Instant::now();
                metadata.push((id.clone(), name.clone(), started));
                let host = Arc::clone(&host);
                let invocation = openpaw_core::ToolInvocationContext {
                    turn_id: turn_id.map(str::to_owned),
                    response_index: recording.1,
                    tool_call_id: id.clone(),
                };
                let hooks = hook_chain.clone();
                let cancel = cancel.clone();
                tasks.spawn(async move {
                    let mut interrupted = false;
                    let outcome = std::panic::AssertUnwindSafe(execute_prepared_cancelled(
                        hooks.as_deref(),
                        &*host,
                        &name,
                        input,
                        &invocation,
                        &cancel,
                        &mut interrupted,
                    ))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        interrupted = true;
                        ToolCallOutcome::error("tool task panicked")
                    });
                    (index, outcome, interrupted)
                });
            }
            let mut completed: Vec<Option<(ToolCallOutcome, bool)>> =
                (0..metadata.len()).map(|_| None).collect();
            let mut cancelled = false;
            while !tasks.is_empty() {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        cancelled = true;
                        tasks.abort_all();
                        break;
                    }
                    joined = tasks.join_next() => {
                        let Some(joined) = joined else {
                            break;
                        };
                        if let Ok((index, outcome, interrupted)) = joined {
                            let (id, name, started) = &metadata[index];
                            record_tool_result(recording, id, name, &outcome, Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)), true, interrupted)?;
                            completed[index] = Some((outcome, interrupted));
                        }
                    }
                }
            }
            drop(tasks);
            for (index, (id, name, started)) in metadata.into_iter().enumerate() {
                let has_result = completed[index].is_some();
                let (outcome, interrupted) = completed[index].take().unwrap_or_else(|| {
                    if cancelled {
                        (ToolCallOutcome::error("cancelled by user"), true)
                    } else {
                        (ToolCallOutcome::error("tool task failed"), true)
                    }
                });
                let duration_ms =
                    Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                if !has_result {
                    record_tool_result(
                        recording,
                        &id,
                        &name,
                        &outcome,
                        duration_ms,
                        true,
                        interrupted,
                    )?;
                }
                event_sink(AgentEvent::ToolResult {
                    media: outcome.media.clone(),
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
                    media: outcome.media,
                });
            }
        }

        // ── 阶段 3：在审批门后串行执行确认调用 ─────────────────────────
        for (id, name, input, reason) in confirm {
            let invocation = openpaw_core::ToolInvocationContext {
                turn_id: turn_id.map(str::to_owned),
                response_index: recording.1,
                tool_call_id: id.clone(),
            };
            let started = Instant::now();
            let mut authorized = false;
            let mut interrupted = false;
            let response = cancel_or(
                &cancel,
                self.approval
                    .request(ApprovalRequest::new(name.clone(), input.clone(), reason)),
            )
            .await;
            let outcome = match response {
                None => {
                    interrupted = true;
                    ToolCallOutcome::error("cancelled by user")
                }
                Some(openpaw_core::ApprovalResponse::Approved) => {
                    crate::turn_execution::observe(
                        recording.0,
                        recording.1,
                        "tool_start",
                        serde_json::json!({"call_id":id,"name":name,"input":input,"authorization":"approved"}),
                    )?;
                    authorized = true;
                    event_sink(AgentEvent::ToolExecutionStart {
                        tool_use_id: id.clone(),
                        name: name.clone(),
                        input: observable_tool_input(&name, &input),
                    });
                    execute_prepared_cancelled(
                        self.hook_chain.as_deref(),
                        &*self.tool_host,
                        &name,
                        input,
                        &invocation,
                        &cancel,
                        &mut interrupted,
                    )
                    .await
                }
                Some(openpaw_core::ApprovalResponse::Denied { reason }) => {
                    ToolCallOutcome::error(format!("denied by user: {reason}"))
                }
            };
            let duration_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            record_tool_result(
                recording,
                &id,
                &name,
                &outcome,
                duration_ms,
                authorized,
                interrupted,
            )?;
            event_sink(AgentEvent::ToolResult {
                media: outcome.media.clone(),
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
                media: outcome.media,
            });
        }

        Ok(results)
    }

    /// Serial path: validate → classify → approve → call, one tool at a
    /// time. A failure produces an error result, never aborts the batch.
    async fn execute_tools_sequential(
        &self,
        tool_uses: Vec<ToolUseCall>,
        event_sink: &mut (dyn FnMut(AgentEvent) + Send),
        recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
        turn_id: Option<&str>,
    ) -> Result<Vec<ContentBlock>> {
        let ctx = self.check_context();
        let cancel = self.current_cancel_token();
        let mut results = Vec::with_capacity(tool_uses.len());

        for (id, name, input) in tool_uses {
            let invocation = openpaw_core::ToolInvocationContext {
                turn_id: turn_id.map(str::to_owned),
                response_index: recording.1,
                tool_call_id: id.clone(),
            };
            let started = Instant::now();
            let mut authorized = false;
            let mut interrupted = false;
            event_sink(AgentEvent::ClassifierStatus {
                tool_use_id: id.clone(),
                status: ClassifierStatus::Checking,
            });
            let prepared = self.prepare_tool_input(&name, input, &ctx, &cancel).await;
            let (input, decision) = match prepared {
                PreparedToolInput::Classified { input, decision } => (input, decision),
                PreparedToolInput::Rejected(outcome) => {
                    record_tool_result(recording, &id, &name, &outcome, None, false, false)?;
                    event_sink(tool_result_event(&id, &name, &outcome, None));
                    results.push(tool_result_block(&id, &name, &outcome));
                    continue;
                }
                PreparedToolInput::Cancelled => {
                    let outcome = ToolCallOutcome::error("cancelled by user");
                    record_tool_result(recording, &id, &name, &outcome, None, false, true)?;
                    event_sink(tool_result_event(&id, &name, &outcome, None));
                    results.push(tool_result_block(&id, &name, &outcome));
                    continue;
                }
            };

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
                    let response = cancel_or(
                        &cancel,
                        self.approval.request(ApprovalRequest::new(
                            name.clone(),
                            input.clone(),
                            reason,
                        )),
                    )
                    .await;
                    match response {
                        None => {
                            interrupted = true;
                            ToolCallOutcome::error("cancelled by user")
                        }
                        Some(openpaw_core::ApprovalResponse::Approved) => {
                            crate::turn_execution::observe(
                                recording.0,
                                recording.1,
                                "tool_start",
                                serde_json::json!({"call_id":id,"name":name,"input":input,"authorization":"approved"}),
                            )?;
                            authorized = true;
                            event_sink(AgentEvent::ToolExecutionStart {
                                tool_use_id: id.clone(),
                                name: name.clone(),
                                input: observable_tool_input(&name, &input),
                            });
                            execute_prepared_cancelled(
                                self.hook_chain.as_deref(),
                                &*self.tool_host,
                                &name,
                                input,
                                &invocation,
                                &cancel,
                                &mut interrupted,
                            )
                            .await
                        }
                        Some(openpaw_core::ApprovalResponse::Denied { reason }) => {
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
                    crate::turn_execution::observe(
                        recording.0,
                        recording.1,
                        "tool_start",
                        serde_json::json!({"call_id":id,"name":name,"input":input,"authorization":"approved"}),
                    )?;
                    authorized = true;
                    event_sink(AgentEvent::ToolExecutionStart {
                        tool_use_id: id.clone(),
                        name: name.clone(),
                        input: observable_tool_input(&name, &input),
                    });
                    execute_prepared_cancelled(
                        self.hook_chain.as_deref(),
                        &*self.tool_host,
                        &name,
                        input,
                        &invocation,
                        &cancel,
                        &mut interrupted,
                    )
                    .await
                }
            };

            let duration_ms =
                Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
            record_tool_result(
                recording,
                &id,
                &name,
                &outcome,
                duration_ms,
                authorized,
                interrupted,
            )?;
            event_sink(AgentEvent::ToolResult {
                media: outcome.media.clone(),
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
                media: outcome.media,
            });
        }

        Ok(results)
    }

    async fn prepare_tool_input(
        &self,
        name: &str,
        input: serde_json::Value,
        ctx: &CheckContext,
        cancel: &CancellationToken,
    ) -> PreparedToolInput {
        match cancel_or(cancel, self.tool_host.validate_input(name, &input)).await {
            None => return PreparedToolInput::Cancelled,
            Some(Err(reason)) => {
                return PreparedToolInput::Rejected(ToolCallOutcome::error(format!(
                    "invalid input: {reason}"
                )))
            }
            Some(Ok(())) => {}
        }

        let mut final_input = input;
        if let Some(chain) = self.hook_chain.as_deref() {
            if chain.hook_count(HookPoint::BeforeToolCall) > 0 {
                let payload =
                    serde_json::json!({ "tool_name": name, "input": final_input.clone() });
                match cancel_or(cancel, chain.run(HookPoint::BeforeToolCall, payload)).await {
                    None => return PreparedToolInput::Cancelled,
                    Some(HookDecision::Skip) => {
                        return PreparedToolInput::Rejected(ToolCallOutcome::error(
                            "blocked by hook",
                        ))
                    }
                    Some(HookDecision::Continue) => {}
                    Some(HookDecision::Replace(value)) => {
                        let Some(replacement) = value.get("input").cloned() else {
                            return PreparedToolInput::Rejected(ToolCallOutcome::error(
                                "invalid hook replacement: missing input",
                            ));
                        };
                        match cancel_or(cancel, self.tool_host.validate_input(name, &replacement))
                            .await
                        {
                            None => return PreparedToolInput::Cancelled,
                            Some(Err(reason)) => {
                                return PreparedToolInput::Rejected(ToolCallOutcome::error(
                                    format!("invalid input after hook: {reason}"),
                                ))
                            }
                            Some(Ok(())) => final_input = replacement,
                        }
                    }
                }
            }
        }

        match cancel_or(
            cancel,
            self.guardrail.check_with_context(name, &final_input, ctx),
        )
        .await
        {
            None => PreparedToolInput::Cancelled,
            Some(decision) => PreparedToolInput::Classified {
                input: final_input,
                decision,
            },
        }
    }
}

async fn cancel_or<F>(cancel: &CancellationToken, future: F) -> Option<F::Output>
where
    F: Future,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = future => Some(result),
    }
}

fn record_tool_result(
    recording: (Option<&crate::turn_execution::ExecutionRecorder>, usize),
    id: &str,
    name: &str,
    outcome: &ToolCallOutcome,
    duration_ms: Option<u64>,
    started: bool,
    interrupted: bool,
) -> Result<()> {
    let disposition = if interrupted {
        if started {
            "unknown"
        } else {
            "cancelled"
        }
    } else if !started {
        "denied"
    } else if outcome.is_error {
        "error"
    } else {
        "completed"
    };
    crate::turn_execution::observe(
        recording.0,
        recording.1,
        "tool_result",
        serde_json::json!({"call_id":id,"name":name,"outcome":outcome,"duration_ms":duration_ms,"disposition":disposition}),
    )
}

fn tool_result_event(
    id: &str,
    name: &str,
    outcome: &ToolCallOutcome,
    duration_ms: Option<u64>,
) -> AgentEvent {
    AgentEvent::ToolResult {
        media: outcome.media.clone(),
        tool_use_id: id.to_string(),
        content: outcome.content.clone(),
        is_error: outcome.is_error,
        name: Some(name.to_string()),
        duration_ms,
    }
}

/// 将活动事件限制为少量白名单字段，禁止携带文件正文、替换文本、命令和任意工具负载。
fn observable_tool_input(name: &str, input: &serde_json::Value) -> serde_json::Value {
    const MAX_FIELD_CHARS: usize = 512;
    let keys: &[&str] = match name {
        "web_search" => &["query"],
        "web_fetch" | "web_extract" => &["url"],
        "read_file" | "write_file" | "edit_file" | "notebook_edit" => &["path"],
        "grep" => &["pattern", "path"],
        "glob" => &["pattern"],
        _ => &[],
    };
    let mut projected = serde_json::Map::new();
    for key in keys {
        let Some(value) = input.get(*key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let prefix = value.chars().take(MAX_FIELD_CHARS).collect::<String>();
        projected.insert((*key).to_string(), serde_json::Value::String(prefix));
    }
    serde_json::Value::Object(projected)
}

fn tool_result_block(id: &str, name: &str, outcome: &ToolCallOutcome) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: outcome.content.clone(),
        is_error: outcome.is_error,
        name: Some(name.to_string()),
        protected: false,
        media: outcome.media.clone(),
    }
}

// 在工具 future 外层统一竞争取消信号，保证 Host 中止后不会继续等待阻塞工具或 After hook。
// `biased` 使已经就绪的取消优先返回；底层 future 被丢弃后不得再向本回合发布结果。
async fn execute_prepared_cancelled(
    hooks: Option<&HookChain>,
    host: &dyn ToolHost,
    name: &str,
    input: serde_json::Value,
    invocation: &openpaw_core::ToolInvocationContext,
    cancel: &CancellationToken,
    interrupted: &mut bool,
) -> ToolCallOutcome {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => { *interrupted = true; ToolCallOutcome::error("cancelled by user") },
        outcome = execute_prepared(hooks, host, name, input, invocation) => outcome,
    }
}

/// 功能：执行已通过最终输入校验、护栏和审批的工具调用，并应用 After hook。
/// 约束：Before hook 已在安全分类前运行，本函数不得再次改写输入或重复授权。
async fn execute_prepared(
    hook_chain: Option<&HookChain>,
    host: &dyn ToolHost,
    name: &str,
    input: serde_json::Value,
    invocation: &openpaw_core::ToolInvocationContext,
) -> ToolCallOutcome {
    let Some(chain) = hook_chain else {
        return sanitize_outcome(execute_one(host, name, input, invocation).await);
    };

    let mut outcome = execute_one(host, name, input.clone(), invocation).await;

    // AfterToolCall：观察者可以改写工具结果。
    if chain.hook_count(HookPoint::AfterToolCall) > 0 {
        let payload = serde_json::json!({
            "tool_name": name,
            "input": input,
            "result": {
                "content": outcome.content.clone(),
                "is_error": outcome.is_error,
                "media": outcome.media,
            },
        });
        if let HookDecision::Replace(val) = chain.run(HookPoint::AfterToolCall, payload).await {
            outcome.media = match val.pointer("/result/media") {
                Some(value) => match serde_json::from_value(value.clone()) {
                    Ok(media) => media,
                    Err(_) => return ToolCallOutcome::error("invalid media in hook replacement"),
                },
                None => Vec::new(),
            };
            if let Some(content) = val.pointer("/result/content").and_then(|c| c.as_str()) {
                outcome.content = content.to_string();
            }
            if let Some(is_error) = val.pointer("/result/is_error").and_then(|e| e.as_bool()) {
                outcome.is_error = is_error;
            }
        }
    }
    sanitize_outcome(outcome)
}

fn sanitize_outcome(mut outcome: ToolCallOutcome) -> ToolCallOutcome {
    if outcome.is_error {
        outcome.media.clear();
    }
    for media in &outcome.media {
        if let Err(error) = media.bytes() {
            return ToolCallOutcome::error(error.to_string());
        }
    }
    outcome
}

/// 功能：校验后调用 ToolHost，并把传输错误转换为错误结果，避免单个失败中止批次。
async fn execute_one(
    host: &dyn ToolHost,
    name: &str,
    input: serde_json::Value,
    invocation: &openpaw_core::ToolInvocationContext,
) -> ToolCallOutcome {
    match host.call_with_context(name, input, invocation).await {
        Ok(outcome) => outcome,
        Err(e) => ToolCallOutcome::error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn interaction_publishes_before_wait_and_preserves_budget_cancel_and_commit_failure() {
        use crate::turn_execution::{
            run_turn, TurnInteraction, TurnInteractionResolution, TurnOptions,
        };
        use openpaw_core::ToolExecutor;
        use openpaw_llm::{MockMode, MockProvider};
        for mode in ["continue", "pause", "cancel", "record-failure", "exhausted"] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("source.txt"), "real read").unwrap();
            let provider = Arc::new(MockProvider::scripted(vec![
                MockMode::ToolUse {
                    tool_name: "read_file".into(),
                    input: serde_json::json!({"path":"source.txt"}),
                    text: String::new(),
                },
                MockMode::Fixed("finished".into()),
            ]));
            let host = Arc::new(openpaw_tools::ToolRegistry::new(directory.path()));
            let tools = host.list_tools().await.unwrap();
            let agent = AgentLoop::new(provider.clone())
                .with_tool_host(host)
                .with_budget(IterationBudget::new(if mode == "exhausted" {
                    1
                } else {
                    2
                }));
            let mut session = Session::new("fixture", "fixture");
            session.messages.push(Message::user_text("read once"));
            let original = serde_json::to_value(&session).unwrap();
            let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let token = CancellationToken::new();
            let (observed, polled, cancel) = (published.clone(), waiting.clone(), token.clone());
            let interaction = Arc::new(move || {
                let (observed, polled, cancel) = (observed.clone(), polled.clone(), cancel.clone());
                Some(TurnInteraction {
                    presentation: "canonical plan".into(),
                    resolve: Box::pin(async move {
                        assert!(observed.load(Ordering::SeqCst), "等待前必须先可靠发布计划");
                        polled.store(true, Ordering::SeqCst);
                        if mode == "cancel" {
                            cancel.cancel();
                            std::future::pending::<()>().await;
                        }
                        Ok(if mode == "pause" {
                            TurnInteractionResolution::Pause("paused".into())
                        } else {
                            TurnInteractionResolution::Continue {
                                instruction: "actual accepted answer".into(),
                                tools: Vec::new(),
                                system: Some("current constraints".into()),
                            }
                        })
                    }),
                })
            });
            let recorder = Arc::new(move |fact: crate::turn_execution::ExecutionObservation| {
                if mode == "record-failure"
                    && fact.kind == "text"
                    && fact.payload["text"] == "canonical plan"
                {
                    Err(Error::session("fixture commit failure"))
                } else {
                    Ok(())
                }
            });
            let mut sink = |event| {
                if matches!(event,AgentEvent::TextDelta {ref text} if text=="canonical plan") {
                    published.store(true, Ordering::SeqCst);
                }
            };
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                run_turn(
                    &agent,
                    &mut session,
                    &tools,
                    &mut sink,
                    TurnOptions {
                        tool_batch_coordination: Some(interaction),
                        recorder: Some(recorder),
                        cancel: token,
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.result.is_err(), mode == "record-failure");
            assert_eq!(
                waiting.load(Ordering::SeqCst),
                !matches!(mode, "record-failure" | "exhausted")
            );
            assert_eq!(
                provider.call_count(),
                if matches!(mode, "pause" | "cancel" | "record-failure") {
                    1
                } else {
                    2
                },
                "{mode}: 交互不得重置迭代预算"
            );
            if matches!(mode, "cancel" | "record-failure") {
                assert_eq!(serde_json::to_value(&session).unwrap(), original);
            }
        }
    }

    #[tokio::test]
    async fn media_hook_replacements_are_validated_and_never_keep_stale_media() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("media.gif"), b"GIF89afixture")
            .await
            .unwrap();
        let host = openpaw_tools::ToolRegistry::new(workspace.path());
        let original =
            openpaw_core::InlineMedia::from_bytes(b"GIF89afixture", "image/gif", None).unwrap();
        for replacement in [
            serde_json::json!({"result":{"content":"redacted"}}),
            serde_json::json!({"result":{"is_error":true,"media":[original.clone()]}}),
            serde_json::json!({"result":{"media":[{"id":"bad","mime":"image/gif","data":"???"}]}}),
            serde_json::json!({"result":{"media":"wrong-shape"}}),
        ] {
            let mut chain = HookChain::new();
            chain.register(HookPoint::AfterToolCall, "media-replacement", move |_| {
                let replacement = replacement.clone();
                async move {
                    openpaw_plugins::HookResult {
                        decision: HookDecision::Replace(replacement),
                        modified_payload: None,
                    }
                }
            });
            let outcome = execute_prepared(
                Some(&chain),
                &host,
                "read_file",
                serde_json::json!({"path":"media.gif"}),
                &openpaw_core::ToolInvocationContext::default(),
            )
            .await;
            assert!(outcome.media.is_empty());
        }
    }
    use futures::stream::BoxStream;
    use openpaw_core::{CompletionResponse, LlmProvider, Role, Session, StreamEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EmptyFinalProvider {
        empty_calls: AtomicUsize,
        calls: AtomicUsize,
        stop_reason: StopReason,
    }

    struct ReasoningThenErrorProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ReasoningThenErrorProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            unreachable!("reasoning 重试回归测试只使用流式接口")
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Ok(Box::pin(futures::stream::iter([
                    Ok(StreamEvent::ThinkingDelta {
                        index: 0,
                        text: "discarded attempt".into(),
                    }),
                    Err(Error::Provider("stream ended without Final event".into())),
                ])));
            }
            Ok(Box::pin(futures::stream::iter([Ok(StreamEvent::Final(
                Box::new(CompletionResponse {
                    content: vec![ContentBlock::Text {
                        text: "recovered after reasoning".into(),
                    }],
                    stop_reason: StopReason::EndTurn,
                    usage: Default::default(),
                }),
            ))])))
        }

        fn capabilities(&self) -> openpaw_core::Capabilities {
            Default::default()
        }

        fn name(&self) -> &str {
            "reasoning-then-error"
        }

        fn default_model(&self) -> &str {
            "reasoning-then-error"
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for EmptyFinalProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            unreachable!("空响应回归测试只使用流式接口")
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut remaining = self.empty_calls.load(Ordering::SeqCst);
            let empty = loop {
                if remaining == 0 {
                    break false;
                }
                match self.empty_calls.compare_exchange_weak(
                    remaining,
                    remaining - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break true,
                    Err(current) => remaining = current,
                }
            };
            let content = (!empty)
                .then(|| ContentBlock::Text {
                    text: "recovered".into(),
                })
                .into_iter()
                .collect();
            let response = CompletionResponse {
                content,
                stop_reason: self.stop_reason,
                usage: Default::default(),
            };
            Ok(Box::pin(futures::stream::iter([Ok(StreamEvent::Final(
                Box::new(response),
            ))])))
        }

        fn capabilities(&self) -> openpaw_core::Capabilities {
            Default::default()
        }

        fn name(&self) -> &str {
            "empty-final"
        }

        fn default_model(&self) -> &str {
            "empty-final"
        }
    }

    async fn run_empty_final_case(empty_calls: usize) -> (Result<StopReason>, Session, usize) {
        let provider = Arc::new(EmptyFinalProvider {
            empty_calls: AtomicUsize::new(empty_calls),
            calls: AtomicUsize::new(0),
            stop_reason: StopReason::EndTurn,
        });
        let agent =
            AgentLoop::new(provider.clone()).with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
        let mut session = Session::new("empty-final", "empty-final");
        session.messages.push(Message::user_text("hi"));
        let result = agent.run_turn(&mut session, None, &[], &mut |_| {}).await;
        (result, session, provider.calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn empty_success_response_is_retried_before_completion() {
        let (result, session, calls) = run_empty_final_case(1).await;
        assert_eq!(result.unwrap(), StopReason::EndTurn);
        assert_eq!(calls, 2);
        assert_eq!(session.messages.last().unwrap().text(), "recovered");
    }

    #[tokio::test]
    async fn end_turn_continuation_keeps_original_budget_and_budget_final_is_terminal() {
        let provider = Arc::new(EmptyFinalProvider {
            empty_calls: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            stop_reason: StopReason::EndTurn,
        });
        let continuations = Arc::new(AtomicUsize::new(0));
        let count = continuations.clone();
        let agent = AgentLoop::new(provider.clone())
            .with_budget(IterationBudget::new(1))
            .with_end_turn_continuation(Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Some("继续剩余工作".into())
            }));
        let mut session = Session::new("bounded", "bounded");
        session.messages.push(Message::user_text("work"));
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            agent.run_turn(&mut session, None, &[], &mut |_| {}),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, StopReason::EndTurn);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            3,
            "一次预算、一次原grace、一次禁工具终答"
        );
        assert_eq!(
            continuations.load(Ordering::SeqCst),
            2,
            "预算终答不得再调用续行策略"
        );
    }

    #[tokio::test]
    async fn end_turn_continuation_never_runs_after_cancel_or_max_tokens() {
        for cancelled in [false, true] {
            let provider = Arc::new(EmptyFinalProvider {
                empty_calls: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                stop_reason: StopReason::MaxTokens,
            });
            let count = Arc::new(AtomicUsize::new(0));
            let observed = count.clone();
            let agent =
                AgentLoop::new(provider.clone()).with_end_turn_continuation(Arc::new(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Some("must not continue".into())
                }));
            let token = CancellationToken::new();
            if cancelled {
                token.cancel();
            }
            let mut session = Session::new("terminal", "terminal");
            session.messages.push(Message::user_text("work"));
            let result = agent
                .run_turn_with_cancel_token(&mut session, None, &[], &mut |_| {}, token)
                .await;
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                usize::from(!cancelled)
            );
            assert_eq!(result.is_err(), !cancelled);
        }
    }

    #[tokio::test]
    async fn exhausted_empty_success_responses_fail_without_empty_assistant() {
        let (result, session, calls) = run_empty_final_case(usize::MAX).await;
        assert!(matches!(result, Err(Error::Provider(message)) if message.contains("空响应")));
        assert_eq!(calls, 2);
        assert_eq!(session.messages.len(), 1);
        assert!(session
            .messages
            .iter()
            .all(|message| message.role != Role::Assistant || !message.content.is_empty()));
    }

    #[tokio::test]
    async fn reasoning_only_attempt_can_retry_without_leaking_partial_events() {
        let provider = Arc::new(ReasoningThenErrorProvider {
            calls: AtomicUsize::new(0),
        });
        let agent =
            AgentLoop::new(provider.clone()).with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
        let mut session = Session::new("reasoning-retry", "reasoning-then-error");
        session.messages.push(Message::user_text("hi"));
        let mut events = Vec::new();
        let result = agent
            .run_turn(&mut session, None, &[], &mut |event| events.push(event))
            .await;
        assert_eq!(result.unwrap(), StopReason::EndTurn);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(!events.iter().any(|event| matches!(
            event,
            AgentEvent::ThinkingDelta { text } if text.contains("discarded attempt")
        )));
        assert_eq!(
            session.messages.last().unwrap().text(),
            "recovered after reasoning"
        );
    }

    #[tokio::test]
    async fn max_tokens_is_not_persisted_as_a_completed_assistant_response() {
        let agent = AgentLoop::new(Arc::new(EmptyFinalProvider {
            empty_calls: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            stop_reason: StopReason::MaxTokens,
        }));
        let mut session = Session::new("max-tokens", "max-tokens");
        session
            .messages
            .push(Message::user_text("write a long answer"));
        let mut events = Vec::new();
        let result = agent
            .run_turn(&mut session, None, &[], &mut |event| events.push(event))
            .await;
        assert!(matches!(
            result,
            Err(Error::Provider(message)) if message.contains("max_output_tokens")
        ));
        assert_eq!(session.messages.len(), 1);
        assert!(events.is_empty());
    }
}
