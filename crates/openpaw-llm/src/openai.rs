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
    /// - 旧system保持开头；显式blocks优先，dynamic在本轮真实用户文本前
    /// - tool_result不被视为新用户提问，系统上下文不拆断工具响应链
    /// - assistant tool_use → `tool_calls`
    /// - user tool_result → `role: tool` messages
    fn build_messages(&self, req: &CompletionRequest) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = Vec::new();
        let blocks = req.system_blocks.as_ref();
        let stable = blocks.map(|b| b.stable.as_str()).or(req.system.as_deref());
        if let Some(system) = stable.filter(|s| blocks.is_none() || !s.is_empty()) {
            messages.push(json!({"role": "system", "content": system}));
        }
        // 当前能力必须靠近本轮问题；旧assistant自述不是当前权限来源。
        // 只对显式blocks采用此布局，原system-only调用保持消息顺序不变。
        let dynamic = blocks.map(|b| b.dynamic.as_str()).filter(|s| !s.is_empty());
        let current_user = req.messages.iter().rposition(|m| {
            m.role == Role::User
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { .. }))
        });
        if current_user.is_none() {
            if let Some(context) = dynamic {
                messages.push(json!({"role": "system", "content": context}));
            }
        }
        for (index, m) in req.messages.iter().enumerate() {
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
                        // 混合工具结果和文本时，先输出全部tool结果，不能用system拆断调用链。
                        if current_user == Some(index) {
                            if let Some(context) = dynamic {
                                messages.push(json!({"role": "system", "content": context}));
                            }
                        }
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
        let stop_reason =
            Self::map_finish_reason(choice.get("finish_reason").and_then(|f| f.as_str()));
        if let Some(tool_calls) = message
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .filter(|_| stop_reason == StopReason::ToolUse)
        {
            for (index, tc) in tool_calls.iter().enumerate() {
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
                let input = parse_tool_arguments(args_str, index, &name)?;
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }
        Ok(CompletionResponse {
            content,
            stop_reason,
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
                            for frame in decoder.finish() {
                                state.absorb(&frame, &mut queue);
                            }
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

// 参数正文可能包含文件、密钥或个人资料；诊断只保留安全结构信息。
fn parse_tool_arguments(args: &str, index: usize, name: &str) -> Result<serde_json::Value> {
    serde_json::from_str(args).map_err(|error| {
        let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect();
        Error::Provider(format!(
            "invalid OpenAI streamed tool arguments: tool_index={index} name={name} bytes={} json={:?} line={} column={}",
            args.len(), error.classify(), error.line(), error.column()
        ))
    })
}

/// Accumulates OpenAI stream chunks: text deltas flow through; tool_calls
/// fragments accumulate by index and are emitted as one ToolUse event at
/// finish (OpenAI has no per-tool block lifecycle).
#[derive(Default)]
struct OpenAiStreamState {
    text: String,
    finalized: bool,
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
            self.finish(queue);
            return;
        }
        if data.is_null() {
            queue.push_back(Err(Error::Provider("malformed OpenAI SSE JSON".into())));
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
                self.text.push_str(text);
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
        if self.finalized {
            return;
        }
        self.finalized = true;
        // EOF不是成功终态；未收到finish_reason不能把部分文本或工具输入当完整响应。
        if self.stop_reason.is_none() {
            queue.push_back(Err(Error::Provider(
                "OpenAI stream ended without finish_reason".into(),
            )));
            return;
        }
        // Assemble accumulated tool_call fragments: emit a ToolUseStart per
        // call, and the Final response carries the assembled blocks.
        let calls: Vec<(usize, String, String, String)> = std::mem::take(&mut self.tool_calls)
            .into_iter()
            .map(|(idx, (id, name, args))| (idx, id, name, args))
            .collect();
        let mut final_content: Vec<ContentBlock> = Vec::new();
        if !self.text.is_empty() {
            final_content.push(ContentBlock::Text {
                text: self.text.clone(),
            });
        }
        // length等停止原因不能把截断的JSON误报为参数错误，也不能执行它。
        // 整批校验成功后才发布任何工具事件，避免半批事件阻止安全恢复。
        for (idx, id, name, args) in calls
            .iter()
            .filter(|_| self.stop_reason == Some(StopReason::ToolUse))
        {
            let input = match parse_tool_arguments(args, *idx, name) {
                Ok(input) => input,
                Err(error) => {
                    queue.push_back(Err(error));
                    return;
                }
            };
            final_content.push(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input,
            });
        }
        for (idx, id, name, _) in calls
            .iter()
            .filter(|_| self.stop_reason == Some(StopReason::ToolUse))
        {
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
