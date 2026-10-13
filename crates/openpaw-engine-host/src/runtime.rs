//! 功能：管理每个 session 的 AgentLoop、取消、审批、事件广播和 JSONL 写入。
//! 输入输出边界：接收 HTTP 层的回合请求，向 events/office 输出事件和动作。
//! 设计约束：每个 session 独占一个活动回合；不同 session 可以并行；本模块拥有 session 写入权。
//! 禁止承担：不定义 OpenCode 路由，不让 Office 失败改变 AgentLoop 结果。
//! 关联：`REQ-B-HOST-002`、`REQ-B-HOST-006`、`REQ-B-HOST-007`。
use crate::approval::HostApprovalManager;
use crate::events;
use crate::message_projection;
use crate::office::OfficeDispatcher;
use crate::protocol::{
    unix_millis, HostConfig, OpenCodeEvent, OpenCodeTurnErrorV1, RequestAdmission, SessionSummary,
    SessionTime, SessionWire, TurnAcceptV1, TurnError, TurnOutcome, TurnSchedulerConfig, TurnState,
    UpdateSessionRequest,
};
use crate::session_mutation::{
    clone_messages_for_fork, failed_message_prefix_len, normalize_revert_boundary,
    visible_prefix_len, SessionForkOriginV1, SessionMetadataV1, SessionMutationEvent,
    SessionMutationProjection, SessionRevertCommittedV1, SessionRevertStagedV1,
};
use openpaw_core::{LlmProvider, Session, SessionEvent};
use openpaw_engine::scheduler::{ScheduleError, TurnAdmission, TurnScheduler};
use openpaw_engine::turn_execution::{self, SessionCheckpoint, TurnOptions};
use openpaw_engine::AgentLoop;
use openpaw_store::journal;
use openpaw_store::transcript::retain_turn_state_for_messages;
use openpaw_store::{SessionError, SessionReader, SessionWriter};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
struct HostLifecycleState {
    accepting_turns: bool,
    in_flight_admissions: usize,
    next_task_id: u64,
    tasks: HashMap<u64, tokio::task::AbortHandle>,
}
/// Host 懒加载会话失败的稳定内部分类，不得携带绝对路径、会话正文、凭据或底层错误原文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLoadError {
    NotFound,
    Corrupt,
    PermissionDenied,
    Unavailable,
}
/// 功能：表示会话元数据、分支或回退变更的稳定失败分类。
/// 数据边界：只携带可公开的中文错误，不包含路径、正文、凭据或底层 I/O 细节。
/// 关联：`API-HOST-SESSION-UPDATE-001`、`API-HOST-SESSION-FORK-001`、`API-HOST-SESSION-REVERT-001`。
#[derive(Debug)]
pub(crate) enum SessionMutationError {
    /// 请求字段或消息边界不合法。
    BadRequest(String),
    /// 目标会话不存在或正在删除。
    NotFound,
    /// 活动回合与本次变更冲突。
    Conflict(String),
    /// JSONL 持久化或回滚失败。
    Persistence(String),
}
/// 功能：表示 Host 已停止接收新回合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostShuttingDown;
pub(crate) struct HostAdmissionLease {
    state: Arc<AppState>,
    armed: bool,
}
impl Drop for HostAdmissionLease {
    fn drop(&mut self) {
        if self.armed {
            self.state.finish_admission();
        }
    }
}
struct TaskCompletionGuard {
    state: Arc<AppState>,
    task_id: u64,
}
impl Drop for TaskCompletionGuard {
    fn drop(&mut self) {
        self.state.finish_task(self.task_id);
    }
}
/// 供同步 HTTP handler 等待后台回合；Drop 不取消任务，关闭期由 Host registry 终止。
pub(crate) struct RegisteredTurnTask {
    completed: tokio::sync::oneshot::Receiver<()>,
}
impl RegisteredTurnTask {
    /// 等待已登记回合结束；handler 取消只丢弃等待端，不会脱离 registry 中的任务。
    pub(crate) async fn wait(self) {
        let _ = self.completed.await;
    }
}
/// session gate 的原子接收结果；仅 Accepted 可启动 AgentLoop，Replay 不得重复副作用。
pub(crate) enum AcceptTurnResult {
    Accepted(AcceptedTurn),
    Replay(RequestAdmission),
    IdempotencyConflict,
    Busy,
    Deleting,
    QueueFull,
    TemporarilyBusy,
    PersistenceFailed(String),
}
/// 保存首次接收后的稳定身份和调度凭据，只能移动到单个 registry 任务。
pub(crate) struct AcceptedTurn {
    pub(crate) turn_id: String,
    pub(crate) user_message_id: String,
    pub(crate) admission: TurnAdmission,
}
/// 功能：保存单个 session 的运行时资源和并发状态。
/// 并发与取消：`turn_gate` 线性化 reserve/abort/delete；取消同时通知 AgentLoop 和审批管理器。
pub struct SessionHandle {
    pub id: String,
    pub session: Mutex<Session>,
    pub message_cache: std::sync::RwLock<Vec<openpaw_core::Message>>,
    pub writer: std::sync::Mutex<Option<SessionWriter>>,
    session_path: PathBuf,
    pub agent: AgentLoop,
    pub approvals: Arc<HostApprovalManager>,
    pub active: AtomicBool,
    pub workspace: PathBuf,
    session_provider: String,
    session_model: String,
    pub created_at: i64,
    pub office: OfficeDispatcher,
    pub tools: Vec<openpaw_core::ToolSpec>,
    pub title: std::sync::Mutex<String>,
    mutation: std::sync::RwLock<SessionMutationProjection>,
    pub updated_at: std::sync::atomic::AtomicI64,
    pub cancel_requested: AtomicBool,
    deleting: AtomicBool,
    pub global_events: broadcast::Sender<OpenCodeEvent>,
    turn_sequence: std::sync::atomic::AtomicU64,
    pub office_working: Mutex<HashSet<u8>>,
    pub activity: std::sync::Mutex<Option<SessionActivity>>,
    current_turn: std::sync::Mutex<Option<ActiveTurn>>,
    // 消息快照与回合结果必须通过同一发布闸门对 HTTP 读者可见。
    snapshot_gate: std::sync::RwLock<()>,
    turn_outcomes: std::sync::RwLock<Vec<TurnOutcome>>,
    turn_errors: std::sync::RwLock<HashMap<String, OpenCodeTurnErrorV1>>,
    request_admissions: std::sync::RwLock<HashMap<String, RequestAdmission>>,
    turn_gate: std::sync::Mutex<()>,
    turn_cancel: std::sync::Mutex<CancellationToken>,
}
#[derive(Debug, Clone)]
struct ActiveTurn {
    outcome: TurnOutcome,
    queue_ticket: Option<u64>,
}
/// 功能：描述一个 session 当前正在执行的脱敏活动。
/// 数据流：AgentEvent -> SessionHandle.activity -> `/session/status` -> AI Office。
/// 约束：`tool` 仅允许工具名，不携带输入输出；回合结束时清空。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionActivity {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub task: String,
}
impl SessionHandle {
    /// 并发与取消：使用 acquire 顺序读取原子状态。
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn is_deleting(&self) -> bool {
        self.deleting.load(Ordering::Acquire)
    }
    /// 并发与取消：不持锁、不跨 await，活动 AgentLoop 期间也可立即读取。
    pub(crate) fn provider_model(&self) -> (String, String) {
        (self.session_provider.clone(), self.session_model.clone())
    }
    /// 功能：读取已持久化的 requestID 接收事实，供 HTTP 层在构造 Provider 前完成重放判断。
    /// 并发与取消：只读锁不跨 await；返回副本，避免调用方持有内部锁。
    pub(crate) fn request_admission(&self, request_id: &str) -> Option<RequestAdmission> {
        let _snapshot = self
            .snapshot_gate
            .read()
            .unwrap_or_else(|error| error.into_inner());
        self.request_admissions
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(request_id)
            .cloned()
    }
    /// 功能：取消当前回合以及相关审批等待。
    /// 并发与取消：与 reserve_turn 在同一 gate 下线性化，避免取消丢失。
    pub async fn abort(&self) {
        // 让 abort 与 reserve_turn 线性化。只有持有此 gate 时才替换令牌，
        // 因此取消不会先命中旧令牌、随后又被新回合覆盖。
        {
            let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
            self.cancel_requested.store(true, Ordering::Release);
            self.turn_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel();
            self.agent.cancel();
            self.approvals.abort_all();
        }
    }
    /// 功能：原子保留当前 session 的一个活动回合。
    /// 并发与取消：同一 session 并发调用只有一个成功；不同 session 互不阻塞。
    pub fn reserve_turn(&self) -> bool {
        let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.deleting.load(Ordering::Acquire) {
            return false;
        }
        if self
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        *self.turn_cancel.lock().unwrap_or_else(|e| e.into_inner()) = CancellationToken::new();
        self.cancel_requested.store(false, Ordering::Release);
        self.approvals.begin_turn();
        true
    }
    /// 并发与取消：与 reserve/abort 共用 gate。
    pub fn release_turn(&self) {
        let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
        self.active.store(false, Ordering::Release);
    }
    /// 功能：回滚已经保留但尚未交给后台回合的活动闸门。
    pub fn rollback_reserved_turn(&self) {
        // 必须在同一把 gate 内完成清理和 active=false；否则新回合可能在释放后
        // 抢先 reserve，再被旧 handler 的延迟清理误伤 Office 状态。
        let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        self.cancel_requested.store(true, Ordering::Release);
        self.turn_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
        self.agent.cancel();
        self.approvals.abort_all();
        finish_turn_projection(self);
        self.active.store(false, Ordering::Release);
    }
    fn begin_delete(&self) -> bool {
        let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.active.load(Ordering::Acquire) {
            return false;
        }
        self.deleting.store(true, Ordering::Release);
        true
    }
    fn cancel_delete(&self) {
        let _gate = self.turn_gate.lock().unwrap_or_else(|e| e.into_inner());
        self.deleting.store(false, Ordering::Release);
    }
    pub fn next_turn_id(&self) -> String {
        let sequence = self
            .turn_sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        format!("{sequence}_{}", Uuid::new_v4().simple())
    }
    pub fn emit(&self, event: OpenCodeEvent) {
        let _ = self.global_events.send(event);
    }
    pub fn set_activity(&self, kind: &str, tool: Option<&str>, task: &str) {
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *activity = Some(SessionActivity {
            kind: kind.to_string(),
            tool: tool.map(str::to_string),
            task: task.to_string(),
        });
    }
    pub fn clear_activity(&self) {
        let mut activity = self
            .activity
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *activity = None;
    }
    pub fn activity_snapshot(&self) -> Option<SessionActivity> {
        self.activity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    /// 功能：取得当前 queued/running 回合及其实时队列位置。
    /// 并发与取消：克隆短锁快照后再查询 scheduler，避免锁顺序反转。
    pub(crate) fn current_turn_snapshot(
        &self,
        scheduler: &TurnScheduler,
    ) -> Option<(TurnOutcome, Option<usize>)> {
        let active = self
            .current_turn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()?;
        let position = active
            .queue_ticket
            .and_then(|ticket| scheduler.queue_position(ticket));
        Some((active.outcome, position))
    }
    /// 功能：读取按首次出现顺序排列的每回合最新状态。
    /// 并发与取消：使用同步读锁，不等待 AgentLoop session 锁。
    pub(crate) fn turn_outcomes(&self) -> Vec<TurnOutcome> {
        let _snapshot = self
            .snapshot_gate
            .read()
            .unwrap_or_else(|error| error.into_inner());
        self.turn_outcomes
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    /// 功能：读取消息、终态与安全错误的同一原子快照并生成 REST 展示。
    /// 参数：无，读取当前会话。
    /// 返回：OpenCode 公共消息列表，失败占位不进入模型上下文。
    /// 错误：无；锁中毒时沿用已恢复的投影。
    /// 状态：只读。
    /// 并发与取消：持有 snapshot_gate 读锁，避免终态先于消息或详情可见。
    /// 权限：仅投影当前会话已经过滤的错误。
    /// 关联：`CHG-2026-0022`。
    pub(crate) fn wire_messages(&self) -> Vec<serde_json::Value> {
        let _snapshot = self
            .snapshot_gate
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let messages = self
            .message_cache
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let outcomes = self
            .turn_outcomes
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let errors = self
            .turn_errors
            .read()
            .unwrap_or_else(|error| error.into_inner());
        message_projection::messages_wire(&self.id, self.created_at, &outcomes, &messages, &errors)
    }
    /// 功能：设置当前回合状态并追加 `turn_outcome` 未知事件。
    /// 并发与取消：同 session 单回合保证状态转换串行；writer 锁保护追加。
    pub(crate) fn persist_turn_outcome(
        &self,
        outcome: TurnOutcome,
        queue_ticket: Option<u64>,
    ) -> Result<(), String> {
        self.persist_turn_outcome_event(&outcome)?;
        let _snapshot = self
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.project_turn_outcome_locked(outcome, queue_ticket);
        Ok(())
    }
    /// 功能：仅追加回合状态事件，不提前发布内存终态。
    /// 并发与取消：调用方必须在同一 session 的 `turn_gate` 内完成后续消息快照和终态投影。
    /// 约束：用于终态事务，避免 `turn_outcomes` 先于 `message_cache` 对 HTTP 读接口可见。
    fn persist_turn_outcome_event(&self, outcome: &TurnOutcome) -> Result<(), String> {
        append_event(self, &outcome.to_session_event())
    }
    fn project_turn_outcome_locked(&self, outcome: TurnOutcome, queue_ticket: Option<u64>) {
        upsert_turn_outcome(
            &mut self
                .turn_outcomes
                .write()
                .unwrap_or_else(|error| error.into_inner()),
            outcome.clone(),
        );
        *self
            .current_turn
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            (!outcome.state.is_terminal()).then_some(ActiveTurn {
                outcome,
                queue_ticket,
            });
    }
    /// 功能：在状态事件写盘失败时以内存 failed 终态闭合回合，并尝试重建 writer 后补写。
    /// 并发与取消：同 session 单回合调用；writer 重建受 writer 锁保护。
    fn record_persistence_failure(&self, attempted: &TurnOutcome) -> TurnOutcome {
        let failed = self.persistence_failure_outcome(attempted);
        let _snapshot = self
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.project_turn_outcome_locked(failed.clone(), None);
        failed
    }

    fn publish_terminal_snapshot(
        &self,
        messages: Vec<openpaw_core::Message>,
        outcome: TurnOutcome,
        failure: Option<OpenCodeTurnErrorV1>,
    ) {
        let _snapshot = self
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *self
            .message_cache
            .write()
            .unwrap_or_else(|error| error.into_inner()) = messages;
        self.updated_at
            .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
        if let Some(failure) = failure {
            self.turn_errors
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .insert(outcome.turn_id.clone(), failure);
        }
        self.project_turn_outcome_locked(outcome, None);
    }
    /// 功能：生成并尝试补写持久化失败终态，但暂不发布内存投影。
    /// 并发与取消：终态收尾路径在 `turn_gate` 内调用，待消息缓存刷新后再发布。
    fn persistence_failure_outcome(&self, attempted: &TurnOutcome) -> TurnOutcome {
        let failed = terminal_outcome(
            attempted,
            TurnState::Failed,
            Some(turn_error(
                "persistence_failed",
                "本轮状态保存失败，请重新发送。",
                true,
            )),
        );
        if let Err(error) = self.retry_persist_turn_outcome(&failed) {
            tracing::error!(session_id = %self.id, %error, "无法补写持久化失败终态");
        }
        failed
    }
    fn retry_persist_turn_outcome(&self, outcome: &TurnOutcome) -> Result<(), String> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "会话写入器锁状态异常".to_string())?;
        journal::append_with_retry(&mut writer, &self.session_path, &outcome.to_session_event())
    }
}
/// 功能：保存 Host 进程级配置、session 表、Office 投影和全局事件。
/// 并发与取消：session 表使用读写锁；生命周期锁防止懒加载/删除竞态。
pub struct AppState {
    pub config: HostConfig,
    pub sessions: RwLock<HashMap<String, Arc<SessionHandle>>>,
    pub office: OfficeDispatcher,
    pub global_events: broadcast::Sender<OpenCodeEvent>,
    pub(crate) scheduler: Arc<TurnScheduler>,
    session_slots: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    lifecycle: std::sync::Mutex<HostLifecycleState>,
    shutdown: CancellationToken,
    lifecycle_notify: Notify,
}
struct SessionHandleInit {
    session: Session,
    messages: Vec<openpaw_core::Message>,
    writer: SessionWriter,
    session_path: PathBuf,
    agent: AgentLoop,
    approvals: Arc<HostApprovalManager>,
    workspace: PathBuf,
    tools: Vec<openpaw_core::ToolSpec>,
    title: String,
    mutation: SessionMutationProjection,
    turn_sequence: u64,
    turn_outcomes: Vec<TurnOutcome>,
    turn_errors: HashMap<String, OpenCodeTurnErrorV1>,
    request_admissions: HashMap<String, RequestAdmission>,
}
impl AppState {
    pub fn new(config: HostConfig) -> Arc<Self> {
        Self::new_with_scheduler(config, TurnSchedulerConfig::from_env())
    }
    /// 功能：使用显式并发和队列上限创建 Host 状态。
    /// 并发与取消：全部 session 共享指定容量；同 session 仍限制为一个活动回合。
    pub fn new_with_turn_limits(
        config: HostConfig,
        max_concurrent_turns: usize,
        max_queued_turns: usize,
    ) -> Result<Arc<Self>, String> {
        if !(1..=32).contains(&max_concurrent_turns) {
            return Err("最大并发回合数必须在 1..=32 范围内".into());
        }
        if max_queued_turns > 1024 {
            return Err("最大排队回合数必须在 0..=1024 范围内".into());
        }
        Ok(Self::new_with_scheduler(
            config,
            TurnSchedulerConfig {
                max_concurrent_turns,
                max_queued_turns,
            },
        ))
    }
    /// 功能：使用显式调度容量创建共享运行时状态，供可靠性测试和嵌入式 Host 使用。
    /// 并发与取消：所有 session 共享同一调度器；同 session 闸门仍独立生效。
    pub(crate) fn new_with_scheduler(
        config: HostConfig,
        scheduler_config: TurnSchedulerConfig,
    ) -> Arc<Self> {
        let office = OfficeDispatcher::new(config.office_url.clone());
        let (global_events, _) = broadcast::channel(512);
        Arc::new(Self {
            config,
            sessions: RwLock::new(HashMap::new()),
            office,
            global_events,
            scheduler: TurnScheduler::new(scheduler_config.into()),
            session_slots: Mutex::new(HashMap::new()),
            lifecycle: std::sync::Mutex::new(HostLifecycleState {
                accepting_turns: true,
                in_flight_admissions: 0,
                next_task_id: 0,
                tasks: HashMap::new(),
            }),
            shutdown: CancellationToken::new(),
            lifecycle_notify: Notify::new(),
        })
    }
    /// 功能：启动阶段流式扫描全部 session，并闭合仍为 queued/running 的孤儿回合。
    /// 并发与取消：Host ready 前串行执行；文件扫描在线程池中完成。
    pub(crate) async fn recover_orphaned_turns(&self) -> usize {
        let directory = self.config.home.join("sessions");
        tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(directory) else {
                return 0;
            };
            entries
                .flatten()
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
                })
                .map(
                    |entry| match recover_orphaned_turns_in_file(&entry.path()) {
                        Ok(recovered) => recovered,
                        Err(error) => {
                            tracing::warn!(error = ?error, "启动恢复跳过了不可用的 session 文件");
                            0
                        }
                    },
                )
                .sum()
        })
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "启动恢复任务异常结束");
            0
        })
    }
    async fn session_slot(&self, id: &str) -> Arc<Mutex<()>> {
        let mut slots = self.session_slots.lock().await;
        slots
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    async fn remove_session_slot(&self, id: &str, slot: &Arc<Mutex<()>>) {
        let mut slots = self.session_slots.lock().await;
        if Arc::strong_count(slot) <= 2
            && slots
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            slots.remove(id);
        }
    }
    /// 功能：在线性化边界内登记一个尚未写入 session 的新回合接收过程。
    /// 并发与取消：与 `begin_shutdown` 使用同一同步锁，确定请求和关闭的唯一先后关系。
    pub(crate) fn begin_admission(
        self: &Arc<Self>,
    ) -> Result<HostAdmissionLease, HostShuttingDown> {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !lifecycle.accepting_turns {
            return Err(HostShuttingDown);
        }
        lifecycle.in_flight_admissions = lifecycle.in_flight_admissions.saturating_add(1);
        Ok(HostAdmissionLease {
            state: self.clone(),
            armed: true,
        })
    }
    fn finish_admission(&self) {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        lifecycle.in_flight_admissions = lifecycle.in_flight_admissions.saturating_sub(1);
        drop(lifecycle);
        self.lifecycle_notify.notify_waiters();
    }
    fn finish_task(&self, task_id: u64) {
        self.lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tasks
            .remove(&task_id);
        self.lifecycle_notify.notify_waiters();
    }
    /// 功能：把一个已完整持久化首状态的回合登记为 Host 所有的后台任务。
    /// 并发与取消：启动 barrier 防止任务在 registry 插入前完成；shutdown 可取消 session 并强制 abort。
    pub(crate) fn spawn_turn(
        self: &Arc<Self>,
        mut lease: HostAdmissionLease,
        handle: Arc<SessionHandle>,
        turn: ScheduledTurn,
    ) -> RegisteredTurnTask {
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel::<()>();
        let task_id = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !lifecycle.accepting_turns {
                lifecycle.in_flight_admissions = lifecycle.in_flight_admissions.saturating_sub(1);
                drop(lifecycle);
                lease.armed = false;
                let queued_at = handle
                    .current_turn
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .and_then(|active| active.outcome.queued_at);
                let outcome = TurnOutcome {
                    turn_id: turn.turn_id,
                    user_message_id: turn.user_message_id,
                    state: TurnState::Cancelled,
                    queued_at,
                    started_at: None,
                    completed_at: Some(chrono::Utc::now().timestamp_millis()),
                    error: Some(turn_error(
                        "host_shutdown",
                        "Host 正在关闭，本轮已取消。",
                        true,
                    )),
                };
                drop(turn.admission);
                finish_turn_with_outcome(&handle, outcome, "无法持久化关闭期取消终态");
                let _ = completed_tx.send(());
                self.lifecycle_notify.notify_waiters();
                return RegisteredTurnTask {
                    completed: completed_rx,
                };
            }
            lifecycle.next_task_id = lifecycle.next_task_id.saturating_add(1);
            lifecycle.next_task_id
        };
        let state = self.clone();
        let task = tokio::spawn(async move {
            let _completion = TaskCompletionGuard { state, task_id };
            let _ = start_rx.await;
            run_turn(handle, turn).await;
            let _ = completed_tx.send(());
        });
        {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            lifecycle.tasks.insert(task_id, task.abort_handle());
            lifecycle.in_flight_admissions = lifecycle.in_flight_admissions.saturating_sub(1);
        }
        lease.armed = false;
        let _ = start_tx.send(());
        self.lifecycle_notify.notify_waiters();
        RegisteredTurnTask {
            completed: completed_rx,
        }
    }
    /// 功能：启动受控关闭、停止新 admission、取消活动回合并有界等待任务收敛。
    /// 返回：正常完成或强制终止后任务注册表已清空时为 true；仍有任务时为 false。
    /// 并发与取消：与 begin_admission 共用锁；先取消已知 session，再等待已胜出的 admission 注册。
    pub async fn shutdown_gracefully(&self, timeout: Duration) -> bool {
        {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            lifecycle.accepting_turns = false;
        }
        self.shutdown.cancel();
        let deadline = tokio::time::Instant::now() + timeout;
        let sessions: Vec<_> = self.sessions.read().await.values().cloned().collect();
        for session in sessions {
            if session.is_active() {
                session.abort().await;
            }
        }
        if !self
            .wait_for_lifecycle(deadline, |lifecycle| lifecycle.in_flight_admissions == 0)
            .await
        {
            self.abort_registered_tasks();
            return self.harvest_registered_tasks(deadline).await;
        }
        if self
            .wait_for_lifecycle(deadline, |lifecycle| lifecycle.tasks.is_empty())
            .await
        {
            return true;
        }
        self.abort_registered_tasks();
        self.harvest_registered_tasks(deadline).await
    }
    async fn wait_for_lifecycle(
        &self,
        deadline: tokio::time::Instant,
        settled: impl Fn(&HostLifecycleState) -> bool,
    ) -> bool {
        loop {
            if settled(
                &self
                    .lifecycle
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            ) {
                return true;
            }
            if tokio::time::timeout_at(deadline, self.lifecycle_notify.notified())
                .await
                .is_err()
            {
                return false;
            }
        }
    }
    async fn harvest_registered_tasks(&self, original_deadline: tokio::time::Instant) -> bool {
        let harvest_deadline = std::cmp::max(
            original_deadline,
            tokio::time::Instant::now() + Duration::from_millis(100),
        );
        self.wait_for_lifecycle(harvest_deadline, |lifecycle| lifecycle.tasks.is_empty())
            .await
    }
    fn abort_registered_tasks(&self) {
        let handles: Vec<_> = self
            .lifecycle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tasks
            .values()
            .cloned()
            .collect();
        for handle in handles {
            handle.abort();
        }
    }
    /// 并发与取消：`shutdown_gracefully` 取消源令牌后所有克隆同时可见。
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
    /// 并发与取消：兼容入口；同 session 槽阻止删除和懒加载互相复活，不同 session 互不等待。
    pub async fn get(&self, id: &str) -> Option<Arc<SessionHandle>> {
        self.get_typed(id).await.ok()
    }
    /// 功能：获取或懒加载指定 session，并保留稳定失败分类。
    /// 并发与取消：全局 slot map 锁只用于取得槽；磁盘和 ToolHost await 仅持有目标 session 槽。
    pub(crate) async fn get_typed(&self, id: &str) -> Result<Arc<SessionHandle>, SessionLoadError> {
        if !valid_session_id(id) {
            return Err(SessionLoadError::NotFound);
        }
        if let Some(handle) = self.sessions.read().await.get(id).cloned() {
            if handle.is_deleting() {
                return Err(SessionLoadError::NotFound);
            }
            return Ok(handle);
        }
        let slot = self.session_slot(id).await;
        let _session_lifecycle = slot.lock().await;
        if let Some(handle) = self.sessions.read().await.get(id).cloned() {
            if handle.is_deleting() {
                return Err(SessionLoadError::NotFound);
            }
            return Ok(handle);
        }
        let loaded = self.load_existing(id).await;
        let handle = match loaded {
            Ok(handle) => handle,
            Err(error) => {
                drop(_session_lifecycle);
                self.remove_session_slot(id, &slot).await;
                return Err(error);
            }
        };
        self.sessions
            .write()
            .await
            .insert(id.to_string(), handle.clone());
        Ok(handle)
    }
    /// 功能：删除单个非活动会话及其 JSONL 文件。
    pub async fn delete_session(&self, id: &str) -> Result<bool, String> {
        if !valid_session_id(id) {
            return Ok(false);
        }
        let slot = self.session_slot(id).await;
        let _session_lifecycle = slot.lock().await;
        let handle = self.sessions.read().await.get(id).cloned();
        if let Some(handle) = handle.as_ref() {
            if !handle.begin_delete() {
                return Err("会话正在执行，无法删除".into());
            }
            self.sessions.write().await.remove(id);
            handle
                .writer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        }
        let path = self
            .config
            .home
            .join("sessions")
            .join(format!("{id}.jsonl"));
        match std::fs::remove_file(&path) {
            Ok(()) => {
                self.remove_session_slot(id, &slot).await;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.remove_session_slot(id, &slot).await;
                Ok(handle.is_some())
            }
            Err(error) => {
                if let Some(handle) = handle {
                    if let Ok(writer) = SessionWriter::open_append(&path) {
                        *handle.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(writer);
                    }
                    handle.cancel_delete();
                    self.sessions.write().await.insert(id.to_string(), handle);
                }
                Err(format!("删除会话文件失败：{error}"))
            }
        }
    }
    async fn load_existing(&self, id: &str) -> Result<Arc<SessionHandle>, SessionLoadError> {
        let path = self
            .config
            .home
            .join("sessions")
            .join(format!("{id}.jsonl"));
        let mut read = SessionReader::read(&path).map_err(classify_session_error)?;
        if read.meta.id != id {
            return Err(SessionLoadError::Corrupt);
        }
        if read.torn_tail_lines > 0 {
            repair_torn_session_tail(&path)?;
            read = SessionReader::read(&path).map_err(classify_session_error)?;
        }
        let provider = build_provider_for_model(&read.meta.provider, Some(&read.meta.model))
            .unwrap_or_else(|error| {
                Arc::new(openpaw_core::NullProvider::new(error)) as Arc<dyn LlmProvider>
            });
        let workspace = absolute_workspace(session_workspace(&read.events, &self.config.workspace))
            .map_err(|_| SessionLoadError::Unavailable)?;
        let tool_host = openpaw_tools::arc_registry(&workspace);
        let tools = tool_host
            .list_tools()
            .await
            .map_err(|_| SessionLoadError::Unavailable)?;
        let mut writer = SessionWriter::open_append(&path).map_err(classify_session_error)?;
        let projection = restored_session_projection(id, &read.events)?;
        let messages = projection.messages;
        let mut turn_outcomes = projection.turn_outcomes;
        let turn_errors = projection.turn_errors;
        recover_hanging_outcomes(&mut turn_outcomes, &mut writer)?;
        let request_admissions = projection.request_admissions;
        let mutation = projection.mutation;
        let turn_sequence = projection.turn_sequence_high_water;
        staged_visible_len(id, &messages, mutation.revert_message_id.as_deref())
            .ok_or(SessionLoadError::Corrupt)?;
        let mut session = Session::new(read.meta.model.clone(), read.meta.provider.clone());
        session.meta = read.meta;
        let approvals = HostApprovalManager::new(
            id.to_string(),
            self.global_events.clone(),
            CancellationToken::new(),
        );
        let agent = configured_agent(
            provider,
            session.meta.model.clone(),
            tool_host,
            approvals.clone(),
        )
        .map_err(|_| SessionLoadError::Unavailable)?;
        let title = session_title(&mutation, &messages);
        let published_messages = messages.clone();
        session.messages = messages;
        Ok(self.build_session_handle(SessionHandleInit {
            session,
            messages: published_messages,
            writer,
            session_path: path,
            agent,
            approvals,
            workspace,
            tools,
            title,
            mutation,
            turn_sequence,
            turn_outcomes,
            turn_errors,
            request_admissions,
        }))
    }
    /// 功能：创建 session、JSONL meta 和 AgentLoop 运行资源。
    /// 参数：provider_name、model 覆盖默认模型配置，workspace、title 指定工作区与标题。
    /// 返回：已登记且无历史消息或错误的新会话句柄。
    /// 错误：provider 构建、模型配置或会话文件创建失败时返回说明。
    /// 状态：创建独立消息、回合和错误缓存，不启动模型执行。
    /// 并发与取消：创建本身不启动回合；后续 prompt 由 reserve_turn 控制。
    /// 权限：仅写入 Host 配置的 session 目录，不调用 Office 或模型服务。
    /// 关联：`REQ-B-HOST-002`、`CHG-2026-0022`。
    pub async fn create_session(
        self: &Arc<Self>,
        provider_name: Option<&str>,
        model: Option<&str>,
        workspace: Option<PathBuf>,
        title: Option<&str>,
    ) -> Result<Arc<SessionHandle>, String> {
        let provider_tag = provider_name.unwrap_or(&self.config.provider);
        let provider = build_provider_for_model(provider_tag, model)?;
        let model_name = model.unwrap_or(provider.default_model()).trim().to_string();
        if model_name.trim().is_empty() {
            return Err("model 不能为空".into());
        }
        let workspace =
            absolute_workspace(workspace.unwrap_or_else(|| self.config.workspace.clone()))?;
        let id = Uuid::new_v4().simple().to_string();
        let mut session = Session::new(model_name.clone(), provider.name());
        session.meta.id = id.clone();
        let cancel = CancellationToken::new();
        let approvals = HostApprovalManager::new(id.clone(), self.global_events.clone(), cancel);
        let tool_host = openpaw_tools::arc_registry(&workspace);
        let tools = tool_host.list_tools().await.map_err(|e| e.to_string())?;
        let agent = configured_agent(provider, model_name, tool_host, approvals.clone())?;
        let home = self.config.home.clone();
        let path = home.join("sessions").join(format!("{id}.jsonl"));
        let mut writer = SessionWriter::create(&path).map_err(|e| e.to_string())?;
        let metadata = SessionEvent::Metadata {
            title: title.map(str::to_string),
            mode: None,
            model_override: None,
            cwd: Some(workspace.to_string_lossy().into_owned()),
            workspace_id: None,
            workspace_name: None,
            task_id: None,
        };
        if let Err(error) = writer
            .append(&SessionEvent::Meta(session.meta.clone()))
            .and_then(|_| writer.append(&metadata))
        {
            drop(writer);
            if let Err(cleanup_error) = std::fs::remove_file(&path) {
                tracing::warn!(%cleanup_error, "创建会话失败后无法清理未完成的 session 文件");
            }
            return Err(error.to_string());
        }
        let handle = self.build_session_handle(SessionHandleInit {
            session,
            messages: Vec::new(),
            writer,
            session_path: path,
            agent,
            approvals,
            workspace,
            tools,
            title: title.unwrap_or("New session").to_string(),
            mutation: SessionMutationProjection {
                title: title.map(str::to_string),
                ..Default::default()
            },
            turn_sequence: 0,
            turn_outcomes: Vec::new(),
            turn_errors: HashMap::new(),
            request_admissions: HashMap::new(),
        });
        self.sessions.write().await.insert(id, handle.clone());
        Ok(handle)
    }
    fn build_session_handle(&self, init: SessionHandleInit) -> Arc<SessionHandle> {
        let SessionHandleInit {
            session,
            messages,
            writer,
            session_path,
            agent,
            approvals,
            workspace,
            tools,
            title,
            mutation,
            turn_sequence,
            turn_outcomes,
            turn_errors,
            request_admissions,
        } = init;
        let id = session.meta.id.clone();
        let created_at = unix_millis(session.meta.created_at);
        let updated_at = session_file_updated_at(&session_path, created_at);
        Arc::new(SessionHandle {
            id,
            session_provider: session.meta.provider.clone(),
            session_model: session.meta.model.clone(),
            session: Mutex::new(session),
            message_cache: std::sync::RwLock::new(messages),
            writer: std::sync::Mutex::new(Some(writer)),
            session_path,
            agent,
            approvals,
            active: AtomicBool::new(false),
            workspace,
            created_at,
            office: self.office.clone(),
            tools,
            title: std::sync::Mutex::new(title),
            mutation: std::sync::RwLock::new(mutation),
            updated_at: std::sync::atomic::AtomicI64::new(updated_at),
            cancel_requested: AtomicBool::new(false),
            deleting: AtomicBool::new(false),
            global_events: self.global_events.clone(),
            turn_sequence: AtomicU64::new(turn_sequence),
            office_working: Mutex::new(HashSet::new()),
            activity: std::sync::Mutex::new(None),
            current_turn: std::sync::Mutex::new(None),
            snapshot_gate: std::sync::RwLock::new(()),
            turn_outcomes: std::sync::RwLock::new(turn_outcomes),
            turn_errors: std::sync::RwLock::new(turn_errors),
            request_admissions: std::sync::RwLock::new(request_admissions),
            turn_gate: std::sync::Mutex::new(()),
            turn_cancel: std::sync::Mutex::new(CancellationToken::new()),
        })
    }
    pub async fn wire_session(&self, handle: &SessionHandle) -> SessionWire {
        wire_session_snapshot(handle)
    }
    /// 功能：原子更新会话标题和归档状态，并发布完整 Session 更新事件。
    /// 参数：`id` 为目标会话；`patch` 至少包含非空标题或归档时间。
    /// 返回：成功时返回已经持久化并发布到内存的公共 Session 投影。
    /// 错误：非法字段、会话缺失、活动归档冲突或 JSONL 写入失败返回稳定分类。
    /// 状态：先追加一条 `session_metadata_v1`，成功后同时发布标题、归档和更新时间。
    /// 并发与取消：`turn_gate` 与回合接收线性化；同步持久化临界区不可取消。
    /// 权限：只修改目标 session JSONL，不读取或写入工作区文件。
    /// 关联：`API-HOST-SESSION-UPDATE-001`、`DAT-HOST-SESSION-MUTATION-001`。
    pub(crate) async fn update_session(
        &self,
        id: &str,
        patch: UpdateSessionRequest,
    ) -> Result<SessionWire, SessionMutationError> {
        let title = match patch.title {
            Some(title) if title.trim().is_empty() => {
                return Err(SessionMutationError::BadRequest("title 不能为空".into()))
            }
            Some(title) => Some(title),
            None => None,
        };
        let archived = patch.time.and_then(|time| time.archived);
        if archived.is_some_and(|value| value < 0) {
            return Err(SessionMutationError::BadRequest(
                "time.archived 不能为负数".into(),
            ));
        }
        if title.is_none() && archived.is_none() {
            return Err(SessionMutationError::BadRequest(
                "PATCH 至少需要 title 或 time.archived".into(),
            ));
        }

        let handle = self.get_typed(id).await.map_err(|error| match error {
            SessionLoadError::NotFound => SessionMutationError::NotFound,
            SessionLoadError::Corrupt
            | SessionLoadError::PermissionDenied
            | SessionLoadError::Unavailable => {
                SessionMutationError::Persistence("无法读取会话状态".into())
            }
        })?;
        let metadata = SessionMetadataV1 { title, archived };
        {
            let _gate = handle
                .turn_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if handle.deleting.load(Ordering::Acquire) {
                return Err(SessionMutationError::NotFound);
            }
            if metadata.archived.is_some() && handle.active.load(Ordering::Acquire) {
                return Err(SessionMutationError::Conflict(
                    "会话正在执行，无法更改归档状态".into(),
                ));
            }
            let event = SessionMutationEvent::Metadata(metadata.clone());
            append_event(&handle, &event.to_session_event())
                .map_err(SessionMutationError::Persistence)?;
            {
                let _snapshot = handle
                    .snapshot_gate
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(title) = metadata.title.as_ref() {
                    *handle
                        .title
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = title.clone();
                }
                handle
                    .mutation
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .apply(event);
                handle
                    .updated_at
                    .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
            }
        }
        let wire = self.wire_session(&handle).await;
        handle.emit(events::session_changed("session.updated", &wire));
        Ok(wire)
    }
    /// 功能：从源会话当前可见历史创建一个独立、持久化的新根会话。
    /// 参数：`source_id` 为直接源会话；`message_id` 可选，存在时复制严格早于该消息的前缀。
    /// 返回：成功时返回已加入运行时表的新会话句柄。
    /// 错误：源会话缺失、边界非法、源会话活动或分支文件事务失败时返回稳定分类。
    /// 状态：消息、工具调用和内部引用全部换新编号；不复制归档、回退、回合、错误或幂等状态。
    /// 并发与取消：源 `turn_gate` 线性化活动检查、序号分配和文件发布；文件事务同步不可取消。
    /// 权限：只在 Host session 目录创建新 JSONL，不读取或修改工作区文件。
    /// 关联：`API-HOST-SESSION-FORK-001`、`DAT-HOST-SESSION-MUTATION-002`。
    pub(crate) async fn fork_session(
        &self,
        source_id: &str,
        message_id: Option<&str>,
    ) -> Result<Arc<SessionHandle>, SessionMutationError> {
        let source = self
            .get_typed(source_id)
            .await
            .map_err(|error| match error {
                SessionLoadError::NotFound => SessionMutationError::NotFound,
                SessionLoadError::Corrupt
                | SessionLoadError::PermissionDenied
                | SessionLoadError::Unavailable => {
                    SessionMutationError::Persistence("无法读取源会话状态".into())
                }
            })?;
        let new_id = Uuid::new_v4().simple().to_string();
        let final_path = self
            .config
            .home
            .join("sessions")
            .join(format!("{new_id}.jsonl"));
        let temporary_path = final_path.with_extension("jsonl.tmp");
        let handle = {
            let _gate = source
                .turn_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if source.deleting.load(Ordering::Acquire) {
                return Err(SessionMutationError::NotFound);
            }
            if source.active.load(Ordering::Acquire) {
                return Err(SessionMutationError::Conflict(
                    "会话正在执行，无法创建分支".into(),
                ));
            }
            let (source_messages, source_title, revert_message_id) = {
                let _snapshot = source
                    .snapshot_gate
                    .read()
                    .unwrap_or_else(|error| error.into_inner());
                (
                    source
                        .message_cache
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                    source
                        .title
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                    source
                        .mutation
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .revert_message_id
                        .clone(),
                )
            };
            let visible_len =
                staged_visible_len(source_id, &source_messages, revert_message_id.as_deref())
                    .ok_or_else(|| {
                        SessionMutationError::Persistence("会话回退游标无法匹配完整消息历史".into())
                    })?;
            let visible_messages = &source_messages[..visible_len];
            let fork_len = match message_id {
                Some(message_id) => visible_prefix_len(source_id, visible_messages, message_id)
                    .or_else(|| {
                        source
                            .wire_messages()
                            .iter()
                            .any(|message| {
                                message["info"]["id"] == message_id
                                    && message["info"]["error"].is_object()
                            })
                            .then(|| {
                                failed_message_prefix_len(
                                    source_id,
                                    visible_messages,
                                    &source.turn_outcomes(),
                                    message_id,
                                )
                            })
                            .flatten()
                    })
                    .ok_or_else(|| {
                        SessionMutationError::BadRequest("messageID 不存在或已被回退隐藏".into())
                    })?,
                None => visible_messages.len(),
            };
            let messages = clone_messages_for_fork(&visible_messages[..fork_len]);
            let ordinal = next_fork_ordinal(&self.config.home, source_id)?;
            let title = format!("{source_title} ({ordinal})");
            let origin = SessionForkOriginV1 {
                source_session_id: source_id.to_string(),
                ordinal,
            };
            let (provider_name, model_name) = source.provider_model();
            let provider = build_provider_for_model(&provider_name, Some(&model_name))
                .unwrap_or_else(|error| {
                    Arc::new(openpaw_core::NullProvider::new(error)) as Arc<dyn LlmProvider>
                });
            let approvals = HostApprovalManager::new(
                new_id.clone(),
                self.global_events.clone(),
                CancellationToken::new(),
            );
            let tool_host = openpaw_tools::arc_registry(&source.workspace);
            let agent =
                configured_agent(provider, model_name.clone(), tool_host, approvals.clone())
                    .map_err(|_| {
                        SessionMutationError::Persistence("无法创建会话分支运行时".into())
                    })?;
            let mut session = Session::new(model_name, provider_name);
            session.meta.id = new_id.clone();
            session.messages = messages.clone();
            let metadata = SessionEvent::Metadata {
                title: Some(title.clone()),
                mode: None,
                model_override: None,
                cwd: Some(source.workspace.to_string_lossy().into_owned()),
                workspace_id: None,
                workspace_name: None,
                task_id: None,
            };
            let mut records = vec![
                SessionEvent::Meta(session.meta.clone()),
                metadata,
                SessionMutationEvent::ForkOrigin(origin.clone()).to_session_event(),
            ];
            records.extend(messages.iter().cloned().map(SessionEvent::Message));
            let writer = journal::publish(&temporary_path, &final_path, &records)
                .map_err(|_| SessionMutationError::Persistence("无法创建或发布会话分支".into()))?;
            self.build_session_handle(SessionHandleInit {
                session,
                messages,
                writer,
                session_path: final_path,
                agent,
                approvals,
                workspace: source.workspace.clone(),
                tools: source.tools.clone(),
                title: title.clone(),
                mutation: SessionMutationProjection {
                    title: Some(title),
                    fork_origin: Some(origin),
                    ..Default::default()
                },
                turn_sequence: 0,
                turn_outcomes: Vec::new(),
                turn_errors: HashMap::new(),
                request_admissions: HashMap::new(),
            })
        };
        self.sessions
            .write()
            .await
            .insert(handle.id.clone(), handle.clone());
        let wire = self.wire_session(&handle).await;
        handle.emit(events::session_changed("session.created", &wire));
        Ok(handle)
    }
    /// 功能：暂存一个会话级回退游标，并隐藏该消息及其后的可见历史。
    /// 参数：`id` 为目标会话；`message_id` 为第一条需要隐藏的消息编号。
    /// 返回：成功时返回包含 `revert.messageID` 的公共 Session 投影。
    /// 错误：会话缺失、边界不可见、活动回合冲突或 JSONL 写入失败时返回稳定分类。
    /// 状态：完整历史保留在 `Session` 和 `message_cache`；客户端根据返回游标隐藏尾部。
    /// 并发与取消：`turn_gate` 与 prompt、fork、delete 线性化；同步持久化临界区不可取消。
    /// 权限：只追加目标 session JSONL，不恢复或修改工作区文件。
    /// 关联：`API-HOST-SESSION-REVERT-001`、`DAT-HOST-SESSION-MUTATION-003`。
    pub(crate) async fn revert_session(
        &self,
        id: &str,
        message_id: &str,
    ) -> Result<SessionWire, SessionMutationError> {
        if message_id.trim().is_empty() {
            return Err(SessionMutationError::BadRequest(
                "messageID 不能为空".into(),
            ));
        }
        let handle = self
            .get_typed(id)
            .await
            .map_err(session_mutation_load_error)?;
        {
            let _gate = handle
                .turn_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if handle.deleting.load(Ordering::Acquire) {
                return Err(SessionMutationError::NotFound);
            }
            if handle.active.load(Ordering::Acquire) {
                return Err(SessionMutationError::Conflict(
                    "会话正在执行，无法回退".into(),
                ));
            }
            let messages = self.message_snapshot(&handle);
            let turn_outcomes = handle.turn_outcomes();
            let current_revert_message_id = handle
                .mutation
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .revert_message_id
                .clone();
            let visible_len =
                staged_visible_len(id, &messages, current_revert_message_id.as_deref())
                    .ok_or_else(|| {
                        SessionMutationError::Persistence("会话回退游标无法匹配完整消息历史".into())
                    })?;
            if visible_prefix_len(id, &messages[..visible_len], message_id).is_none()
                && !handle.wire_messages().iter().any(|message| {
                    message["info"]["id"] == message_id && message["info"]["error"].is_object()
                })
            {
                return Err(SessionMutationError::BadRequest(
                    "messageID 不存在或已被回退隐藏".into(),
                ));
            }
            let (revert_message_id, _) =
                normalize_revert_boundary(id, &messages[..visible_len], &turn_outcomes, message_id)
                    .ok_or_else(|| {
                        SessionMutationError::BadRequest("messageID 不存在或已被回退隐藏".into())
                    })?;
            let event = SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
                message_id: revert_message_id,
            });
            append_event(&handle, &event.to_session_event())
                .map_err(SessionMutationError::Persistence)?;
            let _snapshot = handle
                .snapshot_gate
                .write()
                .unwrap_or_else(|error| error.into_inner());
            handle
                .mutation
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .apply(event);
            handle
                .updated_at
                .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
        }
        let wire = self.wire_session(&handle).await;
        handle.emit(events::session_changed("session.updated", &wire));
        Ok(wire)
    }
    /// 功能：清除尚未提交的会话回退游标，并恢复完整消息快照。
    /// 参数：`id` 为目标会话编号。
    /// 返回：成功或本来没有游标时都返回当前公共 Session 投影。
    /// 错误：会话缺失、状态冲突或 JSONL 写入失败时返回稳定分类。
    /// 状态：存在游标时追加 `session_revert_cleared_v1` 并清除游标；完整消息缓存保持不变。
    /// 并发与取消：`turn_gate` 与 prompt 接收线性化；同步持久化临界区不可取消。
    /// 权限：只追加目标 session JSONL，不恢复或修改工作区文件。
    /// 关联：`API-HOST-SESSION-UNREVERT-001`、`DAT-HOST-SESSION-MUTATION-003`。
    pub(crate) async fn unrevert_session(
        &self,
        id: &str,
    ) -> Result<SessionWire, SessionMutationError> {
        let handle = self
            .get_typed(id)
            .await
            .map_err(session_mutation_load_error)?;
        let changed = {
            let _gate = handle
                .turn_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if handle.deleting.load(Ordering::Acquire) {
                return Err(SessionMutationError::NotFound);
            }
            let staged = handle
                .mutation
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .revert_message_id
                .is_some();
            if !staged {
                false
            } else {
                if handle.active.load(Ordering::Acquire) {
                    return Err(SessionMutationError::Conflict(
                        "会话正在执行，无法取消回退".into(),
                    ));
                }
                let event = SessionMutationEvent::RevertCleared;
                append_event(&handle, &event.to_session_event())
                    .map_err(SessionMutationError::Persistence)?;
                let _snapshot = handle
                    .snapshot_gate
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                handle
                    .mutation
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .apply(event);
                handle
                    .updated_at
                    .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
                true
            }
        };
        let wire = self.wire_session(&handle).await;
        if changed {
            handle.emit(events::session_changed("session.updated", &wire));
        }
        Ok(wire)
    }
    /// 约束：不读取模型锁，避免慢模型或工具调用阻塞 REST/SSE 轮询。
    pub fn message_snapshot(&self, handle: &SessionHandle) -> Vec<openpaw_core::Message> {
        let _snapshot = handle
            .snapshot_gate
            .read()
            .unwrap_or_else(|error| error.into_inner());
        handle
            .message_cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub fn update_message_snapshot(
        &self,
        handle: &SessionHandle,
        messages: Vec<openpaw_core::Message>,
    ) {
        let _snapshot = handle
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *handle
            .message_cache
            .write()
            .unwrap_or_else(|error| error.into_inner()) = messages;
    }
    /// 功能：扫描 home 下 JSONL 并生成会话摘要列表。
    /// 参数：见签名；入口路径/名称或显式配置。
    /// 返回：见签名的退出码、Provider 或摘要。
    /// 错误：配置和读取失败脱敏处理；doctor WARN 不失败。
    /// 状态：诊断不连接模型；摘要不写 JSONL。
    /// 并发与取消：使用已有运行时和锁，未新增调度或恢复决策。
    /// 权限：密钥不进入输出或摘要。
    /// 关联：CHG-2026-0023 共享配置/目录。
    pub async fn summaries(&self) -> Vec<SessionSummary> {
        let home = self.config.home.clone();
        let default_workspace = self.config.workspace.clone();
        let active_ids: HashSet<String> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|session| session.is_active())
            .map(|session| session.id.clone())
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            let Ok(ids) = openpaw_store::catalog::discover(&home) else {
                return out;
            };
            for id in ids {
                let Ok(read) = openpaw_store::catalog::read(&home, &id) else {
                    continue;
                };
                let path = read.path.clone();
                let projection = read.transcript;
                let title = projection.title();
                let directory =
                    absolute_workspace(session_workspace(&read.events, &default_workspace))
                        .unwrap_or_else(|_| default_workspace.clone())
                        .to_string_lossy()
                        .into_owned();
                let meta = read.meta;
                let updated = session_file_updated_at(&path, unix_millis(meta.created_at));
                let active = active_ids.contains(&meta.id);
                let archived = projection.mutation.archived;
                let revert = projection
                    .mutation
                    .revert_message_id
                    .map(|message_id| crate::protocol::SessionRevert { message_id });
                out.push(SessionSummary {
                    session: SessionWire {
                        slug: meta.id.clone(),
                        id: meta.id,
                        title,
                        directory,
                        time: SessionTime {
                            created: unix_millis(meta.created_at),
                            updated,
                            archived,
                        },
                        revert,
                    },
                    created_at: unix_millis(meta.created_at),
                    updated_at: updated,
                    provider: meta.provider,
                    model: meta.model,
                    active,
                });
            }
            out.sort_by_key(|summary| Reverse(summary.updated_at));
            out.truncate(50);
            out
        })
        .await
        .unwrap_or_default()
    }
}
fn absolute_workspace(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| format!("无法解析工作区绝对路径：{error}"))
}
fn session_workspace(events: &[SessionEvent], fallback: &std::path::Path) -> PathBuf {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            SessionEvent::Metadata { cwd: Some(cwd), .. } if !cwd.trim().is_empty() => {
                Some(PathBuf::from(cwd))
            }
            _ => None,
        })
        .unwrap_or_else(|| fallback.to_path_buf())
}
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
fn configured_agent(
    provider: Arc<dyn LlmProvider>,
    model: String,
    tool_host: Arc<dyn openpaw_core::ToolHost>,
    approvals: Arc<HostApprovalManager>,
) -> Result<AgentLoop, String> {
    let mut agent = AgentLoop::new(provider)
        .with_model(model)
        .with_tool_host(tool_host)
        .with_compact_options(openpaw_engine::compaction::CompactOptions {
            protect_head_pairs: 0,
            keep_recent_pairs: 1,
            ..Default::default()
        });
    agent = agent
        .with_max_tokens(openpaw_llm::config::ConfigSnapshot::from_env().max_output_tokens(None)?);
    Ok(openpaw_engine::approval::configure(
        agent,
        std::env::var("OPENPAW_ENGINE_PERMISSIONS").as_deref() == Ok("manual"),
        approvals,
    ))
}
/// 功能：按协议名称构造 Host Provider。
/// 参数：协议名称。
/// 返回：共享 LlmProvider。
/// 错误：脱敏配置错误。
/// 状态：保留 Host mock 测试选择。
/// 并发与取消：请求由 AgentLoop 管理。
/// 权限：构造不连接网络。
/// 关联：共享 LLM 工厂。
#[cfg(test)]
pub fn build_provider(name: &str) -> Result<Arc<dyn LlmProvider>, String> {
    build_provider_for_model(name, None)
}
/// 功能：用显式模型覆盖环境后调用共享 Provider 工厂，保留 Host mock 模式。
/// 参数：见签名；入口路径/名称或显式配置。
/// 返回：见签名的退出码、Provider 或摘要。
/// 错误：配置和读取失败脱敏处理；doctor WARN 不失败。
/// 状态：诊断不连接模型；摘要不写 JSONL。
/// 并发与取消：使用已有运行时和锁，未新增调度或恢复决策。
/// 权限：密钥不进入输出或摘要。
/// 关联：CHG-2026-0023 共享配置/目录。
pub(crate) fn build_provider_for_model(
    name: &str,
    model: Option<&str>,
) -> Result<Arc<dyn LlmProvider>, String> {
    let config = openpaw_llm::config::ConfigSnapshot::from_env().resolve(
        name,
        openpaw_llm::config::Overrides {
            model: model.map(str::to_string),
            ..Default::default()
        },
    )?;
    match name {
        "mock" => {
            let mode = std::env::var("OPENPAW_ENGINE_MOCK").unwrap_or_default();
            let p = match mode.as_str() {
                "tools" => openpaw_llm::MockProvider::scripted(vec![
                    openpaw_llm::MockMode::ToolUse {
                        tool_name: "read_file".into(),
                        input: serde_json::json!({"path":"README.md"}),
                        text: "reading".into(),
                    },
                    openpaw_llm::MockMode::Fixed("读取完成".into()),
                ]),
                "stream" => {
                    openpaw_llm::MockProvider::stream_text_delayed("streamed response", 8, 40)
                }
                _ => openpaw_llm::MockProvider::counter("mock-reply-"),
            };
            Ok(Arc::new(p))
        }
        _ => Ok(config.build()),
    }
}
/// 功能：解析用于请求指纹的有效模型。
/// 参数：协议名称。
/// 返回：有效模型。
/// 错误：未知协议或无效模型。
/// 状态：不修改环境。
/// 并发与取消：同步只读。
/// 权限：不读取密钥或请求网络。
/// 关联：共享配置和 HTTP 幂等接收。
pub fn default_model(name: &str) -> Result<String, String> {
    openpaw_llm::config::ConfigSnapshot::from_env().model(name, None)
}
fn wire_session_snapshot(handle: &SessionHandle) -> SessionWire {
    let _snapshot = handle
        .snapshot_gate
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let mutation = handle
        .mutation
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    SessionWire {
        id: handle.id.clone(),
        title: handle
            .title
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        slug: handle.id.clone(),
        directory: handle.workspace.display().to_string(),
        time: SessionTime {
            created: handle.created_at,
            updated: handle.updated_at.load(Ordering::Acquire),
            archived: mutation.archived,
        },
        revert: mutation
            .revert_message_id
            .map(|message_id| crate::protocol::SessionRevert { message_id }),
    }
}
fn session_title(
    mutation: &SessionMutationProjection,
    messages: &[openpaw_core::Message],
) -> String {
    openpaw_store::transcript::session_title(mutation.title.as_deref(), messages)
}
fn staged_visible_len(
    session_id: &str,
    messages: &[openpaw_core::Message],
    revert_message_id: Option<&str>,
) -> Option<usize> {
    match revert_message_id {
        Some(message_id) => visible_prefix_len(session_id, messages, message_id),
        None => Some(messages.len()),
    }
}
fn session_mutation_load_error(error: SessionLoadError) -> SessionMutationError {
    match error {
        SessionLoadError::NotFound => SessionMutationError::NotFound,
        SessionLoadError::Corrupt
        | SessionLoadError::PermissionDenied
        | SessionLoadError::Unavailable => {
            SessionMutationError::Persistence("无法读取会话状态".into())
        }
    }
}
fn restored_session_projection(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<openpaw_store::transcript::Transcript, SessionLoadError> {
    openpaw_store::transcript::replay(session_id, events).map_err(|_| SessionLoadError::Corrupt)
}
fn classify_session_error(error: SessionError) -> SessionLoadError {
    match error {
        SessionError::Io { source, .. } => classify_io_kind(source.kind()),
        SessionError::Json { .. }
        | SessionError::JsonSerialize(_)
        | SessionError::MissingMeta { .. } => SessionLoadError::Corrupt,
    }
}
fn recover_orphaned_turns_in_file(path: &std::path::Path) -> Result<usize, SessionLoadError> {
    let read = journal::read_repaired(path).map_err(classify_session_error)?;
    let mut outcomes = restored_session_projection(&read.meta.id, &read.events)?.turn_outcomes;
    let mut writer = SessionWriter::open_append(path).map_err(classify_session_error)?;
    recover_hanging_outcomes(&mut outcomes, &mut writer)
}
fn recover_hanging_outcomes(
    outcomes: &mut Vec<TurnOutcome>,
    writer: &mut SessionWriter,
) -> Result<usize, SessionLoadError> {
    let hanging: Vec<_> = outcomes
        .iter()
        .filter(|outcome| !outcome.state.is_terminal())
        .cloned()
        .collect();
    for outcome in &hanging {
        let recovered = terminal_outcome(
            outcome,
            TurnState::Failed,
            Some(turn_error(
                "host_restarted",
                "Host 重启中断了上一回合，请重新发送。",
                true,
            )),
        );
        writer
            .append(&recovered.to_session_event())
            .map_err(classify_session_error)?;
        upsert_turn_outcome(outcomes, recovered);
    }
    Ok(hanging.len())
}
fn classify_io_kind(kind: std::io::ErrorKind) -> SessionLoadError {
    match kind {
        std::io::ErrorKind::NotFound => SessionLoadError::NotFound,
        std::io::ErrorKind::PermissionDenied => SessionLoadError::PermissionDenied,
        _ => SessionLoadError::Unavailable,
    }
}
fn upsert_turn_outcome(outcomes: &mut Vec<TurnOutcome>, outcome: TurnOutcome) {
    if let Some(existing) = outcomes
        .iter_mut()
        .find(|existing| existing.turn_id == outcome.turn_id)
    {
        if !existing.state.is_terminal() {
            *existing = outcome;
        }
    } else {
        outcomes.push(outcome);
    }
}
fn turn_error(code: &str, message: &str, retryable: bool) -> TurnError {
    TurnError {
        code: code.to_string(),
        message: message.to_string(),
        retryable,
    }
}
fn terminal_outcome(
    source: &TurnOutcome,
    state: TurnState,
    error: Option<TurnError>,
) -> TurnOutcome {
    debug_assert!(state.is_terminal());
    TurnOutcome {
        turn_id: source.turn_id.clone(),
        user_message_id: source.user_message_id.clone(),
        state,
        queued_at: source.queued_at,
        started_at: source.started_at,
        completed_at: Some(chrono::Utc::now().timestamp_millis()),
        error,
    }
}
fn classify_turn_error(error: &openpaw_core::Error) -> TurnError {
    match error {
        openpaw_core::Error::RateLimited { .. } => turn_error(
            "provider_rate_limited",
            "模型服务当前请求过多，请稍后重试。",
            true,
        ),
        openpaw_core::Error::Timeout(_) => {
            turn_error("provider_timeout", "模型服务响应超时，请稍后重试。", true)
        }
        openpaw_core::Error::AuthFailed { .. } => turn_error(
            "provider_auth_failed",
            "模型服务鉴权失败，请检查服务配置。",
            false,
        ),
        openpaw_core::Error::ContextTooLong(_) => turn_error(
            "context_too_long",
            "当前对话内容超过模型上下文限制，请新建对话或缩短输入。",
            false,
        ),
        openpaw_core::Error::ServerError { status, .. } => {
            if matches!(status, 503 | 529) {
                turn_error(
                    "provider_overloaded",
                    "模型服务当前负载过高，请稍后重试。",
                    true,
                )
            } else {
                turn_error(
                    "provider_server_error",
                    "模型服务发生服务器错误，请稍后重试。",
                    true,
                )
            }
        }
        openpaw_core::Error::Provider(_) | openpaw_core::Error::Other(_) => {
            let detail = error.to_string().to_lowercase();
            let class = openpaw_core::classify_api_error(error);
            let protocol_error = detail.contains("sse utf-8")
                || detail.contains("utf-8 码点中间结束")
                || detail.contains("sse data")
                || detail.contains("[done]")
                || detail.contains("finish_reason")
                || detail.contains("arguments json")
                || detail.contains("tool_use json")
                || detail.contains("max_output_tokens")
                || ((detail.contains("tool_call") || detail.contains("tool_use"))
                    && (detail.contains("缺少")
                        || detail.contains("不完整")
                        || detail.contains("必须是")))
                || detail.contains("空响应")
                || detail.contains("empty response")
                || detail.contains("stream ended without final");
            if class == openpaw_core::ApiErrorClass::RetryableOverloaded {
                turn_error(
                    "provider_overloaded",
                    "模型服务当前负载过高，请稍后重试。",
                    true,
                )
            } else if protocol_error {
                turn_error(
                    "provider_protocol_error",
                    "模型服务返回了不完整的流式响应，请稍后重试。",
                    true,
                )
            } else {
                match class {
                    openpaw_core::ApiErrorClass::FallbackCandidate => turn_error(
                        "provider_server_error",
                        "模型服务发生服务器错误，请稍后重试。",
                        true,
                    ),
                    openpaw_core::ApiErrorClass::RetryableTransient => turn_error(
                        "provider_timeout",
                        "模型服务连接暂时不稳定，请稍后重试。",
                        true,
                    ),
                    openpaw_core::ApiErrorClass::ContextTooLong => turn_error(
                        "context_too_long",
                        "当前对话内容超过模型上下文限制，请新建对话或缩短输入。",
                        false,
                    ),
                    _ => turn_error("turn_failed", "本轮处理失败，请稍后重试。", false),
                }
            }
        }
        _ => turn_error(
            "turn_failed",
            "本轮处理失败，请稍后重试。",
            error.is_retryable(),
        ),
    }
}
/// 功能：在同一 session gate 内原子完成幂等检查、回合保留、容量接收和首状态事务写入。
/// 并发与取消：turn_gate 线性化同键并发、旧回合终态提交和新回合接收；不同 session 互不阻塞。
pub(crate) fn accept_turn(
    state: &AppState,
    handle: &SessionHandle,
    message: openpaw_core::Message,
    turn_id: String,
    user_message_id: String,
    request_admission: Option<RequestAdmission>,
) -> AcceptTurnResult {
    let _gate = handle
        .turn_gate
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(candidate) = request_admission.as_ref() {
        if let Some(existing) = handle
            .request_admissions
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&candidate.request_id)
            .cloned()
        {
            return if existing.fingerprint == candidate.fingerprint {
                AcceptTurnResult::Replay(existing)
            } else {
                AcceptTurnResult::IdempotencyConflict
            };
        }
    }
    // 不允许不同 requestID 重用同一消息身份，否则重启恢复会把新回合正文去重丢弃。
    if handle
        .turn_outcomes()
        .iter()
        .any(|turn| turn.user_message_id == user_message_id)
    {
        return AcceptTurnResult::IdempotencyConflict;
    }
    if handle.deleting.load(Ordering::Acquire) {
        return AcceptTurnResult::Deleting;
    }
    if handle
        .active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return AcceptTurnResult::Busy;
    }
    *handle
        .turn_cancel
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = CancellationToken::new();
    handle.cancel_requested.store(false, Ordering::Release);
    handle.approvals.begin_turn();
    let admission = match state.scheduler.admit() {
        Ok(admission) => admission,
        Err(ScheduleError::QueueFull) => {
            handle.active.store(false, Ordering::Release);
            return AcceptTurnResult::QueueFull;
        }
    };
    let queued = admission.is_queued();
    let now = chrono::Utc::now().timestamp_millis();
    let outcome = TurnOutcome {
        turn_id: turn_id.clone(),
        user_message_id: user_message_id.clone(),
        state: if queued {
            TurnState::Queued
        } else {
            TurnState::Running
        },
        queued_at: queued.then_some(now),
        started_at: (!queued).then_some(now),
        completed_at: None,
        error: None,
    };
    let mut session = match handle.session.try_lock() {
        Ok(session) => session,
        Err(_) => {
            handle.active.store(false, Ordering::Release);
            return AcceptTurnResult::TemporarilyBusy;
        }
    };
    let queue_ticket = admission.queue_ticket();
    if let Err(error) = append_user_and_turn(
        handle,
        &mut session,
        message,
        outcome,
        queue_ticket,
        request_admission,
    ) {
        handle.active.store(false, Ordering::Release);
        return AcceptTurnResult::PersistenceFailed(error);
    }
    AcceptTurnResult::Accepted(AcceptedTurn {
        turn_id,
        user_message_id,
        admission,
    })
}
/// 功能：把用户消息与首个 queued/running outcome 原子追加到 session JSONL 和内存快照。
/// 参数：handle、session 为已预留会话，message 为输入，outcome、queue_ticket、request_admission 为接收状态。
/// 返回：消息与接收状态提交成功时返回 Ok。
/// 错误：JSONL 追加失败时返回持久化错误，不发布删除或新输入。
/// 状态：有 staged revert 时原子裁剪旧尾部与错误，并先发布公共消息删除。
/// 并发与取消：要求同 session reserve_turn 已成功；同步临界区内不可取消。
/// 权限：只修改当前 session 历史；正文不写额外日志。
/// 关联：`CHG-2026-0019`、`CHG-2026-0022`。
pub(crate) fn append_user_and_turn(
    handle: &SessionHandle,
    session: &mut Session,
    message: openpaw_core::Message,
    outcome: TurnOutcome,
    queue_ticket: Option<u64>,
    request_admission: Option<RequestAdmission>,
) -> Result<(), String> {
    let accept = TurnAcceptV1 {
        message: message.clone(),
        outcome: outcome.clone(),
        request_admission: request_admission.clone(),
    };
    let staged_message_id = handle
        .mutation
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .revert_message_id
        .clone();
    let mut published_messages = handle
        .message_cache
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let visible_len = staged_visible_len(
        &handle.id,
        &published_messages,
        staged_message_id.as_deref(),
    )
    .ok_or_else(|| "会话回退游标无法匹配完整消息历史".to_string())?;
    let mutation_event = staged_message_id.map(|message_id| {
        SessionMutationEvent::RevertCommitted(Box::new(SessionRevertCommittedV1 {
            message_id,
            accept: accept.clone(),
        }))
    });
    let previous_wire = mutation_event.is_some().then(|| handle.wire_messages());
    let persisted_event = mutation_event
        .as_ref()
        .map(SessionMutationEvent::to_session_event)
        .unwrap_or_else(|| accept.to_session_event());
    append_event(handle, &persisted_event)?;
    if mutation_event.is_some() {
        published_messages.truncate(visible_len);
        session.messages = published_messages.clone();
    }
    session.messages.push(message.clone());
    published_messages.push(message.clone());
    {
        let _snapshot = handle
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *handle
            .message_cache
            .write()
            .unwrap_or_else(|error| error.into_inner()) = published_messages.clone();
        if let Some(event) = mutation_event.as_ref() {
            let mut outcomes = handle
                .turn_outcomes
                .write()
                .unwrap_or_else(|error| error.into_inner());
            let mut admissions = handle
                .request_admissions
                .write()
                .unwrap_or_else(|error| error.into_inner());
            retain_turn_state_for_messages(
                &published_messages,
                &mut outcomes,
                &mut admissions,
                &mut handle
                    .turn_errors
                    .write()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            handle
                .mutation
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .apply(event.clone());
            handle
                .updated_at
                .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
        }
        upsert_turn_outcome(
            &mut handle
                .turn_outcomes
                .write()
                .unwrap_or_else(|error| error.into_inner()),
            outcome.clone(),
        );
        if let Some(admission) = request_admission {
            handle
                .request_admissions
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .insert(admission.request_id.clone(), admission);
        }
    }
    *handle
        .current_turn
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(ActiveTurn {
        outcome,
        queue_ticket,
    });
    if let Some(previous_wire) = previous_wire {
        let retained_ids: HashSet<_> = handle
            .wire_messages()
            .iter()
            .filter_map(|message| message["info"]["id"].as_str().map(str::to_string))
            .collect();
        for message in previous_wire {
            if let Some(id) = message["info"]["id"]
                .as_str()
                .filter(|id| !retained_ids.contains(*id))
            {
                handle.emit(events::message_removed(&handle.id, id));
            }
        }
        handle.emit(events::session_changed(
            "session.updated",
            &wire_session_snapshot(handle),
        ));
    }
    Ok(())
}
struct TurnCleanup {
    handle: Arc<SessionHandle>,
    message_count_before: usize,
    checkpoint: Option<SessionCheckpoint>,
    finished: bool,
}
impl TurnCleanup {
    fn new(handle: Arc<SessionHandle>, message_count_before: usize) -> Self {
        Self {
            handle,
            message_count_before,
            checkpoint: None,
            finished: false,
        }
    }
    fn restore_session(&self, session: &mut Session) {
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.restore(session);
        } else {
            session.messages.truncate(self.message_count_before);
        }
    }
    fn finish(&mut self) {
        self.finished = true;
    }
}
impl Drop for TurnCleanup {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _gate = self
            .handle
            .turn_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.handle.cancel_requested.store(true, Ordering::Release);
        self.handle
            .turn_cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel();
        self.handle.agent.cancel();
        self.handle.approvals.abort_all();
        if let Ok(mut session) = self.handle.session.try_lock() {
            self.restore_session(&mut session);
        }
        finish_turn_projection(&self.handle);
        self.handle.active.store(false, Ordering::Release);
    }
}
/// 功能：封装一个已接收回合的执行参数和全局调度凭据。
/// 约束：不得克隆或记录正文；每个 admission 只能消费一次。
pub(crate) struct ScheduledTurn {
    pub(crate) turn_id: String,
    pub(crate) user_message_id: String,
    pub(crate) text: String,
    pub(crate) provider_tools: Vec<openpaw_core::ToolSpec>,
    pub(crate) provider: Arc<dyn LlmProvider>,
    pub(crate) model: String,
    pub(crate) admission: TurnAdmission,
}
/// 功能：在后台任务中执行一个完整 AgentLoop 回合。
/// 参数：handle 为会话句柄，turn 为已持久化输入及调度凭据。
/// 返回：无；通过终态快照和 SSE 报告执行结果。
/// 错误：执行失败安全分类后持久化；持久化失败按 persistence_failed 闭合。
/// 状态：失败详情与终态原子提交，展示占位不进入模型上下文；完成后释放活动状态。
/// 并发与取消：由 `SessionHandle` 的 cancel token 和 AgentLoop cancel 控制。
/// 权限：沿用回合工具权限；上游错误仅公开经过过滤的摘要和可靠元数据。
/// 关联：`REQ-B-HOST-006`、`CHG-2026-0022`。
pub(crate) async fn run_turn(handle: Arc<SessionHandle>, turn: ScheduledTurn) {
    let ScheduledTurn {
        turn_id,
        user_message_id,
        text,
        provider_tools,
        provider,
        model,
        admission,
    } = turn;
    if !handle.is_active() {
        handle.active.store(false, Ordering::Release);
        handle.clear_activity();
        handle.office.clear_owner(&handle.id);
        if let Ok(mut working) = handle.office_working.try_lock() {
            working.clear();
        }
        return;
    }
    let provider_name = provider.name().to_string();
    let message_count_before = handle
        .message_cache
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .len();
    let mut cleanup = TurnCleanup::new(handle.clone(), message_count_before);
    let turn_cancel = handle
        .turn_cancel
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let queued_at = handle
        .current_turn
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(|active| active.outcome.queued_at);
    let Some(_permit) = admission.wait(turn_cancel.clone()).await else {
        let outcome = TurnOutcome {
            turn_id,
            user_message_id,
            state: TurnState::Cancelled,
            queued_at,
            started_at: None,
            completed_at: Some(chrono::Utc::now().timestamp_millis()),
            error: Some(turn_error("cancelled", "本轮已取消。", true)),
        };
        finish_turn_with_outcome(&handle, outcome, "无法持久化排队取消终态");
        cleanup.finish();
        return;
    };
    let started_at = chrono::Utc::now().timestamp_millis();
    let running = TurnOutcome {
        turn_id: turn_id.clone(),
        user_message_id: user_message_id.clone(),
        state: TurnState::Running,
        queued_at,
        started_at: Some(started_at),
        completed_at: None,
        error: None,
    };
    let running_persisted = {
        let _gate = handle
            .turn_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match handle.persist_turn_outcome(running.clone(), None) {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(session_id = %handle.id, %error, "无法持久化运行状态");
                handle.record_persistence_failure(&running);
                finish_turn_projection(&handle);
                handle.active.store(false, Ordering::Release);
                false
            }
        }
    };
    if !running_persisted {
        cleanup.finish();
        return;
    }
    let mut roles = {
        let mut previous = handle.office_working.lock().await;
        std::mem::take(&mut *previous)
    };
    let mut last_tool_role = None;
    let mut active_tool_roles = HashMap::new();
    let mut text_parts_started = HashSet::new();
    let mut reasoning_parts_started = HashSet::new();
    let mut completed_responses = HashSet::new();
    let mut response_index = 0usize;
    let mut active_response_index = 0usize;
    handle.office.turn_started(&handle.id, &mut roles);
    handle.set_activity("thinking", None, "正在思考");
    handle.emit(events::session_status(&handle.id, "busy"));
    let (mut generated, result) = {
        let mut session = handle.session.lock().await;
        // AgentLoop 会在 await 期间直接修改 messages、token 计数和压缩状态。
        // Provider、工具或事件投影若 panic，必须恢复整个回合前快照，避免半轮内存
        // 上下文进入下一次请求；生成消息尚未进入 JSONL，因此无需截断持久文件。
        if handle.cancel_requested.load(Ordering::Acquire) {
            (Vec::new(), Ok(openpaw_core::StopReason::EndTurn))
        } else {
            if !text.trim().is_empty()
                && handle
                    .title
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_str()
                    == "New session"
            {
                let title: String = text.chars().take(80).collect();
                let metadata = SessionEvent::Metadata {
                    title: Some(title.clone()),
                    mode: None,
                    model_override: None,
                    cwd: None,
                    workspace_id: None,
                    workspace_name: None,
                    task_id: None,
                };
                if let Err(error) = append_event(&handle, &metadata) {
                    tracing::error!(session_id = %handle.id, %error, "无法持久化会话标题");
                } else {
                    *handle
                        .title
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = title.clone();
                    handle
                        .mutation
                        .write()
                        .unwrap_or_else(|error| error.into_inner())
                        .title = Some(title.clone());
                }
                let session_wire = SessionWire {
                    id: handle.id.clone(),
                    title: title.clone(),
                    slug: handle.id.clone(),
                    directory: handle.workspace.display().to_string(),
                    time: SessionTime {
                        created: unix_millis(session.meta.created_at),
                        updated: handle.updated_at.load(Ordering::Acquire),
                        archived: None,
                    },
                    revert: None,
                };
                handle.emit(events::session_changed("session.updated", &session_wire));
            }
            let mut sink = |event: openpaw_engine::AgentEvent| {
                match &event {
                    openpaw_engine::AgentEvent::ToolUseStart { name, .. } => {
                        handle.set_activity(
                            "tool",
                            safe_activity_tool(name),
                            &tool_activity_task(name),
                        );
                    }
                    openpaw_engine::AgentEvent::MessageStart => {
                        if response_index > 0 {
                            completed_responses.insert(active_response_index);
                        }
                        active_response_index = response_index;
                        response_index = response_index.saturating_add(1);
                    }
                    openpaw_engine::AgentEvent::TextDelta { .. } => {
                        handle.set_activity("responding", None, "正在撰写回复");
                        if text_parts_started.insert(active_response_index) {
                            handle.emit(events::text_part_started_for_turn_response(
                                &handle.id,
                                Some(&turn_id),
                                active_response_index,
                                false,
                            ));
                        }
                    }
                    openpaw_engine::AgentEvent::ThinkingDelta { .. }
                        if reasoning_parts_started.insert(active_response_index) =>
                    {
                        handle.set_activity("thinking", None, "正在思考");
                        handle.emit(events::text_part_started_for_turn_response(
                            &handle.id,
                            Some(&turn_id),
                            active_response_index,
                            true,
                        ));
                    }
                    openpaw_engine::AgentEvent::TurnUsage { .. } => {
                        completed_responses.insert(active_response_index);
                    }
                    _ => {}
                }
                handle.office.observe(
                    &handle.id,
                    &event,
                    &mut roles,
                    &mut last_tool_role,
                    &mut active_tool_roles,
                );
                if !matches!(
                    event,
                    openpaw_engine::AgentEvent::ClassifierStatus { .. }
                        | openpaw_engine::AgentEvent::CompactionPerformed { .. }
                        | openpaw_engine::AgentEvent::TurnUsage { .. }
                ) {
                    if let Some(wire_event) = events::to_wire_for_turn_response(
                        &handle.id,
                        Some(&turn_id),
                        active_response_index,
                        &event,
                    ) {
                        handle.emit(wire_event);
                    }
                    if let openpaw_engine::AgentEvent::ToolResult {
                        name: Some(name), ..
                    } = &event
                    {
                        if matches!(name.as_str(), "todo_add" | "todo_update" | "todo_list") {
                            handle.emit(events::todo_updated(&handle.id, &handle.workspace));
                        }
                    }
                }
            };
            let attachments = {
                let messages = handle
                    .message_cache
                    .read()
                    .unwrap_or_else(|error| error.into_inner());
                openpaw_store::media::attachment_snapshot(&messages)
                    .into_values()
                    .collect()
            };
            let execution = turn_execution::run_turn(
                &handle.agent,
                &mut session,
                &provider_tools,
                &mut sink,
                TurnOptions {
                    cancel: turn_cancel.clone(),
                    provider_and_model: Some((provider, model)),
                    attachments,
                    ..Default::default()
                },
            )
            .await;
            cleanup.checkpoint = Some(execution.checkpoint);
            let result = execution.result;
            if let Err(error) = &result {
                tracing::warn!(session_id = %handle.id, turn_id = %turn_id, error = %error, "回合执行失败");
            }
            (execution.generated_messages, result)
        }
    };
    let reason = result
        .as_ref()
        .map(|reason| format!("{reason:?}"))
        .unwrap_or_else(|error| error.to_string());
    let cancelled = handle.cancel_requested.load(Ordering::Acquire) || turn_cancel.is_cancelled();
    handle
        .office
        .turn_ended(&handle.id, &mut roles, &reason, cancelled);
    *handle.office_working.lock().await = roles.clone();
    let snapshot = {
        let mut session = handle.session.lock().await;
        // terminal commit 与 abort 共用 turn_gate。先取得 gate 的一方决定本轮最终是
        // completed/failed 还是 cancelled，避免完整 assistant 已落盘后又被标成 cancelled。
        let _turn_gate = handle
            .turn_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let cancelled =
            handle.cancel_requested.load(Ordering::Acquire) || turn_cancel.is_cancelled();
        let mut assistant_index = 0usize;
        let mut generated_index = 0usize;
        let mut message_ids = std::collections::HashMap::new();
        for message in &mut generated {
            let original_id = message.uuid.take();
            if message.uuid.is_none() {
                let suffix = match message.role {
                    openpaw_core::Role::Assistant => {
                        let suffix = if assistant_index == 0 {
                            "assistant".to_string()
                        } else {
                            format!("assistant_{assistant_index}")
                        };
                        assistant_index += 1;
                        suffix
                    }
                    openpaw_core::Role::User => format!("tool_{generated_index}"),
                };
                message.uuid = Some(format!("msg_{}_{}_{}", handle.id, turn_id, suffix));
                generated_index += 1;
            }
            if let Some(original) = original_id {
                message_ids.insert(original, message.uuid.clone());
            }
        }
        for message in &mut session.messages {
            if let Some(id) = message.uuid.as_ref().and_then(|id| message_ids.get(id)) {
                message.uuid = id.clone();
            }
        }
        if cancelled {
            cleanup.restore_session(&mut session);
        }
        if response_index > 0 {
            completed_responses.insert(active_response_index);
        }
        let intended_terminal = if cancelled {
            terminal_outcome(
                &running,
                TurnState::Cancelled,
                Some(turn_error("cancelled", "本轮已取消。", true)),
            )
        } else {
            match &result {
                Ok(_) => terminal_outcome(&running, TurnState::Completed, None),
                Err(error) => terminal_outcome(
                    &running,
                    TurnState::Failed,
                    Some(classify_turn_error(error)),
                ),
            }
        };
        let mut failure = if intended_terminal.state == TurnState::Failed {
            result
                .as_ref()
                .err()
                .zip(intended_terminal.error.as_ref())
                .map(|(source, classification)| OpenCodeTurnErrorV1 {
                    message_id: if active_response_index == 0 {
                        format!("msg_{}_{}_assistant", handle.id, turn_id)
                    } else {
                        format!(
                            "msg_{}_{}_assistant_{}",
                            handle.id, turn_id, active_response_index
                        )
                    },
                    error: message_projection::provider_error(
                        source,
                        classification,
                        &provider_name,
                        &text,
                    ),
                })
        } else {
            None
        };
        let terminal = if cancelled {
            if let Err(error) = handle.persist_turn_outcome_event(&intended_terminal) {
                tracing::error!(session_id = %handle.id, %error, "无法持久化回合终态");
                handle.persistence_failure_outcome(&intended_terminal)
            } else {
                intended_terminal
            }
        } else {
            if let Err(error) = persist_finish_transaction(
                &handle,
                &turn_id,
                &generated,
                intended_terminal.clone(),
                failure.clone(),
            ) {
                tracing::error!(session_id = %handle.id, %error, "无法提交回合消息事务");
                cleanup.restore_session(&mut session);
                failure = None;
                let failed = terminal_outcome(
                    &running,
                    TurnState::Failed,
                    Some(turn_error(
                        "persistence_failed",
                        "本轮回复保存失败，请重新发送。",
                        true,
                    )),
                );
                if let Err(terminal_error) = handle.persist_turn_outcome_event(&failed) {
                    tracing::error!(session_id = %handle.id, %terminal_error, "无法持久化回合失败终态");
                    handle.persistence_failure_outcome(&failed)
                } else {
                    failed
                }
            } else {
                intended_terminal
            }
        };
        cleanup.checkpoint = None;
        cleanup.message_count_before = session.messages.len();
        let mut snapshot = handle
            .message_cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if terminal.state == TurnState::Completed {
            snapshot.extend(generated);
        }
        let committed = matches!(terminal.state, TurnState::Completed | TurnState::Cancelled);
        handle.publish_terminal_snapshot(snapshot.clone(), terminal, failure);
        if committed {
            let mut completed: Vec<_> = completed_responses.into_iter().collect();
            completed.sort_unstable();
            for response in completed {
                handle.emit(events::assistant_completed_for_turn_response(
                    &handle.id,
                    Some(&turn_id),
                    response,
                ));
            }
        }
        finish_turn_projection(&handle);
        handle.active.store(false, Ordering::Release);
        snapshot
    };
    drop(snapshot);
    cleanup.finish();
}
fn session_file_updated_at(path: &std::path::Path, fallback: i64) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(fallback)
}
fn next_fork_ordinal(home: &std::path::Path, source_id: &str) -> Result<u64, SessionMutationError> {
    openpaw_store::catalog::next_fork_ordinal(home, source_id)
        .map_err(|_| SessionMutationError::Persistence("无法扫描会话分支序号".into()))
}
fn finish_turn_projection(handle: &SessionHandle) {
    if let Some(outcome) = handle
        .turn_outcomes()
        .last()
        .filter(|turn| turn.state == TurnState::Failed)
    {
        if let Some(message) = handle
            .wire_messages()
            .iter()
            .rev()
            .find(|message| message["info"]["error"].is_object())
        {
            if let (Some(id), Ok(error)) = (
                message["info"]["id"].as_str(),
                serde_json::from_value(message["info"]["error"].clone()),
            ) {
                let failure = OpenCodeTurnErrorV1 {
                    message_id: id.to_string(),
                    error,
                };
                for event in events::turn_failed(
                    &handle.id,
                    outcome
                        .started_at
                        .or(outcome.queued_at)
                        .unwrap_or(handle.created_at),
                    outcome.completed_at.unwrap_or(handle.created_at),
                    &failure,
                ) {
                    handle.emit(event);
                }
            }
        }
    }
    handle.clear_activity();
    handle.office.clear_owner(&handle.id);
    if let Ok(mut working) = handle.office_working.try_lock() {
        working.clear();
    }
    handle.emit(events::session_status(&handle.id, "idle"));
    handle.emit(events::session_idle(&handle.id));
}
fn finish_turn_with_outcome(
    handle: &SessionHandle,
    outcome: TurnOutcome,
    log_message: &'static str,
) {
    let _gate = handle
        .turn_gate
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Err(error) = handle.persist_turn_outcome(outcome.clone(), None) {
        tracing::error!(session_id = %handle.id, %error, "{log_message}");
        handle.record_persistence_failure(&outcome);
    }
    finish_turn_projection(handle);
    handle.active.store(false, Ordering::Release);
}
/// 将内部工具名转换为可读的中文活动提示；不暴露参数和工具结果。
fn tool_activity_task(name: &str) -> String {
    let label = match name {
        "web_search" => "搜索网页",
        "web_fetch" => "读取网页",
        "web_extract" => "提取网页内容",
        "read_file" => "读取文件",
        "grep" => "搜索文件",
        "glob" => "扫描文件",
        "write_file" => "撰写文件",
        "edit_file" => "修改文件",
        "notebook_edit" => "更新笔记",
        "bash" => "执行受控命令",
        "todo_add" => "新增任务记录",
        "todo_update" => "更新任务记录",
        "todo_list" => "读取任务记录",
        _ => "调用工具",
    };
    format!("正在{label}")
}
/// 只把已登记的工具名暴露给状态接口；未知或异常超长名称统一脱敏。
fn safe_activity_tool(name: &str) -> Option<&str> {
    match name {
        "web_search" | "web_fetch" | "web_extract" | "read_file" | "grep" | "glob"
        | "write_file" | "edit_file" | "notebook_edit" | "bash" | "todo_add" | "todo_update"
        | "todo_list" => Some(name),
        _ => None,
    }
}
fn append_event(handle: &SessionHandle, event: &SessionEvent) -> Result<(), String> {
    let mut writer = handle
        .writer
        .lock()
        .map_err(|_| "会话写入器锁状态异常".to_string())?;
    journal::append(&mut writer, event)
}
fn persist_finish_transaction(
    handle: &SessionHandle,
    turn_id: &str,
    messages: &[openpaw_core::Message],
    terminal: TurnOutcome,
    open_code_error: Option<OpenCodeTurnErrorV1>,
) -> Result<(), String> {
    let mut writer = handle
        .writer
        .lock()
        .map_err(|_| "会话写入器锁状态异常".to_string())?;
    journal::finish(&mut writer, turn_id, messages, terminal, open_code_error)
}
fn repair_torn_session_tail(path: &std::path::Path) -> Result<(), SessionLoadError> {
    journal::read_repaired(path)
        .map(|_| ())
        .map_err(classify_session_error)
}
#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::StreamEvent;

    #[tokio::test]
    async fn failed_agent_configuration_leaves_no_ghost_session() {
        const TEST_NAME: &str =
            "runtime::tests::failed_agent_configuration_leaves_no_ghost_session";
        if std::env::var_os("OPENPAW_GHOST_SESSION_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env("OPENPAW_GHOST_SESSION_CHILD", "1")
                .env("OPENPAW_MAX_OUTPUT_TOKENS", "invalid")
                .output()
                .expect("spawn isolated test");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }

        let home = tempfile::tempdir().expect("home");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            ..Default::default()
        });
        assert!(state.create_session(None, None, None, None).await.is_err());
        assert!(state.summaries().await.is_empty());
        let session_dir = home.path().join("sessions");
        let count = std::fs::read_dir(session_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .count();
        assert_eq!(count, 0);
    }

    #[test]
    fn retryable_provider_failures_use_stable_retryable_turn_codes() {
        let overloaded = classify_turn_error(&openpaw_core::Error::Provider(
            "OpenAI 兼容服务流式请求失败：Our servers are currently overloaded".into(),
        ));
        assert_eq!(overloaded.code, "provider_overloaded");
        assert!(overloaded.retryable);

        let unavailable = classify_turn_error(&openpaw_core::Error::server_error(
            503,
            "service unavailable",
        ));
        assert_eq!(unavailable.code, "provider_overloaded");
        assert!(unavailable.retryable);

        let server_error = classify_turn_error(&openpaw_core::Error::server_error(
            500,
            "internal server error",
        ));
        assert_eq!(server_error.code, "provider_server_error");
        assert!(server_error.retryable);

        let protocol_error = classify_turn_error(&openpaw_core::Error::Provider(
            "OpenAI 流在 [DONE] 前结束".into(),
        ));
        assert_eq!(protocol_error.code, "provider_protocol_error");
        assert!(protocol_error.retryable);

        let empty_response = classify_turn_error(&openpaw_core::Error::Provider(
            "OpenAI 流返回空响应，拒绝生成成功响应".into(),
        ));
        assert_eq!(empty_response.code, "provider_protocol_error");
        assert!(empty_response.retryable);

        for message in [
            "SSE 在 UTF-8 码点中间结束：残留 2 字节",
            "OpenAI tool_call 0 缺少 id 或 function.name",
            "Anthropic tool_use JSON 不完整或无效",
            "max_output_tokens exceeded before a complete response",
        ] {
            let classified = classify_turn_error(&openpaw_core::Error::Provider(message.into()));
            assert_eq!(classified.code, "provider_protocol_error", "{message}");
            assert!(classified.retryable, "{message}");
        }
    }

    struct ToolThenErrorProvider {
        calls: std::sync::atomic::AtomicUsize,
        panic_after_tool: bool,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ToolThenErrorProvider {
        async fn complete(
            &self,
            _request: openpaw_core::CompletionRequest,
        ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
            unreachable!("AgentLoop uses stream in this test")
        }

        async fn stream(
            &self,
            _request: openpaw_core::CompletionRequest,
        ) -> openpaw_core::Result<
            futures::stream::BoxStream<'static, openpaw_core::Result<openpaw_core::StreamEvent>>,
        > {
            let call = self.calls.fetch_add(1, Ordering::AcqRel);
            if call > 0 {
                if self.panic_after_tool {
                    panic!("intentional provider panic after tool result");
                }
                return Ok(Box::pin(futures::stream::iter([
                    Ok(StreamEvent::TextDelta {
                        index: 0,
                        text: "partial failure".into(),
                    }),
                    Err(openpaw_core::Error::server_error(
                        503,
                        "provider failed after tool result",
                    )),
                ])));
            }
            let response = openpaw_core::CompletionResponse {
                content: vec![
                    openpaw_core::ContentBlock::ToolUse {
                        id: "error_call".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({"path":"README.md"}),
                    },
                    openpaw_core::ContentBlock::Text {
                        text: "partial assistant".into(),
                    },
                ],
                stop_reason: openpaw_core::StopReason::ToolUse,
                ..Default::default()
            };
            Ok(Box::pin(futures::stream::iter(vec![Ok(
                openpaw_core::StreamEvent::Final(Box::new(response)),
            )])))
        }

        fn capabilities(&self) -> openpaw_core::Capabilities {
            openpaw_core::Capabilities {
                tool_use: true,
                streaming: true,
                ..Default::default()
            }
        }

        fn name(&self) -> &str {
            "tool-then-error"
        }

        fn default_model(&self) -> &str {
            "error-model"
        }
    }

    #[test]
    fn dropping_promoted_queue_entry_returns_running_capacity() {
        let scheduler = TurnScheduler::new(
            TurnSchedulerConfig {
                max_concurrent_turns: 1,
                max_queued_turns: 1,
            }
            .into(),
        );
        let running = scheduler.admit().expect("running admission");
        let queued = scheduler.admit().expect("queued admission");

        drop(running);
        drop(queued);

        assert!(matches!(
            scheduler.admit().expect("capacity must be returned"),
            TurnAdmission::Running(_)
        ));
    }

    fn scheduled_turn(
        state: &AppState,
        handle: &SessionHandle,
        turn_id: String,
        user_message_id: &str,
        text: &str,
        provider: Arc<dyn LlmProvider>,
    ) -> ScheduledTurn {
        ScheduledTurn {
            turn_id,
            user_message_id: user_message_id.into(),
            text: text.into(),
            provider_tools: handle.tools.clone(),
            provider,
            model: "mock-1".into(),
            admission: state.scheduler.admit().expect("test turn admission"),
        }
    }

    #[tokio::test]
    async fn abort_before_detached_turn_and_next_turn_are_linearized() {
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");

        assert!(handle.reserve_turn());
        let first_turn = handle.next_turn_id();
        handle.abort().await;
        run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                first_turn,
                "test-user-1",
                "cancelled before start",
                build_provider("mock").expect("mock provider"),
            ),
        )
        .await;
        assert!(!handle.is_active());
        assert!(handle
            .session
            .lock()
            .await
            .messages
            .iter()
            .all(|message| message.role != openpaw_core::Role::Assistant));

        assert!(handle.reserve_turn());
        let second_turn = handle.next_turn_id();
        run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                second_turn,
                "test-user-2",
                "next turn",
                build_provider("mock").expect("mock provider"),
            ),
        )
        .await;
        assert!(!handle.is_active());
        assert!(handle
            .session
            .lock()
            .await
            .messages
            .iter()
            .any(|message| message.role == openpaw_core::Role::Assistant));
    }

    #[tokio::test]
    async fn provider_failure_rolls_back_partial_context_and_survives_restart() {
        for panic_after_tool in [false, true] {
            let home = tempfile::tempdir().expect("home");
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("README.md"), "tool fixture").expect("fixture");
            let config = HostConfig {
                home: home.path().into(),
                workspace: workspace.path().into(),
                office_url: None,
                ..HostConfig::default()
            };
            let state = AppState::new(config.clone());
            let handle = state
                .create_session(Some("mock"), None, None, Some("error rollback"))
                .await
                .expect("session");
            let id = handle.id.clone();
            let mut events = handle.global_events.subscribe();
            assert!(handle.reserve_turn());
            let turn_id = handle.next_turn_id();
            let user_message_id = format!("msg_{id}_{turn_id}_user");
            let initial = TurnOutcome {
                turn_id: turn_id.clone(),
                user_message_id: user_message_id.clone(),
                state: TurnState::Running,
                queued_at: None,
                started_at: Some(chrono::Utc::now().timestamp_millis()),
                completed_at: None,
                error: None,
            };
            {
                let mut session = handle.session.lock().await;
                append_user_and_turn(
                    &handle,
                    &mut session,
                    openpaw_core::Message::user_text("trigger provider error")
                        .with_uuid(user_message_id.clone()),
                    initial,
                    None,
                    None,
                )
                .expect("initial turn");
            }

            run_turn(
                handle.clone(),
                scheduled_turn(
                    &state,
                    &handle,
                    turn_id.clone(),
                    &user_message_id,
                    "trigger provider error",
                    Arc::new(ToolThenErrorProvider {
                        calls: std::sync::atomic::AtomicUsize::new(0),
                        panic_after_tool,
                    }),
                ),
            )
            .await;

            assert!(!handle.is_active());
            let messages = handle.session.lock().await.messages.clone();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].role, openpaw_core::Role::User);
            assert_eq!(messages[0].text(), "trigger provider error");
            let session = handle.session.lock().await;
            assert_eq!(session.total_input_tokens, 0);
            assert_eq!(session.total_output_tokens, 0);
            drop(session);
            let cached = handle
                .message_cache
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            assert_eq!(
                serde_json::to_value(&cached).expect("cached messages json"),
                serde_json::to_value(&messages).expect("session messages json")
            );
            let outcomes = handle.turn_outcomes();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].state, TurnState::Failed);
            assert_eq!(
                outcomes[0].error.as_ref().map(|error| error.code.as_str()),
                Some(if panic_after_tool {
                    "turn_failed"
                } else {
                    "provider_overloaded"
                })
            );
            assert_eq!(
                outcomes[0].error.as_ref().map(|error| error.retryable),
                Some(!panic_after_tool)
            );
            assert!(
                panic_after_tool
                    || std::iter::from_fn(|| events.try_recv().ok())
                        .take_while(|event| event.event_type != "session.idle")
                        .any(|event| {
                            event.properties["info"]["id"]
                                == format!("msg_{id}_{turn_id}_assistant_1")
                                && event.properties["info"]["time"]["completed"].is_number()
                        })
            );

            drop(handle);
            drop(state);
            let restarted = AppState::new(config);
            let restored = restarted.get(&id).await.expect("restored session");
            let restored_messages = restored.session.lock().await.messages.clone();
            assert_eq!(
                serde_json::to_value(&restored_messages).expect("restored messages json"),
                serde_json::to_value(&messages).expect("original messages json")
            );
            assert_eq!(restored.turn_outcomes(), outcomes);
        }
    }

    #[tokio::test]
    async fn workspace_survives_host_restart_and_is_used_by_summaries() {
        let home = tempfile::tempdir().expect("home");
        let original_workspace = tempfile::tempdir().expect("original workspace");
        let fallback_workspace = tempfile::tempdir().expect("fallback workspace");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: fallback_workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(
                Some("mock"),
                Some("mock-1"),
                Some(original_workspace.path().into()),
                None,
            )
            .await
            .expect("session");
        let id = handle.id.clone();
        drop(handle);
        drop(state);

        let restarted = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: fallback_workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let restored = restarted.get(&id).await.expect("restored session");
        assert_eq!(restored.workspace, original_workspace.path());
        let summaries = restarted.summaries().await;
        let summary = summaries
            .iter()
            .find(|summary| summary.session.id == id)
            .expect("session summary");
        assert_eq!(
            PathBuf::from(&summary.session.directory),
            original_workspace.path()
        );
        assert_eq!(summary.session.slug, id);
        assert_eq!(summary.session.time.created, summary.created_at);
        assert_eq!(summary.session.time.updated, summary.updated_at);
    }

    #[tokio::test]
    async fn deleted_session_cannot_be_reserved_through_a_stale_handle() {
        let home = tempfile::tempdir().expect("home");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let id = handle.id.clone();

        assert!(state.delete_session(&id).await.expect("delete"));
        assert!(handle.is_deleting());
        assert!(!handle.reserve_turn());
        assert!(state.get(&id).await.is_none());
        assert!(!home
            .path()
            .join("sessions")
            .join(format!("{id}.jsonl"))
            .exists());
    }

    #[tokio::test]
    async fn lazy_load_and_delete_cannot_resurrect_a_session() {
        let home = tempfile::tempdir().expect("home");
        let initial = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = initial
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let id = handle.id.clone();
        drop(handle);
        drop(initial);

        let restarted = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let load_state = restarted.clone();
        let load_id = id.clone();
        let load = tokio::spawn(async move { load_state.get(&load_id).await });
        let delete_state = restarted.clone();
        let delete_id = id.clone();
        let delete = tokio::spawn(async move { delete_state.delete_session(&delete_id).await });
        let (loaded, deleted) = tokio::join!(load, delete);

        assert!(deleted.expect("delete task").expect("delete"));
        if let Some(stale) = loaded.expect("load task") {
            assert!(!stale.reserve_turn());
        }
        assert!(restarted.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn every_tool_loop_assistant_response_gets_a_completion_event() {
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("README.md"), "fixture").expect("fixture");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let mut events = handle.global_events.subscribe();
        assert!(handle.reserve_turn());
        run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                handle.next_turn_id(),
                "test-user",
                "read",
                Arc::new(openpaw_llm::MockProvider::scripted(vec![
                    openpaw_llm::MockMode::ToolUse {
                        tool_name: "read_file".into(),
                        input: serde_json::json!({"path":"README.md"}),
                        text: "reading".into(),
                    },
                    openpaw_llm::MockMode::Fixed("done".into()),
                ])),
            ),
        )
        .await;
        let emitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let assistant_ids: HashSet<String> = emitted
            .iter()
            .filter(|event| event.event_type == "message.updated")
            .filter(|event| event.properties["info"]["role"] == "assistant")
            .filter_map(|event| event.properties["info"]["id"].as_str().map(str::to_string))
            .collect();
        let completed_ids: HashSet<String> = emitted
            .iter()
            .filter(|event| event.event_type == "message.updated")
            .filter(|event| event.properties["info"]["time"]["completed"].is_number())
            .filter_map(|event| event.properties["info"]["id"].as_str().map(str::to_string))
            .collect();
        assert_eq!(assistant_ids.len(), 2);
        assert!(assistant_ids.is_subset(&completed_ids));
    }

    #[tokio::test]
    async fn message_persistence_failure_rolls_back_session_and_cache() {
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, Some("持久化测试"))
            .await
            .expect("session");
        let user = openpaw_core::Message::user_text("写入失败").with_uuid("user");
        {
            let mut session = handle.session.lock().await;
            session.messages.push(user.clone());
        }
        handle.message_cache.write().unwrap().push(user.clone());
        // 模拟删除/关闭阶段导致回合消息无法追加。
        handle.writer.lock().unwrap().take();
        assert!(handle.reserve_turn());
        run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                handle.next_turn_id(),
                "test-user",
                "写入失败",
                build_provider("mock").expect("mock provider"),
            ),
        )
        .await;
        let session_messages = handle.session.lock().await.messages.clone();
        let cached_messages = handle
            .message_cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(session_messages.len(), 1);
        assert_eq!(session_messages[0].role, openpaw_core::Role::User);
        assert_eq!(session_messages[0].text(), "写入失败");
        assert_eq!(cached_messages.len(), 1);
        assert_eq!(cached_messages[0].role, openpaw_core::Role::User);
        assert_eq!(cached_messages[0].text(), "写入失败");
    }

    #[tokio::test]
    async fn idle_event_observes_final_cache_and_inactive_turn() {
        let home = tempfile::tempdir().expect("home");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let mut events = handle.global_events.subscribe();
        assert!(handle.reserve_turn());
        let turn = tokio::spawn(run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                handle.next_turn_id(),
                "test-user",
                "final cache",
                Arc::new(openpaw_llm::MockProvider::fixed("最终回复")),
            ),
        ));

        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .expect("idle event timeout")
                .expect("event");
            if event.event_type != "session.idle" {
                continue;
            }
            assert!(!handle.is_active());
            let cached = handle
                .message_cache
                .read()
                .unwrap_or_else(|error| error.into_inner());
            assert!(cached.iter().any(|message| {
                message.role == openpaw_core::Role::Assistant && message.text() == "最终回复"
            }));
            let outcomes = handle.turn_outcomes();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].state, TurnState::Completed);
            break;
        }
        turn.await.expect("turn task");
    }

    #[tokio::test]
    async fn finish_transaction_does_not_publish_terminal_before_message_cache() {
        let home = tempfile::tempdir().expect("home");
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let turn_id = handle.next_turn_id();
        let terminal = TurnOutcome {
            turn_id: turn_id.clone(),
            user_message_id: "user-message".into(),
            state: TurnState::Completed,
            queued_at: None,
            started_at: Some(chrono::Utc::now().timestamp_millis()),
            completed_at: Some(chrono::Utc::now().timestamp_millis()),
            error: None,
        };
        let assistant = openpaw_core::Message::assistant_text("最终回复").with_uuid("assistant");

        persist_finish_transaction(
            &handle,
            &turn_id,
            std::slice::from_ref(&assistant),
            terminal.clone(),
            None,
        )
        .expect("finish transaction");

        assert!(handle.turn_outcomes().is_empty());
        assert!(handle
            .message_cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty());

        handle
            .message_cache
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .push(assistant);
        let _snapshot = handle
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        handle.project_turn_outcome_locked(terminal, None);
        drop(_snapshot);
        assert_eq!(handle.turn_outcomes()[0].state, TurnState::Completed);
    }

    #[tokio::test]
    async fn delayed_stream_abort_emits_no_delta_after_idle() {
        let home = tempfile::tempdir().expect("home");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("fixture.txt"), "tool output").unwrap();
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            workspace: workspace.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state
            .create_session(None, None, None, None)
            .await
            .expect("session");
        let baseline = {
            let mut session = handle.session.lock().await;
            session.messages.push(
                openpaw_core::Message::user_text("accepted stream input").with_uuid("test-user"),
            );
            session.total_input_tokens = 31;
            session.total_output_tokens = 7;
            *handle.message_cache.write().unwrap() = session.messages.clone();
            serde_json::to_value(&*session).unwrap()
        };
        let mut events = handle.global_events.subscribe();
        assert!(handle.reserve_turn());
        let turn = tokio::spawn(run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                handle.next_turn_id(),
                "test-user",
                "stream",
                Arc::new(openpaw_llm::MockProvider::scripted(vec![
                    openpaw_llm::MockMode::ToolUse {
                        tool_name: "read_file".into(),
                        input: serde_json::json!({"path":"fixture.txt"}),
                        text: "read first".into(),
                    },
                    openpaw_llm::MockMode::DelayedStreamText {
                        text: "abcdefgh".into(),
                        chunks: 8,
                        delay_ms: 30,
                    },
                ])),
            ),
        ));
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
                .await
                .expect("first delta timeout")
                .expect("event");
            if event.event_type == "message.part.delta" {
                break;
            }
        }
        handle.abort().await;
        turn.await.expect("turn task");
        assert_eq!(
            serde_json::to_value(&*handle.session.lock().await).unwrap(),
            baseline,
            "cancel after a completed tool must restore usage and accepted input"
        );
        let remaining: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let idle_index = remaining
            .iter()
            .position(|event| event.event_type == "session.idle")
            .expect("idle event");
        assert!(
            remaining[..idle_index]
                .iter()
                .any(|event| event.properties["info"]["time"]["completed"].is_number())
                && remaining
                    .iter()
                    .skip(idle_index + 1)
                    .all(|event| event.event_type != "message.part.delta")
                && !handle.is_active()
        );
    }

    struct FinishFailureProvider(std::sync::Weak<SessionHandle>);

    #[async_trait::async_trait]
    impl LlmProvider for FinishFailureProvider {
        async fn complete(
            &self,
            _: openpaw_core::CompletionRequest,
        ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
            unreachable!("fixture uses streaming")
        }
        async fn stream(
            &self,
            _: openpaw_core::CompletionRequest,
        ) -> openpaw_core::Result<
            futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>,
        > {
            self.0.upgrade().unwrap().writer.lock().unwrap().take();
            let response = openpaw_core::CompletionResponse {
                content: vec![openpaw_core::ContentBlock::Text {
                    text: "uncommitted reply".into(),
                }],
                stop_reason: openpaw_core::StopReason::EndTurn,
                usage: openpaw_core::Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Default::default()
                },
            };
            Ok(Box::pin(futures::stream::iter([Ok(StreamEvent::Final(
                Box::new(response),
            ))])))
        }
        fn capabilities(&self) -> openpaw_core::Capabilities {
            Default::default()
        }
        fn name(&self) -> &str {
            "finish-failure"
        }
        fn default_model(&self) -> &str {
            "fixture"
        }
    }

    #[tokio::test]
    async fn finish_failure_restores_full_session_after_successful_execution() {
        let home = tempfile::tempdir().unwrap();
        let state = AppState::new(HostConfig {
            home: home.path().into(),
            office_url: None,
            ..HostConfig::default()
        });
        let handle = state.create_session(None, None, None, None).await.unwrap();
        let baseline = {
            let mut session = handle.session.lock().await;
            session
                .messages
                .push(openpaw_core::Message::user_text("accepted input").with_uuid("test-user"));
            session.total_input_tokens = 31;
            session.total_output_tokens = 7;
            *handle.message_cache.write().unwrap() = session.messages.clone();
            serde_json::to_value(&*session).unwrap()
        };
        assert!(handle.reserve_turn());
        run_turn(
            handle.clone(),
            scheduled_turn(
                &state,
                &handle,
                handle.next_turn_id(),
                "test-user",
                "accepted input",
                Arc::new(FinishFailureProvider(Arc::downgrade(&handle))),
            ),
        )
        .await;
        assert_eq!(
            serde_json::to_value(&*handle.session.lock().await).unwrap(),
            baseline
        );
        assert_eq!(
            serde_json::to_value(&*handle.message_cache.read().unwrap()).unwrap(),
            baseline["messages"]
        );
        let outcome = handle.turn_outcomes().pop().unwrap();
        assert_eq!(outcome.state, TurnState::Failed);
        assert_eq!(outcome.error.unwrap().code, "persistence_failed");
        assert!(!handle.is_active());
    }
}
