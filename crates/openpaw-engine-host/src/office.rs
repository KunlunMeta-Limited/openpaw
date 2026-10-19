//! 功能：把 AgentLoop 的工具和状态事件投影为 AI Office 动作。
//! 输入输出边界：输入为引擎事件和回合生命周期，输出为 `/actions` Gateway
//! 队列；Office 请求失败只记录日志，不反向改变引擎回合结果。
//! 设计约束：首个工具由 1 号协调角色交接，后续工具由上一工具角色交接；角色状态按
//! session owner 计数，避免一个 session 结束时错误地把另一个 session 的角色置为空闲。
//! 关联：`API-OFFICE-ACTION-001`、`REQ-B-OFFICE-003`、`CHG-2026-0003`。

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openpaw_engine::{AgentEvent, ClassifierStatus};
use reqwest::{Client, Url};
use serde_json::Value;
use tokio::sync::mpsc;

const MAX_SUBAGENT_OUTPUT_CHARS: usize = 20_000;
const SUBAGENT_OUTPUT_CHUNK_CHARS: usize = 96;
const OFFICE_VISUAL_SYSTEM_PROMPT: &str = r#"你正在 AI Office attached 模式中工作。可将职责清晰、可独立执行的任务通过 delegate_task 交给真实子 Agent，简单问答无需委派。当用户明确要求多 Agent、子 Agent、多空间协作且存在多个独立职责时，必须先调用 delegate_task；失败时说明原因，不能静默退化为单 Agent。按任务实际依赖安排协作，不套用固定领域角色或阶段模板。name 由你根据本次职责选择，优先使用清晰简短的中文功能名称；必要的 API、SQL 等技术标识可以保留。相同职责继续使用稳定 name，不要为近义职责重复创建空间。主 Agent 负责整合真实结果。每轮结束发送有效最终答复，严格遵循用户指定语言、格式和详略；允许纯文本、Markdown、JSON、XML等，不强制章节、文件清单或“无”占位。只有确实需要交付文件时，写入独立文件并调用 register_artifact 登记用户应查看的真实相对路径；shell或子Agent生成的成品也可登记，不登记检索脚本、临时数据和调试文件。子Agent过程文件保留在子空间。图示和图表只是可选展示能力，按用户需要选择合适格式，不因回答涉及某个主题就额外生成图表或强制使用Mermaid。可选图表文件.chart.json支持type为bar、bar3d、line、area、pie、donut、radar，以及title、categories、series[{name,data}]字段；数值必须真实、分类与系列等长，超过预览能力可交付其他通用格式。不得编造文件、工具执行或验证结果。"#;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// 功能：表示发送给 AI Office Gateway 的单个动作。
/// 字段/变体语义：`set_state` 更新角色状态；`desk_visit` 表示两个角色之间的工作交接。
/// 数据来源：`OfficeDispatcher` 根据 AgentEvent 和回合生命周期转换。
/// 数据去向：AI Office `/actions` 接口；序列化名称必须保持网关兼容。
/// 约束：角色编号使用 Office 名册编号；`task` 为空时不输出该字段。
/// 稳定性：跨项目公共动作契约，字段变化必须同步 Schema 和 CHG。
/// 关联：`API-OFFICE-ACTION-001`、`DAT-OFFICE-ACTION-001`。
pub enum OfficeAction {
    #[serde(rename = "set_state")]
    /// 更新一个 Office 角色的工作状态和可选任务描述。
    SetState {
        #[serde(rename = "workspaceId")]
        /// 当前会话的主空间编号；使用 session id 隔离并行对话的动画状态。
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        /// 主空间显示名称。
        workspace_name: String,
        #[serde(rename = "sessionId")]
        /// 动作所属会话编号，供 Gateway 和前端按对话筛选空间。
        session_id: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        /// 子 Agent 空间所属的父空间；主空间省略。
        parent_workspace_id: Option<String>,
        #[serde(rename = "rosterNo")]
        /// Office 名册中的角色编号，序列化字段名为 `rosterNo`。
        roster_no: u8,
        /// 角色状态，例如 `thinking`、`working` 或 `idle`。
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// 当前任务的脱敏短描述；没有任务时省略字段。
        task: Option<String>,
    },
    #[serde(rename = "desk_visit")]
    /// 表示一个回合内的角色拜访动作。
    DeskVisit {
        #[serde(rename = "workspaceId")]
        /// 当前会话的主空间编号。
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        /// 主空间显示名称。
        workspace_name: String,
        #[serde(rename = "sessionId")]
        /// 动作所属会话编号。
        session_id: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        /// 子 Agent 空间所属的父空间；主空间省略。
        parent_workspace_id: Option<String>,
        /// 发起拜访的 Office 名册编号；首站为 1 号角色，后续为上一工具角色。
        visitor: u8,
        /// 被拜访的目标角色名册编号。
        host: u8,
        /// 触发拜访的工具名或简短消息，不承载凭据和完整用户输入。
        message: String,
    },
    #[serde(rename = "workspace_handoff")]
    /// 表示两个真实 Agent 空间之间的任务交接。
    WorkspaceHandoff {
        #[serde(rename = "fromWorkspaceId")]
        from_workspace_id: String,
        #[serde(rename = "toWorkspaceId")]
        to_workspace_id: String,
        #[serde(rename = "fromWorkspaceName")]
        from_workspace_name: String,
        #[serde(rename = "toWorkspaceName")]
        to_workspace_name: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        /// going、returning 或 complete。
        phase: String,
        #[serde(rename = "rosterNo", skip_serializing_if = "Option::is_none")]
        roster_no: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "register_artifact")]
    /// 把已验证的工作区文件登记到所属空间，主空间登记由显式交付工具触发。
    RegisterArtifact {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        /// 相对于 OpenPaw 工作区的路径；不包含文件正文或工具命令。
        path: String,
    },
    #[serde(rename = "workspace_output")]
    /// 把子 Agent 可展示的模型正文增量或最终文本投影到所属空间。
    WorkspaceOutput {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(rename = "workspaceName")]
        workspace_name: String,
        #[serde(rename = "parentWorkspaceId", skip_serializing_if = "Option::is_none")]
        parent_workspace_id: Option<String>,
        /// `model_delta` 表示增量块，`model_final` 表示完整最终正文。
        kind: String,
        /// 只包含 assistant 可展示正文，不包含 reasoning 或工具输入输出。
        text: String,
    },
}

