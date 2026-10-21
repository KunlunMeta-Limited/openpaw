//! `todo_add` / `todo_update` / `todo_list` — a plain-file task list.
//!
//! The list persists as JSONL under `<workspace>/.openpaw/todos.jsonl`
//! (plain files are the database); one JSON object per line.

use std::io::Write;
use std::path::Path;
use tokio_util::sync::CancellationToken;

use openpaw_core::{Error, Result, ToolCallOutcome, ToolSpec};
use serde::Deserialize;
use serde_json::json;

const TODOS_FILE: &str = ".openpaw/todos.jsonl";
const MAX_TODOS: usize = 200;

/// 功能：声明原Todo工具及副作用metadata
/// 参数：无
/// 返回：原三项工具声明
/// 错误：无
/// 状态：只构造声明
/// 并发与取消：只读并行，修改为串行且需审批
/// 权限：审批交给既有引擎；不在这里授权
/// 关联：需求/设计 DES-OP-004；REQ-OP-007。
/// Stability: unstable.
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
            concurrency: Some(openpaw_core::ToolConcurrency::PathScoped),
            requires_approval: true,
            category: Some(openpaw_core::ToolCategory::Write),
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
            concurrency: Some(openpaw_core::ToolConcurrency::PathScoped),
            requires_approval: true,
            category: Some(openpaw_core::ToolCategory::Write),
            ..Default::default()
        },
        ToolSpec {
            name: "todo_list".into(),
            description: "List all todos with their status.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            concurrency: Some(openpaw_core::ToolConcurrency::ParallelSafe),
            category: Some(openpaw_core::ToolCategory::ReadOnly),
            ..Default::default()
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "todo_add" | "todo_update" | "todo_list")
}

/// 用途：原JSONL的结构化Todo
/// 字段：编号id、标题title与状态status
/// 来源去向：原Todo文件到工具或Host只读查询
/// 约束：不通过查询创建文件；坏数据必须报错
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Todo {
    /// 用途：唯一非零编号
    /// 字段：唯一非零编号
    /// 来源去向：Todo文件到只读调用者
    /// 约束：读取时验证，不把损坏转为空
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    pub id: u32,
    /// 用途：原任务标题
    /// 字段：原任务标题
    /// 来源去向：Todo文件到只读调用者
    /// 约束：读取时验证，不把损坏转为空
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    pub title: String,
    /// 用途：状态值pending/in_progress/done
    /// 字段：合法状态pending/in_progress/done
    /// 来源去向：Todo文件到只读调用者
    /// 约束：读取时验证，不把损坏转为空
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    pub status: String,
}
fn todos_path(workspace: &Path) -> Result<std::path::PathBuf> {
    crate::safety::resolve(workspace, TODOS_FILE)
}

/// 功能：真正只读地读取原Todo JSONL
/// 参数：workspace为已授权工作区
/// 返回：结构化列表；仅NotFound为空
/// 错误：损坏、重复ID、无效状态、非普通文件和权限错误上报
/// 状态：不mkdir、不创建锁文件、不修改内容
/// 并发与取消：原子替换保证一轮读取看到完整旧或新文件；外部非协作写入损坏则报错
/// 权限：解析前复用工作区路径检查，拒绝链接逃逸
/// 关联：需求/设计 DES-OP-004；REQ-OP-007。
/// Stability: unstable.
pub fn read(workspace: &Path) -> Result<Vec<Todo>> {
    let path = todos_path(workspace)?;
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(Error::ToolHost(format!("todo read: {e}"))),
    };
    let metadata = file
        .metadata()
        .map_err(|e| Error::ToolHost(format!("todo metadata: {e}")))?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
        return Err(Error::ToolHost(
            "todo requires a regular JSONL file <= 2MiB".into(),
        ));
    }
    use std::io::Read;
    let mut content = String::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut content)
        .map_err(|e| Error::ToolHost(format!("todo read: {e}")))?;
    if content.len() > 2 * 1024 * 1024 {
        return Err(Error::ToolHost("todo file too large".into()));
    }
    let mut todos = vec![];
    let mut ids = std::collections::HashSet::new();
    for (line, text) in content.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let todo: Todo = serde_json::from_str(text)
            .map_err(|e| Error::ToolHost(format!("todo corrupt line {}: {e}", line + 1)))?;
        if todo.id == 0
            || !ids.insert(todo.id)
            || !matches!(todo.status.as_str(), "pending" | "in_progress" | "done")
        {
            return Err(Error::ToolHost(format!(
                "todo corrupt fields at line {}",
                line + 1
            )));
        }
        todos.push(todo);
    }
    Ok(todos)
}

