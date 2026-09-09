//! End-to-end tests for the SQLite index subset: schema creation, the write
//! path, FTS5 search, deletion, the audit trail, and file persistence across
//! reopen.

use std::path::Path;

use openpaw_core::{Message, SessionEvent, SessionMeta, Usage};
use openpaw_store::sqlite::SessionDb;

#[test]
fn open_in_memory_upsert_index_and_list() {
    let db = SessionDb::open_in_memory().unwrap();
    let meta = SessionMeta::new("model-a", "provider-x");
    db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();

    for i in 0..5 {
        db.index_message(
            &meta.id,
            "user",
            i,
            &format!("message number {i} about rust"),
        )
        .unwrap();
    }

    let sessions = db.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1, "exactly one session should be listed");
    assert_eq!(sessions[0].id, meta.id);
    assert_eq!(sessions[0].model, "model-a");
    assert_eq!(sessions[0].provider, "provider-x");
    assert_eq!(sessions[0].workspace_id, "default");
    assert_eq!(db.get_message_count(&meta.id).unwrap(), 5);
    assert!(db.session_exists(&meta.id).unwrap());
    assert_eq!(
        db.get_jsonl_path(&meta.id).unwrap(),
        Some(Path::new("s.jsonl").to_path_buf())
    );
}

#[test]
fn filter_sessions_by_model_and_provider() {
    let db = SessionDb::open_in_memory().unwrap();
    let m1 = SessionMeta::new("model-a", "provider-x");
    let m2 = SessionMeta::new("model-b", "provider-x");
    let m3 = SessionMeta::new("model-b", "provider-y");
    db.upsert_session(&m1, Path::new("a.jsonl")).unwrap();
    db.upsert_session(&m2, Path::new("b.jsonl")).unwrap();
    db.upsert_session(&m3, Path::new("c.jsonl")).unwrap();

    assert_eq!(db.filter_sessions(Some("model-a"), None).unwrap().len(), 1);
    assert_eq!(
        db.filter_sessions(Some("model-b"), Some("provider-x"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(db.filter_sessions(Some("model-b"), None).unwrap().len(), 2);
    assert_eq!(db.filter_sessions(None, None).unwrap().len(), 3);
    assert_eq!(db.get_session_count().unwrap(), 3);
}

#[test]
fn fts_search_hits_with_snippet() {
    let db = SessionDb::open_in_memory().unwrap();
    let meta = SessionMeta::new("m", "p");
    db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();

    db.index_message(&meta.id, "user", 0, "use rust-lang to chase the ferret")
        .unwrap();
    db.index_message(
        &meta.id,
        "assistant",
        1,
        "unrelated filler text about databases",
    )
    .unwrap();

    let hits = db.search("ferret", 10).unwrap();
    assert_eq!(hits.len(), 1, "exactly the matching message should be hit");
    assert_eq!(hits[0].session_id, meta.id);
    assert_eq!(hits[0].role, "user");
    assert_eq!(hits[0].seq, 0);
    assert!(hits[0].text_preview.contains("ferret"));
    assert!(
        hits[0].snippet.contains("[ferret]"),
        "snippet should wrap the keyword in highlight markers, got: {}",
        hits[0].snippet
    );

    // Miss returns empty rather than erroring.
    assert!(db.search("penguin", 10).unwrap().is_empty());

    // Hyphenated queries are escaped into quoted phrases and still match.
    let hyphen = db.search("rust-lang", 10).unwrap();
    assert!(
        !hyphen.is_empty(),
        "hyphenated query should match hyphenated text"
    );

    // Pure syntax-token queries neither error nor match.
    let syntax = db.search("AND OR NOT - :", 10).unwrap();
    assert!(syntax.is_empty());

    // Quoted user input neither errors nor corrupts the result set.
    let _quoted = db.search("\"quoted\" input", 10).unwrap();
}

#[test]
fn index_event_variants_are_counted() {
    let db = SessionDb::open_in_memory().unwrap();
    let meta = SessionMeta::new("m", "p");

    db.index_event(&meta.id, 0, &SessionEvent::Meta(meta.clone()))
        .unwrap();
    db.index_event(
        &meta.id,
        1,
        &SessionEvent::Message(Message::user_text("hello world")),
    )
    .unwrap();
    db.index_event(
        &meta.id,
        2,
        &SessionEvent::Message(Message::assistant_text("greetings")),
    )
    .unwrap();
    db.index_event(&meta.id, 3, &SessionEvent::Usage(Usage::default()))
        .unwrap();
    db.index_event(
        &meta.id,
        4,
        &SessionEvent::ToolCallStart {
            call_id: "c1".into(),
            tool_name: "bash".into(),
        },
    )
    .unwrap();
    db.index_event(
        &meta.id,
        5,
        &SessionEvent::Unknown(Box::new(serde_json::json!({"type": "mystery"}))),
    )
    .unwrap();

    assert!(
        db.session_exists(&meta.id).unwrap(),
        "the meta event should upsert the session"
    );
    assert_eq!(
        db.get_message_count(&meta.id).unwrap(),
        2,
        "only message events are indexed"
    );

    // The tool variant of index_message records the tool name.
    db.index_message_with_tool(&meta.id, "assistant", 6, "running the tool", Some("bash"))
        .unwrap();
    let messages = db.get_session_messages(&meta.id, 10).unwrap();
    let tool_row = messages
        .iter()
        .find(|m| m.seq == 6)
        .expect("tool message row");
    assert_eq!(tool_row.tool_name.as_deref(), Some("bash"));
    assert_eq!(db.get_message_count(&meta.id).unwrap(), 3);
}

#[test]
fn title_and_token_updates() {
    let db = SessionDb::open_in_memory().unwrap();
    let meta = SessionMeta::new("m", "p");
    db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();

    db.update_session_title(&meta.id, "my title").unwrap();
    db.update_session_tokens(&meta.id, 11, 22).unwrap();

    let session = db
        .get_session_by_id(&meta.id)
        .unwrap()
        .expect("session row");
    assert_eq!(session.title, "my title");
    assert_eq!(db.get_total_tokens().unwrap(), (11, 22));

    // Accumulate semantics: each call adds a delta, and a missing session row
    // is created on demand.
    db.add_session_tokens(&meta.id, 10, 5).unwrap();
    db.add_session_tokens(&meta.id, 10, 5).unwrap();
    db.add_session_tokens("gateway-only-session", 100, 50)
        .unwrap();
    assert_eq!(db.get_total_tokens().unwrap(), (131, 82));
}

#[test]
fn delete_session_clears_messages_and_fts() {
    let db = SessionDb::open_in_memory().unwrap();
    let meta = SessionMeta::new("m", "p");
    db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();
    db.index_message(&meta.id, "user", 0, "this row is doomed to be deleted")
        .unwrap();

    assert_eq!(db.get_message_count(&meta.id).unwrap(), 1);
    assert_eq!(db.search("doomed", 10).unwrap().len(), 1);

    db.delete_session(&meta.id).unwrap();

    assert_eq!(db.get_session_count().unwrap(), 0);
    assert_eq!(db.get_message_count(&meta.id).unwrap(), 0);
    assert!(db.get_session_by_id(&meta.id).unwrap().is_none());
    assert!(
        db.search("doomed", 10).unwrap().is_empty(),
        "deleted rows must leave the FTS index"
    );
}

#[test]
fn audit_log_insert_list_and_search() {
    let db = SessionDb::open_in_memory().unwrap();

    db.insert_audit_raw(
        "session",
        Some("actor-1"),
        "session.created",
        Some("target-1"),
        Some("created the session record"),
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    db.insert_audit_raw(
        "hook",
        None,
        "hook.fired",
        None,
        None,
        "2026-01-02T00:00:00Z",
    )
    .unwrap();

    let all = db.list_audit_raw(None, 10).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[0].2, "hook.fired",
        "entries are ordered by ts descending"
    );

    let by_source = db.list_audit_raw(Some("session"), 10).unwrap();
    assert_eq!(by_source.len(), 1);
    assert_eq!(by_source[0].2, "session.created");
    assert_eq!(by_source[0].3.as_deref(), Some("actor-1"));

    let hits = db.search_audit_raw("created", None, 10).unwrap();
    assert_eq!(hits.len(), 1, "FTS over event + detail should hit one row");
    assert_eq!(hits[0].1, "session");

    let scoped = db.search_audit_raw("created", Some("hook"), 10).unwrap();
    assert!(
        scoped.is_empty(),
        "source filter must exclude other sources"
    );
}

#[test]
fn schema_version_matches_expected() {
    let db = SessionDb::open_in_memory().unwrap();
    assert_eq!(
        db.schema_version().unwrap(),
        8,
        "base tables + audit trail = version 8"
    );
}

#[test]
fn file_db_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("nested").join("index.db");
    let meta = SessionMeta::new("persist-model", "persist-provider");

    {
        let db = SessionDb::open(&db_path).unwrap();
        db.upsert_session(&meta, Path::new("s.jsonl")).unwrap();
        db.index_message(
            &meta.id,
            "user",
            0,
            "persistent ferret facts survive reopen",
        )
        .unwrap();
        db.update_session_title(&meta.id, "kept title").unwrap();
        db.update_session_tokens(&meta.id, 11, 22).unwrap();
        db.insert_audit_raw(
            "session",
            None,
            "session.created",
            Some(&meta.id),
            None,
            "2026-02-03T00:00:00Z",
        )
        .unwrap();
    }

    {
        let db = SessionDb::open(&db_path).unwrap();
        assert!(
            db.session_exists(&meta.id).unwrap(),
            "session row must survive reopen"
        );
        assert_eq!(db.get_message_count(&meta.id).unwrap(), 1);

        let session = db
            .get_session_by_id(&meta.id)
            .unwrap()
            .expect("session row");
        assert_eq!(session.title, "kept title");
        assert_eq!(session.message_count, 0);

        assert_eq!(db.get_total_tokens().unwrap(), (11, 22));
        assert!(
            !db.search("ferret", 10).unwrap().is_empty(),
            "FTS index must survive reopen"
        );

        let audit = db.list_audit_raw(Some("session"), 10).unwrap();
        assert_eq!(audit.len(), 1, "audit trail must survive reopen");
        assert_eq!(
            db.schema_version().unwrap(),
            8,
            "reopened DB must not re-run migrations"
        );
    }
}
