//! Demo: conversation → reflection → knowledge-file change, all artifacts
//! landing with `unverified: true` (the M4 reflect-lite acceptance piece).
//!
//! The core path is [`run_demo`]: it builds three fake conversation rounds
//! (the middle one ends in a tool error), evaluates the rule trigger per
//! round, runs one single-shot reflection through a hand-written
//! [`LlmProvider`] mock returning a fixed `ReflectionOutput` (exactly one
//! memory candidate + one skill candidate), persists the candidates into a
//! tempdir-backed store, prints the artifact paths plus their frontmatter,
//! and closes with the unverified-ratio stats. `main` wires `run_demo` to a
//! throwaway home directory; tests/demo_assertions.rs replays `run_demo`
//! and asserts a 100% unverified ratio in both domains.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message, Role,
    StopReason, Usage,
};
use openpaw_memory::skills::FsSkillStore;
use openpaw_memory::FsMemoryStore;
use openpaw_reflect_lite::{
    persist, reflect, scan_unverified, PersistReport, ReflectContext, ReflectLiteConfig,
    TriggerContext, UnverifiedStats,
};

/// Fixed reflection output the mock provider returns: one memory candidate
/// plus one skill candidate, matching the demo transcript below.
const REFLECTION_JSON: &str = r##"{
  "summary": "Distilled the rustfmt-before-commit convention after a failed tool round",
  "skill_candidates": [
    {
      "name": "rustfmt-before-commit",
      "description": "Format all touched Rust files with rustfmt before committing",
      "triggers": ["rust", "commit", "format"],
      "body": "# rustfmt-before-commit\n\n1. Run `cargo fmt --all`.\n2. Confirm a clean tree with `cargo fmt --all -- --check`.\n3. Only then stage and commit.",
      "rationale": "Repeated multi-step procedure the user stated as a durable convention",
      "confidence": "medium"
    }
  ],
  "memory_candidates": [
    {
      "fact": "User formats all Rust code with rustfmt before every commit",
      "tags": ["rust", "convention"],
      "scope": "user",
      "confidence": "high",
      "rationale": "Stated explicitly as a durable convention",
      "supersedes": [],
      "zone": "work",
      "memory_type": "preference"
    }
  ],
  "conflicts": [],
  "task_candidates": []
}"##;

/// Minimal mock over the core [`LlmProvider`] trait: returns the fixed
/// reflection JSON for every completion request.
struct ReflectionMock;

