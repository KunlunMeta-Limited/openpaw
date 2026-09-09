//! Meta tool contract + failure-path tests (plan §3.4 acceptance).

use openpaw_core::ToolExecutor;
use openpaw_tools::ToolRegistry;
use serde_json::json;

async fn host() -> ToolRegistry {
    let dir = tempfile::tempdir().unwrap();
    let path = std::path::PathBuf::from(dir.path());
    std::mem::forget(dir);
    ToolRegistry::new(path)
}

#[tokio::test]
async fn todo_lifecycle_add_update_list() {
    let h = host().await;
    let added = h
        .call("todo_add", json!({"title": "write docs"}))
        .await
        .unwrap();
    assert!(!added.is_error);
    assert!(added.content.contains("added todo #1: write docs"));

    let updated = h
        .call("todo_update", json!({"id": 1, "status": "in_progress"}))
        .await
        .unwrap();
    assert!(!updated.is_error);

    let listed = h.call("todo_list", json!({})).await.unwrap();
    assert!(listed.content.contains("#1 [in_progress] write docs"));

    // Persistence: a new registry over the same workspace sees the todo.
    let ws = std::path::PathBuf::from("target/meta-todo-persist");
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    let h2 = ToolRegistry::new(&ws);
    h2.call("todo_add", json!({"title": "persisted"}))
        .await
        .unwrap();
    let h3 = ToolRegistry::new(&ws);
    let listed = h3.call("todo_list", json!({})).await.unwrap();
    assert!(listed.content.contains("persisted"), "{}", listed.content);
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn todo_failures() {
    let h = host().await;
    // Invalid status.
    let out = h
        .call("todo_update", json!({"id": 1, "status": "archived"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("invalid status"));
    // Unknown id.
    let out = h
        .call("todo_update", json!({"id": 99, "status": "done"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("no todo with id 99"));
}

#[tokio::test]
async fn clarify_contract_with_options_and_context() {
    let h = host().await;
    let out = h
        .call(
            "clarify",
            json!({
                "question": "which database?",
                "options": ["sqlite", "postgres"],
                "context": "schema not decided"
            }),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out
        .content
        .contains("Clarification needed: which database?"));
    assert!(out.content.contains("1. sqlite"));
    assert!(out.content.contains("2. postgres"));
    assert!(out.content.contains("Context: schema not decided"));
    assert!(out.content.contains("[Waiting for user response...]"));
}

#[tokio::test]
async fn clarify_failure_missing_question() {
    let h = host().await;
    let out = h.call("clarify", json!({"options": ["a"]})).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("question parameter is required"));
}

#[tokio::test]
async fn plan_mode_transitions() {
    let h = host().await;
    let enter = h.call("enter_plan_mode", json!({})).await.unwrap();
    assert!(enter.content.contains("plan mode entered"));
    let exit = h.call("exit_plan_mode", json!({})).await.unwrap();
    assert!(exit.content.contains("plan mode exited"));
}

#[tokio::test]
async fn structured_output_contract_and_failures() {
    let h = host().await;
    let out = h
        .call(
            "structured_output",
            json!({"data": {"count": 3, "files": ["a.rs", "b.rs"]}}),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.structured.is_some());
    assert_eq!(out.structured.as_ref().unwrap()["count"], json!(3));
    assert!(out.content.contains("structured output recorded"));

    // Missing field.
    let out = h.call("structured_output", json!({})).await.unwrap();
    assert!(out.is_error);
    // Non-object data.
    let out = h
        .call("structured_output", json!({"data": [1, 2]}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("must be a JSON object"));
}

#[tokio::test]
async fn session_search_finds_indexed_message() {
    // Build a real index.db with one indexed message, then search it
    // through the tool's core path (no OPENPAW_HOME mutation — parallel
    // tests share the process environment).
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let db = openpaw_store::SessionDb::open(&db_path).unwrap();
    let session = openpaw_core::Session::new("mock-1", "mock");
    db.upsert_session(&session.meta, dir.path()).unwrap();
    let msg = openpaw_core::Message::user_text("the xylophone haystack note");
    db.index_event(
        &session.meta.id,
        0,
        &openpaw_core::SessionEvent::Message(msg),
    )
    .unwrap();

    let out =
        openpaw_tools::meta::session_search::search_index_for_tests(&db_path, "xylophone", 10)
            .await
            .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("xylophone"), "{}", out.content);
    assert!(out.content.contains(&session.meta.id));
    assert!(out.content.contains("(1 result(s))"));
}

#[tokio::test]
async fn session_search_contract_and_empty_states() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    // Missing index degrades to an informative ok outcome.
    let out = openpaw_tools::meta::session_search::search_index_for_tests(&db_path, "anything", 10)
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("no session index yet"));

    // Missing query is a tool error.
    let h = host().await;
    let out = h.call("session_search", json!({"limit": 5})).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("query parameter is required"));
}
