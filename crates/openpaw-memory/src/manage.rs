//! `manage` — the unified memory + skill management entry.
//!
//! A single tool surface that routes by `domain`:
//! - `domain: "memory"` → [`crate::memory_tool::run`] (set / get / remove /
//!   review / search / list / list_categories / list_pinned / list_by_task /
//!   list_by_goal)
//! - `domain: "skill"` → [`crate::skills::tool::run`] (list / get / create /
//!   update / archive / restore / delete / merge / reactivate / mark_stale /
//!   pin / unpin)
//!
//! This module is an additive wrapper: each half keeps its own action
//! semantics, its own write-action set, and its fail-closed approval gate,
//! and both standalone entries (`memory_tool::run`, `skills::tool::run`)
//! remain usable. `manage` only dispatches on `domain` and forwards the
//! input untouched.

use std::path::Path;

use openpaw_core::{ApprovalManager, Result, ToolCallOutcome, ToolSpec};

use crate::memory_tool;
use crate::skills::tool as skill_tool;

/// Tool specification for the unified `manage` tool.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "manage".into(),
        description: "Unified management of persistent memories and skills. \
                      Set domain to \"memory\" for memory actions (set, get, remove, review, \
                      search, list, list_categories, list_pinned, list_by_task, list_by_goal) or \
                      to \"skill\" for skill actions (list, get, create, update, archive, \
                      restore, delete, merge, reactivate, mark_stale, pin, unpin). \
                      Actions that write to disk require approval and fail closed when no \
                      approval manager is configured."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "domain": {
                    "type": "string",
                    "enum": ["memory", "skill"],
                    "description": "Which subsystem to manage: persistent memories or skills"
                },
                "action": {
                    "type": "string",
                    "description": "Memory actions: set|get|remove|review|search|list|list_categories|list_pinned|list_by_task|list_by_goal. \
                                     Skill actions: list|get|create|update|archive|restore|delete|merge|reactivate|mark_stale|pin|unpin"
                },

                "id": {
                    "type": "string",
                    "description": "Memory id (required for memory get/remove/review; optional for set to update in place)"
                },
                "content": {
                    "type": "string",
                    "description": "Memory body content (required for memory set)"
                },
                "zone": {
                    "type": "string",
                    "description": "Memory zone / category (for memory set/list; default 'general')"
                },
                "confidence": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "description": "Memory confidence (for memory set; default medium)"
                },
                "pinned": {
                    "type": "boolean",
                    "description": "Load on every session (for memory set)"
                },
                "memory_type": {
                    "type": "string",
                    "description": "Memory classification: fact|procedure|preference|episode|context|insight|reasoning|goal|intention (for memory set)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Memory tags (for memory set)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (required for memory search)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results for memory search/list (default 10, max 100)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Kanban task id (required for memory list_by_task)"
                },
                "goal_id": {
                    "type": "string",
                    "description": "Goal id (required for memory list_by_goal)"
                },
                "quality": {
                    "type": "integer",
                    "description": "SM-2 recall quality 0-5 (for memory review; default 4)"
                },

                "name": {
                    "type": "string",
                    "description": "Skill name (required for all skill actions except 'list' and 'merge')"
                },
                "description": {
                    "type": "string",
                    "description": "Description (for skill create/update)"
                },
                "triggers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Trigger keywords for the skill (for skill create/update)"
                },
                "body": {
                    "type": "string",
                    "description": "Skill body content (for skill create/update/merge)"
                },
                "version": {
                    "type": "string",
                    "description": "Skill version (for skill update)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["user", "project"],
                    "description": "Scope to operate in (default: user)"
                },
                "category": {
                    "type": "string",
                    "description": "Status filter for skill list: active/archived/stale/merged/reactivated"
                },
                "source_names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Source skill names for skill merge (min 2)"
                },
                "merged_name": {
                    "type": "string",
                    "description": "Name for the merged skill (skill merge)"
                },
                "when_to_use": {
                    "type": "string",
                    "description": "AI matching hint — when the skill should be invoked (for skill create/update)"
                },
                "argument_hint": {
                    "type": "string",
                    "description": "Argument hint shown in skill invocation description (for skill create/update)"
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool whitelist for skill execution (for skill create/update)"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Conditional activation file path patterns (glob, for skill create/update)"
                },
                "context": {
                    "type": "string",
                    "enum": ["inline", "fork"],
                    "description": "Execution mode: inline (default) or fork (for skill update)"
                }
            },
            "required": ["domain", "action"]
        }),
        ..Default::default()
    }
}

/// Dispatch a `manage` call by `domain`. The input is forwarded verbatim to
/// the routed half, so parameter names and action semantics are unchanged.
pub async fn run(
    workspace: &Path,
    input: serde_json::Value,
    approval_manager: Option<&dyn ApprovalManager>,
) -> Result<ToolCallOutcome> {
    match input["domain"].as_str().unwrap_or("") {
        "memory" => memory_tool::run(workspace, input, approval_manager).await,
        "skill" => skill_tool::run(workspace, input, approval_manager).await,
        "" => Ok(ToolCallOutcome::error(
            "Error: domain parameter is required (expected \"memory\" or \"skill\")",
        )),
        other => Ok(ToolCallOutcome::error(format!(
            "Error: unknown domain '{other}' (expected \"memory\" or \"skill\")"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::spec;

    #[test]
    fn spec_requires_domain_and_action() {
        let s = spec();
        assert_eq!(s.name, "manage");
        assert_eq!(
            s.input_schema["required"],
            serde_json::json!(["domain", "action"])
        );
        assert_eq!(
            s.input_schema["properties"]["domain"]["enum"],
            serde_json::json!(["memory", "skill"])
        );
    }
}
