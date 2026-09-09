//! End-to-end tests for `web_extract` against a hand-written mock HTTP
//! server bound on loopback — no real network access.
//!
//! The SSRF guard refuses loopback targets, so the fetch cases run with the
//! `OPENPAW_WEB_TEST_ALLOW_LOOPBACK=1` escape hatch (loopback-only allowance,
//! see web_extract::loopback_test_allowance) and the SSRF-rejection case
//! clears it again. All cases live in one test function because the env var
//! is process-global and cargo runs a binary's tests on parallel threads.

use std::io::{Read, Write};
use std::net::TcpListener;

use openpaw_core::ToolCallOutcome;
use openpaw_tools::web::web_extract;
use serde_json::json;

const LOOPBACK_HATCH: &str = "OPENPAW_WEB_TEST_ALLOW_LOOPBACK";

/// Fixture page: heading, paragraph, absolute + relative + duplicate links,
/// plus script/style blocks that must not leak into any output format.
const PAGE: &str = r#"<html>
<head><title>Mock Page</title><style>body { color: red; }</style></head>
<body>
<h1>Hello World</h1>
<p>This is a <b>test</b> paragraph.</p>
<a href="https://example.com/abs">Absolute</a>
<a href="/relative/path">Relative</a>
<a href="/dupe">Dupe</a>
<a href="/dupe">Dupe Again</a>
<script>alert('stripped')</script>
</body>
</html>"#;

/// Spawn a mock HTTP server that answers each accepted connection with the
/// next raw response in `responses`. Returns `127.0.0.1:port`.
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

async fn run_extract(args: serde_json::Value) -> ToolCallOutcome {
    web_extract::run(std::path::Path::new("."), args)
        .await
        .expect("run returns Ok for both success and failure outcomes")
}

#[tokio::test]
async fn web_extract_e2e_contract() {
    std::env::set_var(LOOPBACK_HATCH, "1");

    // Three fetches against the same page: markdown / links / summary.
    let addr = spawn_mock(vec![http_html(PAGE); 3]);
    let base = format!("http://{addr}");

    // 1. Markdown contract: headings preserved, links preserved, script stripped.
    let out = run_extract(json!({"url": base, "format": "markdown"})).await;
    assert!(!out.is_error, "markdown fetch failed: {}", out.content);
    assert!(
        out.content.contains("# Hello World"),
        "expected h1 as markdown heading, got: {}",
        out.content
    );
    assert!(
        out.content.contains("[Absolute](https://example.com/abs)"),
        "expected markdown link, got: {}",
        out.content
    );
    assert!(
        !out.content.contains("alert"),
        "script content must be stripped, got: {}",
        out.content
    );

    // 2. Links format: relative hrefs absolutized against the final URL,
    //    duplicates collapsed, first anchor text kept.
    let out = run_extract(json!({"url": base, "format": "links"})).await;
    assert!(!out.is_error, "links fetch failed: {}", out.content);
    assert!(
        out.content.contains("https://example.com/abs"),
        "got: {}",
        out.content
    );
    let absolute_relative = format!("{base}/relative/path");
    assert!(
        out.content.contains(&absolute_relative),
        "relative link must be absolutized to {absolute_relative}, got: {}",
        out.content
    );
    assert!(out.content.contains("[Relative]("));
    assert_eq!(
        out.content.matches("/dupe").count(),
        1,
        "duplicate href must collapse to one entry, got: {}",
        out.content
    );
    assert!(
        !out.content.contains("Dupe Again"),
        "first anchor text must win, got: {}",
        out.content
    );

    // 3. Summary is the default when format is omitted: markdown + links.
    let out = run_extract(json!({"url": base})).await;
    assert!(!out.is_error, "summary fetch failed: {}", out.content);
    assert!(
        out.content.contains("# Hello World"),
        "summary must contain the markdown part, got: {}",
        out.content
    );
    assert!(
        out.content.contains("## Links"),
        "summary must contain the links section, got: {}",
        out.content
    );
    assert!(out.content.contains("[Dupe]("));

    // 4. SSRF rejection: loopback is refused once the test hatch is cleared
    //    (validation fires before any connection is attempted).
    std::env::set_var(LOOPBACK_HATCH, "0");
    let out = run_extract(json!({"url": format!("{base}/ssrf")})).await;
    assert!(
        out.is_error,
        "127.0.0.1 target must yield an error outcome: {}",
        out.content
    );
    assert!(out.content.contains("refused"), "got: {}", out.content);
    std::env::set_var(LOOPBACK_HATCH, "1");

    // 5. Bad URL surfaces as an error outcome, never an Err return.
    let out = run_extract(json!({"url": "not a url"})).await;
    assert!(
        out.is_error,
        "bad URL must yield an error outcome: {}",
        out.content
    );
    assert!(out.content.contains("invalid URL"), "got: {}", out.content);
}
