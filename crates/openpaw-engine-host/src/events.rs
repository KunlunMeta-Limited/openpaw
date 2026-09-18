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

/// §2.5: `prompt_async` opens a turn with the user message events and a busy
/// status. Keeping the frame here rather than inline in `http.rs` is what makes
/// the whole-turn ordering assertable from a unit test.
pub fn turn_start_events(session_id: &str) -> Vec<Value> {
    vec![session_status(session_id, "busy")]
}

/// §2.5: a finished turn closes with an idle status and `session.idle`, in that
/// order, at the tail of the session's stream.
pub fn turn_finish_events(session_id: &str) -> Vec<Value> {
    vec![session_status(session_id, "idle"), session_idle(session_id)]
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
    use openpaw_core::Usage;
    use openpaw_engine::ClassifierStatus;

    /// Collect the SSE `type` of each frame, in order.
    fn types_of(events: &[Value]) -> Vec<&str> {
        events
            .iter()
            .map(|event| event["type"].as_str().expect("frame carries a type"))
            .collect()
    }

    fn text_delta(text: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: text.into() }
    }

    fn thinking_delta(text: &str) -> AgentEvent {
        AgentEvent::ThinkingDelta { text: text.into() }
    }

    fn tool_result(is_error: bool) -> AgentEvent {
        AgentEvent::ToolResult {
            tool_use_id: "call_1".into(),
            content: "payload".into(),
            is_error,
            name: Some("read_file".into()),
            duration_ms: Some(7),
        }
    }

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

    /// §2.5 mapping table, one row per `AgentEvent::kind()`. `None` means the
    /// kind is explicitly excluded from the OpenCode surface. A fresh mapper is
    /// primed with `MessageStart` so each row observes only its own frames.
    #[test]
    fn every_agent_event_kind_maps_to_expected_sse() {
        let cases: Vec<(AgentEvent, Option<Vec<&str>>)> = vec![
            (AgentEvent::MessageStart, Some(vec!["message.updated"])),
            (
                text_delta("a"),
                Some(vec!["message.part.updated", "message.part.delta"]),
            ),
            (
                thinking_delta("a"),
                Some(vec!["message.part.updated", "message.part.delta"]),
            ),
            (
                AgentEvent::ToolUseStart {
                    tool_use_id: "call_1".into(),
                    name: "read_file".into(),
                },
                Some(vec!["message.part.updated"]),
            ),
            (tool_result(false), Some(vec!["message.part.updated"])),
            (
                AgentEvent::ClassifierStatus {
                    tool_use_id: "call_1".into(),
                    status: ClassifierStatus::AutoDenied {
                        reason: "blocked".into(),
                    },
                },
                None,
            ),
            (
                AgentEvent::CompactionPerformed {
                    before_tokens: 10,
                    after_tokens: 5,
                },
                None,
            ),
            (
                AgentEvent::TurnUsage {
                    usage: Usage::default(),
                },
                None,
            ),
        ];

        let mut kinds: Vec<&str> = cases.iter().map(|(event, _)| event.kind()).collect();
        let covered = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), covered, "each kind must appear exactly once");
        assert_eq!(covered, 8, "AgentEvent exposes 8 kinds");

        for (event, expected) in cases {
            let mut mapper = TurnEventMapper::new("ses_1");
            if !matches!(event, AgentEvent::MessageStart) {
                mapper.apply(AgentEvent::MessageStart);
            }
            let update = mapper.apply(event);
            let types = types_of(&update.events);
            match expected {
                Some(expected) => {
                    assert_eq!(types, expected, "unexpected mapping for this kind")
                }
                None => assert!(types.is_empty(), "excluded kind emitted {types:?}"),
            }
        }
    }

    /// Completion evidence: the unmapped inventory holds exactly the kinds §2.5
    /// excludes — no more.
    #[test]
    fn unmapped_kinds_are_exactly_the_planned_exclusions() {
        let excluded = [
            AgentEvent::ClassifierStatus {
                tool_use_id: "call_1".into(),
                status: ClassifierStatus::AutoDenied {
                    reason: "blocked".into(),
                },
            },
            AgentEvent::CompactionPerformed {
                before_tokens: 9,
                after_tokens: 4,
            },
            AgentEvent::TurnUsage {
                usage: Usage::default(),
            },
        ];

        let mut mapper = TurnEventMapper::new("ses_1");
        mapper.apply(AgentEvent::MessageStart);
        let mut names: Vec<&str> = Vec::new();
        for event in excluded {
            let kind = event.kind();
            assert!(
                mapper.apply(event).events.is_empty(),
                "{kind} must not reach the OpenCode surface"
            );
            names.push(kind);
        }
        names.sort_unstable();
        assert_eq!(
            names,
            ["classifier_status", "compaction_performed", "turn_usage"]
        );
    }

    /// `busy` opens the turn, the assistant stream follows, and the turn closes
    /// with idle `session.status` + `session.idle` at the very tail.
    #[test]
    fn busy_precedes_assistant_stream_and_idle_closes_turn() {
        let session_id = "ses_1";
        let mut sequence: Vec<Value> = turn_start_events(session_id);
        assert_eq!(sequence[0]["properties"]["status"]["type"], "busy");

        let mut mapper = TurnEventMapper::new(session_id);
        for event in [
            AgentEvent::MessageStart,
            thinking_delta("think"),
            text_delta("hi"),
            AgentEvent::ToolUseStart {
                tool_use_id: "call_1".into(),
                name: "read_file".into(),
            },
            tool_result(false),
        ] {
            sequence.extend(mapper.apply(event).events);
        }
        // §2.5: the turn end fills `time.completed` and closes the stream.
        let finished = mapper.finish();
        assert!(finished.messages[0].info.time.completed.is_some());
        sequence.extend(finished.events);
        sequence.extend(turn_finish_events(session_id));

        let types = types_of(&sequence);
        let busy = types
            .iter()
            .position(|kind| *kind == "session.status")
            .expect("busy frame present");
        let first_assistant = types
            .iter()
            .position(|kind| *kind == "message.updated")
            .expect("assistant frame present");
        assert!(
            busy < first_assistant,
            "busy must precede the assistant stream: {types:?}"
        );

        let tail = &types[types.len() - 2..];
        assert_eq!(
            tail,
            ["session.status", "session.idle"],
            "turn tail: {types:?}"
        );
        assert_eq!(
            sequence[sequence.len() - 2]["properties"]["status"]["type"],
            "idle"
        );
    }

    /// The first text/reasoning delta declares the part and then sends the
    /// delta; later deltas must not redeclare it (§2.5: "首次收到时").
    #[test]
    fn first_delta_declares_part_then_delta_and_later_deltas_do_not() {
        for (event, field) in [
            (text_delta("a"), "text"),
            (thinking_delta("a"), "reasoning"),
        ] {
            let mut mapper = TurnEventMapper::new("ses_1");
            mapper.apply(AgentEvent::MessageStart);

            let first = mapper.apply(event.clone());
            assert_eq!(
                types_of(&first.events),
                ["message.part.updated", "message.part.delta"],
                "{field}: first delta order"
            );
            let declared = &first.events[0]["properties"]["part"];
            assert_eq!(declared["type"], field);
            assert_eq!(
                declared["id"], first.events[1]["properties"]["partID"],
                "{field}: delta must target the declared part"
            );

            let later = mapper.apply(event);
            assert_eq!(
                types_of(&later.events),
                ["message.part.delta"],
                "{field}: later deltas must not redeclare the part"
            );
        }
    }

    /// A tool part moves `running` → `completed` / `error` in place, and its
    /// `callID` is the engine's `tool_use_id`.
    #[test]
    fn tool_part_transitions_from_running_to_completed_or_error() {
        for (is_error, expected) in [(false, "completed"), (true, "error")] {
            let mut mapper = TurnEventMapper::new("ses_1");
            mapper.apply(AgentEvent::MessageStart);

            let started = mapper.apply(AgentEvent::ToolUseStart {
                tool_use_id: "call_1".into(),
                name: "read_file".into(),
            });
            let running = &started.events[0]["properties"]["part"];
            assert_eq!(running["type"], "tool");
            assert_eq!(running["callID"], "call_1");
            assert_eq!(running["state"]["status"], "running");
            let part_id = running["id"].clone();

            let finished = mapper.apply(tool_result(is_error));
            let done = &finished.events[0]["properties"]["part"];
            assert_eq!(done["id"], part_id, "the same part must be updated");
            assert_eq!(done["state"]["status"], expected);
            assert_eq!(done["state"]["durationMs"], 7);
            if is_error {
                assert_eq!(done["state"]["error"], "payload");
                assert!(done["state"].get("output").is_none());
            } else {
                assert_eq!(done["state"]["output"], "payload");
                assert!(done["state"].get("error").is_none());
            }
        }
    }
}
