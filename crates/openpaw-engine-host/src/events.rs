use crate::protocol::{Message, MessageInfo, MessageTime, Part, ToolState};
use openpaw_engine::AgentEvent;
use serde_json::{json, Value};
use uuid::Uuid;

pub struct WireUpdate {
    pub events: Vec<Value>,
    pub messages: Vec<Message>,
}

impl WireUpdate {
    fn empty() -> Self {
        Self {
            events: Vec::new(),
            messages: Vec::new(),
        }
    }
}

pub struct TurnEventMapper {
    session_id: String,
    current: Option<Message>,
}

impl TurnEventMapper {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            current: None,
        }
    }

    pub fn apply(&mut self, event: AgentEvent) -> WireUpdate {
        match event {
            AgentEvent::MessageStart => self.start_assistant_message(),
            AgentEvent::TextDelta { text } => self.append_delta("text", text),
            AgentEvent::ThinkingDelta { text } => self.append_delta("reasoning", text),
            AgentEvent::ToolUseStart { tool_use_id, name } => self.start_tool(tool_use_id, name),
            AgentEvent::ToolResult {
                tool_use_id,
                content,
                is_error,
                name,
                duration_ms,
            } => self.finish_tool(tool_use_id, content, is_error, name, duration_ms),
            AgentEvent::ClassifierStatus { .. }
            | AgentEvent::TurnUsage { .. }
            | AgentEvent::CompactionPerformed { .. } => WireUpdate::empty(),
        }
    }

    pub fn finish(&mut self) -> WireUpdate {
        let Some(mut message) = self.current.take() else {
            return WireUpdate::empty();
        };
        message.info.time.completed = Some(now());
        let mut events = message.parts.iter().map(part_updated).collect::<Vec<_>>();
        events.push(message_updated(&message));
        WireUpdate {
            events,
            messages: vec![message],
        }
    }

    fn start_assistant_message(&mut self) -> WireUpdate {
        let mut events = Vec::new();
        let mut messages = Vec::new();
        if let Some(mut previous) = self.current.take() {
            previous.info.time.completed = Some(now());
            events.extend(previous.parts.iter().map(part_updated));
            events.push(message_updated(&previous));
            messages.push(previous);
        }

        let message = Message {
            info: MessageInfo {
                id: wire_id("msg"),
                session_id: self.session_id.clone(),
                role: "assistant".into(),
                time: MessageTime {
                    created: now(),
                    completed: None,
                },
            },
            parts: Vec::new(),
        };
        events.push(message_updated(&message));
        messages.push(message.clone());
        self.current = Some(message);
        WireUpdate { events, messages }
    }

    fn ensure_assistant_message(&mut self) -> WireUpdate {
        if self.current.is_none() {
            self.start_assistant_message()
        } else {
            WireUpdate::empty()
        }
    }

    fn append_delta(&mut self, kind: &str, delta: String) -> WireUpdate {
        let initial = self.ensure_assistant_message();
        let message = self.current.as_mut().expect("message initialized");
        let existing = message.parts.iter().position(|part| {
            matches!(
                (kind, part),
                ("text", Part::Text { .. }) | ("reasoning", Part::Reasoning { .. })
            )
        });

        let mut events = initial.events;
        let part_index = if let Some(index) = existing {
            index
        } else {
            let id = wire_id("prt");
            let part = if kind == "reasoning" {
                Part::Reasoning {
                    id,
                    message_id: message.info.id.clone(),
                    session_id: self.session_id.clone(),
                    text: String::new(),
                }
            } else {
                Part::Text {
                    id,
                    message_id: message.info.id.clone(),
                    session_id: self.session_id.clone(),
                    text: String::new(),
                }
            };
            events.push(part_updated(&part));
            message.parts.push(part);
            message.parts.len() - 1
        };

        let part = &mut message.parts[part_index];
        match part {
            Part::Text { text, .. } | Part::Reasoning { text, .. } => text.push_str(&delta),
            Part::Tool { .. } => unreachable!("text lookup cannot return a tool part"),
        }
        events.push(json!({
            "type": "message.part.delta",
            "properties": {
                "sessionID": self.session_id,
                "messageID": part.message_id(),
                "partID": part.id(),
                "field": "text",
                "delta": delta
            }
        }));
        WireUpdate {
            events,
            messages: vec![message.clone()],
        }
    }

    fn start_tool(&mut self, call_id: String, tool: String) -> WireUpdate {
        let initial = self.ensure_assistant_message();
        let message = self.current.as_mut().expect("message initialized");
        let part = Part::Tool {
            id: wire_id("prt"),
            message_id: message.info.id.clone(),
            session_id: self.session_id.clone(),
            call_id,
            tool,
            state: ToolState {
                status: "running".into(),
                input: json!({}),
                output: String::new(),
                error: String::new(),
                duration_ms: None,
            },
        };
        message.parts.push(part.clone());
        let mut events = initial.events;
        events.push(part_updated(&part));
        WireUpdate {
            events,
            messages: vec![message.clone()],
        }
    }

    fn finish_tool(
        &mut self,
        call_id: String,
        content: String,
        is_error: bool,
        name: Option<String>,
        duration_ms: Option<u64>,
    ) -> WireUpdate {
        let initial = self.ensure_assistant_message();
        let message = self.current.as_mut().expect("message initialized");
        let part_index = message.parts.iter().position(
            |part| matches!(part, Part::Tool { call_id: existing, .. } if existing == &call_id),
        );
        let part = match part_index {
            Some(index) => {
                let Part::Tool { tool, state, .. } = &mut message.parts[index] else {
                    unreachable!("tool lookup can only return a tool part");
                };
                if let Some(name) = name {
                    *tool = name;
                }
                state.status = if is_error { "error" } else { "completed" }.into();
                state.duration_ms = duration_ms;
                if is_error {
                    state.error = content;
                    state.output.clear();
                } else {
                    state.output = content;
                    state.error.clear();
                }
                message.parts[index].clone()
            }
            None => {
                let part = Part::Tool {
                    id: wire_id("prt"),
                    message_id: message.info.id.clone(),
                    session_id: self.session_id.clone(),
                    call_id,
                    tool: name.unwrap_or_else(|| "tool".into()),
                    state: ToolState {
                        status: if is_error { "error" } else { "completed" }.into(),
                        input: json!({}),
                        output: if is_error {
                            String::new()
                        } else {
                            content.clone()
                        },
                        error: if is_error { content } else { String::new() },
                        duration_ms,
                    },
                };
                message.parts.push(part.clone());
                part
            }
        };
        let mut events = initial.events;
        events.push(part_updated(&part));
        if matches!(
            &part,
            Part::Tool { tool, .. }
                if matches!(tool.as_str(), "todo_add" | "todo_update" | "todo_list")
        ) {
            events.push(todo_updated(&self.session_id));
        }
        WireUpdate {
            events,
            messages: vec![message.clone()],
        }
    }
}

