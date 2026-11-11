//! `session_search` — full-text search over past chat session messages
//! (plan §3.4 meta tool #7). Backed by the same SQLite FTS5 index the
//! `openpaw session search` command uses; read-only, user-domain
//! (`OPENPAW_HOME/index.db`), never scoped to the workspace.

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use openpaw_store::SessionDb;
use serde_json::json;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "session_search".into(),
        description: "Full-text search over past chat session messages (SQLite FTS5). Returns matching messages with session id, score, and a highlighted snippet.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search words (FTS5 match; punctuation-heavy queries are quoted automatically)"},
                "limit": {"type": "integer", "description": "Maximum number of hits (default 20)"}
            },
            "required": ["query"]
        }),
        category: Some(ToolCategory::ReadOnly),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        ..Default::default()
    }
}

/// 功能：通过原默认数据根查询会话索引。
/// 参数：workspace保留兼容，input为查询和数量限制。
/// 返回：真实查询结果或工具错误。
/// 错误：复用原SQLite错误结果。
/// 状态：只读原索引，不创建目录。
/// 并发与取消：每次查询独立，无进程环境修改。
/// 权限：默认库调用使用原用户数据根；CLI通过注册表显式注入。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub async fn run(_workspace: &Path, input: serde_json::Value) -> Result<ToolCallOutcome> {
    run_with_index(&openpaw_core::openpaw_home().join("index.db"), input).await
}

/// 功能：以调用方已授权的明确索引执行原会话检索。
/// 参数：db_path为可信索引路径，input为模型查询参数。
/// 返回：原会话查询结果。
/// 错误：缺少查询返回工具错误，索引错误沿原入口报告。
/// 状态：不回退到其他数据根。
/// 并发与取消：查询独立，无全局目录状态。
/// 权限：模型不能选择索引路径。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub(crate) async fn run_with_index(
    db_path: &Path,
    input: serde_json::Value,
) -> Result<ToolCallOutcome> {
    let Some(query) = input["query"].as_str().map(str::to_string) else {
        return Ok(ToolCallOutcome::error("Error: query parameter is required"));
    };
    let limit = input["limit"].as_u64().unwrap_or(20).min(200) as usize;

    search_index_for_tests(db_path, &query, limit).await
}

/// Search one index database. Split from `run` (and public) so tests and
/// embedding hosts can point at a specific home without touching process
/// environment variables.
pub async fn search_index_for_tests(
    db_path: &Path,
    query: &str,
    limit: usize,
) -> Result<ToolCallOutcome> {
    if !db_path.exists() {
        return Ok(ToolCallOutcome::ok(
            "(no session index yet — start a conversation with `openpaw chat`)".to_string(),
        ));
    }
    let db = match SessionDb::open(db_path) {
        Ok(db) => db,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "Error: cannot open session index {}: {e}\nwhy: the index file must be a readable SQLite database.\nnext: remove the index to rebuild it on the next chat.",
                db_path.display()
            )))
        }
    };
    let results = match db.search(query, limit) {
        Ok(results) => results,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "Error: search failed: {e}\nwhy: the full-text query could not be executed against the index.\nnext: simplify the query; punctuation-heavy queries are quoted automatically."
            )))
        }
    };
    if results.is_empty() {
        return Ok(ToolCallOutcome::ok(format!("(no results for \"{query}\")")));
    }
    let mut out = String::new();
    for r in &results {
        // FTS5 rank is negative; negate so a bigger score reads as a
        // better match. Prefer the highlighted snippet when present.
        let snippet = if r.snippet.is_empty() {
            preview(&r.text_preview, 120)
        } else {
            preview(&r.snippet, 120)
        };
        out.push_str(&format!(
            "[{}:{}] score={:.3} {}\n",
            r.session_id, r.seq, -r.rank, snippet
        ));
    }
    out.push_str(&format!("({} result(s))", results.len()));
    Ok(ToolCallOutcome::ok(out))
}

fn preview(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}
