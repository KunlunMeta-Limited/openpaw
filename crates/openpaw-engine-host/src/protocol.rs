//! 功能：定义 OpenCode v1 子集的稳定线协议类型。
//! 输入输出边界：HTTP 请求、SSE 事件和 JSONL 摘要都通过本模块的 serde 类型转换。
//! 设计约束：未知客户端元数据不影响已登记字段；本模块不负责路由、session 写入或业务决策。
//! 关联：`DES-OPENPAW-B-2026-09-15`、`API-HOST-*`、`EVT-HOST-*`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 功能：定义 Host 进程级回合调度容量。
/// 字段语义：`max_concurrent_turns` 为同时运行上限，`max_queued_turns` 为等待队列上限。
/// 数据来源：`OPENPAW_MAX_CONCURRENT_TURNS` 和 `OPENPAW_MAX_QUEUED_TURNS` 环境变量。
/// 数据去向：Engine TurnScheduler 的活动槽位、FIFO 队列与有界排队计数。
/// 约束：并发范围为 1..=32，队列范围为 0..=1024；非法值回退默认 4/32。
/// 稳定性：Host 进程配置；修改默认值或边界必须更新 CHG 和并发测试。
/// 关联：`CHG-2026-0012`、`API-HOST-SESSION-006`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnSchedulerConfig {
    /// 功能：限制同时调用 Provider 的跨会话回合数。
    pub max_concurrent_turns: usize,
    /// 功能：限制已接收但尚未调用 Provider 的等待回合数。
    pub max_queued_turns: usize,
}

impl Default for TurnSchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_turns: 4,
            max_queued_turns: 32,
        }
    }
}

impl TurnSchedulerConfig {
    /// 功能：从环境变量读取并校验 Host 回合调度容量。
    /// 参数：无；读取当前进程环境。
    /// 返回：合法配置或默认 4/32，不向调用方暴露环境变量原文。
    /// 错误：非法、空白或越界值记录告警并回退对应默认值，不阻断 Host 启动。
    /// 状态：不修改环境变量和 session 数据。
    /// 并发与取消：纯同步只读，可在 Host 构造时调用一次。
    /// 权限：环境值只作为整数容量，不进入事件、日志正文或 Office 消息。
    /// 关联：`CHG-2026-0012`。
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            max_concurrent_turns: bounded_env_usize(
                "OPENPAW_MAX_CONCURRENT_TURNS",
                1,
                32,
                defaults.max_concurrent_turns,
            ),
            max_queued_turns: bounded_env_usize(
                "OPENPAW_MAX_QUEUED_TURNS",
                0,
                1024,
                defaults.max_queued_turns,
            ),
        }
    }
}

impl From<TurnSchedulerConfig> for openpaw_engine::scheduler::SchedulerLimits {
    /// 功能：把 Host 已校验的容量转换为 Engine 中立调度输入。
    /// 参数：config 为 Host 显式配置或环境读取结果。
    /// 返回：相同运行和排队容量的 SchedulerLimits。
    /// 错误：无；不替代 HostConfig 的容量校验。
    /// 状态：不改变默认值，不读取或写入环境。
    /// 并发与取消：同步值转换，无锁和取消等待。
    /// 权限：不访问会话或外部资源。
    /// 关联：CHG-2026-0025。
    fn from(config: TurnSchedulerConfig) -> Self {
        Self {
            max_concurrent_turns: config.max_concurrent_turns,
            max_queued_turns: config.max_queued_turns,
        }
    }
}

fn bounded_env_usize(name: &str, minimum: usize, maximum: usize, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => match value.parse::<usize>() {
            Ok(parsed) if (minimum..=maximum).contains(&parsed) => parsed,
            _ => {
                tracing::warn!(variable = name, "回合调度环境变量非法，使用默认值");
                default
            }
        },
        Err(_) => default,
    }
}

/// 功能：沿用共享持久化回合类型；外部序列化字段保持既有合同。
pub use openpaw_store::transcript::{TurnError, TurnOutcome, TurnState};

