//! Memory-side tool entry — create / look up / list / search / remove and
//! review persistent memories through a single `run` dispatch.
//!
//! Conventions (matching the skills tool entry in `skills/tool.rs`):
//! - Business failures are reported as `Ok(ToolCallOutcome::error(..))`,
//!   never as `Err`; `Err` is reserved for unrecoverable harness faults.
//! - Actions that write to disk pass through the `openpaw_core::
//!   ApprovalManager` gate and fail closed when no manager is configured.
//! - Store roots follow the `openpaw memory` CLI domain rules: user scope
//!   lives under `<home>/memories` (`OPENPAW_HOME` or `~/.openpaw`), project
//!   scope under `<workspace>/.openpaw/memories`.

use std::path::Path;

use openpaw_core::{ApprovalManager, ApprovalRequest, Result, ToolCallOutcome};

use crate::memory::{Confidence, MemoryFrontmatter, MemoryType, Scope, Source};
use crate::store::{FsMemoryStore, MemoryStore};

/// Validate a zone name: non-empty, no path separators, no traversal.
fn validate_zone(zone: &str) -> std::result::Result<(), String> {
    if zone.is_empty()
        || zone.contains('/')
        || zone.contains('\\')
        || zone.contains("..")
        || !zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("invalid zone name: {zone:?}"));
    }
    Ok(())
}

/// Memory actions that persist data to disk: they must pass the approval
/// gate before touching the store. Read-only actions are unaffected.
/// (`review` rewrites the SM-2 schedule fields, hence counts as a write.)
const WRITE_ACTIONS: [&str; 3] = ["set", "remove", "review"];

fn is_write_action(action: &str) -> bool {
    WRITE_ACTIONS.contains(&action)
}

/// user scope root: `<home>/memories` (OPENPAW_HOME or ~/.openpaw).
fn user_memories_root() -> std::path::PathBuf {
    openpaw_core::openpaw_home().join("memories")
}

/// project scope root: `<workspace>/.openpaw/memories`.
fn project_memories_root(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".openpaw").join("memories")
}

/// Build the store with both scope roots. Writing creates directories on
/// demand (atomic doc writes), and listing tolerates missing directories.
fn build_store(workspace: &Path) -> FsMemoryStore {
    FsMemoryStore::new(user_memories_root(), Some(project_memories_root(workspace)))
}

fn parse_scope(input: &serde_json::Value) -> Scope {
    match input["scope"].as_str().unwrap_or("user") {
        "project" => Scope::Project,
        _ => Scope::User,
    }
}

fn parse_confidence(input: &serde_json::Value) -> Confidence {
    match input["confidence"].as_str().unwrap_or("medium") {
        "low" => Confidence::Low,
        "high" => Confidence::High,
        _ => Confidence::Medium,
    }
}

fn parse_memory_type(input: &serde_json::Value) -> Option<MemoryType> {
    input["memory_type"]
        .as_str()
        .and_then(MemoryType::from_str_lossy)
}

