//! SSE line parsing + the Anthropic event mapping (unified transport,
//! plan §3.3: the two upstream transports were structurally identical and
//! are merged here).
//!
//! Decoding strategy mirrors upstream: `data.type` is trusted over the
//! `event:` field because some proxies omit `event:`.

use openpaw_core::{ContentBlock, StopReason, StreamEvent, Usage};

/// One decoded SSE frame: the `event:` name (may be empty) and the JSON
/// value of the `data:` payload.
pub struct Frame {
    pub event: String,
    pub data: serde_json::Value,
}

/// Incremental SSE decoder over raw bytes: buffers partial lines, splits
/// `event:`/`data:` fields, and emits a frame per blank line.
#[derive(Default)]
pub struct SseDecoder {
    line_buf: String,
    event: String,
    data: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; returns completed frames in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        let chunk = String::from_utf8_lossy(bytes);
        self.line_buf.push_str(&chunk);
        let mut frames = Vec::new();
        while let Some(pos) = self.line_buf.find('\n') {
            let line: String = self.line_buf.drain(..=pos).collect();
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            if line.is_empty() {
                if let Some(frame) = self.take_frame() {
                    frames.push(frame);
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("event:") {
                self.event = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(rest.trim_start());
            }
            // Comments (`:`...) and unknown fields are ignored.
        }
        frames
    }

    /// Flush at stream end (a final frame without a trailing blank line).
    pub fn finish(&mut self) -> Vec<Frame> {
        let mut frames = Vec::new();
        if !self.line_buf.trim().is_empty() {
            // Process the dangling line, then try to emit.
            let tail = std::mem::take(&mut self.line_buf);
            let line = tail.trim_end();
            if let Some(rest) = line.strip_prefix("data:") {
                self.data.push_str(rest.trim_start());
            }
        }
        if let Some(frame) = self.take_frame() {
            frames.push(frame);
        }
        frames
    }

    fn take_frame(&mut self) -> Option<Frame> {
        if self.data.is_empty() {
            self.event.clear();
            return None;
        }
        let data = serde_json::from_str(&self.data).unwrap_or(serde_json::Value::Null);
        let frame = Frame {
            event: std::mem::take(&mut self.event),
            data,
        };
        self.data.clear();
        Some(frame)
    }
}

// ---- Anthropic stream mapping -------------------------------------------

/// Intermediate content-block builder for streamed Anthropic responses.
#[derive(Debug, Clone)]
enum BlockBuilder {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
}

/// Accumulates an Anthropic SSE stream into [`StreamEvent`]s plus the final
/// assembled [`CompletionResponse`].
#[derive(Debug, Default)]
pub struct AnthropicStreamAssembler {
    blocks: Vec<BlockBuilder>,
    usage: Usage,
    stop_reason: StopReason,
    finished: bool,
}

impl AnthropicStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_block(&mut self, index: usize, block: BlockBuilder) {
        while self.blocks.len() <= index {
            self.blocks.push(BlockBuilder::Text(String::new()));
        }
        self.blocks[index] = block;
    }

