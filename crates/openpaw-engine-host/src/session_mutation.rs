//! 功能：定义 Host 私有的会话元数据、分支与回退事件及其纯状态变换。
//! 输入输出边界：只转换 `SessionEvent::Unknown`、消息前缀和分支消息副本。
//! 禁止承担：不持有 session writer，不获取 runtime 锁，不直接发布 HTTP/SSE 事件。
//! 关联：`CHG-2026-0019`、`DAT-HOST-SESSION-MUTATION-*`。

use std::collections::HashMap;

use openpaw_core::{ContentBlock, Message, Role, SessionEvent};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::{TurnAcceptV1, TurnOutcome, TurnState};

/// 功能：保存一次 PATCH 提供的会话标题与归档字段。
/// 数据边界：缺省字段保持旧投影；`archived=0` 表示取消归档。
/// 关联：`DAT-HOST-SESSION-MUTATION-001`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMetadataV1 {
    /// 功能：保存本次请求提供的新标题；省略时不修改标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// 功能：保存归档 Unix 毫秒；零表示取消归档，省略表示不修改。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) archived: Option<i64>,
}

/// 功能：保存用户分支的直接来源和在该来源下的序号。
/// 数据边界：只用于恢复标题编号和诊断，不投影为公共 `parentID`。
/// 关联：`DAT-HOST-SESSION-MUTATION-002`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionForkOriginV1 {
    /// 功能：保存直接源会话编号。
    #[serde(rename = "sourceSessionID")]
    pub(crate) source_session_id: String,
    /// 功能：保存从一开始的直接分支序号。
    pub(crate) ordinal: u64,
}

/// 功能：保存暂存回退的首条隐藏消息编号。
/// 数据边界：边界消息本身及其尾部均不可见。
/// 关联：`DAT-HOST-SESSION-MUTATION-003`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionRevertStagedV1 {
    /// 功能：保存第一条被回退消息的线协议编号。
    #[serde(rename = "messageID")]
    pub(crate) message_id: String,
}

/// 功能：把逻辑裁剪和替换 prompt 接收保存为一个崩溃原子的提交点。
/// 数据边界：旧尾部仍留在 JSONL 中，但恢复投影不再暴露。
/// 关联：`DAT-HOST-SESSION-MUTATION-003`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRevertCommittedV1 {
    /// 功能：保存第一条永久逻辑删除消息的线协议编号。
    #[serde(rename = "messageID")]
    pub(crate) message_id: String,
    /// 功能：保存与裁剪同时生效的新回合接收事务。
    pub(crate) accept: TurnAcceptV1,
}

/// 功能：表示 Host 能识别的私有会话变更事件。
/// 状态：未知类型和未知版本不会进入此枚举。
/// 关联：`DAT-HOST-SESSION-MUTATION-*`。
#[derive(Debug, Clone)]
pub(crate) enum SessionMutationEvent {
    /// 会话标题或归档状态更新。
    Metadata(SessionMetadataV1),
    /// 用户分支来源。
    ForkOrigin(SessionForkOriginV1),
    /// 暂存回退游标。
    RevertStaged(SessionRevertStagedV1),
    /// 清除暂存回退游标。
    RevertCleared,
    /// 永久提交回退并接收替换 prompt。
    RevertCommitted(SessionRevertCommittedV1),
}

impl SessionMutationEvent {
    /// 功能：把私有会话变更编码为前向兼容的未知 JSONL 事件。
    /// 参数：无，读取当前枚举值。
    /// 返回：带稳定 `type` 字段的 `SessionEvent::Unknown`。
    /// 错误：无；已登记字段均可序列化，异常时退化为空对象。
    /// 状态：纯转换，不写文件或修改投影。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：不添加事件值之外的正文、路径或凭据。
    /// 关联：`DAT-HOST-SESSION-MUTATION-*`。
    pub(crate) fn to_session_event(&self) -> SessionEvent {
        use openpaw_store::transcript::encode;
        match self {
            Self::Metadata(v) => encode(v, "session_metadata_v1"),
            Self::ForkOrigin(v) => encode(v, "session_fork_origin_v1"),
            Self::RevertStaged(v) => encode(v, "session_revert_staged_v1"),
            Self::RevertCleared => encode(&serde_json::json!({}), "session_revert_cleared_v1"),
            Self::RevertCommitted(v) => encode(v, "session_revert_committed_v1"),
        }
    }

