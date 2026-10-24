//! 职责：离线预检、备份、复制和事务重绑团队工作区。
//! 输入输出：本机旧/目标配置及确认摘要到计划或持久迁移回执。
//! 依赖：原HostConfig、Store、路径校验和sandbox OS锁。
//! 禁止职责：不启动HTTP/Provider、不改JSONL、不替换原执行和资源锁。
use crate::{store::Db, HostConfig, HostError, HostResult, SessionBinding};
use openpaw_sandbox::file_lock::{try_lock_exclusive, FileLockGuard};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const APPROVAL: &str = "b8fd7c46bf4a1f94c51eb309e5f2a6d910de56cb:docs/changes/CHG-2026-0007.md";
const RESOURCE_LOCK: &str = ".openpaw/engine-host-resource.lock";
const MAX_PLAN_BYTES: usize = 32 * 1024 * 1024;

/// 功能：只读生成团队工作区迁移计划及确认摘要。
/// 参数：config/target为本机配置路径，teams为明确选择且不重复的团队。
/// 返回：规范JSON，包含payload及其sha256，不自动写输出文件。
/// 错误：Host未停止、日志未清理、越界、冲突、链接或元数据损坏均拒绝。
/// 状态：只打开已有锁及只读数据库，不恢复、不创建目录/索引/消息。
/// 并发与取消：持有原home独占锁直到读完；不执行模型或网络请求。
/// 权限：只限本机可信调用方，计划不是远程授权凭证。
/// 关联：需求/设计REQ-OP-025；DES-OP-013；CHG-2026-0007第12.12节。
pub fn plan_workspaces(config: &Path, target: &Path, teams: &[String]) -> HostResult<String> {
    let old = HostConfig::load(config)?;
    let home = plain_directory(&old.home)?;
    let lock = home.join("engine-host/host.lock");
    plain_file(&lock)?;
    let _owner = try_lock_exclusive(&lock)
        .map_err(|_| HostError::new("conflict", "请先优雅关闭原Host再进行只读预检"))?;
    let payload = build_plan(config, target, teams)?;
    let digest = digest_json(&payload)?;
    let observations = space_observations(&payload)?;
    let output = serde_json::to_string_pretty(
        &json!({"payload":payload,"sha256":digest,"observations":observations}),
    )
    .map_err(HostError::storage)?;
    if output.len() > MAX_PLAN_BYTES {
        return Err(invalid("迁移计划超过32MiB限制；请缩小迁移范围再预检"));
    }
    Ok(output)
}

