//! 职责：最终输入与工具调度回归。
//! 输入输出：Mock输入到真实引擎事件和执行记录。
//! 依赖：原AgentLoop/MockProvider。
//! 禁止职责：不复制执行或审批实现。
//! 工具授权与调度缺陷回归；使用原MockProvider，不复制引擎。
use async_trait::async_trait;
use openpaw_core::*;
use openpaw_engine::{AgentEvent, AgentLoop};
use openpaw_llm::{MockMode, MockProvider};
use openpaw_plugins::{HookChain, HookDecision, HookPoint, HookResult};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Host {
    calls: Mutex<Vec<Value>>,
    invalid: bool,
}
#[async_trait]
impl ToolExecutor for Host {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![ToolSpec::new("write", "test")])
    }
    async fn call(&self, _: &str, args: Value) -> Result<ToolCallOutcome> {
        self.calls.lock().unwrap().push(args);
        Ok(ToolCallOutcome::ok("executed"))
    }
}
#[async_trait]
impl ToolHostValidator for Host {
    async fn validate_input(&self, _: &str, _: &Value) -> std::result::Result<(), String> {
        if self.invalid {
            Err("invalid final input".into())
        } else {
            Ok(())
        }
    }
}
struct Approver(Mutex<Vec<Value>>);
#[async_trait]
impl ApprovalManager for Approver {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        self.0.lock().unwrap().push(req.input);
        ApprovalResponse::Approved
    }
}
struct Confirm;
#[async_trait]
impl ToolGuardrail for Confirm {
    async fn check(&self, _: &str, _: &Value) -> GuardrailDecision {
        GuardrailDecision::Confirm("write".into())
    }
}
fn provider(count: usize) -> Arc<MockProvider> {
    Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: (0..count)
                .map(|_| ("write".into(), json!({"path":"before"})))
                .collect(),
            text: String::new(),
        },
        MockMode::Fixed("done".into()),
    ]))
}
async fn run(engine: AgentLoop) -> Vec<AgentEvent> {
    let mut session = Session::new("test", "mock");
    session.messages.push(Message::user_text("go"));
    let mut events = vec![];
    engine
        .run_turn(&mut session, None, &[], &mut |e| events.push(e))
        .await
        .unwrap();
    events
}
#[tokio::test]
async fn d05_hook_input_is_what_user_approves() {
    for count in [1, 2] {
        let host = Arc::new(Host {
            calls: Mutex::new(vec![]),
            invalid: false,
        });
        let approver = Arc::new(Approver(Mutex::new(vec![])));
        let mut hooks = HookChain::new();
        hooks.register(HookPoint::BeforeToolCall, "rewrite", |_| async {
            HookResult {
                decision: HookDecision::Replace(json!({"input":{"path":"after"}})),
                modified_payload: None,
            }
        });
        run(AgentLoop::new(provider(count))
            .with_tool_host(host.clone())
            .with_guardrail(Arc::new(Confirm))
            .with_approval_manager(approver.clone())
            .with_hook_chain(Arc::new(hooks)))
        .await;
        let approved = approver.0.lock().unwrap();
        let executed = host.calls.lock().unwrap();
        assert_eq!(
            *approved, *executed,
            "approved input must equal executed input"
        );
        assert_eq!(approved[0]["path"], "after");
    }
}
#[tokio::test]
async fn d05_parallel_validation_prevents_all_side_effects() {
    let host = Arc::new(Host {
        calls: Mutex::new(vec![]),
        invalid: true,
    });
    let events = run(AgentLoop::new(provider(2)).with_tool_host(host.clone())).await;
    assert!(host.calls.lock().unwrap().is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolResult { is_error: true, .. }))
            .count(),
        2
    );
}

