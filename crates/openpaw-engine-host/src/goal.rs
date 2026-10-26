//! 功能：保存会话归属的 Goal、计划、执行和验收事实，强制回合与任务权限。
//! 输入输出边界：真实用户回合和模型工具输入进入，持久事实由 runtime 唯一 writer 提交。
//! 禁止承担：不直接执行交付工具，不写共享 Todo，不创建后台驱动或独立 writer。
//! 关联：CHG-2026-0050、goal-command tickets 01–03。

use crate::runtime::SessionHandle;
use async_trait::async_trait;
use openpaw_core::{
    ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor, ToolHost, ToolHostValidator,
    ToolSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Default, Serialize, Deserialize)]
struct GoalState {
    records: Vec<GoalRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
struct GoalRecord {
    id: String,
    objective: String,
    acceptance_criteria: Vec<String>,
    plan_version: u64,
    displayed_version: u64,
    phase: String,
    tasks: Vec<GoalTask>,
    reason: String,
    history: Vec<Value>,
    #[serde(default)]
    confirmed_version: u64,
    #[serde(default)]
    observations: Vec<Observation>,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalTask {
    id: String,
    title: String,
    expected_result: String,
    acceptance_criteria: Vec<String>,
    dependencies: Vec<String>,
    #[serde(default = "pending")]
    status: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delegation_id: Option<String>,
}

fn pending() -> String {
    "pending".into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    criterion: usize,
    observation_id: String,
    conclusion: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Observation {
    id: String,
    task_id: Option<String>,
    tool: String,
    input: Value,
    output: String,
    verification: bool,
    #[serde(default)]
    whole_goal: bool,
}

struct UserTurn {
    turn_id: String,
    message_id: String,
    text: String,
    editable: bool,
    executing: bool,
    task_id: Option<String>,
}

/// 功能：承载单个会话的 Goal 事实与当前真实用户回合，不持有 session 强引用。
/// 数据边界：只由 runtime 组装并恢复；模型不可提供执行许可或用户身份。
pub(crate) struct GoalContext {
    owner: String,
    state: Mutex<GoalState>,
    user: Mutex<Option<UserTurn>>,
    handle: Mutex<Weak<SessionHandle>>,
}

/// 功能：把一次真实子执行的终态限定到原 Goal、任务和委派尝试。
/// 数据边界：仅 runtime 持有；模型和持久状态不能恢复此执行许可。
pub(crate) struct GoalDelegation {
    context: Arc<GoalContext>,
    goal_id: String,
    task_id: String,
    turn_id: String,
    attempt: String,
    finished: bool,
}

impl GoalDelegation {
    /// 功能：在子回合准入前关联真实子会话及其展示身份。
    /// 参数：child 为真实会话编号，workspace 为既有 Office 子空间身份。
    /// 返回：绑定已提交时成功。
    /// 错误：父回合取消、任务已变更或提交失败时拒绝准入。
    /// 状态：仅更新已预留任务归属。
    /// 并发与取消：与 Goal 更新共用 user→state 锁顺序，重新检查原许可。
    /// 权限：不能以其他正在执行的任务代替当前绑定。
    /// 关联：runtime::run_delegated。
    pub(crate) fn bind(&self, child: &str, workspace: &str) -> Result<(), String> {
        let user = self.context.user.lock().unwrap_or_else(|e| e.into_inner());
        if !user
            .as_ref()
            .is_some_and(|user| user.executing && user.turn_id == self.turn_id)
        {
            return Err("Goal 原委派回合已结束".into());
        }
        self.context.check_cancelled()?;
        let mut state = self.context.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next
            .records
            .last_mut()
            .filter(|goal| goal.id == self.goal_id && goal.phase == "active")
            .ok_or("Goal 委派已停止")?;
        let task = goal
            .tasks
            .iter_mut()
            .find(|task| {
                task.id == self.task_id
                    && task.status == "running"
                    && task.delegation_id.as_deref() == Some(&self.attempt)
            })
            .ok_or("Goal 任务委派绑定已改变")?;
        task.assignee_id = Some(child.into());
        task.child_workspace_id = Some(workspace.into());
        self.context.commit(&next)?;
        *state = next;
        drop(state);
        drop(user);
        self.context.project();
        Ok(())
    }

    /// 功能：保存子 Agent 的真实终态，但不替主 Agent 验收。
    /// 参数：status 为真实回合终态，result 为本次真实结果或失败原因。
    /// 返回：持久化成功。
    /// 错误：journal 失败返回错误；原事实不被覆盖。
    /// 状态：完成转 produced，失败或取消转 interrupted；保留后续暂停决定。
    /// 并发与取消：只更新仍匹配本次尝试的任务，迟到结果不改变新执行。
    /// 权限：无 Goal 控制、重规划或验收权限。
    /// 关联：父前台子任务 join 与 GoalTask 产出事实。
    pub(crate) fn finish(&mut self, status: &str, result: &str) -> Result<(), String> {
        let mut state = self.context.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        if let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| goal.id == self.goal_id)
        {
            if let Some(task) = goal.tasks.iter_mut().find(|task| {
                task.id == self.task_id
                    && task.delegation_id.as_deref() == Some(&self.attempt)
                    && matches!(task.status.as_str(), "running" | "interrupted")
            }) {
                task.result = result.into();
                if task.status == "running" {
                    task.status = if status == "completed" {
                        "produced"
                    } else {
                        "interrupted"
                    }
                    .into();
                }
                goal.history.push(json!({"action":"child_result","task_id":self.task_id,"child_session_id":task.assignee_id,"status":status,"turn_id":self.turn_id}));
                self.context.commit(&next)?;
                *state = next;
            }
        }
        self.finished = true;
        drop(state);
        self.context.project();
        Ok(())
    }
}

impl Drop for GoalDelegation {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = self.finish(
                "interrupted",
                "子委派中断，需核对实际结果后继续；不自动重跑",
            ) {
                tracing::error!(%error, "Goal 子委派中断事实保存失败");
            }
        }
    }
}

