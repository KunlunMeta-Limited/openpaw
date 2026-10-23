//! 职责：定义协议无关的作用域、运行视图及安全错误。
//! 输入输出：可信适配器与共享Host之间的Rust数据。
//! 依赖：复用core消息和serde；不保存第二份会话账本。
//! 禁止职责：不声明Office或OpenCode的HTTP DTO。
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// 用途：安全的稳定宿主错误。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct HostError {
    /// 用途：稳定错误分类，供适配器映射，不包含内部原始诊断。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub kind: String,
    /// 用途：可安全展示的中文说明，不含凭据或原始配置。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub message: String,
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}
impl std::error::Error for HostError {}
/// 用途：统一宿主结果别名。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub type HostResult<T> = Result<T, HostError>;
impl HostError {
    /// 功能：构造安全分类错误。
    /// 参数：kind与message为安全分类及可展示文本。
    /// 返回：错误对象。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不写入状态；不得放原始凭据。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
    /// 功能：将内部存储错误降为安全对外诊断。
    /// 参数：内部错误仅作分类依据，不向客户端回显。
    /// 返回：storage_unavailable错误。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不暴露文件内容或密钥。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn storage(_: impl std::fmt::Display) -> Self {
        Self::new(
            "storage_unavailable",
            "宿主存储操作失败，请检查服务端数据与权限",
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// 用途：可信客户端作用域。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct ClientScope {
    /// 用途：可信适配器命名空间，必须与会话绑定完全一致。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub namespace: String,
    /// 用途：已配置办公室槽，同命名空间同时至多一个活动Run。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub slot_id: String,
    /// 用途：客户端稳定会话标识，不作为认证凭据。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub conversation_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：原Session与客户端的固定绑定。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct SessionBinding {
    /// 用途：可信客户端范围，访问时逐字段校验。
    /// 字段：ClientScope，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub scope: ClientScope,
    /// 用途：原JSONL会话ID，不能据此跳过作用域检查。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub session_id: String,
    /// 用途：显式允许根的配置标识，既有Session禁止切换。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub root_id: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// 用途：宿主运行生命周期状态。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub enum RunState {
    /// 用途：输入尚未完整保存，禁止启动执行。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Accepting,
    /// 用途：输入与接受元数据已可靠保存，可开始独立引擎。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Accepted,
    /// 用途：引擎运行中，工具running另由真正执行事实决定。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Running,
    /// 用途：取消已请求，实际工具/写入仍须完成清理。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Cancelling,
    /// 用途：引擎完成且事实保存成功。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Completed,
    /// 用途：运行或持久化失败，不以模型文本掩盖。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Failed,
    /// 用途：人工取消且清理已结束。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Cancelled,
    /// 用途：总/工具预算到期且清理已结束。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    TimedOut,
    /// 用途：重启发现未完运行，不自动重放工具。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Interrupted,
}
impl RunState {
    /// 功能：判断运行是否已完成实际清理。
    /// 参数：self为当前状态。
    /// 返回：终态返回真。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：cancelling仍返回假。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut | Self::Interrupted
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：从原账本和宿主事实生成的运行视图。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct RunView {
    /// 用途：Host分配的稳定Run标识，每次调用独立。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub run_id: String,
    /// 用途：原JSONL会话ID，不能据此跳过作用域检查。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub session_id: String,
    /// 用途：已提交的运行或工具阶段，取消清理期间不能提前终结。
    /// 字段：RunState，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub state: RunState,
    /// 用途：单调持久事实序号，提交后才发布通知。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub revision: u64,
    /// 用途：供展示的累计文本；跨有文字的assistant消息补足段落空行，原事实仍来自JSONL。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub text: String,
    /// 用途：安全错误对象，失败不能伪报完成。
    /// 字段：Option<HostError>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub error: Option<HostError>,
    #[serde(default)]
    /// 用途：工具提议、审批、真正执行与结果的事实投影。
    /// 字段：Vec<ToolView>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub tools: Vec<ToolView>,
    #[serde(default)]
    /// 用途：活动文本缓冲已达上限，需要等待账本提交后重取。
    /// 字段：bool，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub text_truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：精确绑定且可一次消费的持久审批。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct ApprovalView {
    /// 用途：持久审批标识，与菜单展示尝试ID分离。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub approval_id: String,
    /// 用途：原core最终输入绑定，完整JSON比较而非摘要授权。
    /// 字段：openpaw_core::ToolCallBinding，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub binding: openpaw_core::ToolCallBinding,
    /// 用途：审批持久状态，仅pending可以首次决定。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub status: String,
    /// 用途：批准是否已被执行包装器原子领取，领取后不能重复执行。
    /// 字段：bool，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub consumed: bool,
    /// 用途：单调持久事实序号，提交后才发布通知。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub revision: u64,
    /// 用途：审批截止的UTC毫秒时间，到期不批准。
    /// 字段：i64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：同一协调序列下取得的一致视图。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct HostSnapshot {
    /// 用途：单调持久事实序号，提交后才发布通知。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub revision: u64,
    /// 用途：当前作用域的Run列表，不包含其他会话。
    /// 字段：Vec<RunView>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub runs: Vec<RunView>,
    /// 用途：当前作用域未决定审批，重订阅可恢复。
    /// 字段：Vec<ApprovalView>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub pending_approvals: Vec<ApprovalView>,
}
#[derive(Debug, Clone)]
/// 用途：非权威的有界刷新通知。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub enum HostNotice {
    /// 用途：持久事实提交后发出的revision刷新提示。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Changed { revision: u64 },
    /// 用途：可合并丢弃的文本刷新提示，需重取当前视图。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Text,
}
#[derive(Debug, Clone)]
/// 用途：六类只读查询选择。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub enum QueryKind {
    /// 用途：原JSONL完整消息历史，不受浏览器截断覆盖。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    History,
    /// 用途：原JSONL工具调用及结果消息。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    ToolResults,
    /// 用途：原显式技能Store的严格只读列表。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Skills,
    /// 用途：原目标Store的严格只读列表，不调度目标。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Goals,
    /// 用途：复用原Todo结构化读取。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    Todo,
    /// 用途：授权根内最多256KiB普通UTF8文本预览。
    /// 字段：本变体携带的中性状态或查询参数。
    /// 来源去向：共享Host与可信调用方。
    /// 约束：不能借此跳过作用域或取消清理。
    /// 稳定性：T3新增。
    /// 关联：需求/设计DES-OP-006。
    File { path: String },
}
#[derive(Debug, Clone)]
/// 用途：带会话范围和分页的查询参数。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct QueryRequest {
    /// 用途：原JSONL会话ID，不能据此跳过作用域检查。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub session_id: String,
    /// 用途：稳定错误分类，供适配器映射，不包含内部原始诊断。
    /// 字段：QueryKind，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub kind: QueryKind,
    /// 用途：分页起点，从零开始，越过末尾返回空页。
    /// 字段：usize，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub offset: usize,
    /// 用途：分页数量，零默认五项，最多五十项。
    /// 字段：usize，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub limit: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：有界只读查询结果。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct QueryPage {
    /// 用途：原存储的只读投影，长展示文本有界。
    /// 字段：Vec<Value>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub items: Vec<Value>,
    /// 用途：下一页起点，空值表示已经读完。
    /// 字段：Option<usize>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub next_cursor: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// 用途：原工具调用的真实阶段投影。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct ToolView {
    /// 用途：原模型工具调用ID，在同一Run内唯一。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub call_id: String,
    /// 用途：原工具注册名，不创造第二套工具体系。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub tool_name: String,
    /// 用途：已提交的运行或工具阶段，取消清理期间不能提前终结。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub state: String,
}