use std::sync::atomic::{AtomicUsize, Ordering};
struct SchedulerHost {
    concurrency: ToolConcurrency,
    active: AtomicUsize,
    maximum: AtomicUsize,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl ToolExecutor for SchedulerHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let mut spec = ToolSpec::new("work", "scheduling regression");
        spec.concurrency = Some(self.concurrency);
        Ok(vec![spec])
    }
    async fn call(&self, _: &str, args: Value) -> Result<ToolCallOutcome> {
        if args["kind"] == "panic" {
            panic!("intentional tool panic");
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        if args["kind"] == "slow" {
            tokio::time::timeout(std::time::Duration::from_secs(2), self.release.notified())
                .await
                .expect("fast result must be emitted while slow tool is still waiting");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolCallOutcome::ok(args["kind"].as_str().unwrap()))
    }
}
#[async_trait]
impl ToolHostValidator for SchedulerHost {}
fn scheduled_provider(kinds: &[&str]) -> Arc<MockProvider> {
    Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: kinds
                .iter()
                .map(|k| ("work".into(), json!({"kind":k})))
                .collect(),
            text: String::new(),
        },
        MockMode::Fixed("done".into()),
    ]))
}
#[tokio::test]
async fn d06_fast_result_is_reported_before_slow_tool_finishes() {
    let release = Arc::new(tokio::sync::Notify::new());
    let host = Arc::new(SchedulerHost {
        concurrency: ToolConcurrency::ParallelSafe,
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: release.clone(),
    });
    let engine = AgentLoop::new(scheduled_provider(&["slow", "fast"])).with_tool_host(host.clone());
    let mut session = Session::new("test", "mock");
    session.messages.push(Message::user_text("go"));
    let mut durations = vec![];
    engine
        .run_turn(&mut session, None, &[], &mut |event| {
            if let AgentEvent::ToolResult {
                content,
                duration_ms,
                ..
            } = event
            {
                if content == "fast" {
                    release.notify_one();
                }
                durations.push((content, duration_ms));
            }
        })
        .await
        .unwrap();
    assert_eq!(durations[0].0, "fast");
    assert_eq!(host.active.load(Ordering::SeqCst), 0);
    assert!(durations[0].1.unwrap() < 1500);
}
#[tokio::test]
async fn d05_unknown_and_exclusive_metadata_do_not_overlap() {
    for concurrency in [ToolConcurrency::NeverParallel, ToolConcurrency::PathScoped] {
        let host = Arc::new(SchedulerHost {
            concurrency,
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            release: Arc::new(tokio::sync::Notify::new()),
        });
        run(AgentLoop::new(scheduled_provider(&["fast", "fast"])).with_tool_host(host.clone()))
            .await;
        assert_eq!(host.maximum.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn d06_panicking_parallel_tool_keeps_its_call_id() {
    let host = Arc::new(SchedulerHost {
        concurrency: ToolConcurrency::ParallelSafe,
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let events =
        run(AgentLoop::new(scheduled_provider(&["panic", "fast"])).with_tool_host(host)).await;
    let panic = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult {
                tool_use_id,
                is_error: true,
                ..
            } => Some(tool_use_id),
            _ => None,
        })
        .unwrap();
    assert_eq!(panic, "call_0_0");
}
#[tokio::test]
async fn d05_metadata_requires_approval_even_when_guardrail_allows() {
    struct NeedsApproval;
    #[async_trait]
    impl ToolExecutor for NeedsApproval {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            let mut spec = ToolSpec::new("write", "test");
            spec.requires_approval = true;
            Ok(vec![spec])
        }
        async fn call(&self, _: &str, _: Value) -> Result<ToolCallOutcome> {
            panic!("denied tool must never run")
        }
    }
    #[async_trait]
    impl ToolHostValidator for NeedsApproval {}
    let events = run(AgentLoop::new(provider(1))
        .with_tool_host(Arc::new(NeedsApproval))
        .with_approval_manager(Arc::new(AutoDenyManager)))
    .await;
    assert!(events.iter().any(|e|matches!(e,AgentEvent::ToolResult{is_error:true,content,..}if content.contains("denied"))));
}

#[tokio::test]
async fn d05_execution_record_failure_fails_run_and_keeps_result_pairing() {
    struct FailStart;
    #[async_trait]
    impl ToolExecutionObserver for FailStart {
        async fn started(&self, _: &ToolCallBinding) -> Result<()> {
            // 观察失败即使命名Cancelled，也不能误报用户取消或被模型掩盖。
            Err(Error::Cancelled)
        }
    }
    struct ObservedHost {
        concurrency: ToolConcurrency,
        effects: AtomicUsize,
    }
    #[async_trait]
    impl ToolExecutor for ObservedHost {
        async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
            let mut spec = ToolSpec::new("write", "test execution boundary");
            spec.concurrency = Some(self.concurrency);
            Ok(vec![spec])
        }
        async fn call(&self, _: &str, _: Value) -> Result<ToolCallOutcome> {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(ToolCallOutcome::ok("side effect"))
        }
        async fn call_with_context(
            &self,
            name: &str,
            args: Value,
            context: ToolCallContext,
        ) -> Result<ToolCallOutcome> {
            context
                .with_observer(Arc::new(FailStart))
                .execution_started(name, &args)
                .await?;
            self.call(name, args).await
        }
    }
    #[async_trait]
    impl ToolHostValidator for ObservedHost {}
    struct Records(Vec<SessionEvent>);
    #[async_trait]
    impl openpaw_engine::RunObserver for Records {
        async fn record(&mut self, event: SessionEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    for concurrency in [
        ToolConcurrency::NeverParallel,
        ToolConcurrency::ParallelSafe,
    ] {
        let host = Arc::new(ObservedHost {
            concurrency,
            effects: AtomicUsize::new(0),
        });
        let engine = AgentLoop::new(provider(2)).with_tool_host(host.clone());
        let mut session = Session::new("test", "mock");
        session.messages.push(Message::user_text("go"));
        let mut records = Records(vec![]);
        let result = engine
            .run_with_context(
                &mut session,
                None,
                &[],
                &openpaw_engine::RunContext::default(),
                &mut records,
                &mut |_| {},
            )
            .await;
        assert_eq!(result.status, openpaw_engine::RunStatus::Failed);
        assert!(matches!(result.error, Some(Error::Store(_))));
        assert_eq!(host.effects.load(Ordering::SeqCst), 0);
        let results: Vec<_> = session
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|c| match c {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.clone(), *is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, error)| *error));
        assert!(records
            .0
            .iter()
            .any(|e| matches!(e,SessionEvent::RunFinished{status,..} if status=="failed")));
    }
}

#[tokio::test]
async fn d05_real_file_tool_denial_preserves_bytes_and_approval_writes_final_input_once() {
    for approved in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.txt"), "unchanged").unwrap();
        let provider = Arc::new(MockProvider::scripted(vec![
            MockMode::ParallelToolUse {
                tools: vec![(
                    "write_file".into(),
                    json!({"path":"report.txt","content":"unapproved input"}),
                )],
                text: String::new(),
            },
            MockMode::Fixed("done".into()),
        ]));
        struct Decision {
            approved: bool,
            requests: AtomicUsize,
        }
        #[async_trait]
        impl ApprovalManager for Decision {
            async fn request(&self, request: ApprovalRequest) -> ApprovalResponse {
                self.requests.fetch_add(1, Ordering::SeqCst);
                assert_eq!(request.input["content"], "final approved content");
                let binding = request.binding.unwrap();
                assert_eq!(binding.input, request.input);
                assert!(!binding.run_id.is_empty());
                assert_eq!(binding.tool_name, "write_file");
                if self.approved {
                    ApprovalResponse::Approved
                } else {
                    ApprovalResponse::Denied {
                        reason: "user rejected".into(),
                    }
                }
            }
        }
        let decision = Arc::new(Decision {
            approved,
            requests: AtomicUsize::new(0),
        });
        let mut hooks = HookChain::new();
        hooks.register(HookPoint::BeforeToolCall, "final file input", |_| async {
            HookResult {
                decision: HookDecision::Replace(
                    json!({"input":{"path":"report.txt","content":"final approved content"}}),
                ),
                modified_payload: None,
            }
        });
        // 不设置Confirm guardrail，真实注册表的写metadata必须触发原审批器。
        run(AgentLoop::new(provider)
            .with_tool_host(Arc::new(openpaw_tools::ToolRegistry::new(dir.path())))
            .with_approval_manager(decision.clone())
            .with_hook_chain(Arc::new(hooks)))
        .await;
        assert_eq!(decision.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("report.txt")).unwrap(),
            if approved {
                "final approved content"
            } else {
                "unchanged"
            }
        );
    }
}

#[tokio::test]
async fn d07_real_engine_approval_reaches_original_bash_environment() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ParallelToolUse {
            tools: vec![(
                "bash".into(),
                json!({"command":"echo approved-engine > result.txt"}),
            )],
            text: String::new(),
        },
        MockMode::Fixed("done".into()),
    ]));
    let events = run(AgentLoop::new(provider)
        .with_tool_host(Arc::new(openpaw_tools::ToolRegistry::new(dir.path())))
        .with_approval_manager(Arc::new(AutoApproveManager)))
    .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult {
                is_error: false,
                ..
            }
        )),
        "实际命令审批/执行事件：{events:#?}"
    );
    assert!(std::fs::read_to_string(dir.path().join("result.txt"))
        .unwrap()
        .contains("approved-engine"));
}
