//! Stable wire types for the small OpenCode v1 subset exposed by the host.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status { #[serde(rename = "type")] pub kind: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event<T> { pub r#type: String, pub properties: T }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_fixture_round_trips() {
        let value = Status { kind: "busy".into() };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"type":"busy"}"#);
        assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), value);
    }

    #[test]
    fn event_fixture_round_trips() {
        let value = Event { r#type: "session.status".into(), properties: Status { kind: "idle".into() } };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<Event<Status>>(&json).unwrap(), value);
    }
}
