//! LLM provider trait and request / response types.
//!
//! Stability: stable — the serde wire shape of the request/response types is
//! frozen at 1.0; additive fields must carry serde defaults.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::message::{ContentBlock, Message};
use crate::Result;

/// Split system prompt for multi-block prompt caching.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPromptBlocks {
    /// Stable block — cached across turns.
    pub stable: String,
    /// Dynamic block — changes each turn.
    pub dynamic: String,
}

impl SystemPromptBlocks {
    /// Merge both blocks into a single string (for providers that don't
    /// support multi-block system prompts).
    pub fn merge(&self) -> String {
        if self.stable.is_empty() && self.dynamic.is_empty() {
            String::new()
        } else if self.stable.is_empty() {
            self.dynamic.clone()
        } else if self.dynamic.is_empty() {
            self.stable.clone()
        } else {
            format!("{}\n\n{}", self.stable, self.dynamic)
        }
    }
}

/// Default max_tokens for CompletionRequest.
///
/// Stability: stable.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A chat-completion request. The serde shape is the provider wire contract.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    /// System prompt — single string, applied at the start of context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Split system prompt blocks for providers that support multi-block
    /// caching. When set, takes precedence over `system`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_blocks: Option<SystemPromptBlocks>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// If true, providers that support prompt caching should attempt to
    /// cache the system + tools + leading user turns.
    #[serde(default)]
    pub enable_caching: bool,
    #[serde(default)]
    pub enable_extended_thinking: bool,
    /// Stop sequences — generation stops when the output contains them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}

impl Default for CompletionRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            system: None,
            system_blocks: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: None,
            enable_caching: false,
            enable_extended_thinking: false,
            stop_sequences: Vec::new(),
        }
    }
}

/// Tool category for approval routing.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    ReadOnly,
    Write,
    Destructive,
    Network,
    System,
    Communication,
}

/// Concurrency safety classification.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConcurrency {
    ParallelSafe,
    PathScoped,
    NeverParallel,
}

/// Standard tool capability annotations (mirrors the MCP `Tool.annotations`
/// field). Used to infer concurrency safety and destructiveness from an
/// MCP server's declaration.
///
/// Stability: stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// true = the tool does not modify the environment; safe to parallelize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// true = the tool has destructive side effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// true = repeated calls with the same input produce the same result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// true = the tool interacts with the outside world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// Declarative interrupt behavior carried on a ToolSpec. The engine
/// converts this to its full execution policy.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInterruptBehavior {
    /// Interruptible — a cancellation token is passed to the tool.
    #[default]
    Interruptible,
    /// Block — no cancellation token; the tool cannot be interrupted.
    Block,
    /// Block with timeout — no cancellation token, but time-bounded.
    BlockWithTimeout { timeout_secs: u64 },
}

/// Tool declaration: name, description, and JSON-Schema input. The wire
/// format is `name` + `description` + `input_schema` only (internal
/// scheduling metadata is `#[serde(skip)]`).
///
/// Stability: stable.
#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool's input.
    #[serde(default)]
    pub input_schema: serde_json::Value,
    /// Approval routing category. Internal safety metadata — not sent to
    /// the LLM.
    #[serde(default, skip)]
    pub category: Option<ToolCategory>,
    /// Concurrency classification. Internal scheduling metadata.
    #[serde(default, skip)]
    pub concurrency: Option<ToolConcurrency>,
    /// Whether this tool requires explicit user approval before execution.
    #[serde(default, skip)]
    pub requires_approval: bool,
    /// Destructive operation flag (delete/overwrite/send). `None` = not
    /// declared (treated as non-destructive).
    #[serde(default, skip)]
    pub is_destructive: Option<bool>,
    /// MCP-standard capability annotations, when the tool comes from an
    /// MCP server.
    #[serde(default, skip)]
    pub annotations: Option<ToolAnnotations>,
    /// Interrupt behavior declaration. `None` = default (Interruptible).
    #[serde(default, skip)]
    pub interrupt_behavior: Option<ToolInterruptBehavior>,
    /// Max characters of tool result before truncation. `Some(0)` = never
    /// truncate. `None` = host default.
    #[serde(default, skip)]
    pub max_result_chars: Option<usize>,
    /// Deferred dynamic description — generated per input at registration
    /// time by the host, so the wire format stays a plain string.
    #[serde(skip)]
    pub description_fn: Option<DescriptionFn>,
}

