//! 功能：定义内联附件、格式检查和请求预算。
//! 输入输出边界：原件字节、Base64 与中立消息；不读取文件或网络。
//! 禁止承担：协议投影、持久化和模型能力推断。
//! 关联：CHG-2026-0026。
//! Stability: unstable.
use crate::{Capabilities, ContentBlock, Error, Message, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

/// 功能：单张图片的原件字节上限。Stability: unstable.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// 功能：音频和单回合原件总字节上限。Stability: unstable.
pub const MAX_MEDIA_BYTES: usize = 20 * 1024 * 1024;
/// 功能：单次输入的附件数量上限。Stability: unstable.
pub const MAX_ATTACHMENTS: usize = 4;
/// 功能：序列化模型请求的应用字节上限。Stability: unstable.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// 功能：保存附件编号、规范 MIME、原件 Base64 和可选名称。
/// 约束：必须校验后发送；不包含外部路径或 URL。Stability: unstable.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineMedia {
    /// 功能：会话内稳定编号。Stability: unstable.
    pub id: String,
    /// 功能：规范媒体类型。Stability: unstable.
    pub mime: String,
    /// 功能：原件的标准 Base64；禁止输出到诊断。Stability: unstable.
    pub data: String,
    /// 功能：用于展示的可选文件名。Stability: unstable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}
impl std::fmt::Debug for InlineMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineMedia")
            .field("id", &self.id)
            .field("mime", &self.mime)
            .field("encoded_bytes", &self.data.len())
            .finish()
    }
}
impl InlineMedia {
    /// 功能：解析内联上传，统一 MIME 别名并校验原件。
    /// 参数：mime 为客户端声明，url 为 data URL，filename 为可选名称。
    /// 返回：规范内联附件。
    /// 错误：远程 URL、类型不匹配、非法编码或超限。
    /// 状态：分配编号，不写历史。
    /// 并发与取消：同步有界。
    /// 权限：不下载远程资源。
    /// 关联：Host file part。Stability: unstable.
    pub fn from_data_url(mime: &str, url: &str, filename: Option<String>) -> Result<Self> {
        let canonical = |value: &str| match value {
            "audio/mp3" => "audio/mpeg".to_string(),
            "audio/x-wav" => "audio/wav".to_string(),
            _ => value.to_string(),
        };
        let value = url
            .strip_prefix("data:")
            .ok_or_else(|| Error::session("only inline media data URLs are supported"))?;
        let (declared, data) = value
            .split_once(";base64,")
            .ok_or_else(|| Error::session("invalid media data URL"))?;
        if canonical(declared) != canonical(mime) {
            return Err(Error::session("media MIME mismatch"));
        }
        let media = Self {
            id: format!("att_{}", uuid::Uuid::new_v4()),
            mime: canonical(mime),
            data: data.into(),
            filename,
        };
        let bytes = media.bytes()?;
        Ok(Self {
            data: STANDARD.encode(bytes),
            ..media
        })
    }
    /// 功能：由原件构建附件。
    /// 参数：bytes 为原件，mime 为声明类型，filename 为展示名。
    /// 返回：带稳定编号的内联附件。
    /// 错误：头部不匹配、未知类型或超限。
    /// 状态：生成 UUID，不写入历史。
    /// 并发与取消：同步有界；无共享状态。
    /// 权限：调用者负责字节来源权限。
    /// 关联：CLI、Host 和 read_file。Stability: unstable.
    pub fn from_bytes(bytes: &[u8], mime: &str, filename: Option<String>) -> Result<Self> {
        check_bytes(bytes, mime)?;
        Ok(Self {
            id: format!("att_{}", uuid::Uuid::new_v4()),
            mime: mime.into(),
            data: STANDARD.encode(bytes),
            filename,
        })
    }
    /// 功能：校验并解码附件原件。
    /// 参数：无。
    /// 返回：原件字节。
    /// 错误：非法编号、Base64、类型、内容头或大小。
    /// 状态：无修改。
    /// 并发与取消：同步有界。
    /// 权限：不解析文件路径。
    /// 关联：Provider 和历史回看。Stability: unstable.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        if self
            .filename
            .as_ref()
            .is_some_and(|name| name.len() > 255 || name.chars().any(char::is_control))
        {
            return Err(Error::session("invalid attachment filename"));
        }
        if self.id.is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control) {
            return Err(Error::session("invalid attachment ID"));
        }
        if self.data.len() > MAX_MEDIA_BYTES.div_ceil(3) * 4 {
            return Err(Error::session("media exceeds byte limit"));
        }
        let bytes = STANDARD
            .decode(&self.data)
            .map_err(|_| Error::session("invalid media Base64"))?;
        check_bytes(&bytes, &self.mime)?;
        Ok(bytes)
    }
    /// 功能：构建中立图片或音频内容块。
    /// 参数：已校验附件。
    /// 返回：包含相同编号与原件的内容块。
    /// 错误：无；调用者先校验。
    /// 状态：无修改。
    /// 并发与取消：无。
    /// 权限：无外部访问。
    /// 关联：Message。Stability: unstable.
    pub fn block(&self) -> ContentBlock {
        if self.mime.starts_with("image/") {
            ContentBlock::Image {
                url: String::new(),
                media_type: Some(self.mime.clone()),
                data: Some(self.data.clone()),
                id: Some(self.id.clone()),
                filename: self.filename.clone(),
            }
        } else {
            ContentBlock::Audio {
                media: self.clone(),
            }
        }
    }
    /// 功能：创建上游或客户端展示的 data URL。
    /// 参数：已校验附件。
    /// 返回：内联 URL。
    /// 错误：无。
    /// 状态：无修改。
    /// 并发与取消：无。
    /// 权限：禁止用于诊断。
    /// 关联：Provider/Host 投影。Stability: unstable.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.data)
    }

    /// 功能：生成模型输入原件；GIF 仅保留第一帧，历史原件不变。
    /// 参数：无。
    /// 返回：标准 Base64，其他格式保留原字节。
    /// 错误：非法原件或不完整 GIF 容器。
    /// 状态：不修改附件编号和历史。
    /// 并发与取消：同步、有界；不解码像素。
    /// 权限：不访问外部资源。
    /// 关联：两类图片 Provider。Stability: unstable.
    pub fn model_data(&self) -> Result<String> {
        let bytes = self.bytes()?;
        if self.mime != "image/gif" {
            return Ok(self.data.clone());
        }
        let end =
            gif_first_frame_end(&bytes).ok_or_else(|| Error::session("invalid GIF container"))?;
        let mut frame = bytes[..end].to_vec();
        frame.push(0x3b);
        Ok(STANDARD.encode(frame))
    }
}

