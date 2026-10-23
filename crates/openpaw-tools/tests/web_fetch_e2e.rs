//! web_fetch 离线 HTTP、SSRF、坏 URL 和重定向合同。
mod common;

use common::{http_html, http_redirect, spawn_mock};
use openpaw_tools::web::web_fetch;
use serde_json::json;
use std::path::Path;
const TEST_ALLOW_LOOPBACK: &str = "OPENPAW_WEB_TEST_ALLOW_LOOPBACK";
#[tokio::test]
async fn web_fetch_contract() {
    std::env::set_var(TEST_ALLOW_LOOPBACK, "1");
    let addr = spawn_mock(vec![http_html(
        "<html><body><h1>Paw Docs</h1><p>Body text</p></body></html>",
    )]);
    let res = web_fetch::run(Path::new("."), json!({"url": format!("http://{addr}/")}))
        .await
        .unwrap();
    assert!(!res.is_error, "fetch should succeed, got: {}", res.content);
    assert!(res.content.contains("# Paw Docs"));
    assert!(res.content.contains("Body text"));
    assert!(!res.content.contains("<h1>"));

    std::env::remove_var(TEST_ALLOW_LOOPBACK);
    let res = web_fetch::run(Path::new("."), json!({"url": "http://127.0.0.1/"}))
        .await
        .unwrap();
    assert!(res.is_error);
    assert!(res.content.contains("refused"));

    let res = web_fetch::run(Path::new("."), json!({"url": "not a valid url"}))
        .await
        .unwrap();
    assert!(res.is_error);
    assert!(res.content.contains("invalid URL"));

    std::env::set_var(TEST_ALLOW_LOOPBACK, "1");
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
    std::env::remove_var(TEST_ALLOW_LOOPBACK);
    assert!(!res.is_error);
    assert!(res.content.contains("## Landed"));
}
