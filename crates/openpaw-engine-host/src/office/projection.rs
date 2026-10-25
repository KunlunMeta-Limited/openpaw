//! 职责：将共享Run/call事实投影成完整Office快照并可靠同步。
//! 输入输出：原JSONL和Host状态到持久seq/完整内容及官方消息。
//! 依赖：原Store、官方client、单办公室协调任务。
//! 禁止职责：不伪造进度、工具开始或多Agent，不复制会话账本。
use super::{client::Client, dto, OfficeConfig};
use crate::{ClientScope, Host, HostError, HostResult, RunState};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// 用途：限制发送给Office的完整快照消息大小。
/// 字段：UTF-8编码后的官方消息最大字节数。
/// 来源去向：协议常量到快照构建、缓存校验和发送边界。
/// 约束：必须小于等于Office公开的64 KiB消息预算，不能通过截断破坏快照结构。
/// 稳定性：Office协议兼容常量；调整必须同步协议夹具和验收。
/// 关联：需求/设计REQ-OP-011；DES-OP-007。
pub(super) const OFFICE_MESSAGE_LIMIT: usize = 64_000;

/// 用途：限制从历史会话中寻找可继续展示的业务候选数量。
/// 字段：最近Run候选窗口大小。
/// 来源去向：当前办公室投影到进度/委派事实查询。
/// 约束：最新Run和活动Run始终保留；旧事实仍由账本和detail-v1查询。
/// 稳定性：D86私有性能常量；改变时需重跑历史投影回归。
/// 关联：CHG-2026-0007第12.51节。
const RECENT_BUSINESS_CANDIDATES: usize = 12;
/// Office 快照只承载最近一组可点击的业务会话；更早业务事实仍保留在
/// Session/Run 账本和 detail-v1 查询中。这个上限把历史白板与协议封套预算
/// 联系起来，避免旧验收会话无限增长后阻塞整个办公室的新消息。
const MAX_PROTECTED_BUSINESS_CONVERSATIONS: usize = 4;

/// 功能：判断持久化快照是否可以安全发送给配置的Office。
/// 参数：team为办公室作用域，snapshot为待验证的完整快照。
/// 返回：结构、作用域、协议字段和编码字节数均满足限制时为true。
/// 错误：校验失败以false返回，不向调用方传播序列化错误。
/// 状态：纯只读校验；失败的旧缓存由Host事实重新构建。
/// 并发与取消：不执行阻塞I/O、不取得资源锁，调用方自行处理取消。
/// 权限：只接受与team完全一致的工作区快照，防止跨办公室缓存泄漏。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
pub(super) fn snapshot_fits_office_message(team: &str, snapshot: &Value) -> bool {
    if snapshot["schemaVersion"] != "1.0"
        || snapshot["workspace"]["id"] != team
        || !snapshot["conversations"].is_array()
    {
        return false;
    }
    let request = json!({
        "type": "office.snapshot",
        "teamId": team,
        "snapshot": snapshot,
    });
    serde_json::to_vec(&request)
        .map(|bytes| bytes.len() <= OFFICE_MESSAGE_LIMIT && dto::protocol_value(&request))
        .unwrap_or(false)
}

/// 用途：持有Office同步任务；关闭时在Store之前完成最终投影。
/// 字段：停止token、任务、只读配置和有界会话恢复通知。
/// 来源去向：HostServer启动到Host关闭。
/// 约束：网络失败不改变Run结果，冲突停止自动投递。
/// 稳定性：T4内部生命周期。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-009。
pub(crate) struct Coordinator {
    stop: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    config: OfficeConfig,
    address: std::net::SocketAddr,
    ui_refresh: tokio::sync::broadcast::Sender<ClientScope>,
}
impl Coordinator {
    /// 功能：为每个授权办公室启动独立投影和同步循环。
    /// 参数：host为唯一运行服务，config为显式Office配置，address为实际回环监听地址。
    /// 返回：同步任务所有者。
    /// 错误：凭据或客户端初始化错误立即拒绝启用。
    /// 状态：每个办公室先持久投影再发送；同seq重试不改变内容。
    /// 并发与取消：每办公室一个worker，不持资源锁等待网络。
    /// 权限：只读取配置授权槽，人物先验证capabilities。
    /// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
    pub(crate) async fn start(
        host: Host,
        config: OfficeConfig,
        address: std::net::SocketAddr,
    ) -> HostResult<Self> {
        let client = std::sync::Arc::new(Client::new(&config)?);
        let stop = CancellationToken::new();
        let (ui_refresh, _) = tokio::sync::broadcast::channel(32);
        // 只读取已经保存的完整投影，先让人物点击使用可信快照；昂贵的事实重建
        // 仍由后台worker执行，避免Host重启时把首个UI请求排在投影初始化之后。
        let teams: Vec<String> = host.inner.config.slots.keys().cloned().collect();
        let cached = host
            .inner
            .store
            .call(move |db| {
                let mut values = Vec::new();
                for team in teams {
                    if let Some((snapshot, _)) = db.office_projection(&team)? {
                        if snapshot_fits_office_message(&team, &snapshot) {
                            values.push((team.to_owned(), snapshot));
                        }
                    }
                }
                Ok(values)
            })
            .await?;
        {
            let mut cache = host
                .inner
                .office_projection_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (team, snapshot) in cached {
                cache.insert(team, snapshot);
            }
        }
        let mut tasks = Vec::new();
        for team in host.inner.config.slots.keys() {
            let team = team.clone();
            let host = host.clone();
            let config = config.clone();
            let token = stop.clone();
            let client = client.clone();
            tasks.push(tokio::spawn(async move {

                // 已有完整投影时先让Office UI/聊天动作使用这份可信快照，避免Host
                // 重启后首个点击被全量历史重建占住Store；后台仍在短窗口后刷新事实。
                let has_cached_projection = host
                    .inner
                    .office_projection_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&team);
                let cached_state = host
                    .inner
                    .store
                    .call({
                        let team = team.clone();
                        move |db| {
                            Ok(db
                                .office_projection(&team)?
                                .map(|(_, state)| state)
                                .unwrap_or_default())
                        }
                    })
                    .await
                    .unwrap_or_default();
                // A persisted halted state is a recoverable failure from an
                // earlier process (for example a cache size error). Retry it
                // immediately on restart so the UI is not left stale for the
                // normal cached-refresh grace period.
                if has_cached_projection && cached_state != "halted" {
                    tokio::select! {
                        _ = token.cancelled() => return,
                        // Keep the cached UI responsive while allowing the
                        // first worker pass to avoid competing with startup.
                        // One second is enough; a longer grace period leaves
                        // a valid projection visibly stale after restart.
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }

                let mut previous_status=String::new();let mut halted=false;let mut delay=1;
                let mut last_revision=None;
                let mut last_network=tokio::time::Instant::now()-Duration::from_secs(10);
                loop {
                    if token.is_cancelled(){break;}
                    if host
                        .inner
                        .office_ui_busy
                        .load(std::sync::atomic::Ordering::Acquire)
                        != 0
                    {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                        }
                    }
                    let team_for_due = team.clone();
                    let (revision, handoff_due) = host
                        .inner
                        .store
                        .call(move |db| {
                            let revision = db.revision()?;
                            let due = db
                                .office_projection_handoff_until
                                .get(&team_for_due)
                                .is_some_and(|until| *until <= chrono::Utc::now());
                            Ok((Some(revision), due))
                        })
                        .await
                        .unwrap_or((None, false));
                    let now=tokio::time::Instant::now();
                    let refresh=previous_status.is_empty()
                        || revision!=last_revision
                        || handoff_due
                        // Office 回执可能在Host事实不变时变化（离线恢复、seq冲突、
                        // 服务端重启），保留原一秒外发复核；prepare的revision快路径
                        // 确保这不会重新扫描完整账本。
                        || now.duration_since(last_network)>=Duration::from_secs(1)
                        || previous_status == "cached"
                        || !matches!(previous_status.as_str(),"applied"|"cached"|"reserved");
                    let result=if refresh {
                        last_revision=revision;
                        last_network=now;
                        tokio::select!{_=token.cancelled()=>break,result=sync_once(&host,&config,&team,&client,halted,address)=>result}
                    } else {
                        Ok(previous_status.clone())
                    };
                    let status = match result {
                        Ok(s) => {
                            if s != "halted" {
                                halted = false;
                            }
                            delay = 1;
                            s
                        }
                        Err(e) => {
                            if std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some() {
                                eprintln!("engine-host diagnostics Office {} error={} message={}", team, e.kind, e.message);
                            }
                            if e.kind == "sequence_conflict" {
                                halted = true;
                                let team = team.clone();
                                let _ = host
                                    .inner
                                    .store
                                    .call(move |db| {
                                        if let Some((snapshot, _)) = db.office_projection(&team)? {
                                            db.save_office_projection(&team, &snapshot, "halted")?;
                                        }
                                        Ok(())
                                    })
                                    .await;
                            }
                            delay = (delay * 2).min(30);
                            e.kind
                        }
                    };
                    if status!=previous_status {eprintln!("engine-host Office {team}: {status}");previous_status=status;}
                    tokio::select!{_=token.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(delay))=>{}}
                }
            }));
        }
        tasks.push(tokio::spawn(super::approvals::run(
            host.clone(),
            client.clone(),
            ui_refresh.clone(),
            address,
            stop.clone(),
        )));
        tasks.push(tokio::spawn(super::actions::run(
            host.clone(),
            client.clone(),
            config.clone(),
            address,
            stop.clone(),
        )));
        tasks.push(tokio::spawn(super::scene::run(host, client, stop.clone())));
        Ok(Self {
            stop,
            tasks,
            config,
            address,
            ui_refresh,
        })
    }
    /// 功能：通知成功恢复聊天的会话需要重新展示待审批菜单。
    /// 参数：scope为已经轮次服务校验的完整作用域。
    /// 返回：无。
    /// 错误：没有接收器时忽略，原审批仍由原期限管理。
    /// 状态：不创建审批、Run或持久化事件。
    /// 并发与取消：非阻塞广播；其他作用域的展示忽略该通知。
    /// 权限：仅恢复展示，不改变输入、权限或审批状态。
    /// 关联：需求REQ-OP-013；设计DES-OP-008。
    pub(crate) fn refresh_ui(&self, scope: &ClientScope) {
        let _ = self.ui_refresh.send(scope.clone());
    }
    /// 功能：停止网络循环，在Store关闭前保存全部最终快照。
    /// 参数：self为worker所有者，host为已完成Run清理的共享服务。
    /// 返回：最终投影可靠保存的结果。
    /// 错误：存储失败上报；网络不可用保留pending并诊断。
    /// 状态：先全部保存，再最多三秒尝试外发；下次启动继续同版本。
    /// 并发与取消：等待worker退出，不在Store释放后执行投影。
    /// 权限：只访问已配置办公室。
    /// 关联：需求/设计REQ-OP-009/011；DES-OP-009。
    pub(crate) async fn finish(self, host: &Host) -> HostResult<()> {
        self.stop.cancel();
        for task in self.tasks {
            task.await.map_err(HostError::storage)?;
        }
        for team in host.inner.config.slots.keys() {
            if super::workspaces::active(host, &self.config, team).await? {
                prepare(host, &self.config, team).await?;
            }
        }
        let client = Client::new(&self.config)?;
        let flush = async {
            for team in host.inner.config.slots.keys() {
                // A persisted halted row is only authoritative for the final
                // flush when it came from a confirmed external sequence
                // conflict.  It prevents shutdown from immediately replaying
                // the same rejected snapshot; restart still retries it once.
                let halted = host
                    .inner
                    .store
                    .call({
                        let team = team.clone();
                        move |db| {
                            Ok(db
                                .office_projection(&team)?
                                .is_some_and(|(_, state)| state == "halted"))
                        }
                    })
                    .await
                    .unwrap_or(false);
                let _ = sync_once(host, &self.config, team, &client, halted, self.address).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(3), flush)
            .await
            .is_err()
        {
            eprintln!("engine-host Office: 最终快照已保存，部分投递待下次启动核对");
        }
        Ok(())
    }
}
fn status(state: RunState) -> &'static str {
    match state {
        RunState::Accepting | RunState::Accepted => "queued",
        RunState::Running => "running",
        RunState::Cancelling => "blocked",
        RunState::Completed => "completed",
        _ => "failed",
    }
}
/// 功能：将真实工具名称映射到可信成员与场景人物。
/// 参数：tool为已经由Host记录的工具名称。
/// 返回：成员ID与Office人物ID；未知工具归属主角色。
/// 错误：无；未知工具不会扩大权限。
/// 状态：纯只读映射，不创建任务或Run。
/// 并发与取消：无锁、无等待、不可取消。
/// 权限：返回值仅用于有界展示，不能作为授权依据。
/// 关联：需求/设计REQ-OP-021/023；DES-OP-011。
pub(super) fn actor(tool: &str) -> (&'static str, &'static str) {
    match tool {
        "read_file" | "write_file" | "edit_file" => ("file-agent", "file-agent"),
        "glob" | "grep" | "web_search" | "web_fetch" => ("data-agent", "data-agent"),
        "bash" => ("code-agent", "code-agent"),
        _ => ("office-lead", "office-lead"),
    }
}
fn task(
    id: String,
    title: String,
    state: &str,
    summary: String,
    old: &BTreeMap<String, Value>,
    now: &str,
    progress: u64,
) -> Value {
    let mut task = json!({"id":id,"title":dto::bounded(&title,160),"assigneeId":"office-lead","status":state,"progress":progress,"summary":dto::bounded(&summary,1000),"updatedAt":now});
    if let Some(prior) = old.get(&id) {
        let mut comparable = prior.clone();
        comparable["updatedAt"] = Value::String(now.into());
        if comparable == task {
            task["updatedAt"] = prior["updatedAt"].clone();
        }
    }
    task
}

