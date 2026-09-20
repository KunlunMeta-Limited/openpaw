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
use crate::sse::MAX_STREAM_ACCUMULATED_BYTES;

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
    fn build_messages(
        &self,
        req: &CompletionRequest,
        include_reasoning: bool,
    ) -> Vec<serde_json::Value> {
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
                    let mut reasoning_parts: Vec<String> = Vec::new();
                    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                    for block in &m.content {
                        match block {
                            ContentBlock::Text { text } => text_parts.push(text.clone()),
                            ContentBlock::Thinking { thinking, .. } => {
                                reasoning_parts.push(thinking.clone())
                            }
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
                    if include_reasoning && !reasoning_parts.is_empty() {
                        msg["reasoning_content"] = json!(reasoning_parts.join("\n"));
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
        let deepseek_dialect = model.to_ascii_lowercase().starts_with("deepseek-")
            || self.base_url.to_ascii_lowercase().contains("deepseek");
        let mut body = json!({
            "model": model,
            "messages": self.build_messages(req, deepseek_dialect),
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

    fn map_finish_reason(fr: Option<&str>) -> Result<StopReason> {
        match fr {
            Some("tool_calls") | Some("function_call") => Ok(StopReason::ToolUse),
            Some("length") => Ok(StopReason::MaxTokens),
            Some("stop") => Ok(StopReason::EndTurn),
            Some(reason @ ("aborted" | "insufficient_system_resource")) => {
                Err(Error::server_error(
                    503,
                    format!("OpenAI 兼容服务中断生成：finish_reason={reason}"),
                ))
            }
            Some(reason) => Err(Error::Provider(format!(
                "OpenAI 兼容服务返回未支持的 finish_reason={reason}"
            ))),
            None => Err(Error::Provider("OpenAI 兼容服务缺少 finish_reason".into())),
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
        if let Some(thinking) = message.get("reasoning_content").and_then(|c| c.as_str()) {
            if !thinking.is_empty() {
                content.push(ContentBlock::Thinking {
                    thinking: thinking.into(),
                    signature: None,
                });
            }
        }
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
                    .ok_or_else(|| {
                        Error::Provider("OpenAI tool_call 缺少 arguments 字符串".into())
                    })?;
                let input: serde_json::Value = serde_json::from_str(args_str).map_err(|error| {
                    Error::Provider(format!("OpenAI tool_call arguments JSON 无效：{error}"))
                })?;
                if !input.is_object() {
                    return Err(Error::Provider(
                        "OpenAI tool_call arguments 必须是 JSON object".into(),
                    ));
                }
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }
        Ok(CompletionResponse {
            content,
            stop_reason: Self::map_finish_reason(
                choice.get("finish_reason").and_then(|f| f.as_str()),
            )?,
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
                        Some(Ok(bytes)) => match decoder.feed(&bytes) {
                            Ok(frames) => {
                                for frame in frames {
                                    if let Err(error) = state.absorb(&frame, &mut queue) {
                                        queue.push_back(Err(error));
                                        finished = true;
                                        break;
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
                            finished = true;
                        }
                        None => {
                            match decoder.finish() {
                                Ok(frames) => {
                                    for frame in frames {
                                        if let Err(error) = state.absorb(&frame, &mut queue) {
                                            queue.push_back(Err(error));
                                            finished = true;
                                            break;
                                        }
                                    }
                                }
                                Err(error) => {
                                    queue.push_back(Err(error));
                                    finished = true;
                                }
                            }
                            if !finished {
                                if let Err(error) = state.finish(&mut queue) {
                                    queue.push_back(Err(error));
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
    reasoning: String,
    text: String,
    tool_calls: std::collections::BTreeMap<usize, (String, String, String)>, // id, name, args
    usage: Usage,
    stop_reason: Option<StopReason>,
    error: Option<OpenAiStreamError>,
    done: bool,
    accumulated_bytes: usize,
}

#[derive(Debug)]
struct OpenAiStreamError {
    status: Option<u16>,
    code: Option<String>,
    error_type: Option<String>,
    message: String,
    retry_after_secs: Option<u64>,
}

impl OpenAiStreamError {
    /// 把 OpenAI 兼容服务的流内错误转换为 OpenPaw 稳定错误分类。
    fn into_error(self) -> Error {
        let status = self.status.or_else(|| {
            [&self.code, &self.error_type, &Some(self.message.clone())]
                .into_iter()
                .flatten()
                .find_map(|value| parse_status(value))
        });
        let detail = self
            .code
            .as_deref()
            .or(self.error_type.as_deref())
            .map(|kind| format!("{kind}: "))
            .unwrap_or_default();
        let message = format!("OpenAI 兼容服务流式请求失败：{detail}{}", self.message);
        if let Some(status @ 500..=599) = status {
            return Error::server_error(status, message);
        }
        if status == Some(429)
            || self.retry_after_secs.is_some()
            || is_rate_limit_text(&self.message)
            || self.code.as_deref().is_some_and(is_rate_limit_text)
            || self.error_type.as_deref().is_some_and(is_rate_limit_text)
        {
            return Error::rate_limited(message, self.retry_after_secs);
        }
        if is_unavailable_text(&self.message)
            || self.code.as_deref().is_some_and(is_unavailable_text)
            || self.error_type.as_deref().is_some_and(is_unavailable_text)
        {
            return Error::server_error(503, message);
        }
        Error::Provider(message)
    }
}

/// 识别兼容服务在 HTTP 200 流内返回的过载或暂时不可用标记。
fn is_unavailable_text(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("overloaded")
        || lower.contains("capacity")
        || lower.contains("service unavailable")
        || lower.contains("service_unavailable")
        || lower.contains("too busy")
        || lower.contains("insufficient_system_resource")
}

fn is_rate_limit_text(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("rate-limit")
        || lower.contains("too many requests")
        || lower.contains("too_many_requests")
        || lower.contains("too-many-requests")
}

fn parse_status(value: &str) -> Option<u16> {
    if let Ok(status) = value.trim().parse::<u16>() {
        return Some(status);
    }
    let lower = value.to_lowercase();
    [500u16, 502, 503, 529, 429].into_iter().find(|status| {
        lower.contains(&format!("http {status}"))
            || lower.contains(&format!("status {status}"))
            || lower.contains(&format!("http_{status}"))
            || lower.contains(&format!("status_{status}"))
    })
}

fn parse_retry_after(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
}

impl OpenAiStreamState {
    fn reserve_bytes(&mut self, amount: usize) -> Result<()> {
        let Some(total) = self.accumulated_bytes.checked_add(amount) else {
            return Err(Error::Provider("OpenAI 流累计内容长度计算溢出".into()));
        };
        if total > MAX_STREAM_ACCUMULATED_BYTES {
            return Err(Error::Provider(format!(
                "OpenAI 流累计内容超过上限：{total} > {MAX_STREAM_ACCUMULATED_BYTES} 字节"
            )));
        }
        self.accumulated_bytes = total;
        Ok(())
    }

    fn absorb(
        &mut self,
        frame: &crate::sse::Frame,
        queue: &mut std::collections::VecDeque<openpaw_core::Result<StreamEvent>>,
    ) -> Result<()> {
        let data = match &frame.data {
            crate::sse::SseData::Done => {
                if self.done {
                    return Err(Error::Provider("OpenAI 重复返回 [DONE] 终止帧".into()));
                }
                self.done = true;
                return Ok(());
            }
            crate::sse::SseData::Json(data) => data,
        };
        if self.done {
            return Err(Error::Provider("OpenAI [DONE] 之后仍收到数据帧".into()));
        }
        if let Some(error) = data.get("error") {
            let message = error
                .get("message")
                .and_then(|value| value.as_str())
                .or_else(|| error.as_str())
                .unwrap_or("OpenAI 兼容服务返回了未知流式错误");
            let status = error
                .get("status")
                .or_else(|| data.get("status"))
                .and_then(|value| {
                    value
                        .as_u64()
                        .and_then(|status| u16::try_from(status).ok())
                        .or_else(|| value.as_str().and_then(|value| value.parse::<u16>().ok()))
                })
                .or_else(|| {
                    [
                        Some(message),
                        error.get("code").and_then(|value| value.as_str()),
                        error.get("type").and_then(|value| value.as_str()),
                    ]
                    .into_iter()
                    .flatten()
                    .find_map(parse_status)
                });
            let retry_after_secs = error
                .get("retry_after")
                .or_else(|| error.get("retry_after_secs"))
                .or_else(|| data.get("retry_after"))
                .or_else(|| data.get("retry_after_secs"))
                .and_then(parse_retry_after);
            self.error = Some(OpenAiStreamError {
                status,
                code: error
                    .get("code")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                error_type: error
                    .get("type")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                message: message.to_string(),
                retry_after_secs,
            });
            return Ok(());
        }
        if self.error.is_some() {
            return Ok(());
        }
        if let Some(u) = data.get("usage") {
            self.usage = OpenAiProvider::parse_usage(Some(u));
        }
        let Some(choice) = data.get("choices").and_then(|c| c.get(0)) else {
            return Ok(());
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));
        if let Some(thinking) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            if !thinking.is_empty() {
                self.reserve_bytes(thinking.len())?;
                self.reasoning.push_str(thinking);
                queue.push_back(Ok(StreamEvent::ThinkingDelta {
                    index: 0,
                    text: thinking.to_string(),
                }));
            }
        }
        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                self.reserve_bytes(text.len())?;
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
                if idx >= 1024 {
                    return Err(Error::Provider(format!(
                        "OpenAI tool_call index 超过上限：{idx} >= 1024"
                    )));
                }
                let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str());
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str());
                self.reserve_bytes(
                    id.len()
                        .saturating_add(name.map_or(0, str::len))
                        .saturating_add(args.map_or(0, str::len)),
                )?;
                let entry = self
                    .tool_calls
                    .entry(idx)
                    .or_insert_with(|| (id.to_string(), String::new(), String::new()));
                if entry.0.is_empty() && !id.is_empty() {
                    entry.0.push_str(id);
                }
                if let Some(name) = name {
                    entry.1.push_str(name);
                }
                if let Some(args) = args {
                    entry.2.push_str(args);
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.stop_reason = Some(OpenAiProvider::map_finish_reason(Some(fr))?);
        }
        Ok(())
    }

    fn finish(
        &mut self,
        queue: &mut std::collections::VecDeque<openpaw_core::Result<StreamEvent>>,
    ) -> Result<()> {
        if let Some(message) = self.error.take() {
            return Err(message.into_error());
        }
        if !self.done {
            return Err(Error::Provider(
                "OpenAI 流在 [DONE] 前结束，拒绝生成成功响应".into(),
            ));
        }
        let stop_reason = self.stop_reason.ok_or_else(|| {
            Error::Provider("OpenAI 流缺少合法 finish_reason，拒绝生成成功响应".into())
        })?;
        // Assemble accumulated tool_call fragments: emit a ToolUseStart per
        // call, and the Final response carries the assembled blocks.
        let calls: Vec<(usize, String, String, String)> = std::mem::take(&mut self.tool_calls)
            .into_iter()
            .map(|(idx, (id, name, args))| (idx, id, name, args))
            .collect();
        let mut final_content: Vec<ContentBlock> = Vec::new();
        if !self.reasoning.is_empty() {
            final_content.push(ContentBlock::Thinking {
                thinking: std::mem::take(&mut self.reasoning),
                signature: None,
            });
        }
        if !self.text.is_empty() {
            final_content.push(ContentBlock::Text {
                text: std::mem::take(&mut self.text),
            });
        }
        for (idx, id, name, args) in &calls {
            if id.is_empty() || name.is_empty() {
                return Err(Error::Provider(format!(
                    "OpenAI tool_call {idx} 缺少 id 或 function.name"
                )));
            }
            let input: serde_json::Value = serde_json::from_str(args).map_err(|error| {
                Error::Provider(format!(
                    "OpenAI tool_call {idx} arguments JSON 不完整或无效：{error}"
                ))
            })?;
            if !input.is_object() {
                return Err(Error::Provider(format!(
                    "OpenAI tool_call {idx} arguments 必须是 JSON object"
                )));
            }
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
            stop_reason,
            usage: self.usage,
        }))));
        Ok(())
    }
}

#[cfg(test)]
mod stream_state_tests {
    use super::*;
    use crate::sse::{Frame, SseData};
    use openpaw_core::{Message, ToolSpec};

    #[test]
    fn openai_stream_state_enforces_accumulated_limit() {
        let mut state = OpenAiStreamState::default();
        let mut queue = std::collections::VecDeque::new();
        let megabyte = "x".repeat(1024 * 1024);
        for _ in 0..16 {
            let frame = Frame {
                event: String::new(),
                data: SseData::Json(json!({
                    "choices": [{"delta": {"content": megabyte}}]
                })),
            };
            state
                .absorb(&frame, &mut queue)
                .expect("exact accumulated limit remains valid");
        }
        assert_eq!(state.accumulated_bytes, MAX_STREAM_ACCUMULATED_BYTES);

        let over = Frame {
            event: String::new(),
            data: SseData::Json(json!({
                "choices": [{"delta": {"content": "x"}}]
            })),
        };
        let error = state.absorb(&over, &mut queue).unwrap_err();
        assert!(error.to_string().contains("累计内容超过上限"));
        assert!(!queue
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Final(_)))));
    }

    #[test]
    fn deepseek_tool_followup_body_preserves_reasoning_and_tool_schema() {
        let provider =
            OpenAiProvider::new("sk", "deepseek-chat").with_base_url("https://api.deepseek.com");
        let request = CompletionRequest {
            model: "deepseek-chat".into(),
            max_tokens: 100,
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "分析".into(),
                        signature: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        input: json!({"path":"a.md"}),
                    },
                ],
                ..Default::default()
            }],
            tools: vec![ToolSpec::new("read_file", "读取文件")],
            ..Default::default()
        };
        let body = provider.build_body(&request, true);
        assert_eq!(body["max_tokens"], json!(100));
        assert_eq!(body["messages"][0]["reasoning_content"], json!("分析"));
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"));
        assert!(body.get("reasoning_effort").is_none());
    }
}
