//! openpaw-llm — provider implementations over the [`openpaw_core::LlmProvider`]
//! trait.
//!
//! M1 scope: [`EchoProvider`] (offline `openpaw hello` model) and
//! [`MockProvider`] (deterministic scripted provider for contract tests and
//! the future cross-replay harness). Real network providers (anthropic /
//! openai / gemini / bedrock / templates) land with M2.

pub mod anthropic;
/// 功能：导出共享 config 能力。
/// 参数：无。
/// 返回：模块接口。
/// 错误：由模块接口说明。
/// 状态：无独立状态。
/// 并发与取消：由模块接口说明。
/// 权限：保持既有边界。
/// 关联：CHG-2026-0023。
pub mod config;
pub mod echo;
pub mod http;
pub mod mock;
pub mod openai;
pub mod sse;

pub use anthropic::AnthropicProvider;
pub use echo::EchoProvider;
pub use mock::{MockMode, MockProvider};
pub use openai::OpenAiProvider;
