//! WebSearch 适配器架构 + 反爬修复。
//!
//! 统一的搜索引擎适配器 trait，支持 bing / duckduckgo 双引擎。
//! 多引擎降级 + 反爬检测 + 浏览器标头伪装。

pub mod adapters;
pub mod headers;
pub mod utils;

use std::path::Path;
use std::sync::{Mutex, OnceLock};

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

/// 功能：读取原搜索响应，拒绝丢失查询、超大正文和明确挑战页。
/// 参数：response为原HTTP响应，query为调用者的完整原查询。
/// 返回：有界搜索HTML供原解析器处理。
/// 错误：HTTP、读取、查询被改写或挑战错误；不回显响应及敏感URL。
/// 状态：不保存网页、不重试。
/// 并发与取消：沿用原reqwest超时与调用方取消。
/// 权限：不执行页面内容，不绕过引擎访问控制。
/// 关联：CHG-2026-0007第12.161节D174。
pub(crate) async fn search_page(
    mut response: reqwest::Response,
    query: &str,
) -> std::result::Result<String, SearchError> {
    let status = response.status();
    if status.as_u16() == 403 {
        return Err(SearchError::BlockedByAntiBot);
    }
    if status.as_u16() == 429 {
        return Err(SearchError::RateLimited);
    }
    if !status.is_success() {
        return Err(SearchError::Http(format!("HTTP {status} at response")));
    }
    if response
        .url()
        .query_pairs()
        .find(|(k, _)| k == "q")
        .is_none_or(|(_, v)| v != query)
    {
        return Err(SearchError::Parse(
            "redirect changed or removed query; results rejected".into(),
        ));
    }
    const MAX_BYTES: usize = 2 * 1024 * 1024;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| SearchError::Http(super::web_fetch::transport_diagnostic(&e, "search_body")))?
    {
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err(SearchError::Parse("search response exceeds 2MiB".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    let html = String::from_utf8(bytes)
        .map_err(|_| SearchError::Parse("search response is not UTF-8 HTML".into()))?;
    let lower = html.to_ascii_lowercase();
    if lower.contains("id=\"b_captcha\"")
        || lower.contains("id=\"anomaly-form\"")
        || lower.contains("id=\"challenge-form\"")
        || lower.contains("cf-chl-")
    {
        return Err(SearchError::BlockedByAntiBot);
    }
    Ok(html)
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

/// 功能：按原引擎顺序搜索，完全不匹配的非空结果也触发降级。
/// 参数：query为原查询，options为原过滤选项，engines为有序引擎。
/// 返回：有效候选；弱匹配仅在备用引擎不能改善时保留，不代表事实核实。
/// 错误：聚合真实失败；全部反爬时保留原错误类型。
/// 状态：只暂存引擎故障60秒，不缓存查询结果、不改写查询或增加任务检索额度。
/// 并发与取消：各引擎只尝试一次且最多8秒，整链共享12秒截止；调用方取消或到期丢弃真实请求。
/// 权限：保持原域名过滤，不执行网页指令。
/// 关联：CHG-2026-0007第12.161节D174。
pub async fn search_with_fallback(
    query: &str,
    options: &SearchOptions,
    engines: &[AdapterKind],
) -> std::result::Result<Vec<SearchResult>, SearchError> {
    search_diagnostic(query, options, engines)
        .await
        .map(|r| r.results)
}

struct SearchRun {
    results: Vec<SearchResult>,
    engine: &'static str,
    warnings: Vec<String>,
}

type AdapterAttempt = (
    &'static str,
    std::result::Result<Box<dyn WebSearchAdapter>, SearchError>,
);

#[derive(Default)]
struct EngineHealth {
    // 仅固定引擎名及脱敏类别；不是查询/页面缓存，不存网页或用户输入。
    cooldowns: Mutex<std::collections::BTreeMap<&'static str, (std::time::Instant, &'static str)>>,
}
impl EngineHealth {
    fn remaining(&self, engine: &str, now: std::time::Instant) -> Option<(u64, &'static str)> {
        let guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let (until, reason) = guard.get(engine)?;
        let duration = until.checked_duration_since(now)?;
        if duration.is_zero() {
            return None;
        }
        Some((duration.as_secs() + 1, *reason))
    }
    fn observe(
        &self,
        engine: &'static str,
        result: &std::result::Result<Vec<SearchResult>, SearchError>,
    ) {
        let reason = match result {
            Err(SearchError::Http(_)) => Some("transport_or_http"),
            Err(SearchError::RateLimited) => Some("rate_limited"),
            Err(SearchError::BlockedByAntiBot) => Some("anti_bot"),
            _ => None,
        };
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = reason {
            guard.insert(
                engine,
                (
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                    reason,
                ),
            );
        } else {
            guard.remove(engine);
        }
    }
}

// 只检测明显失配，不把词面相交当成语义正确；匹配程度不用于“审核通过”。
fn query_coverage(query: &str, results: &[SearchResult]) -> (usize, usize) {
    let mut terms = std::collections::BTreeSet::new();
    for word in query.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<_> = word.chars().collect();
        if chars.iter().any(|c| ('\u{3400}'..='\u{9fff}').contains(c)) {
            for pair in chars.windows(2) {
                terms.insert(pair.iter().collect::<String>().to_lowercase());
            }
        } else if word.len() >= 2
            && !matches!(
                word.to_ascii_lowercase().as_str(),
                "the" | "of" | "in" | "to" | "and" | "or" | "site"
            )
        {
            terms.insert(word.to_lowercase());
        }
    }
    let matched = terms
        .iter()
        .filter(|term| {
            results.iter().any(|r| {
                format!(
                    "{} {} {}",
                    r.title,
                    r.url,
                    r.snippet.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(term.as_str())
            })
        })
        .count();
    (matched, terms.len())
}

async fn search_diagnostic(
    query: &str,
    options: &SearchOptions,
    engines: &[AdapterKind],
) -> std::result::Result<SearchRun, SearchError> {
    let adapters = engines
        .iter()
        .map(|&kind| (kind.as_str(), create_adapter(kind)))
        .collect();
    static HEALTH: OnceLock<EngineHealth> = OnceLock::new();
    search_adapters(
        query,
        options,
        adapters,
        Some(HEALTH.get_or_init(EngineHealth::default)),
        engines.len() == 1,
    )
    .await
}

async fn search_adapters(
    query: &str,
    options: &SearchOptions,
    adapters: Vec<AdapterAttempt>,
    health: Option<&EngineHealth>,
    bypass_cooldown: bool,
) -> std::result::Result<SearchRun, SearchError> {
    search_adapters_bounded(
        query,
        options,
        adapters,
        health,
        bypass_cooldown,
        std::time::Duration::from_secs(8),
        std::time::Duration::from_secs(12),
    )
    .await
}

// 一个搜索调用共享截止时间，降级不能为每个后端重新获得30秒。
// 较慢后端失败仍保留已发现的弱候选和明确诊断，不冒充事实核验。
async fn search_adapters_bounded(
    query: &str,
    options: &SearchOptions,
    adapters: Vec<AdapterAttempt>,
    health: Option<&EngineHealth>,
    bypass_cooldown: bool,
    attempt_budget: std::time::Duration,
    total_budget: std::time::Duration,
) -> std::result::Result<SearchRun, SearchError> {
    if adapters.is_empty() {
        return Err(SearchError::InvalidQuery("no engines configured".into()));
    }
    let deadline = tokio::time::Instant::now() + total_budget;

    let mut errors: Vec<String> = Vec::new();
    let mut all_blocked = true;
    let mut weak = None;

    for (engine, adapter) in adapters {
        if tokio::time::Instant::now() >= deadline {
            all_blocked = false;
            errors.push("search: overall_deadline; remaining backends not attempted".into());
            break;
        }
        if !bypass_cooldown {
            if let Some((seconds, reason)) =
                health.and_then(|h| h.remaining(engine, std::time::Instant::now()))
            {
                all_blocked = false;
                errors.push(format!("{engine}: temporarily_deferred ({reason}, retry window ~{seconds}s); explicit engine can probe now"));
                continue;
            }
        }
        let adapter = match adapter {
            Ok(a) => a,
            Err(e) => {
                all_blocked = false;
                errors.push(format!("{engine}: {e}"));
                continue;
            }
        };
        let started = std::time::Instant::now();
        let response = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + attempt_budget),
            adapter.search(query, options),
        )
        .await
        .unwrap_or_else(|_| {
            Err(SearchError::Http(format!(
                "deadline at search_attempt; elapsed_ms={}; no complete response",
                started.elapsed().as_millis()
            )))
        });
        if let Some(health) = health {
            health.observe(engine, &response);
        }
        match response {
            Ok(results) if !results.is_empty() => {
                let (matched, total) = query_coverage(query, &results);
                if total > 0 && matched == 0 {
                    all_blocked = false;
                    errors.push(format!("{engine}: unrelated_results (no query terms in titles/URLs/snippets); not verified evidence"));
                } else if total >= 4 && matched * 4 < total {
                    all_blocked = false;
                    errors.push(format!("{engine}: weak_query_coverage ({matched}/{total}); generic results may not answer this question"));
                    if weak.is_none() {
                        weak = Some((results, engine));
                    }
                } else {
                    return Ok(SearchRun {
                        results,
                        engine,
                        warnings: errors,
                    });
                }
            }
            Ok(_) => {
                all_blocked = false;
                errors.push(format!("{engine}: no results"));
            }
            Err(SearchError::BlockedByAntiBot) => {
                errors.push(format!("{engine}: blocked by anti-bot"));
            }
            Err(e) => {
                all_blocked = false;
                errors.push(format!(
                    "{engine}: {e}; elapsed_ms={}",
                    started.elapsed().as_millis()
                ));
            }
        }
    }

    if let Some((results, engine)) = weak {
        return Ok(SearchRun {
            results,
            engine,
            warnings: errors,
        });
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
    /// When true, automatically fetch the full page content of each
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
    false
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
            with snippets by default. Set fetch_content=true only when full pages are needed. \
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
                    "description": "Default: bing with duckduckgo fallback and brief engine-failure cooldown. Explicit engine forces a fresh probe; use for intentional diagnostics or user request, not repeated retries."
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
                    "description": "Explicitly fetch full page content for each result (default: false). Prefer discovering links first, then fetching relevant pages.",
                    "default": false
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

    let search_started = std::time::Instant::now();
    let search = tokio::select! { biased;
        _ = cancel.cancelled() => return Err(openpaw_core::Error::Cancelled),
        result = search_diagnostic(&a.query, &options, &engines) => result,
    };
    match search {
        Ok(search) => {
            let SearchRun {
                results,
                engine,
                warnings,
            } = search;
            let notice = if warnings.is_empty() {
                String::new()
            } else {
                format!("Search diagnostics: {}. Results are source candidates, not verified answers. Do not repeat identical failing queries or guess page URLs.\n\n", warnings.join("; "))
            };
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
                let mut outcome = ToolCallOutcome::ok(format!("{notice}{out}"));
                outcome.structured = Some(
                    serde_json::json!({"engine":engine,"warnings":warnings,"search_ms":search_started.elapsed().as_millis(),"fetch_content":false}),
                );
                return Ok(outcome);
            }

            let (out, failed) = fetch_results(results, a.fetch_max_chars, &cancel).await?;

            let mut outcome = ToolCallOutcome::ok(format!("{notice}{out}"));
            outcome.structured = Some(
                serde_json::json!({"engine":engine,"warnings":warnings,"fetch_content":true,"fetch_failed_count":failed,"pages_verified":false}),
            );
            Ok(outcome)
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
) -> Result<(String, usize)> {
    let mut tasks = tokio::task::JoinSet::new();
    for (idx, r) in results.into_iter().enumerate() {
        let child = cancel.child_token();
        tasks.spawn(async move {
            let input = serde_json::json!({"url":r.url,"max_chars":max_chars});
            let (content, failed) =
                match crate::web::web_fetch::run_cancellable(Path::new("."), input, child).await {
                    Ok(outcome) if !outcome.is_error => (outcome.content, false),
                    Ok(outcome) => (format!("[fetch failed: {}]", outcome.content), true),
                    Err(e) => (format!("[fetch error: {e}]"), true),
                };
            (idx, r.title, r.url, r.snippet, content, failed)
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
    fetched.sort_by_key(|(idx, _, _, _, _, _)| *idx);
    let failures = fetched
        .iter()
        .filter(|(_, _, _, _, _, failed)| *failed)
        .count();
    let text = fetched
        .iter()
        .map(|(idx, title, url, snippet, content, _)| {
            let header = match snippet {
                Some(s) => format!("{}. {} — {}\n   摘要: {}", idx + 1, title, url, s),
                None => format!("{}. {} — {}", idx + 1, title, url),
            };
            format!("{}\n   正文:\n{}", header, indent_content(content, 3))
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    Ok((text, failures))
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

    struct SearchFixture(Vec<SearchResult>);
    #[async_trait]
    impl WebSearchAdapter for SearchFixture {
        fn name(&self) -> &'static str {
            "fixture"
        }
        async fn search(
            &self,
            _: &str,
            _: &SearchOptions,
        ) -> std::result::Result<Vec<SearchResult>, SearchError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn d174_unrelated_nonempty_results_fall_back_and_weak_results_remain_explicit() {
        let result = |title: &str| SearchResult {
            title: title.into(),
            url: "https://example.com/page".into(),
            snippet: None,
        };
        let adapters = vec![
            (
                "bing",
                Ok(
                    Box::new(SearchFixture(vec![result("基金业协会和证券从业平台")]))
                        as Box<dyn WebSearchAdapter>,
                ),
            ),
            (
                "duckduckgo",
                Ok(
                    Box::new(SearchFixture(vec![result("牛街小吃 爆肚 豆汁 店铺资料")]))
                        as Box<dyn WebSearchAdapter>,
                ),
            ),
        ];
        let output = search_adapters(
            "牛街小吃 爆肚 豆汁",
            &SearchOptions::default(),
            adapters,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(output.engine, "duckduckgo");
        assert!(output.warnings[0].contains("unrelated_results"));
        assert!(output.results[0].title.contains("牛街"));
        let weak = search_adapters(
            "北京地铁 票价 起步价 公里 里程计价 规则",
            &SearchOptions::default(),
            vec![
                (
                    "bing",
                    Ok(Box::new(SearchFixture(vec![result("北京概况")]))
                        as Box<dyn WebSearchAdapter>),
                ),
                ("duckduckgo", Err(SearchError::RateLimited)),
            ],
            None,
            false,
        )
        .await
        .unwrap();
        assert!(weak
            .warnings
            .iter()
            .any(|w| w.contains("weak_query_coverage")));
        assert!(weak.warnings.iter().any(|w| w.contains("Rate limited")));
        assert!(!default_fetch_content());
        let input: Args =
            serde_json::from_value(serde_json::json!({"query":"normal query"})).unwrap();
        assert!(!input.fetch_content);
        let failed = search_adapters(
            "交通费用",
            &SearchOptions::default(),
            vec![(
                "bing",
                Ok(Box::new(SearchFixture(vec![result("基金平台")])) as Box<dyn WebSearchAdapter>),
            )],
            None,
            false,
        )
        .await
        .err()
        .unwrap();
        assert!(failed.to_string().contains("unrelated_results"));
    }

    #[tokio::test]
    async fn d183_shared_deadline_cancels_slow_backends_and_preserves_fast_fallback() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering::SeqCst},
            Arc,
        };
        struct Slow {
            live: Arc<AtomicUsize>,
        }
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, SeqCst);
            }
        }
        #[async_trait]
        impl WebSearchAdapter for Slow {
            fn name(&self) -> &'static str {
                "slow"
            }
            async fn search(
                &self,
                _: &str,
                _: &SearchOptions,
            ) -> std::result::Result<Vec<SearchResult>, SearchError> {
                self.live.fetch_add(1, SeqCst);
                let _guard = Guard(self.live.clone());
                std::future::pending().await
            }
        }
        let live = Arc::new(AtomicUsize::new(0));
        let slow = || Ok(Box::new(Slow { live: live.clone() }) as Box<dyn WebSearchAdapter>);
        let fast = || {
            Ok(Box::new(SearchFixture(vec![SearchResult {
                title: "Rust async runtime".into(),
                url: "https://example.org/async".into(),
                snippet: None,
            }])) as Box<dyn WebSearchAdapter>)
        };
        let health = EngineHealth::default();
        let found = search_adapters_bounded(
            "Rust async",
            &SearchOptions::default(),
            vec![("bing", slow()), ("duckduckgo", fast())],
            Some(&health),
            false,
            std::time::Duration::from_millis(25),
            std::time::Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert_eq!(found.engine, "duckduckgo");
        assert!(found
            .warnings
            .iter()
            .any(|v| v.contains("deadline") && v.contains("elapsed_ms=")));
        assert_eq!(
            live.load(SeqCst),
            0,
            "deadline must drop the actual backend future"
        );
        assert!(health
            .remaining("bing", std::time::Instant::now())
            .is_some());
        let started = std::time::Instant::now();
        let failed = search_adapters_bounded(
            "Rust async",
            &SearchOptions::default(),
            vec![("bing", slow()), ("duckduckgo", slow())],
            None,
            true,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(45),
        )
        .await
        .err()
        .unwrap();
        assert!(failed.to_string().contains("deadline"));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "fallback must share one overall deadline"
        );
        assert_eq!(live.load(SeqCst), 0);
        let cancel = CancellationToken::new();
        {
            let options = SearchOptions::default();
            let call = search_adapters_bounded(
                "Rust async",
                &options,
                vec![("bing", slow())],
                None,
                true,
                std::time::Duration::from_secs(8),
                std::time::Duration::from_secs(12),
            );
            tokio::pin!(call);
            tokio::select! { biased; _=&mut call=>panic!("backend must still be pending"), _=tokio::time::sleep(std::time::Duration::from_millis(10))=>cancel.cancel() }
            assert!(cancel.is_cancelled());
            // 与原工具取消分支相同：退出作用域后真正丢弃调用，而不是留后台请求。
        }
        assert_eq!(
            live.load(SeqCst),
            0,
            "cancellation must drop the actual backend request"
        );
    }

    #[tokio::test]
    async fn d174_engine_failure_cooldown_is_temporary_and_explicit_probe_restores_search() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering::SeqCst},
            Arc,
        };
        struct Probe {
            calls: Arc<AtomicUsize>,
            fails: bool,
        }
        #[async_trait]
        impl WebSearchAdapter for Probe {
            fn name(&self) -> &'static str {
                "probe"
            }
            async fn search(
                &self,
                _: &str,
                _: &SearchOptions,
            ) -> std::result::Result<Vec<SearchResult>, SearchError> {
                self.calls.fetch_add(1, SeqCst);
                if self.fails {
                    Err(SearchError::Http("timeout at search_request".into()))
                } else {
                    Ok(vec![SearchResult {
                        title: "Rust async runtime".into(),
                        url: "https://example.org/async".into(),
                        snippet: None,
                    }])
                }
            }
        }
        let health = EngineHealth::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let attempt = |fails| {
            vec![(
                "probe",
                Ok(Box::new(Probe {
                    calls: calls.clone(),
                    fails,
                }) as Box<dyn WebSearchAdapter>),
            )]
        };
        let failed = search_adapters(
            "Rust async",
            &SearchOptions::default(),
            attempt(true),
            Some(&health),
            false,
        )
        .await
        .err()
        .unwrap();
        assert!(failed.to_string().contains("timeout"));
        assert_eq!(calls.load(SeqCst), 1);
        let skipped = search_adapters(
            "unrelated new query",
            &SearchOptions::default(),
            attempt(true),
            Some(&health),
            false,
        )
        .await
        .err()
        .unwrap();
        assert!(skipped.to_string().contains("temporarily_deferred"));
        assert_eq!(
            calls.load(SeqCst),
            1,
            "暂时不可达引擎不应为每个查询重新等超时"
        );
        assert!(health
            .remaining(
                "probe",
                std::time::Instant::now() + std::time::Duration::from_secs(61)
            )
            .is_none());
        let restored = search_adapters(
            "Rust async",
            &SearchOptions::default(),
            attempt(false),
            Some(&health),
            true,
        )
        .await
        .unwrap();
        assert_eq!(restored.engine, "probe");
        assert_eq!(calls.load(SeqCst), 2);
        assert!(health
            .remaining("probe", std::time::Instant::now())
            .is_none());
        search_adapters(
            "Rust async",
            &SearchOptions::default(),
            attempt(false),
            Some(&health),
            false,
        )
        .await
        .unwrap();
        assert_eq!(calls.load(SeqCst), 3);
        // 空结果/低相关不等于引擎不可达，不屏蔽下一查询。
        health.observe("probe", &Ok(vec![]));
        assert!(health
            .remaining("probe", std::time::Instant::now())
            .is_none());
        health.observe("probe", &Err(SearchError::RateLimited));
        assert!(health
            .remaining("probe", std::time::Instant::now())
            .is_some());
        health.observe("probe", &Ok(vec![]));
        assert!(health
            .remaining("probe", std::time::Instant::now())
            .is_none());
    }

    #[tokio::test]
    async fn d174_query_redirect_challenge_and_http_errors_are_not_empty_success() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, location, body, expected) in [
            (
                "302 Found",
                "/other?q=changed",
                "",
                "changed or removed query",
            ),
            (
                "200 OK",
                "",
                "<form id=\"challenge-form\">check</form>",
                "anti-bot",
            ),
            (
                "429 Too Many Requests",
                "",
                "secret-token-in-response",
                "Rate limited",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                for step in 0..if location.is_empty() { 1 } else { 2 } {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = [0; 8192];
                    let n = stream.read(&mut request).await.unwrap();
                    if step == 0 {
                        assert!(String::from_utf8_lossy(&request[..n]).contains("q=Rust%20async"));
                    }
                    let response = if step == 0 && !location.is_empty() {
                        format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    } else {
                        format!(
                            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            if step == 0 { status } else { "200 OK" },
                            body.len()
                        )
                    };
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let adapter = adapters::duckduckgo::DuckDuckGoAdapter::new()
                .unwrap()
                .with_base_url(format!("http://{addr}/html"));
            let error = adapter
                .search("Rust async", &SearchOptions::default())
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret-token"));
            server.await.unwrap();
        }
    }

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

            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base=format!("http://{}",listener.local_addr().unwrap());
            let server=tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut stream,_)=listener.accept().await.unwrap();let mut request=[0;8192];
                    let n=stream.read(&mut request).await.unwrap();
                    let binary=String::from_utf8_lossy(&request[..n]).contains("/binary");
                    let body=if binary{"secret binary response"}else{"<h1>Useful page [fetch failed: literal label]</h1>"};
                    let kind=if binary{"image/jpeg"}else{"text/html"};
                    let response=format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let results=vec![
                SearchResult{title:"Binary".into(),url:format!("{base}/binary"),snippet:None},
                SearchResult{title:"Text".into(),url:format!("{base}/text"),snippet:None},
            ];
            let (text,failures)=fetch_results(results,100,&CancellationToken::new()).await.unwrap();
            assert_eq!(failures,1,"失败数必须按真实结果统计，不能由网页中的同名标签伪造");
            assert!(text.contains("unsupported_content"));assert!(text.contains("Useful page"));
            assert!(!text.contains("secret binary response"));
            assert!(text.find("Binary").unwrap()<text.find("Text").unwrap());
            server.await.unwrap();
        });
    }
}
