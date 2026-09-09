//! WebSearch 适配器架构 + 反爬修复。
//!
//! 统一的搜索引擎适配器 trait，支持 bing / duckduckgo 双引擎。
//! 多引擎降级 + 反爬检测 + 浏览器标头伪装。

pub mod adapters;
pub mod headers;
pub mod utils;

use std::path::Path;
use std::sync::OnceLock;

use async_trait::async_trait;
use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

// ── Types ─────────────────────────────────────────────────────────────

/// 搜索结果统一格式
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

/// 搜索选项
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub max_results: Option<usize>,
    pub allowed_domains: Option<Vec<String>>,
    pub blocked_domains: Option<Vec<String>>,
}

/// 适配器类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Bing,
    DuckDuckGo,
}

impl AdapterKind {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "bing" => Some(Self::Bing),
            "duckduckgo" => Some(Self::DuckDuckGo),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::DuckDuckGo => "duckduckgo",
        }
    }
}

// ── Error types ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Blocked by anti-bot detection — try again later or use a different engine")]
    BlockedByAntiBot,
    #[error("Invalid query: {0}")]
    InvalidQuery(String),
    #[error("Adapter init failed: {0}")]
    AdapterInit(String),
    /// 全部引擎均失败时的聚合错误（携带各引擎的错误摘要）。
    #[error("all search engines failed: {0}")]
    AllEnginesFailed(String),
}

// ── Adapter trait ─────────────────────────────────────────────────────

#[async_trait]
pub trait WebSearchAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> std::result::Result<Vec<SearchResult>, SearchError>;
}

// ── Factory + fallback ────────────────────────────────────────────────

/// 进程级共享搜索 HTTP client。配置与两 adapter 原有构建一致（30s 总超时 /
/// 10s 连接超时 / browser headers / limited redirect）；`reqwest::Client`
/// 内部为 Arc——adapter 每次实例化时 clone 复用同一连接池与 DNS 缓存，
/// 而非每次 `web_search` 调用重建。
pub(crate) fn shared_search_client() -> std::result::Result<reqwest::Client, SearchError> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .default_headers(headers::browser_headers())
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(SearchError::AdapterInit)
}

/// 创建适配器实例
pub fn create_adapter(
    kind: AdapterKind,
) -> std::result::Result<Box<dyn WebSearchAdapter>, SearchError> {
    match kind {
        AdapterKind::Bing => Ok(Box::new(adapters::bing::BingAdapter::new()?)),
        AdapterKind::DuckDuckGo => Ok(Box::new(adapters::duckduckgo::DuckDuckGoAdapter::new()?)),
    }
}

/// 多引擎降级搜索 — 按引擎列表顺序尝试，返回第一个成功的结果。
/// 全部失败时返回聚合错误；全部被反爬拦截时保留统一的反爬错误。
pub async fn search_with_fallback(
    query: &str,
    options: &SearchOptions,
    engines: &[AdapterKind],
) -> std::result::Result<Vec<SearchResult>, SearchError> {
    if engines.is_empty() {
        return Err(SearchError::InvalidQuery("no engines configured".into()));
    }

    let mut errors: Vec<String> = Vec::new();
    let mut all_blocked = true;

    for &kind in engines {
        let adapter = match create_adapter(kind) {
            Ok(a) => a,
            Err(e) => {
                all_blocked = false;
                errors.push(format!("{}: {e}", kind.as_str()));
                continue;
            }
        };
        match adapter.search(query, options).await {
            Ok(results) if !results.is_empty() => return Ok(results),
            Ok(_) => {
                all_blocked = false;
                errors.push(format!("{}: no results", kind.as_str()));
            }
            Err(SearchError::BlockedByAntiBot) => {
                errors.push(format!("{}: blocked by anti-bot", kind.as_str()));
            }
            Err(e) => {
                all_blocked = false;
                errors.push(format!("{}: {e}", kind.as_str()));
            }
        }
    }

    if all_blocked {
        return Err(SearchError::BlockedByAntiBot);
    }
    Err(SearchError::AllEnginesFailed(errors.join("; ")))
}

// ── Language-aware engine selection ───────────────────────────────────

/// 检测查询语言：包含 CJK 字符判定为中文，否则英文
pub fn detect_query_language(query: &str) -> &'static str {
    let has_cjk = query.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c) // CJK Unified Ideographs
            || ('\u{3400}'..='\u{4DBF}').contains(&c) // CJK Extension A
    });
    if has_cjk {
        "zh"
    } else {
        "en"
    }
}

/// 构建搜索引擎降级链。中文与英文查询统一以 Bing 优先、DuckDuckGo 兜底
/// （查询语言经由 [`detect_query_language`] 驱动各适配器的市场/区域参数）。
/// 用户显式指定引擎时只用该引擎；无法识别的引擎名回退默认降级链。
pub fn engines_for_query(_query: &str, user_engine: Option<&str>) -> Vec<AdapterKind> {
    if let Some(kind) = user_engine.and_then(AdapterKind::from_str) {
        return vec![kind];
    }
    vec![AdapterKind::Bing, AdapterKind::DuckDuckGo]
}

// ── Domain filtering ──────────────────────────────────────────────────