impl OfficeAction {
    fn session_id(&self) -> &str {
        match self {
            Self::SetState { session_id, .. }
            | Self::DeskVisit { session_id, .. }
            | Self::WorkspaceHandoff { session_id, .. }
            | Self::RegisterArtifact { session_id, .. }
            | Self::WorkspaceOutput { session_id, .. } => session_id,
        }
    }
}

#[derive(Debug, Clone)]
struct OfficeScope {
    workspace_id: String,
    workspace_name: String,
    session_id: String,
    parent_workspace_id: Option<String>,
}

impl OfficeScope {
    fn main(session_id: &str) -> Self {
        Self {
            workspace_id: session_id.to_string(),
            workspace_name: "我的空间".into(),
            session_id: session_id.to_string(),
            parent_workspace_id: None,
        }
    }
}

#[derive(Clone)]
/// 功能：维护 Office 动作发送队列和跨 session 的角色占用关系。
/// 数据来源：Host 回合开始、AgentEvent 工具事件和回合结束通知。
/// 数据去向：Gateway HTTP POST；角色占用只用于决定何时发送 `idle`。
/// 约束：发送采用有界非阻塞队列；网络超时或队列满不得阻塞 AgentLoop。
/// 并发与取消：队列由 Tokio worker 顺序消费；每个 owner 独立维护工作角色集合。
/// 权限：动作内容必须脱敏，不能绕过 Host 鉴权或写入敏感数据。
/// 稳定性：Office 投影 L3 边界，修改必须建立 CHG。
/// 关联：`API-OFFICE-ACTION-001`、`REQ-B-OFFICE-003`。
pub struct OfficeDispatcher {
    tx: mpsc::Sender<OfficeAction>,
    owners: Arc<Mutex<HashMap<u8, HashSet<String>>>>,
    scopes: Arc<Mutex<HashMap<String, OfficeScope>>>,
    removed_sessions: Arc<Mutex<HashSet<String>>>,
    enabled: bool,
}

#[derive(Debug, Clone)]
/// 功能：记录一个活动工具在 Office 中对应的角色和短完成态。
/// 约束：不保存路径、扩展名、完整 URL、正文或命令；只保存限长的内容主题。
pub(crate) struct ActiveToolProjection {
    role: u8,
    completed_task: String,
    failed_task: String,
}

/// 子 Agent 单回合的办公室投影状态，仅由 engine-host 委派执行器持有。
pub(crate) struct SubagentOfficeTurn {
    working: HashSet<u8>,
    last_tool_role: Option<u8>,
    tool_failed: bool,
    artifact_count: usize,
    active_tools: HashMap<String, ActiveToolProjection>,
    workspace: PathBuf,
    pending_artifacts: HashMap<String, String>,
    model_output: String,
    model_pending: String,
    model_output_chars: usize,
}

