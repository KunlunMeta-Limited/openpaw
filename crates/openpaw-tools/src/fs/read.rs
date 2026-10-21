//! `read_file` — read a file with line numbers (same-source semantics:
//! 10 MB guard, offset/limit window, `(empty file)` marker).

use std::path::Path;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read a file and return its contents with line numbers. Use offset and limit to read specific sections of large files. Always read a file before editing it.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to workspace"},
                "offset": {"type": "integer", "description": "Start line (0-based, default 0)"},
                "limit": {"type": "integer", "description": "Max lines to return (default 2000)"}
            },
            "required": ["path"]
        }),
        category: Some(ToolCategory::ReadOnly),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
}

/// 功能：复用有界文本读取并返回原带行号工具结果。
/// 参数：workspace为授权工作根，args为原path及offset/limit输入。
/// 返回：带行号文本、空文件或窗口越界提示。
/// 错误：路径、大小或读取失败进入原工具错误结果。
/// 状态：只读，不创建文件。
/// 并发与取消：异步读取，宿主负责共享资源锁。
/// 权限：复用原路径验证，不读取授权根外文件。
/// 关联：需求/设计REQ-OP-014；DES-OP-006。
pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("read: bad args: {e}"))),
    };
    let content = match read_text_bounded(workspace, &a.path, 10 * 1024 * 1024).await {
        Ok(content) => content,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    let offset = a.offset.unwrap_or(0);
    let limit = a.limit.unwrap_or(2000);

    let all_lines: Vec<&str> = content.lines().collect();
    let total = all_lines.len();

    if total == 0 {
        return Ok(ToolCallOutcome::ok("(empty file)\n"));
    }
    let start = offset.min(total);
    if start >= total {
        return Ok(ToolCallOutcome::ok(format!(
            "offset {offset} is beyond end of file ({total} lines)\n"
        )));
    }
    let end = start.saturating_add(limit).min(total);

    let mut out = String::new();
    for (i, line) in all_lines[start..end].iter().enumerate() {
        out.push_str(&format!("{:>5}\t{}\n", start + i + 1, line));
    }
    if end < total {
        out.push_str(&format!("... ({} more lines)\n", total - end));
    }
    Ok(ToolCallOutcome::ok(out))
}

/// 功能：在授权根内读取有字节上限的普通UTF8文本。
/// 参数：workspace为授权根，path为相对或根内绝对路径，max_bytes为硬上限。
/// 返回：无行号的原始文本，供原read_file和Host预览共用。
/// 错误：越界、非普通文件、过大、无效UTF8和IO错误。
/// 状态：只读，不创建文件。
/// 并发与取消：异步读取max_bytes加一字节；调用方负责协作资源锁。
/// 权限：复用原safety路径检查，不能读取根外链接。
/// 关联：需求/设计REQ-OP-014；DES-OP-006。
pub async fn read_text_bounded(workspace: &Path, path: &str, max_bytes: usize) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let path = crate::safety::resolve(workspace, path)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| openpaw_core::Error::ToolHost(format!("read: {e}")))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|e| openpaw_core::Error::ToolHost(format!("read metadata: {e}")))?;
    if !metadata.is_file() {
        return Err(openpaw_core::Error::ToolHost(
            "not a regular text file".into(),
        ));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(openpaw_core::Error::ToolHost(format!(
            "file too large (max {max_bytes} bytes)"
        )));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| openpaw_core::Error::ToolHost(format!("read: {e}")))?;
    if bytes.len() > max_bytes {
        return Err(openpaw_core::Error::ToolHost(
            "file too large during read".into(),
        ));
    }
    if bytes.contains(&0) {
        return Err(openpaw_core::Error::ToolHost(
            "not a plain text file".into(),
        ));
    }
    String::from_utf8(bytes).map_err(|_| openpaw_core::Error::ToolHost("file is not UTF8".into()))
}