    /// 功能：从未知 JSONL 事件解析已登记的会话变更版本。
    /// 参数：`event` 为 SessionReader 按顺序读取的单个事件。
    /// 返回：类型和字段合法时返回事件；其他事件返回 None。
    /// 错误：结构损坏或未知版本安全返回 None，不修改原文件。
    /// 状态：纯解析，不更新内存投影。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：只读取事件自身，不访问工作区或凭据。
    /// 关联：`DAT-HOST-SESSION-MUTATION-*`。
    #[cfg(test)]
    pub(crate) fn from_session_event(event: &SessionEvent) -> Option<Self> {
        use openpaw_store::transcript as store;
        let parsed = store::SessionMutationEvent::from_session_event(event)?;
        Some(match parsed {
            store::SessionMutationEvent::Metadata(v) => Self::Metadata(SessionMetadataV1 {
                title: v.title,
                archived: v.archived,
            }),
            store::SessionMutationEvent::ForkOrigin(v) => Self::ForkOrigin(SessionForkOriginV1 {
                source_session_id: v.source_session_id,
                ordinal: v.ordinal,
            }),
            store::SessionMutationEvent::RevertStaged(v) => {
                Self::RevertStaged(SessionRevertStagedV1 {
                    message_id: v.message_id,
                })
            }
            store::SessionMutationEvent::RevertCleared => Self::RevertCleared,
            store::SessionMutationEvent::RevertCommitted(v) => {
                Self::RevertCommitted(serde_json::from_value(serde_json::to_value(v).ok()?).ok()?)
            }
        })
    }
}

/// 功能：保存按 JSONL 顺序折叠后的会话变更附加状态。
/// 状态：归档零值被规范化为 None，committed 清除暂存游标。
/// 关联：`DAT-HOST-SESSION-MUTATION-*`。
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionMutationProjection {
    /// 最后一次显式标题更新。
    pub(crate) title: Option<String>,
    /// 正数 Unix 毫秒归档时间；未归档为 None。
    pub(crate) archived: Option<i64>,
    /// 当前会话自身的直接分支来源。
    pub(crate) fork_origin: Option<SessionForkOriginV1>,
    /// 第一条暂存隐藏消息编号。
    pub(crate) revert_message_id: Option<String>,
}

impl SessionMutationProjection {
    /// 功能：把一个已解析私有事件顺序折叠进当前附加状态。
    /// 参数：`event` 为按 JSONL 顺序读取的私有会话变更。
    /// 返回：无。
    /// 错误：无；结构校验已由解析器完成。
    /// 状态：只修改当前投影，不写文件。
    /// 并发与取消：调用方独占投影，同步不可取消。
    /// 权限：不读取事件外的资源。
    /// 关联：`DAT-HOST-SESSION-MUTATION-*`。
    pub(crate) fn apply(&mut self, event: SessionMutationEvent) {
        use openpaw_store::transcript as store;
        let mut projection = store::SessionMutationProjection {
            title: self.title.clone(),
            archived: self.archived,
            revert_message_id: self.revert_message_id.clone(),
            fork_origin: self
                .fork_origin
                .as_ref()
                .map(|v| store::SessionForkOriginV1 {
                    source_session_id: v.source_session_id.clone(),
                    ordinal: v.ordinal,
                }),
        };
        if let Some(event) =
            store::SessionMutationEvent::from_session_event(&event.to_session_event())
        {
            projection.apply(event);
        }
        self.title = projection.title;
        self.archived = projection.archived;
        self.revert_message_id = projection.revert_message_id;
        self.fork_origin = projection.fork_origin.map(|v| SessionForkOriginV1 {
            source_session_id: v.source_session_id,
            ordinal: v.ordinal,
        });
    }
}