    /// Feed one decoded frame; returns the events it produces.
    pub fn absorb(&mut self, frame: &Frame) -> Vec<openpaw_core::Result<StreamEvent>> {
        let mut out: Vec<openpaw_core::Result<StreamEvent>> = Vec::new();
        if self.finished {
            return out;
        }
        let v = &frame.data;
        // `data.type` is trusted over `event:` (proxy tolerance).
        let ty = v
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or(&frame.event);

        match ty {
            "message_start" => {
                if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                    self.usage = parse_usage(u);
                }
                out.push(Ok(StreamEvent::MessageStart));
            }
            "content_block_start" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let cb = v.get("content_block");
                let block_type = cb
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match block_type {
                    "text" => self.ensure_block(index, BlockBuilder::Text(String::new())),
                    "thinking" => self.ensure_block(
                        index,
                        BlockBuilder::Thinking {
                            text: String::new(),
                            signature: None,
                        },
                    ),
                    "tool_use" => {
                        let id = cb
                            .and_then(|b| b.get("id"))
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = cb
                            .and_then(|b| b.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        out.push(Ok(StreamEvent::ToolUseStart {
                            index,
                            id: id.clone(),
                            name: name.clone(),
                        }));
                        self.ensure_block(
                            index,
                            BlockBuilder::ToolUse {
                                id,
                                name,
                                partial_json: String::new(),
                            },
                        );
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let delta = v.get("delta");
                let dt = delta
                    .and_then(|d| d.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match dt {
                    "text_delta" => {
                        let text = delta
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(BlockBuilder::Text(buf)) = self.blocks.get_mut(index) {
                            buf.push_str(&text);
                        }
                        out.push(Ok(StreamEvent::TextDelta { index, text }));
                    }
                    "thinking_delta" => {
                        let text = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(BlockBuilder::Thinking { text: buf, .. }) =
                            self.blocks.get_mut(index)
                        {
                            buf.push_str(&text);
                        }
                        out.push(Ok(StreamEvent::ThinkingDelta { index, text }));
                    }
                    "input_json_delta" => {
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(BlockBuilder::ToolUse { partial_json, .. }) =
                            self.blocks.get_mut(index)
                        {
                            partial_json.push_str(&partial);
                        }
                        out.push(Ok(StreamEvent::ToolUseInputDelta {
                            index,
                            partial_json: partial,
                        }));
                    }
                    "signature_delta" => {
                        let sig = delta
                            .and_then(|d| d.get("signature"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(BlockBuilder::Thinking { signature, .. }) =
                            self.blocks.get_mut(index)
                        {
                            match signature {
                                Some(existing) => existing.push_str(&sig),
                                None => *signature = Some(sig),
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                out.push(Ok(StreamEvent::BlockStop { index }));
            }
            "message_delta" => {
                if let Some(d) = v.get("delta") {
                    if let Some(sr) = d.get("stop_reason").and_then(|s| s.as_str()) {
                        self.stop_reason = match sr {
                            "end_turn" => StopReason::EndTurn,
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            "stop_sequence" => StopReason::StopSequence,
                            _ => StopReason::Other,
                        };
                    }
                }
                if let Some(u) = v.get("usage") {
                    let new_usage = parse_usage(u);
                    // message_delta usage replaces output_tokens, keeps input.
                    self.usage.output_tokens = new_usage.output_tokens;
                    if new_usage.cache_read_tokens > 0 {
                        self.usage.cache_read_tokens = new_usage.cache_read_tokens;
                    }
                    if new_usage.cache_creation_tokens > 0 {
                        self.usage.cache_creation_tokens = new_usage.cache_creation_tokens;
                    }
                }
            }
            "message_stop" => {
                self.finished = true;
            }
            "error" => {
                let msg = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("stream error")
                    .to_string();
                out.push(Err(openpaw_core::Error::Provider(msg)));
            }
            "ping" => {}
            _ => {}
        }
        out
    }

    /// Assemble the final response from the accumulated blocks.
    pub fn finalize(&self) -> openpaw_core::Result<CompletionResponseShape> {
        let mut content = Vec::new();
        for block in &self.blocks {
            match block {
                BlockBuilder::Text(text) => content.push(ContentBlock::Text { text: text.clone() }),
                BlockBuilder::Thinking { text, signature } => {
                    content.push(ContentBlock::Thinking {
                        thinking: text.clone(),
                        signature: signature.clone(),
                    })
                }
                BlockBuilder::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input: serde_json::Value = if partial_json.trim().is_empty() {
                        serde_json::Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(partial_json).map_err(|e| {
                            openpaw_core::Error::Provider(format!("tool_use json decode: {e}"))
                        })?
                    };
                    content.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input,
                    });
                }
            }
        }
        Ok(CompletionResponseShape {
            content,
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}

/// Alias so the provider layer speaks the same type names as core.
pub type CompletionResponseShape = openpaw_core::CompletionResponse;

fn parse_usage(u: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        cache_read_tokens: u
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_creation_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frames_from(raw: &str) -> Vec<Frame> {
        let mut d = SseDecoder::new();
        let mut frames = d.feed(raw.as_bytes());
        frames.extend(d.finish());
        frames
    }

    #[test]
    fn sse_decoder_splits_events_and_data() {
        let raw = "event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\n";
        let frames = frames_from(raw);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message_start");
        assert_eq!(frames[0].data["type"], json!("message_start"));
    }

    #[test]
    fn sse_decoder_handles_crlf_and_partial_feeds() {
        let mut d = SseDecoder::new();
        let mut frames = d.feed(b"data: {\"a\"");
        assert!(frames.is_empty());
        frames = d.feed(b":1}\r\n\r\n");
        frames.extend(d.finish());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data["a"], json!(1));
    }

    #[test]
    fn anthropic_assembler_full_stream() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pa\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"th\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut asm = AnthropicStreamAssembler::new();
        let mut all: Vec<StreamEvent> = Vec::new();
        for frame in frames_from(raw) {
            for ev in asm.absorb(&frame) {
                all.push(ev.expect("event ok"));
            }
        }
        // Event ordering: MessageStart, TextDelta×2, ToolUseStart, deltas, BlockStop.
        assert!(matches!(all[0], StreamEvent::MessageStart));
        assert!(matches!(all[1], StreamEvent::TextDelta { ref text, .. } if text == "Hel"));
        assert!(matches!(
            all[3],
            StreamEvent::ToolUseStart { ref name, .. } if name == "read_file"
        ));

        let resp = asm.finalize().unwrap();
        assert_eq!(resp.text(), "Hello");
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.usage.input_tokens, 12);
        assert_eq!(resp.usage.output_tokens, 7);
        assert!(matches!(
            resp.content[1],
            ContentBlock::ToolUse { ref name, ref input, .. }
                if name == "read_file" && input["path"] == json!("a.rs")
        ));
    }
}
