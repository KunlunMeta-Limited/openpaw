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
use tokio_util::sync::CancellationToken;

use crate::web::preapproved;

/// Cache entry for WebFetch results.
struct CacheEntry {
    // 多留一个字符，准确区分恰好达到上限与正文仍有后续。
    content: String,
    fetched_at: Instant,
}

/// 全局 WebFetch 缓存 — 单 Mutex 设计。50 条，15 分钟 TTL。
static FETCH_CACHE: Mutex<Option<LruCache<String, CacheEntry>>> = Mutex::new(None);

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_CAPACITY: usize = 50;
const MAX_CACHED_CHARS: usize = 100_001;

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

// 缓存已通过原安全与正文检查的文本，截取参数只影响展示，不影响来源身份。
// 不缓存截断提示；同页从小范围扩读时仍能获得后续原文。
fn render_excerpt(content: &str, max_chars: usize) -> String {
    let mut chars = content.chars();
    let excerpt: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{excerpt}\n... (truncated at {max_chars} chars)")
    } else {
        excerpt
    }
}

#[derive(Deserialize)]
struct Args {
    url: String,
    #[serde(default = "default_max_chars")]
    max_chars: usize,
    #[serde(default)]
    fresh: bool,
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

/// 功能：声明原网页读取工具及可选实时抓取参数。
/// 参数：无。
/// 返回：原工具元数据与JSON Schema。
/// 错误：无。
/// 状态：无。
/// 并发与取消：声明可共享，不发请求。
/// 权限：仅声明公开HTTP读取能力，不授予工作区外文件权限。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "web_fetch".into(),
        description: "Fetch a web page as Markdown. max_chars counts Unicode characters from the start; offset/pagination is not supported. Expand max_chars to read more of the same page; cached body is reused unless fresh=true. Stop when the needed facts are obtained. Preserves headings, code blocks, and links.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch"},
                "max_chars": {"type": "integer", "minimum":1,"maximum":100000,"description": "Max characters to return (default 20000)"},
                "fresh": {"type":"boolean","default":false,"description":"Bypass local cache for time-sensitive queries. Returns source URL and retrieval time; retrieval time is not the page observation/update time."}
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

/// 功能：兼容原入口读取公开网页。
/// 参数：workspace为兼容工作根，args为url/max_chars及可选fresh。
/// 返回：Markdown或明确的工具错误。
/// 错误：HTTP与输入错误作为工具错误返回。
/// 状态：默认保留原十五分钟缓存；fresh绕过旧缓存。
/// 并发与取消：内部独立token；有取消上下文的调用使用run_cancellable。
/// 权限：保留原URL和逐跳地址校验，不执行网页脚本。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    run_cancellable(workspace, args, CancellationToken::new()).await
}

/// 功能：按调用取消上下文读取公开网页。
/// 参数：workspace为兼容工作根，args为工具输入，cancel为本次调用token。
/// 返回：Markdown或工具错误。
/// 错误：取消返回Cancelled；其余沿用原工具错误。
/// 状态：成功后更新完整缓存；不保存部分响应。
/// 并发与取消：取消丢弃纯HTTP等待，不创建分离任务。
/// 权限：不绕过URL/DNS预查和重定向校验。
/// 关联：需求/设计REQ-OP-006、REQ-OP-010；DES-OP-004、DES-OP-007。
pub(crate) async fn run_cancellable(
    _workspace: &Path,
    args: serde_json::Value,
    cancel: CancellationToken,
) -> Result<ToolCallOutcome> {
    fetch_with_deadline(args, cancel, Duration::from_secs(30)).await
}

async fn fetch_with_deadline(
    args: serde_json::Value,
    cancel: CancellationToken,
    timeout: Duration,
) -> Result<ToolCallOutcome> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(openpaw_core::Error::Cancelled),
        _ = tokio::time::sleep(timeout) => Ok(web_error("deadline", "dns/request/redirect/body", true,
            "单次网页读取总时限已到；未取得完整正文。请换有效来源，勿将其当作模型响应超时。")),
        result = fetch(args, &cancel) => result,
    }
}

