//! SSE 字节解码和 Anthropic 流事件组装。
//!
//! 输入边界：接收 HTTP 层任意切分的原始字节，只有完整 SSE 行才执行严格 UTF-8 和 JSON
//! 解码；输出边界：向 provider 层交付有序 [`Frame`] 或明确错误。
//! 禁止职责：本模块不发起网络请求、不重试 provider，也不把截断或畸形输入收敛为成功。
//! 关联：`CHG-2026-0013/B10`、`G02`、`G07`、`G08`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->

use openpaw_core::{ContentBlock, Error, Result, StopReason, StreamEvent, Usage};

const MAX_SSE_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_DATA_BYTES: usize = 2 * 1024 * 1024;
/// 功能：限制单个 provider 流在组装阶段保留的正文、thinking 和 tool 参数总字节数。
/// 参数：无；值按 UTF-8 字节计算。
/// 返回：供同 crate 的 OpenAI 与 Anthropic 组装器执行一致资源门禁。
/// 错误：累计值超过该上限时返回 provider 错误，不生成成功终态。
/// 状态：编译期常量，不随请求或会话变化。
/// 并发与取消：无共享可变状态；取消不会改变上限。
/// 权限：不包含正文、凭据或用户配置。
/// 关联：`CHG-2026-0013/B10`、`G08`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
pub(crate) const MAX_STREAM_ACCUMULATED_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTENT_BLOCKS: usize = 1024;

/// 功能：区分普通 JSON 数据与 OpenAI `[DONE]` 终止证据。
/// 字段/变体语义：`Json` 只承载严格解析后的 JSON；`Done` 只对应精确 `[DONE]`。
/// 数据来源：[`SseDecoder`] 完成 SSE data 拼接后解析。
/// 数据去向：OpenAI 与 Anthropic 协议状态机。
/// 约束：畸形 JSON 不会生成任何变体；Anthropic 不接受 `Done`。
/// 稳定性：crate 公共解析类型；新增变体必须更新两个 provider 的穷尽处理。
/// 关联：`CHG-2026-0013/B11`、`G07`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Debug)]
pub enum SseData {
    /// 严格 JSON 值，不包含 OpenAI 终止标记。
    Json(serde_json::Value),
    /// 精确匹配且只允许 OpenAI 消费的 `[DONE]` 终止标记。
    Done,
}

/// 功能：表示完成 UTF-8 和 data 校验的一条 SSE 事件。
/// 字段/变体语义：`event` 是可为空的 SSE 事件名，`data` 明确区分 JSON 与 `[DONE]`。
/// 数据来源：[`SseDecoder`] 从 HTTP 响应字节中解析。
/// 数据去向：OpenAI 与 Anthropic 流组装器。
/// 约束：`data` 不包含畸形 JSON；单帧数据不得超过 [`MAX_SSE_DATA_BYTES`]。
/// 稳定性：crate 公共解析类型；字段语义变更必须同步 provider 契约测试。
/// 关联：`CHG-2026-0013/B10`、`G02`、`G07`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Debug)]
pub struct Frame {
    /// SSE `event:` 字段；上游省略时为空字符串。
    pub event: String,
    /// 严格解析后的 `data:` 内容或显式 OpenAI 终止证据。
    pub data: SseData,
}