/// Dynamic tool description function: generates a description for a given
/// input at call time (registered by the host, not serialized).
///
/// Stability: stable.
pub type DescriptionFn = Arc<dyn Fn(&serde_json::Value) -> String + Send + Sync>;

impl Default for ToolSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            input_schema: serde_json::Value::Object(serde_json::Map::new()),
            category: None,
            concurrency: None,
            requires_approval: false,
            is_destructive: None,
            annotations: None,
            interrupt_behavior: None,
            max_result_chars: None,
            description_fn: None,
        }
    }
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            ..Default::default()
        }
    }

    /// Whether this tool is destructive. `None` is treated as false.
    pub fn is_destructive(&self) -> bool {
        self.is_destructive.unwrap_or(false)
    }

    /// Effective interrupt behavior. `None` = Interruptible.
    pub fn interrupt_behavior(&self) -> ToolInterruptBehavior {
        self.interrupt_behavior.unwrap_or_default()
    }

    /// Effective description — prefers the dynamic description function.
    pub fn describe(&self, input: &serde_json::Value) -> String {
        if let Some(f) = &self.description_fn {
            f(input)
        } else {
            self.description.clone()
        }
    }

    /// Builder — set the dynamic description function.
    pub fn with_description_fn(mut self, f: DescriptionFn) -> Self {
        self.description_fn = Some(f);
        self
    }
}

// Manual Debug: the function-pointer field does not derive.
impl std::fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSpec")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("category", &self.category)
            .field("concurrency", &self.concurrency)
            .field("requires_approval", &self.requires_approval)
            .field("is_destructive", &self.is_destructive)
            .field("annotations", &self.annotations)
            .field("interrupt_behavior", &self.interrupt_behavior)
            .field("max_result_chars", &self.max_result_chars)
            .field(
                "description_fn",
                &self.description_fn.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

/// A chat-completion response: content blocks, stop reason, token usage.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

impl CompletionResponse {
    /// Concatenate all text blocks. Returns empty string if none.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Why generation stopped.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[default]
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Incomplete,
    Other,
}

/// Token usage for a single LLM response.
///
/// All fields are `u32` (~4.29 billion max). Accumulation uses
/// `saturating_add`, so extreme long sessions saturate rather than wrap.
///
/// Stability: stable.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_creation_tokens: u32,
}

impl Usage {
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }
}

/// 功能：报告工具、流式和模型输入模态能力；媒体默认关闭。
///
/// Stability: stable.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// 功能：显式声明模型接收图片。Stability: unstable.
    pub image_input: bool,
    /// 功能：显式声明模型接收音频。Stability: unstable.
    pub audio_input: bool,
    pub tool_use: bool,
    pub prompt_caching: bool,
    pub streaming: bool,
    pub extended_thinking: bool,
    pub context_window: u32,
    /// Maximum output tokens per response. Used by max_tokens escalation
    /// after a `MaxTokens` stop reason.
    pub max_output_tokens: u32,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            image_input: false,
            audio_input: false,
            tool_use: false,
            prompt_caching: false,
            streaming: false,
            extended_thinking: false,
            context_window: 128_000,
            max_output_tokens: 8192,
        }
    }
}

/// Heuristic: does this model id refer to an extended-thinking model?
///
/// Stability: unstable — the model-id table tracks vendor naming and is
/// expected to grow with each new model family.
pub fn is_thinking_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("claude-3-7")
        || m.starts_with("claude-4")
        || m.contains("claude-sonnet-4")
        || m.contains("claude-opus-4")
        || m.contains("reasoner")
        || m.starts_with("deepseek-r")
}

/// Abstraction over chat-completion providers.
///
/// Implementations should be cheap to clone (typically wrap an
/// `Arc` of a client plus credentials).
///
/// Stability: stable.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;

    /// Stream a completion. The default implementation falls back to
    /// [`Self::complete`] and synthesises a single [`StreamEvent::Final`]
    /// — providers without native streaming still satisfy the contract.
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let resp = self.complete(req).await?;
        let s = futures::stream::iter(vec![Ok(StreamEvent::Final(Box::new(resp)))]);
        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> Capabilities;

    /// 功能：查询实际选择模型的能力。
    /// 参数：model 为当前请求模型。
    /// 返回：能力声明；默认沿用既有能力。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：无网络。
    /// 权限：不自动更换模型。
    /// 关联：媒体接收与自动压缩。Stability: unstable.
    fn model_capabilities(&self, _model: &str) -> Capabilities {
        self.capabilities()
    }

    /// Human-readable provider tag for logging/UX, e.g. `"echo"`.
    /// Not stable as a config key.
    fn name(&self) -> &str;

    /// The default model identifier this provider was configured with.
    fn default_model(&self) -> &str;
}

