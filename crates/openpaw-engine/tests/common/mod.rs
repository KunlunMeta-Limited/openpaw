//! Parity harness shared code (plan §6.2).
//!
//! A scenario is a JSONL script with four step kinds:
//! - `mock`      — builds the deterministic provider script
//! - `user`      — feeds a user message (one run_turn per step)
//! - `seed`      — injects filler user/assistant pairs (compaction setups)
//! - `guardrail` — installs an allowlist guardrail
//! - `compact`   — enables compaction with the given options
//! - `expect`    — structure-only assertions
//!
//! Comparison keys are structure only — turn count, stop reasons,
//! tool_use name sequences, event-kind sequences, error flags. Model text
//! is never a comparison key. Both sides inject the same deterministic
//! mock through `LlmProvider`, so fixtures are reproducible and
//! model-independent; the upstream side replays the identical script and
//! compares fixtures.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use openpaw_core::{
    AllowlistGuardrail, Error, Message, Session, StopReason, ToolCallOutcome, ToolHost, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop, CompactOptions};
use openpaw_llm::{MockMode, MockProvider};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Expect {
    pub stop: String,
    pub tool_calls: Vec<String>,
    /// Tool names whose result must be an error (e.g. guardrail denies).
    pub error_tools: Vec<String>,
    pub final_reply: Option<String>,
    pub min_parallel: usize,
    /// The run must show a compaction event.
    pub expect_compaction: bool,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub mock: Vec<MockMode>,
    pub user: Vec<String>,
    pub seed_pairs: usize,
    pub allow_tools: Option<Vec<String>>,
    pub compact: Option<CompactOptions>,
    pub expect: Expect,
}

pub fn scenario_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity/scenarios")
}

pub fn parse_scenario(path: &PathBuf) -> Scenario {
    let raw = std::fs::read_to_string(path).expect("read scenario");
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scenario")
        .to_string();
    let mut mock: Vec<MockMode> = Vec::new();
    let mut user: Vec<String> = Vec::new();
    let mut seed_pairs = 0usize;
    let mut allow_tools: Option<Vec<String>> = None;
    let mut compact: Option<CompactOptions> = None;
    let mut expect: Option<Expect> = None;

    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).expect("scenario line is JSON");
        let step = v
            .get("step")
            .and_then(|s| s.as_str())
            .expect("step field")
            .to_string();
        match step.as_str() {
            "mock" => mock.push(parse_mock_mode(&v)),
            "user" => user.push(
                v.get("text")
                    .and_then(|t| t.as_str())
                    .expect("user text")
                    .to_string(),
            ),
            "seed" => seed_pairs = v.get("pairs").and_then(|p| p.as_u64()).unwrap_or(0) as usize,
            "guardrail" => {
                allow_tools = Some(
                    v.get("allow")
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .expect("guardrail allow list"),
                );
            }
            "compact" => {
                compact = Some(CompactOptions {
                    model_limit: v
                        .get("model_limit")
                        .and_then(|m| m.as_u64())
                        .unwrap_or(128_000) as usize,
                    keep_recent_pairs: v
                        .get("keep_recent_pairs")
                        .and_then(|k| k.as_u64())
                        .unwrap_or(2) as usize,
                    min_messages_before_compact: v
                        .get("min_messages")
                        .and_then(|m| m.as_u64())
                        .unwrap_or(4) as usize,
                    ..Default::default()
                });
            }
            "expect" => {
                expect = Some(Expect {
                    stop: v
                        .get("stop")
                        .and_then(|s| s.as_str())
                        .unwrap_or("end_turn")
                        .to_string(),
                    tool_calls: v
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    error_tools: v
                        .get("error_tools")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    final_reply: v
                        .get("final_reply")
                        .and_then(|t| t.as_str())
                        .map(String::from),
                    min_parallel: v.get("min_parallel").and_then(|m| m.as_u64()).unwrap_or(0)
                        as usize,
                    expect_compaction: v
                        .get("expect_compaction")
                        .and_then(|c| c.as_bool())
                        .unwrap_or(false),
                });
            }
            other => panic!("unknown scenario step: {other}"),
        }
    }

    Scenario {
        name,
        mock,
        user,
        seed_pairs,
        allow_tools,
        compact,
        expect: expect.expect("scenario needs an expect step"),
    }
}

