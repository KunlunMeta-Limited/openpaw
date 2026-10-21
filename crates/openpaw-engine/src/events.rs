//! Events emitted by the agent loop during a turn.

use openpaw_core::Usage;

/// Classifier / guardrail progress for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierStatus {
    Checking,
    AutoApproved { reason: String },
    SuggestConfirm { reason: String },
    AutoDenied { reason: String },
}

/// One observable step of a turn. The UI (or the parity harness) consumes
/// these; the loop never blocks on the sink.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    MessageStart,
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolUseStart {
        tool_use_id: String,
        name: String,
    },
    /// 已通过校验和审批、即将真实执行的工具调用。
    /// `input` 只保留供活动界面使用的限长白名单字段，不包含正文或命令。
    ToolExecutionStart {
        /// 已校验并获准执行的调用编号，与后续 ToolResult 关联。
        tool_use_id: String,
        /// 实际即将执行的工具名称，不代表执行成功。
        name: String,
        /// 经过白名单和长度限制的展示属性，不含原始正文、命令或凭据。
        input: serde_json::Value,
    },
    ToolResult {
        /// 功能：已批准工具结果的媒体附件；Office 不消费原件。
        media: Vec<openpaw_core::InlineMedia>,
        tool_use_id: String,
        content: String,
        is_error: bool,
        name: Option<String>,
        duration_ms: Option<u64>,
    },
    ClassifierStatus {
        tool_use_id: String,
        status: ClassifierStatus,
    },
    /// E2: emitted when the compaction subsystem lands.
    CompactionPerformed {
        before_tokens: u32,
        after_tokens: u32,
    },
    TurnUsage {
        usage: Usage,
    },
}

impl AgentEvent {
    /// 功能：取得不包含正文的固定事件分类。
    /// 参数：当前事件。
    /// 返回：用于入口适配和 parity 比较的稳定分类字符串。
    /// 错误：无。
    /// 状态：只读，不消费事件。
    /// 并发与取消：同步无锁。
    /// 权限：不返回工具输入或消息正文。
    /// 关联：共享 Engine 事件与 CLI/Host parity。
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::MessageStart => "message_start",
            AgentEvent::TextDelta { .. } => "text_delta",
            AgentEvent::ThinkingDelta { .. } => "thinking_delta",
            AgentEvent::ToolUseStart { .. } => "tool_use_start",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::ClassifierStatus { .. } => "classifier_status",
            AgentEvent::CompactionPerformed { .. } => "compaction_performed",
            AgentEvent::TurnUsage { .. } => "turn_usage",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kinds_are_stable_tags() {
        assert_eq!(AgentEvent::MessageStart.kind(), "message_start");
        assert_eq!(
            AgentEvent::TextDelta { text: "x".into() }.kind(),
            "text_delta"
        );
        assert_eq!(
            AgentEvent::ToolResult {
                media: Vec::new(),
                tool_use_id: "c1".into(),
                content: "y".into(),
                is_error: false,
                name: None,
                duration_ms: None,
            }
            .kind(),
            "tool_result"
        );
        assert_eq!(
            AgentEvent::ToolExecutionStart {
                tool_use_id: "c2".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path":"README.md"}),
            }
            .kind(),
            "tool_execution_start"
        );
    }
}
