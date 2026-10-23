//! Deterministic mock provider — the backbone of contract tests, the
//! engine E1 tests, and the parity harness (same injected provider on both
//! sides of a comparison run).

use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::BoxStream;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider, Message,
    StopReason, StreamEvent, Usage,
};

use openpaw_core::Result;

/// Response strategy of the mock.
#[derive(Debug, Clone)]
pub enum MockMode {
    /// Always reply with this text.
    Fixed(String),
    /// Reply with `prefix + <index of call>`, incrementing per call.
    Counter(String),
    /// Emit a tool_use block for `tool_name` with the given input, then a
    /// short text block.
    ToolUse {
        tool_name: String,
        input: serde_json::Value,
        text: String,
    },
    /// Emit several tool_use blocks in one response (parallel dispatch).
    ParallelToolUse {
        tools: Vec<(String, serde_json::Value)>,
        text: String,
    },
    /// Deterministic script: response N uses entry N; the last entry
    /// repeats once the script is exhausted.
    Scripted(Vec<MockMode>),
    /// Streaming text: `stream()` emits MessageStart + N TextDeltas +
    /// Final; `complete()` returns the full text.
    StreamText { text: String, chunks: usize },
    /// 功能：按固定间隔产生文本分片，用于验证 Host 在流式输出期间的取消顺序。
    /// 约束：只用于测试；`delay_ms` 作用于首帧后的每个事件，不改变最终完整响应。
    DelayedStreamText {
        text: String,
        chunks: usize,
        delay_ms: u64,
    },
}

impl Default for MockMode {
    fn default() -> Self {
        MockMode::Fixed(String::new())
    }
}

/// Resolve the effective mode for call `n`, following `Scripted` one level.
fn resolve_mode(mode: &MockMode, n: u32) -> &MockMode {
    match mode {
        MockMode::Scripted(steps) if !steps.is_empty() => {
            let idx = (n as usize).min(steps.len() - 1);
            &steps[idx]
        }
        other => other,
    }
}

/// Deterministic mock provider.
pub struct MockProvider {
    mode: MockMode,
    calls: Mutex<u32>,
    model: String,
}

