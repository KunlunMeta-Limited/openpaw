//! 功能：把 AgentLoop 的工具和状态事件投影为 AI Office 动作。
//! 输入输出边界：输入为引擎事件和回合生命周期，输出为 `/actions` Gateway
//! 队列；Office 请求失败只记录日志，不反向改变引擎回合结果。
//! 设计约束：首个工具由 1 号协调角色交接，后续工具由上一工具角色交接；角色状态按
//! session owner 计数，避免一个 session 结束时错误地把另一个 session 的角色置为空闲。
//! 关联：`API-OFFICE-ACTION-001`、`REQ-B-OFFICE-003`、`CHG-2026-0003`。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openpaw_engine::{AgentEvent, ClassifierStatus};
use reqwest::Client;
use tokio::sync::mpsc;

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
        /// 发起拜访的 Office 名册编号；首站为 1 号角色，后续为上一工具角色。
        visitor: u8,
        /// 被拜访的目标角色名册编号。
        host: u8,
        /// 触发拜访的工具名或简短消息，不承载凭据和完整用户输入。
        message: String,
    },
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
}

impl OfficeDispatcher {
    /// 功能：创建 Office 动作分发器并启动可选的 Gateway 后台发送 worker。
    /// 参数：`url` 为 Gateway `/actions` 地址；传入 `None` 时保留队列但丢弃网络投影。
    /// 返回：可在线程间共享的分发器。
    /// 错误：不会因 worker 启动或 HTTP 请求失败返回错误；失败只记录警告并丢弃动作。
    /// 状态：初始没有角色占用；队列容量为 128 个动作。
    /// 并发与取消：`send` 使用非阻塞入队；每个 HTTP POST 最多等待 500 毫秒，不影响回合。
    /// 权限：URL 来自受控配置；动作请求不得携带 Basic Authentication 凭据。
    /// 关联：`REQ-B-OFFICE-008`、`CHG-2026-0003`。
    pub fn new(url: Option<String>) -> Self {
        let (tx, mut rx) = mpsc::channel::<OfficeAction>(128);
        if let Some(url) = url {
            let worker = async move {
                let client = Client::new();
                while let Some(action) = rx.recv().await {
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
        }
    }

    fn send(&self, action: OfficeAction) {
        if let Err(error) = self.tx.try_send(action) {
            tracing::warn!(%error, "办公室动作队列已满或已关闭，已忽略本次动作");
        }
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
        self.send(OfficeAction::SetState {
            roster_no: role,
            state: state.into(),
            task,
        });
        working.insert(role);
    }

    fn release(&self, owner: &str, role: u8) {
        let should_idle = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(role_owners) = owners.get_mut(&role) else {
                return;
            };
            role_owners.remove(owner);
            let empty = role_owners.is_empty();
            if empty {
                owners.remove(&role);
            }
            empty
        };
        if should_idle {
            self.send(OfficeAction::SetState {
                roster_no: role,
                state: "idle".into(),
                task: None,
            });
        }
    }

    /// 清理指定 session 仍持有的全部角色；用于 abort、panic 或任务被外部终止后的兜底路径。
    pub fn clear_owner(&self, owner: &str) {
        let roles = {
            let mut owners = self
                .owners
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut roles = Vec::new();
            for (role, role_owners) in owners.iter_mut() {
                role_owners.remove(owner);
                if role_owners.is_empty() {
                    roles.push(*role);
                }
            }
            for role in &roles {
                owners.remove(role);
            }
            roles
        };
        for role in roles {
            self.send(OfficeAction::SetState {
                roster_no: role,
                state: "idle".into(),
                task: None,
            });
        }
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
        for role in working.drain() {
            self.release(owner, role);
        }
        self.set_working(owner, 1, "thinking", Some("处理当前请求".into()), working);
    }

    /// 功能：观察单个 AgentEvent，并按工具类别更新 Office 角色状态。
    /// 参数：`owner` 为 session 所有者；`event` 为引擎事件；`working` 为角色占用集合；
    /// `last_tool_role` 保存当前工具链末端；`active_tool_roles` 记录仍在执行的工具调用与角色映射。
    /// 返回：无；未知工具不产生动作，审批等待只更新 6 号审核角色。
    /// 错误：无；投影发送失败由队列记录并忽略。
    /// 状态：工具角色变化时由上一角色发起交接；链尾在工具完成后保持工作，直到后续交接或回合结束。
    /// 并发与取消：调用顺序即事件顺序；取消由回合结束逻辑统一清理。
    /// 权限：只使用工具名和脱敏状态，不读取或发送工具输入。
    /// 关联：`EVT-HOST-TOOL-001`、`REQ-B-OFFICE-003`。
    pub(crate) fn observe(
        &self,
        owner: &str,
        event: &AgentEvent,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        active_tool_roles: &mut HashMap<String, u8>,
    ) {
        match event {
            AgentEvent::ToolUseStart { tool_use_id, name } => {
                if let Some(role) = tool_role(name) {
                    active_tool_roles.insert(tool_use_id.clone(), role);
                    self.send_tool_working(
                        owner,
                        role,
                        name,
                        working,
                        last_tool_role,
                        active_tool_roles,
                    );
                }
            }
            AgentEvent::ToolResult {
                tool_use_id, name, ..
            } => {
                let role = active_tool_roles
                    .remove(tool_use_id)
                    .or_else(|| name.as_deref().and_then(tool_role));
                if let Some(role) = role {
                    let role_still_active =
                        active_tool_roles.values().any(|active| *active == role);
                    if !role_still_active {
                        if Some(role) == *last_tool_role {
                            self.set_working(
                                owner,
                                role,
                                "working",
                                Some(format!(
                                    "{} 已完成，等待后续处理",
                                    name.as_deref().unwrap_or("任务")
                                )),
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
        name: &str,
        working: &mut HashSet<u8>,
        last_tool_role: &mut Option<u8>,
        active_tool_roles: &HashMap<String, u8>,
    ) {
        let previous = last_tool_role.unwrap_or(1);
        if previous != role {
            self.send(OfficeAction::DeskVisit {
                visitor: previous,
                host: role,
                message: name.to_string(),
            });
        }
        self.set_working(owner, role, "working", Some(name.to_string()), working);
        *last_tool_role = Some(role);

        if previous != 1
            && previous != role
            && !active_tool_roles.values().any(|active| *active == previous)
            && working.remove(&previous)
        {
            self.release(owner, previous);
        }
    }

    /// 功能：结束一个 session 回合并释放本 owner 持有的全部 Office 角色。
    /// 参数：`owner` 为 session 所有者；`working` 为本回合角色集合；`reason` 为结束原因；
    /// `cancelled` 指示是否由用户取消。
    /// 返回：无；`working` 最终为空，不额外制造与真实任务无关的角色动作。
    /// 错误：无；Office 网络失败不影响已返回的引擎结果。
    /// 状态：逐一释放旧角色；只有最后一个 owner 释放角色时才发送 `idle`。
    /// 并发与取消：按 owner 释放，不能把其他 session 仍占用的角色置为空闲。
    /// 权限：整理消息不得包含敏感凭据或完整对话正文。
    /// 关联：`REQ-B-OFFICE-003`、`REQ-B-OFFICE-008`。
    pub fn turn_ended(
        &self,
        owner: &str,
        working: &mut HashSet<u8>,
        _reason: &str,
        _cancelled: bool,
    ) {
        for role in working.drain() {
            self.release(owner, role);
        }
    }
}

fn tool_role(name: &str) -> Option<u8> {
    match name {
        "web_search" | "web_fetch" | "web_extract" => Some(2),
        "read_file" | "grep" | "glob" => Some(3),
        "write_file" | "edit_file" | "notebook_edit" => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_engine::AgentEvent;

    fn recording_dispatcher() -> (OfficeDispatcher, mpsc::Receiver<OfficeAction>) {
        let (tx, rx) = mpsc::channel(32);
        (
            OfficeDispatcher {
                tx,
                owners: Arc::new(Mutex::new(HashMap::new())),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn tool_roles_form_a_handoff_chain_in_event_order() {
        assert_eq!(tool_role("read_file"), Some(3));
        assert_eq!(tool_role("todo_add"), None);
        assert_eq!(tool_role("unknown"), None);
        let (d, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut active_tool_roles = HashMap::new();
        d.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "1".into(),
                name: "web_search".into(),
            },
            &mut roles,
            &mut last_tool_role,
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
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "2".into(),
                name: "read_file".into(),
            },
            &mut roles,
            &mut last_tool_role,
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
            &mut active_tool_roles,
        );
        d.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "3".into(),
                name: "write_file".into(),
            },
            &mut roles,
            &mut last_tool_role,
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(4));
        assert!(!roles.contains(&2));
        assert!(!roles.contains(&3));
        assert!(roles.contains(&4));
        let visits: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
            .filter_map(|action| match action {
                OfficeAction::DeskVisit { visitor, host, .. } => Some((visitor, host)),
                _ => None,
            })
            .collect();
        assert_eq!(visits, vec![(1, 2), (2, 3), (3, 4)]);
    }

    #[tokio::test]
    async fn todo_tools_do_not_emit_office_actions() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut roles = HashSet::new();
        let mut last_tool_role = None;
        let mut active_tool_roles = HashMap::new();

        for (index, name) in ["todo_add", "todo_update", "todo_list"].iter().enumerate() {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolUseStart {
                    tool_use_id: format!("todo-{index}"),
                    name: (*name).into(),
                },
                &mut roles,
                &mut last_tool_role,
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
        let mut active_tool_roles = HashMap::new();
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "call".into(),
                name: "read_file".into(),
            },
            &mut roles,
            &mut last_tool_role,
            &mut active_tool_roles,
        );
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
            &mut active_tool_roles,
        );
        assert_eq!(last_tool_role, Some(3));
        assert!(roles.contains(&3));
        assert!(roles.contains(&6));
        let emitted: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert_eq!(
            emitted
                .iter()
                .filter(|action| matches!(action, OfficeAction::DeskVisit { .. }))
                .count(),
            1
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
        let mut active_tool_roles = HashMap::new();

        for tool_use_id in ["read-1", "read-2"] {
            dispatcher.observe(
                "session-a",
                &AgentEvent::ToolUseStart {
                    tool_use_id: tool_use_id.into(),
                    name: "read_file".into(),
                },
                &mut roles,
                &mut last_tool_role,
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
        let mut active_tool_roles = HashMap::new();

        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "web".into(),
                name: "web_search".into(),
            },
            &mut roles,
            &mut last_tool_role,
            &mut active_tool_roles,
        );
        dispatcher.observe(
            "session-a",
            &AgentEvent::ToolUseStart {
                tool_use_id: "read".into(),
                name: "read_file".into(),
            },
            &mut roles,
            &mut last_tool_role,
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
            roster_no: 1,
            state: "thinking".into(),
            task: None,
        })
        .unwrap();
        assert_eq!(state["type"], "set_state");
        assert_eq!(state["rosterNo"], 1);
        let visit = serde_json::to_value(OfficeAction::DeskVisit {
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
        let active_tool_roles = HashMap::from([("read".to_string(), 3)]);
        dispatcher.turn_started("session-a", &mut working);
        dispatcher.send_tool_working(
            "session-a",
            3,
            "read_file",
            &mut working,
            &mut last_tool_role,
            &active_tool_roles,
        );
        while actions.try_recv().is_ok() {}

        dispatcher.turn_ended("session-a", &mut working, "done", false);

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
    async fn one_session_cannot_idle_another_sessions_role() {
        let (dispatcher, mut actions) = recording_dispatcher();
        let mut first = HashSet::new();
        let mut second = HashSet::new();
        dispatcher.turn_started("session-a", &mut first);
        dispatcher.turn_started("session-b", &mut second);
        dispatcher.turn_ended("session-a", &mut first, "done", false);

        let first_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(!first_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                roster_no: 1,
                state,
                ..
            } if state == "idle"
        )));

        dispatcher.turn_ended("session-b", &mut second, "done", false);
        let second_batch: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        assert!(second_batch.iter().any(|action| matches!(
            action,
            OfficeAction::SetState {
                roster_no: 1,
                state,
                ..
            } if state == "idle"
        )));
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