/// 功能：把任意 HTTP 分块增量解码为完整 SSE 帧。
/// 字段/变体语义：内部只保留未完成行、当前事件名、当前帧数据和失败状态。
/// 数据来源：provider 的 `reqwest::Response::bytes_stream()`。
/// 数据去向：[`Frame`] 序列或 [`Error::Provider`]。
/// 约束：UTF-8 无损且严格；行、单帧数据和内部残留均有固定上限。
/// 稳定性：crate 公共传输组件；错误必须保持 fail-closed，不能替换为 `U+FFFD`。
/// 关联：`CHG-2026-0013/B10`、`G02`、`G07`、`G08`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Default)]
pub struct SseDecoder {
    line_buf: String,
    utf8_tail: Vec<u8>,
    event: String,
    data: String,
    failed: Option<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; returns completed frames in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        self.ensure_healthy()?;
        let mut input = std::mem::take(&mut self.utf8_tail);
        input.extend_from_slice(bytes);
        let mut frames = Vec::new();
        match std::str::from_utf8(&input) {
            Ok(text) => self.process_text(text, &mut frames)?,
            Err(error) => {
                if error.error_len().is_some() {
                    return self.fail(format!("SSE UTF-8 解码失败：{error}"));
                }
                let valid_up_to = error.valid_up_to();
                let valid = std::str::from_utf8(&input[..valid_up_to])
                    .map_err(|inner| Error::Provider(format!("SSE UTF-8 前缀解码失败：{inner}")))?;
                self.process_text(valid, &mut frames)?;
                self.utf8_tail.extend_from_slice(&input[valid_up_to..]);
                if self.utf8_tail.len() > 3 {
                    return self.fail(format!(
                        "SSE UTF-8 未完成尾部超过 3 字节：{}",
                        self.utf8_tail.len()
                    ));
                }
            }
        }
        Ok(frames)
    }

    /// 功能：在 HTTP EOF 时校验并冲刷最后一行和最后一帧。
    /// 参数：无；使用此前 `feed` 保存的内部状态。
    /// 返回：EOF 才完成的零个或一个 [`Frame`]。
    /// 错误：截断 UTF-8、畸形 JSON或缓冲超限时明确失败。
    /// 状态：成功后清空所有待处理数据；失败后实例保持失败状态。
    /// 并发与取消：必须在输入流结束后串行调用一次；取消无需调用。
    /// 权限：只处理内存字节，不持久化响应正文。
    /// 关联：`CHG-2026-0013/B10`、`G02`、`G07`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
    pub fn finish(&mut self) -> Result<Vec<Frame>> {
        self.ensure_healthy()?;
        if !self.utf8_tail.is_empty() {
            return self.fail(format!(
                "SSE 在 UTF-8 码点中间结束：残留 {} 字节",
                self.utf8_tail.len()
            ));
        }
        let mut frames = Vec::new();
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            self.process_line(&line, &mut frames)?;
        }
        if let Some(frame) = self.take_frame()? {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn ensure_healthy(&self) -> Result<()> {
        match &self.failed {
            Some(message) => Err(Error::Provider(message.clone())),
            None => Ok(()),
        }
    }

    fn fail<T>(&mut self, message: impl Into<String>) -> Result<T> {
        let message = message.into();
        self.failed = Some(message.clone());
        Err(Error::Provider(message))
    }

    fn process_text(&mut self, mut text: &str, frames: &mut Vec<Frame>) -> Result<()> {
        while let Some(pos) = text.find('\n') {
            self.extend_line(&text[..pos])?;
            let line = std::mem::take(&mut self.line_buf);
            self.process_line(&line, frames)?;
            text = &text[pos + 1..];
        }
        self.extend_line(text)
    }

    fn extend_line(&mut self, text: &str) -> Result<()> {
        let Some(new_len) = self.line_buf.len().checked_add(text.len()) else {
            return self.fail("SSE 行长度计算溢出");
        };
        if new_len > MAX_SSE_LINE_BYTES {
            return self.fail(format!(
                "SSE 单行超过上限：{new_len} > {MAX_SSE_LINE_BYTES} 字节"
            ));
        }
        self.line_buf.push_str(text);
        Ok(())
    }

    fn process_line(&mut self, line: &str, frames: &mut Vec<Frame>) -> Result<()> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if let Some(frame) = self.take_frame()? {
                frames.push(frame);
            }
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let separator = usize::from(!self.data.is_empty());
            let Some(new_len) = self
                .data
                .len()
                .checked_add(separator)
                .and_then(|len| len.checked_add(rest.len()))
            else {
                return self.fail("SSE data 长度计算溢出");
            };
            if new_len > MAX_SSE_DATA_BYTES {
                return self.fail(format!(
                    "SSE 单帧 data 超过上限：{new_len} > {MAX_SSE_DATA_BYTES} 字节"
                ));
            }
            if separator == 1 {
                self.data.push('\n');
            }
            self.data.push_str(rest);
        }
        // SSE 注释和未知字段按规范忽略，但仍受单行上限保护。
        Ok(())
    }

    fn take_frame(&mut self) -> Result<Option<Frame>> {
        if self.data.is_empty() {
            self.event.clear();
            return Ok(None);
        }
        let raw_data = std::mem::take(&mut self.data);
        let data = if raw_data.trim() == "[DONE]" {
            SseData::Done
        } else {
            match serde_json::from_str(&raw_data) {
                Ok(data) => SseData::Json(data),
                Err(error) => return self.fail(format!("SSE data JSON 解码失败：{error}")),
            }
        };
        let frame = Frame {
            event: std::mem::take(&mut self.event),
            data,
        };
        Ok(Some(frame))
    }
}