/// 功能：保存 engine-host 启动、认证、工作区、Office 投影和默认 provider 配置。
/// 状态：`engine-host` feature 开启时由 CLI 创建并在启动前校验。
/// 并发与取消：配置本身不可变；回合取消由 runtime 的 `SessionHandle` 管理。
/// 权限：用户名和密码仅用于 Basic Authentication，不得写入日志或事件。
/// 关联：`REQ-B-HOST-001`、`REQ-B-HOST-003`、`REQ-B-HOST-008`。
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// 功能：指定监听主机；默认回环 `127.0.0.1`，容器部署可显式使用通配地址 `0.0.0.0`。
    pub hostname: String,
    /// 功能：指定监听端口；0 表示由操作系统分配空闲端口。
    pub port: u16,
    /// 功能：指定 OpenPaw home 根目录，用于 session 和目标数据。
    pub home: PathBuf,
    /// 功能：指定当前回合使用的工作区根目录。
    pub workspace: PathBuf,
    /// 功能：Basic Authentication 用户名；与 password 必须成对出现。
    pub username: Option<String>,
    /// 功能：Basic Authentication 密码；不得进入日志和 Office 动作。
    pub password: Option<String>,
    /// 功能：Office action Gateway 地址；None 表示丢弃投影但不影响回合。
    pub office_url: Option<String>,
    /// 功能：默认 provider 标识；只允许 mock、echo、anthropic、openai。
    pub provider: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            hostname: "127.0.0.1".into(),
            port: 0,
            home: openpaw_core::openpaw_home(),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            username: std::env::var("OPENCODE_SERVER_USERNAME").ok(),
            password: std::env::var("OPENCODE_SERVER_PASSWORD").ok(),
            office_url: Some(
                std::env::var("OPENPAW_OFFICE_ACTIONS_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8765/actions".into()),
            ),
            provider: "mock".into(),
        }
    }
}

impl HostConfig {
    /// 功能：校验监听地址、成对凭据和 provider 白名单。
    /// 返回：校验成功返回 `Ok(())`；失败返回中文原因。
    /// 错误：既非回环也非通配地址、半配置凭据或未知 provider 会被拒绝。
    /// 关联：`API-HOST-HEALTH-001`、`REQ-B-HOST-003`。
    pub fn validate(&self) -> Result<(), String> {
        let hostname = self.hostname.trim();
        if hostname.is_empty() {
            return Err("引擎主机 hostname 不能为空".into());
        }
        // `Dockerfile.engine-host` 以 `0.0.0.0` 启动；仍拒绝绑定具体外部地址。
        if hostname != "127.0.0.1" && hostname != "0.0.0.0" {
            return Err("引擎主机必须绑定 127.0.0.1，容器部署可显式使用 0.0.0.0".into());
        }
        if self.username.is_some() != self.password.is_some() {
            return Err("引擎主机鉴权必须同时设置用户名和密码".into());
        }
        match self.provider.as_str() {
            "mock" | "echo" | "anthropic" | "openai" => Ok(()),
            other => Err(format!("不支持的 provider：`{other}`")),
        }
    }
}

/// 功能：表示一帧 OpenCode SSE/REST 事件。
/// 字段/变体语义：`event_type` 是稳定事件名，`properties` 保存该事件的 JSON 属性。
/// 数据来源：`events.rs` 的 AgentEvent 转换。
/// 数据去向：OpenWork SDK、Host 测试和全局事件订阅。
/// 约束：事件名称和属性字段必须符合 `EVT-HOST-*` 登记；不得写入敏感数据。
/// 稳定性：跨项目公共协议，修改必须建立契约 CHG。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeEvent {
    /// 功能：保存事件类型，例如 `message.part.delta`。
    #[serde(rename = "type")]
    pub event_type: String,
    /// 功能：保存事件属性；缺省时序列化为空 JSON 对象。
    #[serde(default)]
    pub properties: serde_json::Value,
}

