//! Hook e2e tests — AgentLoop wired to an `openpaw_plugins::HookChain`.
//!
//! Uses the same deterministic-mock scaffolding as the other engine e2e
//! suites (scripted `MockProvider` + event-sink capture + echo host);
//! the parity runner in `tests/common` builds its loop without a hook
//! injection point, so the chain is attached inline here.
use openpaw_core::{
    ApprovalManager, ApprovalRequest, ApprovalResponse, ContentBlock, Error, GuardrailDecision,
    Message, Result, Session, StopReason, ToolCallOutcome, ToolGuardrail, ToolHost, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};
use openpaw_plugins::{HookChain, HookDecision, HookPoint, HookResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
/// Echo host that counts executed calls, so tests can prove a blocked
/// tool call never reached the host.
struct CountingEchoHost {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl openpaw_core::ToolExecutor for CountingEchoHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("echo_tool", "echoes text")])
    }
    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if name != "echo_tool" {
            return Err(Error::ToolHost(format!("unknown tool {name}")));
        }
        Ok(ToolCallOutcome::ok(format!(
            "echo: {}",
            args.get("text").and_then(|v| v.as_str()).unwrap_or("")
        )))
    }
}
#[async_trait::async_trait]
impl openpaw_core::ToolHostValidator for CountingEchoHost {}
struct StrictEchoHost {
    calls: AtomicUsize,
    validations: AtomicUsize,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}
