//! Session types and the append-only session event model.
//!
//! The JSONL format (`{"type":"...", ...}` per line) is shared with the
//! upstream-compatible store so transcripts are mutually readable. Unknown
//! `type` values are captured by the `Unknown` variant for forward
//! compatibility.
//!
//! Stability: stable — the JSONL event schema is frozen at 1.0; new event
//! types may be added (unknown types fall to `Unknown` on old readers).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::message::Message;
use crate::provider::Usage;

/// Identifier for a conversation session (newtype for clarity).
///
/// Stability: stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Session-level metadata persisted as the `meta` event header.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub provider: String,
    /// Owning workspace id. `None` / `Some("default")` = default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// Linked task id (sub-agent sessions inherit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Linked goal id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
}

impl SessionMeta {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            created_at: Utc::now(),
            model: model.into(),
            provider: provider.into(),
            workspace_id: None,
            workspace_name: None,
            task_id: None,
            goal_id: None,
        }
    }
}

/// COT: dynamic injection record tracking which thinking paths were injected
/// and whether the agent adopted them.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionRecord {
    pub msg_idx: usize,
    pub memory_id: String,
    pub problem_type: String,
    pub trigger_type: String,
    pub adopted: Option<bool>,
    pub at: DateTime<Utc>,
}

/// 用途：追加式会话事实和模型上下文
/// 字段：消息、用量、运行与快照事件
/// 来源去向：引擎到Store
/// 约束：新事件保留Unknown前向兼容
/// 稳定性：仅新增类型
/// 关联：需求/设计 REQ-OP-004。
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Meta(SessionMeta),
    Message(Message),
    Usage(Usage),
    ToolCallStart {
        call_id: String,
        tool_name: String,
    },
    ToolCallComplete {
        call_id: String,
        tool_name: String,
        success: bool,
        duration_ms: Option<u64>,
    },
    Compaction {
        before_tokens: u32,
        after_tokens: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_count_after: Option<usize>,
    },
    GuardrailDeny {
        tool_name: String,
        reason: String,
    },
    Checkpoint {
        label: Option<String>,
    },
    /// Session-level metadata update (title, mode, model override, ...).
    Metadata {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_override: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    /// 用途：替换模型上下文但保留历史事实
    /// 字段：messages为完整上下文
    /// 来源去向：引擎到Store
    /// 约束：不是新的用户消息
    /// 稳定性：新增事件
    /// 关联：需求/设计 REQ-OP-004。
    ContextSnapshot {
        /// 用途：压缩后的模型输入
        /// 字段：消息数组
        /// 来源去向：压缩到恢复
        /// 约束：不计为新事实
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-003。
        messages: Vec<Message>,
    },
    /// 用途：可靠接受一次运行
    /// 字段：运行标识run_id
    /// 来源去向：引擎到账本
    /// 约束：先于副作用
    /// 稳定性：新增事件
    /// 关联：需求/设计 REQ-OP-003。
    RunStarted {
        /// 用途：关联本次执行
        /// 字段：标识
        /// 来源去向：调用方到账本
        /// 约束：同一运行固定
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-002。
        run_id: String,
    },
    /// 用途：保存运行终态
    /// 字段：运行标识、状态、可选诊断
    /// 来源去向：引擎到账本
    /// 约束：实际清理后写入
    /// 稳定性：新增事件
    /// 关联：需求/设计 REQ-OP-003。
    RunFinished {
        /// 用途：关联开始事件
        /// 字段：标识
        /// 来源去向：运行到账本
        /// 约束：不能代表授权
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-002。
        run_id: String,
        /// 用途：终态
        /// 字段：completed/failed/cancelled/timed_out
        /// 来源去向：引擎到恢复
        /// 约束：不将取消当成功
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-002。
        status: String,
        /// 用途：可选失败诊断
        /// 字段：文本
        /// 来源去向：错误到账本
        /// 约束：不得含密钥
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-003。
        error: Option<String>,
    },
    /// Forward-compatibility catch-all for unknown event types.
    #[serde(untagged)]
    Unknown(Box<serde_json::Value>),
}

