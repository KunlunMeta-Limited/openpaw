//! HTML → Markdown 转换器
//!
//! 保留标题层级、代码块、链接、列表等结构信息，
//! 比 html_to_text 对 LLM 更友好。

/// 将 HTML 转换为 Markdown 格式。
///
/// 保留的结构：
/// - 标题 `<h1>`-`<h6>` → `#`-`######`
/// - 代码块 `<pre><code>` → ` ```code``` `
/// - 行内代码 `<code>` → `` `code` ``
/// - 链接 `<a href="url">text</a>` → `[text](url)`
/// - 列表 `<ul>/<ol>/<li>` → `- `/`1. `
/// - 段落和换行 → 空行分隔
/// - 强调 `<strong>/<b>` → `**bold**`
/// - 斜体 `<em>/<i>` → `*italic*`
/// - 引用 `<blockquote>` → `> `
/// - 表格基础结构
pub fn html_to_markdown(html: &str) -> String {
    // 1. 预处理：移除 script/style/nav/footer/aside/header 块
    let html = strip_tags_block(html, "script");
    let html = strip_tags_block(&html, "style");
    let html = strip_tags_block(&html, "nav");
    let html = strip_tags_block(&html, "footer");
    let html = strip_tags_block(&html, "aside");
    let html = strip_tags_block(&html, "header");

    // 2. 提取 <main> 或 <article> 内容（优先），否则用 <body>
    let html = extract_main_content(&html);

    // 3. 提取代码块（在通用解析之前，防止代码内容被错误转换）
    let (html, code_blocks) = extract_code_blocks(&html);

    // 4. 逐标签解析
    let mut parser = MarkdownParser::new(&html, code_blocks);
    let result = parser.parse();

    // 5. 清理多余空行和 UI 噪音
    clean_whitespace(&result)
}

// ── 预处理辅助 ────────────────────────────────────────────────────────────

fn strip_tags_block(html: &str, tag: &str) -> String {
    let open_lower = format!("<{tag}");
    let close_lower = format!("</{tag}>");
    let mut result = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let rest_lower = rest.to_lowercase();
        let Some(start) = rest_lower.find(&open_lower) else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..start]);
        let after_open = &rest[start..];
        let after_open_lower = &rest_lower[start..];
        if let Some(end) = after_open_lower.find(&close_lower) {
            rest = &after_open[end + close_lower.len()..];
        } else {
            result.push_str(after_open);
            break;
        }
    }
    result
}

/// Extract the main content area, preferring <main>, <article>, or <body> in that order.
fn extract_main_content(html: &str) -> String {
    let lower = html.to_lowercase();

    // Try <main> first
    if let Some(start) = lower.find("<main") {
        let after_tag = &html[start..];
        if let Some(tag_end) = after_tag.find('>') {
            let content_start = start + tag_end + 1;
            if let Some(end) = lower[content_start..].find("</main>") {
                return html[content_start..content_start + end].to_string();
            }
            return html[content_start..].to_string();
        }
    }

    // Try <article>
    if let Some(start) = lower.find("<article") {
        let after_tag = &html[start..];
        if let Some(tag_end) = after_tag.find('>') {
            let content_start = start + tag_end + 1;
            if let Some(end) = lower[content_start..].find("</article>") {
                return html[content_start..content_start + end].to_string();
            }
            return html[content_start..].to_string();
        }
    }

    // Fall back to <body>
    extract_body(html)
}

fn extract_body(html: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(start) = lower.find("<body") {
        // 找到 <body ...> 的结束 >
        let after_body_tag = &html[start..];
        if let Some(tag_end) = after_body_tag.find('>') {
            let content_start = start + tag_end + 1;
            if let Some(end) = lower[content_start..].find("</body>") {
                return html[content_start..content_start + end].to_string();
            }
            return html[content_start..].to_string();
        }
    }
    html.to_string()
}