impl OfficeDispatcher {
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, mpsc::Receiver<OfficeAction>) {
        let (tx, rx) = mpsc::channel(128);
        (
            Self {
                tx,
                owners: Arc::new(Mutex::new(HashMap::new())),
                scopes: Arc::new(Mutex::new(HashMap::new())),
                removed_sessions: Arc::new(Mutex::new(HashSet::new())),
                enabled: false,
            },
            rx,
        )
    }

    /// 功能：创建 Office 动作分发器并启动可选的 Gateway 后台发送 worker。
    /// 参数：`url` 为 Gateway `/actions` 地址；传入 `None` 时保留队列但丢弃网络投影。
    /// 返回：可在线程间共享的分发器。
    /// 错误：不会因 worker 启动或 HTTP 请求失败返回错误；失败只记录警告并丢弃动作。
    /// 状态：初始没有角色占用；队列容量为 128 个动作。
    /// 并发与取消：`send` 使用非阻塞入队；每个 HTTP POST 最多等待 500 毫秒，不影响回合。
    /// 权限：URL 来自受控配置；动作请求不得携带 Basic Authentication 凭据。
    /// 关联：`REQ-B-OFFICE-008`、`CHG-2026-0003`。
    pub fn new(url: Option<String>) -> Self {
        let enabled = url.is_some();
        let (tx, mut rx) = mpsc::channel::<OfficeAction>(128);
        let removed_sessions = Arc::new(Mutex::new(HashSet::new()));
        if let Some(url) = url {
            let worker_removed_sessions = removed_sessions.clone();
            let worker = async move {
                let client = Client::new();
                while let Some(action) = rx.recv().await {
                    if worker_removed_sessions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .contains(action.session_id())
                    {
                        continue;
                    }
                    let result = tokio::time::timeout(
                        Duration::from_millis(500),
                        client.post(&url).json(&action).send(),
                    )
                    .await;
                    match result {
                        Ok(Ok(response)) if response.status().is_success() => {}
                        Ok(Ok(response)) => {
                            tracing::warn!(status = %response.status(), "办公室动作被网关拒绝")
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "办公室动作请求失败，已忽略本次动作")
                        }
                        Err(_) => tracing::warn!("办公室动作请求超时（500 毫秒），已忽略本次动作"),
                    }
                }
            };
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(worker);
            }
        } else {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { while rx.recv().await.is_some() {} });
            }
        }
        Self {
            tx,
            owners: Arc::new(Mutex::new(HashMap::new())),
            scopes: Arc::new(Mutex::new(HashMap::new())),
            removed_sessions,
            enabled,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 仅在 attached Office 模式中追加可视化指导，独立 Host/CLI 不改变默认提示词。
    pub(crate) fn system_prompt(&self) -> Option<String> {
        self.enabled
            .then(|| OFFICE_VISUAL_SYSTEM_PROMPT.to_string())
    }

    /// 功能：复用既有动作把显式交付文件登记到主空间。
    /// 参数：session_id为会话，path为已校验相对路径；返回：无。
    /// 错误：队列失败只记录诊断，不影响工具或引擎终态；状态：仅写投影队列。
    /// 并发与取消：沿用dispatcher有界队列；权限：路径校验由调用工具完成。
    /// 关联：register_artifact工具与Gateway结构化产物恢复。
    pub(crate) fn register_artifact(&self, session_id: &str, path: String) {
        self.send(OfficeAction::RegisterArtifact {
            session_id: session_id.to_string(),
            workspace_id: session_id.to_string(),
            path,
        });
    }

    fn send(&self, action: OfficeAction) {
        if self
            .removed_sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(action.session_id())
        {
            return;
        }
        if let Err(error) = self.tx.try_send(action) {
            tracing::warn!(%error, "办公室动作队列已满或已关闭，已忽略本次动作");
        }
    }

    fn scope(&self, owner: &str) -> OfficeScope {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(owner)
            .cloned()
            .unwrap_or_else(|| OfficeScope::main(owner))
    }

    fn register_scope(&self, owner: &str, scope: OfficeScope) {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(owner.to_string(), scope);
    }

    fn send_state(&self, owner: &str, role: u8, state: &str, task: Option<String>) {
        let scope = self.scope(owner);
        self.send(OfficeAction::SetState {
            workspace_id: scope.workspace_id,
            workspace_name: scope.workspace_name,
            session_id: scope.session_id,
            parent_workspace_id: scope.parent_workspace_id,
            roster_no: role,
            state: state.into(),
            task,
        });
    }

    fn send_visit(&self, owner: &str, visitor: u8, host: u8, message: &str) {
        let scope = self.scope(owner);
        self.send(OfficeAction::DeskVisit {
            workspace_id: scope.workspace_id,
            workspace_name: scope.workspace_name,
            session_id: scope.session_id,
            parent_workspace_id: scope.parent_workspace_id,
            visitor,
            host,
            message: message.into(),
        });
    }

    fn set_working(
        &self,
        owner: &str,
        role: u8,
        state: &str,
        task: Option<String>,
        working: &mut HashSet<u8>,
    ) {
        self.owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(role)
            .or_default()
            .insert(owner.to_string());
        self.send_state(owner, role, state, task);
        working.insert(role);
    }

    fn release(&self, owner: &str, role: u8) {
        let released = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(role_owners) = owners.get_mut(&role) else {
                return;
            };
            let released = role_owners.remove(owner);
            let empty = role_owners.is_empty();
            if empty {
                owners.remove(&role);
            }
            released
        };
        if released {
            self.send_state(owner, role, "idle", None);
        }
    }

    /// 清理指定 session 仍持有的全部角色；用于 abort、panic 或任务被外部终止后的兜底路径。
    pub fn clear_owner(&self, owner: &str) {
        let released_roles = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut released_roles = Vec::new();
            let mut empty_roles = Vec::new();
            for (role, role_owners) in owners.iter_mut() {
                if role_owners.remove(owner) {
                    released_roles.push(*role);
                }
                if role_owners.is_empty() {
                    empty_roles.push(*role);
                }
            }
            for role in empty_roles {
                owners.remove(&role);
            }
            released_roles
        };
        for role in released_roles {
            self.send_state(owner, role, "idle", None);
        }
    }

    /// 删除已结束会话的 Office scope，避免长期创建和删除会话造成内存累积。
    pub(crate) fn remove_scope(&self, owner: &str) {
        self.removed_sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(owner.to_string());
        self.clear_owner(owner);
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(owner);
    }

    /// 功能：开始一个 session 回合，清理该 owner 上一回合的角色并让 1 号角色进入思考状态。
    /// 参数：`owner` 为 session 所有者编号；`working` 为调用方保存的本回合角色集合。
    /// 返回：无；`working` 会被更新为当前占用角色。
    /// 错误：无；动作入队失败只记录警告。
    /// 状态：释放旧角色后发送 `set_state(thinking)`。
    /// 并发与取消：通过 owner 计数避免清理其他 session 的同一角色。
    /// 权限：不改变工具权限或审批状态。
    /// 关联：`REQ-B-OFFICE-003`、`EVT-HOST-SESSION-001`。
    pub fn turn_started(&self, owner: &str, working: &mut HashSet<u8>) {
        if !self
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(owner)
        {
            self.register_scope(owner, OfficeScope::main(owner));
        }
        for role in working.drain() {
            self.release(owner, role);
        }
        self.set_working(owner, 1, "thinking", Some("处理当前请求".into()), working);
    }

    /// 功能：观察单个 AgentEvent，并按工具类别更新 Office 角色状态。
    /// 参数：`owner` 为 session 所有者；`event` 为引擎事件；`working` 为角色占用集合；
    /// `last_tool_role` 保存当前工具链末端；`tool_failed` 保存末端工具结果；
    /// `active_tools` 记录仍在执行的工具调用及其 Office 投影。
    /// 返回：无；未知工具不产生动作，审批等待只更新 6 号审核角色。
    /// 错误：无；投影发送失败由队列记录并忽略。
    /// 状态：工具角色变化时由上一角色发起交接；链尾在工具完成后保持工作，直到后续交接或回合结束。
    /// 并发与取消：调用顺序即事件顺序；取消由回合结束逻辑统一清理。
    /// 权限：只使用引擎提供的白名单参数生成限长摘要，不读取或发送正文和命令。
    /// 关联：`EVT-HOST-TOOL-001`、`REQ-B-OFFICE-003`。
    pub(crate) fn observe(
        &self,
        owner: &str,
        event: &AgentEvent,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        tool_failed: &mut bool,
        active_tools: &mut HashMap<String, ActiveToolProjection>,
    ) {
        match event {
            AgentEvent::ToolExecutionStart {
                tool_use_id,
                name,
                input,
            } => {
                if let Some(role) = tool_role(name) {
                    let task = tool_activity_summary(name, input);
                    let completed_task = tool_activity_completion(name, input);
                    active_tools.insert(
                        tool_use_id.clone(),
                        ActiveToolProjection {
                            role,
                            completed_task,
                            failed_task: format!("{task}；失败，正在调整"),
                        },
                    );
                    self.send_tool_working(
                        owner,
                        role,
                        &task,
                        working,
                        last_tool_role,
                        active_tools,
                    );
                }
            }
            AgentEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                let projection = active_tools.remove(tool_use_id);
                if let Some(projection) = projection {
                    let role = projection.role;
                    let role_still_active = active_tools.values().any(|active| active.role == role);
                    if !role_still_active {
                        if Some(role) == *last_tool_role {
                            *tool_failed = *is_error;
                            self.set_working(
                                owner,
                                role,
                                "working",
                                Some(if *is_error {
                                    projection.failed_task
                                } else {
                                    projection.completed_task
                                }),
                                working,
                            );
                        } else if working.remove(&role) {
                            self.release(owner, role);
                        }
                    }
                }
            }
            AgentEvent::ClassifierStatus {
                status: ClassifierStatus::SuggestConfirm { .. },
                ..
            } => {
                self.set_working(owner, 6, "working", Some("等待审批".into()), working);
            }
            _ => {}
        }
    }

    fn send_tool_working(
        &self,
        owner: &str,
        role: u8,
        task: &str,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        active_tools: &HashMap<String, ActiveToolProjection>,
    ) {
        let previous = last_tool_role.unwrap_or(1);
        if previous != role {
            self.send_visit(owner, previous, role, handoff_message(role));
        }
        self.set_working(owner, role, "working", Some(task.to_string()), working);
        *last_tool_role = Some(role);

        if previous != 1
            && previous != role
            && !active_tools.values().any(|active| active.role == previous)
            && working.remove(&previous)
        {
            self.release(owner, previous);
        }
    }

    /// 功能：结束一个 session 回合并释放本 owner 持有的全部 Office 角色。
    /// 参数：`owner` 为 session 所有者；`working` 为本回合角色集合；最后角色用于成果交付；
    /// `completed` 指示回合是否成功完成。
    /// 返回：无；成功的工具回合先由最后角色向 1 号角色交付，再释放全部角色。
    /// 错误：无；Office 网络失败不影响已返回的引擎结果。
    /// 状态：逐一释放旧角色；只有最后一个 owner 释放角色时才发送 `idle`。
    /// 并发与取消：按 owner 释放，不能把其他 session 仍占用的角色置为空闲。
    /// 权限：整理消息不得包含敏感凭据或完整对话正文。
    /// 关联：`REQ-B-OFFICE-003`、`REQ-B-OFFICE-008`。
    pub fn turn_ended(
        &self,
        owner: &str,
        working: &mut HashSet<u8>,
        last_tool_role: Option<u8>,
        artifact_count: usize,
        completed: bool,
    ) {
        if completed {
            if artifact_count > 0 {
                let previous = last_tool_role.unwrap_or(1);
                if previous != 5 {
                    self.send_visit(owner, previous, 5, "请整理产物");
                }
                self.set_working(
                    owner,
                    5,
                    "working",
                    Some(format!("整理 {artifact_count} 项产物")),
                    working,
                );
                self.send_visit(owner, 5, 1, "产物已整理，请查收");
            } else if let Some(role) = last_tool_role.filter(|role| *role != 1) {
                self.send_visit(owner, role, 1, "任务已完成，请查收");
            }
        }
        for role in working.drain() {
            self.release(owner, role);
        }
    }

    /// 创建一个真实子 Agent 空间，并让主空间向该空间交接任务。
    pub(crate) fn subagent_started(
        &self,
        parent_session_id: &str,
        workspace_id: &str,
        workspace_name: &str,
        workspace: &Path,
    ) -> SubagentOfficeTurn {
        self.register_scope(
            workspace_id,
            OfficeScope {
                workspace_id: workspace_id.to_string(),
                workspace_name: workspace_name.to_string(),
                session_id: parent_session_id.to_string(),
                parent_workspace_id: Some(parent_session_id.to_string()),
            },
        );
        self.send(OfficeAction::WorkspaceHandoff {
            from_workspace_id: parent_session_id.to_string(),
            to_workspace_id: workspace_id.to_string(),
            from_workspace_name: "我的空间".into(),
            to_workspace_name: workspace_name.to_string(),
            session_id: parent_session_id.to_string(),
            phase: "going".into(),
            roster_no: Some(5),
            message: Some("正在分配子任务".into()),
        });
        let mut turn = SubagentOfficeTurn {
            working: HashSet::new(),
            last_tool_role: None,
            tool_failed: false,
            artifact_count: 0,
            active_tools: HashMap::new(),
            workspace: workspace.to_path_buf(),
            pending_artifacts: HashMap::new(),
            model_output: String::new(),
            model_pending: String::new(),
            model_output_chars: 0,
        };
        self.turn_started(workspace_id, &mut turn.working);
        turn
    }

    /// 把子 Agent 的真实 AgentEvent 投影到其独立办公室空间。
    pub(crate) fn observe_subagent(
        &self,
        workspace_id: &str,
        event: &AgentEvent,
        turn: &mut SubagentOfficeTurn,
    ) {
        match event {
            AgentEvent::TextDelta { text } => {
                let remaining = MAX_SUBAGENT_OUTPUT_CHARS.saturating_sub(turn.model_output_chars);
                if remaining > 0 {
                    let accepted = text.chars().take(remaining).collect::<String>();
                    turn.model_output_chars += accepted.chars().count();
                    turn.model_output.push_str(&accepted);
                    turn.model_pending.push_str(&accepted);
                    if turn.model_pending.chars().count() >= SUBAGENT_OUTPUT_CHUNK_CHARS {
                        let scope = self.scope(workspace_id);
                        self.send(OfficeAction::WorkspaceOutput {
                            session_id: scope.session_id,
                            workspace_id: scope.workspace_id,
                            workspace_name: scope.workspace_name,
                            parent_workspace_id: scope.parent_workspace_id,
                            kind: "model_delta".into(),
                            text: std::mem::take(&mut turn.model_pending),
                        });
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_use_id,
                name,
                input,
            } if matches!(name.as_str(), "write_file" | "edit_file" | "notebook_edit") => {
                if let Some(path) = input
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|path| workspace_relative_path(&turn.workspace, path))
                {
                    turn.pending_artifacts.insert(tool_use_id.clone(), path);
                }
            }
            AgentEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                let artifact = turn.pending_artifacts.remove(tool_use_id);
                if !*is_error {
                    if let Some(path) = artifact {
                        let scope = self.scope(workspace_id);
                        self.send(OfficeAction::RegisterArtifact {
                            session_id: scope.session_id,
                            workspace_id: scope.workspace_id,
                            path,
                        });
                        turn.artifact_count = turn.artifact_count.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
        self.observe(
            workspace_id,
            event,
            &mut turn.working,
            &mut turn.last_tool_role,
            &mut turn.tool_failed,
            &mut turn.active_tools,
        );
    }

    /// 结束子 Agent 回合，释放角色并把结果交回父空间。
    pub(crate) fn subagent_ended(
        &self,
        parent_session_id: &str,
        workspace_id: &str,
        workspace_name: &str,
        mut turn: SubagentOfficeTurn,
        completed: bool,
    ) {
        if !turn.model_output.is_empty() {
            let scope = self.scope(workspace_id);
            self.send(OfficeAction::WorkspaceOutput {
                session_id: scope.session_id,
                workspace_id: scope.workspace_id,
                workspace_name: scope.workspace_name,
                parent_workspace_id: scope.parent_workspace_id,
                kind: "model_final".into(),
                text: turn.model_output.clone(),
            });
        }
        self.turn_ended(
            workspace_id,
            &mut turn.working,
            turn.last_tool_role,
            turn.artifact_count,
            completed && !turn.tool_failed,
        );
        self.send(OfficeAction::WorkspaceHandoff {
            from_workspace_id: workspace_id.to_string(),
            to_workspace_id: parent_session_id.to_string(),
            from_workspace_name: workspace_name.to_string(),
            to_workspace_name: "我的空间".into(),
            session_id: parent_session_id.to_string(),
            phase: "returning".into(),
            roster_no: Some(5),
            message: Some(if completed {
                "子任务已完成，正在交回结果".into()
            } else {
                "子任务已结束，正在返回状态".into()
            }),
        });
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(workspace_id);
        let dispatcher = self.clone();
        let parent_session_id = parent_session_id.to_string();
        let workspace_id = workspace_id.to_string();
        let workspace_name = workspace_name.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(900)).await;
                dispatcher.send(OfficeAction::WorkspaceHandoff {
                    from_workspace_id: workspace_id,
                    to_workspace_id: parent_session_id.clone(),
                    from_workspace_name: workspace_name,
                    to_workspace_name: "我的空间".into(),
                    session_id: parent_session_id,
                    phase: "complete".into(),
                    roster_no: Some(5),
                    message: None,
                });
            });
        }
    }
}

