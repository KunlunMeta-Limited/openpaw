//! 功能：为 CLI 与 Host 提供完整会话快照及受保护的回合执行。
//! 输入输出边界：使用既有 AgentLoop、Core 类型、取消令牌和事件回调。
//! 禁止承担：不写文件，不读取环境，不生成 HTTP/SSE/Office 或终端消息。
//! 关联：CHG-2026-0025。
use crate::{AgentEvent, AgentLoop};
use futures::FutureExt;
use openpaw_core::{LlmProvider, Session, StopReason, ToolSpec};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 功能：保存已接收输入之后、执行之前的完整会话状态。
#[derive(Clone)]
pub struct SessionCheckpoint(Session);

impl SessionCheckpoint {
    /// 功能：捕获完整会话快照。
    /// 参数：session 为调用方独占或已加锁的会话。
    /// 返回：包含消息、用量及压缩状态的检查点。
    /// 错误：无。
    /// 状态：只读，不修改会话。
    /// 并发与取消：同步复制，不持有共享锁。
    /// 权限：不记录或返回额外正文，不访问外部资源。
    /// 关联：CHG-2026-0025。
    pub fn capture(session: &Session) -> Self {
        Self(session.clone())
    }

    /// 功能：恢复执行前的完整会话状态。
    /// 参数：session 为需要恢复且由调用方独占的会话。
    /// 返回：无。
    /// 错误：无。
    /// 状态：替换消息、用量、压缩及其他会话字段，保留检查点中的已接收输入。
    /// 并发与取消：同步恢复；调用方负责与接收和提交线性化。
    /// 权限：不撤销工具已完成的外部操作。
    /// 关联：CHG-2026-0025。
    pub fn restore(&self, session: &mut Session) {
        *session = self.0.clone();
    }
}

/// 功能：保存一次调用的中立配置，不改变 AgentLoop 默认模型。
#[derive(Default)]
pub struct TurnOptions {
    /// 功能：整理开始时更新入口既有状态，不接收摘要或原件正文。
    pub context_progress: Option<ContextProgress>,
    /// 功能：可信入口选择只整理历史，不执行业务模型或工具。
    pub compact_only: bool,
    /// 功能：入口提供的可靠回合身份；存在时为工具模型副本生成原件引用。
    pub context_turn_id: Option<String>,
    /// 本回合使用的可选系统提示。
    pub system: Option<String>,
    /// 调用方拥有的取消令牌，内部清理不反向取消它。
    pub cancel: CancellationToken,
    /// 功能：当前有效完整历史的附件快照，用于压缩后的会话内回看。
    pub attachments: Vec<openpaw_core::InlineMedia>,
    /// 本回合可选 Provider 与模型。
    pub provider_and_model: Option<(Arc<dyn LlmProvider>, String)>,
    /// 可靠执行记录回调；失败立即中止回合，不继续未登记的工具副作用。
    pub recorder: Option<ExecutionRecorder>,
}

/// 功能：只报告整理开始的中立状态回调，不携带敏感内容。
pub type ContextProgress = Arc<dyn Fn() + Send + Sync>;

/// 功能：描述与传输、存储无关的真实可见输出和工具执行事实。
/// 约束：包含授权会话的完整正文，不得发送到诊断日志或 Office 动作。
#[derive(Clone)]
pub struct ExecutionObservation {
    /// 本回合模型响应序号，与原消息事件顺序一致。
    pub response_index: usize,
    /// 登记的事实类型：response_start、text、response、tool_start、tool_result。
    pub kind: String,
    /// 类型对应的完整事实，工具事实始终携带 call_id。
    pub payload: serde_json::Value,
}

/// 功能：由入口提供的同步可靠记录器；每次成功表示该事实已经提交。
pub type ExecutionRecorder =
    Arc<dyn Fn(ExecutionObservation) -> openpaw_core::Result<()> + Send + Sync>;

