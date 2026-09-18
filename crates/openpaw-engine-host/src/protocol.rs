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

/// Create-session request body. §3.2 makes the body optional, so an empty body
/// yields the defaults; the fields skip `null` on the way out so an empty body
/// round-trips as `{}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// Empty strings are **omitted** on the wire. Verified against live agent
    /// bytes: a running tool part is `{"status":"running","input":{}}` with no
    /// `output`/`error` keys at all. §3.2's snapshot spells them as `""`; the
    /// fixtures follow the host, and that text discrepancy is tracked as D-6.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Not part of the §3.2 snapshot; emitted only when the tool reported a
    /// duration. Observed live as e.g. `"durationMs":17585`.
    #[serde(
        rename = "durationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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

// ---------------------------------------------------------------------------
// SSE event properties (§3.2 "SSE 事件")
//
// The frame is one `data:` line carrying `Event<T>`; each event kind binds one
// of the property shapes below. Every ID is camelCase on the wire, exactly as
// the snapshot spells it.
// ---------------------------------------------------------------------------

/// `session.created` / `session.updated` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshotProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub info: Session,
}

/// `session.status` properties: `{ "sessionID", "status": { "type": ... } }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionStatusProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub status: Status,
}

/// `session.idle` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionIdleProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
}

/// `message.updated` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageUpdatedProperties {
    pub info: MessageInfo,
}

/// `message.part.updated` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartUpdatedProperties {
    pub part: Part,
}

/// `message.part.delta` properties. §3.2 pinpoints `field` as unreliable:
/// consumers must trust the preceding `message.part.updated` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartDeltaProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "partID")]
    pub part_id: String,
    pub field: String,
    pub delta: String,
}

/// `todo.updated` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoUpdatedProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub todos: Vec<TodoItem>,
}

/// `permission.replied` properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRepliedProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(rename = "requestID")]
    pub request_id: String,
}

/// `session.error` properties. Emitted on engine and persistence failures; not
/// listed in the §3.2 event table, frozen here so its shape cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionErrorProperties {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub error: SessionErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionErrorBody {
    pub name: String,
    pub data: SessionErrorData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionErrorData {
    pub message: String,
}

