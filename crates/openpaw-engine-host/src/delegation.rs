//! 功能：在 engine-host 兼容层提供真实子 Agent 委派工具。
//! 边界：复用 OpenPaw AgentLoop 和内置工具，不修改核心 crate，也不允许子 Agent 递归委派。
//! 禁止承担职责：不操作桌面、不递归委派、不继承或传播父会话授权。
//! 关联设计章节：ADR-006 父子授权边界；CHG-2026-0032 Office 多模态合并设计。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, StreamEvent,
    ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor, ToolHost, ToolHostValidator,
    ToolSpec,
};
use openpaw_engine::AgentEvent;
use openpaw_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::approval::HostApprovalManager;
use crate::office::{OfficeDispatcher, SubagentOfficeTurn};

const MAX_DELEGATE_TASK_CHARS: usize = 12_000;
const MAX_DELEGATE_NAME_CHARS: usize = 40;
const MAX_PARALLEL_SUBAGENTS: usize = 4;
const DEFAULT_GLOBAL_SUBAGENTS: usize = 4;

/// 功能：共享父子模型请求额度及同工作区写入互斥；不持有会话授权。
pub(crate) struct DelegationShared {
    model_slots: Arc<Semaphore>,
    runtime: RwLock<std::sync::Weak<crate::runtime::AppState>>,
    workspace_write_locks: Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>,
}

