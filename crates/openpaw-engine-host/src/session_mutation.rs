//! 功能：把 Store 的共享历史变换接入 Host 会话协议校验。
//! 输入输出边界：Host 先验证公开消息身份，再使用中立变更类型和历史函数。
//! 禁止承担：不定义第二份持久类型，不写文件，不发布协议事件。
//! 关联：CHG-2026-0025。
pub(crate) use openpaw_store::transcript::{
    clone_messages_for_fork, failed_message_prefix_len, normalize_revert_boundary,
    visible_prefix_len, SessionForkOriginV1, SessionMetadataV1, SessionMutationEvent,
    SessionMutationProjection, SessionRevertCommittedV1, SessionRevertStagedV1,
};
#[cfg(test)]
mod tests {
    use openpaw_core::{CompactMetadata, CompactType, ContentBlock, Message, Role, SessionEvent};

    use super::{
        clone_messages_for_fork, visible_prefix_len, SessionForkOriginV1, SessionMetadataV1,
        SessionMutationEvent, SessionMutationProjection, SessionRevertStagedV1,
    };

    #[test]
    fn private_events_roundtrip_and_unknown_versions_are_ignored() {
        let cases = [
            SessionMutationEvent::Metadata(SessionMetadataV1 {
                title: Some("Renamed".into()),
                archived: Some(123),
            }),
            SessionMutationEvent::ForkOrigin(SessionForkOriginV1 {
                source_session_id: "source".into(),
                ordinal: 2,
            }),
            SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
                message_id: "m2".into(),
            }),
            SessionMutationEvent::RevertCleared,
        ];
        for expected in cases {
            let event = expected.to_session_event();
            let parsed = SessionMutationEvent::from_session_event(&event).expect("roundtrip");
            assert_eq!(
                serde_json::to_value(parsed.to_session_event()).unwrap(),
                serde_json::to_value(event).unwrap()
            );
        }
        assert!(
            SessionMutationEvent::from_session_event(&SessionEvent::Unknown(Box::new(
                serde_json::json!({"type":"session_revert_staged_v2","messageID":"m2"}),
            )))
            .is_none()
        );
    }

    #[test]
    fn visible_prefix_stops_strictly_before_the_boundary() {
        let messages = vec![
            Message::user_text("one").with_uuid("m1"),
            Message::assistant_text("two").with_uuid("m2"),
            Message::user_text("three").with_uuid("m3"),
        ];
        assert_eq!(visible_prefix_len("session", &messages, "m2"), Some(1));
        assert_eq!(visible_prefix_len("session", &messages, "missing"), None);
    }

    #[test]
    fn projection_folds_metadata_fork_origin_and_revert_lifecycle() {
        let mut projection = SessionMutationProjection::default();
        projection.apply(SessionMutationEvent::Metadata(SessionMetadataV1 {
            title: Some("Renamed".into()),
            archived: Some(123),
        }));
        projection.apply(SessionMutationEvent::ForkOrigin(SessionForkOriginV1 {
            source_session_id: "source".into(),
            ordinal: 2,
        }));
        projection.apply(SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
            message_id: "m2".into(),
        }));
        assert_eq!(projection.title.as_deref(), Some("Renamed"));
        assert_eq!(projection.archived, Some(123));
        assert_eq!(
            projection.fork_origin.as_ref().map(|origin| origin.ordinal),
            Some(2)
        );
        assert_eq!(projection.revert_message_id.as_deref(), Some("m2"));
        projection.apply(SessionMutationEvent::RevertCleared);
        assert_eq!(projection.revert_message_id, None);
    }

    #[test]
    fn visible_prefix_recognizes_legacy_wire_message_ids() {
        let messages = vec![Message::user_text("one"), Message::assistant_text("two")];
        assert_eq!(
            visible_prefix_len("session", &messages, "msg_session_1"),
            Some(1)
        );
    }

    #[test]
    fn fork_clone_rekeys_messages_tool_links_and_compaction_boundary() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-old".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path":"README.md"}),
                }],
                uuid: Some("assistant-old".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call-old".into(),
                        content: "ok".into(),
                        is_error: false,
                        name: Some("read_file".into()),
                        protected: false,
                    },
                    ContentBlock::CompactBoundary {
                        metadata: CompactMetadata {
                            compact_type: CompactType::Manual,
                            pre_compact_token_count: 12,
                            last_user_message_uuid: Some("assistant-old".into()),
                            pre_compact_discovered_tools: vec![],
                        },
                    },
                ],
                uuid: Some("user-old".into()),
                ..Default::default()
            },
        ];

        let cloned = clone_messages_for_fork(&messages);
        assert_eq!(cloned.len(), 2);
        assert_ne!(cloned[0].uuid, messages[0].uuid);
        assert_ne!(cloned[1].uuid, messages[1].uuid);
        let ContentBlock::ToolUse { id: new_call, .. } = &cloned[0].content[0] else {
            panic!("tool use");
        };
        let ContentBlock::ToolResult { tool_use_id, .. } = &cloned[1].content[0] else {
            panic!("tool result");
        };
        assert_eq!(tool_use_id, new_call);
        assert_ne!(new_call, "call-old");
        let ContentBlock::CompactBoundary { metadata } = &cloned[1].content[1] else {
            panic!("compact boundary");
        };
        assert_eq!(metadata.last_user_message_uuid, cloned[0].uuid);
    }
}
