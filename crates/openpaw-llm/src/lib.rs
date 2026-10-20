//! openpaw-llm — provider implementations over the [`openpaw_core::LlmProvider`]
//! trait.
//!
//! M1 scope: [`EchoProvider`] (offline `openpaw hello` model) and
//! [`MockProvider`] (deterministic scripted provider for contract tests and
//! the future cross-replay harness). Real network providers (anthropic /
//! openai / gemini / bedrock / templates) land with M2.

pub mod anthropic;
pub mod echo;
pub mod http;
pub mod mock;
pub mod openai;
pub mod sse;

pub use anthropic::AnthropicProvider;
pub use echo::EchoProvider;
pub use mock::{MockMode, MockProvider};
pub use openai::OpenAiProvider;

/// 职责：共享运行与装配支持。
/// 输入输出：配置、生命周期到有效运行结果。
/// 依赖：复用核心类型和现有实现。
/// 禁止职责：不处理Office协议或复制存储。
pub mod assembly;
pub use assembly::{assemble_provider, resolve_model};
