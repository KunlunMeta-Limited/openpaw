//! 功能：将客户端输入拆为共享模型消息及中立展示注解。
//! 输入输出边界：只转换请求和已校验属性，OpenCode 字段不进入 Core。
//! 禁止承担：不写历史，不访问媒体 URL，不解析旧英文附件模板。
//! 关联：CHG-2026-0027。
use crate::protocol::MessageRequest;
use openpaw_core::{ContentBlock, InlineMedia, Message};
use openpaw_store::transcript::{MessagePresentationV1, PartPresentationV1};

/// 功能：保留有属性文本的独立边界，并构造对应展示注解。
/// 参数：body 为待接收请求，直接 text 保持优先。
/// 返回：共享消息及可选展示属性。
/// 错误：媒体格式或编码非法。
/// 状态：纯转换，不接收回合。
/// 并发与取消：同步无共享状态。
/// 权限：不下载远程 URL，不解释元数据。
/// 关联：CHG-2026-0027；MessageRequest 与 HTTP 共用。
pub(crate) fn prepare(
    body: &MessageRequest,
) -> openpaw_core::Result<(Message, Option<MessagePresentationV1>)> {
    let has_attributes = body.text.is_none()
        && body.parts.iter().any(|part| {
            part.kind.as_deref() == Some("text")
                && (part.synthetic.is_some() || part.metadata.is_some())
        });
    if !has_attributes
        && !body
            .parts
            .iter()
            .any(|part| part.kind.as_deref() == Some("file"))
    {
        return Ok((Message::user_text(body.text()), None));
    }
    let mut message = Message::default();
    let mut parts = Vec::new();
    if let Some(text) = &body.text {
        message
            .content
            .push(ContentBlock::Text { text: text.clone() });
    }
    for part in &body.parts {
        match part.kind.as_deref() {
            Some("file") => {
                let mime = part
                    .mime
                    .as_deref()
                    .ok_or_else(|| openpaw_core::Error::session("file part requires MIME"))?;
                let url = part.url.as_deref().ok_or_else(|| {
                    openpaw_core::Error::session("file part requires inline data URL")
                })?;
                message
                    .content
                    .push(InlineMedia::from_data_url(mime, url, part.filename.clone())?.block());
            }
            Some("text") if body.text.is_none() => {
                if let Some(text) = &part.text {
                    if part.synthetic.is_some() || part.metadata.is_some() {
                        parts.push(PartPresentationV1 {
                            block_index: message.content.len(),
                            auxiliary: part.synthetic.unwrap_or(false),
                            metadata: part.metadata.clone(),
                        });
                    }
                    message
                        .content
                        .push(ContentBlock::Text { text: text.clone() });
                }
            }
            _ => {}
        }
    }
    let presentation = (!parts.is_empty()).then_some(MessagePresentationV1 { version: 1, parts });
    Ok((message, presentation))
}

/// 功能：将中立文本属性还原到 OpenCode part。
/// 参数：part 为已投影内容，index 为原始块编号，presentation 为原消息注解。
/// 返回：无。
/// 错误：无；未匹配属性时保持原 part。
/// 状态：仅修改返回值，不修改模型内容。
/// 并发与取消：同步无共享状态。
/// 权限：元数据仅返回授权消息接口，不访问其中 URL。
/// 关联：CHG-2026-0027；REST 与 SSE 共用。
pub(crate) fn decorate(
    part: &mut serde_json::Value,
    index: usize,
    presentation: Option<&MessagePresentationV1>,
) {
    if let Some(annotation) =
        presentation.and_then(|value| value.parts.iter().find(|item| item.block_index == index))
    {
        part["synthetic"] = annotation.auxiliary.into();
        if let Some(metadata) = &annotation.metadata {
            part["metadata"] = metadata.clone().into();
        }
    }
}
