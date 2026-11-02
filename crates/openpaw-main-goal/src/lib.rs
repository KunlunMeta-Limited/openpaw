//! 功能：保存会话归属的 Goal、计划、执行和验收事实，强制回合与任务权限。
//! 输入输出边界：真实用户回合和模型工具输入进入，持久事实由 runtime 唯一 writer 提交。
//! 禁止承担：不直接执行交付工具，不写共享 Todo，不创建后台驱动或独立 writer。
//! 关联：CHG-2026-0050、goal-command tickets 01–08；runtime 保留前台驱动及原预算。

use async_trait::async_trait;
use openpaw_core::{
    ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor, ToolHost, ToolHostValidator,
    ToolInvocationContext, ToolSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// 功能：向 Goal module 提供当前会话已经拥有的同步运行效果。
/// 数据边界：不携带 Session、writer、Engine 或 Office 的具体类型。
/// 约束：所有方法不得等待 session 异步锁，不得重新进入 Goal 的持锁更新。
pub trait GoalRuntime: Send + Sync {
    /// 功能：经 runtime 唯一 writer 提交完整 Goal 扩展事件。
    /// 参数：event 为已校验且归属当前会话的 session_goal_state_v1。
    /// 返回：原 journal 提交成功。
    /// 错误：不可用或提交失败返回错误，Goal 不得发布内存更新。
    /// 状态：只追加事件，不启动执行。
    /// 并发与取消：同步提交；锁顺序为 Goal user、state、writer。
    /// 权限：不得写其他会话或直接操作文件。
    /// 关联：GoalContext 的全部持久转换。
    fn commit(&self, event: Value) -> Result<(), String>;

    /// 功能：检查 runtime 当前原回合的取消状态。
    /// 参数：无。
    /// 返回：取消或会话不可用时为真。
    /// 错误：无。
    /// 状态：只读，不替换取消令牌。
    /// 并发与取消：每次读取原令牌，不等待异步锁。
    /// 权限：不能以持久状态恢复执行许可。
    /// 关联：交付动作和真实子执行准入。
    fn is_cancelled(&self) -> bool;

    /// 功能：读取当前会话已有的持久工具执行事实。
    /// 参数：无。
    /// 返回：可用时返回事实快照，不可用时为无。
    /// 错误：无。
    /// 状态：不执行工具或调用模型。
    /// 并发与取消：仅短暂读取事实锁，不等待 session。
    /// 权限：仅当前会话，不包含其他聊天的记录。
    /// 关联：中断任务实际结果复核。
    fn execution_facts(&self) -> Option<Value>;

    /// 功能：关闭原前台回合的子执行准入并请求取消。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无；会话已释放时无动作。
    /// 状态：runtime 仍负责子执行登记与最终 join。
    /// 并发与取消：只在释放 Goal user/state 锁后调用。
    /// 权限：不能创建、接管或后台重启子执行。
    /// 关联：Goal 暂停和原回合收尾。
    fn cancel_children(&self);

    /// 功能：发布 Goal 事实到既有 Office 投影。
    /// 参数：goal 为当前会话已经提交的完整只读快照。
    /// 返回：无。
    /// 错误：投影失败由 Host 隔离，不回写 Goal 或 AgentLoop 结果。
    /// 状态：只作展示，不授予执行或验收权限。
    /// 并发与取消：仅在释放 Goal user/state 锁后调用。
    /// 权限：不得发送凭据或新增消息字段。
    /// 关联：既有 Office 任务列表和白板。
    fn project(&self, goal: Value);
}

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
    #[serde(default)]
    review: Option<PlanReview>,
}

