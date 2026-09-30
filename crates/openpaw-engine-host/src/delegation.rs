//! 功能：在 engine-host 兼容层提供真实子 Agent 委派工具。
//! 边界：复用 OpenPaw AgentLoop 和内置工具，不修改核心 crate，也不允许子 Agent 递归委派。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, Message, Role,
    Session, StreamEvent, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor, ToolHost,
    ToolHostValidator, ToolSpec,
};
use openpaw_engine::{AgentEvent, AgentLoop};
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
const MAX_PERSISTED_DELEGATIONS: usize = 32;

pub(crate) struct DelegationShared {
    model_slots: Arc<Semaphore>,
    workspace_write_locks: Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>,
}

impl DelegationShared {
    pub(crate) fn from_env() -> Arc<Self> {
        let slots = std::env::var("OPENPAW_ENGINE_MAX_MODEL_REQUESTS")
            .or_else(|_| std::env::var("OPENPAW_ENGINE_MAX_SUBAGENTS"))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=32).contains(value))
            .unwrap_or(DEFAULT_GLOBAL_SUBAGENTS);
        Arc::new(Self {
            model_slots: Arc::new(Semaphore::new(slots)),
            workspace_write_locks: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_slots(slots: usize) -> Arc<Self> {
        Arc::new(Self {
            model_slots: Arc::new(Semaphore::new(slots)),
            workspace_write_locks: Mutex::new(HashMap::new()),
        })
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
struct DelegateTaskInput {
    task: String,
    #[serde(default)]
    name: Option<String>,
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
    manual_permissions: bool,
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
    running: bool,
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
    completed: CachedDelegationOutcome,
}

impl DelegationContext {
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
            manual_permissions: std::env::var("OPENPAW_ENGINE_PERMISSIONS").as_deref()
                == Ok("manual"),
            enabled,
        })
    }

    /// 同一父 session 每轮执行前更新 provider/model，使子 Agent 跟随请求级覆盖值。
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

    pub(crate) fn limit_provider(&self, provider: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
        Arc::new(LimitedProvider {
            inner: provider,
            slots: self.shared.model_slots.clone(),
        })
    }

    async fn run(&self, input: DelegateTaskInput) -> openpaw_core::Result<ToolCallOutcome> {
        let task = input.task.trim();
        if task.is_empty() || task.chars().count() > MAX_DELEGATE_TASK_CHARS {
            return Ok(ToolCallOutcome::error(format!(
                "task 必须为 1..={MAX_DELEGATE_TASK_CHARS} 个字符"
            )));
        }
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let has_explicit_name = input
            .name
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let requested_name = normalized_name(input.name.as_deref(), sequence)?;
        let requested_logical_key = logical_agent_key(&requested_name);
        let task_fingerprint = normalized_task_fingerprint(task);
        let (workspace_id, name, logical_key) = {
            let mut records = self
                .delegations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some((_, record)) = records
                .iter_mut()
                .find(|(_, record)| record.task_fingerprint == task_fingerprint)
            {
                if record.running {
                    return Ok(ToolCallOutcome::error(format!(
                        "相同子任务已由“{}”执行中，请等待并复用结果，不要重复创建空间。",
                        record.workspace_name
                    )));
                }
                if let Some(cached) = &record.completed {
                    return Ok(ToolCallOutcome {
                        content: cached.content.clone(),
                        is_error: false,
                        structured: Some(cached.structured.clone()),
                    });
                }
            }
            let logical_key = if records.contains_key(&requested_logical_key) {
                requested_logical_key.clone()
            } else if has_explicit_name {
                best_reusable_delegation(&records, &requested_name, task)
                    .unwrap_or_else(|| requested_logical_key.clone())
            } else {
                requested_logical_key.clone()
            };
            if let Some(record) = records.get_mut(&logical_key) {
                if record.running {
                    return Ok(ToolCallOutcome::error(format!(
                        "子 Agent“{}”已有任务执行中；请合并职责或等待完成后继续使用该 Agent。",
                        record.workspace_name
                    )));
                }
                record.task_fingerprint = task_fingerprint.clone();
                record.task = task.to_string();
                record.running = true;
                record.completed = None;
                (
                    record.workspace_id.clone(),
                    record.workspace_name.clone(),
                    logical_key,
                )
            } else {
                let workspace_id = if has_explicit_name {
                    format!(
                        "{}:agent:{:016x}",
                        self.parent_session_id,
                        stable_logical_hash(&requested_logical_key)
                    )
                } else {
                    format!(
                        "{}:agent:{sequence}-{}",
                        self.parent_session_id,
                        Uuid::new_v4().simple()
                    )
                };
                records.insert(
                    requested_logical_key.clone(),
                    DelegationRecord {
                        workspace_id: workspace_id.clone(),
                        workspace_name: requested_name.clone(),
                        task_fingerprint: task_fingerprint.clone(),
                        task: task.to_string(),
                        running: true,
                        completed: None,
                    },
                );
                (workspace_id, requested_name.clone(), requested_logical_key)
            }
        };
        let (provider, model, cancel) = {
            let provider_model = self
                .provider_model
                .read()
                .unwrap_or_else(|error| error.into_inner());
            (
                provider_model.provider.clone(),
                provider_model.model.clone(),
                provider_model.cancel.clone(),
            )
        };
        let _session_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(cancelled_outcome(&workspace_id, &name, &self.parent_session_id)),
            permit = self.slots.acquire() => permit
                .map_err(|_| Error::ToolHost("子 Agent 调度器已关闭".into()))?,
        };
        let child_tools = Arc::new(WorkspaceToolHost::new(
            self.workspace.clone(),
            self.shared.workspace_write_lock(&self.workspace),
        ));
        let tools = child_tools.list_tools().await?;
        let tool_host: Arc<dyn ToolHost> = child_tools;
        let mut agent = AgentLoop::new(provider.clone())
            .with_model(model.clone())
            .with_tool_host(tool_host)
            .with_stream_timeout(crate::runtime::engine_stream_timeout())
            .with_approval_manager(self.approvals.clone());
        if self.manual_permissions {
            agent = agent.with_guardrail(Arc::new(ChildManualGuardrail));
        }
        if let Ok(value) = std::env::var("OPENPAW_MAX_OUTPUT_TOKENS") {
            if let Ok(tokens) = value.trim().parse::<u32>() {
                if tokens > 0 {
                    agent = agent.with_max_tokens(tokens);
                }
            }
        }

        let mut session = Session::new(model.clone(), provider.name());
        session.meta.id = workspace_id.clone();
        session.parent_session_id = Some(self.parent_session_id.clone());
        session.agent_type = Some("fork".into());
        session.messages.push(Message::user_text(task));
        let office = Arc::new(SubagentOfficeGuard::new(
            self.office.clone(),
            self.parent_session_id.clone(),
            workspace_id.clone(),
            name.clone(),
            self.workspace.clone(),
        ));
        let sink_office = office.clone();
        let mut sink = move |event: AgentEvent| sink_office.observe(&event);
        let system = Some(
            "你是主 Agent 委派的独立子 Agent。只完成当前子任务，必要时使用工具核实或生成产物；不要尝试再次委派。最终给出可供主 Agent 直接整合的清晰结果。"
                .to_string(),
        );
        let result = agent
            .run_turn_with_provider_and_cancel_token(
                &mut session,
                system,
                &tools,
                &mut sink,
                cancel.clone(),
                (provider, model),
            )
            .await;
        let cancelled = cancel.is_cancelled();
        let completed = result.is_ok() && !cancelled;
        office.finish(completed);
        if cancelled {
            self.finish_delegation(&logical_key, &task_fingerprint, None);
            return Ok(cancelled_outcome(
                &workspace_id,
                &name,
                &self.parent_session_id,
            ));
        }
        if let Err(error) = result {
            self.finish_delegation(&logical_key, &task_fingerprint, None);
            return Ok(ToolCallOutcome {
                content: format!("子 Agent“{name}”执行失败：{error}"),
                is_error: true,
                structured: Some(json!({
                    "workspaceId": workspace_id,
                    "workspaceName": name,
                    "parentSessionId": self.parent_session_id,
                    "status": "failed"
                })),
            });
        }
        let text = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(Message::text)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "子 Agent 已完成任务，但没有返回文本结果。".into());
        let structured = json!({
            "workspaceId": workspace_id.clone(),
            "workspaceName": name.clone(),
            "parentSessionId": self.parent_session_id,
            "status": "completed",
            "result": text.clone()
        });
        let outcome = ToolCallOutcome {
            content: format!("子 Agent“{name}”已完成：\n{text}"),
            is_error: false,
            structured: Some(structured.clone()),
        };
        self.finish_delegation(
            &logical_key,
            &task_fingerprint,
            Some(CachedDelegationOutcome {
                content: outcome.content.clone(),
                structured,
            }),
        );
        Ok(outcome)
    }

    fn finish_delegation(
        &self,
        logical_key: &str,
        task_fingerprint: &str,
        completed: Option<CachedDelegationOutcome>,
    ) {
        let snapshot = {
            let mut records = self
                .delegations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(record) = records.get_mut(logical_key) {
                if record.task_fingerprint == task_fingerprint {
                    record.running = false;
                    record.completed = completed;
                }
            }
            persisted_delegation_state(&records)
        };
        if let Err(error) = save_delegations(&self.state_file, &snapshot) {
            tracing::warn!(%error, "无法持久化子 Agent 委派状态");
        }
    }

    pub(crate) fn context_prompt(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let records = self
            .delegations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let available = records
            .values()
            .filter(|record| record.completed.is_some())
            .map(|record| format!("- name={}；既有任务={}", record.workspace_name, record.task))
            .collect::<Vec<_>>();
        (!available.is_empty()).then(|| {
            format!(
                "本会话已有以下已完成子 Agent，可继续复用：\n{}\n优先继续使用既有 name；完全相同任务会直接返回缓存，高置信的相似职责和任务会复用既有空间。不要为近义职责重复创建空间。",
                available.join("\n")
            )
        })
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

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
}