/// `session.created` frame.
pub type SessionCreatedEvent = Event<SessionSnapshotProperties>;
/// `session.updated` frame.
pub type SessionUpdatedEvent = Event<SessionSnapshotProperties>;
/// `session.status` frame.
pub type SessionStatusEvent = Event<SessionStatusProperties>;
/// `session.idle` frame.
pub type SessionIdleEvent = Event<SessionIdleProperties>;
/// `message.updated` frame.
pub type MessageUpdatedEvent = Event<MessageUpdatedProperties>;
/// `message.part.updated` frame.
pub type MessagePartUpdatedEvent = Event<PartUpdatedProperties>;
/// `message.part.delta` frame.
pub type MessagePartDeltaEvent = Event<PartDeltaProperties>;
/// `todo.updated` frame.
pub type TodoUpdatedEvent = Event<TodoUpdatedProperties>;
/// `permission.asked` frame — properties are the permission object itself.
pub type PermissionAskedEvent = Event<PermissionRequest>;
/// `permission.replied` frame.
pub type PermissionRepliedEvent = Event<PermissionRepliedProperties>;
/// `session.error` frame.
pub type SessionErrorEvent = Event<SessionErrorProperties>;

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

    /// Live agent bytes for a running tool part are
    /// `{"status":"running","input":{}}` with **no** `output`/`error` keys.
    /// §3.2's snapshot spells them as `""`; the fixture records the host, and
    /// that text discrepancy is tracked as D-6.
    #[test]
    fn running_tool_part_omits_empty_output_and_error() {
        // Compare parsed values, not source text: `#[serde(tag = "type")]` writes
        // `type` first while the live frames write it last. Key order carries no
        // meaning in JSON, so only the field set may be asserted.
        let json = r#"{"callID":"call_0","id":"prt_1","messageID":"msg_1","sessionID":"ses_1","state":{"input":{},"status":"running"},"tool":"read_file","type":"tool"}"#;
        let expected: serde_json::Value = serde_json::from_str(json).unwrap();
        let value = serde_json::from_str::<Part>(json).unwrap();
        assert_eq!(value.message_id(), "msg_1");

        let actual = serde_json::to_value(&value).unwrap();
        assert_eq!(actual, expected);
        assert!(
            actual["state"].get("output").is_none() && actual["state"].get("error").is_none(),
            "empty output/error must stay off the wire"
        );
    }

    #[test]
    fn reasoning_part_snapshot_round_trips() {
        let json = r#"{"id":"prt_2","messageID":"msg_1","sessionID":"ses_1","type":"reasoning","text":"weighing options"}"#;
        let expected: serde_json::Value = serde_json::from_str(json).unwrap();
        let value = serde_json::from_str::<Part>(json).unwrap();
        assert_eq!(value.id(), "prt_2");
        assert_eq!(serde_json::to_value(&value).unwrap(), expected);
    }

    #[test]
    fn empty_create_body_round_trips() {
        let value = serde_json::from_str::<CreateSession>("{}").unwrap();
        assert_eq!(value, CreateSession::default());
        assert_eq!(serde_json::to_string(&value).unwrap(), "{}");
    }

    /// §3.2 pins `time` to Unix milliseconds. Guard the emitter too, so a
    /// switch to seconds cannot slip past the snapshot fixtures.
    #[test]
    fn emitted_timestamps_are_unix_milliseconds() {
        // 1e12 ms is 2001-09-09; seconds-since-epoch is ~1.7e9, far below.
        const MILLISECOND_FLOOR: i64 = 1_000_000_000_000;

        let message = crate::events::user_message("ses_1", Some("msg_1".into()), "hi".into());
        assert!(
            message.info.time.created > MILLISECOND_FLOOR,
            "message created must be milliseconds, got {}",
            message.info.time.created
        );
        let completed = message.info.time.completed.expect("user message completes");
        assert!(
            completed > MILLISECOND_FLOOR,
            "message completed must be milliseconds, got {completed}"
        );
    }

    /// The frozen properties must describe what `events.rs` really emits;
    /// otherwise the "contract" would be a second, drifting source of truth.
    #[test]
    fn emitted_session_events_match_frozen_properties() {
        let session = Session {
            id: "ses_1".into(),
            title: "New session".into(),
            slug: "ses_1".into(),
            directory: "/opt/openwork".into(),
            time: Time {
                created: 0,
                updated: 0,
            },
        };

        let created = crate::events::session_event("session.created", &session);
        let typed: SessionCreatedEvent = serde_json::from_value(created.clone()).unwrap();
        assert_eq!(typed.r#type, "session.created");
        assert_eq!(typed.properties.session_id, "ses_1");
        assert_eq!(serde_json::to_value(&typed).unwrap(), created);

        let status = crate::events::session_status("ses_1", "busy");
        let typed: SessionStatusEvent = serde_json::from_value(status.clone()).unwrap();
        assert_eq!(typed.properties.status.kind, "busy");
        assert_eq!(serde_json::to_value(&typed).unwrap(), status);

        let idle = crate::events::session_idle("ses_1");
        let typed: SessionIdleEvent = serde_json::from_value(idle.clone()).unwrap();
        assert_eq!(typed.properties.session_id, "ses_1");
        assert_eq!(serde_json::to_value(&typed).unwrap(), idle);

        let todo = crate::events::todo_updated("ses_1");
        let typed: TodoUpdatedEvent = serde_json::from_value(todo.clone()).unwrap();
        assert!(typed.properties.todos.is_empty());
        assert_eq!(serde_json::to_value(&typed).unwrap(), todo);
    }

    #[test]
    fn emitted_message_events_match_frozen_properties() {
        let message = crate::events::user_message("ses_1", Some("msg_1".into()), "hi".into());
        let events = crate::events::message_events(&message);
        assert_eq!(events.len(), 2);

        let updated: MessageUpdatedEvent = serde_json::from_value(events[0].clone()).unwrap();
        assert_eq!(updated.properties.info.session_id, "ses_1");
        assert_eq!(updated.properties.info.role, "user");
        assert_eq!(serde_json::to_value(&updated).unwrap(), events[0]);

        let part: MessagePartUpdatedEvent = serde_json::from_value(events[1].clone()).unwrap();
        assert_eq!(part.properties.part.message_id(), "msg_1");
        assert_eq!(serde_json::to_value(&part).unwrap(), events[1]);
    }
}
