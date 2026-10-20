//! E1 engine tests — loop semantics with the deterministic mock provider.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use openpaw_core::{
    AllowlistGuardrail, AutoApproveManager, AutoDenyManager, CompletionRequest, ContentBlock,
    Error, GuardrailDecision, IterationBudget, LlmProvider, Message, Result, RetryPolicy, Session,
    StopReason, StreamEvent, ToolGuardrail, ToolHost, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};
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

    // Final assistant reply is "done".
    assert_eq!(session.messages.last().unwrap().text(), "done");
    // Tool result message exists with the echoed output.
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
    // Event sequence contains tool_use_start then tool_result.
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
    // Both results landed.
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
    // Classifier event recorded the denial.
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

#[async_trait::async_trait]
impl LlmProvider for FlakyProvider {
    async fn complete(&self, _req: CompletionRequest) -> Result<openpaw_core::CompletionResponse> {
        unreachable!("engine E1 uses stream")
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent>>> {
        if self.fail_times.fetch_sub(1, Ordering::SeqCst) > 1 {
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
    }

    fn capabilities(&self) -> openpaw_core::Capabilities {
        Default::default()
    }
    fn name(&self) -> &str {
        "flaky"
    }
    fn default_model(&self) -> &str {
        "flaky-1"
    }
}

#[tokio::test]
async fn stream_retry_recovers_from_overloaded() {
    let p = Arc::new(FlakyProvider {
        fail_times: AtomicUsize::new(3), // fails twice, third attempt succeeds
    });
    let l = AgentLoop::new(p).with_retry_policy(RetryPolicy::new(5, 1, 5, 2.0));
    let mut session = Session::new("flaky-1", "flaky");
    session.messages.push(Message::user_text("hi"));

    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(session.messages.last().unwrap().text(), "recovered");
}

/// Provider whose stream lingers long enough to be cancelled mid-flight.
struct SlowProvider {
    delay_ms: u64,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for SlowProvider {
    async fn complete(&self, _req: CompletionRequest) -> Result<openpaw_core::CompletionResponse> {
        unreachable!("engine E1 uses stream")
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let delay = self.delay_ms;
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
    }

    fn capabilities(&self) -> openpaw_core::Capabilities {
        Default::default()
    }
    fn name(&self) -> &str {
        "slow"
    }
    fn default_model(&self) -> &str {
        "slow-1"
    }
}

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

    assert!(matches!(stop, Err(openpaw_core::Error::Cancelled)));
    // Exactly one provider attempt — the in-flight stream was cancelled.
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn external_parent_cancellation_remains_effective_between_turns() {
    // D02：外部父token是调用方的持续取消范围，不能被引擎覆盖。
    let p = Arc::new(MockProvider::counter("r-"));
    let token = CancellationToken::new();
    token.cancel();
    let l = AgentLoop::new(p.clone()).with_cancel_token(token);
    let mut session = Session::new("mock-1", "mock");
    for input in ["a", "b"] {
        session.messages.push(Message::user_text(input));
        let (result, _) = run_simple(&l, &mut session, &[]).await;
        assert!(matches!(result, Err(openpaw_core::Error::Cancelled)));
    }
    assert_eq!(p.call_count(), 0);
}

#[tokio::test]
async fn budget_exhaustion_ends_turn_gracefully() {
    // Endless tool-use loop; the tiny budget must end the turn instead of
    // spinning forever.
    let p = Arc::new(MockProvider::tool_use(
        "echo_tool",
        serde_json::json!({"text": "again"}),
        "looping",
    ));
    let l = AgentLoop::new(p)
        .with_tool_host(echo_tool_host())
        .with_budget(IterationBudget::new(2))
        .with_parallel_tools(false);
    let mut session = Session::new("mock-1", "mock");
    session.messages.push(Message::user_text("go"));

    let (stop, _) = run_simple(&l, &mut session, &[]).await;
    assert!(matches!(stop, Err(openpaw_core::Error::BudgetExhausted(_))));
    // A handful of iterations ran, then the budget (2 + 1 grace) ended it.
    assert!(session.messages.len() < 12, "budget must bound the loop");
}
