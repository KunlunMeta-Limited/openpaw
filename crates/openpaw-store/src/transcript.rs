//! 功能：共享持久会话记录 codec 和纯历史重放。
//! 参数：JSONL 事件快照。
//! 返回：已提交历史和变更投影。
//! 错误：已登记版本损坏时返回脱敏错误。
//! 状态：不写盘，不恢复活动回合。
//! 并发与取消：对稳定切片同步重放。
//! 权限：不访问模型、工具或传输。
//! 关联：Host 和 CLI 读取。
use openpaw_core::{ContentBlock, Message, Role, SessionEvent};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// 功能：表示已登记持久记录的结构或提交一致性错误。
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("invalid committed transcript record")]
pub struct ProjectionError;

/// 功能：编码稳定 type 的持久 Unknown 记录。
/// 参数：可序列化对象和已登记类型名。
/// 返回：SessionEvent::Unknown。
/// 错误：异常或非对象返回仅 type 的最小记录，由读取一致性校验拒绝。
/// 状态：纯编码，不写盘。
/// 并发与取消：同步无锁。
/// 权限：只消费调用方已获授权的事件值。
/// 关联：Host 持久写入 adapter。
pub fn encode(value: &impl Serialize, kind: &str) -> SessionEvent {
    let mut value = serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value
        .as_object_mut()
        .unwrap()
        .insert("type".into(), serde_json::json!(kind));
    SessionEvent::Unknown(Box::new(value))
}
/// 功能：解码指定版本的持久记录。
/// 参数：event 和预期 type。
/// 返回：匹配返回 Some；其他类型返回 None。
/// 错误：已匹配的结构损坏返回 ProjectionError，不回显正文。
/// 状态：纯解码。
/// 并发与取消：同步无锁。
/// 权限：不读取记录之外的资源。
/// 关联：Host 类型 adapter 和 Store replay。
pub fn decode<T: DeserializeOwned>(
    event: &SessionEvent,
    kind: &str,
) -> Result<Option<T>, ProjectionError> {
    let SessionEvent::Unknown(value) = event else {
        return Ok(None);
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some(kind) {
        return Ok(None);
    }
    serde_json::from_value((**value).clone())
        .map(Some)
        .map_err(|_| ProjectionError)
}
fn validate_record(event: &SessionEvent) -> Result<(), ProjectionError> {
    decode::<crate::execution_attempts::ExecutionAttemptV1>(event, "execution_attempt_v1")?;
    decode::<TurnAcceptV1>(event, "turn_accept_v1")?;
    decode::<MessagePresentationRecordV1>(event, "message_presentation_v1")?;
    decode::<TurnFinishPrepareV1>(event, "turn_finish_prepare_v1")?;
    decode::<TurnFinishCommitV1>(event, "turn_finish_commit_v1")?;
    decode::<TurnOutcome>(event, "turn_outcome")?;
    decode::<RequestAdmission>(event, "request_admission")?;
    decode::<SessionMetadataV1>(event, "session_metadata_v1")?;
    decode::<SessionForkOriginV1>(event, "session_fork_origin_v1")?;
    decode::<SessionRevertStagedV1>(event, "session_revert_staged_v1")?;
    decode::<SessionRevertCommittedV1>(event, "session_revert_committed_v1")?;
    Ok(())
}
/// 功能：表示一个回合在 Host 调度和执行状态机中的稳定状态。
/// 变体语义：状态只允许 queued、running、completed、failed、cancelled 单向推进。
/// 数据来源：HTTP 接收、调度器、AgentLoop 结果和 Host 重启恢复。
/// 数据去向：session JSONL `turn_outcome` 未知事件和 `GET /session/:id/turns`。
/// 约束：终态不得回退为活动态；失败状态不作为模型消息。
/// 稳定性：跨 Gateway 公共协议，枚举值受 Schema 固定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnState {
    /// 回合已接受，正在等待全局 Provider 容量。
    Queued,
    /// 回合已取得全局 permit，正在执行 AgentLoop。
    Running,
    /// 回合成功结束并完成消息持久化。
    Completed,
    /// 回合因脱敏错误结束。
    Failed,
    /// 回合在排队或运行期间被用户取消。
    Cancelled,
}

impl TurnState {
    /// 功能：判断状态是否为不可继续推进的终态。
    /// 返回：completed、failed、cancelled 返回 true，其余返回 false。
    /// 状态：纯只读，不改变回合。
    /// 关联：`CHG-2026-0012`。
    /// 参数：见签名或字段类型；只消费调用方提供的数据。
    /// 错误：不适用时无；配置或读取错误由调用者处理，取消不表示批准。
    /// 并发与取消：同步操作不等待外部 I/O；异步等待取消按当前模块生命周期清理。
    /// 权限：不扩大工具或文件权限，不记录认证信息。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// 功能：向 Gateway 暴露脱敏后的回合失败信息。
/// 字段语义：`code` 为稳定机器码，`message` 为安全中文说明，`retryable` 表示可否重试。
/// 数据来源：Host 对 `openpaw_core::Error`、取消和重启异常的分类。
/// 数据去向：JSONL 与 turns REST；原始 Provider 错误不得进入本类型。
/// 约束：只允许 CHG 登记的错误码，不含凭据、Prompt、路径或上游响应正文。
/// 稳定性：跨 Gateway 公共协议。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnError {
    /// 功能：保存稳定错误分类码。
    pub code: String,
    /// 功能：保存可直接展示的脱敏中文失败说明。
    pub message: String,
    /// 功能：指示相同输入稍后重试是否可能成功。
    pub retryable: bool,
}

/// 功能：表示 Gateway 可查询的单回合状态快照。
/// 字段语义：ID 使用固定 `turnID/userMessageID`；时间均为 Unix 毫秒且按状态可选。
/// 数据来源：runtime 状态机和 session JSONL 的 `turn_outcome` 未知事件。
/// 数据去向：`GET /session/:id/turns` 和 Host 重启恢复。
/// 约束：序列化字段严格为 camelCase，`turnID/userMessageID` 保留大写 ID；error 仅在失败/取消时出现。
/// 稳定性：跨 Gateway 公共协议，字段删除或改名必须升版。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutcome {
    /// 功能：保存回合稳定编号。
    #[serde(rename = "turnID")]
    pub turn_id: String,
    /// 功能：保存触发本回合的用户消息编号。
    #[serde(rename = "userMessageID")]
    pub user_message_id: String,
    /// 功能：保存当前或最终回合状态。
    pub state: TurnState,
    /// 功能：保存进入全局等待队列的时间；立即运行时省略。
    #[serde(rename = "queuedAt", skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<i64>,
    /// 功能：保存取得 Provider permit 的时间。
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// 功能：保存 completed、failed 或 cancelled 的结束时间。
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    /// 功能：保存脱敏错误；成功和活动状态省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
}

