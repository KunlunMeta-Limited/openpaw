//! Context compaction — E2 minimal rewrite (token estimate + threshold
//! trigger + single summary), semantics aligned to the upstream pipeline
//! (see docs/spikes/S1-agent-loop.md section 2 ④).
//!
//! Deliberately deferred: microcompact sweeps, snip compaction, cached
//! microcompact, PTL retry, time-based decay, post-compact file/skill
//! reinjection.

use openpaw_core::{
    CompactType, CompletionRequest, ContentBlock, Error, LlmProvider, Message, Result, Role,
    Session,
};

/// CJK-aware token estimate: ASCII ≈ /4, CJK ideographs/kana/hangul ≈ ×1.5,
/// other multibyte ≈ ×1.8 (same-source formula).
pub fn estimate_tokens(text: &str) -> usize {
    let ascii_len = text.chars().filter(|c| c.is_ascii()).count();
    let cjk_len = text
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            (0x4E00..=0x9FFF).contains(&cp)
                || (0x3400..=0x4DBF).contains(&cp)
                || (0x3040..=0x309F).contains(&cp)
                || (0x30A0..=0x30FF).contains(&cp)
                || (0xAC00..=0xD7AF).contains(&cp)
        })
        .count();
    let other_len = text.chars().count() - ascii_len - cjk_len;
    (ascii_len as f64 / 4.0 + cjk_len as f64 * 1.5 + other_len as f64 * 1.8).ceil() as usize
}

/// Estimate the full context size: system + every message block + the
/// serialized tool schemas (approximation).
pub fn estimate_session_tokens(system: &str, session: &Session, tools_json_approx: usize) -> usize {
    let mut total = estimate_tokens(system) + tools_json_approx;
    for msg in &session.messages {
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => total += estimate_tokens(text),
                ContentBlock::Thinking { thinking, .. } => total += estimate_tokens(thinking),
                ContentBlock::ToolUse { input, .. } => {
                    total += estimate_tokens(&serde_json::to_string(input).unwrap_or_default());
                }
                ContentBlock::ToolResult { content, .. } => total += estimate_tokens(content),
                ContentBlock::Image { url, data, .. } => {
                    total += estimate_tokens(url);
                    if let Some(d) = data {
                        total += d.len() / 4;
                    }
                }
                ContentBlock::Document { url, data, .. } => {
                    total += estimate_tokens(url);
                    if let Some(d) = data {
                        total += d.len() / 4;
                    }
                }
                ContentBlock::CompactBoundary { .. } | ContentBlock::Unknown(_) => {}
            }
        }
    }
    total
}

/// Context-aware buffer (mirrors the upstream autocompact buffer).
pub fn context_aware_buffer(model_limit: usize) -> usize {
    if model_limit >= 800_000 {
        50_000
    } else if model_limit >= 400_000 {
        30_000
    } else {
        13_000
    }
}

#[derive(Debug, Clone)]
pub struct CompactOptions {
    pub model_limit: usize,
    pub headroom: f64,
    pub keep_recent_pairs: usize,
    pub protect_head_pairs: usize,
    pub min_messages_before_compact: usize,
    pub trigger_proactive: bool,
    pub proactive_headroom: f64,
    pub use_context_aware_buffer: bool,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            model_limit: 128_000,
            headroom: 0.18,
            keep_recent_pairs: 4,
            protect_head_pairs: 1,
            min_messages_before_compact: 8,
            trigger_proactive: true,
            proactive_headroom: 0.30,
            use_context_aware_buffer: true,
        }
    }
}

/// Why a compaction was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactTrigger {
    None,
    /// Estimated context exceeded the hard threshold — compact now.
    HardLimit,
    /// Estimated context exceeded the proactive threshold — compact early.
    Proactive,
    /// A ContextTooLong API error demands reactive compaction.
    ApiError,
}

/// Trigger decision over a precomputed token estimate (the loop computes
/// the estimate once per iteration and reuses it).
pub fn should_compact_with_options_estimated(
    session: &Session,
    opts: &CompactOptions,
    estimated: usize,
) -> CompactTrigger {
    if session.messages.len() <= opts.min_messages_before_compact {
        return CompactTrigger::None;
    }

    let hard_threshold = if opts.use_context_aware_buffer {
        opts.model_limit
            .saturating_sub(context_aware_buffer(opts.model_limit))
    } else {
        (opts.model_limit as f64 * (1.0 - opts.headroom)) as usize
    };
    let proactive_threshold = if opts.use_context_aware_buffer {
        opts.model_limit
            .saturating_sub(context_aware_buffer(opts.model_limit) * 5 / 2)
    } else {
        (opts.model_limit as f64 * (1.0 - opts.proactive_headroom)) as usize
    };

    if estimated > hard_threshold {
        return CompactTrigger::HardLimit;
    }
    if opts.trigger_proactive && estimated > proactive_threshold {
        return CompactTrigger::Proactive;
    }
    CompactTrigger::None
}