impl OpenCodeEvent {
    /// 功能：使用事件名和可序列化属性创建协议事件。
    /// 参数：`event_type` 为线协议事件名，`properties` 为对应属性。
    /// 返回：可直接用于 REST 或 SSE 的事件值。
    /// 错误：属性序列化失败时使用 JSON null，以保持事件发送路径不 panic。
    /// 关联：`EVT-HOST-*`。
    pub fn new(event_type: impl Into<String>, properties: impl Serialize) -> Self {
        Self {
            event_type: event_type.into(),
            properties: serde_json::to_value(properties).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// 功能：提供 Office/摘要接口使用的会话列表对象。
/// 数据来源：runtime 的 JSONL meta 和活动状态。
/// 数据去向：AI Office `/office/sessions` 和 Host 列表响应。
/// 约束：时间为 Unix 毫秒；`active` 只表示当前进程活动回合。
/// 关联：`API-HOST-SESSION-002`、`REQ-B-OFFICE-005`。
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    /// OpenCode 会话公共投影；列表与创建、详情使用相同字段。
    #[serde(flatten)]
    pub session: SessionWire,
    #[serde(rename = "createdAt")]
    /// 会话创建时间，Unix 毫秒。
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    /// 会话最近更新时间，Unix 毫秒。
    pub updated_at: i64,
    /// 创建会话使用的 provider 标识。
    pub provider: String,
    /// 创建会话使用的模型标识。
    pub model: String,
    /// 当前进程是否存在活动回合。
    pub active: bool,
}

/// 功能：表示 OpenCode `/session` REST 响应的会话对象。
/// 数据来源：OpenPaw session meta 和工作区配置。
/// 数据去向：OpenWork SDK 的 session 创建、查询和事件同步。
/// 稳定性：跨项目公共协议；字段名受 serde 约束。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWire {
    /// 会话稳定编号。
    pub id: String,
    /// 会话标题。
    pub title: String,
    /// 与 id 相同的兼容 slug。
    pub slug: String,
    /// 会话所属工作区的规范化路径。
    pub directory: String,
    /// 创建和更新时间的 Unix 毫秒。
    pub time: SessionTime,
    /// 暂存回退游标；没有可取消回退时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<SessionRevert>,
}

/// 功能：表示会话创建和更新时间。
/// 约束：created 在创建时固定；updated 在首条用户消息或其他登记更新时变化。
/// 关联：`API-HOST-SESSION-003`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTime {
    /// 会话创建时间，Unix 毫秒。
    pub created: i64,
    /// 会话最近更新时间，Unix 毫秒。
    pub updated: i64,
    /// 会话归档时间，Unix 毫秒；活动会话省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<i64>,
}

/// 功能：表示客户端仍可取消的会话级暂存回退。
/// 数据来源：Host 私有 `session_revert_staged_v1` 投影。
/// 数据去向：Session REST 响应、SSE 和 OpenWork transcript。
/// 关联：`API-HOST-SESSION-F02`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRevert {
    /// 第一条被隐藏消息的稳定编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
}

/// 功能：解析 `POST /session` 的可选创建参数。
/// 参数/字段：provider、model、directory、title 均可省略，由 Host 默认值补齐。
/// 错误：非法 JSON 由 HTTP 层返回 400。
/// 关联：`API-HOST-SESSION-001`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CreateSessionRequest {
    /// 默认 provider 覆盖值。
    pub provider: Option<String>,
    /// 默认 model 覆盖值。
    pub model: Option<String>,
    /// 会话显示标题。
    pub title: Option<String>,
}

/// 功能：解析 `PATCH /session/:id` 的可选标题与时间更新。
/// 参数/字段：至少提供 `title` 或 `time.archived` 之一。
/// 错误：空请求、空标题和负归档时间由 HTTP/runtime 返回 400。
/// 状态：请求本身不持有旧值，runtime 以单条事件原子合并。
/// 并发与取消：反序列化同步完成；会话 gate 由 runtime 获取。
/// 权限：沿用路由 Basic Authentication。
/// 关联：`API-HOST-SESSION-UPDATE-001`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateSessionRequest {
    /// 新会话标题；省略表示不修改。
    pub title: Option<String>,
    /// 会话时间字段更新；当前只登记 archived。
    pub time: Option<UpdateSessionTime>,
}

/// 功能：表示会话更新时间对象中允许写入的字段。
/// 数据边界：正数表示归档，零表示取消归档。
/// 关联：`API-HOST-SESSION-F01`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateSessionTime {
    /// 归档 Unix 毫秒；零表示恢复会话。
    pub archived: Option<i64>,
}

/// 功能：解析创建会话分支的可选消息边界。
/// 参数/字段：`messageID` 存在时复制严格早于该消息的可见前缀。
/// 错误：未知或隐藏边界由 runtime 返回 400。
/// 状态：不携带源会话之外的可变状态。
/// 并发与取消：反序列化同步完成；源会话 gate 由 runtime 获取。
/// 权限：沿用路由 Basic Authentication。
/// 关联：`API-HOST-SESSION-FORK-001`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ForkSessionRequest {
    /// 严格排除的首条消息编号；省略时复制全部可见消息。
    #[serde(rename = "messageID")]
    pub message_id: Option<String>,
}

