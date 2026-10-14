//! 功能：共享 REST 消息、回退删除身份与安全错误的纯投影。
//! 输入输出边界：读取模型消息和已提交的回合快照，返回 OpenCode 兼容展示值。
//! 禁止承担：不写 session、不执行模型或网络调用、不将错误展示加入模型上下文。
//! 关联：`CHG-2026-0022`。
use crate::protocol::{
    OpenCodeErrorDataV1, OpenCodeErrorV1, OpenCodeTurnErrorV1, TurnError, TurnOutcome, TurnState,
};
use std::collections::HashMap;

/// 功能：把原有安全回合分类转换为 OpenCode 错误。
/// 参数：error 为已过滤的稳定 TurnError。
/// 返回：不包含猜测 HTTP 状态和 provider 的错误。
/// 错误：无。
/// 状态：纯转换，不修改回合。
/// 并发与取消：同步无锁，不可取消。
/// 权限：只使用既有安全分类说明。
/// 关联：`CHG-2026-0022`。
pub(crate) fn classified_error(error: &TurnError) -> OpenCodeErrorV1 {
    OpenCodeErrorV1 {
        name: if error.code.starts_with("provider_") || error.code == "context_too_long" {
            "APIError"
        } else {
            "UnknownError"
        }
        .into(),
        data: OpenCodeErrorDataV1 {
            message: error.message.clone(),
            code: error.code.clone(),
            is_retryable: error.retryable,
            status_code: None,
            provider_id: None,
        },
    }
}

/// 功能：从 HTTP 失败提取可靠元数据及有界安全摘要。
/// 参数：source 为内部错误，classification 为安全分类，provider 为实际 provider 名称，input 为当前输入。
/// 返回：可持久化和公开的 OpenCode 错误；不确定的摘要被舍弃。
/// 错误：无；无法解析摘要时保留稳定分类说明。
/// 状态：不写文件，不改变重试或回合状态。
/// 并发与取消：同步纯投影，不可取消。
/// 权限：只读取已知 API key 来过滤回显，禁止输出凭据、请求正文、URL 或路径。
/// 关联：`CHG-2026-0022`。
pub(crate) fn provider_error(
    source: &openpaw_core::Error,
    classification: &TurnError,
    provider: &str,
    input: &str,
) -> OpenCodeErrorV1 {
    let mut result = classified_error(classification);
    if provider.len() <= 64
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        result.data.provider_id = Some(provider.into());
    }
    let (status, summary) = match source {
        openpaw_core::Error::AuthFailed { status, message }
        | openpaw_core::Error::ServerError { status, message } => {
            (Some(*status), Some(message.as_str()))
        }
        openpaw_core::Error::RateLimited { message, .. } => (
            Some(429),
            message.split_once(" — ").map(|(_, summary)| summary),
        ),
        openpaw_core::Error::Provider(message) => {
            let prefix = format!("{provider} http ");
            message
                .strip_prefix(&prefix)
                .and_then(|tail| tail.split_once(": "))
                .and_then(|(code, summary)| {
                    code.parse::<u16>()
                        .ok()
                        .filter(|status| (400..=599).contains(status))
                        .map(|status| (Some(status), Some(summary)))
                })
                .unwrap_or((None, None))
        }
        _ => (None, None),
    };
    result.data.status_code = status;
    if let Some(status) = status {
        result.data.message.push_str(&format!(" HTTP {status}"));
    }
    if let Some(summary) = summary.and_then(|raw| safe_upstream_message(raw, input)) {
        result.data.message.push_str(&format!("：{summary}"));
    }
    result
}

fn safe_upstream_message(raw: &str, input: &str) -> Option<String> {
    let trimmed = raw.trim();
    let parsed: serde_json::Value;
    let summary = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        parsed = serde_json::from_str(trimmed).ok()?;
        parsed
            .pointer("/error/message")
            .or_else(|| parsed.get("message"))?
            .as_str()?
    } else {
        trimmed
    };
    let lower = summary.to_lowercase();
    if summary.is_empty()
        || summary.chars().count() > 300
        || [
            "bearer",
            "basic ",
            "sk-",
            "token",
            "secret",
            "password",
            "api_key",
            "apikey",
            "authorization",
            "prompt",
            "://",
            "\\",
            "/",
            "@",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        || (!input.is_empty() && summary.contains(input))
        || input
            .split_whitespace()
            .filter(|word| word.chars().count() >= 12)
            .any(|word| summary.contains(word))
        || ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "DEEPSEEK_API_KEY"]
            .iter()
            .filter_map(|key| std::env::var(key).ok())
            .any(|key| !key.is_empty() && summary.contains(&key))
    {
        return None;
    }
    let clean: String = summary
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    (!clean.trim().is_empty()).then(|| clean.trim().to_string())
}

