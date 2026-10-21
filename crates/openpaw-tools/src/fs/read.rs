//! 功能：读取文本或图片/音频；文本保留行号、10 MiB、offset/limit 和空文件语义。
//! 输入输出边界：工作区相对路径进入，文本或内联原件结果返回。
//! 禁止承担：不调用模型、不下载 URL、不绕过 safety 或审批。
//! 关联：CHG-2026-0026。

use std::path::Path;
use tokio::io::AsyncReadExt;

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

/// 功能：声明文本与媒体文件读取工具。
/// 参数：无。
/// 返回：只读、可并行的规格。
/// 错误：无。
/// 状态：无。
/// 并发与取消：同步构建。
/// 权限：执行仍经过工作区和审批检查。
/// 关联：ToolRegistry。
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read text with line numbers, or return an original PNG/JPEG/WebP/GIF image or WAV/MP3 audio for model understanding. Media ignores offset/limit. Use read_attachment with its ID to revisit it after compaction. Always read a file before editing it.".into(),
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

/// 功能：读取已获准的工作区文本或媒体。
/// 参数：workspace 为根路径，args 含 path 及可选文本行范围。
/// 返回：文本行或内联媒体，失败为错误工具结果。
/// 错误：路径越界、文件缺失、编码或大小非法。
/// 状态：只读，不安装媒体缓存。
/// 并发与取消：异步读取，调用方可丢弃 future。
/// 权限：沿用 safety::resolve，审批由 AgentLoop 执行。
/// 关联：read_media、read_attachment。
pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("read: bad args: {e}"))),
    };
    let path = match crate::safety::resolve(workspace, &a.path) {
        Ok(p) => p,
        Err(e) => return Ok(ToolCallOutcome::error(e.to_string())),
    };

    const MAX_READ_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            return Ok(ToolCallOutcome::error(
                "read requires an available regular file",
            ))
        }
    };
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut header = Vec::new();
    if let Ok(file) = tokio::fs::File::open(&path).await {
        let _ = file.take(12).read_to_end(&mut header).await;
    }
    if openpaw_core::media::detect_mime(&header).is_some()
        || matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "wav" | "mp3"
        )
    {
        return match super::media::read_media(&path).await {
            Ok(media) => {
                let mut outcome =
                    ToolCallOutcome::ok(format!("Attachment {} ({})", media.id, media.mime));
                outcome.media.push(media);
                Ok(outcome)
            }
            Err(error) => Ok(ToolCallOutcome::error(error.to_string())),
        };
    }
    if metadata.len() > MAX_READ_SIZE {
        return Ok(ToolCallOutcome::error(format!(
            "file too large: {} bytes (max {MAX_READ_SIZE} bytes). Use offset/limit to read sections.",
            metadata.len()
        )));
    }

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(ToolCallOutcome::error(format!(
                "read {}: {e}",
                path.display()
            )))
        }
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
