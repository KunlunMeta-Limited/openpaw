use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, InlineMedia, LlmProvider,
    Message, Session, StopReason, ToolExecutor,
};
use openpaw_engine::{
    compaction::CompactOptions,
    turn_execution::{run_turn, TurnOptions},
    AgentLoop,
};
use std::sync::{Arc, Mutex};

struct MediaProvider {
    id: String,
    calls: Mutex<usize>,
    fail: bool,
}
#[async_trait::async_trait]
impl LlmProvider for MediaProvider {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        assert_eq!(req.model, "override-model");
        if req
            .system
            .as_deref()
            .is_some_and(|system| system.starts_with("Summarize"))
        {
            assert!(!req.messages[0].text().contains("R0lGOD"));
            return Ok(CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: "Old context summarized".into(),
                }],
                ..Default::default()
            });
        }
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            assert!(
                req.messages
                    .iter()
                    .any(|message| message.text().contains(&self.id)),
                "attachment ID must survive deterministic compaction"
            );
            Ok(CompletionResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "read-original".into(),
                    name: "read_attachment".into(),
                    input: serde_json::json!({"attachmentID":self.id}),
                }],
                stop_reason: StopReason::ToolUse,
                ..Default::default()
            })
        } else {
            assert!(req.messages.iter().flat_map(|message| &message.content).any(|block| matches!(block, ContentBlock::ToolResult { media, tool_use_id, .. } if tool_use_id == "read-original" && media[0].id == self.id)));
            if self.fail {
                return Err(openpaw_core::Error::session(
                    "fixture second request failed",
                ));
            }
            Ok(CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: "original available".into(),
                }],
                ..Default::default()
            })
        }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            image_input: true,
            audio_input: true,
            ..Default::default()
        }
    }
    fn name(&self) -> &str {
        "media-fixture"
    }
    fn default_model(&self) -> &str {
        "vision"
    }
}

#[tokio::test]
async fn compaction_reread_collects_full_outputs_and_failure_clears_transient_state() {
    for (fail, bytes, mime) in [
        (false, b"GIF89afixture".as_slice(), "image/gif"),
        (true, b"GIF89afixture".as_slice(), "image/gif"),
        (false, b"RIFF0000WAVEfixture".as_slice(), "audio/wav"),
        (true, b"ID3fixture".as_slice(), "audio/mpeg"),
    ] {
        let media = InlineMedia::from_bytes(bytes, mime, None).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let tools = Arc::new(openpaw_tools::ToolRegistry::new(workspace.path()));
        let provider = Arc::new(MediaProvider {
            id: media.id.clone(),
            calls: Mutex::new(0),
            fail,
        });
        let agent = AgentLoop::new(provider.clone())
            .with_tool_host(tools.clone())
            .with_max_tokens(64)
            .with_compact_options(CompactOptions {
                model_limit: 12_000,
                protect_head_pairs: 0,
                keep_recent_pairs: 1,
                use_context_aware_buffer: false,
                ..Default::default()
            });
        let mut session = Session::new("vision", "media-fixture");
        let mut first = Message::user_text("old attachment").identified();
        first.content.push(media.block());
        session.messages.push(first);
        for _ in 0..24 {
            session
                .messages
                .push(Message::assistant_text("x".repeat(2500)).identified());
            session
                .messages
                .push(Message::user_text("old question").identified());
        }
        session
            .messages
            .push(Message::user_text("inspect original").identified());
        let baseline = serde_json::to_value(&session).unwrap();
        let mut compacted = 0;
        let execution = run_turn(
            &agent,
            &mut session,
            &tools.list_tools().await.unwrap(),
            &mut |event| {
                if matches!(
                    event,
                    openpaw_engine::AgentEvent::CompactionPerformed { .. }
                ) {
                    compacted += 1;
                }
            },
            TurnOptions {
                provider_and_model: Some((provider, "override-model".into())),
                attachments: vec![media.clone()],
                ..Default::default()
            },
        )
        .await;
        assert!(compacted > 0);
        if fail {
            assert!(execution.result.is_err());
            assert!(execution.generated_messages.is_empty());
            assert_eq!(serde_json::to_value(&session).unwrap(), baseline);
        } else {
            assert!(execution.result.is_ok());
            assert_eq!(execution.generated_messages.len(), 3);
            assert_eq!(execution.generated_messages[2].text(), "original available");
            assert_eq!(
                openpaw_core::media::message_media(&execution.generated_messages),
                vec![media.clone()]
            );
            assert!(execution.generated_messages.iter().all(|message| !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::CompactBoundary { .. }))));
        }
        assert!(
            tools
                .call(
                    "read_attachment",
                    serde_json::json!({"attachmentID":media.id})
                )
                .await
                .unwrap()
                .is_error
        );
    }
}
