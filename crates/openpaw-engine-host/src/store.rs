//! 职责：串行化Host元数据与原JSONL写入，恢复不重放副作用。
//! 输入输出：可等待命令到已提交事实、一致视图和通知。
//! 依赖：原SessionWriter/Reader/SessionDb，单一SQLite元数据库。
//! 禁止职责：不在host.db复制聊天消息，不自动重试工具。
use crate::{types::*, HostError, HostResult};
use openpaw_core::{Session, SessionEvent};
use openpaw_sandbox::file_lock::{try_lock_exclusive, FileLockGuard};
use openpaw_store::{SessionDb, SessionReader, SessionWriter};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{broadcast, mpsc, oneshot};

enum Job {
    Call(Box<dyn FnOnce(&mut Db) + Send>),
    Close(oneshot::Sender<HostResult<()>>),
}
#[derive(Clone)]
/// 用途：可克隆的单写存储命令句柄。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct Store {
    tx: mpsc::Sender<Job>,
}
/// 用途：原JSONL写入器及Host元数据的唯一串行所有者。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct Db {
    conn: Connection,
    home: PathBuf,
    writers: HashMap<String, SessionWriter>,
    index: SessionDb,
    owner: Option<FileLockGuard>,
    /// 用途：有界展示通知广播；关键事实从持久Store重取。
    /// 字段：broadcast::Sender<HostNotice>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) notices: broadcast::Sender<HostNotice>,
    /// 用途：活动Run的有界文本展示缓存，不作为第二份账本。
    /// 字段：Arc<Mutex<HashMap<String, RunText>>>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) live: Arc<Mutex<HashMap<String, RunText>>>,
    /// 最近一次由Office投影读取的Host事实revision；仅用于避免空闲期
    /// 重复扫描同一批Run，不是第二份投影或持久事实。
    pub(crate) office_projection_source: HashMap<String, u64>,
    /// 交接动画顺序窗口的内存截止时间；过期后才再次重建投影。
    /// 不写入Office快照，也不作为业务状态来源。
    pub(crate) office_projection_handoff_until: HashMap<String, chrono::DateTime<chrono::Utc>>,
}
impl Store {
    /// 功能：恢复原JSONL及宿主元数据并启动单写协调器。
    /// 参数：home为授权数据根，notices与live为展示通知及缓冲。
    /// 返回：持有home所有权的Store。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：恢复不重放工具；未知schema或损坏拒绝启动。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) async fn open(
        home: PathBuf,
        roots: BTreeMap<String, PathBuf>,
        notices: broadcast::Sender<HostNotice>,
        live: Arc<Mutex<HashMap<String, RunText>>>,
    ) -> HostResult<Self> {
        let db = tokio::task::spawn_blocking(move || Db::open(home, roots, notices, live))
            .await
            .map_err(HostError::storage)??;
        let (tx, mut rx) = mpsc::channel::<Job>(256);
        std::thread::Builder::new()
            .name("openpaw-host-store".into())
            .spawn(move || {
                let mut db = db;
                while let Some(job) = rx.blocking_recv() {
                    match job {
                        Job::Call(job) => job(&mut db),
                        Job::Close(reply) => {
                            let result = db.close();
                            drop(db);
                            let _ = reply.send(result);
                            return;
                        }
                    }
                }
            })
            .map_err(HostError::storage)?;
        Ok(Self { tx })
    }
    /// 功能：将同步存储操作送入单写线程并等待完成。
    /// 参数：f为不跨异步等待的原存储操作。
    /// 返回：原操作已完成的结果。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：调用方取消也不丢弃已派发的持久写入。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db) -> HostResult<T> + Send + 'static,
    ) -> HostResult<T> {
        let queued_at = Instant::now();
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Job::Call(Box::new(move |db| {
                let started_at = Instant::now();
                let result = if db.owner.is_some() {
                    f(db)
                } else {
                    Err(HostError::new("storage_unavailable", "Host存储已关闭"))
                };
                if std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some() {
                    eprintln!(
                        "engine-host diagnostics store queue_ms={} run_ms={}",
                        started_at.duration_since(queued_at).as_millis(),
                        started_at.elapsed().as_millis()
                    );
                }
                let _ = tx.send(result);
            })))
            .await
            .map_err(HostError::storage)?;
        rx.await.map_err(HostError::storage)?
    }
}
impl Store {
    /// 功能：关闭原账本及两个SQLite连接后确认存储已结束。
    /// 参数：self为单写Store句柄。
    /// 返回：全部句柄析构完成后的结果。
    /// 错误：队列、检查点或线程提前退出错误。
    /// 状态：此后拒绝新存储调用，home锁最后释放。
    /// 并发与取消：必须先等待全部Run清理；不通过提前发回确认冒充句柄已释放。
    /// 权限：只关闭本实例持有的数据，不删除文件。
    /// 关联：需求/设计REQ-OP-009；DES-OP-006。
    pub(crate) async fn close(&self) -> HostResult<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Job::Close(tx))
            .await
            .map_err(HostError::storage)?;
        rx.await.map_err(HostError::storage)?
    }
}
impl Db {
    /// 功能：读取当前Run最后一份业务步骤派生事实。
    /// 参数：run为可信运行标识。
    /// 返回：已有计划或无计划。
    /// 错误：SQL或已存JSON损坏明确上报。
    /// 状态：只读，不为旧Run补造计划。
    /// 并发与取消：由原单写Store串行调用，无资源锁。
    /// 权限：校验Run和Session删除门禁。
    /// 关联：需求/设计REQ-OP-012；CHG-2026-0006第9.9节。
    pub(crate) fn progress(&self, run: &str) -> HostResult<Option<serde_json::Value>> {
        // 投影会按办公室中的每个Run查询进度；这里不需要完整Run视图或JSONL正文。
        // 旧实现调用get_run会解码整个Session账本，多个办公室启动时把数百次昂贵读取
        // 串在同一Store队列中。只读取既有host_runs.session并完成同样的范围校验，
        // 进度事实仍来自host_events，正文和终态语义保持不变。
        let session: String = self
            .conn
            .query_row("SELECT session FROM host_runs WHERE id=?1", [run], |row| {
                row.get(0)
            })
            .optional()
            .map_err(HostError::storage)?
            .ok_or_else(|| HostError::new("not_found", "运行不存在"))?;
        self.binding(&session)?;
        let raw:Option<String>=self.conn.query_row("SELECT reference FROM host_events WHERE kind='run_progress' AND json_extract(reference,'$.run_id')=?1 ORDER BY revision DESC LIMIT 1",[run],|r|r.get(0)).optional().map_err(HostError::storage)?;
        raw.map(|v| serde_json::from_str(&v).map_err(HostError::storage))
            .transpose()
    }

    /// 功能：将步骤报告与原Run及真实调用绑定，持久化后通知投影。
    /// 参数：binding为引擎可信最终调用绑定，value为报告输入。
    /// 返回：已提交或重复内容无需追加时为空。
    /// 错误：范围、取消、假完成、容量或存储失败明确拒绝。
    /// 状态：每Run最多256次有效报告，原host_events保留固定分母和证据。
    /// 并发与取消：状态检查和追加处于原Store单写命令，不等待审批或工作区锁。
    /// 权限：仅Office绑定的同Run report_progress，不批准其他工具。
    /// 关联：需求/设计REQ-OP-012；CHG-2026-0006第9.9节。
    pub(crate) fn report_progress(
        &mut self,
        binding: &openpaw_core::ToolCallBinding,
        value: &serde_json::Value,
    ) -> HostResult<()> {
        let run = self.get_run(&binding.run_id)?;
        if self.binding(&run.session_id)?.scope.namespace != "office"
            || binding.tool_name != "report_progress"
            || binding.input != *value
        {
            return Err(HostError::new(
                "scope_denied",
                "步骤报告不属于当前Office运行",
            ));
        }
        let previous = self.progress(&run.run_id)?;
        let mut stmt=self.conn.prepare("SELECT reference FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.run_id')=?1 ORDER BY revision").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([&run.run_id], |r| r.get::<_, String>(0))
            .map_err(HostError::storage)?;
        let mut started = Vec::new();
        for row in rows {
            started.push(
                serde_json::from_str(&row.map_err(HostError::storage)?)
                    .map_err(HostError::storage)?,
            );
        }
        let delegated = self.delegated_writes(&run.run_id)?;
        let next = crate::task_progress::update_with_delegations(
            previous.clone(),
            value,
            &run,
            &started,
            &delegated,
        )
        .map_err(|mut error| {
            if error.kind == "invalid_progress" {
                // 拒绝仍是拒绝；仅提供可恢复的本轮证据标识，不替模型选择或确认完成。
                error
                    .message
                    .push_str(&crate::task_progress::recovery_hint(&run, &started));
            }
            error
        })?;
        if previous.as_ref() == Some(&next) {
            return Ok(());
        }
        let count:u64=self.conn.query_row("SELECT COUNT(*) FROM host_events WHERE kind='run_progress' AND json_extract(reference,'$.run_id')=?1",[&run.run_id],|r|r.get(0)).map_err(HostError::storage)?;
        if count >= 256 {
            return Err(HostError::new(
                "capacity_exceeded",
                "本轮步骤报告已达256次上限",
            ));
        }
        let revision = self.bump("run_progress", &next.to_string())?;
        let _ = self.notices.send(HostNotice::Changed { revision });
        Ok(())
    }

