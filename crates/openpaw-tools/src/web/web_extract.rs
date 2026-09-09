//! `web_extract` — fetch a URL and extract structured content in one step.
//!
//! Port notes: the upstream tool drives a CDP browser pool and supports
//! `wait_until` / `wait_for_selector` (JavaScript/SPA rendering). This port
//! is degraded to a plain HTTP fetch + HTML extraction — no script runs, so
//! those wait parameters are gone from the schema and the description no
//! longer references browser navigation. A browser fallback is deferred to
//! the sandbox/browser batch.
//!
//! SSRF validation (scheme/hostname/IP checks + a DNS-rebinding lookup) is
//! shared with `web_fetch` — the guard lives there and both tools run the
//! exact same validation on the initial URL and every redirect hop.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};

use crate::web::web_fetch::{check_dns_rebinding, loopback_test_allowance, validate_url_ssrf};

const DEFAULT_MAX_CHARS: usize = 30_000;
const MAX_CHARS_CAP: usize = 1_000_000;
/// Refuse oversized payloads before buffering (mirrors web_fetch's cap).
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// Manual redirect budget; every hop re-runs the full SSRF validation.
const MAX_REDIRECTS: usize = 5;
const USER_AGENT: &str = "openpaw/0.1";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "web_extract".into(),
        description: "Fetch a URL and extract structured content in one step. Provides multiple \
            output formats: readable text (strips scripts/styles), markdown (preserves \
            structure), links (all hyperlinks), or a comprehensive summary combining content and \
            links. Use this for research, data collection, and content analysis tasks."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch and extract content from"
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown", "links", "summary"],
                    "description": "Extraction format: 'text' for readable text only, 'markdown' \
                        for structured markdown, 'links' for all hyperlinks, 'summary' for \
                        markdown + links combined. Default: summary.",
                    "default": "summary"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return. Default: 30000.",
                    "default": 30000
                }
            },
            "required": ["url"]
        }),
        category: Some(ToolCategory::Network),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let Some(url_str) = args.get("url").and_then(serde_json::Value::as_str) else {
        return Ok(ToolCallOutcome::error(
            "web_extract: missing or non-string 'url'",
        ));
    };
    let format = args
        .get("format")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("summary")
        .to_string();
    if !matches!(format.as_str(), "text" | "markdown" | "links" | "summary") {
        return Ok(ToolCallOutcome::error(format!(
            "web_extract: invalid format '{format}' (expected one of: text, markdown, links, summary)"
        )));
    }
    let max_chars = match args.get("max_chars") {
        None | Some(serde_json::Value::Null) => DEFAULT_MAX_CHARS,
        Some(v) => match v.as_u64() {
            Some(n) => (n as usize).min(MAX_CHARS_CAP),
            None => {
                return Ok(ToolCallOutcome::error(
                    "web_extract: 'max_chars' must be a non-negative integer",
                ))
            }
        },
    };

    let mut current_url = match url::Url::parse(url_str) {
        Ok(u) => u,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "web_extract: invalid URL: {e}"
            )))
        }
    };
    // Test-only loopback allowance (shared with web_fetch); false in production.
    let allow_loopback = loopback_test_allowance();
    if let Err(msg) = validate_url_ssrf(&current_url, allow_loopback) {
        return Ok(ToolCallOutcome::error(format!("web_extract: {msg}")));
    }
    if let Err(msg) = check_dns_rebinding(&current_url, allow_loopback).await {
        return Ok(ToolCallOutcome::error(format!("web_extract: {msg}")));
    }

    let client = http_client();
    let mut resp = match client
        .get(current_url.as_str())
        .header("User-Agent", USER_AGENT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(ToolCallOutcome::error(format!("web_extract: {e}"))),
    };

    // Follow redirects manually so the *same* SSRF validation (scheme/hostname/
    // IP + DNS rebinding) applies to every hop; automatic redirects would skip it.
    for _ in 0..MAX_REDIRECTS {
        if !resp.status().is_redirection() {
            break;
        }
        let Some(location) = resp.headers().get("location") else {
            break;
        };
        let location_str = location.to_str().unwrap_or("");
        let redirect_url = match current_url.join(location_str) {
            Ok(u) => u,
            Err(e) => {
                return Ok(ToolCallOutcome::error(format!(
                    "web_extract: bad redirect URL: {e}"
                )))
            }
        };
        if let Err(msg) = validate_url_ssrf(&redirect_url, allow_loopback) {
            return Ok(ToolCallOutcome::error(format!(
                "web_extract: redirect blocked: {msg}"
            )));
        }
        if let Err(msg) = check_dns_rebinding(&redirect_url, allow_loopback).await {
            return Ok(ToolCallOutcome::error(format!(
                "web_extract: redirect blocked: {msg}"
            )));
        }
        current_url = redirect_url;
        resp = match client
            .get(current_url.as_str())
            .header("User-Agent", USER_AGENT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolCallOutcome::error(format!(
                    "web_extract: redirect failed: {e}"
                )))
            }
        };
    }

    if !resp.status().is_success() {
        return Ok(ToolCallOutcome::error(format!(
            "web_extract: HTTP {}",
            resp.status()
        )));
    }

    if let Some(len) = resp.content_length() {
        if len > MAX_BODY_BYTES as u64 {
            return Ok(ToolCallOutcome::error(format!(
                "web_extract: response too large: Content-Length {len} bytes (max {MAX_BODY_BYTES})"
            )));
        }
    }
    let body = match resp.bytes().await {
        Ok(bytes) => {
            if bytes.len() > MAX_BODY_BYTES {
                return Ok(ToolCallOutcome::error(format!(
                    "web_extract: response too large: {} bytes (max {MAX_BODY_BYTES})",
                    bytes.len()
                )));
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "web_extract: read body: {e}"
            )))
        }
    };

    let links = extract_links(&body, &current_url);
    let content = match format.as_str() {
        "links" => format!(
            "=== Links from {current_url} ===\n{}",
            format_link_list(&links)
        ),
        "summary" => {
            let mut out = crate::web::html_to_md::html_to_markdown(&body);
            out.push_str("\n\n## Links\n");
            out.push_str(&format_link_list(&links));
            out
        }
        // "text" and "markdown" both reuse html_to_markdown: it already strips
        // script/style blocks and keeps heading/list structure, which is the
        // closest HTTP-only approximation of the upstream "readable text" mode.
        _ => crate::web::html_to_md::html_to_markdown(&body),
    };

    Ok(ToolCallOutcome::ok(truncate_output(content, max_chars)))
}

