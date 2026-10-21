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
    /// 功能：Office action Gateway 地址；仅显式配置时启用 Office 投影和附加提示词。
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
            office_url: std::env::var("OPENPAW_OFFICE_ACTIONS_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
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
    /// 实际委派父会话编号；普通会话与 Fork 根会话省略。
    #[serde(rename = "parentID", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
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
/// 约束：直接 text 优先；有展示属性的文本保留独立块，内联 file 作为媒体输入；未知 part 沿用忽略规则。
/// 关联：`API-HOST-SESSION-006`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MessageRequest {
    /// 客户端用户消息编号；同时作为缺省幂等键，供页面确认接收事实。
    #[serde(rename = "messageID")]
    pub message_id: Option<String>,
    /// 客户端提供的单 session 幂等键；省略时保持旧版非幂等行为。
    #[serde(rename = "requestID", alias = "requestId")]
    pub request_id: Option<String>,
    /// OpenCode 消息 part 列表；有展示属性时保留文本边界和输入顺序。
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
/// 字段/变体语义：text 传递正文，file 传递 MIME、名称与内联原件。
/// 约束：其他 part 沿用忽略规则；file 必须通过格式与预算检查。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MessagePart {
    /// 线协议字段 `type`，例如 text。
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// 文本 part 的正文。
    pub text: Option<String>,
    /// 功能：OpenCode 辅助文本标志，仅影响展示属性。
    pub synthetic: Option<bool>,
    /// 功能：文本展示元数据对象，不参与模型正文。
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// 功能：内联媒体的 MIME。
    pub mime: Option<String>,
    /// 功能：内联 data URL；不自动加载远程地址。
    pub url: Option<String>,
    /// 功能：客户端提供的可选展示名称。
    pub filename: Option<String>,
}

impl MessageRequest {
    /// 功能：保留有属性文本的独立边界，并构造对应展示注解。
    /// 参数：body 为待接收请求，直接 text 保持优先。
    /// 返回：共享消息及可选展示属性。
    /// 错误：媒体格式或编码非法。
    /// 状态：纯转换，不接收回合。
    /// 并发与取消：同步无共享状态。
    /// 权限：不下载远程 URL，不解释元数据。
    /// 关联：CHG-2026-0027；MessageRequest 与 HTTP 共用。
    pub(crate) fn prepare(
        &self,
    ) -> openpaw_core::Result<(
        openpaw_core::Message,
        Option<openpaw_store::transcript::MessagePresentationV1>,
    )> {
        use openpaw_core::{ContentBlock, InlineMedia, Message};
        use openpaw_store::transcript::{MessagePresentationV1, PartPresentationV1};
        let body = self;
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
                    message.content.push(
                        InlineMedia::from_data_url(mime, url, part.filename.clone())?.block(),
                    );
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
        let presentation =
            (!parts.is_empty()).then_some(MessagePresentationV1 { version: 1, parts });
        Ok((message, presentation))
    }