impl TurnOutcome {
    /// 功能：把回合快照包装为前向兼容的 `SessionEvent::Unknown` JSON。
    /// 返回：带 `type=turn_outcome` 的未知事件，不进入模型消息上下文。
    /// 错误：本类型字段均可序列化；异常时返回最小空对象供写入层报错。
    /// 状态：不修改当前快照。
    /// 关联：`CHG-2026-0012`。
    /// 参数：见签名或字段类型；只消费调用方提供的数据。
    /// 并发与取消：同步操作不等待外部 I/O；异步等待取消按当前模块生命周期清理。
    /// 权限：不扩大工具或文件权限，不记录认证信息。
    pub fn to_session_event(&self) -> openpaw_core::SessionEvent {
        encode(self, "turn_outcome")
    }

    /// 功能：从未知 JSONL 事件解析已登记的回合快照。
    /// 参数：`event` 为 SessionReader 返回的事件。
    /// 返回：仅 `type=turn_outcome` 且字段合法时返回快照，否则返回 None。
    /// 错误：未知事件和旧版本附加事件被安全忽略。
    /// 状态：纯解析，不修改 JSONL。
    /// 关联：`CHG-2026-0012`。
    /// 并发与取消：同步操作不等待外部 I/O；异步等待取消按当前模块生命周期清理。
    /// 权限：不扩大工具或文件权限，不记录认证信息。
    pub fn from_session_event(event: &openpaw_core::SessionEvent) -> Option<Self> {
        decode(event, "turn_outcome").ok().flatten()
    }
}

/// 功能：持久记录一次已接收消息请求与稳定回合身份的对应关系。
/// 字段语义：`request_id` 在单 session 内唯一，`fingerprint` 只保存规范化请求的摘要。
/// 数据来源：HTTP message/prompt 接收路径。
/// 数据去向：session JSONL `request_admission` 未知事件和运行时幂等索引。
/// 约束：不得包含原始 Prompt、认证信息、路径或时间；同键不同指纹必须冲突。
/// 稳定性：向后兼容的 JSONL 内部事件；旧版 reader 可忽略未知类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAdmission {
    /// 客户端提供的幂等键。
    #[serde(rename = "requestID")]
    pub request_id: String,
    /// 规范化正文、provider、model 和协议版本的确定性 256-bit 十六进制摘要。
    pub fingerprint: String,
    /// Host 首次接收时生成的稳定回合编号。
    #[serde(rename = "turnID")]
    pub turn_id: String,
    /// Host 首次接收时生成的稳定用户消息编号。
    #[serde(rename = "userMessageID")]
    pub user_message_id: String,
}

impl RequestAdmission {
    /// 功能：从 session 未知事件恢复持久幂等接收事实。
    /// 参数：`event` 为 SessionReader 读取的单条事件。
    /// 返回：合法 `request_admission` 返回记录，其他事件返回 None。
    /// 错误：字段缺失、类型错误或未知版本安全返回 None，不修改原文件。
    /// 状态：纯解析，不更新运行时索引。
    /// 并发与取消：纯同步解析，无锁且不可取消。
    /// 权限：不记录或返回事件之外的文件路径和正文。
    /// 关联：`CHG-2026-0013`、`DAT-HOST-IDEMPOTENCY-003`。
    pub fn from_session_event(event: &openpaw_core::SessionEvent) -> Option<Self> {
        decode(event, "request_admission").ok().flatten()
    }
}

/// 功能：把用户消息、首个回合状态和可选幂等事实封装为一次崩溃原子的接收记录。
/// 字段语义：单条 JSONL 行即接收提交点，避免多次 fsync 之间出现半接收回合。
/// 数据来源：Host 完成同 session 幂等检查、回合保留和全局容量接收之后。
/// 数据去向：session JSONL `turn_accept_v1` 未知事件和重启恢复投影。
/// 约束：message 必须是带 UUID 的 user，且 UUID 必须与 outcome/admission 的 userMessageID 一致。
/// 稳定性：Host 私有持久化事件；旧 reader 可忽略未知类型，新 reader 同时兼容旧三事件格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnAcceptV1 {
    /// 功能：保存本轮原始用户消息；不包含 Provider 回复。
    pub message: openpaw_core::Message,
    /// 功能：保存 queued 或 running 首状态。
    pub outcome: TurnOutcome,
    /// 功能：保存可选 requestID 幂等事实。
    #[serde(
        rename = "requestAdmission",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_admission: Option<RequestAdmission>,
    /// 功能：随输入原子保存展示注解；旧记录缺省无注解。
    #[serde(
        rename = "messagePresentation",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub message_presentation: Option<MessagePresentationV1>,
}

/// 功能：保存消息内容块的中立展示属性，不参与模型输入。
/// 字段语义：version 固定为 1，parts 按内容块编号绑定；metadata 不解释客户端业务键。
/// 关联：CHG-2026-0027；不包含媒体原件副本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePresentationV1 {
    /// 功能：展示注解版本，当前仅支持 1。
    pub version: u8,
    /// 功能：具有展示属性的文本块；编号不得重复。
    pub parts: Vec<PartPresentationV1>,
}

/// 功能：把辅助标志与不透明元数据绑定到同一消息中的文本块。
/// 关联：CHG-2026-0027；块编号以完整原始消息为准。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartPresentationV1 {
    /// 功能：原始 Message.content 的零基编号。
    #[serde(rename = "blockIndex")]
    pub block_index: usize,
    /// 功能：标记自动生成的辅助正文，正文仍供模型使用。
    pub auxiliary: bool,
    /// 功能：保存客户端元数据对象，不访问其中的 URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// 功能：为分支的独立 Message 记录保存展示属性。
/// 关联：CHG-2026-0027；与分支文件整体发布，普通接收使用 TurnAcceptV1。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePresentationRecordV1 {
    /// 功能：属性所属的已发布消息编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
    /// 功能：该消息的中立展示属性。
    pub presentation: MessagePresentationV1,
}

impl MessagePresentationV1 {
    /// 功能：校验注解版本、文本块身份与唯一性。
    /// 参数：message 为属性所属的完整原始输入。
    /// 返回：合法时返回 Ok。
    /// 错误：版本、角色、块编号或重复编号非法时返回 ProjectionError。
    /// 状态：只读，不写历史。
    /// 并发与取消：同步无共享状态。
    /// 权限：不解析元数据业务键，不访问网络或文件。
    /// 关联：CHG-2026-0027；接收与恢复共同校验。
    pub fn validate(&self, message: &Message) -> Result<(), ProjectionError> {
        let mut indices = HashSet::new();
        if self.version != 1 || message.role != openpaw_core::Role::User {
            return Err(ProjectionError);
        }
        for part in &self.parts {
            if !indices.insert(part.block_index)
                || !matches!(
                    message.content.get(part.block_index),
                    Some(ContentBlock::Text { .. })
                )
            {
                return Err(ProjectionError);
            }
        }
        Ok(())
    }
}

