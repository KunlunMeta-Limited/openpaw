//! `write_file` — write (or create) a file, creating parent directories.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "write_file".into(),
        description: "Write content to a file, creating it (and parent directories) if needed. Prefer edit_file for modifying existing files.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to workspace"},
                "content": {"type": "string", "description": "Full file content to write"}
            },
            "required": ["path", "content"]
        }),
        category: Some(ToolCategory::Write),
        concurrency: Some(ToolConcurrency::PathScoped),
        is_destructive: Some(true),
        ..Default::default()
    }
}

pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("write: bad args: {e}"))),
    };
    let path = match crate::safety::resolve(workspace, &a.path) {
        Ok(p) => p,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Ok(ToolCallOutcome::error(format!(
                "write mkdir {}: {e}",
                parent.display()
            )));
        }
    }
    if let Err(e) = tokio::fs::write(&path, &a.content).await {
        return Ok(ToolCallOutcome::error(format!(
            "write {}: {e}",
            path.display()
        )));
    }

    Ok(ToolCallOutcome::ok(format!(
        "wrote {} bytes to {}",
        a.content.len(),
        a.path
    )))
}