pub fn user_message(session_id: &str, message_id: Option<String>, text: String) -> Message {
    let id = message_id.unwrap_or_else(|| wire_id("msg"));
    Message {
        info: MessageInfo {
            id: id.clone(),
            session_id: session_id.into(),
            role: "user".into(),
            time: MessageTime {
                created: now(),
                completed: Some(now()),
            },
        },
        parts: vec![Part::Text {
            id: wire_id("prt"),
            message_id: id,
            session_id: session_id.into(),
            text,
        }],
    }
}

pub fn message_events(message: &Message) -> Vec<Value> {
    std::iter::once(message_updated(message))
        .chain(message.parts.iter().map(part_updated))
        .collect()
}

pub fn session_event(kind: &str, session: &crate::protocol::Session) -> Value {
    json!({"type": kind, "properties": {"sessionID": session.id, "info": session}})
}

pub fn session_status(session_id: &str, kind: &str) -> Value {
    json!({"type":"session.status","properties":{"sessionID":session_id,"status":{"type":kind}}})
}

pub fn session_idle(session_id: &str) -> Value {
    json!({"type":"session.idle","properties":{"sessionID":session_id}})
}

pub fn todo_updated(session_id: &str) -> Value {
    json!({"type":"todo.updated","properties":{"sessionID":session_id,"todos":[]}})
}

fn message_updated(message: &Message) -> Value {
    json!({"type":"message.updated","properties":{"info":message.info}})
}

fn part_updated(part: &Part) -> Value {
    json!({"type":"message.part.updated","properties":{"part":part}})
}

fn wire_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_declares_part_and_carries_all_ids() {
        let mut mapper = TurnEventMapper::new("ses_1");
        let start = mapper.apply(AgentEvent::MessageStart);
        assert_eq!(start.events[0]["type"], "message.updated");
        let update = mapper.apply(AgentEvent::TextDelta { text: "hi".into() });
        assert_eq!(update.events[0]["type"], "message.part.updated");
        let delta = &update.events[1];
        assert_eq!(delta["properties"]["sessionID"], "ses_1");
        assert!(delta["properties"]["messageID"].as_str().is_some());
        assert!(delta["properties"]["partID"].as_str().is_some());
        assert_eq!(update.messages[0].parts.len(), 1);
    }

    #[test]
    fn finishing_turn_completes_assistant_snapshot() {
        let mut mapper = TurnEventMapper::new("ses_1");
        mapper.apply(AgentEvent::MessageStart);
        mapper.apply(AgentEvent::TextDelta { text: "ok".into() });
        let update = mapper.finish();
        assert!(update.messages[0].info.time.completed.is_some());
        assert_eq!(update.events.last().unwrap()["type"], "message.updated");
    }

    #[test]
    fn first_delta_synthesizes_message_start() {
        let mut mapper = TurnEventMapper::new("ses_1");
        let update = mapper.apply(AgentEvent::TextDelta { text: "hi".into() });
        assert_eq!(update.events[0]["type"], "message.updated");
        assert_eq!(update.events[1]["type"], "message.part.updated");
        assert_eq!(update.events[2]["type"], "message.part.delta");
    }

    #[test]
    fn todo_tool_result_emits_todo_update() {
        let mut mapper = TurnEventMapper::new("ses_1");
        mapper.apply(AgentEvent::ToolUseStart {
            tool_use_id: "call_1".into(),
            name: "todo_add".into(),
        });
        let update = mapper.apply(AgentEvent::ToolResult {
            tool_use_id: "call_1".into(),
            content: "added todo #1".into(),
            is_error: false,
            name: Some("todo_add".into()),
            duration_ms: Some(1),
        });
        assert_eq!(update.events[0]["type"], "message.part.updated");
        assert_eq!(update.events[1]["type"], "todo.updated");
        assert_eq!(update.events[1]["properties"]["sessionID"], "ses_1");
        assert_eq!(update.events[1]["properties"]["todos"], json!([]));
    }
}
