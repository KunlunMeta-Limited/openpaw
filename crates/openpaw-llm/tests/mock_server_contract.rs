//! Mock-server contract tests (plan §3.3 acceptance shape): non-streaming
//! / streaming / tool_use matrix across the two landed network providers,
//! driven by a hand-written HTTP mock (no extra dependencies).
use openpaw_core::{CompletionRequest, ContentBlock, Error, LlmProvider, Message, StreamEvent};
use openpaw_llm::{AnthropicProvider, OpenAiProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
/// Spawn a mock HTTP server that answers each accepted connection with the
/// next raw response in `responses`. Returns `host:port`.
fn spawn_mock(responses: Vec<String>) -> String {
    spawn_mock_bytes(responses.into_iter().map(String::into_bytes).collect())
}
fn spawn_mock_bytes(responses: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("addr").to_string();
    std::thread::spawn(move || {
        for resp in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read until end of headers, then drain the body.
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            let header_end = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break buf.len();
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = find_header_end(&buf) {
                    break pos;
                }
            };
            let body_len = parse_content_length(&buf[..header_end]);
            while buf.len() < header_end + body_len {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let _ = stream.write_all(&resp);
            let _ = stream.flush();
        }
    });
    addr
}
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}
fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}
fn http_json(body: &serde_json::Value) -> String {
    let body = serde_json::to_string(body).unwrap();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn http_sse(frames: &[&str]) -> String {
    let payload = sse_payload(frames.iter().copied());
    String::from_utf8(http_sse_chunked(&payload, &[payload.len().max(1)])).unwrap()
}
fn sse_payload<'a>(frames: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut payload = Vec::new();
    for frame in frames {
        payload.extend_from_slice(frame.as_bytes());
        payload.extend_from_slice(b"\n\n");
    }
    payload
}
fn http_sse_chunked(payload: &[u8], chunk_sizes: &[usize]) -> Vec<u8> {
    assert!(!chunk_sizes.is_empty());
    let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    let mut offset = 0;
    let mut index = 0;
    while offset < payload.len() {
        let size = chunk_sizes[index % chunk_sizes.len()].max(1);
        let end = offset.saturating_add(size).min(payload.len());
        response.extend_from_slice(format!("{:x}\r\n", end - offset).as_bytes());
        response.extend_from_slice(&payload[offset..end]);
        response.extend_from_slice(b"\r\n");
        offset = end;
        index += 1;
    }
    response.extend_from_slice(b"0\r\n\r\n");
    response
}
fn http_sse_close(payload: &[u8]) -> Vec<u8> {
    let mut response =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n".to_vec();
    response.extend_from_slice(payload);
    response
}
fn deterministic_chunk_sizes(count: usize) -> Vec<usize> {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 32) as usize % 1024) + 1
        })
        .collect()
}
fn long_text_utf16(target_units: usize) -> String {
    const PATTERN: &str = "ASCII 中文 😀 e\u{301}\r\n# Markdown\n```rust\nfn main() { println!(\"你好🌍\"); }\n```\n{\"tool\":{\"path\":\"C:\\\\tmp\\\\文件.rs\"}}\n";
    let mut output = String::with_capacity(target_units);
    let mut remaining = target_units;
    let mut chars = PATTERN.chars().cycle();
    while remaining > 0 {
        let ch = chars.next().expect("cycled pattern");
        let units = ch.len_utf16();
        if units <= remaining {
            output.push(ch);
            remaining -= units;
        }
    }
    assert_eq!(output.encode_utf16().count(), target_units);
    output
}
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
fn openai_text_payload(text: &str) -> Vec<u8> {
    let data = serde_json::to_string(&json!({
        "choices": [{"delta": {"content": text}}]
    }))
    .unwrap();
    let finish = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
    let frames = [
        format!("data: {data}"),
        finish.to_string(),
        "data: [DONE]".to_string(),
    ];
    sse_payload(frames.iter().map(String::as_str))
}
fn anthropic_text_payload(text: &str) -> Vec<u8> {
    let delta = serde_json::to_string(&json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": text}
    }))
    .unwrap();
    let frames = [
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":1}}}"#.to_string(),
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#.to_string(),
        format!("data: {delta}"),
        r#"data: {"type":"content_block_stop","index":0}"#.to_string(),
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#.to_string(),
        r#"data: {"type":"message_stop"}"#.to_string(),
    ];
    sse_payload(frames.iter().map(String::as_str))
}
fn http_error(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn req(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: model.to_string(),
        max_tokens: 100,
        messages: vec![Message::user_text("hi")],
        ..Default::default()
    }
}
async fn collect(provider_name: &str, response: Vec<u8>) -> Vec<openpaw_core::Result<StreamEvent>> {
    use futures::StreamExt;
    let addr = spawn_mock_bytes(vec![response]);
    match provider_name {
        "openai" => {
            OpenAiProvider::new("sk-test", "gpt-4o")
                .with_base_url(format!("http://{addr}"))
                .stream(req("gpt-4o"))
                .await
                .expect("openai stream starts")
                .collect()
                .await
        }
        _ => {
            AnthropicProvider::new("sk-test", "claude-sonnet-4")
                .with_base_url(format!("http://{addr}"))
                .stream(req("claude-sonnet-4"))
                .await
                .expect("anthropic stream starts")
                .collect()
                .await
        }
    }
}
fn assert_failed_without_final(events: &[openpaw_core::Result<StreamEvent>], label: &str) {
    let has_final = events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::Final(_))));
    assert!(
        events.iter().any(Result::is_err) && !has_final,
        "{label}: invalid terminal stream"
    );
}
fn final_text(events: &[openpaw_core::Result<StreamEvent>], label: &str) -> String {
    let final_response = events.iter().find_map(|event| match event {
        Ok(StreamEvent::Final(response)) => Some(response.as_ref()),
        _ => None,
    });
    assert!(
        events.iter().all(Result::is_ok),
        "{label}: successful stream contained an error"
    );
    final_response
        .expect("successful stream must have one Final")
        .text()
}
fn padded_data_frame(json: &str, data_len: usize) -> Vec<u8> {
    assert!(data_len > json.len());
    let padding = data_len - json.len() - 1;
    let first_padding = padding / 2;
    let second_padding = padding - first_padding;
    let mut frame = Vec::with_capacity(data_len + 16);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(json.as_bytes());
    frame.extend(std::iter::repeat_n(b' ', first_padding));
    frame.extend_from_slice(b"\ndata: ");
    frame.extend(std::iter::repeat_n(b' ', second_padding));
    frame.extend_from_slice(b"\n\n");
    frame
}
// ── Anthropic ────────────────────────────────────────────────────────────
#[tokio::test]
async fn anthropic_non_streaming_contract() {
    let addr = spawn_mock(vec![http_json(&json!({
        "content": [{"type": "text", "text": "hello from anthropic"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 11, "output_tokens": 4},
    }))]);
    let p = AnthropicProvider::new("sk-test", "claude-sonnet-4")
        .with_base_url(format!("http://{addr}"));
    let resp = p.complete(req("claude-sonnet-4")).await.unwrap();
    assert_eq!(resp.text(), "hello from anthropic");
    assert_eq!(resp.usage.input_tokens, 11);
}
#[tokio::test]
async fn anthropic_streaming_tool_use_contract() {
    let addr = spawn_mock(vec![http_sse(&[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"grep\"}}",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pattern\\\":\\\"TODO\\\"}\"}}",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ])]);
    let p = AnthropicProvider::new("sk-test", "claude-sonnet-4")
        .with_base_url(format!("http://{addr}"));
    use futures::StreamExt;
    let stream = p.stream(req("claude-sonnet-4")).await.unwrap();
    let events: Vec<StreamEvent> = stream.map(|e| e.expect("event ok")).collect().await;
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::MessageStart)));
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "grep")));
    match events.last() {
        Some(StreamEvent::Final(resp)) => {
            assert_eq!(resp.stop_reason, openpaw_core::StopReason::ToolUse);
            assert!(matches!(
                resp.content[0],
                ContentBlock::ToolUse { ref input, .. } if input["pattern"] == json!("TODO")
            ));
        }
        other => panic!("expected Final, got {other:?}"),
    }
}
#[tokio::test]
async fn anthropic_auth_error_classified() {
    let addr = spawn_mock(vec![http_error(
        "401 Unauthorized",
        "{\"error\":{\"message\":\"invalid x-api-key\"}}",
    )]);
    let p = AnthropicProvider::new("bad-key", "claude-sonnet-4")
        .with_base_url(format!("http://{addr}"));
    let err = p.complete(req("claude-sonnet-4")).await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed { .. }), "got: {err:?}");
}
// ── OpenAI ───────────────────────────────────────────────────────────────
#[tokio::test]
async fn openai_non_streaming_tool_calls_contract() {
    let addr = spawn_mock(vec![http_json(&json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "reasoning_content": "需要先读取文件",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
    }))]);
    let p = OpenAiProvider::new("sk-test", "gpt-4o").with_base_url(format!("http://{addr}"));
    let resp = p.complete(req("gpt-4o")).await.unwrap();
    assert_eq!(resp.stop_reason, openpaw_core::StopReason::ToolUse);
    assert!(matches!(
        resp.content[1],
        ContentBlock::ToolUse { ref name, ref input, .. }
            if name == "read_file" && input["path"] == json!("a.rs")
    ));
    assert!(matches!(
        resp.content[0],
        ContentBlock::Thinking { ref thinking, .. } if thinking == "需要先读取文件"
    ));
    assert_eq!(resp.usage.input_tokens, 7);
}
#[tokio::test]
async fn openai_streaming_text_contract() {
    let addr = spawn_mock(vec![http_sse(&[
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Plan\"}}]}",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}",
        "data: [DONE]",
    ])]);
    let p = OpenAiProvider::new("sk-test", "gpt-4o").with_base_url(format!("http://{addr}"));
    use futures::StreamExt;
    let stream = p.stream(req("gpt-4o")).await.unwrap();
    let events: Vec<StreamEvent> = stream.map(|e| e.expect("event ok")).collect().await;
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.concat(), "Hello");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ThinkingDelta { text, .. } if text == "Plan"
    )));
    match events.last() {
        Some(StreamEvent::Final(resp)) => {
            assert_eq!(resp.stop_reason, openpaw_core::StopReason::EndTurn);
            assert_eq!(resp.text(), "Hello");
        }
        other => panic!("expected Final, got {other:?}"),
    }
}
#[tokio::test]
async fn openai_streaming_error_frame_is_not_an_empty_success() {
    let addr = spawn_mock(vec![http_sse(&[
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}",
        "data: {\"error\":{\"type\":\"service_unavailable_error\",\"message\":\"Service is too busy.\"}}",
    ])]);
    let p = OpenAiProvider::new("sk-test", "gpt-4o").with_base_url(format!("http://{addr}"));
    use futures::StreamExt;
    let stream = p.stream(req("gpt-4o")).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(events.iter().any(|event| matches!(
        event,
        Err(Error::ServerError { status: 503, message })
            if message.contains("流式请求失败") && message.contains("too busy")
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::Final(_)))));
}
#[tokio::test]
async fn openai_rate_limit_classified() {
    let addr = spawn_mock(vec![http_error(
        "429 Too Many Requests",
        "{\"error\":{\"message\":\"rate limited\"}}",
    )]);
    let p = OpenAiProvider::new("sk", "gpt-4o").with_base_url(format!("http://{addr}"));
    let err = p.complete(req("gpt-4o")).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }), "got: {err:?}");
}
#[tokio::test]
async fn streaming_long_text_survives_fixed_and_random_http_chunks() {
    let fixed = [1usize, 2, 3, 4, 7, 31, 1024];
    let random = deterministic_chunk_sizes(257);
    for (units, expected_hash) in [
        (50_000, 0x38b6_32d3_0569_ef10_u64),
        (180_000, 0x52be_7987_619a_89a5_u64),
    ] {
        let text = long_text_utf16(units);
        assert_eq!(fnv1a64(text.as_bytes()), expected_hash);
        assert!(!text.contains('\u{fffd}'));
        for sizes in fixed
            .iter()
            .map(std::slice::from_ref)
            .chain(std::iter::once(random.as_slice()))
        {
            let openai = collect(
                "openai",
                http_sse_chunked(&openai_text_payload(&text), sizes),
            )
            .await;
            let openai_text = final_text(&openai, &format!("openai/{units}/{sizes:?}"));
            assert_eq!(openai_text, text);
            assert_eq!(fnv1a64(openai_text.as_bytes()), expected_hash);
            assert!(!openai_text.contains('\u{fffd}'));
            let anthropic = collect(
                "anthropic",
                http_sse_chunked(&anthropic_text_payload(&text), sizes),
            )
            .await;
            let anthropic_text = final_text(&anthropic, &format!("anthropic/{units}/{sizes:?}"));
            assert_eq!(anthropic_text, text);
            assert_eq!(fnv1a64(anthropic_text.as_bytes()), expected_hash);
            assert!(!anthropic_text.contains('\u{fffd}'));
        }
    }
}
#[tokio::test]
async fn streaming_clean_eof_at_25_50_99_percent_never_finalizes() {
    for units in [50_000, 180_000] {
        let text = long_text_utf16(units);
        for (provider, payload) in [
            ("openai", openai_text_payload(&text)),
            ("anthropic", anthropic_text_payload(&text)),
        ] {
            for percent in [25usize, 50, 99] {
                let end = payload.len() * percent / 100;
                let response = http_sse_close(&payload[..end]);
                let events = collect(provider, response).await;
                assert_failed_without_final(&events, &format!("{provider}/{units}/eof-{percent}"));
            }
        }
    }
}
#[tokio::test]
async fn openai_terminal_ordering_matrix_rejects_invalid_streams() {
    let finish = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
    let extra = r#"data: {"choices":[{"delta":{"content":"late"}}]}"#;
    let error = r#"data: {"error":{"message":"late failure"}}"#;
    let cases = [
        ("missing-done", vec![finish]),
        (
            "duplicate-done",
            vec![finish, "data: [DONE]", "data: [DONE]"],
        ),
        ("data-after-done", vec![finish, "data: [DONE]", extra]),
        ("error-after-done", vec![finish, "data: [DONE]", error]),
        ("missing-finish-reason", vec!["data: [DONE]"]),
        (
            "aborted-generation",
            vec![
                r#"data: {"choices":[{"delta":{},"finish_reason":"aborted"}]}"#,
                "data: [DONE]",
            ],
        ),
        (
            "insufficient-resource",
            vec![
                r#"data: {"choices":[{"delta":{},"finish_reason":"insufficient_system_resource"}]}"#,
                "data: [DONE]",
            ],
        ),
        (
            "unsupported-finish-reason",
            vec![
                r#"data: {"choices":[{"delta":{},"finish_reason":"content_filter"}]}"#,
                "data: [DONE]",
            ],
        ),
    ];
    for (label, frames) in cases {
        let payload = sse_payload(frames);
        let events = collect("openai", http_sse_chunked(&payload, &[7])).await;
        assert_failed_without_final(&events, label);
    }
}
#[tokio::test]
async fn anthropic_terminal_ordering_matrix_rejects_invalid_streams() {
    let start = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":1}}}"#;
    let stop = r#"data: {"type":"message_stop"}"#;
    let ping = r#"data: {"type":"ping"}"#;
    let error = r#"data: {"type":"error","error":{"message":"late failure"}}"#;
    let cases = [
        ("missing-message-stop", vec![start]),
        ("duplicate-message-stop", vec![start, stop, stop]),
        ("data-after-message-stop", vec![start, stop, ping]),
        ("error-after-message-stop", vec![start, stop, error]),
        ("done-is-invalid", vec![start, "data: [DONE]"]),
    ];
    for (label, frames) in cases {
        let payload = sse_payload(frames);
        let events = collect("anthropic", http_sse_chunked(&payload, &[31])).await;
        assert_failed_without_final(&events, label);
    }
}
#[tokio::test]
async fn incomplete_tool_arguments_fail_without_final() {
    let openai = sse_payload([
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\""}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]);
    let events = collect("openai", http_sse_chunked(&openai, &[3])).await;
    assert_failed_without_final(&events, "openai-incomplete-tool-json");
    let anthropic = sse_payload([
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file"}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\""}}"#,
        r#"data: {"type":"content_block_stop","index":0}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        r#"data: {"type":"message_stop"}"#,
    ]);
    let events = collect("anthropic", http_sse_chunked(&anthropic, &[4])).await;
    assert_failed_without_final(&events, "anthropic-incomplete-tool-json");
}
#[tokio::test]
async fn provider_http_data_frame_enforces_two_mib_boundary() {
    const LIMIT: usize = 2 * 1024 * 1024;
    let openai_ping = r#"{"choices":[]}"#;
    let openai_suffix = sse_payload([
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]);
    let anthropic_ping = r#"{"type":"ping"}"#;
    let anthropic_suffix = sse_payload([r#"data: {"type":"message_stop"}"#]);
    for (provider, json, suffix) in [
        ("openai", openai_ping, openai_suffix),
        ("anthropic", anthropic_ping, anthropic_suffix),
    ] {
        let mut exact = padded_data_frame(json, LIMIT);
        exact.extend_from_slice(&suffix);
        let events = collect(provider, http_sse_chunked(&exact, &[1024])).await;
        let text = final_text(&events, &format!("{provider}/2mib-exact"));
        assert!(text.is_empty());
        let mut over = padded_data_frame(json, LIMIT + 1);
        over.extend_from_slice(&suffix);
        let events = collect(provider, http_sse_chunked(&over, &[1024])).await;
        assert_failed_without_final(&events, &format!("{provider}/2mib-over"));
    }
}
#[tokio::test]
async fn provider_http_rejects_malformed_json_and_truncated_utf8() {
    let malformed = b"data: {\"broken\":\n\n".to_vec();
    let truncated = [
        ("two-byte", vec![0xc2]),
        ("three-byte", vec![0xe4, 0xb8]),
        ("four-byte", vec![0xf0, 0x9f, 0x98]),
    ];
    for provider in ["openai", "anthropic"] {
        let events = collect(provider, http_sse_close(&malformed)).await;
        assert_failed_without_final(&events, &format!("{provider}/malformed-json"));
        for (label, tail) in &truncated {
            let mut payload = b"data: {\"text\":\"".to_vec();
            payload.extend_from_slice(tail);
            let events = collect(provider, http_sse_close(&payload)).await;
            assert_failed_without_final(&events, &format!("{provider}/{label}-truncated"));
        }
    }
}
