//! Tool host abstractions — the agent loop's execution surface.
//!
//! Stability: stable.

use async_trait::async_trait;

use crate::provider::ToolSpec;
use crate::Result;

/// 功能：关联引擎已经接受的实际工具调用，不包含传输地址或授权。Stability: unstable.
#[derive(Debug, Clone, Default)]
pub struct ToolInvocationContext {
    /// 功能：入口提供的原回合身份；旧入口可以没有。
    pub turn_id: Option<String>,
    /// 功能：该回合中的模型响应序号。
    pub response_index: usize,
    /// 功能：本次调用在模型响应中的确切身份。
    pub tool_call_id: String,
}

/// 功能：保存单次工具执行的文字、结构化结果及内联媒体，供模型续轮使用。
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallOutcome {
    /// 功能：与文本结果关联的内联附件。Stability: unstable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<crate::InlineMedia>,
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
    /// 功能：构建成功的纯文本工具结果。
    /// 参数：content 为正文。
    /// 返回：媒体列表为空的成功结果。
    /// 错误：无。
    /// 状态：无。
    /// 并发与取消：同步。
    /// 权限：无。
    /// 关联：富结果可随后添加 media。Stability: stable.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            structured: None,
            media: Vec::new(),
        }
    }

    /// 功能：构建不携带媒体的工具失败结果。
    /// 参数：content 为可展示错误。
    /// 返回：错误结果。
    /// 错误：无。
    /// 状态：无。
    /// 并发与取消：同步。
    /// 权限：调用者应过滤敏感内容。
    /// 关联：AgentLoop。Stability: stable.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            structured: None,
            media: Vec::new(),
        }
    }
}

use serde::{Deserialize, Serialize};

/// Core tool execution trait: enumerate + call.
///
/// Stability: stable.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>>;
    async fn call(&self, name: &str, args: serde_json::Value) -> Result<ToolCallOutcome>;

    /// 功能：携带引擎确定的身份调用工具，旧宿主默认沿用原入口。
    /// 参数：name、args为已校验调用；context为可信执行身份。
    /// 返回：原工具结果。
    /// 错误：沿用call的错误。
    /// 状态：默认实现不保存上下文。
    /// 并发与取消：沿用原工具语义。
    /// 权限：上下文不授予权限，不接受模型伪造身份。
    /// 关联：用户提问及实际调用记录。Stability: unstable.
    async fn call_with_context(
        &self,
        name: &str,
        args: serde_json::Value,
        _context: &ToolInvocationContext,
    ) -> Result<ToolCallOutcome> {
        self.call(name, args).await
    }

    /// 功能：安装当前回合可见附件。
    /// 参数：经过有效历史边界裁剪的原件。
    /// 返回：无。
    /// 错误：无。
    /// 状态：替换会话范围的临时查找表。
    /// 并发与取消：调用者独占回合；结束必须清空。
    /// 权限：不授予其他会话读取权。
    /// 关联：read_attachment。Stability: unstable.
    fn set_media_context(&self, _media: Vec<crate::InlineMedia>) {}

    /// 功能：发布经过 Hook 和预算校验的本回合附件。
    /// 参数：合法工具结果原件。
    /// 返回：无。
    /// 错误：无。
    /// 状态：加入临时查找表。
    /// 并发与取消：回合结束调用 set_media_context 清空。
    /// 权限：仅当前会话。
    /// 关联：Engine 工具续轮。Stability: unstable.
    fn remember_media(&self, _media: Vec<crate::InlineMedia>) {}
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