impl GoalContext {
    /// 功能：创建无执行授权的会话 Goal 上下文。
    /// 参数：owner 为会话稳定身份。
    /// 返回：空上下文的共享引用。
    /// 错误：无。
    /// 状态：不创建目标或写入文件。
    /// 并发与取消：独立短锁；没有后台工作。
    /// 权限：只归属 owner。
    /// 关联：runtime::session_tool_host。
    pub(crate) fn new(owner: &str) -> Arc<Self> {
        Arc::new(Self {
            owner: owner.into(),
            state: Mutex::new(GoalState::default()),
            user: Mutex::new(None),
            handle: Mutex::new(Weak::new()),
        })
    }

    /// 功能：关联 runtime 唯一写入入口。
    /// 参数：handle 为已组装的本会话。
    /// 返回：无。
    /// 错误：无。
    /// 状态：只安装弱引用。
    /// 并发与取消：组装时调用，无 await。
    /// 权限：不授予执行许可。
    /// 关联：runtime::build_session_handle。
    pub(crate) fn attach(&self, handle: &Arc<SessionHandle>) {
        *self.handle.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(handle);
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
            .is_none_or(|handle| handle.main_cancel_token().is_cancelled())
        {
            return Err("当前 Goal 回合已取消，不再启动工作".into());
        }
        Ok(())
    }

    /// 功能：在真实子创建前原子预留一个满足依赖的 Goal 任务。
    /// 参数：task 为可选 Goal Task 编号，attempt 为本次委派唯一身份。
    /// 返回：Goal 执行返回独占绑定；普通聊天不绑定。
    /// 错误：缺少许可、未验收依赖、重复绑定或持久化失败时不启动子执行。
    /// 状态：先持久标记 running，未准入或 future 丢弃转 interrupted。
    /// 并发与取消：user→state 锁内核对许可、状态与取消，重复委派只有一项能成功。
    /// 权限：持久 active 或其他子任务 running 不构成执行许可。
    /// 关联：runtime::run_delegated、GoalDelegation。
    pub(crate) fn reserve_delegation(
        self: &Arc<Self>,
        task: Option<&str>,
        attempt: &str,
    ) -> Result<Option<GoalDelegation>, String> {
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if task.is_none()
            && !user
                .as_ref()
                .is_some_and(|user| user.executing || user.editable)
            && state
                .records
                .last()
                .is_none_or(|goal| matches!(goal.phase.as_str(), "ended" | "completed"))
        {
            return Ok(None);
        }
        let user = user
            .as_ref()
            .filter(|user| user.executing)
            .ok_or("本回合没有 Goal 委派执行许可")?;
        self.check_cancelled()?;
        let id = task.ok_or("Goal 委派必须提供独立的 goal_task_id，task_id 仍表示真实子会话")?;
        let mut next = state.clone();
        let goal = next
            .records
            .last_mut()
            .filter(|goal| goal.phase == "active")
            .ok_or("当前 Goal 未授权执行")?;
        let index = goal
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or("委派任务不属于当前 Goal 计划")?;
        if goal.tasks[index].status != "pending" {
            return Err("Goal 任务已被启动或已有产出，不能重复委派".into());
        }
        if !goal.tasks[index].dependencies.iter().all(|dep| {
            goal.tasks
                .iter()
                .any(|task| task.id == *dep && task.status == "accepted")
        }) {
            return Err("委派任务依赖尚未验收，不能启动".into());
        }
        let task = &mut goal.tasks[index];
        task.status = "running".into();
        task.assignee_id = None;
        task.delegation_id = Some(attempt.into());
        task.child_workspace_id = Some(attempt.into());
        let goal_id = goal.id.clone();
        let turn_id = user.turn_id.clone();
        self.commit(&next)?;
        *state = next;
        drop(state);
        // 返回后 runtime 负责准入；不在持有 user 锁时投影或等待子执行。
        Ok(Some(GoalDelegation {
            context: self.clone(),
            goal_id,
            task_id: id.into(),
            turn_id,
            attempt: attempt.into(),
            finished: false,
        }))
    }

