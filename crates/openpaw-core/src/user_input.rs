//! 功能：定义与界面、网络和存储无关的单题问答事实。Stability: unstable.
//! 输入：可信入口提供实际会话及调用归属，Engine校验问题和明确答案。
//! 输出：同一中立记录供Host唯一writer保存、可靠恢复和只读历史投影。
//! 边界：不展示卡片、不执行网络或存储，不把答案、历史或暂停事实作为工具授权。
//! 关联：CHG-2026-0054；Engine user_input协调、Host question适配与context可靠投影。
use serde::{Deserialize, Serialize};

/// 功能：用户可选择的普通答案。Stability: unstable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionOption {
    /// 功能：该题内稳定选项身份，custom为保留值。
    pub id: String,
    /// 功能：选项的可读标签。
    pub label: String,
    /// 功能：可选简短说明。
    #[serde(default)]
    pub description: String,
}

/// 功能：一次提问的单题内容。Stability: unstable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionRequest {
    /// 功能：待用户回答的问题。
    pub question: String,
    /// 功能：二至八个互斥普通选项。
    pub options: Vec<UserQuestionOption>,
    /// 功能：可选提问背景。
    #[serde(default)]
    pub description: String,
}

/// 功能：经过明确提交的普通或自定义答案。Stability: unstable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionAnswer {
    /// 功能：普通选项身份，或保留值custom。
    pub choice_id: String,
    /// 功能：实际选项标签，自定义答案固定为其他。
    pub label: String,
    /// 功能：仅custom答案允许携带的文字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_text: Option<String>,
}

/// 功能：问题的逻辑状态，不是界面的展示进度。Stability: unstable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserQuestionState {
    /// 功能：已保存，尚未轮到展示。
    Queued,
    /// 功能：正在等待原期限内的明确回答。
    Waiting,
    /// 功能：答案已可靠接受。
    Answered,
    /// 功能：原作答期限已结束。
    Expired,
    /// 功能：所属执行被明确取消。
    Cancelled,
    /// 功能：展示、保存或服务中断，未取得有效答案。
    Interrupted,
}

/// 功能：可信执行入口确定的问题归属。Stability: unstable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserQuestionScope {
    /// 功能：负责向用户展示该题的主会话。
    pub parent_session_id: String,
    /// 功能：实际发起问题的会话。
    pub asking_session_id: String,
    /// 功能：触发该执行的原用户消息身份。
    pub source_user_message_id: String,
}

/// 功能：可核对的完整问题事实，不包含网络凭据。Stability: unstable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserQuestionRecord {
    /// 功能：逻辑问题的稳定身份。
    pub question_id: String,
    /// 功能：真实归属。
    pub scope: UserQuestionScope,
    /// 功能：原执行回合。
    pub turn_id: String,
    /// 功能：原模型响应序号。
    pub response_index: usize,
    /// 功能：原工具调用身份。
    pub tool_call_id: String,
    /// 功能：问题及可选答案。
    pub request: UserQuestionRequest,
    /// 功能：当前逻辑状态。
    pub state: UserQuestionState,
    /// 功能：展示代次，不重置逻辑题。
    pub generation: u64,
    /// 功能：本代展示身份。
    pub display_id: String,
    /// 功能：轮到展示时确定的原截止时间，Unix毫秒。
    pub deadline_ms: Option<i64>,
    /// 功能：可靠接受后才出现的明确答案。
    pub answer: Option<UserQuestionAnswer>,
    /// 功能：未取得答案时的脱敏结束原因。
    pub reason: Option<String>,
}