impl TurnAcceptV1 {
    /// 功能：把接收事务编码为单条前向兼容 session 未知事件。
    /// 参数：无，使用当前消息、首状态和幂等事实。
    /// 返回：`type=turn_accept_v1` 的 `SessionEvent::Unknown`。
    /// 错误：字段序列化失败时返回最小对象并由写入层报告；正常字段均可序列化。
    /// 状态：纯转换，不修改内存投影或 session 文件。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：只包含本轮用户消息和脱敏状态，不包含凭据、路径或 Provider 错误正文。
    /// 关联：`CHG-2026-0013`、`DAT-HOST-TURN-001`。
    pub fn to_session_event(&self) -> openpaw_core::SessionEvent {
        encode(self, "turn_accept_v1")
    }

    /// 功能：从 session 未知事件解析崩溃原子的回合接收记录。
    /// 参数：`event` 为 SessionReader 返回的单条事件。
    /// 返回：类型和字段均合法时返回记录，其他事件返回 None。
    /// 错误：结构错误由恢复状态机归类；本解析器不修改文件。
    /// 状态：纯解析，不更新消息、outcome 或 requestID 索引。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：只读取事件自身内容，不记录正文或文件路径。
    /// 关联：`CHG-2026-0013`、`DAT-HOST-TURN-001`。
    pub fn from_session_event(event: &openpaw_core::SessionEvent) -> Option<Self> {
        decode(event, "turn_accept_v1").ok().flatten()
    }
}

/// 功能：标记一次生成消息批次的开始，使 commit 之前的 Message 在恢复时保持不可见。
/// 字段语义：`turn_id` 关联当前回合；正文仍逐条写入既有 Message 事件。
/// 数据来源：AgentLoop 已完成且生成消息 UUID 已固定之后。
/// 数据去向：session JSONL `turn_finish_prepare_v1` 未知事件。
/// 约束：一个 prepare 只允许由同 turn 的合法 commit 闭合。
/// 稳定性：Host 私有持久化事件，旧 reader 可忽略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnFinishPrepareV1 {
    /// 功能：保存当前生成批次所属的稳定回合编号。
    #[serde(rename = "turnID")]
    pub turn_id: String,
}

impl TurnFinishPrepareV1 {
    /// 功能：把生成批次起点编码为 session 未知事件。
    /// 参数：无。
    /// 返回：`type=turn_finish_prepare_v1` 的未知事件。
    /// 错误：字符串字段可序列化；异常时由写入层报告。
    /// 状态：不写文件、不修改回合。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：只包含不透明 turnID。
    /// 关联：`CHG-2026-0013`。
    pub fn to_session_event(&self) -> openpaw_core::SessionEvent {
        encode(self, "turn_finish_prepare_v1")
    }

    /// 功能：解析生成消息事务的 prepare 事件。
    /// 参数：`event` 为 SessionReader 读取的事件。
    /// 返回：合法 prepare 返回记录，其他事件返回 None。
    /// 错误：字段损坏返回 None，由恢复状态机按普通未知事件处理。
    /// 状态：纯解析。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：不接触消息正文。
    /// 关联：`CHG-2026-0013`。
    pub fn from_session_event(event: &openpaw_core::SessionEvent) -> Option<Self> {
        decode(event, "turn_finish_prepare_v1").ok().flatten()
    }
}

/// 功能：提交一批逐条持久化的生成消息及其唯一终态。
/// 字段语义：只保存有界 message ID 清单和脱敏 terminal，不复制长回复正文。
/// 数据来源：prepare 后全部 Message 已成功 fsync 的回合结束路径。
/// 数据去向：session JSONL `turn_finish_commit_v1` 和重启恢复状态机。
/// 约束：message ID 顺序必须与 prepare 区间中的 Message 完全一致，terminal 必须为终态。
/// 稳定性：Host 私有持久化事件，旧 reader 可忽略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnFinishCommitV1 {
    /// 功能：保存当前生成批次所属的稳定回合编号。
    #[serde(rename = "turnID")]
    pub turn_id: String,
    /// 功能：按 JSONL 顺序保存本批次生成消息 UUID；不保存正文。
    #[serde(rename = "generatedMessageIDs")]
    pub generated_message_ids: Vec<String>,
    /// 功能：保存与消息批次同时生效的 completed 或 failed 终态。
    pub terminal: TurnOutcome,
    /// 功能：保存与 failed 终态原子提交的安全 OpenCode 错误；旧记录缺省为空。
    #[serde(
        rename = "openCodeError",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub open_code_error: Option<RecordedFailure>,
}

/// 功能：记录失败回合的展示消息身份及安全错误，不作为模型消息。
/// 字段语义：messageID 与实时 assistant header 一致，error 仅含已过滤的公开详情。
/// 数据来源：Host 最终失败投影；数据去向：私有 finish commit、SSE 与 REST。
/// 约束：只能随同一回合 failed 终态提交，禁止携带原始 Provider 响应。
/// 稳定性：Host 私有可选字段，旧 reader 可忽略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedFailure {
    /// 功能：保存该回合稳定的 assistant 展示消息编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
    /// 功能：保存实时事件与历史查询共用的安全错误。
    pub error: SafeError,
}

/// 功能：表示已有 OpenCode 消费者支持的错误 envelope。
/// 字段语义：name 为 APIError 或 UnknownError，data 为安全详情。
/// 数据来源：Host 错误分类；数据去向：session.error 与 assistant info.error。
/// 约束：不包含响应头、原始响应体、凭据或请求文本。
/// 稳定性：OpenCode v1 兼容子集，不改变公共 turns 合同。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeError {
    /// 功能：保存消费者识别的错误名称。
    pub name: String,
    /// 功能：保存有界且过滤后的错误详情。
    pub data: SafeErrorData,
}

/// 功能：保存可公开的失败说明及可靠可知的 API 元数据。
/// 字段语义：message/code/isRetryable 必填，状态码与 provider 无事实依据时省略。
/// 数据来源：稳定 TurnError 与过滤后的 HTTP 错误摘要。
/// 数据去向：Host 私有 JSONL、OpenCode SSE/REST；不进入模型上下文。
/// 约束：上游说明最多 300 字符，禁止 raw body、headers 或 URL。
/// 稳定性：OpenCode v1 可选详情，不修改 TurnError。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeErrorData {
    /// 功能：保存安全分类说明与允许公开的上游摘要。
    pub message: String,
    /// 功能：保存稳定的 Host 错误分类码。
    pub code: String,
    /// 功能：保留是否可能重试成功的原有判断。
    pub is_retryable: bool,
    /// 功能：保存确知的上游 HTTP 状态码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// 功能：保存实际执行 provider 的稳定名称。
    #[serde(
        rename = "providerID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_id: Option<String>,
}

