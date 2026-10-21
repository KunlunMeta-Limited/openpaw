//! 职责：串行化Host元数据与原JSONL写入，恢复不重放副作用。
//! 输入输出：可等待命令到已提交事实、一致视图和通知。
//! 依赖：原SessionWriter/Reader/SessionDb，单一SQLite元数据库。
//! 禁止职责：不在host.db复制聊天消息，不自动重试工具。
use crate::{types::*, HostError, HostResult};
use openpaw_core::{Session, SessionEvent};
use openpaw_sandbox::file_lock::{try_lock_exclusive, FileLockGuard};
use openpaw_store::{SessionDb, SessionReader, SessionWriter};
use rusqlite::{params, Connection};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
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
    /// 字段：Arc<Mutex<HashMap<String, (String,bool)>>>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) live: Arc<Mutex<HashMap<String, (String, bool)>>>,
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
        live: Arc<Mutex<HashMap<String, (String, bool)>>>,
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
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Job::Call(Box::new(move |db| {
                let result = if db.owner.is_some() {
                    f(db)
                } else {
                    Err(HostError::new("storage_unavailable", "Host存储已关闭"))
                };
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
    fn open(
        home: PathBuf,
        roots: BTreeMap<String, PathBuf>,
        notices: broadcast::Sender<HostNotice>,
        live: Arc<Mutex<HashMap<String, (String, bool)>>>,
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
        if version != 0 && version != 1 {
            return Err(HostError::new(
                "unsupported",
                "不支持的Host数据版本，拒绝写入",
            ));
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS host_roots(id TEXT PRIMARY KEY,path TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS host_bindings(id TEXT PRIMARY KEY, namespace TEXT NOT NULL,slot TEXT NOT NULL,conversation TEXT NOT NULL,data TEXT NOT NULL, UNIQUE(namespace,slot,conversation));
        CREATE TABLE IF NOT EXISTS host_runs(id TEXT PRIMARY KEY,session TEXT NOT NULL,data TEXT NOT NULL,input_event_offset INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS host_approvals(id TEXT PRIMARY KEY,run TEXT NOT NULL,call TEXT NOT NULL,data TEXT NOT NULL,UNIQUE(run,call));
        CREATE TABLE IF NOT EXISTS host_events(revision INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,reference TEXT NOT NULL);
        PRAGMA user_version=1;").map_err(HostError::storage)?;
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
        };
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
        let mut stmt = self.conn.prepare(sql).map_err(HostError::storage)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(HostError::storage)?;
        rows.map(|s| {
            serde_json::from_str(&s.map_err(HostError::storage)?).map_err(HostError::storage)
        })
        .collect()
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
        self.bindings()?
            .into_iter()
            .find(|b| b.session_id == id)
            .ok_or_else(|| HostError::new("not_found", "会话不存在"))
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
        let mut run = self
            .runs()?
            .into_iter()
            .find(|r| r.run_id == id)
            .ok_or_else(|| HostError::new("not_found", "运行不存在"))?;
        let events = self.events(&run.session_id)?;
        let mut active = false;
        let mut text = String::new();
        let mut tools = std::collections::BTreeMap::new();
        for e in events {
            match e {
                SessionEvent::RunStarted { run_id } => active = run_id == id,
                SessionEvent::Message(m) if active && m.role == openpaw_core::Role::Assistant => {
                    for block in m.content {
                        if let openpaw_core::ContentBlock::Text { text: t } = block {
                            text.push_str(&t);
                        }
                    }
                }
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
                _ => {}
            }
        }
        let mut stmt=self.conn.prepare("SELECT kind,reference FROM host_events WHERE kind IN ('tool_started','tool_result') ORDER BY revision").map_err(HostError::storage)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(HostError::storage)?;
        for row in rows {
            let (kind, value) = row.map_err(HostError::storage)?;
            let value: serde_json::Value =
                serde_json::from_str(&value).map_err(HostError::storage)?;
            if value["run_id"].as_str() == Some(id) {
                if let Some(tool) = value["call_id"].as_str().and_then(|c| tools.get_mut(c)) {
                    tool.state = if kind == "tool_started" {
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
        for a in self.approvals()? {
            if a.binding.run_id == id && a.status == "pending" {
                if let Some(t) = tools.get_mut(&a.binding.call_id) {
                    t.state = "awaiting_approval".into();
                }
            }
        }
        run.tools = tools.into_values().collect();
        if !run.state.is_terminal() {
            if let Some(live) = self.live.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
                if live.0.len() >= text.len() {
                    run.text = live.0.clone();
                    run.text_truncated = live.1;
                } else {
                    run.text = text;
                }
            } else {
                run.text = text;
            }
        } else {
            run.text = text;
        }
        Ok(run)
    }
    /// 功能：原子保存Run状态与生命周期revision。
    /// 参数：run为新视图，offset为原输入事件位置。
    /// 返回：提交后的Run视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不保存第二份消息；cancelling不能退回running，提交后通知。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn save_run(&mut self, mut run: RunView, offset: usize) -> HostResult<RunView> {
        // 取消不能被稍晚开始的执行任务逆转为running。
        if run.state == RunState::Running
            && self
                .runs()?
                .iter()
                .any(|r| r.run_id == run.run_id && r.state == RunState::Cancelling)
        {
            run.state = RunState::Cancelling;
        }
        // 状态与revision同一SQLite事务，发布永远在提交之后。
        let tx = self.conn.transaction().map_err(HostError::storage)?;
        tx.execute(
            "INSERT INTO host_events(kind,reference) VALUES('run',?1)",
            [&run.run_id],
        )
        .map_err(HostError::storage)?;
        run.revision = tx.last_insert_rowid() as u64;
        let mut persisted = run.clone();
        persisted.text.clear();
        persisted.tools.clear();
        tx.execute("INSERT INTO host_runs(id,session,data,input_event_offset) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![run.run_id,run.session_id,serde_json::to_string(&persisted).map_err(HostError::storage)?,offset]).map_err(HostError::storage)?;
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
    fn recover(&mut self) -> HostResult<()> {
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
