//! `grep` — regex search over workspace files (line matches, capped).

use std::path::{Path, PathBuf};

use openpaw_core::{Error, Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;
use walkdir::WalkDir;

const MAX_MATCHES: usize = 200;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    /// Glob filter for file names, e.g. `*.rs`.
    #[serde(default)]
    include: Option<String>,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "grep".into(),
        description: "Search file contents with a regex. Returns matches as `path:line: text`, capped at 200. Use `include` to filter file names (e.g. `*.rs`).".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression to search for"},
                "path": {"type": "string", "description": "Directory or file to search (default: workspace root)"},
                "include": {"type": "string", "description": "Glob filter for file names, e.g. *.rs"}
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
        Err(e) => return Ok(ToolCallOutcome::error(format!("grep: bad args: {e}"))),
    };
    // Compile the regex once: bad patterns fail fast with a clear error,
    // and the compiled regex is moved into the blocking task (no recompile).
    let re = match regex::Regex::new(&a.pattern) {
        Ok(re) => re,
        Err(err) => {
            return Ok(ToolCallOutcome::error(format!(
                "grep: invalid regex: {err}"
            )))
        }
    };
    let include_re = match &a.include {
        Some(glob_pat) => Some(
            glob::Pattern::new(glob_pat)
                .map_err(|e| Error::ToolHost(format!("grep: invalid include glob: {e}")))?,
        ),
        None => None,
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

    // Blocking directory walk in a spawn_blocking so the async runtime is
    // not stalled by large trees.
    let root2 = root.clone();
    let (matches, truncated) = tokio::task::spawn_blocking(move || {
        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in WalkDir::new(&root2)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path: PathBuf = entry.path().to_path_buf();
            if let Some(pat) = &include_re {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string());
                match name {
                    Some(n) if pat.matches(&n) => {}
                    _ => continue,
                }
            }
            // Skip binary-looking and oversized files.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > 4 * 1024 * 1024 {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (idx, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    if matches.len() >= MAX_MATCHES {
                        truncated = true;
                        return (matches, truncated);
                    }
                    let rel = path
                        .strip_prefix(&root2)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    matches.push(format!("{rel}:{}: {}", idx + 1, line.trim_end()));
                }
            }
        }
        (matches, truncated)
    })
    .await
    .map_err(|e| Error::ToolHost(format!("grep task: {e}")))?;

    if matches.is_empty() {
        return Ok(ToolCallOutcome::ok("no matches found"));
    }
    let mut out = matches.join("\n");
    if truncated {
        out.push_str(&format!(
            "\n... (results truncated at {MAX_MATCHES} matches)"
        ));
    }
    Ok(ToolCallOutcome::ok(out))
}