    /// 功能：从原会话事件恢复 Goal 历史。
    /// 参数：events 为 Store 读取的完整事件。
    /// 返回：合法快照恢复成功。
    /// 错误：归属或快照损坏时返回脱敏错误。
    /// 状态：恢复事实，不恢复用户回合许可。
    /// 并发与取消：仅冷加载调用。
    /// 权限：拒绝其他会话的 Goal 事件。
    /// 关联：session_goal_state_v1。
    pub(crate) fn restore(&self, events: &[openpaw_core::SessionEvent]) -> Result<(), String> {
        for event in events {
            if let openpaw_core::SessionEvent::Unknown(value) = event {
                if value["type"] == "session_goal_state_v1" {
                    if value["owner"] != self.owner {
                        return Err("Goal 归属损坏".into());
                    }
                    *self.state.lock().unwrap_or_else(|e| e.into_inner()) =
                        serde_json::from_value(value["state"].clone())
                            .map_err(|_| "Goal 记录损坏")?;
                }
            }
        }
        Ok(())
    }

    fn commit(&self, state: &GoalState) -> Result<(), String> {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
            .ok_or("Goal 会话不可用")?;
        handle.commit_goal(json!({"type":"session_goal_state_v1","owner":self.owner,"state":state}))
    }

    /// 功能：绑定真实用户回合，处理创建和最近展示版本的初始执行确认。
    /// 参数：turn 与 message 为 runtime 身份，text 为未替换的原输入。
    /// 返回：确定性拒绝回复，或交给模型规划。
    /// 错误：持久化失败停止处理。
    /// 状态：新草稿和确认事实先落盘；重复目标不会覆盖当前记录。
    /// 并发与取消：每个会话已有单活动回合，Goal 锁串行化提交。
    /// 权限：草稿只有编辑权限；初始确认只授予本回合执行许可。
    /// 关联：runtime::run_turn。
    pub(crate) fn begin(
        &self,
        turn: &str,
        message: &str,
        text: &str,
    ) -> Result<Option<String>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let command = text.trim();
        let creating = command
            .strip_prefix("/goal ")
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "continue");
        let current = state.records.last();
        let unfinished =
            current.is_some_and(|goal| goal.phase != "ended" && goal.phase != "completed");
        let reply = if let Some(objective) = creating {
            if unfinished {
                Some(
                    "当前聊天已有未结束的 Goal，请先明确结束旧目标，再创建新目标；旧记录会保留。"
                        .into(),
                )
            } else {
                let mut next = state.clone();
                next.records.push(GoalRecord {
                    id: format!("goal-{}", uuid::Uuid::new_v4().simple()),
                    objective: objective.into(),
                    acceptance_criteria: Vec::new(),
                    plan_version: 0,
                    displayed_version: 0,
                    phase: "draft".into(),
                    tasks: Vec::new(),
                    reason: "等待生成初始计划".into(),
                    history: Vec::new(),
                    confirmed_version: 0,
                    observations: Vec::new(),
                    evidence: Vec::new(),
                });
                self.commit(&next)?;
                *state = next;
                None
            }
        } else if command == "确认执行"
            && current.is_some_and(|goal| {
                goal.phase == "draft"
                    && goal.plan_version > 0
                    && goal.displayed_version == goal.plan_version
            })
        {
            let mut next = state.clone();
            let goal = next.records.last_mut().unwrap();
            goal.confirmed_version = goal.plan_version;
            goal.phase = "active".into();
            goal.reason = "用户确认最近展示的完整计划，开始前台执行".into();
            self.commit(&next)?;
            *state = next;
            None
        } else if unfinished && matches!(command, "/goal continue" | "确认执行") {
            Some("当前计划尚未开始执行。请核对最近展示的完整计划。".into())
        } else {
            None
        };
        let editable = reply.is_none()
            && state
                .records
                .last()
                .is_some_and(|goal| goal.phase == "draft");
        *self.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(UserTurn {
            turn_id: turn.into(),
            message_id: message.into(),
            text: text.into(),
            editable,
            executing: reply.is_none()
                && command == "确认执行"
                && state
                    .records
                    .last()
                    .is_some_and(|goal| goal.phase == "active"),
            task_id: None,
        });
        Ok(reply)
    }

    /// 功能：提供当前 Goal 的规划或执行验收约束。
    /// 参数：无。
    /// 返回：存在未结束 Goal 时的系统上下文。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：短锁，不持 session 锁。
    /// 权限：明确禁止规划期间的交付工具。
    /// 关联：goal_read、goal_update。
    pub(crate) fn prompt(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let goal = state.records.last()?;
        if goal.phase != "draft" {
            return (!matches!(goal.phase.as_str(), "ended" | "completed")).then(|| format!("当前 Goal {}。先 goal_read。仅在本次确认授权回合按依赖推进：goal_update(action=start_task,task_id) 后执行工具；record_result(result) 只表示产出未验证。独立任务可并行 delegate_task(goal_task_id=计划任务编号)，task_id 只表示真实子会话；子结果为 produced，主 Agent 用 start_task 选中该 produced 任务只做读取检查，不重新执行交付。读取实际交付物或运行检查后，accept_task 携带 evidence 数组，每个验收条件提供 criterion(从0起)、真实 observation_id 和 conclusion。工具结果中的 Goal observation 编号可追溯，不能编造或仅凭完成声明。所有任务 accepted 后另做整体检查，再 accept_goal，不能把任务100%当作整体完成。执行期间不得修改整体目标或验收条件；遇到普通失败先修复。已有原预算、取消及180秒期限不重置。当前事实：{}",goal.id,json!(goal)));
        }
        Some(format!("当前处于 Goal 待确认规划阶段。整体目标：{}。先 goal_read，再用 goal_update(action=set_plan) 提交完整计划。每项任务必须有稳定 id、title、expected_result、acceptance_criteria、dependencies；按独立交付结果拆解，整体验收条件必须明确。用户自然语言修改时重交完整新版本，保留身份和必要历史。禁止执行交付工作、写文件、shell、桌面动作、委派或共享 Todo。明确结束旧目标才用 action=end，并提供原用户输入中的 user_excerpt；不得把结束冒充验收完成。最终完整计划由运行时展示，等待用户确认执行。",goal.objective))
    }

    fn read(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .records
            .last()
            .map(|goal| json!(goal))
            .unwrap_or(json!({}))
    }

    /// 功能：读取当前完整 Goal 事实供 Office 只读投影。
    /// 参数：无。
    /// 返回：已有完整计划时返回事实，否则无。
    /// 错误：无。
    /// 状态：只读，不授予执行权限。
    /// 并发与取消：克隆短锁快照后释放。
    /// 权限：仅当前会话。
    /// 关联：Office 任务和白板投影。
    pub(crate) fn current(&self) -> Option<Value> {
        let mut value = self.read();
        value["session_id"] = json!(self.owner);
        (value["plan_version"].as_u64().unwrap_or(0) > 0).then_some(value)
    }

    fn update(&self, input: Value) -> Result<Value, String> {
        let mut user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if user.as_ref().is_some_and(|user| user.executing) {
            return self.execute_update(user.as_mut().unwrap(), input);
        }
        let user = user
            .as_ref()
            .filter(|user| user.editable)
            .ok_or("本回合没有 Goal 用户编辑权限")?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("当前聊天没有 Goal")?;
        if goal.phase != "draft" {
            return Err("已结束的 Goal 不可重新修改，请创建新目标".into());
        }
        if input["goal_id"] != goal.id
            || input["expected_version"].as_u64() != Some(goal.plan_version)
        {
            return Err("Goal 归属或计划版本不匹配，请重新读取当前计划".into());
        }
        let reason = input["reason"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 1000)
            .ok_or("必须提供不超过1000字符的变更原因")?;
        let previous = json!({"plan_version":goal.plan_version,"objective":goal.objective,"acceptance_criteria":goal.acceptance_criteria,"tasks":goal.tasks,"phase":goal.phase,"reason":reason,"turn_id":user.turn_id,"user_message_id":user.message_id});
        match input["action"].as_str() {
            Some("set_plan") => {
                if input["tasks"].as_array().is_some_and(|tasks| {
                    tasks.iter().any(|task| {
                        task.as_object().is_none_or(|task| {
                            task.keys().any(|key| {
                                ![
                                    "id",
                                    "title",
                                    "expected_result",
                                    "acceptance_criteria",
                                    "dependencies",
                                ]
                                .contains(&key.as_str())
                            })
                        })
                    })
                }) {
                    return Err("待确认任务只能包含计划定义，不得提供执行状态或验收事实".into());
                }
                let tasks: Vec<GoalTask> =
                    serde_json::from_value(input["tasks"].clone()).map_err(|_| "任务格式无效")?;
                validate_tasks(&tasks)?;
                let criteria: Vec<String> =
                    serde_json::from_value(input["acceptance_criteria"].clone())
                        .map_err(|_| "整体验收条件格式无效")?;
                validate_criteria(&criteria)?;
                if let Some(objective) = input.get("objective") {
                    goal.objective = objective
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 4000)
                        .ok_or("整体目标无效")?
                        .into();
                }
                goal.tasks = tasks;
                goal.acceptance_criteria = criteria;
                goal.plan_version = goal
                    .plan_version
                    .checked_add(1)
                    .filter(|v| *v <= 9_007_199_254_740_991)
                    .ok_or("计划版本已耗尽")?;
            }
            Some("end") => {
                let excerpt = input["user_excerpt"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && user.text.contains(*s))
                    .ok_or("结束目标必须引用本轮真实用户要求")?;
                goal.phase = "ended".into();
                goal.reason = format!("用户结束目标：{excerpt}");
            }
            _ => return Err("未知 Goal 更新操作".into()),
        }
        goal.reason = reason.into();
        goal.history.push(previous);
        if render_plan(goal).encode_utf16().count() > 45_000 {
            return Err("完整计划过长，请精简描述后重新提交".into());
        }
        let result = json!(goal);
        self.commit(&next)?;
        *state = next;
        Ok(result)
    }

    fn execute_update(&self, user: &mut UserTurn, input: Value) -> Result<Value, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("当前聊天没有 Goal")?;
        if goal.phase != "active"
            || input["goal_id"] != goal.id
            || input["expected_version"].as_u64() != Some(goal.plan_version)
        {
            return Err("Goal 归属、版本或当前执行授权不匹配".into());
        }
        let reason = input["reason"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 1000)
            .ok_or("必须提供变更原因")?;
        let action = input["action"].as_str().unwrap_or("");
        if action == "accept_goal" {
            if !goal.tasks.iter().all(|task| task.status == "accepted") {
                return Err("整体验收前必须先验收所有当前任务".into());
            }
            goal.evidence = validate_evidence(
                &input["evidence"],
                &goal.acceptance_criteria,
                &goal.observations,
                None,
            )
            .map_err(|error| format!("整体验收需要任务全部通过后的独立检查：{error}"))?;
            goal.phase = "completed".into();
        } else {
            let id = input["task_id"].as_str().ok_or("必须提供任务身份")?;
            let index = goal
                .tasks
                .iter()
                .position(|task| task.id == id)
                .ok_or("任务不属于当前 Goal")?;
            match action {
                "start_task" => {
                    if !matches!(goal.tasks[index].status.as_str(), "pending" | "produced")
                        || user.task_id.as_deref().is_some_and(|current| current != id)
                    {
                        return Err("请先完成当前任务的产出与验收，不能重复启动任务".into());
                    }
                    if !goal.tasks[index].dependencies.iter().all(|dep| {
                        goal.tasks
                            .iter()
                            .any(|task| task.id == *dep && task.status == "accepted")
                    }) {
                        return Err("任务依赖尚未验收，不能启动".into());
                    }
                    if goal.tasks[index].status == "pending" {
                        goal.tasks[index].status = "running".into();
                        goal.tasks[index].assignee_id = None;
                        goal.tasks[index].child_workspace_id = None;
                        goal.tasks[index].delegation_id = None;
                    }
                }
                "record_result" => {
                    if user.task_id.as_deref() != Some(id) || goal.tasks[index].status != "running"
                    {
                        return Err("任务尚未由本回合启动".into());
                    }
                    goal.tasks[index].result = input["result"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or("产出说明不得为空，尚未验证不算通过")?
                        .into();
                    goal.tasks[index].status = "produced".into();
                }
                "accept_task" => {
                    if user.task_id.as_deref() != Some(id) || goal.tasks[index].status != "produced"
                    {
                        return Err("任务必须先记录实际产出，未验证内容不能通过".into());
                    }
                    goal.tasks[index].evidence = validate_evidence(
                        &input["evidence"],
                        &goal.tasks[index].acceptance_criteria,
                        &goal.observations,
                        Some(id),
                    )?;
                    goal.tasks[index].status = "accepted".into();
                }
                _ => {
                    return Err(
                        "执行回合只允许任务执行与验收，不能自行修改整体目标或验收条件".into(),
                    )
                }
            }
        }
        goal.reason = reason.into();
        goal.history.push(json!({"action":action,"task_id":input["task_id"],"reason":reason,"turn_id":user.turn_id,"user_message_id":user.message_id}));
        let result = json!(goal);
        self.commit(&next)?;
        *state = next;
        if action == "start_task" {
            user.task_id = input["task_id"].as_str().map(str::to_owned);
        }
        if action == "accept_task" || action == "accept_goal" {
            user.task_id = None;
        }
        drop(state);
        self.project();
        Ok(result)
    }

    fn project(&self) {
        if let (Some(handle), Some(goal)) = (
            self.handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade(),
            self.current(),
        ) {
            handle.office.project_goal(&self.owner, goal);
        }
    }

    fn observe(
        &self,
        name: &str,
        input: Value,
        outcome: &mut ToolCallOutcome,
    ) -> Result<(), String> {
        if outcome.is_error {
            return Ok(());
        }
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let Some(user) = user.as_ref().filter(|user| user.executing) else {
            return Ok(());
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| goal.phase == "active")
        else {
            return Ok(());
        };
        let id = format!("obs-{}", uuid::Uuid::new_v4().simple());
        goal.observations.push(Observation {
            id: id.clone(),
            task_id: user.task_id.clone(),
            tool: name.into(),
            input,
            output: outcome.content.clone(),
            verification: matches!(
                name,
                "read_file" | "read_attachment" | "grep" | "glob" | "bash"
            ),
            whole_goal: user.task_id.is_none()
                && goal.tasks.iter().all(|task| task.status == "accepted"),
        });
        self.commit(&next)?;
        *state = next;
        outcome
            .content
            .push_str(&format!("\nGoal observation: {id}"));
        Ok(())
    }

    /// 功能：在原 AgentLoop 的正常结束处判断是否还有目标工作。
    /// 参数：无。
    /// 返回：有当前执行许可且未完成时返回续行提示。
    /// 错误：无。
    /// 状态：只读，不创建回合或重置预算。
    /// 并发与取消：Engine 在原取消及迭代闸门内调用。
    /// 权限：磁盘 active 状态不构成许可。
    /// 关联：AgentLoop::with_end_turn_continuation。
    pub(crate) fn continuation(&self) -> Option<String> {
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if !user.as_ref().is_some_and(|user| user.executing) {
            return None;
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).records.last().filter(|goal| goal.phase == "active").map(|_| "Goal 仍有未完成工作。继续按当前依赖执行、检查产出或核对整体验收；普通回复结束不能代替目标完成，继续使用本回合剩余预算。".into())
    }

    /// 功能：判断本回合是否有真实用户授予的前台执行许可。
    /// 参数：无。
    /// 返回：当前回合的执行模式。
    /// 错误：无。
    /// 状态：只读，不依赖磁盘阶段推断权限。
    /// 并发与取消：工具实际执行仍逐次检查取消和任务。
    /// 权限：仅用于展示工具集合，不能代替 allowed。
    /// 关联：runtime::run_turn 工具声明筛选。
    pub(crate) fn is_execution_turn(&self) -> bool {
        self.user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|user| user.executing)
    }

    /// 功能：生成已持久计划或执行验收事实的规范聊天展示。
    /// 参数：无。
    /// 返回：计划全文或当前执行结果与可追溯验收结论。
    /// 错误：无。
    /// 状态：不会把生成文本当成已提交展示。
    /// 并发与取消：短锁快照。
    /// 权限：不启动执行。
    /// 关联：runtime::run_turn 终态事务。
    pub(crate) fn presentation(&self) -> Option<String> {
        if self
            .user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|user| user.executing)
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let goal = state.records.last()?;
            let accepted = goal
                .tasks
                .iter()
                .filter(|task| task.status == "accepted")
                .count();
            let mut text = format!(
                "\n\nGoal {}：已验收 {accepted}/{} 项。{}\n",
                goal.id,
                goal.tasks.len(),
                if goal.phase == "completed" {
                    "整体验收已通过"
                } else {
                    "整体尚未验收通过"
                }
            );
            for task in &goal.tasks {
                text.push_str(&format!(
                    "\n任务 {}（{}）：{}\n",
                    task.title, task.status, task.result
                ));
                for evidence in &task.evidence {
                    text.push_str(&format!(
                        "验收依据 {}：{}\n",
                        evidence.observation_id, evidence.conclusion
                    ));
                }
            }
            for evidence in &goal.evidence {
                text.push_str(&format!(
                    "整体验收依据 {}：{}\n",
                    evidence.observation_id, evidence.conclusion
                ));
            }
            return Some(text);
        }
        let editing = self
            .user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|user| user.editable);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .last()
            .filter(|goal| goal.plan_version > 0 && (goal.phase == "draft" || editing))
            .map(render_plan)
    }

    /// 功能：在聊天事务提交成功后记录最近已展示计划版本。
    /// 参数：无。
    /// 返回：持久化成功。
    /// 错误：journal 失败返回错误，不授权未展示版本。
    /// 状态：只更新 displayed_version。
    /// 并发与取消：终态提交闸门内调用。
    /// 权限：不产生执行许可。
    /// 关联：初始确认必须绑定最近展示版本。
    pub(crate) fn displayed(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        if let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| goal.displayed_version != goal.plan_version)
        {
            goal.displayed_version = goal.plan_version;
            self.commit(&next)?;
            *state = next;
        }
        Ok(())
    }

    /// 功能：校验工具在当前 Goal 和真实用户回合内是否获准。
    /// 参数：name 为工具稳定名称。
    /// 返回：获准时成功。
    /// 错误：规划中的交付工具返回确认提示。
    /// 状态：只读；结束目标不解除本回合编辑限制。
    /// 并发与取消：每次调用重新检查，不依赖模型工具列表。
    /// 权限：未知工具同样按交付工具拒绝。
    /// 关联：ToolHostValidator 与 ToolExecutor 双重检查。
    pub(crate) fn allowed(&self, name: &str) -> Result<(), String> {
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let editing = user.as_ref().is_some_and(|user| user.editable);
        let executing = user.as_ref().is_some_and(|user| user.executing);
        if executing {
            self.check_cancelled()?;
            if name == "delegate_task" {
                return self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .records
                    .last()
                    .filter(|goal| goal.phase == "active")
                    .map(|_| ())
                    .ok_or_else(|| "当前 Goal 已停止或完成，本回合不能继续委派".into());
            }
            if matches!(
                name,
                "goal_read" | "goal_update" | "read_file" | "read_attachment" | "grep" | "glob"
            ) {
                return Ok(());
            }
            if self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .records
                .last()
                .is_some_and(|goal| {
                    goal.phase == "active"
                        && goal.tasks.iter().any(|task| {
                            task.status == "running"
                                && task.delegation_id.is_none()
                                && user.as_ref().and_then(|user| user.task_id.as_deref())
                                    == Some(task.id.as_str())
                        })
                })
            {
                return Ok(());
            }
            return Err("Goal 交付工具必须绑定已满足依赖且正在执行的任务".into());
        }
        if (!editing
            && self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .records
                .last()
                .is_none_or(|goal| goal.phase == "ended" || goal.phase == "completed"))
            || matches!(
                name,
                "goal_read" | "goal_update" | "read_file" | "read_attachment" | "grep" | "glob"
            )
        {
            return Ok(());
        }
        Err("Goal 计划尚待确认，禁止执行交付工具；请先展示计划并等待确认执行".into())
    }

    /// 功能：在所有终态清除临时许可，并保存未完成 Goal 的暂停事实。
    /// 参数：无。
    /// 返回：无。
    /// 错误：暂停提交失败记安全日志，不授予后续执行权限。
    /// 状态：已有验收保留，running 转 interrupted；不启动后台执行。
    /// 并发与取消：由 runtime 收尾调用。
    /// 权限：下轮必须重新绑定真实用户输入。
    /// 关联：TurnCleanup。
    pub(crate) fn finish(&self) {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        if let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| goal.phase == "active")
        {
            goal.phase = "paused".into();
            goal.reason = "本次前台执行已结束，未完成工作等待原聊天显式继续".into();
            for task in &mut goal.tasks {
                if task.status == "running" {
                    task.status = "interrupted".into();
                }
            }
            if let Err(error) = self.commit(&next) {
                tracing::error!(%error,"Goal 暂停事实保存失败");
            } else {
                *state = next;
            }
        }
        drop(state);
        self.project();
    }
}

