//! 职责：按真实工作根统一缓存存储与共享/独占资源锁。
//! 输入输出：显式根与取消token到原OS锁守卫。
//! 依赖：Tokio公平锁、same-file及原sandbox文件锁。
//! 禁止职责：不实现沙箱，不宣称可阻止非协作外部写入。
use crate::{HostConfig, HostError, HostResult};
use openpaw_sandbox::file_lock::{try_lock_exclusive, try_lock_shared, FileLockGuard};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio_util::sync::CancellationToken;

/// 用途：同一真实工作根缓存的资源与原Store。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct Root {
    /// 用途：规范化授权工作根，不通过字符串小写推断目录身份。
    /// 字段：PathBuf，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) path: PathBuf,
    /// 用途：按此根缓存的原技能Store，查询只用checked入口。
    /// 字段：openpaw_memory::skills::store::FsSkillStore，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) skills: openpaw_memory::skills::store::FsSkillStore,
    /// 用途：按此根缓存的原目标Store，不调用目标调度。
    /// 字段：openpaw_goal_ledger::FsGoalStore，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) goals: openpaw_goal_ledger::FsGoalStore,
    gate: Arc<RwLock<()>>,
    lock_path: PathBuf,
}
/// 用途：按固定顺序持有公平锁和原OS锁的守卫。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct ResourceGuard {
    _os: FileLockGuard,
    _read: Option<OwnedRwLockReadGuard<()>>,
    _write: Option<OwnedRwLockWriteGuard<()>>,
}
impl Root {
    /// 功能：按进程内公平锁到原OS锁顺序取得资源。
    /// 参数：write为独占标记，cancel为本次操作取消。
    /// 返回：持锁守卫，析构按序释放。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：等待可取消，执行/清理期间调用方必须持有守卫。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) async fn acquire(
        &self,
        write: bool,
        cancel: &CancellationToken,
    ) -> HostResult<ResourceGuard> {
        let (read, write_guard) = if write {
            (
                None,
                Some(
                    tokio::select! {biased;_=cancel.cancelled()=>return Err(HostError::new("cancelled","资源等待已取消")),v=self.gate.clone().write_owned()=>v},
                ),
            )
        } else {
            (
                Some(
                    tokio::select! {biased;_=cancel.cancelled()=>return Err(HostError::new("cancelled","资源等待已取消")),v=self.gate.clone().read_owned()=>v},
                ),
                None,
            )
        };
        loop {
            if cancel.is_cancelled() {
                return Err(HostError::new("cancelled", "资源等待已取消"));
            }
            let result = if write {
                try_lock_exclusive(&self.lock_path)
            } else {
                try_lock_shared(&self.lock_path)
            };
            match result {
                Ok(os) => {
                    return Ok(ResourceGuard {
                        _os: os,
                        _read: read,
                        _write: write_guard,
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::select! {biased;_=cancel.cancelled()=>return Err(HostError::new("cancelled","资源等待已取消")),_=tokio::time::sleep(Duration::from_millis(15))=>{}}
                }
                Err(e) => return Err(HostError::storage(e)),
            }
        }
    }
}
/// 功能：识别真实工作根并建立共享资源缓存。
/// 参数：config为已校验显式本机配置。
/// 返回：规范home及按根标识缓存的资源。
/// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
/// 状态：验证互含及嵌套后创建宿主协作锁目录；同根别名共享。
/// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
/// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) fn prepare(config: &HostConfig) -> HostResult<(PathBuf, BTreeMap<String, Arc<Root>>)> {
    // 尚未创建home，先规范化最长既存父目录，拒绝根/数据互含后才写入。
    let home = openpaw_tools::safety::resolve(
        Path::new(if cfg!(windows) {
            config
                .home
                .ancestors()
                .last()
                .and_then(Path::to_str)
                .unwrap_or("/")
        } else {
            "/"
        }),
        &config.home.to_string_lossy(),
    )
    .map_err(|_| HostError::new("invalid_input", "无法规范化数据根"))?;
    let mut roots: BTreeMap<String, Arc<Root>> = BTreeMap::new();
    for (id, path) in &config.roots {
        let path = openpaw_tools::safety::dunce_canonicalize(path)
            .map_err(|_| HostError::new("invalid_input", "无法规范化工作根"))?;
        if home.starts_with(&path) || path.starts_with(&home) {
            return Err(HostError::new("scope_denied", "home与工作根必须互不包含"));
        }
        let mut alias = None;
        for existing in roots.values() {
            if same_file::is_same_file(&path, &existing.path).map_err(HostError::storage)? {
                alias = Some(existing.clone());
                break;
            }
            if path.starts_with(&existing.path) || existing.path.starts_with(&path) {
                return Err(HostError::new("invalid_input", "不支持嵌套工作根"));
            }
        }
        let root = if let Some(alias) = alias {
            alias
        } else {
            let meta = openpaw_tools::safety::resolve(&path, ".openpaw")
                .map_err(|_| HostError::new("scope_denied", "工作根元数据链接越界"))?;
            std::fs::create_dir_all(&meta).map_err(HostError::storage)?;
            let lock_path =
                openpaw_tools::safety::resolve(&path, ".openpaw/engine-host-resource.lock")
                    .map_err(|_| HostError::new("scope_denied", "锁路径越界"))?;
            Arc::new(Root {
                skills: openpaw_memory::skills::store::FsSkillStore::new(
                    home.join("skills"),
                    Some(meta.join("skills")),
                ),
                goals: openpaw_goal_ledger::FsGoalStore::new(meta.join("goals")),
                path,
                gate: Arc::new(RwLock::new(())),
                lock_path,
            })
        };
        roots.insert(id.clone(), root);
    }
    std::fs::create_dir_all(&home).map_err(HostError::storage)?;
    Ok((home, roots))
}