/// Process-level shared HTTP client. Same configuration as the web_fetch
/// client (30s total timeout, 10s connect timeout, redirects disabled for
/// hop-by-hop SSRF validation) but a separate static instance, so web_fetch
/// and web_extract never share mutable client state.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build shared reqwest client for web_extract")
    })
}

/// One extracted hyperlink: absolute URL + plain anchor text.
#[derive(Debug)]
struct ExtractedLink {
    url: String,
    text: String,
}

/// Regex-based `<a href>` extraction. The upstream tool walked the live DOM;
/// over a static HTML string a regex pass is the closest equivalent: capture
/// href + anchor text, decode entities, absolutize relative URLs against the
/// final response URL, and dedupe by URL keeping the first anchor text.
fn extract_links(html: &str, base: &url::Url) -> Vec<ExtractedLink> {
    let re = anchor_regex();
    let mut out: Vec<ExtractedLink> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cap in re.captures_iter(html) {
        let href_raw = cap
            .get(1)
            .or_else(|| cap.get(2))
            .or_else(|| cap.get(3))
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim();
        let href = decode_html_entities(href_raw);
        // Skip empty and script-URI hrefs; everything else (including
        // fragment/mailto links) is absolutized via join.
        if href.is_empty() || href.to_ascii_lowercase().starts_with("javascript:") {
            continue;
        }
        let absolute = match base.join(&href) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let url_str = absolute.to_string();
        if !seen.insert(url_str.clone()) {
            continue;
        }
        let text = plain_text(cap.get(4).map(|m| m.as_str()).unwrap_or(""));
        out.push(ExtractedLink { url: url_str, text });
    }
    out
}

/// Render the deduped link list as markdown bullets.
fn format_link_list(links: &[ExtractedLink]) -> String {
    if links.is_empty() {
        return "(no links found)".to_string();
    }
    let mut out = String::new();
    for l in links {
        if l.text.is_empty() {
            out.push_str(&format!("- {}\n", l.url));
        } else {
            out.push_str(&format!("- [{}]({})\n", l.text, l.url));
        }
    }
    out
}

/// `<a href="...">…</a>` matcher: href in double/single quotes or unquoted,
/// anchor text up to the closing tag. Case-insensitive, dot-matches-newline.
fn anchor_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)<a\b[^>]*?\shref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))[^>]*>(.*?)</a\s*>"#,
        )
        .expect("valid anchor regex")
    })
}

/// Inner markup → plain text: drop tags, decode entities, collapse whitespace.
fn plain_text(html: &str) -> String {
    static TAG_RE: OnceLock<regex::Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| regex::Regex::new(r"<[^>]*>").expect("valid tag regex"));
    let decoded = decode_html_entities(&tag_re.replace_all(html, " "));
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode the common HTML entities. Needed for hrefs (`?a=1&amp;b=2` must
/// join correctly) and anchor text; html_to_md keeps its decoder private.
fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Character-level truncation with a trailing notice (same style as the bash
/// tool's output truncation).
fn truncate_output(out: String, max_chars: usize) -> String {
    if out.chars().count() <= max_chars {
        return out;
    }
    let truncated: String = out.chars().take(max_chars).collect();
    format!("{truncated}\n... (output truncated at {max_chars} characters)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_guard_rejects_bad_targets() {
        for bad in [
            "http://localhost/",
            "http://127.0.0.1/",
            "file:///etc/passwd",
        ] {
            let url = url::Url::parse(bad).unwrap();
            assert!(
                validate_url_ssrf(&url, false).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn ssrf_guard_allows_public_http() {
        let url = url::Url::parse("https://example.com/page").unwrap();
        assert!(validate_url_ssrf(&url, false).is_ok());
    }

    #[test]
    fn extract_links_absolutizes_and_dedupes() {
        let base = url::Url::parse("http://example.com/dir/page").unwrap();
        let html = r#"<a href="/a">First</a><a href="/a">Second</a> \
            <a href="https://x.y/">X</a><a href="javascript:void(0)">js</a>"#;
        let links = extract_links(html, &base);
        assert_eq!(links.len(), 2, "dedupe + javascript skip: {links:?}");
        assert_eq!(links[0].url, "http://example.com/a");
        assert_eq!(links[0].text, "First", "first anchor text wins");
        assert_eq!(links[1].url, "https://x.y/");
    }

    #[test]
    fn truncate_output_appends_notice() {
        let out = truncate_output("abcdefghij".to_string(), 4);
        assert!(out.starts_with("abcd"));
        assert!(out.contains("truncated at 4 characters"));
    }
}
