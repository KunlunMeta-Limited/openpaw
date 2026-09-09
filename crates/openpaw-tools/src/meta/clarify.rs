//! `clarify` — ask the user a clarifying question and wait for their
//! response (same-source semantics; the pause itself is enforced by the
//! NeverParallel classification and the UI layer).

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde_json::json;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "clarify".into(),
        description: "Ask the user a clarifying question when the task is ambiguous. The agent will pause and wait for the user's response before continuing.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question to ask the user"},
                "options": {"type": "array", "items": {"type": "string"}, "description": "Optional suggested answers"},
                "context": {"type": "string", "description": "Brief context explaining why clarification is needed"}
            },
            "required": ["question"]
        }),
        category: Some(ToolCategory::Communication),
        concurrency: Some(ToolConcurrency::NeverParallel),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, input: serde_json::Value) -> Result<ToolCallOutcome> {
    let question = input["question"].as_str().unwrap_or("");
    if question.is_empty() {
        return Ok(ToolCallOutcome::error(
            "Error: question parameter is required",
        ));
    }

    let mut content = format!("Clarification needed: {question}");

    if let Some(options) = input["options"].as_array() {
        let opts: Vec<&str> = options.iter().filter_map(|v| v.as_str()).collect();
        if !opts.is_empty() {
            content.push_str("\n\nOptions:");
            for (i, opt) in opts.iter().enumerate() {
                content.push_str(&format!("\n  {}. {opt}", i + 1));
            }
        }
    }

    if let Some(ctx) = input["context"].as_str() {
        if !ctx.is_empty() {
            content.push_str(&format!("\n\nContext: {ctx}"));
        }
    }

    content.push_str("\n\n[Waiting for user response...]");

    Ok(ToolCallOutcome::ok(content))
}
