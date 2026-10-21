//! `structured_output` — emit a structured JSON payload for downstream
//! consumers (sub-agents, harnesses). The payload must be a JSON object.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolSpec};
use serde_json::json;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "structured_output".into(),
        description: "Emit a structured JSON object as the task's final output. Use this when the caller asked for machine-readable results.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "data": {"type": "object", "description": "The structured result object"}
            },
            "required": ["data"]
        }),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, input: serde_json::Value) -> Result<ToolCallOutcome> {
    let Some(data) = input.get("data") else {
        return Ok(ToolCallOutcome::error(
            "structured_output: `data` field is required",
        ));
    };
    if !data.is_object() {
        return Ok(ToolCallOutcome::error(
            "structured_output: `data` must be a JSON object",
        ));
    }
    let pretty = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    Ok(ToolCallOutcome {
        media: Vec::new(),
        content: format!("structured output recorded:\n{pretty}"),
        is_error: false,
        structured: Some(data.clone()),
    })
}