fn persist(workspace: &Path, todos: &[Todo]) -> Result<()> {
    let path = todos_path(workspace)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::ToolHost("todo missing parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| Error::ToolHost(format!("todo temporary: {e}")))?;
    for todo in todos {
        serde_json::to_writer(&mut temporary, todo)
            .map_err(|e| Error::ToolHost(format!("todo encode: {e}")))?;
        temporary
            .write_all(b"\n")
            .map_err(|e| Error::ToolHost(format!("todo write: {e}")))?;
    }
    if temporary
        .as_file()
        .metadata()
        .map_err(|e| Error::ToolHost(format!("todo temporary metadata: {e}")))?
        .len()
        > 2 * 1024 * 1024
    {
        return Err(Error::ToolHost(
            "todo write exceeds readable 2MiB limit".into(),
        ));
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| Error::ToolHost(format!("todo sync: {e}")))?;
    // 同目录原子替换，失败保留原文件；临时文件由既有tempfile RAII删除。
    temporary
        .persist(&path)
        .map_err(|e| Error::ToolHost(format!("todo atomic replace: {}", e.error)))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::ToolHost(format!("todo directory sync: {e}")))?;
    Ok(())
}

/// 功能：兼容原Todo工具入口
/// 参数：workspace/name/args为原输入
/// 返回：原文本格式工具结果
/// 错误：读写和损坏错误向上传播
/// 状态：修改时加锁并原子替换
/// 并发与取消：无外部取消token时仍完整提交
/// 权限：本入口不提供审批；受控调用由原ToolRegistry/引擎授权
/// 关联：需求/设计 DES-OP-004；REQ-OP-007。
/// Stability: unstable.
pub async fn run(workspace: &Path, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
    run_cancellable(workspace, name, args, CancellationToken::new(), None).await
}
/// 功能：在原OS锁下完整读改写Todo
/// 参数：workspace/name/args为原输入，cancel为调用token
/// 返回：完整提交后的结果
/// 错误：锁、权限、损坏或提交失败；取消明确返回
/// 状态：写操作mkdir/锁文件/原子替换，只读不创建文件
/// 并发与取消：可取消等待锁；提交过程无await，完成后才释放锁
/// 权限：复用工作区路径安全；建议锁仅约束协作方
/// 关联：需求/设计 DES-OP-004；REQ-OP-007。
/// Stability: unstable.
pub(crate) async fn run_cancellable(
    workspace: &Path,
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
    context: Option<&openpaw_core::ToolCallContext>,
) -> Result<ToolCallOutcome> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if name == "todo_list" {
        return list(workspace).await;
    }
    if !matches!(name, "todo_add" | "todo_update") {
        return Err(Error::ToolHost(format!("unknown todo tool: {name}")));
    }
    let path = todos_path(workspace)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::ToolHost("todo missing parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| Error::ToolHost(format!("todo mkdir: {e}")))?;
    let lock_path = crate::safety::resolve(workspace, ".openpaw/todos.lock")?;
    let _guard = loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match openpaw_sandbox::try_lock_exclusive(&lock_path) {
            Ok(guard) => break guard,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::select! { biased;_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(std::time::Duration::from_millis(5))=>{} }
            }
            Err(e) => return Err(Error::ToolHost(format!("todo lock: {e}"))),
        }
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // 锁等待结束后重验路径，再保存running；审批时不持此锁。
    let _ = todos_path(workspace)?;
    if let Some(context) = context {
        context.execution_started(name, &args).await?;
    }
    match name {
        "todo_add" => add(workspace, args).await,
        "todo_update" => update(workspace, args).await,
        _ => unreachable!(),
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
    let mut todos = read(workspace)?;
    let id = todos
        .iter()
        .map(|t| t.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::ToolHost("todo id exhausted".into()))?;
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
    let mut todos = read(workspace)?;
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
    let todos = read(workspace)?;
    if todos.is_empty() {
        return Ok(ToolCallOutcome::ok("(no todos)"));
    }
    let mut out = String::new();
    for t in &todos {
        out.push_str(&format!("#{} [{}] {}\n", t.id, t.status, t.title));
    }
    Ok(ToolCallOutcome::ok(out))
}