fn cancelled_outcome(workspace_id: &str, name: &str, parent_session_id: &str) -> ToolCallOutcome {
    ToolCallOutcome {
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
    let fallback = format!("子 Agent {sequence}");
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

fn logical_agent_key(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalized_task_fingerprint(task: &str) -> String {
    task.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn normalized_agent_capability(name: &str) -> String {
    let mut value = name
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    const SUFFIXES: &[&str] = &[
        "子agent",
        "agent",
        "智能体",
        "助手",
        "专家",
        "工作室",
        "空间",
        "调研",
        "研究",
        "分析",
        "收集",
        "整理",
        "规划",
        "策划",
    ];
    while let Some(suffix) = SUFFIXES.iter().find(|suffix| value.ends_with(**suffix)) {
        value.truncate(value.len() - suffix.len());
    }
    let chars = value.chars().collect::<Vec<_>>();
    let trailing_ascii = chars
        .iter()
        .rev()
        .take_while(|character| character.is_ascii_alphanumeric())
        .count();
    if (1..=2).contains(&trailing_ascii)
        && chars.len() > trailing_ascii
        && !chars[chars.len() - trailing_ascii - 1].is_ascii()
    {
        value = chars[..chars.len() - trailing_ascii].iter().collect();
    }
    value
}

fn normalized_task_similarity_text(task: &str) -> String {
    let mut value = task
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    const GENERIC_ACTIONS: &[&str] = &[
        "请", "帮忙", "负责", "进行", "完成", "整理", "收集", "调研", "研究", "分析", "补充",
        "生成", "输出", "提供", "汇总", "查找", "搜索", "撰写", "编写", "相关",
    ];
    for action in GENERIC_ACTIONS {
        value = value.replace(action, "");
    }
    value
}

fn character_bigrams(value: &str) -> HashSet<(char, char)> {
    let chars = value.chars().collect::<Vec<_>>();
    chars.windows(2).map(|pair| (pair[0], pair[1])).collect()
}

fn common_prefix_chars(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn text_similarity_score(left: &str, right: &str) -> u16 {
    if left.is_empty() || right.is_empty() {
        return 0;
    }
    if left == right {
        return 1_000;
    }
    let left_chars = left.chars().count();
    let right_chars = right.chars().count();
    let shorter = left_chars.min(right_chars);
    let mut score = if shorter >= 2 && (left.contains(right) || right.contains(left)) {
        880
    } else {
        0
    };
    let left_bigrams = character_bigrams(left);
    let right_bigrams = character_bigrams(right);
    if !left_bigrams.is_empty() && !right_bigrams.is_empty() {
        let overlap = left_bigrams.intersection(&right_bigrams).count();
        score = score.max(((2_000 * overlap) / (left_bigrams.len() + right_bigrams.len())) as u16);
    }
    let prefix = common_prefix_chars(left, right);
    if prefix >= 2 {
        score = score.max((550 + prefix.min(6) * 50) as u16);
    }
    score.min(1_000)
}

fn reusable_delegation_score(
    record: &DelegationRecord,
    requested_name: &str,
    task: &str,
) -> Option<u16> {
    let name_score = text_similarity_score(
        &normalized_agent_capability(&record.workspace_name),
        &normalized_agent_capability(requested_name),
    );
    let task_score = text_similarity_score(
        &normalized_task_similarity_text(&record.task),
        &normalized_task_similarity_text(task),
    );
    if name_score == 1_000 {
        return Some(800 + task_score / 5);
    }
    if name_score >= 850 && task_score >= 120 {
        return Some((name_score * 7 + task_score * 3) / 10);
    }
    if name_score >= 620 && task_score >= 450 {
        return Some((name_score * 6 + task_score * 4) / 10);
    }
    (task_score >= 920).then_some(650 + name_score / 4)
}

fn best_reusable_delegation(
    records: &HashMap<String, DelegationRecord>,
    requested_name: &str,
    task: &str,
) -> Option<String> {
    records
        .iter()
        .filter_map(|(logical_key, record)| {
            reusable_delegation_score(record, requested_name, task)
                .map(|score| (logical_key, score))
        })
        .max_by(|(left_key, left_score), (right_key, right_score)| {
            left_score
                .cmp(right_score)
                .then_with(|| right_key.cmp(left_key))
        })
        .map(|(logical_key, _)| logical_key.clone())
}

fn stable_logical_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

fn delegation_state_file(workspace: &std::path::Path, parent_session_id: &str) -> PathBuf {
    workspace
        .join(".openpaw")
        .join("engine-host-delegations")
        .join(format!("{parent_session_id}.json"))
}

/// 删除会话对应的持久化委派快照；文件不存在视为已清理。
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
    if state.version != 1 {
        return HashMap::new();
    }
    state
        .records
        .into_iter()
        .take(MAX_PERSISTED_DELEGATIONS)
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
        .map(|record| {
            (
                record.logical_key,
                DelegationRecord {
                    workspace_id: record.workspace_id,
                    workspace_name: record.workspace_name,
                    task_fingerprint: record.task_fingerprint,
                    task: record.task,
                    running: false,
                    completed: Some(record.completed),
                },
            )
        })
        .collect()
}

fn persisted_delegation_state(
    records: &HashMap<String, DelegationRecord>,
) -> PersistedDelegationState {
    PersistedDelegationState {
        version: 1,
        records: records
            .iter()
            .filter_map(|(logical_key, record)| {
                record
                    .completed
                    .clone()
                    .map(|completed| PersistedDelegationRecord {
                        logical_key: logical_key.clone(),
                        workspace_id: record.workspace_id.clone(),
                        workspace_name: record.workspace_name.clone(),
                        task_fingerprint: record.task_fingerprint.clone(),
                        task: record.task.clone(),
                        completed,
                    })
            })
            .take(MAX_PERSISTED_DELEGATIONS)
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
    std::fs::write(&temporary, serde_json::to_vec(state)?)?;
    if file.exists() {
        std::fs::remove_file(file)?;
    }
    std::fs::rename(temporary, file)
}

fn delegate_spec() -> ToolSpec {
    ToolSpec {
        name: "delegate_task".into(),
        description: "把一个清晰、可独立完成的子任务交给真实子 Agent，并等待其结果。委派前先合并重复或高度重叠的职责；同一逻辑 Agent 使用稳定且唯一的 name，后续工作继续复用该 name。适合并行研究、设计、实现或审核；简单任务不要调用。".into(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["task"],
            "properties": {
                "task": { "type": "string", "minLength": 1, "maxLength": MAX_DELEGATE_TASK_CHARS },
                "name": { "type": "string", "minLength": 1, "maxLength": MAX_DELEGATE_NAME_CHARS }
            }
        }),
        category: Some(ToolCategory::Communication),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        max_result_chars: Some(24_000),
        ..ToolSpec::default()
    }
}

/// 父 Agent 使用的组合 ToolHost；除 delegate_task 外全部调用仍交给原 ToolRegistry。
pub(crate) struct DelegatingToolHost {
    builtin: WorkspaceToolHost,
    context: Arc<DelegationContext>,
}

impl DelegatingToolHost {
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
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        let mut tools = self.builtin.list_tools().await?;
        if self.context.enabled {
            tools.push(delegate_spec());
        }
        Ok(tools)
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> openpaw_core::Result<ToolCallOutcome> {
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

struct SubagentOfficeGuard {
    office: OfficeDispatcher,
    parent_session_id: String,
    workspace_id: String,
    workspace_name: String,
    turn: Mutex<Option<SubagentOfficeTurn>>,
}

impl SubagentOfficeGuard {
    fn new(
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

    fn observe(&self, event: &AgentEvent) {
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

    fn finish(&self, completed: bool) {
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

struct ChildManualGuardrail;

#[async_trait]
impl openpaw_core::ToolGuardrail for ChildManualGuardrail {
    async fn check(
        &self,
        name: &str,
        _args: &serde_json::Value,
    ) -> openpaw_core::GuardrailDecision {
        openpaw_core::GuardrailDecision::Confirm(format!(
            "manual approval required for child tool `{name}`"
        ))
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
        manual_permissions: bool,
        cancel: CancellationToken,
    ) -> Arc<DelegationContext> {
        let state_file = delegation_state_file(&workspace, parent_session_id);
        let delegations = load_delegations(&state_file, parent_session_id);
        let sequence = delegations.len() as u64;
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
            shared: DelegationShared::with_slots(DEFAULT_GLOBAL_SUBAGENTS),
            manual_permissions,
            enabled: true,
        })
    }

    #[tokio::test]
    async fn tool_list_adds_delegate_without_removing_builtin_tools() {
        let host = test_host();
        let tools = host.list_tools().await.unwrap();
        assert_eq!(tools.len(), 22);
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
    async fn repeated_logical_agent_reuses_one_workspace_across_turns() {
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
        assert_eq!(
            first["workspaceId"],
            second.structured.unwrap()["workspaceId"]
        );
    }

    #[tokio::test]
    async fn similar_capability_reuses_existing_workspace_for_a_new_task() {
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
        assert_eq!(first["workspaceId"], second["workspaceId"]);
        assert_eq!(second["workspaceName"], "安全规范调研 Agent");
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
    async fn identical_task_reuses_completed_result_instead_of_creating_a_space() {
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
        assert_eq!(
            first.structured.unwrap()["workspaceId"],
            second.structured.unwrap()["workspaceId"]
        );
    }

    #[tokio::test]
    async fn completed_delegation_survives_parent_context_recreation() {
        let workspace = tempfile::tempdir().unwrap().keep();
        let make_host = || {
            let (events, _) = broadcast::channel(8);
            let approvals =
                HostApprovalManager::new("persistent-parent", events, CancellationToken::new());
            let context = test_context(
                "persistent-parent",
                workspace.clone(),
                OfficeDispatcher::new(None),
                approvals,
                Arc::new(openpaw_llm::EchoProvider::new()),
                "echo",
                false,
                CancellationToken::new(),
            );
            Arc::new(DelegatingToolHost::new(workspace.clone(), context))
        };
        let first = make_host()
            .call(
                "delegate_task",
                json!({"name":"交通住宿","task":"整理交通和住宿信息"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        let recreated = make_host();
        assert!(recreated
            .context
            .context_prompt()
            .unwrap()
            .contains("name=交通住宿"));
        let replay = recreated
            .call(
                "delegate_task",
                json!({"name":"另一个近义名称","task":"整理交通和住宿信息"}),
            )
            .await
            .unwrap()
            .structured
            .unwrap();
        assert_eq!(first["workspaceId"], replay["workspaceId"]);
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
    async fn child_manual_approval_uses_parent_manager_and_clears_pending() {
        let workspace = tempfile::tempdir().unwrap().keep();
        std::fs::write(workspace.join("README.md"), "approved child read").unwrap();
        let (events, mut event_rx) = broadcast::channel(16);
        let approvals = HostApprovalManager::new("parent-manual", events, CancellationToken::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(openpaw_llm::MockProvider::scripted(vec![
            openpaw_llm::MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: json!({"path":"README.md"}),
                text: "准备读取".into(),
            },
            openpaw_llm::MockMode::Fixed("子 Agent 已读取文件".into()),
        ]));
        let context = test_context(
            "parent-manual",
            workspace.clone(),
            OfficeDispatcher::new(None),
            approvals.clone(),
            provider,
            "mock-1",
            true,
            CancellationToken::new(),
        );
        let host = Arc::new(DelegatingToolHost::new(workspace, context));
        let running = {
            let host = host.clone();
            tokio::spawn(async move {
                host.call("delegate_task", json!({"task":"读取 README"}))
                    .await
                    .unwrap()
            })
        };

        let asked = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("permission asked timeout")
            .expect("permission event");
        assert_eq!(asked.event_type, "permission.asked");
        assert_eq!(asked.properties["sessionID"], "parent-manual");
        assert_eq!(asked.properties["permission"], "read_file");
        let pending = approvals.list().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].permission, "read_file");
        assert!(
            approvals
                .reply(&pending[0].id, true, "approved in test".into())
                .await
        );

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), running)
            .await
            .expect("approved child completion timeout")
            .expect("delegate task");
        assert!(!outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.contains("子 Agent 已读取文件"));
        assert!(approvals.list().await.is_empty());
        let replied = event_rx.recv().await.expect("permission replied event");
        assert_eq!(replied.event_type, "permission.replied");
        assert_eq!(replied.properties["sessionID"], "parent-manual");
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
