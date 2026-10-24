//! 职责：读取显式Host配置并校验允许根与启动参数。
//! 输入输出：本机TOML到运行配置；密钥只引用环境变量名。
//! 依赖：原Provider装配和路径安全，不使用全局CWD或home切换。
//! 禁止职责：不接受客户端任意目录、不回显密钥。
use crate::{HostError, HostResult};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 用途：显式本机启动配置。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct HostConfig {
    /// 用途：可信本机协作配置，缺省不启用独立子Agent。
    /// 字段：可选AgentsConfig，原TOML可省略。
    /// 来源去向：可信配置到共享运行装配。
    /// 约束：不接受浏览器、模型或.env授权角色能力。
    /// 稳定性：T6新增可选字段。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    #[serde(default)]
    pub agents: Option<AgentsConfig>,
    /// 用途：可选Office适配配置，缺省保持原健康服务。
    /// 字段：OfficeConfig或无。
    /// 来源去向：可信TOML到Host适配器。
    /// 约束：不得由浏览器改变；密钥只引用变量名。
    /// 稳定性：T4新增可选字段；Rust字面量需补None。
    /// 关联：需求/设计REQ-OP-010/011/012；DES-OP-007。
    #[serde(default)]
    pub office: Option<crate::OfficeConfig>,
    /// 用途：显式Host数据根，必须与所有工作根互不包含。
    /// 字段：PathBuf，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub home: PathBuf,
    /// 用途：允许根标识到既存绝对目录，同一真实根合并资源。
    /// 字段：BTreeMap<String, PathBuf>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub roots: BTreeMap<String, PathBuf>,
    /// 用途：办公室槽到允许根标识的固定映射。
    /// 字段：BTreeMap<String, String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub slots: BTreeMap<String, String>,
    #[serde(default = "four")]
    /// 用途：活动Run硬容量，默认四个，满时返回容量错误。
    /// 字段：usize，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub max_active_runs: usize,
    #[serde(default = "echo")]
    /// 用途：原Provider装配类型，不在Host重写选择逻辑。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub provider: String,
    /// 用途：可选显式模型，优先于原Provider默认模型。
    /// 字段：Option<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub model: Option<String>,
    /// 用途：可选模型端点，沿用原装配的无嵌入凭据校验。
    /// 字段：Option<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub base_url: Option<String>,
    /// 用途：模型密钥环境变量名，不能使用工具保留的系统变量。
    /// 字段：Option<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub api_key_env: Option<String>,
    /// 用途：未来SDK认证用户名变量名，须与密码成对配置。
    /// 字段：Option<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub basic_username_env: Option<String>,
    /// 用途：未来SDK认证密码变量名，值不进入诊断或工具环境。
    /// 字段：Option<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub basic_password_env: Option<String>,
    #[serde(default = "default_tools")]
    /// 用途：工具提议、审批、真正执行与结果的事实投影。
    /// 字段：Vec<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub tools: Vec<String>,
    #[serde(default = "total")]
    /// 用途：Run总预算秒数，到期仍等待实际清理。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub run_timeout_secs: u64,
    #[serde(default = "idle")]
    /// 用途：Provider流空闲预算秒数，与总预算分开。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub stream_idle_secs: u64,
    #[serde(default = "tool")]
    /// 用途：单工具预算秒数，到期通知原执行器取消。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub tool_timeout_secs: u64,
    #[serde(default = "approval")]
    /// 用途：人工审批有效秒数，过期保持拒绝。
    /// 字段：u64，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub approval_timeout_secs: u64,
}
fn four() -> usize {
    4
}
fn echo() -> String {
    "echo".into()
}
fn total() -> u64 {
    1800
}
fn idle() -> u64 {
    60
}
fn tool() -> u64 {
    300
}
fn approval() -> u64 {
    300
}
fn default_tools() -> Vec<String> {
    ["read_file", "glob", "grep", "todo_list", "think"]
        .map(str::to_owned)
        .to_vec()
}
impl HostConfig {
    /// 功能：读取并验证显式本机TOML。
    /// 参数：path为已授权配置文件。
    /// 返回：校验后的HostConfig。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读配置，不回显解析原文。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub fn load(path: &Path) -> HostResult<Self> {
        let (config, _) = Self::read_config(path)?;
        config.validate()?;
        Ok(config)
    }
    /// 功能：显式加载Host配置，并以共用模型环境填补未填写的模型字段。
    /// 参数：path为可信TOML；environment为只读.env与系统环境快照。
    /// 返回：完成原权限和路径校验的HostConfig，TOML明确值始终优先。
    /// 错误：读取、格式、路径或权限配置错误；不回显原始文件。
    /// 状态：不创建目录、不修改进程环境；原load入口语义保持不变。
    /// 并发与取消：启动时同步只读，无共享可变状态。
    /// 权限：不从.env接收工作根、工具或办公室授权。
    /// 关联：需求/设计REQ-OP-003、REQ-OP-009；DES-OP-002、DES-OP-006。
    pub fn load_with_model_environment(
        path: &Path,
        environment: &openpaw_llm::model_config::ModelEnvironment,
    ) -> HostResult<Self> {
        let (mut config, explicit_provider) = Self::read_config(path)?;
        if !explicit_provider {
            config.provider = environment.provider(None, "echo").to_owned();
        }
        if config.model.is_none() {
            config.model = environment.value("OPENPAW_MODEL").map(str::to_owned);
        }
        if config.base_url.is_none() {
            config.base_url = environment.value("OPENPAW_BASE_URL").map(str::to_owned);
        }
        if config.api_key_env.is_none() {
            config.api_key_env =
                openpaw_llm::model_config::ModelEnvironment::key_name(&config.provider)
                    .map(str::to_owned);
        }
        config.validate()?;
        Ok(config)
    }
    fn read_config(path: &Path) -> HostResult<(Self, bool)> {
        let text = std::fs::read_to_string(path)
            .map_err(|_| HostError::new("invalid_input", "无法读取Host配置"))?;
        let value: toml::Value = toml::from_str(&text)
            .map_err(|_| HostError::new("invalid_input", "Host配置格式或字段不合法"))?;
        // 必须在serde应用echo默认值之前记录是否明确填写，避免默认值覆盖.env。
        let explicit_provider = value.get("provider").is_some();
        let config: Self = value
            .try_into()
            .map_err(|_| HostError::new("invalid_input", "Host配置格式或字段不合法"))?;
        Ok((config, explicit_provider))
    }
    /// 功能：校验目录、槽、预算、工具和凭据引用。
    /// 参数：self为待启动配置。
    /// 返回：合法返回成功。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不创建目录、不修改环境变量。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub fn validate(&self) -> HostResult<()> {
        if !self.home.is_absolute()
            || self.roots.is_empty()
            || self.slots.is_empty()
            || self.max_active_runs == 0
            || self.max_active_runs > 256
            || [
                self.run_timeout_secs,
                self.stream_idle_secs,
                self.tool_timeout_secs,
                self.approval_timeout_secs,
            ]
            .contains(&0)
        {
            return Err(HostError::new(
                "invalid_input",
                "配置需要绝对home、授权根、办公室槽和正数预算/容量（最多256）",
            ));
        }
        for (id, path) in &self.roots {
            valid_id(id)?;
            if !path.is_absolute() || !path.is_dir() {
                return Err(HostError::new(
                    "invalid_input",
                    "授权根必须是已存在绝对目录",
                ));
            }
        }
        for (id, root) in &self.slots {
            valid_id(id)?;
            if !self.roots.contains_key(root) {
                return Err(HostError::new("invalid_input", "办公室引用未知根"));
            }
        }
        for name in [
            &self.api_key_env,
            &self.basic_username_env,
            &self.basic_password_env,
        ]
        .into_iter()
        .flatten()
        {
            let system = [
                "PATH",
                "HOME",
                "USER",
                "LANG",
                "LC_ALL",
                "TERM",
                "TMPDIR",
                "TEMP",
                "TMP",
                "SYSTEMROOT",
                "WINDIR",
                "COMSPEC",
                "PATHEXT",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
            ];
            if name.is_empty()
                || system.contains(&name.to_ascii_uppercase().as_str())
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(HostError::new(
                    "invalid_input",
                    "凭据变量名不合法或与工具系统环境冲突",
                ));
            }
        }
        if let Some(office) = &self.office {
            office.validate(self)?;
        }
        let allowed = [
            "read_file",
            "glob",
            "grep",
            "todo_list",
            "think",
            "sleep",
            "write_file",
            "edit_file",
            "notebook_edit",
            "todo_add",
            "todo_update",
            "bash",
            "web_search",
            "web_fetch",
        ];
        if self.tools.iter().any(|t| !allowed.contains(&t.as_str())) {
            return Err(HostError::new("unsupported", "工具白名单包含未支持项"));
        }
        if let Some(agents) = &self.agents {
            agents.validate(self)?;
        }
        let user = self
            .basic_username_env
            .as_ref()
            .and_then(|n| std::env::var(n).ok())
            .filter(|s| !s.is_empty());
        let pass = self
            .basic_password_env
            .as_ref()
            .and_then(|n| std::env::var(n).ok())
            .filter(|s| !s.is_empty());
        if self.basic_username_env.is_some() != self.basic_password_env.is_some()
            || user.is_some() != pass.is_some()
            || (self.basic_username_env.is_some() && user.is_none())
        {
            return Err(HostError::new(
                "invalid_input",
                "Basic用户名和密码必须同时配置且有效",
            ));
        }
        Ok(())
    }
}
/// 功能：验证绑定标识的非空、长度及控制符。
/// 参数：id为可信适配器提供的标识。
/// 返回：合法返回成功。
/// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
/// 状态：无状态变化。
/// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
/// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) fn valid_id(id: &str) -> HostResult<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(HostError::new("invalid_input", "标识为空、过长或含控制符"))
    } else {
        Ok(())
    }
}