/// 按域名过滤搜索结果
pub fn filter_by_domains(
    mut results: Vec<SearchResult>,
    allowed: &Option<Vec<String>>,
    blocked: &Option<Vec<String>>,
) -> Vec<SearchResult> {
    if let Some(ref allowed) = allowed {
        results.retain(|r| {
            if let Ok(host) = url::Url::parse(&r.url) {
                let host_str = host.host_str().unwrap_or("");
                allowed
                    .iter()
                    .any(|d| host_str == d || host_str.ends_with(&format!(".{d}")))
            } else {
                false
            }
        });
    }

    if let Some(ref blocked) = blocked {
        results.retain(|r| {
            if let Ok(host) = url::Url::parse(&r.url) {
                let host_str = host.host_str().unwrap_or("");
                !blocked
                    .iter()
                    .any(|d| host_str == d || host_str.ends_with(&format!(".{d}")))
            } else {
                true
            }
        });
    }

    results
}

// ── Tool spec + run ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct Args {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    allowed_domains: Option<Vec<String>>,
    #[serde(default)]
    blocked_domains: Option<Vec<String>>,
    /// When true (default), automatically fetch the full page content of each
    /// search result using web_fetch, instead of returning only snippets.
    #[serde(default = "default_fetch_content")]
    fetch_content: bool,
    /// Max chars to fetch per page (only used when fetch_content=true).
    #[serde(default = "default_fetch_max_chars")]
    fetch_max_chars: usize,
}

fn default_limit() -> usize {
    5
}

fn default_fetch_content() -> bool {
    true
}

fn default_fetch_max_chars() -> usize {
    20_000
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "web_search".into(),
        description: "Search the web for current information. Returns titles, URLs, snippets, \
            AND the full page content of each result (fetched automatically). \
            Supports multiple search engines with automatic fallback: bing (default, first for all languages), \
            duckduckgo (fallback). Domain filtering via allowed_domains/blocked_domains.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "limit": {"type": "integer", "description": "Max results (default 5)", "default": 5},
                "engine": {
                    "type": "string",
                    "enum": ["bing", "duckduckgo"],
                    "description": "Search engine to use. Default: bing (with fallback to duckduckgo)."
                },
                "allowed_domains": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Only include results from these domains"
                },
                "blocked_domains": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Exclude results from these domains"
                },
                "fetch_content": {
                    "type": "boolean",
                    "description": "Automatically fetch full page content for each result (default: true). Set to false for faster search with snippets only.",
                    "default": true
                },
                "fetch_max_chars": {
                    "type": "integer",
                    "description": "Max chars to fetch per page when fetch_content=true (default: 20000)",
                    "default": 20000
                }
            },
            "required": ["query"]
        }),
        category: Some(ToolCategory::Network),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("web_search: bad args: {e}"))),
    };

    // 空查询验证
    if a.query.trim().is_empty() {
        return Ok(ToolCallOutcome::error(
            "web_search: query must not be empty",
        ));
    }

    let options = SearchOptions {
        max_results: Some(a.limit),
        allowed_domains: a.allowed_domains.clone(),
        blocked_domains: a.blocked_domains.clone(),
    };

    // 语言感知引擎降级链
    let engines = engines_for_query(&a.query, a.engine.as_deref());

    match search_with_fallback(&a.query, &options, &engines).await {
        Ok(results) => {
            if results.is_empty() {
                return Ok(ToolCallOutcome::ok(format!("no results for: {}", a.query)));
            }

            // If fetch_content is disabled, return snippets only (legacy behavior)
            if !a.fetch_content {
                let out = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| match &r.snippet {
                        Some(s) => format!("{}. {} — {}\n   {}", i + 1, r.title, r.url, s),
                        None => format!("{}. {} — {}", i + 1, r.title, r.url),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(ToolCallOutcome::ok(out));
            }

            // Auto-fetch full page content for each search result (concurrently)
            let fetch_args: Vec<(usize, SearchResult)> = results.into_iter().enumerate().collect();

            let fetch_tasks: Vec<_> = fetch_args
                .into_iter()
                .map(|(idx, r)| {
                    let url = r.url.clone();
                    let title = r.title.clone();
                    let snippet = r.snippet.clone();
                    let max_chars = a.fetch_max_chars;
                    tokio::spawn(async move {
                        let fetch_input = serde_json::json!({
                            "url": url,
                            "max_chars": max_chars,
                        });
                        let content =
                            match crate::web::web_fetch::run(Path::new("."), fetch_input).await {
                                Ok(outcome) if !outcome.is_error => outcome.content,
                                Ok(outcome) => format!("[fetch failed: {}]", outcome.content),
                                Err(e) => format!("[fetch error: {e}]"),
                            };
                        (idx, title, url, snippet, content)
                    })
                })
                .collect();

            // Collect results, preserving original order
            let mut fetched: Vec<(usize, String, String, Option<String>, String)> =
                Vec::with_capacity(fetch_tasks.len());
            for task in fetch_tasks {
                if let Ok(item) = task.await {
                    fetched.push(item);
                }
            }
            fetched.sort_by_key(|(idx, _, _, _, _)| *idx);

            // Format output: snippet + full page content for each result
            let out = fetched
                .iter()
                .map(|(idx, title, url, snippet, content)| {
                    let header = match snippet {
                        Some(s) => format!("{}. {} — {}\n   摘要: {}", idx + 1, title, url, s),
                        None => format!("{}. {} — {}", idx + 1, title, url),
                    };
                    format!("{}\n   正文:\n{}", header, indent_content(content, 3))
                })
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            Ok(ToolCallOutcome::ok(out))
        }
        Err(SearchError::BlockedByAntiBot) => Ok(ToolCallOutcome::error(
            "All search engines blocked by anti-bot. Try again later or use web_fetch directly.",
        )),
        Err(e) => Ok(ToolCallOutcome::error(format!("web_search error: {e}"))),
    }
}

/// Indent each line of `content` by `spaces` spaces for readable nesting.
fn indent_content(content: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    content
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
