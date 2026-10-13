//! 功能：有界读取用户明确选择或已获准的本地媒体原件。
//! 边界：不解析工作区权限；Agent 调用必须先经过 safety::resolve。
//! 关联：CHG-2026-0026。
use openpaw_core::{Error, InlineMedia, Result};
use std::path::Path;
use tokio::io::AsyncReadExt;

/// 功能：读取一个本地图片或录音。
/// 参数：path 为用户明确选择或规范化后的路径。
/// 返回：校验后的内联附件。
/// 错误：文件缺失、类型未知或超过原件大小上限。
/// 状态：只读文件。
/// 并发与取消：有界异步读取，可丢弃 future。
/// 权限：不绕过 Agent 工作区检查。
/// 关联：CLI /attach 和 read_file。
pub async fn read_media(path: &Path) -> Result<InlineMedia> {
    if !tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::ToolHost("media file is unavailable".into()))?
        .is_file()
    {
        return Err(Error::ToolHost(
            "media source must be a regular file".into(),
        ));
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::ToolHost("media file is unavailable".into()))?;
    if !file
        .metadata()
        .await
        .map_err(|_| Error::ToolHost("media metadata unavailable".into()))?
        .is_file()
    {
        return Err(Error::ToolHost(
            "media source must be a regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take((openpaw_core::media::MAX_MEDIA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Error::ToolHost("media read failed".into()))?;
    if bytes.len() > openpaw_core::media::MAX_MEDIA_BYTES {
        return Err(Error::ToolHost("media exceeds byte limit".into()));
    }
    let mime = openpaw_core::media::detect_mime(&bytes)
        .ok_or_else(|| Error::ToolHost("unsupported media header".into()))?;
    InlineMedia::from_bytes(
        &bytes,
        mime,
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    )
}