/// 功能：按人工确认的准确计划备份、复制并原子重绑既有团队。
/// 参数：config/target为原配置，plan为预检JSON，confirm为SHA256，backup为全新备份目录。
/// 返回：成功回执JSON，列明备份、摘要和迁移团队，不修改原配置。
/// 错误：计划漂移、锁冲突、磁盘不足、复制/事务错误不伪报成功。
/// 状态：先完整备份再复制核验，最后单事务提交；原项目及JSONL不改。
/// 并发与取消：持有原home及所涉工作根锁到提交/清理结束；中断不重放模型。
/// 权限：工具/HTTP无此入口，只接受本机管理员的准确摘要确认。
/// 关联：需求/设计REQ-OP-025；DES-OP-013；CHG-2026-0007第12.12节。
pub fn apply_workspaces(
    config: &Path,
    target: &Path,
    plan: &Path,
    confirm: &str,
    backup: &Path,
) -> HostResult<String> {
    plain_file(plan)?;
    let mut bytes = Vec::new();
    // 限制实际读取量，不能先读取任意大文件再检查长度。
    File::open(plan)
        .map_err(HostError::storage)?
        .take(MAX_PLAN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(HostError::storage)?;
    if bytes.len() > MAX_PLAN_BYTES {
        return Err(invalid("迁移计划超过32MiB限制"));
    }
    let envelope: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("迁移计划JSON不合法"))?;
    let expected = &envelope["payload"];
    if confirm.len() != 64
        || !confirm.bytes().all(|b| b.is_ascii_hexdigit())
        || envelope["sha256"].as_str() != Some(confirm)
        || digest_json(expected)? != confirm
    {
        return Err(HostError::new("conflict", "确认摘要与准确迁移计划不一致"));
    }
    let teams: Vec<String> = serde_json::from_value(expected["teams"].clone())
        .map_err(|_| invalid("迁移团队清单缺失"))?;
    let old = HostConfig::load(config)?;
    let next = HostConfig::load(target)?;
    if teams
        .iter()
        .any(|team| !old.slots.contains_key(team) || !next.slots.contains_key(team))
    {
        return Err(invalid("计划包含未知团队"));
    }
    let home = plain_directory(&old.home)?;
    let lock = home.join("engine-host/host.lock");
    plain_file(&lock)?;
    let _owner = try_lock_exclusive(&lock)
        .map_err(|_| HostError::new("conflict", "Host仍运行或原数据目录不可独占"))?;
    let mut locks = Vec::<FileLockGuard>::new();
    let mut paths = BTreeSet::new();
    for team in &teams {
        for cfg in [&old, &next] {
            paths.insert(plain_directory(&cfg.roots[&cfg.slots[team]])?);
        }
    }
    // 先用原OS建议锁阻止协作写入；不把它解释成对非协作外部进程的沙箱。
    let mut created = Vec::new();
    let mut backed_up = false;
    let mut committed = false;
    let result = (|| {
        for root in &paths {
            let meta = root.join(".openpaw");
            ensure_directory(&meta, &mut created)?;
            let lock = root.join(RESOURCE_LOCK);
            if !lock.exists() {
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock)
                    .map_err(HostError::storage)?;
                file.sync_all().map_err(HostError::storage)?;
                created.push(lock.clone());
            }
            plain_file(&lock)?;
            locks.push(
                try_lock_exclusive(&lock)
                    .map_err(|_| HostError::new("conflict", "工作区仍被协作执行占用"))?,
            );
        }
        let actual = build_plan(config, target, &teams)?;
        if actual != *expected {
            return Err(HostError::new(
                "conflict",
                "配置、账本、文件或目标状态已变化；请重新预检并审核摘要",
            ));
        }
        let parent = backup
            .parent()
            .ok_or_else(|| invalid("备份目录需要已有父目录"))?;
        let parent = plain_directory(parent)?;
        let name = backup
            .file_name()
            .ok_or_else(|| invalid("备份目录名不合法"))?;
        let backup = parent.join(name);
        if backup.exists() || backup.symlink_metadata().is_ok() {
            return Err(HostError::new("conflict", "备份必须是全新目录"));
        }
        for path in std::iter::once(&home).chain(paths.iter()) {
            if backup.starts_with(path)
                || path.starts_with(&backup)
                || directory_within(&parent, path)?
            {
                return Err(HostError::new(
                    "scope_denied",
                    "备份与home及全部迁移根必须互不包含",
                ));
            }
        }
        let home_files = file_manifest(&home, true)?;
        let needed = manifest_size(&home_files)?.saturating_add(16 * 1024 * 1024);
        if available_bytes(&parent)? < needed {
            return Err(HostError::new(
                "storage_unavailable",
                "备份磁盘可用空间不足",
            ));
        }
        fs::create_dir(&backup).map_err(HostError::storage)?;
        backed_up = true;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))
                .map_err(HostError::storage)?;
        }
        // 备份一旦创建就保留，即使后续失败也不删除唯一恢复现场。
        write_new(&backup.join("plan.json"), &bytes)?;
        let backup_home = backup.join("home");
        fs::create_dir(&backup_home).map_err(HostError::storage)?;
        let mut backup_created = Vec::new();
        copy_manifest(&home, &backup_home, &home_files, &mut backup_created)?;
        copy_one(
            config,
            &backup.join("old-config.toml"),
            expected["old_config_sha256"].as_str(),
        )?;
        copy_one(
            target,
            &backup.join("target-config.toml"),
            expected["target_config_sha256"].as_str(),
        )?;
        write_new(
            &backup.join("backed-up.json"),
            &serde_json::to_vec(&json!({"sha256":confirm,"phase":"backed_up","home":home}))
                .map_err(HostError::storage)?,
        )?;
        let mut events = Vec::new();
        for change in expected["moves"]
            .as_array()
            .ok_or_else(|| invalid("缺少迁移范围"))?
        {
            let source = Path::new(
                change["old_path"]
                    .as_str()
                    .ok_or_else(|| invalid("缺少源目录"))?,
            );
            let destination = Path::new(
                change["new_path"]
                    .as_str()
                    .ok_or_else(|| invalid("缺少目标目录"))?,
            );
            let files = &change["files"];
            let needed = manifest_size(files)?.saturating_add(16 * 1024 * 1024);
            if available_bytes(destination)? < needed {
                return Err(HostError::new(
                    "storage_unavailable",
                    "目标工作区磁盘可用空间不足",
                ));
            }
            copy_manifest(source, destination, files, &mut created)?;
            let mut event = change.clone();
            event["version"] = json!(1);
            event["confirmation_sha256"] = json!(confirm);
            event["approval"] = json!(APPROVAL);
            event["manifest_sha256"] = json!(digest_json(files)?);
            events.push(event);
        }
        // 文件落盘后再次核对源与既有数据；外部非协作写入不能被无声接受。
        let final_plan = build_plan(config, target, &teams)?;
        if [
            "inventory",
            "ledgers",
            "old_config_sha256",
            "target_config_sha256",
            "teams",
            "home",
        ]
        .iter()
        .any(|key| final_plan[*key] != expected[*key])
            || file_manifest(&home, true)? != home_files
        {
            return Err(HostError::new("conflict", "备份后原数据已改变，拒绝提交"));
        }
        for event in &events {
            let source = Path::new(event["old_path"].as_str().unwrap());
            let destination = Path::new(event["new_path"].as_str().unwrap());
            if file_manifest(source, false)? != event["files"] {
                return Err(HostError::new("conflict", "源文件在复制后已变化"));
            }
            let actual = file_manifest(destination, false)?;
            let mut expected_files = event["target_files"].as_array().unwrap().clone();
            for entry in event["files"].as_array().unwrap() {
                if !expected_files.iter().any(|e| e["path"] == entry["path"]) {
                    expected_files.push(entry.clone());
                }
            }
            expected_files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            if actual != json!(expected_files) {
                return Err(HostError::new("conflict", "目标文件在复制后已变化"));
            }
        }
        let roots = canonical_roots(&next)?;
        Db::commit_workspace_rebind(&home, &expected["inventory"], &roots, &events)?;
        committed = true;
        // 提交后不得再用清理路径撤销已生效文件；失败回执仍可由原审计判断提交事实。
        created.clear();
        let receipt = json!({"phase":"committed","sha256":confirm,"backup":backup,"teams":teams,"home":home,"target_config":plain_file(target)?,"source_files_preserved":true,"jsonl_unchanged":true});
        write_new(
            &backup.join("committed.json"),
            &serde_json::to_vec_pretty(&receipt).map_err(HostError::storage)?,
        )?;
        serde_json::to_string_pretty(&receipt).map_err(HostError::storage)
    })();
    drop(locks);
    if let Err(error) = &result {
        if backed_up {
            if let Ok(record) = serde_json::to_vec_pretty(
                &json!({"phase":if committed {"committed_receipt_failed"} else {"not_committed"},"error":error.to_string(),"created_paths":created,"sha256":confirm,"note":"原项目/JSONL不改；保留复制文件供核查，不递归删除。恢复须使用完整home与旧配置；先查原workspace_rebind判断提交事实。"}),
            ) {
                let _ = write_new(&backup.join("failed.json"), &record);
            }
        }
        cleanup_created(&created);
    }
    result
}