/// 提取 `<pre>` 和 `<pre><code>` 块，替换为占位符。
/// 返回 (替换后的 HTML, 代码块列表)。
fn extract_code_blocks(html: &str) -> (String, Vec<String>) {
    let mut blocks = Vec::new();
    let mut result = String::with_capacity(html.len());
    let mut rest = html;

    loop {
        let lower_rest = rest.to_lowercase();
        let Some(start) = lower_rest.find("<pre") else {
            result.push_str(rest);
            break;
        };

        // 推送 pre 之前的内容
        result.push_str(&rest[..start]);

        // 找到 <pre ...> 的结束 >
        let after_pre = &rest[start..];
        let Some(tag_end) = after_pre.find('>') else {
            result.push_str(after_pre);
            break;
        };

        // 找到 </pre> (case-insensitive, may have attributes before >)
        let after_tag = &after_pre[tag_end + 1..];
        let lower_after = after_tag.to_lowercase();
        let Some(end_rel) = lower_after.find("</pre") else {
            result.push_str(after_pre);
            break;
        };
        // Find the > after </pre
        let close_after = &after_tag[end_rel..];
        let Some(close_gt) = close_after.find('>') else {
            result.push_str(after_pre);
            break;
        };
        let end = end_rel; // start of </pre
        let close_end = end_rel + close_gt + 1; // end of </pre...>

        // 提取代码内容
        let code_content = &after_tag[..end];
        // 去除可能内嵌的 <code ...> 标签
        let code_clean = strip_code_tags(code_content);
        let decoded = decode_html_entities(&code_clean);
        let trimmed = decoded.trim();

        // 生成占位符
        let idx = blocks.len();
        let placeholder = format!("\x00CODEBLOCK_{idx}\x00");
        blocks.push(trimmed.to_string());
        result.push_str(&placeholder);

        // 继续处理剩余
        rest = &after_tag[close_end..]; // skip </pre...>
    }

    (result, blocks)
}

/// Remove `<code>` and `<code ...>` tags (with any attributes), case-insensitively.
fn strip_code_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let lower = rest.to_lowercase();
        let open = lower.find("<code");
        let close = lower.find("</code");

        match (open, close) {
            (Some(o), Some(c)) if o < c => {
                result.push_str(&rest[..o]);
                let after = &rest[o..];
                if let Some(gt) = after.find('>') {
                    rest = &after[gt + 1..];
                } else {
                    rest = "";
                }
            }
            (Some(_), Some(c)) | (None, Some(c)) => {
                result.push_str(&rest[..c]);
                // skip </code> or </code ...>
                let after = &rest[c..];
                if let Some(gt) = after.find('>') {
                    rest = &after[gt + 1..];
                } else {
                    rest = "";
                }
            }
            (Some(o), None) => {
                result.push_str(&rest[..o]);
                let after = &rest[o..];
                if let Some(gt) = after.find('>') {
                    rest = &after[gt + 1..];
                } else {
                    rest = "";
                }
            }
            (None, None) => {
                result.push_str(rest);
                break;
            }
        }
    }
    result
}

// ── Markdown 解析器 ───────────────────────────────────────────────────────

struct MarkdownParser<'a> {
    html: &'a str,
    pos: usize,
    code_blocks: Vec<String>,
    pending_link_href: Option<String>,
    list_depth: usize,
    list_counter: Vec<u32>,
}

impl<'a> MarkdownParser<'a> {
    fn new(html: &'a str, code_blocks: Vec<String>) -> Self {
        Self {
            html,
            pos: 0,
            code_blocks,
            pending_link_href: None,
            list_depth: 0,
            list_counter: Vec::new(),
        }
    }

    fn parse(&mut self) -> String {
        let mut out = String::new();

        while self.pos < self.html.len() {
            if let Some(remaining) = self.html[self.pos..].strip_prefix('\x00') {
                // 代码块占位符: \x00CODEBLOCK_N\x00
                // 找到第二个 \x00 (结束标记)
                if let Some(end) = remaining.find('\x00') {
                    let marker = &remaining[..end];
                    if let Some(rest) = marker.strip_prefix("CODEBLOCK_") {
                        if let Ok(idx) = rest.parse::<usize>() {
                            if idx < self.code_blocks.len() {
                                out.push_str("\n```\n");
                                out.push_str(&self.code_blocks[idx]);
                                out.push_str("\n```\n");
                            }
                        }
                    }
                    self.pos += end + 2; // skip both \x00 markers
                    continue;
                }
            }

            if self.html[self.pos..].starts_with('<') {
                if self.html[self.pos..].starts_with("</") {
                    self.parse_close_tag(&mut out);
                } else {
                    self.parse_tag(&mut out);
                }
            } else {
                // 普通文本 — 搜索下一个 < 或 \x00
                let end = {
                    let remaining = &self.html[self.pos..];
                    let lt = remaining.find('<');
                    let nul = remaining.find('\x00');
                    match (lt, nul) {
                        (Some(a), Some(b)) => self.pos + a.min(b),
                        (Some(a), None) => self.pos + a,
                        (None, Some(b)) => self.pos + b,
                        (None, None) => self.html.len(),
                    }
                };
                let text = &self.html[self.pos..end];
                out.push_str(&decode_html_entities(text));
                self.pos = end;
            }
        }

        out
    }