fn message_wire(
    session_id: &str,
    created_at: i64,
    outcomes: &[TurnOutcome],
    index: usize,
    message: &openpaw_core::Message,
    presentation: Option<&openpaw_store::transcript::MessagePresentationV1>,
) -> serde_json::Value {
    let message_id = message
        .uuid
        .clone()
        .unwrap_or_else(|| format!("msg_{session_id}_{index}"));
    let role = match message.role {
        openpaw_core::Role::User => "user",
        openpaw_core::Role::Assistant => "assistant",
    };
    let parts = message.content.iter().enumerate().filter_map(|(part_index, block)| {
        let part_id = format!("prt_{message_id}_{part_index}");
        match block {
            openpaw_core::ContentBlock::Text { .. } => input_part(session_id, &message_id, part_index, block, presentation),
            openpaw_core::ContentBlock::Thinking { thinking, .. } => Some(serde_json::json!({"id":part_id,"messageID":message_id,"sessionID":session_id,"type":"reasoning","text":thinking})),
            openpaw_core::ContentBlock::Image { .. } | openpaw_core::ContentBlock::Audio { .. } => input_part(session_id, &message_id, part_index, block, presentation),
            openpaw_core::ContentBlock::ToolUse { id, name, input } => Some(serde_json::json!({"id":part_id,"messageID":message_id,"sessionID":session_id,"type":"tool","callID":id,"tool":name,"state":{"status":"running","input":input,"output":"","error":""}})),
            openpaw_core::ContentBlock::ToolResult { tool_use_id, content, is_error, name, .. } => Some(serde_json::json!({"id":part_id,"messageID":message_id,"sessionID":session_id,"type":"tool","callID":tool_use_id,"tool":name,"state":{"status":if *is_error {"error"} else {"completed"},"input":{},"output":if *is_error {""} else {content},"error":if *is_error {content} else {""}}})),
            _ => None,
        }
    }).collect::<Vec<_>>();
    let turn = outcomes.iter().find(|turn| {
        turn.user_message_id == message_id
            || message_id.starts_with(&format!("msg_{session_id}_{}_", turn.turn_id))
    });
    let created = message
        .timestamp
        .as_deref()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.timestamp_millis())
        .or_else(|| {
            turn.as_ref()
                .and_then(|turn| turn.started_at.or(turn.queued_at))
        })
        .unwrap_or(created_at);
    let completed = turn.as_ref().and_then(|turn| turn.completed_at);
    serde_json::json!({"info":{"id":message_id,"sessionID":session_id,"role":role,"time":{"created":created,"completed":completed}},"parts":parts})
}