fn workspace_relative_path(workspace: &Path, value: &str) -> Option<String> {
    let workspace = normalized_path(workspace)?;
    let input = Path::new(value);
    let absolute = if input.is_absolute() {
        normalized_path(input)?
    } else {
        normalized_path(&workspace.join(input))?
    };
    let relative = absolute.strip_prefix(&workspace).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(
        relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_string_lossy()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn normalized_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Some(normalized)
}

fn tool_role(name: &str) -> Option<u8> {
    match name {
        "web_search" | "web_fetch" | "web_extract" => Some(2),
        "read_file" | "grep" | "glob" => Some(3),
        "write_file" | "edit_file" | "notebook_edit" => Some(4),
        _ => None,
    }
}

fn handoff_message(role: u8) -> &'static str {
    match role {
        2 => "请搜索内容",
        3 => "请读取资料",
        4 => "请整理并写入",
        5 => "请验证结果",
        6 => "请审核操作",
        _ => "请处理任务",
    }
}

/// 功能：为每次真实工具调用生成短状态，使连续调用能在 UI 中及时刷新。
/// 安全：只显示截短的内容主题；路径、扩展名、完整 URL、正文和命令均不进入 UI。
pub(crate) fn tool_activity_summary(name: &str, input: &Value) -> String {
    match name {
        "web_search" => input
            .get("query")
            .and_then(Value::as_str)
            .map(compact_search_topic)
            .filter(|topic| !topic.is_empty())
            .map(|topic| format!("搜索：{topic}"))
            .unwrap_or_else(|| "搜索网页内容".into()),
        "web_fetch" | "web_extract" => web_source(input)
            .map(|source| format!("查阅：{source}"))
            .unwrap_or_else(|| "查阅网页资料".into()),
        "read_file" => file_topic(input)
            .map(|topic| format!("读取：{topic}"))
            .unwrap_or_else(|| "读取本地资料".into()),
        "write_file" => file_topic(input)
            .map(|topic| format!("写入：{topic}"))
            .unwrap_or_else(|| "整理并写入文档".into()),
        "edit_file" => file_topic(input)
            .map(|topic| format!("修改：{topic}"))
            .unwrap_or_else(|| "修改文档内容".into()),
        "notebook_edit" => file_topic(input)
            .map(|topic| format!("更新笔记：{topic}"))
            .unwrap_or_else(|| "更新笔记内容".into()),
        "grep" => "检索本地资料".into(),
        "glob" => "查找本地文件".into(),
        "bash" => "执行受控命令".into(),
        "todo_add" => "新增任务记录".into(),
        "todo_update" => "更新任务记录".into(),
        "todo_list" => "读取任务记录".into(),
        _ => "处理工具任务".into(),
    }
}