fn validate_evidence(
    input: &Value,
    criteria: &[String],
    observations: &[Observation],
    task: Option<&str>,
) -> Result<Vec<Evidence>, String> {
    let evidence: Vec<Evidence> =
        serde_json::from_value(input.clone()).map_err(|_| "必须提供逐项验收依据")?;
    if evidence.is_empty()
        || (0..criteria.len()).any(|index| !evidence.iter().any(|entry| entry.criterion == index))
        || evidence.iter().any(|entry| {
            entry.criterion >= criteria.len()
                || entry.conclusion.trim().is_empty()
                || !observations.iter().any(|observation| {
                    observation.id == entry.observation_id
                        && observation.task_id.as_deref() == task
                        && observation.verification
                        && (task.is_some() || observation.whole_goal)
                })
        })
    {
        return Err(
            "验收依据必须逐项引用本任务实际成功的读取或检查，不能仅声明 done 或编造编号".into(),
        );
    }
    Ok(evidence)
}

fn validate_criteria(criteria: &[String]) -> Result<(), String> {
    if criteria.is_empty() || criteria.iter().any(|s| s.trim().is_empty()) {
        return Err("验收条件不得为空".into());
    }
    Ok(())
}

fn task_summary(task: &GoalTask) -> String {
    format!(
        "预期结果：{}\n验收条件：{}\n依赖：{}",
        task.expected_result,
        task.acceptance_criteria.join("；"),
        if task.dependencies.is_empty() {
            "无".into()
        } else {
            task.dependencies.join("、")
        }
    )
}