const COMPACTION_SYSTEM: &str = "Summarize the following conversation preserving: key decisions made, tool results and their outcomes, user preferences stated, any facts the user asked to remember, and any tasks that were started but not yet completed. Be concise — aim for 1/5 the original length. Start with '[Context Summary]'.";

/// Render the compacted range as a transcript for the summarizer.
fn format_for_summary(messages: &[Message]) -> String {
    let mut buf = String::new();
    for msg in messages {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    buf.push_str(&format!("[{role}] {text}\n"));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let input_preview: String = serde_json::to_string(input)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect();
                    buf.push_str(&format!("[{role} tool_use] {name}({input_preview})\n"));
                }
                ContentBlock::ToolResult {
                    content: _,
                    protected: true,
                    ..
                } => {
                    buf.push_str(&format!(
                        "[{role} tool_result (protected)] [full content preserved]\n"
                    ));
                }
                ContentBlock::ToolResult { content, .. } => {
                    let preview: String = content.chars().take(500).collect();
                    buf.push_str(&format!("[{role} tool_result] {preview}\n"));
                }
                ContentBlock::Thinking { thinking, .. } => {
                    let preview: String = thinking.chars().take(100).collect();
                    buf.push_str(&format!("[{role} thinking] {preview}...\n"));
                }
                ContentBlock::Image { url, .. } | ContentBlock::Document { url, .. } => {
                    buf.push_str(&format!("[{role} media] {url}\n"));
                }
                ContentBlock::CompactBoundary { .. } | ContentBlock::Unknown(_) => {}
            }
        }
    }
    buf
}

/// Adjust the cut point so no tool_result in the kept tail loses its
/// tool_use (pair integrity across the boundary).
fn adjust_index_to_preserve_tool_pairs(messages: &[Message], start_index: usize) -> usize {
    if start_index >= messages.len() {
        return start_index;
    }
    let mut required: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &messages[start_index..] {
        for block in &msg.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                required.insert(tool_use_id.clone());
            }
        }
    }
    // Walk backwards from the cut point: extend the kept tail to include
    // any tool_use whose id is required.
    let mut adjusted = start_index;
    for (i, msg) in messages[..start_index].iter().enumerate().rev() {
        let supplies = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { id, .. } if required.contains(id)));
        if supplies {
            adjusted = i;
        }
    }
    adjusted
}

/// Boundary marker message recording the pre-compaction size.
pub fn create_compact_boundary_message(
    compact_type: CompactType,
    pre_compact_token_count: usize,
) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::CompactBoundary {
            metadata: openpaw_core::CompactMetadata {
                compact_type,
                pre_compact_token_count,
                last_user_message_uuid: None,
                pre_compact_discovered_tools: Vec::new(),
            },
        }],
        ..Default::default()
    }
}