impl TurnFinishCommitV1 {
    /// 功能：把生成批次提交点编码为 session 未知事件。
    /// 参数：无。
    /// 返回：`type=turn_finish_commit_v1` 的未知事件。
    /// 错误：字段序列化异常由写入层报告。
    /// 状态：纯转换，不修改文件或内存投影。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：不包含生成消息正文、工具输出副本或 Provider 原始错误。
    /// 关联：`CHG-2026-0013`。
    pub fn to_session_event(&self) -> openpaw_core::SessionEvent {
        encode(self, "turn_finish_commit_v1")
    }

    /// 功能：解析生成消息事务的 commit 事件。
    /// 参数：`event` 为 SessionReader 读取的事件。
    /// 返回：字段可反序列化时返回记录，事务一致性由恢复状态机继续校验。
    /// 错误：未知或结构损坏事件返回 None，不直接投影消息。
    /// 状态：纯解析。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：不读取消息正文。
    /// 关联：`CHG-2026-0013`。
    pub fn from_session_event(event: &openpaw_core::SessionEvent) -> Option<Self> {
        decode(event, "turn_finish_commit_v1").ok().flatten()
    }
}

/// 功能：保存一次 PATCH 提供的会话标题与归档字段。
/// 数据边界：缺省字段保持旧投影；`archived=0` 表示取消归档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadataV1 {
    /// 功能：保存本次请求提供的新标题；省略时不修改标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 功能：保存归档 Unix 毫秒；零表示取消归档，省略表示不修改。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<i64>,
}

/// 功能：保存用户分支的直接来源和在该来源下的序号。
/// 数据边界：只用于恢复标题编号和诊断，不投影为公共 `parentID`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForkOriginV1 {
    /// 功能：保存直接源会话编号。
    #[serde(rename = "sourceSessionID")]
    pub source_session_id: String,
    /// 功能：保存从一开始的直接分支序号。
    pub ordinal: u64,
}

/// 功能：保存暂存回退的首条隐藏消息编号。
/// 数据边界：边界消息本身及其尾部均不可见。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRevertStagedV1 {
    /// 功能：保存第一条被回退消息的线协议编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
}

/// 功能：把逻辑裁剪和替换 prompt 接收保存为一个崩溃原子的提交点。
/// 数据边界：旧尾部仍留在 JSONL 中，但恢复投影不再暴露。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRevertCommittedV1 {
    /// 功能：保存第一条永久逻辑删除消息的线协议编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
    /// 功能：保存与裁剪同时生效的新回合接收事务。
    pub accept: TurnAcceptV1,
}

/// 功能：表示 Host 能识别的私有会话变更事件。
#[derive(Debug, Clone)]
pub enum SessionMutationEvent {
    /// 会话标题或归档状态更新。
    Metadata(SessionMetadataV1),
    /// 用户分支来源。
    ForkOrigin(SessionForkOriginV1),
    /// 暂存回退游标。
    RevertStaged(SessionRevertStagedV1),
    /// 清除暂存回退游标。
    RevertCleared,
    /// 永久提交回退并接收替换 prompt；装箱避免大载荷扩大所有枚举值。
    RevertCommitted(Box<SessionRevertCommittedV1>),
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
    pub fn to_session_event(&self) -> SessionEvent {
        let (kind, mut value) = match self {
            Self::Metadata(value) => ("session_metadata_v1", json_value(value)),
            Self::ForkOrigin(value) => ("session_fork_origin_v1", json_value(value)),
            Self::RevertStaged(value) => ("session_revert_staged_v1", json_value(value)),
            Self::RevertCleared => ("session_revert_cleared_v1", serde_json::json!({})),
            Self::RevertCommitted(value) => ("session_revert_committed_v1", json_value(value)),
        };
        if let Some(object) = value.as_object_mut() {
            object.insert("type".into(), serde_json::json!(kind));
        }
        SessionEvent::Unknown(Box::new(value))
    }

    /// 功能：从未知 JSONL 事件解析已登记的会话变更版本。
    /// 参数：`event` 为 SessionReader 按顺序读取的单个事件。
    /// 返回：类型和字段合法时返回事件；其他事件返回 None。
    /// 错误：结构损坏或未知版本安全返回 None，不修改原文件。
    /// 状态：纯解析，不更新内存投影。
    /// 并发与取消：同步无锁，不可取消。
    /// 权限：只读取事件自身，不访问工作区或凭据。
    /// 关联：`DAT-HOST-SESSION-MUTATION-*`。
    pub fn from_session_event(event: &SessionEvent) -> Option<Self> {
        let SessionEvent::Unknown(value) = event else {
            return None;
        };
        let kind = value.get("type")?.as_str()?;
        let value = (**value).clone();
        match kind {
            "session_metadata_v1" => serde_json::from_value(value).ok().map(Self::Metadata),
            "session_fork_origin_v1" => serde_json::from_value(value).ok().map(Self::ForkOrigin),
            "session_revert_staged_v1" => {
                serde_json::from_value(value).ok().map(Self::RevertStaged)
            }
            "session_revert_cleared_v1" => Some(Self::RevertCleared),
            "session_revert_committed_v1" => serde_json::from_value(value)
                .ok()
                .map(Self::RevertCommitted),
            _ => None,
        }
    }
}

/// 功能：保存按 JSONL 顺序折叠后的会话变更附加状态。
#[derive(Debug, Clone, Default)]
pub struct SessionMutationProjection {
    /// 最后一次显式标题更新。
    /// 功能：解析显式标题或首个持久 user 文本的摘要。
    pub title: Option<String>,
    /// 正数 Unix 毫秒归档时间；未归档为 None。
    pub archived: Option<i64>,
    /// 当前会话自身的直接分支来源。
    pub fork_origin: Option<SessionForkOriginV1>,
    /// 第一条暂存隐藏消息编号。
    pub revert_message_id: Option<String>,
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
    pub fn apply(&mut self, event: SessionMutationEvent) {
        match event {
            SessionMutationEvent::Metadata(metadata) => {
                if let Some(title) = metadata.title {
                    self.title = Some(title);
                }
                if let Some(archived) = metadata.archived {
                    self.archived = (archived > 0).then_some(archived);
                }
            }
            SessionMutationEvent::ForkOrigin(origin) => self.fork_origin = Some(origin),
            SessionMutationEvent::RevertStaged(revert) => {
                self.revert_message_id = Some(revert.message_id)
            }
            SessionMutationEvent::RevertCleared | SessionMutationEvent::RevertCommitted(_) => {
                self.revert_message_id = None
            }
        }
    }
}

/// 功能：返回消息在 Host REST 响应中的稳定编号。
/// 参数：`session_id`、零基消息序号和消息值。
/// 返回：优先使用持久 UUID，否则使用兼容 `msg_<session>_<index>` 编号。
/// 错误：无。
/// 状态：纯计算。
/// 并发与取消：同步无锁，不可取消。
/// 权限：不读取消息正文之外的资源。
/// 关联：`API-HOST-SESSION-F03`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
pub fn wire_message_id(session_id: &str, index: usize, message: &Message) -> String {
    message
        .uuid
        .clone()
        .unwrap_or_else(|| format!("msg_{session_id}_{index}"))
}