/// 功能：解析暂存会话回退的消息边界。
/// 参数/字段：`messageID` 必填；`partID` 仅用于显式拒绝未支持能力。
/// 错误：缺失消息、未知边界或任意 partID 返回 400。
/// 状态：请求不直接修改会话。
/// 并发与取消：反序列化同步完成；状态变更由 runtime gate 线性化。
/// 权限：沿用路由 Basic Authentication。
/// 关联：`API-HOST-SESSION-REVERT-001`。
#[derive(Debug, Clone, Deserialize)]
pub struct RevertSessionRequest {
    /// 第一条需要隐藏的消息编号。
    #[serde(rename = "messageID")]
    pub message_id: String,
    /// 消息内部 part 边界；本版本不支持。
    #[serde(rename = "partID")]
    pub part_id: Option<String>,
}

/// 功能：解析 prompt/message 请求，支持旧的 text 字段和 OpenCode parts。
/// 数据来源：HTTP JSON body。
/// 数据去向：runtime 的 AgentLoop 回合。
/// 约束：只有 type 为 text 的 part 参与正文拼接。
/// 关联：`API-HOST-SESSION-006`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MessageRequest {
    /// 客户端用户消息编号；同时作为缺省幂等键，供页面确认接收事实。
    #[serde(rename = "messageID")]
    pub message_id: Option<String>,
    /// 客户端提供的单 session 幂等键；省略时保持旧版非幂等行为。
    #[serde(rename = "requestID", alias = "requestId")]
    pub request_id: Option<String>,
    /// OpenCode 消息 part 列表，按输入顺序拼接文本。
    #[serde(default)]
    pub parts: Vec<MessagePart>,
    /// 兼容简化客户端的直接文本字段。
    #[serde(default)]
    pub text: Option<String>,
    /// 本次回合的 provider/model 覆盖值。
    pub model: Option<ModelRequest>,
}

/// 功能：复用 Store 的接收记录及安全错误数据，Host 保留公开展示校验。
pub(crate) use openpaw_store::transcript::{
    RecordedFailure as OpenCodeTurnErrorV1, RequestAdmission, SafeError as OpenCodeErrorV1,
    SafeErrorData as OpenCodeErrorDataV1, TurnAcceptV1,
};

/// 功能：指定单次消息请求使用的 provider 和 model。
/// 约束：providerID 只能是 Host 支持的 provider；缺省时沿用 session/default 配置。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelRequest {
    /// 线协议字段 `providerID`，provider 的稳定标识。
    #[serde(rename = "providerID")]
    pub provider_id: Option<String>,
    /// 线协议字段 `modelID`，模型的稳定标识。
    #[serde(rename = "modelID")]
    pub model_id: Option<String>,
}

/// 功能：表示一个 OpenCode 消息 part。
/// 字段/变体语义：当前只把 kind 为 text 且存在 text 的 part 送入 AgentLoop。
/// 约束：其他 part 类型保留为可忽略输入，不得改变正文拼接顺序。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MessagePart {
    /// 线协议字段 `type`，例如 text。
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// 文本 part 的正文。
    pub text: Option<String>,
}

impl MessageRequest {
    /// 功能：按请求顺序取得所有 text part 的拼接结果。
    /// 返回：直接 text 字段优先；否则拼接 type=text 的 part，不插入分隔符。
    /// 状态：不会修改请求对象，也不会产生写入。
    /// 关联：`API-HOST-SESSION-006`。
    pub fn text(&self) -> String {
        if let Some(text) = &self.text {
            return text.clone();
        }
        self.parts
            .iter()
            .filter_map(|part| {
                (part.kind.as_deref() == Some("text"))
                    .then_some(part.text.as_deref())
                    .flatten()
            })
            .collect()
    }
}

/// 功能：解析 permission reply 请求。
/// 字段/变体语义：reply 支持 once、always、reject；reason 用于拒绝说明。
/// 约束：具体审批状态转换由 `approval.rs` 执行。
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionReply {
    /// 审批回复枚举。
    pub reply: Option<String>,
    /// 拒绝或处理原因。
    pub reason: Option<String>,
}

/// 功能：表示待审批权限请求及其 OpenCode 线协议字段。
/// 数据来源：`HostApprovalManager`。
/// 数据去向：permission GET、permission.asked/replied 事件和 Office 权限面板。
/// 安全：`input` 仅供内部执行，不序列化到外部响应。
/// 关联：`API-HOST-PERMISSION-001`、`EVT-HOST-PERMISSION-001`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionInfo {
    /// 权限请求稳定编号。
    pub id: String,
    #[serde(rename = "sessionID")]
    /// 所属 session 编号，线协议字段名为 `sessionID`。
    pub session_id: String,
    /// 请求的权限或工具名称。
    pub permission: String,
    /// 触发审批的规则或路径提示。
    pub patterns: Vec<String>,
    /// 不含敏感输入的附加元数据。
    pub metadata: serde_json::Value,
    /// 长期授权候选项；当前实现不跨回合保存授权。
    pub always: Vec<String>,
    #[serde(skip)]
    /// 供内部 ToolHost 执行使用的原始 JSON 输入，不序列化到外部。
    pub input: serde_json::Value,
}

