//! Golden bidirectional read/write tests (quality gate Q6, same-source S1).
//!
//! `tests/golden/` holds sample documents in the shared format. Every golden
//! frontmatter document must: parse → rewrite → reparse with identical
//! frontmatter mapping and body, and the rewrite must be byte-stable. The
//! golden session transcript must parse with all events preserved.
use openpaw_core::SessionEvent;
use openpaw_store::frontmatter::{read_doc, write_doc_atomic};
use openpaw_store::session::SessionReader;
use std::path::PathBuf;
fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}
fn list_md_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("golden frontmatter dir must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    out.sort();
    assert!(
        out.len() >= 3,
        "expected at least 3 golden frontmatter samples, found {}",
        out.len()
    );
    out
}
#[test]
fn golden_frontmatter_bidirectional_roundtrip() {
    for path in list_md_files(&golden_dir().join("frontmatter")) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // 1. parse the golden sample
        let doc: openpaw_store::FrontmatterDoc<serde_yml::Mapping> =
            read_doc(&path).unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        assert!(
            !doc.frontmatter.is_empty(),
            "{name}: frontmatter must not be empty"
        );
        assert!(
            !doc.body.trim().is_empty(),
            "{name}: body must not be empty"
        );
        // 2. rewrite to a temp file
        let tmp = tempfile::tempdir().unwrap();
        let out_path = tmp.path().join(&name);
        write_doc_atomic(&out_path, &doc).unwrap();
        // 3. reparse — mapping and body must match
        let reparsed: openpaw_store::FrontmatterDoc<serde_yml::Mapping> =
            read_doc(&out_path).unwrap();
        assert_eq!(
            doc.frontmatter, reparsed.frontmatter,
            "{name}: frontmatter mismatch after roundtrip"
        );
        assert_eq!(
            doc.body, reparsed.body,
            "{name}: body mismatch after roundtrip"
        );
        // 4. byte stability: rewrite of the reparse is byte-identical
        let first = std::fs::read_to_string(&out_path).unwrap();
        write_doc_atomic(&out_path, &reparsed).unwrap();
        let second = std::fs::read_to_string(&out_path).unwrap();
        assert_eq!(first, second, "{name}: writer output not byte-stable");
    }
}
#[test]
fn golden_session_transcript_is_readable() {
    let path = golden_dir().join("sessions").join("session-001.jsonl");
    let out =
        SessionReader::read(&path).unwrap_or_else(|e| panic!("golden session parse failed: {e}"));
    assert_eq!(out.meta.provider, "anthropic");
    assert_eq!(out.meta.model, "claude-sonnet-4");
    let kinds: Vec<&str> = out
        .events
        .iter()
        .map(|e| match e {
            SessionEvent::Meta(_) => "meta",
            SessionEvent::Message(_) => "message",
            SessionEvent::Usage(_) => "usage",
            SessionEvent::ToolCallStart { .. } => "tool_call_start",
            SessionEvent::ToolCallComplete { .. } => "tool_call_complete",
            SessionEvent::Unknown(_) => "unknown",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds[0], "meta");
    assert!(kinds.contains(&"tool_call_start"));
    assert!(kinds.contains(&"tool_call_complete"));
    // Forward compatibility: an unknown event type must survive the read.
    assert!(
        kinds.contains(&"unknown"),
        "unknown event kinds must be preserved, got: {kinds:?}"
    );
}
#[test]
fn golden_session_writer_output_reparses_identically() {
    // Writer → reader roundtrip on our own output must be lossless for the
    // event set we model.
    let path = golden_dir().join("sessions").join("session-001.jsonl");
    let original = SessionReader::read(&path).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("copy.jsonl");
    let mut w = openpaw_store::SessionWriter::create(&out_path).unwrap();
    for ev in &original.events {
        w.append(ev).unwrap();
    }
    let copy = SessionReader::read(&out_path).unwrap();
    assert_eq!(original.meta.id, copy.meta.id);
    assert_eq!(original.events.len(), copy.events.len());
    for (a, b) in original.events.iter().zip(copy.events.iter()) {
        assert_eq!(
            serde_json::to_string(a).unwrap(),
            serde_json::to_string(b).unwrap(),
            "event mismatch in rewritten transcript"
        );
    }
}
