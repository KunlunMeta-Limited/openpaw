//! 职责：Office v1严格DTO和Unicode、结构及传输边界。
//! 输入输出：浏览器JSON到已验证请求，运行文本到合法NDJSON。
//! 依赖：serde与冻结Office Schema。
//! 禁止职责：不进行执行、历史持久化或HTTP调用。
use crate::{HostError, HostResult};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// 用途：新版Office send请求；运行身份使用官方turnId。
/// 字段：版本、办公室、会话及文本轮次。
/// 来源去向：浏览器到Host接受入口。
/// 约束：无null、未知字段或任意工作目录。
/// 稳定性：严格兼容Office 02dbac4。
/// 关联：需求/设计REQ-OP-010；API-OFFICE-CHAT。
pub(super) struct ChatRequest {
    /// 用途：聊天协议版本。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：浏览器到严格校验器。
    /// 约束：必须等于1.0。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub version: String,
    /// 用途：办公室ID。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：浏览器到可信槽映射。
    /// 约束：官方ASCII标识且必须已授权。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub team_id: String,
    /// 用途：浏览器会话ID。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：浏览器到固定Session绑定。
    /// 约束：非空最多100 UTF-16单元；无控制符。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub conversation_id: String,
    /// 用途：完整历史及本次用户输入。
    /// 字段：Vec<Turn>，官方配置或协议字段。
    /// 来源去向：浏览器到原账本接受入口。
    /// 约束：最多10完整轮次与本次输入；不能导入工具授权。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub messages: Vec<Turn>,
    /// 用途：客户端持久轮次身份。
    /// 字段：非空字符串，最多100 UTF-16单元。
    /// 来源去向：Office到Host幂等关联。
    /// 约束：同键异内容冲突，不作为授权。
    /// 稳定性：Office 02dbac4官方字段。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub turn_id: String,
}
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// 用途：浏览器完整历史文本，不包含工具与授权事实。
/// 字段：user/assistant角色和纯文本。
/// 来源去向：请求校验后映射原Message。
/// 约束：不得覆盖原JSONL。
/// 稳定性：固定Office v1协议。
/// 关联：需求/设计REQ-OP-010；DES-OP-007。
pub(super) struct Turn {
    /// 用途：文本角色。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：浏览器到原Message角色。
    /// 约束：只允许user或assistant，按完整轮次交替。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub role: String,
    /// 用途：纯文本内容。
    /// 字段：String，官方配置或协议字段。
    /// 来源去向：浏览器历史到原JSONL。
    /// 约束：输入4000、回复50000、历史总量100000 UTF-16单元。
    /// 稳定性：T4新增，保持冻结Office协议。
    /// 关联：需求/设计REQ-OP-010；DES-OP-007。
    pub content: String,
}
impl ChatRequest {
    /// 功能：验证完整轮次、UTF-16长度和当前用户输入。
    /// 参数：self为严格反序列化后的请求。
    /// 返回：合法时成功。
    /// 错误：版本、标识、顺序或容量不合法。
    /// 状态：不创建会话或Run。
    /// 并发与取消：纯计算，无锁。
    /// 权限：办公室授权在共享Host另行校验。
    /// 关联：需求/设计REQ-OP-010；CHG-2026-0005 9.2。
    pub(super) fn validate(&self) -> HostResult<()> {
        let valid = self.version == "1.0"
            && scene_id(&self.team_id)
            && scene_id(&self.conversation_id)
            && valid_turn_id(&self.turn_id)
            && !self.messages.is_empty()
            && self.messages.len() <= 21
            && self.messages.len() % 2 == 1;
        if !valid {
            return Err(HostError::new(
                "invalid_input",
                "聊天版本、标识或历史轮数无效",
            ));
        }
        let last = self.messages.len() - 1;
        let mut total = 0;
        for (i, t) in self.messages.iter().enumerate() {
            let n = units(&t.content);
            if t.role != if i % 2 == 0 { "user" } else { "assistant" }
                || t.content.trim().is_empty()
                || n > if i % 2 == 0 { 4000 } else { 50000 }
            {
                return Err(HostError::new(
                    "invalid_input",
                    "聊天角色、顺序或文本长度无效",
                ));
            }
            if i != last {
                total += n;
            }
        }
        if total > 100000 {
            return Err(HostError::new("invalid_input", "聊天历史超过100000字符"));
        }
        Ok(())
    }
}
/// 功能：按照浏览器UTF-16单元计算字符限制。
/// 参数：s为Unicode文本。
/// 返回：UTF-16单元数。
/// 错误：无。
/// 状态：无。
/// 并发与取消：纯函数。
/// 权限：无。
/// 关联：需求/设计REQ-OP-010；API-OFFICE-CHAT。
pub(super) fn units(s: &str) -> usize {
    s.encode_utf16().count()
}
/// 功能：验证官方场景标识。
/// 参数：s为办公室或场景ID。
/// 返回：符合ASCII字符集和长度时为真。
/// 错误：无。
/// 状态：无。
/// 并发与取消：纯函数。
/// 权限：不是授权凭证。
/// 关联：需求/设计REQ-OP-010；API-OFFICE-MESSAGES。
pub(super) fn scene_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
/// 功能：按UTF-16限制截取，保留合法Unicode边界。
/// 参数：s为文本，max为最大单元数。
/// 返回：原文的合法前缀。
/// 错误：无。
/// 状态：不修改原文。
/// 并发与取消：纯函数。
/// 权限：无。
/// 关联：需求/设计REQ-OP-010/012。
pub(super) fn bounded(s: &str, max: usize) -> String {
    let mut n = 0;
    s.chars()
        .take_while(|c| {
            n += c.len_utf16();
            n <= max
        })
        .collect()
}
/// 功能：拒绝null、原型保留字段和过深结构。
/// 参数：value为待验证JSON。
/// 返回：协议值合法时为真。
/// 错误：无；假表示不符合协议。
/// 状态：只读。
/// 并发与取消：纯函数。
/// 权限：不将任意JSON用作配置。
/// 关联：需求/设计REQ-OP-010；API-OFFICE-MESSAGES。
pub(super) fn protocol_value(value: &Value) -> bool {
    fn walk(v: &Value, d: usize) -> bool {
        if d > 64 {
            return false;
        }
        match v {
            Value::Null => false,
            Value::Array(a) => a.iter().all(|v| walk(v, d + 1)),
            Value::Object(o) => o.iter().all(|(k, v)| {
                !matches!(k.as_str(), "__proto__" | "prototype" | "constructor") && walk(v, d + 1)
            }),
            _ => true,
        }
    }
    walk(value, 0)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
/// 用途：Office官方单POST四操作分派。
/// 字段：send/history/resume/cancel各自严格载荷。
/// 来源去向：Node转发到Host协议入口。
/// 约束：不默认为旧版send。
/// 稳定性：固定Office 02dbac4官方协议。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。
pub(super) enum Operation {
    /// 用途：可靠接受用户轮次并订阅真实运行。
    /// 字段：严格校验的官方请求载荷。
    /// 来源去向：Office Node转发至Host共享服务。
    /// 约束：办公室与会话范围必须一致。
    /// 稳定性：固定Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    Send(ChatRequest),
    /// 用途：只读分页查询原会话账本。
    /// 字段：严格校验的官方请求载荷。
    /// 来源去向：Office Node转发至Host共享服务。
    /// 约束：办公室与会话范围必须一致。
    /// 稳定性：固定Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    History(HistoryRequest),
    /// 用途：恢复已有轮次快照和增量订阅。
    /// 字段：严格校验的官方请求载荷。
    /// 来源去向：Office Node转发至Host共享服务。
    /// 约束：办公室与会话范围必须一致。
    /// 稳定性：固定Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    Resume(ControlRequest),
    /// 用途：显式取消并等待真实清理结束。
    /// 字段：严格校验的官方请求载荷。
    /// 来源去向：Office Node转发至Host共享服务。
    /// 约束：办公室与会话范围必须一致。
    /// 稳定性：固定Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    Cancel(ControlRequest),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// 用途：只读历史请求。
/// 字段：版本、办公室、会话、游标和页大小。
/// 来源去向：Node到历史投影。
/// 约束：不创建Session；limit为1到50。
/// 稳定性：固定Office 02dbac4官方协议。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。
pub(super) struct HistoryRequest {
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub version: String,
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub team_id: String,
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub conversation_id: String,
    #[serde(default)]
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub cursor: String,
    #[serde(default = "default_limit")]
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub limit: usize,
}
fn default_limit() -> usize {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// 用途：恢复或显式取消请求。
/// 字段：版本、办公室、会话及轮次。
/// 来源去向：Node到原Run订阅或取消服务。
/// 约束：查找必须绑定完整范围。
/// 稳定性：固定Office 02dbac4官方协议。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。
pub(super) struct ControlRequest {
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub version: String,
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub team_id: String,
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub conversation_id: String,
    /// 用途：官方聊天操作字段。
    /// 字段：按字段类型承载版本、范围或分页/轮次身份。
    /// 来源去向：Office Node到Host校验/查找入口。
    /// 约束：由Operation::scope严格校验，不作为授权。
    /// 稳定性：Office 02dbac4官方协议。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub turn_id: String,
}
impl Operation {
    /// 功能：校验操作身份和各自字段边界。
    /// 参数：self为严格反序列化后的请求。
    /// 返回：可信形状的客户端范围（授权仍由Host校验）。
    /// 错误：版本、标识、长度或操作字段非法。
    /// 状态：不创建数据。
    /// 并发与取消：纯计算。
    /// 权限：ID不是授权凭证。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。
    pub(super) fn scope(&self) -> HostResult<crate::ClientScope> {
        let (v, t, c) = match self {
            Self::Send(r) => {
                r.validate()?;
                (&r.version, &r.team_id, &r.conversation_id)
            }
            Self::History(r) => {
                if !(1..=50).contains(&r.limit) || units(&r.cursor) > 500 {
                    return Err(HostError::new("invalid_input", "历史分页参数无效"));
                }
                (&r.version, &r.team_id, &r.conversation_id)
            }
            Self::Resume(r) | Self::Cancel(r) => {
                if !valid_turn_id(&r.turn_id) {
                    return Err(HostError::new("invalid_input", "turnId无效"));
                }
                (&r.version, &r.team_id, &r.conversation_id)
            }
        };
        if v != "1.0" || !scene_id(t) || !scene_id(c) {
            return Err(HostError::new("invalid_input", "版本或会话标识无效"));
        }
        Ok(crate::ClientScope {
            namespace: "office".into(),
            slot_id: t.clone(),
            conversation_id: c.clone(),
        })
    }
}
fn valid_turn_id(s: &str) -> bool {
    !s.is_empty() && units(s) <= 100 && !s.chars().any(char::is_control)
}
