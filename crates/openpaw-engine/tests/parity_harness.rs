//! Parity harness minimal set — the E1 exit line (plan §6.2).
//!
//! Three scenarios: pure chat / single tool serial / two tools parallel.
//! Each run exports a JSON transcript fixture under `target/parity/`; the
//! upstream side replays the identical script and the two fixture sets are
//! compared on structure keys only.
mod common;
use common::{
    assert_expectations, export_fixture, fixture_json, parse_scenario, run_scenario, scenario_dir,
};
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
    let t1 = run_scenario(&sc).await;
    let bytes1 = fixture_json(&t1);
    let t2 = run_scenario(&sc).await;
    let bytes2 = fixture_json(&t2);
    assert_eq!(bytes1, bytes2, "parity transcripts must be deterministic");
}
async fn run_file(name: &str) {
    let path = scenario_dir().join(format!("{name}.jsonl"));
    let sc = parse_scenario(&path);
    let transcript = run_scenario(&sc).await;
    assert_expectations(&sc, &transcript);
    let fixture = export_fixture(&transcript);
    println!("parity fixture: {}", fixture.display());
}