fn invalid(message: &str) -> HostError {
    HostError::new("invalid_input", message)
}

fn build_plan(config: &Path, target: &Path, teams: &[String]) -> HostResult<Value> {
    let (config_path, target_path) = (plain_file(config)?, plain_file(target)?);
    let old = HostConfig::load(&config_path)?;
    let next = HostConfig::load(&target_path)?;
    let home = plain_directory(&old.home)?;
    // 本流程完整备份业务home，但没有复制模型.env的授权；两者混放时先拒绝，
    // 不能通过略过凭据后仍宣称备份完整，也不能在离线迁移中读取密钥正文。
    match fs::symlink_metadata(home.join(".env")) {
        Ok(_) => return Err(HostError::new(
            "unsupported",
            "Host数据home内存在.env，本迁移不读取或复制模型配置；需先单独审核此目录布局的备份方案",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(HostError::storage(e)),
    }
    if plain_directory(&next.home)? != home {
        return Err(HostError::new("scope_denied", "迁移不能更换Host home"));
    }
    let old_roots = canonical_roots(&old)?;
    let roots = canonical_roots(&next)?;
    for (id, path) in &old_roots {
        if roots.get(id) != Some(path) {
            return Err(HostError::new(
                "scope_denied",
                "保留旧根身份和路径，目标根必须使用新ID",
            ));
        }
    }
    let selected: BTreeSet<_> = teams.iter().cloned().collect();
    if selected.is_empty() || selected.len() != teams.len() {
        return Err(invalid("团队列表不能为空或重复"));
    }
    let mut expected_config = next.clone();
    expected_config.roots = old.roots.clone();
    expected_config.slots = old.slots.clone();
    if serde_json::to_value(&expected_config).map_err(HostError::storage)?
        != serde_json::to_value(&old).map_err(HostError::storage)?
        || old.slots.keys().ne(next.slots.keys())
    {
        return Err(HostError::new(
            "scope_denied",
            "迁移只允许所选团队指向新增工作根，其他配置必须不变",
        ));
    }
    for (team, root) in &old.slots {
        if !selected.contains(team) && next.slots.get(team) != Some(root) {
            return Err(HostError::new("scope_denied", "未选择团队不能变更工作根"));
        }
    }
    for path in roots.values() {
        if directory_within(&home, path)? || directory_within(path, &home)? {
            return Err(HostError::new("scope_denied", "数据目录和工作根不能互含"));
        }
    }
    // 只读SQLite之前也先校验真实路径，不能等后续整目录manifest才发现DB链接。
    plain_file(&home.join("engine-host/host.db"))?;
    let inventory = Db::workspace_inventory(&home)?;
    let registered: BTreeMap<String, String> =
        serde_json::from_value(inventory["roots"].clone()).map_err(HostError::storage)?;
    for (id, path) in &registered {
        if old_roots.get(id).is_none_or(|root| root != Path::new(path)) {
            return Err(HostError::new("scope_denied", "旧配置与持久根身份不一致"));
        }
    }
    let bindings: Vec<SessionBinding> =
        serde_json::from_value(inventory["bindings"].clone()).map_err(HostError::storage)?;
    for binding in &bindings {
        if old.slots.get(&binding.scope.slot_id) != Some(&binding.root_id) {
            return Err(HostError::new("scope_denied", "旧配置不匹配既有会话绑定"));
        }
    }
    let mut ledgers = Vec::new();
    for binding in &bindings {
        if binding.session_id.len() != 32
            || !binding.session_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(HostError::new("storage_unavailable", "迁移会话标识损坏"));
        }
        let path = home
            .join("sessions")
            .join(format!("{}.jsonl", binding.session_id));
        plain_file(&path)?;
        let read = openpaw_store::SessionReader::read(&path).map_err(HostError::storage)?;
        if read.meta.id != binding.session_id || read.torn_tail_lines != 0 {
            return Err(HostError::new(
                "storage_unavailable",
                "账本身份或尾部不完整；请先正常恢复并关闭Host",
            ));
        }
        ledgers.push(json!({"session":binding.session_id,"sha256":hash_file(&path)?,"size":fs::metadata(&path).map_err(HostError::storage)?.len()}));
    }
    let mut moves = Vec::new();
    for team in &selected {
        let old_id = old.slots.get(team).ok_or_else(|| invalid("未知团队"))?;
        let new_id = next
            .slots
            .get(team)
            .ok_or_else(|| invalid("目标配置缺少团队"))?;
        let (source, destination) = (&old_roots[old_id], &roots[new_id]);
        if old_id == new_id
            || source == destination
            || same_file::is_same_file(source, destination).map_err(HostError::storage)?
        {
            return Err(invalid("所选团队未迁移到独立物理根"));
        }
        for (other, id) in &next.slots {
            if other != team
                && same_file::is_same_file(&roots[id], destination).map_err(HostError::storage)?
            {
                return Err(HostError::new("scope_denied", "目标根不能与其他团队共享"));
            }
        }
        let files = file_manifest(source, false)?;
        let existing = file_manifest(destination, false)?;
        let mut conflicts = Vec::new();
        for item in files.as_array().unwrap() {
            if existing
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["path"] == item["path"] && entry != item)
            {
                conflicts.push(item["path"].clone());
            }
            let relative = relative_file(item)?;
            let path = destination.join(relative);
            // 目录/文件互换也算冲突，不能借create_dir_all覆盖原目标。
            if path.is_dir() {
                conflicts.push(item["path"].clone());
            }
            for parent in path.ancestors().skip(1).take_while(|p| *p != destination) {
                if parent.is_file() {
                    conflicts.push(item["path"].clone());
                }
            }
        }
        if !conflicts.is_empty() {
            return Err(HostError::new(
                "conflict",
                "目标已有不同内容或目录冲突；原文件不覆盖，请人工处理后重新预检",
            ));
        }
        let sessions: Vec<_> = bindings
            .iter()
            .filter(|b| b.scope.slot_id == *team)
            .map(|b| b.session_id.clone())
            .collect();
        let counts = inventory["team_counts"].get(team).cloned().unwrap_or_else(
            || json!({"sessions":0,"runs":0,"requests":0,"approvals":0,"children":0}),
        );
        moves.push(json!({"team":team,"old_root":old_id,"new_root":new_id,"old_path":source,"new_path":destination,"sessions":sessions,"counts":counts,"files":files,"target_files":existing}));
    }
    let mut sorted = teams.to_vec();
    sorted.sort();
    Ok(
        json!({"format":"openpaw-workspace-plan-v1","approval":APPROVAL,"home":home,"old_config_sha256":hash_file(&config_path)?,"target_config_sha256":hash_file(&target_path)?,"teams":sorted,"inventory":inventory,"ledgers":ledgers,"moves":moves}),
    )
}

