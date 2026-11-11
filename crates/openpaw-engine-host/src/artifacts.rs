//! 职责：捕获真实成功工具产物并保存不可变字节与访问能力。
//! 输入输出：可信工具绑定到原blob、hash与版本存储。
//! 依赖：原Host/Store、路径安全及ring。
//! 禁止职责：不生成Office DTO/URL，不扫描旧文件补造历史，不执行工具第二次。
use crate::{Host, HostError, HostResult};
use ring::{
    digest, hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
};
const FILE_LIMIT: u64 = 8 * 1024 * 1024;
/// 用途：保持原不可变产物总字节配额。
/// 字段：512MiB固定上限。
/// 来源去向：原捕获与配额回归。
/// 约束：不因模块移动放宽。
/// 稳定性：Host私有常量。
/// 关联：需求/设计及本轮方案：CHG-2026-0007 §12.169；REQ-OP-014；DES-OP-007。；需求/设计及本轮方案：REQ-OP-014；DES-OP-008；CHG-2026-0007 §12.169。
pub(crate) const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;

/// 功能：计算原始字节的小写SHA256。
/// 参数：bytes为原始文件或能力字节。
/// 返回：64位小写十六进制hash。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn sha256(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// 功能：建立仅当前用户访问的版本目录。
/// 参数：dir为Host home内固定目录。
/// 返回：原操作结果。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn private_dir(dir: &FsPath) -> HostResult<()> {
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(HostError::new("scope_denied", "版本目录不允许符号链接"));
    }
    std::fs::create_dir_all(dir).map_err(HostError::storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(HostError::storage)?;
    }
    // Only our unpublished UUID temporary files are restart garbage; no blob or workspace is removed.
    for entry in std::fs::read_dir(dir).map_err(HostError::storage)? {
        let entry = entry.map_err(HostError::storage)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(id) = name
            .strip_prefix(".blob-")
            .or_else(|| name.strip_prefix(".secret-"))
        {
            if id.len() == 32 && uuid::Uuid::parse_str(id).is_ok() {
                std::fs::remove_file(entry.path()).map_err(HostError::storage)?;
            }
        }
    }
    Ok(())
}
fn private_file(path: &FsPath) -> HostResult<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path).map_err(HostError::storage)
}
/// 功能：读取或原子建立持久随机Host能力密钥。
/// 参数：dir为私有版本目录。
/// 返回：32字节随机密钥，仅在Host内存使用。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn secret(dir: &FsPath) -> HostResult<Vec<u8>> {
    let path = dir.join("access.key");
    if path.exists() {
        return read_secret(&path);
    }
    let mut bytes = vec![0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| HostError::new("storage_unavailable", "无法取得安全随机数"))?;
    let temp = dir.join(format!(".secret-{}", uuid::Uuid::new_v4().simple()));
    let mut f = private_file(&temp)?;
    f.write_all(&bytes)
        .and_then(|_| f.sync_all())
        .map_err(HostError::storage)?;
    drop(f);
    std::fs::rename(&temp, &path).map_err(HostError::storage)?;
    sync_dir(dir)?;
    Ok(bytes)
}
/// 功能：读取原私有版本目录的能力密钥并校验文件类型与长度。
/// 参数：path为Host固定密钥路径。
/// 返回：原密钥字节。
/// 错误：符号链接、损坏或读取失败明确拒绝。
/// 状态：只读，不生成新密钥或回退。
/// 并发与取消：原Store有序访问。
/// 权限：仅服务端可信路径，字节不进入Office消息。
/// 关联：需求/设计及本轮方案：CHG-2026-0007 §12.169；REQ-OP-014；DES-OP-007。；需求/设计及本轮方案：REQ-OP-014；DES-OP-008；CHG-2026-0007 §12.169。
pub(crate) fn read_secret(path: &FsPath) -> HostResult<Vec<u8>> {
    if std::fs::symlink_metadata(path)
        .map_err(HostError::storage)?
        .file_type()
        .is_symlink()
    {
        return Err(HostError::new("scope_denied", "能力密钥不允许符号链接"));
    }
    let bytes = std::fs::read(path).map_err(HostError::storage)?;
    if bytes.len() != 32 {
        return Err(HostError::new("storage_unavailable", "版本能力密钥损坏"));
    }
    Ok(bytes)
}
fn sync_dir(dir: &FsPath) -> HostResult<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(HostError::storage)?;
    }
    let _ = dir;
    Ok(())
}
/// 功能：从随机Host密钥和准确版本范围导出单版本能力。
/// 参数：secret为持久随机密钥；version/scope/artifact为可信索引。
/// 返回：不可用于其他版本的稳定能力，不写入模型或日志。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn capability(secret: &[u8], version: &str, scope: &str, artifact: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hex(hmac::sign(
        &key,
        format!("openpaw-artifact-v1\0{scope}\0{artifact}\0{version}").as_bytes(),
    )
    .as_ref())
}
/// 功能：常量工作量核对URL能力hash。
/// 参数：access为请求能力，expected为可信hash。
/// 返回：是否严格匹配；空或超长能力拒绝。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn valid_capability(access: &str, expected: &str) -> bool {
    if access.len() != 64 || expected.len() != 64 || !access.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    sha256(access.as_bytes())
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |different, (a, b)| different | (a ^ b))
        == 0
}
/// 功能：在总配额内原子保存内容地址blob。
/// 参数：dir为固定私有目录；hash/bytes为一致内容。
/// 返回：保存成功或已有相同blob。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：临时文件不公开；只删除本次失败临时文件，不回收用户或旧版本。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn publish_blob(dir: &FsPath, hash: &str, bytes: &[u8]) -> HostResult<()> {
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(HostError::new(
            "capacity_exceeded",
            "文件已写入，但版本捕获超过单文件8MiB上限",
        ));
    }
    let path = dir.join(format!("{hash}.blob"));
    if path.exists() {
        let data = read_exact_file(&path, FILE_LIMIT)?;
        if sha256(&data) != hash {
            return Err(HostError::new(
                "storage_unavailable",
                "已有版本blob校验失败",
            ));
        }
        return Ok(());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir).map_err(HostError::storage)? {
        let entry = entry.map_err(HostError::storage)?;
        if entry.file_name().to_string_lossy().ends_with(".blob") {
            let m = entry.metadata().map_err(HostError::storage)?;
            total = total.saturating_add(m.len());
        }
    }
    if total.saturating_add(bytes.len() as u64) > TOTAL_LIMIT {
        return Err(HostError::new(
            "capacity_exceeded",
            "文件已写入，但版本保存总量达到512MiB上限；旧版及工作区文件均保留",
        ));
    }
    let temp = dir.join(format!(".blob-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut f = private_file(&temp)?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(HostError::storage)?;
        drop(f);
        std::fs::rename(&temp, &path).map_err(HostError::storage)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
fn read_exact_file(path: &FsPath, limit: u64) -> HostResult<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(HostError::storage)?;
    if !meta.is_file() || meta.len() > limit {
        return Err(HostError::new(
            "scope_denied",
            "版本目标必须是上限内普通文件",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(HostError::storage)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(HostError::storage)?;
    if bytes.len() as u64 > limit {
        return Err(HostError::new("capacity_exceeded", "文件读取超过8MiB上限"));
    }
    Ok(bytes)
}
/// 功能：读取与hash一致的已发布blob。
/// 参数：dir为私有目录；row为可信版本索引。
/// 返回：准确原始字节，不回退当前工作区。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn read_blob(dir: &FsPath, row: &Value) -> HostResult<Vec<u8>> {
    let hash = row["hash"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| HostError::new("storage_unavailable", "版本hash损坏"))?;
    let bytes = read_exact_file(&dir.join(format!("{hash}.blob")), FILE_LIMIT)?;
    if sha256(&bytes) != hash || row["bytes"].as_u64() != Some(bytes.len() as u64) {
        return Err(HostError::new(
            "storage_unavailable",
            "版本原件SHA256或大小校验失败",
        ));
    }
    Ok(bytes)
}
fn mime(path: &str) -> Option<&'static str> {
    match FsPath::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => Some("text/markdown"),
        "txt" | "log" => Some("text/plain"),
        "rs" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "css" | "toml" | "yaml" | "yml"
        | "sh" => Some("text/plain"),
        "csv" => Some("text/csv"),
        "tsv" => Some("text/tab-separated-values"),
        "json" => Some("application/json"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}
/// 功能：在原Root写许可仍持有时捕获真实工具目标。
/// 参数：host/root/binding为可信工具上下文。
/// 返回：捕获结果；未支持工具或文件类型不捕获。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：不执行写工具第二次；失败由调用方单独报告。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) async fn capture(
    host: &Host,
    root: PathBuf,
    binding: openpaw_core::ToolCallBinding,
) -> HostResult<()> {
    if !matches!(
        binding.tool_name.as_str(),
        "write_file" | "edit_file" | "render_visual" | "generate_image"
    ) {
        return Ok(());
    }
    let Some(target) = binding.input["path"].as_str() else {
        return Ok(());
    };
    let Some(mime) = mime(target) else {
        return Ok(());
    };
    let target = target.to_owned();
    let (path, bytes) = tokio::task::spawn_blocking(move || {
        let actual = openpaw_tools::safety::resolve(&root, &target)
            .map_err(|_| HostError::new("scope_denied", "产物路径不在原授权工作区内"))?;
        let path = actual
            .strip_prefix(&root)
            .map_err(|_| HostError::new("scope_denied", "产物路径不在原授权工作区内"))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = read_exact_file(&actual, FILE_LIMIT)?;
        Ok::<_, HostError>((path, bytes))
    })
    .await
    .map_err(HostError::storage)??;
    host.inner
        .store
        .call_priority(move |db| {
            db.capture_artifact(&binding, &path, mime, &bytes)
                .map(|_| ())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn immutable_blob_capability_integrity_quota_and_private_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("versions");
        private_dir(&dir).unwrap();
        let secret_a = secret(&dir).unwrap();
        assert_eq!(secret_a, secret(&dir).unwrap());
        let a = capability(&secret_a, "v1", "team1-chat1", "file1");
        assert!(valid_capability(&a, &sha256(a.as_bytes())));
        for other in [
            capability(&secret_a, "v2", "team1-chat1", "file1"),
            capability(&secret_a, "v1", "team2-chat1", "file1"),
            capability(&secret_a, "v1", "team1-chat2", "file1"),
            capability(&secret_a, "v1", "team1-chat1", "file2"),
        ] {
            assert!(!valid_capability(&other, &sha256(a.as_bytes())));
        }
        let first = b"# original\n";
        let hash = sha256(first);
        publish_blob(&dir, &hash, first).unwrap();
        publish_blob(&dir, &hash, first).unwrap();
        let row = json!({"hash":hash,"bytes":first.len()});
        assert_eq!(read_blob(&dir, &row).unwrap(), first);
        std::fs::write(dir.join(format!("{hash}.blob")), b"corrupt").unwrap();
        assert!(read_blob(&dir, &row).is_err());
        assert!(publish_blob(&dir, &hash, first).is_err());
        assert!(publish_blob(&dir, &sha256(b"large"), &vec![0; FILE_LIMIT as usize + 1]).is_err());
        let sparse = dir.join(format!("{}.blob", "f".repeat(64)));
        std::fs::File::create(&sparse)
            .unwrap()
            .set_len(TOTAL_LIMIT)
            .unwrap();
        assert!(publish_blob(&dir, &sha256(b"next"), b"next")
            .unwrap_err()
            .message
            .contains("512MiB"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(dir.join("access.key"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn blob_and_secret_symlink_targets_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("private");
        private_dir(&dir).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, b"x").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("access.key")).unwrap();
        assert!(secret(&dir).is_err());
        std::os::unix::fs::symlink(&outside, dir.join(format!("{}.blob", sha256(b"x")))).unwrap();
        assert!(read_blob(&dir, &json!({"hash":sha256(b"x"),"bytes":1})).is_err());
    }
}
