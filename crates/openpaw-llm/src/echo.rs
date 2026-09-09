//! Echo provider — replies with the user's text. Powers `openpaw hello`
//! with zero configuration and zero network access.

use async_trait::async_trait;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message, Role,
    StopReason, Usage,
};

use openpaw_core::Result;

/// An offline provider that echoes the last user message back.
#[derive(Debug, Clone, Default)]
pub struct EchoProvider {
    model: String,
}

impl EchoProvider {
    pub fn new() -> Self {
        Self {
            model: "echo-1".to_string(),
        }
    }

    /// Extract the concatenated text of the last user message.
    pub fn last_user_text(req: &CompletionRequest) -> Option<String> {
        req.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.text())
    }
}

#[async_trait]
impl LlmProvider for EchoProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let echoed = Self::last_user_text(&req).unwrap_or_default();
        let reply = format!("Echo: {echoed}");

        // Estimate usage deterministically: ~4 chars per token.
        let input_tokens = estimate(&req);
        let output_tokens = estimate_chars(&reply);

        Ok(CompletionResponse {
            content: vec![ContentBlock::Text { text: reply }],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_use: false,
            prompt_caching: false,
            streaming: false,
            extended_thinking: false,
            context_window: 128_000,
            max_output_tokens: 4096,
        }
    }

    fn name(&self) -> &str {
        "echo"
    }

    fn default_model(&self) -> &str {
        &self.model
    }
}

fn estimate_chars(s: &str) -> u32 {
    ((s.chars().count() as f64 / 4.0).ceil() as u32).max(1)
}

fn estimate(req: &CompletionRequest) -> u32 {
    let system = req.system.as_deref().unwrap_or("");
    let msgs: u32 = req
        .messages
        .iter()
        .map(|m: &Message| estimate_chars(&m.text()))
        .sum();
    estimate_chars(system).saturating_add(msgs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_user(text: &str) -> CompletionRequest {
        CompletionRequest {
            model: "echo-1".into(),
            messages: vec![Message::user_text(text)],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn echoes_last_user_message() {
        let p = EchoProvider::new();
        let resp = p.complete(req_with_user("hello world")).await.unwrap();
        assert_eq!(resp.text(), "Echo: hello world");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert!(resp.usage.input_tokens > 0);
        assert!(resp.usage.output_tokens > 0);
    }

    #[tokio::test]
    async fn uses_latest_user_turn() {
        let mut req = req_with_user("first");
        req.messages.push(Message::assistant_text("mid"));
        req.messages.push(Message::user_text("second"));
        let resp = EchoProvider::new().complete(req).await.unwrap();
        assert_eq!(resp.text(), "Echo: second");
    }

    #[tokio::test]
    async fn empty_messages_echo_nothing() {
        let p = EchoProvider::new();
        let resp = p.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(resp.text(), "Echo: ");
    }

    #[test]
    fn provider_tags() {
        let p = EchoProvider::new();
        assert_eq!(p.name(), "echo");
        assert_eq!(p.default_model(), "echo-1");
        assert!(!p.capabilities().tool_use);
    }
}
