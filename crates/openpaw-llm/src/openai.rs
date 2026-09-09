//! OpenAI Chat Completions provider (`/v1/chat/completions`), with a
//! configurable base URL so OpenAI-compatible endpoints (self-hosted
//! servers, template providers) work through the same implementation.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, Error, LlmProvider, Result,
    Role, StopReason, StreamEvent, Usage,
};
use serde_json::json;

use crate::http::{classify_http_error, complete_client, stream_client};

pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    default_model: String,
    client: reqwest::Client,
    stream_client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, default_model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: default_model.into(),
            client: complete_client(),
            stream_client: stream_client(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Map core messages onto the OpenAI chat shape:
    /// - system prompt → leading `system` message
    /// - assistant tool_use → `tool_calls`
    /// - user tool_result → `role: tool` messages
    fn build_messages(&self, req: &CompletionRequest) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = Vec::new();
        if let Some(system) = &req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        for m in &req.messages {
            match m.role {
                Role::User => {
                    // Split tool_result blocks into `tool` messages; text
                    // stays a normal user message.
                    let mut text_parts: Vec<String> = Vec::new();
                    for block in &m.content {
                        match block {
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                }));
                            }
                            ContentBlock::Text { text } => text_parts.push(text.clone()),
                            _ => {}
                        }
                    }
                    if !text_parts.is_empty() {
                        messages.push(json!({"role": "user", "content": text_parts.join("\n")}));
                    }
                }
                Role::Assistant => {
                    let mut text_parts: Vec<String> = Vec::new();
                    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                    for block in &m.content {
                        match block {
                            ContentBlock::Text { text } => text_parts.push(text.clone()),
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": serde_json::to_string(input).unwrap_or_default(),
                                    },
                                }));
                            }
                            _ => {}
                        }
                    }
                    let mut msg = json!({"role": "assistant"});
                    if !text_parts.is_empty() {
                        msg["content"] = json!(text_parts.join("\n"));
                    }
                    if !tool_calls.is_empty() {
                        msg["tool_calls"] = json!(tool_calls);
                    }
                    messages.push(msg);
                }
            }
        }
        messages
    }

    fn build_body(&self, req: &CompletionRequest, stream: bool) -> serde_json::Value {
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        };
        let mut body = json!({
            "model": model,
            "messages": self.build_messages(req),
            "max_tokens": if req.max_tokens > 0 { req.max_tokens } else { 4096 },
            "stream": stream,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            let tools: Vec<serde_json::Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        },
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }
        body
    }

    fn parse_usage(u: Option<&serde_json::Value>) -> Usage {
        match u {
            Some(u) => Usage {
                input_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                output_tokens: u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
                ..Default::default()
            },
            None => Usage::default(),
        }
    }

    fn map_finish_reason(fr: Option<&str>) -> StopReason {
        match fr {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("stop") => StopReason::EndTurn,
            _ => StopReason::Other,
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = self.build_body(&req, false);
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
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
            return Err(classify_http_error(status.as_u16(), &body, "openai"));
        }

        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Provider(format!("decoding response: {e}")))?;

        let choice = v
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| Error::Provider("openai: no choices in response".into()))?;
        let message = choice.get("message").cloned().unwrap_or(json!({}));
        let mut content: Vec<ContentBlock> = Vec::new();
        if let Some(text) = message.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text: text.into() });
            }
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let function = tc.get("function").cloned().unwrap_or(json!({}));
                let name = function
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_str = function
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                let input: serde_json::Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }
        Ok(CompletionResponse {
            content,
            stop_reason: Self::map_finish_reason(
                choice.get("finish_reason").and_then(|f| f.as_str()),
            ),
            usage: Self::parse_usage(v.get("usage")),
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let body = self.build_body(&req, true);
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .stream_client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
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
            return Err(classify_http_error(status, &body, "openai"));
        }

        let stream = resp
            .bytes_stream()
            .map_err(|e| Error::Provider(format!("stream read: {e}")));

        // Chunk assembler state: accumulating tool_calls by index.
        let state = (
            crate::sse::SseDecoder::new(),
            stream,
            std::collections::VecDeque::new(),
            OpenAiStreamState::default(),
            false,
        );
        Ok(Box::pin(futures::stream::unfold(
            state,
            |(mut decoder, mut stream, mut queue, mut state, mut finished)| async move {
                loop {
                    if let Some(ev) = queue.pop_front() {
                        return Some((ev, (decoder, stream, queue, state, finished)));
                    }
                    if finished {
                        return None;
                    }
                    match stream.next().await {
                        Some(Ok(bytes)) => {
                            for frame in decoder.feed(&bytes) {
                                state.absorb(&frame, &mut queue);
                            }
                        }
                        Some(Err(e)) => {
                            queue.push_back(Err(e));
                            finished = true;
                        }
                        None => {
                            state.finish(&mut queue);
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
            streaming: true,
            context_window: 128_000,
            ..Capabilities::default()
        }
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }
}

/// Accumulates OpenAI stream chunks: text deltas flow through; tool_calls
/// fragments accumulate by index and are emitted as one ToolUse event at
/// finish (OpenAI has no per-tool block lifecycle).
#[derive(Default)]
struct OpenAiStreamState {
    tool_calls: std::collections::BTreeMap<usize, (String, String, String)>, // id, name, args
    usage: Usage,
    stop_reason: Option<StopReason>,
}

impl OpenAiStreamState {
    fn absorb(
        &mut self,
        frame: &crate::sse::Frame,
        queue: &mut std::collections::VecDeque<openpaw_core::Result<StreamEvent>>,
    ) {
        let data = &frame.data;
        if data.as_str() == Some("[DONE]") {
            return;
        }
        if let Some(u) = data.get("usage") {
            self.usage = OpenAiProvider::parse_usage(Some(u));
        }
        let Some(choice) = data.get("choices").and_then(|c| c.get(0)) else {
            return;
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));
        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                queue.push_back(Ok(StreamEvent::TextDelta {
                    index: 0,
                    text: text.to_string(),
                }));
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let entry = self.tool_calls.entry(idx).or_insert_with(|| {
                    (
                        tc.get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        String::new(),
                        String::new(),
                    )
                });
                if let Some(name) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                {
                    entry.1.push_str(name);
                }
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    entry.2.push_str(args);
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.stop_reason = Some(OpenAiProvider::map_finish_reason(Some(fr)));
        }
    }

    fn finish(
        &mut self,
        queue: &mut std::collections::VecDeque<openpaw_core::Result<StreamEvent>>,
    ) {
        // Assemble accumulated tool_call fragments: emit a ToolUseStart per
        // call, and the Final response carries the assembled blocks.
        let calls: Vec<(usize, String, String, String)> = std::mem::take(&mut self.tool_calls)
            .into_iter()
            .map(|(idx, (id, name, args))| (idx, id, name, args))
            .collect();
        let mut final_content: Vec<ContentBlock> = Vec::new();
        for (idx, id, name, args) in &calls {
            let input: serde_json::Value = serde_json::from_str(args).unwrap_or(json!({}));
            final_content.push(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input,
            });
            queue.push_back(Ok(StreamEvent::ToolUseStart {
                index: *idx,
                id: id.clone(),
                name: name.clone(),
            }));
        }
        queue.push_back(Ok(StreamEvent::Final(Box::new(CompletionResponse {
            content: final_content,
            stop_reason: self.stop_reason.unwrap_or(StopReason::Other),
            usage: self.usage,
        }))));
    }
}
