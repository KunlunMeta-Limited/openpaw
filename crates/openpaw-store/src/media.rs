//! 功能：从已提交且有效的会话历史建立附件查找快照。
//! 边界：调用者先完成事务回放及回退裁剪，不扫描原始 JSONL。
//! 关联：CHG-2026-0026。
use openpaw_core::{InlineMedia, Message};
use std::collections::HashMap;

/// 功能：建立当前会话的附件编号索引。
/// 参数：messages 为有效历史，不含暂存隐藏尾部。
/// 返回：编号对应的原件；重复编号保留首次内容。
/// 错误：无；媒体合法性在读取发送时重新校验。
/// 状态：无文件写入。
/// 并发与取消：同步只读。
/// 权限：不接受任意会话路径。
/// 关联：CLI/Host read_attachment。
pub fn attachment_snapshot(messages: &[Message]) -> HashMap<String, InlineMedia> {
    let mut snapshot = HashMap::new();
    for media in openpaw_core::media::message_media(messages) {
        snapshot.entry(media.id.clone()).or_insert(media);
    }
    snapshot
}