/// 功能：返回消息在 Host REST 响应中的稳定编号。
/// 参数：`session_id`、零基消息序号和消息值。
/// 返回：优先使用持久 UUID，否则使用兼容 `msg_<session>_<index>` 编号。
/// 错误：无。
/// 状态：纯计算。
/// 并发与取消：同步无锁，不可取消。
/// 权限：不读取消息正文之外的资源。
/// 关联：`API-HOST-SESSION-F03`。
pub(crate) fn wire_message_id(session_id: &str, index: usize, message: &Message) -> String {
    openpaw_store::transcript::wire_message_id(session_id, index, message)
}

/// 功能：解析严格早于指定消息边界的可见前缀长度。
/// 参数：`session_id` 用于旧消息兼容编号，`messages` 为当前完整或可见列表，`message_id` 为边界。
/// 返回：命中边界返回其零基位置，未知边界返回 None。
/// 错误：无。
/// 状态：纯读取，不修改消息。
/// 并发与取消：调用方提供稳定切片，同步不可取消。
/// 权限：不访问文件或网络。
/// 关联：`API-HOST-SESSION-FORK-001`、`API-HOST-SESSION-REVERT-001`。
pub(crate) fn visible_prefix_len(
    session_id: &str,
    messages: &[Message],
    message_id: &str,
) -> Option<usize> {
    openpaw_store::transcript::visible_prefix_len(session_id, messages, message_id)
}

/// 功能：把可见 assistant 回退边界规范化为触发同一回合的 user 消息边界。
/// 参数：session、可见消息、回合结果和客户端请求的消息编号。
/// 返回：权威游标及前缀；真实或已验证失败 assistant 归到触发输入，未知边界返回 None。
/// 错误：无；无有效回合关联且找不到此前真实用户输入时保留原 assistant 边界。
/// 状态：纯读取，不修改消息、回合结果或持久化投影。
/// 并发与取消：调用方提供稳定快照，同步无锁，不可取消。
/// 权限：仅读取角色、内容块类型及不透明 ID，不读取正文值、文件或网络。
/// 关联：`API-HOST-SESSION-REVERT-001`、`CHG-2026-0021`、`CHG-2026-0022`、`CHG-2026-0023`。
pub(crate) fn normalize_revert_boundary(
    session_id: &str,
    messages: &[Message],
    turn_outcomes: &[TurnOutcome],
    message_id: &str,
) -> Option<(String, usize)> {
    let Some(requested_prefix) = visible_prefix_len(session_id, messages, message_id) else {
        // runtime 已验证该 ID 存在于公共错误投影；这里仅还原其真实 user 边界。
        return turn_outcomes
            .iter()
            .find(|turn| {
                turn.state == TurnState::Failed
                    && is_turn_assistant_message_id(session_id, &turn.turn_id, message_id)
            })
            .and_then(|turn| {
                visible_prefix_len(session_id, messages, &turn.user_message_id)
                    .map(|prefix| (turn.user_message_id.clone(), prefix))
            });
    };
    let original = (message_id.to_string(), requested_prefix);
    if messages[requested_prefix].role != Role::Assistant {
        return Some(original);
    }
    turn_outcomes
        .iter()
        .find(|outcome| is_turn_assistant_message_id(session_id, &outcome.turn_id, message_id))
        .and_then(|outcome| {
            visible_prefix_len(session_id, messages, &outcome.user_message_id)
                .map(|prefix| (outcome.user_message_id.clone(), prefix))
        })
        .or_else(|| {
            // 分支和旧记录可能没有回合结果；纯工具结果不是触发本轮的用户输入。
            messages[..requested_prefix]
                .iter()
                .rposition(|message| {
                    message.role == Role::User
                        && (message.content.is_empty()
                            || !message
                                .content
                                .iter()
                                .all(|block| matches!(block, ContentBlock::ToolResult { .. })))
                })
                .map(|prefix| {
                    (
                        wire_message_id(session_id, prefix, &messages[prefix]),
                        prefix,
                    )
                })
        })
        .or(Some(original))
}