fn tool_activity_completion(name: &str, input: &Value) -> String {
    match name {
        "web_search" => input
            .get("query")
            .and_then(Value::as_str)
            .map(compact_search_topic)
            .filter(|topic| !topic.is_empty())
            .map(|topic| format!("已搜索：{topic}，正在整理"))
            .unwrap_or_else(|| "已完成搜索，正在整理".into()),
        "web_fetch" | "web_extract" => web_source(input)
            .map(|source| format!("已查阅：{source}，正在整理"))
            .unwrap_or_else(|| "已查阅网页资料，正在整理".into()),
        "read_file" => file_topic(input)
            .map(|topic| format!("已读取：{topic}，正在整理"))
            .unwrap_or_else(|| "已读取本地资料，正在整理".into()),
        "write_file" => file_topic(input)
            .map(|topic| format!("已写入：{topic}，继续处理"))
            .unwrap_or_else(|| "已写入文档，继续处理".into()),
        "edit_file" => file_topic(input)
            .map(|topic| format!("已修改：{topic}，继续处理"))
            .unwrap_or_else(|| "已修改文档，继续处理".into()),
        "notebook_edit" => file_topic(input)
            .map(|topic| format!("已更新笔记：{topic}，继续处理"))
            .unwrap_or_else(|| "已更新笔记，继续处理".into()),
        "grep" | "glob" => "已检索本地资料，正在整理".into(),
        "bash" => "受控命令已结束，继续处理".into(),
        "todo_add" | "todo_update" | "todo_list" => "任务记录已更新，继续处理".into(),
        _ => "工具任务已结束，继续处理".into(),
    }
}

