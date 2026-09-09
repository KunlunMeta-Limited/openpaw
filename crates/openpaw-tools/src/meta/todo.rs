//! `todo_add` / `todo_update` / `todo_list` — a plain-file task list.
//!
//! The list persists as JSONL under `<workspace>/.openpaw/todos.jsonl`
//! (plain files are the database); one JSON object per line.

use std::path::Path;

use openpaw_core::{Error, Result, ToolCallOutcome, ToolSpec};
use serde::Deserialize;
use serde_json::json;

const TODOS_FILE: &str = ".openpaw/todos.jsonl";
const MAX_TODOS: usize = 200;

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "todo_add".into(),
            description: "Add a new todo item. Returns the assigned ID.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"title": {"type": "string", "description": "Task description"}},
                "required": ["title"]
            }),
            ..Default::default()
        },
        ToolSpec {
            name: "todo_update".into(),
            description: "Update a todo's status to pending, in_progress, or done.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "Todo ID"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                },
                "required": ["id", "status"]
            }),
            ..Default::default()
        },
        ToolSpec {
            name: "todo_list".into(),
            description: "List all todos with their status.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "todo_add" | "todo_update" | "todo_list")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Todo {
    id: u32,
    title: String,
    status: String,
}

fn todos_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(TODOS_FILE)
}

fn load(workspace: &Path) -> Vec<Todo> {
    let Ok(content) = std::fs::read_to_string(todos_path(workspace)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn persist(workspace: &Path, todos: &[Todo]) -> Result<()> {
    let path = todos_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::ToolHost(format!("todo mkdir: {e}")))?;
    }
    let mut buf = String::new();
    for t in todos {
        buf.push_str(&serde_json::to_string(t).unwrap_or_default());
        buf.push('\n');
    }
    std::fs::write(&path, buf).map_err(|e| Error::ToolHost(format!("todo write: {e}")))?;
    Ok(())
}

pub async fn run(workspace: &Path, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
    match name {
        "todo_add" => add(workspace, args).await,
        "todo_update" => update(workspace, args).await,
        "todo_list" => list(workspace).await,
        _ => Err(Error::ToolHost(format!("unknown todo tool: {name}"))),
    }
}

#[derive(Deserialize)]
struct AddArgs {
    title: String,
}

async fn add(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: AddArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("todo_add: bad args: {e}"))),
    };
    let mut todos = load(workspace);
    let id = todos.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    todos.push(Todo {
        id,
        title: a.title.clone(),
        status: "pending".into(),
    });
    if todos.len() > MAX_TODOS {
        let excess = todos.len() - MAX_TODOS;
        todos.drain(0..excess);
    }
    persist(workspace, &todos)?;
    Ok(ToolCallOutcome::ok(format!(
        "added todo #{id}: {}",
        a.title
    )))
}

#[derive(Deserialize)]
struct UpdateArgs {
    id: u32,
    status: String,
}

async fn update(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: UpdateArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "todo_update: bad args: {e}"
            )))
        }
    };
    if !matches!(a.status.as_str(), "pending" | "in_progress" | "done") {
        return Ok(ToolCallOutcome::error(format!(
            "todo_update: invalid status `{}` (pending | in_progress | done)",
            a.status
        )));
    }
    let mut todos = load(workspace);
    let Some(todo) = todos.iter_mut().find(|t| t.id == a.id) else {
        return Ok(ToolCallOutcome::error(format!(
            "todo_update: no todo with id {}",
            a.id
        )));
    };
    todo.status = a.status.clone();
    persist(workspace, &todos)?;
    Ok(ToolCallOutcome::ok(format!(
        "todo #{} → {}",
        a.id, a.status
    )))
}

async fn list(workspace: &Path) -> Result<ToolCallOutcome> {
    let todos = load(workspace);
    if todos.is_empty() {
        return Ok(ToolCallOutcome::ok("(no todos)"));
    }
    let mut out = String::new();
    for t in &todos {
        out.push_str(&format!("#{} [{}] {}\n", t.id, t.status, t.title));
    }
    Ok(ToolCallOutcome::ok(out))
}
