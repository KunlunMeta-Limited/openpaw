//! 功能：把 AgentLoop 的内部 `AgentEvent` 转换为 OpenCode v1 事件。
//! 输入输出边界：只负责事件字段、稳定 id 和顺序，不负责执行工具或持久化消息。
//! 关联：`EVT-HOST-*`、`API-HOST-SSE-*`。

use std::path::Path;

use openpaw_engine::AgentEvent;

use crate::protocol::{OpenCodeEvent, SessionWire};

/// 功能：生成 OpenWork 可消费的会话创建或更新事件。
/// 参数：`event_type` 为 `session.created` 或 `session.updated`，`session` 为完整公共投影。
/// 返回：`properties={sessionID,info}` 的 OpenCode 事件。
/// 错误：无；序列化失败由 `OpenCodeEvent::new` 退化为 null。
/// 状态：纯投影，不修改 session 或 JSONL。
/// 并发与取消：同步无锁，不可取消。
/// 权限：只发送公共 Session 字段，不包含私有 fork 来源。
/// 关联：`EVT-HOST-SESSION-001`、`EVT-HOST-SESSION-004`。
pub(crate) fn session_changed(event_type: &'static str, session: &SessionWire) -> OpenCodeEvent {
    OpenCodeEvent::new(
        event_type,
        serde_json::json!({"sessionID":session.id,"info":session}),
    )
}

/// 功能：将单个 AgentEvent 转换为 OpenCode 事件。
/// 参数：`session_id` 为会话编号，`event` 为引擎事件。
/// 返回：使用兼容默认回合编号的协议事件；内部事件若按协议排除则返回 None。
/// 约束：生成的 id 对客户端不透明；事件字段必须符合 `EVT-HOST-*`。
/// 关联：`EVT-HOST-MESSAGE-*`。
#[allow(dead_code)]
pub fn to_wire(session_id: &str, event: &AgentEvent) -> Option<OpenCodeEvent> {
    to_wire_for_turn_response(session_id, None, 0, event)
}

/// 功能：把事件转换为带回合编号的协议事件。
/// 参数：`turn_id` 可选，用于区分同一 session 的并行/连续回合。
/// 返回：包含稳定 message/part id 的 OpenCode 事件。
/// 并发与取消：只生成事件，不改变回合状态或取消信号。
/// 关联：`EVT-HOST-MESSAGE-001`。
#[allow(dead_code)]
pub fn to_wire_for_turn(
    session_id: &str,
    turn_id: Option<&str>,
    event: &AgentEvent,
) -> Option<OpenCodeEvent> {
    to_wire_for_turn_response(session_id, turn_id, 0, event)
}

/// 功能：按模型响应序号转换 AgentEvent，保证工具循环中的消息/part id 不重复。
/// 参数：`response_index` 标记同一回合内的第几个 assistant 响应。
/// 返回：可发送到 SSE 和消息历史的协议事件；不属于兼容协议的内部事件返回 None。
/// 约束：转换只读输入；不改变 AgentLoop、JSONL 或 Office 状态。
/// 关联：`EVT-HOST-MESSAGE-*`、`EVT-HOST-TOOL-001`。
pub fn to_wire_for_turn_response(
    session_id: &str,
    turn_id: Option<&str>,
    response_index: usize,
    event: &AgentEvent,
) -> Option<OpenCodeEvent> {
    let message_id = assistant_message_id(session_id, turn_id, response_index);
    match event {
        AgentEvent::MessageStart => Some(OpenCodeEvent::new(
            "message.updated",
            serde_json::json!({"info":{"id":message_id,"sessionID":session_id,"role":"assistant","time":{"created":chrono::Utc::now().timestamp_millis()}}}),
        )),
        AgentEvent::TextDelta { text } => Some(OpenCodeEvent::new(
            "message.part.delta",
            serde_json::json!({"partID":part_id(session_id, turn_id, response_index, "text"),"sessionID":session_id,"messageID":message_id,"field":"text","delta":text}),
        )),
        AgentEvent::ThinkingDelta { text } => Some(OpenCodeEvent::new(
            "message.part.delta",
            serde_json::json!({"partID":part_id(session_id, turn_id, response_index, "reasoning"),"sessionID":session_id,"messageID":message_id,"field":"text","delta":text}),
        )),
        AgentEvent::ToolUseStart {
            tool_use_id, name, ..
        } => Some(OpenCodeEvent::new(
            "message.part.updated",
            serde_json::json!({"part":{"id":part_id(session_id, turn_id, response_index, tool_use_id),"messageID":message_id,"sessionID":session_id,"type":"tool","callID":tool_use_id,"tool":name,"state":{"status":"running","input":{},"output":"","error":""}}}),
        )),
        AgentEvent::ToolResult {
            media,
            tool_use_id,
            content,
            is_error,
            name,
            duration_ms,
        } => Some(OpenCodeEvent::new(
            "message.part.updated",
            serde_json::json!({"part":{"id":part_id(session_id, turn_id, response_index, tool_use_id),"messageID":message_id,"sessionID":session_id,"type":"tool","callID":tool_use_id,"tool":name,"state":{"status":if *is_error {"error"} else {"completed"},"input":{},"output":content,"error":if *is_error {content} else {""},"durationMs":duration_ms},"attachments":crate::protocol::projection::attachments(media)}}),
        )),
        AgentEvent::ClassifierStatus {
            tool_use_id,
            status,
        } => {
            let _ = (tool_use_id, status, session_id);
            None
        }
        AgentEvent::CompactionPerformed { .. } => None,
        AgentEvent::ToolExecutionStart { .. } => None,
        AgentEvent::TurnUsage { .. } => None,
    }
}