/// 功能：把工具结果投影为短活动状态；后续工具开始或正文事件会自然覆盖它。
pub(crate) fn tool_result_activity(name: Option<&str>, is_error: bool) -> &'static str {
    if is_error {
        return match name {
            Some("web_search" | "web_fetch" | "web_extract") => "搜索失败，正在调整",
            Some("read_file" | "grep" | "glob") => "资料处理失败，正在调整",
            Some("write_file" | "edit_file" | "notebook_edit") => "文档处理失败，正在调整",
            _ => "工具处理失败，正在调整",
        };
    }
    match name {
        Some("web_search" | "web_fetch" | "web_extract") => "正在整理搜索结果",
        Some("read_file" | "grep" | "glob") => "正在整理已读取资料",
        Some("write_file" | "edit_file" | "notebook_edit") => "文档处理完成，继续整理",
        Some("todo_add" | "todo_update" | "todo_list") => "任务记录已更新，继续处理",
        _ => "正在整理工具结果",
    }
}

fn compact_search_topic(value: &str) -> String {
    compact_topic(value, 18)
}

fn file_topic(input: &Value) -> Option<String> {
    let value = input.get("path")?.as_str()?;
    let normalized_path = value.replace('\\', "/");
    let stem = Path::new(&normalized_path).file_stem()?.to_str()?;
    let topic = compact_topic(stem, 12);
    (!topic.is_empty()).then_some(topic)
}

fn web_source(input: &Value) -> Option<String> {
    let value = input.get("url")?.as_str()?;
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    let source = compact_topic(host, 20);
    (!source.is_empty()).then_some(source)
}

