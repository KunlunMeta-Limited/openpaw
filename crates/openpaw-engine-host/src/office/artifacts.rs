//! 职责：成功工具产物的不可变字节、单版本能力及Office官方交付适配。
//! 输入输出：真实工具目标到受限blob、确定版本URL和官方result/deliveries。
//! 依赖：原Root锁、Store、路径安全、ring；不增加模型工具。
//! 禁止职责：不扫描工作区补造历史，不把文件存在当作内容验收，不执行HTML。
use super::{dto, OfficeConfig};
use crate::{Host, HostError, HostResult};
use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ring::{
    digest, hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
};

const FILE_LIMIT: u64 = 8 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;

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
fn read_secret(path: &FsPath) -> HostResult<Vec<u8>> {
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

fn access(row: &Value, dir: &FsPath) -> HostResult<String> {
    let scope = serde_json::to_string(&row["scope"]).map_err(HostError::storage)?;
    // Scope JSON must use ClientScope field order, identical to the stored binding key.
    let scope: crate::ClientScope = serde_json::from_str(&scope).map_err(HostError::storage)?;
    let scope = serde_json::to_string(&scope).map_err(HostError::storage)?;
    Ok(capability(
        &read_secret(&dir.join("access.key"))?,
        row["version"].as_str().unwrap_or(""),
        &scope,
        row["artifact"].as_str().unwrap_or(""),
    ))
}
fn url(row: &Value, dir: &FsPath, suffix: &str) -> HostResult<String> {
    Ok(format!(
        "{}/office/artifacts/{}{}?access={}",
        row["origin"].as_str().unwrap_or(""),
        row["version"].as_str().unwrap_or(""),
        suffix,
        access(row, dir)?
    ))
}
fn artifact(row: &Value, dir: &FsPath) -> HostResult<Value> {
    Ok(
        json!({"id":row["artifact"],"versionId":row["version"],"version":format!("v{}",row["number"]),"runId":row["delivery"],"title":row["title"],"kind":if row["mime"].as_str().unwrap_or("").starts_with("image/"){"image"}else{"document"},"mimeType":row["mime"],"sha256":row["hash"],"producedAt":row["at"],"url":url(row,dir,"")?}),
    )
}
// The preview is derived separately; raw bytes/hash/download remain exactly as written.
fn preview(row: &Value, rows: &[Value], dir: &FsPath) -> HostResult<Option<Value>> {
    if row["mime"] != "text/markdown" || row["bytes"].as_u64().is_some_and(|size| size > 32000) {
        return Ok(None);
    }
    let bytes = read_blob(dir, row)?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    if dto::units(text) > 8000 {
        return Ok(None);
    }
    let mut images = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rendered = text.to_owned();
    for image in rows.iter().filter(|r| {
        r["sequence"].as_i64() <= row["sequence"].as_i64()
            && r["mime"].as_str().unwrap_or("").starts_with("image/")
    }) {
        let Some(path) = image["path"].as_str() else {
            continue;
        };
        if !seen.insert(path) {
            continue;
        }
        let base = FsPath::new(row["path"].as_str().unwrap_or(""))
            .parent()
            .unwrap_or(FsPath::new(""));
        // Resolve only existing captured relative links in the same conversation; never read new files.
        let full = base.join(path).to_string_lossy().to_string();
        let relative = FsPath::new(path)
            .strip_prefix(base)
            .ok()
            .and_then(|p| p.to_str());
        let target = relative.unwrap_or(&full);
        // An explicit current-directory prefix names the same captured file; no path is opened here.
        let mut replaced = false;
        for candidate in [target.to_owned(), format!("./{target}")] {
            let needle = format!("]({candidate})");
            if rendered.contains(&needle) {
                rendered = rendered.replace(&needle, &format!("]({})", url(image, dir, "")?));
                replaced = true;
            }
        }
        if replaced {
            images.push(path);
        }
    }
    if images.is_empty() {
        return Ok(None);
    }
    let mut blocks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for ch in rendered.chars() {
        if units + ch.len_utf16() > 3500 {
            blocks.push(json!({"type":"text","text":chunk}));
            chunk = String::new();
            units = 0;
        }
        units += ch.len_utf16();
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        blocks.push(json!({"type":"text","text":chunk}));
    }
    Ok(Some(
        json!({"kind":"custom","id":format!("preview-{}",row["version"].as_str().unwrap_or("")),"title":"图文预览","purpose":"artifact-preview","artifactId":row["artifact"],"versionId":row["version"],"span":"full","blocks":blocks}),
    ))
}
/// 功能：从可信版本构造固定历史交付。
/// 参数：row为准确版本；rows为同会话版本目录；dir为私有blob根。
/// 返回：只含该次真实写入的官方不可变交付对象。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn delivery(row: &Value, rows: &[Value], dir: &FsPath) -> HostResult<Value> {
    let mut result = json!({"runId":row["delivery"],"title":row["title"],"status":"ready","producedAt":row["at"],"artifacts":[artifact(row,dir)?],"note":"原工具成功写出的确定版本；未进行内容验收。"});
    if let Some(section) = preview(row, rows, dir)? {
        result["sections"] = json!([section]);
    }
    Ok(
        json!({"id":row["delivery"],"sequence":row["sequence"],"turnId":row["turn"],"title":row["title"],"createdAt":row["at"],"result":result}),
    )
}
// 目录仅改变展示身份；原始交付、捕获时点图文和版本访问能力均不变。
fn catalog_id(version: &str) -> String {
    format!("catalog-{version}")
}
fn catalog(mut original: Value, version: &str) -> Value {
    let id = catalog_id(version);
    original["id"] = json!(id);
    original["turnId"] = json!("");
    original["result"]["runId"] = json!(id);
    if let Some(artifacts) = original["result"]["artifacts"].as_array_mut() {
        for artifact in artifacts {
            artifact["runId"] = json!(id);
        }
    }
    original
}
/// 功能：将准确会话产物接入当前白板与历史目录。
/// 参数：db为原Store；scope为可信会话；board为原业务白板。
/// 返回：保持任务事实并附加官方文件汇总。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：最多8当前文件及20历史目录；同轮同文件只关联最新聊天摘要，旧版本不删除。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30及12.141节r32；REQ-OP-014；DES-OP-007。
pub(crate) fn project(
    db: &crate::store::Db,
    scope: &crate::ClientScope,
    board: &mut Value,
) -> HostResult<()> {
    let (rows, dir) = db.artifacts(scope)?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut current = Vec::new();
    let mut sections = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        if !seen.insert(row["artifact"].clone().to_string()) {
            continue;
        }
        if current.len() >= 8 {
            break;
        }
        current.push(artifact(row, &dir)?);
        if let Some(section) = preview(row, &rows, &dir)? {
            sections.push(section);
        }
    }
    if !board.is_object() {
        *board =
            json!({"template":"task-board-v1","title":"文件交付","status":"completed","steps":[]});
    }
    if !board["result"].is_object() {
        board["result"] = json!({});
    }
    board["result"]["artifacts"] = json!(current);
    board["result"]["sections"] = json!(sections);
    board["result"]["status"] = json!("ready");
    board["result"]["producedAt"] = rows[0]["at"].clone();
    // 新的成功文件不能覆盖同会话先前的捕获失败说明。
    let prior_note = board["result"]["note"].as_str().unwrap_or_default();
    let note = "文件来自真实成功工具，版本原件可校验和下载；ready表示已保存，不表示内容审核通过。";
    board["result"]["note"] = json!(if prior_note.is_empty() {
        note.to_owned()
    } else {
        dto::bounded(&format!("{prior_note}；{note}"), 300)
    });
    // rows已按准确会话过滤并按序号倒排；不同轮次、不同文件不得合并。
    let mut turns = std::collections::HashSet::new();
    board["deliveries"] = json!(rows.iter().take(20).map(|row| {
        let latest = turns.insert((row["turn"].as_str().unwrap_or(""), row["artifact"].as_str().unwrap_or("")));
        Ok(json!({
            "id": if latest { row["delivery"].clone() } else { json!(catalog_id(row["version"].as_str().unwrap_or(""))) },
            "sequence": row["sequence"],
            "turnId": if latest { row["turn"].clone() } else { json!("") },
            "title": row["title"], "createdAt": row["at"],
            "snapshotUrl": url(row, &dir, if latest { "/delivery" } else { "/catalog" })?
        }))
    }).collect::<HostResult<Vec<Value>>>()?);
    Ok(())
}

