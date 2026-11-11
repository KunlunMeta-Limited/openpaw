//! DuckDuckGo 搜索适配器 — 基于 html 端点的轻量 HTML 解析。
//!
//! 请求 `GET https://html.duckduckgo.com/html/?q=<query>`，携带浏览器标头伪装；
//! 结果块形如 `<a rel="nofollow" class="result__a" href="...">title</a>` +
//! `<a class="result__snippet" ...>snippet</a>`，跳转链接经 `uddg` 参数还原真实 URL。

use std::sync::OnceLock;

use async_trait::async_trait;

use super::super::utils;
use super::super::{filter_by_domains, SearchError, SearchOptions, SearchResult, WebSearchAdapter};

pub struct DuckDuckGoAdapter {
    client: reqwest::Client,
    base_url: String,
}

impl DuckDuckGoAdapter {
    pub fn new() -> Result<Self, SearchError> {
        // Shared process-level client (connection pool + DNS cache reused
        // across adapter instances). reqwest::Client clone is cheap.
        let client = super::super::shared_search_client()?;
        Ok(Self {
            client,
            base_url: "https://html.duckduckgo.com/html/".into(),
        })
    }

    /// 注入 base_url（测试用：指向本地 mock 服务器，请求 URL 为 `{base_url}?q=...`）。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl WebSearchAdapter for DuckDuckGoAdapter {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }

    async fn search(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let max = options.max_results.unwrap_or(5);
        let sep = if self.base_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!("{}{}q={}", self.base_url, sep, urlencoding::encode(query));

        let resp = self.client.get(&url).send().await.map_err(|e| {
            SearchError::Http(crate::web::web_fetch::transport_diagnostic(
                &e,
                "search_request",
            ))
        })?;

        let html = super::super::search_page(resp, query).await?;

        if is_ddg_anti_bot_page(&html) {
            return Err(SearchError::BlockedByAntiBot);
        }

        let mut results = parse_ddg_html(&html, max);
        results = filter_by_domains(results, &options.allowed_domains, &options.blocked_domains);
        results.truncate(max);
        Ok(results)
    }
}

/// 检测 DuckDuckGo 反爬/异常页（anomaly 拦截或验证码挑战）。
fn is_ddg_anti_bot_page(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("anomaly") || lower.contains("captcha")
}

/// Result-anchor regex (compiled once; reused across search calls).
fn anchor_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?is)<a\b([^>]*class="result__a"[^>]*)>(.*?)</a>"#)
            .expect("valid regex")
    })
}

/// href attribute regex inside a result anchor (compiled once).
fn href_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r##"href\s*=\s*"([^"]*)""##).expect("valid regex"))
}

/// Result-snippet regex (compiled once).
fn snippet_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?is)<a\b[^>]*class="result__snippet"[^>]*>(.*?)</a>"#)
            .expect("valid regex")
    })
}

/// 解析 html 端点返回的结果列表（公开以便集成测试复用）。
///
/// 结果锚点与摘要在文档中按同序出现，按下标配对；
/// `href` 可能是 `//duckduckgo.com/l/?uddg=<encoded real url>&rut=...` 跳转形式。
pub fn parse_ddg_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let snippets: Vec<String> = snippet_re()
        .captures_iter(html)
        .map(|c| utils::strip_html_tags(&c[1]).trim().to_string())
        .collect();

    let mut results = Vec::new();
    for (i, cap) in anchor_re().captures_iter(html).enumerate() {
        if results.len() >= limit {
            break;
        }
        let attrs = &cap[1];
        let title = utils::strip_html_tags(&cap[2]).trim().to_string();
        let Some(href) = href_re().captures(attrs).map(|c| c[1].to_string()) else {
            continue;
        };
        let Some(url) = resolve_ddg_url(&href) else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        let snippet = snippets.get(i).filter(|s| !s.is_empty()).cloned();
        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }
    results
}

/// 将 DuckDuckGo 跳转链接还原为真实 URL（公开以便集成测试复用）。
///
/// - `...uddg=<percent-encoded real url>&...` → 解码得到真实 URL；
/// - `//host/path` 协议相对链接 → 补全 https；
/// - 直接外链原样返回；其余（站内相对路径等）返回 None。
pub fn resolve_ddg_url(href: &str) -> Option<String> {
    if let Some(pos) = href.find("uddg=") {
        let rest = &href[pos + "uddg=".len()..];
        let end = rest.find('&').unwrap_or(rest.len());
        let real = urlencoding::decode(&rest[..end]).ok()?;
        let real = real.trim();
        if real.starts_with("http") {
            return Some(real.to_string());
        }
        return None;
    }
    if let Some(stripped) = href.strip_prefix("//") {
        return Some(format!("https://{stripped}"));
    }
    if href.starts_with("http") {
        return Some(href.to_string());
    }
    None
}
