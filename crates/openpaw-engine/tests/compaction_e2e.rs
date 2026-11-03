//! E2 compaction integration: the loop's ④ hook triggers on threshold and
//! rewrites the session with a boundary + summary (M2 exit: thresholds
//! covered at three levels — unit tests cover None/Proactive/HardLimit
//! over the decision function; this file proves the loop wiring).

use std::sync::Arc;

use openpaw_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, Role, Session, StopReason,
};
use openpaw_engine::{AgentEvent, AgentLoop, CompactOptions};
use openpaw_llm::{MockMode, MockProvider, OpenAiProvider};

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

fn calls(ids: &[&str]) -> Message {
    Message {
        role: Role::Assistant,
        content: ids
            .iter()
            .map(|id| ContentBlock::ToolUse {
                id: (*id).into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "outputs/outline.md"}),
            })
            .collect(),
        ..Default::default()
    }
}

fn results(ids: &[&str]) -> Message {
    Message {
        role: Role::User,
        content: ids
            .iter()
            .map(|id| ContentBlock::ToolResult {
                tool_use_id: (*id).into(),
                content: format!("actual result for {id}"),
                is_error: false,
                name: None,
                protected: false,
            })
            .collect(),
        ..Default::default()
    }
}

fn tool_session(split_head_results: bool) -> Session {
    let mut session = Session::new("compact-tools", "mock");
    session
        .messages
        .push(Message::user_text("Keep working until stopped"));
    session.messages.push(calls(&["head-a", "head-b"]));
    if split_head_results {
        session.messages.push(results(&["head-a"]));
        session.messages.push(results(&["head-b"]));
    } else {
        session.messages.push(results(&["head-a", "head-b"]));
    }
    session.messages.extend(big_session(8).messages);
    session.messages.push(calls(&["tail"]));
    session.messages.push(results(&["tail"]));
    session
        .messages
        .push(Message::assistant_text("Checkpoint saved"));
    session
}

fn assert_core_tool_pairs(messages: &[Message]) {
    let mut pending = std::collections::BTreeSet::new();
    for message in messages {
        let only_results = message
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { .. }));
        assert!(
            pending.is_empty() || only_results,
            "compaction interrupted pending calls: {pending:?}"
        );
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => assert!(pending.insert(id.as_str())),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    assert!(pending.remove(tool_use_id.as_str()))
                }
                _ => {}
            }
        }
    }
    assert!(pending.is_empty(), "unanswered calls: {pending:?}");
}

#[tokio::test]
async fn compaction_preserves_parallel_head_tail_and_repeated_compaction() {
    for split_head_results in [false, true] {
        let mut session = tool_session(split_head_results);
        let protected = if split_head_results { 4 } else { 3 };
        let original_head = serde_json::to_value(&session.messages[..protected]).unwrap();
        let original_tail =
            serde_json::to_value(&session.messages[session.messages.len() - 3..]).unwrap();
        let provider = MockProvider::fixed("[Context Summary] actual past work");
        let options = CompactOptions {
            protect_head_pairs: 1,
            keep_recent_pairs: 1,
            ..Default::default()
        };
        for _ in 0..3 {
            assert!(
                openpaw_engine::compaction::compact_session(&provider, &mut session, &options)
                    .await
                    .unwrap()
                    > 0
            );
            assert_core_tool_pairs(&session.messages);
            assert_eq!(
                serde_json::to_value(&session.messages[..protected]).unwrap(),
                original_head
            );
            assert_eq!(
                serde_json::to_value(&session.messages[session.messages.len() - 3..]).unwrap(),
                original_tail
            );
        }
    }
}

#[tokio::test]
async fn compaction_skips_when_closed_head_and_tail_overlap() {
    let mut session = Session::new("compact-tools", "mock");
    session.messages = vec![
        Message::user_text("goal"),
        calls(&["a", "b"]),
        results(&["a"]),
        results(&["b"]),
        Message::assistant_text("done"),
    ];
    let before = serde_json::to_value(&session.messages).unwrap();
    let provider = MockProvider::fixed("summary must not be requested");
    let options = CompactOptions {
        protect_head_pairs: 1,
        keep_recent_pairs: 1,
        ..Default::default()
    };
    assert_eq!(
        openpaw_engine::compaction::compact_session(&provider, &mut session, &options)
            .await
            .unwrap(),
        0
    );
    assert_eq!(provider.call_count(), 0);
    assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
}

#[tokio::test]
async fn compaction_keeps_closed_tool_batch_when_no_recent_messages_requested() {
    let mut session = tool_session(false);
    let provider = MockProvider::fixed("[Context Summary] completed middle");
    let options = CompactOptions {
        protect_head_pairs: 1,
        keep_recent_pairs: 0,
        ..Default::default()
    };
    assert!(
        openpaw_engine::compaction::compact_session(&provider, &mut session, &options)
            .await
            .unwrap()
            > 0
    );
    assert_core_tool_pairs(&session.messages);
}

#[tokio::test]
async fn compacted_parallel_tool_calls_pass_actual_openai_http_validation() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "HTTP fixture did not receive a request"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("HTTP accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut data = Vec::new();
        let (body_start, content_length) = loop {
            let mut buffer = [0_u8; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0 && data.len() < 1_048_576);
            data.extend_from_slice(&buffer[..count]);
            if let Some(index) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&data[..index]);
                assert!(headers.starts_with("POST /chat/completions "));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (index + 4, length);
            }
        };
        assert!(content_length < 1_048_576);
        while data.len() < body_start + content_length {
            let mut buffer = [0_u8; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            data.extend_from_slice(&buffer[..count]);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&data[body_start..body_start + content_length]).unwrap();
        let mut pending = std::collections::BTreeSet::new();
        let mut valid = true;
        for message in body["messages"].as_array().unwrap() {
            if message["role"] == "tool" {
                valid &= pending.remove(message["tool_call_id"].as_str().unwrap());
            } else {
                valid &= pending.is_empty();
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        valid &= pending.insert(call["id"].as_str().unwrap());
                    }
                }
            }
        }
        valid &= pending.is_empty();
        let (status, response) = if valid {
            (
                "200 OK",
                serde_json::json!({"id":"fixture", "object":"chat.completion", "created":0, "model":"fixture", "choices":[{"index":0,"message":{"role":"assistant","content":"tools remain paired"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
            )
        } else {
            (
                "400 Bad Request",
                serde_json::json!({"error":{"message":"insufficient tool messages following tool_calls", "type":"invalid_request_error"}}),
            )
        };
        let response = response.to_string();
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        (valid, body)
    });
    let mut session = tool_session(true);
    let options = CompactOptions {
        protect_head_pairs: 1,
        keep_recent_pairs: 1,
        ..Default::default()
    };
    openpaw_engine::compaction::compact_session(
        &MockProvider::fixed("[Context Summary] middle"),
        &mut session,
        &options,
    )
    .await
    .unwrap();
    let provider =
        OpenAiProvider::new("fixture", "fixture").with_base_url(format!("http://{address}"));
    let response = provider
        .complete(CompletionRequest {
            model: "fixture".into(),
            messages: session.messages,
            ..Default::default()
        })
        .await;
    let (valid, body) = server.join().unwrap();
    assert!(valid, "actual HTTP tool chain was rejected: {body}");
    assert_eq!(response.unwrap().text(), "tools remain paired");
}