fn compact_topic(value: &str, max_chars: usize) -> String {
    let readable = value.replace(['_', '-'], " ");
    let normalized = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_engine::AgentEvent;

    fn recording_dispatcher() -> (OfficeDispatcher, mpsc::Receiver<OfficeAction>) {
        OfficeDispatcher::recording()
    }

    #[test]
    fn attached_prompt_requires_delegation_when_user_explicitly_requests_multi_agent_work() {
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("明确要求多 Agent、子 Agent、多空间协作"));
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("必须先调用 delegate_task"));
        assert!(OFFICE_VISUAL_SYSTEM_PROMPT.contains("不能静默退化为单 Agent"));
    }

    #[tokio::test]
    async fn subagent_actions_keep_parent_session_and_child_workspace_scope() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "tool-1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut turn,
        );
        dispatcher.subagent_ended(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );
        assert!(!dispatcher
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key("parent-session:agent:1"));

        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::WorkspaceHandoff {
                from_workspace_id,
                to_workspace_id,
                session_id,
                phase,
                ..
            } if from_workspace_id == "parent-session"
                && to_workspace_id == "parent-session:agent:1"
                && session_id == "parent-session"
                && phase == "going"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                workspace_id,
                workspace_name,
                session_id,
                parent_workspace_id: Some(parent_workspace_id),
                roster_no: 1,
                state,
                ..
            } if workspace_id == "parent-session:agent:1"
                && workspace_name == "研究 Agent"
                && session_id == "parent-session"
                && parent_workspace_id == "parent-session"
                && state == "thinking"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::DeskVisit {
                workspace_id,
                session_id,
                parent_workspace_id: Some(parent_workspace_id),
                visitor: 1,
                host: 3,
                ..
            } if workspace_id == "parent-session:agent:1"
                && session_id == "parent-session"
                && parent_workspace_id == "parent-session"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::WorkspaceHandoff {
                from_workspace_id,
                to_workspace_id,
                phase,
                ..
            } if from_workspace_id == "parent-session:agent:1"
                && to_workspace_id == "parent-session"
                && phase == "returning"
        )));

        tokio::time::sleep(Duration::from_millis(950)).await;
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::WorkspaceHandoff { phase, .. }) if phase == "complete"
        ));
    }

    #[tokio::test]
    async fn deleted_session_drops_delayed_workspace_handoff_completion() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let turn = dispatcher.subagent_started(
            "deleted-session",
            "deleted-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        dispatcher.subagent_ended(
            "deleted-session",
            "deleted-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );
        while actions.try_recv().is_ok() {}
        dispatcher.remove_scope("deleted-session");

        tokio::time::sleep(Duration::from_millis(950)).await;
        assert!(actions.try_recv().is_err());
    }

    #[tokio::test]
    async fn subagent_registers_only_successful_workspace_relative_artifacts() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "实现 Agent",
            workspace.path(),
        );
        while actions.try_recv().is_ok() {}

        let cases = [
            (
                "write",
                "write_file",
                serde_json::json!({"path":"reports/result.md","content":"private body"}),
                "reports/result.md",
            ),
            (
                "edit",
                "edit_file",
                serde_json::json!({
                    "path": workspace.path().join("notes/plan.md"),
                    "old_string":"private old",
                    "new_string":"private new"
                }),
                "notes/plan.md",
            ),
            (
                "notebook",
                "notebook_edit",
                serde_json::json!({
                    "path":"charts/demo.ipynb",
                    "operation":"insert",
                    "cell_index":0,
                    "source":"private source"
                }),
                "charts/demo.ipynb",
            ),
        ];
        for (tool_use_id, name, input, _) in &cases {
            dispatcher.observe_subagent(
                "parent-session:agent:1",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: (*tool_use_id).into(),
                    name: (*name).into(),
                    input: input.clone(),
                },
                &mut turn,
            );
            dispatcher.observe_subagent(
                "parent-session:agent:1",
                &AgentEvent::ToolResult {
                    tool_use_id: (*tool_use_id).into(),
                    content: "ok".into(),
                    is_error: false,
                    name: Some((*name).into()),
                    duration_ms: Some(1),
                },
                &mut turn,
            );
        }
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "failed".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"failed.md","content":"must not register"}),
            },
            &mut turn,
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ToolResult {
                tool_use_id: "failed".into(),
                content: "denied".into(),
                is_error: true,
                name: Some("write_file".into()),
                duration_ms: Some(1),
            },
            &mut turn,
        );

        let artifacts: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::RegisterArtifact {
                    session_id,
                    workspace_id,
                    path,
                } => Some((session_id, workspace_id, path)),
                _ => None,
            })
            .collect();
        assert_eq!(
            artifacts,
            cases
                .iter()
                .map(|(_, _, _, path)| {
                    (
                        "parent-session".to_string(),
                        "parent-session:agent:1".to_string(),
                        (*path).to_string(),
                    )
                })
                .collect::<Vec<_>>()
        );
        let wire = serde_json::to_string(&OfficeAction::RegisterArtifact {
            session_id: "parent-session".into(),
            workspace_id: "parent-session:agent:1".into(),
            path: "reports/result.md".into(),
        })
        .unwrap();
        assert!(!wire.contains("private"));
        assert!(!wire.contains(&workspace.path().display().to_string()));
        assert!(!wire.contains("command"));
    }

    #[tokio::test]
    async fn subagent_projects_incremental_and_final_model_text_without_reasoning() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let workspace = tempfile::tempdir().unwrap();
        let mut turn = dispatcher.subagent_started(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            workspace.path(),
        );
        while actions.try_recv().is_ok() {}

        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::ThinkingDelta {
                text: "不得投影的内部推理".into(),
            },
            &mut turn,
        );
        let first = "研究结果".repeat(30);
        let second = "，这是最终结论。";
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::TextDelta {
                text: first.clone(),
            },
            &mut turn,
        );
        dispatcher.observe_subagent(
            "parent-session:agent:1",
            &AgentEvent::TextDelta {
                text: second.into(),
            },
            &mut turn,
        );
        dispatcher.subagent_ended(
            "parent-session",
            "parent-session:agent:1",
            "研究 Agent",
            turn,
            true,
        );

        let outputs = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::WorkspaceOutput { kind, text, .. } => Some((kind, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(outputs.iter().any(|(kind, text)| kind == "model_delta"
            && text.contains("研究结果")
            && !text.contains("内部推理")));
        assert_eq!(
            outputs.last(),
            Some(&("model_final".into(), format!("{first}{second}")))
        );
    }

    #[tokio::test]
    async fn tool_roles_form_a_handoff_chain_in_event_order() {
        assert_eq!(tool_role("read_file"), Some(3));
        assert_eq!(tool_role("todo_add"), None);
        assert_eq!(tool_role("unknown"), None);
        let (d, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "1".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "1".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "2".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "3".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"private-resource"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(4));
        assert!(!roles.contains(&2));
        assert!(!roles.contains(&3));
        assert!(roles.contains(&4));
        let visits: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::DeskVisit {
                    visitor,
                    host,
                    message,
                    ..
                } => Some((visitor, host, message)),
                _ => None,
            })
            .collect();
        assert_eq!(
            visits,
            vec![
                (1, 2, "请搜索内容".into()),
                (2, 3, "请读取资料".into()),
                (3, 4, "请整理并写入".into()),
            ]
        );
    }

    #[tokio::test]
    async fn todo_tools_do_not_emit_office_actions() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        for (index, name) in ["todo_add", "todo_update", "todo_list"].iter().enumerate() {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: format!("todo-{index}"),
                    name: (*name).into(),
                    input: Value::Null,
                },
                &mut roles,
                &mut last_tool_role,
                &mut tool_failed,
                &mut active_tool_roles,
            );
        }

        assert_eq!(last_tool_role, None);
        assert!(roles.is_empty());
        assert!(actions.try_recv().is_err());
    }

    #[tokio::test]
    async fn approval_sets_reviewer_without_adding_a_visit() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();
        dispatcher.observe(
            "session-a",
            &AgentEvent::ClassifierStatus {
                tool_use_id: "call".into(),
                status: ClassifierStatus::SuggestConfirm {
                    reason: "manual".into(),
                },
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, None);
        assert!(roles.contains(&6));
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert_eq!(
            emitted
                .iter()
                .filter(|action| matches!(action, OfficeAction::DeskVisit { .. }))
                .count(),
            0
        );
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                roster_no: 6,
                state,
                ..
            } if state == "working"
        )));
    }

    #[tokio::test]
    async fn chain_tail_stays_working_after_its_last_tool_result() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        for tool_use_id in ["read-1", "read-2"] {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolExecutionStart {
                    tool_use_id: tool_use_id.into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path":"README.md"}),
                },
                &mut roles,
                &mut last_tool_role,
                &mut tool_failed,
                &mut active_tool_roles,
            );
        }
        while actions.try_recv().is_ok() {}

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "read-1".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert!(roles.contains(&3));
        assert!(actions.try_recv().is_err());

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "read-2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("read_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert!(roles.contains(&3));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 3,
                state,
                ..
            }) if state == "working"
        ));
    }

    #[tokio::test]
    async fn active_previous_role_is_released_only_after_its_last_result() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tool_roles = HashMap::new();

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "web".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "read".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(3));
        assert!(roles.contains(&2));
        assert!(roles.contains(&3));
        while actions.try_recv().is_ok() {}

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "web".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tool_roles,
        );

        assert!(!roles.contains(&2));
        assert!(roles.contains(&3));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 2,
                state,
                ..
            }) if state == "idle"
        ));
    }

    #[test]
    fn action_wire_uses_gateway_names() {
        let state = serde_json::to_value(OfficeAction::SetState {
            workspace_id: "session-a".into(),
            workspace_name: "我的空间".into(),
            session_id: "session-a".into(),
            parent_workspace_id: None,
            roster_no: 1,
            state: "thinking".into(),
            task: None,
        })
        .unwrap();
        assert_eq!(state["type"], "set_state");
        assert_eq!(state["rosterNo"], 1);
        assert_eq!(state["workspaceId"], "session-a");
        assert_eq!(state["sessionId"], "session-a");
        let visit = serde_json::to_value(OfficeAction::DeskVisit {
            workspace_id: "session-a".into(),
            workspace_name: "我的空间".into(),
            session_id: "session-a".into(),
            parent_workspace_id: None,
            visitor: 1,
            host: 3,
            message: "read_file".into(),
        })
        .unwrap();
        assert_eq!(visit["type"], "desk_visit");
    }

    #[test]
    fn turn_started_only_clears_previous_working_roles() {
        let dispatcher = OfficeDispatcher::new(None);
        let mut working = HashSet::from([3, 5]);
        dispatcher.turn_started("session-a", &mut working);
        assert_eq!(working, HashSet::from([1]));
    }

    #[tokio::test]
    async fn turn_ended_releases_all_roles_without_fake_review_work() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        let mut last_tool_role = None;
        let active_tool_roles = HashMap::from([(
            "read".to_string(),
            ActiveToolProjection {
                role: 3,
                completed_task: "已读取资料，正在整理".into(),
                failed_task: "读取资料；失败，正在调整".into(),
            },
        )]);
        dispatcher.turn_started("session-a", &mut working);
        dispatcher.send_tool_working(
            "session-a",
            3,
            "读取本地资料",
            &mut working,
            &mut last_tool_role,
            &active_tool_roles,
        );
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, last_tool_role, 0, false);

        assert!(working.is_empty());
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { roster_no: 1, state, .. } if state == "idle"
        )));
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { roster_no: 3, state, .. } if state == "idle"
        )));
        assert!(!emitted
            .iter()
            .any(|action| matches!(action, OfficeAction::SetState { roster_no: 5, .. })));
    }

    #[tokio::test]
    async fn parallel_sessions_release_only_their_own_workspace_role() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut first = HashSet::new();
        let mut second = HashSet::new();
        dispatcher.turn_started("session-a", &mut first);
        dispatcher.turn_started("session-b", &mut second);
        dispatcher.turn_ended("session-a", &mut first, None, 0, false);

        let first_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(first_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-a" && state == "idle"
        )));

        dispatcher.turn_ended("session-b", &mut second, None, 0, false);
        let second_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(second_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-b" && state == "idle"
        )));
    }

    #[tokio::test]
    async fn completed_tool_turn_emits_generic_delivery() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        dispatcher.turn_started("session-a", &mut working);
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, Some(4), 0, true);

        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 4,
                host: 1,
                message,
                ..
            }) if message == "任务已完成，请查收"
        ));
    }

    #[tokio::test]
    async fn clear_owner_idles_only_the_aborted_session_workspace() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut first = HashSet::new();
        let mut second = HashSet::new();
        dispatcher.turn_started("session-a", &mut first);
        dispatcher.turn_started("session-b", &mut second);
        while actions.try_recv().is_ok() {}

        dispatcher.clear_owner("session-a");

        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            } if session_id == "session-a" && state == "idle"
        )));
        assert!(!emitted.iter().any(|action| matches!(
            action,
            OfficeAction::SetState { session_id, .. } if session_id == "session-b"
        )));

        dispatcher.clear_owner("session-b");
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                session_id,
                roster_no: 1,
                state,
                ..
            }) if session_id == "session-b" && state == "idle"
        ));
    }

    #[tokio::test]
    async fn completed_artifact_turn_is_delivered_through_liu_shi() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut working = HashSet::new();
        dispatcher.turn_started("session-a", &mut working);
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, Some(4), 2, true);

        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 4,
                host: 5,
                message,
                ..
            }) if message == "请整理产物"
        ));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::SetState {
                roster_no: 5,
                state,
                task: Some(task),
                ..
            }) if state == "working" && task == "整理 2 项产物"
        ));
        assert!(matches!(
            actions.try_recv(),
            Ok(OfficeAction::DeskVisit {
                visitor: 5,
                host: 1,
                message,
                ..
            }) if message == "产物已整理，请查收"
        ));
    }

    #[test]
    fn activity_summary_exposes_only_allowed_fields() {
        let document = serde_json::json!({
            "path":"private/项目资料汇总版本.md",
            "content":"private-body"
        });
        assert_eq!(
            tool_activity_summary(
                "web_search",
                &serde_json::json!({"query":"主题甲","token":"private-token"}),
            ),
            "搜索：主题甲"
        );
        assert_eq!(
            tool_activity_summary("write_file", &document),
            "写入：项目资料汇总版本"
        );
        assert_eq!(
            tool_activity_summary(
                "web_fetch",
                &serde_json::json!({"url":"https://user:password@example.invalid/private-resource"}),
            ),
            "查阅：example.invalid"
        );
        assert_eq!(
            tool_activity_summary("read_file", &document),
            "读取：项目资料汇总版本"
        );
        assert_eq!(
            tool_activity_completion("read_file", &document),
            "已读取：项目资料汇总版本，正在整理"
        );
        let visible = [
            tool_activity_summary("write_file", &document),
            tool_activity_summary("read_file", &document),
            tool_activity_completion("read_file", &document),
        ]
        .join(" ");
        assert!(!visible.contains("private/"));
        assert!(!visible.contains(".md"));
        assert!(!visible.contains("private-body"));
    }

    #[test]
    fn tool_result_activity_allows_later_events_to_replace_failures() {
        assert_eq!(
            tool_result_activity(Some("web_search"), true),
            "搜索失败，正在调整"
        );
        assert_eq!(
            tool_result_activity(Some("web_search"), false),
            "正在整理搜索结果"
        );
        assert_eq!(
            tool_result_activity(Some("read_file"), false),
            "正在整理已读取资料"
        );
    }

    #[tokio::test]
    async fn later_success_replaces_a_failed_search_state() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tools = HashMap::new();

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "search-1".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题甲"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "search-1".into(),
                content: "error".into(),
                is_error: true,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "search-2".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query":"主题乙"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "search-2".into(),
                content: "ok".into(),
                is_error: false,
                name: Some("web_search".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );

        let tasks: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::SetState {
                    roster_no: 2,
                    task: Some(task),
                    ..
                } => Some(task),
                _ => None,
            })
            .collect();
        assert_eq!(
            tasks,
            vec![
                "搜索：主题甲",
                "搜索：主题甲；失败，正在调整",
                "搜索：主题乙",
                "已搜索：主题乙，正在整理",
            ]
        );
        assert!(!tool_failed);
    }

    #[tokio::test]
    async fn failed_tool_does_not_emit_a_successful_delivery() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut tool_failed = false;
        let mut active_tools = HashMap::new();
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_use_id: "write".into(),
                name: "write_file".into(),
                input: serde_json::json!({"path":"private-resource"}),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolResult {
                tool_use_id: "write".into(),
                content: "denied".into(),
                is_error: true,
                name: Some("write_file".into()),
                duration_ms: Some(1),
            },
            &mut roles,
            &mut last_tool_role,
            &mut tool_failed,
            &mut active_tools,
        );
        while actions.try_recv().is_ok() {}
        dispatcher.turn_ended("session-a", &mut roles, last_tool_role, 0, !tool_failed);
        assert!(!std::iter::from_fn(|| actions.try_recv().ok())
            .any(|action| matches!(action, OfficeAction::DeskVisit { host: 1, .. })));
    }

    #[tokio::test]
    async fn unresponsive_gateway_does_not_block_event_projection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                connections.push(stream);
            }
        });
        let dispatcher = OfficeDispatcher::new(Some(format!("http://{address}/actions")));
        let mut roles = HashSet::new();
        let started = std::time::Instant::now();

        dispatcher.turn_started("session-a", &mut roles);

        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(roles, HashSet::from([1]));
        server.abort();
    }
}
