//! Stable wire types for the small OpenCode v1 subset exposed by the host.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub reason: String,
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
    pub decision: PermissionDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    Reasoning { text: String },
    Tool { call_id: String, name: Option<String>, status: String, output: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem { pub id: String, pub content: String, pub status: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provider { pub id: String, pub name: String }

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
    fn permission_fixture_round_trips() {
        let value = PermissionReply {
            decision: PermissionDecision::Once,
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"decision":"once"}"#);
        assert_eq!(
            serde_json::from_str::<PermissionReply>(&json).unwrap(),
            value
        );
    }
}