/// 功能：解析严格早于指定消息边界的可见前缀长度。
/// 参数：`session_id` 用于旧消息兼容编号，`messages` 为当前完整或可见列表，`message_id` 为边界。
/// 返回：命中边界返回其零基位置，未知边界返回 None。
/// 错误：无。
/// 状态：纯读取，不修改消息。
/// 并发与取消：调用方提供稳定切片，同步不可取消。
/// 权限：不访问文件或网络。
/// 关联：`API-HOST-SESSION-FORK-001`、`API-HOST-SESSION-REVERT-001`。
pub fn visible_prefix_len(
    session_id: &str,
    messages: &[Message],
    message_id: &str,
) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .position(|(index, message)| wire_message_id(session_id, index, message) == message_id)
}

fn json_value(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!({}))
}

/// 功能：保存完整已提交历史与附加持久状态。
pub struct Transcript {
    /// 功能：仅含已提交来源的模型检查点，适用性由执行入口校验。
    pub model_contexts: Vec<openpaw_core::ModelContextCheckpoint>,
    /// 实际执行尝试，不作为有效模型上下文；Revert 只影响可见投影。
    pub execution_attempts: Vec<crate::execution_attempts::ExecutionAttemptV1>,
    /// 完整已提交消息，staged revert 仅裁剪显示前缀。
    pub messages: Vec<openpaw_core::Message>,
    /// 按首次出现顺序保存的持久回合最新状态。
    pub turn_outcomes: Vec<TurnOutcome>,
    /// 不作为模型消息的安全失败详情。
    pub turn_errors: HashMap<String, RecordedFailure>,
    /// 单会话持久幂等接收事实。
    pub request_admissions: HashMap<String, RequestAdmission>,
    /// 功能：完整消息身份对应的展示属性，不作为模型内容。
    pub message_presentations: HashMap<String, MessagePresentationV1>,
    /// 标题、归档、分支和回退附加状态。
    pub mutation: SessionMutationProjection,
    /// 已读回合编号的高水位，回退不会降低它。
    pub turn_sequence_high_water: u64,
}
struct PendingFinishTransaction {
    context: Option<openpaw_core::ModelContextCheckpoint>,
    turn_id: String,
    messages: Vec<openpaw_core::Message>,
}
/// 功能：限制单个 finish 事务批次的消息数量。
pub const MAX_FINISH_TRANSACTION_MESSAGES: usize = 4096;
/// 功能：重放唯一已提交历史与持久附加状态。
/// 参数：session_id 和稳定事件切片。
/// 返回：Transcript。
/// 错误：已登记记录损坏、提交错配或回退边界非法时失败。
/// 状态：忽略未提交批次，不恢复活动回合。
/// 并发与取消：同步无共享锁；保留 4096 消息事务上限。
/// 权限：无网络、工具、writer 或传输依赖。
/// 关联：Host 恢复/摘要和 CLI catalog。
pub fn replay(session_id: &str, events: &[SessionEvent]) -> Result<Transcript, ProjectionError> {
    let mut projection = Transcript {
        model_contexts: Vec::new(),
        execution_attempts: Vec::new(),
        messages: Vec::new(),
        turn_outcomes: Vec::new(),
        turn_errors: HashMap::new(),
        request_admissions: HashMap::new(),
        message_presentations: HashMap::new(),
        mutation: SessionMutationProjection::default(),
        turn_sequence_high_water: 0,
    };
    let mut message_ids = HashSet::new();
    let mut pending_finish: Option<PendingFinishTransaction> = None;
    for event in events {
        validate_record(event)?;
        if let SessionEvent::Metadata {
            title: Some(title), ..
        } = event
        {
            projection.mutation.title = Some(title.clone());
        }
        if let Some(mutation) = SessionMutationEvent::from_session_event(event) {
            pending_finish = None;
            match mutation {
                SessionMutationEvent::RevertCommitted(commit) => {
                    apply_revert_commit(
                        session_id,
                        &mut projection,
                        &mut message_ids,
                        (*commit).clone(),
                    )?;
                    projection
                        .mutation
                        .apply(SessionMutationEvent::RevertCommitted(commit));
                }
                mutation => projection.mutation.apply(mutation),
            }
            continue;
        }
        if let Some(prepare) = TurnFinishPrepareV1::from_session_event(event) {
            pending_finish = Some(PendingFinishTransaction {
                context: None,
                turn_id: prepare.turn_id,
                messages: Vec::new(),
            });
            continue;
        }
        if let Some(pending) = pending_finish.as_mut() {
            if let Some(context) =
                decode::<openpaw_core::ModelContextCheckpoint>(event, "model_context_checkpoint_v1")
                    .ok()
                    .flatten()
            {
                pending.context = Some(context);
                continue;
            }
            if let SessionEvent::Message(message) = event {
                if pending.messages.len() >= MAX_FINISH_TRANSACTION_MESSAGES {
                    return Err(ProjectionError);
                }
                pending.messages.push(message.clone());
                continue;
            }
            if let Some(commit) = TurnFinishCommitV1::from_session_event(event) {
                let mut pending = pending_finish.take().ok_or(ProjectionError)?;
                let context = pending.context.take();
                let completed = commit.terminal.state == TurnState::Completed;
                apply_finish_commit(
                    session_id,
                    &mut projection,
                    &mut message_ids,
                    pending,
                    commit,
                )?;
                if completed {
                    projection.model_contexts.extend(context);
                }
                continue;
            }
            // prepare 后未出现合法 commit 即视为崩溃残留；忽略整批生成消息，
            // 当前事件仍按旧格式或下一事务继续恢复。
            pending_finish = None;
        }
        if let Some(context) =
            decode::<openpaw_core::ModelContextCheckpoint>(event, "model_context_checkpoint_v1")
                .ok()
                .flatten()
        {
            projection.model_contexts.push(context);
        } else if let Some(record) =
            decode::<crate::execution_attempts::ExecutionAttemptV1>(event, "execution_attempt_v1")?
        {
            crate::execution_attempts::apply(&mut projection.execution_attempts, record)?;
        } else if let Some(accept) = TurnAcceptV1::from_session_event(event) {
            apply_turn_accept(&mut projection, &mut message_ids, accept)?;
        } else if let Some(outcome) = TurnOutcome::from_session_event(event) {
            projection.turn_sequence_high_water = projection
                .turn_sequence_high_water
                .max(turn_sequence_number(&outcome.turn_id));
            apply_restored_outcome(&mut projection.turn_outcomes, outcome)?;
        } else if let Some(admission) = RequestAdmission::from_session_event(event) {
            apply_restored_admission(&mut projection.request_admissions, admission)?;
        } else if TurnFinishCommitV1::from_session_event(event).is_some() {
            return Err(ProjectionError);
        } else if let SessionEvent::Message(message) = event {
            push_restored_message(&mut projection.messages, &mut message_ids, message.clone());
        } else if let Some(record) =
            decode::<MessagePresentationRecordV1>(event, "message_presentation_v1")?
        {
            let message = projection
                .messages
                .iter()
                .find(|message| message.uuid.as_deref() == Some(record.message_id.as_str()))
                .ok_or(ProjectionError)?;
            record.presentation.validate(message)?;
            if projection
                .message_presentations
                .insert(record.message_id, record.presentation)
                .is_some()
            {
                return Err(ProjectionError);
            }
        } else if is_transaction_event(event) {
            return Err(ProjectionError);
        }
    }
    if let Some(boundary) = projection.mutation.revert_message_id.as_deref() {
        visible_prefix_len(session_id, &projection.messages, boundary).ok_or(ProjectionError)?;
    }
    Ok(projection)
}
fn apply_turn_accept(
    projection: &mut Transcript,
    message_ids: &mut HashSet<String>,
    accept: TurnAcceptV1,
) -> Result<(), ProjectionError> {
    if accept.message.role != openpaw_core::Role::User
        || accept.message.uuid.as_deref() != Some(accept.outcome.user_message_id.as_str())
        || accept.outcome.state.is_terminal()
    {
        return Err(ProjectionError);
    }
    if let Some(admission) = accept.request_admission.as_ref() {
        if admission.turn_id != accept.outcome.turn_id
            || admission.user_message_id != accept.outcome.user_message_id
        {
            return Err(ProjectionError);
        }
    }
    if let Some(presentation) = accept.message_presentation {
        presentation.validate(&accept.message)?;
        if projection
            .message_presentations
            .insert(accept.outcome.user_message_id.clone(), presentation)
            .is_some()
        {
            return Err(ProjectionError);
        }
    }
    projection.turn_sequence_high_water = projection
        .turn_sequence_high_water
        .max(turn_sequence_number(&accept.outcome.turn_id));
    push_restored_message(&mut projection.messages, message_ids, accept.message);
    apply_restored_outcome(&mut projection.turn_outcomes, accept.outcome)?;
    if let Some(admission) = accept.request_admission {
        apply_restored_admission(&mut projection.request_admissions, admission)?;
    }
    Ok(())
}
fn apply_revert_commit(
    session_id: &str,
    projection: &mut Transcript,
    message_ids: &mut HashSet<String>,
    commit: SessionRevertCommittedV1,
) -> Result<(), ProjectionError> {
    if projection.mutation.revert_message_id.as_deref() != Some(commit.message_id.as_str()) {
        return Err(ProjectionError);
    }
    let prefix_len = visible_prefix_len(session_id, &projection.messages, &commit.message_id)
        .ok_or(ProjectionError)?;
    projection.messages.truncate(prefix_len);
    *message_ids = projection
        .messages
        .iter()
        .filter_map(|message| message.uuid.clone())
        .collect();
    projection
        .message_presentations
        .retain(|id, _| message_ids.contains(id));
    retain_turn_state_for_messages(
        &projection.messages,
        &mut projection.turn_outcomes,
        &mut projection.request_admissions,
        &mut projection.turn_errors,
    );
    projection.execution_attempts.retain(|record| {
        projection
            .turn_outcomes
            .iter()
            .any(|turn| turn.turn_id == record.turn_id)
    });
    apply_turn_accept(projection, message_ids, commit.accept)
}
fn apply_finish_commit(
    session_id: &str,
    projection: &mut Transcript,
    message_ids: &mut HashSet<String>,
    pending: PendingFinishTransaction,
    commit: TurnFinishCommitV1,
) -> Result<(), ProjectionError> {
    if pending.turn_id != commit.turn_id
        || commit.terminal.turn_id != commit.turn_id
        || !commit.terminal.state.is_terminal()
        || pending.messages.len() != commit.generated_message_ids.len()
    {
        return Err(ProjectionError);
    }
    let actual_ids: Option<Vec<_>> = pending
        .messages
        .iter()
        .map(|message| message.uuid.clone())
        .collect();
    let Some(actual_ids) = actual_ids else {
        return Err(ProjectionError);
    };
    if actual_ids != commit.generated_message_ids
        || actual_ids.iter().collect::<HashSet<_>>().len() != actual_ids.len()
    {
        return Err(ProjectionError);
    }
    if let Some(failure) = commit.open_code_error {
        let Some(classification) = commit.terminal.error.as_ref() else {
            return Err(ProjectionError);
        };
        if commit.terminal.state != TurnState::Failed
            || !is_turn_assistant_message_id(session_id, &commit.turn_id, &failure.message_id)
            || failure.error.data.code != classification.code
            || failure.error.data.is_retryable != classification.retryable
            || failure.error.data.message.chars().count() > 512
            || !matches!(failure.error.name.as_str(), "APIError" | "UnknownError")
        {
            return Err(ProjectionError);
        }
        projection
            .turn_errors
            .insert(commit.turn_id.clone(), failure);
    }
    for message in pending.messages {
        push_restored_message(&mut projection.messages, message_ids, message);
    }
    projection.turn_sequence_high_water = projection
        .turn_sequence_high_water
        .max(turn_sequence_number(&commit.terminal.turn_id));
    apply_restored_outcome(&mut projection.turn_outcomes, commit.terminal)
}
fn retained_user_message_ids(messages: &[openpaw_core::Message]) -> HashSet<String> {
    messages
        .iter()
        .filter(|message| message.role == openpaw_core::Role::User)
        .filter_map(|message| message.uuid.clone())
        .collect()
}
/// 功能：按保留的用户消息裁剪回合、幂等与失败投影。
/// 参数：messages 为保留历史；outcomes、admissions、errors 为需要同步裁剪的附加状态。
/// 返回：无。
/// 错误：无。
/// 状态：只保留仍有对应用户消息的回合及其幂等和失败记录，不修改消息。
/// 并发与取消：调用方独占三个容器；同步处理，不跨 await。
/// 权限：不访问文件、网络或工具，不改变保留消息的正文。
/// 关联：CHG-2026-0025。
pub fn retain_turn_state_for_messages(
    messages: &[openpaw_core::Message],
    outcomes: &mut Vec<TurnOutcome>,
    admissions: &mut HashMap<String, RequestAdmission>,
    errors: &mut HashMap<String, RecordedFailure>,
) {
    let retained_user_ids = retained_user_message_ids(messages);
    outcomes.retain(|outcome| retained_user_ids.contains(&outcome.user_message_id));
    let retained_turn_ids: HashSet<_> = outcomes
        .iter()
        .map(|outcome| outcome.turn_id.clone())
        .collect();
    errors.retain(|turn_id, _| retained_turn_ids.contains(turn_id));
    admissions.retain(|_, admission| {
        retained_user_ids.contains(&admission.user_message_id)
            && retained_turn_ids.contains(&admission.turn_id)
    });
}
fn push_restored_message(
    messages: &mut Vec<openpaw_core::Message>,
    message_ids: &mut HashSet<String>,
    message: openpaw_core::Message,
) {
    if let Some(id) = message.uuid.as_ref() {
        if !message_ids.insert(id.clone()) {
            return;
        }
    }
    messages.push(message);
}
fn apply_restored_outcome(
    outcomes: &mut Vec<TurnOutcome>,
    outcome: TurnOutcome,
) -> Result<(), ProjectionError> {
    if let Some(existing) = outcomes
        .iter_mut()
        .find(|existing| existing.turn_id == outcome.turn_id)
    {
        if existing.user_message_id != outcome.user_message_id {
            return Err(ProjectionError);
        }
        if existing.state.is_terminal() {
            if outcome.state.is_terminal() && existing != &outcome {
                return Err(ProjectionError);
            }
            return Ok(());
        }
        *existing = outcome;
    } else {
        outcomes.push(outcome);
    }
    Ok(())
}
fn apply_restored_admission(
    admissions: &mut HashMap<String, RequestAdmission>,
    admission: RequestAdmission,
) -> Result<(), ProjectionError> {
    if let Some(existing) = admissions.get(&admission.request_id) {
        if existing != &admission {
            return Err(ProjectionError);
        }
        return Ok(());
    }
    admissions.insert(admission.request_id.clone(), admission);
    Ok(())
}
fn is_transaction_event(event: &SessionEvent) -> bool {
    let SessionEvent::Unknown(value) = event else {
        return false;
    };
    matches!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("turn_accept_v1" | "turn_finish_prepare_v1" | "turn_finish_commit_v1")
    )
}
fn turn_sequence_number(turn_id: &str) -> u64 {
    turn_id
        .split('_')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}
