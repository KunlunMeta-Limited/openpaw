//! E2E: `openpaw chat` — two-turn conversation with session persistence.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn chat_two_turns_and_session_persisted() {
    let home = tempfile::tempdir().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("chat")
        .arg("--provider")
        .arg("mock")
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openpaw chat");
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(b"first question\nsecond question\nexit\n")
            .expect("write stdin");
    }
    let output = child.wait_with_output().expect("wait");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mock-reply-0") && stdout.contains("mock-reply-1"),
        "two counter replies expected, got: {stdout}"
    );

    // Session transcript exists under the given home and reparses.
    let sessions = PathBuf::from(home.path()).join("sessions");
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&sessions).expect("sessions dir") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "jsonl") {
            found = Some(p);
        }
    }
    let transcript = found.expect("session transcript file");
    let outcome = openpaw_store::SessionReader::read(&transcript).expect("parse transcript");
    assert_eq!(outcome.meta.provider, "mock");
    let message_events = outcome
        .events
        .iter()
        .filter(|e| matches!(e, openpaw_core::SessionEvent::Message(_)))
        .count();
    // 2 user + 2 assistant messages persisted.
    assert!(
        message_events >= 4,
        "expected ≥4 message events, got {message_events}"
    );
}

#[test]
fn chat_unknown_provider_fails_with_three_segment_error() {
    let home = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("chat")
        .arg("--provider")
        .arg("hologram")
        .arg("--home")
        .arg(home.path())
        .output()
        .expect("run chat");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown provider `hologram`"));
    assert!(stderr.contains("why:"));
    assert!(stderr.contains("next:"));
}
