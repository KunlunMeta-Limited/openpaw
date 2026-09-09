//! `web_fetch` — fetch a URL and return its text content.
//!
//! Includes pre-approved domain list (higher max_chars) and
//! LRU cache (15min TTL, 50 entries) for repeated fetches.
//!
//! 测试专用 env 门控（`OPENPAW_WEB_TEST_ALLOW_LOOPBACK=1`）仅用于
//! E2E 的本地 mock server（监听在 127.0.0.1）：设置后 run() 跳过
//! SSRF/DNS-rebinding 校验。生产环境不设置该变量，校验始终生效。

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lru::LruCache;
use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

use crate::web::preapproved;

/// Cache entry for WebFetch results.
struct CacheEntry {
    content: String,
    fetched_at: Instant,
}

/// 全局 WebFetch 缓存 — 单 Mutex 设计。50 条，15 分钟 TTL。
static FETCH_CACHE: Mutex<Option<LruCache<String, CacheEntry>>> = Mutex::new(None);

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_CAPACITY: usize = 50;

/// 进程级复用的 HTTP 客户端（复用连接池与 TLS 握手）。
/// 配置保持不变：30s 总超时、10s 连接超时、禁用自动重定向
/// （重定向需逐跳手动处理以做 SSRF 校验）。
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build shared reqwest client for web_fetch")
    })
}

/// 构建缓存 key。
///
/// key 必须包含影响输出内容的全部参数：`max_chars` 决定截断长度。
/// 旧实现只按 URL 缓存，先以小的 max_chars 抓取后，TTL 内的大
/// max_chars 请求会命中截断缓存，返回错误精度的内容。
fn build_cache_key(url: &url::Url, max_chars: usize) -> String {
    format!("{}|{}", url.as_str(), max_chars)
}

#[derive(Deserialize)]
struct Args {
    url: String,
    #[serde(default = "default_max_chars")]
    max_chars: usize,
}

fn default_max_chars() -> usize {
    20_000
}

/// 测试专用门控变量：仅 E2E 进程设置为 "1"，生产环境永不设置。
const TEST_ALLOW_LOOPBACK_ENV: &str = "OPENPAW_WEB_TEST_ALLOW_LOOPBACK";

/// Test-only escape hatch shared by the web tools: the in-repo mock servers
/// bind on loopback, which the SSRF guard refuses. When the env var is "1",
/// loopback targets pass (private/link-local ranges still refuse); agents
/// cannot modify the host process environment, so the model cannot reach it.
pub(crate) fn loopback_test_allowance() -> bool {
    std::env::var_os(TEST_ALLOW_LOOPBACK_ENV).is_some_and(|v| v == "1")
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "web_fetch".into(),
        description: "Fetch a web page and return its content as Markdown (preserving headings, code blocks, and links).".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch"},
                "max_chars": {"type": "integer", "description": "Max characters to return (default 20000)"}
            },
            "required": ["url"]
        }),
        category: Some(ToolCategory::Network),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
    .with_description_fn(Arc::new(|input: &serde_json::Value| {
        input
            .get("url")
            .and_then(|v| v.as_str())
            .map(|u| format!("Fetching {u}"))
            .unwrap_or_else(|| "Fetching".to_string())
    }))
}

