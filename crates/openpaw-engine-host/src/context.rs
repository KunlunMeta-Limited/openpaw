//! 功能：将当前会话的可靠原件接到中立Engine分页工具。
//! 边界：只读该会话journal，按可见输入验证引用；不创建writer或执行工具。
//! 关联：CHG-2026-0051。
use openpaw_core::ToolHost;
use openpaw_engine::context::{wrap_tools, OriginalReader};
use std::{path::Path, sync::Arc};

/// 功能：保存一次真实子执行与确切父调用的可靠来源，不携带执行许可。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildExecutionRecord {
    /// 功能：真实父会话。
    pub(crate) parent_session_id: String,
    /// 功能：本次父回合。
    pub(crate) parent_turn_id: String,
    /// 功能：本次父输入，不等于员工创建来源。
    pub(crate) parent_user_message_id: String,
    /// 功能：启动工具所在模型响应。
    pub(crate) response_index: usize,
    /// 功能：引擎确定的启动调用。
    pub(crate) tool_call_id: String,
    /// 功能：真实启动或同步委派工具名。
    pub(crate) tool_name: String,
    /// 功能：真实子会话。
    pub(crate) child_session_id: String,
    /// 功能：本次子回合。
    pub(crate) child_turn_id: String,
    /// 功能：本次子输入。
    pub(crate) child_user_message_id: String,
    /// 功能：accepted或result_saved；受理不表示完成。
    pub(crate) stage: String,
    /// 功能：可靠保存时间，Unix毫秒。
    pub(crate) occurred_at: i64,
    /// 功能：实际完成、失败、取消或暂停，仅保存结果时存在。
    pub(crate) status: Option<String>,
    /// 功能：完整原始子结果，不是父模型预览或验收。
    pub(crate) outcome: Option<openpaw_core::ToolCallOutcome>,
}

/// 功能：解析同一journal内版本化子执行记录，归属由可靠事实投影核验。
/// 参数：events为原owner已可靠读取的事件。
/// 返回：按保存顺序排列的记录。
/// 错误：已知版本结构非法时拒绝，不降级为成功。
/// 状态：只读，不产生执行许可。
/// 并发与取消：同步处理独立快照。
/// 权限：解析不代替父子来源与可见性校验。
/// 关联：runtime保存、重启及可靠原件投影。
pub(crate) fn child_executions(
    events: &[openpaw_core::SessionEvent],
) -> Result<Vec<ChildExecutionRecord>, String> {
    events
        .iter()
        .filter_map(|event| match event {
            openpaw_core::SessionEvent::Unknown(value) if value["type"] == "child_execution_v1" => {
                Some(
                    serde_json::from_value(value["record"].clone())
                        .map_err(|_| "子执行记录结构无效".into()),
                )
            }
            _ => None,
        })
        .collect()
}

/// 功能：为当前会话构造授权原件工具宿主。
/// 参数：inner为原宿主，home/id为Host已拥有的会话位置，window为模型实际窗口。
/// 返回：携带会话范围读取器的宿主。
/// 错误：原件不存在、隐藏或跨会话时由工具明确返回错误。
/// 状态：组装时不读盘，不创建写入者。
/// 并发与取消：每次读取新journal快照，不持有会话异步锁。
/// 权限：仅访问id所属会话及其当前可见记录。
/// 关联：runtime::session_tool_host。
pub(crate) fn wrap(
    inner: Arc<dyn ToolHost>,
    home: &Path,
    id: &str,
    window: usize,
) -> Arc<dyn ToolHost> {
    let path = home.join("sessions").join(format!("{id}.jsonl"));
    let owner = id.to_owned();
    let reader: OriginalReader = Arc::new(move |reference| {
        let key = openpaw_engine::context::parse_reference(reference, &owner)?;
        let read = openpaw_store::SessionReader::read(&path).map_err(|_| "原件记录不可读")?;
        let history =
            openpaw_store::transcript::replay(&owner, &read.events).map_err(|_| "原件历史无效")?;
        let visible = history.visible_messages(&owner);
        let visible_turn = |turn: &str| {
            history.turn_outcomes.iter().any(|t| {
                t.turn_id == turn
                    && visible
                        .iter()
                        .any(|m| m.uuid.as_deref() == Some(t.user_message_id.as_str()))
            })
        };
        let turn = key.turn.as_str();
        if let Some(goal) = turn.strip_prefix("goal/") {
            let (source_turn, text, is_error) = crate::goal::GoalContext::original_observation(
                &read.events,
                &owner,
                goal,
                &key.call,
            )?;
            if !visible_turn(&source_turn) {
                return Err("Goal原件不存在或已隐藏".into());
            }
            return Ok((text, is_error));
        }
        if turn.starts_with("message/") {
            return openpaw_engine::context::message_original(&key, visible);
        }
        if !visible_turn(turn) {
            return Err("原件不存在或已隐藏".into());
        }
        history
            .tool_original(turn, key.response, &key.call)
            .ok_or_else(|| "原件不存在或未保存".into())
    });
    wrap_tools(inner, reader, window)
}