impl DelegationShared {
    /// 功能：创建 Host 共享模型额度与工作区锁表。
    /// 参数：读取并校验两个既有并发环境变量。
    /// 返回：独立共享状态 Arc。
    /// 错误：非法值使用默认并发四。
    /// 状态：创建空工作区锁表。
    /// 并发与取消：各父会话共用信号量，同一规范工作区共用写锁。
    /// 权限：只限制并发，不增加工具权限。
    /// 关联：CHG-2026-0032。
    pub(crate) fn from_env() -> Arc<Self> {
        let slots = std::env::var("OPENPAW_ENGINE_MAX_MODEL_REQUESTS")
            .or_else(|_| std::env::var("OPENPAW_ENGINE_MAX_SUBAGENTS"))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=32).contains(value))
            .unwrap_or(DEFAULT_GLOBAL_SUBAGENTS);
        Arc::new(Self {
            model_slots: Arc::new(Semaphore::new(slots)),
            runtime: RwLock::new(std::sync::Weak::new()),
            workspace_write_locks: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_slots(slots: usize) -> Arc<Self> {
        Arc::new(Self {
            model_slots: Arc::new(Semaphore::new(slots)),
            runtime: RwLock::new(std::sync::Weak::new()),
            workspace_write_locks: Mutex::new(HashMap::new()),
        })
    }

    /// 功能：绑定共享普通会话运行时的弱引用。
    /// 参数：runtime 为当前 Host。
    /// 返回：无。
    /// 错误：无。
    /// 状态：仅更新弱引用，不持有 Host 循环引用。
    /// 并发与取消：短写锁；不跨 await。
    /// 权限：仅内部组装，模型不能替换。
    /// 关联：CHG-2026-0034。
    pub(crate) fn bind_runtime(&self, runtime: &Arc<crate::runtime::AppState>) {
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(runtime);
    }

    fn workspace_write_lock(&self, workspace: &PathBuf) -> Arc<AsyncMutex<()>> {
        let key = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.clone());
        self.workspace_write_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateTaskInput {
    task: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
}

struct ProviderModel {
    provider: Arc<dyn LlmProvider>,
    model: String,
    cancel: CancellationToken,
}

/// 每个父 session 独享子空间编号和会话额度，并共享 Host 级并发与工作区写锁。
pub(crate) struct DelegationContext {
    parent_session_id: String,
    workspace: PathBuf,
    office: OfficeDispatcher,
    approvals: Arc<HostApprovalManager>,
    provider_model: RwLock<ProviderModel>,
    sequence: AtomicU64,
    delegations: Mutex<HashMap<String, DelegationRecord>>,
    state_file: PathBuf,
    slots: Semaphore,
    shared: Arc<DelegationShared>,
    #[cfg(test)]
    _runtime_keeper: Option<Arc<crate::runtime::AppState>>,
    enabled: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedDelegationOutcome {
    content: String,
    structured: serde_json::Value,
}

struct DelegationRecord {
    workspace_id: String,
    workspace_name: String,
    task_fingerprint: String,
    task: String,
    completed: Option<CachedDelegationOutcome>,
}

#[derive(Serialize, Deserialize)]
struct PersistedDelegationState {
    version: u8,
    records: Vec<PersistedDelegationRecord>,
}

#[derive(Serialize, Deserialize)]
struct PersistedDelegationRecord {
    logical_key: String,
    workspace_id: String,
    workspace_name: String,
    task_fingerprint: String,
    task: String,
    #[serde(default)]
    completed: Option<CachedDelegationOutcome>,
}

impl DelegationContext {
    /// 功能：构造父会话独有的文字委派上下文。
    /// 参数：会话、工作区、Office、父审批、模型与共享额度。
    /// 返回：独立委派上下文 Arc。
    /// 错误：不可读或无效快照不复用结果。
    /// 状态：恢复空间身份与 v2 可选结果；v1 只恢复身份。
    /// 并发与取消：模型上下文有读写锁，记录有独立同步锁。
    /// 权限：不继承桌面登记或父上传附件；审批按子执行隔离。
    /// 关联：ADR-006、CHG-2026-0032。
    pub(crate) fn new(
        parent_session_id: String,
        workspace: PathBuf,
        office: OfficeDispatcher,
        approvals: Arc<HostApprovalManager>,
        provider: Arc<dyn LlmProvider>,
        model: String,
        shared: Arc<DelegationShared>,
    ) -> Arc<Self> {
        let enabled = office.is_enabled();
        let state_file = delegation_state_file(&workspace, &parent_session_id);
        let delegations = load_delegations(&state_file, &parent_session_id);
        let sequence = delegations.len() as u64;
        Arc::new(Self {
            parent_session_id,
            workspace,
            office,
            approvals,
            provider_model: RwLock::new(ProviderModel {
                provider,
                model,
                cancel: CancellationToken::new(),
            }),
            sequence: AtomicU64::new(sequence),
            delegations: Mutex::new(delegations),
            state_file,
            slots: Semaphore::new(MAX_PARALLEL_SUBAGENTS),
            shared,
            #[cfg(test)]
            _runtime_keeper: None,
            enabled,
        })
    }

    /// 功能：绑定本轮子任务所用模型与取消令牌。
    /// 参数：本轮 provider、model、cancel。
    /// 返回：无。
    /// 错误：无。
    /// 状态：只替换请求级模型上下文，不修改缓存。
    /// 并发与取消：同步写锁；子执行接收同轮取消。
    /// 权限：不复制父审批授权或桌面身份。
    /// 关联：runtime 回合执行与 ADR-006。
    pub(crate) fn begin_turn(
        &self,
        provider: Arc<dyn LlmProvider>,
        model: String,
        cancel: CancellationToken,
    ) {
        *self
            .provider_model
            .write()
            .unwrap_or_else(|error| error.into_inner()) = ProviderModel {
            provider,
            model,
            cancel,
        };
    }

    /// 功能：为父子模型调用共用 Host 请求额度。
    /// 参数：实际 provider。
    /// 返回：能力透明的 provider 包装。
    /// 错误：额度关闭时模型请求返回既有错误。
    /// 状态：不改变模型能力或请求内容。
    /// 并发与取消：流持有额度直到结束或被丢弃。
    /// 权限：不增加模型或工具权限。
    /// 关联：CHG-2026-0032。
    pub(crate) fn limit_provider(&self, provider: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
        Arc::new(LimitedProvider {
            inner: provider,
            slots: self.shared.model_slots.clone(),
        })
    }

    /// 功能：为真实子执行组装仅工作区内置工具。
    /// 参数：无。
    /// 返回：使用共享写锁的子工具 Host。
    /// 错误：无。
    /// 状态：每个会话维护自己的媒体上下文。
    /// 并发与取消：共享工作区写锁；取消由普通 runtime 管理。
    /// 权限：无桌面工具、无递归委派。
    /// 关联：ADR-006、CHG-2026-0034。
    pub(crate) fn child_tool_host(&self) -> Arc<dyn ToolHost> {
        Arc::new(WorkspaceToolHost::new(
            self.workspace.clone(),
            self.shared.workspace_write_lock(&self.workspace),
        ))
    }

    async fn run(&self, input: DelegateTaskInput) -> openpaw_core::Result<ToolCallOutcome> {
        let task = input.task.trim();
        if task.is_empty() || task.chars().count() > MAX_DELEGATE_TASK_CHARS {
            return Ok(ToolCallOutcome::error(format!(
                "task 必须为 1..={MAX_DELEGATE_TASK_CHARS} 个字符"
            )));
        }
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let name = normalized_name(input.name.as_deref(), sequence)?;
        // 展示空间沿用旧协议，真实会话使用 runtime 分配的合法 UUID。
        let workspace_id = format!(
            "{}:agent:{sequence}-{}",
            self.parent_session_id,
            Uuid::new_v4().simple()
        );
        let (provider, model, cancel) = {
            let current = self
                .provider_model
                .read()
                .unwrap_or_else(|e| e.into_inner());
            (
                current.provider.clone(),
                current.model.clone(),
                current.cancel.clone(),
            )
        };
        let _permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(cancelled_outcome(&workspace_id, &name, &self.parent_session_id)),
            permit = self.slots.acquire() => permit.map_err(|_| Error::ToolHost("子 Agent 调度器已关闭".into()))?,
        };
        let runtime = self
            .shared
            .runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
            .ok_or_else(|| Error::ToolHost("委派运行时不可用，未启动子任务".into()))?;
        runtime
            .run_delegated(
                input.task_id,
                crate::runtime::ChildSession {
                    parent_id: self.parent_session_id.clone(),
                    workspace_id,
                    name,
                    parent_message_id: None,
                },
                self.workspace.clone(),
                task.to_string(),
                provider,
                model,
                cancel,
                self.approvals.clone(),
            )
            .await
            .map_err(Error::ToolHost)
    }

    /// 功能：可靠清除已完成结果，保留逻辑子空间。
    /// 参数：无。
    /// 返回：持久化成功返回空值。
    /// 错误：文件写入失败原样返回；调用方不得提交 Revert。
    /// 状态：先保存无结果快照，再更新内存，不恢复旧缓存。
    /// 并发与取消：调用方持有空闲回合锁；记录锁覆盖保存，不可取消。
    /// 权限：不撤销文件或工具副作用。
    /// 关联：Q5、staged Revert。
    pub(crate) fn invalidate_results(&self) -> std::io::Result<()> {
        let mut records = self.delegations.lock().unwrap_or_else(|e| e.into_inner());
        // 新持久子会话不使用旧答案缓存；不要为正常 Revert 新建空的旧格式快照。
        if records.is_empty() {
            return Ok(());
        }
        let mut snapshot = persisted_delegation_state(&records);
        for record in &mut snapshot.records {
            record.completed = None;
        }
        save_delegations(&self.state_file, &snapshot)?;
        for record in records.values_mut() {
            record.completed = None;
        }
        Ok(())
    }

    /// 功能：向父模型列出明确可续办的真实子会话身份。
    /// 参数：无。
    /// 返回：本父工作区存在未归档子会话时返回 ID 与显示名提示。
    /// 错误：无。
    /// 状态：只读统一会话目录，不读取旧答案缓存、不启动执行。
    /// 并发与取消：目录扫描使用阻塞任务；不持有回合锁或网络连接。
    /// 权限：只列出可信父关系与同目录的子身份。
    /// 关联：CHG-2026-0035、明确 task_id 续办。
    pub(crate) async fn context_prompt(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let runtime = self
            .shared
            .runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()?;
        runtime
            .child_context(&self.parent_session_id, &self.workspace)
            .await
    }
}

struct LimitedProvider {
    inner: Arc<dyn LlmProvider>,
    slots: Arc<Semaphore>,
}

#[async_trait]
impl LlmProvider for LimitedProvider {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        let _permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Provider("模型请求调度器已关闭".into()))?;
        self.inner.complete(req).await
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<BoxStream<'static, openpaw_core::Result<StreamEvent>>> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Provider("模型请求调度器已关闭".into()))?;
        let stream = self.inner.stream(req).await?;
        Ok(Box::pin(futures::stream::unfold(
            (stream, permit),
            |(mut stream, permit)| async move {
                stream.next().await.map(|event| (event, (stream, permit)))
            },
        )))
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn model_capabilities(&self, model: &str) -> Capabilities {
        self.inner.model_capabilities(model)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
}

fn cancelled_outcome(workspace_id: &str, name: &str, parent_session_id: &str) -> ToolCallOutcome {
    ToolCallOutcome {
        media: Vec::new(),
        content: format!("子 Agent“{name}”已随父回合取消。"),
        is_error: true,
        structured: Some(json!({
            "workspaceId": workspace_id,
            "workspaceName": name,
            "parentSessionId": parent_session_id,
            "status": "cancelled"
        })),
    }
}

fn normalized_name(value: Option<&str>, sequence: u64) -> openpaw_core::Result<String> {
    let fallback = format!("协作子任务{}", chinese_digits(sequence));
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&fallback);
    if value.chars().count() > MAX_DELEGATE_NAME_CHARS {
        return Err(Error::ToolHost(format!(
            "name 最多允许 {MAX_DELEGATE_NAME_CHARS} 个字符"
        )));
    }
    Ok(value.to_string())
}

fn chinese_digits(value: u64) -> String {
    const DIGITS: [char; 10] = ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
    value
        .to_string()
        .bytes()
        .map(|digit| DIGITS[usize::from(digit - b'0')])
        .collect()
}

fn delegation_state_file(workspace: &std::path::Path, parent_session_id: &str) -> PathBuf {
    workspace
        .join(".openpaw")
        .join("engine-host-delegations")
        .join(format!("{parent_session_id}.json"))
}

/// 功能：删除会话私有委派快照。
/// 参数：工作区与父会话编号。
/// 返回：删除成功或不存在时返回空值。
/// 错误：其他文件错误原样返回。
/// 状态：只删除指定会话快照。
/// 并发与取消：调用方须先结束该会话执行。
/// 权限：不删除用户文件、journal 或产物。
/// 关联：runtime 删除会话。
pub(crate) fn remove_persisted_delegation_state(
    workspace: &std::path::Path,
    parent_session_id: &str,
) -> std::io::Result<()> {
    match std::fs::remove_file(delegation_state_file(workspace, parent_session_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn load_delegations(
    file: &std::path::Path,
    parent_session_id: &str,
) -> HashMap<String, DelegationRecord> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return HashMap::new();
    };
    let Ok(state) = serde_json::from_str::<PersistedDelegationState>(&text) else {
        return HashMap::new();
    };
    if !matches!(state.version, 1 | 2) {
        return HashMap::new();
    }
    let reusable = state.version == 2;
    state
        .records
        .into_iter()
        .filter(|record| {
            !record.logical_key.is_empty()
                && record
                    .workspace_id
                    .starts_with(&format!("{parent_session_id}:agent:"))
                && !record.workspace_name.trim().is_empty()
                && record.workspace_name.chars().count() <= MAX_DELEGATE_NAME_CHARS
                && !record.task.trim().is_empty()
                && record.task.chars().count() <= MAX_DELEGATE_TASK_CHARS
        })
        .map(|mut record| {
            let workspace_name = record.workspace_name.trim().to_string();
            if let Some(structured) = record
                .completed
                .as_mut()
                .and_then(|value| value.structured.as_object_mut())
            {
                structured.insert("workspaceName".into(), json!(workspace_name.clone()));
            }
            (
                record.logical_key,
                DelegationRecord {
                    workspace_id: record.workspace_id,
                    workspace_name,
                    task_fingerprint: record.task_fingerprint,
                    task: record.task,
                    completed: if reusable { record.completed } else { None },
                },
            )
        })
        .collect()
}

/// 功能：读取旧临时子执行中确实保存的任务和最终结果。
/// 参数：workspace 为可信工作目录，parent 为已登记父身份。
/// 返回：保留旧展示编号的可证实事实，不提供可执行 task_id。
/// 错误：旧文件缺失或格式不支持时返回空，不清空源文件。
/// 状态：只读；完整历史缺失须由使用方明确标注。
/// 并发与取消：同步读取旧小快照，不写盘、不执行工具。
/// 权限：仅访问由父会话编号派生的既有快照。
/// 关联：CHG-2026-0040、ADR-0002。
pub(crate) fn legacy_records(workspace: &std::path::Path, parent: &str) -> Vec<serde_json::Value> {
    let mut records:Vec<_>=load_delegations(&delegation_state_file(workspace,parent),parent).into_values()
        .map(|record|json!({"id":record.workspace_id,"name":record.workspace_name,"task":record.task,"result":record.completed.map(|value|value.content)})).collect();
    records.sort_by_key(|record| record["id"].as_str().unwrap_or("").to_string());
    records
}

fn persisted_delegation_state(
    records: &HashMap<String, DelegationRecord>,
) -> PersistedDelegationState {
    PersistedDelegationState {
        version: 2,
        records: records
            .iter()
            .map(|(logical_key, record)| PersistedDelegationRecord {
                logical_key: logical_key.clone(),
                workspace_id: record.workspace_id.clone(),
                workspace_name: record.workspace_name.clone(),
                task_fingerprint: record.task_fingerprint.clone(),
                task: record.task.clone(),
                completed: record.completed.clone(),
            })
            .collect(),
    }
}

fn save_delegations(
    file: &std::path::Path,
    state: &PersistedDelegationState,
) -> std::io::Result<()> {
    let Some(directory) = file.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".delegations-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| {
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(&serde_json::to_vec(state)?)?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temporary, file)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn delegate_spec() -> ToolSpec {
    ToolSpec {
        name: "delegate_task".into(),
        description: "把清晰独立的子任务交给真实子 Agent 并等待结果。继续已有员工必须传上次结果或员工清单中的真实 task_id，保留其自己的工作历史；不传 task_id 始终创建新身份，即使 name 或 task 相同。name 仅用于新员工显示，优先中文；不能传 workspaceId 代替 task_id。简单任务不要调用。".into(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["task"],
            "properties": {
                "task": { "type": "string", "minLength": 1, "maxLength": MAX_DELEGATE_TASK_CHARS },
                "name": { "type": "string", "minLength": 1, "maxLength": MAX_DELEGATE_NAME_CHARS },
                "task_id": { "type": "string", "minLength": 1, "maxLength": 128 }
            }
        }),
        category: Some(ToolCategory::Communication),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        max_result_chars: Some(24_000),
        ..ToolSpec::default()
    }
}

fn artifact_spec() -> ToolSpec {
    ToolSpec {
        name: "register_artifact".into(),
        description: "登记本轮应交付用户查看的真实文件；临时、检索和调试文件不要登记。文件可由文件工具、shell或子Agent生成；不会修改文件，也不要求最终答复采用特定格式。".into(),
        input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","minLength":1,"maxLength":1000}}}),
        category: Some(ToolCategory::Communication),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        ..ToolSpec::default()
    }
}

fn artifact_path(input: &serde_json::Value) -> std::result::Result<&str, String> {
    let path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty() && path.len() <= 1000)
        .ok_or_else(|| "path必须是1到1000字节的工作区相对路径".to_string())?;
    if std::path::Path::new(path).is_absolute()
        || path
            .split(['/', '\\'])
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("path必须是工作区内规范相对路径".into());
    }
    Ok(path)
}

