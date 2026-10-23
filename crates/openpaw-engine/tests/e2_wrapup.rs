//! Engine E2 wrap-up: fallback provider wiring and token-budget
//! continuation through the loop.
use async_trait::async_trait;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, Error, LlmProvider, Message,
    Result, Session, StopReason, StreamEvent,
};
use openpaw_engine::{AgentLoop, FallbackCompressProvider, TokenBudgetConfig};
use openpaw_llm::MockProvider;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
/// Fails with ContextTooLong for `fail_model`, succeeds otherwise.
struct ModelSwitchMock {
    fail_model: String,
    calls: AtomicUsize,
}
#[async_trait]
impl LlmProvider for ModelSwitchMock {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
        unreachable!("loop uses stream")
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if req.model == self.fail_model {
            return Err(Error::ContextTooLong("140k > 128k".into()));
        }
        let resp = CompletionResponse {
            content: vec![ContentBlock::Text {
                text: format!("ok via {}", req.model),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            StreamEvent::Final(Box::new(resp)),
        )])))
    }
    fn capabilities(&self) -> Capabilities {
        Default::default()
    }
    fn name(&self) -> &str {
        "switchmock"
    }
    fn default_model(&self) -> &str {
        &self.fail_model
    }
}
#[tokio::test]
async fn run_turn_survives_context_error_via_fallback_model() {
    let inner = Arc::new(ModelSwitchMock {
        fail_model: "big".into(),
        calls: AtomicUsize::new(0),
    });
    let provider = FallbackCompressProvider::new(inner.clone() as Arc<dyn LlmProvider>, "small");
    let l = AgentLoop::new(Arc::new(provider))
        .with_model("big")
        .with_stream_timeout(std::time::Duration::from_secs(5));
    let mut session = Session::new("fb", "switchmock");
    session.messages.push(Message::user_text("hi"));
    let mut sink = |_ev: openpaw_engine::AgentEvent| {};
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(
        session.messages.last().unwrap().text(),
        "ok via small",
        "the fallback model must answer"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2, "primary + fallback");
}
#[tokio::test]
async fn token_budget_nudges_then_stops_on_diminishing() {
    // Counter replies make per-turn usage nearly flat → after the
    // diminishing window (3 continuations) the loop must stop even though
    // raw budget remains.
    let p = Arc::new(MockProvider::counter("r-"));
    let l = AgentLoop::new(p)
        .with_token_budget(TokenBudgetConfig::default().with_budget(1_000_000))
        .with_stream_timeout(std::time::Duration::from_secs(5));
    let mut session = Session::new("tb", "mock");
    session.messages.push(Message::user_text("work on it"));
    let mut sink = |_ev: openpaw_engine::AgentEvent| {};
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    // Nudge messages landed (diminishing window = 3 continuations).
    let nudges = session
        .messages
        .iter()
        .filter(|m| m.text().contains("remaining token budget"))
        .count();
    assert!(nudges >= 3, "expected ≥3 nudge continuations, got {nudges}");
    // The loop did not run away: a handful of replies at most.
    let replies = session
        .messages
        .iter()
        .filter(|m| m.text().starts_with("r-"))
        .count();
    assert!(replies <= 6, "diminishing detection must bound the loop");
}
#[tokio::test]
async fn token_budget_disabled_never_nudges() {
    let p = Arc::new(MockProvider::counter("x-"));
    let l = AgentLoop::new(p);
    let mut session = Session::new("tb", "mock");
    session.messages.push(Message::user_text("hi"));
    let mut sink = |_ev: openpaw_engine::AgentEvent| {};
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(session.messages.len(), 2, "no nudge without a budget");
}
#[tokio::test]
async fn cancel_still_terminates_nudged_loop() {
    let p = Arc::new(MockProvider::counter("y-"));
    let token = CancellationToken::new();
    let l = AgentLoop::new(p)
        .with_token_budget(TokenBudgetConfig::default().with_budget(1_000_000))
        .with_cancel_token(token.clone());
    // Cancel from a watcher task mid-run: the nudged loop must respect it.
    let l2 = Arc::new(l);
    let w = Arc::clone(&l2);
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        w.cancel();
    });
    let mut session = Session::new("tb", "mock");
    session.messages.push(Message::user_text("hi"));
    let mut sink = |_ev: openpaw_engine::AgentEvent| {};
    let stop = l2.run_turn(&mut session, None, &[], &mut sink).await;
    canceller.await.unwrap();
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
}

/// 验证预算总结与协议纠正仍允许用户请求的合法结构化正文。
#[tokio::test]
async fn budget_summary_and_dsml_repair_preserve_json_and_xml_answers() {
    for final_text in [
        r#"{"ok":true,"value":42}"#,
        "<result><value>42</value></result>",
    ] {
        for repair in [false, true] {
            let mut script = vec![openpaw_llm::MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: serde_json::json!({"path":"result.txt"}),
                text: String::new(),
            }];
            if repair {
                script.push(openpaw_llm::MockMode::Fixed("<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"todo_update\"></｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>".into()));
            }
            script.push(openpaw_llm::MockMode::Fixed(final_text.into()));
            let provider = Arc::new(MockProvider::scripted(script));
            let workspace = tempfile::tempdir().unwrap();
            std::fs::write(workspace.path().join("result.txt"), "42").unwrap();
            let agent = AgentLoop::new(provider.clone())
                .with_tool_host(openpaw_tools::arc_registry(workspace.path()))
                .with_budget(openpaw_core::IterationBudget::new_elastic(1, 1, 0));
            let mut session = Session::new("mock-1", "mock");
            session
                .messages
                .push(Message::user_text("按指定结构化格式返回42"));
            let mut sink = |_| {};
            let execution = openpaw_engine::turn_execution::run_turn(
                &agent,
                &mut session,
                &[],
                &mut sink,
                openpaw_engine::turn_execution::TurnOptions {
                    system: Some("保持用户指定的输出格式".into()),
                    ..Default::default()
                },
            )
            .await;
            let stop = execution.result.unwrap();
            let summary = execution.generated_messages.last().unwrap();
            assert_eq!(summary.text(), final_text);
            assert!(summary.uuid.is_some());
            assert_eq!(summary.uuid, session.messages.last().unwrap().uuid);
            assert_eq!(stop, StopReason::EndTurn);
            assert_eq!(session.messages.last().unwrap().text(), final_text);
            assert_eq!(provider.call_count(), if repair { 3 } else { 2 });
        }
    }
}
