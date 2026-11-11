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

fn http_encoded(content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

fn gb2312_page(declaration: &str) -> Vec<u8> {
    // “北京地铁”的真实GB2312字节；不通过被测解码库反向生成预期。
    let mut body = format!("<html><head>{declaration}</head><body><h1>").into_bytes();
    body.extend_from_slice(b"\xb1\xb1\xbe\xa9\xb5\xd8\xcc\xfa");
    body.extend_from_slice(b"</h1></body></html>");
    body
}

#[tokio::test]
async fn r38_declared_charsets_decode_strictly_and_keep_success_cache_and_fresh() {
    let _guard = lock_env();
    set_gate();
    let utf16 = "<h1>北京地铁</h1>"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut bom_utf16 = vec![0xff, 0xfe];
    bom_utf16.extend_from_slice(&utf16);
    let mut bom_utf16be = vec![0xfe, 0xff];
    bom_utf16be.extend(
        "<h1>北京地铁</h1>"
            .encode_utf16()
            .flat_map(u16::to_be_bytes),
    );
    let cases = [
        ("http", "text/html; charset=gb2312", gb2312_page(""), "北京地铁"),
        ("meta", "text/html", gb2312_page("<meta charset='gb2312'>"), "北京地铁"),
        ("equiv", "text/html", gb2312_page("<meta content='text/html; charset=GB2312' http-equiv='Content-Type'>"), "北京地铁"),
        ("aliases", "text/html; charset=GBK", gb2312_page("<meta charset=gb2312>"), "北京地铁"),
        ("comment", "text/html", gb2312_page("<!-- <meta charset=bad-encoding> --><meta charset=gb2312>"), "北京地铁"),
        ("script", "text/html", gb2312_page("<script>const example='<meta charset=bad-encoding>';</script><meta charset=gb2312>"), "北京地铁"),
        ("utf8", "text/html; charset=UTF-8", "<h1>北京地铁</h1>".as_bytes().to_vec(), "北京地铁"),
        ("utf8-original", "text/html; charset=GBK", "<h1>北京地铁</h1>".as_bytes().to_vec(), "北京地铁"),
        ("bom-utf8", "text/html", b"\xef\xbb\xbf<h1>UTF8</h1>".to_vec(), "UTF8"),
        ("bom-utf16", "text/html", bom_utf16, "北京地铁"),
        ("bom-utf16be", "text/html", bom_utf16be, "北京地铁"),
        ("http-utf16", "text/html; charset=utf-16le", utf16, "北京地铁"),
        ("latin", "text/html; charset=windows-1252", b"<h1>caf\xe9</h1>".to_vec(), "café"),
    ];
    for (name, content_type, body, expected) in cases {
        let addr = spawn_mock_bytes(vec![
            http_encoded(content_type, &body),
            http_html("<h1>Fresh replacement</h1>").into_bytes(),
        ]);
        let input = json!({"url":format!("http://{addr}/{name}")});
        let first = web_fetch::run(Path::new("."), input.clone()).await.unwrap();
        assert!(!first.is_error, "{name}: {}", first.content);
        assert!(
            first.content.contains(expected),
            "{name}: {}",
            first.content
        );
        assert!(!first.content.contains('\u{fffd}'));
        let cached = web_fetch::run(Path::new("."), input.clone()).await.unwrap();
        assert_eq!(
            cached.content, first.content,
            "{name}: original success cache must remain"
        );
        let mut fresh = input;
        fresh["fresh"] = json!(true);
        let fresh = web_fetch::run(Path::new("."), fresh).await.unwrap();
        assert!(!fresh.is_error, "{name}: {}", fresh.content);
        assert!(fresh.content.contains("Fresh replacement"));
        assert!(fresh.content.contains("retrieval_mode: fresh"));
    }
    clear_gate();
}

#[tokio::test]
async fn r38_unknown_conflicting_damaged_and_binary_text_is_refused_and_not_cached() {
    let _guard = lock_env();
    set_gate();
    let mut late = vec![b' '; 1024];
    late.extend(gb2312_page("<meta charset=gb2312>"));
    let mut conflicting_utf16 = vec![0xff, 0xfe];
    conflicting_utf16.extend(
        "<meta charset=utf-8><h1>北京地铁</h1>"
            .encode_utf16()
            .flat_map(u16::to_le_bytes),
    );
    let cases = [
        ("no-declaration", "text/html", gb2312_page("")),
        (
            "unknown-http",
            "text/html; charset=unknown-charset",
            gb2312_page(""),
        ),
        (
            "unknown-on-utf8",
            "text/html; charset=unknown-charset",
            b"<h1>Valid UTF8</h1>".to_vec(),
        ),
        ("utf16-conflict", "text/html", conflicting_utf16),
        (
            "unknown-meta",
            "text/html",
            gb2312_page("<meta charset=unknown-charset>"),
        ),
        (
            "conflict",
            "text/html; charset=UTF-8",
            gb2312_page("<meta charset=GBK>"),
        ),
        (
            "duplicate-http",
            "text/html; charset=GBK; charset=UTF-8",
            gb2312_page(""),
        ),
        ("empty-http", "text/html; charset=", gb2312_page("")),
        (
            "conflict-meta",
            "text/html",
            gb2312_page("<meta charset=GBK><meta charset=UTF-8>"),
        ),
        (
            "bom-conflict",
            "text/html; charset=GBK",
            b"\xef\xbb\xbf<h1>UTF8</h1>".to_vec(),
        ),
        ("broken-gbk", "text/html; charset=GBK", b"<h1>\x81".to_vec()),
        (
            "binary",
            "text/html; charset=GBK",
            b"<h1>abc\0def</h1>".to_vec(),
        ),
        ("late-meta", "text/html", late),
        (
            "plain-meta",
            "text/plain",
            gb2312_page("<meta charset=GBK>"),
        ),
    ];
    for (name, content_type, body) in cases {
        let addr = spawn_mock_bytes(vec![
            http_encoded(content_type, &body),
            http_html("<h1>Recovered UTF8 page</h1>").into_bytes(),
        ]);
        let input = json!({"url":format!("http://{addr}/{name}")});
        let bad = web_fetch::run(Path::new("."), input.clone()).await.unwrap();
        assert!(bad.is_error, "{name}: {}", bad.content);
        assert_eq!(
            bad.structured.as_ref().unwrap()["kind"],
            "unsupported_encoding",
            "{name}"
        );
        let recovered = web_fetch::run(Path::new("."), input).await.unwrap();
        assert!(!recovered.is_error, "{name}: {}", recovered.content);
        assert!(recovered.content.contains("Recovered UTF8 page"));
    }
    clear_gate();
}

#[tokio::test]
async fn d175_bad_http_binary_empty_and_challenge_responses_are_not_cached_success() {
    let _guard = lock_env();
    set_gate();
    for (status, kind, body, expected) in [
        ("404 Not Found", "text/html", "missing", "http_status"),
        ("403 Forbidden", "text/html", "denied", "http_status"),
        (
            "429 Too Many Requests",
            "text/html",
            "limited",
            "http_status",
        ),
        ("200 OK", "image/jpeg", "binary", "unsupported_content"),
        (
            "200 OK",
            "text/html",
            "<html><body> </body></html>",
            "empty_content",
        ),
        (
            "200 OK",
            "text/html",
            "<title>Just a moment</title><script src=\"/cf-chl-a\"></script>",
            "challenge",
        ),
        ("200 OK", "text/html", "a\0b", "unsupported_encoding"),
    ] {
        let first=format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        let addr = spawn_mock(vec![first, http_html("<h1>Recovered actual page</h1>")]);
        let input = json!({"url":format!("http://{addr}/same")});
        let bad = web_fetch::run(Path::new("."), input.clone()).await.unwrap();
        assert!(bad.is_error, "{expected}: {}", bad.content);
        assert_eq!(bad.structured.as_ref().unwrap()["kind"], expected);
        let good = web_fetch::run(Path::new("."), input).await.unwrap();
        assert!(!good.is_error, "{}", good.content);
        assert!(good.content.contains("Recovered actual page"));
    }
    clear_gate();
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

#[tokio::test]
async fn d188_expanding_excerpt_reuses_body_and_fresh_replaces_all_sizes() {
    let _guard = lock_env();
    set_gate();
    // 每次真实连接消耗一个响应：fresh取得SECOND证明之前扩读没有重发HTTP。
    let addr = spawn_mock(vec![
        http_html("<p>正文😀第一版后续内容</p>"),
        http_html("<p>SECOND新版后续内容</p>"),
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        http_html("<p>THIRD不同查询</p>"),
    ]);
    let url = format!("http://{addr}/expanded");
    let first = web_fetch::run(Path::new("."), json!({"url":url,"max_chars":2}))
        .await
        .unwrap();
    assert_eq!(first.content, "正文\n... (truncated at 2 chars)");
    let expanded = web_fetch::run(Path::new("."), json!({"url":url,"max_chars":100}))
        .await
        .unwrap();
    assert!(!expanded.is_error);
    assert!(expanded.content.contains("正文😀第一版后续内容"));
    assert!(!expanded.content.contains("truncated"));
    let fresh = web_fetch::run(
        Path::new("."),
        json!({"url":url,"max_chars":3,"fresh":true}),
    )
    .await
    .unwrap();
    assert!(fresh.content.contains("SEC\n... (truncated at 3 chars)"));
    assert!(fresh.content.contains("retrieval_mode: fresh"));
    let expanded = web_fetch::run(Path::new("."), json!({"url":url,"max_chars":100}))
        .await
        .unwrap();
    assert!(expanded.content.contains("SECOND新版后续内容"));
    assert!(!expanded.content.contains("retrieval_mode: fresh"));
    let refused = web_fetch::run(Path::new("."), json!({"url":url,"fresh":true}))
        .await
        .unwrap();
    assert!(refused.is_error);
    let retained = web_fetch::run(Path::new("."), json!({"url":url,"max_chars":100}))
        .await
        .unwrap();
    assert_eq!(
        retained.content, expanded.content,
        "fresh失败不能缓存错误或伪造刷新成功"
    );
    let other = web_fetch::run(Path::new("."), json!({"url":format!("{url}?other=1")}))
        .await
        .unwrap();
    assert!(other.content.contains("THIRD不同查询"));
    clear_gate();
}

#[tokio::test]
async fn r7_fresh_bypasses_cache_without_changing_legacy_output() {
    let _guard = lock_env();
    set_gate();
    let addr = spawn_mock(vec![
        http_html("<h1>FIRST</h1>"),
        http_html("<h1>SECOND</h1>"),
    ]);
    let url = format!("http://{addr}/fresh");
    let first = web_fetch::run(Path::new("."), json!({"url":url}))
        .await
        .unwrap();
    let cached = web_fetch::run(Path::new("."), json!({"url":url}))
        .await
        .unwrap();
    let fresh = web_fetch::run(Path::new("."), json!({"url":url,"fresh":true}))
        .await
        .unwrap();
    clear_gate();
    assert_eq!(first.content, cached.content);
    assert!(
        fresh.content.contains("SECOND"),
        "fresh must fetch a new response: {}",
        fresh.content
    );
    assert!(fresh.content.contains(&url));
    assert!(fresh.content.contains("retrieved_at_unix_ms:"));
}

#[tokio::test]
async fn d41_unknown_length_body_is_rejected_before_end_of_stream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _guard = lock_env();
    set_gate();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/oversized", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 8192];
        let _ = stream.read(&mut request).await;
        if stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .is_err()
        {
            return;
        }
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..96 {
            if stream.write_all(b"10000\r\n").await.is_err()
                || stream.write_all(&chunk).await.is_err()
                || stream.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let _ = stream.write_all(b"0\r\n\r\n").await;
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        web_fetch::run(Path::new("."), json!({"url":url})),
    )
    .await;
    server.abort();
    let _ = server.await;
    clear_gate();
    let result = result
        .expect("oversized response must fail before waiting for its terminator")
        .unwrap();
    assert!(
        result.is_error && result.content.contains("too large"),
        "{}",
        result.content
    );
}
