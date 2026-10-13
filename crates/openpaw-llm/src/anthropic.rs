//! Anthropic Messages API provider (`/v1/messages`).
//!
//! Our `ContentBlock`/`Usage`/`StopReason` serde shapes mirror the
//! Anthropic wire format, so non-streaming responses deserialize directly;
//! streaming goes through the unified SSE assembler.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, Result, StreamEvent,
};

use crate::http::{classify_http_error, complete_client, stream_client};
use crate::sse::{AnthropicStreamAssembler, SseDecoder};

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
    stream_client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com".into(),
            default_model: default_model.into(),
            client: complete_client(),
            stream_client: stream_client(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }

    fn build_body(&self, req: &CompletionRequest, stream: bool) -> serde_json::Value {
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        };
        let max_tokens = if req.max_tokens > 0 {
            req.max_tokens
        } else {
            4096
        };

        // Messages: pass core Messages through; the serde shapes match the
        // Anthropic wire format. ToolResult blocks ride on user messages.
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
            .collect();

        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": messages,
            "stream": stream,
        });
        if let Some(system) = &req.system {
            body["system"] = serde_json::json!(system);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = self.build_body(&req, false);
        let url = self.endpoint();
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("http send: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read body: {e}>"));
            return Err(classify_http_error(status.as_u16(), &body, "anthropic"));
        }

        let parsed: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| Error::Provider(format!("decoding response: {e}")))?;
        Ok(parsed)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let body = self.build_body(&req, true);
        let url = self.endpoint();
        let resp = self
            .stream_client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("http send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read body: {e}>"));
            return Err(classify_http_error(status, &body, "anthropic"));
        }

        let stream = resp
            .bytes_stream()
            .map_err(|e| Error::Provider(format!("stream read: {e}")));
        let state = (
            SseDecoder::new(),
            AnthropicStreamAssembler::new(),
            stream,
            std::collections::VecDeque::new(),
            false, // upstream exhausted → finalize emitted
        );
        Ok(Box::pin(futures::stream::unfold(
            state,
            |(mut decoder, mut assembler, mut stream, mut queue, mut finished)| async move {
                loop {
                    if let Some(ev) = queue.pop_front() {
                        return Some((ev, (decoder, assembler, stream, queue, finished)));
                    }
                    if finished {
                        return None;
                    }
                    match stream.next().await {
                        Some(Ok(bytes)) => match decoder.feed(&bytes) {
                            Ok(frames) => {
                                for frame in frames {
                                    match assembler.absorb(&frame) {
                                        Ok(events) => queue.extend(events.into_iter().map(Ok)),
                                        Err(error) => {
                                            queue.push_back(Err(error));
                                            finished = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                queue.push_back(Err(error));
                                finished = true;
                            }
                        },
                        Some(Err(e)) => {
                            queue.push_back(Err(e));
                            finished = true; // don't finalize after a read error
                        }
                        None => {
                            match decoder.finish() {
                                Ok(frames) => {
                                    for frame in frames {
                                        match assembler.absorb(&frame) {
                                            Ok(events) => queue.extend(events.into_iter().map(Ok)),
                                            Err(error) => {
                                                queue.push_back(Err(error));
                                                finished = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    queue.push_back(Err(error));
                                    finished = true;
                                }
                            }
                            if !finished {
                                match assembler.finalize() {
                                    Ok(resp) => {
                                        queue.push_back(Ok(StreamEvent::Final(Box::new(resp))));
                                    }
                                    Err(error) => queue.push_back(Err(error)),
                                }
                            }
                            finished = true;
                        }
                    }
                }
            },
        )))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_use: true,
            prompt_caching: true,
            streaming: true,
            extended_thinking: true,
            context_window: 200_000,
            max_output_tokens: 64_000,
        }
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::{ContentBlock, Message, Role, ToolSpec};
    use serde_json::json;

    #[test]
    fn body_shape_matches_wire_format() {
        let p = AnthropicProvider::new("sk-test", "claude-sonnet-4");
        let req = CompletionRequest {
            model: "claude-sonnet-4".into(),
            system: Some("be brief".into()),
            max_tokens: 100,
            messages: vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a.rs"}),
                    }],
                    ..Default::default()
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "contents".into(),
                        is_error: false,
                        name: None,
                        protected: false,
                    }],
                    ..Default::default()
                },
            ],
            tools: vec![ToolSpec::new("read_file", "reads")],
            ..Default::default()
        };
        let body = p.build_body(&req, false);
        assert_eq!(body["model"], json!("claude-sonnet-4"));
        assert_eq!(body["max_tokens"], json!(100));
        assert_eq!(body["system"], json!("be brief"));
        assert_eq!(body["stream"], json!(false));
        assert_eq!(body["tools"][0]["name"], json!("read_file"));
        assert!(body["tools"][0]["input_schema"].is_object());

        let msgs = body["messages"].as_array().unwrap();
        // tool_result rides on a user message with the anthropic tag shape.
        let tr = &msgs[2]["content"][0];
        assert_eq!(tr["type"], json!("tool_result"));
        assert_eq!(tr["tool_use_id"], json!("t1"));
    }
}