/// 功能：保存或转换当前接口的 is_turn_assistant_message_id 信息。
/// 参数：见签名或字段类型；只消费调用方提供的数据。
/// 返回：见签名；字段或类型本身无额外返回。
/// 错误：不适用时无；配置或读取错误由调用者处理，取消不表示批准。
/// 状态：仅修改当前内存对象或构造读取结果；不改原始 JSONL。
/// 并发与取消：同步操作不等待外部 I/O；异步等待取消按当前模块生命周期清理。
/// 权限：不扩大工具或文件权限，不记录认证信息。
/// 关联：CHG-2026-0023 的共享能力和入口 adapter。
pub fn is_turn_assistant_message_id(session_id: &str, turn_id: &str, message_id: &str) -> bool {
    let base = format!("msg_{session_id}_{turn_id}_assistant");
    let Some(suffix) = message_id.strip_prefix(&base) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    let Some(index) = suffix.strip_prefix('_') else {
        return false;
    };
    index
        .parse::<usize>()
        .is_ok_and(|value| value > 0 && value.to_string() == index)
}

impl Transcript {
    /// 功能：按可靠执行定位键读取已保存的完整工具结果。
    /// 参数：turn/response/call标识同一工具调用。
    /// 返回：实际正文及错误状态；不存在返回None。
    /// 错误：无，不合成缺失原件。
    /// 状态：只读。
    /// 并发与取消：同步快照读取。
    /// 权限：调用方必须先校验会话及来源回合可见性。
    /// 关联：Host与CLI的read_context_record。
    pub fn tool_original(&self, turn: &str, response: u64, call: &str) -> Option<(String, bool)> {
        let record = self.execution_attempts.iter().find(|r| {
            r.turn_id == turn
                && r.kind == "tool_result"
                && r.response_index as u64 == response
                && r.payload["call_id"] == call
        })?;
        Some((
            record.payload["outcome"]["content"].as_str()?.to_owned(),
            record.payload["outcome"]["is_error"].as_bool()?,
        ))
    }
    /// 功能：取得当前可见的已提交消息前缀。
    /// 参数：session_id，用于旧消息稳定编号。
    /// 返回：借用的可见切片。
    /// 错误：replay 已校验回退游标。
    /// 状态：不改变完整历史。
    /// 并发与取消：调用方提供稳定 Transcript。
    /// 权限：隐藏 staged 边界及尾部。
    /// 关联：CLI show/search 和 Host 形状适配。
    pub fn visible_messages(&self, session_id: &str) -> &[Message] {
        let len = self
            .mutation
            .revert_message_id
            .as_deref()
            .and_then(|id| visible_prefix_len(session_id, &self.messages, id))
            .unwrap_or(self.messages.len());
        &self.messages[..len]
    }
    /// 功能：解析显式标题或首个持久 user 文本的摘要。
    /// 参数：见签名或字段类型；只消费调用方提供的数据。
    /// 返回：见签名；字段或类型本身无额外返回。
    /// 错误：不适用时无；配置或读取错误由调用者处理，取消不表示批准。
    /// 状态：仅修改当前内存对象或构造读取结果；不改原始 JSONL。
    /// 并发与取消：同步操作不等待外部 I/O；异步等待取消按当前模块生命周期清理。
    /// 权限：不扩大工具或文件权限，不记录认证信息。
    /// 关联：CHG-2026-0023 的共享能力和入口 adapter。
    pub fn title(&self) -> String {
        session_title_with_presentation(
            self.mutation.title.as_deref(),
            &self.messages,
            &self.message_presentations,
        )
    }
}

