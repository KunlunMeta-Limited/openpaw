//! Engine × tools integration: the real `ToolRegistry` (fs tools + think)
//! drives `run_turn` end-to-end through the mock provider — the M2 exit
//! demo shape ("let the agent complete a small real task with grep").

use std::sync::Arc;

use openpaw_core::{ContentBlock, Message, Session, StopReason};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};

#[tokio::test]
async fn grep_task_roundtrip_through_real_registry() {
    // Workspace with a file containing a TODO line.
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("code.rs"),
        "fn main() {}\n// TODO: fix overflow\nfn aux() {}\n",
    )
    .unwrap();

    // Script: turn 1 asks for grep; turn 2 answers with the result.
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "grep".into(),
            input: serde_json::json!({"pattern": "TODO", "include": "*.rs"}),
            text: "searching".into(),
        },
        MockMode::Fixed("found the TODO in code.rs".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(openpaw_tools::arc_registry(ws.path()))
        .with_parallel_tools(false);

    let mut session = Session::new("integration", "mock");
    session
        .messages
        .push(Message::user_text("find the TODO in rust files"));

    let log: Arc<std::sync::Mutex<Vec<AgentEvent>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: openpaw_engine::AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let stop = l
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);

    // The tool result carries REAL grep output from the real filesystem.
    let tool_result = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("tool result present");
    assert!(!tool_result.1);
    assert!(
        tool_result.0.contains("code.rs:2: // TODO: fix overflow"),
        "real grep output expected, got: {}",
        tool_result.0
    );

    // Final reply from the second script step.
    assert_eq!(
        session.messages.last().unwrap().text(),
        "found the TODO in code.rs"
    );

    // Event chain proves the full path: request → classify → execute → result.
    let events = log.lock().unwrap_or_else(|e| e.into_inner());
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
async fn deny_guardrail_blocks_real_registry_call() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("secret.txt"), "classified").unwrap();

    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "read_file".into(),
            input: serde_json::json!({"path": "secret.txt"}),
            text: "reading".into(),
        },
        MockMode::Fixed("cannot read that".into()),
    ]));
    let l = AgentLoop::new(p)
        .with_tool_host(openpaw_tools::arc_registry(ws.path()))
        .with_guardrail(Arc::new(openpaw_core::AllowlistGuardrail {
            allowed: vec!["grep".into(), "glob".into()],
        }))
        .with_parallel_tools(false);

    let mut session = Session::new("deny-test", "mock");
    session.messages.push(Message::user_text("read the secret"));

    let stop = {
        let mut sink = |_ev: openpaw_engine::AgentEvent| {};
        l.run_turn(&mut session, None, &[], &mut sink)
            .await
            .unwrap()
    };
    assert_eq!(stop, StopReason::EndTurn);

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
    assert!(tr.0.contains("blocked by guardrail"));
    // The file content never leaked into the session.
    assert!(!session
        .messages
        .iter()
        .any(|m| m.text().contains("classified")));
}