fn validate_tasks(tasks: &[GoalTask]) -> Result<(), String> {
    if tasks.is_empty() || tasks.len() > 1000 {
        return Err("计划必须包含1至1000项独立交付任务".into());
    }
    let ids: HashSet<_> = tasks.iter().map(|task| task.id.as_str()).collect();
    if ids.len() != tasks.len() {
        return Err("任务编号不能重复".into());
    }
    for task in tasks {
        validate_criteria(&task.acceptance_criteria)?;
        if task.id.is_empty()
            || task.id.len() > 50
            || !task
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || task.title.trim().is_empty()
            || task.title.encode_utf16().count() > 160
            || task.expected_result.trim().is_empty()
        {
            return Err("任务编号、标题或完整交付描述无效／过长".into());
        }
        let deps: HashSet<_> = task.dependencies.iter().collect();
        if deps.len() != task.dependencies.len()
            || task
                .dependencies
                .iter()
                .any(|id| id == &task.id || !ids.contains(id.as_str()))
        {
            return Err("任务依赖必须指向当前计划的其他任务，且不能重复".into());
        }
    }
    let mut resolved = HashSet::new();
    loop {
        let before = resolved.len();
        for task in tasks {
            if task.dependencies.iter().all(|id| resolved.contains(id)) {
                resolved.insert(task.id.clone());
            }
        }
        if resolved.len() == tasks.len() {
            return Ok(());
        }
        if resolved.len() == before {
            return Err("任务依赖不能成环".into());
        }
    }
}

