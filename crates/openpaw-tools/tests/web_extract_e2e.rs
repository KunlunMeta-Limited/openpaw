//! web_extract 离线格式、链接归一化和 SSRF 合同。
mod common;

use common::{http_html, spawn_mock};
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
async fn run_extract(args: serde_json::Value) -> ToolCallOutcome {
    web_extract::run(std::path::Path::new("."), args)
        .await
        .expect("run returns Ok for both success and failure outcomes")
}
#[tokio::test]
async fn web_extract_e2e_contract() {
    std::env::set_var(LOOPBACK_HATCH, "1");
    let addr = spawn_mock(vec![http_html(PAGE); 3]);
    let base = format!("http://{addr}");
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
    std::env::set_var(LOOPBACK_HATCH, "0");
    let out = run_extract(json!({"url": format!("{base}/ssrf")})).await;
    assert!(
        out.is_error,
        "127.0.0.1 target must yield an error outcome: {}",
        out.content
    );
    assert!(out.content.contains("refused"), "got: {}", out.content);
    std::env::set_var(LOOPBACK_HATCH, "1");
    let out = run_extract(json!({"url": "not a url"})).await;
    assert!(
        out.is_error,
        "bad URL must yield an error outcome: {}",
        out.content
    );
    assert!(out.content.contains("invalid URL"), "got: {}", out.content);
    std::env::remove_var(LOOPBACK_HATCH);
}