/// 功能：在可见发布或工具副作用之前可靠登记中立事实。
/// 参数：recorder 为入口记录器，response_index、kind、payload 描述当前事实。
/// 返回：成功为空。
/// 错误：记录器错误立即向回合传播，禁止继续执行。
/// 状态：默认未配置时保持原行为；不自行写盘。
/// 并发与取消：同步回调；调用方负责 writer 所有权和顺序。
/// 权限：完整内容仅供授权会话记录，不发送日志或 Office。
/// 关联：CHG-2026-0039、TurnOptions。
pub(crate) fn observe(
    recorder: Option<&ExecutionRecorder>,
    response_index: usize,
    kind: &str,
    payload: serde_json::Value,
) -> openpaw_core::Result<()> {
    if let Some(recorder) = recorder {
        recorder(ExecutionObservation {
            response_index,
            kind: kind.into(),
            payload,
        })?;
    }
    Ok(())
}

/// 功能：返回执行结果及可用于后续存储提交失败恢复的检查点。
pub struct TurnExecution {
    /// 模型回合的结果；panic 转换为既有 session 错误。
    pub result: openpaw_core::Result<StopReason>,
    /// 已接收输入之后的检查点；成功执行不等于持久化提交。
    pub checkpoint: SessionCheckpoint,
    /// 功能：完整真实产出；失败/取消时为空，不包含压缩摘要。
    pub generated_messages: Vec<openpaw_core::Message>,
}

struct RestoreGuard<'a> {
    session: &'a mut Session,
    checkpoint: Option<SessionCheckpoint>,
}
impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.restore(self.session);
        }
    }
}

