//! 职责：T1真实行为回归。
//! 输入输出：确定性输入、故障到断言及账本证据。
//! 依赖：原引擎、Provider或Store公共入口。
//! 禁止职责：不复制生产逻辑，不冒充Office或真实模型服务验收。

use openpaw_core::{Message, Session};
use openpaw_engine::AgentLoop;
use openpaw_llm::MockProvider;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn externally_cancelled_turn_is_not_normal_completion() {
    let token = CancellationToken::new();
    token.cancel();
    let provider = Arc::new(MockProvider::fixed("must not run"));
    let agent = AgentLoop::new(provider.clone()).with_cancel_token(token);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("hi"));
    assert!(
        agent
            .run_turn(&mut session, None, &[], &mut |_| {})
            .await
            .is_err(),
        "外部取消不能被覆盖，也不能当作正常完成"
    );
    assert_eq!(provider.call_count(), 0);
}

#[tokio::test]
async fn configured_idle_timeout_is_effective() {
    use async_trait::async_trait;
    use openpaw_core::*;
    struct Silent;
    #[async_trait]
    impl LlmProvider for Silent {
        fn name(&self) -> &str {
            "silent"
        }
        fn default_model(&self) -> &str {
            "real-default"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse> {
            futures::future::pending().await
        }
    }
    let agent = AgentLoop::new(Arc::new(Silent))
        .with_stream_timeout(Duration::from_millis(20))
        .with_retry_policy(openpaw_core::RetryPolicy::new(0, 1, 1, 1.0));
    let mut session = Session::new("real-default", "silent");
    let bounded = tokio::time::timeout(
        Duration::from_millis(200),
        agent.run_turn(&mut session, None, &[], &mut |_| {}),
    )
    .await;
    assert!(bounded.is_ok(), "配置的等待超时没有生效");
    assert!(bounded.unwrap().is_err());
}

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use openpaw_core::*;
use openpaw_engine::{AgentEvent, CompactOptions, RunContext, RunObserver, RunStatus};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

#[derive(Default)]
struct Journal {
    events: Vec<SessionEvent>,
    fail: Option<&'static str>,
}
#[async_trait]
impl RunObserver for Journal {
    async fn record(&mut self, event: SessionEvent) -> Result<()> {
        let kind = match event {
            SessionEvent::Message(ref m) if m.role == Role::Assistant => "assistant",
            SessionEvent::Message(ref m)
                if m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. })) =>
            {
                "result"
            }
            SessionEvent::RunFinished { .. } => "terminal",
            _ => "other",
        };
        if self.fail == Some(kind) {
            return Err(Error::Store("injected disk failure".into()));
        }
        self.events.push(event);
        Ok(())
    }
}

struct ProbeProvider {
    mode: &'static str,
    entered: tokio::sync::Notify,
    calls: AtomicUsize,
    models: Mutex<Vec<String>>,
}
impl ProbeProvider {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            entered: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
            models: Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl LlmProvider for ProbeProvider {
    fn name(&self) -> &str {
        "probe"
    }
    fn default_model(&self) -> &str {
        "真实默认模型"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        self.models.lock().unwrap().push(req.model);
        self.entered.notify_one();
        if self.mode == "summary_wait" {
            return futures::future::pending().await;
        }
        Ok(CompletionResponse {
            content: vec![ContentBlock::Text { text: "ok".into() }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 3,
                ..Default::default()
            },
        })
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        match self.mode {
            "connect_wait" => futures::future::pending().await,
            "stream_wait" => Ok(futures::stream::pending().boxed()),
            "backoff" => Err(Error::rate_limited("wait", Some(60))),
            "torn" => Ok(futures::stream::iter(vec![Ok(StreamEvent::TextDelta {
                index: 0,
                text: "partial".into(),
            })])
            .boxed()),
            _ => Ok(futures::stream::iter(vec![Ok(StreamEvent::Final(Box::new(
                self.complete(req).await?,
            )))])
            .boxed()),
        }
    }
}

#[tokio::test]
async fn model_defaults_and_explicit_override_reach_provider() {
    for explicit in [None, Some("显式模型")] {
        let provider = Arc::new(ProbeProvider::new("normal"));
        let mut agent = AgentLoop::new(provider.clone());
        if let Some(model) = explicit {
            agent = agent.with_model(model);
        }
        agent
            .run_turn(&mut Session::new("m", "p"), None, &[], &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            provider.models.lock().unwrap().as_slice(),
            &[explicit.unwrap_or("真实默认模型")]
        );
    }
}

#[tokio::test]
async fn cancellation_covers_connection_stream_backoff_and_compaction() {
    for mode in ["connect_wait", "stream_wait", "backoff", "summary_wait"] {
        let provider = Arc::new(ProbeProvider::new(mode));
        let mut agent = AgentLoop::new(provider.clone());
        let mut session = Session::new("m", "p");
        if mode == "summary_wait" {
            for _ in 0..12 {
                session
                    .messages
                    .push(Message::user_text("long context ".repeat(100)));
            }
            agent = agent.with_compact_options(CompactOptions {
                model_limit: 10,
                min_messages_before_compact: 1,
                keep_recent_pairs: 1,
                protect_head_pairs: 1,
                ..Default::default()
            });
        }
        let context = RunContext::default();
        let mut journal = Journal::default();
        let mut sink = |_| {};
        let run =
            agent.run_with_context(&mut session, None, &[], &context, &mut journal, &mut sink);
        let cancel = async {
            provider.entered.notified().await;
            context.cancel.cancel();
        };
        let (outcome, _) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(run, cancel) })
                .await
                .unwrap();
        assert_eq!(outcome.status, RunStatus::Cancelled, "{mode}");
        assert!(
            matches!(journal.events.last(), Some(SessionEvent::RunFinished { status, .. }) if status == "cancelled")
        );
    }
}