/// 压缩旧业务会话的可选展示字段。
///
/// 任务、进度、白板状态和成员 ID 是历史回看的核心事实，不能因 Office
/// 快照预算被清空；交接事件、职责长文本和说明性字段属于可重建展示，
/// 可以先移除。返回值表示本次是否实际缩小了对象。
fn compact_business_conversation(conversation: &mut Value) -> bool {
    if let Some(events) = conversation["events"].as_array_mut() {
        if !events.is_empty() {
            events.clear();
            return true;
        }
    }
    if let Some(employees) = conversation["employees"].as_array_mut() {
        for employee in employees {
            if let Some(object) = employee.as_object_mut() {
                if object.remove("role").is_some() {
                    return true;
                }
                // 人物点击只需要稳定的成员身份；在线状态和场景绑定会由
                // 当前 scene/read 查询补齐。去掉这些可重建字段，避免历史
                // 业务会话在容量压力下挤掉仍有用的任务事实。
                for key in ["online", "sceneActorId"] {
                    if object.remove(key).is_some() {
                        return true;
                    }
                }
            }
        }
    }
    if let Some(whiteboard) = conversation["whiteboard"].as_object_mut() {
        // members 布局把这些字段列为必填；用合法空值压缩内容，不能删除
        // 字段后再把不符合 Schema 的快照交给 Office。
        for key in ["tagline", "note", "description"] {
            if whiteboard[key] != json!("") {
                whiteboard[key] = json!("");
                return true;
            }
        }
        if whiteboard["link"] != json!({"label": "", "url": ""}) {
            whiteboard["link"] = json!({"label": "", "url": ""});
            return true;
        }
        if let Some(steps) = whiteboard["steps"].as_array_mut() {
            if steps.len() > 3 {
                let keep_from = steps.len().saturating_sub(3);
                steps.drain(..keep_from);
                return true;
            }
        }
    }
    if let Some(tasks) = conversation["tasks"].as_array_mut() {
        for task in tasks.iter_mut() {
            if let Some(object) = task.as_object_mut() {
                if object.remove("summary").is_some() {
                    return true;
                }
            }
        }
        // 完整任务仍由 detail-v1/账本提供；快照保留主任务和最近七项，
        // 让包含大量历史进度报告的业务会话可以在 Office 64 KiB 封套内
        // 与其他会话共存。每项的状态、进度、ID和负责人仍然保留。
        const MAX_COMPACT_TASKS: usize = 8;
        if tasks.len() > MAX_COMPACT_TASKS {
            let keep_from = tasks.len().saturating_sub(MAX_COMPACT_TASKS - 1);
            tasks.drain(1..keep_from);
            return true;
        }
    }
    let title = conversation["title"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if title.chars().count() > 24 {
        conversation["title"] = json!(dto::bounded(&title, 24));
        return true;
    }
    false
}

/// 判断终态Run是否已经属于此前成功投影的历史。
///
/// `None` 表示尚无成功投影基线；首次看到的终态Run仍需经过逐人交接。
/// 只有Run revision不晚于上一次成功投影版本时，才可以直接恢复全部交接，
/// 避免Host重启或历史裁剪后重复播放已经消费过的动画。
fn is_historical_terminal(run_revision: u64, previous_source_revision: Option<u64>) -> bool {
    previous_source_revision.is_some_and(|revision| run_revision <= revision)
}

/// 将一项已由真实子Run产生、但尚未到展示时机的交接事实加入快照。
/// 该函数只操作内存中的规范快照；Run状态、权限和账本仍由首次投影读取。
/// 只有最后一个队列变体才允许提升主卡，避免后续人物尚未展示时提前报完成。
fn apply_handoff_item(
    conversation: &mut Value,
    item: &Value,
    event: &Value,
    final_item: bool,
    handoff_total: usize,
) {
    if let Some(tasks) = conversation["tasks"].as_array_mut() {
        tasks.retain(|existing| existing["id"] != item["id"]);
        tasks.push(item.clone());
    }
    if let Some(events) = conversation["events"].as_array_mut() {
        if !events.iter().any(|existing| existing["id"] == event["id"]) {
            events.push(event.clone());
        }
    }
    // The initial projection deliberately keeps the lead card blocked while
    // the handoff queue reveals one colleague at a time.  Once the last
    // revealed card is a real completed child, the queue variant must promote
    // the lead card as well.  Previously only the whiteboard steps were
    // recomputed, so a completed run remained visibly blocked forever after
    // the final handoff.
    if let Some(tasks) = conversation["tasks"].as_array_mut() {
        let all_children_completed = tasks.len() > 1
            && tasks
                .iter()
                .skip(1)
                .all(|task| task["status"] == "completed");
        let completed_children = tasks
            .iter()
            .skip(1)
            .filter(|task| task["status"] == "completed")
            .count() as u64;
        let total_children = tasks.len().saturating_sub(1) as u64;
        if let Some(main) = tasks.first_mut() {
            if main["status"] == "blocked" {
                if final_item && all_children_completed {
                    main["status"] = json!("completed");
                    main["progress"] = json!(100);
                    main["summary"] = json!("本次目标已完成");
                } else if total_children > 0 {
                    // 每个队列变体都要同步主卡进度；只在最后一项公开后
                    // 才允许提升为completed，避免白板和人物交接出现跳变。
                    let total = handoff_total.max(total_children as usize) as u64;
                    main["progress"] = json!(completed_children * 100 / (total + 1));
                    main["summary"] = json!("回复已结束；正在依次展示后续成员交接");
                }
            }
        }
    }
    let Some(tasks) = conversation["tasks"].as_array() else {
        return;
    };
    let main = tasks.first().cloned().unwrap_or(Value::Null);
    let completed = tasks
        .iter()
        .skip(1)
        .filter(|task| task["status"] == "completed")
        .count();
    let total = handoff_total.max(tasks.len().saturating_sub(1));
    let mut steps = Vec::new();
    for task in tasks.iter().skip(1).rev().take(3).rev() {
        let label = match task["status"].as_str() {
            Some("completed") => "已完成",
            Some("running") => "处理中",
            Some("blocked") => "等待确认",
            Some("failed") => "未完成",
            _ => "待处理",
        };
        steps.push(json!({
            "id": task["id"],
            "title": dto::bounded(&format!("{} · {label}", dto::bounded(task["title"].as_str().unwrap_or(""), 24)), 40),
            "progress": task["progress"],
        }));
    }
    let description = if main["status"] == "completed" {
        format!(
            "已确认完成 {completed}/{total} 步；{}",
            main["summary"].as_str().unwrap_or("本次目标已完成")
        )
    } else {
        format!(
            "已确认完成 {completed} 项；{}",
            main["summary"].as_str().unwrap_or("正在处理你的请求")
        )
    };
    conversation["whiteboard"] = json!({
        "template": "task-board-v1",
        "title": dto::bounded(main["title"].as_str().unwrap_or(""), 32),
        "status": main["status"],
        "tagline": "",
        "description": dto::bounded(&description, 500),
        "note": "",
        "link": {"label": "", "url": ""},
        "steps": steps,
    });
}
/// 功能：读取事实、生成有界完整快照并在变更时分配持久版本。
/// 参数：host为共享服务，config为Office配置，team为固定办公室。
/// 返回：已保存的规范化完整快照。
/// 错误：账本/SQL、容量或seq溢出错误。
/// 状态：不变内容保持原seq和时间；新内容先保存再返回。
/// 并发与取消：所有读取/保存通过同一Store命令，不读取自己HTTP接口。
/// 权限：只读取namespace=office及team绑定。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
pub(super) async fn prepare(host: &Host, config: &OfficeConfig, team: &str) -> HostResult<Value> {
    prepare_internal(host, config, team, false).await
}

/// 功能：在处理Office只读UI动作时准备一次可信投影。
/// 参数：host为共享运行服务，config为Office配置，team为已授权办公室。
/// 返回：从持久事实构建或推进后的规范快照。
/// 错误：Store、作用域、协议容量或快照序列错误会直接返回Host错误。
/// 状态：只推进已确认交接窗口；不改变Run、工具或审批事实。
/// 并发与取消：调用方持有UI优先协调；Store操作可由上层取消，网络不在此函数内执行。
/// 权限：仅读取team允许的会话和工作区，不跨办公室复用快照。
/// 关联：需求/设计REQ-OP-011/012；DES-OP-007/009。
pub(super) async fn prepare_for_ui(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
) -> HostResult<Value> {
    prepare_internal(host, config, team, true).await
}

async fn prepare_internal(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    allow_ui_busy: bool,
) -> HostResult<Value> {
    let diagnostics = std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some();
    let started_at = std::time::Instant::now();
    let team = team.to_owned();
    let name = config
        .names
        .get(&team)
        .cloned()
        .unwrap_or_else(|| team.clone());
    // 浏览器启动、SSE重连和多个打开的页面可能同时请求同一个办公室。
    // 首个请求需要从事实账本建立投影，后续请求必须等待该结果后读取缓存，
    // 不能并行重复扫描全部Session；不同办公室仍可并行。
    let projection_lock = {
        let mut locks = host
            .inner
            .office_projection_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks
            .entry(team.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _projection_guard = projection_lock.lock().await;
    let employees:Vec<_>=crate::agents::ROLES.iter().map(|role|json!({"id":role.employee,"name":role.name,"sceneActorId":role.actor,"role":role.duty,"online":crate::agents::enabled(role,&host.inner.config)})).collect();
    let diagnostics_team = team.clone();
    let ui_busy = host.inner.office_ui_busy.clone();
    let terminal_runs = host.inner.office_terminal_runs.clone();
    let handoff_queue = host.inner.office_handoff_queue.clone();
    let result = host.inner.store.call(move|db|{
        let diagnostics = std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some();
        let fact_started = std::time::Instant::now();
        let source_revision = db.revision()?;
        // 用上一次成功投影的事实版本区分“本轮新完成的Run”和重启后
        // 已经展示过的旧终态。不能用本次扫描的source_revision比较：
        // 当前扫描本来就包含刚完成Run的revision，会把新交接误判成历史，
        // 从而一次性公开所有人物任务，人物动画尚未到位时任务就显示完成。
        let previous_source_revision = db.office_projection_source.get(&team).copied();
        let handoff_due = db
            .office_projection_handoff_until
            .get(&team)
            .is_some_and(|until| *until <= chrono::Utc::now());
        if diagnostics {
            eprintln!(
                "engine-host diagnostics projection-source team={} current={} cached={:?} handoff={}",
                team,
                source_revision,
                db.office_projection_source.get(&team),
                handoff_due
            );
        }
        // 交接窗口只改变“哪些已确认事实可以展示”，不改变Run事实。首次
        // 扫描时已把后续交接快照预计算到内存队列；到期后直接推进一个快照，
        // 避免人物点击与全量账本扫描竞争同一Store队列。
        if handoff_due
            && db.office_projection_source.get(&team).copied() == Some(source_revision)
        {
            let queued = handoff_queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&team)
                .and_then(|items| items.first().cloned());
            if let Some(next) = queued {
                let mut remove = false;
                let mut snapshot = next;
                let has_more = handoff_queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&team)
                    .is_some_and(|items| items.len() > 1);
                let prior_state = db
                    .office_projection(&team)?
                    .map(|(_, state)| state)
                    .unwrap_or_else(|| "pending".into());
                let seq = db
                    .office_projection(&team)?
                    .and_then(|(value, _)| value["seq"].as_u64())
                    .ok_or_else(|| HostError::new("sequence_conflict", "交接快照缺少序号"))?;
                let next_seq = seq
                    .checked_add(1)
                    .filter(|n| *n < 9007199254740991)
                    .ok_or_else(|| HostError::new("sequence_conflict", "Office序号已达安全整数上限"))?;
                snapshot["seq"] = json!(next_seq);
                db.save_office_projection(&team, &snapshot, &prior_state)?;
                if has_more {
                    db.office_projection_handoff_until.insert(
                        team.clone(),
                        chrono::Utc::now() + chrono::Duration::milliseconds(2200),
                    );
                } else {
                    remove = true;
                    db.office_projection_handoff_until.remove(&team);
                }
                {
                    let mut queue = handoff_queue
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some(items) = queue.get_mut(&team) {
                        items.remove(0);
                        if remove {
                            queue.remove(&team);
                        }
                    }
                }
                db.office_projection_source.insert(team.clone(), source_revision);
                return Ok(snapshot);
            }
        }
        if db.office_projection_source.get(&team).copied() == Some(source_revision)
            && !handoff_due
        {
            if let Some((snapshot, _)) = db.office_projection(&team)? {
                if snapshot_fits_office_message(&team, &snapshot) {
                    return Ok(snapshot);
                }
            }
        }
        let name = db.office_creations()?.iter().find(|v|v["slot_id"]==team)
            .and_then(|v|v["name"].as_str()).map(str::to_owned).unwrap_or(name);
        let previous=db.office_projection(&team)?;
        let prior=previous.as_ref().map(|(v,_)|v);
        let bindings:Vec<_>=db.bindings()?.into_iter().filter(|b|b.scope.namespace=="office"&&b.scope.slot_id==team).collect();
        // 终态Run的账本和工具事实已封存，可在进程内复用完整视图；活动Run
        // 每次仍从Store重建，避免把正在变化的任务投影为旧状态。
        let read_run = |id: &str| -> HostResult<crate::RunView> {
            if let Some(cached) = terminal_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(id)
                .cloned()
            {
                return Ok(cached);
            }
            let loaded = db.get_run_projection(id)?;
            if loaded.state.is_terminal() {
                terminal_runs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.to_owned(), loaded.clone());
            }
            Ok(loaded)
        };
        let now=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let mut conversations=Vec::new();
        let mut priorities=BTreeMap::new();
        let mut terminal_conversations=std::collections::BTreeSet::new();
        // 有真实业务计划、委派或工具事实的会话必须优先保留。普通追问没有
        // 可展示的白板事实，容量不足时可以先移除其快照入口；原始会话账本
        // 仍由 detail-v1/JSONL 提供，不因投影裁剪而删除。
        let mut business_conversations = std::collections::BTreeSet::new();
        // 当前业务会话及仍在运行的会话必须保留；旧终态会话的交接
        // 只属于历史展示，容量不足时可从目录移除，完整事实仍在账本中。
        let mut protected_conversations=std::collections::BTreeSet::new();
        let mut active_conversations=std::collections::BTreeSet::new();
        let mut handoff_pending_any = false;
        let mut handoff_items: Vec<(String, Value, Value)> = Vec::new();
        for binding in bindings {
            if !allow_ui_busy && ui_busy.load(std::sync::atomic::Ordering::Acquire) != 0 {
                return Err(HostError::new("projection_deferred", "Office UI动作优先，延后投影扫描"));
            }
            let mut runs=Vec::new();
            // 每个办公室绑定只需要自己的Session。直接复用Store的按Session
            // 只读查询，避免历史会话增多后对每个绑定重复扫描全局Run集合，
            // 也避免把一个办公室的投影延迟放大成 O(绑定数×全局Run数)。
            let session_runs = db.runs_for_session(&binding.session_id)?;
            for r in session_runs.iter() {
                if !allow_ui_busy && ui_busy.load(std::sync::atomic::Ordering::Acquire) != 0 {
                    return Err(HostError::new("projection_deferred", "Office UI动作优先，延后投影扫描"));
                }
                let (offset,input)=db.run_input(r)?;
                if !input.is_empty(){runs.push((offset,r,input));}
            }
            runs.sort_by_key(|(offset,_,_)|*offset);
            let Some((_,latest,_))=runs.last() else {continue;};
            let mut has_business = false;
            for (_, run, _) in &runs {
                if db.progress(&run.run_id)?.is_some()
                    || !db.delegations(&run.run_id)?.is_empty()
                    || run.tools.iter().any(|tool| tool.tool_name != "report_progress")
                {
                    has_business = true;
                    break;
                }
            }
            if has_business {
                business_conversations.insert(binding.scope.conversation_id.clone());
            }
            let title=dto::bounded(runs[0].2.trim(),40);
            let prior_conv=prior.and_then(|v|v["conversations"].as_array()).and_then(|a|a.iter().find(|c|c["id"]==binding.scope.conversation_id));
            let mut old=BTreeMap::new();
            if let Some(a)=prior_conv.and_then(|v|v["tasks"].as_array()){for t in a {if let Some(id)=t["id"].as_str(){old.insert(id.to_owned(),t.clone());}}}
            // 当前轮次和至多两组近期真实业务保留独立身份，普通追问不擦除团队成果。
            // 不复制历史任务库：每次从原Run/计划/委派重建，容量不足仍按原预算裁剪。
            let mut display_ids=vec![latest.run_id.clone()];
            // 旧会话的完整事实仍在JSONL/SQLite中；Office当前投影只需要
            // 最近窗口来保留普通追问前的业务成果。对全部历史Run逐一查
            // progress/delegations会随办公室会话数增长成近似O(n²)，拖慢刷新
            // 和人物详情。最新Run已单独加入display_ids，活动Run也不会从
            // 该窗口遗漏；更早历史通过detail-v1读取，不在快照中伪造删除。
            for (_,candidate,_) in runs.iter().rev().take(RECENT_BUSINESS_CANDIDATES) {
                if display_ids.len()>=3 {break;}
                if candidate.run_id==latest.run_id {continue;}
                if db.progress(&candidate.run_id)?.is_some() || !db.delegations(&candidate.run_id)?.is_empty() {
                    display_ids.push(candidate.run_id.clone());
                }
            }
            let latest_has_work=db.progress(&latest.run_id)?.is_some() || !db.delegations(&latest.run_id)?.is_empty() || latest.tools.iter().any(|t|t.tool_name!="report_progress");
            // 新业务事实出现后才接管白板；普通追问等待Provider时也保留原业务成果。
            // 当前Run状态仍由独立任务卡展示，无旧业务时自然回退到当前目标。
            let board_id=if latest_has_work {&display_ids[0]} else {display_ids.get(1).unwrap_or(&display_ids[0])};
            if !latest.state.is_terminal() {
                active_conversations.insert(binding.scope.conversation_id.clone());
            }
            let mut retained_tasks=Vec::new();
            let mut retained_events=Vec::new();
            let mut retained_whiteboard=Value::Null;
            for run_id in &display_ids {
            let r=read_run(run_id)?;
            let (_,input)=db.run_input(&r)?;
            let main_id=format!("run:{}",r.run_id);
            let plan=db.progress(&r.run_id)?;
            let (steps,mut percent)=crate::task_progress::project(plan.clone(),&r)?;
            let delegations=db.delegations(&r.run_id)?;
            let mut assignments=db.office_task_assignments(&r.run_id)?;
            // 开始时多个连续委派可能暂挂同一步；主角色后来用准确证据核验不同
            // 业务步骤时，展示引用采用唯一明确证据，不能再生一张重复委派卡。
            // 原Store开始关联不改，assign事件ID/首次时间不改，细化taskId不重播拜访。
            for d in &delegations {
                let confirmed:Vec<_>=plan.as_ref().and_then(|p|p["steps"].as_array()).into_iter().flatten().enumerate().filter(|(_,step)|step["state"]=="completed"&&step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&d.call_id))).map(|(index,_)|index).collect();
                if let [index]=confirmed.as_slice() {assignments.insert(d.call_id.clone(),*index);}
                else if confirmed.len()>1 {assignments.remove(&d.call_id);}
            }
            let mut recovered=std::collections::BTreeSet::new();
            // 只合并被主角色用准确成功证据替代的失败尝试；相似标题不构成重试关系。
            // 同角色、同父、同步骤缺一不可，旧Run/JSONL/派发事件仍保留。
            for d in &delegations {
                let Some(index)=assignments.get(&d.call_id).copied() else {continue;};
                let Some(step)=plan.as_ref().and_then(|p|p["steps"].get(index)) else {continue;};
                if step["state"]!="completed" {continue;}
                let failed=match read_run(&d.child_run) {Ok(c)=>c.state.is_terminal()&&c.state!=RunState::Completed,Err(e) if e.kind=="not_found"=>false,Err(e)=>return Err(e)};
                if !failed {continue;}
                for replacement in &delegations {
                    if replacement.role_id!=d.role_id || assignments.get(&replacement.call_id)!=Some(&index) || !step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&replacement.call_id)) {continue;}
                    if read_run(&replacement.child_run)?.state==RunState::Completed {
                        recovered.insert(d.call_id.clone());break;
                    }
                }
            }
            // 一个业务步骤可以容纳多人协作。只有一对一的真实对应才替换原卡片；
            // 不能让第一位同事继承整组进度，或因遍历顺序吞掉另一人的独立任务。
            let mut assignment_counts=BTreeMap::new();
            for d in delegations.iter().filter(|d|!recovered.contains(&d.call_id)) {
                if let Some(index)=assignments.get(&d.call_id) {*assignment_counts.entry(*index).or_insert(0usize)+=1;}
            }
            let mut child_steps=BTreeMap::new();
            let mut child_tasks=Vec::new();
            let mut child_events=Vec::new();
            let mut completed_children=0u64;
            // 重试记录仍保留在账本和动态事件中，但一个Office人物只能有一张
            // 当前任务卡。优先展示该角色最近的活动或成功尝试，全部失败时
            // 才展示最近一次失败，避免一个人物被投影成多个Agent。
            let mut display_calls=BTreeSet::new();
            for role_id in delegations.iter().map(|d|d.role_id.as_str()).collect::<BTreeSet<_>>() {
                let mut chosen=None;
                for d in delegations.iter().rev().filter(|d|d.role_id==role_id) {
                    let state=match read_run(&d.child_run) {
                        Ok(child)=>child.state,
                        Err(e) if e.kind=="not_found"=>continue,
                        Err(e)=>return Err(e),
                    };
                    if state==RunState::Completed || !state.is_terminal() { chosen=Some(d.call_id.clone()); break; }
                    chosen.get_or_insert_with(||d.call_id.clone());
                }
                if let Some(call_id)=chosen { display_calls.insert(call_id); }
            }
            // Office 的 visit 动画是一次一个动作。若同一快照同时暴露多个
            // task.assigned，前端会在忙状态下拒绝后续动作，却已经把后续任务
            // 显示出来。按真实投影版本逐个放行：上一交接事件至少已经在一个
            // 快照周期中被客户端看到并经过动画时长后，才公开下一张任务卡。
            // 这里不猜测业务完成，也不增加协议字段；事件仍使用官方既有字段。
            let handoff_gap = chrono::Duration::milliseconds(2200);
            let mut handoff_ready = BTreeSet::new();
            // A completed conversation may be projected again after another
            // run changes the office revision.  The envelope clipping can
            // remove old assignment events, but completed child Runs remain
            // authoritative.  Do not replay a stale handoff window and make
            // already completed cards regress to blocked.
            // 重启后已有快照中的交接事件已经被客户端消费过，不能因为内存
            // 中的 source revision 尚未预热就再次排队。只有发现新的委派没有
            // 对应既有 assignment 事件时，才重新进入逐人展示窗口；终态失败
            // 也保留原事实，不把旧失败伪装成新的动画。
            // 历史终态会话可能已经被容量裁剪掉部分 assignment 事件。它们的
            // Run 事实没有变化，不能在任何新Run触发投影时再次排队，否则旧交接
            // 变体会把快照推过64KiB并阻塞整个办公室。只有 revision 晚于最近
            // 一次成功投影源的终态Run才允许走本轮逐人动画；失败子Run仍按真实
            // 失败状态展示，不因此获得“完成”结论。
            // 当前Store版本是在本次扫描开始时取得的。终态Run若不晚于该
            // 版本，说明它在本次投影前已经存在；重启时即使内存中没有
            // previous_source_revision，也不能把它当成新交接重新播放。
            let historical_terminal = is_historical_terminal(r.revision, previous_source_revision);
            let resume_completed_handoff = r.state.is_terminal() && historical_terminal;
            if resume_completed_handoff {
                handoff_ready.extend(display_calls.iter().cloned());
            } else {
                let mut chain_open = true;
                for d in delegations.iter().filter(|d| display_calls.contains(&d.call_id)) {
                    if !chain_open {
                        break;
                    }
                    let ready = if handoff_ready.is_empty() {
                        true
                    } else {
                        let previous = delegations.iter().rfind(|candidate| {
                            display_calls.contains(&candidate.call_id)
                                && handoff_ready.contains(&candidate.call_id)
                        });
                        previous
                            .map(|candidate| {
                                prior_conv
                                    .and_then(|v| v["events"].as_array())
                                    .and_then(|events| {
                                        events.iter().find(|event| {
                                            event["id"] == format!("assign:{}", candidate.task_id)
                                        })
                                    })
                                    .and_then(|event| event["occurredAt"].as_str())
                                    .and_then(|at| {
                                        chrono::DateTime::parse_from_rfc3339(at).ok()
                                    })
                                    .map(|at| {
                                        chrono::Utc::now()
                                            - at.with_timezone(&chrono::Utc)
                                            >= handoff_gap
                                    })
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    };
                    if ready {
                        handoff_ready.insert(d.call_id.clone());
                    } else {
                        chain_open = false;
                    }
                }
            }
            for d in &delegations {
                let role=crate::agents::role(&d.role_id).ok_or_else(||HostError::new("storage_unavailable","委派引用未知角色"))?;
                let show_card=display_calls.contains(&d.call_id);
                let child=match read_run(&d.child_run) {Ok(v)=>Some(v),Err(e) if e.kind=="not_found"=>None,Err(e)=>return Err(e)};
                let handoff_visible = handoff_ready.contains(&d.call_id);
                let handoff_hidden = show_card && !handoff_visible;
                if handoff_hidden {
                    // 保留上一版本已经发出的事件，避免等待动画时历史闪退；
                    // 但不再次公开任务卡或执行状态，直到前一交接完成展示窗口。
                    if let Some(events) = prior_conv.and_then(|v| v["events"].as_array()) {
                        child_events.extend(events.iter().filter(|event| {
                            event["taskId"] == d.task_id
                        }).cloned());
                    }
                }
                let state=child.as_ref().map(|c| if !c.state.is_terminal() && (c.state==RunState::Cancelling || c.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource"))) {"blocked"}else{status(c.state)}).unwrap_or(if r.state.is_terminal(){"failed"}else{"queued"});
                let superseded=recovered.contains(&d.call_id);
                let done=state=="completed";
                let verified=done && plan.as_ref().map(|p|p["steps"].as_array().into_iter().flatten().any(|s|
                    s["state"]=="completed" && s["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&d.call_id))
                )).unwrap_or(r.state==RunState::Completed);
                // 子Run可能已经完成，但在Office交接窗口中尚未公开给浏览器。
                // 主卡进度必须按“已展示且已完成”的成员计算；否则所有子Run
                // 会在第一张人物卡出现时把主卡直接推到83%，与人物动画和
                // 白板步骤不一致。最终一项交接公开后由delivered补齐100%。
                if show_card && handoff_visible {
                    completed_children += u64::from(done);
                }
                let summary=if let Some(c)=&child {
                    if done {dto::bounded(&c.text,180)} else if c.state.is_terminal() {"子任务未完成，已保存真实结果".into()}
                    else if state=="blocked" {"等待确认、资源或清理".into()} else {"正在独立处理此任务".into()}
                } else if r.state.is_terminal() {"委派中断，未接受执行；不会自动重放".into()} else {"已登记委派，等待接受执行".into()};
                let mut item=task(d.task_id.clone(),d.title.clone(),if done&&!verified{"blocked"}else{state},if done&&!verified{"独立执行已结束，结果待主角色核验".into()}else{summary},&old,&now,if done{100}else{0});
                item["assigneeId"]=json!(role.employee);
                // 开始关联只是待核验提示；只有确认后的一对一证据才复用业务卡片。
                let linked=assignments.get(&d.call_id).copied().filter(|i| {
                    *i<steps.len() && assignment_counts.get(i)==Some(&1) &&
                    (superseded || !child_steps.contains_key(i) && plan.as_ref().is_some_and(|p|
                        p["steps"][*i]["state"]=="completed" && p["steps"][*i]["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&d.call_id))))
                });
                if let Some(index)=linked {
                    let step=&steps[index];
                    item["id"]=step["id"].clone();item["title"]=step["title"].clone();
                }
                let task_id=item["id"].clone();
                if let Some(c)=&child {
                    // 生命周期动态复用原完整快照保存首次观察时间，不伪造错过的开始时刻。
                    // 业务核验与子Run结束分开；只有assignment能驱动交接动画。
                    let phases=[
                        ("started","task.started",c.state==RunState::Running,"观察到独立执行"),
                        ("blocked","task.blocked",!c.state.is_terminal()&&state=="blocked","曾观察到等待确认、资源或清理"),
                        ("completed","task.completed",!superseded&&item["status"]=="completed","已核验完成"),
                        ("result","note",done&&item["status"]!="completed","独立执行已结束；业务步骤须由主角色核验"),
                        ("incomplete","note",c.state.is_terminal()&&!done,"此尝试未完成；真实结果保留在成员历史"),
                    ];
                    for (phase,kind,observed,label) in phases {
                        let id=format!("activity:{}:{phase}",d.task_id);
                        let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|events|events.iter().find(|event|event["id"]==id));
                        if !observed&&existing.is_none() {continue;}
                        if !show_card || handoff_visible || existing.is_some() {
                            child_events.push(json!({"id":id,"kind":kind,"summary":dto::bounded(&format!("{label}：{}",d.title),300),"occurredAt":existing.map(|event|event["occurredAt"].clone()).unwrap_or_else(||json!(now)),"taskId":task_id,"employeeId":role.employee}));
                        }
                    }
                }
                if show_card && !superseded && handoff_visible {
                    if let Some(index)=linked {child_steps.insert(index,item.clone());} else {child_tasks.push(item.clone());}
                }
                if child.is_some() {
                let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|events|events.iter().find(|event|event["id"]==format!("assign:{}",d.task_id)));
                let mut assignment=existing.cloned().unwrap_or_else(||json!({"id":format!("assign:{}",d.task_id),"kind":"task.assigned","summary":dto::bounded(&format!("交给{}：{}",role.name,d.title),300),"occurredAt":now,"taskId":task_id,"employeeId":role.employee,"fromEmployeeId":"office-lead"}));
                // 精确业务证据后来把委派卡归并为唯一业务步骤时，事件仍是
                // 同一次交接，但必须指向当前稳定任务ID，不能挂回临时卡。
                // 事件ID和首次时间保持不变，因此不会重播拜访动画。
                assignment["taskId"]=task_id;
                if handoff_hidden && !superseded {
                    // 记录完整的下一步任务与交接事件。它们仍来自真实子Run，
                    // 这里只是延迟公开；后续快照会按角色顺序一次加入一项。
                    handoff_items.push((
                        binding.scope.conversation_id.clone(),
                        item.clone(),
                        assignment.clone(),
                    ));
                } else if !show_card || handoff_visible {
                    child_events.push(assignment);
                }
                }
            }
            if !delegations.is_empty() {
                let required_children=display_calls.len() as u64;
                let handoff_pending=display_calls.iter().any(|call| !handoff_ready.contains(call));
                let delivered=r.state==RunState::Completed && !handoff_pending && completed_children==required_children;
                let child_percent=(completed_children+u64::from(delivered))*100/(required_children+1);
                percent=if steps.is_empty() {child_percent}else{percent.min(child_percent)};
                if handoff_pending { percent=percent.min(99); }
                handoff_pending_any |= handoff_pending;
            }
            let child_blocked=child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="blocked");
            let child_failed=child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="failed");
            if r.state==RunState::Completed && child_blocked {percent=percent.min(99);}
            let handoff_pending = display_calls.iter().any(|call| !handoff_ready.contains(call));
            let summary=match r.state {
                RunState::Cancelled=>"已停止，实际操作已清理".to_owned(),
                RunState::Interrupted=>"处理被中断，已完成事项保留".to_owned(),
                RunState::TimedOut|RunState::Failed=>super::turns::message(&r),
                RunState::Cancelling=>"正在停止，等待实际操作清理".to_owned(),
                RunState::Completed if child_failed=>"回复已结束；仍有未恢复的子任务失败，已完成部分保留".to_owned(),
                RunState::Completed if handoff_pending=>"回复已结束；正在依次展示后续成员交接".to_owned(),
                RunState::Completed if percent<100=>"回复已结束；已完成部分保留，其余待核验或继续处理".to_owned(),
                RunState::Completed=>"本次目标已完成".to_owned(),
                _ if child_blocked=>"子任务等待确认、核验或可用资源".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_approval")=>"等待你确认本次操作".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_resource")=>"等待可用资源".to_owned(),
                _=>"正在处理你的请求".to_owned(),
            };
            let main_state=if r.state==RunState::Completed && child_failed {"failed"} else if r.state==RunState::Completed && (percent<100 || handoff_pending) || !r.state.is_terminal() && (child_blocked || handoff_pending || r.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource")) || r.state==RunState::Cancelling) {"blocked"}else{status(r.state)};
            let mut tasks=vec![task(main_id,dto::bounded(&input,80),main_state,summary.clone(),&old,&now,percent)];
            for (index,step) in steps.into_iter().enumerate() {
                if let Some(child)=child_steps.remove(&index) {tasks.push(child);continue;}
                tasks.push(task(step["id"].as_str().unwrap().into(),step["title"].as_str().unwrap().into(),step["status"].as_str().unwrap(),step["summary"].as_str().unwrap().into(),&old,&now,step["progress"].as_u64().unwrap()));
            }
            tasks.extend(child_tasks);
            let mut events=child_events;
            // 工具仍是执行事实，公开任务只引用稳定的业务步骤；重试不增加卡片或重播交接。
            for t in r.tools.iter().filter(|t|t.tool_name!="report_progress") {
                // 保留原内部调用关联号供历史详情追踪，公开任务不再使用逐call卡片。
                let _call_reference=db.office_call_id(&r.run_id,&t.call_id)?;
                if !delegations.is_empty() {continue;} // 协作交接只来自真实委派，主角色核验工具不重复派发同一步。
                let (who,_)=actor(&t.tool_name);
                if who=="office-lead" || !db.office_call_started(&r.run_id,&t.call_id)? {continue;}
                let current=assignments.get(&t.call_id).map(|i|i+1).filter(|i|*i<tasks.len()).unwrap_or(0);
                let id=tasks[current]["id"].as_str().unwrap().to_owned();
                let event_id=format!("assign:{id}:{who}");
                if events.iter().any(|e:&Value|e["id"]==event_id) {continue;}
                if !r.state.is_terminal() && matches!(t.state.as_str(),"running"|"awaiting_resource") {
                    tasks[current]["assigneeId"]=json!(who);
                }
                let label=match who {"file-agent"=>"文件操作","data-agent"=>"资料查询",_=>"执行操作"};
                let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|a|a.iter().find(|e|e["id"]==event_id));
                events.push(existing.cloned().unwrap_or_else(||json!({"id":event_id,"kind":"task.assigned","summary":format!("开始{label}"),"occurredAt":tasks[current]["updatedAt"],"taskId":id,"employeeId":who,"fromEmployeeId":"office-lead"})));
            }
            // assignee也是任务内容；完成全部映射后再比较，避免同内容每秒改变updatedAt/seq。
            for task in &mut tasks {
                if let Some(prior)=old.get(task["id"].as_str().unwrap()) {
                    let mut comparable=prior.clone();comparable["updatedAt"]=task["updatedAt"].clone();
                    if &comparable==task {task["updatedAt"]=prior["updatedAt"].clone();}
                }
            }
            events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            let selected=&tasks[0];
            let board_steps:Vec<_>=if tasks.len()>1 {
                // 白板最多三个步骤（Office 官方 schema 限制）。优先保留当前步骤
                // 及其前一个已完成步骤，全部结束时保留最后三个结果；不能因为
                // 新步骤出现就把刚完成的成果全部挤掉。
                let relative=tasks[1..].iter().position(|t|t["status"]!="completed");
                let first=relative.map(|index| index.saturating_sub(1)).unwrap_or(tasks.len().saturating_sub(4));
                tasks[1..].iter().skip(first).take(3).map(|t| {
                    let label=match t["status"].as_str(){Some("completed")=>"已完成",Some("running")=>"处理中",Some("blocked")=>"等待确认",Some("failed")=>"未完成",_=>"待处理"};
                    json!({"id":t["id"],"title":dto::bounded(&format!("{} · {label}",dto::bounded(t["title"].as_str().unwrap(),24)),40),"progress":t["progress"]})
                }).collect()
            } else {vec![json!({"id":"goal","title":dto::bounded(&summary,80),"progress":percent})]};
            let completed=tasks.iter().skip(1).filter(|t|t["status"]=="completed").count();
            let description=if !delegations.is_empty() && handoff_pending {
                format!("已确认完成 {completed_children} 项；{summary}")
            } else if tasks.len()>1 {
                format!("已确认完成 {completed}/{} 步；{summary}",tasks.len()-1)
            } else {summary};
            let whiteboard=json!({"template":"task-board-v1","title":dto::bounded(selected["title"].as_str().unwrap_or(""),32),"status":selected["status"],"tagline":"","description":dto::bounded(&description,500),"note":"","link":{"label":"","url":""},"steps":board_steps});
            if run_id==board_id {retained_whiteboard=whiteboard;}
            retained_tasks.extend(tasks);
            retained_events.extend(events);
            }
            retained_events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            priorities.insert(binding.scope.conversation_id.clone(),latest.revision);
            if latest.state.is_terminal() {terminal_conversations.insert(binding.scope.conversation_id.clone());}
            // 常驻人物和成员来自同一可信目录，未启用角色也能说明真实状态。
            conversations.push(json!({"id":binding.scope.conversation_id,"title":title,
                "employees":employees,
                "tasks":retained_tasks,"events":retained_events,"whiteboard":retained_whiteboard}));
        }
        if diagnostics {
            eprintln!(
                "engine-host diagnostics projection team={} facts_ms={}",
                team,
                fact_started.elapsed().as_millis()
            );
        }
        // 保护最近的业务会话，使天气/多Agent等真实历史在页面刷新后仍有
        // 可点击入口；超过窗口的旧业务会话只在投影中裁剪，不删除账本事实。
        let mut recent_business: Vec<_> = business_conversations
            .iter()
            .filter_map(|conversation| {
                priorities
                    .get(conversation)
                    .copied()
                    .map(|revision| (conversation.clone(), revision))
            })
            .collect();
        recent_business.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        protected_conversations.extend(
            recent_business
                .into_iter()
                .take(MAX_PROTECTED_BUSINESS_CONVERSATIONS)
                .map(|(conversation, _)| conversation),
        );
        // 活动会话必须保留；终态只保留全办公室最新的一项作为当前入口，
        // 其余历史在容量不足时可裁剪，详情仍从会话账本读取。
        protected_conversations.extend(active_conversations);
        if let Some((conversation, _)) = priorities.iter().max_by_key(|(_, revision)| *revision) {
            protected_conversations.insert(conversation.clone());
        }
        conversations.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
        let mut snapshot=json!({"schemaVersion":"1.0","seq":prior.map(|v|v["seq"].clone()).unwrap_or(json!(1)),"workspace":{"id":team,"name":name},"conversations":conversations,"system":{"cpuPercent":-1,"memoryPercent":-1,"networkKbps":-1}});
        let handoff_conversations: BTreeSet<String> = handoff_items
            .iter()
            .map(|(conversation, _, _)| conversation.clone())
            .collect();
        // 整办公室64KiB预算；按事实版本先缩旧终态详情，保留活动业务卡及全部成员。
        while serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot})).map_err(HostError::storage)?.len()>OFFICE_MESSAGE_LIMIT {
            let convs=snapshot["conversations"].as_array_mut().unwrap();
            let candidate=convs.iter().enumerate().filter(|(_,c)|c["tasks"].as_array().is_some_and(|a|!a.is_empty())&&terminal_conversations.contains(c["id"].as_str().unwrap_or("") ) && !handoff_conversations.contains(c["id"].as_str().unwrap_or("")) && !business_conversations.contains(c["id"].as_str().unwrap_or(""))).min_by_key(|(_,c)|priorities.get(c["id"].as_str().unwrap_or("")));
            let Some((i,_))=candidate else {
                // 旧会话可能已经没有任务卡，但成员目录和标题仍使完整目录
                // 超过协议预算。先压缩最旧终态会话的可选投影字段，再移除
                // 空的历史入口；原始会话和完整成员事实仍可从 detail-v1/账本
                // 查询，当前会话与交接会话永远不在此候选中。
                let fallback=convs.iter().enumerate()
                    .filter(|(_,c)| terminal_conversations.contains(c["id"].as_str().unwrap_or("") )
                        && !protected_conversations.contains(c["id"].as_str().unwrap_or(""))
                        && !business_conversations.contains(c["id"].as_str().unwrap_or("")))
                    .min_by_key(|(_,c)| priorities.get(c["id"].as_str().unwrap_or("")));
                let fallback = fallback.or_else(|| convs.iter().enumerate()
                    .filter(|(_,c)| terminal_conversations.contains(c["id"].as_str().unwrap_or(""))
                        && !protected_conversations.contains(c["id"].as_str().unwrap_or(""))
                        && business_conversations.contains(c["id"].as_str().unwrap_or(""))
                        && !handoff_conversations.contains(c["id"].as_str().unwrap_or("")))
                    .min_by_key(|(_,c)| priorities.get(c["id"].as_str().unwrap_or(""))));
                let Some((index,_))=fallback else {
                    if diagnostics {
                        eprintln!("engine-host diagnostics projection clip exhausted team={} conversations={} terminal={} handoff={}", team, convs.len(), terminal_conversations.len(), handoff_conversations.len());
                    }
                    return Err(HostError::new("protocol_error","Office完整会话目录超限"));
                };
                let c=&mut convs[index];
                if business_conversations.contains(c["id"].as_str().unwrap_or("")) {
                    if compact_business_conversation(c) {
                        continue;
                    }
                    // 已经压缩到最小展示形态的旧业务会话不能继续阻塞
                    // Office 64 KiB 封套。它的完整任务、白板和工具事实仍
                    // 在账本中，且最近窗口已由 protected_conversations 保留。
                    convs.remove(index);
                    continue;
                }
                if !c["whiteboard"].is_null() {
                    c["whiteboard"] = Value::Null;
                    continue;
                }
                if c["tasks"].as_array().is_some_and(|tasks| !tasks.is_empty())
                    || c["events"].as_array().is_some_and(|events| !events.is_empty()) {
                    c["tasks"] = json!([]);
                    c["events"] = json!([]);
                    c["title"] = json!(dto::bounded(c["title"].as_str().unwrap_or(""), 24));
                    continue;
                }
                // 人物身份是点击详情的作用域依据，优先保留数组和ID；只
                // 去掉旧终态职责文本，仍超限时才允许移除整个历史成员目录。
                if c["employees"].as_array().is_some_and(|employees| {
                    employees.iter().any(|employee| employee.get("role").is_some())
                }) {
                    if let Some(employees) = c["employees"].as_array_mut() {
                        for employee in employees {
                            if let Some(object) = employee.as_object_mut() {
                                object.remove("role");
                            }
                        }
                    }
                    continue;
                }
                if c["employees"].as_array().is_some_and(|employees| !employees.is_empty()) {
                    c["employees"] = json!([]);
                    continue;
                }
                convs.remove(index);
                continue;
            };
            let c=&mut convs[i];
            // 会话ID的字典序不代表新旧；审批中的步骤不能被旧历史挤掉。
            if c["tasks"].as_array().is_some_and(|a|a.len()>1){
                let popped=c["tasks"].as_array_mut().unwrap().pop().unwrap();
                c["events"].as_array_mut().unwrap().retain(|e|e["taskId"]!=popped["id"]);
                continue;
            }
            c["tasks"]=json!([]);c["events"]=json!([]);
            // 成员身份是人物点击和侧栏的范围依据，不能随旧任务展示预算一起删除。
            // 可选职责/白板按上游Schema省略；详情仍从可信目录及原账本读取完整信息。
            c.as_object_mut().unwrap().remove("whiteboard");
            for member in c["employees"].as_array_mut().unwrap() {
                member.as_object_mut().unwrap().remove("role");
            }
            c["title"]=json!(dto::bounded(c["title"].as_str().unwrap_or(""),24));
        }
        // 交接变体会在当前快照上逐项加入同事任务和 assignment 事件。普通
        // 目录只需满足当前 envelope；交接目录还必须为“最终一项”预留空间，
        // 否则第一个人物能显示而后续人物会因为一次超限被整批丢弃。
        // 只在确实存在交接且预览变体超限时进行额外整理，避免改变普通历史
        // 目录的保留策略。详情查询仍从Run事实读取被压缩的旧历史。
        if !handoff_items.is_empty() {
            let mut preview = snapshot.clone();
            for (conversation, item, event) in &handoff_items {
                if let Some(conv) = preview["conversations"]
                    .as_array_mut()
                    .and_then(|all| all.iter_mut().find(|c| c["id"] == *conversation))
                {
                    apply_handoff_item(conv, item, event, false, 0);
                }
            }
            while !snapshot_fits_office_message(&team, &preview) {
                let mut changed = false;
                // 先删除旧终态会话中的详情，保留成员身份和会话入口；这比
                // 删除当前会话的任务卡安全，也使人物点击仍能通过作用域校验。
                let conversation_view = snapshot["conversations"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if let Some((index, _)) = conversation_view
                    .iter()
                    .enumerate()
                    .filter(|(_, conversation)| {
                        terminal_conversations.contains(
                            conversation["id"].as_str().unwrap_or_default(),
                        ) && !handoff_conversations
                            .contains(conversation["id"].as_str().unwrap_or_default())
                            && !business_conversations
                                .contains(conversation["id"].as_str().unwrap_or_default())
                            && (conversation["tasks"].as_array().is_some_and(|tasks| !tasks.is_empty())
                                || conversation["events"].as_array().is_some_and(|events| !events.is_empty())
                                || !conversation["whiteboard"].is_null())
                    })
                    .min_by_key(|(_, conversation)| {
                        priorities
                            .get(conversation["id"].as_str().unwrap_or_default())
                            .copied()
                            .unwrap_or_default()
                    })
                {
                    let conversation = &mut snapshot["conversations"]
                        .as_array_mut()
                        .expect("conversation array")[index];
                    conversation["tasks"] = json!([]);
                    conversation["events"] = json!([]);
                    conversation["whiteboard"] = Value::Null;
                    changed = true;
                }
                if !changed {
                    // 旧终态会话的可选详情已经清空仍然超限时，移除最旧的
                    // 会话对象。当前会话和最近交接会话永远受保护；完整历史
                    // 仍可从 detail-v1 查询，不能把预算错误转化为投影失败。
                    let conversation_view = snapshot["conversations"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if let Some(index) = conversation_view
                        .iter()
                        .enumerate()
                        .filter(|(_, conversation)| {
                            terminal_conversations.contains(
                                conversation["id"].as_str().unwrap_or_default(),
                            ) && !handoff_conversations
                                .contains(conversation["id"].as_str().unwrap_or_default())
                                && !business_conversations
                                    .contains(conversation["id"].as_str().unwrap_or_default())
                        })
                        .min_by_key(|(_, conversation)| {
                            priorities
                                .get(conversation["id"].as_str().unwrap_or_default())
                                .copied()
                                .unwrap_or_default()
                        })
                        .map(|(index, _)| index)
                    {
                        snapshot["conversations"]
                            .as_array_mut()
                            .expect("conversation array")
                            .remove(index);
                        changed = true;
                    }
                }
                if !changed {
                    let conversation_view = snapshot["conversations"]
                        .as_array_mut()
                        .map(Vec::as_mut_slice)
                        .unwrap_or(&mut []);
                    if let Some(conversation) = conversation_view
                        .iter_mut()
                        .filter(|conversation| {
                            terminal_conversations.contains(
                                conversation["id"].as_str().unwrap_or_default(),
                            ) && !handoff_conversations
                                .contains(conversation["id"].as_str().unwrap_or_default())
                                && !protected_conversations
                                    .contains(conversation["id"].as_str().unwrap_or_default())
                                && business_conversations
                                    .contains(conversation["id"].as_str().unwrap_or_default())
                        })
                        .min_by_key(|conversation| {
                            priorities
                                .get(conversation["id"].as_str().unwrap_or_default())
                                .copied()
                                .unwrap_or_default()
                        })
                    {
                        changed = compact_business_conversation(conversation);
                    }
                }
                if !changed {
                    // 最后仅压缩当前会话的非 assignment 活动事件。交接链所需
                    // 的 assignment 时间戳必须保留，否则下一窗口无法判断动画
                    // 是否已经展示完成。
                    let mut removed_event = false;
                    if let Some(conversations) = snapshot["conversations"].as_array_mut() {
                        for conversation in conversations {
                            if !handoff_conversations
                                .contains(conversation["id"].as_str().unwrap_or_default())
                            {
                                continue;
                            }
                            if let Some(events) = conversation["events"].as_array_mut() {
                                if let Some(index) = events.iter().rposition(|event| {
                                    event["kind"] != "task.assigned"
                                }) {
                                    events.remove(index);
                                    removed_event = true;
                                    break;
                                }
                            }
                        }
                    }
                    if !removed_event {
                        if diagnostics {
                            eprintln!("engine-host diagnostics handoff clip exhausted team={} conversations={} handoff_items={} size={}", team, snapshot["conversations"].as_array().map_or(0, Vec::len), handoff_items.len(), serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":preview})).map_or(0, |v| v.len()));
                        }
                        return Err(HostError::new(
                            "protocol_error",
                            "Office交接快照在保留当前任务后仍超过64KiB",
                        ));
                    }
                }
                preview = snapshot.clone();
                for (conversation, item, event) in &handoff_items {
                    if let Some(conv) = preview["conversations"]
                        .as_array_mut()
                        .and_then(|all| all.iter_mut().find(|c| c["id"] == *conversation))
                    {
                    apply_handoff_item(conv, item, event, false, 0);
                    }
                }
            }
        }
        // 把本次真实扫描中尚未公开的交接项预组装成逐步快照。每个变体
        // 同时推进各会话的一项，保持“先到人物再显示下一任务”，而不让
        // 后续窗口再次读取全部Run/JSONL。
        if !handoff_items.is_empty() {
            let mut by_conversation: BTreeMap<String, Vec<(Value, Value)>> = BTreeMap::new();
            for (conversation, item, event) in handoff_items {
                by_conversation
                    .entry(conversation)
                    .or_default()
                    .push((item, event));
            }
            let mut remaining = by_conversation;
            let mut current = snapshot.clone();
            let mut variants = Vec::new();
            let mut overflowed = false;
            let mut pending_counts: BTreeMap<String, usize> = remaining
                .iter()
                .map(|(conversation, items)| (conversation.clone(), items.len()))
                .collect();
            // 当前快照中已经公开的交接卡加上队列中尚未公开的卡，构成
            // 本轮真实成员总数；变体据此计算主卡进度，不能只按当前可见
            // 子任务数量把2/5误报成2/2。
            let handoff_totals: BTreeMap<String, usize> = remaining
                .iter()
                .map(|(conversation, items)| {
                    let visible = current["conversations"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|c| c["id"] == *conversation)
                        .map(|c| {
                            c["events"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|event| {
                                    event["kind"] == "task.assigned"
                                        && event["id"]
                                            .as_str()
                                            .is_some_and(|id| id.starts_with("assign:delegation:"))
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    (conversation.clone(), visible + items.len())
                })
                .collect();
            loop {
                let mut progressed = false;
                for (conversation, items) in remaining.iter_mut() {
                    let Some((item, event)) = items.first().cloned() else { continue; };
                    let Some(conv) = current["conversations"]
                        .as_array_mut()
                        .and_then(|all| all.iter_mut().find(|c| c["id"] == *conversation))
                    else { continue; };
                    let final_item = pending_counts.get(conversation).copied() == Some(1);
                    apply_handoff_item(
                        conv,
                        &item,
                        &event,
                        final_item,
                        handoff_totals.get(conversation).copied().unwrap_or(0),
                    );
                    items.remove(0);
                    if let Some(count) = pending_counts.get_mut(conversation) {
                        *count = count.saturating_sub(1);
                    }
                    progressed = true;
                }
                if !progressed {
                    break;
                }
                if !snapshot_fits_office_message(&team, &current) {
                    // The base projection has already been clipped to the
                    // Office envelope. A handoff event can add enough detail
                    // to exceed it; keep the bounded base and do not enqueue
                    // an invalid variant that would halt syncing.
                    overflowed = true;
                    break;
                }
                variants.push(current.clone());
                if remaining.values().all(Vec::is_empty) { break; }
            }
            if overflowed || variants.is_empty() {
                handoff_pending_any = false;
                handoff_queue.lock().unwrap_or_else(|e| e.into_inner()).remove(&team);
            } else {
                handoff_queue.lock().unwrap_or_else(|e| e.into_inner()).insert(team.clone(), variants);
            }
        } else {
            handoff_queue.lock().unwrap_or_else(|e| e.into_inner()).remove(&team);
        }
        let update_handoff_window = |db: &mut crate::store::Db| {
            let now = chrono::Utc::now();
            if handoff_pending_any {
                if db
                    .office_projection_handoff_until
                    .get(&team)
                    .is_none_or(|until| *until <= now)
                {
                    db.office_projection_handoff_until
                        .insert(team.clone(), now + chrono::Duration::milliseconds(2200));
                }
            } else {
                db.office_projection_handoff_until.remove(&team);
            }
        };
        if prior==Some(&snapshot) {
            update_handoff_window(db);
            db.office_projection_source.insert(team.clone(), source_revision);
            return Ok(snapshot);
        }
        if let Some(prior)=prior {
            let next=prior["seq"].as_u64().filter(|n|*n<9007199254740991).ok_or_else(||HostError::new("sequence_conflict","Office序号已达安全整数上限"))?+1;
            snapshot["seq"]=json!(next);
        }
        let state=if previous.as_ref().is_some_and(|(_,s)|s=="halted"){ "halted" }else{"pending"};
        db.save_office_projection(&team,&snapshot,state)?;
        update_handoff_window(db);
        db.office_projection_source.insert(team, source_revision);
        Ok(snapshot)
    }).await;
    if diagnostics {
        eprintln!(
            "engine-host diagnostics projection team={} prepare_ms={}",
            diagnostics_team,
            started_at.elapsed().as_millis()
        );
    }
    result
}

/// 功能：按办公室唯一外发门同步完整快照并核对回执。
/// 参数：host/config/team/client为既有服务，halted为冲突锁存，address为聊天地址。
/// 返回：真实applied、cached、ignored或halted状态。
/// 错误：连接、认证、版本冲突及协议错误，不假定服务端回滚。
/// 状态：先持久完整内容再外发，重复序号保持同内容。
/// 并发与取消：办公室专用门跨HTTP等待，不阻塞共享运行注册表。
/// 权限：只访问已配置team，服务凭据不下发浏览器。
/// 关联：需求/设计REQ-OP-011/013/014；CHG-2026-0006。
pub(super) async fn sync_once(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    client: &Client,
    halted: bool,
    address: std::net::SocketAddr,
) -> HostResult<String> {
    let gate = client.projection_gate(team).await;
    let _guard = gate.lock().await;
    if !super::workspaces::active(host, config, team).await? {
        return Ok("reserved".into());
    }
    let snapshot = prepare(host, config, team).await?;
    // `halted` is an in-process latch for a confirmed external sequence
    // conflict. A persisted halted row may represent a recoverable protocol
    // or cache failure from an older Host, so it must not block restart.
    if halted {
        return Ok("halted".into());
    }
    let caps = client.capabilities().await?;
    let mut actors = vec!["office-lead", "file-agent", "data-agent"];
    if host.inner.config.tools.iter().any(|name| name == "bash") {
        actors.push("code-agent");
    }
    for actor in actors {
        if !caps["scene"]["actors"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["id"] == actor))
        {
            return Err(HostError::new(
                "protocol_error",
                "Office缺少配置映射所需人物",
            ));
        }
    }
    let (_, directory) = client.post(json!({"type":"office.read"})).await?;
    let initialized = directory["offices"]
        .as_array()
        .is_some_and(|a| a.iter().any(|o| o["id"] == team));
    if !initialized {
        client
            .post(json!({"type":"offices.initialize","offices":[snapshot],"chat":{"url":format!("http://{address}/office/chat")}}))
            .await?;
    }
    // chat.url是Office进程内状态；重新注册幂等，Node重启后不依赖浏览器重建。
    if initialized {
        client.post(json!({"type":"offices.initialize","offices":[],"chat":{"url":format!("http://{address}/office/chat")}})).await?;
    }
    let (_, receipt) = client
        .post(json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}))
        .await?;
    let state = receipt["status"].as_str().unwrap_or("pending").to_owned();
    if state == "ignored" && receipt["reasonCode"] == "STALE_SEQ" {
        return Err(HostError::new(
            "sequence_conflict",
            "Office存在较新版本，停止投递以免覆盖外部所有者",
        ));
    }
    let team = team.to_owned();
    let cache_team = team.clone();
    let persisted_snapshot = snapshot.clone();
    let saved_state = state.clone();
    host.inner
        .store
        .call(move |db| {
            if db
                .office_projection(&team)?
                .is_some_and(|(current, _)| current == persisted_snapshot)
            {
                db.save_office_projection(&team, &persisted_snapshot, &saved_state)?;
            }
            Ok(())
        })
        .await?;
    {
        let mut cache = host
            .inner
            .office_projection_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(cache_team, snapshot.clone());
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientScope, HostConfig};

    #[test]
    fn final_handoff_promotes_completed_lead_card() {
        let mut conversation = json!({
            "tasks": [
                {"id":"run:lead","title":"旅游攻略","status":"blocked","progress":83,"summary":"回复已结束；正在依次展示后续成员交接"},
                {"id":"delegation:research","title":"查路线","status":"completed","progress":100,"summary":"路线已完成"}
            ],
            "events": []
        });
        apply_handoff_item(
            &mut conversation,
            &json!({"id":"delegation:food","title":"查美食","status":"completed","progress":100,"summary":"美食已完成"}),
            &json!({"id":"assign:food","kind":"task.assigned"}),
            true,
            2,
        );
        assert_eq!(conversation["tasks"][0]["status"], "completed");
        assert_eq!(conversation["tasks"][0]["progress"], 100);
        assert_eq!(conversation["whiteboard"]["status"], "completed");
        assert_eq!(
            conversation["whiteboard"]["description"],
            "已确认完成 2/2 步；本次目标已完成"
        );
    }

    #[test]
    fn intermediate_handoff_updates_lead_progress_without_finishing_early() {
        let mut conversation = json!({
            "tasks": [
                {"id":"run:lead","title":"旅游攻略","status":"blocked","progress":16,"summary":"回复已结束；正在依次展示后续成员交接"},
                {"id":"delegation:research","title":"查路线","status":"completed","progress":100,"summary":"路线已完成"}
            ],
            "events": []
        });
        apply_handoff_item(
            &mut conversation,
            &json!({"id":"delegation:food","title":"查美食","status":"completed","progress":100,"summary":"美食已完成"}),
            &json!({"id":"assign:food","kind":"task.assigned"}),
            false,
            5,
        );
        assert_eq!(conversation["tasks"][0]["status"], "blocked");
        assert_eq!(conversation["tasks"][0]["progress"], 33);
        assert_eq!(conversation["whiteboard"]["status"], "blocked");
        assert_eq!(
            conversation["whiteboard"]["description"],
            "已确认完成 2 项；回复已结束；正在依次展示后续成员交接"
        );
    }

    #[test]
    fn cached_snapshot_validation_rejects_wrong_scope_and_oversize_envelopes() {
        let mut snapshot = json!({
            "schemaVersion": "1.0",
            "seq": 7,
            "workspace": {"id": "office-1", "name": "办公室"},
            "conversations": [],
            "system": {"cpuPercent": -1, "memoryPercent": -1, "networkKbps": -1}
        });
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        assert!(!snapshot_fits_office_message("office-2", &snapshot));
        snapshot["conversations"] = json!([{"id":"old","title":"x".repeat(65_000)}]);
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        snapshot["schemaVersion"] = json!("legacy");
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
    }

    #[test]
    fn newly_completed_run_keeps_sequential_handoff_window() {
        assert!(!is_historical_terminal(12, None));
        assert!(!is_historical_terminal(12, Some(11)));
        assert!(is_historical_terminal(12, Some(12)));
        assert!(is_historical_terminal(11, Some(12)));
    }

    #[tokio::test]
    async fn consecutive_delegations_use_their_precise_completed_step_without_duplicate_cards() {
        use openpaw_llm::{MockMode, MockProvider};
        // 在真实Provider等待边界冻结运行，避免只测终态而遗漏用户可见的临时重置。
        struct Pausable {
            inner: MockProvider,
            pause: std::sync::atomic::AtomicBool,
            requests: std::sync::atomic::AtomicUsize,
            entered: tokio::sync::Notify,
            release: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for Pausable {
            async fn complete(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.inner.complete(request).await
            }
            async fn stream(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<
                futures::stream::BoxStream<
                    'static,
                    openpaw_core::Result<openpaw_core::StreamEvent>,
                >,
            > {
                let request_index = self
                    .requests
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) || request_index == 7
                {
                    self.entered.notify_one();
                    self.release.acquire().await.unwrap().forget();
                }
                self.inner.stream(request).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.inner.capabilities()
            }
            fn default_model(&self) -> &str {
                self.inner.default_model()
            }
            fn name(&self) -> &str {
                "controlled-projection-wait"
            }
        }
        for (second_evidence, expected_cards) in [("grouped", 4), ("call_2_1", 3), ("call_2_0", 5)]
        {
            let grouped = second_evidence == "grouped";
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("brief.md"), "同一份真实简报").unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let script = vec![
                call(
                    "report_progress",
                    json!({"type":"create","steps":if grouped {json!([{"id":"research","title":"两位同事共同核验目标"}])}else{json!([{"id":"research","title":"李研核验目标"},{"id":"organize","title":"周理整理清单"}])}}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"research","state":"in_progress"}),
                ),
                MockMode::ParallelToolUse {
                    tools: vec![
                        (
                            "delegate_task".into(),
                            json!({"role_id":"research","title":"李研读取简报","instructions":"读取brief.md核验目标","acceptance":"真实读取"}),
                        ),
                        (
                            "delegate_task".into(),
                            json!({"role_id":"organize","title":"周理读取简报","instructions":"读取brief.md整理清单","acceptance":"真实读取"}),
                        ),
                    ],
                    text: String::new(),
                },
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("李研实际读取完成".into()),
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("周理实际读取完成".into()),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"research","state":"completed","evidenceCallIds":if grouped {json!(["call_2_0","call_2_1"])}else{json!(["call_2_0"])}}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":if grouped {"research"}else{"organize"},"state":"completed","evidenceCallIds":if grouped {json!(["call_2_0","call_2_1"])}else{json!([second_evidence])}}),
                ),
                MockMode::Fixed("王明核验两个子任务完成".into()),
                MockMode::Fixed("普通追问已回复".into()),
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"new","title":"独立新业务目标"}]}),
                ),
                MockMode::Fixed("新目标尚未核验".into()),
            ];
            let provider = std::sync::Arc::new(Pausable {
                inner: MockProvider::scripted(script),
                pause: std::sync::atomic::AtomicBool::new(true),
                requests: std::sync::atomic::AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            });
            let host = Host::with_provider(config, provider.clone()).await.unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "consecutive".into(),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            let office = OfficeConfig {
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let run = host
                .submit_run(&scope, &binding.session_id, "核验目标并整理清单".into())
                .await
                .unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider.entered.notified(),
            )
            .await
            .unwrap();
            let initial = prepare(&host, &office, "office-1").await.unwrap();
            assert_eq!(
                initial["conversations"][0]["whiteboard"]["status"],
                "running"
            );
            assert!(initial["conversations"][0]["whiteboard"]["title"]
                .as_str()
                .unwrap()
                .contains("核验目标"));
            provider.release.add_permits(1);
            tokio::time::timeout(Duration::from_secs(10), provider.entered.notified())
                .await
                .unwrap();
            // 交接动画按顺序公开；等待第二个任务在前一事件的动画窗口后出现。
            let awaiting_review = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let candidate = prepare(&host, &office, "office-1").await.unwrap();
                    let tasks = candidate["conversations"][0]["tasks"].as_array().unwrap();
                    if ["code-agent", "file-agent"]
                        .iter()
                        .all(|employee| tasks.iter().any(|t| t["assigneeId"] == *employee))
                    {
                        break candidate;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap();
            let awaiting_tasks = awaiting_review["conversations"][0]["tasks"]
                .as_array()
                .unwrap();
            for employee in ["code-agent", "file-agent"] {
                let own = awaiting_tasks
                    .iter()
                    .find(|t| t["assigneeId"] == employee)
                    .unwrap();
                assert!(own["id"].as_str().unwrap().starts_with("delegation:"));
                assert_eq!(
                    own["status"], "blocked",
                    "子执行结束后等待主角色核验，不能继续显示运行中或伪造已验收"
                );
                assert_eq!(own["progress"], 100);
            }
            assert_eq!(awaiting_tasks[1]["assigneeId"], "office-lead");
            assert_ne!(
                awaiting_tasks[1]["status"], "completed",
                "子执行完成不代替业务验收"
            );
            provider.release.add_permits(1);
            tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            assert_eq!(
                host.get_run(&scope, &run.run_id).await.unwrap().state,
                RunState::Completed
            );

            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            assert_eq!(
                conv["tasks"].as_array().unwrap().len(),
                expected_cards,
                "只有唯一准确证据才合并业务卡；同一call被多个步骤引用不能猜测归属"
            );
            if second_evidence == "call_2_0" {
                assert!(
                    conv["tasks"][0]["progress"].as_u64().unwrap() < 100,
                    "不能用第一人的证据确认尚未核验的另一人"
                );
                assert_eq!(conv["tasks"][0]["status"], "blocked");
            } else {
                assert_eq!(conv["tasks"][0]["progress"], 100);
            }
            if expected_cards != 3 {
                let tasks = conv["tasks"].as_array().unwrap();
                assert_eq!(
                    tasks[1]["assigneeId"], "office-lead",
                    "共同或歧义步骤由主角色负责，不能挂在第一位同事名下"
                );
                for employee in ["code-agent", "file-agent"] {
                    let own: Vec<_> = tasks
                        .iter()
                        .filter(|t| t["assigneeId"] == employee)
                        .collect();
                    assert_eq!(own.len(), 1, "每位同事保留自己的真实任务");
                    assert!(own[0]["id"].as_str().unwrap().starts_with("delegation:"));
                    assert_eq!(
                        own[0]["status"],
                        if second_evidence == "call_2_0" && employee == "file-agent" {
                            "blocked"
                        } else {
                            "completed"
                        }
                    );
                    for event in conv["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|e| e["employeeId"] == employee)
                    {
                        assert_eq!(event["taskId"], own[0]["id"]);
                    }
                }
            }
            if expected_cards == 3 {
                for (index, employee) in ["code-agent", "file-agent"].into_iter().enumerate() {
                    assert_eq!(conv["tasks"][index + 1]["assigneeId"], employee);
                    assert_eq!(conv["tasks"][index + 1]["status"], "completed");
                    for event in conv["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|e| e["employeeId"] == employee)
                    {
                        assert_eq!(event["taskId"], conv["tasks"][index + 1]["id"]);
                    }
                }
            }
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["kind"] == "task.assigned")
                    .count(),
                2
            );
            let run_id = run.run_id.clone();
            host.inner
                .store
                .call(move |db| {
                    let original = db.office_task_assignments(&run_id)?;
                    assert_eq!(original.get("call_2_0"), Some(&0));
                    assert_eq!(
                        original.get("call_2_1"),
                        Some(&0),
                        "原开始关联与执行事实不被展示细化改写"
                    );
                    let ds = db.delegations(&run_id)?;
                    assert_eq!(ds.len(), 2);
                    assert_ne!(ds[0].child_session, ds[1].child_session);
                    for d in ds {
                        assert_eq!(db.get_run(&d.child_run)?.state, RunState::Completed);
                    }
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(
                prepare(&host, &office, "office-1").await.unwrap(),
                snapshot,
                "重复投影保持seq和派工ID，不再新增拜访"
            );
            if expected_cards == 3 {
                // 普通追问不能擦掉此前同事的真实工作、白板和成员详情。
                provider
                    .pause
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                host.submit_run(&scope, &binding.session_id, "谢谢，请保留团队成果".into())
                    .await
                    .unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    provider.entered.notified(),
                )
                .await
                .unwrap();
                let waiting = prepare(&host, &office, "office-1").await.unwrap();
                assert_eq!(
                    waiting["conversations"][0]["whiteboard"], conv["whiteboard"],
                    "普通追问仍在运行时也不能清空已完成业务白板"
                );
                assert!(
                    waiting["conversations"][0]["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|task| task["status"] == "running"),
                    "追问真实运行状态仍应显示在任务列表"
                );
                provider.release.add_permits(1);
                host.wait_idle().await;
                let after = prepare(&host, &office, "office-1").await.unwrap();
                let current = &after["conversations"][0];
                for previous in conv["tasks"].as_array().unwrap() {
                    assert!(current["tasks"].as_array().unwrap().iter().any(|t| t["id"] == previous["id"] && t["status"] == previous["status"]), "后续普通问答不能移除准确已完成业务任务");
                }
                assert_eq!(
                    current["whiteboard"], conv["whiteboard"],
                    "普通追问保留最近业务目标白板"
                );
                let details = crate::office::details::render(
                    &host,
                    &scope,
                    &binding.session_id,
                    "after-followup",
                    json!({"kind":"member","employeeId":"code-agent","actorId":"code-agent"}),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
                assert!(
                    details["content"]["fields"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["label"] == "独立执行引用"),
                    "成员详情仍应指向其原独立Session/Run"
                );
                assert_eq!(
                    prepare(&host, &office, "office-1").await.unwrap(),
                    after,
                    "保留历史不能造成无事实seq增长"
                );
                host.submit_run(&scope, &binding.session_id, "建立独立的新业务计划".into())
                    .await
                    .unwrap();
                host.wait_idle().await;
                let next = prepare(&host, &office, "office-1").await.unwrap();
                assert!(
                    next["conversations"][0]["whiteboard"]["title"]
                        .as_str()
                        .unwrap()
                        .contains("新业务计划"),
                    "真实新计划必须接管白板"
                );
                assert_ne!(next["conversations"][0]["whiteboard"], conv["whiteboard"]);
                assert_ne!(
                    next["conversations"][0]["whiteboard"]["status"], "completed",
                    "不能沿用旧目标100%冒充新目标完成"
                );
            }
            host.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn delegated_business_step_keeps_one_card_owner_and_stable_assignment() {
        use openpaw_llm::{MockMode, MockProvider};
        struct OnceFailure {
            inner: MockProvider,
            calls: std::sync::atomic::AtomicUsize,
            fail_at: Option<usize>,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for OnceFailure {
            async fn complete(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.inner.complete(request).await
            }
            async fn stream(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<
                futures::stream::BoxStream<
                    'static,
                    openpaw_core::Result<openpaw_core::StreamEvent>,
                >,
            > {
                let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.fail_at == Some(index) {
                    return Err(openpaw_core::Error::Provider(
                        "invalid child response".into(),
                    ));
                }
                self.inner.stream(request).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.inner.capabilities()
            }
            fn default_model(&self) -> &str {
                self.inner.default_model()
            }
            fn name(&self) -> &str {
                "one-child-failure"
            }
        }
        for (mark_active, confirm, retry) in [
            (true, true, false),
            (true, false, false),
            (false, true, false),
            (false, false, false),
            (true, true, true),
            (true, false, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("README.md"), "actual project fact").unwrap();
            let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let mut script = vec![call(
                "report_progress",
                json!({"type":"create","steps":[{"id":"lookup","title":"核对项目说明"},{"id":"summary","title":"王明汇总同事的实际结果"}]}),
            )];
            if mark_active {
                script.push(call(
                    "report_progress",
                    json!({"type":"update","stepId":"lookup","state":"in_progress"}),
                ));
            }
            let delegate_call = if retry {
                "call_3"
            } else if mark_active {
                "call_2"
            } else {
                "call_1"
            };
            script.extend(vec![
            call(
                "delegate_task",
                json!({"role_id":"research","title":"李研核对项目目标","instructions":"读取README.md","acceptance":"有文件依据"}),
            ),
            call("read_file", json!({"path":"README.md"})),
            MockMode::Fixed("李研已实际读取项目说明".into()),
        ]);
            if retry {
                // 首个子模型确实失败；父角色明确发起新的委派，不复用或抹去失败Run。
                script.insert(3, call("delegate_task", json!({"role_id":"research","title":"李研重新核对项目目标","instructions":"读取README.md","acceptance":"有文件依据"})));
            }
            if confirm {
                script.push(call(
                "report_progress",
                json!({"type":"update","stepId":"lookup","state":"completed","evidenceCallIds":[delegate_call]}),
                ));
            }
            script.extend(vec![
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"summary","state":"in_progress"}),
                ),
                MockMode::Fixed("王明汇总：同事已读取项目说明，证据已核实".into()),
            ]);
            let provider = OnceFailure {
                inner: MockProvider::scripted(script),
                calls: std::sync::atomic::AtomicUsize::new(0),
                fail_at: retry.then_some(3),
            };
            let host = Host::with_provider(config, std::sync::Arc::new(provider))
                .await
                .unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "linked".into(),
            };
            let session = host.open_session(scope.clone()).await.unwrap();
            host.submit_run(&scope, &session.session_id, "请安排李研核对并汇总".into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                .await
                .unwrap();
            let office = OfficeConfig {
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            // 动态按观察时间排序，首条不再必然是最近派工；用持久委派ID精确对账。
            let session_id = session.session_id.clone();
            let latest_task = host
                .inner
                .store
                .call(move |db| {
                    let parent = db
                        .runs()?
                        .into_iter()
                        .find(|r| r.session_id == session_id)
                        .unwrap();
                    Ok(db
                        .delegations(&parent.run_id)?
                        .last()
                        .unwrap()
                        .task_id
                        .clone())
                })
                .await
                .unwrap();
            let latest_assignment = conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["id"] == format!("assign:{latest_task}"))
                .unwrap();
            assert_eq!(latest_assignment["kind"], "task.assigned");
            assert_eq!(
                conv["tasks"].as_array().unwrap().len(),
                if confirm { 3 } else { 4 },
                "有准确证据才合并；不能按相似任务标题猜测关联"
            );
            if confirm {
                assert_eq!(conv["tasks"][0]["status"], "completed");
                assert_eq!(
                    conv["tasks"][0]["progress"], 100,
                    "正常保存最终汇总后才确认交付"
                );
                assert_eq!(conv["tasks"][1]["assigneeId"], "code-agent");
                assert_eq!(latest_assignment["taskId"], conv["tasks"][1]["id"]);
            } else {
                assert_eq!(conv["tasks"][0]["status"], "blocked");
                assert!(conv["tasks"][0]["progress"].as_u64().unwrap() < 100);
                assert_eq!(
                    conv["tasks"][1]["status"], "blocked",
                    "子任务成功不自动确认未报告步骤"
                );
                if retry {
                    let attempts: Vec<_> = conv["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|t| t["assigneeId"] == "code-agent")
                        .collect();
                    assert_eq!(attempts.len(), 1, "同一人物的重试不能投影为第二个Agent");
                    assert_eq!(
                        attempts[0]["status"], "blocked",
                        "后次真实执行结束仍待业务核验"
                    );
                    assert!(
                        conv["events"].as_array().unwrap().iter().any(|event| {
                            event["summary"].as_str().unwrap_or("").contains("未完成")
                        }),
                        "历史失败仍须保留在真实动态事件中"
                    );
                }
                let child_index = conv["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|task| task["assigneeId"] == "code-agent")
                    .unwrap();
                assert_eq!(
                    latest_assignment["taskId"],
                    conv["tasks"][child_index]["id"]
                );
            }
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event["kind"] == "task.assigned")
                    .count(),
                if retry { 2 } else { 1 }
            );
            let completions: Vec<_> = conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["kind"] == "task.completed")
                .collect();
            assert_eq!(
                completions.len(),
                usize::from(confirm),
                "执行结束不能伪造核验完成事件；仅确认的证据产生完成动态"
            );
            if let Some(event) = completions.first() {
                assert_eq!(event["employeeId"], "code-agent");
                let child_index = if confirm { 1 } else { 3 };
                assert_eq!(event["taskId"], conv["tasks"][child_index]["id"]);
                assert!(event["summary"].as_str().unwrap().contains("已核验"));
            }
            if retry {
                let session_id = session.session_id.clone();
                host.inner
                    .store
                    .call(move |db| {
                        let parent = db
                            .runs()?
                            .into_iter()
                            .find(|r| r.session_id == session_id)
                            .unwrap();
                        let ds = db.delegations(&parent.run_id)?;
                        assert_eq!(ds.len(), 2);
                        assert_eq!(db.get_run(&ds[0].child_run)?.state, RunState::Failed);
                        assert_eq!(db.get_run(&ds[1].child_run)?.state, RunState::Completed);
                        assert_ne!(ds[0].child_session, ds[1].child_session);
                        Ok(())
                    })
                    .await
                    .unwrap();
                if confirm {
                    assert!(
                        conv["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|event| event["taskId"] == conv["tasks"][1]["id"]),
                        "重试的两次真实交接保留并引用同一业务步骤"
                    );
                }
            }
            assert!(conv["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["kind"] == "task.assigned")
                .all(|event| event["fromEmployeeId"] == "office-lead"));
            assert!(
                !conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["kind"] == "task.started"),
                "首次只看到终态，不能捏造已观察开始的历史"
            );
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event["id"].as_str().unwrap().ends_with(":incomplete"))
                    .count(),
                usize::from(retry)
            );
            assert_eq!(
                prepare(&host, &office, "office-1").await.unwrap(),
                snapshot,
                "无新事实不改变seq或重播交接"
            );
            host.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn legacy_snapshot_upgrades_with_new_seq_and_full_catalogue_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        use openpaw_llm::{MockMode, MockProvider};
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["write_file"],"approval_timeout_secs":30})).unwrap();
        let mut script = vec![MockMode::Fixed("已完成旧任务".into()); 100];
        script.extend([
            MockMode::ToolUse { tool_name: "report_progress".into(), input: json!({"type":"create","steps":[{"id":"draft","title":"撰写本轮报告"},{"id":"check","title":"核验本轮报告"}]}), text: String::new() },
            MockMode::ToolUse { tool_name: "write_file".into(), input: json!({"path":"output.txt","content":"must wait for approval"}), text: String::new() },
            MockMode::Fixed("本轮已完成".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
            .await
            .unwrap();
        let office = OfficeConfig {
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "PAWOFFICE_TEST_KEY".into(),
            allowed_origins: vec!["http://127.0.0.1:8765".into()],
            names: Default::default(),
            creation_slots: vec![],
        };
        host.inner.store.call(|db|db.save_office_projection("office-1",&json!({"schemaVersion":"1.0","seq":77,"workspace":{"id":"office-1","name":"旧版"},"employees":[],"tasks":[],"events":[],"whiteboard":{}}),"cached")).await.unwrap();
        let upgraded = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(upgraded["seq"], 78);
        assert!(upgraded.get("employees").is_none());
        assert_eq!(upgraded["conversations"], json!([]));
        assert_eq!(prepare(&host, &office, "office-1").await.unwrap(), upgraded);
        for i in 0..100 {
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: format!("conv-{i:03}"),
            };
            let b = host.open_session(scope.clone()).await.unwrap();
            let r = host
                .submit_run(
                    &scope,
                    &b.session_id,
                    format!("{i}：{}", "中文长请求".repeat(30)),
                )
                .await
                .unwrap();
            loop {
                if host
                    .get_run(&scope, &r.run_id)
                    .await
                    .unwrap()
                    .state
                    .is_terminal()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        let snapshot = prepare(&host, &office, "office-1").await.unwrap();
        assert!(snapshot["conversations"].as_array().unwrap().len() <= 100);
        assert!(!snapshot["conversations"].as_array().unwrap().is_empty());
        for conversation in snapshot["conversations"].as_array().unwrap() {
            let members = conversation["employees"].as_array().unwrap();
            assert_eq!(members.len(), 6, "容量裁剪不能移除会话人物绑定或左侧成员");
            for role in crate::agents::ROLES.iter() {
                assert!(members.iter().any(|member| member["id"] == role.employee
                    && member["sceneActorId"] == role.actor
                    && member["name"] == role.name));
            }
        }
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot})
            )
            .unwrap()
            .len()
                <= 64000
        );
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            snapshot,
            "裁剪后的内容必须稳定，不能每秒改变seq"
        );
        let active_scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "conv-099".into(),
        };
        let active_session = host.open_session(active_scope.clone()).await.unwrap();
        let active_run = host
            .submit_run(
                &active_scope,
                &active_session.session_id,
                "满目录中等待确认写报告".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (view, mut events) = host.snapshot_and_subscribe(&active_scope).await.unwrap();
                if !view.pending_approvals.is_empty() {
                    break;
                }
                events.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        let active_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let current = active_snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "conv-099")
            .unwrap();
        assert_eq!(
            current["tasks"].as_array().unwrap().len(),
            3,
            "64KiB裁剪不得删除正在审批的业务步骤"
        );
        assert_eq!(current["tasks"][0]["status"], "blocked");
        assert_eq!(current["employees"].as_array().unwrap().len(), 6);
        assert!(active_snapshot["conversations"].as_array().unwrap().len() <= 100);
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":active_snapshot})
            )
            .unwrap()
            .len()
                <= 64000
        );
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            active_snapshot
        );
        assert!(!root.join("output.txt").exists());
        host.cancel_run(&active_scope, &active_run.run_id)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        let terminal_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let latest = terminal_snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "conv-099")
            .unwrap();
        assert_eq!(
            latest["tasks"].as_array().unwrap().len(),
            3,
            "先缩旧终态，不能先清掉最新目标"
        );
        assert!(!root.join("output.txt").exists());
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "overflow".into(),
        };
        assert_eq!(
            host.open_session(scope).await.unwrap_err().kind,
            "capacity_exceeded"
        );
        host.shutdown().await.unwrap();
    }
}
