//! 功能：保存会话归属的 Goal 草稿、完整计划及用户编辑来源，包装规划工具权限。
//! 输入输出边界：真实用户回合和模型工具输入进入，持久事实由 runtime 唯一 writer 提交。
//! 禁止承担：不执行交付任务，不写共享 Todo，不创建后台驱动或独立 session writer。
//! 关联：CHG-2026-0050、goal-command ticket 01。

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
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalTask {
    id: String,
    title: String,
    expected_result: String,
    acceptance_criteria: Vec<String>,
    dependencies: Vec<String>,
}

struct UserTurn {
    turn_id: String,
    message_id: String,
    text: String,
    editable: bool,
}

/// 功能：承载单个会话的 Goal 事实与当前真实用户回合，不持有 session 强引用。
/// 数据边界：只由 runtime 组装并恢复；模型不可提供执行许可或用户身份。
pub(crate) struct GoalContext {
    owner: String,
    state: Mutex<GoalState>,
    user: Mutex<Option<UserTurn>>,
    handle: Mutex<Weak<SessionHandle>>,
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

    /// 功能：绑定真实用户回合并接收精确 Goal 创建命令。
    /// 参数：turn 与 message 为 runtime 身份，text 为未替换的原输入。
    /// 返回：确定性拒绝回复，或交给模型规划。
    /// 错误：持久化失败停止处理。
    /// 状态：新草稿先落盘；重复目标不会覆盖当前记录。
    /// 并发与取消：每个会话已有单活动回合，Goal 锁串行化提交。
    /// 权限：所有草稿回合只有编辑权限。
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
                });
                self.commit(&next)?;
                *state = next;
                None
            }
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
        });
        Ok(reply)
    }

    /// 功能：提供草稿规划的模型约束。
    /// 参数：无。
    /// 返回：存在待确认 Goal 时的系统上下文。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：短锁，不持 session 锁。
    /// 权限：明确禁止规划期间的交付工具。
    /// 关联：goal_read、goal_update。
    pub(crate) fn prompt(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let goal = state.records.last().filter(|goal| goal.phase == "draft")?;
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
        let user = self.user.lock().unwrap_or_else(|e| e.into_inner());
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

    /// 功能：生成已持久化草稿的规范完整聊天展示。
    /// 参数：无。
    /// 返回：存在计划时返回完整文本。
    /// 错误：无。
    /// 状态：不会把生成文本当成已提交展示。
    /// 并发与取消：短锁快照。
    /// 权限：不启动执行。
    /// 关联：runtime::run_turn 终态事务。
    pub(crate) fn presentation(&self) -> Option<String> {
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
        let editing = self
            .user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|user| user.editable);
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

    /// 功能：在回合所有终态清除临时用户编辑来源。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：只清理进程内许可，持久计划保留。
    /// 并发与取消：由 runtime 收尾调用。
    /// 权限：下轮必须重新绑定真实用户输入。
    /// 关联：TurnCleanup。
    pub(crate) fn finish(&self) {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
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
            _ => return self.inner.call(name, args).await,
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

/// 功能：声明仅供 Goal 草稿规划回合使用的模型工具。
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
                "提交完整待确认计划或按真实用户要求结束目标；携带 goal_id、expected_version、非空 reason"
            },
        );
        spec.input_schema = if name == "goal_read" {
            json!({"type":"object","properties":{},"additionalProperties":false})
        } else {
            json!({"type":"object","required":["action","goal_id","expected_version","reason"],"properties":{"action":{"enum":["set_plan","end"]},"goal_id":{"type":"string"},"expected_version":{"type":"integer"},"reason":{"type":"string"},"objective":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"tasks":{"type":"array","items":{"type":"object","required":["id","title","expected_result","acceptance_criteria","dependencies"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"expected_result":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"dependencies":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}},"user_excerpt":{"type":"string"}},"additionalProperties":false})
        };
        spec.category = Some(ToolCategory::ReadOnly);
        spec.concurrency = Some(ToolConcurrency::NeverParallel);
        spec.max_result_chars = Some(0);
        tools.push(spec);
    }

    tools
}