#[derive(Clone)]
struct FileState {
    host: Host,
    config: OfficeConfig,
    address: SocketAddr,
}
/// 功能：为原loopback监听器装配单版本只读文件路由。
/// 参数：host/config/address为原Host及已验证本机配置。
/// 返回：无写操作的Router。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30及12.141节r32；REQ-OP-014；DES-OP-007。
pub(crate) fn router(host: Host, config: OfficeConfig, address: SocketAddr) -> Router {
    Router::new()
        .route("/office/artifacts/{version}", get(raw).options(options))
        .route(
            "/office/artifacts/{version}/delivery",
            get(historical).options(options),
        )
        .route(
            "/office/artifacts/{version}/catalog",
            get(catalog_view).options(options),
        )
        .with_state(FileState {
            host,
            config,
            address,
        })
}
fn source(state: &FileState, headers: &HeaderMap) -> HostResult<String> {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ![
        state.address.to_string(),
        format!("localhost:{}", state.address.port()),
    ]
    .contains(&host.to_owned())
        || (!origin.is_empty() && !state.config.allowed_origins.iter().any(|o| o == origin))
    {
        return Err(HostError::new("scope_denied", "文件来源未获授权"));
    }
    Ok(origin.to_owned())
}
fn secured(mut response: Response, origin: &str) -> Response {
    let h = response.headers_mut();
    for (k, v) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'",
        ),
        ("vary", "Origin"),
    ] {
        h.insert(k, v.parse().unwrap());
    }
    if !origin.is_empty() {
        h.insert("access-control-allow-origin", origin.parse().unwrap());
    }
    response
}
fn failure(error: HostError) -> Response {
    let code = if error.kind == "scope_denied" || error.kind == "session_deleted" {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    secured((code,axum::Json(json!({"code":code.as_u16(),"msg":error.message,"data":{"reasonCode":error.kind,"details":{},"issues":[]}}))).into_response(),"")
}
async fn options(State(state): State<FileState>, headers: HeaderMap) -> Response {
    let origin = match source(&state, &headers) {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if headers
        .get("access-control-request-method")
        .and_then(|h| h.to_str().ok())
        != Some("GET")
        || headers
            .get("access-control-request-headers")
            .is_some_and(|h| {
                h.to_str().ok().is_none_or(|v| {
                    v.split(',').any(|x| {
                        !matches!(
                            x.trim().to_ascii_lowercase().as_str(),
                            "accept" | "content-type"
                        )
                    })
                })
            })
    {
        return failure(HostError::new("scope_denied", "仅允许只读文件预检"));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert("access-control-allow-methods", "GET".parse().unwrap());
    secured(response, &origin)
}
async fn raw(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Raw).await
}
async fn historical(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Delivery).await
}
async fn catalog_view(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Catalog).await
}
#[derive(Clone, Copy)]
enum FileView {
    Raw,
    Delivery,
    Catalog,
}
async fn serve(
    state: FileState,
    version: String,
    query: Option<String>,
    headers: HeaderMap,
    view: FileView,
) -> Response {
    let origin = match source(&state, &headers) {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if version.len() > 80
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !query
            .as_deref()
            .is_some_and(|s| s.starts_with("access=") && !s.contains('&'))
    {
        return secured(
            failure(HostError::new("scope_denied", "文件版本或访问凭证无效")),
            &origin,
        );
    }
    let access = query
        .unwrap_or_default()
        .strip_prefix("access=")
        .unwrap_or("")
        .to_owned();
    let result = state
        .host
        .inner
        .store
        .call_priority(move |db| {
            let (bytes, mime) =
                db.artifact_read(&version, &access, !matches!(view, FileView::Raw))?;
            if matches!(view, FileView::Catalog) {
                let original = serde_json::from_slice(&bytes).map_err(HostError::storage)?;
                let bytes =
                    serde_json::to_vec(&catalog(original, &version)).map_err(HostError::storage)?;
                Ok((bytes, mime))
            } else {
                Ok((bytes, mime))
            }
        })
        .await;
    match result {
        Ok((bytes, mime)) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            response
                .headers_mut()
                .insert("content-type", mime.parse().unwrap());
            secured(response, &origin)
        }
        Err(e) => secured(failure(e), &origin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn captured_markdown_images_bind_to_capture_time_version_without_mutating_original() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        secret(dir).unwrap();
        let raw = b"# Review\n\n![diagram](images/flow.svg)\n";
        let hash = sha256(raw);
        publish_blob(dir, &hash, raw).unwrap();
        let scope = json!({"namespace":"office","slot_id":"team-a","conversation_id":"chat-a"});
        let doc = json!({"version":"v-doc","artifact":"doc","delivery":"delivery-doc","path":"notes.md","mime":"text/markdown","hash":hash,"bytes":raw.len(),"sequence":2,"scope":scope,"origin":"http://127.0.0.1:1234","title":"notes.md","at":"2026-10-30T00:00:00Z","turn":"turn-a","number":1});
        let image = |seq, ver| json!({"version":ver,"artifact":"image","path":"images/flow.svg","mime":"image/svg+xml","sequence":seq,"scope":scope,"origin":"http://127.0.0.1:1234"});
        let rows = vec![
            image(3, "future-image"),
            doc.clone(),
            image(1, "captured-image"),
        ];
        let d = delivery(&doc, &rows, dir).unwrap();
        let preview = d["result"]["sections"][0]["blocks"][0]["text"]
            .as_str()
            .unwrap();
        assert!(preview.contains("/captured-image?access="));
        assert!(!preview.contains("future-image"));
        assert_eq!(read_blob(dir, &doc).unwrap(), raw);
        let old = d.clone();
        let mut next = rows.clone();
        next.insert(0, image(4, "newer-image"));
        assert_eq!(delivery(&doc, &next, dir).unwrap(), old);
        let history = catalog(d.clone(), "v-doc");
        assert_eq!(history["id"], "catalog-v-doc");
        assert_eq!(history["turnId"], "");
        assert_eq!(history["result"]["runId"], history["id"]);
        assert_eq!(history["result"]["artifacts"][0]["runId"], history["id"]);
        assert_eq!(history["result"]["sections"], old["result"]["sections"]);
        for key in ["id", "versionId", "url", "sha256", "producedAt"] {
            assert_eq!(
                history["result"]["artifacts"][0][key],
                old["result"]["artifacts"][0][key]
            );
        }
        assert_eq!(
            history,
            catalog(delivery(&doc, &next, dir).unwrap(), "v-doc")
        );
        assert_eq!(d, old, "catalog must not mutate the original delivery");
    }
    #[test]
    fn d143_dot_relative_images_use_captured_versions_without_broadening_links() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        secret(dir).unwrap();
        for (doc_path, image_path, target) in [
            ("notes/article.md", "notes/flow.svg", "flow.svg"),
            ("article.md", "images/flow.svg", "images/flow.svg"),
        ] {
            let raw = format!(
                "![plain]({target})\n![dot](./{target})\n![parent](../{target})\n![remote](https://example.invalid/{target})\n"
            );
            let hash = sha256(raw.as_bytes());
            publish_blob(dir, &hash, raw.as_bytes()).unwrap();
            let scope = json!({"namespace":"office","slot_id":"team-a","conversation_id":"chat-a"});
            let doc = json!({"version":"doc","artifact":"doc","delivery":"delivery-doc","path":doc_path,"mime":"text/markdown","hash":hash,"bytes":raw.len(),"sequence":2,"scope":scope,"origin":"http://127.0.0.1:1234"});
            let image = |sequence, version| json!({"version":version,"artifact":"image","path":image_path,"mime":"image/svg+xml","sequence":sequence,"scope":scope,"origin":"http://127.0.0.1:1234"});
            let rows = vec![image(3, "future"), doc.clone(), image(1, "captured")];
            let rendered = preview(&doc, &rows, dir).unwrap().unwrap();
            let text = rendered["blocks"][0]["text"].as_str().unwrap();
            assert_eq!(text.matches("/captured?access=").count(), 2);
            assert!(!text.contains("/future?access="));
            assert!(text.contains(&format!("![parent](../{target})")));
            assert!(text.contains(&format!("![remote](https://example.invalid/{target})")));
            assert_eq!(read_blob(dir, &doc).unwrap(), raw.as_bytes());
            let mut newer = rows.clone();
            newer.insert(0, image(4, "newer"));
            assert_eq!(preview(&doc, &newer, dir).unwrap().unwrap(), rendered);
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
    #[tokio::test]
    async fn real_tools_http_versions_restart_scope_revocation_and_capture_warning() {
        use crate::{ClientScope, HostConfig, RunState};
        use openpaw_llm::{MockMode, MockProvider};
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"project":root},"slots":{"office-1":"project"},"provider":"mock","tools":["write_file","edit_file"]})).unwrap();
        let config:OfficeConfig=serde_json::from_value(json!({"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_UNUSED","allowed_origins":["http://127.0.0.1:8765"]})).unwrap();
        cfg.office = Some(config.clone());
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "versions".into(),
        };
        let steps = vec![
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"version.md","content":"# first\n"}),
                text: "实际写入第一版".into(),
            },
            MockMode::Fixed("第一版保存。".into()),
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"first","new_string":"second"}),
                text: "实际修改第二版".into(),
            },
            MockMode::Fixed("第二版保存。".into()),
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"second","new_string":"third"}),
                text: "同轮修订第三版".into(),
            },
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"third","new_string":"fourth"}),
                text: "同轮修订第四版".into(),
            },
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"other.md","content":"独立第二文件"}),
                text: "同轮保存不同文件".into(),
            },
            MockMode::Fixed("两文件已保存。".into()),
        ];
        let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::scripted(steps)))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let init = origin.clone();
        host.inner
            .store
            .call_priority(move |db| db.artifact_origin(&init))
            .await
            .unwrap();
        let app = router(host.clone(), config.clone(), address);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let binding = host.open_session(scope.clone()).await.unwrap();
        async fn execute(host: &Host, scope: &ClientScope, session: &str, turn: &str) {
            let run = host
                .submit_idempotent(
                    scope,
                    session,
                    turn.into(),
                    vec![],
                    turn.into(),
                    json!([{"role":"user","content":turn}]).to_string(),
                )
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let (snapshot, _) = host.snapshot_and_subscribe(scope).await.unwrap();
                    if let Some(approval) = snapshot.pending_approvals.first() {
                        host.decide_approval(
                            scope,
                            &approval.approval_id,
                            approval.revision,
                            approval.binding.clone(),
                            true,
                        )
                        .await
                        .unwrap();
                    }
                    let state = host.get_run(scope, &run.run_id).await.unwrap().state;
                    if state.is_terminal() {
                        assert_eq!(state, RunState::Completed);
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        execute(&host, &scope, &binding.session_id, "turn-first").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        assert_eq!(board["result"]["status"], "ready");
        let first = board["result"]["artifacts"][0].clone();
        let old_delivery = board["deliveries"][0].clone();
        let client = reqwest::Client::new();
        let first_url = first["url"].as_str().unwrap();
        let response = client
            .get(first_url)
            .header("origin", "http://127.0.0.1:8765")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://127.0.0.1:8765"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"# first\n");
        assert_eq!(
            client
                .get(first_url)
                .header("origin", "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(first_url)
                .header("host", "evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(first_url.replace("access=", "access=00"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let old: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old["turnId"], "turn-first");
        execute(&host, &scope, &binding.session_id, "turn-second").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        let second = &board["result"]["artifacts"][0];
        assert_eq!(first["id"], second["id"]);
        assert_ne!(first["versionId"], second["versionId"]);
        assert_eq!(board["deliveries"].as_array().unwrap().len(), 2);
        assert_eq!(
            client
                .get(first_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# first\n"
        );
        assert_eq!(
            client
                .get(second["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# second\n"
        );
        let unchanged: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old, unchanged);
        // Same exact call is idempotent; version publication must not replay the tool or allocate another delivery.
        let check_scope = scope.clone();
        host.inner
            .store
            .call_priority(move |db| {
                let (rows, _) = db.artifacts(&check_scope)?;
                assert_eq!(rows.len(), 2);
                let row = rows.last().unwrap();
                let binding = openpaw_core::ToolCallBinding {
                    run_id: row["run_id"].as_str().unwrap().into(),
                    call_id: row["call_id"].as_str().unwrap().into(),
                    tool_name: "write_file".into(),
                    input: json!({"path":"version.md","content":"# first\n"}),
                };
                db.capture_artifact(&binding, "version.md", "text/markdown", b"# first\n")?;
                assert_eq!(db.artifacts(&check_scope)?.0.len(), 2);
                Ok(())
            })
            .await
            .unwrap();
        execute(&host, &scope, &binding.session_id, "turn-third").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        let deliveries = board["deliveries"].as_array().unwrap();
        assert_eq!(board["result"]["artifacts"].as_array().unwrap().len(), 2);
        assert_eq!(
            deliveries.len(),
            5,
            "all immutable versions remain discoverable"
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-first")
                .count(),
            1
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-second")
                .count(),
            1
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-third")
                .count(),
            2,
            "one summary per file in this turn"
        );
        let historical = deliveries.iter().find(|d| d["turnId"] == "").unwrap();
        let catalog_url = historical["snapshotUrl"].as_str().unwrap().to_owned();
        let response = client
            .get(&catalog_url)
            .header("origin", "http://127.0.0.1:8765")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://127.0.0.1:8765"
        );
        let catalog_bytes = response.bytes().await.unwrap();
        let catalog_value: Value = serde_json::from_slice(&catalog_bytes).unwrap();
        for key in ["id", "sequence", "turnId"] {
            assert_eq!(
                catalog_value[key], historical[key],
                "lazy response identity must match descriptor"
            );
        }
        assert_eq!(catalog_value["result"]["runId"], historical["id"]);
        let third = &catalog_value["result"]["artifacts"][0];
        assert_eq!(third["runId"], historical["id"]);
        assert_eq!(
            client
                .get(third["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "# third\n"
        );
        let original_third_url = catalog_url.replace("/catalog?", "/delivery?");
        let original_third_bytes = client
            .get(&original_third_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let original_third: Value = serde_json::from_slice(&original_third_bytes).unwrap();
        assert_eq!(original_third["turnId"], "turn-third");
        assert_ne!(original_third["id"], historical["id"]);
        for key in ["id", "versionId", "url", "sha256"] {
            assert_eq!(third[key], original_third["result"]["artifacts"][0][key]);
        }
        for bad_url in [
            catalog_url.replace("access=", "access=00"),
            format!("{catalog_url}&extra=1"),
        ] {
            assert_eq!(client.get(bad_url).send().await.unwrap().status(), 403);
        }
        assert_eq!(
            client
                .get(&catalog_url)
                .header("origin", "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(&catalog_url)
                .header("host", "evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= 65536);
        assert_eq!(
            client
                .get(old_delivery["snapshotUrl"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
            old
        );
        host.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
        drop(host);
        let host = Host::with_provider(
            cfg.clone(),
            Arc::new(MockProvider::scripted(vec![
                MockMode::ToolUse {
                    tool_name: "write_file".into(),
                    input: json!({"path":"quota.md","content":"原件保留"}),
                    text: "写入配额验收".into(),
                },
                MockMode::Fixed("已写入工作区。".into()),
            ])),
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let app = router(host.clone(), config, address);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert_eq!(
            client
                .get(first_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# first\n"
        );
        let immutable: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old, immutable);
        assert_eq!(
            client
                .get(&catalog_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            catalog_bytes
        );
        assert_eq!(
            client
                .get(&original_third_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            original_third_bytes
        );
        // A different configured conversation has no version directory and cannot obtain another scope's capability.
        let foreign = ClientScope {
            conversation_id: "foreign".into(),
            ..scope.clone()
        };
        host.open_session(foreign.clone()).await.unwrap();
        assert!(host
            .inner
            .store
            .call_priority(move |db| db.artifacts(&foreign).map(|v| v.0))
            .await
            .unwrap()
            .is_empty());
        let scope_check = scope.clone();
        host.inner
            .store
            .call_priority(move |db| {
                let mut board =
                    json!({"result":{"note":"文件版本捕获警告：已写入但没有保存新版本"}});
                project(db, &scope_check, &mut board)?;
                let note = board["result"]["note"].as_str().unwrap();
                assert!(note.contains("文件版本捕获警告") && note.contains("ready表示已保存"));
                Ok(())
            })
            .await
            .unwrap();
        // Force the real storage quota without consuming disk blocks; successful write must remain successful.
        let dir = cfg.home.join("engine-host/artifacts");
        let sparse = dir.join(format!("{}.blob", "e".repeat(64)));
        std::fs::File::create(&sparse)
            .unwrap()
            .set_len(TOTAL_LIMIT)
            .unwrap();
        let quota_scope = ClientScope {
            conversation_id: "quota".into(),
            ..scope.clone()
        };
        let quota_session = host.open_session(quota_scope.clone()).await.unwrap();
        execute(&host, &quota_scope, &quota_session.session_id, "turn-quota").await;
        assert_eq!(
            std::fs::read_to_string(root.join("quota.md")).unwrap(),
            "原件保留"
        );
        let (snapshot, _) = host.snapshot_and_subscribe(&quota_scope).await.unwrap();
        let run = snapshot.runs[0].clone();
        let display = super::super::turns::display(&host, &run).await.unwrap();
        assert!(display.contains("512MiB") && display.contains("已写入"));
        assert_eq!(run.state, RunState::Completed);
        let q = quota_scope.clone();
        assert!(host
            .inner
            .store
            .call_priority(move |db| db.artifacts(&q).map(|a| a.0))
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_file(sparse).unwrap();
        host.delete_session(&scope).await.unwrap();
        assert_eq!(client.get(first_url).send().await.unwrap().status(), 403);
        assert_eq!(client.get(&catalog_url).send().await.unwrap().status(), 403);
        assert_eq!(
            client
                .get(&original_third_url)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert!(root.join("version.md").exists());
        let count = rusqlite::Connection::open(cfg.home.join("engine-host/host.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM host_artifact_versions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        host.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
    }
}
