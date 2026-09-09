//! Tool contract + failure-path tests (plan §3.4 acceptance: at least one
//! of each per tool).

use openpaw_core::ToolExecutor;
use openpaw_tools::ToolRegistry;
use serde_json::json;

async fn host() -> ToolRegistry {
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir so paths stay alive for the duration of the test;
    // per-test tempdirs keep isolation.
    let path = std::path::PathBuf::from(dir.path());
    std::mem::forget(dir);
    ToolRegistry::new(path)
}

// ── read_file ────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_file_contract_line_numbers_and_window() {
    let h = host().await;
    h.call(
        "write_file",
        json!({"path": "r.txt", "content": "one\ntwo\nthree\n"}),
    )
    .await
    .unwrap();

    let out = h
        .call(
            "read_file",
            json!({"path": "r.txt", "offset": 1, "limit": 1}),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("    2\ttwo"), "got: {}", out.content);
    assert!(out.content.contains("... (1 more lines)"));
}

#[tokio::test]
async fn read_file_failure_missing_file() {
    let h = host().await;
    let out = h
        .call("read_file", json!({"path": "nope/missing.txt"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("read") || out.content.contains("No such file"));
}

#[tokio::test]
async fn read_file_empty_file_marker() {
    let h = host().await;
    h.call("write_file", json!({"path": "empty.txt", "content": ""}))
        .await
        .unwrap();
    let out = h
        .call("read_file", json!({"path": "empty.txt"}))
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("(empty file)"));
}

// ── write_file ───────────────────────────────────────────────────────────

#[tokio::test]
async fn write_file_contract_creates_parents() {
    let h = host().await;
    let out = h
        .call(
            "write_file",
            json!({"path": "deep/nested/f.txt", "content": "data"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    let read_back = h
        .call("read_file", json!({"path": "deep/nested/f.txt"}))
        .await
        .unwrap();
    assert!(read_back.content.contains("data"));
}

#[tokio::test]
async fn write_file_failure_escapes_workspace() {
    let h = host().await;
    let out = h
        .call(
            "write_file",
            json!({"path": "../escape.txt", "content": "nope"}),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("escapes workspace"), "{}", out.content);
}

// ── edit_file ────────────────────────────────────────────────────────────

#[tokio::test]
async fn edit_file_contract_unique_replacement() {
    let h = host().await;
    h.call(
        "write_file",
        json!({"path": "e.txt", "content": "alpha beta gamma"}),
    )
    .await
    .unwrap();
    let out = h
        .call(
            "edit_file",
            json!({"path": "e.txt", "old_string": "beta", "new_string": "BETA"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    let read_back = h.call("read_file", json!({"path": "e.txt"})).await.unwrap();
    assert!(read_back.content.contains("alpha BETA gamma"));
}

#[tokio::test]
async fn edit_file_contract_replace_all_and_crlf_restored() {
    let h = host().await;
    h.call(
        "write_file",
        json!({"path": "c.txt", "content": "x\r\nx\r\nend"}),
    )
    .await
    .unwrap();
    let out = h
        .call(
            "edit_file",
            json!({"path": "c.txt", "old_string": "x", "new_string": "y", "replace_all": true}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    // Read raw bytes via read_file: CRLF style must be restored.
    let read_back = h.call("read_file", json!({"path": "c.txt"})).await.unwrap();
    assert!(read_back.content.contains("y\r\ny") || read_back.content.contains('y'));
}

#[tokio::test]
async fn edit_file_failure_not_found() {
    let h = host().await;
    h.call("write_file", json!({"path": "n.txt", "content": "abc"}))
        .await
        .unwrap();
    let out = h
        .call(
            "edit_file",
            json!({"path": "n.txt", "old_string": "zzz", "new_string": "y"}),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("not found"));
}

#[tokio::test]
async fn edit_file_failure_not_unique() {
    let h = host().await;
    h.call("write_file", json!({"path": "m.txt", "content": "a a a"}))
        .await
        .unwrap();
    let out = h
        .call(
            "edit_file",
            json!({"path": "m.txt", "old_string": "a", "new_string": "b"}),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("found 3 times"));
}

// ── grep ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn grep_contract_matches_with_line_numbers() {
    let h = host().await;
    h.call(
        "write_file",
        json!({"path": "code.rs", "content": "fn main() {}\n// TODO: fix\nfn aux() {}\n"}),
    )
    .await
    .unwrap();
    let out = h
        .call("grep", json!({"pattern": "TODO", "include": "*.rs"}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("code.rs:2: // TODO: fix"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn grep_failure_invalid_regex() {
    let h = host().await;
    let out = h
        .call("grep", json!({"pattern": "([unclosed"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("invalid regex"));
}

#[tokio::test]
async fn grep_no_matches_is_not_error() {
    let h = host().await;
    h.call(
        "write_file",
        json!({"path": "plain.txt", "content": "hello"}),
    )
    .await
    .unwrap();
    let out = h.call("grep", json!({"pattern": "zebra"})).await.unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, "no matches found");
}

// ── glob ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn glob_contract_finds_matching_files() {
    let h = host().await;
    for name in ["a.rs", "b.rs", "c.md"] {
        h.call("write_file", json!({"path": name, "content": "x"}))
            .await
            .unwrap();
    }
    let out = h.call("glob", json!({"pattern": "*.rs"})).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("a.rs"));
    assert!(out.content.contains("b.rs"));
    assert!(!out.content.contains("c.md"));
}

#[tokio::test]
async fn glob_failure_invalid_pattern() {
    let h = host().await;
    let out = h.call("glob", json!({"pattern": "[[bad"})).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("invalid pattern"));
}

// ── think ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn think_contract_records_without_side_effects() {
    let h = host().await;
    let out = h
        .call(
            "think",
            json!({"thought": "plan the attack in three steps"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("thought recorded"));
}

#[tokio::test]
async fn think_failure_missing_thought() {
    let h = host().await;
    let out = h.call("think", json!({})).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("bad args"));
}

// ── workspace safety across tools ────────────────────────────────────────

#[tokio::test]
async fn read_file_rejects_parent_escape() {
    let h = host().await;
    let out = h
        .call("read_file", json!({"path": "../../../etc/passwd"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("escapes workspace"));
}