// 仅遍历 GIF 块边界；首帧保留逻辑屏幕、调色板、控制扩展与 LZW 数据，后续帧不进入模型。
fn gif_first_frame_end(bytes: &[u8]) -> Option<usize> {
    let packed = *bytes.get(10)?;
    let mut index = 13
        + if packed & 0x80 != 0 {
            3 * (1usize << ((packed & 7) + 1))
        } else {
            0
        };
    fn subblocks(bytes: &[u8], mut index: usize) -> Option<usize> {
        loop {
            let length = usize::from(*bytes.get(index)?);
            index += 1;
            if length == 0 {
                return Some(index);
            }
            index = index.checked_add(length)?;
            if index > bytes.len() {
                return None;
            }
        }
    }
    loop {
        match *bytes.get(index)? {
            0x21 => {
                bytes.get(index + 1)?;
                index = subblocks(bytes, index + 2)?;
            }
            0x2c => {
                let packed = *bytes.get(index + 9)?;
                index += 10;
                if packed & 0x80 != 0 {
                    index += 3 * (1usize << ((packed & 7) + 1));
                }
                bytes.get(index)?; // LZW minimum code size；像素解码由上游处理。
                return subblocks(bytes, index + 1);
            }
            _ => return None,
        }
    }
}

/// 功能：识别支持的媒体头。
/// 参数：bytes 为原件。
/// 返回：规范 MIME，未知内容为 None。
/// 错误：无；只做容器签名检查，解码有效性由上游确认。
/// 状态：无。
/// 并发与取消：同步常数时间。
/// 权限：无。
/// 关联：check_bytes。Stability: unstable.
pub fn detect_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        Some("audio/wav")
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 255 && bytes[1] & 0xe0 == 0xe0)
    {
        Some("audio/mpeg")
    } else {
        None
    }
}
fn check_bytes(bytes: &[u8], mime: &str) -> Result<()> {
    let limit = if mime.starts_with("image/") {
        MAX_IMAGE_BYTES
    } else {
        MAX_MEDIA_BYTES
    };
    if bytes.len() > limit {
        return Err(Error::session("media exceeds byte limit"));
    }
    if detect_mime(bytes) != Some(mime) {
        return Err(Error::session("unsupported media or MIME/header mismatch"));
    }
    Ok(())
}

