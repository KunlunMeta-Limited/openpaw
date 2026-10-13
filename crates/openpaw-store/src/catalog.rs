//! 功能：在指定 home 内发现并读取持久会话快照。
//! 参数：home 与合法会话 ID。
//! 返回：已提交/可见历史与文件更新时间。
//! 错误：路径、权限和投影错误均脱敏报告。
//! 状态：仅读 JSONL。
//! 并发与取消：容忍既有 Reader 支持的 torn tail。
//! 权限：拒绝外部链接和文件名/meta 身份不符。
//! 关联：CLI 与 Host 会话目录。
use crate::{
    transcript::{self, Transcript},
    SessionReader,
};
use openpaw_core::SessionMeta;
use std::path::{Path, PathBuf};

/// 功能：报告脱敏目录读取或索引刷新错误。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CatalogError(pub String);

/// 功能：保存单会话持久快照、投影和文件信息。
pub struct Snapshot {
    /// 首条持久 meta 身份和模型信息。
    pub meta: SessionMeta,
    /// 纯重放得到的已提交历史和附加状态。
    pub transcript: Transcript,
    /// 范围校验后的 transcript 路径。
    pub path: PathBuf,
    /// 持久文件的修改时间；扫描不更新此值。
    pub updated_at: std::time::SystemTime,
    /// Reader 容忍的 torn tail 行数。
    pub torn_tail_lines: usize,
    /// 只读原始事件，用于入口元数据适配。
    pub events: Vec<openpaw_core::SessionEvent>,
}

/// 功能：校验安全的单文件会话标识。
/// 参数：id 文本。
/// 返回：非空、最多 128 字节且仅 ASCII 字母数字/_/- 时为 true。
/// 错误：无。
/// 状态：纯校验。
/// 并发与取消：同步无锁。
/// 权限：拒绝路径分隔和 traversal。
/// 关联：discover/read。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// 功能：发现指定 home 内 JSONL 身份。
/// 参数：home 根目录。
/// 返回：按 ID 排序的列表；缺目录为空。
/// 错误：非法文件名、链接目录或权限失败返回 CatalogError。
/// 状态：仅读目录。
/// 并发与取消：同步文件 I/O，不执行回合恢复。
/// 权限：仅扫描指定 sessions 目录。
/// 关联：CLI list/search 和 Host summaries。
pub fn discover(home: &Path) -> Result<Vec<String>, CatalogError> {
    let dir = home.join("sessions");
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(m) if m.file_type().is_symlink() => {
            return Err(CatalogError("sessions directory must not be a link".into()))
        }
        Err(_) => {
            return Err(CatalogError(
                "cannot inspect sessions directory permissions".into(),
            ))
        }
        _ => {}
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|_| CatalogError("cannot read sessions directory".into()))?;
    let mut ids = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|_| CatalogError("cannot read session directory entry".into()))?
            .path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|id| valid_id(id))
            .ok_or_else(|| CatalogError("invalid session filename".into()))?;
        ids.push(id.to_string());
    }
    ids.sort();
    Ok(ids)
}

/// 功能：读取并校验已提交会话快照。
/// 参数：home、合法 id。
/// 返回：Snapshot。
/// 错误：路径、meta 身份、JSONL 和已登记投影错误均脱敏返回。
/// 状态：不截断或追加 JSONL。
/// 并发与取消：同步读取；容忍 Reader 的 torn tail。
/// 权限：拒绝目录外路径和文件/目录链接。
/// 关联：SessionReader、transcript::replay、SQLite 刷新。
pub fn read(home: &Path, id: &str) -> Result<Snapshot, CatalogError> {
    if !valid_id(id) {
        return Err(CatalogError("invalid session ID".into()));
    }
    let dir = home.join("sessions");
    let path = dir.join(format!("{id}.jsonl"));
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| CatalogError(format!("session {id}: file missing or unreadable")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CatalogError(format!(
            "session {id}: transcript must be a regular file"
        )));
    }
    if std::fs::symlink_metadata(&dir)
        .map_err(|_| CatalogError("cannot inspect sessions directory".into()))?
        .file_type()
        .is_symlink()
        || path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            != dir.canonicalize().ok()
    {
        return Err(CatalogError(format!(
            "session {id}: transcript outside catalog"
        )));
    }
    let data = SessionReader::read(&path).map_err(|_| {
        CatalogError(format!(
            "session {id}: cannot read valid JSONL; inspect permissions or damaged records"
        ))
    })?;
    if data.meta.id != id {
        return Err(CatalogError(format!(
            "session {id}: meta ID does not match filename"
        )));
    }
    let transcript = transcript::replay(id, &data.events).map_err(|_| {
        CatalogError(format!(
            "session {id}: invalid committed transcript; inspect records without deleting index.db"
        ))
    })?;
    let updated_at = metadata
        .modified()
        .map_err(|_| CatalogError(format!("session {id}: cannot read file timestamp")))?;
    Ok(Snapshot {
        meta: data.meta,
        transcript,
        path,
        updated_at,
        torn_tail_lines: data.torn_tail_lines,
        events: data.events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_rejects_wrong_identity_and_paths_without_modifying_files() {
        let home = tempfile::tempdir().unwrap();
        assert!(discover(home.path()).unwrap().is_empty());
        assert!(read(home.path(), "../outside").is_err());
        let meta = openpaw_core::SessionMeta::new("echo-1", "echo");
        let path = home.path().join("sessions/wrong-id.jsonl");
        let mut writer = crate::SessionWriter::create(&path).unwrap();
        writer
            .append(&openpaw_core::SessionEvent::Meta(meta))
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(read(home.path(), "wrong-id").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[cfg(unix)]
    #[test]
    fn catalog_does_not_follow_external_transcript_links() {
        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("sessions")).unwrap();
        let target = external.path().join("private.jsonl");
        std::fs::write(&target, b"secret invalid JSONL").unwrap();
        std::os::unix::fs::symlink(&target, home.path().join("sessions/linked.jsonl")).unwrap();
        let error = read(home.path(), "linked").err().unwrap().to_string();
        assert!(!error.contains("secret invalid"));
        assert_eq!(std::fs::read(&target).unwrap(), b"secret invalid JSONL");
    }
}
