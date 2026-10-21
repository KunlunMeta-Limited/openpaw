//! 功能：声明当前会话附件回看工具；执行由会话范围 registry 提供。
//! 关联：CHG-2026-0026。
use openpaw_core::{ToolCategory, ToolConcurrency, ToolSpec};

/// 功能：生成只读附件回看工具合同。
/// 参数：无。
/// 返回：工具规格。
/// 错误：无。
/// 状态：无。
/// 并发与取消：可并行；仍经过审批。
/// 权限：只接受当前会话附件编号。
/// 关联：ToolRegistry。
pub fn spec() -> ToolSpec {
    ToolSpec { name: "read_attachment".into(), description: "Read an original image/audio attachment from the current session by attachmentID, including after context compaction.".into(),
        input_schema: serde_json::json!({"type":"object","properties":{"attachmentID":{"type":"string"}},"required":["attachmentID"],"additionalProperties":false}),
        category: Some(ToolCategory::ReadOnly), concurrency: Some(ToolConcurrency::ParallelSafe), is_destructive: Some(false), ..Default::default() }
}