fn render_plan(goal: &GoalRecord) -> String {
    let mut text = format!(
        "\n\n目标：{}\n整体验收条件：\n- {}\n完整计划（版本 {}）：\n",
        goal.objective,
        goal.acceptance_criteria.join("\n- "),
        goal.plan_version
    );
    for task in &goal.tasks {
        text.push_str(&format!(
            "\n任务 {}：{}\n{}\n",
            task.id,
            task.title,
            task_summary(task)
        ));
    }
    text.push_str(&format!(
        "\n变更原因：{}\n{}",
        goal.reason,
        if goal.phase == "ended" {
            "此 Goal 已明确结束，历史保留；未标记为验收完成。"
        } else {
            "计划待确认，尚未开始交付。可以直接用自然语言修改，核对后回复“确认执行”。"
        }
    ));
    text
}

/// 功能：在桌面及工作区工具最外层强制实施 Goal 规划权限。
pub(crate) struct GoalToolHost {
    inner: Arc<dyn ToolHost>,
    context: Arc<GoalContext>,
}

impl GoalToolHost {
    /// 功能：包装现有工具集合。
    /// 参数：inner 为原工具，context 为会话私有 Goal 上下文。
    /// 返回：保留原工具能力的外层包装。
    /// 错误：无。
    /// 状态：不更改持久状态。
    /// 并发与取消：复用原工具执行与取消。
    /// 权限：规划限制在 validate_input 和 call 双重检查。
    /// 关联：runtime::session_tool_host。
    pub(crate) fn new(inner: Arc<dyn ToolHost>, context: Arc<GoalContext>) -> Self {
        Self { inner, context }
    }
}

