//! 职责：保留旧计划模式工具入口，明确报告未实现的执行门禁。
//! 输入输出：旧工具名到错误结果。
//! 依赖：原工具结果类型。
//! 禁止职责：不以提示文字伪造只读状态，不授予执行权限。

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolSpec};
use serde_json::json;

/// 功能：保留旧计划工具的兼容声明并明确当前没有执行门禁。
/// 参数：无。
/// 返回：进入和退出两个旧工具声明，描述为未支持。
/// 错误：无。
/// 状态：无模式状态；普通注册表不自动发布这些声明。
/// 并发与取消：纯构造，无等待。
/// 权限：不能依据该声明推断后续工具为只读。
/// 关联：能力声明修复；需求REQ-OP-005；设计DES-OP-004；能力与错误边界修复。
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "enter_plan_mode".into(),
            description:
                "Unsupported: no enforced read-only plan mode is installed by this tool host."
                    .into(),
            input_schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        },
        ToolSpec {
            name: "exit_plan_mode".into(),
            description: "Unsupported: this tool host has no plan-mode state to exit.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "enter_plan_mode" | "exit_plan_mode")
}

/// 功能：拒绝尚未接入执行门禁的旧计划模式调用。
/// 参数：workspace为原工作区，name为工具名称。
/// 返回：已知工具返回明确的错误结果。
/// 错误：未知名称返回工具宿主错误。
/// 状态：不创建或改变运行模式。
/// 并发与取消：无等待或共享状态。
/// 权限：不宣称已限制后续工具，不授予执行权限。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub async fn run(_workspace: &Path, name: &str) -> Result<ToolCallOutcome> {
    match name {
        "enter_plan_mode" | "exit_plan_mode" => Ok(ToolCallOutcome::error(
            "unsupported: enforced plan mode is not implemented; no mode transition occurred. Do not assume subsequent file or command tools are read-only.",
        )),
        other => Err(openpaw_core::Error::ToolHost(format!(
            "unknown plan tool: {other}"
        ))),
    }
}
