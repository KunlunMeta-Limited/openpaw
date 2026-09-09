//! E2 compaction integration: the loop's ④ hook triggers on threshold and
//! rewrites the session with a boundary + summary (M2 exit: thresholds
//! covered at three levels — unit tests cover None/Proactive/HardLimit
//! over the decision function; this file proves the loop wiring).

use std::sync::Arc;

use openpaw_core::{ContentBlock, Message, Session, StopReason};
use openpaw_engine::{AgentEvent, AgentLoop, CompactOptions};
use openpaw_llm::{MockMode, MockProvider};

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
        model_limit: 800, // tiny limit → threshold crossed immediately
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