/// Single-summary compaction: protect the head, summarize the middle via
/// one LLM call, keep the recent tail. Returns the number of compacted
/// messages (0 = nothing to do).
pub async fn compact_session(
    provider: &dyn LlmProvider,
    session: &mut Session,
    opts: &CompactOptions,
) -> Result<usize> {
    let keep_count = opts.keep_recent_pairs * 2;
    let protect_count = opts.protect_head_pairs * 2;

    if session.messages.len() <= keep_count + protect_count {
        return Ok(0);
    }

    let protected_head: Vec<Message> = session.messages[..protect_count].to_vec();
    let raw_remaining_end = session.messages.len().saturating_sub(keep_count);
    let remaining_end = adjust_index_to_preserve_tool_pairs(&session.messages, raw_remaining_end);
    if remaining_end <= protect_count {
        return Ok(0);
    }

    let old_messages: Vec<Message> = session.messages[protect_count..remaining_end].to_vec();
    let mut recent: Vec<Message> = session.messages[remaining_end..].to_vec();
    let compacted_count = old_messages.len();
    let pre_compact_tokens = estimate_session_tokens("", session, 0);

    let transcript = format_for_summary(&old_messages);
    let req = CompletionRequest {
        model: provider.default_model().to_string(),
        system: Some(COMPACTION_SYSTEM.to_string()),
        messages: vec![Message::user_text(transcript)],
        max_tokens: 2048,
        temperature: Some(0.1),
        ..Default::default()
    };
    let resp = provider
        .complete(req)
        .await
        .map_err(|e| Error::session(format!("compaction summary failed: {e}")))?;
    let summary_text = resp.text();

    session.messages.clear();
    session.messages.extend(protected_head);
    session.messages.push(create_compact_boundary_message(
        CompactType::Auto,
        pre_compact_tokens,
    ));

    // The summary rides on a User message; merge into an adjacent User
    // message when present to preserve role alternation.
    let summary_prefix = format!("[Context summary: {summary_text}]\n\n");
    let mut merged_into_recent = false;
    if let Some(first) = recent.first_mut() {
        if matches!(first.role, Role::User) {
            let has_text = first
                .content
                .iter_mut()
                .find(|b| matches!(b, ContentBlock::Text { .. }));
            match has_text {
                Some(ContentBlock::Text { text }) => text.insert_str(0, &summary_prefix),
                _ => first.content.insert(
                    0,
                    ContentBlock::Text {
                        text: summary_prefix.trim_end().to_string(),
                    },
                ),
            }
            merged_into_recent = true;
        }
    }
    if !merged_into_recent {
        session
            .messages
            .push(Message::user_text(summary_prefix.trim_end()));
    }
    session.messages.extend(recent);

    Ok(compacted_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_cjk_aware() {
        // Pure ASCII: ~4 chars/token.
        assert_eq!(estimate_tokens("abcd"), 1);
        // CJK chars weigh ~1.5 tokens each.
        let cjk = estimate_tokens("你好世界");
        assert_eq!(cjk, 6, "4 CJK × 1.5 = 6");
        // Mixed: 4 ASCII chars (1 token) + 2 CJK (3 tokens).
        let mixed = estimate_tokens("abcd你好");
        assert_eq!(mixed, 4);
    }

    #[test]
    fn context_aware_buffer_tiers() {
        assert_eq!(context_aware_buffer(1_000_000), 50_000);
        assert_eq!(context_aware_buffer(400_000), 30_000);
        assert_eq!(context_aware_buffer(128_000), 13_000);
    }

    fn session_with(n_pairs: usize) -> Session {
        let mut s = Session::new("m", "mock");
        for i in 0..n_pairs {
            s.messages.push(Message::user_text(format!(
                "question {i} with some padding text"
            )));
            s.messages.push(Message::assistant_text(format!(
                "answer {i} with some padding text as well"
            )));
        }
        s
    }

    #[test]
    fn trigger_three_thresholds() {
        // Headroom mode with clean numbers: hard = 800×0.82 = 656,
        // proactive = 800×0.70 = 560.
        let opts = CompactOptions {
            model_limit: 800,
            use_context_aware_buffer: false,
            ..Default::default()
        };
        // Gate: below min_messages_before_compact never triggers, even way
        // past the threshold.
        let tiny = session_with(1); // 2 messages ≤ min(4)
        assert_eq!(
            should_compact_with_options_estimated(&tiny, &opts, 900),
            CompactTrigger::None,
            "below min_messages_before_compact never triggers"
        );

        let big = session_with(20);
        // Tier 1: below proactive threshold → None.
        assert_eq!(
            should_compact_with_options_estimated(&big, &opts, 100),
            CompactTrigger::None
        );
        // Tier 2: past proactive (560) but not hard (656).
        assert_eq!(
            should_compact_with_options_estimated(&big, &opts, 580),
            CompactTrigger::Proactive
        );
        // Tier 3: past hard threshold.
        assert_eq!(
            should_compact_with_options_estimated(&big, &opts, 700),
            CompactTrigger::HardLimit
        );
    }

    #[test]
    fn tool_pair_integrity_adjusts_cut() {
        let mut s = session_with(3);
        // Append a tool_use in an assistant message and its result.
        s.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "x"}),
            }],
            ..Default::default()
        });
        s.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "contents".into(),
                is_error: false,
                name: None,
                protected: false,
            }],
            ..Default::default()
        });
        // A naive cut right before the result would orphan it.
        let naive = s.messages.len() - 1;
        let adjusted = adjust_index_to_preserve_tool_pairs(&s.messages, naive);
        assert!(
            adjusted < naive,
            "cut must move back to include the tool_use"
        );
    }

    #[test]
    fn boundary_message_shape() {
        let m = create_compact_boundary_message(CompactType::Auto, 42);
        assert!(matches!(
            m.content.first(),
            Some(ContentBlock::CompactBoundary { metadata })
                if metadata.pre_compact_token_count == 42
        ));
    }
}
