//! `edit_file` — exact-string replacement with uniqueness enforcement,
//! CRLF normalization, and a `.bak` safety copy (same-source semantics).

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "edit_file".into(),
        description: "Replace an exact string in a file. The old_string must match exactly once unless replace_all is set. Read the file first to find the exact text.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to workspace"},
                "old_string": {"type": "string", "description": "Exact text to replace"},
                "new_string": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)"}
            },
            "required": ["path", "old_string", "new_string"]
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
        Err(e) => return Ok(ToolCallOutcome::error(format!("edit: bad args: {e}"))),
    };
    let path = match crate::safety::resolve(workspace, &a.path) {
        Ok(p) => p,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    if a.old_string.is_empty() {
        return Ok(ToolCallOutcome::error(
            "edit failed: old_string must not be empty",
        ));
    }

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "edit read {}: {e}",
                path.display()
            )))
        }
    };

    // Normalize CRLF → LF before matching so LF patches match CRLF files;
    // the original line-ending style is restored before writing back.
    let had_crlf = content.contains("\r\n");
    let content_lf = content.replace("\r\n", "\n");
    let old_lf = a.old_string.replace("\r\n", "\n");
    let new_lf = a.new_string.replace("\r\n", "\n");

    let count = content_lf.matches(&old_lf).count();
    if count == 0 {
        let preview: String = a.old_string.chars().take(50).collect();
        return Ok(ToolCallOutcome::error(format!(
            "edit failed: old_string not found in file. Searched for: {preview:?}"
        )));
    }
    if !a.replace_all && count > 1 {
        return Ok(ToolCallOutcome::error(format!(
            "edit failed: old_string found {count} times in {} (must be unique or set replace_all=true)",
            a.path
        )));
    }

    let new_content_lf = if a.replace_all {
        content_lf.replace(&old_lf, &new_lf)
    } else {
        content_lf.replacen(&old_lf, &new_lf, 1)
    };

    let new_content = if had_crlf {
        new_content_lf.replace('\n', "\r\n")
    } else {
        new_content_lf
    };

    // Backup before writing — a failed backup aborts the edit so the
    // original file is never overwritten without a safety net.
    let backup = format!("{}.bak", path.display());
    if let Err(e) = tokio::fs::copy(&path, &backup).await {
        return Ok(ToolCallOutcome::error(format!(
            "edit backup {}: {e}",
            path.display()
        )));
    }

    if let Err(e) = tokio::fs::write(&path, &new_content).await {
        return Ok(ToolCallOutcome::error(format!(
            "edit write {}: {e}",
            path.display()
        )));
    }
    let _ = tokio::fs::remove_file(&backup).await;

    Ok(ToolCallOutcome::ok(format!(
        "edited {} ({} replacement{})",
        a.path,
        if a.replace_all { count } else { 1 },
        if a.replace_all && count > 1 { "s" } else { "" }
    )))
}