/// 功能：表示待办事项的 OpenCode 兼容摘要。
/// 数据来源：workspace 下的 todo JSONL。
/// 数据去向：`todo.updated` 事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// 待办稳定编号。
    pub id: String,
    /// 待办正文。
    pub content: String,
    /// 待办状态枚举原文。
    pub status: String,
}

/// 功能：把 UTC 时间转换为 OpenCode 使用的 Unix 毫秒。
/// 参数：`time` 为 UTC 时间。
/// 返回：自 Unix epoch 起的毫秒数。
/// 关联：`API-HOST-SESSION-*`。
pub fn unix_millis(time: DateTime<Utc>) -> i64 {
    time.timestamp_millis()
}

/// Host 本地 OpenCode v1 serde 类型；不得移动到 core/engine。
#[rustfmt::skip]
#[allow(dead_code)]
pub(crate) mod wire {
    use super::*;
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageInfoWire { pub id: String, #[serde(rename="sessionID")] pub session_id: String, pub role: String, pub time: MessageTimeWire }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageTimeWire { #[serde(skip_serializing_if="Option::is_none")] pub created: Option<i64>, #[serde(skip_serializing_if="Option::is_none")] pub completed: Option<i64> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageWire { pub info: MessageInfoWire, #[serde(default)] pub parts: Vec<MessagePartWire> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessagePartWire { #[serde(rename="type")] pub part_type: String, pub id: String, #[serde(rename="messageID")] pub message_id: String, #[serde(rename="sessionID")] pub session_id: String, #[serde(default, skip_serializing_if="Option::is_none")] pub text: Option<String>, #[serde(rename="callID", default, skip_serializing_if="Option::is_none")] pub call_id: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub tool: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub state: Option<ToolStateWire> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ToolStateWire { pub status: ToolStatusWire, #[serde(default)] pub input: serde_json::Value, #[serde(default)] pub output: String, #[serde(default)] pub error: String, #[serde(rename="durationMs", skip_serializing_if="Option::is_none")] pub duration_ms: Option<u64> }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all="lowercase")] pub enum ToolStatusWire { Running, Completed, Error }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderListWire { pub all: Vec<ProviderWire>, pub connected: Vec<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub default: Option<String> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderWire { pub id: String, pub name: String, #[serde(default)] pub models: std::collections::BTreeMap<String, ProviderModelWire> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderModelWire { pub id: String, pub name: String }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct SessionStatusWire { #[serde(rename="type")] pub status_type: SessionStatusTypeWire, #[serde(rename="turnID", default, skip_serializing_if="Option::is_none")] pub turn_id: Option<String>, #[serde(rename="userMessageID", default, skip_serializing_if="Option::is_none")] pub user_message_id: Option<String>, #[serde(rename="queuePosition", default, skip_serializing_if="Option::is_none")] pub queue_position: Option<usize>, #[serde(default, skip_serializing_if="Option::is_none")] pub activity: Option<serde_json::Value> }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all="lowercase")] pub enum SessionStatusTypeWire { Busy, Idle, Queued }
    pub type SseEnvelope = OpenCodeEvent;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::wire::{MessageWire, ProviderListWire, SessionStatusWire, ToolStatusWire};
    use super::{
        HostConfig, OpenCodeEvent, PermissionInfo, SessionRevert, SessionTime, SessionWire,
        TodoItem,
    };

    fn roundtrip<T>(input: &str) -> T
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let parsed: T = serde_json::from_str(input).expect("fixture deserialize");
        let encoded = serde_json::to_vec(&parsed).expect("fixture serialize");
        serde_json::from_slice(&encoded).expect("fixture roundtrip")
    }