/// 功能：验证 assistant ID 是否精确属于指定会话与回合。
/// 参数：session_id、turn_id 为权威归属，message_id 为待验证编号。
/// 返回：稳定 assistant 或规范的正整数响应后缀匹配时返回 true。
/// 错误：无。
/// 状态：纯计算。
/// 并发与取消：同步无锁，不可取消。
/// 权限：不读取正文或外部资源。
/// 关联：`CHG-2026-0021`、`CHG-2026-0022`。
pub(crate) fn is_turn_assistant_message_id(
    session_id: &str,
    turn_id: &str,
    message_id: &str,
) -> bool {
    openpaw_store::transcript::is_turn_assistant_message_id(session_id, turn_id, message_id)
}

/// 功能：将已经验证存在的失败占位边界映射为真实历史前缀。
/// 参数：session_id、messages、outcomes 为稳定快照，message_id 为公共错误消息编号。
/// 返回：该失败回合真实消息之后、下一用户回合之前的位置；不可验证时返回 None。
/// 错误：无。
/// 状态：纯计算，不将占位或错误复制到分支模型历史。
/// 并发与取消：调用方持有会话变更闸门，同步不可取消。
/// 权限：只读取不透明 ID，不访问文件或网络。
/// 关联：`CHG-2026-0022`。
pub(crate) fn failed_message_prefix_len(
    session_id: &str,
    messages: &[Message],
    outcomes: &[TurnOutcome],
    message_id: &str,
) -> Option<usize> {
    let turn = outcomes.iter().find(|turn| {
        turn.state == TurnState::Failed
            && is_turn_assistant_message_id(session_id, &turn.turn_id, message_id)
    })?;
    let start = visible_prefix_len(session_id, messages, &turn.user_message_id)?;
    Some(
        messages
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, message)| {
                message
                    .uuid
                    .as_ref()
                    .is_some_and(|id| outcomes.iter().any(|other| &other.user_message_id == id))
            })
            .map(|(index, _)| index)
            .unwrap_or(messages.len()),
    )
}