/// 父 Agent 使用的组合 ToolHost；委派和交付登记之外的调用全部交给原 ToolRegistry。
pub(crate) struct DelegatingToolHost {
    builtin: WorkspaceToolHost,
    context: Arc<DelegationContext>,
}

impl DelegatingToolHost {
    /// 功能：组合主会话内置工具与 Office 委派工具。
    /// 参数：工作区和父会话委派上下文。
    /// 返回：主会话工具适配器。
    /// 错误：无。
    /// 状态：复用同一工作区写锁。
    /// 并发与取消：文件写操作和子任务使用同一写锁；媒体表独立。
    /// 权限：桌面包装由 runtime 仅在主会话外层安装。
    /// 关联：ADR-006、session_tool_host。
    pub(crate) fn new(workspace: PathBuf, context: Arc<DelegationContext>) -> Self {
        let write_lock = context.shared.workspace_write_lock(&workspace);
        Self {
            builtin: WorkspaceToolHost::new(workspace, write_lock),
            context,
        }
    }
}

#[async_trait]
impl ToolExecutor for DelegatingToolHost {
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.builtin.set_media_context(media);
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.builtin.remember_media(media);
    }
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        let mut tools = self.builtin.list_tools().await?;
        if self.context.enabled {
            tools.push(delegate_spec());
            tools.push(artifact_spec());
        }
        Ok(tools)
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> openpaw_core::Result<ToolCallOutcome> {
        if name == "register_artifact" {
            if !self.context.enabled {
                return Ok(ToolCallOutcome::error("当前Host未启用Office产物登记"));
            }
            let path = match artifact_path(&args) {
                Ok(path) => path,
                Err(error) => return Ok(ToolCallOutcome::error(error)),
            };
            // 路径安全复用tools现有边界，不在Host重写文件工具。
            let resolved = match openpaw_tools::safety::resolve(&self.context.workspace, path) {
                Ok(path) => path,
                Err(error) => return Ok(ToolCallOutcome::error(error.to_string())),
            };
            let metadata = match std::fs::metadata(&resolved) {
                Ok(metadata) if metadata.is_file() && metadata.len() <= 8 * 1024 * 1024 => metadata,
                _ => {
                    return Ok(ToolCallOutcome::error(
                        "产物必须是存在且不超过8MiB的普通文件",
                    ))
                }
            };
            let root = openpaw_tools::safety::dunce_canonicalize(&self.context.workspace)?;
            let relative = resolved
                .strip_prefix(&root)
                .map_err(|_| Error::ToolHost("产物路径越界".into()))?
                .to_string_lossy()
                .replace('\\', "/");
            self.context
                .office
                .register_artifact(&self.context.parent_session_id, relative.clone());
            return Ok(ToolCallOutcome {
                media: Vec::new(),
                content: format!("已登记产物：{relative}"),
                is_error: false,
                structured: Some(
                    json!({"path":relative,"size":metadata.len(),"sessionId":self.context.parent_session_id,"workspaceId":self.context.parent_session_id}),
                ),
            });
        }
        if name != "delegate_task" {
            return self.builtin.call(name, args).await;
        }
        if !self.context.enabled {
            return Ok(ToolCallOutcome::error(
                "当前 Host 未启用 AI Office 子 Agent 委派",
            ));
        }
        let input = match serde_json::from_value::<DelegateTaskInput>(args) {
            Ok(input) => input,
            Err(error) => {
                return Ok(ToolCallOutcome::error(format!(
                    "delegate_task 参数无效：{error}"
                )))
            }
        };
        self.context.run(input).await
    }
}

