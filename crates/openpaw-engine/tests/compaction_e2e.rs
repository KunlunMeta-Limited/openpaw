//! E2 compaction integration: the loop's ④ hook triggers on threshold and
//! rewrites the session with a boundary + summary (M2 exit: thresholds
//! covered at three levels — unit tests cover None/Proactive/HardLimit
//! over the decision function; this file proves the loop wiring).
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message,
    Result, Session, StopReason, StreamEvent,
};
use openpaw_engine::{AgentEvent, AgentLoop, CompactOptions};
use openpaw_llm::{MockMode, MockProvider};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
struct PendingCompactionProvider {
    dropped: Arc<AtomicBool>,
}
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl LlmProvider for PendingCompactionProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
        let _drop = DropFlag(Arc::clone(&self.dropped));
        std::future::pending().await
    }
    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent>>> {
        unreachable!("compaction must be cancelled before the regular provider stream")
    }
    fn capabilities(&self) -> Capabilities {
        Default::default()
    }
    fn name(&self) -> &str {
        "pending-compaction"
    }
    fn default_model(&self) -> &str {
        "pending-compaction-1"
    }
}
fn big_session(pairs: usize) -> Session {
    let mut s = Session::new("compact-test", "mock");
    for i in 0..pairs {
        s.messages.push(Message::user_text(format!(
            "question {i}: please analyze a fairly long piece of context here to grow the estimate"
        )));
        s.messages.push(Message::assistant_text(format!(
            "answer {i}: findings and decisions, with enough padding text to make the session sizable for the threshold test"
        )));
    }
    s
}
#[tokio::test]
async fn compaction_triggers_and_rewrites_session() {
    // Script: call #1 = summary (provider.complete), call #2 = the turn's
    // stream response.
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::Fixed(
            "[Context Summary] user asked a series of analysis questions; last answer pending."
                .into(),
        ),
        MockMode::Fixed("final reply after compaction".into()),
    ]));
    let l = AgentLoop::new(p.clone()).with_compact_options(CompactOptions {
        model_limit: 4000, // 小于业务输出预留，但可容纳带1024余量的独立摘要请求。
        keep_recent_pairs: 2,
        min_messages_before_compact: 4,
        ..Default::default()
    });
    let mut session = big_session(10);
    let before_len = session.messages.len();
    let log: Arc<std::sync::Mutex<Vec<AgentEvent>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    // Session was rewritten: shorter, with a boundary marker.
    assert!(
        session.messages.len() < before_len,
        "compaction must shrink the session: {} → {}",
        before_len,
        session.messages.len()
    );
    let boundary = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .any(|b| matches!(b, ContentBlock::CompactBoundary { .. }));
    assert!(boundary, "a compact boundary must be present");
    // Summary content reached the session.
    let summary_present = session
        .messages
        .iter()
        .any(|m| m.text().contains("[Context Summary]"));
    assert!(summary_present, "summary must ride into the session");
    // The loop emitted CompactionPerformed.
    let events = log.lock().unwrap_or_else(|e| e.into_inner());
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::CompactionPerformed { before_tokens, after_tokens }
            if after_tokens < before_tokens)
    ));
    // The final assistant reply is the second script step.
    assert_eq!(
        session.messages.last().unwrap().text(),
        "final reply after compaction"
    );
    // Two provider calls total: summary (complete) + turn (stream).
    assert_eq!(p.call_count(), 2);
}
#[tokio::test]
async fn compaction_skipped_below_threshold() {
    let p = Arc::new(MockProvider::fixed("plain reply"));
    let l = AgentLoop::new(p).with_compact_options(CompactOptions {
        model_limit: 1_000_000, // never crossed
        ..Default::default()
    });
    let mut session = big_session(10);
    let log: Arc<std::sync::Mutex<Vec<AgentEvent>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert!(!log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|e| matches!(e, AgentEvent::CompactionPerformed { .. })));
    // Session untouched apart from the assistant reply.
    assert_eq!(session.messages.len(), 21);
    assert!(session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .all(|b| !matches!(b, ContentBlock::CompactBoundary { .. })));
}
#[tokio::test]
async fn cancellation_drops_pending_compaction_provider() {
    let dropped = Arc::new(AtomicBool::new(false));
    let provider = Arc::new(PendingCompactionProvider {
        dropped: Arc::clone(&dropped),
    });
    let loop_ = AgentLoop::new(provider).with_compact_options(CompactOptions {
        model_limit: 4000,
        keep_recent_pairs: 2,
        min_messages_before_compact: 4,
        ..Default::default()
    });
    let token = CancellationToken::new();
    let cancel = token.clone();
    let mut session = big_session(10);
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
    assert!(session
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .all(|block| !matches!(block, ContentBlock::CompactBoundary { .. })));
}

struct PartialOverflowProvider;
#[async_trait::async_trait]
impl LlmProvider for PartialOverflowProvider {
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse> {
        panic!("visible output forbids starting a recovery summary")
    }
    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent>>> {
        Ok(Box::pin(futures::stream::iter([
            Ok(StreamEvent::TextDelta {
                text: "已经展示的内容".into(),
                index: 0,
            }),
            Err(openpaw_core::Error::context_too_long(
                "native limit after output",
            )),
        ])))
    }
    fn capabilities(&self) -> Capabilities {
        Default::default()
    }
    fn name(&self) -> &str {
        "partial-overflow"
    }
    fn default_model(&self) -> &str {
        "fixture"
    }
}

#[tokio::test]
async fn native_context_error_after_visible_output_never_recovers_or_replays() {
    let agent =
        AgentLoop::new(Arc::new(PartialOverflowProvider)).with_compact_options(Default::default());
    let mut session = big_session(10);
    let mut visible = String::new();
    let result = agent
        .run_turn(&mut session, None, &[], &mut |event| {
            if let AgentEvent::TextDelta { text } = event {
                visible.push_str(&text);
            }
        })
        .await;
    assert!(result.is_err());
    assert_eq!(visible, "已经展示的内容");
}
