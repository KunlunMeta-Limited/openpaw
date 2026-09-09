//! `skill_manage` 工具入口 — 技能的创建/查询/列表与状态迁移操作面。
//!
//! 精简移植口径：保留母仓与 hub、遥测深度耦合无关的核心工具面
//! （list/get/create/update/archive/restore/delete/merge/reactivate/
//! mark_stale/pin/unpin）；依赖 gap_record 模块的 `gaps` 动作与全部
//! hub 相关工具面剥离。写操作沿用母仓的 fail-closed 许可门，经
//! `openpaw_core::ApprovalManager` 裁决；业务失败一律返回
//! `Ok(ToolCallOutcome::error(..))`（与 openpaw-tools 的失败口径一致）。

use std::path::Path;

use openpaw_core::{ApprovalManager, ApprovalRequest, ToolCallOutcome, ToolSpec};

use crate::skills::{
    FsSkillStore, FsTelemetryStore, Scope, SkillContext, SkillFrontmatter, SkillLifecycle,
    SkillStatus, SkillStore,
};

/// 写技能数据的动作：必须先过许可门再落盘。只读动作不受影响。
const WRITE_ACTIONS: [&str; 4] = ["create", "update", "delete", "merge"];

fn is_write_action(action: &str) -> bool {
    WRITE_ACTIONS.contains(&action)
}

/// user 作用域根：`~/.openpaw/skills`（受 OPENPAW_HOME 环境变量覆盖）。
fn user_skills_root() -> std::path::PathBuf {
    openpaw_core::openpaw_home().join("skills")
}

/// project 作用域根：`<workspace>/.openpaw/skills`（目录存在时启用）。
fn project_skills_root(workspace: &Path) -> Option<std::path::PathBuf> {
    let p = workspace.join(".openpaw").join("skills");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

fn build_store(workspace: &Path) -> FsSkillStore {
    FsSkillStore::new(user_skills_root(), project_skills_root(workspace))
}

/// Tool specification for the skill_manage tool.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "skill_manage".into(),
        description: "Manage skills: list, get, create, update, archive, restore, delete, merge, reactivate, mark_stale, pin, unpin. \
                       Skills are reusable instruction sets that can be activated based on context."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "update", "archive", "restore", "delete", "merge", "reactivate", "mark_stale", "pin", "unpin"],
                    "description": "The skill management action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Skill name (required for all actions except 'list' and 'merge')"
                },
                "description": {
                    "type": "string",
                    "description": "Skill description (for create/update)"
                },
                "triggers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Trigger keywords for the skill (for create/update)"
                },
                "body": {
                    "type": "string",
                    "description": "Skill body content (for create/update/merge)"
                },
                "version": {
                    "type": "string",
                    "description": "Skill version (for update)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["user", "project"],
                    "description": "Scope to operate in (default: user)"
                },
                "category": {
                    "type": "string",
                    "description": "Status filter for list action: active/archived/stale/merged/reactivated"
                },
                "source_names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Source skill names for merge action (min 2)"
                },
                "merged_name": {
                    "type": "string",
                    "description": "Name for the merged skill (merge action)"
                },
                "when_to_use": {
                    "type": "string",
                    "description": "AI matching hint — when the skill should be invoked (for create/update)"
                },
                "argument_hint": {
                    "type": "string",
                    "description": "Argument hint shown in skill invocation description (for create/update)"
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool whitelist for skill execution (for create/update)"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Conditional activation file path patterns (glob, for create/update)"
                },
                "context": {
                    "type": "string",
                    "enum": ["inline", "fork"],
                    "description": "Execution mode: inline (default) or fork (for update)"
                }
            },
            "required": ["action"]
        }),
        ..Default::default()
    }
}

fn parse_scope(input: &serde_json::Value) -> Scope {
    match input["scope"].as_str().unwrap_or("user") {
        "project" => Scope::Project,
        _ => Scope::User,
    }
}

/// 记录 agent 创建来源，便于 curator 区分 agent 产物与人工/内置技能。
/// 走裁剪版遥测的轻量持久化；失败静默（不影响主流程）。
fn mark_agent_created(name: &str) {
    if name.is_empty() {
        return;
    }
    let telemetry = FsTelemetryStore::new(user_skills_root());
    let _ = telemetry.mark_agent_created(name);
}

