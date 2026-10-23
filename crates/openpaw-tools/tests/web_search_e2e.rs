//! web_search 集成测试 — 语言检测、引擎选择、域名过滤、HTML 解析、
//! 反爬检测与 mock HTTP 层（全部离线，不访问真实互联网）。
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
mod common;

use common::{http_html_status, spawn_mock};
use openpaw_core::ToolCategory;
use openpaw_tools::web::web_search::adapters::bing::{parse_bing_html, resolve_bing_url};
use openpaw_tools::web::web_search::adapters::duckduckgo::{
    parse_ddg_html, resolve_ddg_url, DuckDuckGoAdapter,
};
use openpaw_tools::web::web_search::{
    detect_query_language, engines_for_query, filter_by_domains, run, spec, AdapterKind,
    SearchError, SearchOptions, SearchResult, WebSearchAdapter,
};
// ── 语言检测 ──────────────────────────────────────────────────────────
#[test]
fn detect_chinese_query() {
    assert_eq!(detect_query_language("Rust 异步编程"), "zh");
    assert_eq!(detect_query_language("你好世界"), "zh");
}
#[test]
fn detect_english_query() {
    assert_eq!(detect_query_language("Rust async runtime"), "en");
    assert_eq!(detect_query_language("hello world"), "en");
}
#[test]
fn detect_empty_query_defaults_to_english() {
    assert_eq!(detect_query_language(""), "en");
}
// ── 引擎选择 ──────────────────────────────────────────────────────────
#[test]
fn engines_for_chinese_query_defaults_to_bing_first() {
    let engines = engines_for_query("Rust 异步编程", None);
    assert_eq!(engines, vec![AdapterKind::Bing, AdapterKind::DuckDuckGo]);
}
#[test]
fn engines_for_english_query_defaults_to_bing_first() {
    let engines = engines_for_query("Rust async runtime", None);
    assert_eq!(engines, vec![AdapterKind::Bing, AdapterKind::DuckDuckGo]);
}
#[test]
fn engines_with_user_override_bing_single() {
    let engines = engines_for_query("test", Some("bing"));
    assert_eq!(engines, vec![AdapterKind::Bing]);
}
#[test]
fn engines_with_user_override_ddg_single() {
    let engines = engines_for_query("test", Some("duckduckgo"));
    assert_eq!(engines, vec![AdapterKind::DuckDuckGo]);
}
#[test]
fn engines_with_invalid_user_override_falls_back_to_default() {
    let engines = engines_for_query("test", Some("invalid_engine"));
    assert_eq!(engines, vec![AdapterKind::Bing, AdapterKind::DuckDuckGo]);
}
#[test]
fn adapter_kind_from_str() {
    assert_eq!(AdapterKind::from_str("bing"), Some(AdapterKind::Bing));
    assert_eq!(AdapterKind::from_str("BING"), Some(AdapterKind::Bing));
    assert_eq!(
        AdapterKind::from_str("duckduckgo"),
        Some(AdapterKind::DuckDuckGo)
    );
    assert_eq!(
        AdapterKind::from_str("DuckDuckGo"),
        Some(AdapterKind::DuckDuckGo)
    );
    assert_eq!(AdapterKind::from_str("tavily"), None);
    assert_eq!(AdapterKind::from_str("baidu"), None);
    assert_eq!(AdapterKind::from_str("unknown"), None);
}
#[test]
fn adapter_kind_as_str() {
    assert_eq!(AdapterKind::Bing.as_str(), "bing");
    assert_eq!(AdapterKind::DuckDuckGo.as_str(), "duckduckgo");
}
// ── 域名过滤 ──────────────────────────────────────────────────────────
#[test]
fn filter_by_allowed_domains() {
    let results = vec![
        SearchResult {
            title: "A".into(),
            url: "https://doc.rust-lang.org/std/".into(),
            snippet: None,
        },
        SearchResult {
            title: "B".into(),
            url: "https://example.com/".into(),
            snippet: None,
        },
    ];
    let allowed = Some(vec!["doc.rust-lang.org".into()]);
    let filtered = filter_by_domains(results, &allowed, &None);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].url, "https://doc.rust-lang.org/std/");
}
#[test]
fn filter_by_blocked_domains() {
    let results = vec![
        SearchResult {
            title: "A".into(),
            url: "https://good.com/".into(),
            snippet: None,
        },
        SearchResult {
            title: "B".into(),
            url: "https://spam.com/".into(),
            snippet: None,
        },
    ];
    let blocked = Some(vec!["spam.com".into()]);
    let filtered = filter_by_domains(results, &None, &blocked);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].url, "https://good.com/");
}
#[test]
fn filter_by_subdomain() {
    let results = vec![
        SearchResult {
            title: "A".into(),
            url: "https://sub.example.com/".into(),
            snippet: None,
        },
        SearchResult {
            title: "B".into(),
            url: "https://other.com/".into(),
            snippet: None,
        },
    ];
    let allowed = Some(vec!["example.com".into()]);
    let filtered = filter_by_domains(results, &allowed, &None);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].url, "https://sub.example.com/");
}
#[test]
fn filter_no_domains_returns_all() {
    let results = vec![
        SearchResult {
            title: "A".into(),
            url: "https://a.com/".into(),
            snippet: None,
        },
        SearchResult {
            title: "B".into(),
            url: "https://b.com/".into(),
            snippet: None,
        },
    ];
    let filtered = filter_by_domains(results, &None, &None);
    assert_eq!(filtered.len(), 2);
}
// ── SearchResult serde ───────────────────────────────────────────────
#[test]
fn search_result_serializes() {
    let r = SearchResult {
        title: "Test".into(),
        url: "https://example.com".into(),
        snippet: Some("A snippet".into()),
    };
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains("Test"));
    assert!(json.contains("A snippet"));
}
#[test]
fn search_result_deserializes() {
    let json = r#"{"title":"Test","url":"https://example.com","snippet":null}"#;
    let r: SearchResult = serde_json::from_str(json).unwrap();
    assert_eq!(r.title, "Test");
    assert!(r.snippet.is_none());
}
// ── Tool 合同 + 失败路径 ──────────────────────────────────────────────
#[test]
fn spec_contract() {
    let s = spec();
    assert_eq!(s.name, "web_search");
    assert_eq!(s.category, Some(ToolCategory::Network));
    assert_eq!(s.is_destructive, Some(false));
    // engine 枚举收敛为双引擎
    let engine = &s.input_schema["properties"]["engine"];
    assert_eq!(engine["enum"][0], "bing");
    assert_eq!(engine["enum"][1], "duckduckgo");
    // 收敛后描述中不得残留已移除引擎的痕迹
    assert!(!s.description.to_lowercase().contains("tavily"));
    assert!(!s.description.to_lowercase().contains("baidu"));
    assert!(!s.description.to_lowercase().contains("sogou"));
    assert!(s.description.contains("bing"));
}
#[tokio::test]
async fn empty_query_returns_error_outcome() {
    let args = serde_json::json!({"query": "  "});
    let outcome = run(std::path::Path::new("."), args).await.unwrap();
    assert!(outcome.is_error);
    assert!(outcome.content.contains("must not be empty"));
}
#[tokio::test]
async fn bad_args_return_error_outcome() {
    let args = serde_json::json!({"nope": 1});
    let outcome = run(std::path::Path::new("."), args).await.unwrap();
    assert!(outcome.is_error);
    assert!(outcome.content.contains("bad args"));
}
// ── Bing 解析（内嵌 HTML 样本，不联网） ───────────────────────────────
#[test]
fn parse_bing_sample_html() {
    let html = r#"<html><body><ol id="b_results">
<li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9ndWlkZQ">Async guide</a></h2><div class="b_caption"><p class="b_lineclamp4">A guide to <strong>async</strong> Rust.</p></div></li>
<li class="b_algo"><h2><a href="https://docs.rs/tokio">tokio</a></h2><p>Tokio runtime docs.</p></li>
</ol></body></html>"#;
    let results = parse_bing_html(html, 5);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Async guide");
    // 解析阶段保留原始跳转 URL；真实 URL 由 resolve_bing_url 解码
    assert_eq!(
        results[0].url,
        "https://www.bing.com/ck/a?!&u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9ndWlkZQ"
    );
    // 摘要不得泄漏 class 等属性文本
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("A guide to async Rust.")
    );
    assert_eq!(results[1].title, "tokio");
    assert_eq!(results[1].url, "https://docs.rs/tokio");
    assert_eq!(results[1].snippet.as_deref(), Some("Tokio runtime docs."));
}
#[test]
fn parse_bing_respects_limit() {
    let html = r#"<li class="b_algo"><h2><a href="https://a.com">A</a></h2><p>x</p></li>
<li class="b_algo"><h2><a href="https://b.com">B</a></h2><p>y</p></li>"#;
    let results = parse_bing_html(html, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://a.com");
}
#[test]
fn parse_and_resolve_bing_redirect_composition() {
    let encoded = URL_SAFE_NO_PAD.encode(b"https://example.com/guide");
    let redirect = format!("https://www.bing.com/ck/a?!&u=a1{encoded}");
    let html =
        format!(r#"<li class="b_algo"><h2><a href="{redirect}">Guide</a></h2><p>body</p></li>"#);
    let parsed = parse_bing_html(&html, 5);
    assert_eq!(parsed.len(), 1);
    // 与适配器 search() 内部一致的组合逻辑
    let resolved = resolve_bing_url(&parsed[0].url).unwrap_or_else(|| parsed[0].url.clone());
    assert_eq!(resolved, "https://example.com/guide");
}
// ── DuckDuckGo 解析（内嵌 HTML 样本，不联网） ─────────────────────────
const DDG_SAMPLE: &str = r#"<!DOCTYPE html>
<html><body>
<div class="result results_links results_links_deep">
  <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fguide%3Flang%3Dzh&amp;rut=ad62aadba0f7">Rust 异步编程指南</a>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fguide&amp;rut=x">一份关于 <b>Rust 异步</b> 编程的入门指南。</a>
</div>
<div class="result results_links">
  <a rel="nofollow" class="result__a" href="https://docs.rs/tokio">tokio - Rust</a>
  <a class="result__snippet" href="https://docs.rs/tokio">A runtime for writing reliable asynchronous applications.</a>
</div>
</body></html>"#;
#[test]
fn parse_ddg_sample_html() {
    let results = parse_ddg_html(DDG_SAMPLE, 10);
    assert_eq!(results.len(), 2);
    // 跳转链接 uddg 参数解码为真实 URL
    assert_eq!(results[0].title, "Rust 异步编程指南");
    assert_eq!(results[0].url, "https://example.com/guide?lang=zh");
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("一份关于 Rust 异步 编程的入门指南。")
    );
    // 直接外链原样保留
    assert_eq!(results[1].title, "tokio - Rust");
    assert_eq!(results[1].url, "https://docs.rs/tokio");
    assert_eq!(
        results[1].snippet.as_deref(),
        Some("A runtime for writing reliable asynchronous applications.")
    );
}
#[test]
fn parse_ddg_respects_limit() {
    let results = parse_ddg_html(DDG_SAMPLE, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://example.com/guide?lang=zh");
}
#[test]
fn resolve_ddg_url_variants() {
    // 直接外链
    assert_eq!(
        resolve_ddg_url("https://example.com/a"),
        Some("https://example.com/a".into())
    );
    // 跳转形式（含 &amp; 实体分隔的后续参数）
    assert_eq!(
        resolve_ddg_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fx&amp;rut=abc"),
        Some("https://example.com/x".into())
    );
    // 协议相对链接
    assert_eq!(
        resolve_ddg_url("//example.com/page"),
        Some("https://example.com/page".into())
    );
    // 站内相对路径无法还原
    assert_eq!(resolve_ddg_url("/about"), None);
    // uddg 值不是 http URL
    assert_eq!(resolve_ddg_url("//duckduckgo.com/l/?uddg=not-a-url"), None);
}
fn spawn_html_mock(status_line: &str, body: &str) -> String {
    spawn_mock(vec![http_html_status(status_line, body)])
}
fn ddg_options(max_results: usize) -> SearchOptions {
    SearchOptions {
        max_results: Some(max_results),
        allowed_domains: None,
        blocked_domains: None,
    }
}
#[tokio::test]
async fn ddg_mock_success_parses_and_decodes_redirects() {
    let addr = spawn_html_mock("HTTP/1.1 200 OK", DDG_SAMPLE);
    let adapter = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr}"));
    let results = adapter.search("rust async", &ddg_options(5)).await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].url, "https://example.com/guide?lang=zh");
    assert_eq!(results[0].title, "Rust 异步编程指南");
    assert_eq!(results[1].url, "https://docs.rs/tokio");
}
#[tokio::test]
async fn ddg_mock_applies_blocked_domains_and_limit() {
    let addr = spawn_html_mock("HTTP/1.1 200 OK", DDG_SAMPLE);
    let adapter = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr}"));
    // 域名过滤
    let options = SearchOptions {
        max_results: Some(5),
        blocked_domains: Some(vec!["example.com".into()]),
        allowed_domains: None,
    };
    let results = adapter.search("rust async", &options).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://docs.rs/tokio");
    // max_results 截断
    let addr2 = spawn_html_mock("HTTP/1.1 200 OK", DDG_SAMPLE);
    let adapter2 = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr2}"));
    let results2 = adapter2
        .search("rust async", &ddg_options(1))
        .await
        .unwrap();
    assert_eq!(results2.len(), 1);
}
#[tokio::test]
async fn ddg_mock_403_maps_to_anti_bot() {
    let addr = spawn_html_mock("HTTP/1.1 403 Forbidden", "<html>forbidden</html>");
    let adapter = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr}"));
    let err = adapter
        .search("rust async", &ddg_options(5))
        .await
        .unwrap_err();
    assert!(matches!(err, SearchError::BlockedByAntiBot));
}
#[tokio::test]
async fn ddg_mock_anomaly_page_maps_to_anti_bot() {
    let body = r#"<html><head><title>Unfortunately, bots use DuckDuckGo too.</title></head>
<body><div class="anomaly">Please complete a captcha to continue.</div></body></html>"#;
    let addr = spawn_html_mock("HTTP/1.1 200 OK", body);
    let adapter = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr}"));
    let err = adapter
        .search("rust async", &ddg_options(5))
        .await
        .unwrap_err();
    assert!(matches!(err, SearchError::BlockedByAntiBot));
}
#[tokio::test]
async fn ddg_mock_server_error_maps_to_http_error() {
    let addr = spawn_html_mock("HTTP/1.1 500 Internal Server Error", "oops");
    let adapter = DuckDuckGoAdapter::new()
        .unwrap()
        .with_base_url(format!("http://{addr}"));
    let err = adapter
        .search("rust async", &ddg_options(5))
        .await
        .unwrap_err();
    assert!(matches!(err, SearchError::Http(_)));
}