#[async_trait]
impl ToolHostValidator for DelegatingToolHost {
    async fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        if name == "register_artifact" {
            if !self.context.enabled {
                return Err("当前Host未启用Office产物登记".into());
            }
            return artifact_path(input).map(|_| ());
        }
        if name == "delegate_task" {
            if !self.context.enabled {
                return Err("当前 Host 未启用 AI Office 子 Agent 委派".into());
            }
            serde_json::from_value::<DelegateTaskInput>(input.clone())
                .map(|_| ())
                .map_err(|error| format!("delegate_task 参数无效：{error}"))
        } else {
            self.builtin.validate_input(name, input).await
        }
    }

    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        name != "delegate_task" && self.builtin.should_cascade_on_error(name, outcome)
    }
}

struct WorkspaceToolHost {
    builtin: ToolRegistry,
    write_lock: Arc<AsyncMutex<()>>,
}

impl WorkspaceToolHost {
    fn new(workspace: PathBuf, write_lock: Arc<AsyncMutex<()>>) -> Self {
        Self {
            builtin: ToolRegistry::new(workspace),
            write_lock,
        }
    }
}

#[async_trait]
impl ToolExecutor for WorkspaceToolHost {
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.builtin.set_media_context(media);
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.builtin.remember_media(media);
    }
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        self.builtin.list_tools().await
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> openpaw_core::Result<ToolCallOutcome> {
        if matches!(
            name,
            "write_file" | "edit_file" | "notebook_edit" | "bash" | "todo_add" | "todo_update"
        ) {
            let _guard = self.write_lock.lock().await;
            return self.builtin.call(name, args).await;
        }
        self.builtin.call(name, args).await
    }
}

#[async_trait]
impl ToolHostValidator for WorkspaceToolHost {
    async fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        self.builtin.validate_input(name, input).await
    }

    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        self.builtin.should_cascade_on_error(name, outcome)
    }
}

pub(crate) struct SubagentOfficeGuard {
    office: OfficeDispatcher,
    parent_session_id: String,
    workspace_id: String,
    workspace_name: String,
    turn: Mutex<Option<SubagentOfficeTurn>>,
}