fn canonical_roots(config: &HostConfig) -> HostResult<BTreeMap<String, PathBuf>> {
    let mut roots = BTreeMap::<String, PathBuf>::new();
    for (id, path) in &config.roots {
        let path = plain_directory(path)?;
        if path.parent().is_none() {
            return Err(invalid("不能把磁盘根作为迁移工作根"));
        }
        for prior in roots.values() {
            if !same_file::is_same_file(&path, prior).map_err(HostError::storage)?
                && (directory_within(&path, prior)? || directory_within(prior, &path)?)
            {
                return Err(invalid("工作根不能嵌套"));
            }
        }
        roots.insert(id.clone(), path);
    }
    Ok(roots)
}

fn directory_within(directory: &Path, root: &Path) -> HostResult<bool> {
    // 复用原资源模块采用的文件身份比较，绑定挂载/Windows目录别名不能绕过字符串前缀检查。
    for ancestor in directory.ancestors() {
        if same_file::is_same_file(ancestor, root).map_err(HostError::storage)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn no_links(path: &Path) -> HostResult<()> {
    for ancestor in path.ancestors() {
        let meta = fs::symlink_metadata(ancestor).map_err(HostError::storage)?;
        if meta.file_type().is_symlink() {
            return Err(HostError::new(
                "scope_denied",
                "迁移拒绝符号链接或链接父目录",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(HostError::new("scope_denied", "迁移拒绝Windows重解析点"));
            }
        }
    }
    Ok(())
}
fn plain_directory(path: &Path) -> HostResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(HostError::storage)?
            .join(path)
    };
    no_links(&absolute)?;
    if !absolute.is_dir() {
        return Err(invalid("需要已存在的普通目录"));
    }
    openpaw_tools::safety::dunce_canonicalize(&absolute).map_err(HostError::storage)
}
fn plain_file(path: &Path) -> HostResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(HostError::storage)?
            .join(path)
    };
    no_links(&absolute)?;
    if !absolute.is_file() {
        return Err(invalid("需要普通文件"));
    }
    openpaw_tools::safety::dunce_canonicalize(&absolute).map_err(HostError::storage)
}
fn file_manifest(root: &Path, home: bool) -> HostResult<Value> {
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(HostError::storage)? {
            let entry = entry.map_err(HostError::storage)?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(HostError::storage)?;
            let normal = relative
                .to_str()
                .ok_or_else(|| invalid("迁移不接受非Unicode文件名"))?
                .replace('\\', "/");
            let meta = fs::symlink_metadata(&path).map_err(HostError::storage)?;
            no_links(&path)?;
            if meta.is_dir() {
                pending.push(path);
            } else if meta.is_file() {
                if normal == RESOURCE_LOCK
                    || normal == ".openpaw/todos.lock"
                    || home && normal == "engine-host/host.lock"
                {
                    continue;
                }
                files.push(json!({"path":normal,"size":meta.len(),"sha256":hash_file(&path)?}));
                if files.len() > 100000 {
                    return Err(invalid("迁移文件超过100000项，需缩小工作区后重新预检"));
                }
            } else {
                return Err(HostError::new(
                    "scope_denied",
                    "迁移拒绝设备、套接字及其他非普通文件",
                ));
            }
        }
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!(files))
}
fn relative_file(item: &Value) -> HostResult<&Path> {
    let path = Path::new(
        item["path"]
            .as_str()
            .ok_or_else(|| invalid("文件清单缺少路径"))?,
    );
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(HostError::new("scope_denied", "文件清单必须为规范相对路径"));
    }
    Ok(path)
}
fn manifest_size(files: &Value) -> HostResult<u64> {
    files
        .as_array()
        .ok_or_else(|| invalid("文件清单不合法"))?
        .iter()
        .try_fold(0u64, |total, file| {
            total
                .checked_add(
                    file["size"]
                        .as_u64()
                        .ok_or_else(|| invalid("文件大小不合法"))?,
                )
                .ok_or_else(|| invalid("文件总量溢出"))
        })
}
fn ensure_directory(path: &Path, created: &mut Vec<PathBuf>) -> HostResult<()> {
    if path.is_dir() {
        no_links(path)?;
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| invalid("目录缺少父路径"))?;
    ensure_directory(parent, created)?;
    fs::create_dir(path).map_err(HostError::storage)?;
    created.push(path.to_owned());
    Ok(())
}
fn copy_manifest(
    source: &Path,
    destination: &Path,
    files: &Value,
    created: &mut Vec<PathBuf>,
) -> HostResult<()> {
    for item in files.as_array().ok_or_else(|| invalid("文件清单不合法"))? {
        let relative = relative_file(item)?;
        let old = source.join(relative);
        let new = destination.join(relative);
        plain_file(&old)?;
        if hash_file(&old)? != item["sha256"] {
            return Err(HostError::new("conflict", "源文件在复制前已改变"));
        }
        if new.exists() {
            plain_file(&new)?;
            if hash_file(&new)? != item["sha256"] {
                return Err(HostError::new("conflict", "目标文件已改变，禁止覆盖"));
            }
            continue;
        }
        ensure_directory(new.parent().unwrap(), created)?;
        // create_new成功后立即登记，复制/落盘/校验失败均可只清理本轮创建项。
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&new)
            .map_err(HostError::storage)?;
        created.push(new.clone());
        std::io::copy(
            &mut File::open(&old).map_err(HostError::storage)?,
            &mut output,
        )
        .map_err(HostError::storage)?;
        output.sync_all().map_err(HostError::storage)?;
        drop(output);
        fs::set_permissions(
            &new,
            fs::metadata(&old)
                .map_err(HostError::storage)?
                .permissions(),
        )
        .map_err(HostError::storage)?;
        if hash_file(&new)? != item["sha256"] || hash_file(&old)? != item["sha256"] {
            return Err(HostError::new("conflict", "复制文件与已批准清单不一致"));
        }
    }
    Ok(())
}
fn copy_one(source: &Path, target: &Path, expected: Option<&str>) -> HostResult<()> {
    plain_file(source)?;
    let bytes = fs::read(source).map_err(HostError::storage)?;
    if expected.is_some_and(|hash| digest_bytes(&bytes) != hash) {
        return Err(HostError::new("conflict", "备份配置与预检不一致"));
    }
    write_new(target, &bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> HostResult<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(HostError::storage)?;
    file.write_all(bytes).map_err(HostError::storage)?;
    file.sync_all().map_err(HostError::storage)
}
fn cleanup_created(created: &[PathBuf]) {
    for path in created.iter().rev() {
        // 不递归删除、不删除预先存在项；目录非空或清理失败留给备份/人工核查。
        if path.is_dir() {
            let _ = fs::remove_dir(path);
        }
    }
}
fn digest_json(value: &Value) -> HostResult<String> {
    Ok(digest_bytes(
        &serde_json::to_vec(value).map_err(HostError::storage)?,
    ))
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex_digest(ring::digest::digest(&ring::digest::SHA256, bytes))
}

fn hex_digest(digest: ring::digest::Digest) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_file(path: &Path) -> HostResult<String> {
    // 流式计算，不能因为迁移项目包含大文件而将整个文件加载到内存。
    let mut file = File::open(path).map_err(HostError::storage)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(HostError::storage)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex_digest(digest.finish()))
}