#[async_trait]
impl ToolExecutor for GoalToolHost {
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.set_media_context(media);
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.remember_media(media);
    }
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        self.inner.list_tools().await
    }
    async fn call(&self, name: &str, args: Value) -> openpaw_core::Result<ToolCallOutcome> {
        if let Err(error) = self.context.allowed(name) {
            return Ok(ToolCallOutcome::error(error));
        }
        let value = match name {
            "goal_read" => Ok(self.context.read()),
            "goal_update" => self.context.update(args),
            _ => {
                let mut outcome = self.inner.call(name, args.clone()).await?;
                if let Err(error) = self.context.observe(name, args, &mut outcome) {
                    return Err(openpaw_core::Error::Session(error));
                }
                return Ok(outcome);
            }
        };
        Ok(match value {
            Ok(value) => ToolCallOutcome {
                content: value.to_string(),
                is_error: false,
                structured: Some(value),
                media: Vec::new(),
            },
            Err(error) => ToolCallOutcome::error(error),
        })
    }
}

#[async_trait]
impl ToolHostValidator for GoalToolHost {
    async fn validate_input(&self, name: &str, input: &Value) -> Result<(), String> {
        self.context.allowed(name)?;
        if matches!(name, "goal_read" | "goal_update") {
            Ok(())
        } else {
            self.inner.validate_input(name, input).await
        }
    }
    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        self.inner.should_cascade_on_error(name, outcome)
    }
}