#[async_trait::async_trait]
impl openpaw_core::ToolExecutor for StrictEchoHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("echo_tool", "echoes text")])
    }
    async fn call(&self, _name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(args.clone());
        Ok(ToolCallOutcome::ok(format!(
            "echo: {}",
            args.get("text")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        )))
    }
}
#[async_trait::async_trait]
impl openpaw_core::ToolHostValidator for StrictEchoHost {
    async fn validate_input(
        &self,
        _name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        match input.get("text").and_then(|value| value.as_str()) {
            Some("invalid") => Err("replacement rejected".into()),
            Some(_) => Ok(()),
            None => Err("text is required".into()),
        }
    }
}
struct ReplacementGuardrail;
#[async_trait::async_trait]
impl ToolGuardrail for ReplacementGuardrail {
    async fn check(&self, _name: &str, args: &serde_json::Value) -> GuardrailDecision {
        match args.get("text").and_then(|value| value.as_str()) {
            Some("denied") => GuardrailDecision::Deny("replacement denied".into()),
            Some("confirm") => GuardrailDecision::Confirm("replacement confirm".into()),
            _ => GuardrailDecision::Allow,
        }
    }
}
struct RecordingApproval {
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
}
#[async_trait::async_trait]
impl ApprovalManager for RecordingApproval {
    async fn request(&self, request: ApprovalRequest) -> ApprovalResponse {
        self.seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        ApprovalResponse::Approved
    }
}
fn replacement_chain(value: serde_json::Value) -> Arc<HookChain> {
    let mut chain = HookChain::new();
    chain.register(HookPoint::BeforeToolCall, "replace", move |_context| {
        let value = value.clone();
        async move {
            HookResult {
                decision: HookDecision::Replace(value),
                modified_payload: None,
            }
        }
    });
    Arc::new(chain)
}
/// Helper: run one turn and capture events (same pattern as engine_e1).
async fn run_turn(
    loop_: &AgentLoop,
    session: &mut Session,
) -> (Result<StopReason>, Vec<AgentEvent>) {
    let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let mut sink = move |ev: AgentEvent| {
        sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
    };
    let out = loop_.run_turn(session, None, &[], &mut sink).await;
    let events = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (out, events)
}
#[tokio::test]
async fn before_tool_call_hook_blocks_execution() {
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "x"}),
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let host = Arc::new(CountingEchoHost {
        calls: AtomicUsize::new(0),
    });
    let mut chain = HookChain::new();
    chain.register(HookPoint::BeforeToolCall, "blocker", |_ctx| async move {
        HookResult {
            decision: HookDecision::Skip,
            modified_payload: None,
        }
    });
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let l = AgentLoop::new(p)
        .with_tool_host(host_dyn)
        .with_hook_chain(Arc::new(chain))
        .with_parallel_tools(false);
    let mut session = Session::new("hook-block", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_turn(&l, &mut session).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    // The blocked call reached the model as an error tool_result.
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
        .expect("tool result present");
    assert!(tr.1, "hook block must surface as an error result");
    assert!(tr.0.contains("blocked by hook"));
    // The hook decided before the tool host was ever invoked.
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn after_tool_call_hook_observes_result_summary() {
    let p = Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: vec![
                ("echo_tool".into(), serde_json::json!({"text": "a"})),
                ("echo_tool".into(), serde_json::json!({"text": "b"})),
            ],
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let host: Arc<dyn ToolHost> = Arc::new(CountingEchoHost {
        calls: AtomicUsize::new(0),
    });
    let observed: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let mut chain = HookChain::new();
    let sink = Arc::clone(&observed);
    chain.register(HookPoint::AfterToolCall, "observer", move |ctx| {
        let sink = Arc::clone(&sink);
        async move {
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(ctx.payload);
            HookResult {
                decision: HookDecision::Continue,
                modified_payload: None,
            }
        }
    });
    // Two parallel safe calls → phased spawn path (hook chain carried
    // into the spawned tasks via clone).
    let l = AgentLoop::new(p)
        .with_tool_host(host)
        .with_hook_chain(Arc::new(chain));
    let mut session = Session::new("hook-observe", "mock");
    session.messages.push(Message::user_text("search"));
    let (stop, _) = run_turn(&l, &mut session).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    // The observer fired once per executed call, with the result summary.
    let records = observed.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(records.len(), 2, "observer must fire once per tool call");
    let mut texts: Vec<&str> = Vec::new();
    for r in records.iter() {
        assert_eq!(r["tool_name"], "echo_tool");
        assert_eq!(r["result"]["is_error"].as_bool(), Some(false));
        let content = r["result"]["content"].as_str().expect("result content");
        assert!(
            content.starts_with("echo: "),
            "summary in payload: {content}"
        );
        texts.push(r["input"]["text"].as_str().expect("input text"));
    }
    texts.sort_unstable();
    assert_eq!(texts, vec!["a", "b"]);
    // Observer-only hook: outcomes pass through unchanged.
    let mut results: Vec<String> = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    results.sort_unstable();
    assert_eq!(results, vec!["echo: a", "echo: b"]);
}
#[tokio::test]
async fn before_hook_invalid_replacement_is_revalidated_in_serial_path() {
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "valid"}),
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let host = Arc::new(StrictEchoHost {
        calls: AtomicUsize::new(0),
        validations: AtomicUsize::new(0),
        seen: Arc::new(Mutex::new(Vec::new())),
    });
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host_dyn)
        .with_hook_chain(replacement_chain(
            serde_json::json!({"input": {"text": "invalid"}}),
        ))
        .with_parallel_tools(false);
    let mut session = Session::new("replace-invalid", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_turn(&loop_, &mut session).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(host.validations.load(Ordering::SeqCst), 2);
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    assert!(session.messages.iter().flat_map(|message| &message.content).any(
        |block| matches!(block, ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("invalid input after hook"))
    ));
}
#[tokio::test]
async fn before_hook_missing_input_is_rejected_in_parallel_path() {
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: vec![
                ("echo_tool".into(), serde_json::json!({"text": "a"})),
                ("echo_tool".into(), serde_json::json!({"text": "b"})),
            ],
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let host = Arc::new(StrictEchoHost {
        calls: AtomicUsize::new(0),
        validations: AtomicUsize::new(0),
        seen: Arc::new(Mutex::new(Vec::new())),
    });
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host_dyn)
        .with_hook_chain(replacement_chain(serde_json::json!({"other": true})));
    let mut session = Session::new("replace-missing", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_turn(&loop_, &mut session).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(host.validations.load(Ordering::SeqCst), 2);
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn before_hook_replacement_is_reclassified_and_approved() {
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "echo_tool".into(),
            input: serde_json::json!({"text": "valid"}),
            text: "calling".into(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let seen_inputs = Arc::new(Mutex::new(Vec::new()));
    let host = Arc::new(StrictEchoHost {
        calls: AtomicUsize::new(0),
        validations: AtomicUsize::new(0),
        seen: Arc::clone(&seen_inputs),
    });
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let host_dyn: Arc<dyn ToolHost> = host.clone();
    let loop_ = AgentLoop::new(provider)
        .with_tool_host(host_dyn)
        .with_guardrail(Arc::new(ReplacementGuardrail))
        .with_approval_manager(Arc::new(RecordingApproval {
            seen: Arc::clone(&approvals),
        }))
        .with_hook_chain(replacement_chain(
            serde_json::json!({"input": {"text": "confirm"}}),
        ))
        .with_parallel_tools(false);
    let mut session = Session::new("replace-confirm", "mock");
    session.messages.push(Message::user_text("go"));
    let (stop, _) = run_turn(&loop_, &mut session).await;
    assert_eq!(stop.unwrap(), StopReason::EndTurn);
    assert_eq!(host.validations.load(Ordering::SeqCst), 2);
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    let approvals = approvals.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].input["text"], "confirm");
    assert_eq!(approvals[0].reason, "replacement confirm");
    assert_eq!(
        seen_inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())[0]["text"],
        "confirm"
    );
}
