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

fn anthropic_image(media: &openpaw_core::InlineMedia) -> Result<serde_json::Value> {
    Ok(
        serde_json::json!({"type":"image", "source":{"type":"base64","media_type":media.mime,"data":media.model_data()?}}),
    )
}

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
    stream_client: reqwest::Client,
    model_capabilities: std::collections::HashMap<String, Capabilities>,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com".into(),
            default_model: default_model.into(),
            client: complete_client(),
            stream_client: stream_client(),
            model_capabilities: Default::default(),
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

    /// 功能：设置逐模型媒体声明。
    /// 参数：配置快照。
    /// 返回：Provider。
    /// 错误：无；配置阶段校验。
    /// 状态：替换能力目录。
    /// 并发与取消：构造时执行。
    /// 权限：不访问上游。
    /// 关联：ConfigSnapshot。
    pub fn with_model_capabilities(
        mut self,
        capabilities: std::collections::HashMap<String, Capabilities>,
    ) -> Self {
        self.model_capabilities = capabilities;
        self
    }

    fn build_body(&self, req: &CompletionRequest, stream: bool) -> Result<serde_json::Value> {
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

        let mut messages = Vec::new();
        for message in &req.messages {
            let mut caps = self.model_capabilities(&model);
            caps.audio_input = false;
            openpaw_core::media::validate_media(
                &openpaw_core::media::message_media(std::slice::from_ref(message)),
                caps,
            )?;
            let mut content = Vec::new();
            for block in &message.content {
                use openpaw_core::ContentBlock;
                match block {
                    ContentBlock::Image { .. } | ContentBlock::Audio { .. } => {
                        if let Some(media) = openpaw_core::media::block_media(block)? {
                            content.push(anthropic_image(&media)?);
                        }
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content: text,
                        is_error,
                        media,
                        ..
                    } => {
                        let mut parts = vec![serde_json::json!({"type":"text","text":text})];
                        for item in media {
                            parts.push(anthropic_image(item)?);
                        }
                        content.push(serde_json::json!({"type":"tool_result","tool_use_id":tool_use_id,"is_error":is_error,
                            "content":if media.is_empty() { serde_json::json!(text) } else { serde_json::json!(parts) }}));
                    }
                    ContentBlock::CompactBoundary { .. } => {}
                    _ => content.push(
                        serde_json::to_value(block)
                            .map_err(|_| Error::session("invalid content block"))?,
                    ),
                }
            }
            if !content.is_empty() {
                messages.push(serde_json::json!({"role":message.role,"content":content}));
            }
        }

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
        if body.to_string().len() > openpaw_core::media::MAX_REQUEST_BYTES {
            return Err(Error::session("model request exceeds byte limit"));
        }
        Ok(body)
    }
    async fn complete_request(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = self.build_body(&req, false)?;
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

    async fn stream_request(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let body = self.build_body(&req, true)?;
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
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let media = !openpaw_core::media::message_media(&req.messages).is_empty();
        self.complete_request(req).await.map_err(|error| {
            if media {
                openpaw_core::media::redact_error(error)
            } else {
                error
            }
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let media = !openpaw_core::media::message_media(&req.messages).is_empty();
        let stream = self.stream_request(req).await.map_err(|error| {
            if media {
                openpaw_core::media::redact_error(error)
            } else {
                error
            }
        })?;
        Ok(Box::pin(stream.map(move |result| {
            result.map_err(|error| {
                if media {
                    openpaw_core::media::redact_error(error)
                } else {
                    error
                }
            })
        })))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            image_input: false,
            audio_input: false,
            tool_use: true,
            prompt_caching: true,
            streaming: true,
            extended_thinking: true,
            context_window: 200_000,
            max_output_tokens: 64_000,
            default_output_tokens: None,
        }
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model_capabilities(&self, model: &str) -> Capabilities {
        let mut caps = self.capabilities();
        if let Some(declared) = self.model_capabilities.get(model) {
            caps.image_input = declared.image_input;
            caps.context_window = declared.context_window;
        }
        caps.audio_input = false;
        caps
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
                        media: Vec::new(),
                    }],
                    ..Default::default()
                },
            ],
            tools: vec![ToolSpec::new("read_file", "reads")],
            ..Default::default()
        };
        let body = p.build_body(&req, false).unwrap();
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
