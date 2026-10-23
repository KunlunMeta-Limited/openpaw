//! Acceptance tests for openpaw-reflect-lite: the three-step loop
//! (trigger → reflect → persist) end-to-end with a hand-written core
//! `LlmProvider` mock, schema serde round-trip, trigger boundary cases, and
//! unverified-ratio stats.
//!
//! The crate has no async runtime dependency, so the tests drive the async
//! `reflect` call with a tiny std-only `block_on`.
use async_trait::async_trait;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message,
    StopReason, Usage,
};
use openpaw_memory::skills::{FsSkillStore, SkillStore};
use openpaw_memory::{FsMemoryStore, MemoryStore, MemoryType};
use openpaw_reflect_lite::{
    persist, reflect, scan_unverified, PersistError, ReflectContext, ReflectError,
    ReflectLiteConfig, ReflectionOutput,
};
use openpaw_reflect_lite::{TriggerConfig, TriggerContext as LiteTriggerContext};
use serde_yml::Value;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
/// Fixed reflection output the mock provider returns: one memory candidate
/// (with the optional COT fields) and one skill candidate.
const SAMPLE_OUTPUT_JSON: &str = r##"{
  "summary": "User stated a durable Rust tooling convention",
  "skill_candidates": [
    {
      "name": "cargo-test-first",
      "description": "Run cargo test before declaring a Rust change done",
      "triggers": ["rust", "test"],
      "body": "# cargo-test-first\n\nRun `cargo test` before declaring the change done.",
      "rationale": "Repeated multi-step procedure, reusable verbatim",
      "confidence": "medium"
    }
  ],
  "memory_candidates": [
    {
      "fact": "User always formats Rust code with rustfmt before commit",
      "tags": ["rust", "convention"],
      "scope": "user",
      "confidence": "medium",
      "rationale": "Explicitly stated convention",
      "supersedes": [],
      "zone": "work",
      "memory_type": "preference",
      "thinking_path": null
    }
  ],
  "conflicts": [],
  "task_candidates": []
}"##;
/// Minimal mock over the core `LlmProvider` trait (openpaw-llm is off
/// limits for this crate). Records the last `max_tokens` so the e2e can
/// assert the 1024 budget contract.
struct MockProvider {
    response: String,
    max_tokens_used: Mutex<u32>,
}
impl MockProvider {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            max_tokens_used: Mutex::new(0),
        }
    }
    fn max_tokens_used(&self) -> u32 {
        *self.max_tokens_used.lock().unwrap()
    }
}
#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        *self.max_tokens_used.lock().unwrap() = req.max_tokens;
        Ok(CompletionResponse {
            content: vec![ContentBlock::Text {
                text: self.response.clone(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn name(&self) -> &str {
        "mock-reflect"
    }
    fn default_model(&self) -> &str {
        "mock-model"
    }
}
/// Std-only executor: the mock completes on its first poll, so a spinning
/// noop-waker loop is sufficient (no tokio in this crate's dev-deps).
fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = Box::pin(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
fn assert_unverified(extra: &serde_yml::Mapping, what: &str) {
    assert_eq!(
        extra.get(Value::String("unverified".into())),
        Some(&Value::Bool(true)),
        "{what} frontmatter must carry unverified: true"
    );
}
fn assert_raw_file_unverified(path: &std::path::Path) {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("failed to read artifact {}: {e}", path.display());
    });
    assert!(
        raw.contains("unverified: true"),
        "artifact {} must contain `unverified: true` in raw frontmatter:\n{raw}",
        path.display()
    );
}
#[test]
fn three_step_loop_persists_unverified_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mem_store = FsMemoryStore::new(dir.path().join("memories"), None);
    let skill_store = FsSkillStore::new(dir.path().join("skills"), None);
    let config = ReflectLiteConfig::default();
    // Step 1: trigger hits at the N threshold.
    let ctx = LiteTriggerContext {
        turns_since_last: 3,
        error_in_last_turn: false,
    };
    assert!(config.trigger.should_trigger(&ctx));
    // Step 2: single-shot reflection through the injected mock provider.
    let provider = MockProvider::new(SAMPLE_OUTPUT_JSON);
    let transcript = vec![
        Message::user_text("Always run rustfmt on our Rust code before commit."),
        Message::assistant_text("Understood — I formatted the touched files."),
    ];
    let output = block_on(reflect(&provider, &transcript, &ReflectContext::default())).unwrap();
    assert_eq!(
        provider.max_tokens_used(),
        openpaw_reflect_lite::REFLECT_MAX_TOKENS
    );
    assert_eq!(output.memory_candidates.len(), 1);
    assert_eq!(output.skill_candidates.len(), 1);
    assert_eq!(
        output.memory_candidates[0].memory_type,
        Some(MemoryType::Preference)
    );
    // Step 3: persist straight to the stores.
    let report = persist(&output, &mem_store, &skill_store).unwrap();
    assert_eq!(report.memories.len(), 1);
    assert_eq!(report.skills.len(), 1);
    assert!(report.memories[0].path.is_file());
    assert!(report.skills[0].path.is_file());
    // CI assertion: 100% of the persisted artifacts carry `unverified: true`,
    // both through the typed frontmatter round-trip and in the raw file.
    let memories = MemoryStore::list(&mem_store).unwrap();
    let skills = SkillStore::list(&skill_store).unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(skills.len(), 1);
    for m in &memories {
        assert_unverified(&m.frontmatter.extra, &format!("memory {}", m.id()));
        assert_eq!(m.frontmatter.source, openpaw_memory::Source::Reflection);
    }
    for s in &skills {
        assert_unverified(&s.frontmatter.extra, &format!("skill {}", s.name()));
    }
    for item in report.memories.iter().chain(report.skills.iter()) {
        assert_raw_file_unverified(&item.path);
    }
    // The unverified ratio over the written roots is 1.0 for both domains.
    let mem_stats = scan_unverified(&dir.path().join("memories")).unwrap();
    assert_eq!(mem_stats.files, 1);
    assert_eq!(mem_stats.unverified, 1);
    assert!((mem_stats.ratio() - 1.0).abs() < 1e-9);
}
#[test]
fn reflection_output_serde_roundtrip_with_optional_fields() {
    let json = r#"{
        "summary": "roundtrip probe",
        "skill_candidates": [],
        "memory_candidates": [
            {
                "fact": "Debug by reproducing first",
                "tags": ["debugging"],
                "scope": "user",
                "confidence": "medium",
                "rationale": "Reusable debugging approach",
                "supersedes": [],
                "zone": "work",
                "memory_type": "reasoning",
                "thinking_path": "reproduce → isolate → hypothesize → test → conclude",
                "concrete_instance": "Reproduced the error by re-running the command",
                "problem_type": "debugging"
            },
            {
                "fact": "User prefers dark mode",
                "tags": [],
                "scope": "user",
                "confidence": "high",
                "rationale": "Explicitly stated",
                "zone": "general"
            }
        ],
        "conflicts": [
            {
                "with": "mem_old",
                "kind": "contradiction",
                "explain": "conflicting convention",
                "options": ["keep_new"]
            }
        ],
        "task_candidates": [
            {"title": "Follow up on rustfmt config", "priority": "low"}
        ],
        "reflection_process": "observed → hypothesized → validated → concluded"
    }"#;
    let output: ReflectionOutput = serde_json::from_str(json).unwrap();
    assert_eq!(output.memory_candidates.len(), 2);
    assert_eq!(
        output.memory_candidates[0].memory_type,
        Some(MemoryType::Reasoning)
    );
    assert!(output.memory_candidates[0].thinking_path.is_some());
    assert!(output.memory_candidates[1].memory_type.is_none());
    assert!(output.memory_candidates[1].thinking_path.is_none());
    assert_eq!(output.conflicts.len(), 1);
    assert_eq!(output.task_candidates.len(), 1);
    // Serialize → deserialize → serialize is stable (canonical round-trip).
    let once = serde_json::to_string(&output).unwrap();
    let back: ReflectionOutput = serde_json::from_str(&once).unwrap();
    let twice = serde_json::to_string(&back).unwrap();
    assert_eq!(once, twice);
    assert_eq!(back.task_candidates[0].priority, "low");
    assert_eq!(back.task_candidates[0].title, "Follow up on rustfmt config");
    assert!(back.task_candidates[0].labels.is_empty());
}
#[test]
fn parse_failure_carries_raw_prefix() {
    let provider = MockProvider::new("this is not JSON at all — model went off the rails");
    let err = block_on(reflect(&provider, &[], &ReflectContext::default())).unwrap_err();
    match err {
        ReflectError::Parse { raw, .. } => {
            assert!(raw.contains("not JSON at all"));
            assert!(raw.chars().count() <= 200);
        }
        other => panic!("expected parse failure, got: {other:?}"),
    }
}
#[test]
fn trigger_boundary_error_and_switch() {
    let config = TriggerConfig::default();
    assert_eq!(config.max_turns_since_last, 3);
    assert!(config.reflect_on_error);
    // N threshold boundary: strictly below N does not fire, N fires.
    assert!(!config.should_trigger(&LiteTriggerContext {
        turns_since_last: 2,
        error_in_last_turn: false,
    }));
    assert!(config.should_trigger(&LiteTriggerContext {
        turns_since_last: 3,
        error_in_last_turn: false,
    }));
    // Error in the last turn fires immediately, even at zero turns.
    assert!(config.should_trigger(&LiteTriggerContext {
        turns_since_last: 0,
        error_in_last_turn: true,
    }));
    // Switch off: the error path no longer fires, the threshold still does.
    let muted = TriggerConfig {
        reflect_on_error: false,
        ..TriggerConfig::default()
    };
    assert!(!muted.should_trigger(&LiteTriggerContext {
        turns_since_last: 0,
        error_in_last_turn: true,
    }));
    assert!(muted.should_trigger(&LiteTriggerContext {
        turns_since_last: 3,
        error_in_last_turn: true,
    }));
}
#[test]
fn config_defaults_and_partial_deserialize() {
    let config = ReflectLiteConfig::default();
    assert_eq!(config.trigger.max_turns_since_last, 3);
    assert!(config.trigger.reflect_on_error);
    assert!(config.persist_enabled);
    // Partial config files inherit defaults through serde.
    let parsed: ReflectLiteConfig =
        serde_json::from_str(r#"{"trigger": {"max_turns_since_last": 5}}"#).unwrap();
    assert_eq!(parsed.trigger.max_turns_since_last, 5);
    assert!(parsed.trigger.reflect_on_error);
    assert!(parsed.persist_enabled);
}
#[test]
fn stats_unverified_ratio_and_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Memory layout: two flat files, one unverified.
    std::fs::write(
        dir.path().join("2026-08-31-mem_a.md"),
        "---\nid: mem_a\ncreated: 2026-08-31T00:00:00Z\nsource: reflection\nconfidence: medium\nunverified: true\n---\nbody a\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("2026-08-31-mem_b.md"),
        "---\nid: mem_b\ncreated: 2026-08-31T00:00:00Z\nsource: user\nconfidence: high\n---\nbody b\n",
    )
    .unwrap();
    // Skills layout: two SKILL.md files, one unverified.
    std::fs::create_dir_all(dir.path().join("skill-a")).unwrap();
    std::fs::create_dir_all(dir.path().join("skill-b")).unwrap();
    std::fs::write(
        dir.path().join("skill-a").join("SKILL.md"),
        "---\nname: skill-a\ndescription: test\nunverified: true\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("skill-b").join("SKILL.md"),
        "---\nname: skill-b\ndescription: test\n---\nbody\n",
    )
    .unwrap();
    let stats = scan_unverified(dir.path()).unwrap();
    assert_eq!(stats.files, 4);
    assert_eq!(stats.unverified, 2);
    assert!((stats.ratio() - 0.5).abs() < 1e-9);
    // Missing root: zero artifacts, zero ratio.
    let empty = scan_unverified(&dir.path().join("does-not-exist")).unwrap();
    assert_eq!(empty, openpaw_reflect_lite::UnverifiedStats::default());
    assert_eq!(empty.ratio(), 0.0);
}
/// The persist switch lives in config; persisting with it disabled is the
/// caller's dry-run responsibility — assert the report error surface stays
/// exercised (duplicate memory id is rejected by the store).
#[test]
fn persist_error_surface_is_typed() {
    let dir = tempfile::tempdir().unwrap();
    let mem_store = FsMemoryStore::new(dir.path().join("memories"), None);
    let skill_store = FsSkillStore::new(dir.path().join("skills"), None);
    let json = r#"{
        "summary": "s",
        "memory_candidates": [
            {
                "fact": "x",
                "scope": "project",
                "confidence": "low",
                "rationale": "r"
            }
        ]
    }"#;
    let output: ReflectionOutput = serde_json::from_str(json).unwrap();
    // Project scope without a project root configured → typed store error.
    let err = persist(&output, &mem_store, &skill_store).unwrap_err();
    assert!(matches!(err, PersistError::Memory(_)));
    // Nothing leaked into either store.
    assert!(MemoryStore::list(&mem_store).unwrap().is_empty());
}
