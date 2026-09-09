//! `notebook_edit` — edit Jupyter notebook (`.ipynb`) cells.
//!
//! Supports three operations:
//! - `insert`: add a new cell at `cell_index` (out-of-range indexes append)
//! - `replace`: overwrite the source of an existing cell
//! - `delete`: remove a cell
//!
//! The notebook is read, modified in memory, and written back atomically.
//! Path safety is enforced via [`crate::safety::resolve`].
//!
//! Port notes (vs. the upstream tools-fs implementation):
//! - Failure paths (bad args, path escape, read/parse/write errors, cell
//!   index out of range, unknown operation) all return
//!   `Ok(ToolCallOutcome::error(..))` per the openpaw `edit.rs` convention,
//!   instead of propagating `Err(Error::ToolHost(..))`.
//! - `render_hint` / `search_hint` spec fields are dropped (no counterpart
//!   in `openpaw_core::ToolSpec`).
//! - `source` is written back as a single string (upstream semantics kept);
//!   multi-line array sources in untouched cells pass through unchanged.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    operation: String,
    cell_index: u64,
    #[serde(default)]
    cell_type: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "notebook_edit".into(),
        description: "Edit a Jupyter notebook cell. Supports insert, replace, and delete \
            operations on .ipynb files. cell_index is 0-based; for insert, an out-of-range \
            index appends the cell to the end."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the .ipynb file (relative to workspace)"
                },
                "operation": {
                    "type": "string",
                    "enum": ["insert", "replace", "delete"],
                    "description": "Cell operation to perform"
                },
                "cell_index": {
                    "type": "integer",
                    "description": "0-based cell index to operate on",
                    "minimum": 0
                },
                "cell_type": {
                    "type": "string",
                    "enum": ["code", "markdown"],
                    "description": "Cell type (required for insert, optional for replace)"
                },
                "source": {
                    "type": "string",
                    "description": "New cell source content (required for insert and replace)"
                }
            },
            "required": ["path", "operation", "cell_index"]
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
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "notebook_edit: bad args: {e}"
            )))
        }
    };
    let resolved = match crate::safety::resolve(workspace, &a.path) {
        Ok(p) => p,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    // Reject non-ipynb paths early to give a clearer error than JSON parse failure.
    if resolved.extension().and_then(|s| s.to_str()) != Some("ipynb") {
        return Ok(ToolCallOutcome::error(format!(
            "notebook_edit: not a .ipynb file: {}",
            a.path
        )));
    }

    // Read existing notebook (atomicity: read-modify-write under workspace safety).
    let raw = match tokio::fs::read_to_string(&resolved).await {
        Ok(r) => r,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "notebook_edit read {}: {e}",
                resolved.display()
            )))
        }
    };
    let mut notebook: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(nb) => nb,
        Err(e) => return Ok(ToolCallOutcome::error(format!("notebook_edit parse: {e}"))),
    };

    let idx = a.cell_index as usize;
    let op = a.operation.as_str();

    // Apply the operation on the cells array. Returns the resulting cell count
    // or an error message (without borrowing notebook).
    let apply_result: std::result::Result<usize, String> = {
        let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
            Some(c) => c,
            None => {
                return Ok(ToolCallOutcome::error(
                    "notebook_edit: notebook has no 'cells' array",
                ))
            }
        };
        match op {
            "insert" => {
                let cell_type = a.cell_type.as_deref().unwrap_or("code");
                let source = a.source.as_deref().unwrap_or("");
                let new_cell = serde_json::json!({
                    "cell_type": cell_type,
                    "source": source,
                    "metadata": {},
                });
                if idx >= cells.len() {
                    cells.push(new_cell);
                } else {
                    cells.insert(idx, new_cell);
                }
                Ok(cells.len())
            }
            "replace" => {
                if idx >= cells.len() {
                    Err(format!(
                        "replace index {idx} out of range (cells={})",
                        cells.len()
                    ))
                } else {
                    if let Some(source) = a.source.as_deref() {
                        cells[idx]["source"] = serde_json::Value::String(source.to_string());
                    }
                    if let Some(cell_type) = a.cell_type.as_deref() {
                        cells[idx]["cell_type"] = serde_json::Value::String(cell_type.to_string());
                    }
                    Ok(cells.len())
                }
            }
            "delete" => {
                if idx >= cells.len() {
                    Err(format!(
                        "delete index {idx} out of range (cells={})",
                        cells.len()
                    ))
                } else {
                    cells.remove(idx);
                    Ok(cells.len())
                }
            }
            other => Err(format!("unknown operation '{other}'")),
        }
    };

    let final_cell_count = match apply_result {
        Ok(n) => n,
        Err(msg) => return Ok(ToolCallOutcome::error(format!("notebook_edit: {msg}"))),
    };

    // Now safe to serialize — no outstanding mutable borrow of `notebook`.
    let updated = match serde_json::to_string_pretty(&notebook) {
        Ok(s) => s,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "notebook_edit serialize: {e}"
            )))
        }
    };
    if let Err(e) = write_atomic(&resolved, &updated).await {
        return Ok(ToolCallOutcome::error(e.to_string()));
    }

    Ok(ToolCallOutcome::ok(format!(
        "notebook_edit: {op} cell {idx} in {} (cells={})",
        a.path, final_cell_count
    )))
}

/// Atomic write helper: write to a sibling temp file then rename. Mirrors
/// the upstream tools-fs semantics (temp file + rename, cleanup on failure).
async fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| openpaw_core::Error::ToolHost("notebook_edit: path has no parent".into()))?;
    if let Err(e) = tokio::fs::create_dir_all(parent).await {
        return Err(openpaw_core::Error::ToolHost(format!(
            "notebook_edit mkdir: {e}"
        )));
    }
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("notebook"),
        std::process::id()
    ));
    if let Err(e) = tokio::fs::write(&tmp, content).await {
        return Err(openpaw_core::Error::ToolHost(format!(
            "notebook_edit tmp write: {e}"
        )));
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(openpaw_core::Error::ToolHost(format!(
            "notebook_edit rename: {e}"
        )));
    }
    Ok(())
}
