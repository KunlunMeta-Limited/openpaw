//! 功能：将当前会话的可靠原件接到中立Engine分页工具。
//! 边界：只读该会话journal，按可见输入验证引用；不创建writer或执行工具。
//! 关联：CHG-2026-0051。
use openpaw_core::ToolHost;
use openpaw_engine::context::{wrap_tools, OriginalReader};
use std::{path::Path, sync::Arc};

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