pub async fn run(
    workspace: &Path,
    input: serde_json::Value,
    approval_manager: Option<&dyn ApprovalManager>,
) -> openpaw_core::Result<ToolCallOutcome> {
    let action = input["action"].as_str().unwrap_or("").to_string();
    if action.is_empty() {
        return Ok(ToolCallOutcome::error(
            "Error: action parameter is required",
        ));
    }

    let store = build_store(workspace);

    // 写操作需要显式许可裁决。fail-closed：未配置许可管理器 → 拒绝。
    if is_write_action(&action) {
        let target = input["name"]
            .as_str()
            .or_else(|| input["merged_name"].as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        let reason =
            format!("skill_manage action '{action}' writes skill data to disk (target: {target})");
        let Some(mgr) = approval_manager else {
            return Ok(ToolCallOutcome::error(format!(
                "Error: skill_manage '{action}' requires approval but no approval manager is configured (fail-closed). {reason}"
            )));
        };
        let resp = mgr
            .request(ApprovalRequest::new(
                "skill_manage",
                input.clone(),
                reason.clone(),
            ))
            .await;
        if !resp.is_approved() {
            return Ok(ToolCallOutcome::error(format!(
                "Skill write denied: {reason}"
            )));
        }
    }

    match action.as_str() {
        "list" => Ok(list_skills(&store, &input)),
        "get" => Ok(get_skill(&store, &input)),
        "create" => {
            let out = create_skill(&store, &input);
            if !out.is_error {
                mark_agent_created(input["name"].as_str().unwrap_or(""));
            }
            Ok(out)
        }
        "update" => Ok(update_skill(&store, &input)),
        "archive" => Ok(transition_skill(
            &store,
            &input,
            "archive",
            "archived",
            |s, scope, name| s.archive(scope, name),
        )),
        "restore" => Ok(transition_skill(
            &store,
            &input,
            "restore",
            "restored",
            |s, scope, name| s.restore(scope, name),
        )),
        "delete" => Ok(delete_skill(&store, &input)),
        "merge" => {
            let out = merge_skills(&store, &input);
            if !out.is_error {
                mark_agent_created(input["merged_name"].as_str().unwrap_or(""));
            }
            Ok(out)
        }
        "reactivate" => Ok(transition_skill(
            &store,
            &input,
            "reactivate",
            "reactivated",
            |s, scope, name| s.reactivate(scope, name),
        )),
        "mark_stale" => Ok(transition_skill(
            &store,
            &input,
            "mark_stale",
            "marked as stale",
            |s, scope, name| s.mark_stale(scope, name),
        )),
        "pin" => Ok(set_pinned(&store, &input, true)),
        "unpin" => Ok(set_pinned(&store, &input, false)),
        _ => Ok(ToolCallOutcome::error(format!(
            "Error: unknown action '{action}'"
        ))),
    }
}

fn require_name(input: &serde_json::Value, action: &str) -> Result<String, ToolCallOutcome> {
    match input["name"].as_str() {
        Some(n) if !n.is_empty() => Ok(n.to_string()),
        _ => Err(ToolCallOutcome::error(format!(
            "Error: name parameter is required for {action} action"
        ))),
    }
}

fn list_skills(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let skills = match store.list() {
        Ok(s) => s,
        Err(e) => return ToolCallOutcome::error(format!("Error listing skills: {e}")),
    };
    let category_filter = input["category"].as_str().unwrap_or("");

    let filtered: Vec<_> = if category_filter.is_empty() {
        skills
    } else {
        let status_filter = match category_filter {
            "active" => SkillStatus::Active,
            "archived" => SkillStatus::Archived,
            "stale" => SkillStatus::Stale,
            "merged" => SkillStatus::Merged,
            "reactivated" => SkillStatus::Reactivated,
            _ => {
                return ToolCallOutcome::error(format!(
                    "Error: unknown category filter '{category_filter}'"
                ));
            }
        };
        skills
            .into_iter()
            .filter(|s| s.frontmatter.status == status_filter)
            .collect()
    };

    if filtered.is_empty() {
        return ToolCallOutcome::ok("No skills found.");
    }

    let mut content = format!("Found {} skill(s):\n", filtered.len());
    for (i, s) in filtered.iter().enumerate() {
        content.push_str(&format!(
            "\n{}. {} [{}] — {} (scope: {:?}, status: {:?})",
            i + 1,
            s.name(),
            s.frontmatter.version.as_deref().unwrap_or("n/a"),
            s.frontmatter.description,
            s.scope,
            s.frontmatter.status,
        ));
        if !s.frontmatter.triggers.is_empty() {
            content.push_str(&format!(
                "\n   Triggers: {}",
                s.frontmatter.triggers.join(", ")
            ));
        }
        if let Some(ref wtu) = s.frontmatter.when_to_use {
            content.push_str(&format!("\n   When to use: {}", wtu));
        }
        if !s.frontmatter.paths.is_empty() {
            content.push_str(&format!("\n   Paths: {}", s.frontmatter.paths.join(", ")));
        }
    }

    ToolCallOutcome::ok(content)
}

fn get_skill(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let name = match require_name(input, "get") {
        Ok(n) => n,
        Err(out) => return out,
    };

    match store.get(&name) {
        Ok(Some(s)) => {
            let mut content = format!(
                "Skill: {}\nDescription: {}\nVersion: {}\nStatus: {:?}\nScope: {:?}\n",
                s.name(),
                s.frontmatter.description,
                s.frontmatter.version.as_deref().unwrap_or("n/a"),
                s.frontmatter.status,
                s.scope,
            );
            if !s.frontmatter.triggers.is_empty() {
                content.push_str(&format!(
                    "Triggers: {}\n",
                    s.frontmatter.triggers.join(", ")
                ));
            }
            if let Some(ref parent) = s.frontmatter.parent {
                content.push_str(&format!("Parent: {parent}\n"));
            }
            if let Some(ref superseded) = s.frontmatter.superseded_by {
                content.push_str(&format!("Superseded by: {superseded}\n"));
            }
            if let Some(ref wtu) = s.frontmatter.when_to_use {
                content.push_str(&format!("When to use: {}\n", wtu));
            }
            if !s.frontmatter.paths.is_empty() {
                content.push_str(&format!("Paths: {}\n", s.frontmatter.paths.join(", ")));
            }
            content.push_str(&format!("\n--- Content ---\n{}", s.body));
            ToolCallOutcome::ok(content)
        }
        Ok(None) => ToolCallOutcome::ok(format!("Skill '{name}' not found.")),
        Err(e) => ToolCallOutcome::error(format!("Error looking up skill '{name}': {e}")),
    }
}

fn create_skill(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let name = match require_name(input, "create") {
        Ok(n) => n,
        Err(out) => return out,
    };

    let description = input["description"].as_str().unwrap_or("");
    if description.is_empty() {
        return ToolCallOutcome::error(
            "Error: description parameter is required for create action",
        );
    }

    let triggers: Vec<String> = input["triggers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let body = input["body"].as_str().unwrap_or("");
    let scope = parse_scope(input);

    // Check if skill already exists to avoid silent overwrite
    match store.get(&name) {
        Ok(Some(_)) => {
            return ToolCallOutcome::error(format!(
                "Error: skill '{name}' already exists. Use 'update' action to modify it."
            ));
        }
        Ok(None) => {}
        Err(e) => return ToolCallOutcome::error(format!("Error looking up skill '{name}': {e}")),
    }

    let fm = SkillFrontmatter {
        name: name.clone(),
        description: description.to_string(),
        triggers,
        version: Some("1.0.0".into()),
        status: SkillStatus::Active,
        when_to_use: input["when_to_use"].as_str().map(String::from),
        argument_hint: input["argument_hint"].as_str().map(String::from),
        allowed_tools: input["allowed_tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        paths: input["paths"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };

    match store.put(scope, fm, body) {
        Ok(path) => ToolCallOutcome::ok(format!(
            "Skill '{name}' created successfully at {}",
            path.display()
        )),
        Err(e) => ToolCallOutcome::error(format!("Error creating skill '{name}': {e}")),
    }
}

fn update_skill(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let name = match require_name(input, "update") {
        Ok(n) => n,
        Err(out) => return out,
    };

    match store.get(&name) {
        Ok(Some(mut existing)) => {
            if let Some(desc) = input["description"].as_str() {
                existing.frontmatter.description = desc.to_string();
            }
            if let Some(triggers) = input["triggers"].as_array() {
                existing.frontmatter.triggers = triggers
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
            if let Some(body) = input["body"].as_str() {
                existing.body = body.to_string();
            }
            if let Some(version) = input["version"].as_str() {
                existing.frontmatter.version = Some(version.to_string());
            }
            if let Some(wtu) = input["when_to_use"].as_str() {
                existing.frontmatter.when_to_use = Some(wtu.to_string());
            }
            if let Some(paths) = input["paths"].as_array() {
                existing.frontmatter.paths = paths
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
            if let Some(ctx) = input["context"].as_str() {
                existing.frontmatter.context = match ctx {
                    "fork" => Some(SkillContext::Fork),
                    "inline" => Some(SkillContext::Inline),
                    _ => None,
                };
            }

            // Use the skill's actual scope, not the caller-specified scope,
            // to avoid creating a duplicate in the wrong scope.
            match store.put(existing.scope, existing.frontmatter, &existing.body) {
                Ok(path) => ToolCallOutcome::ok(format!(
                    "Skill '{name}' updated successfully at {}",
                    path.display()
                )),
                Err(e) => ToolCallOutcome::error(format!("Error updating skill '{name}': {e}")),
            }
        }
        Ok(None) => ToolCallOutcome::ok(format!("Skill '{name}' not found.")),
        Err(e) => ToolCallOutcome::error(format!("Error looking up skill '{name}': {e}")),
    }
}

/// 状态迁移类动作（archive/restore/reactivate/mark_stale）的统一入口。
fn transition_skill(
    store: &FsSkillStore,
    input: &serde_json::Value,
    action: &str,
    past_tense: &str,
    transition: impl Fn(
        &FsSkillStore,
        Scope,
        &str,
    ) -> std::result::Result<(), crate::skills::SkillStoreError>,
) -> ToolCallOutcome {
    let name = match require_name(input, action) {
        Ok(n) => n,
        Err(out) => return out,
    };
    let scope = parse_scope(input);
    match transition(store, scope, &name) {
        Ok(()) => ToolCallOutcome::ok(format!("Skill '{name}' {past_tense} successfully.")),
        Err(e) => ToolCallOutcome::error(format!("Error: {action} failed for skill '{name}': {e}")),
    }
}

fn delete_skill(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let name = match require_name(input, "delete") {
        Ok(n) => n,
        Err(out) => return out,
    };
    let scope = parse_scope(input);
    match store.delete(scope, &name) {
        Ok(true) => ToolCallOutcome::ok(format!("Skill '{name}' deleted successfully.")),
        Ok(false) => ToolCallOutcome::ok(format!("Skill '{name}' not found in {scope:?} scope.")),
        Err(e) => ToolCallOutcome::error(format!("Error deleting skill '{name}': {e}")),
    }
}

fn merge_skills(store: &FsSkillStore, input: &serde_json::Value) -> ToolCallOutcome {
    let source_names: Vec<String> = input["source_names"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if source_names.len() < 2 {
        return ToolCallOutcome::error(
            "Error: at least 2 source_names are required for merge action",
        );
    }

    let merged_name = input["merged_name"].as_str().unwrap_or("");
    if merged_name.is_empty() {
        return ToolCallOutcome::error("Error: merged_name parameter is required for merge action");
    }

    let merged_body = input["body"].as_str().unwrap_or("");
    let scope = parse_scope(input);
    match store.merge(scope, &source_names, merged_name, merged_body) {
        Ok(path) => ToolCallOutcome::ok(format!(
            "Skills {} merged into '{merged_name}' at {}",
            source_names.join(", "),
            path.display()
        )),
        Err(e) => ToolCallOutcome::error(format!("Error merging skills: {e}")),
    }
}

fn set_pinned(store: &FsSkillStore, input: &serde_json::Value, pinned: bool) -> ToolCallOutcome {
    let action = if pinned { "pin" } else { "unpin" };
    let name = match require_name(input, action) {
        Ok(n) => n,
        Err(out) => return out,
    };

    match store.get(&name) {
        Ok(Some(mut skill)) => {
            skill.frontmatter.pinned = pinned;
            match store.put(skill.scope, skill.frontmatter, &skill.body) {
                Ok(_) => ToolCallOutcome::ok(format!("Skill '{name}' {action}ned successfully.")),
                Err(e) => ToolCallOutcome::error(format!("Error {action}ning skill '{name}': {e}")),
            }
        }
        Ok(None) => ToolCallOutcome::ok(format!("Skill '{name}' not found.")),
        Err(e) => ToolCallOutcome::error(format!("Error looking up skill '{name}': {e}")),
    }
}