fn assistant_message_id(session_id: &str, turn_id: Option<&str>, response_index: usize) -> String {
    let base = turn_id
        .map(|turn| format!("msg_{session_id}_{turn}_assistant"))
        .unwrap_or_else(|| format!("msg_{session_id}_assistant"));
    if response_index == 0 {
        base
    } else {
        format!("{base}_{response_index}")
    }
}

fn part_id(session_id: &str, turn_id: Option<&str>, response_index: usize, part: &str) -> String {
    let base = turn_id
        .map(|turn| format!("prt_{session_id}_{turn}_{part}"))
        .unwrap_or_else(|| format!("prt_{session_id}_{part}"));
    if response_index == 0 {
        base
    } else {
        format!("{base}_{response_index}")
    }
}

/// 功能：生成会话 busy/idle 状态事件。
/// 参数：`session_id` 为会话编号，`status` 为已登记状态字符串。
/// 返回：`session.status` 协议事件。
/// 状态：回合开始通常为 busy，回合结束为 idle。
/// 关联：`EVT-HOST-SESSION-002`。
pub fn session_status(session_id: &str, status: &'static str) -> OpenCodeEvent {
    OpenCodeEvent::new(
        "session.status",
        serde_json::json!({"sessionID":session_id,"status":{"type":status}}),
    )
}

/// 功能：生成回合结束的 `session.idle` 事件。
/// 参数：`session_id` 为会话编号。
/// 返回：不携带消息正文的 idle 事件。
/// 关联：`EVT-HOST-SESSION-003`。
pub fn session_idle(session_id: &str) -> OpenCodeEvent {
    OpenCodeEvent::new("session.idle", serde_json::json!({"sessionID":session_id}))
}

/// 功能：生成非流式 assistant 最终消息和 text part。
/// 参数：`session_id` 为会话编号，`text` 为完整回复文本。
/// 返回：按 message.updated、message.part.updated 顺序返回两个事件。
/// 关联：`EVT-HOST-MESSAGE-001`、`EVT-HOST-MESSAGE-002`。
#[allow(dead_code)]
pub fn assistant_final(session_id: &str, text: &str) -> [OpenCodeEvent; 2] {
    assistant_final_for_turn(session_id, None, text)
}

/// 功能：生成指定回合的最终 assistant 事件对。
/// 参数：`turn_id` 用于隔离回合 id；`text` 为完整文本。
/// 返回：message.updated 和完整 text part。
/// 关联：`EVT-HOST-MESSAGE-*`。
pub fn assistant_final_for_turn(
    session_id: &str,
    turn_id: Option<&str>,
    text: &str,
) -> [OpenCodeEvent; 2] {
    assistant_final_for_turn_response(session_id, turn_id, 0, text)
}