    fn open(
        home: PathBuf,
        roots: BTreeMap<String, PathBuf>,
        notices: broadcast::Sender<HostNotice>,
        live: Arc<Mutex<HashMap<String, RunText>>>,
    ) -> HostResult<Self> {
        let meta = home.join("engine-host");
        std::fs::create_dir_all(&meta).map_err(HostError::storage)?;
        let owner = try_lock_exclusive(&meta.join("host.lock")).map_err(|_| {
            HostError::new("storage_unavailable", "此数据目录已被使用或无法取得所有权")
        })?;
        let conn = Connection::open(meta.join("host.db")).map_err(HostError::storage)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(HostError::storage)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(HostError::storage)?;
        if !(0..=4).contains(&version) {
            return Err(HostError::new(
                "unsupported",
                "不支持的Host数据版本，拒绝写入",
            ));
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS host_roots(id TEXT PRIMARY KEY,path TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS host_bindings(id TEXT PRIMARY KEY, namespace TEXT NOT NULL,slot TEXT NOT NULL,conversation TEXT NOT NULL,data TEXT NOT NULL, UNIQUE(namespace,slot,conversation));
        CREATE TABLE IF NOT EXISTS host_runs(id TEXT PRIMARY KEY,session TEXT NOT NULL,data TEXT NOT NULL,input_event_offset INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS host_approvals(id TEXT PRIMARY KEY,run TEXT NOT NULL,call TEXT NOT NULL,data TEXT NOT NULL,UNIQUE(run,call));
        CREATE TABLE IF NOT EXISTS host_events(revision INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,reference TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS host_office_calls(id INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT NOT NULL,call TEXT NOT NULL,UNIQUE(run,call));
        CREATE TABLE IF NOT EXISTS host_context_import(session TEXT PRIMARY KEY, event_offset INTEGER NOT NULL, count INTEGER NOT NULL, fingerprint TEXT NOT NULL, complete INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS host_office_projection(team TEXT PRIMARY KEY, data TEXT NOT NULL, sync_state TEXT NOT NULL);
        SELECT id,path FROM host_roots LIMIT 0;
        SELECT id,namespace,slot,conversation,data FROM host_bindings LIMIT 0;
        SELECT id,session,data,input_event_offset FROM host_runs LIMIT 0;
        SELECT id,run,call,data FROM host_approvals LIMIT 0;
        SELECT revision,kind,reference FROM host_events LIMIT 0;
        SELECT session,event_offset,count,fingerprint,complete FROM host_context_import LIMIT 0;
        SELECT id,run,call FROM host_office_calls LIMIT 0;
        SELECT team,data,sync_state FROM host_office_projection LIMIT 0;
        CREATE TABLE IF NOT EXISTS host_requests(session TEXT NOT NULL,key TEXT NOT NULL,request TEXT NOT NULL,run TEXT NOT NULL UNIQUE,PRIMARY KEY(session,key));
        SELECT session,key,request,run FROM host_requests LIMIT 0;
        CREATE TABLE IF NOT EXISTS host_session_deletions(scope TEXT PRIMARY KEY,session TEXT NOT NULL UNIQUE,phase TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS host_office_decisions(approval_id TEXT PRIMARY KEY,display_id TEXT NOT NULL UNIQUE,scope TEXT NOT NULL,expires_at INTEGER NOT NULL,state TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS host_office_actions(action_id TEXT PRIMARY KEY,scope TEXT NOT NULL,kind TEXT NOT NULL,status TEXT NOT NULL,expires_at INTEGER NOT NULL,context TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS host_office_scene(event_id TEXT PRIMARY KEY,command_id TEXT NOT NULL,expires_at INTEGER NOT NULL,status TEXT NOT NULL);
        PRAGMA user_version=4;
        COMMIT;").map_err(HostError::storage)?;
        for (id, path) in roots {
            let prior = conn.query_row("SELECT path FROM host_roots WHERE id=?1", [&id], |r| {
                r.get::<_, String>(0)
            });
            match prior {
                Ok(prior) if std::path::Path::new(&prior) != path => {
                    return Err(HostError::new(
                        "scope_denied",
                        "既有工作根身份不能通过配置切换",
                    ))
                }
                Ok(_) => {}
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    conn.execute(
                        "INSERT INTO host_roots(id,path) VALUES(?1,?2)",
                        params![id, path.to_string_lossy()],
                    )
                    .map_err(HostError::storage)?;
                }
                Err(e) => return Err(HostError::storage(e)),
            }
        }
        let index = match SessionDb::open(home.join("index.db")) {
            Ok(index) => index,
            Err(openpaw_store::DbError::Sqlite(rusqlite::Error::SqliteFailure(ref err, _)))
                if matches!(
                    err.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) =>
            {
                // 仅索引可重建：保留损坏文件及其伴随WAL，绝不删除host.db。
                let backup = home.join(format!("index-corrupt-{}", uuid::Uuid::new_v4().simple()));
                std::fs::create_dir(&backup).map_err(HostError::storage)?;
                for name in ["index.db", "index.db-wal", "index.db-shm"] {
                    let path = home.join(name);
                    match std::fs::rename(&path, backup.join(name)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(HostError::storage(e)),
                    }
                }
                eprintln!("engine-host: 损坏的可重建索引已备份，将从原JSONL重建");
                SessionDb::open(home.join("index.db")).map_err(HostError::storage)?
            }
            Err(e) => return Err(HostError::storage(e)),
        };
        let mut db = Self {
            conn,
            home,
            writers: HashMap::new(),
            index,
            owner: Some(owner),
            notices,
            live,
            office_projection_source: HashMap::new(),
            office_projection_handoff_until: HashMap::new(),
        };
        db.validate_workspace_rebinds()?;
        db.recover()?;
        Ok(db)
    }
    /// 功能：读取宿主已有会话绑定。
    /// 参数：self为单写线程内Db。
    /// 返回：原绑定列表。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读元数据。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn bindings(&self) -> HostResult<Vec<SessionBinding>> {
        self.all("SELECT data FROM host_bindings")
    }
    /// 功能：读取持久Run元数据。
    /// 参数：self为单写线程内Db。
    /// 返回：不含重复聊天账本的Run列表。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读宿主元数据。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn runs(&self) -> HostResult<Vec<RunView>> {
        self.all("SELECT data FROM host_runs")
    }
    /// 功能：按Session读取宿主Run元数据，避免人物详情为查一名成员解析整个办公室历史。
    /// 参数：session为已校验的原JSONL会话ID。
    /// 返回：该Session的Run元数据，工具与正文仍按需读取。
    /// 状态：只读，不改变运行生命周期；跨Session仍由调用方校验。
    pub(crate) fn runs_for_session(&self, session: &str) -> HostResult<Vec<RunView>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM host_runs WHERE session=?1")
            .map_err(HostError::storage)?;
        let rows = stmt
            .query_map([session], |row| row.get::<_, String>(0))
            .map_err(HostError::storage)?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(HostError::storage)?).map_err(HostError::storage)
        })
        .collect()
    }
    /// 功能：读取持久审批记录。
    /// 参数：self为单写线程内Db。
    /// 返回：完整绑定及消费状态列表。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读宿主元数据。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn approvals(&self) -> HostResult<Vec<ApprovalView>> {
        self.all("SELECT data FROM host_approvals")
    }
    fn all<T: serde::de::DeserializeOwned>(&self, sql: &str) -> HostResult<Vec<T>> {
        read_json_rows(&self.conn, sql)
    }
    /// 功能：读取已提交事实序号。
    /// 参数：self为单写线程内Db。
    /// 返回：当前最大revision。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读，不分配新序号。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn revision(&self) -> HostResult<u64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(revision),0) FROM host_events",
                [],
                |r| r.get(0),
            )
            .map_err(HostError::storage)
    }
    /// 功能：保存缺失的宿主生命周期事实并分配序号。
    /// 参数：kind与reference为生命周期及原账本引用。
    /// 返回：已提交序号。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：同步SQLite提交；引用不得复制完整聊天正文。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn bump(&self, kind: &str, reference: &str) -> HostResult<u64> {
        if kind == "tool_started" {
            let binding: openpaw_core::ToolCallBinding =
                serde_json::from_str(reference).map_err(HostError::storage)?;
            let exists:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.run_id')=?1 AND json_extract(reference,'$.call_id')=?2)",params![binding.run_id,binding.call_id],|row|row.get(0)).map_err(HostError::storage)?;
            if exists {
                return Err(HostError::new(
                    "approval_conflict",
                    "相同工具调用ID不得再次进入主体",
                ));
            }
        }
        self.conn
            .execute(
                "INSERT INTO host_events(kind,reference) VALUES(?1,?2)",
                params![kind, reference],
            )
            .map_err(HostError::storage)?;
        Ok(self.conn.last_insert_rowid() as u64)
    }
    fn publish(&self, revision: u64) {
        let _ = self.notices.send(HostNotice::Changed { revision });
    }
    /// 功能：定位原会话的固定客户端绑定。
    /// 参数：id为会话ID。
    /// 返回：原SessionBinding。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读；访问范围由共享服务随后校验。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn binding(&self, id: &str) -> HostResult<SessionBinding> {
        let binding = self
            .bindings()?
            .into_iter()
            .find(|b| b.session_id == id)
            .ok_or_else(|| HostError::new("not_found", "会话不存在"))?;
        self.check_live_scope(&binding.scope)?;
        Ok(binding)
    }
    /// 功能：从原账本和宿主事实生成Run当前视图。
    /// 参数：id为Run稳定ID。
    /// 返回：运行、文本和真实工具阶段。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读；pending不能映射为实际running。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn get_run(&self, id: &str) -> HostResult<RunView> {
        // 人物详情及周期投影会逐个查询Run；不要反复解码其他历史Run和工具事实。
        // 仍使用同一Store序列与原账本，精确筛选不引入缓存或改变状态覆盖顺序。
        let data: String = self
            .conn
            .query_row("SELECT data FROM host_runs WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(HostError::storage)?
            .ok_or_else(|| HostError::new("not_found", "运行不存在"))?;
        let mut run: RunView = serde_json::from_str(&data).map_err(HostError::storage)?;
        let events = self.events(&run.session_id)?;
        let mut active = false;
        let text = committed_run_text(&events, id);
        let mut tools = std::collections::BTreeMap::new();
        for e in events {
            match e {
                SessionEvent::RunStarted { run_id } => active = run_id == id,
                SessionEvent::ToolCallStart { call_id, tool_name } if active => {
                    tools.insert(
                        call_id.clone(),
                        ToolView {
                            call_id,
                            tool_name,
                            state: "proposed".into(),
                        },
                    );
                }
                SessionEvent::ToolCallComplete {
                    call_id, success, ..
                } if active => {
                    if let Some(t) = tools.get_mut(&call_id) {
                        t.state = if success { "completed" } else { "failed" }.into();
                    }
                }
                SessionEvent::RunFinished {
                    run_id,
                    error: Some(error),
                    ..
                // 历史通用错误可从账本细分；宿主已锁存的存储等确定分类不能被通用错误覆盖。
                } if run_id == id && run.error.as_ref().is_none_or(|e| matches!(e.kind.as_str(), "interrupted" | "run_failed" | "timeout" | "cancelled")) => {
                    run.error = Some(crate::service::safe_run_error(&error, run.state));
                }
                _ => {}
            }
        }
        let mut stmt=self.conn.prepare("SELECT kind,reference FROM host_events WHERE kind IN ('tool_waiting_resource','tool_started','tool_result') AND json_extract(reference,'$.run_id')=?1 ORDER BY revision").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(HostError::storage)?;
        for row in rows {
            let (kind, value) = row.map_err(HostError::storage)?;
            let value: serde_json::Value =
                serde_json::from_str(&value).map_err(HostError::storage)?;
            if value["run_id"].as_str() == Some(id) {
                if let Some(tool) = value["call_id"].as_str().and_then(|c| tools.get_mut(c)) {
                    tool.state = if kind == "tool_waiting_resource" {
                        "awaiting_resource"
                    } else if kind == "tool_started" {
                        "running"
                    } else if value["success"] == true {
                        "completed"
                    } else {
                        "failed"
                    }
                    .into();
                }
            }
        }
        let mut approvals = self
            .conn
            .prepare("SELECT data FROM host_approvals WHERE run=?1")
            .map_err(HostError::storage)?;
        let rows = approvals
            .query_map([id], |row| row.get::<_, String>(0))
            .map_err(HostError::storage)?;
        for row in rows {
            let a: ApprovalView = serde_json::from_str(&row.map_err(HostError::storage)?)
                .map_err(HostError::storage)?;
            if a.binding.run_id == id && a.status == "pending" {
                if let Some(t) = tools.get_mut(&a.binding.call_id) {
                    t.state = "awaiting_approval".into();
                }
            }
        }
        run.tools = tools.into_values().collect();
        // 检查点只补足与可靠账本相符的展示尾部；不将它恢复为模型消息。
        if run.text.len() > 4 * 1024 * 1024 {
            return Err(HostError::new(
                "storage_unavailable",
                "展示检查点超过原有上限",
            ));
        }
        let checkpoint = std::mem::take(&mut run.text);
        let checkpoint_truncated = run.text_truncated;
        run.text = text.text().to_owned();
        run.text_truncated = false;
        merge_run_display(&mut run, &checkpoint, checkpoint_truncated)?;
        if !run.state.is_terminal() {
            if let Some(live) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
                merge_run_display(&mut run, live.text(), live.truncated())?;
            }
        }
        Ok(run)
    }
    /// 功能：读取Office投影所需的轻量Run视图，不解析完整JSONL正文。
    /// 参数：id为已持久化Run标识。
    /// 返回：状态、工具阶段和错误；回复正文由详情/历史查询按需读取。
    /// 错误：SQL、控制事实损坏或未知Run。
    /// 状态：只读，不改变Run或工具事实；仅供投影扫描使用。
    /// 并发与取消：单Store序列内按Run主键和Host事实精确读取，避免全会话账本扫描。
    /// 权限：调用方仍须先校验Office绑定；不扩大工具或文件权限。
    /// 关联：D76人物详情冷启动优化；不改变公共Run接口。
    pub(crate) fn get_run_projection(&self, id: &str) -> HostResult<RunView> {
        // 轻量路径不能丢失账本正文、终态事实或工具调用关联。投影缓存已经
        // 让人物点击不再触发此方法；没有可信缓存时沿用完整读取，保证旧数据
        // 和测试夹具与公开Run视图完全一致。后续若增加持久化摘要，必须先用
        // JSONL golden 验证它覆盖 get_run 的全部字段，再替换这里。
        self.get_run(id)
    }
    /// 功能：将本Run的有界实时展示尾部合并保存到既有元数据。
    /// 参数：id和session为已接受运行的精确身份。
    /// 返回：检查点写入或无变化、已结束、已删除时成功。
    /// 错误：SQL失败或展示前缀与可靠账本冲突时明确报错。
    /// 状态：只UPDATE既存Run，不新增消息；完整消息已提交时清除冗余尾部。
    /// 并发与取消：原单写线程串行执行；删除墓碑后无操作，不复活会话。
    /// 权限：同时核对Run、Session和存活作用域，不接收客户端正文。
    /// 关联：需求/设计REQ-OP-009、REQ-OP-016；DES-OP-006。
    pub(crate) fn checkpoint_run(&mut self, id: &str, session: &str) -> HostResult<()> {
        let data: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM host_runs WHERE id=?1 AND session=?2",
                params![id, session],
                |r| r.get(0),
            )
            .optional()
            .map_err(HostError::storage)?;
        let Some(data) = data else {
            return Ok(());
        };
        let mut run: RunView = serde_json::from_str(&data).map_err(HostError::storage)?;
        match self.binding(session) {
            Ok(_) => {}
            Err(e) if matches!(e.kind.as_str(), "session_deleted" | "not_found") => return Ok(()),
            Err(e) => return Err(e),
        }
        if run.state.is_terminal() {
            return Ok(());
        }
        let (live, truncated) = {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let Some(live) = live.get(id) else {
                return Ok(());
            };
            (live.text().to_owned(), live.truncated())
        };
        let ledger = committed_run_text(&self.events(session)?, id);
        let mut display = run.clone();
        display.text = ledger.text().to_owned();
        display.text_truncated = false;
        merge_run_display(&mut display, &run.text, run.text_truncated)?;
        merge_run_display(&mut display, &live, truncated)?;
        // 仅保留未提交的尾部展示快照；账本追上后不用重复存完整回复。
        let next = if display.text.len() > ledger.text().len() {
            display.text
        } else {
            String::new()
        };
        let next_truncated = truncated && !next.is_empty();
        if next.len() > 4 * 1024 * 1024 {
            return Err(HostError::new(
                "storage_unavailable",
                "展示检查点超过原有上限",
            ));
        }
        if run.text == next && run.text_truncated == next_truncated {
            return Ok(());
        }
        run.text = next;
        run.text_truncated = next_truncated;
        self.conn.execute(
            "UPDATE host_runs SET data=?3 WHERE id=?1 AND session=?2 AND EXISTS(SELECT 1 FROM host_bindings WHERE id=?2) AND NOT EXISTS(SELECT 1 FROM host_session_deletions WHERE session=?2)",
            params![id, session, serde_json::to_string(&run).map_err(HostError::storage)?],
        ).map_err(HostError::storage)?;
        Ok(())
    }
    /// 功能：原子保存Run状态与生命周期revision。
    /// 参数：run为新视图，offset为原输入事件位置。
    /// 返回：提交后的Run视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不保存第二份消息；cancelling不能退回running，提交后通知。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn save_run(&mut self, run: RunView, offset: usize) -> HostResult<RunView> {
        self.save_run_with_request(run, offset, None)
    }
    /// 功能：将请求关联与首次Run元数据在同一事务提交。
    /// 参数：run为视图，offset为JSONL输入位置，request为可选客户端幂等键和规范请求。
    /// 返回：持久Run视图。
    /// 错误：唯一键冲突、序列化或SQL失败。
    /// 状态：request仅作精确重试比较，不用于恢复模型历史。
    /// 并发与取消：单写线程；先提交关联再允许执行副作用。
    /// 权限：作用域由共享服务校验，键不是授权。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
    pub(crate) fn save_run_with_request(
        &mut self,
        mut run: RunView,
        offset: usize,
        request: Option<(String, String)>,
    ) -> HostResult<RunView> {
        // 接受和删除都经过同一单写队列：预留后发生的删除必须拒绝接受。
        if matches!(run.state, RunState::Accepting | RunState::Accepted) {
            self.check_live_scope(&self.binding(&run.session_id)?.scope)?;
        }
        // 取消不能被稍晚开始的执行任务逆转为running。
        if run.state == RunState::Running
            && self
                .runs()?
                .iter()
                .any(|r| r.run_id == run.run_id && r.state == RunState::Cancelling)
        {
            run.state = RunState::Cancelling;
        }
        let checkpoint: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM host_runs WHERE id=?1 AND session=?2",
                params![run.run_id, run.session_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(HostError::storage)?;
        let checkpoint: Option<RunView> = checkpoint
            .map(|v| serde_json::from_str(&v).map_err(HostError::storage))
            .transpose()?;
        // 状态与revision同一SQLite事务，发布永远在提交之后。
        let tx = self.conn.transaction().map_err(HostError::storage)?;
        tx.execute(
            "INSERT INTO host_events(kind,reference) VALUES('run',?1)",
            [&run.run_id],
        )
        .map_err(HostError::storage)?;
        run.revision = tx.last_insert_rowid() as u64;
        let mut persisted = run.clone();
        persisted.text = checkpoint
            .as_ref()
            .map(|v| v.text.clone())
            .unwrap_or_default();
        persisted.text_truncated = checkpoint.is_some_and(|v| v.text_truncated);
        persisted.tools.clear();
        tx.execute("INSERT INTO host_runs(id,session,data,input_event_offset) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![run.run_id,run.session_id,serde_json::to_string(&persisted).map_err(HostError::storage)?,offset]).map_err(HostError::storage)?;
        if let Some((key, canonical)) = request {
            tx.execute(
                "INSERT INTO host_requests(session,key,request,run) VALUES(?1,?2,?3,?4)",
                params![run.session_id, key, canonical, run.run_id],
            )
            .map_err(HostError::storage)?;
        }
        tx.commit().map_err(HostError::storage)?;
        self.publish(run.revision);
        Ok(run)
    }
    /// 功能：原子保存审批状态及其revision。
    /// 参数：a为协调线程已校验的精确审批。
    /// 返回：提交后的审批视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：决定与消费由同一串行协调器校验，提交后才通知。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn save_approval(&mut self, mut a: ApprovalView) -> HostResult<ApprovalView> {
        let tx = self.conn.transaction().map_err(HostError::storage)?;
        tx.execute(
            "INSERT INTO host_events(kind,reference) VALUES('approval',?1)",
            [&a.approval_id],
        )
        .map_err(HostError::storage)?;
        a.revision = tx.last_insert_rowid() as u64;
        tx.execute("INSERT INTO host_approvals(id,run,call,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![a.approval_id,a.binding.run_id,a.binding.call_id,serde_json::to_string(&a).map_err(HostError::storage)?]).map_err(HostError::storage)?;
        tx.commit().map_err(HostError::storage)?;
        self.publish(a.revision);
        Ok(a)
    }
    /// 功能：创建原JSONL并保存客户端绑定。
    /// 参数：binding为固定范围，session为原core会话。
    /// 返回：可靠保存后的成功。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：先Meta再绑定，失败不会启动工具。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn create_session(
        &mut self,
        binding: SessionBinding,
        session: Session,
    ) -> HostResult<()> {
        self.check_live_scope(&binding.scope)?;
        let path = self.path(&binding.session_id)?;
        let mut writer = SessionWriter::create(path).map_err(HostError::storage)?;
        writer
            .append(&SessionEvent::Meta(session.meta.clone()))
            .map_err(HostError::storage)?;
        self.index
            .upsert_session(&session.meta, writer.path())
            .map_err(HostError::storage)?;
        self.conn.execute("INSERT INTO host_bindings(id,namespace,slot,conversation,data) VALUES(?1,?2,?3,?4,?5)",params![binding.session_id,binding.scope.namespace,binding.scope.slot_id,binding.scope.conversation_id,serde_json::to_string(&binding).map_err(HostError::storage)?]).map_err(HostError::storage)?;
        self.writers.insert(binding.session_id.clone(), writer);
        let rev = self.bump("session", &binding.session_id)?;
        self.publish(rev);
        Ok(())
    }
    /// 功能：读取原会话账本事实。
    /// 参数：id为授权会话ID。
    /// 返回：原SessionEvent顺序列表。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读，不以压缩上下文覆盖历史。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn events(&self, id: &str) -> HostResult<Vec<SessionEvent>> {
        Ok(SessionReader::read(self.path(id)?)
            .map_err(HostError::storage)?
            .events)
    }
    /// 功能：通过原Reader恢复模型上下文。
    /// 参数：id为授权会话ID。
    /// 返回：原core Session。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读恢复上下文，不执行旧工具。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn session(&self, id: &str) -> HostResult<Session> {
        Ok(SessionReader::read(self.path(id)?)
            .map_err(HostError::storage)?
            .restore())
    }
    /// 功能：顺序同步追加原Session事件并更新索引。
    /// 参数：id为会话ID，event为引擎事实。
    /// 返回：可靠保存后的成功。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：JSONL先保存；索引/元数据错误上报，不重试副作用。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn append(&mut self, id: &str, event: SessionEvent) -> HostResult<()> {
        let seq = self.events(id)?.len();
        let writer = self
            .writers
            .get_mut(id)
            .ok_or_else(|| HostError::new("not_found", "会话写入器不存在"))?;
        writer.append(&event).map_err(HostError::storage)?;
        // 索引失败不撤销已保存事实，重启按JSONL重建；当前Run停止以免伪报成功。
        self.index
            .index_event(id, seq, &event)
            .map_err(HostError::storage)?;
        let rev = self.bump("ledger", &format!("{id}:{seq}"))?;
        self.publish(rev);
        Ok(())
    }
    fn path(&self, id: &str) -> HostResult<PathBuf> {
        if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(HostError::new("invalid_input", "非法会话标识"));
        }
        Ok(self.home.join("sessions").join(format!("{id}.jsonl")))
    }
    /// 功能：在已有单写协调器中核对完整历史后缀或恢复首次导入。
    /// 参数：id为原Session，history为仅含user/assistant文本的外部完整轮次。
    /// 返回：历史一致并可靠保存时成功。
    /// 错误：已完成历史后缀不符、导入冲突或原账本写入失败。
    /// 状态：导入只记录来源元数据，消息仍追加原JSONL；不恢复授权。
    /// 并发与取消：调用方必须先预留Session/槽，避免首次导入交错。
    /// 权限：原JSONL已提交前缀逐条比较，校验和不作为授权或防碰撞凭证。
    /// 关联：需求/设计REQ-OP-010；DES-OP-003/007。
    pub(crate) fn reconcile_history(
        &mut self,
        id: &str,
        history: &[openpaw_core::Message],
    ) -> HostResult<()> {
        use openpaw_core::{Message, Role};
        let same = |a: &Message, b: &Message| a.role == b.role && a.text() == b.text();
        let runs: Vec<_> = self
            .runs()?
            .into_iter()
            .filter(|r| r.session_id == id)
            .collect();
        let events = self.events(id)?;
        let imported:Option<(usize,usize,String,bool)>=self.conn.query_row(
            "SELECT event_offset,count,fingerprint,complete FROM host_context_import WHERE session=?1",[id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(HostError::storage)?;
        if runs.is_empty() {
            // 校验和仅诊断同一次未完成导入；真正已保存的文本始终逐条比较。
            let serialized = serde_json::to_vec(history).map_err(HostError::storage)?;
            let fingerprint = format!(
                "fnv1a64:{:016x}",
                serialized
                    .iter()
                    .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b))
                        .wrapping_mul(0x100000001b3))
            );
            let offset = if let Some((offset, count, prior, complete)) = imported {
                if count != history.len() || fingerprint != prior {
                    return Err(HostError::new(
                        "history_conflict",
                        "首次历史导入未完成或内容不同",
                    ));
                }
                if complete {
                    return Ok(());
                }
                offset
            } else {
                self.conn.execute("INSERT INTO host_context_import(session,event_offset,count,fingerprint,complete) VALUES(?1,?2,?3,?4,0)",params![id,events.len(),history.len(),fingerprint]).map_err(HostError::storage)?;
                events.len()
            };
            let mut count = 0;
            for event in events.iter().skip(offset) {
                if let SessionEvent::Message(m) = event {
                    if !history.get(count).is_some_and(|h| same(m, h)) {
                        return Err(HostError::new(
                            "history_conflict",
                            "已提交的导入历史与请求不一致",
                        ));
                    }
                    count += 1;
                }
            }
            for m in &history[count..] {
                self.append(id, SessionEvent::Message(m.clone()))?;
            }
            self.conn
                .execute(
                    "UPDATE host_context_import SET complete=1 WHERE session=?1",
                    [id],
                )
                .map_err(HostError::storage)?;
            return Ok(());
        }
        let mut complete_history = Vec::new();
        if let Some((offset, count, _, true)) = imported {
            for e in events.iter().skip(offset).take(count) {
                if let SessionEvent::Message(m) = e {
                    complete_history.push(m.clone());
                }
            }
        }
        let mut ordered = Vec::new();
        for r in runs.into_iter().filter(|r| r.state == RunState::Completed) {
            let offset: usize = self
                .conn
                .query_row(
                    "SELECT input_event_offset FROM host_runs WHERE id=?1",
                    [&r.run_id],
                    |row| row.get(0),
                )
                .map_err(HostError::storage)?;
            ordered.push((offset, r));
        }
        ordered.sort_by_key(|(offset, _)| *offset);
        for (offset, r) in ordered {
            if let Some(SessionEvent::Message(m)) = events.get(offset) {
                if m.role == Role::User {
                    let view = self.get_run(&r.run_id)?;
                    if !view.text.trim().is_empty() {
                        complete_history.push(m.clone());
                        complete_history.push(Message::assistant_text(view.text));
                    }
                }
            }
        }
        if history.len() > complete_history.len()
            || !history
                .iter()
                .zip(
                    complete_history
                        .iter()
                        .skip(complete_history.len().saturating_sub(history.len())),
                )
                .all(|(a, b)| same(a, b))
        {
            return Err(HostError::new(
                "history_conflict",
                "浏览器完整历史不是已完成会话的后缀",
            ));
        }
        Ok(())
    }
    /// 功能：将原Run/call配对映射为稳定短展示ID。
    /// 参数：run/call为原引擎真实标识。
    /// 返回：数据库分配的展示ID，不替代原工具ID。
    /// 错误：SQL写入或读取失败。
    /// 状态：同一配对复用原ID；不依赖调用排序或文本截断。
    /// 并发与取消：Store单写序列，唯一约束防止重复。
    /// 权限：仅展示映射，不携带执行授权。
    /// 关联：需求/设计REQ-OP-012；DES-OP-007。
    pub(crate) fn office_call_id(&self, run: &str, call: &str) -> HostResult<String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO host_office_calls(run,call) VALUES(?1,?2)",
                params![run, call],
            )
            .map_err(HostError::storage)?;
        let id: u64 = self
            .conn
            .query_row(
                "SELECT id FROM host_office_calls WHERE run=?1 AND call=?2",
                params![run, call],
                |r| r.get(0),
            )
            .map_err(HostError::storage)?;
        Ok(format!("tool:{id}"))
    }
    /// 功能：读取已保存的完整Office投影及投递状态。
    /// 参数：team为受信办公室ID。
    /// 返回：可选的规范化完整快照与状态。
    /// 错误：SQL或存储内容损坏。
    /// 状态：只读；重试不重生成时间或版本。
    /// 并发与取消：在Store单写线程执行。
    /// 权限：仅内部适配器，不对HTTP暴露数据库。
    /// 关联：需求/设计REQ-OP-011/012；DES-OP-009。
    pub(crate) fn office_projection(
        &self,
        team: &str,
    ) -> HostResult<Option<(serde_json::Value, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT data,sync_state FROM host_office_projection WHERE team=?1",
                [team],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(HostError::storage)?;
        row.map(|(v, state)| Ok((serde_json::from_str(&v).map_err(HostError::storage)?, state)))
            .transpose()
    }
    /// 功能：在事务中持久保存完整投影版本与投递状态。
    /// 参数：team为办公室，snapshot为完整规范内容，state为待发送或实际回执。
    /// 返回：可靠保存成功。
    /// 错误：事务或序列化失败，不得外发未保存快照。
    /// 状态：不向host_events追加刷新事件，避免投影自身触发无限版本循环。
    /// 并发与取消：Store单写线程；保存发生在HTTP之前。
    /// 权限：内部调用，不复制消息账本。
    /// 关联：需求/设计REQ-OP-011；DES-OP-009。
    pub(crate) fn save_office_projection(
        &mut self,
        team: &str,
        snapshot: &serde_json::Value,
        state: &str,
    ) -> HostResult<()> {
        let tx = self.conn.transaction().map_err(HostError::storage)?;
        tx.execute("INSERT INTO host_office_projection(team,data,sync_state) VALUES(?1,?2,?3) ON CONFLICT(team) DO UPDATE SET data=excluded.data,sync_state=excluded.sync_state",params![team,serde_json::to_string(snapshot).map_err(HostError::storage)?,state]).map_err(HostError::storage)?;
        tx.commit().map_err(HostError::storage)
    }
    /// 功能：按会话及客户端幂等键查找原始接受记录。
    /// 参数：session为已授权会话，key为客户端轮次键。
    /// 返回：可选的规范请求及Run ID。
    /// 错误：SQL或元数据损坏。
    /// 状态：只读，不创建会话或Run。
    /// 并发与取消：Store单写序列读取。
    /// 权限：调用方必须校验会话归属。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-007。 需求/设计REQ-OP-014。
    pub(crate) fn request_for_key(
        &self,
        session: &str,
        key: &str,
    ) -> HostResult<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT request,run FROM host_requests WHERE session=?1 AND key=?2",
                params![session, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(HostError::storage)
    }
    /// 功能：取得Run的客户端键；升级前Run使用稳定身份。
    /// 参数：run为已授权Run ID。
    /// 返回：客户端键或legacy前缀的Run ID。
    /// 错误：SQL读取失败。
    /// 状态：只读，不回放运行。
    /// 并发与取消：Store单写序列读取。
    /// 权限：调用方必须校验范围。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
    pub(crate) fn request_key(&self, run: &str) -> HostResult<String> {
        Ok(self
            .conn
            .query_row("SELECT key FROM host_requests WHERE run=?1", [run], |r| {
                r.get(0)
            })
            .optional()
            .map_err(HostError::storage)?
            .unwrap_or_else(|| format!("legacy:{run}")))
    }
    /// 功能：按原JSONL输入位置读取Run用户输入，避免将工具结果误作新轮次。
    /// 参数：run为授权Run视图。
    /// 返回：输入事件位置及原始用户文本。
    /// 错误：位置损坏或SQL/账本读取失败；接受失败未保存输入时返回空文本。
    /// 状态：只读原账本，不复制历史。
    /// 并发与取消：Store单写序列读取。
    /// 权限：调用方校验会话归属。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
    pub(crate) fn run_input(&self, run: &RunView) -> HostResult<(usize, String)> {
        let offset: usize = self
            .conn
            .query_row(
                "SELECT input_event_offset FROM host_runs WHERE id=?1",
                [&run.run_id],
                |r| r.get(0),
            )
            .map_err(HostError::storage)?;
        let committed:bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_events WHERE kind='run_input_committed' AND reference=?1)",[&run.run_id],|r|r.get(0)).map_err(HostError::storage)?;
        if !committed {
            // 升级前真正开始的Run有原RunStarted事实；从未接受的失败请求不得借用后来追加的User。
            let started = self
                .events(&run.session_id)?
                .iter()
                .any(|e| matches!(e, SessionEvent::RunStarted { run_id } if run_id == &run.run_id));
            if !started {
                return Ok((offset, String::new()));
            }
        }
        // 正常接受的Run在host_requests中已有规范请求；直接取最后一条user消息，
        // 避免投影为每个Run重新扫描和解析整份JSONL。请求表只是幂等输入索引，
        // 账本仍是事实来源；缺少索引的旧/导入Run继续走下面的JSONL回退路径。
        if let Some(raw) = self
            .conn
            .query_row(
                "SELECT request FROM host_requests WHERE run=?1",
                [&run.run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(HostError::storage)?
        {
            let messages: serde_json::Value =
                serde_json::from_str(&raw).map_err(HostError::storage)?;
            if let Some(input) = messages.as_array().and_then(|items| {
                items.iter().rev().find_map(|item| {
                    (item["role"] == "user")
                        .then(|| item["content"].as_str().map(str::to_owned))
                        .flatten()
                })
            }) {
                return Ok((offset, input));
            }
        }
        let events = self.events(&run.session_id)?;
        let text = match events.get(offset) {
            Some(SessionEvent::Message(m)) if m.role == openpaw_core::Role::User => m.text(),
            _ if matches!(
                run.state,
                RunState::Accepting | RunState::Failed | RunState::Interrupted
            ) =>
            {
                String::new()
            }
            _ => {
                return Err(HostError::new(
                    "storage_unavailable",
                    "Run输入与原账本不一致",
                ))
            }
        };
        Ok((offset, text))
    }
    /// 功能：只读投影首次导入的完整文本轮次。
    /// 参数：session为已授权原Session。
    /// 返回：原事件位置、用户及助手文本；未完成导入不展示为成功。
    /// 错误：导入位置/角色损坏或账本读取失败。
    /// 状态：复用既有导入标记和JSONL，不保存第二份历史。
    /// 并发与取消：Store单写序列读取。
    /// 权限：外部导入仍是不可信文本，不产生工具或授权事实。
    /// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
    pub(crate) fn imported_turns(&self, session: &str) -> HostResult<Vec<(usize, String, String)>> {
        let marker:Option<(usize,usize)>=self.conn.query_row("SELECT event_offset,count FROM host_context_import WHERE session=?1 AND complete=1",[session],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(HostError::storage)?;
        let Some((offset, count)) = marker else {
            return Ok(Vec::new());
        };
        let events = self.events(session)?;
        let Some(messages) = events.get(offset..offset + count) else {
            return Err(HostError::new("storage_unavailable", "导入历史位置损坏"));
        };
        let mut turns = Vec::new();
        for (index, pair) in messages.chunks(2).enumerate() {
            match pair {
                [SessionEvent::Message(u), SessionEvent::Message(a)]
                    if u.role == openpaw_core::Role::User
                        && a.role == openpaw_core::Role::Assistant =>
                {
                    turns.push((offset + index * 2, u.text(), a.text()))
                }
                _ => {
                    return Err(HostError::new(
                        "storage_unavailable",
                        "导入历史轮次结构损坏",
                    ))
                }
            }
        }
        Ok(turns)
    }
    fn recover(&mut self) -> HostResult<()> {
        // 删除意图必须先于恢复writer；否则残留绑定会让已经删除的会话复活。
        let pending: Vec<(String, String)> = {
            let mut q = self
                .conn
                .prepare("SELECT scope,session FROM host_session_deletions WHERE phase!='deleted'")
                .map_err(HostError::storage)?;
            let rows = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(HostError::storage)?;
            rows.collect::<Result<_, _>>().map_err(HostError::storage)?
        };
        for (scope, session) in pending {
            self.purge_session(&scope, &session)?;
        }
        self.validate_agent_metadata()?;
        self.conn
            .execute(
                "UPDATE host_office_decisions SET state='expired' WHERE state='active'",
                [],
            )
            .map_err(HostError::storage)?;
        for b in self.bindings()? {
            let path = self.path(&b.session_id)?;
            let writer = SessionWriter::open_append(&path).map_err(HostError::storage)?;
            let loaded = SessionReader::read(&path).map_err(HostError::storage)?;
            if loaded.meta.id != b.session_id {
                return Err(HostError::new("storage_unavailable", "会话账本身份不匹配"));
            }
            self.index
                .delete_session(&b.session_id)
                .map_err(HostError::storage)?;
            self.index
                .upsert_session(&loaded.meta, &path)
                .map_err(HostError::storage)?;
            for (seq, event) in loaded.events.iter().enumerate() {
                self.index
                    .index_event(&b.session_id, seq, event)
                    .map_err(HostError::storage)?;
            }
            self.writers.insert(b.session_id, writer);
        }
        for mut run in self.runs()? {
            let offset: usize = self
                .conn
                .query_row(
                    "SELECT input_event_offset FROM host_runs WHERE id=?1",
                    [&run.run_id],
                    |r| r.get(0),
                )
                .map_err(HostError::storage)?;
            let events = self.events(&run.session_id)?;
            let has_input = matches!(events.get(offset),Some(SessionEvent::Message(m)) if m.role==openpaw_core::Role::User);
            if !has_input && run.state != RunState::Accepting && run.state != RunState::Failed {
                return Err(HostError::new(
                    "storage_unavailable",
                    "Run输入位置与原账本不一致，拒绝伪造恢复",
                ));
            }
            if offset > events.len() {
                return Err(HostError::new(
                    "storage_unavailable",
                    "Run输入位置超出原账本",
                ));
            }
            if !run.state.is_terminal() {
                let terminal =
                    self.events(&run.session_id)?
                        .into_iter()
                        .rev()
                        .find_map(|e| match e {
                            SessionEvent::RunFinished { run_id, status, .. }
                                if run_id == run.run_id =>
                            {
                                Some(status)
                            }
                            _ => None,
                        });
                run.state = match terminal.as_deref() {
                    Some("completed") => RunState::Completed,
                    Some("cancelled") => RunState::Cancelled,
                    Some("timed_out") => RunState::TimedOut,
                    Some("failed") => RunState::Failed,
                    _ => RunState::Interrupted,
                };
                if run.state == RunState::Interrupted {
                    run.error = Some(HostError::new(
                        "interrupted",
                        "宿主重启，未完成运行不自动重放",
                    ));
                }
                self.save_run(run, 0)?;
            }
        }
        for mut a in self.approvals()? {
            if a.status == "pending" || a.status == "approved" && !a.consumed {
                a.status = "expired".into();
                self.save_approval(a)?;
            }
        }
        Ok(())
    }
    /// 功能：等待存储检查点并释放home所有权。
    /// 参数：self为已等待所有Run清理的Db。
    /// 返回：完成关闭后的成功。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：清空Writer并释放OS守卫；调用方不能提前关闭活动运行。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    fn close(&mut self) -> HostResult<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(HostError::storage)?;
        Ok(())
    }
}