/// 功能：统一会话标题选择。
/// 参数：显式标题及已提交消息。
/// 返回：最多 80 字符标题。
/// 错误：无。
/// 状态：纯计算，显式标题优先于首条 user。
/// 并发与取消：同步无锁。
/// 权限：不增加任何持久化或日志。
/// 关联：Transcript::title 与 Host session_title。
pub fn session_title(explicit: Option<&str>, messages: &[Message]) -> String {
    session_title_with_presentation(explicit, messages, &HashMap::new())
}

/// 功能：统一选择显式标题或首条输入中的非辅助正文。
/// 参数：explicit 为已有标题，messages 为完整历史，presentations 为消息展示属性。
/// 返回：最多 80 字符的标题；仅附件使用附件默认标题。
/// 错误：无；属性已经由接收或 replay 校验。
/// 状态：纯计算，不修改历史或已有标题。
/// 并发与取消：同步无锁。
/// 权限：不解析客户端元数据，不输出正文到日志。
/// 关联：CHG-2026-0027；Transcript 与 Host 共享。
pub fn session_title_with_presentation(
    explicit: Option<&str>,
    messages: &[Message],
    presentations: &HashMap<String, MessagePresentationV1>,
) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| {
            messages
                .iter()
                .find(|m| m.role == openpaw_core::Role::User)
                .map(|message| {
                    let presentation = message.uuid.as_ref().and_then(|id| presentations.get(id));
                    let text: String = message
                        .content
                        .iter()
                        .enumerate()
                        .filter_map(|(index, block)| {
                            if presentation.is_some_and(|value| {
                                value
                                    .parts
                                    .iter()
                                    .any(|part| part.block_index == index && part.auxiliary)
                            }) {
                                None
                            } else {
                                block.as_text()
                            }
                        })
                        .collect();
                    if !text.trim().is_empty() {
                        return text;
                    }
                    if message
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Image { .. }))
                    {
                        "图片附件".into()
                    } else if message
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Audio { .. }))
                    {
                        "音频附件".into()
                    } else {
                        "New session".into()
                    }
                })
        })
        .unwrap_or_else(|| "New session".into())
        .chars()
        .take(80)
        .collect()
}

