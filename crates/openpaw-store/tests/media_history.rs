use openpaw_core::{InlineMedia, Message, Session, SessionEvent};
use openpaw_store::{transcript, SessionReader, SessionWriter};

#[test]
fn presentation_replays_with_inline_originals_and_real_title() {
    use transcript::{MessagePresentationV1, PartPresentationV1, TurnAcceptV1};
    let home = tempfile::tempdir().unwrap();
    let session = Session::new("vision", "openai");
    let media = InlineMedia::from_bytes(b"GIF89afixture", "image/gif", None).unwrap();
    let mut user = Message::user_text("attachment helper").with_uuid("user-1");
    user.content.push(openpaw_core::ContentBlock::Text {
        text: "分析图片".into(),
    });
    user.content.push(media.block());
    let accept = TurnAcceptV1 {
        message: user,
        outcome: serde_json::from_value(serde_json::json!({
            "turnID":"turn-1", "userMessageID":"user-1", "state":"running", "startedAt":1
        }))
        .unwrap(),
        request_admission: None,
        message_presentation: Some(MessagePresentationV1 {
            version: 1,
            parts: vec![PartPresentationV1 {
                block_index: 0,
                auxiliary: true,
                metadata: Some(
                    serde_json::from_value(serde_json::json!({"fixture":"opaque"})).unwrap(),
                ),
            }],
        }),
    };
    let path = home.path().join("session.jsonl");
    let mut writer = SessionWriter::create(&path).unwrap();
    writer
        .append(&SessionEvent::Meta(session.meta.clone()))
        .unwrap();
    writer.append(&accept.to_session_event()).unwrap();
    drop(writer);
    let data = SessionReader::read(&path).unwrap();
    let history = transcript::replay(&session.meta.id, &data.events).unwrap();
    assert_eq!(history.title(), "分析图片");
    assert_eq!(
        history.message_presentations["user-1"],
        accept.message_presentation.clone().unwrap()
    );
    assert_eq!(
        openpaw_store::media::attachment_snapshot(&history.messages)[&media.id],
        media
    );
}

#[test]
fn inline_originals_survive_restart_fork_and_visible_history_boundaries() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::new("vision", "openai");
    let media =
        InlineMedia::from_bytes(b"GIF89afixture", "image/gif", Some("capture.gif".into())).unwrap();
    let mut user = Message::user_text("inspect").with_uuid("user-1");
    user.content.push(media.block());
    let path = home.path().join("session.jsonl");
    let mut writer = SessionWriter::create(&path).unwrap();
    writer
        .append(&SessionEvent::Meta(session.meta.clone()))
        .unwrap();
    writer.append(&SessionEvent::Message(user.clone())).unwrap();
    writer
        .append(&SessionEvent::Message(Message::assistant_text("fixture")))
        .unwrap();
    drop(writer);
    let read = SessionReader::read(&path).unwrap();
    let mut history = transcript::replay(&session.meta.id, &read.events).unwrap();
    assert_eq!(
        openpaw_store::media::attachment_snapshot(history.visible_messages(&session.meta.id))
            [&media.id],
        media
    );
    let fork = transcript::clone_messages_for_fork(&history.messages);
    assert_eq!(
        openpaw_store::media::attachment_snapshot(&fork)[&media.id],
        media
    );
    history.mutation.revert_message_id = Some("user-1".into());
    assert!(
        openpaw_store::media::attachment_snapshot(history.visible_messages(&session.meta.id))
            .is_empty()
    );
    history.mutation.revert_message_id = None;
    assert_eq!(
        openpaw_store::media::attachment_snapshot(history.visible_messages(&session.meta.id))
            [&media.id]
            .bytes()
            .unwrap(),
        b"GIF89afixture"
    );
}