fn parse_mock_mode(v: &Value) -> MockMode {
    let mode = v.get("mode").and_then(|m| m.as_str()).expect("mock mode");
    let text = |k: &str| v.get(k).and_then(|t| t.as_str()).unwrap_or("").to_string();
    match mode {
        "fixed" => MockMode::Fixed(text("text")),
        "tool_use" => MockMode::ToolUse {
            tool_name: text("tool"),
            input: v.get("input").cloned().unwrap_or(Value::Null),
            text: text("text"),
        },
        "parallel_tool_use" => MockMode::ParallelToolUse {
            tools: v
                .get("tools")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .map(|pair| {
                            let arr = pair.as_array().expect("tool pair array");
                            (
                                arr[0].as_str().expect("tool name").to_string(),
                                arr.get(1).cloned().unwrap_or(Value::Null),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            text: text("text"),
        },
        other => panic!("unknown mock mode: {other}"),
    }
}

/// Structure-only transcript record (fixture content).
#[derive(Debug)]
pub struct TurnRecord {
    pub stop: String,
    pub tool_calls: Vec<String>,
    /// Tool names whose result was an error, in order.
    pub error_tools: Vec<String>,
    pub event_kinds: Vec<String>,
}

pub struct ParityTranscript {
    pub scenario: String,
    pub turns: Vec<TurnRecord>,
    pub final_reply: Option<String>,
    pub max_tool_concurrency: usize,
    pub provider_calls: u32,
}

/// A tiny host exposing the demo tools the scenarios reference, with
/// async-sleep concurrency observation for the parallel assertion.
struct DemoHost {
    max_concurrent: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl openpaw_core::ToolExecutor for DemoHost {
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        Ok(vec![
            ToolSpec::new("echo_tool", "echoes text"),
            ToolSpec::new("grep", "search"),
            ToolSpec::new("glob", "list files"),
            ToolSpec::new("bash", "run shell"),
        ])
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> openpaw_core::Result<ToolCallOutcome> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_concurrent.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        match name {
            "echo_tool" => Ok(ToolCallOutcome::ok(format!(
                "echo: {}",
                args.get("text").and_then(|t| t.as_str()).unwrap_or("")
            ))),
            "grep" | "glob" => Ok(ToolCallOutcome::ok(format!("{name} done"))),
            "bash" => Ok(ToolCallOutcome::ok("bash ran")),
            other => Err(Error::ToolHost(format!("unknown tool {other}"))),
        }
    }
}

#[async_trait::async_trait]
impl openpaw_core::ToolHostValidator for DemoHost {}

fn demo_host(max_concurrent: Arc<AtomicUsize>, active: Arc<AtomicUsize>) -> Arc<dyn ToolHost> {
    Arc::new(DemoHost {
        max_concurrent,
        active,
    })
}

/// Run a scenario end-to-end and return the structural transcript.
pub async fn run_scenario(sc: &Scenario) -> ParityTranscript {
    let provider = Arc::new(MockProvider::scripted(sc.mock.clone()));
    let max_conc = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let host = demo_host(Arc::clone(&max_conc), Arc::clone(&active));

    let mut loop_ = AgentLoop::new(provider.clone()).with_tool_host(host);
    if let Some(allow) = &sc.allow_tools {
        loop_ = loop_.with_guardrail(Arc::new(AllowlistGuardrail {
            allowed: allow.clone(),
        }));
    }
    if let Some(opts) = &sc.compact {
        loop_ = loop_.with_compact_options(opts.clone());
    }

    let mut session = Session::new("parity", "mock");
    for i in 0..sc.seed_pairs {
        session.messages.push(Message::user_text(format!(
            "seed question {i} with padding"
        )));
        session.messages.push(Message::assistant_text(format!(
            "seed answer {i} with padding"
        )));
    }

    let mut turns: Vec<TurnRecord> = Vec::new();
    for text in &sc.user {
        session.messages.push(Message::user_text(text.clone()));
        let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_log = Arc::clone(&log);
        let mut sink = move |ev: AgentEvent| {
            sink_log.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        };
        let stop = loop_
            .run_turn(&mut session, None, &[], &mut sink)
            .await
            .expect("run_turn ok");
        let events = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
        turns.push(TurnRecord {
            stop: stop_to_str(&stop),
            tool_calls: events
                .iter()
                .filter_map(|e| match e {
                    AgentEvent::ToolUseStart { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            error_tools: events
                .iter()
                .filter_map(|e| match e {
                    AgentEvent::ToolResult {
                        name,
                        is_error: true,
                        ..
                    } => name.clone(),
                    _ => None,
                })
                .collect(),
            event_kinds: events.iter().map(|e| e.kind().to_string()).collect(),
        });
    }

    let final_reply = session
        .messages
        .last()
        .filter(|m| m.role == openpaw_core::Role::Assistant)
        .map(|m| m.text());

    ParityTranscript {
        scenario: sc.name.clone(),
        turns,
        final_reply,
        max_tool_concurrency: max_conc.load(Ordering::SeqCst),
        provider_calls: provider.call_count(),
    }
}

fn stop_to_str(s: &StopReason) -> String {
    match s {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::StopSequence => "stop_sequence".into(),
        StopReason::Incomplete => "incomplete".into(),
        StopReason::Other => "other".into(),
    }
}

/// Assert scenario expectations against the transcript.
pub fn assert_expectations(sc: &Scenario, t: &ParityTranscript) {
    let all_tool_calls: Vec<String> = t
        .turns
        .iter()
        .flat_map(|turn| turn.tool_calls.iter().cloned())
        .collect();
    assert_eq!(
        all_tool_calls, sc.expect.tool_calls,
        "tool_use name sequence mismatch (structure key)"
    );
    let all_error_tools: Vec<String> = t
        .turns
        .iter()
        .flat_map(|turn| turn.error_tools.iter().cloned())
        .collect();
    assert_eq!(
        all_error_tools, sc.expect.error_tools,
        "error tool sequence mismatch"
    );
    let last_stop = t
        .turns
        .last()
        .map(|turn| turn.stop.clone())
        .unwrap_or_default();
    assert_eq!(last_stop, sc.expect.stop, "final stop reason mismatch");
    if let Some(reply) = &sc.expect.final_reply {
        assert_eq!(
            t.final_reply.as_deref(),
            Some(reply.as_str()),
            "final reply mismatch"
        );
    }
    assert!(
        t.max_tool_concurrency >= sc.expect.min_parallel,
        "expected ≥{} concurrent tool executions, got {}",
        sc.expect.min_parallel,
        t.max_tool_concurrency
    );
    if sc.expect.expect_compaction {
        assert!(
            t.turns
                .iter()
                .any(|turn| turn.event_kinds.iter().any(|k| k == "compaction_performed")),
            "expected a compaction event in the run"
        );
    }
}

/// Export the transcript fixture (JSON) under the supplied output directory and
/// return its path. Fixtures are the cross-repo parity evidence committed
/// to the repo (plan §6.2: "证据以 JSON 转录 fixture 形式导出入库"); the
/// upstream side replays the same script and compares structure keys.
/// 功能：导出可比较的回放转录到调用方专用目录。
/// 参数：t为转录；dir为当前测试独占的输出目录。
/// 返回：写出的JSON路径。
/// 错误：目录创建或写入失败会使测试panic。
/// 状态：创建目录并写入当前场景fixture。
/// 并发与取消：调用方隔离目录，不能同时覆盖同名场景；无取消。
/// 权限：仅测试临时目录写入。
/// 关联：确定性回放与并行测试隔离。
pub fn export_fixture(t: &ParityTranscript, dir: &std::path::Path) -> PathBuf {
    let fixture = serde_json::json!({
        "schema": "openpaw-parity-transcript/1",
        "scenario": t.scenario,
        "provider_calls": t.provider_calls,
        "max_tool_concurrency": t.max_tool_concurrency,
        "final_reply": t.final_reply,
        "turns": t.turns.iter().map(|turn| serde_json::json!({
            "stop": turn.stop,
            "tool_calls": turn.tool_calls,
            "error_tools": turn.error_tools,
            "event_kinds": turn.event_kinds,
        })).collect::<Vec<_>>(),
    });
    std::fs::create_dir_all(dir).expect("create fixture dir");
    let path = dir.join(format!("{}.json", t.scenario));
    std::fs::write(&path, serde_json::to_string_pretty(&fixture).unwrap()).expect("write fixture");
    path
}