fn parse_tags(input: &serde_json::Value) -> Vec<String> {
    input["tags"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn run(
    workspace: &Path,
    input: serde_json::Value,
    approval_manager: Option<&dyn ApprovalManager>,
) -> Result<ToolCallOutcome> {
    let action = input["action"].as_str().unwrap_or("").to_string();
    if action.is_empty() {
        return Ok(ToolCallOutcome::error(
            "Error: action parameter is required",
        ));
    }

    let store = build_store(workspace);

    // Memory writes require an explicit approval decision. Fail-closed:
    // no approval manager configured → deny without touching the store.
    if is_write_action(&action) {
        let target = input["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| input["zone"].as_str().unwrap_or("<unnamed>").to_string());
        let reason =
            format!("memory action '{action}' writes memory data to disk (target: {target})");
        let Some(mgr) = approval_manager else {
            return Ok(ToolCallOutcome::error(format!(
                "Error: memory '{action}' requires approval but no approval manager is configured (fail-closed). {reason}"
            )));
        };
        let resp = mgr
            .request(ApprovalRequest::new(
                "memory_save",
                input.clone(),
                reason.clone(),
            ))
            .await;
        if !resp.is_approved() {
            return Ok(ToolCallOutcome::error(format!(
                "Memory write denied: {reason}"
            )));
        }
    }

    match action.as_str() {
        "search" => Ok(search_memories(&store, &input)),
        "get" => Ok(get_memory(&store, &input)),
        "set" => Ok(set_memory(&store, &input)),
        "remove" => Ok(remove_memory(&store, &input)),
        "review" => Ok(review_memory(&store, &input)),
        "list_categories" => Ok(list_categories(&store)),
        "list" => Ok(list_memories(&store, &input)),
        "list_pinned" => Ok(list_pinned_memories(&store)),
        "list_by_task" => Ok(list_by_task(&store, &input)),
        "list_by_goal" => Ok(list_by_goal(&store, &input)),
        _ => Ok(ToolCallOutcome::error(format!(
            "Error: unknown action '{action}'"
        ))),
    }
}

fn search_memories(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let query = input["query"].as_str().unwrap_or("");
    if query.is_empty() {
        return ToolCallOutcome::error("Error: query parameter is required for search action");
    }

    let limit = input["limit"].as_u64().unwrap_or(10).min(100) as usize;

    match store.search_memories(query, limit) {
        Ok(results) => {
            if results.is_empty() {
                return ToolCallOutcome::ok(format!("No memories found for query: \"{query}\""));
            }

            let mut content = format!("Found {} memory(ies) for \"{query}\":\n", results.len());
            for (i, (m, score)) in results.iter().enumerate() {
                content.push_str(&format!(
                    "\n{}. [{}] zone: {} | confidence: {:?} | score: {:.2}\n   {}",
                    i + 1,
                    m.id(),
                    m.frontmatter.zone,
                    m.frontmatter.confidence,
                    score,
                    m.body.chars().take(200).collect::<String>(),
                ));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error searching memories: {e}")),
    }
}

fn get_memory(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let id = input["id"].as_str().unwrap_or("");
    if id.is_empty() {
        return ToolCallOutcome::error("Error: id parameter is required for get action");
    }

    match store.get(id) {
        Ok(Some(m)) => {
            let mut content = format!(
                "Memory: {}\nZone: {}\nScope: {:?}\nConfidence: {:?}\nSource: {:?}\nPinned: {}\nCreated: {}\n",
                m.id(),
                m.frontmatter.zone,
                m.scope,
                m.frontmatter.confidence,
                m.frontmatter.source,
                m.frontmatter.pinned,
                m.frontmatter.created,
            );
            if !m.frontmatter.tags.is_empty() {
                content.push_str(&format!("Tags: {}\n", m.frontmatter.tags.join(", ")));
            }
            if !m.frontmatter.supersedes.is_empty() {
                content.push_str(&format!(
                    "Supersedes: {}\n",
                    m.frontmatter.supersedes.join(", ")
                ));
            }
            content.push_str(&format!("\n--- Content ---\n{}", m.body));

            ToolCallOutcome::ok(content)
        }
        Ok(None) => ToolCallOutcome::ok(format!("Memory '{id}' not found.")),
        Err(e) => ToolCallOutcome::error(format!("Error looking up memory '{id}': {e}")),
    }
}

fn set_memory(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let content = input["content"].as_str().unwrap_or("");
    if content.is_empty() {
        return ToolCallOutcome::error("Error: content parameter is required for set action");
    }

    let scope = parse_scope(input);
    let zone = input["zone"].as_str().unwrap_or("general");
    if let Err(e) = validate_zone(zone) {
        return ToolCallOutcome::error(format!("Error: {e}"));
    }
    let confidence = parse_confidence(input);
    let pinned = input["pinned"].as_bool().unwrap_or(false);
    let explicit_description = input["description"].as_str().unwrap_or("");
    let memory_type = parse_memory_type(input);
    let tags = parse_tags(input);

    let existing_id = input["id"].as_str().unwrap_or("");

    // If an id is provided and the entry exists, update it in-place.
    if !existing_id.is_empty() {
        match store.get(existing_id) {
            Ok(Some(existing)) => {
                let mut fm = existing.frontmatter.clone();
                fm.zone = zone.to_string();
                fm.confidence = confidence;
                fm.pinned = pinned;
                if !tags.is_empty() {
                    fm.tags = tags;
                }
                if let Some(mt) = memory_type {
                    fm.memory_type = mt;
                }
                // Explicit description wins; otherwise re-derive when the
                // body changed; otherwise keep the existing description.
                if !explicit_description.is_empty() {
                    fm.description = explicit_description.to_string();
                } else if content != existing.body {
                    fm.description = MemoryFrontmatter::derive_description(content);
                }
                // Atomic in-place update.
                match store.update_in_place(existing_id, &fm, content) {
                    Ok(()) => ToolCallOutcome::ok(format!(
                        "Memory '{existing_id}' updated at {}",
                        existing.source_path.display()
                    )),
                    Err(e) => ToolCallOutcome::error(format!(
                        "Error updating memory '{existing_id}': {e}"
                    )),
                }
            }
            Ok(None) => ToolCallOutcome::error(format!(
                "Error: memory '{existing_id}' not found for update. Use action 'set' without an 'id' to create a new memory."
            )),
            Err(e) => ToolCallOutcome::error(format!(
                "Error looking up memory '{existing_id}': {e}"
            )),
        }
    } else {
        // Create a new memory. Agent-written memories are recorded as
        // Source::Agent — never masquerade as User (human-authored).
        let mut fm = MemoryFrontmatter::new(Source::Agent, confidence, tags);
        fm.zone = zone.to_string();
        fm.pinned = pinned;
        fm.description = if explicit_description.is_empty() {
            MemoryFrontmatter::derive_description(content)
        } else {
            explicit_description.to_string()
        };
        if let Some(mt) = memory_type {
            fm.memory_type = mt;
        }
        let new_id = fm.id.clone();

        match store.put(scope, fm, content) {
            Ok(path) => ToolCallOutcome::ok(format!(
                "Memory '{}' created in zone '{}' at {}",
                new_id,
                zone,
                path.display()
            )),
            Err(e) => ToolCallOutcome::error(format!("Error creating memory: {e}")),
        }
    }
}

fn remove_memory(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let id = input["id"].as_str().unwrap_or("");
    if id.is_empty() {
        return ToolCallOutcome::error("Error: id parameter is required for remove action");
    }

    // Find the memory first to determine its scope.
    match store.get(id) {
        Ok(Some(m)) => {
            let scope = m.scope;
            match store.delete(scope, id) {
                Ok(true) => ToolCallOutcome::ok(format!("Memory '{id}' deleted successfully.")),
                Ok(false) => ToolCallOutcome::error(format!("Memory '{id}' could not be deleted.")),
                Err(e) => ToolCallOutcome::error(format!("Error deleting memory '{id}': {e}")),
            }
        }
        Ok(None) => ToolCallOutcome::ok(format!("Memory '{id}' not found.")),
        Err(e) => ToolCallOutcome::error(format!("Error looking up memory '{id}': {e}")),
    }
}

/// Apply an SM-2 review to an existing memory (quality 0..=5).
fn review_memory(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let id = input["id"].as_str().unwrap_or("");
    if id.is_empty() {
        return ToolCallOutcome::error("Error: id parameter is required for review action");
    }
    let quality = input["quality"].as_u64().unwrap_or(4).min(5) as u8;

    match store.review_memory(id, quality) {
        Ok(()) => {
            let next = store
                .get(id)
                .ok()
                .flatten()
                .and_then(|m| m.frontmatter.sm2_next_review)
                .unwrap_or_default();
            ToolCallOutcome::ok(format!(
                "Memory '{id}' reviewed with quality {quality}. Next review: {next}"
            ))
        }
        Err(e) => ToolCallOutcome::error(format!("Error reviewing memory '{id}': {e}")),
    }
}

fn list_categories(store: &FsMemoryStore) -> ToolCallOutcome {
    match store.list_active() {
        Ok(active) => {
            let mut zones: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for m in &active {
                *zones.entry(m.frontmatter.zone.clone()).or_insert(0) += 1;
            }

            if zones.is_empty() {
                return ToolCallOutcome::ok("No memory zones found.");
            }

            let mut content = format!("Memory zones ({} total memories):\n", active.len());
            let mut sorted: Vec<_> = zones.into_iter().collect();
            sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
            for (zone, count) in sorted {
                content.push_str(&format!("  - {zone} ({count} entries)\n"));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error listing categories: {e}")),
    }
}

fn list_memories(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let limit = input["limit"].as_u64().unwrap_or(10).min(100) as usize;
    let zone_filter = input["zone"].as_str().unwrap_or("");

    let memories = if zone_filter.is_empty() {
        store.list_active()
    } else {
        store.list_by_zone(zone_filter)
    };

    match memories {
        Ok(memories) => {
            if memories.is_empty() {
                return ToolCallOutcome::ok(if zone_filter.is_empty() {
                    "No memories found.".to_string()
                } else {
                    format!("No memories found in zone '{zone_filter}'.")
                });
            }

            let total = memories.len();
            let shown = memories.len().min(limit);
            let mut content = format!("Found {total} memory(ies), showing {shown}:\n");
            for (i, m) in memories.iter().take(limit).enumerate() {
                content.push_str(&format!(
                    "\n{}. [{}] zone: {} | confidence: {:?} | scope: {:?} | pinned: {}",
                    i + 1,
                    m.id(),
                    m.frontmatter.zone,
                    m.frontmatter.confidence,
                    m.scope,
                    m.frontmatter.pinned,
                ));
                if !m.frontmatter.tags.is_empty() {
                    content.push_str(&format!("\n   Tags: {}", m.frontmatter.tags.join(", ")));
                }
                content.push_str(&format!(
                    "\n   {}",
                    m.body.chars().take(100).collect::<String>()
                ));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error listing memories: {e}")),
    }
}

fn list_pinned_memories(store: &FsMemoryStore) -> ToolCallOutcome {
    match store.list_pinned() {
        Ok(pinned) => {
            if pinned.is_empty() {
                return ToolCallOutcome::ok("No pinned memories found.");
            }

            let mut content = format!("Found {} pinned memory(ies):\n", pinned.len());
            for (i, m) in pinned.iter().enumerate() {
                content.push_str(&format!(
                    "\n{}. [{}] zone: {} | {}",
                    i + 1,
                    m.id(),
                    m.frontmatter.zone,
                    m.body.chars().take(150).collect::<String>(),
                ));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error listing pinned memories: {e}")),
    }
}

/// List memories linked to a kanban task id.
fn list_by_task(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let task_id = input["task_id"].as_str().unwrap_or("");
    if task_id.is_empty() {
        return ToolCallOutcome::error(
            "Error: task_id parameter is required for list_by_task action",
        );
    }

    match store.list_by_task_id(task_id) {
        Ok(memories) => {
            if memories.is_empty() {
                return ToolCallOutcome::ok(format!("No memories found for task \"{task_id}\""));
            }

            let mut content = format!(
                "Found {} memory(ies) for task \"{task_id}\":\n",
                memories.len()
            );
            for (i, m) in memories.iter().enumerate() {
                content.push_str(&format!(
                    "\n{}. [{}] zone: {} | confidence: {:?} | {}\n   {}",
                    i + 1,
                    m.id(),
                    m.frontmatter.zone,
                    m.frontmatter.confidence,
                    m.frontmatter
                        .description
                        .chars()
                        .take(100)
                        .collect::<String>(),
                    m.body.chars().take(200).collect::<String>(),
                ));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error listing memories by task: {e}")),
    }
}

/// List memories linked to a goal id.
fn list_by_goal(store: &FsMemoryStore, input: &serde_json::Value) -> ToolCallOutcome {
    let goal_id = input["goal_id"].as_str().unwrap_or("");
    if goal_id.is_empty() {
        return ToolCallOutcome::error(
            "Error: goal_id parameter is required for list_by_goal action",
        );
    }

    match store.list_by_goal_id(goal_id) {
        Ok(memories) => {
            if memories.is_empty() {
                return ToolCallOutcome::ok(format!("No memories found for goal_id \"{goal_id}\""));
            }

            let mut content = format!(
                "Found {} memory(ies) for goal_id \"{goal_id}\":\n",
                memories.len()
            );
            for (i, m) in memories.iter().enumerate() {
                content.push_str(&format!(
                    "\n{}. [{}] zone: {} | confidence: {:?} | type: {:?}\n   {}",
                    i + 1,
                    m.id(),
                    m.frontmatter.zone,
                    m.frontmatter.confidence,
                    m.frontmatter.memory_type,
                    m.frontmatter
                        .description
                        .chars()
                        .take(100)
                        .collect::<String>(),
                ));
            }

            ToolCallOutcome::ok(content)
        }
        Err(e) => ToolCallOutcome::error(format!("Error listing memories by goal_id: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_zone;

    #[test]
    fn accepts_valid_zone_names() {
        assert!(validate_zone("abc123").is_ok());
        assert!(validate_zone("my-zone").is_ok());
        assert!(validate_zone("zone_1").is_ok());
        assert!(validate_zone("A-B-C").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_zone("").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(validate_zone("a/b").is_err());
        assert!(validate_zone("a\\b").is_err());
    }

    #[test]
    fn rejects_traversal() {
        assert!(validate_zone("..").is_err());
        assert!(validate_zone("foo/../bar").is_err());
    }

    #[test]
    fn rejects_special_characters() {
        assert!(validate_zone("a.b").is_err());
        assert!(validate_zone("a b").is_err());
        assert!(validate_zone("a:b").is_err());
    }
}