/// 功能：从中立内容取出附件。
/// 参数：block 为消息内容。
/// 返回：内联附件；非媒体返回 None。
/// 错误：旧图片来源无原件或不支持。
/// 状态：不修改旧消息。
/// 并发与取消：同步。
/// 权限：不下载旧 URL。
/// 关联：历史兼容与 Provider。Stability: unstable.
pub fn block_media(block: &ContentBlock) -> Result<Option<InlineMedia>> {
    match block {
        ContentBlock::Audio { media } => Ok(Some(media.clone())),
        ContentBlock::Image {
            url,
            media_type,
            data,
            id,
            filename,
        } => {
            let (mime, encoded) = if let (Some(mime), Some(data)) = (media_type, data) {
                (mime.as_str(), data.as_str())
            } else if let Some(value) = url.strip_prefix("data:") {
                value
                    .split_once(";base64,")
                    .ok_or_else(|| Error::session("invalid image data URL"))?
            } else {
                return Err(Error::session(
                    "image requires inline content; remote URLs are unsupported",
                ));
            };
            Ok(Some(InlineMedia {
                id: id.clone().unwrap_or_else(|| {
                    // 旧内联 Image 没有编号；使用确定性内容标签，不以常量混淆不同原件。
                    let hash = mime
                        .bytes()
                        .chain(encoded.bytes())
                        .fold(0xcbf29ce484222325u64, |hash, byte| {
                            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                        });
                    format!("legacy_{hash:016x}")
                }),
                mime: mime.into(),
                data: encoded.into(),
                filename: filename.clone(),
            }))
        }
        _ => Ok(None),
    }
}

/// 功能：校验输入附件的能力、数量和原件总量。
/// 参数：media 为附件列表，caps 为所选模型能力。
/// 返回：合法时无值。
/// 错误：任何附件不合法或不支持时明确失败。
/// 状态：无修改。
/// 并发与取消：同步有界。
/// 权限：不替用户切换模型。
/// 关联：入口接收和 Provider。Stability: unstable.
pub fn validate_media(media: &[InlineMedia], caps: Capabilities) -> Result<()> {
    if media.len() > MAX_ATTACHMENTS {
        return Err(Error::session("attachment count exceeds limit (4)"));
    }
    let mut total = 0usize;
    for item in media {
        if (item.mime.starts_with("image/") && !caps.image_input)
            || (item.mime.starts_with("audio/") && !caps.audio_input)
        {
            return Err(Error::session(
                "selected model does not support this media input",
            ));
        }
        total = total.saturating_add(item.bytes()?.len());
    }
    if total > MAX_MEDIA_BYTES {
        return Err(Error::session("media exceeds total byte limit"));
    }
    Ok(())
}

/// 功能：列出消息中的用户与工具附件。
/// 参数：messages 为当前有效历史。
/// 返回：包含编号的原件集合，旧 URL 图片不能用于回看。
/// 错误：无；非法历史在发送前另行拒绝。
/// 状态：无修改。
/// 并发与取消：只读。
/// 权限：调用者必须先应用回退/分支边界。
/// 关联：Store 与 read_attachment。Stability: unstable.
pub fn message_media(messages: &[Message]) -> Vec<InlineMedia> {
    let mut result = Vec::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolResult { media, .. } = block {
                result.extend(media.iter().cloned());
            } else if let Ok(Some(media)) = block_media(block) {
                result.push(media);
            }
        }
    }
    result
}