    #[test]
    fn host_config_is_loopback_only() {
        let default = HostConfig::default();
        assert_eq!(default.hostname, "127.0.0.1");
        assert_eq!(default.port, 0);
        assert!(default.validate().is_ok());
        // 容器部署的显式例外：A 线 `Dockerfile.engine-host` 以通配地址启动，
        // 否则 OpenWork 与 AI Office 无法经 Docker 网络访问 host。
        let wildcard = HostConfig {
            hostname: "0.0.0.0".into(),
            ..HostConfig::default()
        };
        assert!(
            wildcard.validate().is_ok(),
            "0.0.0.0 must be accepted for container deployments"
        );
        for hostname in ["localhost", "::1", "192.0.2.1", "example.com", ""] {
            let config = HostConfig {
                hostname: hostname.into(),
                ..HostConfig::default()
            };
            assert!(
                config.validate().is_err(),
                "hostname {hostname} must be rejected"
            );
        }
    }

    #[test]
    fn fixed_opencode_v1_fixtures_match_the_wire_contract() {
        let session: SessionWire = roundtrip(include_str!("../tests/fixtures/session.json"));
        assert_eq!(session.id, session.slug);
        assert_eq!(session.time.created, 0);
        assert_eq!(session.time.updated, 0);
        assert_eq!(session.time.archived, None);
        assert_eq!(session.revert, None);

        let messages: Vec<MessageWire> = roundtrip(include_str!("../tests/fixtures/messages.json"));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].info.session_id, "session-id");
        let tool = messages[1]
            .parts
            .iter()
            .find(|part| part.part_type == "tool")
            .expect("tool fixture");
        assert_eq!(tool.message_id, "msg-assistant");
        assert_eq!(tool.session_id, "session-id");
        assert_eq!(tool.call_id.as_deref(), Some("tool-use-id"));
        assert_eq!(
            tool.state.as_ref().unwrap().status,
            ToolStatusWire::Completed
        );

        let permission: PermissionInfo =
            roundtrip(include_str!("../tests/fixtures/permission.json"));
        assert_eq!(permission.session_id, "session-id");
        let todo: TodoItem = roundtrip(include_str!("../tests/fixtures/todo.json"));
        assert_eq!(todo.id, "todo-id");
        let status: BTreeMap<String, SessionStatusWire> =
            roundtrip(include_str!("../tests/fixtures/status.json"));
        assert_eq!(status.len(), 1);

        let events: Vec<OpenCodeEvent> =
            serde_json::from_str(include_str!("../tests/fixtures/events.json"))
                .expect("events fixture");
        let encoded = serde_json::to_vec(&events).expect("encode events fixture");
        let decoded: Vec<OpenCodeEvent> =
            serde_json::from_slice(&encoded).expect("decode events fixture");
        let types: Vec<&str> = decoded
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        for required in [
            "session.created",
            "session.updated",
            "session.status",
            "session.idle",
            "message.updated",
            "message.part.updated",
            "message.part.delta",
            "permission.asked",
            "permission.replied",
            "todo.updated",
        ] {
            assert!(
                types.contains(&required),
                "missing event fixture: {required}"
            );
        }
        let tool_statuses: Vec<&str> = decoded
            .iter()
            .filter_map(|event| event.properties.pointer("/part/state/status")?.as_str())
            .collect();
        for required in ["running", "completed", "error"] {
            assert!(
                tool_statuses.contains(&required),
                "missing tool status: {required}"
            );
        }

        let provider: ProviderListWire = roundtrip(include_str!("../tests/fixtures/provider.json"));
        let ids: Vec<&str> = provider.all.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["mock", "echo", "anthropic", "openai"]);
        assert_eq!(provider.connected, vec!["mock", "echo"]);

        let message_json = serde_json::to_value(&messages).expect("messages json");
        let tool_json = &message_json[1]["parts"][1];
        assert!(tool_json.get("sessionID").is_some());
        assert!(tool_json.get("messageID").is_some());
        assert!(tool_json.get("callID").is_some());
        assert!(tool_json.get("sessionId").is_none());
        assert!(tool_json.get("messageId").is_none());
        assert!(tool_json.get("callId").is_none());
    }

    #[test]
    fn session_wire_serializes_optional_archive_and_revert_without_parent_id() {
        let session = SessionWire {
            id: "s1".into(),
            title: "Session".into(),
            slug: "s1".into(),
            directory: "/workspace".into(),
            time: SessionTime {
                created: 1,
                updated: 2,
                archived: Some(42),
            },
            revert: Some(SessionRevert {
                message_id: "m2".into(),
            }),
        };
        let value = serde_json::to_value(session).unwrap();
        assert_eq!(value["time"]["archived"], 42);
        assert_eq!(value["revert"]["messageID"], "m2");
        assert!(value.get("parentID").is_none());
    }
}
