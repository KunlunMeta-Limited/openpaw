//! E1 engine tests — loop semantics with the deterministic mock provider.
use futures::StreamExt;
use openpaw_core::{
    AllowlistGuardrail, ApprovalManager, ApprovalRequest, ApprovalResponse, AutoApproveManager,
    AutoDenyManager, CompletionRequest, ContentBlock, Error, GuardrailDecision, IterationBudget,
    LlmProvider, Message, Result, RetryPolicy, Session, StopReason, StreamEvent, ToolGuardrail,
    ToolHost, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
/// Helper: run one turn and capture events.
async fn run_simple(
    loop_: &AgentLoop,
    session: &mut Session,
    tools: &[ToolSpec],
) -> (Result<StopReason>, Vec<AgentEvent>) {
    let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let out = loop_.run_turn(session, None, tools, &mut sink).await;
    let events = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (out, events)
}
fn echo_tool_host() -> Arc<dyn ToolHost> {
    Arc::new(openpaw_core::RegistryToolHost {
        specs: vec![ToolSpec::new("echo_tool", "echoes text")],
        dispatch: |name, args| {
            if name != "echo_tool" {
                return Err(Error::ToolHost(format!("unknown tool {name}")));
            }
            Ok(openpaw_core::ToolCallOutcome::ok(format!(
                "echo: {}",
                args.get("text").and_then(|v| v.as_str()).unwrap_or("")
            )))
        },
    })
}
/// Search host with async sleep so the phased path's spawned tasks truly
/// overlap (concurrency observation for the parallel assertion).
struct SearchHost {
    max_concurrent: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl openpaw_core::ToolExecutor for SearchHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![
            ToolSpec::new("grep", "search"),
            ToolSpec::new("glob", "list files"),
        ])
    }
    async fn call(
        &self,
        name: &str,
        _args: serde_json::Value,
    ) -> Result<openpaw_core::ToolCallOutcome> {
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_concurrent.fetch_max(now_active, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(openpaw_core::ToolCallOutcome::ok(format!("{name} done")))
    }
}
#[async_trait::async_trait]
impl openpaw_core::ToolHostValidator for SearchHost {}
#[rustfmt::skip]
macro_rules! impl_stream_provider {
    ($ty:ty, $name:literal, $model:literal, $receiver:ident, $body:block) => { #[async_trait::async_trait] impl LlmProvider for $ty { async fn complete(&self, _req: CompletionRequest) -> Result<openpaw_core::CompletionResponse> { unreachable!("engine E1 uses stream") } async fn stream(&self, _req: CompletionRequest) -> TestStream { let $receiver = self; $body } fn capabilities(&self) -> openpaw_core::Capabilities { Default::default() } fn name(&self) -> &str { $name } fn default_model(&self) -> &str { $model } } };
}
type TestStream = Result<futures::stream::BoxStream<'static, Result<StreamEvent>>>;
fn user_session(id: &str, model: &str, text: &str) -> Session {
    let mut session = Session::new(id, model);
    session.messages.push(Message::user_text(text));
    session
}
fn event_count(events: &[AgentEvent], predicate: impl Fn(&AgentEvent) -> bool) -> usize {
    events.iter().filter(|event| predicate(event)).count()
}
#[rustfmt::skip]
fn never_connect() -> Arc<NeverConnectProvider> { Arc::new(NeverConnectProvider { calls: AtomicUsize::new(0) }) }
#[rustfmt::skip]
fn flaky_stream_item(fail_calls: usize, emit_before_error: bool) -> Arc<FlakyStreamItemProvider> { Arc::new(FlakyStreamItemProvider { calls: AtomicUsize::new(0), fail_calls, emit_before_error }) }
#[rustfmt::skip]
fn stalling_stream(recover_after: Option<usize>, emit_before_stall: bool) -> Arc<StallingStreamProvider> { Arc::new(StallingStreamProvider { calls: AtomicUsize::new(0), recover_after, emit_before_stall }) }
struct PendingGuardrail {
    dropped: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl ToolGuardrail for PendingGuardrail {
    async fn check(&self, _name: &str, _args: &serde_json::Value) -> GuardrailDecision {
        let _drop = DropFlag(Arc::clone(&self.dropped));
        std::future::pending().await
    }
}
struct PendingApproval {
    dropped: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl ApprovalManager for PendingApproval {
    async fn request(&self, _request: ApprovalRequest) -> ApprovalResponse {
        let _drop = DropFlag(Arc::clone(&self.dropped));
        std::future::pending().await
    }
}
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn pure_chat_ends_turn_with_usage_backfill() {
    let p = Arc::new(MockProvider::fixed("pong"));
    let l = AgentLoop::new(p.clone());
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("ping"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;
    let stop = stop.unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[1].text(), "pong");
    assert!(session.total_input_tokens > 0);
    assert!(session.total_output_tokens > 0);
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::TurnUsage { .. })));
}
#[tokio::test]
async fn streaming_text_forwards_deltas() {
    let p = Arc::new(MockProvider::stream_text("hello world", 3));
    let l = AgentLoop::new(p);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("say hi"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    let kinds: Vec<&str> = events.iter().map(|e| e.kind()).collect();
    assert_eq!(kinds[0], "message_start");
    let delta_count = kinds.iter().filter(|k| **k == "text_delta").count();
    assert!(delta_count >= 2, "expected multiple deltas, got {kinds:?}");
    assert_eq!(session.messages[1].text(), "hello world");
}
#[tokio::test]
async fn single_tool_serial_roundtrip() {
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "x"}),
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(echo_tool_host())
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("call echo"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(session.messages.last().unwrap().text(), "done");
    let tool_result = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("tool result present");
    assert_eq!(tool_result, "echo: x");
    let kinds: Vec<&str> = events.iter().map(|e| e.kind()).collect();
    let tus = kinds
        .iter()
        .position(|&k| k == "tool_use_start")
        .expect("tool_use_start");
    let trs = kinds
        .iter()
        .position(|&k| k == "tool_result")
        .expect("tool_result");
    assert!(tus < trs);
}
#[tokio::test]
async fn two_tools_run_concurrently_in_phased_path() {
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let host: Arc<dyn ToolHost> = Arc::new(SearchHost {
        max_concurrent: Arc::clone(&max_concurrent),
        active: Arc::clone(&active),
    });
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: vec![
                ("grep".into(), serde_json::json!({"pattern": "a"})),
                ("glob".into(), serde_json::json!({"pattern": "*.rs"})),
            ],
            text: "searching".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let l = AgentLoop::new(p).with_tool_host(host);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("search"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert!(
        max_concurrent.load(Ordering::SeqCst) >= 2,
        "two safe tools must overlap in the phased path"
    );
    let tool_results = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
        .count();
    assert_eq!(tool_results, 2);
}
#[tokio::test]
async fn guardrail_deny_produces_error_result() {
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "bash".into(),
            input: serde_json::json!({"cmd": "rm -rf /"}),
            text: "destructive".into(),
        },
        MockMode::Fixed("stopped".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(echo_tool_host())
        .with_guardrail(Arc::new(AllowlistGuardrail {
            allowed: vec!["echo_tool".into()],
        }))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    let tr = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("tool result");
    assert!(tr.1, "deny must be an error result");
    assert!(tr.0.contains("blocked by guardrail"));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ClassifierStatus {
            status: openpaw_engine::ClassifierStatus::AutoDenied { .. },
            ..
        }
    )));
}
#[tokio::test]
async fn confirm_path_approval_approved_runs_tool() {
    struct ConfirmAll;
    #[async_trait::async_trait]
    impl ToolGuardrail for ConfirmAll {
        async fn check(&self, _n: &str, _a: &serde_json::Value) -> GuardrailDecision {
            GuardrailDecision::Confirm("needs approval".into())
        }
    }
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "y"}),
            text: "asking".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(echo_tool_host())
        .with_guardrail(Arc::new(ConfirmAll))
        .with_approval_manager(Arc::new(AutoApproveManager))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    let tr = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .unwrap();
    assert!(!tr.1);
    assert_eq!(tr.0, "echo: y");
}
#[tokio::test]
async fn cancellation_after_approval_prevents_tool_execution() {
    struct ConfirmAll;
    #[async_trait::async_trait]
    impl ToolGuardrail for ConfirmAll {
        async fn check(&self, _n: &str, _a: &serde_json::Value) -> GuardrailDecision {
            GuardrailDecision::Confirm("needs approval".into())
        }
    }
    struct CancelThenApprove(CancellationToken);
    #[async_trait::async_trait]
    impl ApprovalManager for CancelThenApprove {
        async fn request(&self, _request: ApprovalRequest) -> ApprovalResponse {
            self.0.cancel();
            ApprovalResponse::Approved
        }
    }
    let executions = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let host: Arc<dyn ToolHost> = Arc::new(SearchHost {
        max_concurrent: Arc::clone(&executions),
        active,
    });
    let provider = Arc::new(MockProvider::scripted(vec![MockMode::ToolUse {
        tool_name: "grep".into(),
        input: serde_json::json!({"pattern":"x"}),
        text: "asking".into(),
    }]));
    let cancel = CancellationToken::new();
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host)
        .with_guardrail(Arc::new(ConfirmAll))
        .with_approval_manager(Arc::new(CancelThenApprove(cancel.clone())))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let mut sink = |_event: AgentEvent| {};
    let stop = loop_
        .run_turn_with_cancel_token(&mut session, None, &[], &mut sink, cancel)
        .await
        .expect("cancelled turn ends cleanly");
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(session.messages.iter().flat_map(|m| &m.content).any(|block| {
        matches!(block, ContentBlock::ToolResult { content, is_error: true, .. } if content == "cancelled by user")
    }));
}
#[tokio::test]
async fn confirm_path_denied_by_user() {
    struct ConfirmAll;
    #[async_trait::async_trait]
    impl ToolGuardrail for ConfirmAll {
        async fn check(&self, _n: &str, _a: &serde_json::Value) -> GuardrailDecision {
            GuardrailDecision::Confirm("needs approval".into())
        }
    }
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "y"}),
            text: "asking".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(echo_tool_host())
        .with_guardrail(Arc::new(ConfirmAll))
        .with_approval_manager(Arc::new(AutoDenyManager))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    let tr = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .unwrap();
    assert!(tr.1);
    assert!(tr.0.contains("denied by user"));
}
/// Provider that fails N times with an overloaded 503, then succeeds.
struct FlakyProvider {
    fail_times: AtomicUsize,
}
impl_stream_provider!(FlakyProvider, "flaky", "flaky-1", this, {
    let mut remaining = this.fail_times.load(Ordering::SeqCst);
    let should_fail = loop {
        if remaining == 0 {
            break false;
        }
        match this.fail_times.compare_exchange_weak(
            remaining,
            remaining - 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => break true,
            Err(current) => remaining = current,
        }
    };
    if should_fail {
        return Err(Error::server_error(503, "try again later"));
    }
    let resp = openpaw_core::CompletionResponse {
        content: vec![ContentBlock::Text {
            text: "recovered".into(),
        }],
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    };
    Ok(Box::pin(futures::stream::iter(vec![Ok(
        StreamEvent::Final(Box::new(resp)),
    )])))
});
#[tokio::test]
async fn stream_retry_recovers_from_overloaded() {
    let p = Arc::new(FlakyProvider {
        fail_times: AtomicUsize::new(1),
    });
    let l = AgentLoop::new(p).with_retry_policy(RetryPolicy::new(5, 1, 5, 2.0));
    let mut session = Session::new("flaky-1", "flaky");
    session.messages.push(Message::user_text("hi"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(session.messages.last().unwrap().text(), "recovered");
}
#[tokio::test]
async fn stream_retry_limits_overloaded_to_one_retry() {
    let p = Arc::new(FlakyProvider {
        fail_times: AtomicUsize::new(3),
    });
    let l = AgentLoop::new(p.clone()).with_retry_policy(RetryPolicy::new(5, 0, 0, 1.0));
    let mut session = Session::new("flaky-1", "flaky");
    session.messages.push(Message::user_text("hi"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::ServerError { status: 503, .. })));
    assert_eq!(p.fail_times.load(Ordering::SeqCst), 1);
}
/// Provider that returns an overloaded error from inside an established stream.
struct FlakyStreamItemProvider {
    calls: AtomicUsize,
    fail_calls: usize,
    emit_before_error: bool,
}
impl_stream_provider!(
    FlakyStreamItemProvider,
    "flaky-stream-item",
    "flaky-stream-item-1",
    this,
    {
        let call = this.calls.fetch_add(1, Ordering::SeqCst);
        if call < this.fail_calls {
            let error = Err(Error::Provider(
                "Our servers are currently overloaded. Please try again later.".into(),
            ));
            let events = if this.emit_before_error {
                vec![
                    Ok(StreamEvent::MessageStart),
                    Ok(StreamEvent::TextDelta {
                        index: 0,
                        text: "partial".into(),
                    }),
                    error,
                ]
            } else {
                vec![Ok(StreamEvent::MessageStart), error]
            };
            return Ok(Box::pin(futures::stream::iter(events)));
        }
        let response = openpaw_core::CompletionResponse {
            content: vec![ContentBlock::Text {
                text: "recovered from stream error".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            StreamEvent::Final(Box::new(response)),
        )])))
    }
);
#[tokio::test]
async fn stream_item_error_retries_before_visible_output_and_recovers() {
    let provider = flaky_stream_item(1, false);
    let loop_ = AgentLoop::new(provider.clone()).with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
    let mut session = user_session("flaky-stream-item-1", "flaky-stream-item", "hi");
    let (stop, events) = run_simple(&loop_, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        event_count(&events, |event| matches!(event, AgentEvent::MessageStart)),
        1
    );
    assert_eq!(
        session.messages.last().unwrap().text(),
        "recovered from stream error"
    );
}
#[tokio::test]
async fn stream_item_overload_is_limited_to_one_retry() {
    let provider = flaky_stream_item(3, false);
    let loop_ = AgentLoop::new(provider.clone()).with_retry_policy(RetryPolicy::new(5, 0, 0, 1.0));
    let mut session = user_session("flaky-stream-item-1", "flaky-stream-item", "hi");
    let (stop, _) = run_simple(&loop_, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::Provider(message)) if message.contains("overloaded")));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn stream_item_error_after_visible_output_does_not_retry() {
    let provider = flaky_stream_item(1, true);
    let loop_ = AgentLoop::new(provider.clone()).with_retry_policy(RetryPolicy::new(3, 0, 0, 1.0));
    let mut session = user_session("flaky-stream-item-1", "flaky-stream-item", "hi");
    let (stop, events) = run_simple(&loop_, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::Provider(message)) if message.contains("overloaded")));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_count(&events, |event| matches!(
            event,
            AgentEvent::TextDelta { .. }
        )),
        1
    );
}
/// Provider that never finishes creating a stream.
struct NeverConnectProvider {
    calls: AtomicUsize,
}
impl_stream_provider!(
    NeverConnectProvider,
    "never-connect",
    "never-connect-1",
    this,
    {
        this.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
);
/// Provider that can stall before the first frame or after visible output.
struct StallingStreamProvider {
    calls: AtomicUsize,
    recover_after: Option<usize>,
    emit_before_stall: bool,
}
impl_stream_provider!(
    StallingStreamProvider,
    "stalling-stream",
    "stalling-stream-1",
    this,
    {
        let call = this.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if this.recover_after.is_some_and(|attempt| call > attempt) {
            let resp = openpaw_core::CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: "recovered after timeout".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            };
            return Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::Final(Box::new(resp)),
            )])));
        }
        if this.emit_before_stall {
            return Ok(Box::pin(
                futures::stream::iter(vec![
                    Ok(StreamEvent::MessageStart),
                    Ok(StreamEvent::TextDelta {
                        index: 0,
                        text: "partial".into(),
                    }),
                ])
                .chain(futures::stream::pending()),
            ));
        }
        Ok(Box::pin(futures::stream::pending()))
    }
);
#[tokio::test]
async fn stream_connection_timeout_is_bounded_and_retryable() {
    let p = never_connect();
    let l = AgentLoop::new(p.clone())
        .with_stream_timeout(std::time::Duration::from_millis(20))
        .with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
    let mut session = user_session("never-connect-1", "never-connect", "hi");
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::Timeout(message)) if message.contains("connection")));
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn stream_frame_timeout_retries_before_visible_output_and_recovers() {
    let p = stalling_stream(Some(1), false);
    let l = AgentLoop::new(p.clone())
        .with_stream_timeout(std::time::Duration::from_millis(20))
        .with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
    let mut session = user_session("stalling-stream-1", "stalling-stream", "hi");
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        session.messages.last().unwrap().text(),
        "recovered after timeout"
    );
}
#[tokio::test]
async fn stream_frame_timeout_exhausts_budget_with_timeout_error() {
    let p = stalling_stream(None, false);
    let l = AgentLoop::new(p.clone())
        .with_stream_timeout(std::time::Duration::from_millis(20))
        .with_retry_policy(RetryPolicy::new(1, 0, 0, 1.0));
    let mut session = user_session("stalling-stream-1", "stalling-stream", "hi");
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::Timeout(message)) if message.contains("frame")));
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn stream_frame_timeout_after_visible_output_does_not_retry() {
    let p = stalling_stream(None, true);
    let l = AgentLoop::new(p.clone())
        .with_stream_timeout(std::time::Duration::from_millis(20))
        .with_retry_policy(RetryPolicy::new(3, 0, 0, 1.0));
    let mut session = user_session("stalling-stream-1", "stalling-stream", "hi");
    let (stop, events) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(stop, Err(Error::Timeout(message)) if message.contains("frame")));
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_count(&events, |event| matches!(
            event,
            AgentEvent::TextDelta { .. }
        )),
        1
    );
}
#[tokio::test]
async fn stream_cancellation_preempts_connection_timeout() {
    let p = never_connect();
    let token = CancellationToken::new();
    let cancel = token.clone();
    let l = AgentLoop::new(p.clone())
        .with_stream_timeout(std::time::Duration::from_secs(5))
        .with_retry_policy(RetryPolicy::new(3, 0, 0, 1.0));
    let mut session = user_session("never-connect-1", "never-connect", "hi");
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let mut sink = |_event: AgentEvent| {};
    let started = std::time::Instant::now();
    let stop = l
        .run_turn_with_cancel_token(&mut session, None, &[], &mut sink, token)
        .await;
    canceller.await.unwrap();
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
}
/// Provider whose stream lingers long enough to be cancelled mid-flight.
struct SlowProvider {
    delay_ms: u64,
    calls: AtomicUsize,
}
struct PendingStreamProvider {
    entered: Arc<Notify>,
}
impl_stream_provider!(
    PendingStreamProvider,
    "pending-stream",
    "pending-stream-1",
    this,
    {
        this.entered.notify_one();
        Ok(Box::pin(futures::stream::pending()))
    }
);
impl_stream_provider!(SlowProvider, "slow", "slow-1", this, {
    this.calls.fetch_add(1, Ordering::SeqCst);
    let delay = this.delay_ms;
    Ok(Box::pin(futures::stream::once(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        let resp = openpaw_core::CompletionResponse {
            content: vec![ContentBlock::Text {
                text: "late".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        };
        Ok(StreamEvent::Final(Box::new(resp)))
    })))
});
#[tokio::test]
async fn mid_turn_cancel_stops_the_stream() {
    let p = Arc::new(SlowProvider {
        delay_ms: 5_000,
        calls: AtomicUsize::new(0),
    });
    let l = Arc::new(AgentLoop::new(p.clone()));
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("hi"));
    let l2 = Arc::clone(&l);
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        l2.cancel();
    });
    let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let stop = l.run_turn(&mut session, None, &[], &mut sink).await;
    canceller.await.unwrap();
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancel_token_resets_between_turns() {
    // Upstream cancel-reset semantics: run_turn resets the token at entry,
    // so a cancelled token carried in from a previous turn must not poison
    // this turn.
    let p = Arc::new(MockProvider::counter("r-"));
    let token = CancellationToken::new();
    token.cancel();
    let l = AgentLoop::new(p).with_cancel_token(token);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("a"));
    let (stop1, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop1.unwrap(), StopReason::EndTurn);
    // The turn ran normally despite the pre-cancelled token.
    assert_eq!(session.messages.last().unwrap().text(), "r-0");
    session.messages.push(Message::user_text("b"));
    let (stop2, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop2.unwrap(), StopReason::EndTurn);
    assert_eq!(session.messages.last().unwrap().text(), "r-1");
    let caller_token = CancellationToken::new();
    session.messages.push(Message::user_text("c"));
    let mut sink = |_event: AgentEvent| {};
    l.run_turn_with_cancel_token(&mut session, None, &[], &mut sink, caller_token.clone())
        .await
        .unwrap();
    assert!(!caller_token.is_cancelled());
    assert_eq!(session.messages.last().unwrap().text(), "r-2");
}
#[tokio::test]
async fn cancellation_drops_pending_guardrail_without_executing_tool() {
    let provider = Arc::new(MockProvider::scripted(vec![MockMode::ToolUse {
        tool_name: "echo_tool".into(),
        input: serde_json::json!({"text": "x"}),
        text: "calling".into(),
    }]));
    let dropped = Arc::new(AtomicBool::new(false));
    let executions = Arc::new(AtomicUsize::new(0));
    let host: Arc<dyn ToolHost> = Arc::new(SearchHost {
        max_concurrent: Arc::clone(&executions),
        active: Arc::new(AtomicUsize::new(0)),
    });
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host)
        .with_guardrail(Arc::new(PendingGuardrail {
            dropped: Arc::clone(&dropped),
        }))
        .with_parallel_tools(false);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let mut session = Session::new("pending-guardrail", "mock");
    session.messages.push(Message::user_text("go"));
    let mut sink = |_event: AgentEvent| {};
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let stop = loop_
        .run_turn_with_cancel_token(&mut session, None, &[], &mut sink, token)
        .await
        .unwrap();
    canceller.await.unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cancellation_drops_pending_approval_without_executing_tool() {
    struct ConfirmAll;
    #[async_trait::async_trait]
    impl ToolGuardrail for ConfirmAll {
        async fn check(&self, _name: &str, _args: &serde_json::Value) -> GuardrailDecision {
            GuardrailDecision::Confirm("confirm".into())
        }
    }
    let provider = Arc::new(MockProvider::scripted(vec![MockMode::ToolUse {
        tool_name: "echo_tool".into(),
        input: serde_json::json!({"text": "x"}),
        text: "calling".into(),
    }]));
    let dropped = Arc::new(AtomicBool::new(false));
    let executions = Arc::new(AtomicUsize::new(0));
    let host: Arc<dyn ToolHost> = Arc::new(SearchHost {
        max_concurrent: Arc::clone(&executions),
        active: Arc::new(AtomicUsize::new(0)),
    });
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host)
        .with_guardrail(Arc::new(ConfirmAll))
        .with_approval_manager(Arc::new(PendingApproval {
            dropped: Arc::clone(&dropped),
        }))
        .with_parallel_tools(false);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let mut session = Session::new("pending-approval", "mock");
    session.messages.push(Message::user_text("go"));
    let mut sink = |_event: AgentEvent| {};
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let stop = loop_
        .run_turn_with_cancel_token(&mut session, None, &[], &mut sink, token)
        .await
        .unwrap();
    canceller.await.unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn same_agent_loop_rejects_concurrent_turn_and_recovers_after_cancel() {
    let provider = Arc::new(SlowProvider {
        delay_ms: 5_000,
        calls: AtomicUsize::new(0),
    });
    let loop_ = Arc::new(AgentLoop::new(provider));
    let first_token = CancellationToken::new();
    let mut first_session = Session::new("first", "mock");
    first_session.messages.push(Message::user_text("first"));
    let first_loop = Arc::clone(&loop_);
    let first_cancel = first_token.clone();
    let first = tokio::spawn(async move {
        let mut sink = |_event: AgentEvent| {};
        first_loop
            .run_turn_with_cancel_token(&mut first_session, None, &[], &mut sink, first_token)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut second_session = Session::new("second", "mock");
    second_session.messages.push(Message::user_text("second"));
    let mut sink = |_event: AgentEvent| {};
    let second = loop_
        .run_turn(&mut second_session, None, &[], &mut sink)
        .await;
    assert!(matches!(second, Err(Error::Session(message)) if message.contains("active turn")));
    first_cancel.cancel();
    assert_eq!(first.await.unwrap().unwrap(), StopReason::EndTurn);
    let mut third_session = Session::new("third", "mock");
    third_session.messages.push(Message::user_text("third"));
    let third_token = CancellationToken::new();
    third_token.cancel();
    let third = loop_
        .run_turn_with_cancel_token(&mut third_session, None, &[], &mut sink, third_token)
        .await;
    assert_eq!(third.unwrap(), StopReason::EndTurn);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_agent_loop_gate_is_stable_across_one_thousand_races() {
    for iteration in 0..1_000 {
        let entered = Arc::new(Notify::new());
        let loop_ = Arc::new(AgentLoop::new(Arc::new(PendingStreamProvider {
            entered: Arc::clone(&entered),
        })));
        let first_token = CancellationToken::new();
        let first_cancel = first_token.clone();
        let first_loop = Arc::clone(&loop_);
        let first = tokio::spawn(async move {
            let mut session = Session::new(format!("first-{iteration}"), "mock");
            session.messages.push(Message::user_text("first"));
            let mut sink = |_event: AgentEvent| {};
            first_loop
                .run_turn_with_cancel_token(&mut session, None, &[], &mut sink, first_token)
                .await
        });
        entered.notified().await;
        let mut second_session = Session::new(format!("second-{iteration}"), "mock");
        second_session.messages.push(Message::user_text("second"));
        let mut sink = |_event: AgentEvent| {};
        let second = loop_
            .run_turn(&mut second_session, None, &[], &mut sink)
            .await;
        assert!(matches!(second, Err(Error::Session(message)) if message.contains("active turn")));
        first_cancel.cancel();
        assert_eq!(first.await.unwrap().unwrap(), StopReason::EndTurn);
        let released = CancellationToken::new();
        released.cancel();
        let mut third_session = Session::new(format!("third-{iteration}"), "mock");
        third_session.messages.push(Message::user_text("third"));
        assert_eq!(
            loop_
                .run_turn_with_cancel_token(&mut third_session, None, &[], &mut sink, released,)
                .await
                .unwrap(),
            StopReason::EndTurn
        );
    }
}
#[tokio::test]
async fn budget_exhaustion_forces_a_tool_free_final_reply() {
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "again"}),
            text: "looping".into(),
        },
        MockMode::Fixed("最终结果：已完成；产物：result.txt；未完成事项：无".into()),
    ]));
    let l = AgentLoop::new(p.clone())
        .with_tool_host(echo_tool_host())
        .with_budget(IterationBudget::new_elastic(1, 1, 0))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(p.call_count(), 2, "耗尽后只能追加一次最终答复调用");
    assert_eq!(
        session.messages.last().unwrap().text(),
        "最终结果：已完成；产物：result.txt；未完成事项：无"
    );
}

