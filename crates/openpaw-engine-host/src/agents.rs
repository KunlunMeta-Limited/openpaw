//! 职责：集中定义可信角色和常驻人物对应关系，供运行与展示共同复用。
//! 输入输出：可信角色标识到固定身份、职责和原成员标识。
//! 依赖：Host配置；不依赖浏览器输入或修改Office协议。
//! 禁止职责：不创建模型、会话、工具权限或第二套执行循环。

/// 用途：可信角色目录的一项，不代表已有活动Agent。
/// 字段：角色、原成员、人物标识及可公开的姓名职责。
/// 来源去向：固定目录到Host运行装配与Office适配。
/// 约束：仅匹配配置中已有角色，不接收任意客户端别名。
/// 稳定性：Host内部角色定义；原assistant/files/search/command标识保留。
/// 关联：需求/设计REQ-OP-021/022；CHG-2026-0007第9.6节。
pub(crate) struct Role {
    /// 用途：可信角色标识。
    /// 字段：&'static str，可信角色标识。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) id: &'static str,
    /// 用途：保留兼容的业务成员标识。
    /// 字段：&'static str，保留兼容的业务成员标识。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) employee: &'static str,
    /// 用途：Office官方人物标识。
    /// 字段：&'static str，Office官方人物标识。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) actor: &'static str,
    /// 用途：官方人物公开姓名。
    /// 字段：&'static str，官方人物公开姓名。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) name: &'static str,
    /// 用途：角色职责，不构成授权。
    /// 字段：&'static str，角色职责，不构成授权。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) duty: &'static str,
}

/// 用途：一主五子的唯一角色目录。
/// 字段：六项固定身份，索引顺序与官方人物名册一致。
/// 来源去向：原Office名册审查到共享Host。
/// 约束：常驻人物可查询不代表已启用独立Agent。
/// 稳定性：Host内部常量。
/// 关联：需求/设计REQ-OP-021/022；DES-OP-011。
pub(crate) const ROLES: [Role; 6] = [
    Role {
        id: "lead",
        employee: "assistant",
        actor: "office-lead",
        name: "王明",
        duty: "任务统筹、分工与结果汇总",
    },
    Role {
        id: "research",
        employee: "command",
        actor: "code-agent",
        name: "李研",
        duty: "信息检索与技术背景核对",
    },
    Role {
        id: "organize",
        employee: "files",
        actor: "file-agent",
        name: "周理",
        duty: "项目资料查看与整理",
    },
    Role {
        id: "draft",
        employee: "draft",
        actor: "app-agent",
        name: "陈书",
        duty: "报告草稿与交付物撰写",
    },
    Role {
        id: "review",
        employee: "review",
        actor: "review-agent",
        name: "刘市",
        duty: "需求和验收覆盖核对",
    },
    Role {
        id: "audit",
        employee: "search",
        actor: "data-agent",
        name: "赵审",
        duty: "来源、结论与风险复核",
    },
];

/// 功能：按可信角色ID查找目录项。
/// 参数：id为经过范围校验后的角色标识。
/// 返回：固定目录中的角色或无。
/// 错误：无；未知角色不创建隐式映射。
/// 状态：只读。
/// 并发与取消：无锁，无等待。
/// 权限：返回角色不授予执行或查询权限。
/// 关联：需求/设计REQ-OP-021/022；DES-OP-011/012。
pub(crate) fn role(id: &str) -> Option<&'static Role> {
    ROLES.iter().find(|role| role.id == id)
}

/// 功能：从本机实际配置判断角色是否具有当前可用能力。
/// 参数：role为可信固定目录项，config为原Host配置。
/// 返回：独立协作显式启用或对应功能工具存在时为真。
/// 错误：无；未知角色不隐式启用。
/// 状态：只读，不调用模型、不新建任务。
/// 并发与取消：纯计算，无锁及等待。
/// 权限：只用于状态说明，真实工具入口仍二次校验白名单和审批。
/// 关联：需求/设计REQ-OP-021/022；DES-OP-011/012。
pub(crate) fn enabled(role: &Role, config: &crate::HostConfig) -> bool {
    if config.agents.as_ref().is_some_and(|a| a.enabled) {
        return true;
    }
    match role.id {
        "lead" => true,
        "organize" => config
            .tools
            .iter()
            .any(|t| matches!(t.as_str(), "read_file" | "write_file" | "edit_file")),
        "audit" => config
            .tools
            .iter()
            .any(|t| matches!(t.as_str(), "glob" | "grep" | "web_search" | "web_fetch")),
        "research" => config.tools.iter().any(|t| t == "bash"),
        _ => false,
    }
}