fn available_bytes(path: &Path) -> HostResult<u64> {
    fs4::available_space(path).map_err(HostError::storage)
}

fn space_observations(plan: &Value) -> HostResult<Value> {
    let mut observations = Vec::new();
    for change in plan["moves"]
        .as_array()
        .ok_or_else(|| invalid("缺少迁移清单"))?
    {
        let path = Path::new(
            change["new_path"]
                .as_str()
                .ok_or_else(|| invalid("缺少目标路径"))?,
        );
        let required = manifest_size(&change["files"])?;
        let available = available_bytes(path)?;
        if available < required.saturating_add(16 * 1024 * 1024) {
            return Err(HostError::new("storage_unavailable", "目标可用空间不足"));
        }
        observations.push(
            json!({"team":change["team"],"required_bytes":required,"available_bytes":available}),
        );
    }
    Ok(
        json!({"space":observations,"note":"空间为预检时观察值，不作为未来保证；apply重新检查，摘要仅绑定payload"}),
    )
}

/// 功能：校验历史迁移产物的普通路径及本次实际读取文本摘要。
/// 参数：path为本团队新根中的文件，text为既有有界读取原文，expected为原可信清单SHA256。
/// 返回：文件仍为无链接普通路径且实际文本匹配时成功。
/// 错误：链接、缺失或正文变化明确失败，不回退旧共享目录。
/// 状态：只读；直接校验返回正文，不能另读新版本冒充原正文已校验。
/// 并发与取消：调用方持有所属根共享锁；非协作外部修改仍需显式处理。
/// 权限：只用于原Store已鉴权的历史映射，不接收浏览器授权别名。
/// 关联：需求/设计REQ-OP-025；DES-OP-013。
pub(crate) fn verify_migrated_text(path: &Path, text: &str, expected: &str) -> HostResult<()> {
    plain_file(path)?;
    if digest_bytes(text.as_bytes()) != expected {
        return Err(HostError::new(
            "conflict",
            "迁移后的历史产物已改变，请重新选择当前文件",
        ));
    }
    Ok(())
}