impl SubagentOfficeGuard {
    /// 功能：登记本次真实子回合的展示生命周期。
    /// 参数：Office、父会话、兼容子空间身份、显示名及实际工作目录。
    /// 返回：离开作用域即清理的展示 guard。
    /// 错误：展示入队失败不改变引擎执行结果。
    /// 状态：登记子空间并发送一次真实交接。
    /// 并发与取消：同步入队；取消或丢弃时释放展示状态。
    /// 权限：不创建执行权限，不发布任务正文。
    /// 关联：CHG-2026-0034、普通 runtime 子执行。
    pub(crate) fn new(
        office: OfficeDispatcher,
        parent_session_id: String,
        workspace_id: String,
        workspace_name: String,
        workspace: PathBuf,
    ) -> Self {
        let turn = office.subagent_started(
            &parent_session_id,
            &workspace_id,
            &workspace_name,
            &workspace,
        );
        Self {
            office,
            parent_session_id,
            workspace_id,
            workspace_name,
            turn: Mutex::new(Some(turn)),
        }
    }

    /// 功能：投影子回合的实际引擎事件。
    /// 参数：event 为当前子执行的真实事件。
    /// 返回：无。
    /// 错误：展示故障不反向改变执行结果。
    /// 状态：更新当前子空间的工具、文字和产物展示。
    /// 并发与取消：短互斥锁，不等待网络。
    /// 权限：不展示 reasoning，不复制父工具权限。
    /// 关联：CHG-2026-0034。
    pub(crate) fn observe(&self, event: &AgentEvent) {
        if let Some(turn) = self
            .turn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
        {
            self.office
                .observe_subagent(&self.workspace_id, event, turn);
        }
    }

    /// 功能：按已提交的子回合结果结束展示。
    /// 参数：completed 表示实际执行及持久化已成功。
    /// 返回：无。
    /// 错误：展示故障不改变已提交结果。
    /// 状态：最多结束一次并释放场景活动。
    /// 并发与取消：锁内取走 guard；取消不冒充成功。
    /// 权限：只发布真实结果，不重新执行工具。
    /// 关联：CHG-2026-0034。
    pub(crate) fn finish(&self, completed: bool) {
        let turn = self
            .turn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(turn) = turn {
            self.office.subagent_ended(
                &self.parent_session_id,
                &self.workspace_id,
                &self.workspace_name,
                turn,
                completed,
            );
        }
    }
}

impl Drop for SubagentOfficeGuard {
    fn drop(&mut self) {
        let turn = self
            .turn
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(turn) = turn {
            self.office.subagent_ended(
                &self.parent_session_id,
                &self.workspace_id,
                &self.workspace_name,
                turn,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    fn test_host() -> DelegatingToolHost {
        let workspace = tempfile::tempdir().unwrap().keep();
        let (events, _) = broadcast::channel(16);
        let approvals = HostApprovalManager::new("parent", events, CancellationToken::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(openpaw_llm::EchoProvider::new());
        let context = test_context(
            "parent",
            workspace.clone(),
            OfficeDispatcher::new(None),
            approvals,
            provider,
            "echo",
            false,
            CancellationToken::new(),
        );
        DelegatingToolHost::new(workspace, context)
    }

    #[allow(clippy::too_many_arguments)]
    fn test_context(
        parent_session_id: &str,
        workspace: PathBuf,
        office: OfficeDispatcher,
        approvals: Arc<HostApprovalManager>,
        provider: Arc<dyn LlmProvider>,
        model: &str,
        _manual_permissions: bool,
        cancel: CancellationToken,
    ) -> Arc<DelegationContext> {
        let state_file = delegation_state_file(&workspace, parent_session_id);
        let delegations = load_delegations(&state_file, parent_session_id);
        let sequence = delegations.len() as u64;
        let home = tempfile::tempdir().unwrap().keep();
        let mut runtime = crate::runtime::AppState::new(crate::protocol::HostConfig {
            home,
            workspace: workspace.clone(),
            ..Default::default()
        });
        Arc::get_mut(&mut runtime).unwrap().office = office.clone();
        let shared = DelegationShared::with_slots(DEFAULT_GLOBAL_SUBAGENTS);
        shared.bind_runtime(&runtime);
        Arc::new(DelegationContext {
            parent_session_id: parent_session_id.into(),
            workspace,
            office,
            approvals,
            provider_model: RwLock::new(ProviderModel {
                provider,
                model: model.into(),
                cancel,
            }),
            sequence: AtomicU64::new(sequence),
            delegations: Mutex::new(delegations),
            state_file,
            slots: Semaphore::new(MAX_PARALLEL_SUBAGENTS),
            shared,
            _runtime_keeper: Some(runtime),
            enabled: true,
        })
    }

    #[tokio::test]
    async fn tool_list_adds_delegate_without_removing_builtin_tools() {
        let host = test_host();
        let tools = host.list_tools().await.unwrap();
        let builtin = host.builtin.list_tools().await.unwrap();
        assert_eq!(tools.len(), builtin.len() + 2);
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.name == "delegate_task")
                .count(),
            1
        );
        assert!(tools.iter().any(|tool| tool.name == "read_file"));
    }

    #[tokio::test]
    async fn legacy_result_invalidation_preserves_identity_and_propagates_write_failure() {
        let host = test_host();
        host.context.delegations.lock().unwrap().insert(
            "old".into(),
            DelegationRecord {
                workspace_id: "parent:agent:old".into(),
                workspace_name: "review".into(),
                task_fingerprint: "old-task".into(),
                task: "check files".into(),
                completed: Some(CachedDelegationOutcome {
                    content: "known old result".into(),
                    structured: json!({}),
                }),
            },
        );
        host.context.invalidate_results().unwrap();
        let restored = load_delegations(&host.context.state_file, "parent");
        assert_eq!(restored["old"].workspace_id, "parent:agent:old");
        assert!(restored["old"].completed.is_none());
        std::fs::remove_file(&host.context.state_file).unwrap();
        std::fs::create_dir(&host.context.state_file).unwrap();
        assert!(host.context.invalidate_results().is_err());
    }

    #[test]
    fn invalidation_retains_more_than_32_space_identities_after_reload() {
        let host = test_host();
        {
            let mut records = host.context.delegations.lock().unwrap();
            for index in 0..33 {
                records.insert(
                    format!("agent-{index}"),
                    DelegationRecord {
                        workspace_id: format!("parent:agent:{index}"),
                        workspace_name: format!("agent-{index}"),
                        task_fingerprint: format!("task-{index}"),
                        task: format!("task-{index}"),
                        completed: Some(CachedDelegationOutcome {
                            content: "old".into(),
                            structured: json!({}),
                        }),
                    },
                );
            }
        }
        host.context.invalidate_results().unwrap();
        let restored = load_delegations(&host.context.state_file, "parent");
        assert_eq!(restored.len(), 33);
        for index in 0..33 {
            let record = &restored[&format!("agent-{index}")];
            assert_eq!(record.workspace_id, format!("parent:agent:{index}"));
            assert!(record.completed.is_none());
        }
    }

    #[tokio::test]
    async fn artifact_registration_reuses_dispatcher_and_rejects_missing_or_escaping_paths() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("结果.json"), "{}").unwrap();
        let (events, _) = broadcast::channel(8);
        let approvals = HostApprovalManager::new("parent", events, CancellationToken::new());
        let (office, mut actions) = OfficeDispatcher::recording();
        let context = test_context(
            "parent",
            workspace.path().to_path_buf(),
            office,
            approvals,
            Arc::new(openpaw_llm::EchoProvider::new()),
            "echo",
            false,
            CancellationToken::new(),
        );
        let host = DelegatingToolHost::new(workspace.path().to_path_buf(), context);
        let outcome = host
            .call("register_artifact", json!({"path":"结果.json"}))
            .await
            .unwrap();
        assert!(!outcome.is_error);
        assert_eq!(outcome.structured.unwrap()["path"], "结果.json");
        assert!(
            matches!(actions.try_recv().unwrap(), crate::office::OfficeAction::RegisterArtifact { path, workspace_id, .. } if path == "结果.json" && workspace_id == "parent")
        );
        for path in ["../outside.json", "missing.json", "/etc/passwd"] {
            assert!(
                host.call("register_artifact", json!({"path":path}))
                    .await
                    .unwrap()
                    .is_error
            );
        }
        assert_eq!(
            normalized_name(Some("API接口一致性审核"), 1).unwrap(),
            "API接口一致性审核"
        );
    }