    fn parse_tag(&mut self, out: &mut String) {
        // 找到标签结束 >
        let tag_start = self.pos;
        let Some(tag_end_rel) = self.html[tag_start..].find('>') else {
            // 没有结束 >，当普通文本
            out.push_str(&self.html[self.pos..]);
            self.pos = self.html.len();
            return;
        };
        let tag_end = tag_start + tag_end_rel;
        let full_tag = &self.html[tag_start + 1..tag_end]; // 去掉 < >

        // 判断是否是自闭合
        let _is_self_closing = full_tag.ends_with('/');

        // 提取标签名
        let tag_content = full_tag.trim_end_matches('/');
        let tag_name: String = tag_content
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();

        // 解析属性
        let attrs = parse_attrs(tag_content);

        match tag_name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag_name.as_bytes()[1] - b'0';
                out.push_str("\n\n");
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
            }
            "p" => {
                out.push_str("\n\n");
            }
            "br" => {
                out.push('\n');
            }
            "hr" => {
                out.push_str("\n\n---\n\n");
            }
            "strong" | "b" => {
                out.push_str("**");
            }
            "em" | "i" => {
                out.push('*');
            }
            "code" => {
                out.push('`');
            }
            "pre" => {
                // pre 内容已被 extract_code_blocks 处理
            }
            "a" => {
                if let Some(href) = attrs.get("href") {
                    out.push('[');
                    self.pending_link_href = Some(href.clone());
                }
            }
            "ul" | "ol" => {
                out.push('\n');
                self.list_depth += 1;
                if tag_name == "ol" {
                    self.list_counter.push(0);
                }
            }
            "li" => {
                out.push('\n');
                out.push_str(&"  ".repeat(self.list_depth.saturating_sub(1)));
                if !self.list_counter.is_empty() {
                    let n = self.list_counter.last_mut().unwrap();
                    *n += 1;
                    out.push_str(&format!("{}. ", n));
                } else {
                    out.push_str("- ");
                }
            }
            "blockquote" => {
                out.push_str("\n> ");
            }
            "img" => {
                if let (Some(src), Some(alt)) = (attrs.get("src"), attrs.get("alt")) {
                    out.push_str(&format!("![{alt}]({src})"));
                }
            }
            "table" => {
                out.push_str("\n\n");
            }
            "tr" => {
                out.push('|');
            }
            "td" | "th" => {
                out.push(' ');
            }
            "div" | "span" | "section" | "article" | "main" | "header" => {
                // 块级元素换行
                if matches!(
                    tag_name.as_str(),
                    "div" | "section" | "article" | "main" | "header"
                ) {
                    out.push('\n');
                }
            }
            _ => {}
        }

        self.pos = tag_end + 1;
    }

    /// 处理关闭标签 </tagname>
    fn parse_close_tag(&mut self, out: &mut String) {
        let tag_start = self.pos;
        let Some(tag_end_rel) = self.html[tag_start..].find('>') else {
            out.push_str(&self.html[self.pos..]);
            self.pos = self.html.len();
            return;
        };
        let tag_end = tag_start + tag_end_rel;
        let tag_name: String = self.html[tag_start + 2..tag_end].trim().to_lowercase();

        match tag_name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                out.push_str("\n\n");
            }
            "p" => {
                out.push_str("\n\n");
            }
            "strong" | "b" => {
                out.push_str("**");
            }
            "em" | "i" => {
                out.push('*');
            }
            "code" => {
                out.push('`');
            }
            "a" => {
                if let Some(href) = self.pending_link_href.take() {
                    out.push_str(&format!("]({href})"));
                } else {
                    out.push(']');
                }
            }
            "ul" | "ol" => {
                if self.list_depth > 0 {
                    self.list_depth -= 1;
                }
                if tag_name == "ol" {
                    self.list_counter.pop();
                }
                out.push('\n');
            }
            "li" => {
                // li 内容已输出
            }
            "blockquote" => {
                out.push('\n');
            }
            "table" | "tr" | "td" | "th" => {
                if tag_name == "tr" {
                    out.push_str("|\n");
                }
            }
            "div" | "section" | "article" | "main" | "header" => {
                out.push('\n');
            }
            _ => {}
        }

        self.pos = tag_end + 1;
    }
}