/// 功能：声明仅供当前 Goal 回合使用的规划、执行和验收工具。
/// 参数：无。
/// 返回：读取与完整计划更新工具声明。
/// 错误：无。
/// 状态：不修改目标或会话。
/// 并发与取消：工具更新使用原有串行分类。
/// 权限：普通会话的工具列表不增加声明。
/// 关联：runtime::run_turn。
pub(crate) fn plan_tools() -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    for name in ["goal_read", "goal_update"] {
        let mut spec = ToolSpec::new(
            name,
            if name == "goal_read" {
                "读取当前聊天 Goal 及完整计划版本"
            } else {
                "规划用 set_plan/end；执行用 start_task、record_result、accept_task；全任务通过后独立检查 accept_goal。更新携带 goal_id、expected_version、reason；验收 evidence 逐条件引用真实工具观察编号"
            },
        );
        spec.input_schema = if name == "goal_read" {
            json!({"type":"object","properties":{},"additionalProperties":false})
        } else {
            json!({"type":"object","required":["action","goal_id","expected_version","reason"],"properties":{"action":{"enum":["set_plan","end","start_task","record_result","accept_task","accept_goal"]},"goal_id":{"type":"string"},"expected_version":{"type":"integer"},"reason":{"type":"string"},"objective":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"tasks":{"type":"array","items":{"type":"object","required":["id","title","expected_result","acceptance_criteria","dependencies"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"expected_result":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"dependencies":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}},"user_excerpt":{"type":"string"},"task_id":{"type":"string"},"result":{"type":"string"},"evidence":{"type":"array","items":{"type":"object","required":["criterion","observation_id","conclusion"],"properties":{"criterion":{"type":"integer","minimum":0},"observation_id":{"type":"string"},"conclusion":{"type":"string"}},"additionalProperties":false}}},"additionalProperties":false})
        };
        spec.category = Some(ToolCategory::ReadOnly);
        spec.concurrency = Some(ToolConcurrency::NeverParallel);
        spec.max_result_chars = Some(0);
        tools.push(spec);
    }

    tools
}
