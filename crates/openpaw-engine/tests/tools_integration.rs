//! Engine × tools integration: the real `ToolRegistry` (fs tools + think)
//! drives `run_turn` end-to-end through the mock provider — the M2 exit
//! demo shape ("let the agent complete a small real task with grep").
use openpaw_core::{
    ContentBlock, Error, Message, Result, Session, StopReason, ToolCallOutcome, ToolExecutor,
    ToolHost, ToolHostValidator, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
struct ValidatingHost {
    validations: AtomicUsize,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl ToolExecutor for ValidatingHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("echo_tool", "echoes text")])
    }
    async fn call(&self, _name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolCallOutcome::ok(
            args.get("text")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
        ))
    }
}
#[async_trait::async_trait]
impl ToolHostValidator for ValidatingHost {
    async fn validate_input(
        &self,
        _name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        match input.get("text").and_then(|value| value.as_str()) {
            Some("invalid") => Err("text is invalid".into()),
            Some(_) => Ok(()),
            None => Err("text is required".into()),
        }
    }
}
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct PendingValidatorHost {
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl ToolExecutor for PendingValidatorHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("echo_tool", "echoes text")])
    }
    async fn call(&self, _name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::ToolHost("validator must stop the call".into()))
    }
}
#[async_trait::async_trait]
impl ToolHostValidator for PendingValidatorHost {
    async fn validate_input(
        &self,
        _name: &str,
        _input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        let _drop = DropFlag(Arc::clone(&self.dropped));
        std::future::pending().await
    }
}
struct PendingParallelHost {
    entered: Arc<Semaphore>,
    dropped: Arc<Semaphore>,
    calls: AtomicUsize,
    drops: Arc<AtomicUsize>,
}
struct ParallelToolDropGuard {
    dropped: Arc<Semaphore>,
    drops: Arc<AtomicUsize>,
}
impl Drop for ParallelToolDropGuard {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        self.dropped.add_permits(1);
    }
}
#[async_trait::async_trait]
impl ToolExecutor for PendingParallelHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("pending_tool", "waits until dropped")])
    }
    async fn call(&self, _name: &str, _args: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _drop_guard = ParallelToolDropGuard {
            dropped: Arc::clone(&self.dropped),
            drops: Arc::clone(&self.drops),
        };
        self.entered.add_permits(1);
        std::future::pending().await
    }
}
#[async_trait::async_trait]
impl ToolHostValidator for PendingParallelHost {
    async fn validate_input(
        &self,
        _name: &str,
        _input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        Ok(())
    }
}
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
#[tokio::test]
async fn parallel_tools_validate_every_input_before_execution() {
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: vec![
                ("echo_tool".into(), serde_json::json!({"text": "valid"})),
                ("echo_tool".into(), serde_json::json!({"text": "invalid"})),
            ],
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let host = Arc::new(ValidatingHost {
        validations: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    });
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let loop_ = AgentLoop::new(provider).with_tool_host(host_dyn);
    let mut session = Session::new("parallel-validator", "mock");
    session.messages.push(Message::user_text("run both"));
    let mut sink = |_event: AgentEvent| {};
    let stop = loop_
        .run_turn(&mut session, None, &[], &mut sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(host.validations.load(Ordering::SeqCst), 2);
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert!(session.messages.iter().flat_map(|message| &message.content).any(
        |block| matches!(block, ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("invalid input"))
    ));
}
#[tokio::test]
async fn cancellation_drops_pending_validator_without_executing_tool() {
    let provider = Arc::new(MockProvider::scripted(vec![MockMode::ToolUse {
        tool_name: "echo_tool".into(),
        input: serde_json::json!({"text": "valid"}),
        text: "calling".into(),
    }]));
    let dropped = Arc::new(AtomicBool::new(false));
    let host = Arc::new(PendingValidatorHost {
        dropped: Arc::clone(&dropped),
        calls: AtomicUsize::new(0),
    });
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host_dyn)
        .with_parallel_tools(false);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let mut session = Session::new("pending-validator", "mock");
    session.messages.push(Message::user_text("run"));
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
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn aborting_parent_drops_parallel_tools_and_releases_active_turn() {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let provider = Arc::new(MockProvider::scripted(vec![
            MockMode::ParallelToolUse {
                tools: vec![
                    ("pending_tool".into(), serde_json::json!({"call": 1})),
                    ("pending_tool".into(), serde_json::json!({"call": 2})),
                ],
                text: "calling both".into(),
            },
            MockMode::Fixed("active turn released".into()),
        ]));
        let host = Arc::new(PendingParallelHost {
            entered: Arc::new(Semaphore::new(0)),
            dropped: Arc::new(Semaphore::new(0)),
            calls: AtomicUsize::new(0),
            drops: Arc::new(AtomicUsize::new(0)),
        });
        let host_dyn: Arc<dyn ToolHost> = host.clone();
        let loop_ = Arc::new(AgentLoop::new(provider).with_tool_host(host_dyn));
        let running_loop = Arc::clone(&loop_);
        let parent = tokio::spawn(async move {
            let mut session = Session::new("aborted-parallel-tools", "mock");
            session.messages.push(Message::user_text("run both"));
            let mut sink = |_event: AgentEvent| {};
            running_loop
                .run_turn(&mut session, None, &[], &mut sink)
                .await
        });
        host.entered
            .acquire_many(2)
            .await
            .expect("entry semaphore remains open")
            .forget();
        assert_eq!(host.calls.load(Ordering::SeqCst), 2);
        parent.abort();
        assert!(parent
            .await
            .expect_err("parent turn must be aborted")
            .is_cancelled());
        host.dropped
            .acquire_many(2)
            .await
            .expect("drop semaphore remains open")
            .forget();
        assert_eq!(host.drops.load(Ordering::SeqCst), 2);
        let mut retry_session = Session::new("retry-after-abort", "mock");
        retry_session
            .messages
            .push(Message::user_text("run another turn"));
        let mut retry_sink = |_event: AgentEvent| {};
        let stop = loop_
            .run_turn(&mut retry_session, None, &[], &mut retry_sink)
            .await
            .expect("active turn gate must be reusable after parent abort");
        assert_eq!(stop, StopReason::EndTurn);
        assert_eq!(
            retry_session.messages.last().unwrap().text(),
            "active turn released"
        );
    })
    .await
    .expect("parallel tool abort scenario must finish within one second");
}