    /// 功能：构建文字与附件的共享消息。
    /// 参数：请求中按顺序排列的 parts。
    /// 返回：中立用户消息；展示注解由 Host 接收路径另取，服务端身份稍后绑定。
    /// 错误：媒体格式、编码或大小非法。
    /// 状态：不接收回合、不写历史。
    /// 并发与取消：同步有界。
    /// 权限：不下载 URL。
    /// 关联：CHG-2026-0026、API-HOST-SESSION-006。
    pub fn message(&self) -> openpaw_core::Result<openpaw_core::Message> {
        self.prepare().map(|(message, _)| message)
    }
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
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageInfoWire { pub id: String, #[serde(rename="sessionID")] pub session_id: String, pub role: String, pub time: MessageTimeWire,
    /// 功能：保留未完成尝试的回合关系，不将其视为成功模型答复。
    #[serde(default, skip_serializing_if="Option::is_none")] pub execution: Option<serde_json::Value> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageTimeWire { #[serde(skip_serializing_if="Option::is_none")] pub created: Option<i64>, #[serde(skip_serializing_if="Option::is_none")] pub completed: Option<i64> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessageWire { pub info: MessageInfoWire, #[serde(default)] pub parts: Vec<MessagePartWire> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct MessagePartWire { #[serde(rename="type")] pub part_type: String, pub id: String, #[serde(rename="messageID")] pub message_id: String, #[serde(rename="sessionID")] pub session_id: String, #[serde(default, skip_serializing_if="Option::is_none")] pub text: Option<String>,
    /// 功能：保留 OpenCode 辅助文本展示标志。
    #[serde(default, skip_serializing_if="Option::is_none")] pub synthetic: Option<bool>,
    /// 功能：保留文本展示元数据对象。
    #[serde(default, skip_serializing_if="Option::is_none")] pub metadata: Option<serde_json::Map<String, serde_json::Value>>, #[serde(rename="callID", default, skip_serializing_if="Option::is_none")] pub call_id: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub tool: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub state: Option<ToolStateWire>, #[serde(default, skip_serializing_if="Option::is_none")] pub mime: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub url: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub filename: Option<String>, #[serde(rename="attachmentID", default, skip_serializing_if="Option::is_none")] pub attachment_id: Option<String>, #[serde(default, skip_serializing_if="Vec::is_empty")] pub attachments: Vec<serde_json::Value> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ToolStateWire { pub status: ToolStatusWire, #[serde(default)] pub input: serde_json::Value, #[serde(default)] pub output: String, #[serde(default)] pub error: String, #[serde(rename="durationMs", skip_serializing_if="Option::is_none")] pub duration_ms: Option<u64>,
    /// 功能：区分完成、错误、拒绝、取消和未知，不扩展旧 status 枚举。
    #[serde(default, skip_serializing_if="Option::is_none")] pub disposition: Option<String> }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all="lowercase")] pub enum ToolStatusWire { Running, Completed, Error }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderListWire { pub all: Vec<ProviderWire>, pub connected: Vec<String>, #[serde(default, skip_serializing_if="Option::is_none")] pub default: Option<String> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderWire { pub id: String, pub name: String, #[serde(default)] pub models: std::collections::BTreeMap<String, ProviderModelWire> }
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct ProviderModelWire { pub id: String, pub name: String, #[serde(default, skip_serializing_if="Option::is_none")] pub capabilities: Option<serde_json::Value> }
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
            parent_id: None,
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

pub(crate) mod projection {
    //! 功能：共享 REST 消息、回退删除身份与安全错误的纯投影。
    //! 输入输出边界：读取模型消息和已提交的回合快照，返回 OpenCode 兼容展示值。
    //! 禁止承担：不写 session、不执行模型或网络调用、不将错误展示加入模型上下文。
    //! 关联：`CHG-2026-0022`。
    use crate::protocol::{
        OpenCodeErrorDataV1, OpenCodeErrorV1, OpenCodeTurnErrorV1, TurnError, TurnOutcome,
        TurnState,
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
                        .unwrap_or_else(|| {
                            format!("msg_{session_id}_{}_assistant", outcome.turn_id)
                        }),
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

    /// 功能：合并已保存尝试，补充失败/取消过程并丰富既有成功工具元数据。
    /// 参数：session_id、outcomes、records 与 wire 均属于同一授权会话。
    /// 返回：无。
    /// 错误：无；持久记录由 Store 校验。
    /// 状态：只修改公开返回值，不改变成功模型历史。
    /// 并发与取消：同步读取稳定快照；取消后的未确认结果保留 unknown。
    /// 权限：完整输入结果仅通过原鉴权历史接口返回，不发送 Office 动作。
    /// 关联：CHG-2026-0039、员工只读详情。
    pub(crate) fn merge_execution_attempts(
        session_id: &str,
        outcomes: &[TurnOutcome],
        records: &[openpaw_store::execution_attempts::ExecutionAttemptV1],
        wire: &mut Vec<serde_json::Value>,
    ) {
        for start in records.iter().filter(|r| r.kind == "response_start") {
            let Some(turn) = outcomes.iter().find(|t| t.turn_id == start.turn_id) else {
                continue;
            };
            let suffix = if start.response_index == 0 {
                "assistant".into()
            } else {
                format!("assistant_{}", start.response_index)
            };
            let id = format!("msg_{session_id}_{}_{}", start.turn_id, suffix);
            let part_suffix = if start.response_index == 0 {
                String::new()
            } else {
                format!("_{}", start.response_index)
            };
            let matching: Vec<_> = records
                .iter()
                .filter(|r| r.turn_id == start.turn_id && r.response_index == start.response_index)
                .collect();
            let successful = turn.state == TurnState::Completed;
            let existing = wire.iter().position(|m| m["info"]["id"] == id);
            if successful && existing.is_none() {
                continue;
            }
            let position = existing.unwrap_or_else(|| {
                let user = wire.iter().position(|m|m["info"]["id"] == turn.user_message_id).unwrap_or(wire.len().saturating_sub(1));
                let next = wire.iter().enumerate().skip(user+1).find(|(_,m)|m["info"]["role"] == "user").map_or(wire.len(),|(i,_)|i);
                wire.insert(next, serde_json::json!({"info":{"id":id,"sessionID":session_id,"role":"assistant","time":{"created":start.occurred_at}},"parts":[]}));
                next
            });
            let message = &mut wire[position];
            if !successful {
                let full = matching
                    .iter()
                    .find(|r| r.kind == "response")
                    .and_then(|r| r.payload["content"].as_array());
                let text = full
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_else(|| {
                        matching
                            .iter()
                            .filter(|r| r.kind == "text")
                            .filter_map(|r| r.payload["text"].as_str())
                            .collect()
                    });
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(serde_json::json!({"id":format!("prt_{session_id}_{}_text{part_suffix}",start.turn_id),"messageID":id,"sessionID":session_id,"type":"text","text":text}));
                }
                let reasoning = full
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["thinking"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_else(|| {
                        matching
                            .iter()
                            .filter(|r| r.kind == "reasoning")
                            .filter_map(|r| r.payload["text"].as_str())
                            .collect()
                    });
                if !reasoning.is_empty() {
                    parts.push(serde_json::json!({"id":format!("prt_{session_id}_{}_reasoning{part_suffix}",start.turn_id),"messageID":id,"sessionID":session_id,"type":"reasoning","text":reasoning}));
                }
                if let Some(blocks) = full {
                    for block in blocks.iter().filter(|b| b["type"] == "tool_use") {
                        let call = block["id"].as_str().unwrap_or_default();
                        parts.push(serde_json::json!({"id":format!("prt_{session_id}_{}_{call}{part_suffix}",start.turn_id),"messageID":id,"sessionID":session_id,"type":"tool","callID":call,"tool":block["name"],"state":{"status":"running","input":block["input"],"output":"","error":""}}));
                    }
                }
                message["parts"] = parts.into();
                message["info"]["execution"] =
                    serde_json::json!({"turnID":turn.turn_id,"state":turn.state,"incomplete":true});
                message["info"]["time"]
                    .as_object_mut()
                    .unwrap()
                    .remove("completed");
            }
            for part in message["parts"]
                .as_array_mut()
                .into_iter()
                .flatten()
                .filter(|p| p["type"] == "tool")
            {
                let call = part["callID"].clone();
                let prepared = matching
                    .iter()
                    .find(|r| r.kind == "tool_start" && r.payload["call_id"] == call);
                let result = matching
                    .iter()
                    .find(|r| r.kind == "tool_result" && r.payload["call_id"] == call);
                if let Some(prepared) = prepared {
                    part["state"]["input"] = prepared.payload["input"].clone();
                }
                if let Some(result) = result {
                    let outcome = &result.payload["outcome"];
                    let error = outcome["is_error"] == true;
                    part["state"]["status"] = if error { "error" } else { "completed" }.into();
                    part["state"]["output"] = if error {
                        serde_json::json!("")
                    } else {
                        outcome["content"].clone()
                    };
                    part["state"]["error"] = if error {
                        outcome["content"].clone()
                    } else {
                        serde_json::json!("")
                    };
                    part["state"]["disposition"] = result.payload["disposition"].clone();
                    part["state"]["durationMs"] = result.payload["duration_ms"].clone();
                    if let Ok(media) = serde_json::from_value::<Vec<openpaw_core::InlineMedia>>(
                        outcome["media"].clone(),
                    ) {
                        if !media.is_empty() {
                            part["attachments"] = serde_json::json!(attachments(&media));
                        }
                    }
                } else if turn.state.is_terminal() {
                    part["state"]["status"] = "error".into();
                    part["state"]["disposition"] = "unknown".into();
                    part["state"]["error"] =
                        "未保存明确执行结果；不表示未执行或副作用已撤销。".into();
                }
            }
        }
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
            decorate(&mut part, index, presentation);
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

    use openpaw_store::transcript::MessagePresentationV1;
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
}
