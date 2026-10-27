//! 功能：定义与入口、存储和供应商协议无关的模型上下文检查点。
//! 边界：只保存来源校验和替代模型消息，不表示真实用户或工具产出。
//! 禁止承担：不持有存储写入者，不承载Host协议或执行权限。
//! 关联：CHG-2026-0051。
use crate::Message;
use serde::{Deserialize, Serialize};

/// 功能：保存可验证的模型历史替代视图。
/// Stability: experimental；版本不兼容时从完整原件重建。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelContextCheckpoint {
    /// 功能：检查点数据格式版本。
    pub version: u32,
    /// 功能：所属独立会话身份。
    pub owner: String,
    /// 功能：生成时供应商身份。
    pub provider: String,
    /// 功能：生成时实际模型身份。
    pub model: String,
    /// 功能：生成时的Unix毫秒时间，仅作诊断元数据。
    pub created_at: u64,
    /// 功能：本会话已成功安装的摘要请求累计输入用量。
    pub summary_input_tokens: u32,
    /// 功能：本会话已成功安装的摘要请求累计输出用量。
    pub summary_output_tokens: u32,
    /// 功能：已提交原件前缀的稳定消息身份，旧消息使用位置身份。
    pub source_ids: Vec<String>,
    /// 功能：原件前缀序列化内容的完整性校验值，不用于认证。
    pub source_digest: String,
    /// 功能：替代模型消息及必要保留区；不作为真实历史展示。
    pub messages: Vec<Message>,
}
