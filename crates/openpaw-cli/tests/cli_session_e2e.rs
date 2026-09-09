//! E2E: `openpaw session` / `openpaw memory` subcommands.
//!
//! Drives one real `openpaw chat` round (mock provider, temp home) so the
//! JSONL transcript and the SQLite index both exist, then calls the
//! subcommand run functions directly and asserts exit codes plus the
//! persisted index/store state.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use openpaw_cli::{memory_cmd, session_cmd};
use openpaw_memory::{Confidence, FsMemoryStore, MemoryFrontmatter, MemoryStore, Scope, Source};

/// Run one chat turn against the mock provider with `--home`.
fn chat_round(home: &Path, message: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("chat")
        .arg("--provider")
        .arg("mock")
        .arg("--home")
        .arg(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openpaw chat");
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        writeln!(stdin, "{message}").expect("write message");
        stdin.write_all(b"exit\n").expect("write exit");
    }
    let output = child.wait_with_output().expect("wait for chat");
    assert!(
        output.status.success(),
        "chat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn session_and_memory_subcommands_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path();

    // One chat round indexes the session (meta + messages) into
    // <home>/index.db next to the JSONL transcript.
    chat_round(home_path, "e2e probe zebra");

    // `session list` sees the session and exits cleanly.
    assert_eq!(session_cmd::run_list(home_path, 20), 0);

    let db = openpaw_store::sqlite::SessionDb::open(home_path.join("index.db"))
        .expect("open session index");
    let sessions = db.list_sessions().expect("list sessions");
    assert!(!sessions.is_empty(), "chat must index at least one session");
    let sid = sessions[0].id.clone();
    assert_eq!(sessions[0].message_count, 1, "one user message indexed");

    // `session show` resolves the id through the index and re-reads the
    // JSONL transcript.
    assert_eq!(session_cmd::run_show(home_path, &sid), 0);
    assert_eq!(
        session_cmd::run_show(home_path, "no-such-session-id"),
        1,
        "unknown id must exit non-zero"
    );

    // `session search` FTS5-hits both the user probe message and the
    // mock reply keyword.
    assert_eq!(session_cmd::run_search(home_path, "zebra", 20), 0);
    let hits = db.search("zebra", 20).expect("search index");
    assert!(
        hits.iter().any(|r| r.session_id == sid),
        "FTS5 index must contain the user probe message, got: {hits:?}"
    );
    let mock_hits = db.search("mock-reply", 20).expect("search mock reply");
    assert!(
        mock_hits.iter().any(|r| r.session_id == sid),
        "FTS5 index must contain the mock assistant reply"
    );

    // Memory: seed one record through the store (there is no `memory add`
    // subcommand), then exercise the read commands and the delete path.
    let store = FsMemoryStore::new(home_path.join("memories"), None);
    let fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec!["e2e".into()]);
    store
        .put(Scope::User, fm, "E2E memory about cerulean ducks.")
        .expect("seed memory");

    assert_eq!(memory_cmd::run_list(home_path, None), 0);
    assert_eq!(memory_cmd::run_search(home_path, None, "cerulean", 20), 0);

    let active = MemoryStore::list_active(&store).expect("list memories");
    assert_eq!(active.len(), 1, "exactly one seeded memory");
    let mid = active[0].frontmatter.id.clone();
    assert_eq!(memory_cmd::run_show(home_path, None, &mid), 0);
    assert_eq!(
        memory_cmd::run_show(home_path, None, "mem_does_not_exist"),
        1,
        "unknown id must exit non-zero"
    );

    assert_eq!(memory_cmd::run_delete(home_path, None, &mid), 0);
    assert_eq!(
        memory_cmd::run_delete(home_path, None, &mid),
        1,
        "second delete of the same id must fail"
    );
    let remaining = MemoryStore::list_active(&store).expect("list after delete");
    assert!(remaining.is_empty(), "memory must be gone after delete");
}
