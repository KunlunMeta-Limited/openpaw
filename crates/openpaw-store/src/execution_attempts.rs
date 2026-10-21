//! 功能：校验并回放独立于成功模型上下文的执行尝试事实。
//! 边界：只消费同一会话 journal；不执行工具、不写盘、不发布事件。
use crate::transcript::ProjectionError;
use serde::{Deserialize, Serialize};

/// 功能：单个已可靠提交的执行事实；Unknown 类型固定为 execution_attempt_v1。
#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutionAttemptV1 {
    /// 所属回合；根、子会话各自 journal 保存，不复制父记录。
    pub turn_id: String,
    /// 本回合严格递增，从一开始。
    pub sequence: u64,
    /// 模型响应编号，与 SSE 消息编号一致。
    pub response_index: usize,
    /// 事实发生时间，Unix 毫秒。
    pub occurred_at: i64,
    /// response_start/text/response/tool_start/tool_result。
    pub kind: String,
    /// 事实完整内容；仅授权历史读取，不用于诊断和动作消息。
    pub payload: serde_json::Value,
}

/// 功能：校验并追加一条版本化事实。
/// 参数：records 为同会话已回放事实，record 为待追加事实。
/// 返回：成功为空。
/// 错误：字段非法、序号错位、工具身份错配或矛盾结果时返回脱敏错误。
/// 状态：只在校验通过后修改集合。
/// 并发与取消：同步；调用方串行维护 journal 次序。
/// 权限：不读取外部资源，不执行记录内工具。
/// 关联：CHG-2026-0039、Transcript 独立尝试投影。
pub fn apply(
    records: &mut Vec<ExecutionAttemptV1>,
    record: ExecutionAttemptV1,
) -> Result<(), ProjectionError> {
    validate(records, &record)?;
    records.push(record);
    Ok(())
}

/// 功能：只借用既有事实校验下一条记录，避免克隆历史正文。
/// 参数：records 为当前事实快照，record 为待提交事实。
/// 返回：成功为空。
/// 错误：已登记结构或调用因果关系非法时返回脱敏错误。
/// 状态：不修改记录；调用方可靠写入成功后才追加。
/// 并发与取消：同步短期借用，调用方保证写入串行。
/// 权限：不访问外部资源、不执行工具。
/// 关联：CHG-2026-0039、Host 独占 writer。
pub fn validate(
    records: &[ExecutionAttemptV1],
    record: &ExecutionAttemptV1,
) -> Result<(), ProjectionError> {
    let prior: Vec<_> = records
        .iter()
        .filter(|r| r.turn_id == record.turn_id)
        .collect();
    if record.turn_id.is_empty()
        || record.sequence != prior.last().map_or(1, |r| r.sequence + 1)
        || !record.payload.is_object()
    {
        return Err(ProjectionError);
    }
    let same_response = |r: &&ExecutionAttemptV1| r.response_index == record.response_index;
    let has_response = prior
        .iter()
        .any(|r| same_response(r) && r.kind == "response_start");
    match record.kind.as_str() {
        "response_start"
            if !has_response
                && record.response_index
                    == prior.iter().filter(|r| r.kind == "response_start").count() => {}
        "text" | "reasoning" if has_response && record.payload["text"].is_string() => {}
        "response"
            if has_response
                && !prior
                    .iter()
                    .any(|r| same_response(r) && r.kind == "response") =>
        {
            let _: Vec<openpaw_core::ContentBlock> =
                serde_json::from_value(record.payload["content"].clone())
                    .map_err(|_| ProjectionError)?;
        }
        "tool_start" | "tool_result" if has_response => {
            let call = record.payload["call_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or(ProjectionError)?;
            let name = record.payload["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or(ProjectionError)?;
            let requested = prior
                .iter()
                .filter(|r| same_response(r) && r.kind == "response")
                .any(|r| {
                    r.payload["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|b| b["id"] == call && b["name"] == name)
                });
            let same_call =
                |r: &&ExecutionAttemptV1| same_response(r) && r.payload["call_id"] == call;
            if !requested
                || prior
                    .iter()
                    .any(|r| same_call(r) && r.kind == "tool_result")
            {
                return Err(ProjectionError);
            }
            if record.kind == "tool_start" {
                if prior.iter().any(|r| same_call(r) && r.kind == "tool_start")
                    || record.payload.get("input").is_none()
                {
                    return Err(ProjectionError);
                }
            } else {
                let disposition = record.payload["disposition"]
                    .as_str()
                    .ok_or(ProjectionError)?;
                if !matches!(
                    disposition,
                    "completed" | "error" | "denied" | "cancelled" | "unknown"
                ) {
                    return Err(ProjectionError);
                }
                let started = prior.iter().any(|r| same_call(r) && r.kind == "tool_start");
                if matches!(disposition, "completed" | "error") && !started
                    || matches!(disposition, "denied" | "cancelled") && started
                {
                    return Err(ProjectionError);
                }
                let _: openpaw_core::ToolCallOutcome =
                    serde_json::from_value(record.payload["outcome"].clone())
                        .map_err(|_| ProjectionError)?;
            }
        }
        _ => return Err(ProjectionError),
    }
    Ok(())
}
