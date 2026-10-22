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

/// 功能：从真实Bing结果li容器提取标题、链接和摘要，排除脚本及外围内容。
/// 参数：html为不可信搜索页；limit为实际结果数量上限。
/// 返回：可识别结果；未知结构、缺标题/链接及零限额返回空列表。
/// 错误：无；解析失败不伪造搜索成功条目。
/// 状态：只读，不发请求、不解码跳转、不写缓存。
/// 并发与取消：纯内存有界扫描，无异步等待。
/// 权限：不执行页面脚本，不放宽原网络URL/域名过滤。
/// 关联：CHG-2026-0005第9.11节的搜索结果污染回归。
pub fn parse_bing_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    if limit == 0 {
        return results;
    }
    let clean = without_scripts_styles_comments(html);
    let mut container = None;
    for token in tags().find_iter(&clean) {
        let (name, attrs, closing) = tag_parts(token.as_str());
        if !name.eq_ignore_ascii_case("li") {
            continue;
        }
        if !closing {
            // 缺少闭合或嵌套li时放弃旧容器，禁止借下一个结果的标题/链接补全。
            container = attribute(attrs, "class")
                .filter(|classes| classes.split_ascii_whitespace().any(|c| c == "b_algo"))
                .map(|_| token.end());
            continue;
        }
        let Some(start) = container.take() else {
            continue;
        };
        let chunk = &clean[start..token.start()];
        let Some((_, heading)) = element(chunk, "h2") else {
            continue;
        };
        let Some((attrs, label)) = element(heading, "a") else {
            continue;
        };
        let Some(url) = attribute(attrs, "href").map(utils::decode_entities) else {
            continue;
        };
        let Ok(parsed) = reqwest::Url::parse(&url) else {
            continue;
        };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            continue;
        }
        let title = visible_text(label);
        if title.is_empty() {
            continue;
        }
        let snippet = element(chunk, "p")
            .map(|(_, text)| visible_text(text))
            .filter(|text| !text.is_empty());
        results.push(SearchResult {
            title,
            url,
            snippet,
        });
        if results.len() >= limit {
            break;
        }
    }
    results
}

// 复用既有regex依赖识别标签边界（包括带引号的>）；不把此窄解析器作为通用HTML DOM。
fn tags() -> &'static regex::Regex {
    static TAGS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TAGS.get_or_init(|| {
        regex::Regex::new(r#"(?is)<!--.*?(?:-->|$)|<(?:[^"'<>]|"[^"]*"|'[^']*')*>"#)
            .expect("constant tag pattern")
    })
}

fn tag_parts(tag: &str) -> (&str, &str, bool) {
    let inner = tag.trim_start_matches('<').trim_end_matches('>').trim();
    let closing = inner.starts_with('/');
    let inner = inner.strip_prefix('/').unwrap_or(inner).trim_start();
    let end = inner
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(inner.len());
    (&inner[..end], &inner[end..], closing)
}

fn attribute<'a>(attrs: &'a str, wanted: &str) -> Option<&'a str> {
    static ATTRIBUTES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = ATTRIBUTES.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(?:^|\s)([a-z_:][-a-z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#,
        )
        .expect("constant attribute pattern")
    });
    for matched in regex.captures_iter(attrs) {
        if matched[1].eq_ignore_ascii_case(wanted) {
            return matched
                .get(2)
                .or_else(|| matched.get(3))
                .or_else(|| matched.get(4))
                .map(|m| m.as_str());
        }
    }
    None
}

fn without_scripts_styles_comments(html: &str) -> String {
    let mut clean = String::with_capacity(html.len());
    let mut cursor = 0;
    let mut raw = None;
    for token in tags().find_iter(html) {
        let (name, _, closing) = tag_parts(token.as_str());
        if let Some(blocked) = raw {
            if closing && name.eq_ignore_ascii_case(blocked) {
                raw = None;
            }
        } else {
            clean.push_str(&html[cursor..token.start()]);
            if !closing && name.eq_ignore_ascii_case("script") {
                raw = Some("script");
            } else if !closing && name.eq_ignore_ascii_case("style") {
                raw = Some("style");
            } else if !token.as_str().starts_with("<!--") {
                clean.push_str(token.as_str());
            }
        }
        cursor = token.end();
    }
    // 未闭合脚本之后的部分不作为结果恢复，宁可无结果也不输出执行文本。
    if raw.is_none() {
        clean.push_str(&html[cursor..]);
    }
    clean
}

fn element<'a>(html: &'a str, wanted: &str) -> Option<(&'a str, &'a str)> {
    let mut open = None;
    for token in tags().find_iter(html) {
        let (name, attrs, closing) = tag_parts(token.as_str());
        if name.eq_ignore_ascii_case(wanted) {
            if closing {
                if let Some((start, attributes)) = open {
                    return Some((attributes, &html[start..token.start()]));
                }
            } else {
                open = Some((token.end(), attrs));
            }
        }
    }
    None
}

fn visible_text(html: &str) -> String {
    utils::strip_html_tags(html)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
