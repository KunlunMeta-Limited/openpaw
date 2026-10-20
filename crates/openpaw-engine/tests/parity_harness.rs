//! 职责：验证引擎跨场景确定性及并行回放隔离。
//! 输入输出：读取脚本场景，导出隔离临时目录的转录并断言。
//! 依赖：tests/common的Provider和工具夹具。
//! 禁止职责：不改变生产推理或共享回放输出目录。
//! Parity harness minimal set — the E1 exit line (plan §6.2).
//!
//! Three scenarios: pure chat / single tool serial / two tools parallel.
//! Scenario runs export JSON fixtures under `tests/parity/fixtures/`; the
//! upstream side replays the identical script and the two fixture sets are
//! compared on structure keys only.

mod common;

use common::{assert_expectations, export_fixture, parse_scenario, run_scenario, scenario_dir};

#[tokio::test]
async fn scenario_01_pure_chat() {
    run_file("01-pure-chat").await;
}

#[tokio::test]
async fn scenario_02_single_tool_serial() {
    run_file("02-single-tool-serial").await;
}

#[tokio::test]
async fn scenario_03_two_tools_parallel() {
    run_file("03-two-tools-parallel").await;
}

#[tokio::test]
async fn scenario_04_guardrail_deny() {
    run_file("04-guardrail-deny").await;
}

#[tokio::test]
async fn scenario_05_compaction_trigger() {
    run_file("05-compaction-trigger").await;
}

#[tokio::test]
async fn fixtures_are_deterministic_across_replays() {
    // Same scenario, two runs → identical fixture bytes (structure keys
    // only; no timing or text beyond scripted replies).
    let path = scenario_dir().join("01-pure-chat.jsonl");
    let sc = parse_scenario(&path);

    // Other tests export this same scenario concurrently. Give this replay
    // its own directory so a reader cannot observe another writer truncating it.
    let output = tempfile::tempdir().unwrap();
    let t1 = run_scenario(&sc).await;
    let f1 = export_fixture(&t1, output.path());
    let bytes1 = std::fs::read_to_string(&f1).unwrap();

    let t2 = run_scenario(&sc).await;
    let f2 = export_fixture(&t2, output.path());
    let bytes2 = std::fs::read_to_string(&f2).unwrap();

    assert_eq!(bytes1, bytes2, "parity transcripts must be deterministic");
}

async fn run_file(name: &str) {
    let path = scenario_dir().join(format!("{name}.jsonl"));
    let sc = parse_scenario(&path);
    let transcript = run_scenario(&sc).await;
    assert_expectations(&sc, &transcript);
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity/fixtures");
    let fixture = export_fixture(&transcript, &output);
    println!("parity fixture: {}", fixture.display());
}