#[tokio::test]
async fn total_and_stream_idle_timeouts_are_distinct() {
    for (mode, total) in [
        ("connect_wait", Some(Duration::from_millis(20))),
        ("stream_wait", None),
    ] {
        let agent = AgentLoop::new(Arc::new(ProbeProvider::new(mode)))
            .with_stream_timeout(Duration::from_millis(40));
        let context = RunContext {
            total_timeout: total,
            ..Default::default()
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            agent.run_with_context(
                &mut Session::new("m", "p"),
                None,
                &[],
                &context,
                &mut Journal::default(),
                &mut |_| {},
            ),
        )
        .await
        .unwrap();
        assert_eq!(outcome.status, RunStatus::TimedOut);
        let diagnostic = outcome.error.unwrap().to_string();
        assert!(diagnostic.contains(if total.is_some() {
            "total budget"
        } else {
            "stream idle"
        }));
    }
}

#[tokio::test]
async fn partial_output_is_saved_once_without_retry() {
    let provider = Arc::new(ProbeProvider::new("torn"));
    let agent = AgentLoop::new(provider.clone());
    let mut journal = Journal::default();
    let mut output = String::new();
    let outcome = agent
        .run_with_context(
            &mut Session::new("m", "p"),
            None,
            &[],
            &RunContext::default(),
            &mut journal,
            &mut |ev| {
                if let AgentEvent::TextDelta { text } = ev {
                    output.push_str(&text);
                }
            },
        )
        .await;
    assert_eq!(outcome.status, RunStatus::Failed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(output, "partial");
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Message(m) if m.text() == "partial"))
            .count(),
        1
    );
}

fn tool_provider() -> Arc<MockProvider> {
    Arc::new(MockProvider::scripted(vec![
        openpaw_llm::MockMode::ToolUse {
            tool_name: "change".into(),
            input: serde_json::json!({}),
            text: "proposed".into(),
        },
        openpaw_llm::MockMode::Fixed("done".into()),
    ]))
}
fn count_host(count: Arc<AtomicUsize>) -> Arc<dyn ToolHost> {
    Arc::new(RegistryToolHost {
        specs: vec![],
        dispatch: move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(ToolCallOutcome::ok("real result"))
        },
    })
}