impl Db {
    /// 功能：拒绝已登记删除意图的完整范围。
    /// 参数：scope为可信会话范围。
    /// 返回：未删除时成功。
    /// 错误：删除中或已删除返回session_deleted；存储错误透传。
    /// 状态：只读墓碑，禁止旧ID重新创建。
    /// 并发与取消：单写序列，与接受和删除原子排序。
    /// 权限：不将ID当认证凭据。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.4节。 需求/设计REQ-OP-014。
    pub(crate) fn check_live_scope(&self, scope: &ClientScope) -> HostResult<()> {
        if self.deletion(scope)?.is_some() {
            return Err(HostError::new(
                "session_deleted",
                "会话已删除或正在删除，请新建会话",
            ));
        }
        Ok(())
    }
    /// 功能：读取最小删除墓碑。
    /// 参数：scope为完整可信范围。
    /// 返回：可选中性删除视图。
    /// 错误：SQL或序列化错误。
    /// 状态：不包含正文、输入或输出。
    /// 并发与取消：单写线程只读。
    /// 权限：精确范围查找。
    /// 关联：需求/设计REQ-OP-014；DAT-HOST-INTERACTION。 需求/设计REQ-OP-014。
    pub(crate) fn deletion(&self, scope: &ClientScope) -> HostResult<Option<DeleteView>> {
        let key = serde_json::to_string(scope).map_err(HostError::storage)?;
        self.conn
            .query_row(
                "SELECT session,phase FROM host_session_deletions WHERE scope=?1",
                [key],
                |r| {
                    Ok(DeleteView {
                        session_id: r.get(0)?,
                        phase: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(HostError::storage)
    }
    /// 功能：持久化删除意图并关闭目标会话的审批。
    /// 参数：scope为可信完整范围。
    /// 返回：既有或新建的删除视图。
    /// 错误：不存在、权限或SQL错误。
    /// 状态：先保存墓碑再取消；重复调用不新建会话。
    /// 并发与取消：单写排序；不等待网络或运行清理。
    /// 权限：仅目标范围，不修改工作区文件。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.4节。 需求/设计REQ-OP-014。
    pub(crate) fn begin_delete(&mut self, scope: &ClientScope) -> HostResult<DeleteView> {
        if let Some(v) = self.deletion(scope)? {
            return Ok(v);
        }
        let binding = self
            .bindings()?
            .into_iter()
            .find(|b| b.scope == *scope)
            .ok_or_else(|| HostError::new("not_found", "会话不存在"))?;
        let key = serde_json::to_string(scope).map_err(HostError::storage)?;
        self.conn
            .execute(
                "INSERT INTO host_session_deletions VALUES(?1,?2,'deleting',?3)",
                params![
                    key,
                    binding.session_id,
                    chrono::Utc::now().timestamp_millis()
                ],
            )
            .map_err(HostError::storage)?;
        for child in self.owned_child_sessions(&binding.session_id)? {
            self.begin_delete(&child.scope)?;
        }
        for mut a in self.approvals()? {
            if self
                .runs()?
                .iter()
                .any(|r| r.session_id == binding.session_id && r.run_id == a.binding.run_id)
                && (a.status == "pending" || a.status == "approved" && !a.consumed)
            {
                a.status = "cancelled".into();
                self.save_approval(a)?;
            }
        }
        self.conn
            .execute(
                "UPDATE host_office_decisions SET state='cancelled' WHERE scope=?1",
                [&key],
            )
            .map_err(HostError::storage)?;
        self.conn
            .execute(
                "UPDATE host_session_deletions SET phase='cancelling' WHERE scope=?1",
                [key],
            )
            .map_err(HostError::storage)?;
        Ok(DeleteView {
            session_id: binding.session_id,
            phase: "cancelling".into(),
        })
    }
    /// 功能：实际清理结束后前进删除阶段。
    /// 参数：scope和session为墓碑身份。
    /// 返回：删除完成。
    /// 错误：任何物理删除或索引清理失败保留意图并报错。
    /// 状态：先移除账本和派生正文，最后保留最小墓碑；不回滚副作用。
    /// 并发与取消：只在目标Run完成清理后调用；恢复时先于writer打开。
    /// 权限：只删除本会话及自动损尾备份，保留项目和用户备份。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.4节。 需求/设计REQ-OP-014。
    pub(crate) fn purge_session(&mut self, scope: &str, session: &str) -> HostResult<()> {
        let phase: String = self
            .conn
            .query_row(
                "SELECT phase FROM host_session_deletions WHERE scope=?1 AND session=?2",
                params![scope, session],
                |r| r.get(0),
            )
            .map_err(HostError::storage)?;
        if phase == "deleted" {
            return Ok(());
        }
        self.conn.execute("UPDATE host_session_deletions SET phase='purging' WHERE scope=?1 AND phase!='deleted'",[scope]).map_err(HostError::storage)?;
        for child in self.owned_child_sessions(session)? {
            let deletion = self.begin_delete(&child.scope)?;
            if deletion.phase != "deleted" {
                let key = serde_json::to_string(&child.scope).map_err(HostError::storage)?;
                self.purge_session(&key, &child.session_id)?;
            }
        }
        self.writers.remove(session);
        let path = self.path(session)?;
        let remove = |path: &std::path::Path| match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(HostError::storage(e)),
        };
        remove(&path)?;
        let prefix = format!("{session}.jsonl.torn-");
        if let Some(parent) = path.parent() {
            match std::fs::read_dir(parent) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(HostError::storage)?;
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if let Some(suffix) = name
                            .strip_prefix(&prefix)
                            .and_then(|s| s.strip_suffix(".bak"))
                        {
                            if suffix.split_once('-').is_some_and(|(a, b)| {
                                !a.is_empty()
                                    && !b.is_empty()
                                    && a.bytes().all(|c| c.is_ascii_digit())
                                    && b.bytes().all(|c| c.is_ascii_digit())
                            }) {
                                remove(&entry.path())?;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(HostError::storage(e)),
            }
        }
        self.index
            .delete_session(session)
            .map_err(HostError::storage)?;
        let run_ids: Vec<_> = self
            .runs()?
            .into_iter()
            .filter(|r| r.session_id == session)
            .map(|r| r.run_id)
            .collect();
        let tx = self.conn.transaction().map_err(HostError::storage)?;
        for id in &run_ids {
            tx.execute("DELETE FROM host_events WHERE kind='approval' AND reference IN (SELECT id FROM host_approvals WHERE run=?1)",[id]).map_err(HostError::storage)?;
            tx.execute("DELETE FROM host_office_decisions WHERE approval_id IN (SELECT id FROM host_approvals WHERE run=?1)",[id]).map_err(HostError::storage)?;
            tx.execute("DELETE FROM host_approvals WHERE run=?1", [id])
                .map_err(HostError::storage)?;
            tx.execute("DELETE FROM host_office_calls WHERE run=?1", [id])
                .map_err(HostError::storage)?;
            tx.execute(
                "DELETE FROM host_office_scene WHERE event_id LIKE ?1",
                [format!("{id}:%")],
            )
            .map_err(HostError::storage)?;
            tx.execute("DELETE FROM host_events WHERE reference=?1 OR CASE WHEN json_valid(reference) THEN json_extract(reference,'$.run_id')=?1 ELSE 0 END",[id]).map_err(HostError::storage)?;
        }
        tx.execute(
            "DELETE FROM host_events WHERE reference=?1 OR reference LIKE ?2",
            params![session, format!("{session}:%")],
        )
        .map_err(HostError::storage)?;
        for sql in [
            "DELETE FROM host_runs WHERE session=?1",
            "DELETE FROM host_requests WHERE session=?1",
            "DELETE FROM host_context_import WHERE session=?1",
        ] {
            tx.execute(sql, [session]).map_err(HostError::storage)?;
        }
        tx.execute("DELETE FROM host_office_actions WHERE scope=?1", [scope])
            .map_err(HostError::storage)?;
        // 清除缓存正文但保留seq；下一次prepare分配更高完整版本，不重置协调器。
        let target: ClientScope = serde_json::from_str(scope).map_err(HostError::storage)?;
        if target.namespace == "office" {
            let saved: Option<String> = tx
                .query_row(
                    "SELECT data FROM host_office_projection WHERE team=?1",
                    [&target.slot_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(HostError::storage)?;
            if let Some(saved) = saved {
                let mut snapshot: serde_json::Value =
                    serde_json::from_str(&saved).map_err(HostError::storage)?;
                if let Some(convs) = snapshot["conversations"].as_array_mut() {
                    convs.retain(|c| c["id"] != target.conversation_id);
                }
                let seq = snapshot["seq"]
                    .as_u64()
                    .filter(|n| *n < 9007199254740991)
                    .ok_or_else(|| HostError::new("sequence_conflict", "快照序号耗尽"))?;
                snapshot["seq"] = serde_json::json!(seq + 1);
                tx.execute(
                    "UPDATE host_office_projection SET data=?2,sync_state='pending' WHERE team=?1",
                    params![
                        target.slot_id,
                        serde_json::to_string(&snapshot).map_err(HostError::storage)?
                    ],
                )
                .map_err(HostError::storage)?;
            }
        }
        tx.execute("DELETE FROM host_bindings WHERE id=?1", [session])
            .map_err(HostError::storage)?;
        tx.execute(
            "UPDATE host_session_deletions SET phase='deleted' WHERE scope=?1",
            [scope],
        )
        .map_err(HostError::storage)?;
        tx.commit().map_err(HostError::storage)?;
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        for id in run_ids {
            live.remove(&id);
        }
        drop(live);
        let revision = self.bump("deleted", session)?;
        self.publish(revision);
        Ok(())
    }
    /// 功能：按实际开始时最近的业务计划关联调用与步骤，避免历史动画随进度移动。
    /// 参数：run为当前运行ID。
    /// 返回：调用ID到原计划步骤下标的映射；没有计划的调用由投影归主目标。
    /// 错误：SQL或已存计划/执行绑定损坏。
    /// 状态：只读重建；无开始步骤的委派可由后续已校验完成证据补关联，已有绑定不移动。
    /// 并发与取消：在原Store序列内按revision读取，无网络或资源锁。
    /// 权限：只使用同Run的已存计划和实际tool_started事实。
    /// 关联：需求/设计REQ-OP-012；CHG-2026-0006第9.9节。
    pub(crate) fn office_task_assignments(
        &self,
        run: &str,
    ) -> HostResult<std::collections::BTreeMap<String, usize>> {
        let mut stmt = self.conn.prepare("SELECT kind,reference FROM host_events WHERE kind IN ('run_progress','tool_started') AND json_extract(reference,'$.run_id')=?1 ORDER BY revision").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([run], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(HostError::storage)?;
        let mut current = None;
        let mut assignments = std::collections::BTreeMap::new();
        let mut unassigned_delegations = std::collections::BTreeSet::new();
        for row in rows {
            let (kind, raw) = row.map_err(HostError::storage)?;
            let v: serde_json::Value = serde_json::from_str(&raw).map_err(HostError::storage)?;
            if kind == "run_progress" {
                current = v["steps"]
                    .as_array()
                    .and_then(|steps| steps.iter().position(|s| s["state"] == "in_progress"));
                // 模型可能漏报开始；仅用原校验通过的准确证据补未关联委派，不按标题猜。
                // 普通工具及已有开始绑定保持原位，避免进度更新重播历史动画。
                for (index, step) in v["steps"].as_array().into_iter().flatten().enumerate() {
                    for call in step["evidenceCallIds"].as_array().into_iter().flatten() {
                        if let Some(call) = call.as_str() {
                            if unassigned_delegations.remove(call) {
                                assignments.entry(call.to_owned()).or_insert(index);
                            }
                        }
                    }
                }
            } else if let Some(call) = v["call_id"].as_str() {
                if let Some(index) = current {
                    assignments.insert(call.to_owned(), index);
                } else if v["tool_name"] == "delegate_task" {
                    unassigned_delegations.insert(call.to_owned());
                }
            }
        }
        Ok(assignments)
    }
    /// 功能：读取永久办公室分配事实，不使用有期限的UI导航记录。
    /// 参数：无。
    /// 返回：原host_events中按提交顺序保存的创建归属。
    /// 错误：SQL或JSON损坏，不能当成尚无办公室。
    /// 状态：只读；删除会话不删除办公室归属。
    /// 并发与取消：复用唯一Store线程，不持文件锁。
    /// 权限：仅返回控制标识与显示名称，无密钥或消息正文。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.10节。
    pub(crate) fn office_creations(&self) -> HostResult<Vec<serde_json::Value>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT reference FROM host_events WHERE kind='office_created' ORDER BY revision",
            )
            .map_err(HostError::storage)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(HostError::storage)?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(HostError::storage)?).map_err(HostError::storage)
        })
        .collect()
    }
    /// 功能：在真实Session创建后可靠登记一次办公室分配。
    /// 参数：action/slot/name/session为已审核创建动作及固定绑定。
    /// 返回：首次提交成功；同动作同内容重试保持不变。
    /// 错误：动作或槽冲突、绑定不匹配、SQL失败。
    /// 状态：沿用host_events和数据版本，不创建第二套业务数据库。
    /// 并发与取消：Store串行；外层创建门及投影门覆盖分配到UI响应。
    /// 权限：再次验证Office命名空间和main会话绑定，根仍由Host配置决定。
    /// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.10节。
    pub(crate) fn save_office_creation(
        &self,
        action: &str,
        slot: &str,
        name: &str,
        session: &str,
    ) -> HostResult<()> {
        let record =
            serde_json::json!({"action_id":action,"slot_id":slot,"name":name,"session_id":session});
        for old in self.office_creations()? {
            if old["action_id"] == action {
                return if old == record {
                    Ok(())
                } else {
                    Err(HostError::new(
                        "scope_conflict",
                        "相同创建动作的内容发生冲突",
                    ))
                };
            }
            if old["slot_id"] == slot {
                return Err(HostError::new("scope_conflict", "办公室槽已经分配"));
            }
        }
        let binding = self.binding(session)?;
        if binding.scope.namespace != "office"
            || binding.scope.slot_id != slot
            || binding.scope.conversation_id != "main"
        {
            return Err(HostError::new("scope_denied", "办公室创建缺少真实固定会话"));
        }
        let revision = self.bump("office_created", &record.to_string())?;
        self.publish(revision);
        Ok(())
    }
    /// 功能：登记已领取UI动作或有限导航上下文。
    /// 参数：id/scope/kind/state/expiry/context仅含控制标识。
    /// 返回：首次动作true，重复ID不覆盖原范围。
    /// 错误：SQL或序列化错误。
    /// 状态：清除过期元数据，每会话最多64个导航。
    /// 并发与取消：单写序列提交后才分派。
    /// 权限：上下文不保存消息正文或凭据。
    /// 关联：需求/设计REQ-OP-014；DAT-HOST-INTERACTION。 需求/设计REQ-OP-014。
    pub(crate) fn office_action(
        &self,
        id: &str,
        scope: &ClientScope,
        kind: &str,
        state: &str,
        expiry: i64,
        context: &serde_json::Value,
    ) -> HostResult<bool> {
        self.conn
            .execute(
                "DELETE FROM host_office_actions WHERE expires_at<?1",
                [chrono::Utc::now().timestamp_millis()],
            )
            .map_err(HostError::storage)?;
        let key = serde_json::to_string(scope).map_err(HostError::storage)?;
        if kind == "navigation" {
            self.conn.execute("UPDATE host_office_actions SET kind='navigation',status=?3,expires_at=?4,context=?5 WHERE action_id=?1 AND scope=?2",params![id,key,state,expiry,serde_json::to_string(context).map_err(HostError::storage)?]).map_err(HostError::storage)?;
        }
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO host_office_actions VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    key,
                    kind,
                    state,
                    expiry,
                    serde_json::to_string(context).map_err(HostError::storage)?
                ],
            )
            .map_err(HostError::storage)?;
        self.conn.execute("DELETE FROM host_office_actions WHERE scope=?1 AND kind='navigation' AND action_id NOT IN (SELECT action_id FROM host_office_actions WHERE scope=?1 AND kind='navigation' ORDER BY expires_at DESC LIMIT 64)",[key]).map_err(HostError::storage)?;
        Ok(n == 1)
    }
    /// 功能：读取同范围未过期的导航标识。
    /// 参数：id为原对话框ID，scope为当前动作范围。
    /// 返回：仅控制上下文。
    /// 错误：过期、越界或SQL错误。
    /// 状态：不重置期限。
    /// 并发与取消：只读单写队列。
    /// 权限：跨会话不可复用按钮。
    /// 关联：需求/设计REQ-OP-014；API-OFFICE-UI。 需求/设计REQ-OP-014。
    pub(crate) fn office_navigation(
        &self,
        id: &str,
        scope: &ClientScope,
    ) -> HostResult<serde_json::Value> {
        self.check_live_scope(scope)?;
        let key = serde_json::to_string(scope).map_err(HostError::storage)?;
        let value:Option<String>=self.conn.query_row("SELECT context FROM host_office_actions WHERE action_id=?1 AND scope=?2 AND kind='navigation' AND expires_at>?3",params![id,key,chrono::Utc::now().timestamp_millis()],|r|r.get(0)).optional().map_err(HostError::storage)?;
        serde_json::from_str(
            &value.ok_or_else(|| HostError::new("ui_expired", "详情按钮已过期，请重新打开"))?,
        )
        .map_err(HostError::storage)
    }
    /// 功能：保存审批展示尝试，保持共享审批为唯一授权。
    /// 参数：approval/display/scope/expiry/state为已核对控制字段。
    /// 返回：提交完成。
    /// 错误：SQL错误。
    /// 状态：新尝试覆盖旧展示ID，期限不延长。
    /// 并发与取消：单写序列。
    /// 权限：不保存另一份批准或输入正文。
    /// 关联：需求/设计REQ-OP-013；DAT-HOST-INTERACTION。 需求/设计REQ-OP-014。
    pub(crate) fn office_decision(
        &self,
        approval: &str,
        display: &str,
        scope: &ClientScope,
        expiry: i64,
        state: &str,
    ) -> HostResult<()> {
        // 显示任务可能刚从HTTP返回，此时删除已经提交；不能重建已清除的展示记录。
        self.check_live_scope(scope)?;
        self.conn.execute("INSERT INTO host_office_decisions VALUES(?1,?2,?3,?4,?5) ON CONFLICT(approval_id) DO UPDATE SET display_id=excluded.display_id,state=excluded.state",params![approval,display,serde_json::to_string(scope).map_err(HostError::storage)?,expiry,state]).map_err(HostError::storage)?;
        Ok(())
    }
}

