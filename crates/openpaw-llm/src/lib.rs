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