/// 用途：实时delta与原账本共用的消息展示累积器，不改写模型事实。
/// 字段：text为已显示前缀，boundary延迟消息分隔，limit为可选UTF-8字节预算，truncated记录溢出。
/// 来源去向：可靠assistant消息边界/文字块到Host展示视图。
/// 约束：消息内部原样保留；空消息不造回复；跨消息只补足两换行。
/// 稳定性：Host内部实现，不改变JSONL、公共DTO或数据库版本。
/// 关联：需求/设计REQ-OP-010；DES-OP-007、DES-OP-009。
#[derive(Default)]
pub(crate) struct RunText {
    text: String,
    boundary: bool,
    limit: Option<usize>,
    truncated: bool,
}
impl RunText {
    /// 功能：创建有界实时展示缓冲。
    /// 参数：limit为包含分隔符的UTF-8字节上限。
    /// 返回：空累积器。
    /// 错误：无。
    /// 状态：溢出后保留已接受前缀，不拆开UTF-8字符。
    /// 并发与取消：无内部锁，由调用方原live锁保护。
    /// 权限：仅Host内部，不产生或授权工具操作。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub(crate) fn bounded(limit: usize) -> Self {
        Self {
            limit: Some(limit),
            ..Self::default()
        }
    }
    /// 功能：按同一规则追加一个消息内的文字块或流分块。
    /// 参数：chunk为模型原文，不做trim或Markdown修补。
    /// 返回：无。
    /// 错误：无；超过预算标记truncated并拒绝该块及后续块。
    /// 状态：只在新消息首个非换行字符到来时补足段落边界。
    /// 并发与取消：同步内存更新；调用方管理live锁和通知。
    /// 权限：仅展示，不修改原消息或执行上下文。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007、DES-OP-009。
    pub(crate) fn append(&mut self, chunk: &str) {
        if chunk.is_empty() || self.truncated {
            return;
        }
        let leading = chunk.bytes().take_while(|b| *b == b'\n').count();
        // 前导换行可能跨多个delta；在看到正文前保持待分隔，才能与整块恢复一致。
        let body_started = leading < chunk.len();
        let padding = if self.boundary && body_started {
            let trailing = self
                .text
                .bytes()
                .rev()
                .take_while(|b| *b == b'\n')
                .take(2)
                .count();
            2usize.saturating_sub(trailing.saturating_add(leading))
        } else {
            0
        };
        if self.limit.is_some_and(|limit| {
            self.text
                .len()
                .saturating_add(padding)
                .saturating_add(chunk.len())
                > limit
        }) {
            self.truncated = true;
            return;
        }
        for _ in 0..padding {
            self.text.push('\n');
        }
        self.text.push_str(chunk);
        if body_started {
            self.boundary = false;
        }
    }
    /// 功能：在可靠assistant消息记录完成后标记下一条文字边界。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：不立即追加字符；空、Thinking或纯工具消息不会制造空段落。
    /// 并发与取消：调用方先等待事实保存成功，再在原live锁下调用。
    /// 权限：仅Host投影，不修改Provider上下文。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub(crate) fn end_message(&mut self) {
        self.boundary = !self.text.is_empty();
    }
    /// 功能：读取已接受的展示文本。
    /// 参数：无。
    /// 返回：当前前缀的借用。
    /// 错误：无。
    /// 状态：只读，不增加尾随分隔符。
    /// 并发与取消：调用方保证共享访问同步。
    /// 权限：调用方须先校验Run范围。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
    /// 功能：读取实时字节预算溢出标记。
    /// 参数：无。
    /// 返回：是否曾拒绝溢出分块。
    /// 错误：无。
    /// 状态：只读；终态可由原账本恢复完整文本。
    /// 并发与取消：调用方保证共享访问同步。
    /// 权限：仅Host展示状态。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod text_tests {
    use super::RunText;

    #[test]
    fn message_boundaries_preserve_chunks_and_original_whitespace() {
        for (first, second, expected) in [
            ("说明。", "# 标题", "说明。\n\n# 标题"),
            ("说明。\n", "\n# 标题", "说明。\n\n# 标题"),
            ("说明。\n\n", "\n# 标题", "说明。\n\n\n# 标题"),
            ("", "# 标题", "# 标题"),
        ] {
            let mut persisted = RunText::default();
            persisted.append(first);
            persisted.end_message();
            persisted.append(second);
            assert_eq!(persisted.text(), expected);
            let mut live = RunText::bounded(1024);
            for c in first.chars() {
                live.append(&c.to_string());
            }
            live.end_message();
            live.append("");
            live.end_message();
            for c in second.chars() {
                live.append(&c.to_string());
            }
            live.end_message();
            assert_eq!(live.text(), persisted.text());
        }
        let mut text = RunText::default();
        text.append("块一");
        text.append("块二");
        text.end_message();
        text.end_message();
        text.append("块三");
        assert_eq!(text.text(), "块一块二\n\n块三");
    }

    #[test]
    fn byte_limit_counts_separator_and_keeps_utf8_prefix() {
        let mut text = RunText::bounded(8);
        text.append("中");
        text.end_message();
        text.append("文");
        assert_eq!(text.text(), "中\n\n文");
        assert!(!text.truncated());
        text.append("!");
        text.append("ignored");
        assert_eq!(text.text(), "中\n\n文");
        assert!(text.truncated());
        let mut overflow = RunText::bounded(7);
        overflow.append("中");
        overflow.end_message();
        overflow.append("文");
        assert_eq!(overflow.text(), "中");
        assert!(overflow.truncated());
    }
}
