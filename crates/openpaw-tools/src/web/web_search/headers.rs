//! 浏览器标头伪装 — 模拟真实浏览器请求以绕过搜索引擎反爬。
//!
//! 参考 Claude Code 的 BingSearchAdapter BROWSER_HEADERS 实现。
//! 关键：Sec-Ch-Ua 版本号必须与 User-Agent 中的版本一致。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// 构建完整的浏览器标头组合（模拟 Edge 131 on Windows）
pub fn browser_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();

    headers.insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
        ),
    );

    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,\
             image/avif,image/webp,image/apng,*/*;q=0.8",
        ),
    );

    headers.insert(
        HeaderName::from_static("accept-language"),
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );

    headers.insert(
        HeaderName::from_static("accept-encoding"),
        HeaderValue::from_static("gzip, deflate, br"),
    );

    // Sec-Ch-Ua: version must match UA (131)
    headers.insert(
        HeaderName::from_static("sec-ch-ua"),
        HeaderValue::from_static(
            "\"Microsoft Edge\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\"",
        ),
    );

    headers.insert(
        HeaderName::from_static("sec-ch-ua-mobile"),
        HeaderValue::from_static("?0"),
    );

    headers.insert(
        HeaderName::from_static("sec-ch-ua-platform"),
        HeaderValue::from_static("\"Windows\""),
    );

    // Sec-Fetch-*: simulate user typing URL in address bar
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("navigate"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("none"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-user"),
        HeaderValue::from_static("?1"),
    );

    headers.insert(
        HeaderName::from_static("upgrade-insecure-requests"),
        HeaderValue::from_static("1"),
    );

    headers.insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        HeaderName::from_static("pragma"),
        HeaderValue::from_static("no-cache"),
    );

    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_headers_has_user_agent() {
        let h = browser_headers();
        let ua = h.get("user-agent").unwrap().to_str().unwrap();
        assert!(ua.contains("Edg/131"));
    }

    #[test]
    fn browser_headers_has_sec_ch_ua() {
        let h = browser_headers();
        let s = h.get("sec-ch-ua").unwrap().to_str().unwrap();
        assert!(s.contains("131"));
    }

    #[test]
    fn browser_headers_has_sec_fetch() {
        let h = browser_headers();
        assert_eq!(
            h.get("sec-fetch-dest").unwrap().to_str().unwrap(),
            "document"
        );
        assert_eq!(
            h.get("sec-fetch-mode").unwrap().to_str().unwrap(),
            "navigate"
        );
        assert_eq!(h.get("sec-fetch-site").unwrap().to_str().unwrap(), "none");
    }
}