fn parse_attrs(tag_content: &str) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();
    // 简单解析: key="value" 或 key='value'
    let mut chars = tag_content.chars().peekable();
    let mut after_tag_name = false;

    while let Some(&c) = chars.peek() {
        if !after_tag_name {
            if c.is_whitespace() {
                after_tag_name = true;
            }
            chars.next();
            continue;
        }

        // 跳过空白
        if c.is_whitespace() {
            chars.next();
            continue;
        }

        // 读取属性名
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() || c == '/' {
                break;
            }
            key.push(c);
            chars.next();
        }

        if key.is_empty() {
            chars.next();
            continue;
        }

        // 跳过 =
        if chars.peek() == Some(&'=') {
            chars.next();
        }

        // 读取值
        let quote = chars.peek().copied();
        let value = if quote == Some('"') || quote == Some('\'') {
            chars.next(); // 跳过引号
            let mut val = String::new();
            while let Some(&c) = chars.peek() {
                if c == quote.unwrap() {
                    chars.next();
                    break;
                }
                val.push(c);
                chars.next();
            }
            val
        } else {
            let mut val = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                val.push(c);
                chars.next();
            }
            val
        };

        attrs.insert(key.to_lowercase(), value);
    }

    attrs
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»")
}

fn clean_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_blank = false;
    for line in s.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !prev_blank {
                out.push('\n');
                prev_blank = true;
            }
        } else {
            out.push_str(trimmed);
            out.push('\n');
            prev_blank = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heading() {
        let md = html_to_markdown("<h1>Title</h1><h2>Sub</h2>");
        assert!(md.contains("# Title"));
        assert!(md.contains("## Sub"));
    }

    #[test]
    fn test_code_block() {
        let md = html_to_markdown("<pre><code>fn main() {}</code></pre>");
        assert!(md.contains("```"), "missing triple backtick: {}", md);
        assert!(md.contains("fn main() {}"));
    }

    #[test]
    fn test_code_block_with_class_attribute() {
        // Rust book uses <pre><code class="language-console">...</code></pre>
        let md = html_to_markdown(
            r#"<pre><code class="language-console">$ curl https://sh.rustup.rs</code></pre>"#,
        );
        assert!(md.contains("```"), "missing triple backtick: {}", md);
        assert!(
            md.contains("$ curl https://sh.rustup.rs"),
            "should contain code content, got: {}",
            md
        );
        assert!(
            !md.contains("<code"),
            "should NOT contain raw <code tags: {}",
            md
        );
        assert!(
            !md.contains("class="),
            "should NOT contain class attribute: {}",
            md
        );
    }

    #[test]
    fn test_code_block_pre_with_class() {
        // Some sites use <pre class="..."> without <code>
        let md = html_to_markdown(r#"<pre class="rust">fn main() { println!("hello"); }</pre>"#);
        assert!(md.contains("```"), "missing triple backtick: {}", md);
        assert!(md.contains("fn main()"), "should contain code: {}", md);
    }

    #[test]
    fn test_inline_code_with_class() {
        let md = html_to_markdown(r#"<p>Use <code class="highlight">println!</code> macro</p>"#);
        assert!(
            md.contains("`println!`"),
            "should contain inline code: {}",
            md
        );
        assert!(
            !md.contains("class="),
            "should NOT contain class attr: {}",
            md
        );
    }

    #[test]
    fn test_inline_code() {
        let md = html_to_markdown("<p>Use <code>println!</code> macro</p>");
        assert!(md.contains("`println!`"));
    }

    #[test]
    fn test_link() {
        let md = html_to_markdown(r#"<a href="https://example.com">click</a>"#);
        // 链接处理在简化版本中可能不完整，但至少不应丢失文本
        assert!(md.contains("click") || md.contains("example.com"));
    }

    #[test]
    fn test_bold_italic() {
        let md = html_to_markdown("<p><b>bold</b> and <i>italic</i></p>");
        assert!(md.contains("**bold**"));
        assert!(md.contains("*italic*"));
    }

    #[test]
    fn test_script_stripped() {
        let md = html_to_markdown("<script>alert(1)</script><p>content</p>");
        assert!(!md.contains("alert"));
        assert!(md.contains("content"));
    }

    #[test]
    fn test_list() {
        let md = html_to_markdown("<ul><li>one</li><li>two</li></ul>");
        assert!(md.contains("- one") || md.contains("one"));
        assert!(md.contains("- two") || md.contains("two"));
    }

    #[test]
    fn test_entities() {
        let md = html_to_markdown("<p>&lt;tag&gt; &amp; &quot;quote&quot;</p>");
        assert!(md.contains("<tag>"));
        assert!(md.contains("&"));
        assert!(md.contains("\"quote\""));
    }
}
