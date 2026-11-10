//! 职责：适配Office官方聊天与业务消息，集中配置和边界校验。
//! 输入输出：可信本机配置、浏览器协议、共享Host事实。
//! 依赖：现有Host、reqwest与Office冻结协议。
//! 禁止职责：不创建第二套引擎、工具、账本或审批系统。
mod actions;
mod approvals;
/// 职责：真实成功文件的不可变版本与Office只读交付适配。
/// 输入输出：原工具结果、受限blob和单版本访问响应。
/// 依赖：原Root许可、Store、路径校验与Office官方Schema。
/// 禁止职责：不重放写入、不扫描工作区补造历史、不授予执行权限。
pub(crate) mod artifacts;
mod chat;
/// 职责：复用Office原HTTP连接池并校验响应作用域与决策结果。
/// 输入输出：可信Host请求与严格校验后的Office响应。
/// 依赖：原OfficeConfig、reqwest和冻结决策契约。
/// 禁止职责：不更改目的地址、不替代用户选择、不授予工具权限。
pub(crate) mod client;
mod details;
mod dto;
mod projection;
mod scene;
mod turns;
mod workspaces;

// 存储核对复用同一纯展示，不复制Office错误格式或信任浏览器自述。
pub(crate) use turns::display_in as chat_display;

use crate::{HostConfig, HostError, HostResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 用途：显式启用Office接入，密钥只保留环境变量名。
/// 字段：服务根地址、密钥引用、精确来源和办公室名称。
/// 来源去向：本机TOML到Host内部适配器。
/// 约束：只能引用已授权槽，浏览器不能改变配置。
/// 稳定性：T4新增可选配置，旧TOML缺省关闭。
/// 关联：需求/设计REQ-OP-010/011/012；DES-OP-007。
pub struct OfficeConfig {
    /// 用途：仅向人物当前任务窗口投影真实在途任务，并用短标题显示实际动作。
    /// 字段：bool，缺省false保持原快照任务计数与终态展示。
    /// 来源去向：可信Host配置到Office业务快照。
    /// 约束：完整历史仍由原账本查询；启用后页面计数仅代表当前窗口。
    /// 稳定性：新增可选配置，不增加Office协议字段。
    /// 关联：REQ-OP-012/014；CHG-2026-0007第12.113节。
    #[serde(default)]
    pub activity_window: bool,
    /// Office Fastify服务根；禁止用户信息、查询、片段和重定向。
    /// 用途：Office服务根地址。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：可信本机TOML到固定HTTP端点。
    /// 约束：无用户信息、query、fragment或非根路径。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub base_url: String,
    /// Office服务密钥的系统环境变量名；与模型密钥分离。
    /// 用途：Office服务凭据变量名。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：本机TOML到系统环境查询。
    /// 约束：必须使用PAWOFFICE_命名，秘密不输出。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub api_key_env: String,
    /// 精确浏览器origin（含协议和端口），不允许通配或null。
    /// 用途：允许浏览器精确来源。
    /// 字段：Vec<String>，官方配置或协议字段。
    /// 来源去向：本机TOML到请求来源校验。
    /// 约束：不允许通配或null；最多32项。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub allowed_origins: Vec<String>,
    /// 已授权槽到显示名称；未指定时显示槽ID。
    #[serde(default)]
    /// 用途：办公室显示名称映射。
    /// 字段：BTreeMap<String, String>，官方配置或协议字段。
    /// 来源去向：本机TOML到完整业务投影。
    /// 约束：仅已授权槽；名称最多80 UTF-16单元。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub names: BTreeMap<String, String>,
    /// 用途：等待页面创建后才发布的预授权槽。
    /// 字段：不重复的槽ID列表，缺省为空。
    /// 来源去向：可信本机TOML到创建分配器。
    /// 约束：只能引用Host现有槽；名称不是磁盘路径。
    /// 稳定性：可选字段，旧配置仍立即初始化全部槽。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.10节。
    #[serde(default)]
    pub creation_slots: Vec<String>,
}
impl OfficeConfig {
    /// 功能：校验Office连接和显式工具授权，不进行网络调用。
    /// 参数：host为所属Host配置。
    /// 返回：全部约束满足时成功。
    /// 错误：地址、来源、名称、槽或写工具非法。
    /// 状态：不读取或输出密钥内容。
    /// 并发与取消：启动阶段只读，不持运行资源锁。
    /// 权限：默认只读，显式写工具仍必须经过共享审批。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007；CHG-2026-0005。
    pub(crate) fn validate(&self, host: &HostConfig) -> HostResult<()> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| HostError::new("invalid_input", "Office服务地址无效"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || self.allowed_origins.is_empty()
            || self.allowed_origins.len() > 32
            || host.slots.len() > 100
            || self.api_key_env.is_empty()
            || !self
                .api_key_env
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || !self.api_key_env.starts_with("PAWOFFICE_")
        {
            return Err(HostError::new(
                "invalid_input",
                "Office地址、密钥引用或来源无效",
            ));
        }
        for origin in &self.allowed_origins {
            let u = reqwest::Url::parse(origin)
                .map_err(|_| HostError::new("invalid_input", "Office origin无效"))?;
            if !matches!(u.scheme(), "http" | "https")
                || u.origin().ascii_serialization() != *origin
            {
                return Err(HostError::new("invalid_input", "Office必须配置精确origin"));
            }
        }
        for (id, name) in &self.names {
            if !host.slots.contains_key(id) || name.is_empty() || dto::units(name) > 80 {
                return Err(HostError::new("invalid_input", "Office名称或槽无效"));
            }
        }
        let mut reserved = std::collections::BTreeSet::new();
        for slot in &self.creation_slots {
            if !host.slots.contains_key(slot) || !reserved.insert(slot) {
                return Err(HostError::new(
                    "invalid_input",
                    "创建预留槽必须唯一且已授权",
                ));
            }
        }
        for id in host.slots.keys() {
            if !dto::scene_id(id) {
                return Err(HostError::new("invalid_input", "Office槽不符合官方ID约束"));
            }
        }
        if host.tools.iter().any(|name| {
            !matches!(
                name.as_str(),
                "read_file"
                    | "clarify"
                    | "glob"
                    | "grep"
                    | "todo_list"
                    | "think"
                    | "web_search"
                    | "web_fetch"
                    | "write_file"
                    | "edit_file"
                    | "todo_add"
                    | "todo_update"
                    | "bash"
                    | "generate_image"
                    | "render_visual"
            )
        }) {
            return Err(HostError::new(
                "unsupported",
                "Office未开放此工具；写工具须原审批或显式持续产物策略，命令仍逐次审批并托管清理",
            ));
        }
        Ok(())
    }
}

/// 功能：组合原Office聊天与已授权只读版本路由。
/// 参数：host/config/address为原共享服务及监听地址。
/// 返回：同一个loopback监听器的Router。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn router(
    host: crate::Host,
    config: OfficeConfig,
    address: std::net::SocketAddr,
) -> axum::Router {
    chat::router(host.clone(), config.clone(), address)
        .merge(artifacts::router(host, config, address))
}
pub(crate) use projection::Coordinator;