/// 功能：复制会话消息并重新生成消息、工具调用和内部引用编号。
/// 参数：`messages` 为已经按 fork 边界裁剪的可见消息。
/// 返回：正文和角色保持一致、身份与源会话隔离的新消息列表。
/// 错误：无。
/// 状态：不修改源消息；使用随机 UUID 生成新身份。
/// 并发与取消：同步无共享锁，不可取消。
/// 权限：只复制调用方提供的消息，不访问工作区。
/// 关联：`API-HOST-SESSION-FORK-001`。
pub(crate) fn clone_messages_for_fork(messages: &[Message]) -> Vec<Message> {
    let message_ids: HashMap<String, String> = messages
        .iter()
        .filter_map(|message| {
            message
                .uuid
                .as_ref()
                .map(|id| (id.clone(), Uuid::new_v4().simple().to_string()))
        })
        .collect();
    let tool_ids: HashMap<String, String> = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => {
                Some((id.clone(), Uuid::new_v4().simple().to_string()))
            }
            _ => None,
        })
        .collect();

    messages
        .iter()
        .cloned()
        .map(|mut message| {
            message.uuid = Some(
                message
                    .uuid
                    .as_ref()
                    .and_then(|id| message_ids.get(id))
                    .cloned()
                    .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
            );
            for block in &mut message.content {
                match block {
                    ContentBlock::ToolUse { id, .. } => {
                        if let Some(replacement) = tool_ids.get(id) {
                            *id = replacement.clone();
                        }
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        if let Some(replacement) = tool_ids.get(tool_use_id) {
                            *tool_use_id = replacement.clone();
                        }
                    }
                    ContentBlock::CompactBoundary { metadata } => {
                        if let Some(last_user) = metadata.last_user_message_uuid.as_mut() {
                            if let Some(replacement) = message_ids.get(last_user) {
                                *last_user = replacement.clone();
                            }
                        }
                    }
                    _ => {}
                }
            }
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use openpaw_core::{CompactMetadata, CompactType, ContentBlock, Message, Role, SessionEvent};

    use super::{
        clone_messages_for_fork, visible_prefix_len, SessionForkOriginV1, SessionMetadataV1,
        SessionMutationEvent, SessionMutationProjection, SessionRevertStagedV1,
    };

    #[test]
    fn private_events_roundtrip_and_unknown_versions_are_ignored() {
        let cases = [
            SessionMutationEvent::Metadata(SessionMetadataV1 {
                title: Some("Renamed".into()),
                archived: Some(123),
            }),
            SessionMutationEvent::ForkOrigin(SessionForkOriginV1 {
                source_session_id: "source".into(),
                ordinal: 2,
            }),
            SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
                message_id: "m2".into(),
            }),
            SessionMutationEvent::RevertCleared,
        ];
        for expected in cases {
            let event = expected.to_session_event();
            let parsed = SessionMutationEvent::from_session_event(&event).expect("roundtrip");
            assert_eq!(
                serde_json::to_value(parsed.to_session_event()).unwrap(),
                serde_json::to_value(event).unwrap()
            );
        }
        assert!(
            SessionMutationEvent::from_session_event(&SessionEvent::Unknown(Box::new(
                serde_json::json!({"type":"session_revert_staged_v2","messageID":"m2"}),
            )))
            .is_none()
        );
    }

    #[test]
    fn visible_prefix_stops_strictly_before_the_boundary() {
        let messages = vec![
            Message::user_text("one").with_uuid("m1"),
            Message::assistant_text("two").with_uuid("m2"),
            Message::user_text("three").with_uuid("m3"),
        ];
        assert_eq!(visible_prefix_len("session", &messages, "m2"), Some(1));
        assert_eq!(visible_prefix_len("session", &messages, "missing"), None);
    }

    #[test]
    fn projection_folds_metadata_fork_origin_and_revert_lifecycle() {
        let mut projection = SessionMutationProjection::default();
        projection.apply(SessionMutationEvent::Metadata(SessionMetadataV1 {
            title: Some("Renamed".into()),
            archived: Some(123),
        }));
        projection.apply(SessionMutationEvent::ForkOrigin(SessionForkOriginV1 {
            source_session_id: "source".into(),
            ordinal: 2,
        }));
        projection.apply(SessionMutationEvent::RevertStaged(SessionRevertStagedV1 {
            message_id: "m2".into(),
        }));
        assert_eq!(projection.title.as_deref(), Some("Renamed"));
        assert_eq!(projection.archived, Some(123));
        assert_eq!(
            projection.fork_origin.as_ref().map(|origin| origin.ordinal),
            Some(2)
        );
        assert_eq!(projection.revert_message_id.as_deref(), Some("m2"));
        projection.apply(SessionMutationEvent::RevertCleared);
        assert_eq!(projection.revert_message_id, None);
    }

    #[test]
    fn visible_prefix_recognizes_legacy_wire_message_ids() {
        let messages = vec![Message::user_text("one"), Message::assistant_text("two")];
        assert_eq!(
            visible_prefix_len("session", &messages, "msg_session_1"),
            Some(1)
        );
    }

    #[test]
    fn fork_clone_rekeys_messages_tool_links_and_compaction_boundary() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-old".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path":"README.md"}),
                }],
                uuid: Some("assistant-old".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call-old".into(),
                        content: "ok".into(),
                        is_error: false,
                        name: Some("read_file".into()),
                        protected: false,
                    },
                    ContentBlock::CompactBoundary {
                        metadata: CompactMetadata {
                            compact_type: CompactType::Manual,
                            pre_compact_token_count: 12,
                            last_user_message_uuid: Some("assistant-old".into()),
                            pre_compact_discovered_tools: vec![],
                        },
                    },
                ],
                uuid: Some("user-old".into()),
                ..Default::default()
            },
        ];

        let cloned = clone_messages_for_fork(&messages);
        assert_eq!(cloned.len(), 2);
        assert_ne!(cloned[0].uuid, messages[0].uuid);
        assert_ne!(cloned[1].uuid, messages[1].uuid);
        let ContentBlock::ToolUse { id: new_call, .. } = &cloned[0].content[0] else {
            panic!("tool use");
        };
        let ContentBlock::ToolResult { tool_use_id, .. } = &cloned[1].content[0] else {
            panic!("tool result");
        };
        assert_eq!(tool_use_id, new_call);
        assert_ne!(new_call, "call-old");
        let ContentBlock::CompactBoundary { metadata } = &cloned[1].content[1] else {
            panic!("compact boundary");
        };
        assert_eq!(metadata.last_user_message_uuid, cloned[0].uuid);
    }
}