/// 功能：绑定后端固定审核与真实计划更新，不携带可恢复的执行许可。
/// 数据边界：由Goal保存，Host只读取并向原问答通道展示。
/// 约束：身份、计划全文与问题一旦入队不可由模型改写；Stability: experimental。
#[derive(Clone, Serialize, Deserialize)]
pub struct PlanReview {
    /// 功能：当前Goal身份。
    pub goal_id: String,
    /// 功能：需要明确批准的完整计划版本。
    pub plan_version: u64,
    /// 功能：原活动回合身份。
    pub turn_id: String,
    /// 功能：原真实用户消息身份。
    pub source_user_message_id: String,
    /// 功能：提交计划的模型响应序号。
    pub response_index: usize,
    /// 功能：真实goal_update调用身份，不是模型可指定的新调用。
    pub tool_call_id: String,
    /// 功能：引擎应先可靠展示的完整规范计划。
    pub plan: String,
    /// 功能：由后端确定的固定问题及选项。
    pub request: Value,
    /// 功能：审核记录状态；持久状态不授予执行许可。
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalTask {
    id: String,
    title: String,
    expected_result: String,
    acceptance_criteria: Vec<String>,
    dependencies: Vec<String>,
    #[serde(default)]
    replaces: Vec<String>,
    #[serde(default = "current_task")]
    current: bool,
    #[serde(default = "pending")]
    status: String,
    #[serde(default)]
    block_reason: String,
    #[serde(default)]
    result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manual: Option<ManualConstraint>,
    #[serde(default)]
    evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delegation_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ManualConstraint {
    previous_status: String,
    previous_block_reason: String,
    sources: Vec<ManualSource>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ManualSource {
    task_id: String,
    turn_id: String,
    user_message_id: String,
    user_excerpt: String,
}

fn pending() -> String {
    "pending".into()
}

fn dependencies_satisfied(task: &GoalTask, tasks: &[GoalTask]) -> bool {
    task.dependencies.iter().all(|dependency| {
        tasks
            .iter()
            .any(|task| task.current && task.id == *dependency && task.status == "accepted")
    })
}

fn pause_execution(goal: &mut GoalRecord, reason: &str) {
    if goal.phase == "active" {
        goal.phase = "paused".into();
        goal.reason = reason.into();
    }
    for task in &mut goal.tasks {
        if task.current && matches!(task.status.as_str(), "running" | "produced") {
            task.status = "interrupted".into();
        }
    }
}

fn recheck_block(goal: &mut GoalRecord, input: &Value, reason: &str) -> Result<(), String> {
    let task = goal
        .tasks
        .iter_mut()
        .find(|task| task.current && input["task_id"] == task.id)
        .ok_or("任务不属于当前 Goal")?;
    if task.status != "blocked" {
        return Err("只有执行阻塞任务可以重检；人工阻塞须由用户明确恢复".into());
    }
    let status = input["status"]
        .as_str()
        .filter(|s| matches!(*s, "pending" | "blocked"))
        .ok_or("执行阻塞重检后只能等待条件或恢复 pending")?;
    task.status = status.into();
    task.block_reason = if status == "blocked" {
        reason.into()
    } else {
        String::new()
    };
    if status == "pending" {
        task.assignee_id = None;
        task.child_workspace_id = None;
        task.delegation_id = None;
    }
    Ok(())
}

fn refresh_waiting(goal: &mut GoalRecord, executing: bool) {
    let tasks: Vec<_> = goal.tasks.iter().filter(|task| task.current).collect();
    let available = tasks.iter().all(|task| task.status == "accepted")
        || tasks.iter().any(|task| {
            matches!(task.status.as_str(), "running" | "produced" | "interrupted")
                || (task.status == "pending" && dependencies_satisfied(task, &goal.tasks))
        });
    if available {
        goal.phase = if executing { "active" } else { "paused" }.into();
    } else {
        goal.phase = "blocked".into();
        goal.reason = format!(
            "等待条件：{}",
            tasks
                .iter()
                .filter(|task| !task.block_reason.is_empty())
                .map(|task| format!("{}：{}", task.title, task.block_reason))
                .collect::<Vec<_>>()
                .join("；")
        );
        let mut used = 0;
        goal.reason = goal
            .reason
            .chars()
            .take_while(|character| {
                used += character.len_utf16();
                used <= 1000
            })
            .collect();
    }
}

fn current_task() -> bool {
    true
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
    #[serde(default)]
    plan_version: u64,
    #[serde(default)]
    turn_id: String,
    #[serde(default)]
    is_error: bool,
}

#[derive(Clone)]
struct UserTurn {
    turn_id: String,
    message_id: String,
    text: String,
    editable: bool,
    executing: bool,
    task_id: Option<String>,
    main_start: Option<Value>,
}

/// 功能：承载单个会话的 Goal 事实与当前真实用户回合，不持有 session 强引用。
/// 数据边界：只由 runtime 组装并恢复；模型不可提供执行许可或用户身份。
/// 约束：调用方负责同会话单活动回合；只安装同步效果，不取得 writer、Loop 或预算。
/// 关联：CHG-2026-0050、GoalRuntime。
pub struct GoalContext {
    owner: String,
    state: Mutex<GoalState>,
    user: Mutex<Option<UserTurn>>,
    runtime: Mutex<Option<Arc<dyn GoalRuntime>>>,
}

/// 功能：把一次真实子执行的终态限定到原 Goal、任务和委派尝试。
/// 数据边界：仅 runtime 持有；模型和持久状态不能恢复此执行许可。
/// 约束：丢弃未收尾的 guard 保留中断事实，真实子执行的取消及 join 仍由 runtime 完成。
/// 关联：GoalContext::reserve_delegation。
pub struct GoalDelegation {
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
    pub fn bind(&self, child: &str, workspace: &str) -> Result<(), String> {
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
                    && task.current
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
    pub fn finish(&mut self, status: &str, result: &str) -> Result<(), String> {
        let mut state = self.context.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        if let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| goal.id == self.goal_id)
        {
            if let Some(task) = goal.tasks.iter_mut().find(|task| {
                task.id == self.task_id
                    && task.current
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
    /// 功能：从已保存Goal事件定位完整观察及其来源回合。
    /// 参数：events为会话记录，owner/goal/id为可信归属和观察定位键。
    /// 返回：来源回合、原始正文及错误状态。
    /// 错误：归属、观察或正文缺失时明确失败。
    /// 状态：只读，不恢复执行许可。
    /// 并发与取消：同步快照读取。
    /// 权限：调用方仍须验证来源回合当前可见。
    /// 关联：read_context_record。
    pub fn original_observation(
        events: &[openpaw_core::SessionEvent],
        owner: &str,
        goal: &str,
        id: &str,
    ) -> Result<(String, String, bool), String> {
        let value = events
            .iter()
            .rev()
            .find_map(|e| match e {
                openpaw_core::SessionEvent::Unknown(v)
                    if v["type"] == "session_goal_state_v1" && v["owner"] == owner =>
                {
                    Some(v)
                }
                _ => None,
            })
            .ok_or("Goal原件不存在")?;
        let observation = value["state"]["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|g| g["id"] == goal)
            .flat_map(|g| g["observations"].as_array().into_iter().flatten())
            .find(|o| o["id"] == id)
            .ok_or("Goal原件不存在")?;
        Ok((
            observation["turn_id"]
                .as_str()
                .ok_or("Goal原件来源缺失")?
                .into(),
            observation["output"]
                .as_str()
                .ok_or("Goal原件正文缺失")?
                .into(),
            observation["is_error"].as_bool().unwrap_or(false),
        ))
    }
    /// 功能：创建无执行授权的会话 Goal 上下文。
    /// 参数：owner 为会话稳定身份。
    /// 返回：空上下文的共享引用。
    /// 错误：无。
    /// 状态：不创建目标或写入文件。
    /// 并发与取消：独立短锁；没有后台工作。
    /// 权限：只归属 owner。
    /// 关联：runtime::session_tool_host。
    pub fn new(owner: &str) -> Arc<Self> {
        Arc::new(Self {
            owner: owner.into(),
            state: Mutex::new(GoalState::default()),
            user: Mutex::new(None),
            runtime: Mutex::new(None),
        })
    }

    /// 功能：关联 runtime 唯一写入入口。
    /// 参数：runtime 为已组装会话提供的同步效果 adapter。
    /// 返回：无。
    /// 错误：无。
    /// 状态：只安装 adapter；生产 adapter 内部保存会话弱引用。
    /// 并发与取消：组装时调用，无 await。
    /// 权限：不授予执行许可。
    /// 关联：runtime::build_session_handle。
    pub fn attach(&self, runtime: Arc<dyn GoalRuntime>) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(runtime);
    }

    fn runtime(&self) -> Option<Arc<dyn GoalRuntime>> {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.runtime().is_none_or(|runtime| runtime.is_cancelled()) {
            return Err("当前 Goal 回合已取消，不再启动工作".into());
        }
        Ok(())
    }

    /// 功能：在真实子创建前原子预留一个满足依赖的 Goal 任务。
    /// 参数：task为Goal任务，attempt为唯一尝试，invocation为真实委派调用身份。
    /// 返回：Goal 执行返回独占绑定；普通聊天不绑定。
    /// 错误：普通聊天误带Goal参数时提示移除；缺少许可、未验收依赖、重复绑定或持久化失败时不启动子执行。
    /// 状态：先持久标记 running，未准入或 future 丢弃转 interrupted。
    /// 并发与取消：user→state 锁内核对许可、状态与取消，重复委派只有一项能成功。
    /// 权限：持久 active 或其他子任务 running 不构成执行许可。
    /// 关联：runtime::run_delegated、GoalDelegation。Stability: experimental。
    pub fn reserve_delegation(
        self: &Arc<Self>,
        task: Option<&str>,
        attempt: &str,
        invocation: Option<&ToolInvocationContext>,
    ) -> Result<Option<GoalDelegation>, String> {
        let mut user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !user
            .as_ref()
            .is_some_and(|user| user.executing || user.editable)
            && state
                .records
                .last()
                .is_none_or(|goal| matches!(goal.phase.as_str(), "ended" | "completed"))
        {
            return if task.is_some() {
                Err("普通聊天委派请移除 goal_task_id；仅已确认的 Goal 执行回合使用计划中的真实任务编号，不要编造。".into())
            } else {
                Ok(None)
            };
        }
        let user = user
            .as_mut()
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
            .position(|task| task.current && task.id == id)
            .ok_or("委派任务不属于当前 Goal 计划")?;
        if question_paused_tasks(goal, &user.turn_id).contains(id) {
            return Err("该分支问答已暂停，等待原聊天用户明确继续，不能同回合重新委派".into());
        }
        let transferring = goal.tasks[index].status == "running"
            && user.task_id.as_deref() == Some(id)
            && goal.tasks[index].assignee_id.is_none()
            && goal.tasks[index].delegation_id.is_none()
            && goal.tasks[index].result.is_empty()
            && goal.tasks[index].evidence.is_empty()
            && invocation.is_some_and(|call| {
                let facts = self.runtime().and_then(|runtime| runtime.execution_facts());
                let Some(facts) = facts.as_ref().and_then(Value::as_array) else {
                    return false;
                };
                let Some(start) = user.main_start.as_ref().and_then(Value::as_array) else {
                    return false;
                };
                call.turn_id.as_deref() == Some(user.turn_id.as_str())
                    && facts.len() == start.len() + 1
                    && facts[..start.len()] == start[..]
                    && facts.last().is_some_and(|fact| {
                        fact["kind"] == "tool_start"
                            && fact["turn_id"] == user.turn_id
                            && fact["response_index"].as_u64() == Some(call.response_index as u64)
                            && fact["payload"]["call_id"] == call.tool_call_id
                            && matches!(
                                fact["payload"]["name"].as_str(),
                                Some("start_agent" | "delegate_task")
                            )
                    })
            });
        if goal.tasks[index].status != "pending" && !transferring {
            return Err("Goal任务已启动或已有产出，不能重复委派；委派应从pending直接start_agent，不要先start_task。仅本回合尚未开展任何工作的主任务可安全转交。".into());
        }
        if !dependencies_satisfied(&goal.tasks[index], &goal.tasks) {
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
        if transferring {
            user.task_id = None;
            user.main_start = None;
        }
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
    /// 状态：保留事实，把失去执行回合的活动状态恢复为暂停及中断，不恢复许可。
    /// 并发与取消：仅冷加载调用。
    /// 权限：拒绝其他会话的 Goal 事件。
    /// 关联：session_goal_state_v1。
    pub fn restore(&self, events: &[openpaw_core::SessionEvent]) -> Result<(), String> {
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
        if let Some(goal) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .last_mut()
            .filter(|goal| !matches!(goal.phase.as_str(), "draft" | "ended" | "completed"))
        {
            pause_execution(goal, "服务重启中断，未完成工作等待原聊天显式继续");
        }
        Ok(())
    }

    fn commit(&self, state: &GoalState) -> Result<(), String> {
        let runtime = self.runtime().ok_or("Goal 会话不可用")?;
        runtime.commit(json!({"type":"session_goal_state_v1","owner":self.owner,"state":state}))
    }

    /// 功能：绑定真实用户回合，处理创建和最近展示版本的初始执行确认。
    /// 参数：turn 与 message 为 runtime 身份，text 为未替换的原输入。
    /// 返回：确定性拒绝回复，或交给模型规划。
    /// 错误：持久化失败停止处理。
    /// 状态：新草稿和确认事实先落盘；重复目标不会覆盖当前记录。
    /// 并发与取消：每个会话已有单活动回合，Goal 锁串行化提交。
    /// 权限：草稿只有编辑权限；初始确认只授予本回合执行许可。
    /// 关联：runtime::run_turn。
    pub fn begin(&self, turn: &str, message: &str, text: &str) -> Result<Option<String>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let command = text.trim();
        let creating = command
            .strip_prefix("/goal ")
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "continue");
        if state
            .records
            .last()
            .and_then(|goal| goal.review.as_ref())
            .is_some_and(|review| {
                matches!(review.status.as_str(), "queued" | "waiting") && review.turn_id != turn
            })
        {
            let mut next = state.clone();
            next.records
                .last_mut()
                .unwrap()
                .review
                .as_mut()
                .unwrap()
                .status = "interrupted".into();
            self.commit(&next)?;
            *state = next;
        }
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
                    review: None,
                });
                self.commit(&next)?;
                *state = next;
                None
            }
        } else if command == "确认执行"
            && current.is_some_and(|goal| {
                matches!(goal.phase.as_str(), "draft" | "paused" | "blocked")
                    && goal.plan_version > 0
                    && goal.confirmed_version < goal.plan_version
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
        } else if command == "/goal continue" {
            if current.is_some_and(|goal| {
                matches!(goal.phase.as_str(), "paused" | "blocked")
                    && goal.confirmed_version > 0
                    && goal.confirmed_version == goal.plan_version
                    && goal.displayed_version == goal.plan_version
            }) {
                let mut next = state.clone();
                let goal = next.records.last_mut().unwrap();
                goal.phase = "active".into();
                goal.reason = "用户在原聊天显式继续；中断产物必须先复核，已有验收保留".into();
                self.commit(&next)?;
                *state = next;
                None
            } else if current.is_some_and(|goal| {
                matches!(goal.phase.as_str(), "draft" | "paused" | "blocked")
                    && goal.plan_version > goal.confirmed_version
            }) {
                // 明确继续未确认计划只开放重新规划；新版本仍必须经过固定审核。
                None
            } else {
                Some(
                    if !unfinished {
                        "当前聊天没有可继续的 Goal，不能接管其他聊天的目标。"
                    } else {
                        "当前计划尚未确认或最新版本尚未展示，请先核对完整计划。"
                    }
                    .into(),
                )
            }
        } else if unfinished && command == "确认执行" {
            Some("当前计划尚未开始执行。请核对最近展示的完整计划。".into())
        } else {
            None
        };
        let editable = reply.is_none()
            && state
                .records
                .last()
                .is_some_and(|goal| matches!(goal.phase.as_str(), "draft" | "paused" | "blocked"));
        *self.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(UserTurn {
            turn_id: turn.into(),
            message_id: message.into(),
            text: text.into(),
            editable,
            executing: reply.is_none()
                && matches!(command, "确认执行" | "/goal continue")
                && state
                    .records
                    .last()
                    .is_some_and(|goal| goal.phase == "active"),
            task_id: None,
            main_start: None,
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
    pub fn prompt(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let goal = state.records.last()?;
        if matches!(goal.phase.as_str(), "paused" | "blocked") {
            return Some(format!("Goal 已停止。只允许读取、复核和根据本轮真实用户要求编辑计划或结束目标；人工暂停用manual_pause、明确恢复用manual_resume，携带task_id和user_excerpt引用本轮原文；恢复旧来源任务可解除其替换或拆分任务约束；不能写文件、shell、桌面动作或委派。修改目标或验收条件须引用 user_excerpt，保留仍有效已验收事实。补充条件时可用 recheck_task(task_id,status=pending或blocked,reason) 重检执行阻塞，条件不足仍保持 blocked；这由模型判断，无阻塞证据门槛。人工阻塞不可由普通重检解除。编辑完成后仍停止，只有原聊天 /goal continue 授予执行许可。当前事实：{}",self.model_view(json!(goal))));
        }
        if goal.phase != "draft" {
            return (!matches!(goal.phase.as_str(), "ended" | "completed")).then(|| format!("当前 Goal {}。先 goal_read（含持久执行事实）。仅在本次确认或显式继续授权回合按依赖推进：主Agent自己执行才用goal_update(action=start_task,task_id)；委派必须从pending直接start_agent(goal_task_id)，不要先start_task；中断或上一回合 produced 任务禁止原样重复。先读取实际产物及 execution_attempts，再用 recheck_task(task_id,observation_id,status=pending或produced,result) 引用本轮真实成功读取；确需补做才回 pending，完整产物直接 produced 并验收。保留仍有效 accepted 结果。record_result(result) 只表示产出未验证。独立任务优先 start_agent(goal_task_id=计划任务编号)，父继续独立工作，需要子结果时 wait_agent(session_id,turn_id)，task_id 仍表示真实子会话；本回合子结果 produced 可用 start_task 选中只读验收，不能重复交付。wait_agent/get_agent_status 的交付或完成状态不作为验收依据；用 goal_read 找到可靠观察 reference，再 read_context_record 读取实际子结果，使用这次读取返回的 Goal observation 编号。预览需沿其原件引用继续读，长原件按页核对，不能把状态或摘要当作全文。上轮中断先读取实际原件再 recheck_task，不重复委派。读取实际交付物或运行检查后，accept_task 携带 evidence 数组，每个验收条件提供 criterion(从0起)、真实 observation_id 和 conclusion。工具结果中的 Goal observation 编号可追溯，不能编造或仅凭完成声明。所有任务 accepted 后另做整体检查，再 accept_goal，不能把任务100%当作整体完成。目标内可用 replan 提交完整当前任务定义（含 replaces 来源），增补、拆分、改写、替换和调整依赖；保留失败事实、有效验收和历史身份。不得提交 status/result/evidence/current 等运行事实。执行期间不得修改整体目标或验收条件；越界暂停等待用户编辑及显式继续。遇到普通失败先修复或重规划。人工manual_blocked及其用户来源不可自动解除，continue、重检、移除依赖或新建等价任务均不能绕过；重规划保留原身份、replaces和人工来源，独立工作仍可推进。确实无法推进可 block_task(task_id,reason)，说明原因和所需条件，无工具尝试次数或阻塞证据门槛；受阻依赖等待，独立工作继续。continue 后先重检 blocked 任务，用 recheck_task(task_id,status=pending或blocked,reason) 提交模型判断，仍缺条件保持 blocked，满足才回 pending；人工阻塞不能由普通重检解除。已有原预算和取消约束保持。当前事实：{}",goal.id,self.model_view(json!(goal))));
        }
        Some(format!("当前处于 Goal 待确认规划阶段。整体目标：{}。先 goal_read，再用 goal_update(action=set_plan) 提交完整计划。每项任务必须有稳定 id、title、expected_result、acceptance_criteria、dependencies；按独立交付结果拆解，整体验收条件必须明确。用户自然语言修改时重交完整新版本，保留身份和必要历史。禁止执行交付工作、写文件、shell、桌面动作、委派或共享 Todo。明确结束旧目标才用 action=end，并提供原用户输入中的 user_excerpt；不得把结束冒充验收完成。最终完整计划由运行时展示，等待用户确认执行。",goal.objective))
    }

    fn queue_review(
        &self,
        user: &mut UserTurn,
        invocation: &ToolInvocationContext,
    ) -> Result<(), String> {
        // 无调用上下文的旧嵌入入口不伪造工具身份；Host一律使用call_with_context。
        if invocation.turn_id.is_none() && invocation.tool_call_id.is_empty() {
            return Ok(());
        }
        // 新版本先关闭旧许可；后续绑定或持久化失败也不能沿旧许可交付。
        user.executing = false;
        user.editable = false;
        if invocation.turn_id.as_deref() != Some(user.turn_id.as_str())
            || invocation.tool_call_id.is_empty()
        {
            return Err("计划审核调用不属于当前回合".into());
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("当前没有Goal")?;
        goal.review = Some(PlanReview {
            goal_id: goal.id.clone(),
            plan_version: goal.plan_version,
            turn_id: user.turn_id.clone(),
            source_user_message_id: user.message_id.clone(),
            response_index: invocation.response_index,
            tool_call_id: invocation.tool_call_id.clone(),
            plan: render_plan(goal),
            status: "queued".into(),
            request: json!({"question":format!("是否执行 Goal 计划 v{}？",goal.plan_version),"description":"完整计划已显示在聊天中。执行只批准这一版本；修改后会再次确认；取消保留历史并结束目标。也可通过其他需求填写修改要求。","options":[{"id":"execute","label":"执行","description":""},{"id":"modify","label":"修改","description":""},{"id":"cancel","label":"取消","description":""}]}),
        });
        self.commit(&next)?;
        *state = next;
        Ok(())
    }

    /// 功能：读取本回合尚待安排的唯一计划审核。
    /// 参数：无。
    /// 返回：当前真实回合的入队审核副本，无待审核时为空。
    /// 错误：无。
    /// 状态：只读，不标记展示或授予许可。
    /// 并发与取消：短锁，已取消回合不返回审核。
    /// 权限：不接受模型提供的问题或版本。
    /// 关联：Host工具批次协调；Stability: experimental。
    pub fn pending_review(&self) -> Option<PlanReview> {
        if self.check_cancelled().is_err() {
            return None;
        }
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let review = state.records.last()?.review.as_ref()?;
        (review.status == "queued"
            && user
                .as_ref()
                .is_some_and(|user| user.turn_id == review.turn_id))
        .then(|| review.clone())
    }

    /// 功能：在引擎可靠发布完整计划后登记该版本已展示。
    /// 参数：review为本回合原始审核绑定。
    /// 返回：审核进入等待状态。
    /// 错误：版本、调用或回合改变，取消及保存失败时拒绝。
    /// 状态：只提交展示事实，不授权。
    /// 并发与取消：不跨异步锁，等待前核对原取消信号。
    /// 权限：仅由可信协调器在发布成功后调用。
    /// 关联：TurnInteraction；Stability: experimental。
    pub fn review_displayed(&self, review: &PlanReview) -> Result<(), String> {
        self.check_cancelled()?;
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if user
            .as_ref()
            .is_none_or(|user| user.turn_id != review.turn_id)
        {
            return Err("审核回合已结束".into());
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("审核目标已失效")?;
        if goal.id != review.goal_id
            || goal.plan_version != review.plan_version
            || goal.review.as_ref().is_none_or(|saved| {
                saved.tool_call_id != review.tool_call_id || saved.status != "queued"
            })
        {
            return Err("审核计划或展示已改变".into());
        }
        goal.displayed_version = review.plan_version;
        goal.review.as_mut().unwrap().status = "waiting".into();
        self.commit(&next)?;
        *state = next;
        Ok(())
    }

    /// 功能：按已可靠保存的真实问题结果结算固定计划审核。
    /// 参数：review为原绑定，record为唯一writer确认后的问答事实，缺失表示中断。
    /// 返回：execute、custom、modify、cancel或paused，供原回合继续或暂停。
    /// 错误：旧版本、跨回合、伪造问题、取消或保存失败均不授权。
    /// 状态：只执行execute授予当前版本许可；custom只开放规划。
    /// 并发与取消：短锁提交，取消子任务在释放Goal锁后进行。
    /// 权限：普通提问及自定义执行文字均不能授予许可。
    /// 关联：Host审核协调器；Stability: experimental。
    pub fn settle_review(
        &self,
        review: &PlanReview,
        record: Option<&openpaw_core::user_input::UserQuestionRecord>,
    ) -> Result<String, String> {
        self.check_cancelled()?;
        let mut user_guard = self.user.lock().unwrap_or_else(|e| e.into_inner());
        let mut next_user = user_guard
            .as_ref()
            .filter(|user| {
                user.turn_id == review.turn_id && user.message_id == review.source_user_message_id
            })
            .cloned()
            .ok_or("审核回合归属已失效")?;
        let user = &mut next_user;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("审核目标已失效")?;
        if goal.id != review.goal_id
            || goal.plan_version != review.plan_version
            || goal.displayed_version != review.plan_version
            || goal.review.as_ref().is_none_or(|saved| {
                saved.status != "waiting" || saved.tool_call_id != review.tool_call_id
            })
        {
            return Err("审核版本已过期或已经处理".into());
        }
        let mut answer = None;
        if let Some(record) = record {
            if record.scope.asking_session_id != self.owner
                || record.scope.parent_session_id != self.owner
                || record.scope.source_user_message_id != review.source_user_message_id
                || record.turn_id != review.turn_id
                || record.response_index != review.response_index
                || record.tool_call_id != review.tool_call_id
                || record.question_id
                    != format!(
                        "{}/{}/{}/{}",
                        self.owner, review.turn_id, review.response_index, review.tool_call_id
                    )
                || serde_json::to_value(&record.request).ok() != Some(review.request.clone())
                || !openpaw_core::user_input::UserQuestionState::Answered.eq(&record.state)
            {
                return Err("计划审核回答归属无效".into());
            }
            answer = record.answer.as_ref();
        }
        let action = answer.map_or("paused", |answer| answer.choice_id.as_str());
        match action {
            "execute"
                if answer.is_some_and(|answer| {
                    answer.label == "执行" && answer.custom_text.is_none()
                }) =>
            {
                goal.confirmed_version = goal.plan_version;
                goal.phase = "active".into();
                user.executing = true;
            }
            "custom"
                if answer
                    .and_then(|answer| answer.custom_text.as_deref())
                    .is_some_and(|text| !text.trim().is_empty()) =>
            {
                user.text = answer.unwrap().custom_text.clone().unwrap();
                user.editable = true;
                goal.phase = if goal.confirmed_version > 0 {
                    "paused"
                } else {
                    "draft"
                }
                .into();
            }
            "modify"
                if answer.is_some_and(|answer| {
                    answer.label == "修改" && answer.custom_text.is_none()
                }) =>
            {
                goal.phase = if goal.confirmed_version > 0 {
                    "paused"
                } else {
                    "draft"
                }
                .into();
            }
            "paused" if answer.is_none() => {
                goal.phase = if goal.confirmed_version > 0 {
                    "paused"
                } else {
                    "draft"
                }
                .into();
            }
            "cancel"
                if answer.is_some_and(|answer| {
                    answer.label == "取消" && answer.custom_text.is_none()
                }) =>
            {
                goal.phase = "ended".into();
            }
            _ => return Err("审核答案无效，未批准执行".into()),
        }
        goal.review.as_mut().unwrap().status = action.into();
        goal.reason = format!("用户计划审核：{action}，版本{}", review.plan_version);
        goal.history.push(json!({"action":"plan_review","plan_version":review.plan_version,"turn_id":review.turn_id,"tool_call_id":review.tool_call_id,"decision":action}));
        self.commit(&next)?;
        *state = next;
        *user_guard = Some(next_user);
        drop(state);
        drop(user_guard);
        if action == "cancel" {
            if let Some(runtime) = self.runtime() {
                runtime.cancel_children();
            }
        }
        Ok(action.into())
    }

    fn read(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut value = state
            .records
            .last()
            .map(|goal| json!(goal))
            .unwrap_or(json!({}));
        if let Some(facts) = self.runtime().and_then(|runtime| runtime.execution_facts()) {
            value["execution_attempts"] = facts;
        }
        value
    }

    // 模型只使用当前计划和可回读事实；完整read/current继续供持久化和Office。
    fn model_view(&self, mut value: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.remove("review");
        }
        if !value["id"].is_string() {
            return value;
        }
        let goal_id = value["id"].clone();
        if let Some(tasks) = value["tasks"].as_array_mut() {
            let retired = tasks.iter().filter(|t| t["current"] == false).count();
            let mut index = 0;
            tasks.retain_mut(|task| {
                if task["current"] != false {
                    return true;
                }
                index += 1;
                if let Some(object) = task.as_object_mut() {
                    object.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "id" | "title" | "status" | "current" | "replaces"
                        )
                    });
                }
                index + 32 > retired
            });
            value["retired_task_count"] = json!(retired);
        }
        let required: HashSet<String> = value["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["evidence"].as_array().into_iter().flatten())
            .filter_map(|e| e["observation_id"].as_str().map(str::to_owned))
            .collect();
        if let Some(observations) = value["observations"].as_array_mut() {
            let total = observations.len();
            let mut index = 0;
            observations.retain(|o| {
                let keep =
                    index + 32 >= total || o["id"].as_str().is_some_and(|id| required.contains(id));
                index += 1;
                keep
            });
            for observation in observations.iter_mut() {
                let id = observation["id"].clone();
                if let Some(object) = observation.as_object_mut() {
                    if object
                        .get("input")
                        .is_some_and(|v| v.to_string().len() > 1024)
                    {
                        object.remove("input");
                    }
                    object.remove("output");
                }
                observation["reference"] = json!(format!(
                    "ctx:v1:{}",
                    json!([
                        self.owner,
                        format!("goal/{}", goal_id.as_str().unwrap()),
                        0,
                        id
                    ])
                ));
            }
            value["observation_count"] = json!(total);
        }
        if let Some(history) = value["history"].as_array_mut() {
            let total = history.len();
            if total > 16 {
                history.drain(..total - 16);
            }
            for entry in history {
                if let Some(object) = entry.as_object_mut() {
                    object.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "action"
                                | "task_id"
                                | "turn_id"
                                | "version"
                                | "plan_version"
                                | "reason"
                                | "user_message_id"
                                | "user_excerpt"
                                | "tasks"
                        )
                    });
                    if let Some(tasks) = object.get_mut("tasks").and_then(Value::as_array_mut) {
                        tasks.truncate(32);
                        for task in tasks {
                            if let Some(task) = task.as_object_mut() {
                                task.retain(|key, _| {
                                    matches!(
                                        key.as_str(),
                                        "id" | "title" | "expected_result" | "current" | "status"
                                    )
                                });
                            }
                        }
                    }
                }
            }
            value["history_count"] = json!(total);
        }
        if let Some(facts) = value["execution_attempts"].as_array_mut() {
            let total = facts.len();
            if total > 32 {
                facts.drain(..total - 32);
            }
            for fact in facts {
                let payload = &fact["payload"];
                let reference = (fact["kind"] == "tool_result").then(|| {
                    format!(
                        "ctx:v1:{}",
                        json!([
                            self.owner,
                            fact["turn_id"],
                            fact["response_index"],
                            payload["call_id"]
                        ])
                    )
                });
                *fact = json!({"kind":fact["kind"],"turn_id":fact["turn_id"],"name":payload["name"],"disposition":payload["disposition"]});
                if let Some(reference) = reference {
                    fact["reference"] = json!(reference);
                }
            }
            value["execution_attempt_count"] = json!(total);
        }
        value
    }

    /// 功能：读取当前完整 Goal 事实供 Office 只读投影。
    /// 参数：无。
    /// 返回：已有完整计划时返回事实，否则无。
    /// 错误：无。
    /// 状态：只读，不授予执行权限。
    /// 并发与取消：克隆短锁快照后释放。
    /// 权限：仅当前会话。
    /// 关联：Office 任务和白板投影。
    pub fn current(&self) -> Option<Value> {
        let mut value = self.read();
        value["session_id"] = json!(self.owner);
        (value["plan_version"].as_u64().unwrap_or(0) > 0).then_some(value)
    }

    fn update(&self, input: Value, invocation: &ToolInvocationContext) -> Result<Value, String> {
        let plan_change = matches!(input["action"].as_str(), Some("set_plan" | "replan"));
        // 计划提交、关闭旧许可和审核绑定始终持有同一user锁；并发工具不能插入新版本间隙。
        let mut user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if user.as_ref().is_some_and(|user| user.executing) {
            let result = self
                .execute_update(user.as_mut().unwrap(), input)
                .and_then(|value| {
                    if plan_change {
                        self.queue_review(user.as_mut().unwrap(), invocation)?;
                    }
                    Ok(value)
                });
            let paused = self.read()["phase"] == "paused";
            drop(user);
            if paused {
                if let Some(runtime) = self.runtime() {
                    runtime.cancel_children();
                }
            }
            self.project();
            return result;
        }
        let user = user
            .as_mut()
            .filter(|user| user.editable)
            .ok_or("本回合没有 Goal 用户编辑权限")?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("当前聊天没有 Goal")?;
        if !matches!(goal.phase.as_str(), "draft" | "paused" | "blocked") {
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
            Some(action @ ("manual_pause" | "manual_resume")) => {
                if goal.confirmed_version == 0
                    || !matches!(goal.phase.as_str(), "paused" | "blocked")
                {
                    return Err("人工暂停或恢复必须先停止已确认 Goal".into());
                }
                let excerpt = input["user_excerpt"]
                    .as_str()
                    .filter(|s| {
                        !s.trim().is_empty()
                            && s.encode_utf16().count() <= 1000
                            && user.text.contains(*s)
                    })
                    .ok_or("人工控制必须引用本轮真实用户要求，不能由执行模型自行授权")?;
                let id = input["task_id"].as_str().ok_or("必须提供任务身份")?;
                if action == "manual_pause" {
                    let task = goal
                        .tasks
                        .iter()
                        .find(|task| task.current && task.id == id)
                        .ok_or("任务不属于当前 Goal")?;
                    if matches!(task.status.as_str(), "accepted" | "running") {
                        return Err("已验收或仍在执行的任务不能人工暂停；先等待停止完成".into());
                    }
                    let source = ManualSource {
                        task_id: id.into(),
                        turn_id: user.turn_id.clone(),
                        user_message_id: user.message_id.clone(),
                        user_excerpt: excerpt.into(),
                    };
                    constrain_manual(&mut goal.tasks, &source);
                } else {
                    if !goal.tasks.iter().any(|task| {
                        task.manual.as_ref().is_some_and(|manual| {
                            manual.sources.iter().any(|source| source.task_id == id)
                        })
                    }) {
                        return Err("该任务没有可由用户恢复的人工暂停来源".into());
                    }
                    for task in &mut goal.tasks {
                        if let Some(mut manual) = task.manual.take() {
                            manual.sources.retain(|source| source.task_id != id);
                            if manual.sources.is_empty() {
                                if task.current {
                                    task.status = manual.previous_status;
                                    task.block_reason = manual.previous_block_reason;
                                }
                            } else {
                                task.manual = Some(manual);
                            }
                        }
                    }
                }
                goal.phase = "paused".into();
                refresh_waiting(goal, false);
            }
            Some("recheck_task") => {
                recheck_block(goal, &input, reason)?;
                refresh_waiting(goal, false);
            }
            Some("set_plan") => {
                let tasks = parse_plan_definitions(&input["tasks"])?;
                let criteria: Vec<String> =
                    serde_json::from_value(input["acceptance_criteria"].clone())
                        .map_err(|_| "整体验收条件格式无效")?;
                validate_criteria(&criteria)?;
                if goal.confirmed_version > 0 {
                    input["user_excerpt"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && user.text.contains(*s))
                        .ok_or("停止后修改需求必须引用本轮真实用户输入")?;
                    goal.phase = "paused".into();
                    goal.evidence.clear();
                }
                if let Some(objective) = input.get("objective") {
                    goal.objective = objective
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 4000)
                        .ok_or("整体目标无效")?
                        .into();
                }
                goal.tasks = merge_plan_definitions(&goal.tasks, tasks)?;
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
        if goal.phase != "blocked" {
            goal.reason = reason.into();
        }
        let mut previous = previous;
        previous["action"] = input["action"].clone();
        previous["task_id"] = input["task_id"].clone();
        previous["user_excerpt"] = input["user_excerpt"].clone();
        goal.history.push(previous);
        if render_plan(goal).encode_utf16().count() > 45_000 {
            return Err("完整计划过长，请精简描述后重新提交".into());
        }
        let result = json!(goal);
        self.commit(&next)?;
        *state = next;
        drop(state);
        if plan_change {
            self.queue_review(user, invocation)?;
        }
        Ok(result)
    }

    fn execute_update(&self, user: &mut UserTurn, input: Value) -> Result<Value, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        let goal = next.records.last_mut().ok_or("当前聊天没有 Goal")?;
        if !(goal.phase == "active"
            || (goal.phase == "blocked" && input["action"] == "recheck_task"))
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
        let paused = question_paused_tasks(goal, &user.turn_id);
        if input["task_id"]
            .as_str()
            .is_some_and(|id| paused.contains(id))
        {
            return Err(
                "该分支问答已暂停，等待原聊天用户明确继续；不能同回合重检、执行或验收".into(),
            );
        }
        if input.get("objective").is_some() || input.get("acceptance_criteria").is_some() {
            goal.phase = "paused".into();
            goal.reason =
                "执行模型请求改变整体目标或整体验收条件；已拒绝，等待用户停止后编辑并显式继续"
                    .into();
            for task in goal
                .tasks
                .iter_mut()
                .filter(|task| task.current && task.status == "running")
            {
                task.status = "interrupted".into();
            }
            goal.history.push(json!({"action":"scope_change_rejected","reason":goal.reason,"turn_id":user.turn_id,"user_message_id":user.message_id}));
            self.commit(&next)?;
            *state = next;
            user.task_id = None;
            drop(state);
            return Err(
                "整体目标或整体验收条件变更已拒绝；已停止执行，等待用户编辑并显式继续".into(),
            );
        }
        if action == "replan" {
            let definitions = parse_plan_definitions(&input["tasks"])?;
            for old in goal
                .tasks
                .iter()
                .filter(|task| task.current && paused.contains(&task.id))
            {
                if definitions
                    .iter()
                    .find(|new| new.id == old.id)
                    .is_none_or(|new| !same_task_definition(old, new))
                    || definitions.iter().any(|new| {
                        new.id != old.id
                            && !goal
                                .tasks
                                .iter()
                                .any(|task| task.id == new.id && same_task_definition(task, new))
                            && (new.title == old.title
                                || new.expected_result == old.expected_result
                                || new.replaces.contains(&old.id))
                    })
                {
                    return Err(
                        "暂停分支及依赖等待用户明确继续，不能通过重规划、替换或新建等价任务绕过"
                            .into(),
                    );
                }
            }
            let mut previous = goal.tasks.clone();
            for task in previous
                .iter_mut()
                .filter(|task| task.current && task.status == "running")
            {
                let changed = definitions
                    .iter()
                    .find(|new| new.id == task.id)
                    .is_none_or(|new| !same_task_definition(task, new));
                if changed
                    && task.assignee_id.is_none()
                    && task.delegation_id.is_none()
                    && goal
                        .observations
                        .iter()
                        .rev()
                        .find(|o| o.task_id.as_deref() == Some(&task.id))
                        .is_some_and(|o| o.is_error)
                {
                    task.status = "pending".into();
                }
            }
            let tasks = merge_plan_definitions(&previous, definitions)?;
            let previous = json!({"action":"replan","plan_version":goal.plan_version,"objective":goal.objective,"acceptance_criteria":goal.acceptance_criteria,"tasks":goal.tasks,"reason":reason,"turn_id":user.turn_id,"user_message_id":user.message_id});
            goal.tasks = tasks;
            goal.plan_version = goal
                .plan_version
                .checked_add(1)
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or("计划版本已耗尽")?;
            goal.evidence.clear();
            goal.history.push(previous);
            if render_plan(goal).encode_utf16().count() > 45_000 {
                return Err("完整计划过长，请精简描述后重新提交".into());
            }
        } else if action == "accept_goal" {
            if !goal
                .tasks
                .iter()
                .filter(|task| task.current)
                .all(|task| task.status == "accepted")
            {
                return Err("整体验收前必须先验收所有当前任务".into());
            }
            goal.evidence = validate_evidence(
                &input["evidence"],
                &goal.acceptance_criteria,
                &goal.observations,
                None,
                goal.plan_version,
            )
            .map_err(|error| format!("整体验收需要任务全部通过后的独立检查：{error}"))?;
            goal.phase = "completed".into();
        } else {
            let id = input["task_id"].as_str().ok_or("必须提供任务身份")?;
            let index = goal
                .tasks
                .iter()
                .position(|task| task.current && task.id == id)
                .ok_or("任务不属于当前 Goal")?;
            match action {
                "block_task" => {
                    let task = &mut goal.tasks[index];
                    if !matches!(task.status.as_str(), "pending" | "running" | "blocked") {
                        return Err(
                            "只有待执行或主执行中的任务可以报告执行阻塞；已有产出、中断、验收和人工约束不可改写".into(),
                        );
                    }
                    if task.status == "running" && task.delegation_id.is_some() {
                        return Err("委派仍在执行，须等待子执行安全结束后再报告阻塞".into());
                    }
                    task.status = "blocked".into();
                    task.block_reason = reason.into();
                }
                "recheck_task" if goal.tasks[index].status == "blocked" => {
                    recheck_block(goal, &input, reason)?;
                }
                "recheck_task" => {
                    if !matches!(
                        goal.tasks[index].status.as_str(),
                        "interrupted" | "produced"
                    ) {
                        return Err("只有中断或上轮未验收任务可以复核".into());
                    }
                    let observation = goal
                        .observations
                        .iter_mut()
                        .find(|observation| {
                            observation.id == input["observation_id"].as_str().unwrap_or("")
                                && observation.turn_id == user.turn_id
                                && !observation.is_error
                                && observation.task_id.as_deref().is_none_or(|task| task == id)
                                && observation.verification
                        })
                        .ok_or("复核必须先读取真实产物或执行事实，引用本轮成功观察编号")?;
                    let status = input["status"]
                        .as_str()
                        .filter(|s| matches!(*s, "pending" | "produced"))
                        .ok_or("复核后只能等待补做或记录已有产物")?;
                    let result = input["result"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or("复核结论不得为空")?;
                    observation.task_id = Some(id.into());
                    goal.tasks[index].status = status.into();
                    goal.tasks[index].result = result.into();
                    goal.tasks[index].evidence.clear();
                    if status == "pending" {
                        goal.tasks[index].assignee_id = None;
                        goal.tasks[index].child_workspace_id = None;
                        goal.tasks[index].delegation_id = None;
                    }
                }
                "start_task" => {
                    if !matches!(goal.tasks[index].status.as_str(), "pending" | "produced")
                        || user.task_id.as_deref().is_some_and(|current| current != id)
                    {
                        return Err("请先复核中断产物或完成当前任务验收，不能重复启动任务".into());
                    }
                    if !dependencies_satisfied(&goal.tasks[index], &goal.tasks) {
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
                        goal.plan_version,
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
        if goal.phase != "completed" {
            refresh_waiting(goal, true);
        }
        if action != "replan" {
            goal.history.push(json!({"action":action,"task_id":input["task_id"],"reason":reason,"turn_id":user.turn_id,"user_message_id":user.message_id}));
        }
        let result = json!(goal);
        self.commit(&next)?;
        *state = next;
        if action == "start_task" || (action == "recheck_task" && input["status"] == "produced") {
            user.task_id = input["task_id"].as_str().map(str::to_owned);
            user.main_start = self.runtime().and_then(|runtime| runtime.execution_facts());
        }
        if action == "recheck_task"
            && input["status"] == "pending"
            && user.task_id.as_deref() == input["task_id"].as_str()
        {
            user.task_id = None;
        }
        if action == "accept_task"
            || action == "accept_goal"
            || (action == "block_task" && user.task_id.as_deref() == input["task_id"].as_str())
        {
            user.task_id = None;
        }
        if action == "replan"
            && !state.records.last().unwrap().tasks.iter().any(|task| {
                task.current
                    && task.status == "running"
                    && user.task_id.as_deref() == Some(&task.id)
            })
        {
            user.task_id = None;
        }
        drop(state);
        Ok(result)
    }

    fn project(&self) {
        if let (Some(runtime), Some(goal)) = (self.runtime(), self.current()) {
            runtime.project(goal);
        }
    }

    fn observe(
        &self,
        name: &str,
        input: Value,
        outcome: &mut ToolCallOutcome,
    ) -> Result<(), String> {
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
        let verification = if name == "read_context_record" {
            let page: Value = serde_json::from_str(&outcome.content).unwrap_or(Value::Null);
            page["kind"] == "page"
                && page["is_error"] == false
                && page["reference"] == input["reference"]
                && page["content"].is_string()
        } else {
            matches!(
                name,
                "read_file" | "read_attachment" | "grep" | "glob" | "bash"
            )
        };
        goal.observations.push(Observation {
            id: id.clone(),
            task_id: user.task_id.clone(),
            tool: name.into(),
            input,
            output: outcome.content.clone(),
            verification: !outcome.is_error && verification,
            whole_goal: user.task_id.is_none()
                && goal
                    .tasks
                    .iter()
                    .filter(|task| task.current)
                    .all(|task| task.status == "accepted"),
            plan_version: goal.plan_version,
            is_error: outcome.is_error,
            turn_id: user.turn_id.clone(),
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
    pub fn continuation(&self) -> Option<String> {
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if !user.as_ref().is_some_and(|user| user.executing) {
            return None;
        }
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let goal = state.records.last().filter(|goal| goal.phase == "active")?;
        let paused = question_paused_tasks(goal, &user.as_ref()?.turn_id);
        if !paused.is_empty()
            && !goal.tasks.iter().any(|task| {
                task.current
                    && !paused.contains(&task.id)
                    && (matches!(task.status.as_str(), "running" | "produced" | "interrupted")
                        || (task.status == "pending" && dependencies_satisfied(task, &goal.tasks)))
            })
        {
            return None;
        }
        Some("Goal 仍有未完成工作。继续按当前依赖执行、检查产出或核对整体验收；普通回复结束不能代替目标完成，继续使用本回合剩余预算。".into())
    }

    /// 功能：判断本回合是否有真实用户授予的前台执行许可。
    /// 参数：无。
    /// 返回：当前回合的执行模式。
    /// 错误：无。
    /// 状态：只读，不依赖磁盘阶段推断权限。
    /// 并发与取消：工具实际执行仍逐次检查取消和任务。
    /// 权限：仅用于展示工具集合，不能代替 allowed。
    /// 关联：runtime::run_turn 工具声明筛选。
    pub fn is_execution_turn(&self) -> bool {
        self.user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|user| user.executing)
    }

    /// 功能：按实际回合许可筛选工具及Goal专用委派参数。
    /// 参数：tools为会话完整声明在本回合的副本。
    /// 返回：无；原位保留合法工具，普通回合隐藏goal_task_id。
    /// 错误：无；未获准的工具只从本轮声明移除。
    /// 状态：不改原始声明缓存、会话或计划。
    /// 并发与取消：读取当前许可，实际调用仍重新校验权限与取消。
    /// 权限：声明不授予执行许可；保留原有allowed筛选。
    /// 关联：Host run_turn、reserve_delegation、CHG-2026-0055。Stability: experimental。
    pub fn filter_tools(&self, tools: &mut Vec<ToolSpec>) {
        if !self.is_execution_turn() {
            tools.retain(|tool| self.allowed(&tool.name).is_ok());
            for tool in tools
                .iter_mut()
                .filter(|tool| matches!(tool.name.as_str(), "start_agent" | "delegate_task"))
            {
                if let Some(properties) = tool.input_schema["properties"].as_object_mut() {
                    properties.remove("goal_task_id");
                }
                tool.description
                    .push_str("当前回合不提供Goal任务绑定参数。");
            }
        }
    }

    /// 功能：生成已持久计划或执行验收事实的规范聊天展示。
    /// 参数：无。
    /// 返回：计划全文或当前执行结果与可追溯验收结论。
    /// 错误：无。
    /// 状态：不会把生成文本当成已提交展示。
    /// 并发与取消：短锁快照。
    /// 权限：不启动执行。
    /// 关联：runtime::run_turn 终态事务。
    pub fn presentation(&self) -> Option<String> {
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
                .filter(|task| task.current && task.status == "accepted")
                .count();
            let mut text = format!(
                "\n\nGoal {}：已验收 {accepted}/{} 项。{}\n",
                goal.id,
                goal.tasks.iter().filter(|task| task.current).count(),
                if goal.phase == "completed" {
                    "整体验收已通过"
                } else {
                    "整体尚未验收通过"
                }
            );
            text.push_str(&format!("变更原因：{}\n", goal.reason));
            for entry in goal
                .history
                .iter()
                .filter(|entry| entry["action"] == "replan")
            {
                text.push_str(&format!(
                    "重规划原因：{}\n",
                    entry["reason"].as_str().unwrap_or("")
                ));
            }
            for task in goal.tasks.iter().filter(|task| task.current) {
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
                text.push_str(&format!("{}\n", task_summary(task)));
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
        let turn = self
            .user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|user| user.turn_id.clone());
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .last()
            .filter(|goal| {
                goal.plan_version > 0
                    && (goal.phase == "draft"
                        || editing
                        || goal
                            .review
                            .as_ref()
                            .is_some_and(|review| review.status == "queued"))
                    && !(goal.phase != "ended"
                        && goal.displayed_version == goal.plan_version
                        && goal.review.as_ref().is_some_and(|review| {
                            review.status != "queued"
                                && Some(review.turn_id.as_str()) == turn.as_deref()
                        }))
            })
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
    pub fn displayed(&self) -> Result<(), String> {
        if self.presentation().is_none() {
            return Ok(());
        }
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
    /// 权限：产出及整体验收可用 bash 且沿用原审批；其他未知工具仍按交付工具拒绝。
    /// 关联：ToolHostValidator与ToolExecutor双重检查；新派发和只读等待。Stability: experimental。
    pub fn allowed(&self, name: &str) -> Result<(), String> {
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .last()
            .and_then(|goal| goal.review.as_ref())
            .is_some_and(|review| matches!(review.status.as_str(), "queued" | "waiting"))
            && !matches!(name, "goal_read" | "get_agent_status" | "wait_agent")
        {
            return Err("当前计划版本正在等待固定审核，尚未批准执行或再次修改".into());
        }
        let editing = user.as_ref().is_some_and(|user| user.editable);
        let executing = user.as_ref().is_some_and(|user| user.executing);
        if executing {
            self.check_cancelled()?;
            if matches!(name, "delegate_task" | "start_agent") {
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
                "goal_read"
                    | "request_user_input"
                    | "get_agent_status"
                    | "wait_agent"
                    | "goal_update"
                    | "read_file"
                    | "read_attachment"
                    | "read_context_record"
                    | "grep"
                    | "glob"
            ) {
                return Ok(());
            }
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let permitted = state.records.last().is_some_and(|goal| {
                let selected = goal.tasks.iter().find(|task| {
                    task.current
                        && user.as_ref().and_then(|user| user.task_id.as_deref())
                            == Some(task.id.as_str())
                });
                goal.phase == "active"
                    && (selected.is_some_and(|task| {
                        task.status == "running" && task.delegation_id.is_none()
                    }) || (name == "bash"
                        && (selected.is_some_and(|task| task.status == "produced")
                            || goal
                                .tasks
                                .iter()
                                .filter(|task| task.current)
                                .all(|task| task.status == "accepted"))))
            });
            if permitted {
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
                "goal_read"
                    | "request_user_input"
                    | "goal_update"
                    | "read_file"
                    | "read_attachment"
                    | "read_context_record"
                    | "grep"
                    | "glob"
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
    /// 状态：已有验收保留，running/produced 转 interrupted 并保留产物事实；不启动后台执行。
    /// 并发与取消：由 runtime 收尾调用。
    /// 权限：下轮必须重新绑定真实用户输入。
    /// 关联：TurnCleanup。
    pub fn finish(&self) {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = state.clone();
        if let Some(goal) = next
            .records
            .last_mut()
            .filter(|goal| !matches!(goal.phase.as_str(), "draft" | "ended" | "completed"))
        {
            pause_execution(goal, "本次前台执行已结束，未完成工作等待原聊天显式继续");
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
    plan_version: u64,
) -> Result<Vec<Evidence>, String> {
    let evidence: Vec<Evidence> =
        serde_json::from_value(input.clone()).map_err(|_| "必须提供逐项验收依据")?;
    if evidence.is_empty()
        || (0..criteria.len()).any(|index| !evidence.iter().any(|entry| entry.criterion == index))
    {
        return Err("验收依据必须覆盖每个条件，不能仅声明 done".into());
    }
    for entry in &evidence {
        if entry.criterion >= criteria.len() || entry.conclusion.trim().is_empty() {
            return Err(format!("验收依据条件{}越界或缺少核对结论", entry.criterion));
        }
        let observation = observations
            .iter()
            .find(|observation| observation.id == entry.observation_id)
            .ok_or_else(|| {
                format!(
                    "验收依据条件{}引用的观察{}不存在，请使用真实读取返回的编号",
                    entry.criterion, entry.observation_id
                )
            })?;
        if observation.task_id.as_deref() != task {
            return Err(format!("验收依据条件{}的观察{}来自{}，未绑定当前任务；先选中任务，再用read_context_record或读取工具复核原件",entry.criterion,entry.observation_id,observation.tool));
        }
        if !observation.verification {
            return Err(format!("验收依据条件{}的观察{}来自{}，不是成功的产物读取或检查；wait_agent/get_agent_status仅接收结果或状态，请用read_context_record回读实际原件后逐项核对",entry.criterion,entry.observation_id,observation.tool));
        }
        if task.is_none() && (!observation.whole_goal || observation.plan_version != plan_version) {
            return Err(format!(
                "验收依据条件{}不是全部任务通过后的当前版本独立检查",
                entry.criterion
            ));
        }
    }
    Ok(evidence)
}

fn validate_criteria(criteria: &[String]) -> Result<(), String> {
    if criteria.is_empty() || criteria.iter().any(|s| s.trim().is_empty()) {
        return Err("验收条件不得为空".into());
    }
    Ok(())
}

fn parse_plan_definitions(input: &Value) -> Result<Vec<GoalTask>, String> {
    if input.as_array().is_some_and(|tasks| {
        tasks.iter().any(|task| {
            task.as_object().is_none_or(|task| {
                task.keys().any(|key| {
                    ![
                        "id",
                        "title",
                        "expected_result",
                        "acceptance_criteria",
                        "dependencies",
                        "replaces",
                    ]
                    .contains(&key.as_str())
                })
            })
        })
    }) {
        return Err("任务只能包含计划定义，不得提供执行状态或验收事实".into());
    }
    let tasks: Vec<GoalTask> = serde_json::from_value(input.clone()).map_err(|_| "任务格式无效")?;
    validate_tasks(&tasks)?;
    Ok(tasks)
}

fn same_task_definition(old: &GoalTask, new: &GoalTask) -> bool {
    old.title == new.title
        && old.expected_result == new.expected_result
        && old.acceptance_criteria == new.acceptance_criteria
        && old.dependencies == new.dependencies
}

// 可信子终态只冻结原父回合的分支；新用户明确继续后沿既有产物复核规则处理。
fn question_paused_tasks(goal: &GoalRecord, turn: &str) -> HashSet<String> {
    let mut paused: HashSet<String> = goal
        .history
        .iter()
        .filter(|entry| {
            entry["action"] == "child_result"
                && entry["status"] == "paused"
                && entry["turn_id"] == turn
        })
        .filter_map(|entry| entry["task_id"].as_str().map(str::to_owned))
        .collect();
    loop {
        let before = paused.len();
        for task in goal.tasks.iter().filter(|task| task.current) {
            if task
                .dependencies
                .iter()
                .chain(&task.replaces)
                .any(|id| paused.contains(id))
            {
                paused.insert(task.id.clone());
            }
        }
        if paused.len() == before {
            return paused;
        }
    }
}

fn merge_plan_definitions(
    previous: &[GoalTask],
    mut definitions: Vec<GoalTask>,
) -> Result<Vec<GoalTask>, String> {
    let current_ids: HashSet<_> = definitions.iter().map(|task| task.id.clone()).collect();
    for new in &mut definitions {
        if let Some(old) = previous.iter().find(|old| old.id == new.id) {
            if !old.current {
                return Err("历史任务身份不可重新使用；请新建任务并声明replaces来源".into());
            }
            if !new.replaces.is_empty() && new.replaces != old.replaces {
                return Err("已有任务的替换来源不可改写".into());
            }
            new.replaces = old.replaces.clone();
            if same_task_definition(old, new) {
                *new = old.clone();
            } else if matches!(old.status.as_str(), "running" | "produced" | "interrupted") {
                return Err(
                    "正在执行、已产出或中断任务必须先检查实际结果，不能通过重规划清除执行事实"
                        .into(),
                );
            } else if let Some(manual) = &old.manual {
                if matches!(manual.previous_status.as_str(), "produced" | "interrupted") {
                    return Err(
                        "人工暂停保留的产出或中断事实须先由用户恢复并复核，不能通过改写清除".into(),
                    );
                }
                new.manual = Some(manual.clone());
                new.status = old.status.clone();
                new.block_reason = old.block_reason.clone();
                new.result = old.result.clone();
            }
        } else {
            if new.replaces.iter().any(|id| current_ids.contains(id)) {
                return Err("替换来源必须退出当前计划；拆分后使用新身份保留来源关系".into());
            }
            let sources: HashSet<_> = new.replaces.iter().collect();
            if sources.len() != new.replaces.len()
                || new.replaces.iter().any(|id| {
                    id == &new.id
                        || !previous.iter().any(|old| old.current && old.id == *id)
                        || previous
                            .iter()
                            .any(|old| old.id == *id && old.status == "interrupted")
                })
            {
                return Err("替换来源必须是本计划中未中断的现有任务，且不可重复或自引用".into());
            }
            let mut sources = Vec::new();
            for old in previous.iter().filter(|old| new.replaces.contains(&old.id)) {
                if let Some(manual) = &old.manual {
                    if matches!(manual.previous_status.as_str(), "produced" | "interrupted") {
                        return Err(
                            "人工暂停保留的产物须先由用户恢复并复核，不能通过替换清除".into()
                        );
                    }
                    for source in &manual.sources {
                        if !sources
                            .iter()
                            .any(|existing: &ManualSource| existing.task_id == source.task_id)
                        {
                            sources.push(source.clone());
                        }
                    }
                }
            }
            if !sources.is_empty() {
                new.manual = Some(ManualConstraint {
                    previous_status: "pending".into(),
                    previous_block_reason: String::new(),
                    sources,
                });
                new.status = "manual_blocked".into();
                new.block_reason = "继承原任务的人工暂停，等待用户明确恢复".into();
            }
        }
    }
    for old in previous.iter().filter(|old| old.current) {
        if !definitions.iter().any(|new| new.id == old.id) {
            if matches!(old.status.as_str(), "running" | "produced" | "interrupted") {
                return Err("执行中的任务须先检查或处理失败，再替换".into());
            }
            if !definitions.iter().any(|new| new.replaces.contains(&old.id)) {
                return Err("移除当前任务必须声明替换或拆分来源replaces，历史不会删除".into());
            }
        }
    }
    let mut historical: Vec<_> = previous
        .iter()
        .filter(|old| !old.current || !definitions.iter().any(|new| new.id == old.id))
        .cloned()
        .collect();
    for old in &mut historical {
        if old.current {
            old.current = false;
            old.status = "superseded".into();
        }
    }
    definitions.extend(historical);
    let sources = definitions
        .iter()
        .filter_map(|task| task.manual.as_ref())
        .flat_map(|manual| manual.sources.clone())
        .collect::<Vec<_>>();
    for source in sources {
        constrain_manual(&mut definitions, &source);
    }
    Ok(definitions)
}

fn task_summary(task: &GoalTask) -> String {
    let mut summary = format!(
        "阻塞原因：{}\n预期结果：{}\n验收条件：{}\n依赖：{}",
        task.block_reason,
        task.expected_result,
        task.acceptance_criteria.join("；"),
        if task.dependencies.is_empty() {
            "无".into()
        } else {
            task.dependencies.join("、")
        }
    );
    summary.push_str(&format!("\n状态：{}", task.status));
    if let Some(manual) = &task.manual {
        for source in &manual.sources {
            summary.push_str(&format!(
                "\n人工暂停（来源任务 {}，用户消息 {}）：{}",
                source.task_id, source.user_message_id, source.user_excerpt
            ));
        }
    }
    summary
}

fn constrain_manual(tasks: &mut [GoalTask], source: &ManualSource) {
    let mut protected = HashSet::from([source.task_id.clone()]);
    protected.extend(
        tasks
            .iter()
            .filter(|task| {
                task.current
                    && task.manual.as_ref().is_some_and(|manual| {
                        manual
                            .sources
                            .iter()
                            .any(|existing| existing.task_id == source.task_id)
                    })
            })
            .map(|task| task.id.clone()),
    );
    loop {
        let before = protected.len();
        for task in tasks.iter().filter(|task| task.current) {
            if task
                .dependencies
                .iter()
                .chain(&task.replaces)
                .any(|id| protected.contains(id))
            {
                protected.insert(task.id.clone());
            }
        }
        if before == protected.len() {
            break;
        }
    }
    for task in tasks
        .iter_mut()
        .filter(|task| task.current && protected.contains(&task.id) && task.status != "accepted")
    {
        let manual = task.manual.get_or_insert_with(|| ManualConstraint {
            previous_status: task.status.clone(),
            previous_block_reason: task.block_reason.clone(),
            sources: Vec::new(),
        });
        if !manual
            .sources
            .iter()
            .any(|existing| existing.task_id == source.task_id)
        {
            manual.sources.push(source.clone());
        }
        task.status = "manual_blocked".into();
        task.block_reason = format!("人工暂停：{}", source.user_excerpt);
    }
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
    for task in goal.tasks.iter().filter(|task| task.current) {
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
        } else if goal.plan_version > goal.confirmed_version {
            "当前版本尚未批准交付，请通过确认菜单选择执行／修改／取消。可以在原聊天补充修改要求；菜单失效后可回复“确认执行”。/goal continue 不会跳过新版本确认。"
        } else if goal.confirmed_version > 0 {
            "Goal 已停止，修改要求没有恢复交付。核对后请在原聊天发送 /goal continue；中断任务先复核真实产物。"
        } else {
            "计划待确认，尚未开始交付。可以直接用自然语言修改，核对后回复“确认执行”。"
        }
    ));
    text
}

/// 功能：在桌面及工作区工具最外层强制实施 Goal 规划权限。
/// 数据边界：包装既有 ToolHost，Goal 控制只使用当前上下文，其他工具仍交给原实现。
/// 约束：不创建回合或 writer；工具声明不能代替调用时的权限与取消检查。
/// 关联：GoalRuntime、CHG-2026-0050。
pub struct GoalToolHost {
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
    pub fn new(inner: Arc<dyn ToolHost>, context: Arc<GoalContext>) -> Self {
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
        self.call_with_context(name, args, &ToolInvocationContext::default())
            .await
    }
    /// 功能：实施Goal权限并向内层保留引擎实际调用身份。
    /// 参数：name、args为工具输入，context为原回合及调用身份。
    /// 返回：Goal结果或原工具结果。
    /// 错误：沿用权限、原工具与可靠观察保存错误。
    /// 状态：只由既有Goal更新和观察入口保存事实。
    /// 并发与取消：保留原工具取消和调用串行约束。
    /// 权限：调用身份不替代Goal确认或工具审批。
    /// 关联：ToolExecutor、CHG-2026-0054。Stability: experimental。
    async fn call_with_context(
        &self,
        name: &str,
        args: Value,
        context: &ToolInvocationContext,
    ) -> openpaw_core::Result<ToolCallOutcome> {
        if let Err(error) = self.context.allowed(name) {
            return Ok(ToolCallOutcome::error(error));
        }
        let value = match name {
            "goal_read" => Ok(self.context.read()),
            "goal_update" => self.context.update(args, context),
            _ => {
                let mut outcome = self
                    .inner
                    .call_with_context(name, args.clone(), context)
                    .await?;
                if let Err(error) = self.context.observe(name, args, &mut outcome) {
                    return Err(openpaw_core::Error::Session(error));
                }
                return Ok(outcome);
            }
        };
        Ok(match value {
            Ok(value) => {
                let value = self.context.model_view(value);
                ToolCallOutcome {
                    content: value.to_string(),
                    is_error: false,
                    structured: Some(value),
                    media: Vec::new(),
                }
            }
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
pub fn plan_tools() -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    for name in ["goal_read", "goal_update"] {
        let mut spec = ToolSpec::new(
            name,
            if name == "goal_read" {
                "读取当前聊天 Goal 及完整计划版本"
            } else {
                "规划用 set_plan/end；已停止用户编辑可manual_pause/manual_resume并引用user_excerpt；执行用 replan、start_task、record_result、accept_task、block_task；执行阻塞用recheck_task(status=pending或blocked,reason)由模型重检，无observation证据门槛，人工阻塞不可解除；replan仅提交任务定义，replaces记录替换/拆分来源；全任务通过后独立检查 accept_goal。更新携带 goal_id、expected_version、reason；验收 evidence 逐条件引用真实读取或检查编号；子结果先选中任务并用read_context_record回读原件，wait_agent状态不能直接验收"
            },
        );
        spec.input_schema = if name == "goal_read" {
            json!({"type":"object","properties":{},"additionalProperties":false})
        } else {
            json!({"type":"object","required":["action","goal_id","expected_version","reason"],"properties":{"action":{"enum":["set_plan","end","manual_pause","manual_resume","replan","start_task","record_result","accept_task","accept_goal","recheck_task","block_task"]},"goal_id":{"type":"string"},"expected_version":{"type":"integer"},"reason":{"type":"string"},"objective":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"tasks":{"type":"array","items":{"type":"object","required":["id","title","expected_result","acceptance_criteria","dependencies"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"expected_result":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"dependencies":{"type":"array","items":{"type":"string"}},"replaces":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}},"user_excerpt":{"type":"string"},"task_id":{"type":"string"},"result":{"type":"string"},"status":{"enum":["pending","produced","blocked"]},"observation_id":{"type":"string"},"evidence":{"type":"array","items":{"type":"object","required":["criterion","observation_id","conclusion"],"properties":{"criterion":{"type":"integer","minimum":0},"observation_id":{"type":"string"},"conclusion":{"type":"string"}},"additionalProperties":false}}},"additionalProperties":false})
        };
        spec.category = Some(ToolCategory::ReadOnly);
        spec.concurrency = Some(ToolConcurrency::NeverParallel);
        spec.max_result_chars = Some(0);
        tools.push(spec);
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::{GoalContext, GoalRuntime, GoalToolHost};
    use openpaw_core::{
        NullToolHost, SessionEvent, ToolCallOutcome, ToolExecutor, ToolHostValidator, ToolSpec,
    };
    use serde_json::{json, Value};
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    struct UnavailableRuntime;

    impl GoalRuntime for UnavailableRuntime {
        fn commit(&self, _event: Value) -> Result<(), String> {
            Err("journal unavailable".into())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn execution_facts(&self) -> Option<Value> {
            None
        }
        fn cancel_children(&self) {}
        fn project(&self, _goal: Value) {
            panic!("failed persistence must not publish a successful projection");
        }
    }

    #[test]
    fn failed_commit_keeps_goal_unpublished() {
        let context = GoalContext::new("session-a");
        context.attach(Arc::new(UnavailableRuntime));
        assert_eq!(
            context.begin("turn-a", "message-a", "/goal 交付报告"),
            Err("journal unavailable".into())
        );
        assert!(context.prompt().is_none());
        assert!(context.current().is_none());
    }

    #[derive(Default)]
    struct RecordingRuntime {
        events: Mutex<Vec<SessionEvent>>,
        cancelled: AtomicBool,
        facts: Mutex<Vec<Value>>,
    }

    impl GoalRuntime for RecordingRuntime {
        fn commit(&self, event: Value) -> Result<(), String> {
            self.events
                .lock()
                .unwrap()
                .push(SessionEvent::Unknown(Box::new(event)));
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
        fn execution_facts(&self) -> Option<Value> {
            Some(json!(*self.facts.lock().unwrap()))
        }
        fn cancel_children(&self) {}
        fn project(&self, _goal: Value) {}
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        match std::pin::pin!(future)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("synchronous Goal controls must not await external work"),
        }
    }

    fn confirmed_goal() -> (Arc<GoalContext>, Arc<RecordingRuntime>) {
        let context = GoalContext::new("session-a");
        let runtime = Arc::new(RecordingRuntime::default());
        context.attach(runtime.clone());
        context
            .begin("turn-plan", "message-plan", "/goal 交付报告")
            .unwrap();
        let host = GoalToolHost::new(Arc::new(NullToolHost), context.clone());
        let goal = ready(host.call("goal_read", json!({})))
            .unwrap()
            .structured
            .unwrap();
        let plan = ready(host.call(
            "goal_update",
            json!({
                "action":"set_plan", "goal_id":goal["id"], "expected_version":0,
                "reason":"按报告交付拆解", "acceptance_criteria":["报告可读"],
                "tasks":[{"id":"report", "title":"生成报告", "expected_result":"report.txt",
                    "acceptance_criteria":["内容完整"], "dependencies":[]}]
            }),
        ))
        .unwrap();
        assert!(!plan.is_error, "{}", plan.content);
        context.displayed().unwrap();
        context.finish();
        context
            .begin("turn-execute", "message-execute", "确认执行")
            .unwrap();
        (context, runtime)
    }

    #[test]
    fn original_pages_require_successful_reads_before_acceptance() {
        for mode in 0..8 {
            let (context, _) = confirmed_goal();
            let update = |action: &str, extra: Value| {
                let mut input = json!({"action":action,"task_id":"report","goal_id":context.current().unwrap()["id"],"expected_version":1,"reason":"核对已保存产物"});
                input
                    .as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                context.update(input, &Default::default())
            };
            update("start_task", json!({})).unwrap();
            update("record_result", json!({"result":"已保存的报告正文"})).unwrap();
            let mut page = json!({"kind":"page","reference":"saved-report","start":0,"content":"已保存的报告正文","is_error":false});
            match mode {
                1 => page["is_error"] = json!(true),
                2 => page["reference"] = json!("different-record"),
                3 => page["kind"] = json!("tool_result_preview"),
                _ => {}
            }
            let mut outcome = ToolCallOutcome::ok(if mode == 4 {
                "invalid page".into()
            } else {
                page.to_string()
            });
            outcome.is_error = mode == 5;
            let name = match mode {
                6 => "wait_agent",
                7 => "get_agent_status",
                _ => "read_context_record",
            };
            context
                .observe(name, json!({"reference":"saved-report"}), &mut outcome)
                .unwrap();
            let state = context.current().unwrap();
            let observation = state["observations"].as_array().unwrap().last().unwrap();
            assert_eq!(observation["task_id"], "report");
            assert_eq!(observation["turn_id"], "turn-execute");
            let result = update(
                "accept_task",
                json!({"evidence":[{"criterion":0,"observation_id":observation["id"],"conclusion":"实际读取内容符合报告条件"}]}),
            );
            assert_eq!(result.is_ok(), mode == 0, "case {mode}");
            assert_eq!(
                context.current().unwrap()["tasks"][0]["status"],
                if mode == 0 { "accepted" } else { "produced" }
            );
        }
    }

    #[test]
    fn explicit_resume_verifies_preserved_original_without_restarting_work() {
        let (context, runtime) = confirmed_goal();
        let goal = context.current().unwrap()["id"].clone();
        let update = |context: &GoalContext, action: &str, extra: Value| {
            let mut input = json!({"action":action,"task_id":"report","goal_id":goal,"expected_version":1,"reason":"复核已有交付"});
            input
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            context.update(input, &Default::default())
        };
        update(&context, "start_task", json!({})).unwrap();
        update(
            &context,
            "record_result",
            json!({"result":"已保存的报告正文"}),
        )
        .unwrap();
        context.finish();
        let restored = GoalContext::new("session-a");
        restored.attach(runtime.clone());
        restored.restore(&runtime.events.lock().unwrap()).unwrap();
        assert!(!restored.is_execution_turn());
        restored
            .begin("turn-resume", "message-resume", "/goal continue")
            .unwrap();
        let mut outcome = ToolCallOutcome::ok(json!({"kind":"page","reference":"saved-report","content":"已保存的报告正文","is_error":false}).to_string());
        restored
            .observe(
                "read_context_record",
                json!({"reference":"saved-report"}),
                &mut outcome,
            )
            .unwrap();
        let state = restored.current().unwrap();
        let id = state["observations"].as_array().unwrap().last().unwrap()["id"].clone();
        update(
            &restored,
            "recheck_task",
            json!({"observation_id":id,"status":"produced","result":"已保存的报告正文"}),
        )
        .unwrap();
        update(&restored,"accept_task",json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"本轮已读取并核对原件"}]})).unwrap();
        let state = restored.current().unwrap();
        assert_eq!(state["tasks"][0]["status"], "accepted");
        assert_eq!(
            state["history"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["action"] == "start_task")
                .count(),
            1
        );
    }

    #[test]
    fn main_task_without_work_can_transfer_once_but_started_work_cannot() {
        for started_work in [false, true] {
            let (context, runtime) = confirmed_goal();
            let goal = context.current().unwrap();
            context.update(json!({"action":"start_task","goal_id":goal["id"],"expected_version":1,"task_id":"report","reason":"开始当前任务"}), &Default::default()).unwrap();
            if started_work {
                runtime.facts.lock().unwrap().push(json!({"turn_id":"turn-execute","sequence":10,"kind":"tool_start","payload":{"name":"write_file","call_id":"work"}}));
            }
            let invocation = openpaw_core::ToolInvocationContext {
                turn_id: Some("turn-execute".into()),
                response_index: 1,
                tool_call_id: "dispatch".into(),
            };
            runtime.facts.lock().unwrap().push(json!({"turn_id":"turn-execute","response_index":1,"kind":"tool_start","payload":{"name":"start_agent","call_id":"dispatch"}}));
            let result =
                context.reserve_delegation(Some("report"), "attempt-transfer", Some(&invocation));
            if started_work {
                assert!(result.is_err(), "已开始的工作不得重复派发");
            } else {
                let binding = result.expect("尚未开展工作的主任务应能原子转交").unwrap();
                assert!(context
                    .reserve_delegation(Some("report"), "attempt-duplicate", None)
                    .is_err());
                binding.bind("real-child", "child-workspace").unwrap();
                assert_eq!(
                    context.current().unwrap()["tasks"][0]["assignee_id"],
                    "real-child"
                );
            }
        }
    }

    #[test]
    fn fixed_review_binds_version_and_only_explicit_execute_grants_permission() {
        for choice in [
            "execute",
            "modify",
            "cancel",
            "custom",
            "paused",
            "failed-save",
            "cancelled",
        ] {
            let (context, runtime) = confirmed_goal();
            let goal = context.current().unwrap();
            let host = GoalToolHost::new(Arc::new(NullToolHost), context.clone());
            let invocation = openpaw_core::ToolInvocationContext {
                turn_id: Some("turn-execute".into()),
                response_index: 2,
                tool_call_id: "new-plan".into(),
            };
            let outcome = ready(host.call_with_context("goal_update",json!({"action":"replan","goal_id":goal["id"],"expected_version":1,"reason":"重规划","tasks":[{"id":"report","title":"生成报告","expected_result":"report.txt","acceptance_criteria":["内容完整"],"dependencies":[]}]}),&invocation)).unwrap();
            assert!(!outcome.is_error, "{}", outcome.content);
            assert!(!context.is_execution_turn());
            assert!(context.allowed("start_agent").is_err());
            assert!(
                context.allowed("goal_update").is_err(),
                "不得跳过尚待审核的版本"
            );
            let review = context.pending_review().unwrap();
            assert_eq!(review.plan_version, 2);
            context.review_displayed(&review).unwrap();
            let answer_choice = if matches!(choice, "failed-save" | "cancelled") {
                "execute"
            } else {
                choice
            };
            let record: openpaw_core::user_input::UserQuestionRecord = serde_json::from_value(json!({
                "question_id":"session-a/turn-execute/2/new-plan","scope":{"parent_session_id":"session-a","asking_session_id":"session-a","source_user_message_id":"message-execute"},
                "turn_id":"turn-execute","response_index":2,"tool_call_id":"new-plan","request":review.request,"state":"answered","generation":1,"display_id":"review-1","deadline_ms":9999999999999i64,
                "answer":{"choice_id":answer_choice,"label":match answer_choice {"execute"=>"执行","modify"=>"修改","cancel"=>"取消",_=>"其他"},"custom_text":if choice=="custom" {json!("执行")} else {Value::Null}},"reason":null,
            })).unwrap();
            let mut wrong = record.clone();
            wrong.scope.asking_session_id = "another-session".into();
            assert!(context.settle_review(&review, Some(&wrong)).is_err());
            let mut stale = review.clone();
            stale.plan_version -= 1;
            assert!(
                context.settle_review(&stale, Some(&record)).is_err(),
                "旧版本答案不得授权当前版本"
            );
            if choice == "failed-save" {
                context.attach(Arc::new(UnavailableRuntime));
            }
            if choice == "cancelled" {
                runtime.cancelled.store(true, Ordering::SeqCst);
            }
            let result = context.settle_review(&review, (choice != "paused").then_some(&record));
            if matches!(choice, "failed-save" | "cancelled") {
                assert!(result.is_err());
                assert!(!context.is_execution_turn());
                continue;
            }
            assert_eq!(result.unwrap(), choice);
            assert_eq!(
                context.is_execution_turn(),
                choice == "execute",
                "自定义执行文字不能自授权"
            );
            assert!(
                context.settle_review(&review, Some(&record)).is_err(),
                "重复答案不得重复接续"
            );
        }
    }

    #[test]
    fn replan_closes_old_permission_before_releasing_update_for_projection() {
        struct GatedProjection {
            inner: Arc<RecordingRuntime>,
            gate: Arc<std::sync::Barrier>,
        }
        impl GoalRuntime for GatedProjection {
            fn commit(&self, event: Value) -> Result<(), String> {
                self.inner.commit(event)
            }
            fn is_cancelled(&self) -> bool {
                false
            }
            fn execution_facts(&self) -> Option<Value> {
                self.inner.execution_facts()
            }
            fn cancel_children(&self) {}
            fn project(&self, _: Value) {
                self.gate.wait();
                self.gate.wait();
            }
        }
        let (context, runtime) = confirmed_goal();
        let gate = Arc::new(std::sync::Barrier::new(2));
        context.attach(Arc::new(GatedProjection {
            inner: runtime,
            gate: gate.clone(),
        }));
        let copy = context.clone();
        let worker = std::thread::spawn(move || {
            let host = GoalToolHost::new(Arc::new(NullToolHost), copy.clone());
            ready(host.call_with_context("goal_update", json!({
                "action":"replan", "goal_id":copy.current().unwrap()["id"], "expected_version":1,
                "reason":"并发保护", "tasks":[{"id":"report", "title":"更新报告",
                "expected_result":"report.txt", "acceptance_criteria":["内容完整"], "dependencies":[]}]
            }), &openpaw_core::ToolInvocationContext {
                turn_id:Some("turn-execute".into()), response_index:2, tool_call_id:"parallel-plan".into()
            })).unwrap()
        });
        gate.wait();
        let permission = context.allowed("start_agent");
        gate.wait();
        assert!(!worker.join().unwrap().is_error);
        assert!(
            permission.is_err(),
            "提交新版本后不得暴露仍获旧许可的并发窗口"
        );
    }

    #[test]
    fn final_budget_replan_displays_full_plan_before_next_turn_confirmation() {
        let (context, _) = confirmed_goal();
        let host = GoalToolHost::new(Arc::new(NullToolHost), context.clone());
        let goal = context.current().unwrap();
        let invocation = openpaw_core::ToolInvocationContext {
            turn_id: Some("turn-execute".into()),
            response_index: 9,
            tool_call_id: "last-budget-plan".into(),
        };
        let outcome = ready(host.call_with_context("goal_update", json!({
            "action":"replan", "goal_id":goal["id"], "expected_version":1,
            "reason":"最后一轮调整", "tasks":[{"id":"report", "title":"更新报告",
                "expected_result":"report.txt", "acceptance_criteria":["新内容完整"], "dependencies":[]}]
        }), &invocation)).unwrap();
        assert!(!outcome.is_error, "{}", outcome.content);
        let presentation = context
            .presentation()
            .expect("预算耗尽仍须展示待审核的完整新版本");
        assert!(presentation.contains("新内容完整"));
        assert_eq!(context.current().unwrap()["displayed_version"], 1);
        context.displayed().unwrap();
        assert_eq!(context.current().unwrap()["displayed_version"], 2);
        context.finish();
        context
            .begin("turn-after-budget", "after-budget", "确认执行")
            .unwrap();
        assert!(context.is_execution_turn());
    }

    #[test]
    fn missing_plan_presentation_cannot_mark_new_version_displayed() {
        let (context, runtime) = confirmed_goal();
        let goal = context.current().unwrap();
        context.update(json!({"action":"replan", "goal_id":goal["id"], "expected_version":1,
            "reason":"审核保存前更新", "tasks":[{"id":"report", "title":"更新报告",
            "expected_result":"report.txt", "acceptance_criteria":["新内容完整"], "dependencies":[]}]}), &Default::default()).unwrap();
        context.attach(Arc::new(UnavailableRuntime));
        assert!(context
            .queue_review(
                context.user.lock().unwrap().as_mut().unwrap(),
                &openpaw_core::ToolInvocationContext {
                    turn_id: Some("turn-execute".into()),
                    response_index: 9,
                    tool_call_id: "failed-review".into()
                }
            )
            .is_err());
        context.attach(runtime);
        assert!(context.presentation().is_none());
        context.displayed().unwrap();
        assert_eq!(
            context.current().unwrap()["displayed_version"],
            1,
            "未展示的新版本不得标记为可文字确认"
        );
        context.finish();
        context
            .begin("next-turn", "next-message", "确认执行")
            .unwrap();
        assert!(!context.is_execution_turn());
    }

    #[test]
    fn failed_review_queue_closes_prior_execution_permission() {
        let (context, _) = confirmed_goal();
        assert!(context.is_execution_turn());
        context.attach(Arc::new(UnavailableRuntime));
        assert!(context
            .queue_review(
                context.user.lock().unwrap().as_mut().unwrap(),
                &openpaw_core::ToolInvocationContext {
                    turn_id: Some("turn-execute".into()),
                    response_index: 1,
                    tool_call_id: "new-plan".into()
                }
            )
            .is_err());
        assert!(!context.is_execution_turn());
        assert!(context.allowed("write_file").is_err());
        assert!(context
            .reserve_delegation(Some("report"), "attempt-after-failed-queue", None)
            .is_err());
    }

    struct DelegationTools;
    #[async_trait::async_trait]
    impl ToolExecutor for DelegationTools {
        async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
            Ok(["start_agent", "delegate_task", "get_agent_status"]
                .into_iter()
                .map(|name| {
                    let mut spec = ToolSpec::new(name, "委派或查询真实子会话");
                    spec.input_schema = json!({"type":"object","required":["task"],"additionalProperties":false,"properties":{"task":{"type":"string"},"task_id":{"type":"string"},"goal_task_id":{"type":"string"}}});
                    spec
                })
                .collect())
        }
        async fn call(&self, _name: &str, _input: Value) -> openpaw_core::Result<ToolCallOutcome> {
            panic!("声明回归不能启动真实工作")
        }
    }
    #[async_trait::async_trait]
    impl ToolHostValidator for DelegationTools {}

    #[test]
    fn cached_delegation_declarations_keep_goal_capability_for_later_turns() {
        let context = GoalContext::new("cold-session");
        let host = GoalToolHost::new(Arc::new(DelegationTools), context);
        assert!(
            ready(host.list_tools())
                .unwrap()
                .iter()
                .all(|spec| spec.input_schema["properties"]
                    .get("goal_task_id")
                    .is_some()),
            "Host创建会话时缓存完整声明，之后才开始普通或Goal回合"
        );
    }

    #[test]
    fn ordinary_delegation_hides_goal_parameters_and_explains_invalid_binding() {
        let context = GoalContext::new("ordinary");
        context
            .begin("ordinary-turn", "ordinary-message", "安排独立查询")
            .unwrap();
        let host = GoalToolHost::new(Arc::new(DelegationTools), context.clone());
        let mut tools = ready(host.list_tools()).unwrap();
        context.filter_tools(&mut tools);
        for spec in tools {
            let goal_parameter = spec.input_schema["properties"].get("goal_task_id");
            if spec.name == "get_agent_status" {
                assert!(goal_parameter.is_some(), "不更改其他工具的参数");
            } else {
                assert!(goal_parameter.is_none(), "普通委派不应暴露Goal任务绑定");
                assert!(spec.input_schema["properties"].get("task_id").is_some());
                assert_eq!(spec.input_schema["required"], json!(["task"]));
                assert_eq!(spec.input_schema["additionalProperties"], false);
            }
        }
        assert!(context
            .reserve_delegation(None, "ordinary-attempt", None)
            .unwrap()
            .is_none());
        let reason = context
            .reserve_delegation(Some("invented"), "invalid-attempt", None)
            .err()
            .unwrap();
        assert!(
            reason.contains("普通聊天") && reason.contains("goal_task_id"),
            "{reason}"
        );
        assert!(context.current().is_none());
    }

    #[test]
    fn goal_delegation_parameters_follow_current_turn_authority() {
        let (context, _) = confirmed_goal();
        let host = GoalToolHost::new(Arc::new(DelegationTools), context.clone());
        let cached = ready(host.list_tools()).unwrap();
        let mut tools = cached.clone();
        context.filter_tools(&mut tools);
        assert!(tools.iter().all(|spec| spec.input_schema["properties"]
            .get("goal_task_id")
            .is_some()));
        context.finish();
        context
            .begin("later-turn", "later-message", "看看当前进度")
            .unwrap();
        tools = cached.clone();
        context.filter_tools(&mut tools);
        assert!(tools.iter().all(|spec| spec.name == "get_agent_status"));
        assert!(context.reserve_delegation(None, "bypass", None).is_err());
        assert!(context
            .reserve_delegation(Some("report"), "no-permission", None)
            .is_err());
        context
            .begin("continue-turn", "continue-message", "/goal continue")
            .unwrap();
        tools = cached;
        context.filter_tools(&mut tools);
        assert_eq!(tools.len(), 3);
        assert!(
            tools.iter().all(|spec| spec.input_schema["properties"]
                .get("goal_task_id")
                .is_some()),
            "新许可回合应从原始缓存恢复Goal字段"
        );
    }

    #[test]
    fn cancellation_rejects_new_delegation_without_changing_task() {
        let (context, runtime) = confirmed_goal();
        runtime.cancelled.store(true, Ordering::SeqCst);
        let error = context
            .reserve_delegation(Some("report"), "attempt-a", None)
            .err()
            .unwrap();
        assert!(error.contains("取消"), "{error}");
        assert_eq!(context.current().unwrap()["tasks"][0]["status"], "pending");
    }

    #[test]
    fn detached_restore_exposes_paused_facts_without_execution_authority() {
        let (_original, runtime) = confirmed_goal();
        let events = runtime.events.lock().unwrap().clone();
        let restored = GoalContext::new("session-a");
        restored.restore(&events).unwrap();
        let goal = restored.current().unwrap();
        assert_eq!(goal["phase"], "paused");
        assert_eq!(goal["plan_version"], 1);
        assert_eq!(goal["tasks"][0]["status"], "pending");
        assert!(!restored.is_execution_turn());
        assert_eq!(
            restored.begin("turn-continue", "message-continue", "/goal continue"),
            Err("Goal 会话不可用".into())
        );
        assert_eq!(restored.current().unwrap()["phase"], "paused");
        assert!(!restored.is_execution_turn());
    }
}
