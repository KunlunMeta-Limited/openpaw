use openpaw_engine::AgentEvent;
pub fn to_wire(event: AgentEvent) -> Option<serde_json::Value> {
    let (kind, properties) = match event {
        AgentEvent::MessageStart => ("message.updated", serde_json::json!({"role":"assistant"})),
        AgentEvent::TextDelta { text } => (
            "message.part.delta",
            serde_json::json!({"field":"text","delta":text}),
        ),
        AgentEvent::ThinkingDelta { text } => (
            "message.part.delta",
            serde_json::json!({"field":"reasoning","delta":text}),
        ),
        AgentEvent::ToolUseStart { tool_use_id, name } => (
            "message.part.updated",
            serde_json::json!({"callID":tool_use_id,"tool":name,"status":"running"}),
        ),
        AgentEvent::ToolResult {
            tool_use_id,
            content,
            is_error,
            name,
            duration_ms,
        } => (
            "message.part.updated",
            serde_json::json!({"callID":tool_use_id,"tool":name,"output":content,"status":if is_error {"error"} else {"completed"},"durationMs":duration_ms}),
        ),
        AgentEvent::ClassifierStatus {
            tool_use_id,
            status,
        } => (
            "message.part.updated",
            serde_json::json!({"callID":tool_use_id,"status":format!("{status:?}")}),
        ),
        AgentEvent::TurnUsage { .. } => ("turn.usage", serde_json::json!({})),
        AgentEvent::CompactionPerformed { .. } => return None,
    };
    Some(serde_json::json!({"type":kind,"properties":properties}))
}
