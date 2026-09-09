//! Contract + failure-path tests for the exec tools (bash / clipboard / sleep).
//!
//! Each test uses its own leaked tempdir workspace; no global state is
//! touched, so tests are safe to run in parallel.

use std::path::PathBuf;

use openpaw_tools::exec::environment::{
    Environment, LocalEnvironment, SandboxPolicy, SandboxedEnvironment,
};
use openpaw_tools::exec::{bash, clipboard, sleep};

fn workspace() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir so the workspace stays alive for the async test body.
    let path = PathBuf::from(dir.path());
    std::mem::forget(dir);
    path
}

// ── bash ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn bash_contract_echo_output() {
    let ws = workspace();
    let out = bash::run(&ws, serde_json::json!({"command": "echo exec-e2e-ok"}))
        .await
        .unwrap();
    assert!(!out.is_error, "got: {}", out.content);
    assert!(out.content.contains("exec-e2e-ok"), "got: {}", out.content);
    assert!(
        out.content.contains("[exit code: 0]"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn bash_nonzero_exit_code_is_error_outcome() {
    let ws = workspace();
    // `exit 3` works in both cmd /C and sh -c.
    let out = bash::run(&ws, serde_json::json!({"command": "exit 3"}))
        .await
        .unwrap();
    assert!(out.is_error, "got: {}", out.content);
    assert!(
        out.content.contains("[exit code: 3]"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn bash_dangerous_command_blocked() {
    let ws = workspace();
    let out = bash::run(&ws, serde_json::json!({"command": "rm -rf /"}))
        .await
        .unwrap();
    assert!(out.is_error, "got: {}", out.content);
    assert!(out.content.contains("blocked"), "got: {}", out.content);
}

#[tokio::test]
async fn bash_command_substitution_requires_approval() {
    let ws = workspace();
    // $(...) makes the AST chain refuse to judge statically → approval path.
    let out = bash::run(&ws, serde_json::json!({"command": "echo $(whoami)"}))
        .await
        .unwrap();
    assert!(out.is_error, "got: {}", out.content);
    assert!(out.content.contains("approval"), "got: {}", out.content);
}

#[tokio::test]
async fn bash_bad_args_error_outcome() {
    let ws = workspace();
    let out = bash::run(&ws, serde_json::json!({"timeout_ms": 100}))
        .await
        .unwrap();
    assert!(out.is_error, "got: {}", out.content);
    assert!(out.content.contains("bad args"), "got: {}", out.content);
}

// ── sleep ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sleep_contract_short_duration() {
    let ws = workspace();
    let start = std::time::Instant::now();
    let out = sleep::run(&ws, serde_json::json!({"duration_ms": 20}))
        .await
        .unwrap();
    assert!(!out.is_error, "got: {}", out.content);
    assert!(out.content.contains("slept 20ms"), "got: {}", out.content);
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(15),
        "expected a real wait, elapsed {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn sleep_bad_params_fall_back_to_zero() {
    let ws = workspace();
    // Missing duration_ms defaults to 0.
    let out = sleep::run(&ws, serde_json::json!({})).await.unwrap();
    assert!(!out.is_error, "got: {}", out.content);
    assert!(out.content.contains("slept 0ms"), "got: {}", out.content);
    // A non-numeric duration_ms is treated as 0 (no panic, no Err).
    let out = sleep::run(&ws, serde_json::json!({"duration_ms": "not-a-number"}))
        .await
        .unwrap();
    assert!(!out.is_error, "got: {}", out.content);
    assert!(out.content.contains("slept 0ms"), "got: {}", out.content);
}

// ── clipboard ────────────────────────────────────────────────────────────

#[tokio::test]
async fn clipboard_unknown_action_is_error_outcome() {
    let ws = workspace();
    let out = clipboard::run(&ws, serde_json::json!({"action": "invalid"}))
        .await
        .unwrap();
    assert!(out.is_error, "got: {}", out.content);
    assert!(
        out.content.contains("unknown clipboard action"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn clipboard_missing_action_is_error_outcome() {
    let ws = workspace();
    let out = clipboard::run(&ws, serde_json::json!({})).await.unwrap();
    assert!(out.is_error, "got: {}", out.content);
}

// ── environment ──────────────────────────────────────────────────────────

#[tokio::test]
async fn local_environment_exec_echo_contract() {
    let ws = workspace();
    let env = LocalEnvironment;
    let result = env.exec("echo env-e2e-ok", Some(&ws)).await;
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("env-e2e-ok"),
        "got: {}",
        result.stdout
    );
}

#[test]
fn sandboxed_environment_construction_and_policy() {
    // Construction surface only: restricted/isolated execution paths are
    // covered by the sandbox crate's own (ignored) integration tests.
    let ws = workspace();
    let restricted = SandboxedEnvironment::new(SandboxPolicy::Restricted, ws);
    assert_eq!(restricted.name(), "sandboxed-restricted");
    assert_eq!(restricted.policy(), &SandboxPolicy::Restricted);

    let none = SandboxedEnvironment::new(SandboxPolicy::None, PathBuf::from("nowhere"));
    assert_eq!(none.name(), "local");
    assert_eq!(none.policy(), &SandboxPolicy::None);
}
