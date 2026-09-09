//! `think` — scratchpad reasoning pause. The thought is recorded in the
//! transcript via the tool call itself; the tool returns confirmation only.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Deserialize)]
struct Args {
    thought: String,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "think".into(),
        description: "Use this tool to think through a problem step by step without taking action. It records your reasoning and returns immediately — plan here before acting on complex tasks.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "thought": {"type": "string", "description": "Your reasoning, in full sentences"}
            },
            "required": ["thought"]
        }),
        category: Some(ToolCategory::ReadOnly),
        concurrency: Some(ToolConcurrency::NeverParallel),
        is_destructive: Some(false),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("think: bad args: {e}"))),
    };
    let words = a.thought.split_whitespace().count();
    Ok(ToolCallOutcome::ok(format!(
        "thought recorded ({words} words)"
    )))
}
