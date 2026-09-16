//! Stable wire types for the small OpenCode v1 subset exposed by the host.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event<T> {
    pub r#type: String,
    pub properties: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub directory: String,
    pub time: Time,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Time {
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct CreateSession {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SessionQuery {
    #[serde(default)]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    pub metadata: serde_json::Value,
    pub always: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Once,
    Always,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionReply {
    #[serde(alias = "decision")]
    pub reply: PermissionDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub info: MessageInfo,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageInfo {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub role: String,
    pub time: MessageTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageTime {
    pub created: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        text: String,
    },
    Reasoning {
        id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        text: String,
    },
    Tool {
        id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        #[serde(rename = "callID")]
        call_id: String,
        tool: String,
        state: ToolState,
    },
}

impl Part {
    pub fn id(&self) -> &str {
        match self {
            Self::Text { id, .. } | Self::Reasoning { id, .. } | Self::Tool { id, .. } => id,
        }
    }

    pub fn message_id(&self) -> &str {
        match self {
            Self::Text { message_id, .. }
            | Self::Reasoning { message_id, .. }
            | Self::Tool { message_id, .. } => message_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolState {
    pub status: String,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Prompt {
    #[serde(default, rename = "messageID", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    pub parts: Vec<PromptPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: BTreeMap<String, ProviderModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderList {
    pub all: Vec<Provider>,
    pub connected: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_fixture_round_trips() {
        let value = Status {
            kind: "busy".into(),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"type":"busy"}"#);
        assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), value);
    }

    #[test]
    fn event_fixture_round_trips() {
        let value = Event {
            r#type: "session.status".into(),
            properties: Status {
                kind: "idle".into(),
            },
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<Event<Status>>(&json).unwrap(), value);
    }

    #[test]
    fn permission_fixture_accepts_v1_reply_and_legacy_decision() {
        let value = PermissionReply {
            reply: PermissionDecision::Once,
        };
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"reply":"once"}"#
        );
        assert_eq!(
            serde_json::from_str::<PermissionReply>(r#"{"decision":"once"}"#).unwrap(),
            value
        );
    }

    #[test]
    fn prompt_model_fixture_round_trips() {
        let json = r#"{"model":{"providerID":"mock","modelID":"mock-1"},"parts":[{"type":"text","text":"user prompt"}]}"#;
        let value = serde_json::from_str::<Prompt>(json).unwrap();
        assert_eq!(value.model.as_ref().unwrap().provider_id, "mock");
        assert_eq!(value.model.as_ref().unwrap().model_id, "mock-1");
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }

    #[test]
    fn message_snapshot_uses_opencode_ids() {
        let json = r#"{"info":{"id":"msg_1","sessionID":"ses_1","role":"assistant","time":{"created":1,"completed":2}},"parts":[{"type":"text","id":"prt_1","messageID":"msg_1","sessionID":"ses_1","text":"ok"}]}"#;
        let value = serde_json::from_str::<Message>(json).unwrap();
        assert_eq!(value.info.session_id, "ses_1");
        assert_eq!(value.parts[0].id(), "prt_1");
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }

    #[test]
    fn provider_list_fixture_round_trips() {
        let json = r#"{"all":[{"id":"mock","name":"OpenPaw mock","models":{"mock-1":{"id":"mock-1","name":"mock-1"}}},{"id":"echo","name":"OpenPaw echo","models":{"echo-1":{"id":"echo-1","name":"echo-1"}}},{"id":"anthropic","name":"Anthropic","models":{"claude-sonnet-4":{"id":"claude-sonnet-4","name":"claude-sonnet-4"}}},{"id":"openai","name":"OpenAI","models":{"gpt-4o":{"id":"gpt-4o","name":"gpt-4o"}}}],"connected":["mock","echo"]}"#;
        let value = serde_json::from_str::<ProviderList>(json).unwrap();
        assert_eq!(value.all.len(), 4);
        assert_eq!(value.connected, ["mock", "echo"]);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }
}
