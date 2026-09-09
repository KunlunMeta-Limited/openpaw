//! `enter_plan_mode` / `exit_plan_mode` — plan-mode transitions.
//!
//! E2 scope: the tools confirm the transition in their results; durable
//! mode state (gating write tools) lands with the engine agent-state work.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolSpec};
use serde_json::json;

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "enter_plan_mode".into(),
            description: "Switch to plan mode: research and design without making changes. Present the plan when done, then call exit_plan_mode.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        },
        ToolSpec {
            name: "exit_plan_mode".into(),
            description: "Leave plan mode and return to execution. Call this after presenting your plan.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "enter_plan_mode" | "exit_plan_mode")
}

pub async fn run(_workspace: &Path, name: &str) -> Result<ToolCallOutcome> {
    match name {
        "enter_plan_mode" => Ok(ToolCallOutcome::ok(
            "plan mode entered: research and design only, no file changes",
        )),
        "exit_plan_mode" => Ok(ToolCallOutcome::ok("plan mode exited: execution resumed")),
        other => Err(openpaw_core::Error::ToolHost(format!(
            "unknown plan tool: {other}"
        ))),
    }
}
