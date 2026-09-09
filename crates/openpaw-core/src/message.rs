//! Core message and content-block types shared across providers.
//!
//! Stability: stable — the serde wire format (`type` tags, field names) is
//! frozen; new variants may be added with forward-compat fallbacks.

use serde::{Deserialize, Serialize};

/// Author of a message.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Compaction boundary type.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactType {
    Auto,
    Manual,
    Micro,
}

/// Compaction boundary metadata.
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactMetadata {
    pub compact_type: CompactType,
    pub pre_compact_token_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_compact_discovered_tools: Vec<String>,
}

/// One conversation message: role plus a list of content blocks.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// RFC3339 timestamp of when this message was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Token count for this message (if known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
    /// Unique id. Used for compaction boundary tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            role: Role::User,
            content: Vec::new(),
            timestamp: None,
            token_count: None,
            uuid: None,
        }
    }
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    /// Set the uuid (builder style).
    pub fn with_uuid(mut self, uuid: impl Into<String>) -> Self {
        self.uuid = Some(uuid.into());
        self
    }

    /// Concatenated plain text of all Text blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A single block within a message. Mirrors the most expressive provider
/// content-block taxonomy; other providers translate down.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Extended-thinking block. Some providers emit this before the final
    /// text block; it is auxiliary content, not the user-visible reply.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        protected: bool,
    },
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    Document {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// Full-compaction boundary marker.
    CompactBoundary {
        metadata: CompactMetadata,
    },
    /// Forward-compatibility catch-all for unknown block types.
    /// Must stay last: serde matches tags in order and falls through here.
    #[serde(untagged)]
    Unknown(Box<serde_json::Value>),
}

impl ContentBlock {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }

    pub fn as_thinking(&self) -> Option<&str> {
        match self {
            ContentBlock::Thinking { thinking, .. } => Some(thinking),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_user_text_no_metadata() {
        let msg = Message::user_text("hello");
        assert_eq!(msg.role, Role::User);
        assert!(msg.timestamp.is_none());
        assert!(msg.token_count.is_none());
    }

    #[test]
    fn message_text_concatenation() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: "a".into() },
                ContentBlock::Text { text: "b".into() },
            ],
            ..Default::default()
        };
        assert_eq!(msg.text(), "ab");
    }

    #[test]
    fn message_backward_compat_deserialization() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, Role::User);
        assert!(msg.timestamp.is_none());
        assert!(msg.uuid.is_none());
    }

    #[test]
    fn message_full_roundtrip() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "response".into(),
                },
                ContentBlock::Image {
                    url: "https://example.com/img.png".into(),
                    media_type: None,
                    data: None,
                },
            ],
            timestamp: Some("2026-08-30T12:00:00Z".into()),
            token_count: Some(42),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, Role::Assistant);
        assert_eq!(parsed.content.len(), 2);
        assert_eq!(parsed.timestamp.as_deref(), Some("2026-08-30T12:00:00Z"));
    }

    #[test]
    fn content_block_unknown_fallback() {
        let json = r#"{"type":"future_block","data":"hello"}"#;
        let parsed: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(parsed, ContentBlock::Unknown(_)));
    }

    #[test]
    fn content_block_compact_boundary_roundtrip() {
        let block = ContentBlock::CompactBoundary {
            metadata: CompactMetadata {
                compact_type: CompactType::Auto,
                pre_compact_token_count: 1000,
                last_user_message_uuid: None,
                pre_compact_discovered_tools: vec![],
            },
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"compact_boundary\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ContentBlock::CompactBoundary { .. }));
    }
}