/// In-memory view of a session. Persistence is decoupled (openpaw-store).
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    /// Token accumulators use u32 with saturating_add — saturation, not
    /// wrap, on extreme sessions.
    #[serde(default)]
    pub total_input_tokens: u32,
    #[serde(default)]
    pub total_output_tokens: u32,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default = "default_dirty")]
    pub dirty: bool,
    #[serde(default)]
    pub cancelled: bool,
    /// COT: dynamic injection records tracking which memories were injected
    /// and whether they were adopted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub injection_records: Vec<InjectionRecord>,
    /// Agent type marker ("fork" = forked child session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

fn default_dirty() -> bool {
    true
}

impl Session {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            meta: SessionMeta::new(model, provider),
            messages: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            parent_session_id: None,
            dirty: true,
            cancelled: false,
            injection_records: Vec::new(),
            agent_type: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.meta.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_json_shape_is_typed_tag() {
        let meta = SessionMeta::new("echo-1", "echo");
        let line = serde_json::to_string(&SessionEvent::Meta(meta)).unwrap();
        assert!(line.starts_with(r#"{"type":"meta""#), "got: {line}");
    }

    #[test]
    fn unknown_event_type_falls_to_unknown() {
        let line = r#"{"type":"hologram","x":1}"#;
        let ev: SessionEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(ev, SessionEvent::Unknown(_)));
    }

    #[test]
    fn session_event_roundtrip_matrix() {
        let events = vec![
            SessionEvent::Message(Message::user_text("hello")),
            SessionEvent::Usage(Usage::default()),
            SessionEvent::ToolCallStart {
                call_id: "c1".into(),
                tool_name: "bash".into(),
            },
            SessionEvent::ToolCallComplete {
                call_id: "c1".into(),
                tool_name: "bash".into(),
                success: true,
                duration_ms: Some(12),
            },
            SessionEvent::GuardrailDeny {
                tool_name: "bash".into(),
                reason: "destructive".into(),
            },
            SessionEvent::Compaction {
                before_tokens: 1000,
                after_tokens: 300,
                message_count_after: Some(4),
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            let back: SessionEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                json,
                "roundtrip mismatch"
            );
        }
    }

    #[test]
    fn session_defaults_are_forward_compatible() {
        // Upstream transcripts may carry extra fields (thought snapshots
        // etc.); deserialization must ignore them.
        let json = r#"{"meta":{"id":"abc","created_at":"2026-08-30T00:00:00Z","model":"m","provider":"p"},
                       "messages":[],"extra_field":{"x":1}}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.meta.id, "abc");
        assert_eq!(s.total_input_tokens, 0);
        assert!(s.dirty);
    }

    #[test]
    fn session_injection_records_json_roundtrip() {
        let at = Utc::now();
        let mut session = Session::new("m", "p");
        session.injection_records.push(InjectionRecord {
            msg_idx: 2,
            memory_id: "mem_001".into(),
            problem_type: "debugging".into(),
            trigger_type: "dynamic".into(),
            adopted: Some(true),
            at,
        });

        // Write the session as one JSONL line, then read it back.
        let line = serde_json::to_string(&session).unwrap();
        assert!(line.contains("\"injection_records\""), "got: {line}");
        let back: Session = serde_json::from_str(&line).unwrap();
        assert_eq!(back.injection_records.len(), 1);
        let rec = &back.injection_records[0];
        assert_eq!(rec.msg_idx, 2);
        assert_eq!(rec.memory_id, "mem_001");
        assert_eq!(rec.problem_type, "debugging");
        assert_eq!(rec.trigger_type, "dynamic");
        assert_eq!(rec.adopted, Some(true));
        assert_eq!(rec.at, at);
    }

    #[test]
    fn session_without_injection_records_stays_backward_compatible() {
        // Old transcripts carry no injection_records field: default to empty.
        let json = r#"{"meta":{"id":"abc","created_at":"2026-08-30T00:00:00Z","model":"m","provider":"p"},"messages":[]}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert!(s.injection_records.is_empty());

        // Empty records are omitted from serialized output.
        let line = serde_json::to_string(&s).unwrap();
        assert!(!line.contains("injection_records"));
    }
}