/// 功能：核对原host_events迁移审计的版本、范围及文件清单完整性。
/// 参数：event为可信数据库中读取的单个workspace_rebind对象。
/// 返回：已知版本和规范内容摘要一致时成功。
/// 错误：未知版本、重复ID、路径或摘要损坏均拒绝恢复。
/// 状态：纯校验，不补造缺失审计、不修改历史。
/// 并发与取消：由原Store或离线预检串行调用，无异步等待。
/// 权限：事件不能扩大团队根授权，仍需当前绑定及根身份校验。
/// 关联：需求/设计REQ-OP-025；DES-OP-013。
pub(crate) fn validate_migration_event(event: &Value) -> HostResult<()> {
    let bad = || HostError::new("storage_unavailable", "迁移审计版本、范围或摘要损坏");
    if event["version"] != 1 || event["approval"].as_str() != Some(APPROVAL) {
        return Err(bad());
    }
    for key in ["team", "old_root", "new_root"] {
        crate::config::valid_id(event[key].as_str().ok_or_else(bad)?).map_err(|_| bad())?;
    }
    if event["old_root"] == event["new_root"] || event["old_path"] == event["new_path"] {
        return Err(bad());
    }
    for key in ["old_path", "new_path"] {
        if !Path::new(event[key].as_str().ok_or_else(bad)?).is_absolute() {
            return Err(bad());
        }
    }
    let files = event["files"].as_array().ok_or_else(bad)?;
    let mut paths = BTreeSet::new();
    for file in files {
        relative_file(file).map_err(|_| bad())?;
        if !paths.insert(file["path"].as_str().unwrap())
            || file["size"].as_u64().is_none()
            || !file["sha256"]
                .as_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(bad());
        }
    }
    let mut ids = BTreeSet::new();
    for id in event["sessions"].as_array().ok_or_else(bad)? {
        let id = id.as_str().ok_or_else(bad)?;
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) || !ids.insert(id) {
            return Err(bad());
        }
    }
    if event["manifest_sha256"].as_str() != Some(digest_json(&event["files"])?.as_str())
        || !event["confirmation_sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(bad());
    }
    Ok(())
}