impl Db {
    /// 功能：只根据可靠的真实开始观察事实判断是否允许场景交接。
    /// 参数：run/call为当前运行的精确工具标识。
    /// 返回：原工具开始事件存在时为真。
    /// 错误：SQL读取失败。
    /// 状态：拒绝、校验失败或审批过期均不能借终态伪造执行动画。
    /// 并发与取消：投影同次Store读取，不持工作区锁。
    /// 权限：仅适配器可信Run/call。
    /// 关联：需求/设计REQ-OP-015；DES-OP-007。
    pub(crate) fn office_call_started(&self, run: &str, call: &str) -> HostResult<bool> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.run_id')=?1 AND json_extract(reference,'$.call_id')=?2)",params![run,call],|r|r.get(0)).map_err(HostError::storage)
    }

    /// 功能：登记有限场景反馈并清除过期去重键。
    /// 参数：event/command为真实事实和唯一命令标识，expiry为原观察期限，state为投递状态。
    /// 返回：登记完成。
    /// 错误：SQL错误。
    /// 状态：不保存正文，不在重启时重投。
    /// 并发与取消：单写序列，无网络等待。
    /// 权限：仅Office适配器内部。
    /// 关联：需求/设计REQ-OP-015；DAT-HOST-INTERACTION。 需求/设计REQ-OP-014。
    pub(crate) fn office_scene(
        &self,
        event: &str,
        command: &str,
        expiry: i64,
        state: &str,
    ) -> HostResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn
            .execute("DELETE FROM host_office_scene WHERE expires_at<?1", [now])
            .map_err(HostError::storage)?;
        self.conn
            .execute(
                "INSERT INTO host_office_scene VALUES(?1,?2,?3,?4) ON CONFLICT(event_id) DO UPDATE SET status=excluded.status WHERE command_id=excluded.command_id",
                params![event, command, expiry, state],
            )
            .map_err(HostError::storage)?;
        Ok(())
    }
}

