//! 职责：适配Office官方聊天与业务消息，集中配置和边界校验。
//! 输入输出：可信本机配置、浏览器协议、共享Host事实。
//! 依赖：现有Host、reqwest与Office冻结协议。
//! 禁止职责：不创建第二套引擎、工具、账本或审批系统。
mod chat;
mod client;
mod dto;
mod projection;

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
}
impl OfficeConfig {
    /// 功能：校验Office连接和只读授权，不进行网络调用。
    /// 参数：host为所属Host配置。
    /// 返回：全部约束满足时成功。
    /// 错误：地址、来源、名称、槽或写工具非法。
    /// 状态：不读取或输出密钥内容。
    /// 并发与取消：启动阶段只读，不持运行资源锁。
    /// 权限：Office T4仅开放已审核的读取工具。
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
        for id in host.slots.keys() {
            if !dto::scene_id(id) {
                return Err(HostError::new("invalid_input", "Office槽不符合官方ID约束"));
            }
        }
        if host.tools.iter().any(|name| {
            !matches!(
                name.as_str(),
                "read_file" | "glob" | "grep" | "todo_list" | "think"
            )
        }) {
            return Err(HostError::new(
                "unsupported",
                "Office T4不开放写入或执行工具",
            ));
        }
        Ok(())
    }
}

pub(crate) use chat::router;
pub(crate) use projection::Coordinator;