/// 功能：将完整模型历史投影为公共消息，折叠工具结果并补齐失败展示。
/// 参数：session_id、created_at 为会话身份，outcomes/errors/presentations 为同一快照附加状态，messages 为模型历史。
/// 返回：有序 REST 消息列表；失败占位仅存在于返回值。
/// 错误：无；不匹配的旧工具结果保持独立投影。
/// 状态：纯计算，不改变模型消息或持久化。
/// 并发与取消：调用方提供原子快照；同步不可取消。
/// 权限：错误必须来自安全分类或已过滤的私有元数据。
/// 关联：`CHG-2026-0020`、`CHG-2026-0022`。
pub(crate) fn messages_wire(
    session_id: &str,
    created_at: i64,
    outcomes: &[TurnOutcome],
    messages: &[openpaw_core::Message],
    errors: &HashMap<String, OpenCodeTurnErrorV1>,
    presentations: &HashMap<String, openpaw_store::transcript::MessagePresentationV1>,
) -> Vec<serde_json::Value> {
    let mut result = Vec::<serde_json::Value>::with_capacity(messages.len());
    let mut tool_parts = std::collections::HashMap::<String, (usize, usize)>::new();
    for (message_index, message) in messages.iter().enumerate() {
        let tool_results = (!message.content.is_empty()
            && message.role == openpaw_core::Role::User)
            .then(|| {
                message
                    .content
                    .iter()
                    .map(|block| match block {
                        openpaw_core::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            media,
                            ..
                        } => Some((tool_use_id, content, *is_error, media)),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .flatten();
        if let Some(tool_results) = tool_results {
            let targets = tool_results
                .iter()
                .map(|(tool_use_id, _, _, _)| tool_parts.get(*tool_use_id).copied())
                .collect::<Option<Vec<_>>>();
            if let Some(targets) = targets {
                for ((wire_message_index, part_index), (_, content, is_error, media)) in
                    targets.into_iter().zip(tool_results)
                {
                    let state = &mut result[wire_message_index]["parts"][part_index]["state"];
                    state["status"] =
                        serde_json::json!(if is_error { "error" } else { "completed" });
                    state["output"] = serde_json::json!(if is_error { "" } else { content });
                    state["error"] = serde_json::json!(if is_error { content } else { "" });
                    if !media.is_empty() {
                        result[wire_message_index]["parts"][part_index]["attachments"] =
                            serde_json::json!(attachments(media));
                    }
                }
                continue;
            }
        }

        let wire_message_index = result.len();
        result.push(message_wire(
            session_id,
            created_at,
            outcomes,
            message_index,
            message,
            message.uuid.as_ref().and_then(|id| presentations.get(id)),
        ));
        if message.role != openpaw_core::Role::Assistant {
            continue;
        }
        for (part_index, part) in result[wire_message_index]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if part["type"] == "tool" {
                if let Some(call_id) = part["callID"].as_str() {
                    tool_parts.insert(call_id.to_string(), (wire_message_index, part_index));
                }
            }
        }
    }
    for outcome in outcomes
        .iter()
        .filter(|turn| turn.state == TurnState::Failed)
    {
        let Some(user_index) = result
            .iter()
            .position(|message| message["info"]["id"] == outcome.user_message_id)
        else {
            continue;
        };
        let next_user = result
            .iter()
            .enumerate()
            .skip(user_index + 1)
            .find(|(_, message)| message["info"]["role"] == "user")
            .map(|(index, _)| index)
            .unwrap_or(result.len());
        let failure = errors.get(&outcome.turn_id).cloned().or_else(|| {
            outcome.error.as_ref().map(|error| OpenCodeTurnErrorV1 {
                message_id: result[user_index + 1..next_user]
                    .iter()
                    .rev()
                    .find(|message| message["info"]["role"] == "assistant")
                    .and_then(|message| message["info"]["id"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("msg_{session_id}_{}_assistant", outcome.turn_id)),
                error: classified_error(error),
            })
        });
        let Some(failure) = failure else { continue };
        if let Some(message) = result
            .iter_mut()
            .find(|message| message["info"]["id"] == failure.message_id)
        {
            message["info"]["error"] = serde_json::json!(failure.error);
        } else {
            result.insert(next_user, serde_json::json!({"info":{
                "id":failure.message_id,"sessionID":session_id,"role":"assistant",
                "time":{"created":outcome.started_at.or(outcome.queued_at).unwrap_or(created_at),"completed":outcome.completed_at},
                "error":failure.error
            },"parts":[]}));
        }
    }
    result
}

/// 功能：将单个用户输入块投影为公开 part。
/// 参数：会话、消息编号、原始块次序、内容和可选展示属性。
/// 返回：text/file part 或 None。
/// 错误：无；无原件的旧图片不自动下载。
/// 状态：纯转换。
/// 并发与取消：无共享状态。
/// 权限：媒体仅用于授权消息响应，不输出诊断。
/// 关联：REST 与即时 SSE 输入。
pub(crate) fn input_part(
    session_id: &str,
    message_id: &str,
    index: usize,
    block: &openpaw_core::ContentBlock,
    presentation: Option<&openpaw_store::transcript::MessagePresentationV1>,
) -> Option<serde_json::Value> {
    let mut part = serde_json::json!({"id":format!("prt_{message_id}_{index}"),"messageID":message_id,"sessionID":session_id});
    if let openpaw_core::ContentBlock::Text { text } = block {
        part["type"] = "text".into();
        part["text"] = text.clone().into();
        crate::input_presentation::decorate(&mut part, index, presentation);
    } else if let Ok(Some(media)) = openpaw_core::media::block_media(block) {
        part["type"] = "file".into();
        part["mime"] = media.mime.clone().into();
        part["url"] = media.data_url().into();
        part["attachmentID"] = media.id.into();
        if let Some(name) = media.filename {
            part["filename"] = name.into();
        }
    } else {
        return None;
    }
    Some(part)
}

/// 功能：投影工具结果的内联附件。
/// 参数：media 为已经校验的原件。
/// 返回：带稳定编号的 file 列表。
/// 错误：无。
/// 状态：纯计算。
/// 并发与取消：同步。
/// 权限：只用于授权消息接口。
/// 关联：REST/SSE 工具结果。
pub(crate) fn attachments(media: &[openpaw_core::InlineMedia]) -> Vec<serde_json::Value> {
    media.iter().map(|item| {
        let mut part = serde_json::json!({"type":"file","attachmentID":item.id,"mime":item.mime,"url":item.data_url()});
        if let Some(filename) = &item.filename { part["filename"] = filename.clone().into(); }
        part
    }).collect()
}
