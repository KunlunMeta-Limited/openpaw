//! 功能：管理每个 session 的 AgentLoop、取消、审批、事件广播和 JSONL 写入。
//! 输入输出边界：接收 HTTP 层的回合请求，向 events/office 输出事件和动作。
//! 设计约束：每个 session 独占一个活动回合；不同 session 可以并行；本模块拥有 session 写入权。
//! 禁止承担：不定义 OpenCode 路由，不让 Office 失败改变 AgentLoop 结果。
//! 关联：`REQ-B-HOST-002`、`REQ-B-HOST-006`、`REQ-B-HOST-007`。
use crate::approval::HostApprovalManager;
use crate::events;
use crate::office::OfficeDispatcher;
use crate::protocol::{
    unix_millis, HostConfig, OpenCodeEvent, RequestAdmission, SessionSummary, SessionTime,
    SessionWire, TurnAcceptV1, TurnError, TurnFinishCommitV1, TurnFinishPrepareV1, TurnOutcome,
    TurnSchedulerConfig, TurnState,
};
use futures::FutureExt;
use openpaw_core::{LlmProvider, Session, SessionEvent};
use openpaw_engine::AgentLoop;
use openpaw_store::{SessionError, SessionReader, SessionWriter};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
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
struct SchedulerState {
    running: usize,
    next_ticket: u64,
    queued: VecDeque<Arc<QueuedEntry>>,
}
struct QueuedEntry {
    ticket: u64,
    promoted: AtomicBool,
    notify: Notify,
}
/// 为全部 session 提供有界 FIFO 调度；同步锁线性化接收、取消和 permit 交接。
pub(crate) struct TurnScheduler {
    config: TurnSchedulerConfig,
    state: std::sync::Mutex<SchedulerState>,
}
impl TurnScheduler {
    /// 使用已校验容量创建进程级调度器，后续操作通过内部锁线性化。
    fn new(config: TurnSchedulerConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: std::sync::Mutex::new(SchedulerState {
                running: 0,
                next_ticket: 0,
                queued: VecDeque::new(),
            }),
        })
    }
    /// 在写入用户消息前原子接收回合，多个 session 按接收顺序获得队列票号。
    pub(crate) fn admit(self: &Arc<Self>) -> Result<TurnAdmission, ScheduleError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running < self.config.max_concurrent_turns {
            state.running += 1;
            return Ok(TurnAdmission::Running(TurnPermit {
                scheduler: self.clone(),
                released: false,
            }));
        }
        if state.queued.len() >= self.config.max_queued_turns {
            return Err(ScheduleError::QueueFull);
        }
        state.next_ticket = state.next_ticket.saturating_add(1);
        let entry = Arc::new(QueuedEntry {
            ticket: state.next_ticket,
            promoted: AtomicBool::new(false),
            notify: Notify::new(),
        });
        state.queued.push_back(entry.clone());
        Ok(TurnAdmission::Queued(QueuedTurn {
            scheduler: self.clone(),
            entry,
            armed: true,
        }))
    }
    /// 查询票号的 1 基队列位置，与接收、取消和 permit 释放共用同一锁。
    pub(crate) fn queue_position(&self, ticket: u64) -> Option<usize> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .queued
            .iter()
            .position(|entry| entry.ticket == ticket)
            .map(|index| index + 1)
    }
    fn cancel_queued(&self, ticket: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = state.queued.iter().position(|entry| entry.ticket == ticket) else {
            return false;
        };
        state.queued.remove(index);
        true
    }
    fn release_running(&self) {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(next) = state.queued.pop_front() {
                next.promoted.store(true, Ordering::Release);
                Some(next)
            } else {
                state.running = state.running.saturating_sub(1);
                None
            }
        };
        if let Some(next) = next {
            next.notify.notify_one();
        }
    }
}
/// 功能：表示 Host 调度接收失败的稳定原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduleError {
    QueueFull,
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
/// 持有运行槽位或 FIFO 排队凭据；`wait` 同时监听晋升与 session CancellationToken。
pub(crate) enum TurnAdmission {
    Running(TurnPermit),
    Queued(QueuedTurn),
}
impl TurnAdmission {
    pub(crate) fn is_queued(&self) -> bool {
        matches!(self, Self::Queued(_))
    }
    pub(crate) fn queue_ticket(&self) -> Option<u64> {
        match self {
            Self::Running(_) => None,
            Self::Queued(queued) => Some(queued.entry.ticket),
        }
    }
    /// 等待运行槽位或 session 取消；竞争结果以调度锁中的线性化顺序为准。
    pub(crate) async fn wait(self, cancel: CancellationToken) -> Option<TurnPermit> {
        match self {
            Self::Running(permit) => Some(permit),
            Self::Queued(queued) => queued.wait(cancel).await,
        }
    }
}
pub(crate) struct TurnPermit {
    scheduler: Arc<TurnScheduler>,
    released: bool,
}
impl Drop for TurnPermit {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            self.scheduler.release_running();
        }
    }
}
pub(crate) struct QueuedTurn {
    scheduler: Arc<TurnScheduler>,
    entry: Arc<QueuedEntry>,
    armed: bool,
}
impl QueuedTurn {
    async fn wait(mut self, cancel: CancellationToken) -> Option<TurnPermit> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                if self.scheduler.cancel_queued(self.entry.ticket) {
                    self.armed = false;
                    None
                } else if self.entry.promoted.load(Ordering::Acquire) {
                    self.armed = false;
                    Some(TurnPermit { scheduler: self.scheduler.clone(), released: false })
                } else {
                    self.armed = false;
                    None
                }
            }
            _ = self.entry.notify.notified() => {
                self.armed = false;
                Some(TurnPermit { scheduler: self.scheduler.clone(), released: false })
            }
        }
    }
}
impl Drop for QueuedTurn {
    fn drop(&mut self) {
        if self.armed
            && !self.scheduler.cancel_queued(self.entry.ticket)
            && self.entry.promoted.load(Ordering::Acquire)
        {
            // release_running 已把活动槽位交给本票据；若等待 future 此时被丢弃，
            // 必须继续把槽位交给下一项或归还容量，不能让并发额度永久泄漏。
            self.scheduler.release_running();
        }
    }
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
    pub title: Mutex<String>,
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
        let (path, length) = writer_snapshot(self)?;
        if let Err(error) = append_event(self, &outcome.to_session_event()) {
            if let Err(rollback_error) = restore_writer_snapshot(self, &path, length) {
                return Err(format!("{error}；回合状态写入回滚失败：{rollback_error}"));
            }
            return Err(error);
        }
        Ok(())
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
        let event = outcome.to_session_event();
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "会话写入器锁状态异常".to_string())?;
        let rollback = writer.as_ref().and_then(|current| {
            std::fs::metadata(current.path())
                .ok()
                .map(|metadata| (current.path().to_path_buf(), metadata.len()))
        });
        if let Some(current) = writer.as_mut() {
            if current.append(&event).is_ok() {
                return Ok(());
            }
        }
        *writer = None;
        if let Some((path, length)) = rollback {
            truncate_session_file(&path, length)?;
        }
        let mut recovered = SessionWriter::open_append(&self.session_path)
            .map_err(|error| format!("无法重建会话写入器：{error}"))?;
        recovered
            .append(&event)
            .map_err(|error| format!("无法补写回合终态：{error}"))?;
        *writer = Some(recovered);
        Ok(())
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
    turn_outcomes: Vec<TurnOutcome>,
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
            scheduler: TurnScheduler::new(scheduler_config),
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
        let provider = build_provider(&read.meta.provider).unwrap_or_else(|error| {
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
        let projection = restored_session_projection(&read.events)?;
        let messages = projection.messages;
        let mut turn_outcomes = projection.turn_outcomes;
        recover_hanging_outcomes(&mut turn_outcomes, &mut writer)?;
        let request_admissions = projection.request_admissions;
        let mut session = Session::new(read.meta.model.clone(), read.meta.provider.clone());
        session.meta = read.meta;
        session.messages = messages.clone();
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
        let title = session_title(&read.events, &messages);
        Ok(self.build_session_handle(SessionHandleInit {
            session,
            messages,
            writer,
            session_path: path,
            agent,
            approvals,
            workspace,
            tools,
            title,
            turn_outcomes,
            request_admissions,
        }))
    }
    /// 功能：创建 session、JSONL meta 和 AgentLoop 运行资源。
    /// 并发与取消：创建本身不启动回合；后续 prompt 由 reserve_turn 控制。
    pub async fn create_session(
        self: &Arc<Self>,
        provider_name: Option<&str>,
        model: Option<&str>,
        workspace: Option<PathBuf>,
        title: Option<&str>,
    ) -> Result<Arc<SessionHandle>, String> {
        let provider_tag = provider_name.unwrap_or(&self.config.provider);
        let provider = build_provider(provider_tag)?;
        let model_name = model.unwrap_or(provider.default_model()).to_string();
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
            turn_outcomes: Vec::new(),
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
            turn_outcomes,
            request_admissions,
        } = init;
        let id = session.meta.id.clone();
        let created_at = unix_millis(session.meta.created_at);
        let turn_sequence = restored_turn_sequence(&turn_outcomes);
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
            title: Mutex::new(title),
            updated_at: std::sync::atomic::AtomicI64::new(created_at),
            cancel_requested: AtomicBool::new(false),
            deleting: AtomicBool::new(false),
            global_events: self.global_events.clone(),
            turn_sequence: AtomicU64::new(turn_sequence),
            office_working: Mutex::new(HashSet::new()),
            activity: std::sync::Mutex::new(None),
            current_turn: std::sync::Mutex::new(None),
            snapshot_gate: std::sync::RwLock::new(()),
            turn_outcomes: std::sync::RwLock::new(turn_outcomes),
            request_admissions: std::sync::RwLock::new(request_admissions),
            turn_gate: std::sync::Mutex::new(()),
            turn_cancel: std::sync::Mutex::new(CancellationToken::new()),
        })
    }
    pub async fn wire_session(&self, handle: &SessionHandle) -> SessionWire {
        SessionWire {
            id: handle.id.clone(),
            title: handle.title.lock().await.clone(),
            slug: handle.id.clone(),
            directory: handle.workspace.display().to_string(),
            time: SessionTime {
                created: handle.created_at,
                updated: handle.updated_at.load(Ordering::Acquire),
            },
        }
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
    pub async fn summaries(&self) -> Vec<SessionSummary> {
        let dir = self.config.home.join("sessions");
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
            let Ok(entries) = std::fs::read_dir(dir) else {
                return out;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(read) = SessionReader::read(&path) else {
                    continue;
                };
                let Ok(projection) = restored_session_projection(&read.events) else {
                    continue;
                };
                let title = session_title(&read.events, &projection.messages);
                let directory =
                    absolute_workspace(session_workspace(&read.events, &default_workspace))
                        .unwrap_or_else(|_| default_workspace.clone())
                        .to_string_lossy()
                        .into_owned();
                let meta = read.meta;
                let updated = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                    .unwrap_or_else(|| unix_millis(meta.created_at));
                let active = active_ids.contains(&meta.id);
                out.push(SessionSummary {
                    session: SessionWire {
                        slug: meta.id.clone(),
                        id: meta.id,
                        title,
                        directory,
                        time: SessionTime {
                            created: unix_millis(meta.created_at),
                            updated,
                        },
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
/// 功能：对所有工具调用要求人工确认的 Host guardrail。
pub struct ManualGuardrail;
#[async_trait::async_trait]
impl openpaw_core::ToolGuardrail for ManualGuardrail {
    async fn check(
        &self,
        name: &str,
        _args: &serde_json::Value,
    ) -> openpaw_core::GuardrailDecision {
        openpaw_core::GuardrailDecision::Confirm(format!("manual approval required for `{name}`"))
    }
}
fn configured_agent(
    provider: Arc<dyn LlmProvider>,
    model: String,
    tool_host: Arc<dyn openpaw_core::ToolHost>,
    approvals: Arc<HostApprovalManager>,
) -> Result<AgentLoop, String> {
    let mut agent = AgentLoop::new(provider)
        .with_model(model)
        .with_tool_host(tool_host);
    if let Ok(value) = std::env::var("OPENPAW_MAX_OUTPUT_TOKENS") {
        let parsed = value.trim().parse::<std::num::NonZeroU32>();
        let max_tokens = parsed
            .map_err(|_| "OPENPAW_MAX_OUTPUT_TOKENS 必须是正整数".to_string())?
            .get();
        agent = agent.with_max_tokens(max_tokens)
    }
    if std::env::var("OPENPAW_ENGINE_PERMISSIONS").as_deref() == Ok("manual") {
        agent = agent.with_guardrail(Arc::new(ManualGuardrail));
    }
    Ok(agent.with_approval_manager(approvals))
}
/// Anthropic provider 的缺省模型，未设置 `OPENPAW_ANTHROPIC_MODEL` 时使用。
/// 单一来源：`build_provider`、`default_model` 与 `/provider` 列表都引用它，避免多处漂移。
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4";
/// OpenAI 兼容 provider 的缺省模型，未设置 `OPENPAW_OPENAI_MODEL` 时使用。
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o";

/// 功能：按 provider 标识创建 Host 使用的 LLM provider。
/// 并发与取消：provider 可被多个 session 独占使用，回合取消由 AgentLoop 负责。
pub fn build_provider(name: &str) -> Result<Arc<dyn LlmProvider>, String> {
    match name {
        "echo" => Ok(Arc::new(openpaw_llm::EchoProvider::new())),
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
        "anthropic" => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| "未设置 ANTHROPIC_API_KEY".to_string())?;
            let model = std::env::var("OPENPAW_ANTHROPIC_MODEL")
                .unwrap_or_else(|_| DEFAULT_ANTHROPIC_MODEL.into());
            let mut provider = openpaw_llm::AnthropicProvider::new(key, model);
            if let Some(base_url) = provider_base_url("OPENPAW_ANTHROPIC_BASE_URL")? {
                provider = provider.with_base_url(base_url);
            }
            Ok(Arc::new(provider))
        }
        "openai" => {
            let key =
                std::env::var("OPENAI_API_KEY").map_err(|_| "未设置 OPENAI_API_KEY".to_string())?;
            let model = std::env::var("OPENPAW_OPENAI_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.into());
            let mut provider = openpaw_llm::OpenAiProvider::new(key, model);
            if let Some(base_url) = provider_base_url("OPENPAW_OPENAI_BASE_URL")? {
                provider = provider.with_base_url(base_url);
            }
            Ok(Arc::new(provider))
        }
        other => Err(format!("不支持的 provider：`{other}`")),
    }
}
/// 功能：解析指定 provider 的默认模型，不触发凭据读取或网络连接。
/// 用途：HTTP 幂等重放必须在 Provider 构造前确定请求指纹。
pub fn default_model(name: &str) -> Result<String, String> {
    match name {
        "echo" => Ok("echo-1".into()),
        "mock" => Ok("mock-1".into()),
        "anthropic" => Ok(std::env::var("OPENPAW_ANTHROPIC_MODEL")
            .unwrap_or_else(|_| DEFAULT_ANTHROPIC_MODEL.into())),
        "openai" => {
            Ok(std::env::var("OPENPAW_OPENAI_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.into()))
        }
        other => Err(format!("不支持的 provider：`{other}`")),
    }
}
fn provider_base_url(variable: &str) -> Result<Option<String>, String> {
    let Ok(value) = std::env::var(variable) else {
        return Ok(None);
    };
    let value = value.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(value).map_err(|error| format!("{variable} 不是有效 URL：{error}"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(format!(
            "{variable} 必须是无 query/fragment 的 http(s) 基础 URL"
        ));
    }
    Ok(Some(value.to_string()))
}
fn session_title(events: &[SessionEvent], messages: &[openpaw_core::Message]) -> String {
    events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Metadata {
                title: Some(title), ..
            } => Some(title.clone()),
            _ => None,
        })
        .or_else(|| {
            messages
                .iter()
                .find(|message| message.role == openpaw_core::Role::User)
                .map(openpaw_core::Message::text)
        })
        .unwrap_or_else(|| "New session".into())
        .chars()
        .take(80)
        .collect()
}
struct RestoredSessionProjection {
    messages: Vec<openpaw_core::Message>,
    turn_outcomes: Vec<TurnOutcome>,
    request_admissions: HashMap<String, RequestAdmission>,
}
struct PendingFinishTransaction {
    turn_id: String,
    messages: Vec<openpaw_core::Message>,
}
const MAX_FINISH_TRANSACTION_MESSAGES: usize = 4096;
fn restored_session_projection(
    events: &[SessionEvent],
) -> Result<RestoredSessionProjection, SessionLoadError> {
    let mut projection = RestoredSessionProjection {
        messages: Vec::new(),
        turn_outcomes: Vec::new(),
        request_admissions: HashMap::new(),
    };
    let mut message_ids = HashSet::new();
    let mut pending_finish: Option<PendingFinishTransaction> = None;
    for event in events {
        if let Some(prepare) = TurnFinishPrepareV1::from_session_event(event) {
            pending_finish = Some(PendingFinishTransaction {
                turn_id: prepare.turn_id,
                messages: Vec::new(),
            });
            continue;
        }
        if let Some(pending) = pending_finish.as_mut() {
            if let SessionEvent::Message(message) = event {
                if pending.messages.len() >= MAX_FINISH_TRANSACTION_MESSAGES {
                    return Err(SessionLoadError::Corrupt);
                }
                pending.messages.push(message.clone());
                continue;
            }
            if let Some(commit) = TurnFinishCommitV1::from_session_event(event) {
                let pending = pending_finish.take().ok_or(SessionLoadError::Corrupt)?;
                apply_finish_commit(&mut projection, &mut message_ids, pending, commit)?;
                continue;
            }
            // prepare 后未出现合法 commit 即视为崩溃残留；忽略整批生成消息，
            // 当前事件仍按旧格式或下一事务继续恢复。
            pending_finish = None;
        }
        if let Some(accept) = TurnAcceptV1::from_session_event(event) {
            apply_turn_accept(&mut projection, &mut message_ids, accept)?;
        } else if let Some(outcome) = TurnOutcome::from_session_event(event) {
            apply_restored_outcome(&mut projection.turn_outcomes, outcome)?;
        } else if let Some(admission) = RequestAdmission::from_session_event(event) {
            apply_restored_admission(&mut projection.request_admissions, admission)?;
        } else if TurnFinishCommitV1::from_session_event(event).is_some() {
            return Err(SessionLoadError::Corrupt);
        } else if let SessionEvent::Message(message) = event {
            push_restored_message(&mut projection.messages, &mut message_ids, message.clone());
        } else if is_transaction_event(event) {
            return Err(SessionLoadError::Corrupt);
        }
    }
    Ok(projection)
}
fn apply_turn_accept(
    projection: &mut RestoredSessionProjection,
    message_ids: &mut HashSet<String>,
    accept: TurnAcceptV1,
) -> Result<(), SessionLoadError> {
    if accept.message.role != openpaw_core::Role::User
        || accept.message.uuid.as_deref() != Some(accept.outcome.user_message_id.as_str())
        || accept.outcome.state.is_terminal()
    {
        return Err(SessionLoadError::Corrupt);
    }
    if let Some(admission) = accept.request_admission.as_ref() {
        if admission.turn_id != accept.outcome.turn_id
            || admission.user_message_id != accept.outcome.user_message_id
        {
            return Err(SessionLoadError::Corrupt);
        }
    }
    push_restored_message(&mut projection.messages, message_ids, accept.message);
    apply_restored_outcome(&mut projection.turn_outcomes, accept.outcome)?;
    if let Some(admission) = accept.request_admission {
        apply_restored_admission(&mut projection.request_admissions, admission)?;
    }
    Ok(())
}
fn apply_finish_commit(
    projection: &mut RestoredSessionProjection,
    message_ids: &mut HashSet<String>,
    pending: PendingFinishTransaction,
    commit: TurnFinishCommitV1,
) -> Result<(), SessionLoadError> {
    if pending.turn_id != commit.turn_id
        || commit.terminal.turn_id != commit.turn_id
        || !commit.terminal.state.is_terminal()
        || pending.messages.len() != commit.generated_message_ids.len()
    {
        return Err(SessionLoadError::Corrupt);
    }
    let actual_ids: Option<Vec<_>> = pending
        .messages
        .iter()
        .map(|message| message.uuid.clone())
        .collect();
    let Some(actual_ids) = actual_ids else {
        return Err(SessionLoadError::Corrupt);
    };
    if actual_ids != commit.generated_message_ids
        || actual_ids.iter().collect::<HashSet<_>>().len() != actual_ids.len()
    {
        return Err(SessionLoadError::Corrupt);
    }
    for message in pending.messages {
        push_restored_message(&mut projection.messages, message_ids, message);
    }
    apply_restored_outcome(&mut projection.turn_outcomes, commit.terminal)
}
fn push_restored_message(
    messages: &mut Vec<openpaw_core::Message>,
    message_ids: &mut HashSet<String>,
    message: openpaw_core::Message,
) {
    if let Some(id) = message.uuid.as_ref() {
        if !message_ids.insert(id.clone()) {
            return;
        }
    }
    messages.push(message);
}
fn apply_restored_outcome(
    outcomes: &mut Vec<TurnOutcome>,
    outcome: TurnOutcome,
) -> Result<(), SessionLoadError> {
    if let Some(existing) = outcomes
        .iter_mut()
        .find(|existing| existing.turn_id == outcome.turn_id)
    {
        if existing.user_message_id != outcome.user_message_id {
            return Err(SessionLoadError::Corrupt);
        }
        if existing.state.is_terminal() {
            if outcome.state.is_terminal() && existing != &outcome {
                return Err(SessionLoadError::Corrupt);
            }
            return Ok(());
        }
        *existing = outcome;
    } else {
        outcomes.push(outcome);
    }
    Ok(())
}
fn apply_restored_admission(
    admissions: &mut HashMap<String, RequestAdmission>,
    admission: RequestAdmission,
) -> Result<(), SessionLoadError> {
    if let Some(existing) = admissions.get(&admission.request_id) {
        if existing != &admission {
            return Err(SessionLoadError::Corrupt);
        }
        return Ok(());
    }
    admissions.insert(admission.request_id.clone(), admission);
    Ok(())
}
fn is_transaction_event(event: &SessionEvent) -> bool {
    let SessionEvent::Unknown(value) = event else {
        return false;
    };
    matches!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("turn_accept_v1" | "turn_finish_prepare_v1" | "turn_finish_commit_v1")
    )
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
    let mut read = SessionReader::read(path).map_err(classify_session_error)?;
    let expected_id = path.file_stem().and_then(|value| value.to_str());
    if Some(read.meta.id.as_str()) != expected_id {
        return Err(SessionLoadError::Corrupt);
    }
    if read.torn_tail_lines > 0 {
        repair_torn_session_tail(path)?;
        read = SessionReader::read(path).map_err(classify_session_error)?;
    }
    let mut outcomes = restored_session_projection(&read.events)?.turn_outcomes;
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
fn restored_turn_sequence(outcomes: &[TurnOutcome]) -> u64 {
    outcomes
        .iter()
        .filter_map(|outcome| outcome.turn_id.split('_').next()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
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
/// 并发与取消：要求同 session reserve_turn 已成功；同步临界区内不可取消。
pub(crate) fn append_user_and_turn(
    handle: &SessionHandle,
    session: &mut Session,
    message: openpaw_core::Message,
    outcome: TurnOutcome,
    queue_ticket: Option<u64>,
    request_admission: Option<RequestAdmission>,
) -> Result<(), String> {
    let (path, length) = writer_snapshot(handle)?;
    let accept = TurnAcceptV1 {
        message: message.clone(),
        outcome: outcome.clone(),
        request_admission: request_admission.clone(),
    };
    if let Err(error) = append_event(handle, &accept.to_session_event()) {
        restore_writer_snapshot(handle, &path, length)
            .map_err(|rollback_error| format!("{error}；接收事务回滚失败：{rollback_error}"))?;
        return Err(error);
    }
    session.messages.push(message.clone());
    {
        let _snapshot = handle
            .snapshot_gate
            .write()
            .unwrap_or_else(|error| error.into_inner());
        handle
            .message_cache
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .push(message);
        upsert_turn_outcome(
            &mut handle
                .turn_outcomes
                .write()
                .unwrap_or_else(|error| error.into_inner()),
            outcome.clone(),
        );
    }
    if let Some(admission) = request_admission {
        handle
            .request_admissions
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(admission.request_id.clone(), admission);
    }
    *handle
        .current_turn
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(ActiveTurn {
        outcome,
        queue_ticket,
    });
    Ok(())
}
struct TurnCleanup {
    handle: Arc<SessionHandle>,
    message_count_before: usize,
    finished: bool,
}
impl TurnCleanup {
    fn new(handle: Arc<SessionHandle>, message_count_before: usize) -> Self {
        Self {
            handle,
            message_count_before,
            finished: false,
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
            session.messages.truncate(self.message_count_before);
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
/// 并发与取消：由 `SessionHandle` 的 cancel token 和 AgentLoop cancel 控制。
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
    let (before, result) = {
        let mut session = handle.session.lock().await;
        let before = session.messages.len();
        // AgentLoop 会在 await 期间直接修改 messages、token 计数和压缩状态。
        // Provider、工具或事件投影若 panic，必须恢复整个回合前快照，避免半轮内存
        // 上下文进入下一次请求；生成消息尚未进入 JSONL，因此无需截断持久文件。
        let checkpoint = session.clone();
        if handle.cancel_requested.load(Ordering::Acquire) {
            (before, Ok(openpaw_core::StopReason::EndTurn))
        } else {
            if !text.trim().is_empty() && handle.title.lock().await.as_str() == "New session" {
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
                    *handle.title.lock().await = title.clone();
                }
                let session_wire = SessionWire {
                    id: handle.id.clone(),
                    title: title.clone(),
                    slug: handle.id.clone(),
                    directory: handle.workspace.display().to_string(),
                    time: SessionTime {
                        created: unix_millis(session.meta.created_at),
                        updated: handle.updated_at.load(Ordering::Acquire),
                    },
                };
                let mut properties =
                    serde_json::to_value(&session_wire).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(object) = properties.as_object_mut() {
                    object.insert("sessionID".into(), serde_json::json!(handle.id));
                }
                handle.emit(OpenCodeEvent::new("session.updated", properties));
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
                        if response_index > 0 && completed_responses.insert(active_response_index) {
                            handle.emit(events::assistant_completed_for_turn_response(
                                &handle.id,
                                Some(&turn_id),
                                active_response_index,
                            ));
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
                    openpaw_engine::AgentEvent::TurnUsage { .. }
                        if completed_responses.insert(active_response_index) =>
                    {
                        handle.emit(events::assistant_completed_for_turn_response(
                            &handle.id,
                            Some(&turn_id),
                            active_response_index,
                        ));
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
            let execution =
                std::panic::AssertUnwindSafe(handle.agent.run_turn_with_provider_and_cancel_token(
                    &mut session,
                    None,
                    &provider_tools,
                    &mut sink,
                    turn_cancel.clone(),
                    (provider, model),
                ))
                .catch_unwind()
                .await;
            let result = execution.unwrap_or_else(|_| {
                tracing::error!(session_id = %handle.id, turn_id = %turn_id, "AgentLoop 异常中止");
                Err(openpaw_core::Error::session("agent loop panicked"))
            });
            if result.is_err() {
                if let Err(error) = &result {
                    tracing::warn!(session_id = %handle.id, turn_id = %turn_id, error = %error, "回合执行失败");
                }
                *session = checkpoint;
            }
            (before, result)
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
        for message in session.messages.iter_mut().skip(before) {
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
        }
        if cancelled {
            session.messages.truncate(before);
        }
        if response_index > 0 && completed_responses.insert(active_response_index) {
            handle.emit(events::assistant_completed_for_turn_response(
                &handle.id,
                Some(&turn_id),
                active_response_index,
            ));
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
        let terminal = if cancelled {
            if let Err(error) = handle.persist_turn_outcome_event(&intended_terminal) {
                tracing::error!(session_id = %handle.id, %error, "无法持久化回合终态");
                handle.persistence_failure_outcome(&intended_terminal)
            } else {
                intended_terminal
            }
        } else {
            let generated = session.messages[before..].to_vec();
            if let Err(error) =
                persist_finish_transaction(&handle, &turn_id, &generated, intended_terminal.clone())
            {
                tracing::error!(session_id = %handle.id, %error, "无法提交回合消息事务");
                session.messages.truncate(before);
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
        let snapshot = session.messages.clone();
        handle.publish_terminal_snapshot(snapshot.clone(), terminal);
        finish_turn_projection(&handle);
        handle.active.store(false, Ordering::Release);
        snapshot
    };
    drop(snapshot);
    cleanup.finish();
}
fn finish_turn_projection(handle: &SessionHandle) {
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
    writer
        .as_mut()
        .ok_or_else(|| "会话正在删除".to_string())?
        .append(event)
        .map_err(|e| e.to_string())
}
fn persist_finish_transaction(
    handle: &SessionHandle,
    turn_id: &str,
    messages: &[openpaw_core::Message],
    terminal: TurnOutcome,
) -> Result<(), String> {
    if messages.len() > MAX_FINISH_TRANSACTION_MESSAGES
        || terminal.turn_id != turn_id
        || !terminal.state.is_terminal()
    {
        return Err("回合消息事务不符合持久化约束".into());
    }
    let generated_message_ids: Option<Vec<_>> = messages
        .iter()
        .map(|message| message.uuid.clone())
        .collect();
    let generated_message_ids =
        generated_message_ids.ok_or_else(|| "回合生成消息缺少稳定 UUID".to_string())?;
    if generated_message_ids.iter().collect::<HashSet<_>>().len() != generated_message_ids.len() {
        return Err("回合生成消息 UUID 重复".into());
    }
    let (path, length) = writer_snapshot(handle)?;
    let result = (|| {
        append_event(
            handle,
            &TurnFinishPrepareV1 {
                turn_id: turn_id.to_string(),
            }
            .to_session_event(),
        )?;
        for message in messages {
            append_event(handle, &SessionEvent::Message(message.clone()))?;
        }
        append_event(
            handle,
            &TurnFinishCommitV1 {
                turn_id: turn_id.to_string(),
                generated_message_ids,
                terminal: terminal.clone(),
            }
            .to_session_event(),
        )
    })();
    if let Err(error) = result {
        restore_writer_snapshot(handle, &path, length)
            .map_err(|rollback_error| format!("{error}；回合消息事务回滚失败：{rollback_error}"))?;
        return Err(error);
    }
    Ok(())
}
/// 读取当前 JSONL 文件长度，作为回合消息批量追加的强制回滚点。
fn writer_snapshot(handle: &SessionHandle) -> Result<(std::path::PathBuf, u64), String> {
    let writer = handle
        .writer
        .lock()
        .map_err(|_| "会话写入器锁状态异常".to_string())?;
    let writer = writer.as_ref().ok_or_else(|| "会话正在删除".to_string())?;
    let path = writer.path().to_path_buf();
    let length = std::fs::metadata(&path)
        .map_err(|error| format!("无法读取会话文件长度：{error}"))?
        .len();
    Ok((path, length))
}
/// 将 JSONL 文件截回指定长度；用于回合消息追加失败后的事务回滚。
fn truncate_session_file(path: &std::path::Path, length: u64) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("打开会话文件失败：{error}"))?;
    file.set_len(length)
        .map_err(|error| format!("截断会话文件失败：{error}"))?;
    file.sync_data()
        .map_err(|error| format!("同步截断后的会话文件失败：{error}"))
}
/// 将 session 文件截回事务快照并重建 append writer，避免旧句柄继续持有错误尾部。
fn restore_writer_snapshot(
    handle: &SessionHandle,
    path: &std::path::Path,
    length: u64,
) -> Result<(), String> {
    let mut writer = handle
        .writer
        .lock()
        .map_err(|_| "会话写入器锁状态异常".to_string())?;
    *writer = None;
    truncate_session_file(path, length)?;
    let recovered =
        SessionWriter::open_append(path).map_err(|error| format!("无法重建会话写入器：{error}"))?;
    *writer = Some(recovered);
    Ok(())
}
/// 删除 SessionReader 已确认的最后一条 torn JSONL 行；必须在重新打开 writer 前执行。
fn repair_torn_session_tail(path: &std::path::Path) -> Result<(), SessionLoadError> {
    let bytes = std::fs::read(path).map_err(|error| classify_io_kind(error.kind()))?;
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
        .map(|index| index + 1)
        .unwrap_or(0);
    let start = bytes[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    truncate_session_file(path, start as u64).map_err(|_| SessionLoadError::Unavailable)
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
        let scheduler = TurnScheduler::new(TurnSchedulerConfig {
            max_concurrent_turns: 1,
            max_queued_turns: 1,
        });
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
                "stream",
                Arc::new(openpaw_llm::MockProvider::stream_text_delayed(
                    "abcdefgh", 8, 30,
                )),
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
}
