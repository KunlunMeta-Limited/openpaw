//! `glob` — file pattern search over the workspace.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

const MAX_RESULTS: usize = 500;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "glob".into(),
        description: "Find files matching a glob pattern (e.g. `src/**/*.rs`). Returns relative paths, capped at 500.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern, e.g. **/*.rs or src/*.md"},
                "path": {"type": "string", "description": "Directory to search (default: workspace root)"}
            },
            "required": ["pattern"]
        }),
        category: Some(ToolCategory::ReadOnly),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
}

pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("glob: bad args: {e}"))),
    };
    let root = match &a.path {
        Some(p) => match crate::safety::resolve(workspace, p) {
            Ok(r) => r,
            Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
        },
        None => match crate::safety::resolve(workspace, ".") {
            Ok(r) => r,
            Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
        },
    };

    // Interpret the pattern relative to the resolved root.
    let full = root.join(&a.pattern).to_string_lossy().to_string();
    let entries = match glob::glob(&full) {
        Ok(e) => e,
        Err(err) => {
            return Ok(ToolCallOutcome::error(format!(
                "glob: invalid pattern: {err}"
            )))
        }
    };

    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in entries.filter_map(|e| e.ok()) {
        if out.len() >= MAX_RESULTS {
            truncated = true;
            break;
        }
        let rel = entry
            .strip_prefix(&root)
            .unwrap_or(&entry)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
    }

    if out.is_empty() {
        return Ok(ToolCallOutcome::ok("no files matched"));
    }
    out.sort();
    let mut text = out.join("\n");
    if truncated {
        text.push_str(&format!("\n... (results truncated at {MAX_RESULTS} files)"));
    }
    Ok(ToolCallOutcome::ok(text))
}