    #[tokio::test]
    async fn standalone_host_does_not_expose_office_delegation_tool() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let (events, _) = broadcast::channel(8);
        let approvals = HostApprovalManager::new("standalone", events, CancellationToken::new());
        let context = DelegationContext::new(
            "standalone".into(),
            workspace.clone(),
            OfficeDispatcher::new(None),
            approvals,
            Arc::new(openpaw_llm::EchoProvider::new()),
            "echo".into(),
            DelegationShared::with_slots(2),
        );
        let host = DelegatingToolHost::new(workspace, context);

        assert!(host
            .list_tools()
            .await
            .unwrap()
            .iter()
            .all(|tool| tool.name != "delegate_task"));
    }

    #[tokio::test]
    async fn parent_and_child_writes_share_one_workspace_lock() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let shared = DelegationShared::with_slots(2);
        let parent =
            WorkspaceToolHost::new(workspace.clone(), shared.workspace_write_lock(&workspace));
        let child =
            WorkspaceToolHost::new(workspace.clone(), shared.workspace_write_lock(&workspace));

        let guard = parent.write_lock.lock().await;
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            child.write_lock.lock()
        )
        .await
        .is_err());
        drop(guard);
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            child.write_lock.lock()
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn parent_and_child_providers_share_one_process_limit() {
        let shared = DelegationShared::with_slots(1);
        let first = Arc::new(LimitedProvider {
            inner: Arc::new(openpaw_llm::EchoProvider::new()),
            slots: shared.model_slots.clone(),
        });
        let second = Arc::new(LimitedProvider {
            inner: Arc::new(openpaw_llm::EchoProvider::new()),
            slots: shared.model_slots.clone(),
        });
        let first_stream = first.stream(CompletionRequest::default()).await.unwrap();

        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            second.stream(CompletionRequest::default())
        )
        .await
        .is_err());
        drop(first_stream);
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            second.stream(CompletionRequest::default())
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn delegate_task_runs_a_real_child_agent_and_returns_its_result() {
        let host = test_host();
        let outcome = host
            .call(
                "delegate_task",
                json!({"name":"研究 Agent","task":"整理测试结论"}),
            )
            .await
            .unwrap();
        assert!(!outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.contains("Echo: 整理测试结论"));
        let structured = outcome.structured.unwrap();
        assert_eq!(structured["workspaceName"], "研究 Agent");
        assert_eq!(structured["parentSessionId"], "parent");
        assert_eq!(structured["status"], "completed");
        assert!(structured["workspaceId"]
            .as_str()
            .unwrap()
            .starts_with("parent:agent:"));
    }

    #[tokio::test]
    async fn delegate_names_preserve_model_selected_functional_labels() {
        let host = test_host();
        let audit = host
            .call(
                "delegate_task",
                json!({"name":"audit-review","task":"review release evidence"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let first_unknown = host
            .call(
                "delegate_task",
                json!({"name":"alpha-helper","task":"collect alpha facts"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let second_unknown = host
            .call(
                "delegate_task",
                json!({"name":"beta-helper","task":"implement event streaming cancellation semantics"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();

        assert_eq!(audit["workspaceName"], "audit-review");
        assert_eq!(first_unknown["workspaceName"], "alpha-helper");
        assert_eq!(second_unknown["workspaceName"], "beta-helper");
        assert_ne!(
            first_unknown["workspaceName"],
            second_unknown["workspaceName"]
        );
    }

    #[tokio::test]
    async fn legacy_workspace_identity_and_missing_transcript_are_preserved_without_reuse() {
        let host = test_host();
        save_delegations(
            &host.context.state_file,
            &PersistedDelegationState {
                version: 2,
                records: vec![PersistedDelegationRecord {
                    logical_key: "review".into(),
                    workspace_id: "parent:agent:old".into(),
                    workspace_name: "历史审核".into(),
                    task_fingerprint: "old".into(),
                    task: "历史任务".into(),
                    completed: Some(CachedDelegationOutcome {
                        content: "可证实的旧结果".into(),
                        structured: json!({"workspaceId":"parent:agent:old"}),
                    }),
                }],
            },
        )
        .unwrap();
        let restored = load_delegations(&host.context.state_file, "parent");
        assert_eq!(restored["review"].workspace_name, "历史审核");
        assert_eq!(
            restored["review"].completed.as_ref().unwrap().content,
            "可证实的旧结果"
        );
        assert!(
            !host
                .context
                .context_prompt()
                .await
                .unwrap_or_default()
                .contains("可证实的旧结果"),
            "旧答案不得冒充持久子历史"
        );
    }

    #[tokio::test]
    async fn same_name_without_explicit_identity_creates_distinct_children() {
        let host = test_host();
        let first = host
            .call(
                "delegate_task",
                json!({"name":"交通信息调研","task":"整理交通信息"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let second = host
            .call(
                "delegate_task",
                json!({"name":" 交通信息调研 ","task":"补充实时交通限制"}),
            )
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        assert_ne!(
            first["workspaceId"],
            second.structured.unwrap()["workspaceId"]
        );
    }

    #[tokio::test]
    async fn similar_names_never_merge_new_child_identities() {
        let host = test_host();
        let first = host
            .call(
                "delegate_task",
                json!({"name":"安全规范调研 Agent","task":"收集系统安全规范和约束"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let second = host
            .call(
                "delegate_task",
                json!({"name":"安全规范研究助手","task":"补充系统安全规范与限制说明"}),
            )
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        assert!(second.content.contains("补充系统安全规范与限制说明"));
        let second = second.structured.unwrap();
        assert_ne!(first["workspaceId"], second["workspaceId"]);
        assert_eq!(second["workspaceName"], "安全规范研究助手");
    }

    #[tokio::test]
    async fn unrelated_capabilities_keep_distinct_workspaces() {
        let host = test_host();
        let first = host
            .call(
                "delegate_task",
                json!({"name":"接口设计 Agent","task":"设计服务接口和字段"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let second = host
            .call(
                "delegate_task",
                json!({"name":"发布审核 Agent","task":"检查发布门禁和验收证据"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        assert_ne!(first["workspaceId"], second["workspaceId"]);
    }

    #[tokio::test]
    async fn identical_task_is_new_execution_in_a_distinct_child() {
        let host = test_host();
        let first = host
            .call(
                "delegate_task",
                json!({"name":"美食调研 A","task":"整理本地美食建议"}),
            )
            .await
            .unwrap();
        let second = host
            .call(
                "delegate_task",
                json!({"name":"美食调研 B","task":"整理本地美食建议"}),
            )
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        assert_ne!(
            first.structured.unwrap()["workspaceId"],
            second.structured.unwrap()["workspaceId"]
        );
    }

    #[tokio::test]
    async fn parallel_delegations_receive_distinct_child_workspaces() {
        let host = Arc::new(test_host());
        let first = {
            let host = host.clone();
            tokio::spawn(async move {
                host.call("delegate_task", json!({"task":"任务 A"}))
                    .await
                    .unwrap()
            })
        };
        let second = {
            let host = host.clone();
            tokio::spawn(async move {
                host.call("delegate_task", json!({"task":"任务 B"}))
                    .await
                    .unwrap()
            })
        };
        let first = first.await.unwrap().structured.unwrap();
        let second = second.await.unwrap().structured.unwrap();
        assert_ne!(first["workspaceId"], second["workspaceId"]);
        assert_eq!(first["parentSessionId"], "parent");
        assert_eq!(second["parentSessionId"], "parent");
    }

    #[tokio::test]
    async fn fifth_parallel_delegation_waits_for_a_parent_session_slot() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let (events, _) = broadcast::channel(16);
        let approvals = HostApprovalManager::new("parent", events, CancellationToken::new());
        let cancel = CancellationToken::new();
        let (office, mut actions) = OfficeDispatcher::recording();
        let context = test_context(
            "parent",
            workspace.clone(),
            office,
            approvals,
            Arc::new(openpaw_llm::MockProvider::stream_text_delayed(
                "仍在执行的子任务",
                100,
                20,
            )),
            "mock-1",
            false,
            cancel.clone(),
        );
        let host = Arc::new(DelegatingToolHost::new(workspace, context));
        let mut running = Vec::new();
        for index in 0..5 {
            let host = host.clone();
            running.push(tokio::spawn(async move {
                host.call("delegate_task", json!({"task":format!("任务 {index}")}))
                    .await
                    .unwrap()
            }));
        }

        let mut started = HashSet::new();
        while started.len() < MAX_PARALLEL_SUBAGENTS {
            let action = tokio::time::timeout(std::time::Duration::from_secs(1), actions.recv())
                .await
                .expect("parallel child start timeout")
                .expect("office action channel");
            if let crate::office::OfficeAction::SetState {
                workspace_id,
                roster_no: 1,
                state,
                ..
            } = action
            {
                if state == "thinking" {
                    started.insert(workspace_id);
                }
            }
        }
        assert_eq!(started.len(), MAX_PARALLEL_SUBAGENTS);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), actions.recv())
                .await
                .is_err()
        );

        cancel.cancel();
        for task in running {
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .expect("cancelled delegation timeout")
                .expect("delegate task");
            assert!(outcome.is_error);
            assert_eq!(outcome.structured.unwrap()["status"], "cancelled");
        }
    }

    #[tokio::test]
    async fn parent_cancel_stops_child_and_releases_child_office_state() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let (events, _) = broadcast::channel(16);
        let approvals = HostApprovalManager::new("parent", events, CancellationToken::new());
        let cancel = CancellationToken::new();
        let (office, mut actions) = OfficeDispatcher::recording();
        let context = test_context(
            "parent",
            workspace.clone(),
            office,
            approvals,
            Arc::new(openpaw_llm::MockProvider::stream_text_delayed(
                "仍在执行的子任务",
                100,
                20,
            )),
            "mock-1",
            false,
            cancel.clone(),
        );
        let host = Arc::new(DelegatingToolHost::new(workspace, context));
        let running = {
            let host = host.clone();
            tokio::spawn(async move {
                host.call(
                    "delegate_task",
                    json!({"name":"研究 Agent","task":"长时间研究"}),
                )
                .await
                .unwrap()
            })
        };

        loop {
            let action = tokio::time::timeout(std::time::Duration::from_secs(1), actions.recv())
                .await
                .expect("child office start timeout")
                .expect("office action channel");
            if matches!(
                action,
                crate::office::OfficeAction::SetState {
                    ref workspace_id,
                    roster_no: 1,
                    ref state,
                    ..
                } if workspace_id.starts_with("parent:agent:") && state == "thinking"
            ) {
                break;
            }
        }
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), running)
            .await
            .expect("cancelled child must stop promptly")
            .expect("delegate task");
        assert!(outcome.is_error);
        assert_eq!(outcome.structured.unwrap()["status"], "cancelled");

        let mut child_idle = false;
        let mut returning = false;
        while !(child_idle && returning) {
            let action = tokio::time::timeout(std::time::Duration::from_secs(1), actions.recv())
                .await
                .expect("child office cleanup timeout")
                .expect("office action channel");
            match action {
                crate::office::OfficeAction::SetState {
                    workspace_id,
                    roster_no: 1,
                    state,
                    ..
                } if workspace_id.starts_with("parent:agent:") && state == "idle" => {
                    child_idle = true
                }
                crate::office::OfficeAction::WorkspaceHandoff {
                    from_workspace_id,
                    to_workspace_id,
                    phase,
                    ..
                } if from_workspace_id.starts_with("parent:agent:")
                    && to_workspace_id == "parent"
                    && phase == "returning" =>
                {
                    returning = true
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn successful_child_write_registers_artifact_for_its_workspace() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let (events, _) = broadcast::channel(16);
        let approvals =
            HostApprovalManager::new("parent-artifact", events, CancellationToken::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(openpaw_llm::MockProvider::scripted(vec![
            openpaw_llm::MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"reports/result.md","content":"private body"}),
                text: "准备写入".into(),
            },
            openpaw_llm::MockMode::Fixed("产物已完成".into()),
        ]));
        let (office, mut actions) = OfficeDispatcher::recording();
        let context = test_context(
            "parent-artifact",
            workspace.clone(),
            office,
            approvals,
            provider,
            "mock-1",
            false,
            CancellationToken::new(),
        );
        let outcome = DelegatingToolHost::new(workspace.clone(), context)
            .call("delegate_task", json!({"task":"生成结果文档"}))
            .await
            .unwrap();

        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(
            std::fs::read_to_string(workspace.join("reports/result.md")).unwrap(),
            "private body"
        );
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            crate::office::OfficeAction::RegisterArtifact {
                session_id,
                workspace_id,
                path,
            } if session_id == "parent-artifact"
                && workspace_id.starts_with("parent-artifact:agent:")
                && path == "reports/result.md"
        )));
    }

    #[tokio::test]
    async fn concurrent_parent_sessions_keep_different_child_counts_isolated() {
        let make_host = |parent: &str| {
            let workspace = tempfile::tempdir().unwrap().keep();
            let (events, _) = broadcast::channel(16);
            let approvals = HostApprovalManager::new(parent, events, CancellationToken::new());
            let context = test_context(
                parent,
                workspace.clone(),
                OfficeDispatcher::new(None),
                approvals,
                Arc::new(openpaw_llm::EchoProvider::new()),
                "echo",
                false,
                CancellationToken::new(),
            );
            Arc::new(DelegatingToolHost::new(workspace, context))
        };
        let first = make_host("parent-a");
        let second = make_host("parent-b");
        let mut tasks = Vec::new();
        for index in 0..2 {
            let host = first.clone();
            tasks.push(tokio::spawn(async move {
                host.call("delegate_task", json!({"task":format!("A-{index}")}))
                    .await
                    .unwrap()
                    .structured
                    .unwrap()
            }));
        }
        for index in 0..3 {
            let host = second.clone();
            tasks.push(tokio::spawn(async move {
                host.call("delegate_task", json!({"task":format!("B-{index}")}))
                    .await
                    .unwrap()
                    .structured
                    .unwrap()
            }));
        }

        let mut first_ids = Vec::new();
        let mut second_ids = Vec::new();
        for task in tasks {
            let result = task.await.unwrap();
            match result["parentSessionId"].as_str().unwrap() {
                "parent-a" => first_ids.push(result["workspaceId"].as_str().unwrap().to_string()),
                "parent-b" => second_ids.push(result["workspaceId"].as_str().unwrap().to_string()),
                parent => panic!("unexpected parent session: {parent}"),
            }
        }
        first_ids.sort();
        second_ids.sort();
        assert_eq!(first_ids.len(), 2);
        assert!(first_ids
            .iter()
            .any(|id| id.starts_with("parent-a:agent:1-")));
        assert!(first_ids
            .iter()
            .any(|id| id.starts_with("parent-a:agent:2-")));
        assert_eq!(second_ids.len(), 3);
        assert!(second_ids
            .iter()
            .any(|id| id.starts_with("parent-b:agent:1-")));
        assert!(second_ids
            .iter()
            .any(|id| id.starts_with("parent-b:agent:2-")));
        assert!(second_ids
            .iter()
            .any(|id| id.starts_with("parent-b:agent:3-")));
    }

    #[tokio::test]
    async fn recreated_parent_context_does_not_reuse_a_child_workspace_id() {
        let make_host = || {
            let workspace = tempfile::tempdir().unwrap().keep();
            let (events, _) = broadcast::channel(8);
            let approvals =
                HostApprovalManager::new("restart-parent", events, CancellationToken::new());
            let context = test_context(
                "restart-parent",
                workspace.clone(),
                OfficeDispatcher::new(None),
                approvals,
                Arc::new(openpaw_llm::EchoProvider::new()),
                "echo",
                false,
                CancellationToken::new(),
            );
            DelegatingToolHost::new(workspace, context)
        };

        let before_restart = make_host()
            .call("delegate_task", json!({"task":"重启前任务"}))
            .await
            .unwrap()
            .structured
            .unwrap()["workspaceId"]
            .as_str()
            .unwrap()
            .to_string();
        let after_restart = make_host()
            .call("delegate_task", json!({"task":"重启后任务"}))
            .await
            .unwrap()
            .structured
            .unwrap()["workspaceId"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(before_restart.starts_with("restart-parent:agent:1-"));
        assert!(after_restart.starts_with("restart-parent:agent:1-"));
        assert_ne!(before_restart, after_restart);
    }

    #[tokio::test]
    async fn invalid_delegate_input_is_a_tool_error_not_a_host_failure() {
        let outcome = test_host()
            .call("delegate_task", json!({"name":"缺少任务"}))
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("参数无效"));
    }
}