#[async_trait]
impl LlmProvider for ReflectionMock {
    async fn complete(&self, _req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        Ok(CompletionResponse {
            content: vec![ContentBlock::Text {
                text: REFLECTION_JSON.to_string(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn name(&self) -> &str {
        "demo-mock"
    }

    fn default_model(&self) -> &str {
        "demo-model"
    }
}

/// Std-only executor: the mock completes on its first poll, so a spinning
/// noop-waker loop suffices (this crate has no async runtime dependency).
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

/// What one demo run produced — the assertion surface for
/// tests/demo_assertions.rs.
pub struct DemoReport {
    /// The persisted memory artifact.
    pub memory_path: PathBuf,
    /// The persisted skill artifact (SKILL.md).
    pub skill_path: PathBuf,
    /// Unverified stats over the memories root.
    pub memory_stats: UnverifiedStats,
    /// Unverified stats over the skills root.
    pub skill_stats: UnverifiedStats,
}

/// Three fake conversation rounds; round 2 ends in a tool error so the
/// trigger's error switch has something to react to.
fn demo_transcript() -> Vec<Message> {
    vec![
        // Round 1: a setup question.
        Message::user_text("How do we format Rust code in this repo before committing?"),
        Message::assistant_text("Run `cargo fmt --all`; the repo-root rustfmt.toml applies."),
        // Round 2: a tool round that fails — the error turn.
        Message::user_text("Now run the format check across the workspace."),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "tool_1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "cargo fmt --all -- --check"}),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "tool_1".into(),
                    content: "bash: cargo: command not found".into(),
                    is_error: true,
                    name: Some("bash".into()),
                    protected: false,
                },
            ],
            ..Default::default()
        },
        // Round 3: the user states the durable convention.
        Message::user_text("Remember: we always run rustfmt on all Rust code before every commit."),
        Message::assistant_text("Noted — that is now a convention I will follow."),
    ]
}

/// The demo core path over `home` (a scratch directory). Prints one
/// `[n/4]` banner per stage; returns the artifacts and final stats.
pub fn run_demo(home: &Path) -> DemoReport {
    println!("demo: conversation -> reflection -> knowledge-file change (unverified)");
    println!("home: {}", home.display());
    let transcript = demo_transcript();

    // [1/4] Trigger decision per round (N threshold + error switch).
    println!();
    println!("[1/4] trigger - rule: turns_since_last >= N or error in last turn");
    let config = ReflectLiteConfig::default();
    let rounds = [
        (
            "setup question",
            TriggerContext {
                turns_since_last: 1,
                error_in_last_turn: false,
            },
        ),
        (
            "failed tool round",
            TriggerContext {
                turns_since_last: 2,
                error_in_last_turn: true,
            },
        ),
        (
            "durable convention",
            TriggerContext {
                turns_since_last: 3,
                error_in_last_turn: false,
            },
        ),
    ];
    for (i, (label, ctx)) in rounds.iter().enumerate() {
        let fired = config.trigger.should_trigger(ctx);
        let reason = if ctx.turns_since_last >= config.trigger.max_turns_since_last {
            "turn threshold N"
        } else if ctx.error_in_last_turn {
            "error switch"
        } else {
            "below N"
        };
        println!(
            "  round {} {:<22} turns_since_last={} error_in_last_turn={} -> {} ({reason})",
            i + 1,
            format!("\"{label}\""),
            ctx.turns_since_last,
            ctx.error_in_last_turn,
            if fired { "TRIGGER" } else { "no trigger" }
        );
    }

    // [2/4] Single-shot reflection through the injected mock provider.
    println!();
    println!("[2/4] reflect - single-shot LLM reflection (mock provider, fixed schema output)");
    let output = block_on(reflect(
        &ReflectionMock,
        &transcript,
        &ReflectContext::default(),
    ))
    .expect("mock provider returns a parseable ReflectionOutput");
    println!("  summary: {}", output.summary);
    println!(
        "  candidates: {} memory, {} skill",
        output.memory_candidates.len(),
        output.skill_candidates.len()
    );

    // [3/4] Persist — every candidate lands unverified.
    println!();
    println!("[3/4] persist - direct store writes, `unverified: true` forced on every artifact");
    let mem_store = FsMemoryStore::new(home.join("memories"), None);
    let skill_store = FsSkillStore::new(home.join("skills"), None);
    let report: PersistReport =
        persist(&output, &mem_store, &skill_store).expect("persist writes both stores");
    assert_eq!(
        report.memories.len(),
        1,
        "demo mock yields exactly one memory candidate"
    );
    assert_eq!(
        report.skills.len(),
        1,
        "demo mock yields exactly one skill candidate"
    );
    for item in report.memories.iter().chain(report.skills.iter()) {
        println!("  artifact: {}", item.path.display());
        println!("  --- frontmatter ---");
        let raw = std::fs::read_to_string(&item.path).expect("read persisted artifact");
        println!("{}", frontmatter_block(&raw));
        println!("  ------------------");
    }

    // [4/4] Stats — unverified ratio over the fresh knowledge roots.
    println!();
    println!("[4/4] stats - unverified ratio per knowledge domain");
    let memory_stats = scan_unverified(&home.join("memories")).expect("scan memories root");
    let skill_stats = scan_unverified(&home.join("skills")).expect("scan skills root");
    for (label, stats) in [("memories", memory_stats), ("skills", skill_stats)] {
        println!(
            "  {:<10} {:>5} total  {:>5} unverified  {:>6.1}%",
            label,
            stats.files,
            stats.unverified,
            stats.ratio() * 100.0
        );
    }

    DemoReport {
        memory_path: report.memories[0].path.clone(),
        skill_path: report.skills[0].path.clone(),
        memory_stats,
        skill_stats,
    }
}

/// The `--- yaml ---` block at the top of a persisted artifact (the store
/// writer always opens with the `---` delimiter line).
fn frontmatter_block(raw: &str) -> &str {
    match raw[3..].find("\n---") {
        Some(i) => &raw[..i + 3 + 4],
        None => raw,
    }
}

#[cfg(not(test))]
fn main() {
    let home = tempfile::tempdir().expect("create scratch home");
    let _report = run_demo(home.path());
    // Exit code 0: every artifact above landed unverified, awaiting human
    // review — exactly what `openpaw evolve status` / `evolve reset` manage.
}
