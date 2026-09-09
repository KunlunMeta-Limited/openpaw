//! 共享辅助函数 — HTML 解析、反爬检测。

/// 检测百度反爬挑战页面
pub fn is_anti_bot_page(html: &str) -> bool {
    html.contains("请输入验证码")
        || html.contains("百度安全验证")
        || html.contains("wappass.baidu.com")
        || html.contains("captcha")
        || html.contains("antirobot")
        // 页面过短且无搜索结果容器
        || (html.len() < 2000 && !html.contains("c-container"))
}

/// 检测搜狗反爬挑战页面
pub fn is_sogou_anti_bot_page(html: &str) -> bool {
    html.contains("请输入验证码")
        || html.contains("搜狗验证码")
        || html.contains("antispider")
        || html.contains("captcha")
        || (html.len() < 2000 && !html.contains("vrwrap"))
}

/// 去除 HTML 标签，保留文本内容
pub fn strip_html_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    decode_entities(&out)
}

/// 解码常见 HTML 实体
pub fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// 提取两个标记之间的文本
pub fn extract_text_between(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    let raw = &rest[..j];
    Some(strip_html_tags(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_baidu_captcha() {
        assert!(is_anti_bot_page("<html>请输入验证码</html>"));
    }

    #[test]
    fn detects_baidu_short_page() {
        assert!(is_anti_bot_page("<html>short</html>"));
    }

    #[test]
    fn normal_page_not_anti_bot() {
        assert!(!is_anti_bot_page(
            "<html><div class=\"c-container\">results</div></html>"
        ));
    }

    #[test]
    fn strip_tags_works() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
    }

    #[test]
    fn decode_entities_works() {
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn extract_text_between_works() {
        assert_eq!(
            extract_text_between("<a>hello</a>", "<a>", "</a>"),
            Some("hello".into())
        );
    }
}