pub async fn run(_workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("web_fetch: bad args: {e}"))),
    };

    // Security: Validate URL to prevent SSRF (private IP, localhost, DNS rebinding, etc.)
    let mut current_url = match url::Url::parse(&a.url) {
        Ok(u) => u,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "web_fetch: invalid URL: {e}"
            )))
        }
    };

    // 测试门控：仅在 E2E 进程中放行本地 mock 目标（环回），生产环境恒 false，
    // 校验始终完整生效。
    let allow_loopback = loopback_test_allowance();

    if let Err(msg) = validate_url_ssrf(&current_url, allow_loopback) {
        return Ok(ToolCallOutcome::error(format!("web_fetch: {msg}")));
    }

    // Check pre-approved domain — raise max_chars for trusted documentation sites
    let is_preapproved = preapproved::is_preapproved_url(&current_url);
    let effective_max_chars = if is_preapproved {
        a.max_chars.max(preapproved::PREAPPROVED_MAX_CHARS)
    } else {
        a.max_chars
    };

    // Check cache — return cached result if within TTL.
    // key 纳入 effective_max_chars，避免不同截断精度的请求互相污染缓存。
    let cache_key = build_cache_key(&current_url, effective_max_chars);
    if let Some(cached) = get_cached(&cache_key) {
        return Ok(ToolCallOutcome::ok(cached));
    }

    // DNS rebinding protection — check the actually resolved IPs for
    // domain names, not just the literal host string.
    if let Err(msg) = check_dns_rebinding(&current_url, allow_loopback).await {
        return Ok(ToolCallOutcome::error(format!("web_fetch: {msg}")));
    }

    let client = http_client();

    // Follow redirects manually, applying the *same* SSRF validation
    // (scheme/hostname/IP + DNS rebinding) to every hop so redirect checks can
    // never be weaker than the initial check.
    const MAX_REDIRECTS: usize = 10;
    let mut resp = match client
        .get(current_url.as_str())
        .header("User-Agent", "openpaw/0.1")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(ToolCallOutcome::error(format!("web_fetch: {e}"))),
    };

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
                    "web_fetch: bad redirect URL: {e}"
                )))
            }
        };
        if let Err(msg) = validate_url_ssrf(&redirect_url, allow_loopback) {
            return Ok(ToolCallOutcome::error(format!(
                "web_fetch: redirect blocked: {msg}"
            )));
        }
        if let Err(msg) = check_dns_rebinding(&redirect_url, allow_loopback).await {
            return Ok(ToolCallOutcome::error(format!(
                "web_fetch: redirect blocked: {msg}"
            )));
        }
        current_url = redirect_url;
        resp = match client
            .get(current_url.as_str())
            .header("User-Agent", "openpaw/0.1")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolCallOutcome::error(format!(
                    "web_fetch: redirect failed: {e}"
                )))
            }
        };
    }

    if !resp.status().is_success() {
        return Ok(ToolCallOutcome::error(format!("HTTP {}", resp.status())));
    }

    // Check Content-Length before reading body to prevent OOM from
    // malicious servers streaming gigabytes of data.
    if let Some(len) = resp.content_length() {
        if len > 5 * 1024 * 1024 {
            return Ok(ToolCallOutcome::error(format!(
                "response too large: Content-Length {} bytes (max 5MB)",
                len
            )));
        }
    }

    // Limit response body size to 5MB to prevent OOM (secondary check)
    let body = match resp.bytes().await {
        Ok(bytes) => {
            if bytes.len() > 5 * 1024 * 1024 {
                return Ok(ToolCallOutcome::error(format!(
                    "response too large: {} bytes (max 5MB)",
                    bytes.len()
                )));
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!("web_fetch body: {e}")));
        }
    };

    let text = crate::web::html_to_md::html_to_markdown(&body);

    let truncated = if text.chars().count() > effective_max_chars {
        let t: String = text.chars().take(effective_max_chars).collect();
        format!("{t}\n... (truncated at {} chars)", effective_max_chars)
    } else {
        text
    };

    // Store in cache for future fetches
    put_cached(&cache_key, &truncated);

    Ok(ToolCallOutcome::ok(truncated))
}

/// 统一的 SSRF IP 判定。对 IPv4 阻断环回、私网、链路本地、未指定、广播、
/// 文档段、CGN（100.64.0.0/10）与基准测试段（198.18.0.0/15）；对 IPv6 阻断
/// 环回、链路本地、唯一本地与未指定地址；并递归检查 IPv4 映射/兼容形式的
/// IPv6 地址（如 `::ffff:127.0.0.1`），使其无法绕过 IPv4 检查。
#[allow(clippy::incompatible_msrv)]
pub(crate) fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || (octets[0] == 100 && octets[1] >= 64 && octets[1] <= 127)
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_unicast_link_local()
                || v6.is_unique_local()
                || v6.is_unspecified()
            {
                return true;
            }
            // IPv4-mapped (::ffff:x.x.x.x) and IPv4-compatible (::x.x.x.x)
            // IPv6 addresses must go through the full IPv4 checks, otherwise
            // ::ffff:127.0.0.1 bypasses Ipv6Addr::is_loopback() which returns
            // false for that form.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_private_ip(std::net::IpAddr::V4(v4));
            }
            false
        }
    }
}

/// 统一的 SSRF URL 校验：检查 scheme（仅 http/https）、主机名（阻断
/// `localhost`/`.localhost` 与明显的 localhost 字面量），并通过
/// `is_private_ip` 检查 IP 字面量。初始 URL 与每个重定向目标共用本函数，
/// 两条代码路径不可能出现口径分歧。`allow_loopback` 仅为测试门控
/// （`loopback_test_allowance()`）：放行环回目标以支持本地 mock，其余
/// 私网段仍然拒绝；生产环境该参数恒为 false。
pub(crate) fn validate_url_ssrf(
    url: &url::Url,
    allow_loopback: bool,
) -> std::result::Result<(), String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "only http/https schemes are allowed, got '{scheme}'"
        ));
    }
    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if is_private_ip(std::net::IpAddr::V4(ip)) && !(allow_loopback && ip.is_loopback())
                {
                    return Err(format!("refused to connect to private IP ({ip})"));
                }
            }
            url::Host::Ipv6(ip) => {
                if is_private_ip(std::net::IpAddr::V6(ip)) && !(allow_loopback && ip.is_loopback())
                {
                    return Err(format!("refused to connect to private IPv6 ({ip})"));
                }
            }
            url::Host::Domain(domain) => {
                let lower = domain.to_lowercase();
                let loopback_literal = lower == "127.0.0.1" || lower == "::1";
                if lower == "localhost"
                    || lower.ends_with(".localhost")
                    || lower == "0.0.0.0"
                    || (loopback_literal && !allow_loopback)
                {
                    return Err(format!("refused to connect to localhost ({domain})"));
                }
            }
        }
    }
    Ok(())
}

