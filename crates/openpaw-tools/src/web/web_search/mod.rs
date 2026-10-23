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
use tokio_util::sync::CancellationToken;

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

/// 功能：声明原搜索工具和有界数量参数。
/// 参数：无。
/// 返回：搜索工具元数据。
/// 错误：无。
/// 状态：无。
/// 并发与取消：声明可共享，不发HTTP。
/// 权限：仅公开信息检索，不执行网页指令。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
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
                "limit": {"type": "integer", "minimum":1,"maximum":10, "description": "Max results (default 5)", "default": 5},
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
                    "type": "integer", "minimum":1,"maximum":20000,
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

/// 功能：兼容原入口搜索公开网页。
/// 参数：workspace为兼容工作根，args为原搜索输入。
/// 返回：原格式标题/URL/摘要与可选正文。
/// 错误：输入/引擎失败作为工具错误返回。
/// 状态：复用原引擎降级与抓取缓存。
/// 并发与取消：有取消上下文时使用run_cancellable，不分离遗留抓取任务。
/// 权限：保持原域名过滤与网页地址校验。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    run_cancellable(workspace, args, CancellationToken::new()).await
}

/// 功能：使用本次调用token搜索和托管正文抓取。
/// 参数：workspace为兼容工作根，args为原输入，cancel为调用token。
/// 返回：原格式搜索内容或工具错误。
/// 错误：取消返回Cancelled；输入/引擎错误沿用工具错误。
/// 状态：不创建新的存储/搜索服务。
/// 并发与取消：取消停止引擎等待，终止并等待所有正文任务再返回。
/// 权限：复用原搜索/域名过滤/抓取校验，不授予其他工具权限。
/// 关联：需求/设计REQ-OP-006、REQ-OP-010；DES-OP-004、DES-OP-007。
pub(crate) async fn run_cancellable(
    _workspace: &Path,
    args: serde_json::Value,
    cancel: CancellationToken,
) -> Result<ToolCallOutcome> {
    if cancel.is_cancelled() {
        return Err(openpaw_core::Error::Cancelled);
    }
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

    if !(1..=10).contains(&a.limit) || !(1..=20_000).contains(&a.fetch_max_chars) {
        return Ok(ToolCallOutcome::error(
            "web_search: limit must be 1..10 and fetch_max_chars 1..20000",
        ));
    }
    let options = SearchOptions {
        max_results: Some(a.limit),
        allowed_domains: a.allowed_domains.clone(),
        blocked_domains: a.blocked_domains.clone(),
    };

    // 语言感知引擎降级链
    let engines = engines_for_query(&a.query, a.engine.as_deref());

    let search = tokio::select! { biased;
        _ = cancel.cancelled() => return Err(openpaw_core::Error::Cancelled),
        result = search_with_fallback(&a.query, &options, &engines) => result,
    };
    match search {
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

            let out = fetch_results(results, a.fetch_max_chars, &cancel).await?;

            Ok(ToolCallOutcome::ok(out))
        }
        Err(SearchError::BlockedByAntiBot) => Ok(ToolCallOutcome::error(
            "All search engines blocked by anti-bot. Try again later or use web_fetch directly.",
        )),
        Err(e) => Ok(ToolCallOutcome::error(format!("web_search error: {e}"))),
    }
}

async fn fetch_results(
    results: Vec<SearchResult>,
    max_chars: usize,
    cancel: &CancellationToken,
) -> Result<String> {
    let mut tasks = tokio::task::JoinSet::new();
    for (idx, r) in results.into_iter().enumerate() {
        let child = cancel.child_token();
        tasks.spawn(async move {
            let input = serde_json::json!({"url":r.url,"max_chars":max_chars});
            let content =
                match crate::web::web_fetch::run_cancellable(Path::new("."), input, child).await {
                    Ok(outcome) if !outcome.is_error => outcome.content,
                    Ok(outcome) => format!("[fetch failed: {}]", outcome.content),
                    Err(e) => format!("[fetch error: {e}]"),
                };
            (idx, r.title, r.url, r.snippet, content)
        });
    }
    let mut fetched = Vec::new();
    while !tasks.is_empty() {
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                // abort只发出取消；等待JoinSet排空才能允许Host释放资源并确认终态。
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(openpaw_core::Error::Cancelled);
            }
            result = tasks.join_next() => match result {
                Some(Ok(item)) => fetched.push(item),
                Some(Err(e)) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return Err(openpaw_core::Error::ToolHost(format!("web_search page task failed: {e}")));
                }
                None => break,
            }
        }
    }
    if cancel.is_cancelled() {
        return Err(openpaw_core::Error::Cancelled);
    }
    fetched.sort_by_key(|(idx, _, _, _, _)| *idx);
    Ok(fetched
        .iter()
        .map(|(idx, title, url, snippet, content)| {
            let header = match snippet {
                Some(s) => format!("{}. {} — {}\n   摘要: {}", idx + 1, title, url, s),
                None => format!("{}. {} — {}", idx + 1, title, url),
            };
            format!("{}\n   正文:\n{}", header, indent_content(content, 3))
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d40_auto_fetch_tasks_close_before_cancel_returns() {
        if std::env::var("OPENPAW_D40_SEARCH_CHILD").as_deref() != Ok("1") {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "web::web_search::tests::d40_auto_fetch_tasks_close_before_cancel_returns",
                    "--nocapture",
                ])
                .env("OPENPAW_D40_SEARCH_CHILD", "1")
                .env("OPENPAW_WEB_TEST_ALLOW_LOOPBACK", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/slow", listener.local_addr().unwrap());
            let (ready, started) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                let (headers_ready, mut headers_received) = tokio::sync::mpsc::channel(2);
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let headers_ready = headers_ready.clone();
                    connections.spawn(async move {
                        let mut buf = [0; 8192];
                        let _ = stream.read(&mut buf).await;
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                            .await
                            .unwrap();
                        headers_ready.send(()).await.unwrap();
                        // 完整响应前连接必须被取消关闭，不能等到原30秒HTTP超时。
                        let closed = stream.read(&mut buf).await;
                        assert!(
                            matches!(closed, Ok(0))
                                || closed.as_ref().err().is_some_and(|e| matches!(
                                    e.kind(),
                                    std::io::ErrorKind::ConnectionReset
                                        | std::io::ErrorKind::ConnectionAborted
                                )),
                            "{closed:?}"
                        );
                    });
                }
                for _ in 0..2 {
                    headers_received.recv().await.unwrap();
                }
                let _ = ready.send(());
                while let Some(result) = connections.join_next().await {
                    result.unwrap();
                }
            });
            let cancel = CancellationToken::new();
            let child = cancel.clone();
            let results = (0..2)
                .map(|i| SearchResult {
                    title: format!("page{i}"),
                    url: url.clone(),
                    snippet: None,
                })
                .collect();
            let call = tokio::spawn(async move { fetch_results(results, 100, &child).await });
            tokio::time::timeout(std::time::Duration::from_secs(3), started)
                .await
                .unwrap()
                .unwrap();
            cancel.cancel();
            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(1), call)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(openpaw_core::Error::Cancelled)
            ));
            tokio::time::timeout(std::time::Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
        });
    }
}
