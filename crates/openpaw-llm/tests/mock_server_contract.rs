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

fn capture_request(response: String) -> (String, std::sync::mpsc::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        let header_end = loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = find_header_end(&request) {
                break end;
            }
        };
        let length = parse_content_length(&request[..header_end]);
        while request.len() < header_end + length {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
        }
        tx.send(serde_json::from_slice(&request[header_end..header_end + length]).unwrap())
            .unwrap();
        socket.write_all(response.as_bytes()).unwrap();
    });
    (address, rx)
}

#[tokio::test]
async fn declared_default_output_is_omitted_and_explicit_http_budgets_are_preserved() {
    use futures::StreamExt;
    for streaming in [false, true] {
        for (use_default, output, expected) in [
            (true, 65536, None),
            (false, 16384, Some(16384)),
            (false, 0, Some(4096)),
        ] {
            let response = if streaming {
                http_sse(&[
                    r#"data: {"choices":[{"delta":{"content":"summary"},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])
            } else {
                http_json(
                    &json!({"choices":[{"message":{"role":"assistant","content":"summary"},"finish_reason":"stop"}]}),
                )
            };
            let (address, rx) = capture_request(response);
            let provider = OpenAiProvider::new("fixture", "known-proxy")
                .with_base_url(format!("http://{address}/v1"))
                .with_model_capabilities(std::collections::HashMap::from([(
                    "known-proxy".into(),
                    openpaw_core::Capabilities {
                        default_output_tokens: Some(65536),
                        ..Default::default()
                    },
                )]));
            let request = CompletionRequest {
                model: "known-proxy".into(),
                messages: vec![Message::user_text("summarize")],
                max_tokens: output,
                use_provider_default_output: use_default,
                ..Default::default()
            };
            if streaming {
                let events: Vec<_> = provider.stream(request).await.unwrap().collect().await;
                assert!(events.iter().all(Result::is_ok));
                assert!(events
                    .iter()
                    .any(|e| matches!(e, Ok(StreamEvent::Final(_)))));
            } else {
                assert_eq!(provider.complete(request).await.unwrap().text(), "summary");
            }
            let body = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(body["stream"], streaming);
            assert_eq!(body.get("max_tokens").and_then(|v| v.as_u64()), expected);
            assert_eq!(body.get("max_tokens").is_none(), use_default);
            assert!(body.get("use_provider_default_output").is_none());
        }
    }
}

#[tokio::test]
async fn undeclared_or_underreserved_provider_defaults_fail_before_http() {
    for (declared, model, reserve) in [
        (None, "known", 65536),
        (Some(0), "known", 65536),
        (Some(65536), "known", 2048),
        (Some(65536), "other-model", 65536),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let provider = OpenAiProvider::new("fixture", "known")
            .with_base_url(format!("http://{}/v1", listener.local_addr().unwrap()))
            .with_model_capabilities(std::collections::HashMap::from([(
                "known".into(),
                openpaw_core::Capabilities {
                    default_output_tokens: declared,
                    ..Default::default()
                },
            )]));
        let request = CompletionRequest {
            model: model.into(),
            max_tokens: reserve,
            use_provider_default_output: true,
            ..Default::default()
        };
        let error = provider.complete(request.clone()).await.unwrap_err();
        assert!(error.to_string().contains("默认输出额度"));
        assert!(provider.stream(request).await.is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn provider_default_output_is_scoped_to_the_official_endpoint_and_exact_model() {
    for (url, expected) in [
        ("https://api.deepseek.com", Some(65536)),
        ("https://api.deepseek.com/v1/", Some(65536)),
        ("https://api.deepseek.com:443/v1", Some(65536)),
        ("http://api.deepseek.com/v1", None),
        ("https://api.deepseek.com:444/v1", None),
        ("https://api.deepseek.com/other", None),
        ("https://api.deepseek.com.example/v1", None),
        ("https://proxy.example/deepseek", None),
    ] {
        let provider = OpenAiProvider::new("fixture", "deepseek-flash").with_base_url(url);
        assert_eq!(
            provider
                .model_capabilities("deepseek-flash")
                .default_output_tokens,
            expected,
            "{url}"
        );
        assert_eq!(
            provider
                .model_capabilities("deepseek-chat")
                .default_output_tokens,
            None
        );
    }
    let provider = OpenAiProvider::new("fixture", "deepseek-flash")
        .with_base_url("https://api.deepseek.com/v1");
    let declared = openpaw_core::Capabilities {
        context_window: 128000,
        ..Default::default()
    };
    let provider = provider.with_model_capabilities(std::collections::HashMap::from([(
        "deepseek-flash".into(),
        declared,
    )]));
    let caps = provider.model_capabilities("deepseek-flash");
    assert_eq!(caps.context_window, 128000);
    assert_eq!(caps.default_output_tokens, Some(65536));
    let provider = provider.with_model_capabilities(std::collections::HashMap::from([(
        "deepseek-flash".into(),
        openpaw_core::Capabilities {
            default_output_tokens: Some(8192),
            ..declared
        },
    )]));
    assert_eq!(
        provider
            .model_capabilities("deepseek-flash")
            .default_output_tokens,
        Some(8192)
    );
}

#[test]
fn configured_default_output_requires_a_positive_openai_declaration() {
    use openpaw_llm::config::ConfigSnapshot;
    for value in ["0", "-1", "4294967296", "\"invalid\"", "1.5"] {
        let snapshot = ConfigSnapshot::from_pairs([(
            "OPENPAW_MODEL_CAPABILITIES".into(),
            format!(
                r#"{{"openai":{{"fixture":{{"input":["text"],"default_output_tokens":{value}}}}}}}"#
            ),
        )]);
        assert!(snapshot.capabilities("openai", "fixture").is_err());
    }
    for protocol in ["anthropic", "echo", "mock"] {
        let snapshot = ConfigSnapshot::from_pairs([(
            "OPENPAW_MODEL_CAPABILITIES".into(),
            format!(
                r#"{{"{protocol}":{{"fixture":{{"input":["text"],"default_output_tokens":8192}}}}}}"#
            ),
        )]);
        assert!(snapshot.capabilities(protocol, "fixture").is_err());
    }
    let snapshot = ConfigSnapshot::from_pairs([
        ("OPENAI_API_KEY".into(), "fixture".into()),
        ("OPENPAW_OPENAI_BASE_URL".into(), "https://proxy.example/v1".into()),
        ("OPENPAW_OPENAI_MODEL".into(), "fixture".into()),
        ("OPENPAW_MAX_OUTPUT_TOKENS".into(), "16384".into()),
        ("OPENPAW_MODEL_CAPABILITIES".into(), r#"{"openai":{"fixture":{"input":["text"],"context_window":128000,"default_output_tokens":65536}}}"#.into()),
    ]);
    let config = snapshot.resolve("openai", Default::default()).unwrap();
    assert_eq!(config.max_output_tokens, 16384);
    assert_eq!(
        config
            .build()
            .model_capabilities("fixture")
            .default_output_tokens,
        Some(65536)
    );
}

#[tokio::test]
async fn images_and_tool_media_are_encoded_in_actual_http_requests() {
    let media = openpaw_core::InlineMedia::from_bytes(
        b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;",
        "image/gif",
        Some("test.gif".into()),
    )
    .unwrap();
    let mut animation = media.bytes().unwrap();
    animation.pop();
    animation.extend_from_slice(&media.bytes().unwrap()[19..]);
    let media = openpaw_core::InlineMedia::from_bytes(&animation, "image/gif", None).unwrap();
    let expected_data = media.model_data().unwrap();
    let expected_url = format!("data:image/gif;base64,{expected_data}");
    for anthropic in [false, true] {
        let response = if anthropic {
            json!({"id":"fixture","type":"message","role":"assistant","model":"vision","content":[{"type":"text","text":"contract reply"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}})
        } else {
            json!({"id":"fixture","choices":[{"message":{"role":"assistant","content":"contract reply"},"finish_reason":"stop"}]})
        };
        let (address, rx) = capture_request(http_json(&response));
        let caps = std::collections::HashMap::from([(
            "vision".into(),
            openpaw_core::Capabilities {
                image_input: true,
                ..Default::default()
            },
        )]);
        let provider: Box<dyn LlmProvider> = if anthropic {
            Box::new(
                AnthropicProvider::new("fixture-key", "vision")
                    .with_base_url(format!("http://{address}"))
                    .with_model_capabilities(caps),
            )
        } else {
            Box::new(
                OpenAiProvider::new("fixture-key", "vision")
                    .with_base_url(format!("http://{address}/v1"))
                    .with_model_capabilities(caps),
            )
        };
        let mut user = Message::user_text("before");
        user.content.push(media.block());
        user.content.push(ContentBlock::Text {
            text: "after".into(),
        });
        let assistant = Message {
            role: openpaw_core::Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "read_file".into(),
                input: json!({"path":"test.gif"}),
            }],
            ..Default::default()
        };
        let tool = Message {
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                name: Some("read_file".into()),
                content: "original".into(),
                is_error: false,
                protected: false,
                media: vec![media.clone()],
            }],
            ..Default::default()
        };
        assert_eq!(
            provider
                .complete(CompletionRequest {
                    model: "vision".into(),
                    messages: vec![user, assistant, tool],
                    ..Default::default()
                })
                .await
                .unwrap()
                .text(),
            "contract reply"
        );
        let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["text"], "before");
        assert_eq!(messages[0]["content"][2]["text"], "after");
        if anthropic {
            assert_eq!(messages[0]["content"][1]["source"]["data"], expected_data);
            assert_eq!(messages[2]["content"][0]["tool_use_id"], "call-1");
            assert_eq!(
                messages[2]["content"][0]["content"][1]["source"]["data"],
                expected_data
            );
            assert!(messages[0].get("uuid").is_none());
        } else {
            assert_eq!(messages[0]["content"][1]["image_url"]["url"], expected_url);
            assert_eq!(messages[2]["role"], "tool");
            assert_eq!(messages[2]["tool_call_id"], "call-1");
            assert_eq!(messages[3]["content"][1]["image_url"]["url"], expected_url);
        }
    }
}

#[tokio::test]
async fn native_wav_and_mp3_inputs_use_chat_and_model_overrides_are_checked() {
    for (bytes, mime, format) in [
        (b"RIFF0000WAVEfixture".as_slice(), "audio/wav", "wav"),
        (b"ID3fixture".as_slice(), "audio/mpeg", "mp3"),
    ] {
        let media = openpaw_core::InlineMedia::from_bytes(bytes, mime, None).unwrap();
        let (address, rx) = capture_request(http_json(
            &json!({"choices":[{"message":{"role":"assistant","content":"audio contract"},"finish_reason":"stop"}]}),
        ));
        let provider = OpenAiProvider::new("fixture", "audio")
            .with_base_url(format!("http://{address}/v1"))
            .with_model_capabilities(std::collections::HashMap::from([(
                "audio".into(),
                openpaw_core::Capabilities {
                    audio_input: true,
                    ..Default::default()
                },
            )]));
        let mut user = Message::default();
        user.content.push(media.block());
        let req = CompletionRequest {
            model: "other-text-model".into(),
            messages: vec![user.clone()],
            ..Default::default()
        };
        assert!(provider
            .complete(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not support"));
        provider
            .complete(CompletionRequest {
                model: "audio".into(),
                messages: vec![user],
                ..Default::default()
            })
            .await
            .unwrap();
        let request = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(
            request["messages"][0]["content"][0]["input_audio"]["data"],
            media.data
        );
        assert_eq!(
            request["messages"][0]["content"][0]["input_audio"]["format"],
            format
        );
        assert!(request.get("audio").is_none());
    }
}

#[tokio::test]
async fn media_provider_errors_remove_upstream_echo_for_complete_and_stream() {
    let media = openpaw_core::InlineMedia::from_bytes(b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;", "image/gif", None).unwrap();
    for anthropic in [false, true] {
        for stream in [false, true] {
            let echo = format!("private-echo {}", media.data);
            let response = format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{echo}",echo.len());
            let (address, rx) = capture_request(response);
            let caps = std::collections::HashMap::from([(
                "vision".into(),
                openpaw_core::Capabilities {
                    image_input: true,
                    ..Default::default()
                },
            )]);
            let provider: Box<dyn LlmProvider> = if anthropic {
                Box::new(
                    AnthropicProvider::new("fixture", "vision")
                        .with_base_url(format!("http://{address}"))
                        .with_model_capabilities(caps),
                )
            } else {
                Box::new(
                    OpenAiProvider::new("fixture", "vision")
                        .with_base_url(format!("http://{address}/v1"))
                        .with_model_capabilities(caps),
                )
            };
            let request = CompletionRequest {
                model: "vision".into(),
                messages: vec![Message {
                    content: vec![media.block()],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let error = if stream {
                match provider.stream(request).await {
                    Err(error) => error,
                    Ok(_) => panic!("expected failure"),
                }
            } else {
                provider.complete(request).await.unwrap_err()
            };
            assert!(matches!(
                error,
                openpaw_core::Error::AuthFailed { status: 401, .. }
            ));
            assert!(
                !error.to_string().contains("private-echo")
                    && !error.to_string().contains(&media.data)
            );
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        }
    }
}
