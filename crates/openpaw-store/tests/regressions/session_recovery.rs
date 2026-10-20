//! 职责：T1真实行为回归。
//! 输入输出：确定性输入、故障到断言及账本证据。
//! 依赖：原引擎、Provider或Store公共入口。
//! 禁止职责：不复制生产逻辑，不冒充Office或真实模型服务验收。

use openpaw_core::{Message, SessionEvent, SessionMeta};
use openpaw_store::{SessionReader, SessionWriter};

#[test]
fn damaged_tail_is_backed_up_before_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("会话.jsonl");
    let mut writer = SessionWriter::create(&path).unwrap();
    writer
        .append(&SessionEvent::Meta(SessionMeta::new("m", "p")))
        .unwrap();
    drop(writer);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"mess")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut writer = SessionWriter::open_append(&path).unwrap();
    writer
        .append(&SessionEvent::Message(Message::user_text("after crash")))
        .unwrap();
    let out = SessionReader::read(&path).expect("追加前必须修复损坏尾，不能把损坏行变成中间行");
    assert_eq!(out.events.len(), 2);
    assert_eq!(out.torn_tail_lines, 0);
    assert!(std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.path() != path && std::fs::read(entry.path()).unwrap() == original));
}

#[test]
fn valid_last_record_without_newline_remains_separate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let data = serde_json::to_vec(&SessionEvent::Meta(SessionMeta::new("m", "p"))).unwrap();
    std::fs::write(&path, data).unwrap();
    SessionWriter::open_append(&path)
        .unwrap()
        .append(&SessionEvent::Message(Message::user_text("hi")))
        .unwrap();
    assert_eq!(SessionReader::read(path).unwrap().events.len(), 2);
}

#[test]
fn middle_corruption_is_not_modified() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let meta = serde_json::to_string(&SessionEvent::Meta(SessionMeta::new("m", "p"))).unwrap();
    let data = format!("{meta}\ninvalid\n{meta}\n");
    std::fs::write(&path, &data).unwrap();
    assert!(
        SessionWriter::open_append(&path).is_err(),
        "中损必须拒绝追加"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), data);
}

#[test]
fn compacted_context_restores_without_duplicating_original_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let mut writer = SessionWriter::create(&path).unwrap();
    writer
        .append(&SessionEvent::Meta(SessionMeta::new("m", "p")))
        .unwrap();
    writer
        .append(&SessionEvent::Message(Message::user_text("old fact")))
        .unwrap();
    writer
        .append(&SessionEvent::Usage(openpaw_core::Usage {
            input_tokens: 3,
            output_tokens: 2,
            ..Default::default()
        }))
        .unwrap();
    writer
        .append(&SessionEvent::ContextSnapshot {
            messages: vec![Message::user_text("summary")],
        })
        .unwrap();
    writer
        .append(&SessionEvent::Message(Message::assistant_text("new reply")))
        .unwrap();
    drop(writer);
    let out = SessionReader::read(path).unwrap();
    let restored = out.restore();
    assert_eq!(
        restored
            .messages
            .iter()
            .map(Message::text)
            .collect::<Vec<_>>(),
        ["summary", "new reply"]
    );
    assert_eq!(restored.total_input_tokens, 3);
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, SessionEvent::Message(m) if m.text() == "old fact")));
}

#[test]
fn torn_utf8_tail_and_repeated_repair_preserve_unique_backups() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    SessionWriter::create(&path)
        .unwrap()
        .append(&SessionEvent::Meta(SessionMeta::new("m", "p")))
        .unwrap();
    for _ in 0..2 {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"text\":\"\xe4\xb8")
            .unwrap();
        assert_eq!(SessionReader::read(&path).unwrap().torn_tail_lines, 1);
        SessionWriter::open_append(&path)
            .unwrap()
            .append(&SessionEvent::Message(Message::user_text("recovered")))
            .unwrap();
    }
    assert_eq!(SessionReader::read(&path).unwrap().events.len(), 3);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}