/// DNS rebinding 防护：`validate_url_ssrf` 只检查 URL 中的主机字符串，攻击者
/// 可让 DNS 在校验时返回公网 IP、连接时返回私网 IP（DNS rebinding），或返回
/// 多条记录其中之一指向内网。对域名（非 IP 字面量）执行 DNS 解析，用同一个
/// `is_private_ip` 检查每个解析结果。DNS 解析失败不在此阻断 — 交由 reqwest
/// 报错。
pub(crate) async fn check_dns_rebinding(
    url: &url::Url,
    allow_loopback: bool,
) -> std::result::Result<(), String> {
    let is_domain = url
        .host()
        .map(|h| matches!(h, url::Host::Domain(_)))
        .unwrap_or(false);
    if !is_domain {
        return Ok(());
    }
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let host_port = format!("{host}:{port}");
    // Bind the .await result before matching to avoid lifetime issues with the
    // future's temporary value.
    let resolved = tokio::net::lookup_host(&host_port).await;
    match resolved {
        Ok(addrs) => {
            for addr in addrs {
                if is_private_ip(addr.ip()) && !(allow_loopback && addr.ip().is_loopback()) {
                    return Err(format!(
                        "refused to connect to {host}: DNS resolved to private IP ({})",
                        addr.ip()
                    ));
                }
            }
        }
        Err(_) => { /* DNS resolution failed; let reqwest handle the error */ }
    }
    Ok(())
}

/// Get a cached WebFetch result if within TTL.
fn get_cached(key: &str) -> Option<String> {
    let mut guard = FETCH_CACHE.lock().ok()?;
    let cache = guard
        .get_or_insert_with(|| LruCache::new(std::num::NonZeroUsize::new(CACHE_CAPACITY).unwrap()));
    let entry = cache.get(key)?;
    if entry.fetched_at.elapsed() < CACHE_TTL {
        Some(entry.content.clone())
    } else {
        cache.pop(key);
        None
    }
}

/// Store a WebFetch result in cache.
fn put_cached(key: &str, content: &str) {
    if let Ok(mut guard) = FETCH_CACHE.lock() {
        let cache = guard.get_or_insert_with(|| {
            LruCache::new(std::num::NonZeroUsize::new(CACHE_CAPACITY).unwrap())
        });
        cache.put(
            key.to_string(),
            CacheEntry {
                content: content.to_string(),
                fetched_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_ip_rejects_loopback() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_rejects_private_ranges() {
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_rejects_ipv4_mapped_ipv6() {
        assert!(is_private_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_allows_public() {
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn validate_url_ssrf_rejects_localhost() {
        let url = url::Url::parse("http://localhost/").unwrap();
        assert!(validate_url_ssrf(&url, false).is_err());

        let url = url::Url::parse("http://127.0.0.1/").unwrap();
        assert!(validate_url_ssrf(&url, false).is_err());
    }

    /// 测试门控只放行环回目标——非环回私网段仍然拒绝。
    #[test]
    fn loopback_allowance_still_blocks_other_private_ranges() {
        let url = url::Url::parse("http://10.0.0.5/").unwrap();
        assert!(validate_url_ssrf(&url, true).is_err());
        let url = url::Url::parse("http://192.168.1.5/").unwrap();
        assert!(validate_url_ssrf(&url, true).is_err());
        // 环回在门控下放行（本地 mock 依赖此路径）。
        let url = url::Url::parse("http://127.0.0.1/").unwrap();
        assert!(validate_url_ssrf(&url, true).is_ok());
    }

    #[test]
    fn validate_url_ssrf_rejects_non_http_schemes() {
        let url = url::Url::parse("file:///etc/passwd").unwrap();
        assert!(validate_url_ssrf(&url, false).is_err());
    }

    #[test]
    fn validate_url_ssrf_allows_public_http() {
        let url = url::Url::parse("https://example.com/").unwrap();
        assert!(validate_url_ssrf(&url, false).is_ok());
    }

    /// 缓存 key 必须区分 max_chars，防止小 max_chars 的截断结果
    /// 污染大 max_chars 的请求。
    #[test]
    fn cache_key_distinguishes_max_chars() {
        let url = url::Url::parse("https://example.com/").unwrap();
        let small = build_cache_key(&url, 100);
        let large = build_cache_key(&url, 20_000);

        assert_ne!(small, large, "不同 max_chars 不得共享缓存条目");
        // 相同参数产生相同 key（缓存命中依赖这一点）
        assert_eq!(large, build_cache_key(&url, 20_000));
    }

    /// 进程级复用 Client 应返回同一实例。
    #[test]
    fn http_client_is_shared() {
        let a = http_client();
        let b = http_client();
        assert!(std::ptr::eq(a, b), "应复用同一个 Client 实例");
    }
}
