//! Bing 搜索适配器 — 支持国际搜索 + 重定向 URL 解码。

use async_trait::async_trait;

use super::super::headers::browser_headers;
use super::super::utils;
use super::super::{
    detect_query_language, SearchError, SearchOptions, SearchResult, WebSearchAdapter,
};

pub struct BingAdapter {
    client: reqwest::Client,
}

impl BingAdapter {
    pub fn new() -> Result<Self, SearchError> {
        // Shared process-level client (connection pool + DNS cache reused
        // across adapter instances). reqwest::Client clone is cheap.
        let client = super::super::shared_search_client()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl WebSearchAdapter for BingAdapter {
    fn name(&self) -> &'static str {
        "bing"
    }

    async fn search(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let max = options.max_results.unwrap_or(5);
        // Language-aware setmkt
        let lang = detect_query_language(query);
        let setmkt = match lang {
            "zh" => "zh-CN",
            _ => "en-US",
        };
        let url = format!(
            "https://www.bing.com/search?q={}&count={}&setmkt={}",
            urlencoding::encode(query),
            max * 2,
            setmkt
        );

        // Override Accept-Language for language consistency
        let mut headers = browser_headers();
        let accept_lang = match lang {
            "zh" => "zh-CN,zh;q=0.9,en;q=0.8",
            _ => "en-US,en;q=0.9",
        };
        if let Ok(val) = reqwest::header::HeaderValue::from_str(accept_lang) {
            headers.insert(reqwest::header::ACCEPT_LANGUAGE, val);
        }

        let resp = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| SearchError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(SearchError::Http(format!("HTTP {}", resp.status())));
        }

        let html = resp
            .text()
            .await
            .map_err(|e| SearchError::Http(e.to_string()))?;

        let mut results = parse_bing_html(&html, max);
        // 解码 Bing 重定向 URL 为真实 URL
        for r in &mut results {
            r.url = resolve_bing_url(&r.url).unwrap_or_else(|| r.url.clone());
        }
        results = super::super::filter_by_domains(
            results,
            &options.allowed_domains,
            &options.blocked_domains,
        );
        results.truncate(max);
        Ok(results)
    }
}

/// 解析 Bing 结果页 HTML（公开以便集成测试复用）。
pub fn parse_bing_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Bing results are in <li class="b_algo"> containers
    for chunk in html.split("class=\"b_algo\"") {
        if results.len() >= limit {
            break;
        }

        // Extract title from <h2><a href="...">title</a></h2>
        let href = extract_bing_href(chunk);
        let title = utils::extract_text_between(chunk, "<h2>", "</h2>")
            .or_else(|| utils::extract_text_between(chunk, "<a", "</a>"))
            .unwrap_or_else(|| "untitled".into());

        let clean_title = utils::strip_html_tags(&title);

        // Extract snippet from <p class="b_lineclamp..."> or <p>
        let snippet = extract_p_text(chunk)
            .map(|s| utils::strip_html_tags(&s).trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(url) = href {
            if !clean_title.trim().is_empty() && !url.is_empty() {
                results.push(SearchResult {
                    title: clean_title.trim().to_string(),
                    url,
                    snippet,
                });
            }
        }
    }

    results
}

fn extract_bing_href(chunk: &str) -> Option<String> {
    // Look for href in <h2><a href="...">
    if let Some(h2_start) = chunk.find("<h2") {
        let h2_rest = &chunk[h2_start..];
        if let Some(a_start) = h2_rest.find("<a") {
            let a_rest = &h2_rest[a_start..];
            if let Some(href_start) = a_rest.find("href=\"") {
                let rest = &a_rest[href_start + 6..];
                if let Some(end) = rest.find('"') {
                    let href = &rest[..end];
                    if href.starts_with("http") {
                        return Some(href.to_string());
                    }
                }
            }
        }
    }
    // Fallback: first href with http
    if let Some(start) = chunk.find("href=\"http") {
        let rest = &chunk[start + 6..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// 提取 `<p ...>snippet</p>` 的内文。先跳过 `<p` 的属性区再取到 `</p>`，
/// 避免 class 等属性文本被当作正文泄漏进摘要。
fn extract_p_text(chunk: &str) -> Option<String> {
    let start = chunk.find("<p")? + 2;
    let rest = &chunk[start..];
    let gt = rest.find('>')? + 1;
    let rest = &rest[gt..];
    let end = rest.find("</p>")?;
    Some(rest[..end].to_string())
}

/// 解码 Bing 重定向 URL 为真实 URL（公开以便集成测试复用）。
///
/// Bing 将外部 URL 包装为 `bing.com/ck/a?...&u=a1aHR0cHM...&...` 格式。
/// `u=` 参数前缀 `a1`=https, `a0`=http，其余为 base64url 编码的 URL。
pub fn resolve_bing_url(raw_url: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    // 直接外部 URL（非 bing.com）
    if !raw_url.contains("bing.com") && raw_url.starts_with("http") {
        return Some(raw_url.to_string());
    }

    // 提取 u 参数
    let u_match = raw_url.split("u=").nth(1)?;
    let encoded = u_match.split('&').next()?;

    if encoded.len() < 3 {
        return None;
    }

    let prefix = &encoded[..2];
    let b64 = &encoded[2..];

    // base64url 解码
    let decoded = URL_SAFE_NO_PAD.decode(b64).ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;

    // a1 = https, a0 = http
    match prefix {
        "a1" if decoded_str.starts_with("http") => Some(decoded_str),
        "a0" if decoded_str.starts_with("http") => Some(decoded_str),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    #[test]
    fn resolve_direct_url_passes_through() {
        let url = "https://example.com/page";
        assert_eq!(resolve_bing_url(url), Some(url.to_string()));
    }

    #[test]
    fn resolve_bing_redirect_https() {
        // a1 + base64url("https://example.com")
        let encoded = URL_SAFE_NO_PAD.encode(b"https://example.com");
        let redirect = format!("https://www.bing.com/ck/a?...&u=a1{}", encoded);
        assert_eq!(
            resolve_bing_url(&redirect),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn resolve_bing_redirect_http() {
        let encoded = URL_SAFE_NO_PAD.encode(b"http://example.org");
        let redirect = format!("https://www.bing.com/ck/a?u=a0{}", encoded);
        assert_eq!(
            resolve_bing_url(&redirect),
            Some("http://example.org".to_string())
        );
    }

    #[test]
    fn resolve_invalid_bing_redirect_returns_none() {
        let redirect = "https://www.bing.com/ck/a?u=xxinvalid";
        assert_eq!(resolve_bing_url(redirect), None);
    }
}