impl Db {
    /// 功能：记录UI响应的送达事实，不能回滚已经提交的业务删除。
    /// 参数：id/scope为领取对象，status为delivered或unconfirmed。
    /// 返回：状态提交完成。
    /// 错误：SQL错误。
    /// 状态：只存控制标识；导航期限不重置，过期记录按原规则清除。
    /// 并发与取消：单写队列，不等待网络。
    /// 权限：完整scope匹配，保留删除后的有限UI未确认诊断。
    /// 关联：需求/设计REQ-OP-014；DAT-HOST-INTERACTION。
    pub(crate) fn office_action_state(
        &self,
        id: &str,
        scope: &ClientScope,
        status: &str,
    ) -> HostResult<()> {
        self.conn
            .execute(
                "UPDATE host_office_actions SET status=?3 WHERE action_id=?1 AND scope=?2",
                params![
                    id,
                    serde_json::to_string(scope).map_err(HostError::storage)?,
                    status
                ],
            )
            .map_err(HostError::storage)?;
        // purge已移除含导航内容的原动作；仅补最小回执，不能因410让已删除会话复活。
        let key = serde_json::to_string(scope).map_err(HostError::storage)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO host_office_actions SELECT ?1,?2,'receipt',?3,?4,'{}' WHERE EXISTS(SELECT 1 FROM host_session_deletions WHERE scope=?2 AND phase='deleted')",
            params![id,key,if status=="delivered" {"deleted_ui_confirmed"} else {"deleted_ui_unconfirmed"},chrono::Utc::now().timestamp_millis()+600000],
        ).map_err(HostError::storage)?;
        Ok(())
    }
}