#[tokio::test]
async fn budget_exhaustion_repairs_dsml_without_exposing_protocol_text() {
    let dsml = r#"<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name="todo_update"></｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>"#;
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "again"}),
            text: "looping".into(),
        },
        MockMode::StreamText {
            text: dsml.into(),
            chunks: 4,
        },
        MockMode::StreamText {
            text: "最终结果：已完成；产物：result.txt；未完成事项：无".into(),
            chunks: 3,
        },
    ]));
    let l = AgentLoop::new(p.clone())
        .with_tool_host(echo_tool_host())
        .with_budget(IterationBudget::new_elastic(1, 1, 0))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;

    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(p.call_count(), 3, "DSML 候选只允许一次纯文本纠正");
    assert_eq!(
        session.messages.last().unwrap().text(),
        "最终结果：已完成；产物：result.txt；未完成事项：无"
    );
    assert!(events.iter().all(|event| {
        !matches!(event, AgentEvent::TextDelta { text } if text.contains("DSML") || text.contains("todo_update"))
    }));
}

#[tokio::test]
async fn budget_exhaustion_rejects_repeated_dsml_without_exposing_protocol_text() {
    let dsml = r#"<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name="todo_update"></｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>"#;
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "again"}),
            text: "looping".into(),
        },
        MockMode::StreamText {
            text: dsml.into(),
            chunks: 4,
        },
    ]));
    let l = AgentLoop::new(p.clone())
        .with_tool_host(echo_tool_host())
        .with_budget(IterationBudget::new_elastic(1, 1, 0))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, events) = run_simple(&l, &mut session, &[]).await;

    assert!(matches!(
        stop,
        Err(Error::Provider(message)) if message.contains("未返回可展示的最终答复")
    ));
    assert_eq!(p.call_count(), 3, "重复 DSML 后必须停止，不能无限纠正");
    assert!(events.iter().all(|event| {
        !matches!(event, AgentEvent::TextDelta { text } if text.contains("DSML") || text.contains("todo_update"))
    }));
    assert!(matches!(
        session
            .messages
            .last()
            .and_then(|message| message.content.last()),
        Some(ContentBlock::ToolResult { .. })
    ));
}

#[tokio::test]
async fn budget_exhaustion_without_final_text_is_an_error() {
    let p = Arc::new(MockProvider::tool_use(
        "echo_tool",
        serde_json::json!({"text": "again"}),
        "looping",
    ));
    let l = AgentLoop::new(p.clone())
        .with_tool_host(echo_tool_host())
        .with_budget(IterationBudget::new_elastic(2, 2, 0))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(
        stop,
        Err(Error::Provider(message)) if message.contains("未返回可展示的最终答复")
    ));
    assert_eq!(p.call_count(), 3, "两次工具迭代后只能追加一次最终答复调用");
    assert!(matches!(
        session
            .messages
            .last()
            .and_then(|message| message.content.last()),
        Some(ContentBlock::ToolResult { .. })
    ));
}