/// 功能：把可见 assistant 回退边界规范化为触发同一回合的 user 消息边界。
/// 参数：session、可见消息、回合结果和客户端请求的消息编号。
/// 返回：权威游标及前缀；真实或已验证失败、取消 assistant 归到触发输入，未知边界返回 None。
/// 错误：无；无有效回合关联且找不到此前真实用户输入时保留原 assistant 边界。
/// 状态：纯读取，不修改消息、回合结果或持久化投影。
/// 并发与取消：调用方提供稳定快照，同步无锁，不可取消。
/// 权限：仅读取角色、内容块类型及不透明 ID，不读取正文值、文件或网络。
/// 关联：`API-HOST-SESSION-REVERT-001`、`CHG-2026-0021`、`CHG-2026-0022`、`CHG-2026-0023`。
pub fn normalize_revert_boundary(
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
                matches!(turn.state, TurnState::Failed | TurnState::Cancelled)
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

/// 功能：将已经验证存在的失败或取消尝试边界映射为真实历史前缀。
/// 参数：session_id、messages、outcomes 为稳定快照，message_id 为公共错误消息编号。
/// 返回：该失败回合真实消息之后、下一用户回合之前的位置；不可验证时返回 None。
/// 错误：无。
/// 状态：纯计算，不将占位或错误复制到分支模型历史。
/// 并发与取消：调用方持有会话变更闸门，同步不可取消。
/// 权限：只读取不透明 ID，不访问文件或网络。
/// 关联：`CHG-2026-0022`。
pub fn failed_message_prefix_len(
    session_id: &str,
    messages: &[Message],
    outcomes: &[TurnOutcome],
    message_id: &str,
) -> Option<usize> {
    let turn = outcomes.iter().find(|turn| {
        matches!(turn.state, TurnState::Failed | TurnState::Cancelled)
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
pub fn clone_messages_for_fork(messages: &[Message]) -> Vec<Message> {
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
    use super::*;
    use serde_json::json;
    fn unknown(value: serde_json::Value) -> SessionEvent {
        SessionEvent::Unknown(Box::new(value))
    }
    #[test]
    fn presentation_records_validate_links_and_keep_old_accept_defaults() {
        let media =
            openpaw_core::InlineMedia::from_bytes(b"GIF89afixture", "image/gif", None).unwrap();
        let mut user = Message::user_text("helper").with_uuid("user-1");
        user.content.push(media.block());
        let record = json!({"type":"turn_accept_v1","message":user,
            "outcome":{"turnID":"1_turn","userMessageID":"user-1","state":"running"},
            "messagePresentation":{"version":1,"parts":[{"blockIndex":0,"auxiliary":true}]}});
        assert_eq!(
            replay("session", &[unknown(record.clone())])
                .unwrap()
                .message_presentations
                .len(),
            1
        );
        for mutation in ["version", "image-block", "duplicate"] {
            let mut bad = record.clone();
            match mutation {
                "version" => bad["messagePresentation"]["version"] = 2.into(),
                "image-block" => bad["messagePresentation"]["parts"][0]["blockIndex"] = 1.into(),
                _ => bad["messagePresentation"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"blockIndex":0,"auxiliary":false})),
            }
            assert!(replay("session", &[unknown(bad)]).is_err());
        }
        let mut old = record.clone();
        old.as_object_mut().unwrap().remove("messagePresentation");
        assert!(replay("session", &[unknown(old)])
            .unwrap()
            .message_presentations
            .is_empty());
        assert!(replay("session", &[unknown(json!({"type":"message_presentation_v1","messageID":"missing","presentation":record["messagePresentation"]}))]).is_err());
        let image_message = Message {
            content: vec![media.block()],
            ..Message::default()
        };
        assert_eq!(session_title(None, &[image_message]), "图片附件");
        let audio =
            openpaw_core::InlineMedia::from_bytes(b"RIFF0000WAVEfixture", "audio/wav", None)
                .unwrap();
        assert_eq!(
            session_title(
                None,
                &[Message {
                    content: vec![audio.block()],
                    ..Message::default()
                }]
            ),
            "音频附件"
        );
    }
    #[test]
    fn incomplete_finish_stays_hidden_and_invalid_registered_records_fail_closed() {
        let user = Message::user_text("visible user");
        let reply = Message::assistant_text("uncommitted secret");
        let events = vec![
            SessionEvent::Message(user),
            unknown(json!({"type":"turn_finish_prepare_v1","turnID":"1_turn"})),
            SessionEvent::Message(reply),
        ];
        let p = replay("session", &events).unwrap();
        assert_eq!(p.visible_messages("session").len(), 1);
        for record in [
            json!({"type":"turn_finish_prepare_v1"}),
            json!({"type":"turn_finish_commit_v1"}),
            json!({"type":"session_revert_staged_v1"}),
            json!({"type":"session_metadata_v1","title":42}),
            json!({"type":"turn_accept_v1"}),
        ] {
            let mut bad = events.clone();
            bad.push(unknown(record));
            assert!(replay("session", &bad).is_err());
        }
        let mut forward = events;
        forward.push(unknown(json!({"type":"future_v2"})));
        assert_eq!(replay("session", &forward).unwrap().messages.len(), 1);
    }
    #[test]
    fn finish_commit_rejects_wrong_turn_ids_and_preserves_committed_history() {
        let mut reply = Message::assistant_text("committed reply");
        reply.uuid = Some("reply-id".into());
        let terminal = json!({"turnID":"1_turn","userMessageID":"user-id","state":"completed","completedAt":123});
        let mut events = vec![
            unknown(json!({"type":"turn_finish_prepare_v1","turnID":"1_turn"})),
            SessionEvent::Message(reply),
        ];
        let commit = json!({"type":"turn_finish_commit_v1","turnID":"1_turn","generatedMessageIDs":["reply-id"],"terminal":terminal});
        events.push(unknown(commit.clone()));
        assert_eq!(
            replay("session", &events).unwrap().messages[0].text(),
            "committed reply"
        );
        for bad in [
            json!({"type":"turn_finish_commit_v1","turnID":"wrong","generatedMessageIDs":["reply-id"],"terminal":terminal}),
            json!({"type":"turn_finish_commit_v1","turnID":"1_turn","generatedMessageIDs":["wrong-id"],"terminal":terminal}),
        ] {
            *events.last_mut().unwrap() = unknown(bad);
            assert!(replay("session", &events).is_err());
        }
    }

    mod host_history_contract {

        use openpaw_core::{
            CompactMetadata, CompactType, ContentBlock, Message, Role, SessionEvent,
        };

        use super::super::{
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
                            media: Vec::new(),
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
}