impl Db {
    /// 功能：从原控制事件读取并验证一个父Run的持久委派引用。
    /// 参数：parent为真实父Run ID。
    /// 返回：按派发顺序的版本1引用；正文仍从JSONL查询。
    /// 错误：未知版本、父子范围/根不一致或数据库错误。
    /// 状态：只读，不从意图恢复执行。
    /// 并发与取消：在原单写Store序列内读取。
    /// 权限：必须先由调用方验证父scope；子绑定逐字段核对。
    /// 关联：需求/设计REQ-OP-022/023；DES-OP-012。
    pub(crate) fn delegations(
        &self,
        parent: &str,
    ) -> HostResult<Vec<crate::orchestration::Delegation>> {
        let parent_session: String = self
            .conn
            .query_row(
                "SELECT session FROM host_runs WHERE id=?1",
                [parent],
                |row| row.get(0),
            )
            .optional()
            .map_err(HostError::storage)?
            .ok_or_else(|| HostError::new("not_found", "父运行不存在"))?;
        let owner = self.binding(&parent_session)?;
        let mut stmt = self.conn.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=?1 ORDER BY revision").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([parent], |r| r.get::<_, String>(0))
            .map_err(HostError::storage)?;
        let mut result = Vec::new();
        for row in rows {
            let d: crate::orchestration::Delegation =
                serde_json::from_str(&row.map_err(HostError::storage)?)
                    .map_err(HostError::storage)?;
            let child = self.binding(&d.child_session)?;
            if d.version != 1
                || d.run_id != parent
                || owner.session_id != d.parent_session
                || owner.scope != d.parent_scope
                || owner.scope.namespace != "office"
                || child.scope != d.child_scope
                || child.scope.namespace != "host-agent"
                || child.scope.slot_id != owner.scope.slot_id
                || child.root_id != owner.root_id
                || crate::agents::role(&d.role_id).is_none()
                || d.role_id == "lead"
            {
                return Err(HostError::new(
                    "storage_unavailable",
                    "持久委派版本或所属关系不合法",
                ));
            }
            result.push(d);
        }
        Ok(result)
    }
    /// 功能：可靠保存新的委派控制意图，通知现有投影订阅者。
    /// 参数：d为原Store序列内生成的可信父子引用。
    /// 返回：保存成功。
    /// 错误：数据库或序列化错误。
    /// 状态：先于子Run接受保存，不复制子消息或工具结果。
    /// 并发与取消：原Store串行，不另建任务数据库。
    /// 权限：仅共享编排入口可调用，客户端不能提交引用。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn save_delegation(&self, d: &crate::orchestration::Delegation) -> HostResult<()> {
        let rev = self.bump(
            "agent_delegation",
            &serde_json::to_string(d).map_err(HostError::storage)?,
        )?;
        self.publish(rev);
        Ok(())
    }
    /// 功能：将一个真实子Run解析回唯一且仍有效的Office父会话。
    /// 参数：child为实际子Run ID。
    /// 返回：可信所属引用，普通Run返回无。
    /// 错误：关联损坏、范围失效或存储错误。
    /// 状态：只读，不创建绑定或改变原审批ID。
    /// 并发与取消：原单写序列内校验，删除后不能再解析。
    /// 权限：核对完整父子namespace/team/conversation/root，不按字符串前缀授权。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn delegation_for_child(
        &self,
        child: &str,
    ) -> HostResult<Option<crate::orchestration::Delegation>> {
        let parent:Option<String> = self.conn.query_row("SELECT json_extract(reference,'$.run_id') FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.child_run')=?1",[child],|r|r.get(0)).optional().map_err(HostError::storage)?;
        match parent {
            None => Ok(None),
            Some(parent) => {
                let d = self
                    .delegations(&parent)?
                    .into_iter()
                    .find(|d| d.child_run == child)
                    .ok_or_else(|| HostError::new("scope_denied", "子运行不属于当前主任务"))?;
                self.check_live_scope(&d.parent_scope)?;
                let run = self.get_run(child)?;
                if run.session_id != d.child_session {
                    return Err(HostError::new("scope_denied", "子运行与所属会话不符"));
                }
                Ok(Some(d))
            }
        }
    }
}

impl Db {
    /// 功能：将准确待审Run解析为原范围或已验证后代的准确子范围。
    /// 参数：scope为请求方完整范围，run为审批真实Run ID。
    /// 返回：最后原check_scope应使用的准确范围。
    /// 错误：未知、跨父会话或篡改所属一律scope_denied。
    /// 状态：不修改审批、版本、期限或输入绑定。
    /// 并发与取消：原Store串行，父删除先检查墓碑。
    /// 权限：父授权仅覆盖其持久登记后代，不按team或runId猜授权。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn approval_scope(&self, scope: &ClientScope, run: &str) -> HostResult<ClientScope> {
        self.check_live_scope(scope)?;
        let current = self.get_run(run)?;
        let binding = self.binding(&current.session_id)?;
        if binding.scope == *scope {
            return Ok(scope.clone());
        }
        let d = self
            .delegation_for_child(run)?
            .filter(|d| d.parent_scope == *scope)
            .ok_or_else(|| HostError::new("scope_denied", "审批不属于此会话或其真实子任务"))?;
        crate::service::check_scope(&binding, &d.child_scope)?;
        Ok(d.child_scope)
    }
}

impl Db {
    /// 功能：在删除及恢复阶段解析父会话仍残留的子账本引用。
    /// 参数：session为已有删除墓碑的父会话或当前删除目标。
    /// 返回：尚有绑定的可信子会话；已经清除的子项不再重复删除。
    /// 错误：版本、父子范围和根不一致拒绝继续清除。
    /// 状态：只读控制引用；允许读取墓碑中的父身份但不授予查询权限。
    /// 并发与取消：仅原Store删除序列，活动工具清理由Host先等待。
    /// 权限：逐字段校验父子绑定，绝不按任意路径删除工作区。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    fn owned_child_sessions(&self, session: &str) -> HostResult<Vec<SessionBinding>> {
        let bindings = self.bindings()?;
        let Some(parent) = bindings.iter().find(|b| b.session_id == session) else {
            return Ok(Vec::new());
        };
        let mut stmt=self.conn.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.parent_session')=?1").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([session], |r| r.get::<_, String>(0))
            .map_err(HostError::storage)?;
        let mut children = Vec::new();
        for row in rows {
            let d: crate::orchestration::Delegation =
                serde_json::from_str(&row.map_err(HostError::storage)?)
                    .map_err(HostError::storage)?;
            if d.version != 1
                || parent.scope != d.parent_scope
                || d.child_scope.namespace != "host-agent"
                || d.child_scope.slot_id != parent.scope.slot_id
            {
                return Err(HostError::new(
                    "storage_unavailable",
                    "删除阶段的父子引用不合法",
                ));
            }
            if let Some(child) = bindings.iter().find(|b| b.session_id == d.child_session) {
                if child.scope != d.child_scope || child.root_id != parent.root_id {
                    return Err(HostError::new("scope_denied", "子会话范围或根不一致"));
                }
                children.push(child.clone());
            } else if self
                .deletion(&d.child_scope)?
                .is_none_or(|v| v.phase != "deleted")
            {
                return Err(HostError::new(
                    "storage_unavailable",
                    "子账本引用丢失，拒绝猜测删除",
                ));
            }
        }
        Ok(children)
    }
}

impl Db {
    /// 功能：启动时拒绝未知版本或所属损坏的协作控制引用。
    /// 参数：self为已先恢复删除墓碑的原数据库。
    /// 返回：全部协作引用可被当前版本准确解释。
    /// 错误：未知版本、丢失主组、伪造父子绑定或SQL故障。
    /// 状态：不重放意图，不恢复执行，不复制模型消息。
    /// 并发与取消：启动单写阶段执行，尚未接受客户端请求。
    /// 权限：以原绑定/Run逐字段验证，不以模型角色或ID前缀授权。
    /// 关联：需求/设计REQ-OP-009/022；DES-OP-012。
    fn validate_agent_metadata(&self) -> HostResult<()> {
        let mut query = self.conn.prepare("SELECT kind,reference FROM host_events WHERE kind IN ('agent_group','agent_delegation','agent_budget') ORDER BY revision").map_err(HostError::storage)?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(HostError::storage)?;
        let mut groups = std::collections::BTreeSet::new();
        let mut delegated = std::collections::BTreeSet::new();
        for row in rows {
            let (kind, raw) = row.map_err(HostError::storage)?;
            let value: serde_json::Value =
                serde_json::from_str(&raw).map_err(HostError::storage)?;
            let parent = value["run_id"]
                .as_str()
                .ok_or_else(|| HostError::new("storage_unavailable", "协作元数据缺少父Run"))?;
            if value["version"] != 1 {
                return Err(HostError::new(
                    "storage_unavailable",
                    "不支持的协作元数据版本",
                ));
            }
            let run = self.get_run(parent)?;
            let owner = self.binding(&run.session_id)?;
            if owner.scope.namespace != "office" {
                return Err(HostError::new(
                    "storage_unavailable",
                    "协作主Run不属于Office绑定",
                ));
            }
            if kind == "agent_group" {
                let scope: ClientScope =
                    serde_json::from_value(value["scope"].clone()).map_err(HostError::storage)?;
                let config: crate::AgentsConfig =
                    serde_json::from_value(value["config"].clone()).map_err(HostError::storage)?;
                if scope != owner.scope || !config.enabled || !groups.insert(parent.to_owned()) {
                    return Err(HostError::new(
                        "storage_unavailable",
                        "协作主组绑定损坏或重复",
                    ));
                }
            } else {
                if !groups.contains(parent) {
                    return Err(HostError::new(
                        "storage_unavailable",
                        "协作引用缺少先前可靠主组",
                    ));
                }
                if kind == "agent_delegation" {
                    delegated.insert(parent.to_owned());
                }
            }
        }
        for parent in delegated {
            let mut calls = std::collections::BTreeSet::new();
            let mut sessions = std::collections::BTreeSet::new();
            let mut runs = std::collections::BTreeSet::new();
            for d in self.delegations(&parent)? {
                if !calls.insert(d.call_id)
                    || !sessions.insert(d.child_session.clone())
                    || !runs.insert(d.child_run.clone())
                {
                    return Err(HostError::new("storage_unavailable", "协作子引用重复"));
                }
                match self.get_run(&d.child_run) {
                    Ok(run) if run.session_id == d.child_session => {}
                    Err(e) if e.kind == "not_found" => {} // 已保存意图但未接受，不自动重放。
                    _ => {
                        return Err(HostError::new(
                            "storage_unavailable",
                            "协作子Run与会话不匹配",
                        ))
                    }
                }
            }
        }
        Ok(())
    }
    /// 功能：校验重试委派与原持久最终输入完全一致。
    /// 参数：binding为当前可信工具上下文。
    /// 返回：原绑定相同才允许读取原子任务结果。
    /// 错误：缺失绑定、损坏记录或输入冲突。
    /// 状态：只读，不创建第二个子Run。
    /// 并发与取消：在原Store单写序列执行，不等待模型。
    /// 权限：逐字段比较Run、call、工具及完整输入，不以摘要代替。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn check_delegation_input(
        &self,
        binding: &openpaw_core::ToolCallBinding,
    ) -> HostResult<()> {
        let original: String = self.conn.query_row("SELECT reference FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.run_id')=?1 AND json_extract(reference,'$.call_id')=?2 ORDER BY revision LIMIT 1", params![&binding.run_id,&binding.call_id], |r|r.get(0)).map_err(HostError::storage)?;
        let original: openpaw_core::ToolCallBinding =
            serde_json::from_str(&original).map_err(HostError::storage)?;
        if original != *binding {
            return Err(HostError::new(
                "request_conflict",
                "同一委派调用不能替换原最终输入",
            ));
        }
        Ok(())
    }
    /// 功能：从父委派的真实成功子Run提取有范围的文件写入证据。
    /// 参数：parent为已校验Office主Run ID。
    /// 返回：准确父call到其已成功子写路径，不返回正文或伪造父工具记录。
    /// 错误：损坏所属、子Run查询和存储错误。
    /// 状态：只读原Run及原tool_started，失败/未接受子不提供成功证据。
    /// 并发与取消：原Store单写序列读取，与进度提交一致。
    /// 权限：先验证父子完整绑定；只有同子Run成功且实际开始的写工具可用。
    /// 关联：需求/设计REQ-OP-022/023；DES-OP-012。
    fn delegated_writes(
        &self,
        parent: &str,
    ) -> HostResult<Vec<crate::task_progress::DelegatedWrite>> {
        let mut result = Vec::new();
        for d in self.delegations(parent)? {
            let child = match self.get_run(&d.child_run) {
                Ok(r) => r,
                Err(e) if e.kind == "not_found" => continue,
                Err(e) => return Err(e),
            };
            if child.state != RunState::Completed {
                continue;
            }
            let mut query=self.conn.prepare("SELECT reference FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.run_id')=?1").map_err(HostError::storage)?;
            let rows = query
                .query_map([&d.child_run], |r| r.get::<_, String>(0))
                .map_err(HostError::storage)?;
            let mut paths = Vec::new();
            for row in rows {
                let b: openpaw_core::ToolCallBinding =
                    serde_json::from_str(&row.map_err(HostError::storage)?)
                        .map_err(HostError::storage)?;
                if b.run_id == d.child_run
                    && matches!(b.tool_name.as_str(), "write_file" | "edit_file")
                    && child.tools.iter().any(|t| {
                        t.call_id == b.call_id
                            && t.tool_name == b.tool_name
                            && t.state == "completed"
                    })
                {
                    if let Some(path) = b
                        .input
                        .get("path")
                        .or_else(|| b.input.get("file_path"))
                        .and_then(serde_json::Value::as_str)
                    {
                        paths.push(path.to_owned());
                    }
                }
            }
            result.push(crate::task_progress::DelegatedWrite {
                call_id: d.call_id,
                paths,
            });
        }
        Ok(result)
    }
}