/// 功能：复用原投影同步屏障确保问题的父会话存在于Office。
/// 参数：host/config/scope为可信绑定，transport为原协调器句柄。
/// 返回：同步且服务端确认会话存在后成功。
/// 错误：网络、协议、删除或对话尚未就绪，不能假称已提问。
/// 状态：只同步原投影，不新建对话或执行授权。
/// 并发与取消：由外层原问题期限及取消包裹，不持根锁。
/// 权限：只使用显式Office与原父会话范围。
/// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
pub(crate) async fn prepare_question(
    host: &crate::Host,
    config: &OfficeConfig,
    scope: &crate::ClientScope,
    transport: &crate::question::Transport,
) -> HostResult<()> {
    projection::sync_once(
        host,
        config,
        &scope.slot_id,
        &transport.client,
        false,
        transport.address,
    )
    .await?;
    let (_, data) = transport
        .client
        .post(serde_json::json!({"type":"office.read","teamId":scope.slot_id}))
        .await?;
    if !data["snapshot"]["conversations"]
        .as_array()
        .is_some_and(|v| v.iter().any(|c| c["id"] == scope.conversation_id))
    {
        return Err(HostError::new(
            "office_unavailable",
            "问题所属对话尚未完成同步",
        ));
    }
    Ok(())
}
