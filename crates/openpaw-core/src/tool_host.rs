//! Tool host abstractions — the agent loop's execution surface.
//!
//! Stability: stable.

use async_trait::async_trait;

use crate::provider::ToolSpec;
use crate::Result;

/// Result of a single tool invocation, ready to be fed back into the LLM
/// transcript as a `tool_result` block.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallOutcome {
    /// Plain-text content for the `tool_result` block. Non-text content is
    /// summarised inline (e.g. `[image: 12 KB png]`).
    pub content: String,
    pub is_error: bool,
    /// Optional structured result for downstream consumers (sub-agents,
    /// harness assertions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
}

impl ToolCallOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            structured: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            structured: None,
        }
    }
}

use serde::{Deserialize, Serialize};

/// 用途：复用工具枚举与执行
/// 字段：调用接口
/// 来源去向：引擎到ToolHost
/// 约束：取消后必须等待实际结束
/// 稳定性：新增默认方法兼容旧实现
/// 关联：需求/设计 REQ-OP-003。
///
/// Stability: stable.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>>;
    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome>;

    /// 功能：通知工具取消并等待调用实际清理完成，兼容旧执行器。
    /// 参数：name、args为本次工具输入，cancel为本次调用取消信号。
    /// 返回：实际调用结果；取消前未启动则返回取消错误。
    /// 错误：透传工具错误，不自动重试副作用。
    /// 状态：默认实现仍等待旧call返回，受控执行器应覆盖并杀树回收。
    /// 并发与取消：调用方必须等待此future，不能drop后宣称已清理；旧实现不保证及时中断。
    /// 权限：不提供授权，不改变既有安全检查。
    /// 关联：需求/设计 REQ-OP-003；DES-OP-002。
    async fn call_cancellable(
        &self,
        name: &str,
        args: serde_json::Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolCallOutcome> {
        if cancel.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        self.call(name, args).await
    }
}

/// Input validation runs before guardrails; failures short-circuit the
/// tool call with an error result.
///
/// Stability: stable.
#[async_trait]
pub trait ToolHostValidator: Send + Sync {
    async fn validate_input(
        &self,
        _name: &str,
        _input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        Ok(())
    }

    /// Whether a tool error should cascade-cancel other in-flight tools of
    /// the same batch. Typical: bash failures.
    fn should_cascade_on_error(&self, _tool_name: &str, _outcome: &ToolCallOutcome) -> bool {
        false
    }
}

/// The aggregate surface the agent loop consumes. Sub-traits grow with E2
/// (truncation, caching, alias resolution) — the blanket impl keeps the
/// loop code stable.
///
/// Stability: stable.
pub trait ToolHost: ToolExecutor + ToolHostValidator {}

impl<T> ToolHost for T where T: ToolExecutor + ToolHostValidator {}

/// A tool host that exposes nothing. The loop still works — just without
/// tools (pure conversation turns).
///
/// Stability: stable.
pub struct NullToolHost;

#[async_trait]
impl ToolExecutor for NullToolHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(Vec::new())
    }

    async fn call(&self, name: &str, _: serde_json::Value) -> Result<ToolCallOutcome> {
        Err(crate::Error::ToolHost(format!(
            "no tool host configured; cannot call {name:?}"
        )))
    }
}

#[async_trait]
impl ToolHostValidator for NullToolHost {}

/// In-memory registry host for tests and small deployments: holds static
/// specs and a dispatch closure.
///
/// Stability: stable.
pub struct RegistryToolHost<F>
where
    F: Fn(&str, serde_json::Value) -> Result<ToolCallOutcome> + Send + Sync,
{
    pub specs: Vec<ToolSpec>,
    pub dispatch: F,
}

#[async_trait]
impl<F> ToolExecutor for RegistryToolHost<F>
where
    F: Fn(&str, serde_json::Value) -> Result<ToolCallOutcome> + Send + Sync,
{
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self.specs.clone())
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome> {
        (self.dispatch)(name, args)
    }
}

#[async_trait]
impl<F> ToolHostValidator for RegistryToolHost<F> where
    F: Fn(&str, serde_json::Value) -> Result<ToolCallOutcome> + Send + Sync
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn null_host_lists_nothing_and_errors_on_call() {
        let h = NullToolHost;
        assert!(h.list_tools().await.unwrap().is_empty());
        let err = h.call("bash", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("no tool host configured"));
    }

    #[tokio::test]
    async fn registry_host_dispatches() {
        let host = RegistryToolHost {
            specs: vec![ToolSpec::new("echo_tool", "echoes input")],
            dispatch: |name, args| {
                if name == "echo_tool" {
                    Ok(ToolCallOutcome::ok(format!(
                        "echo: {}",
                        args.get("text").and_then(|v| v.as_str()).unwrap_or("")
                    )))
                } else {
                    Err(crate::Error::ToolHost(format!("unknown tool {name}")))
                }
            },
        };
        let out = host.call("echo_tool", json!({"text": "hi"})).await.unwrap();
        assert_eq!(out.content, "echo: hi");
        assert!(!out.is_error);
        assert!(host.validate_input("echo_tool", &json!({})).await.is_ok());
    }
}