// ---- Anthropic stream mapping -------------------------------------------

/// Anthropic 流式内容块的内部累加状态。
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

/// 功能：把 Anthropic SSE 帧组装为增量事件和最终响应。
/// 字段/变体语义：按 block index 保存文本、thinking 和 tool 参数，并跟踪终止与资源状态。
/// 数据来源：[`SseDecoder`] 输出的 Anthropic JSON 帧。
/// 数据去向：provider 流中的 [`StreamEvent`] 和最终 [`openpaw_core::CompletionResponse`]。
/// 约束：必须看到 `message_stop` 才能成功；累计保留内容不得超过固定上限。
/// 稳定性：crate 公共组装组件；截断、畸形 tool JSON 和越界必须明确失败。
/// 关联：`CHG-2026-0013/B10`、`G07`、`G08`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Debug, Default)]
pub struct AnthropicStreamAssembler {
    blocks: std::collections::BTreeMap<usize, BlockBuilder>,
    usage: Usage,
    stop_reason: StopReason,
    finished: bool,
    accumulated_bytes: usize,
    failure: Option<String>,
}

impl AnthropicStreamAssembler {
    /// 功能：创建空的 Anthropic 流组装器。
    /// 参数：无。
    /// 返回：未接收事件、未终止且未失败的组装器。
    /// 错误：构造阶段不执行解析或 I/O，不返回错误。
    /// 状态：block、usage 和累计字节均为空或零。
    /// 并发与取消：实例由单个响应流串行驱动；取消时可直接丢弃。
    /// 权限：不读取凭据或外部状态。
    /// 关联：`CHG-2026-0013/B10`、`G07`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
    pub fn new() -> Self {
        Self::default()
    }

    fn fail<T>(&mut self, message: impl Into<String>) -> Result<T> {
        let message = message.into();
        self.failure = Some(message.clone());
        Err(Error::Provider(message))
    }

    fn reserve_bytes(&mut self, amount: usize) -> Result<()> {
        let Some(total) = self.accumulated_bytes.checked_add(amount) else {
            return self.fail("Anthropic 流累计内容长度计算溢出");
        };
        if total > MAX_STREAM_ACCUMULATED_BYTES {
            return self.fail(format!(
                "Anthropic 流累计内容超过上限：{total} > {MAX_STREAM_ACCUMULATED_BYTES} 字节"
            ));
        }
        self.accumulated_bytes = total;
        Ok(())
    }

    fn insert_block(&mut self, index: usize, block: BlockBuilder) -> Result<()> {
        if index >= MAX_CONTENT_BLOCKS {
            return self.fail(format!(
                "Anthropic content block index 超过上限：{index} >= {MAX_CONTENT_BLOCKS}"
            ));
        }
        if self.blocks.contains_key(&index) {
            return self.fail(format!("Anthropic content block 重复开始：{index}"));
        }
        self.blocks.insert(index, block);
        Ok(())
    }