/// 功能：生成指定回合及响应序号的最终 assistant 事件对。
/// 参数：`response_index` 区分工具循环中的多次响应。
/// 返回：先消息完成事件，再 text part 事件。
/// 关联：`EVT-HOST-MESSAGE-*`。
pub fn assistant_final_for_turn_response(
    session_id: &str,
    turn_id: Option<&str>,
    response_index: usize,
    text: &str,
) -> [OpenCodeEvent; 2] {
    let message_id = assistant_message_id(session_id, turn_id, response_index);
    [
        OpenCodeEvent::new(
            "message.updated",
            serde_json::json!({"info":{"id":message_id,"sessionID":session_id,"role":"assistant","time":{"created":chrono::Utc::now().timestamp_millis(),"completed":chrono::Utc::now().timestamp_millis()}}}),
        ),
        assistant_text_part_for_turn_response(session_id, turn_id, response_index, text),
    ]
}

/// 功能：发送已声明响应的完整 text part。
/// 参数：回合和响应序号用于生成与开始事件一致的 part id。
/// 返回：`message.part.updated` 事件。
/// 顺序：必须在 assistant completion marker 之前发送。
/// 关联：`EVT-HOST-MESSAGE-002`。
pub fn assistant_text_part_for_turn_response(
    session_id: &str,
    turn_id: Option<&str>,
    response_index: usize,
    text: &str,
) -> OpenCodeEvent {
    let message_id = assistant_message_id(session_id, turn_id, response_index);
    OpenCodeEvent::new(
        "message.part.updated",
        serde_json::json!({"part":{"id":part_id(session_id, turn_id, response_index, "text"),"messageID":message_id,"sessionID":session_id,"type":"text","text":text}}),
    )
}

/// 功能：生成默认回合的 text 或 reasoning part 开始事件。
/// 参数：`reasoning=true` 时 part 类型为 reasoning。
/// 返回：空文本的 part 声明事件。
/// 关联：`EVT-HOST-MESSAGE-002`。
#[allow(dead_code)]
pub fn text_part_started(session_id: &str, reasoning: bool) -> OpenCodeEvent {
    text_part_started_for_turn(session_id, None, reasoning)
}

/// 功能：生成指定回合的 text/reasoning part 开始事件。
/// 参数：`turn_id` 用于保持 part id 稳定。
/// 返回：空文本 part 声明。
/// 关联：`EVT-HOST-MESSAGE-002`。
pub fn text_part_started_for_turn(
    session_id: &str,
    turn_id: Option<&str>,
    reasoning: bool,
) -> OpenCodeEvent {
    text_part_started_for_turn_response(session_id, turn_id, 0, reasoning)
}

/// 功能：生成指定回合和响应序号的 part 开始事件。
/// 参数：`reasoning` 决定 reasoning 或 text 类型。
/// 返回：可供后续 delta 追加的空 part。
/// 关联：`EVT-HOST-MESSAGE-002`、`EVT-HOST-MESSAGE-003`。
pub fn text_part_started_for_turn_response(
    session_id: &str,
    turn_id: Option<&str>,
    response_index: usize,
    reasoning: bool,
) -> OpenCodeEvent {
    let kind = if reasoning { "reasoning" } else { "text" };
    OpenCodeEvent::new(
        "message.part.updated",
        serde_json::json!({"part":{"id":part_id(session_id, turn_id, response_index, kind),"messageID":assistant_message_id(session_id, turn_id, response_index),"sessionID":session_id,"type":kind,"text":""}}),
    )
}

/// 功能：生成默认回合的 assistant 流式完成标记。
/// 返回：携带 completed 时间的 `message.updated` 事件。
/// 状态：不改变 session 状态；idle 由调用方另行发送。
/// 关联：`EVT-HOST-MESSAGE-001`。
#[allow(dead_code)]
pub fn assistant_completed(session_id: &str, turn_id: Option<&str>) -> OpenCodeEvent {
    assistant_completed_for_turn_response(session_id, turn_id, 0)
}

/// 功能：生成指定回合和响应序号的 assistant 完成标记。
/// 返回：`message.updated` 事件。
/// 关联：`EVT-HOST-MESSAGE-001`。
pub fn assistant_completed_for_turn_response(
    session_id: &str,
    turn_id: Option<&str>,
    response_index: usize,
) -> OpenCodeEvent {
    let now = chrono::Utc::now().timestamp_millis();
    OpenCodeEvent::new(
        "message.updated",
        serde_json::json!({"info":{"id":assistant_message_id(session_id, turn_id, response_index),"sessionID":session_id,"role":"assistant","time":{"completed":now}}}),
    )
}

