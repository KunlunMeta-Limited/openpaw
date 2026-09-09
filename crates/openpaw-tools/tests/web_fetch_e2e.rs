//! web_fetch E2E — 用 std::net::TcpListener 手写 mock HTTP server 驱动，
//! 测试不访问真实互联网。
//!
//! 覆盖：抓取合同（GET 返回 HTML → 输出 Markdown）、SSRF 拒绝
//! （loopback 目标 → error outcome）、坏 URL → error outcome、
//! 逐跳手动重定向跟随（Location 头一跳）。
//!
//! mock server 参照 openpaw-llm/tests/mock_server_contract.rs 的
//! spawn_mock/http_json 手写风格：每接受一个连接，按顺序吐出下一条
//! 原始响应（响应带 Connection: close，迫使客户端每跳新建连接）。
//!
//! `await_holding_lock` 在本文件是有意为之：ENV_LOCK 是 std Mutex，
//! 持有它跨 `.await` 正是为了把「设门控 → run() → 清门控」整段串行化，
//! 防止并行测试互踩进程级 env。

// clippy 看到的是 std guard 跨 await；这里跨 await 是设计（见文件头注释）。
#![allow(clippy::await_holding_lock)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;

use openpaw_tools::web::web_fetch;
use serde_json::json;

/// env 门控是进程级共享的：并行测试用这把锁串行化
/// "设置/移除门控 + 调用 run()" 的临界区。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 与 web_fetch::run 约定的测试专用门控变量。
const TEST_ALLOW_LOOPBACK: &str = "OPENPAW_WEB_TEST_ALLOW_LOOPBACK";

fn set_gate() {
    std::env::set_var(TEST_ALLOW_LOOPBACK, "1");
}

fn clear_gate() {
    std::env::remove_var(TEST_ALLOW_LOOPBACK);
}

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Spawn a mock HTTP server that answers each accepted connection with the
/// next raw response in `responses`. Returns `host:port`.
fn spawn_mock(responses: Vec<String>) -> String {
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
            let _ = stream.write_all(resp.as_bytes());
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

fn http_html(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn http_redirect(location: &str) -> String {
    format!(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

// ── 合同路径：GET 返回 HTML → 输出 Markdown ─────────────────────────────

#[tokio::test]
async fn fetch_html_contract_returns_markdown() {
    let _guard = lock_env();
    set_gate();
    let addr = spawn_mock(vec![http_html(
        "<html><body><h1>Paw Docs</h1><p>Body text</p></body></html>",
    )]);
    let res = web_fetch::run(Path::new("."), json!({"url": format!("http://{addr}/")}))
        .await
        .unwrap();
    clear_gate();

    assert!(!res.is_error, "fetch should succeed, got: {}", res.content);
    assert!(
        res.content.contains("# Paw Docs"),
        "missing heading, got: {}",
        res.content
    );
    assert!(
        res.content.contains("Body text"),
        "missing body text, got: {}",
        res.content
    );
    assert!(
        !res.content.contains("<h1>"),
        "raw html leaked into output, got: {}",
        res.content
    );
}

// ── SSRF 拒绝：loopback 目标 → error outcome（门控关闭，完整校验路径）────

#[tokio::test]
async fn ssrf_rejects_loopback_target() {
    let _guard = lock_env();
    clear_gate();
    let res = web_fetch::run(Path::new("."), json!({"url": "http://127.0.0.1/"}))
        .await
        .unwrap();
    clear_gate();

    assert!(
        res.is_error,
        "loopback target must be an error outcome, got: {}",
        res.content
    );
    assert!(
        res.content.contains("refused"),
        "expected SSRF refusal message, got: {}",
        res.content
    );
}

// ── 坏 URL → error outcome ───────────────────────────────────────────────

#[tokio::test]
async fn bad_url_is_error_outcome() {
    // URL 解析发生在门控之前，本用例与门控状态无关。
    let res = web_fetch::run(Path::new("."), json!({"url": "not a valid url"}))
        .await
        .unwrap();

    assert!(
        res.is_error,
        "bad URL must be an error outcome, got: {}",
        res.content
    );
    assert!(
        res.content.contains("invalid URL"),
        "expected invalid URL message, got: {}",
        res.content
    );
}

// ── 重定向跟随：Location 头一跳到 mock 页面 ─────────────────────────────

#[tokio::test]
async fn redirect_is_followed_to_mock_page() {
    let _guard = lock_env();
    set_gate();
    // 第一个连接：301 + Location: /final；第二个连接：最终页面。
    let addr = spawn_mock(vec![
        http_redirect("/final"),
        http_html("<html><body><h2>Landed</h2></body></html>"),
    ]);
    let res = web_fetch::run(
        Path::new("."),
        json!({"url": format!("http://{addr}/start")}),
    )
    .await
    .unwrap();
    clear_gate();

    assert!(
        !res.is_error,
        "redirect should be followed to the mock page, got: {}",
        res.content
    );
    assert!(
        res.content.contains("## Landed"),
        "expected markdown of the redirect target page, got: {}",
        res.content
    );
}