    /// 功能：吸收一个已解码 Anthropic 帧并生成对应增量事件。
    /// 参数：`frame` 必须是严格 JSON 解码后的单个 SSE 帧。
    /// 返回：该帧产生的零个或多个 [`StreamEvent`]。
    /// 错误：协议字段缺失、block 顺序错误、provider error 或累计内容越界时失败。
    /// 状态：成功时更新 block、usage、stop reason 或 `message_stop` 标记。
    /// 并发与取消：必须按网络顺序串行调用；失败后不可恢复或继续组装。
    /// 权限：只保留生成最终响应所需内容，不记录凭据或请求正文。
    /// 关联：`CHG-2026-0013/B10`、`G07`、`G08`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
    pub fn absorb(&mut self, frame: &Frame) -> Result<Vec<StreamEvent>> {
        if let Some(message) = &self.failure {
            return Err(Error::Provider(message.clone()));
        }
        if self.finished {
            return self.fail("Anthropic message_stop 之后仍收到数据帧");
        }
        let mut out = Vec::new();
        let SseData::Json(v) = &frame.data else {
            return self.fail("Anthropic 流收到不允许的 [DONE] 终止帧");
        };
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
                out.push(StreamEvent::MessageStart);
            }
            "content_block_start" => {
                let index = required_index(v, "content_block_start")?;
                let cb = v.get("content_block");
                let block_type = cb
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match block_type {
                    "text" => self.insert_block(index, BlockBuilder::Text(String::new()))?,
                    "thinking" => self.insert_block(
                        index,
                        BlockBuilder::Thinking {
                            text: String::new(),
                            signature: None,
                        },
                    )?,
                    "tool_use" => {
                        let id = cb
                            .and_then(|b| b.get("id"))
                            .and_then(|i| i.as_str())
                            .ok_or_else(|| Error::Provider("Anthropic tool_use 缺少 id".into()))?
                            .to_string();
                        let name = cb
                            .and_then(|b| b.get("name"))
                            .and_then(|n| n.as_str())
                            .ok_or_else(|| Error::Provider("Anthropic tool_use 缺少 name".into()))?
                            .to_string();
                        self.reserve_bytes(id.len().saturating_add(name.len()))?;
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: id.clone(),
                            name: name.clone(),
                        });
                        self.insert_block(
                            index,
                            BlockBuilder::ToolUse {
                                id,
                                name,
                                partial_json: String::new(),
                            },
                        )?;
                    }
                    other => {
                        return self
                            .fail(format!("Anthropic content_block_start 类型无效：{other}"));
                    }
                }
            }
            "content_block_delta" => {
                let index = required_index(v, "content_block_delta")?;
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
                            .ok_or_else(|| {
                                Error::Provider("Anthropic text_delta 缺少 text".into())
                            })?
                            .to_string();
                        self.reserve_bytes(text.len())?;
                        match self.blocks.get_mut(&index) {
                            Some(BlockBuilder::Text(buf)) => buf.push_str(&text),
                            _ => {
                                return self.fail(format!(
                                    "Anthropic text_delta block 不存在或类型不匹配：{index}"
                                ))
                            }
                        }
                        out.push(StreamEvent::TextDelta { index, text });
                    }
                    "thinking_delta" => {
                        let text = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(|t| t.as_str())
                            .ok_or_else(|| {
                                Error::Provider("Anthropic thinking_delta 缺少 thinking".into())
                            })?
                            .to_string();
                        self.reserve_bytes(text.len())?;
                        match self.blocks.get_mut(&index) {
                            Some(BlockBuilder::Thinking { text: buf, .. }) => buf.push_str(&text),
                            _ => {
                                return self.fail(format!(
                                    "Anthropic thinking_delta block 不存在或类型不匹配：{index}"
                                ))
                            }
                        }
                        out.push(StreamEvent::ThinkingDelta { index, text });
                    }
                    "input_json_delta" => {
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|s| s.as_str())
                            .ok_or_else(|| {
                                Error::Provider(
                                    "Anthropic input_json_delta 缺少 partial_json".into(),
                                )
                            })?
                            .to_string();
                        self.reserve_bytes(partial.len())?;
                        match self.blocks.get_mut(&index) {
                            Some(BlockBuilder::ToolUse { partial_json, .. }) => {
                                partial_json.push_str(&partial)
                            }
                            _ => {
                                return self.fail(format!(
                                    "Anthropic input_json_delta block 不存在或类型不匹配：{index}"
                                ))
                            }
                        }
                        out.push(StreamEvent::ToolUseInputDelta {
                            index,
                            partial_json: partial,
                        });
                    }
                    "signature_delta" => {
                        let sig = delta
                            .and_then(|d| d.get("signature"))
                            .and_then(|s| s.as_str())
                            .ok_or_else(|| {
                                Error::Provider("Anthropic signature_delta 缺少 signature".into())
                            })?
                            .to_string();
                        self.reserve_bytes(sig.len())?;
                        match self.blocks.get_mut(&index) {
                            Some(BlockBuilder::Thinking { signature, .. }) => match signature {
                                Some(existing) => existing.push_str(&sig),
                                None => *signature = Some(sig),
                            },
                            _ => {
                                return self.fail(format!(
                                    "Anthropic signature_delta block 不存在或类型不匹配：{index}"
                                ))
                            }
                        }
                    }
                    other => return self.fail(format!("Anthropic delta 类型无效：{other}")),
                }
            }
            "content_block_stop" => {
                let index = required_index(v, "content_block_stop")?;
                if !self.blocks.contains_key(&index) {
                    return self.fail(format!(
                        "Anthropic content_block_stop block 不存在：{index}"
                    ));
                }
                out.push(StreamEvent::BlockStop { index });
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
                return self.fail(format!("Anthropic 流式请求失败：{msg}"));
            }
            "ping" => {}
            _ => {}
        }
        Ok(out)
    }

    /// 功能：在 HTTP EOF 后校验终止证据并构建最终 Anthropic 响应。
    /// 参数：无；读取此前按序吸收的全部帧状态。
    /// 返回：通过终止和 tool JSON 校验的完整响应。
    /// 错误：缺少 `message_stop`、先前协议失败或 tool 参数不是完整 JSON object 时失败。
    /// 状态：只读组装，不修改已累加内容，允许调用方在 EOF 时调用一次。
    /// 并发与取消：不得与 `absorb` 并发；取消或网络错误时调用方不应伪造 finalize。
    /// 权限：只返回内存响应，不落盘或记录模型正文。
    /// 关联：`CHG-2026-0013/B10`、`G07`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
    pub fn finalize(&self) -> Result<openpaw_core::CompletionResponse> {
        if let Some(message) = &self.failure {
            return Err(Error::Provider(message.clone()));
        }
        if !self.finished {
            return Err(Error::Provider(
                "Anthropic 流在 message_stop 前结束，拒绝生成成功响应".into(),
            ));
        }
        let mut content = Vec::new();
        for block in self.blocks.values() {
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
                            Error::Provider(format!("Anthropic tool_use JSON 不完整或无效：{e}"))
                        })?
                    };
                    if !input.is_object() {
                        return Err(Error::Provider(
                            "Anthropic tool_use 参数必须是 JSON object".into(),
                        ));
                    }
                    content.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input,
                    });
                }
            }
        }
        Ok(openpaw_core::CompletionResponse {
            content,
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}

fn required_index(value: &serde_json::Value, event: &str) -> Result<usize> {
    let raw = value
        .get("index")
        .and_then(|index| index.as_u64())
        .ok_or_else(|| Error::Provider(format!("Anthropic {event} 缺少合法 index")))?;
    usize::try_from(raw)
        .map_err(|_| Error::Provider(format!("Anthropic {event} index 超出平台范围：{raw}")))
}

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

    fn decode_chunks(raw: &[u8], sizes: &[usize]) -> Result<Vec<Frame>> {
        let mut decoder = SseDecoder::new();
        let mut frames = Vec::new();
        let mut offset = 0;
        let mut size_index = 0;
        while offset < raw.len() {
            let size = sizes[size_index % sizes.len()].max(1);
            let end = offset.saturating_add(size).min(raw.len());
            frames.extend(decoder.feed(&raw[offset..end])?);
            offset = end;
            size_index += 1;
        }
        frames.extend(decoder.finish()?);
        Ok(frames)
    }

    fn deterministic_random_sizes(count: usize) -> Vec<usize> {
        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 32) as usize % 257) + 1
            })
            .collect()
    }

    fn frames_from(raw: &str) -> Vec<Frame> {
        decode_chunks(raw.as_bytes(), &[raw.len().max(1)]).expect("decode SSE")
    }

    #[test]
    fn sse_decoder_splits_events_and_data() {
        let raw = "event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\n";
        let frames = frames_from(raw);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message_start");
        match &frames[0].data {
            SseData::Json(value) => assert_eq!(value["type"], json!("message_start")),
            SseData::Done => panic!("unexpected done"),
        }
    }

    #[test]
    fn sse_decoder_handles_crlf_and_partial_feeds() {
        let mut d = SseDecoder::new();
        let mut frames = d.feed(b"data: {\"a\"").expect("partial feed");
        assert!(frames.is_empty());
        frames = d.feed(b":1}\r\n\r\n").expect("final feed");
        frames.extend(d.finish().expect("finish SSE"));
        assert_eq!(frames.len(), 1);
        match &frames[0].data {
            SseData::Json(value) => assert_eq!(value["a"], json!(1)),
            SseData::Done => panic!("unexpected done"),
        }
    }

    #[test]
    fn sse_decoder_preserves_unicode_for_fixed_and_random_chunks() {
        let text = "ASCII 中文 😀 e\u{301}\r\n# Markdown\n```rust\nfn main() { println!(\"你好🌍\"); }\n```\n";
        let json = serde_json::to_string(&json!({
            "choices": [{"delta": {"content": text}}]
        }))
        .unwrap();
        let raw = format!("data: {json}\n\ndata: [DONE]\n\n");
        let mut matrices = vec![
            vec![1],
            vec![2],
            vec![3],
            vec![4],
            vec![7],
            vec![31],
            vec![1024],
        ];
        matrices.push(deterministic_random_sizes(128));

        for sizes in matrices {
            let frames = decode_chunks(raw.as_bytes(), &sizes).expect("chunked decode");
            assert_eq!(frames.len(), 2, "sizes={sizes:?}");
            match &frames[0].data {
                SseData::Json(value) => {
                    assert_eq!(value["choices"][0]["delta"]["content"], json!(text));
                }
                SseData::Done => panic!("first frame cannot be done"),
            }
            assert!(matches!(frames[1].data, SseData::Done));
        }
    }

    #[test]
    fn sse_decoder_rejects_invalid_and_truncated_utf8() {
        let invalid = b"data: {\"text\":\"\xff\"}\n\n";
        let error = decode_chunks(invalid, &[1]).unwrap_err();
        assert!(error.to_string().contains("UTF-8 解码失败"));

        for (label, truncated) in [
            ("two-byte", &[0xc2][..]),
            ("three-byte", &[0xe4, 0xb8][..]),
            ("four-byte", &[0xf0, 0x9f, 0x98][..]),
        ] {
            let mut decoder = SseDecoder::new();
            decoder.feed(b"data: {\"text\":\"").expect("valid prefix");
            decoder.feed(truncated).expect("incomplete tail");
            let error = decoder.finish().unwrap_err();
            assert!(
                error.to_string().contains("UTF-8 码点中间结束"),
                "{label}: {error}"
            );
        }
    }

    #[test]
    fn sse_decoder_rejects_malformed_json() {
        let error = decode_chunks(b"data: {\"broken\":\n\n", &[3]).unwrap_err();
        assert!(error.to_string().contains("JSON 解码失败"));
    }

    #[test]
    fn sse_decoder_enforces_line_and_data_limits() {
        let exact_line = "x".repeat(MAX_SSE_LINE_BYTES);
        let mut decoder = SseDecoder::new();
        decoder.feed(exact_line.as_bytes()).expect("line at limit");
        assert!(decoder.finish().expect("finish exact line").is_empty());

        let over_line = "x".repeat(MAX_SSE_LINE_BYTES + 1);
        let mut decoder = SseDecoder::new();
        let error = decoder.feed(over_line.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("单行超过上限"));

        fn multiline_json(total_data_bytes: usize) -> String {
            let first_len = total_data_bytes / 2;
            let second_len = total_data_bytes - first_len - 1;
            let first = format!("[{}", " ".repeat(first_len - 1));
            let second = format!("{}]", " ".repeat(second_len - 1));
            format!("data: {first}\ndata: {second}\n\n")
        }

        let exact_data = multiline_json(MAX_SSE_DATA_BYTES);
        let frames = decode_chunks(exact_data.as_bytes(), &[1024]).expect("data at limit");
        assert_eq!(frames.len(), 1);
        assert!(matches!(&frames[0].data, SseData::Json(value) if value.is_array()));

        let over_data = multiline_json(MAX_SSE_DATA_BYTES + 1);
        let error = decode_chunks(over_data.as_bytes(), &[1024]).unwrap_err();
        assert!(error.to_string().contains("data 超过上限"));
    }

    #[test]
    fn anthropic_assembler_enforces_accumulated_limit() {
        let mut assembler = AnthropicStreamAssembler::new();
        let start = Frame {
            event: String::new(),
            data: SseData::Json(json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text"}
            })),
        };
        assembler.absorb(&start).expect("start block");
        let megabyte = "中".repeat(1024 * 1024 / "中".len());
        let remaining = MAX_STREAM_ACCUMULATED_BYTES - megabyte.len() * 16;
        for _ in 0..16 {
            let delta = Frame {
                event: String::new(),
                data: SseData::Json(json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": megabyte}
                })),
            };
            assembler.absorb(&delta).expect("within accumulated limit");
        }
        if remaining > 0 {
            let delta = Frame {
                event: String::new(),
                data: SseData::Json(json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "x".repeat(remaining)}
                })),
            };
            assembler.absorb(&delta).expect("exact accumulated limit");
        }
        let over = Frame {
            event: String::new(),
            data: SseData::Json(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "x"}
            })),
        };
        let error = assembler.absorb(&over).unwrap_err();
        assert!(error.to_string().contains("累计内容超过上限"));
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
            all.extend(asm.absorb(&frame).expect("event ok"));
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
