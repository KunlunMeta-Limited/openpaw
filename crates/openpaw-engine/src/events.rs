//! Events emitted by the agent loop during a turn.

use openpaw_core::Usage;

/// Classifier / guardrail progress for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierStatus {
    Checking,
    AutoApproved { reason: String },
    SuggestConfirm { reason: String },
    AutoDenied { reason: String },
}

/// One observable step of a turn. The UI (or the parity harness) consumes
/// these; the loop never blocks on the sink.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    MessageStart,
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolUseStart {
        tool_use_id: String,
        name: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        name: Option<String>,
        duration_ms: Option<u64>,
    },
    ClassifierStatus {
        tool_use_id: String,
        status: ClassifierStatus,
    },
    /// E2: emitted when the compaction subsystem lands.
    CompactionPerformed {
        before_tokens: u32,
        after_tokens: u32,
    },
    TurnUsage {
        usage: Usage,
    },
}

impl AgentEvent {
    /// Structure-only event kind tag — the parity harness compares these
    /// sequences, never message text (plan §6.2 comparison keys).
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::MessageStart => "message_start",
            AgentEvent::TextDelta { .. } => "text_delta",
            AgentEvent::ThinkingDelta { .. } => "thinking_delta",
            AgentEvent::ToolUseStart { .. } => "tool_use_start",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::ClassifierStatus { .. } => "classifier_status",
            AgentEvent::CompactionPerformed { .. } => "compaction_performed",
            AgentEvent::TurnUsage { .. } => "turn_usage",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kinds_are_stable_tags() {
        assert_eq!(AgentEvent::MessageStart.kind(), "message_start");
        assert_eq!(
            AgentEvent::TextDelta { text: "x".into() }.kind(),
            "text_delta"
        );
        assert_eq!(
            AgentEvent::ToolResult {
                tool_use_id: "c1".into(),
                content: "y".into(),
                is_error: false,
                name: None,
                duration_ms: None,
            }
            .kind(),
            "tool_result"
        );
    }
}