/// 功能：移除媒体请求错误中的上游回显。
/// 参数：error 为真实错误。
/// 返回：保留状态和重试语义的脱敏错误。
/// 错误：无。
/// 状态：无修改。
/// 并发与取消：同步。
/// 权限：禁止原件进入日志和错误投影。
/// 关联：媒体 Provider/Engine。Stability: unstable.
pub fn redact_error(error: Error) -> Error {
    let message = "media request failed; upstream details omitted".to_string();
    match error {
        Error::AuthFailed { status, .. } => Error::AuthFailed { status, message },
        Error::ServerError { status, .. } => Error::ServerError { status, message },
        Error::RateLimited {
            retry_after_secs, ..
        } => Error::RateLimited {
            message,
            retry_after_secs,
        },
        Error::ContextTooLong(_) => Error::ContextTooLong(message),
        Error::Timeout(_) => Error::Timeout(message),
        Error::Session(_) => error,
        _ => Error::Provider(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_models_receive_first_frame_while_original_is_preserved() {
        let first = STANDARD
            .decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==")
            .unwrap();
        let mut animated = first[..first.len() - 1].to_vec();
        animated.extend_from_slice(&first[19..]);
        let media = InlineMedia::from_bytes(&animated, "image/gif", None).unwrap();
        assert_eq!(STANDARD.decode(media.model_data().unwrap()).unwrap(), first);
        assert_eq!(media.bytes().unwrap(), animated);
        let truncated = InlineMedia::from_bytes(b"GIF89afake", "image/gif", None).unwrap();
        assert!(truncated.model_data().is_err());
    }

    #[test]
    fn boundaries_mime_aliases_legacy_ids_and_safe_diagnostics() {
        let caps = Capabilities {
            image_input: true,
            audio_input: true,
            ..Default::default()
        };
        let mut bytes = vec![0; MAX_IMAGE_BYTES];
        bytes[..6].copy_from_slice(b"GIF89a");
        let image = InlineMedia::from_bytes(&bytes, "image/gif", None).unwrap();
        assert!(validate_media(&vec![image.clone(); 4], caps).is_ok());
        assert!(validate_media(&vec![image.clone(); 5], caps).is_err());
        bytes.push(0);
        assert!(InlineMedia::from_bytes(&bytes, "image/gif", None).is_err());
        let mut wav = vec![0; MAX_MEDIA_BYTES];
        wav[..12].copy_from_slice(b"RIFF0000WAVE");
        let audio = InlineMedia::from_bytes(&wav, "audio/wav", None).unwrap();
        assert!(validate_media(std::slice::from_ref(&audio), caps).is_ok());
        assert!(validate_media(&[audio.clone(), image.clone()], caps).is_err());
        assert!(validate_media(std::slice::from_ref(&audio), Capabilities::default()).is_err());
        let alias = InlineMedia::from_data_url("audio/x-wav", &audio.data_url(), None).unwrap();
        assert_eq!(alias.mime, "audio/wav");
        assert!(InlineMedia::from_data_url("image/gif", &audio.data_url(), None).is_err());
        assert!(
            InlineMedia::from_data_url("image/gif", "data:image/gif;base64,???", None).is_err()
        );
        let mut invalid = image.clone();
        invalid.filename = Some("bad\nname".into());
        assert!(invalid.bytes().is_err());
        let mut first = image.block();
        if let ContentBlock::Image { id, .. } = &mut first {
            *id = None;
        }
        let mut second = first.clone();
        if let ContentBlock::Image { data, .. } = &mut second {
            *data = Some(STANDARD.encode(b"GIF89aother"));
        }
        assert_ne!(
            block_media(&first).unwrap().unwrap().id,
            block_media(&second).unwrap().unwrap().id
        );
        assert!(!format!("{audio:?}").contains(&audio.data[..50]));
        let error = redact_error(Error::ServerError {
            status: 503,
            message: audio.data.clone(),
        });
        assert!(matches!(error, Error::ServerError { status: 503, .. }));
        assert!(!error.to_string().contains(&audio.data[..50]));
    }

    #[test]
    fn old_inline_jsonl_keeps_empty_tool_media_compatible() {
        let old: ContentBlock =
            serde_json::from_str(r#"{"type":"tool_result","tool_use_id":"old","content":"plain"}"#)
                .unwrap();
        assert!(matches!(&old,ContentBlock::ToolResult { media,.. } if media.is_empty()));
        assert!(serde_json::to_value(old).unwrap().get("media").is_none());
        let old: ContentBlock = serde_json::from_str(
            r#"{"type":"image","url":"data:image/gif;base64,R0lGODlhZml4dHVyZQ=="}"#,
        )
        .unwrap();
        assert_eq!(
            block_media(&old).unwrap().unwrap().bytes().unwrap(),
            b"GIF89afixture"
        );
    }
}
