//! 职责：提供提问声明，并拒绝未装配真实回答通道的直接调用。
//! 输入输出：问题参数到明确未支持结果；Host另行桥接真实回答。
//! 依赖：原工具声明及结果类型。
//! 禁止职责：不将串行调度或提示文字当作用户等待。

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde_json::json;

/// 功能：提供原提问参数声明，说明需要宿主真实回答通道。
/// 参数：无。
/// 返回：保留原问题、选项和上下文Schema的工具声明。
/// 错误：无。
/// 状态：不创建问题；普通注册表不自动发布该声明。
/// 并发与取消：只声明NeverParallel，不将串行调度当作等待回答。
/// 权限：真实回答桥接由Host装配，声明本身不授权执行。
/// 关联：问题桥接；需求REQ-OP-005；设计DES-OP-004；能力与错误边界修复。
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "clarify".into(),
        description: "Requires a host-provided user-answer channel; the builtin CLI implementation cannot wait for answers.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question to ask the user"},
                "options": {"type": "array", "items": {"type": "string"}, "description": "Optional suggested answers"},
                "context": {"type": "string", "description": "Brief context explaining why clarification is needed"}
            },
            "required": ["question"]
        }),
        category: Some(ToolCategory::Communication),
        concurrency: Some(ToolConcurrency::NeverParallel),
        ..Default::default()
    }
}

/// 功能：对未装配用户回答通道的直接调用返回明确失败。
/// 参数：workspace为工作区，input包含问题及可选选项。
/// 返回：缺问题或未支持的工具错误结果。
/// 错误：无宿主异常。
/// 状态：不创建问题，不接收或猜测答案。
/// 并发与取消：无等待和共享状态。
/// 权限：不能将答案或提示当作执行授权。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub async fn run(_workspace: &Path, input: serde_json::Value) -> Result<ToolCallOutcome> {
    let question = input["question"].as_str().unwrap_or("");
    if question.is_empty() {
        return Ok(ToolCallOutcome::error(
            "Error: question parameter is required",
        ));
    }

    Ok(ToolCallOutcome::error(
        "unsupported: no user-answer channel is configured; no question was presented and no answer was received. Ask the user in your reply and end the turn before doing work that depends on their answer.",
    ))
}