impl MockProvider {
    pub fn fixed(text: impl Into<String>) -> Self {
        Self {
            mode: MockMode::Fixed(text.into()),
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn counter(prefix: impl Into<String>) -> Self {
        Self {
            mode: MockMode::Counter(prefix.into()),
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn tool_use(
        tool_name: impl Into<String>,
        input: serde_json::Value,
        text: impl Into<String>,
    ) -> Self {
        Self {
            mode: MockMode::ToolUse {
                tool_name: tool_name.into(),
                input,
                text: text.into(),
            },
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn parallel_tool_use(
        tools: Vec<(String, serde_json::Value)>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            mode: MockMode::ParallelToolUse {
                tools,
                text: text.into(),
            },
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn scripted(steps: Vec<MockMode>) -> Self {
        assert!(!steps.is_empty(), "scripted provider needs >=1 step");
        Self {
            mode: MockMode::Scripted(steps),
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn stream_text(text: impl Into<String>, chunks: usize) -> Self {
        Self {
            mode: MockMode::StreamText {
                text: text.into(),
                chunks,
            },
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    /// 功能：创建在相邻流事件之间等待固定毫秒数的测试 provider。
    /// 参数：`text` 为完整回复，`chunks` 为期望文本分片数，`delay_ms` 为事件间隔。
    /// 返回：使用 `DelayedStreamText` 模式的确定性 MockProvider。
    /// 错误：构造阶段无错误；空文本和过大分片数会在流生成时安全归一化。
    /// 状态：每次调用递增 provider 调用计数，不访问网络或文件系统。
    /// 并发与取消：延迟使用 Tokio timer；丢弃流或取消上层 future 会停止后续等待。
    /// 权限：测试辅助入口，不读取凭据或扩大工具权限。
    /// 关联：`TEST-HOST-CANCEL-001`、`CHG-2026-0012`。
    pub fn stream_text_delayed(text: impl Into<String>, chunks: usize, delay_ms: u64) -> Self {
        Self {
            mode: MockMode::DelayedStreamText {
                text: text.into(),
                chunks,
                delay_ms,
            },
            calls: Mutex::new(0),
            model: "mock-1".to_string(),
        }
    }

    pub fn call_count(&self) -> u32 {
        *self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_call(&self) -> u32 {
        let mut c = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let n = *c;
        *c = c.wrapping_add(1);
        n
    }
}

fn content_and_stop(mode: &MockMode, n: u32) -> (Vec<ContentBlock>, StopReason) {
    match mode {
        MockMode::Fixed(text) => (
            vec![ContentBlock::Text { text: text.clone() }],
            StopReason::EndTurn,
        ),
        MockMode::Counter(prefix) => (
            vec![ContentBlock::Text {
                text: format!("{prefix}{n}"),
            }],
            StopReason::EndTurn,
        ),
        MockMode::StreamText { text, .. } => (
            vec![ContentBlock::Text { text: text.clone() }],
            StopReason::EndTurn,
        ),
        MockMode::DelayedStreamText { text, .. } => (
            vec![ContentBlock::Text { text: text.clone() }],
            StopReason::EndTurn,
        ),
        MockMode::ToolUse {
            tool_name,
            input,
            text,
        } => (
            vec![
                ContentBlock::ToolUse {
                    id: format!("call_{n}"),
                    name: tool_name.clone(),
                    input: input.clone(),
                },
                ContentBlock::Text { text: text.clone() },
            ],
            StopReason::ToolUse,
        ),
        MockMode::ParallelToolUse { tools, text } => {
            let mut content: Vec<ContentBlock> = tools
                .iter()
                .enumerate()
                .map(|(i, (name, input))| ContentBlock::ToolUse {
                    id: format!("call_{n}_{i}"),
                    name: name.clone(),
                    input: input.clone(),
                })
                .collect();
            content.push(ContentBlock::Text { text: text.clone() });
            (content, StopReason::ToolUse)
        }
        MockMode::Scripted(_) => unreachable!("resolve_mode never returns Scripted"),
    }
}

fn output_tokens_of(content: &[ContentBlock]) -> u32 {
    let chars: usize = content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.chars().count(),
            ContentBlock::ToolUse { .. } => 16,
            _ => 0,
        })
        .sum();
    ((chars as f64 / 4.0).ceil() as u32).max(1)
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let n = self.next_call();
        let mode = resolve_mode(&self.mode, n);
        let (content, stop) = content_and_stop(mode, n);
        let output_tokens = output_tokens_of(&content);
        Ok(CompletionResponse {
            content,
            stop_reason: stop,
            usage: Usage {
                input_tokens: estimate_request(&req),
                output_tokens,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let n = self.next_call();
        let mode = resolve_mode(&self.mode, n).clone();

        if let MockMode::StreamText { text, chunks }
        | MockMode::DelayedStreamText { text, chunks, .. } = mode
        {
            let delay_ms = match &self.mode {
                MockMode::DelayedStreamText { delay_ms, .. } => *delay_ms,
                MockMode::Scripted(steps) => steps
                    .iter()
                    .find_map(|step| match step {
                        MockMode::DelayedStreamText { delay_ms, .. } => Some(*delay_ms),
                        _ => None,
                    })
                    .unwrap_or(0),
                _ => 0,
            };
            let resp = CompletionResponse {
                content: vec![ContentBlock::Text { text: text.clone() }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: estimate_request(&req),
                    output_tokens: output_tokens_of(&[ContentBlock::Text { text: text.clone() }]),
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            };
            let char_count = text.chars().count();
            let chunks = chunks.max(1).min(char_count.max(1));
            let per_chunk = (char_count / chunks).max(1);
            let mut events: Vec<StreamEvent> = vec![StreamEvent::MessageStart];
            let mut offset = 0;
            for i in 0..chunks {
                let take: usize = if i + 1 == chunks {
                    char_count.saturating_sub(offset)
                } else {
                    per_chunk
                };
                let delta: String = text.chars().skip(offset).take(take).collect();
                offset += take;
                events.push(StreamEvent::TextDelta {
                    index: 0,
                    text: delta,
                });
            }
            events.push(StreamEvent::Final(Box::new(resp)));
            if delay_ms == 0 {
                return Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))));
            }
            let stream = futures::stream::unfold(
                (events, 0usize, delay_ms),
                |(events, index, delay_ms)| async move {
                    if index >= events.len() {
                        return None;
                    }
                    if index > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    let event = events[index].clone();
                    Some((Ok(event), (events, index + 1, delay_ms)))
                },
            );
            return Ok(Box::pin(stream));
        }

        // Non-streaming modes: single Final event (the trait default shape).
        let (content, stop) = content_and_stop(&mode, n);
        let resp = CompletionResponse {
            content,
            stop_reason: stop,
            usage: Usage {
                input_tokens: estimate_request(&req),
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            StreamEvent::Final(Box::new(resp)),
        )])))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_use: matches!(
                self.mode,
                MockMode::ToolUse { .. } | MockMode::ParallelToolUse { .. } | MockMode::Scripted(_)
            ),
            streaming: true,
            ..Capabilities::default()
        }
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn default_model(&self) -> &str {
        &self.model
    }
}

fn estimate_request(req: &CompletionRequest) -> u32 {
    let system = req.system.as_deref().unwrap_or("");
    let total: usize = system.chars().count()
        + req
            .messages
            .iter()
            .map(|m: &Message| m.text().chars().count())
            .sum::<usize>();
    ((total as f64 / 4.0).ceil() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(text: &str) -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::user_text(text)],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn fixed_mode_returns_same_text() {
        let p = MockProvider::fixed("ok");
        for _ in 0..3 {
            let r = p.complete(req("x")).await.unwrap();
            assert_eq!(r.text(), "ok");
            assert_eq!(r.stop_reason, StopReason::EndTurn);
        }
        assert_eq!(p.call_count(), 3);
    }

    #[tokio::test]
    async fn counter_mode_increments() {
        let p = MockProvider::counter("reply-");
        assert_eq!(p.complete(req("a")).await.unwrap().text(), "reply-0");
        assert_eq!(p.complete(req("b")).await.unwrap().text(), "reply-1");
    }

    #[tokio::test]
    async fn tool_use_mode_emits_block_and_stop_reason() {
        let p = MockProvider::tool_use("read_file", serde_json::json!({"path": "a.rs"}), "reading");
        let r = p.complete(req("go")).await.unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert!(r
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name == "read_file")));
        assert!(p.capabilities().tool_use);
    }

    #[tokio::test]
    async fn parallel_tool_use_emits_multiple_blocks() {
        let p = MockProvider::parallel_tool_use(
            vec![
                ("grep".into(), serde_json::json!({"pattern": "a"})),
                ("glob".into(), serde_json::json!({"pattern": "*.rs"})),
            ],
            "searching",
        );
        let r = p.complete(req("go")).await.unwrap();
        let tool_uses = r
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count();
        assert_eq!(tool_uses, 2);
        assert_eq!(r.stop_reason, StopReason::ToolUse);
    }

    #[tokio::test]
    async fn scripted_mode_steps_then_repeats_last() {
        let p = MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "echo_tool".into(),
                input: serde_json::json!({"text": "x"}),
                text: "calling".into(),
            },
            MockMode::Fixed("done".into()),
        ]);
        let first = p.complete(req("a")).await.unwrap();
        assert_eq!(first.stop_reason, StopReason::ToolUse);
        let second = p.complete(req("b")).await.unwrap();
        assert_eq!(second.text(), "done");
        // Exhausted → last repeats.
        let third = p.complete(req("c")).await.unwrap();
        assert_eq!(third.text(), "done");
    }

    #[tokio::test]
    async fn stream_text_emits_message_start_deltas_final() {
        use futures::StreamExt;
        let p = MockProvider::stream_text("hello world", 3);
        let stream = p.stream(req("s")).await.unwrap();
        let events: Vec<StreamEvent> = stream.map(|e| e.expect("stream event ok")).collect().await;
        assert!(matches!(events[0], StreamEvent::MessageStart));
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas.concat(), "hello world");
        assert!(!deltas.is_empty());
        match events.last() {
            Some(StreamEvent::Final(resp)) => assert_eq!(resp.text(), "hello world"),
            other => panic!("expected Final last, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delayed_stream_waits_between_events() {
        use futures::StreamExt;
        let p = MockProvider::stream_text_delayed("cancel me", 3, 50);
        let mut stream = p.stream(req("s")).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            StreamEvent::MessageStart
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), stream.next())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), stream.next())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn stream_chunk_count_larger_than_text_is_safe() {
        use futures::StreamExt;
        let p = MockProvider::stream_text_delayed("x", 100, 0);
        let events: Vec<_> = p.stream(req("s")).await.unwrap().collect().await;
        assert_eq!(events.len(), 3);
        let p = MockProvider::stream_text("", 0);
        let events: Vec<_> = p.stream(req("s")).await.unwrap().collect().await;
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn default_stream_yields_single_final() {
        use futures::StreamExt;
        let p = MockProvider::fixed("streamed");
        let stream = p.stream(req("s")).await.unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(StreamEvent::Final(resp)) => {
                assert_eq!(resp.text(), "streamed");
            }
            other => panic!("expected Final, got {other:?}"),
        }
    }

    #[test]
    fn usage_estimation_is_deterministic() {
        let a = estimate_request(&req("hello world hello world"));
        let b = estimate_request(&req("hello world hello world"));
        assert_eq!(a, b);
        assert!(a >= 1);
    }
}