/// 功能：生成用户消息已写入的 message.updated 事件。
/// 参数：`session_id` 为会话编号，`message_id` 为已接受并持久化的用户消息编号。
/// 返回：带 created/completed 时间的用户消息事件。
/// 错误：无。
/// 状态：不修改会话。
/// 并发与取消：纯投影，无锁或取消点。
/// 权限：由 HTTP 接收路径执行鉴权。
/// 关联：`EVT-HOST-MESSAGE-001`。
pub fn user_message_updated(session_id: &str, message_id: &str) -> OpenCodeEvent {
    let now = chrono::Utc::now().timestamp_millis();
    OpenCodeEvent::new(
        "message.updated",
        serde_json::json!({"info":{"id":message_id,"sessionID":session_id,"role":"user","time":{"created":now,"completed":now}}}),
    )
}

/// 功能：发布已经持久化的用户输入正文。
/// 参数：session_id、message_id 为权威身份，text 为接收后的完整正文。
/// 返回：与 REST part ID 一致的 message.part.updated。
/// 错误：无。
/// 状态：纯投影，不写入会话或日志。
/// 并发与取消：接收提交后、后台执行前同步发布。
/// 权限：沿用 prompt 鉴权，不访问其他会话。
/// 关联：`CHG-2026-0022`、`EVT-HOST-MESSAGE-002`。
pub(crate) fn user_text_part(session_id: &str, message_id: &str, text: &str) -> OpenCodeEvent {
    OpenCodeEvent::new(
        "message.part.updated",
        serde_json::json!({"part":{
            "id":format!("prt_{message_id}_0"),"messageID":message_id,
            "sessionID":session_id,"type":"text","text":text
        }}),
    )
}

/// 功能：通知消费者移除已经提交删除的公共消息。
/// 参数：session_id 为会话编号，message_id 为提交前 REST 投影中的消息编号。
/// 返回：message.removed 事件。
/// 错误：无。
/// 状态：不修改持久化，调用方必须先完成删除事务。
/// 并发与取消：在回退提交闸门内、清除游标前发布。
/// 权限：仅携带不透明 ID。
/// 关联：`CHG-2026-0022`。
pub(crate) fn message_removed(session_id: &str, message_id: &str) -> OpenCodeEvent {
    OpenCodeEvent::new(
        "message.removed",
        serde_json::json!({"sessionID":session_id,"messageID":message_id}),
    )
}

/// 功能：将最终失败绑定到本回合 assistant，再通知消费者结束运行。
/// 参数：session_id、created、completed 为身份与时间，failure 为已过滤的错误。
/// 返回：依序为 assistant message.updated 与 session.error。
/// 错误：无。
/// 状态：调用方须先发布终态 snapshot；不修改模型消息。
/// 并发与取消：回合结束闸门内、idle 前同步发布；取消不调用本函数。
/// 权限：只包含已经过滤的公开错误。
/// 关联：`CHG-2026-0022`。
pub(crate) fn turn_failed(
    session_id: &str,
    created: i64,
    completed: i64,
    failure: &crate::protocol::OpenCodeTurnErrorV1,
) -> [OpenCodeEvent; 2] {
    [
        OpenCodeEvent::new(
            "message.updated",
            serde_json::json!({"info":{
                "id":failure.message_id,"sessionID":session_id,"role":"assistant",
                "time":{"created":created,"completed":completed},"error":failure.error
            }}),
        ),
        OpenCodeEvent::new(
            "session.error",
            serde_json::json!({"sessionID":session_id,"error":failure.error}),
        ),
    ]
}