#[tokio::test]
async fn persistence_failure_blocks_execution_and_never_replays_side_effect() {
    for (fail, expected) in [("assistant", 0), ("result", 1), ("terminal", 1)] {
        let count = Arc::new(AtomicUsize::new(0));
        let provider = tool_provider();
        let agent = AgentLoop::new(provider.clone()).with_tool_host(count_host(count.clone()));
        let outcome = agent
            .run_with_context(
                &mut Session::new("m", "p"),
                None,
                &[],
                &RunContext::default(),
                &mut Journal {
                    fail: Some(fail),
                    ..Default::default()
                },
                &mut |_| {},
            )
            .await;
        assert_eq!(outcome.status, RunStatus::Failed);
        assert_eq!(count.load(Ordering::SeqCst), expected);
        assert_eq!(
            provider.call_count(),
            if fail == "terminal" { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn tool_call_chain_and_usage_are_complete_and_ordered() {
    let count = Arc::new(AtomicUsize::new(0));
    let agent = AgentLoop::new(tool_provider()).with_tool_host(count_host(count));
    let mut journal = Journal::default();
    let outcome = agent
        .run_with_context(
            &mut Session::new("m", "p"),
            None,
            &[],
            &RunContext::default(),
            &mut journal,
            &mut |_| {},
        )
        .await;
    assert_eq!(outcome.status, RunStatus::Completed);
    let call = journal
        .events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolCallStart { call_id, .. } => Some(call_id),
            _ => None,
        })
        .unwrap();
    assert!(!call.is_empty());
    assert!(journal
        .events
        .iter()
        .any(|e| matches!(e, SessionEvent::ToolCallComplete { call_id, .. } if call_id == call)));
    assert!(journal.events.iter().any(|e| matches!(e, SessionEvent::Message(m) if m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == call)))));
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Usage(_)))
            .count(),
        2
    );
}

struct CleaningHost {
    entered: tokio::sync::Notify,
    stopping: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}
#[async_trait]
impl ToolExecutor for CleaningHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![])
    }
    async fn call(&self, _: &str, _: serde_json::Value) -> Result<ToolCallOutcome> {
        panic!("必须使用可取消调用")
    }
    async fn call_cancellable(
        &self,
        _: &str,
        _: serde_json::Value,
        token: CancellationToken,
    ) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        token.cancelled().await;
        self.stopping.notify_one();
        self.release.notified().await;
        Ok(ToolCallOutcome::ok("cleanup finished"))
    }
}
#[async_trait]
impl ToolHostValidator for CleaningHost {}

#[tokio::test]
async fn tool_cancellation_and_timeout_wait_for_actual_cleanup() {
    for tool_timeout in [None, Some(Duration::from_millis(20))] {
        let host = Arc::new(CleaningHost {
            entered: tokio::sync::Notify::new(),
            stopping: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let agent = AgentLoop::new(tool_provider()).with_tool_host(host.clone());
        let mut session = Session::new("m", "p");
        let mut journal = Journal::default();
        let context = RunContext {
            tool_timeout,
            ..Default::default()
        };
        let mut sink = |_| {};
        let run =
            agent.run_with_context(&mut session, None, &[], &context, &mut journal, &mut sink);
        tokio::pin!(run);
        let workflow = async {
            tokio::select! { biased; result = &mut run => panic!("提前结束: {result:?}"), _ = host.entered.notified() => {} }
            if tool_timeout.is_none() {
                context.cancel.cancel();
            }
            tokio::select! { biased; result = &mut run => panic!("未等待清理: {result:?}"), _ = host.stopping.notified() => {} }
            // Run仍在等待宿主清理；显式释放后才能返回终态。
            tokio::select! { biased; result = &mut run => panic!("清理前释放: {result:?}"), _ = tokio::task::yield_now() => {} }
            host.release.notify_one();
            (&mut run).await
        };
        let outcome = tokio::time::timeout(Duration::from_secs(2), workflow)
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            if tool_timeout.is_some() {
                RunStatus::TimedOut
            } else {
                RunStatus::Cancelled
            }
        );
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn same_engine_rejects_concurrency_and_other_engine_is_independent() {
    let provider = Arc::new(ProbeProvider::new("connect_wait"));
    let agent = AgentLoop::new(provider.clone());
    let mut first_session = Session::new("m", "p");
    let mut first_journal = Journal::default();
    let context = RunContext::default();
    let mut sink = |_| {};
    let run = agent.run_with_context(
        &mut first_session,
        None,
        &[],
        &context,
        &mut first_journal,
        &mut sink,
    );
    let other = async {
        provider.entered.notified().await;
        let rejected = agent
            .run_with_context(
                &mut Session::new("m", "p"),
                None,
                &[],
                &RunContext::default(),
                &mut Journal::default(),
                &mut |_| {},
            )
            .await;
        assert_eq!(rejected.status, RunStatus::Failed);
        let independent = AgentLoop::new(Arc::new(MockProvider::fixed("other")))
            .run_turn(&mut Session::new("m", "p"), None, &[], &mut |_| {})
            .await;
        assert!(independent.is_ok());
        context.cancel.cancel();
    };
    let (outcome, _) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(run, other) })
            .await
            .unwrap();
    assert_eq!(outcome.status, RunStatus::Cancelled);
}

#[tokio::test]
async fn compaction_persists_snapshot_and_summary_usage_with_effective_model() {
    let provider = Arc::new(ProbeProvider::new("normal"));
    let agent = AgentLoop::new(provider.clone())
        .with_model("override")
        .with_compact_options(CompactOptions {
            model_limit: 10,
            min_messages_before_compact: 1,
            keep_recent_pairs: 1,
            protect_head_pairs: 1,
            ..Default::default()
        });
    let mut session = Session::new("m", "p");
    for _ in 0..12 {
        session
            .messages
            .push(Message::user_text("long context ".repeat(100)));
    }
    let mut journal = Journal::default();
    let outcome = agent
        .run_with_context(
            &mut session,
            None,
            &[],
            &RunContext::default(),
            &mut journal,
            &mut |_| {},
        )
        .await;
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.usage.input_tokens, 10);
    assert_eq!(session.total_output_tokens, 6);
    assert!(journal
        .events
        .iter()
        .any(|e| matches!(e, SessionEvent::ContextSnapshot { .. })));
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Usage(_)))
            .count(),
        2
    );
    assert!(provider
        .models
        .lock()
        .unwrap()
        .iter()
        .all(|model| model == "override"));
}

