//! `read_file` — read a file with line numbers (same-source semantics:
//! 10 MB guard, offset/limit window, `(empty file)` marker).

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read a file and return its contents with line numbers. Use offset and limit to read specific sections of large files. Always read a file before editing it.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to workspace"},
                "offset": {"type": "integer", "description": "Start line (0-based, default 0)"},
                "limit": {"type": "integer", "description": "Max lines to return (default 2000)"}
            },
            "required": ["path"]
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
        Err(e) => return Ok(ToolCallOutcome::error(format!("read: bad args: {e}"))),
    };
    let path = match crate::safety::resolve(workspace, &a.path) {
        Ok(p) => p,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    const MAX_READ_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "read {}: {e}",
                path.display()
            )))
        }
    };
    if metadata.len() > MAX_READ_SIZE {
        return Ok(ToolCallOutcome::error(format!(
            "file too large: {} bytes (max {MAX_READ_SIZE} bytes). Use offset/limit to read sections.",
            metadata.len()
        )));
    }

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "read {}: {e}",
                path.display()
            )))
        }
    };

    let offset = a.offset.unwrap_or(0);
    let limit = a.limit.unwrap_or(2000);

    let all_lines: Vec<&str> = content.lines().collect();
    let total = all_lines.len();

    if total == 0 {
        return Ok(ToolCallOutcome::ok("(empty file)\n"));
    }
    let start = offset.min(total);
    if start >= total {
        return Ok(ToolCallOutcome::ok(format!(
            "offset {offset} is beyond end of file ({total} lines)\n"
        )));
    }
    let end = start.saturating_add(limit).min(total);

    let mut out = String::new();
    for (i, line) in all_lines[start..end].iter().enumerate() {
        out.push_str(&format!("{:>5}\t{}\n", start + i + 1, line));
    }
    if end < total {
        out.push_str(&format!("... ({} more lines)\n", total - end));
    }
    Ok(ToolCallOutcome::ok(out))
}