/// 功能：读取 workspace todo JSONL 并生成 `todo.updated` 事件。
/// 参数：`workspace` 为工作区根目录。
/// 返回：格式正确的 todo 摘要；坏行被忽略。
/// 错误：文件不存在或无法解析时返回空 todo 列表，不影响回合。
/// 关联：`EVT-HOST-TOOL-001`。
pub fn todo_updated(session_id: &str, workspace: &Path) -> OpenCodeEvent {
    let todos = std::fs::read_to_string(workspace.join(".openpaw/todos.jsonl"))
        .ok()
        .map(|content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter_map(|todo| {
                    Some(serde_json::json!({
                        "id": todo.get("id")?.to_string().trim_matches('"').to_string(),
                        "content": todo.get("title")?.as_str()?,
                        "status": todo.get("status")?.as_str()?,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    OpenCodeEvent::new(
        "todo.updated",
        serde_json::json!({"sessionID":session_id,"todos":todos}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SessionTime, SessionWire};

    #[test]
    fn session_events_use_info_envelope() {
        let session = SessionWire {
            parent_id: None,
            id: "s1".into(),
            title: "Session".into(),
            slug: "s1".into(),
            directory: "/workspace".into(),
            time: SessionTime {
                created: 1,
                updated: 2,
                archived: None,
            },
            revert: None,
        };
        for kind in ["session.created", "session.updated"] {
            let event = session_changed(kind, &session);
            assert_eq!(event.event_type, kind);
            assert_eq!(event.properties["sessionID"], "s1");
            assert_eq!(event.properties["info"]["id"], "s1");
            assert!(event.properties.get("title").is_none());
        }
    }

    #[test]
    fn text_and_tool_events_use_v1_property_names() {
        let text = to_wire("s1", &AgentEvent::TextDelta { text: "hi".into() });
        let text = text.expect("text delta is mapped");
        assert_eq!(text.event_type, "message.part.delta");
        assert_eq!(text.properties["sessionID"], "s1");
        assert_eq!(text.properties["field"], "text");
        assert_eq!(text.properties["delta"], "hi");
        let tool = to_wire(
            "s1",
            &AgentEvent::ToolUseStart {
                tool_use_id: "call1".into(),
                name: "read_file".into(),
            },
        );
        let tool = tool.expect("tool start is mapped");
        assert_eq!(tool.event_type, "message.part.updated");
        assert_eq!(tool.properties["part"]["callID"], "call1");
        assert_eq!(tool.properties["part"]["state"]["status"], "running");
    }

    #[test]
    fn final_event_pair_makes_non_streaming_text_visible() {
        let [message, part] = assistant_final("s1", "done");
        assert_eq!(message.event_type, "message.updated");
        assert_eq!(message.properties["info"]["role"], "assistant");
        assert_eq!(part.event_type, "message.part.updated");
        assert_eq!(part.properties["part"]["text"], "done");
    }

    #[test]
    fn turn_ids_keep_stream_events_distinct() {
        let first = to_wire_for_turn("s1", Some("1"), &AgentEvent::TextDelta { text: "a".into() })
            .expect("first");
        let second = to_wire_for_turn("s1", Some("2"), &AgentEvent::TextDelta { text: "b".into() })
            .expect("second");
        assert_ne!(first.properties["partID"], second.properties["partID"]);
        assert_ne!(
            first.properties["messageID"],
            second.properties["messageID"]
        );
    }

    #[test]
    fn response_index_keeps_tool_loop_messages_distinct() {
        let first = to_wire_for_turn_response("s1", Some("turn"), 0, &AgentEvent::MessageStart)
            .expect("first");
        let second = to_wire_for_turn_response("s1", Some("turn"), 1, &AgentEvent::MessageStart)
            .expect("second");
        assert_ne!(
            first.properties["info"]["id"],
            second.properties["info"]["id"]
        );
        let second_delta = to_wire_for_turn_response(
            "s1",
            Some("turn"),
            1,
            &AgentEvent::TextDelta { text: "b".into() },
        )
        .expect("second delta");
        assert_eq!(
            second_delta.properties["messageID"],
            second.properties["info"]["id"]
        );
        assert!(second_delta.properties["partID"]
            .as_str()
            .expect("part id")
            .ends_with("_1"));
    }

    #[test]
    fn internal_events_are_not_emitted_as_opencode_events() {
        assert!(to_wire(
            "s1",
            &AgentEvent::CompactionPerformed {
                before_tokens: 10,
                after_tokens: 5,
            }
        )
        .is_none());
        assert!(to_wire(
            "s1",
            &AgentEvent::ClassifierStatus {
                tool_use_id: "call".into(),
                status: openpaw_engine::ClassifierStatus::AutoDenied {
                    reason: "policy".into(),
                },
            }
        )
        .is_none());
        assert!(to_wire(
            "s1",
            &AgentEvent::TurnUsage {
                usage: openpaw_core::Usage::default(),
            }
        )
        .is_none());
    }
}