struct PendingGate {
    stage: &'static str,
    entered: tokio::sync::Notify,
    calls: AtomicUsize,
}
#[async_trait]
impl ToolExecutor for PendingGate {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![])
    }
    async fn call(&self, _: &str, _: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolCallOutcome::ok("must not execute"))
    }
}
#[async_trait]
impl ToolHostValidator for PendingGate {
    async fn validate_input(
        &self,
        _: &str,
        _: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        if self.stage == "validation" {
            self.entered.notify_one();
            futures::future::pending().await
        } else {
            Ok(())
        }
    }
}
#[async_trait]
impl ToolGuardrail for PendingGate {
    async fn check(&self, _: &str, _: &serde_json::Value) -> GuardrailDecision {
        if self.stage == "classification" {
            self.entered.notify_one();
            futures::future::pending().await
        } else {
            GuardrailDecision::Confirm("approval required".into())
        }
    }
}
#[async_trait]
impl ApprovalManager for PendingGate {
    async fn request(&self, _: ApprovalRequest) -> ApprovalResponse {
        self.entered.notify_one();
        futures::future::pending().await
    }
}

#[tokio::test]
async fn validation_classification_and_approval_waits_cancel_without_side_effects() {
    // 并行路径的输入校验一致性归T2/D05；此处覆盖已实现路径的取消边界。
    for (parallel, stage) in [
        (false, "validation"),
        (false, "classification"),
        (false, "approval"),
        (true, "classification"),
        (true, "approval"),
    ] {
        let gate = Arc::new(PendingGate {
            stage,
            entered: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let provider: Arc<dyn LlmProvider> = if parallel {
            Arc::new(MockProvider::parallel_tool_use(
                vec![
                    ("change".into(), serde_json::json!({})),
                    ("other".into(), serde_json::json!({})),
                ],
                "",
            ))
        } else {
            tool_provider()
        };
        let agent = AgentLoop::new(provider)
            .with_tool_host(gate.clone())
            .with_guardrail(gate.clone())
            .with_approval_manager(gate.clone())
            .with_parallel_tools(parallel);
        let mut session = Session::new("m", "p");
        let mut journal = Journal::default();
        let context = RunContext::default();
        let mut sink = |_| {};
        let run =
            agent.run_with_context(&mut session, None, &[], &context, &mut journal, &mut sink);
        let cancel = async {
            gate.entered.notified().await;
            context.cancel.cancel();
        };
        let (outcome, _) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(run, cancel) })
                .await
                .unwrap();
        assert_eq!(
            outcome.status,
            RunStatus::Cancelled,
            "{stage} parallel={parallel}"
        );
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            journal
                .events
                .iter()
                .filter(|e| matches!(e, SessionEvent::ToolCallStart { .. }))
                .count(),
            if parallel { 2 } else { 1 }
        );
        assert_eq!(
            journal
                .events
                .iter()
                .filter(|e| matches!(e, SessionEvent::ToolCallComplete { .. }))
                .count(),
            if parallel { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn local_cancel_does_not_poison_next_run_or_parent() {
    let parent = CancellationToken::new();
    let provider = Arc::new(MockProvider::counter("r-"));
    let agent = AgentLoop::new(provider.clone()).with_cancel_token(parent.clone());
    let mut first = Session::new("m", "p");
    let mut sink = |event| {
        if matches!(event, AgentEvent::TurnUsage { .. }) {
            agent.cancel();
        }
    };
    assert!(matches!(
        agent.run_turn(&mut first, None, &[], &mut sink).await,
        Err(Error::Cancelled)
    ));
    assert!(!parent.is_cancelled());
    let mut second = Session::new("m", "p");
    assert!(agent
        .run_turn(&mut second, None, &[], &mut |_| {})
        .await
        .is_ok());
    assert_eq!(second.messages.last().unwrap().text(), "r-1");
}

struct ParallelCleanup {
    entered: tokio::sync::Semaphore,
    stopping: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    finished: AtomicUsize,
}
#[async_trait]
impl ToolExecutor for ParallelCleanup {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![])
    }
    async fn call(&self, _: &str, _: serde_json::Value) -> Result<ToolCallOutcome> {
        panic!("必须传递取消上下文")
    }
    async fn call_cancellable(
        &self,
        _: &str,
        _: serde_json::Value,
        token: CancellationToken,
    ) -> Result<ToolCallOutcome> {
        self.entered.add_permits(1);
        token.cancelled().await;
        self.stopping.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(ToolCallOutcome::ok("reaped"))
    }
}
#[async_trait]
impl ToolHostValidator for ParallelCleanup {}

#[tokio::test]
async fn parallel_cancellation_joins_every_started_tool() {
    let host = Arc::new(ParallelCleanup {
        entered: tokio::sync::Semaphore::new(0),
        stopping: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        finished: AtomicUsize::new(0),
    });
    let provider = Arc::new(MockProvider::parallel_tool_use(
        vec![
            ("one".into(), serde_json::json!({})),
            ("two".into(), serde_json::json!({})),
        ],
        "",
    ));
    let agent = AgentLoop::new(provider).with_tool_host(host.clone());
    let mut session = Session::new("m", "p");
    let mut journal = Journal::default();
    let context = RunContext::default();
    let mut sink = |_| {};
    let run = agent.run_with_context(&mut session, None, &[], &context, &mut journal, &mut sink);
    tokio::pin!(run);
    let workflow = async {
        tokio::select! { biased; result = &mut run => panic!("提前结束: {result:?}"), permit = host.entered.acquire_many(2) => permit.unwrap().forget() }
        context.cancel.cancel();
        tokio::select! { biased; result = &mut run => panic!("未通知所有工具: {result:?}"), permit = host.stopping.acquire_many(2) => permit.unwrap().forget() }
        tokio::select! { biased; result = &mut run => panic!("清理完成前返回: {result:?}"), _ = tokio::task::yield_now() => {} }
        host.release.add_permits(2);
        (&mut run).await
    };
    let outcome = tokio::time::timeout(Duration::from_secs(3), workflow)
        .await
        .unwrap();
    assert_eq!(outcome.status, RunStatus::Cancelled);
    assert_eq!(host.finished.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn observer_timeout_is_storage_failure_instead_of_run_timeout() {
    struct TimedOutStore;
    #[async_trait]
    impl RunObserver for TimedOutStore {
        async fn record(&mut self, event: SessionEvent) -> Result<()> {
            if matches!(event, SessionEvent::Message(_)) {
                Err(Error::Timeout("disk worker timed out".into()))
            } else {
                Ok(())
            }
        }
    }
    let agent = AgentLoop::new(Arc::new(MockProvider::fixed("reply")));
    let outcome = agent
        .run_with_context(
            &mut Session::new("m", "p"),
            None,
            &[],
            &RunContext::default(),
            &mut TimedOutStore,
            &mut |_| {},
        )
        .await;
    assert_eq!(outcome.status, RunStatus::Failed);
    assert!(matches!(outcome.error, Some(Error::Store(_))));
}