/// Exact token counting capability. Not all providers support it; return
/// `None` to fall back to estimation. Kept separate from [`LlmProvider`]
/// to avoid trait bloat.
///
/// Stability: stable.
#[async_trait]
pub trait TokenCounter: Send + Sync {
    async fn count_tokens(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Option<usize>;
}

/// Events emitted by [`LlmProvider::stream`].
///
/// Contract: `MessageStart` is always first (streaming providers);
/// `TextDelta` / `ThinkingDelta` carry incremental text; `ToolUseStart`
/// declares a new tool_use block and `ToolUseInputDelta` appends its
/// partial JSON; `Final` is always last and carries the fully assembled
/// [`CompletionResponse`]. Providers MUST emit exactly one `Final`.
///
/// Stability: stable.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    MessageStart,
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolUseInputDelta {
        index: usize,
        partial_json: String,
    },
    BlockStop {
        index: usize,
    },
    Final(Box<CompletionResponse>),
}

/// A no-op provider that returns an error on every call.
///
/// Placeholder when the real provider cannot be built (e.g. missing API
/// key) so the application can still start.
///
/// Stability: stable.
pub struct NullProvider {
    reason: String,
}

impl NullProvider {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for NullProvider {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
        Err(crate::Error::Provider(format!(
            "no LLM provider configured: {reason}. Set the provider API key to continue.",
            reason = self.reason
        )))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn name(&self) -> &str {
        "null"
    }

    fn default_model(&self) -> &str {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_approval_is_skipped_in_serde() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "test".into(),
            requires_approval: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(
            !json.contains("requires_approval"),
            "requires_approval should be skipped, got: {json}"
        );
    }

    #[test]
    fn wire_format_is_name_description_schema_only() {
        let spec = ToolSpec {
            name: "grep".into(),
            description: "Search".into(),
            category: Some(ToolCategory::ReadOnly),
            is_destructive: Some(false),
            requires_approval: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["description", "input_schema", "name"]);
    }

    #[test]
    fn system_prompt_blocks_merge() {
        let blocks = SystemPromptBlocks {
            stable: "Stable prompt".to_string(),
            dynamic: "Dynamic prompt".to_string(),
        };
        assert_eq!(blocks.merge(), "Stable prompt\n\nDynamic prompt");

        let only_stable = SystemPromptBlocks {
            stable: "S".into(),
            dynamic: String::new(),
        };
        assert_eq!(only_stable.merge(), "S");

        let both_empty = SystemPromptBlocks {
            stable: String::new(),
            dynamic: String::new(),
        };
        assert!(both_empty.merge().is_empty());
    }

    #[test]
    fn is_thinking_model_matrix() {
        assert!(is_thinking_model("claude-3-7-sonnet-20250219"));
        assert!(is_thinking_model("claude-sonnet-4-20250514"));
        assert!(is_thinking_model("deepseek-reasoner"));
        assert!(!is_thinking_model("claude-3-5-sonnet-20241022"));
        assert!(!is_thinking_model("gpt-4o"));
    }

    #[test]
    fn usage_saturating_total() {
        let u = Usage {
            input_tokens: u32::MAX,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        assert_eq!(u.total_tokens(), u32::MAX);
    }

    #[tokio::test]
    async fn null_provider_errors_and_default_stream_falls_back() {
        let p = NullProvider::new("missing key");
        let err = p.complete(CompletionRequest::default()).await.unwrap_err();
        assert!(err.to_string().contains("no LLM provider configured"));

        // The default `stream` impl surfaces `complete` errors too.
        let stream = p.stream(CompletionRequest::default()).await;
        assert!(stream.is_err());
    }

    #[test]
    fn interrupt_behavior_default_is_interruptible() {
        let spec = ToolSpec::new("t", "d");
        assert_eq!(
            spec.interrupt_behavior(),
            ToolInterruptBehavior::Interruptible
        );
    }
}