// 与实时RunText使用相同消息分隔规则；这里只投影原事实，不解析第二种消息格式。
fn committed_run_text(events: &[SessionEvent], id: &str) -> RunText {
    let mut active = false;
    let mut text = RunText::default();
    for event in events {
        match event {
            SessionEvent::RunStarted { run_id } => active = run_id == id,
            SessionEvent::Message(message)
                if active && message.role == openpaw_core::Role::Assistant =>
            {
                for block in &message.content {
                    if let openpaw_core::ContentBlock::Text { text: chunk } = block {
                        text.append(chunk);
                    }
                }
                text.end_message();
            }
            _ => {}
        }
    }
    text
}
fn merge_run_display(run: &mut RunView, candidate: &str, truncated: bool) -> HostResult<()> {
    if run.text.starts_with(candidate) {
        if run.text.len() == candidate.len() {
            run.text_truncated |= truncated;
        }
    } else if candidate.starts_with(&run.text) {
        run.text = candidate.to_owned();
        run.text_truncated = truncated;
    } else {
        return Err(HostError::new(
            "storage_unavailable",
            "展示检查点与可靠账本前缀不一致",
        ));
    }
    Ok(())
}

fn read_json_rows<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
) -> HostResult<Vec<T>> {
    let mut statement = conn.prepare(sql).map_err(HostError::storage)?;
    let result = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(HostError::storage)?
        .map(|row| {
            serde_json::from_str(&row.map_err(HostError::storage)?).map_err(HostError::storage)
        })
        .collect();
    result
}

fn rebind_events(conn: &Connection) -> HostResult<Vec<serde_json::Value>> {
    let events: Vec<serde_json::Value> = read_json_rows(
        conn,
        "SELECT reference FROM host_events WHERE kind='workspace_rebind' ORDER BY revision",
    )?;
    for event in &events {
        crate::workspace_migration::validate_migration_event(event)?;
        if event["version"] != 1
            || event["team"].as_str().is_none()
            || event["old_root"].as_str().is_none()
            || event["new_root"].as_str().is_none()
            || !event["old_path"]
                .as_str()
                .is_some_and(|s| std::path::Path::new(s).is_absolute())
            || !event["new_path"]
                .as_str()
                .is_some_and(|s| std::path::Path::new(s).is_absolute())
            || event["sessions"].as_array().is_none()
            || event["files"].as_array().is_none()
            || event["manifest_sha256"].as_str().is_none()
            || event["confirmation_sha256"].as_str().is_none()
        {
            return Err(HostError::new(
                "storage_unavailable",
                "工作区迁移审计版本或字段损坏",
            ));
        }
    }
    Ok(events)
}

fn migration_inventory(conn: &Connection) -> HostResult<serde_json::Value> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(HostError::storage)?;
    if version != 4 {
        return Err(HostError::new(
            "unsupported",
            "迁移仅接受完整的Host版本4元数据",
        ));
    }
    let bindings: Vec<SessionBinding> =
        read_json_rows(conn, "SELECT data FROM host_bindings ORDER BY id")?;
    let mismatches: u64 = conn.query_row("SELECT COUNT(*) FROM host_bindings WHERE id IS NOT json_extract(data,'$.session_id') OR namespace IS NOT json_extract(data,'$.scope.namespace') OR slot IS NOT json_extract(data,'$.scope.slot_id') OR conversation IS NOT json_extract(data,'$.scope.conversation_id')",[],|row|row.get(0)).map_err(HostError::storage)?;
    if mismatches != 0 {
        return Err(HostError::new(
            "storage_unavailable",
            "迁移前绑定主键与完整范围不一致",
        ));
    }

    let runs: Vec<RunView> = read_json_rows(conn, "SELECT data FROM host_runs ORDER BY id")?;
    let approvals: Vec<ApprovalView> =
        read_json_rows(conn, "SELECT data FROM host_approvals ORDER BY id")?;
    let revision: u64 = conn
        .query_row(
            "SELECT COALESCE(MAX(revision),0) FROM host_events",
            [],
            |r| r.get(0),
        )
        .map_err(HostError::storage)?;
    let deleting: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM host_session_deletions WHERE phase != 'deleted'",
            [],
            |r| r.get(0),
        )
        .map_err(HostError::storage)?;
    if runs.iter().any(|run| !run.state.is_terminal())
        || approvals
            .iter()
            .any(|a| a.status == "pending" || a.status == "approved" && !a.consumed)
        || deleting != 0
    {
        return Err(HostError::new(
            "conflict",
            "迁移要求无活动Run、未消费审批和未完成删除；请先正常清理并关闭Host",
        ));
    }
    let roots: BTreeMap<String, String> = conn
        .prepare("SELECT id,path FROM host_roots ORDER BY id")
        .map_err(HostError::storage)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(HostError::storage)?
        .collect::<Result<_, _>>()
        .map_err(HostError::storage)?;
    let requests: u64 = conn
        .query_row("SELECT COUNT(*) FROM host_requests", [], |r| r.get(0))
        .map_err(HostError::storage)?;
    let mut team_counts = BTreeMap::new();
    for team in bindings
        .iter()
        .map(|b| &b.scope.slot_id)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let count = |sql: &str| -> HostResult<u64> {
            conn.query_row(sql, [team], |r| r.get(0))
                .map_err(HostError::storage)
        };
        // 分项聚合，避免Run、请求、审批多表连接把数量乘大。
        let sessions = count("SELECT COUNT(*) FROM host_bindings WHERE slot=?1")?;
        let runs = count("SELECT COUNT(*) FROM host_runs WHERE session IN (SELECT id FROM host_bindings WHERE slot=?1)")?;
        let requests = count("SELECT COUNT(*) FROM host_requests WHERE session IN (SELECT id FROM host_bindings WHERE slot=?1)")?;
        let approvals = count("SELECT COUNT(*) FROM host_approvals WHERE run IN (SELECT id FROM host_runs WHERE session IN (SELECT id FROM host_bindings WHERE slot=?1))")?;
        let children = count("SELECT COUNT(*) FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.parent_scope.slot_id')=?1")?;
        team_counts.insert(team.clone(), serde_json::json!({"sessions":sessions,"runs":runs,"requests":requests,"approvals":approvals,"children":children}));
    }
    let rebinds = rebind_events(conn)?;
    // 只输出审核需要的ID和数量；审批输入、消息和Provider秘密不进入计划。
    Ok(
        serde_json::json!({"revision":revision,"bindings":bindings,"roots":roots,"run_count":runs.len(),"request_count":requests,"approval_count":approvals.len(),"team_counts":team_counts,"rebinds":rebinds}),
    )
}

