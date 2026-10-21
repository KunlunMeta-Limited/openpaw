//! `bash` — run a shell command in the workspace directory.
//!
//! Commands are analyzed by the crate's bash AST safety chain before they
//! enter an [`Environment`]: `Dangerous` is blocked, `RequiresApproval` is
//! refused with an approval hint, `Safe` executes through the environment —
//! [`LocalEnvironment`] (`cmd /C` on Windows, `sh -c` elsewhere) by default,
//! or a caller-provided environment such as `SandboxedEnvironment` via
//! [`run_with_env`]. Output is capped and truncated; the process is killed
//! on timeout.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

use super::environment::{Environment, LocalEnvironment};
use crate::bashast::{analyze_command_safety_for_shell, CommandSafety};
use tokio_util::sync::CancellationToken;

/// Default timeout for a single command: 120s.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Hard upper bound for a single command: 10 minutes.
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Max characters of combined output before truncation.
const MAX_OUTPUT_CHARS: usize = 30_000;

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "bash".into(),
        description: "Run a shell command in the workspace directory. Prefer specialized tools (read_file, write_file, edit_file, glob, grep) over bash for file operations — they are safer and produce fewer output tokens. Use bash for tasks those tools cannot handle (e.g. running tests, installing packages, git operations). Commands time out after 120s by default; set timeout_ms for longer runs. Do not use for file creation or editing — use write_file or edit_file instead.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute"},
                "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds (default 120000, max 600000)"}
            },
            "required": ["command"]
        }),
        category: Some(ToolCategory::Destructive),
        concurrency: Some(ToolConcurrency::NeverParallel),
        requires_approval: true,
        is_destructive: Some(true),
        ..Default::default()
    }
    .with_description_fn(Arc::new(|input: &serde_json::Value| {
        input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| format!("Running {}", truncate_chars(cmd, 60)))
            .unwrap_or_else(|| "Running".to_string())
    }))
}

pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let env = LocalEnvironment;
    run_with_env(workspace, args, &env).await
}

/// 功能：通过注入环境执行未经外部审批的命令
/// 参数：workspace为根，args为原输入，env为指定环境
/// 返回：工具结果
/// 错误：无效输入/危险或待审批命令为错误结果
/// 状态：复用调用上下文执行入口
/// 并发与取消：内部超时通知环境并等待回收
/// 权限：复杂命令没有本次批准则不能执行
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub async fn run_with_env(
    workspace: &Path,
    args: serde_json::Value,
    env: &dyn Environment,
) -> Result<ToolCallOutcome> {
    run_with_context(workspace, args, env, None, CancellationToken::new()).await
}

/// 功能：在真正执行前判断危险和调用授权
/// 参数：args为最终输入，env提供实际shell，context为可选本次批准
/// 返回：校验通过或错误工具结果
/// 错误：不解析/危险时拒绝，不调用环境
/// 状态：只读分析
/// 并发与取消：取消上下文不能批准
/// 权限：危险命令仍拒绝；批准必须精确匹配完整输入
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub(crate) fn check_call(
    args: &serde_json::Value,
    env: &dyn Environment,
    context: Option<&openpaw_core::ToolCallContext>,
) -> std::result::Result<(), ToolCallOutcome> {
    let a: Args = serde_json::from_value(args.clone())
        .map_err(|e| ToolCallOutcome::error(format!("bash: bad args: {e}")))?;
    if a.command.contains('\0') {
        return Err(ToolCallOutcome::error("bash: command contains NUL"));
    }
    let safety = analyze_command_safety_for_shell(&a.command, env.shell_type());
    match safety {
        CommandSafety::Safe => Ok(()),
        CommandSafety::Dangerous(reason) => {
            Err(ToolCallOutcome::error(format!("blocked: {reason}")))
        }
        CommandSafety::RequiresApproval(reason)
            if !context.is_some_and(|c| c.is_approved_for("bash", args)) =>
        {
            Err(ToolCallOutcome::error(format!(
                "requires approval: {reason}"
            )))
        }
        CommandSafety::RequiresApproval(_) => Ok(()),
    }
}

/// 功能：用原Environment执行本次已绑定的命令
/// 参数：workspace/args/env为原输入，context为本次授权，cancel为token
/// 返回：清理完成的工具结果
/// 错误：无效输入、危险、取消或环境失败
/// 状态：仅本次调用可能启动进程
/// 并发与取消：超时/取消通知token后等待原future完成，不drop实际进程
/// 权限：审批只覆盖本次完整JSON，不能改判危险命令
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub(crate) async fn run_with_context(
    workspace: &Path,
    args: serde_json::Value,
    env: &dyn Environment,
    context: Option<&openpaw_core::ToolCallContext>,
    cancel: CancellationToken,
) -> Result<ToolCallOutcome> {
    if let Err(outcome) = check_call(&args, env, context) {
        return Ok(outcome);
    }
    let a: Args =
        serde_json::from_value(args).map_err(|e| openpaw_core::Error::ToolHost(e.to_string()))?;
    let timeout = Duration::from_millis(a.timeout_ms.min(MAX_TIMEOUT_MS));
    let execution_cancel = cancel.child_token();
    let execution = env.exec_cancellable(&a.command, Some(workspace), execution_cancel.clone());
    tokio::pin!(execution);
    let (result, timed_out) = tokio::select! {biased;
        _=cancel.cancelled()=>{execution_cancel.cancel();let _=execution.await;return Err(openpaw_core::Error::Cancelled);},
        _=tokio::time::sleep(timeout)=>{execution_cancel.cancel();(execution.await,true)},
        result=&mut execution=>(result,false),
    };
    if timed_out {
        return Ok(ToolCallOutcome::error(format!(
            "bash timeout after {}ms; execution cleanup completed",
            timeout.as_millis()
        )));
    }
    if cancel.is_cancelled() {
        return Err(openpaw_core::Error::Cancelled);
    }
    let mut out = result.stdout;
    if !result.stderr.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("[stderr]\n");
        out.push_str(&result.stderr);
    }
    if out.is_empty() {
        out.push_str("(no output)");
    }
    // 退出原因始终可见，不能被展示截断吞掉。
    let content = format!(
        "{}\n[exit code: {}]",
        truncate_output(out),
        result.exit_code
    );
    Ok(if result.exit_code == 0 {
        ToolCallOutcome::ok(content)
    } else {
        ToolCallOutcome::error(content)
    })
}

/// Truncate a string to at most `max` characters on a char boundary.
fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Cap combined command output, appending a notice when truncated.
fn truncate_output(out: String) -> String {
    if out.chars().count() <= MAX_OUTPUT_CHARS {
        return out;
    }
    let truncated: String = out.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{truncated}\n... (output truncated at {MAX_OUTPUT_CHARS} characters)")
}