/// 功能：调用既有 AgentLoop 并保护完整会话状态。
/// 参数：agent、session、tools、event_sink 沿用既有执行输入，options 提供本回合配置。
/// 返回：结果和提交失败时可使用的检查点。
/// 错误：执行错误原样返回；panic 转为不含原始载荷的 session 错误。
/// 状态：错误、取消或 future 被丢弃时恢复检查点；成功时保留生成状态等待调用方提交。
/// 并发与取消：调用方独占 session；使用原令牌的子令牌执行，不反向取消调用方。
/// 权限：沿用 AgentLoop 工具/审批策略，不撤销外部副作用。
/// 关联：CHG-2026-0025。
pub async fn run_turn(
    agent: &AgentLoop,
    session: &mut Session,
    tools: &[ToolSpec],
    event_sink: &mut (dyn FnMut(AgentEvent) + Send),
    options: TurnOptions,
) -> TurnExecution {
    let checkpoint = SessionCheckpoint::capture(session);
    let mut guard = RestoreGuard {
        session,
        checkpoint: Some(checkpoint),
    };
    let cancel = options.cancel.clone();
    let mut generated_messages = Vec::new();
    let execution = agent.run_turn_collect(
        guard.session,
        tools,
        event_sink,
        TurnOptions {
            cancel: cancel.clone(),
            ..options
        },
        &mut generated_messages,
    );
    let result = std::panic::AssertUnwindSafe(execution)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(openpaw_core::Error::session("agent loop panicked")));
    let checkpoint = guard.checkpoint.take().expect("execution checkpoint");
    if result.is_err() || cancel.is_cancelled() {
        checkpoint.restore(guard.session);
        generated_messages.clear();
    }
    TurnExecution {
        result,
        checkpoint,
        generated_messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::Message;

    fn session() -> Session {
        let mut session = Session::new("mock-1", "mock");
        session.messages.push(Message::user_text("accepted input"));
        session.total_input_tokens = 31;
        session.total_output_tokens = 7;
        session.agent_type = Some("original".into());
        session
    }

    #[tokio::test]
    async fn sink_panic_restores_full_session_and_does_not_cancel_parent() {
        let mut session = session();
        let baseline = serde_json::to_value(&session).unwrap();
        let agent = AgentLoop::new(Arc::new(openpaw_llm::MockProvider::fixed("reply")));
        let cancel = CancellationToken::new();
        let mut sink = |event| {
            if matches!(event, AgentEvent::TurnUsage { .. }) {
                panic!("fixture sink panic");
            }
        };
        let outcome = run_turn(
            &agent,
            &mut session,
            &[],
            &mut sink,
            TurnOptions {
                cancel: cancel.clone(),
                ..Default::default()
            },
        )
        .await;
        assert!(outcome.result.is_err());
        assert_eq!(serde_json::to_value(&session).unwrap(), baseline);
        assert!(!cancel.is_cancelled());
        let outcome = run_turn(
            &agent,
            &mut session,
            &[],
            &mut |_| {},
            TurnOptions::default(),
        )
        .await;
        assert!(
            outcome.result.is_ok(),
            "next turn must have no leaked active gate"
        );
    }

    #[tokio::test]
    async fn cancellation_after_usage_restores_full_session() {
        let mut session = session();
        let baseline = serde_json::to_value(&session).unwrap();
        let agent = AgentLoop::new(Arc::new(openpaw_llm::MockProvider::fixed("reply")));
        let cancel = CancellationToken::new();
        let cancel_sink = cancel.clone();
        let mut sink = move |event| {
            if matches!(event, AgentEvent::TurnUsage { .. }) {
                cancel_sink.cancel();
            }
        };
        let outcome = run_turn(
            &agent,
            &mut session,
            &[],
            &mut sink,
            TurnOptions {
                cancel,
                ..Default::default()
            },
        )
        .await;
        assert!(outcome.result.is_ok());
        assert_eq!(serde_json::to_value(&session).unwrap(), baseline);
    }

    #[tokio::test]
    async fn panic_after_compaction_restores_original_history_and_usage() {
        let mut session = session();
        for _ in 0..5 {
            session.messages.push(Message::assistant_text("old reply"));
            session.messages.push(Message::user_text("old input"));
        }
        let baseline = serde_json::to_value(&session).unwrap();
        let agent = AgentLoop::new(Arc::new(openpaw_llm::MockProvider::fixed("summary")))
            .with_compact_options(crate::compaction::CompactOptions {
                model_limit: 10,
                keep_recent_pairs: 1,
                protect_head_pairs: 0,
                min_messages_before_compact: 0,
                use_context_aware_buffer: false,
                ..Default::default()
            });
        let mut compacted = false;
        let mut sink = |event| {
            if matches!(event, AgentEvent::CompactionPerformed { .. }) {
                compacted = true;
                panic!("injected failure after history rewrite");
            }
        };
        let outcome = run_turn(&agent, &mut session, &[], &mut sink, TurnOptions::default()).await;
        assert!(compacted);
        assert!(outcome.result.is_err());
        assert_eq!(serde_json::to_value(&session).unwrap(), baseline);
    }

    #[tokio::test]
    async fn dropping_running_future_restores_partial_tool_round() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("fixture.txt"), "tool result").unwrap();
        let provider = openpaw_llm::MockProvider::scripted(vec![
            openpaw_llm::MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: serde_json::json!({"path":"fixture.txt"}),
                text: "read".into(),
            },
            openpaw_llm::MockMode::DelayedStreamText {
                text: "long reply".into(),
                chunks: 10,
                delay_ms: 100,
            },
        ]);
        let tools = openpaw_tools::arc_registry(workspace.path());
        let specs = tools.list_tools().await.unwrap();
        let agent = AgentLoop::new(Arc::new(provider)).with_tool_host(tools);
        let mut session = session();
        let baseline = serde_json::to_value(&session).unwrap();
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        let mut responses = 0;
        let mut sink = move |event| {
            if matches!(event, AgentEvent::MessageStart) {
                responses += 1;
                if responses == 2 {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        };
        {
            let future = run_turn(
                &agent,
                &mut session,
                &specs,
                &mut sink,
                TurnOptions {
                    cancel: cancel.clone(),
                    ..Default::default()
                },
            );
            tokio::pin!(future);
            tokio::select! { _ = &mut future => panic!("fixture must still be streaming"), _ = rx => {} }
        }
        assert_eq!(serde_json::to_value(&session).unwrap(), baseline);
        assert!(!cancel.is_cancelled());
    }
}