impl Db {
    /// 功能：只读取得离线迁移所需原绑定和数量，不恢复或新建元数据。
    /// 参数：home为已核验且持有原OS独占锁的数据目录。
    /// 返回：同一SQLite读事务中的范围和审核信息。
    /// 错误：残留WAL、未知版本、活动执行、审批或损坏均拒绝。
    /// 状态：immutable只读打开，不创建WAL/SHM、索引或锁文件。
    /// 并发与取消：调用方必须持有原home独占锁，禁止与Host同跑。
    /// 权限：只供本机已批准迁移协调器，不向HTTP暴露。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013；CHG-2026-0007第12.12节。
    pub(crate) fn workspace_inventory(home: &std::path::Path) -> HostResult<serde_json::Value> {
        for suffix in ["-wal", "-journal"] {
            let path = home.join(format!("engine-host/host.db{suffix}"));
            match std::fs::metadata(path) {
                Ok(meta) if meta.len() != 0 => {
                    return Err(HostError::new(
                        "conflict",
                        "数据库仍有未检查点日志；先用原配置正常启动并关闭Host，不删除日志",
                    ))
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(HostError::storage(e)),
            }
        }
        let path = home.join("engine-host/host.db");
        let mut uri = "file:".to_owned();
        for byte in path.to_string_lossy().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"/-_.:".contains(byte) {
                uri.push(*byte as char);
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        uri.push_str("?mode=ro&immutable=1");
        let conn = Connection::open_with_flags(
            uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(HostError::storage)?;
        migration_inventory(&conn)
    }

    /// 功能：在原Host数据库单事务中登记目标根、重绑父子会话并保存迁移审计。
    /// 参数：home及expected为已锁定预检对象，roots为原有加新根，moves为已复制核验的团队事件。
    /// 返回：全部绑定和审计原子提交成功。
    /// 错误：预检漂移、旧绑定不符、根身份冲突或SQL失败回滚事务。
    /// 状态：不改JSONL、消息、运行、请求或审批事实，不新建表。
    /// 并发与取消：调用方持有home和工作根OS独占锁，提交不可半途释放锁。
    /// 权限：仅本机协调器可调用；事件中的团队与全部既有绑定逐项核对。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013；CHG-2026-0007第12.12节。
    pub(crate) fn commit_workspace_rebind(
        home: &std::path::Path,
        expected: &serde_json::Value,
        roots: &BTreeMap<String, PathBuf>,
        moves: &[serde_json::Value],
    ) -> HostResult<()> {
        let mut conn = Connection::open_with_flags(
            home.join("engine-host/host.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(HostError::storage)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(HostError::storage)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(HostError::storage)?;
        if migration_inventory(&tx)? != *expected {
            return Err(HostError::new(
                "conflict",
                "迁移前元数据已变化，必须重新预检",
            ));
        }
        for (id, path) in roots {
            let prior: Option<String> = tx
                .query_row("SELECT path FROM host_roots WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(HostError::storage)?;
            if prior
                .as_ref()
                .is_some_and(|p| std::path::Path::new(p) != path)
            {
                return Err(HostError::new(
                    "scope_denied",
                    "迁移禁止替换既有根标识的物理路径",
                ));
            }
            tx.execute(
                "INSERT OR IGNORE INTO host_roots(id,path) VALUES(?1,?2)",
                params![id, path.to_string_lossy()],
            )
            .map_err(HostError::storage)?;
        }
        let mut changed = std::collections::BTreeSet::new();
        for event in moves {
            crate::workspace_migration::validate_migration_event(event)?;
            let field = |key: &str| {
                event[key]
                    .as_str()
                    .ok_or_else(|| HostError::new("invalid_input", "迁移事件字段缺失"))
            };
            let (team, old, new) = (field("team")?, field("old_root")?, field("new_root")?);
            if roots.get(old).map(|p| p.as_path()) != Some(std::path::Path::new(field("old_path")?))
                || roots.get(new).map(|p| p.as_path())
                    != Some(std::path::Path::new(field("new_path")?))
            {
                return Err(HostError::new(
                    "scope_denied",
                    "迁移根与已核验物理目录不一致",
                ));
            }
            let ids: Vec<String> =
                serde_json::from_value(event["sessions"].clone()).map_err(HostError::storage)?;
            let mut actual = Vec::new();
            for mut binding in
                read_json_rows::<SessionBinding>(&tx, "SELECT data FROM host_bindings ORDER BY id")?
            {
                if binding.scope.slot_id != team {
                    continue;
                }
                if binding.root_id != old || !changed.insert(binding.session_id.clone()) {
                    return Err(HostError::new("scope_denied", "迁移团队绑定不一致或重复"));
                }
                actual.push(binding.session_id.clone());
                binding.root_id = new.to_owned();
                tx.execute(
                    "UPDATE host_bindings SET data=?2 WHERE id=?1",
                    params![
                        binding.session_id,
                        serde_json::to_string(&binding).map_err(HostError::storage)?
                    ],
                )
                .map_err(HostError::storage)?;
            }
            if ids != actual {
                return Err(HostError::new("conflict", "迁移会话清单不完整"));
            }
            tx.execute(
                "INSERT INTO host_events(kind,reference) VALUES('workspace_rebind',?1)",
                [serde_json::to_string(event).map_err(HostError::storage)?],
            )
            .map_err(HostError::storage)?;
        }
        tx.commit().map_err(HostError::storage)?;
        Ok(())
    }

    /// 功能：仅按可信迁移事实解析此会话的历史绝对产物路径。
    /// 参数：session为已鉴权Session，path为原工具账本中的路径。
    /// 返回：本团队新根下的规范相对路径与迁移时SHA256，或无历史映射。
    /// 错误：迁移版本、清单或路径损坏明确拒绝，不回退到旧共享根。
    /// 状态：只读原host_events，不改JSONL或建立浏览器可写别名。
    /// 并发与取消：原Store序列内执行，文件读取仍持有所属根共享锁。
    /// 权限：逐项核对session/team归属，不能用另一团队的迁移读取旧根。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013。
    pub(crate) fn migrated_artifact(
        &self,
        session: &str,
        path: &str,
    ) -> HostResult<Option<(String, String)>> {
        let binding = self.binding(session)?;
        if !std::path::Path::new(path).is_absolute() {
            return Ok(None);
        }
        for event in rebind_events(&self.conn)?.into_iter().rev() {
            if event["team"].as_str() != Some(&binding.scope.slot_id)
                || !event["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id.as_str() == Some(session))
            {
                continue;
            }
            let old = std::path::Path::new(event["old_path"].as_str().unwrap());
            let Ok(relative) = std::path::Path::new(path).strip_prefix(old) else {
                continue;
            };
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(HostError::new("scope_denied", "历史产物路径不规范"));
            }
            let name = relative
                .to_str()
                .ok_or_else(|| HostError::new("invalid_input", "历史产物路径不可表示"))?
                .replace('\\', "/");
            let file = event["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["path"].as_str() == Some(&name))
                .ok_or_else(|| HostError::new("not_found", "该历史产物没有被此次团队迁移复制"))?;
            let hash = file["sha256"]
                .as_str()
                .ok_or_else(|| HostError::new("storage_unavailable", "历史产物迁移摘要损坏"))?;
            return Ok(Some((name, hash.to_owned())));
        }
        Ok(None)
    }

    fn validate_workspace_rebinds(&self) -> HostResult<()> {
        let mut latest = BTreeMap::new();
        for event in rebind_events(&self.conn)? {
            let root = event["new_root"].as_str().unwrap();
            // 源和目标均核对持久根目录，避免损坏的旧路径被当成可信产物别名。
            for (id, field) in [
                (event["old_root"].as_str().unwrap(), "old_path"),
                (root, "new_path"),
            ] {
                let path: String = self
                    .conn
                    .query_row("SELECT path FROM host_roots WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .map_err(HostError::storage)?;
                if event[field].as_str() != Some(&path) {
                    return Err(HostError::new(
                        "storage_unavailable",
                        "迁移根身份与审计不一致",
                    ));
                }
            }
            for id in event["sessions"].as_array().unwrap() {
                let id = id
                    .as_str()
                    .ok_or_else(|| HostError::new("storage_unavailable", "迁移会话标识损坏"))?;
                latest.insert(
                    id.to_owned(),
                    (event["team"].as_str().unwrap().to_owned(), root.to_owned()),
                );
            }
        }
        for binding in self.bindings()? {
            if latest.get(&binding.session_id).is_some_and(|(team, root)| {
                team != &binding.scope.slot_id || root != &binding.root_id
            }) {
                return Err(HostError::new(
                    "storage_unavailable",
                    "迁移后父子绑定与最新审计不一致",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod office_regressions {
    use super::*;
    use crate::{ClientScope, Host, HostConfig};
    use openpaw_core::{Message, Role};
    use serde_json::json;
    #[tokio::test]
    async fn targeted_run_read_preserves_tool_order_approval_scope_and_readonly_revision() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let binding = host
            .open_session(ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "targeted-read".into(),
            })
            .await
            .unwrap();
        host.inner
            .store
            .call(move |db| {
                let session = &binding.session_id;
                for id in ["target", "other"] {
                    let offset = db.events(session)?.len();
                    db.append(session, SessionEvent::RunStarted { run_id: id.into() })?;
                    db.append(
                        session,
                        SessionEvent::ToolCallStart {
                            call_id: "same-call".into(),
                            tool_name: "read_file".into(),
                        },
                    )?;
                    db.save_run(
                        RunView {
                            run_id: id.into(),
                            session_id: session.clone(),
                            state: RunState::Running,
                            revision: 0,
                            text: String::new(),
                            tools: vec![],
                            error: None,
                            text_truncated: false,
                        },
                        offset,
                    )?;
                }
                for (run, kind, success) in [
                    ("target", "tool_waiting_resource", false),
                    ("target", "tool_started", false),
                    ("other", "tool_result", false),
                    ("target", "tool_result", true),
                ] {
                    db.bump(
                        kind,
                        &json!({"run_id": run, "call_id":"same-call", "tool_name":"read_file", "input":{}, "success":success}).to_string(),
                    )?;
                }
                let approval = |id: &str, run: &str| ApprovalView {
                    approval_id: id.into(),
                    binding: openpaw_core::ToolCallBinding {
                        run_id: run.into(),
                        call_id: "same-call".into(),
                        tool_name: "read_file".into(),
                        input: json!({"path":"example"}),
                    },
                    status: "pending".into(),
                    consumed: false,
                    revision: 0,
                    expires_at_ms: i64::MAX,
                };
                db.save_approval(approval("other-approval", "other"))?;
                let revision = db.revision()?;
                let target = db.get_run("target")?;
                assert_eq!(target.tools.len(), 1);
                assert_eq!(target.tools[0].state, "completed");
                assert_eq!(db.get_run("other")?.tools[0].state, "awaiting_approval");
                assert_eq!(db.revision()?, revision);
                db.save_approval(approval("target-approval", "target"))?;
                assert_eq!(db.get_run("target")?.tools[0].state, "awaiting_approval");
                let mut resolved = approval("target-approval", "target");
                resolved.status = "rejected".into();
                db.save_approval(resolved)?;
                assert_eq!(db.get_run("target")?.tools[0].state, "completed");
                assert_eq!(db.get_run("missing").unwrap_err().kind, "not_found");
                db.conn
                    .execute("UPDATE host_runs SET data='broken' WHERE id='target'", [])
                    .map_err(HostError::storage)?;
                assert_eq!(
                    db.get_run("target").unwrap_err().kind,
                    "storage_unavailable"
                );
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn stream_checkpoint_is_bounded_scoped_prefix_only_and_never_revives_deleted_data() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "checkpoint".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let session = binding.session_id.clone();
        host.inner.store.call(move |db| {
            let offset=db.events(&session)?.len();
            db.append(&session,SessionEvent::Message(Message::user_text("输入")))?;
            db.append(&session,SessionEvent::RunStarted{run_id:"checkpoint-run".into()})?;
            let run=RunView{run_id:"checkpoint-run".into(),session_id:session.clone(),state:RunState::Running,revision:0,text:String::new(),tools:vec![],error:None,text_truncated:false};
            db.save_run(run,offset)?;
            let mut buffer=RunText::bounded(4*1024*1024);buffer.append("可靠前缀");
            db.live.lock().unwrap().insert("checkpoint-run".into(),buffer);
            db.checkpoint_run("checkpoint-run","wrong-session")?;
            assert!(db.runs()?.iter().find(|r|r.run_id=="checkpoint-run").unwrap().text.is_empty());
            db.checkpoint_run("checkpoint-run",&session)?;
            let raw=db.runs()?.into_iter().find(|r|r.run_id=="checkpoint-run").unwrap();
            assert_eq!(raw.text,"可靠前缀");
            // 相同内容不执行UPDATE：触发器证明不会每秒重写不变检查点。
            db.conn.execute_batch("CREATE TRIGGER checkpoint_failure BEFORE UPDATE ON host_runs BEGIN SELECT RAISE(ABORT,'checkpoint-write-failure'); END;").map_err(HostError::storage)?;
            db.checkpoint_run("checkpoint-run",&session)?;
            db.live.lock().unwrap().get_mut("checkpoint-run").unwrap().append("，尚未完成");
            assert_eq!(db.checkpoint_run("checkpoint-run",&session).unwrap_err().kind,"storage_unavailable");
            db.conn.execute_batch("DROP TRIGGER checkpoint_failure;").map_err(HostError::storage)?;
            db.checkpoint_run("checkpoint-run",&session)?;
            // 已提交的assistant消息追上后清冗余，不用检查点替代JSONL。
            db.append(&session,SessionEvent::Message(Message::assistant_text("可靠前缀，尚未完成")))?;
            db.checkpoint_run("checkpoint-run",&session)?;
            assert!(db.runs()?.iter().find(|r|r.run_id=="checkpoint-run").unwrap().text.is_empty());
            assert_eq!(db.get_run("checkpoint-run")?.text,"可靠前缀，尚未完成");
            let mut bad=RunText::bounded(4*1024*1024);bad.append("与原账本不符");
            db.live.lock().unwrap().insert("checkpoint-run".into(),bad);
            assert!(db.checkpoint_run("checkpoint-run",&session).is_err());
            db.live.lock().unwrap().remove("checkpoint-run");
            // 持久损坏也拒绝；不能因为live缺席就把任意文字当可信历史。
            let mut corrupt=db.runs()?.into_iter().find(|r|r.run_id=="checkpoint-run").unwrap();corrupt.text="损坏".into();
            db.conn.execute("UPDATE host_runs SET data=?1 WHERE id='checkpoint-run'",[serde_json::to_string(&corrupt).unwrap()]).map_err(HostError::storage)?;
            assert!(db.get_run("checkpoint-run").is_err());
            corrupt.text="x".repeat(4*1024*1024+1);
            db.conn.execute("UPDATE host_runs SET data=?1 WHERE id='checkpoint-run'",[serde_json::to_string(&corrupt).unwrap()]).map_err(HostError::storage)?;
            assert!(db.get_run("checkpoint-run").is_err());
            corrupt.text.clear();corrupt.state=RunState::Interrupted;
            db.conn.execute("UPDATE host_runs SET data=?1 WHERE id='checkpoint-run'",[serde_json::to_string(&corrupt).unwrap()]).map_err(HostError::storage)?;
            db.begin_delete(&binding.scope)?;
            db.checkpoint_run("checkpoint-run",&session)?;
            db.purge_session(&serde_json::to_string(&binding.scope).unwrap(),&session)?;
            db.checkpoint_run("checkpoint-run",&session)?;
            assert!(db.runs()?.is_empty()); assert!(db.binding(&session).is_err());
            Ok(())
        }).await.unwrap();
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn failed_acceptance_offset_does_not_claim_later_user_input() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "input-boundary".into(),
        };
        let binding = host.open_session(scope).await.unwrap();
        host.inner
            .store
            .call(move |db| {
                let offset = db.events(&binding.session_id)?.len();
                let failed = RunView {
                    run_id: "failed-before-input".into(),
                    session_id: binding.session_id.clone(),
                    state: RunState::Failed,
                    revision: 0,
                    text: String::new(),
                    tools: vec![],
                    error: Some(HostError::new("storage_unavailable", "输入未提交")),
                    text_truncated: false,
                };
                db.save_run(failed.clone(), offset)?;
                db.append(
                    &binding.session_id,
                    SessionEvent::Message(Message::user_text("后一个请求的输入")),
                )?;
                db.append(
                    &binding.session_id,
                    SessionEvent::RunStarted {
                        run_id: "another-run".into(),
                    },
                )?;
                assert_eq!(db.run_input(&failed)?.1, "");
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn torn_import_resumes_exact_prefix_and_projection_keeps_version_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config.clone()).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "repair".into(),
        };
        let binding = host.open_session(scope).await.unwrap();
        let id = binding.session_id;
        let sid = id.clone();
        host.inner.store.call(move|db|{
            let history=vec![Message::user_text("imported-user"),Message::assistant_text("imported-answer")];
            let bytes=serde_json::to_vec(&history).unwrap();let fingerprint=format!("fnv1a64:{:016x}",bytes.iter().fold(0xcbf29ce484222325u64,|h,b|(h^u64::from(*b)).wrapping_mul(0x100000001b3)));
            let offset=db.events(&sid)?.len();
            // 模拟导入marker已保存、仅第一条JSONL完成后进程退出；不执行任何工具。
            db.conn.execute("INSERT INTO host_context_import(session,event_offset,count,fingerprint,complete) VALUES(?1,?2,2,?3,0)",params![sid,offset,fingerprint]).unwrap();
            db.append(&sid,SessionEvent::Message(history[0].clone()))?;
            let snapshot=json!({"seq":17,"stableTime":"2026-10-21T00:00:00Z"});db.save_office_projection("office-1",&snapshot,"pending")?;
            Ok(())
        }).await.unwrap();
        host.shutdown().await.unwrap();
        let host = Host::start(config).await.unwrap();
        let sid = id.clone();
        host.inner
            .store
            .call(move |db| {
                let history = vec![
                    Message::user_text("imported-user"),
                    Message::assistant_text("imported-answer"),
                ];
                db.reconcile_history(&sid, &history)?;
                db.reconcile_history(&sid, &history)?;
                let messages: Vec<_> = db
                    .events(&sid)?
                    .into_iter()
                    .filter_map(|e| {
                        if let SessionEvent::Message(m) = e {
                            Some(m)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(messages.len(), 2);
                assert_eq!(messages[0].role, Role::User);
                assert_eq!(messages[1].text(), "imported-answer");
                let (snapshot, state) = db.office_projection("office-1")?.unwrap();
                assert_eq!(snapshot["seq"], 17);
                assert_eq!(snapshot["stableTime"], "2026-10-21T00:00:00Z");
                assert_eq!(state, "pending");
                assert!(db
                    .reconcile_history(
                        &sid,
                        &[
                            Message::user_text("different"),
                            Message::assistant_text("history")
                        ]
                    )
                    .is_err());
                let first = db.office_call_id("run", "z-call")?;
                let second = db.office_call_id("run", "a-call")?;
                assert_ne!(first, second);
                assert_eq!(db.office_call_id("run", "z-call")?, first);
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn deleted_ui_receipt_has_no_navigation_body_and_cannot_revive_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cfg = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(cfg).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "delete-receipt".into(),
        };
        host.open_session(scope.clone()).await.unwrap();
        let s = scope.clone();
        host.inner
            .store
            .call(move |db| {
                db.office_action(
                    "delete-action",
                    &s,
                    "conversation.delete",
                    "claimed",
                    chrono::Utc::now().timestamp_millis() + 50000,
                    &json!({"private":"must be removed"}),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        host.delete_session(&scope).await.unwrap();
        let s = scope.clone();
        host.inner.store.call(move|db|{
            db.office_action_state("delete-action",&s,"unconfirmed")?;
            let (kind,status,body):(String,String,String)=db.conn.query_row("SELECT kind,status,context FROM host_office_actions WHERE action_id='delete-action'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(HostError::storage)?;
            assert_eq!((kind.as_str(),status.as_str(),body.as_str()),("receipt","deleted_ui_unconfirmed","{}"));
            assert!(db.office_navigation("delete-action",&s).is_err());
            assert!(db.office_decision("late-approval","late-display",&s,chrono::Utc::now().timestamp_millis()+10000,"unknown").is_err());
            let now=chrono::Utc::now().timestamp_millis();
            db.office_scene("run:terminal","original",now+10000,"queued")?;
            db.office_scene("run:terminal","original",now+10000,"forwarded")?;
            db.office_scene("run:terminal","forged",now+30000,"completed")?;
            let (id,status,expires):(String,String,i64)=db.conn.query_row("SELECT command_id,status,expires_at FROM host_office_scene WHERE event_id='run:terminal'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(HostError::storage)?;
            assert_eq!((id.as_str(),status.as_str(),expires),("original","forwarded",now+10000));
            Ok(())
        }).await.unwrap();
        assert!(host.open_session(scope).await.is_err());
        host.shutdown().await.unwrap();
    }
}