fn web_error(kind: &str, stage: &str, retryable: bool, message: &str) -> ToolCallOutcome {
    let mut outcome = ToolCallOutcome::error(format!("web_fetch [{kind}/{stage}]: {message}"));
    outcome.structured = Some(serde_json::json!({"kind":kind,"stage":stage,"retryable":retryable}));
    outcome
}

/// 功能：将HTTP错误原因链归类为固定脱敏诊断。
/// 参数：error为原reqwest错误；stage为可信调用阶段。
/// 返回：固定错误类别及阶段，不回显URL、凭据或响应正文。
/// 错误：未知原因明确标为transport，不推断服务不可用。
/// 状态：不重试、不缓存。
/// 并发与取消：纯计算，不改变原取消链。
/// 权限：仅公开诊断，不增加网络访问能力。
/// 关联：CHG-2026-0007第12.161节D175。
pub(crate) fn transport_diagnostic(error: &reqwest::Error, stage: &str) -> String {
    use std::error::Error;
    let mut chain = String::new();
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    for _ in 0..8 {
        let Some(cause) = current else { break };
        chain.push_str(&cause.to_string().to_ascii_lowercase());
        current = cause.source();
    }
    let kind = if error.is_timeout() && error.is_connect() {
        "connect_timeout"
    } else if error.is_timeout() {
        "timeout"
    } else if chain.contains("dns")
        || chain.contains("resolve")
        || chain.contains("name or service")
    {
        "dns"
    } else if chain.contains("certificate") || chain.contains("tls") {
        "tls"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() || error.is_decode() {
        "body_read"
    } else {
        "transport"
    };
    format!("{kind} at {stage}; no complete response; verify connection or use another source")
}

// 仅识别显式charset参数；不猜测编码、不将未知声明降级为成功。
fn declared_charset(
    value: &str,
) -> std::result::Result<Option<&'static encoding_rs::Encoding>, ()> {
    let mut found = None;
    for parameter in value.split(';').skip(1) {
        let parameter = parameter.trim();
        let Some((name, label)) = parameter.split_once('=') else {
            if parameter.eq_ignore_ascii_case("charset") {
                return Err(());
            }
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("charset") {
            continue;
        }
        let label = label.trim();
        let label = if label.starts_with('"') || label.starts_with('\'') {
            let quote = label.as_bytes()[0] as char;
            label
                .strip_prefix(quote)
                .and_then(|s| s.strip_suffix(quote))
                .ok_or(())?
        } else {
            label
        };
        agree_charset(
            &mut found,
            encoding_rs::Encoding::for_label(label.as_bytes()).ok_or(())?,
        )?;
    }
    Ok(found)
}

fn agree_charset(
    found: &mut Option<&'static encoding_rs::Encoding>,
    next: &'static encoding_rs::Encoding,
) -> std::result::Result<(), ()> {
    if found.is_some_and(|previous| previous != next) {
        return Err(());
    }
    *found = Some(next);
    Ok(())
}

/// 功能：严格解码已有文字响应，兼容明确声明的传统网页编码。
/// 参数：bytes为已受5MiB上限约束的完整正文，content_type为实际HTTP头。
/// 返回：有效UTF-8原文或按明确编码严格解出的Unicode文本。
/// 错误：未知/冲突/损坏编码、无声明的非UTF-8及二进制NUL返回拒绝。
/// 状态：无缓存写入；只在完整成功后由原调用链缓存。
/// 并发与取消：有界同步解码，不创建任务；原调用链在返回前检查取消。
/// 权限：不执行页面，不增加网络权限；只扫描前1024字节的HTML声明。
/// 关联：CHG-2026-0007第12.164节r38 r2，REQ-OP-010、DES-OP-007。
fn decode_web_text<'a>(
    bytes: &'a [u8],
    content_type: &str,
) -> std::result::Result<std::borrow::Cow<'a, str>, ()> {
    let mut encoding = declared_charset(content_type)?;
    let bom_length = if let Some((bom, length)) = encoding_rs::Encoding::for_bom(bytes) {
        agree_charset(&mut encoding, bom)?;
        length
    } else {
        0
    };
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    if media_type.is_empty()
        || media_type.eq_ignore_ascii_case("text/html")
        || media_type.eq_ignore_ascii_case("application/xhtml+xml")
    {
        // 只扫描ASCII标记；空格替代非ASCII仅用于定位声明，不是正文的有损解码。
        let prefix_bytes = &bytes[..bytes.len().min(1024)];
        let prefix: String =
            if encoding.is_some_and(|e| e == encoding_rs::UTF_16LE || e == encoding_rs::UTF_16BE) {
                // 明确UTF-16时按码元定位ASCII标记；扫描窗口仍按原字节计量。
                prefix_bytes[bom_length..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        let unit = if encoding == Some(encoding_rs::UTF_16LE) {
                            u16::from_le_bytes([pair[0], pair[1]])
                        } else {
                            u16::from_be_bytes([pair[0], pair[1]])
                        };
                        if unit <= 0x7f {
                            char::from(unit as u8)
                        } else {
                            ' '
                        }
                    })
                    .collect()
            } else {
                prefix_bytes
                    .iter()
                    .map(|&b| if b.is_ascii() { char::from(b) } else { ' ' })
                    .collect()
            };
        static META: OnceLock<regex::Regex> = OnceLock::new();
        static ATTR: OnceLock<regex::Regex> = OnceLock::new();
        let tags = META.get_or_init(|| regex::Regex::new(r#"(?is)<!--.*?(?:-->|$)|<script\b[^>]*>.*?(?:</script\s*>|$)|<style\b[^>]*>.*?(?:</style\s*>|$)|<meta\b(?:[^"'<>]|"[^"]*"|'[^']*')*>"#).expect("constant HTML charset tag pattern"));
        let attributes = ATTR.get_or_init(|| regex::Regex::new(r#"(?i)(?:^|\s)([a-z_:][-a-z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#).expect("constant HTML charset attribute pattern"));
        for tag in tags.find_iter(&prefix) {
            if !tag
                .as_str()
                .get(..5)
                .is_some_and(|s| s.eq_ignore_ascii_case("<meta"))
            {
                continue;
            }
            let mut http_equiv = None;
            let mut content = None;
            for attribute in attributes.captures_iter(tag.as_str()) {
                let name = &attribute[1];
                let value = attribute
                    .get(2)
                    .or_else(|| attribute.get(3))
                    .or_else(|| attribute.get(4))
                    .ok_or(())?
                    .as_str();
                if name.eq_ignore_ascii_case("charset") {
                    agree_charset(
                        &mut encoding,
                        encoding_rs::Encoding::for_label(value.as_bytes()).ok_or(())?,
                    )?;
                } else if name.eq_ignore_ascii_case("http-equiv") {
                    http_equiv = Some(value);
                } else if name.eq_ignore_ascii_case("content") {
                    content = Some(value);
                }
            }
            if http_equiv.is_some_and(|v| v.eq_ignore_ascii_case("content-type")) {
                if let Some(declared) = content.map(declared_charset).transpose()?.flatten() {
                    agree_charset(&mut encoding, declared)?;
                }
            }
        }
    }
    let is_utf16 =
        encoding.is_some_and(|e| e == encoding_rs::UTF_16LE || e == encoding_rs::UTF_16BE);
    if bom_length == 0 && !is_utf16 {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return if text.contains('\0') {
                Err(())
            } else {
                Ok(text.into())
            };
        }
    }
    let decoded = encoding
        .ok_or(())?
        .decode_without_bom_handling_and_without_replacement(&bytes[bom_length..])
        .ok_or(())?;
    if decoded.contains('\0') {
        Err(())
    } else {
        Ok(decoded)
    }
}

async fn fetch(args: serde_json::Value, cancel: &CancellationToken) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("web_fetch: bad args: {e}"))),
    };

    if !(1..=100_000).contains(&a.max_chars) {
        return Ok(ToolCallOutcome::error(
            "web_fetch: max_chars must be between 1 and 100000",
        ));
    }
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
    // 保留原请求URL（含查询）隔离，正文扩读无需重新联网。
    let cache_key = current_url.as_str().to_owned();
    if !a.fresh {
        if let Some(cached) = get_cached(&cache_key, effective_max_chars) {
            return Ok(ToolCallOutcome::ok(cached));
        }
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
        Err(e) => {
            return Ok(web_error(
                "transport",
                "request",
                true,
                &transport_diagnostic(&e, "request"),
            ))
        }
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
                return Ok(web_error(
                    "transport",
                    "redirect",
                    true,
                    &transport_diagnostic(&e, "redirect"),
                ))
            }
        };
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let advice = match status.as_u16() {
            401 | 403 => "访问被拒绝；不要重复请求同一受限地址，换公开来源，不绕过访问控制",
            404 | 410 => "页面不存在；从搜索结果发现真实链接，不继续猜测目录",
            429 => "来源限流；稍后再试或换来源，不立即循环请求",
            500..=599 => "来源服务异常；保留已有资料，换来源或稍后再试",
            _ => "未取得成功响应；核对来源地址",
        };
        return Ok(web_error(
            "http_status",
            "response",
            status.is_server_error() || status.as_u16() == 429,
            &format!("HTTP {status}; {advice}"),
        ));
    }

    let content_type_header = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let content_type = content_type_header
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !content_type.is_empty()
        && !(content_type.starts_with("text/")
            || matches!(
                content_type.as_str(),
                "application/json" | "application/xml" | "application/xhtml+xml"
            )
            || content_type.ends_with("+json")
            || content_type.ends_with("+xml"))
    {
        return Ok(web_error(
            "unsupported_content",
            "headers",
            false,
            "来源返回非文本文件，web_fetch不进行图片/PDF/二进制解析；请使用可读正文来源。",
        ));
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

    // 必须在追加前检查上限；未知长度/分块响应不能先整体读取再拒绝。
    let mut bytes = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if chunk.len() > 5 * 1024 * 1024 - bytes.len() {
                    return Ok(ToolCallOutcome::error(
                        "response too large: body exceeds 5MB",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Ok(web_error(
                    "transport",
                    "body",
                    true,
                    &transport_diagnostic(&e, "body"),
                ))
            }
        }
    }
    let body = match decode_web_text(&bytes, &content_type_header) {
        Ok(body) => body,
        Err(()) => return Ok(web_error(
            "unsupported_encoding",
            "body",
            false,
            "响应编码未知、声明冲突、正文损坏或包含二进制字节；仅接受UTF-8或明确声明且可严格解码的文本，请换可读来源。",
        )),
    };

    let text = crate::web::html_to_md::html_to_markdown(&body);
    if text.trim().is_empty() {
        return Ok(web_error(
            "empty_content",
            "body",
            false,
            "响应没有可读正文；不能视为资料核实成功。",
        ));
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains("id=\"challenge-form\"")
        || lower.contains("id=\"anomaly-form\"")
        || lower.contains("cf-chl-")
        || lower.contains("<title>just a moment")
    {
        return Ok(web_error(
            "challenge",
            "body",
            false,
            "来源返回验证挑战页，未取得正文；请换公开来源。",
        ));
    }

    let truncated = render_excerpt(&text, effective_max_chars);
    if cancel.is_cancelled() {
        return Err(openpaw_core::Error::Cancelled);
    }
    // 成功才更新正文缓存；fresh仍强制联网，原返回的抓取时间不进入缓存。
    put_cached(&cache_key, &text);
    if a.fresh {
        let retrieved = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| openpaw_core::Error::ToolHost("system clock precedes Unix epoch".into()))?
            .as_millis();
        return Ok(ToolCallOutcome::ok(format!("source_url: {current_url}\nretrieved_at_unix_ms: {retrieved}\nretrieval_mode: fresh (origin/CDN data may still be old; verify page data date)\n\n{truncated}")));
    }
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
fn get_cached(key: &str, max_chars: usize) -> Option<String> {
    let mut guard = FETCH_CACHE.lock().ok()?;
    let cache = guard
        .get_or_insert_with(|| LruCache::new(std::num::NonZeroUsize::new(CACHE_CAPACITY).unwrap()));
    let entry = cache.get(key)?;
    if entry.fetched_at.elapsed() < CACHE_TTL {
        Some(render_excerpt(&entry.content, max_chars))
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
                content: content.chars().take(MAX_CACHED_CHARS).collect(),
                fetched_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d175_total_deadline_covers_redirect_and_body_and_cancel_closes_connection() {
        if std::env::var("OPENPAW_D175_FETCH_CHILD").as_deref() != Ok("1") {
            let status=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","web::web_fetch::tests::d175_total_deadline_covers_redirect_and_body_and_cancel_closes_connection","--nocapture"])
                .env("OPENPAW_D175_FETCH_CHILD","1").env(TEST_ALLOW_LOOPBACK_ENV,"1").status().unwrap();
            assert!(status.success());
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use tokio::io::{AsyncReadExt,AsyncWriteExt};
            for cancelled in [false,true] {
                let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url=format!("http://{}/start",listener.local_addr().unwrap());
                let (ready,received)=tokio::sync::oneshot::channel();
                let server=tokio::spawn(async move {
                    let (mut first,_)=listener.accept().await.unwrap();let mut request=[0;8192];assert!(first.read(&mut request).await.unwrap()>0);
                    first.write_all(b"HTTP/1.1 302 Found\r\nLocation: /end\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();drop(first);
                    let (mut second,_)=listener.accept().await.unwrap();assert!(second.read(&mut request).await.unwrap()>0);
                    second.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100\r\n\r\n").await.unwrap();
                    let _=ready.send(());
                    let closed=second.read(&mut request).await;
                    assert!(matches!(closed,Ok(0))||closed.is_err(),"{closed:?}");
                });
                let cancel=CancellationToken::new();let token=cancel.clone();
                let call=tokio::spawn(async move {fetch_with_deadline(serde_json::json!({"url":url}),token,Duration::from_millis(500)).await});
                tokio::time::timeout(Duration::from_secs(2),received).await.unwrap().unwrap();
                if cancelled {cancel.cancel();}
                let result=tokio::time::timeout(Duration::from_secs(2),call).await.unwrap().unwrap();
                if cancelled {assert!(matches!(result,Err(openpaw_core::Error::Cancelled)));}
                else {let outcome=result.unwrap();assert!(outcome.is_error);assert_eq!(outcome.structured.unwrap()["kind"],"deadline");}
                tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
            }
        });
    }

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

    #[test]
    fn d188_cached_body_keeps_unicode_excerpt_boundary_and_expiry() {
        let key = "https://example.invalid/d188-unicode";
        let text = "正文😀尾部";
        put_cached(key, text);
        assert_eq!(
            get_cached(key, 2).unwrap(),
            "正文\n... (truncated at 2 chars)"
        );
        assert_eq!(get_cached(key, 5).unwrap(), text);
        assert_eq!(get_cached(key, 100).unwrap(), text);
        assert!(get_cached("https://example.invalid/d188-unicode?other=1", 5).is_none());
        {
            let mut guard = FETCH_CACHE.lock().unwrap();
            guard.as_mut().unwrap().get_mut(key).unwrap().fetched_at = Instant::now() - CACHE_TTL;
        }
        assert!(get_cached(key, 5).is_none());
    }

    #[test]
    fn d188_cached_body_is_bounded_without_losing_truncation_signal() {
        let key = "https://example.invalid/d188-bounded";
        put_cached(key, &"字".repeat(120_000));
        let guard = FETCH_CACHE.lock().unwrap();
        assert_eq!(
            guard
                .as_ref()
                .unwrap()
                .peek(key)
                .unwrap()
                .content
                .chars()
                .count(),
            MAX_CACHED_CHARS
        );
        drop(guard);
        let excerpt = get_cached(key, 100_000).unwrap();
        assert!(excerpt.ends_with("... (truncated at 100000 chars)"));
        assert_eq!(excerpt.lines().next().unwrap().chars().count(), 100_000);
    }

    /// 进程级复用 Client 应返回同一实例。
    #[test]
    fn http_client_is_shared() {
        let a = http_client();
        let b = http_client();
        assert!(std::ptr::eq(a, b), "应复用同一个 Client 实例");
    }
}