/// 用途：一层主子协作的显式本机限额与角色工具配置。
/// 字段：启用开关、子运行/委派/模型总预算及各角色缩小后的工具清单。
/// 来源去向：可信TOML到Host；不包含密钥、模型或根覆盖。
/// 约束：默认关闭，角色工具为Host白名单子集，写工具仍逐次审批。
/// 稳定性：T6可选配置，旧配置无需迁移。
/// 关联：需求/设计REQ-OP-022；CHG-2026-0007第9.7.1节。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentsConfig {
    /// 用途：显式启用独立主子协作。
    /// 字段：bool，显式启用独立主子协作。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub enabled: bool,
    /// 用途：每组最多1至2个活动子运行。
    /// 字段：usize，每组最多1至2个活动子运行。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub max_active_children: usize,
    /// 用途：每主Run最多1至12次委派。
    /// 字段：usize，每主Run最多1至12次委派。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub max_delegations: usize,
    /// 用途：共享Provider调用次数预算，覆盖主Run及全部独立子Run。
    /// 字段：u64，共享Provider调用次数预算。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub max_model_requests: u64,
    /// 用途：共享输出token预留预算，覆盖主Run及全部独立子Run。
    /// 字段：u64，共享输出token预留预算。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub max_output_tokens: u64,
    /// 用途：按固定角色缩小的工具白名单。
    /// 字段：BTreeMap<String, Vec<String>>，按固定角色缩小的工具白名单。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub tools: BTreeMap<String, Vec<String>>,
}
impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_active_children: 2,
            max_delegations: 12,
            // 真实联网协作会为每位成员产生多轮“搜索—抓取—核对—交付”。
            // 64 次会在子Run交付前耗尽，导致父Run把已完成资料标为失败；
            // 仍由配置硬上限约束，不改变单Run的AgentLoop预算或工具权限。
            max_model_requests: 384,
            max_output_tokens: 524288,
            tools: BTreeMap::new(),
        }
    }
}
impl AgentsConfig {
    /// 功能：校验协作容量、固定角色和缩小后的工具权限。
    /// 参数：host为原Host配置，不从模型输入补权限。
    /// 返回：合法返回成功。
    /// 错误：非法限额、未知角色/工具或重复工具返回invalid_input。
    /// 状态：只读，不改变原配置或全局容量。
    /// 并发与取消：启动同步校验，无等待。
    /// 权限：所有角色工具必须是Host白名单子集。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    fn validate(&self, host: &HostConfig) -> HostResult<()> {
        if !(1..=2).contains(&self.max_active_children)
            || !(1..=12).contains(&self.max_delegations)
            || self.max_model_requests == 0
            || self.max_output_tokens == 0
            || (self.enabled && host.max_active_runs < 2)
        {
            return Err(HostError::new(
                "invalid_input",
                "协作需要至少2个全局运行槽及合法正数限额",
            ));
        }
        for (id, tools) in &self.tools {
            if crate::agents::role(id).is_none()
                || tools.iter().any(|t| !host.tools.contains(t))
                || tools
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != tools.len()
            {
                return Err(HostError::new(
                    "invalid_input",
                    "角色未知、工具重复或超出Host白名单",
                ));
            }
        }
        Ok(())
    }
    /// 功能：取得固定角色本轮真正允许的工具。
    /// 参数：role为可信目录ID，host为全局白名单。
    /// 返回：显式缩小清单或Host中的只读子集。
    /// 错误：无；未知角色在配置/委派校验处拒绝。
    /// 状态：返回副本，不修改注册表。
    /// 并发与取消：纯计算，无等待。
    /// 权限：主角色默认使用原Host清单；子角色默认不含写入或命令。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn allowed(&self, role: &str, host: &[String]) -> Vec<String> {
        self.tools.get(role).cloned().unwrap_or_else(|| {
            host.iter()
                .filter(|t| {
                    role == "lead"
                        || matches!(
                            t.as_str(),
                            "read_file"
                                | "glob"
                                | "grep"
                                | "todo_list"
                                | "think"
                                | "web_search"
                                | "web_fetch"
                        )
                })
                .cloned()
                .collect()
        })
    }
}
