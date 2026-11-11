//! 职责：将共享Run/call事实投影成完整Office快照并可靠同步。
//! 输入输出：原JSONL和Host状态到持久seq/完整内容及官方消息。
//! 依赖：原Store、官方client、单办公室协调任务。
//! 禁止职责：不伪造进度、工具开始或多Agent，不复制会话账本。
use super::RolePresentation;
use super::{client::Client, dto, OfficeConfig};
use crate::{ClientScope, Host, HostError, HostResult, RunState, RunView};
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

// 正常窗口为新任务、交接与错误说明预留16,000字节；核心事实可超过软目标。
const OFFICE_MESSAGE_TARGET: usize = 48_000;

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

// 只缓存本次裁剪前的字节数，不缓存业务事实，也不改变原裁剪次序。
struct ConversationSizeEdit {
    id: String,
    bytes: usize,
    count: usize,
}

fn conversation_size_edit(
    conversations: &[Value],
    index: usize,
) -> HostResult<ConversationSizeEdit> {
    Ok(ConversationSizeEdit {
        id: conversations[index]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        bytes: serde_json::to_vec(&conversations[index])
            .map_err(HostError::storage)?
            .len(),
        count: conversations.len(),
    })
}

/// 按唯一被修改的会话更新完整封套长度；删除数组元素还需扣除一个逗号。
/// UTF-8及JSON转义仍由原序列化器计算，不用字符数估计协议字节预算。
fn message_size_after_edit(
    snapshot: &Value,
    previous: usize,
    edit: ConversationSizeEdit,
) -> HostResult<usize> {
    let conversations = snapshot["conversations"]
        .as_array()
        .ok_or_else(|| HostError::new("protocol_error", "快照缺少会话目录"))?;
    let remaining = conversations.iter().find(|c| c["id"] == edit.id);
    let (bytes, comma) = match (conversations.len(), remaining) {
        (count, Some(value)) if count == edit.count => (
            serde_json::to_vec(value).map_err(HostError::storage)?.len(),
            0,
        ),
        (count, None) if count + 1 == edit.count => (0, usize::from(edit.count > 1)),
        _ => {
            return Err(HostError::new(
                "protocol_error",
                "快照裁剪范围与字节预算不一致",
            ))
        }
    };
    previous
        .checked_sub(edit.bytes)
        .and_then(|n| n.checked_sub(comma))
        .and_then(|n| n.checked_add(bytes))
        .ok_or_else(|| HostError::new("protocol_error", "快照字节预算溢出"))
}

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
    client: std::sync::Arc<Client>,
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
        let origin = format!("http://{address}");
        host.inner
            .store
            .call_priority(move |db| db.artifact_origin(&origin))
            .await?;
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
                .office_state
                .projection_cache
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
                    .office_state.projection_cache
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
                        .office_state.ui_busy
                        .load(std::sync::atomic::Ordering::Acquire)
                        != 0
                    {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                        }
                    }
                    let revision = host
                        .inner
                        .store
                        .call(move |db| {
                            Ok(Some(db.revision()?))
                        })
                        .await
                        .unwrap_or(None);
                    let now=tokio::time::Instant::now();
                    let refresh=previous_status.is_empty()
                        || revision!=last_revision
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
        tasks.push(tokio::spawn(super::scene::run(
            host,
            client.clone(),
            stop.clone(),
        )));
        Ok(Self {
            stop,
            tasks,
            config,
            address,
            ui_refresh,
            client,
        })
    }
    /// 功能：取得同一Office连接池和恢复通知的短期问答句柄。
    /// 参数：self为已启动协调器。
    /// 返回：连接池、监听地址、恢复广播及关闭token的克隆。
    /// 错误：无。
    /// 状态：不创建网络请求，不延长问题期限。
    /// 并发与取消：短读，不持协调器锁等待用户；关闭token保持同源。
    /// 权限：内部句柄不授予工具或文件权限。
    /// 关联：REQ-OP-013；DES-OP-008；CHG-2026-0007第9.16节。
    pub(crate) fn question_transport(&self) -> crate::question::Transport {
        crate::question::Transport {
            client: self.client.clone(),
            address: self.address,
            refresh: self.ui_refresh.clone(),
            stop: self.stop.clone(),
        }
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
// 普通固定计划保留当前步骤和前一步；持续任务的窗口另按真实在途子Run选择。
fn whiteboard_window<'a>(
    tasks: &'a [Value],
    active: &BTreeSet<String>,
    unbounded: bool,
) -> Vec<&'a Value> {
    let children = tasks.get(1..).unwrap_or_default();
    if unbounded {
        // blocked也可能只是已结束旧执行的待核验状态，不能据此当作在途。
        // active由原子Run终态判断，不从标题、时长或展示百分比推断。
        let mut indices: Vec<_> = children
            .iter()
            .enumerate()
            .filter(|(_, task)| task["id"].as_str().is_some_and(|id| active.contains(id)))
            .map(|(index, task)| (index, task["status"] == "queued"))
            .collect();
        indices.sort_by_key(|&(index, queued)| (queued, index));
        let mut selected: Vec<_> = indices
            .into_iter()
            .take(3)
            .map(|(index, _)| index)
            .collect();
        for index in (0..children.len()).rev() {
            if selected.len() >= 3 {
                break;
            }
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
        selected.sort_unstable();
        return selected.into_iter().map(|index| &children[index]).collect();
    }
    let relative = children.iter().position(|t| t["status"] != "completed");
    let first = relative
        .map(|index| index.saturating_sub(1))
        .unwrap_or(tasks.len().saturating_sub(4));
    children.iter().skip(first).take(3).collect()
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
        let mut displayed = task.clone();
        // 可选摘要可能因封套预算省略；无关revision重建不能让未变的任务时间抖动。
        if prior.get("summary").is_none() {
            displayed.as_object_mut().unwrap().remove("summary");
        }
        if comparable == displayed {
            task["updatedAt"] = prior["updatedAt"].clone();
        }
    }
    task
}

// 活动文字只使用已开始的调用；URL仅保留域名，文件仅保留basename。
fn public_tool_activity(binding: &openpaw_core::ToolCallBinding) -> String {
    let file = || {
        binding
            .input
            .get("path")
            .and_then(Value::as_str)
            .and_then(|path| path.rsplit(['/', '\\']).next())
            .map(|name| {
                dto::bounded(
                    &name.chars().filter(|c| !c.is_control()).collect::<String>(),
                    60,
                )
            })
            .filter(|name| !name.is_empty())
    };
    let (label, target) = match binding.tool_name.as_str() {
        "web_fetch" => (
            "正在读取来源",
            binding
                .input
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_owned)),
        ),
        "web_search" => ("正在检索公开资料", None),
        "read_file" => ("正在读取", file()),
        "write_file" | "edit_file" => ("正在保存", file()),
        "render_visual" => ("正在绘制图表", file()),
        "generate_image" => ("正在生成图片", file()),
        "glob" | "grep" => ("正在查找工作区资料", None),
        "delegate_task" => ("正在协调同事完成分工", None),
        "think" => ("正在分析整理", None),
        _ => ("正在执行已授权操作", None),
    };
    target
        .map(|target| format!("{label} {target}"))
        .unwrap_or_else(|| label.into())
}

fn observed_tool_activity(binding: &openpaw_core::ToolCallBinding, run: &RunView) -> String {
    let text = public_tool_activity(binding);
    if run
        .tools
        .iter()
        .any(|t| t.call_id == binding.call_id && t.state == "completed")
    {
        format!(
            "刚完成：{}；正在组织下一步",
            text.strip_prefix("正在").unwrap_or(&text)
        )
    } else {
        text
    }
}

// 与任务数组同源；长文本不承担消息事实存储，受保护会话预算不足时保留短版。
fn whiteboard_detail(title: &str, tasks: &[Value]) -> String {
    let mut text = format!(
        "# {}\n\n## 当前目标\n\n{}\n\n## 任务进度\n\n以下百分比表示后端已核验进度。\n",
        dto::bounded(title, 100),
        tasks
            .first()
            .and_then(|t| t["summary"].as_str())
            .filter(|summary| !summary.trim().is_empty())
            .unwrap_or(title)
    );
    for task in tasks.iter().take(25) {
        let state = match task["status"].as_str() {
            Some("completed") => "已完成",
            Some("failed") => "未完成",
            Some("blocked") => "等待确认或核验",
            Some("running") => "进行中",
            _ => "待开始",
        };
        let employee = task["assigneeId"]
            .as_str()
            .and_then(|id| crate::agents::ROLES.iter().find(|r| r.employee() == id))
            .map(|r| r.name)
            .unwrap_or("待分配");
        text.push_str(&format!(
            "\n- **{}**：{} · {} · {}%\n",
            dto::bounded(task["title"].as_str().unwrap_or(""), 160),
            employee,
            state,
            task["progress"].as_u64().unwrap_or(0)
        ));
        if let Some(summary) = task["summary"].as_str() {
            text.push_str(&format!(
                "  {}\n",
                dto::bounded(summary, 140).replace('\n', " ")
            ));
        }
    }
    dto::bounded(&text, 4000)
}

// 新阅读器只展示已有任务事实，不把Run结束等同于文件交付或内容验收。
// 保留旧Markdown供较小窗口回退；结构化块在预算不足时整块移除。
fn structured_board(board: &mut Value, tasks: &[Value]) {
    let mut rows: Vec<_> = tasks.iter().skip(1).rev().collect();
    rows.sort_by_key(|task| match task["status"].as_str() {
        Some("running") => 0,
        Some("blocked") => 1,
        Some("queued") => 2,
        Some("failed") => 3,
        _ => 4,
    });
    rows.truncate(12);
    let rows = if rows.is_empty() {
        tasks.iter().take(1).collect()
    } else {
        rows
    };
    let task_rows: Vec<_> = rows.iter().map(|task| {
        let owner = task["assigneeId"].as_str()
            .and_then(|id| crate::agents::ROLES.iter().find(|role| role.employee() == id))
            .map(|role| role.name).unwrap_or("待分配");
        let state = match task["status"].as_str() {
            Some("completed") => "done", Some("running") => "doing",
            Some("blocked" | "failed") => "blocked", _ => "todo",
        };
        json!({"id":task["id"],"title":dto::bounded(task["title"].as_str().unwrap_or("任务"),80),
            "owner":owner,"status":state,"progress":task["progress"],
            "note":dto::bounded(task["summary"].as_str().unwrap_or(""),80)})
    }).collect();
    let text = dto::bounded(
        &whiteboard_detail(board["title"].as_str().unwrap_or(""), tasks),
        4000,
    );
    board["detailSections"] = json!([{"id":"execution-facts","title":"任务执行记录",
        "blocks":[{"type":"text","text":text}]}]);
    let completed = tasks
        .iter()
        .skip(1)
        .filter(|task| task["status"] == "completed")
        .count();
    // draft明确表示执行过程。没有产物证据时不虚构ready/verified或下载地址。
    board["result"] = json!({"title":board["title"],"status":"draft",
        "summary":board["summary"],"tasks":task_rows,
        "metrics":[{"id":"completed-tasks","label":"已完成执行步骤","value":completed.to_string(),"unit":"项"}],
        "note":"此处为真实任务过程；执行完成不代表文件已交付或内容已验收。文件路径及完整工具记录可在成员详情查询。"});
    if let Some(steps) = board["steps"].as_array_mut() {
        for step in steps {
            if let Some(task) = tasks.iter().find(|task| task["id"] == step["id"]) {
                step["state"] = json!(match task["status"].as_str() {
                    Some("completed") => "done",
                    Some("running" | "blocked") => "active",
                    _ => "pending",
                });
            }
        }
    }
}

fn compact_structured_board(conversation: &mut Value) -> bool {
    if let Some(board) = conversation["whiteboard"].as_object_mut() {
        // 不能逐字截断版本文件或历史交付；本轮仅移除可由任务事实重建的过程块。
        if board.get("result").is_some_and(|result| {
            result["artifacts"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        }) {
            // 原件/交付身份不可截断；先去掉可重新只读读取的增强图文和过程块。
            if board.remove("detailSections").is_some() {
                return true;
            }
            if let Some(result) = board.get_mut("result").and_then(Value::as_object_mut) {
                for key in ["sections", "tasks", "metrics"] {
                    if result.remove(key).is_some() {
                        return true;
                    }
                }
            }
            // 目录已由产物投影限制为20条，正文通过snapshotUrl按需读取。
            // 删除目录会让当前文件失去图文预览及版本比较入口，不能视为重复正文裁掉。
            return false;
        }
        for key in ["detailSections", "result"] {
            if board.remove(key).is_some() {
                return true;
            }
        }
    }
    false
}

// 保留主目标与最近成果，按完整任务排版而非截断Markdown；不创建第二份状态。
// 无标题的旧模板不能凭空增加detail，兼容其原有展示契约。
fn whiteboard_tasks(conversation: &Value) -> Option<&[Value]> {
    let title = conversation["whiteboard"]["title"].as_str()?;
    let tasks = conversation["tasks"].as_array()?;
    // 普通追问与业务历史共用任务数组，但白板仍属于最近业务Run。
    // 回补不能把新闲聊或其他Run的卡片混入它的目标和进度。
    let from_step = conversation["whiteboard"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|step| {
            let index = tasks.iter().position(|task| task["id"] == step["id"])?;
            tasks[..=index]
                .iter()
                .rposition(|task| task["id"].as_str().is_some_and(|id| id.starts_with("run:")))
        });
    // 步骤ID优先于可重复的用户标题；无步骤的旧目标才使用原标题兼容。
    let first = from_step
        .or_else(|| {
            tasks.iter().position(|task| {
                task["id"].as_str().is_some_and(|id| id.starts_with("run:"))
                    && dto::bounded(task["title"].as_str().unwrap_or(""), 32) == title
                    && task["status"] == conversation["whiteboard"]["status"]
            })
        })
        .unwrap_or(0);
    let end = tasks
        .iter()
        .enumerate()
        .skip(first + 1)
        .find(|(_, task)| task["id"].as_str().is_some_and(|id| id.starts_with("run:")))
        .map_or(tasks.len(), |(i, _)| i);
    Some(&tasks[first..end])
}

fn bounded_whiteboard_detail(conversation: &Value, count: usize) -> Option<String> {
    let title = conversation["whiteboard"]["title"].as_str()?;
    let tasks = whiteboard_tasks(conversation)?;
    if title.is_empty() || tasks.is_empty() {
        return None;
    }
    let start = tasks.len().saturating_sub(count.saturating_sub(1)).max(1);
    let mut compact: Vec<_> = tasks
        .iter()
        .enumerate()
        .filter(|(i, _)| *i == 0 || *i >= start)
        .map(|(_, task)| {
            let mut task = task.clone();
            task["title"] = json!(dto::bounded(task["title"].as_str().unwrap_or(""), 32));
            if let Some(object) = task.as_object_mut() {
                object.remove("summary");
            }
            task
        })
        .collect();
    compact[0]["summary"] = json!(dto::bounded(title, 80));
    Some(whiteboard_detail(title, &compact))
}

/// 压缩旧业务会话的可选展示字段。
///
/// 任务、进度、白板状态和成员 ID 是历史回看的核心事实，不能因 Office
/// 快照预算被清空；交接事件、职责长文本和说明性字段属于可重建展示，
/// 可以先移除。返回值表示本次是否实际缩小了对象。
fn compact_business_conversation(conversation: &mut Value) -> bool {
    if compact_structured_board(conversation) {
        return true;
    }
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
        for key in ["detail", "summary"] {
            if whiteboard.get(key).is_some_and(|value| value != "") {
                whiteboard[key] = json!("");
                return true;
            }
        }
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

/// 受保护是指身份/状态不可删，不是可选详情无限占用封套。
/// 每次只缩最旧会话的说明或已被新事实取代的动态；保留任务/成员、分派身份与最新动态。
fn compact_protected_details(
    conversations: &mut [Value],
    priorities: &BTreeMap<String, u64>,
) -> HostResult<Option<ConversationSizeEdit>> {
    let mut order: Vec<_> = (0..conversations.len()).collect();
    order.sort_by_key(|&index| {
        priorities
            .get(conversations[index]["id"].as_str().unwrap_or_default())
            .copied()
            .unwrap_or_default()
    });
    for index in order {
        let edit = conversation_size_edit(conversations, index)?;
        let conversation = &mut conversations[index];
        if compact_structured_board(conversation) {
            return Ok(Some(edit));
        }
        let short_detail = bounded_whiteboard_detail(conversation, 3);
        if let Some(board) = conversation["whiteboard"].as_object_mut() {
            // Office用非空detail决定新版阅读器入口；不能为省空间整块清空。
            // 最短版仍绑定同一任务状态/进度，已缩短后继续压其他说明字段。
            if let Some(detail) = board.get("detail").and_then(Value::as_str) {
                let short = short_detail.as_deref().unwrap_or("");
                if serde_json::to_vec(short).map_err(HostError::storage)?.len()
                    < serde_json::to_vec(detail)
                        .map_err(HostError::storage)?
                        .len()
                {
                    board.insert("detail".into(), json!(short));
                    return Ok(Some(edit));
                }
            }
            for key in ["summary", "description", "tagline", "note"] {
                if board
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
                {
                    board.insert(key.into(), json!(""));
                    return Ok(Some(edit));
                }
            }
        }
        if let Some(members) = conversation["employees"].as_array_mut() {
            for member in members {
                if member
                    .as_object_mut()
                    .is_some_and(|m| m.remove("role").is_some())
                {
                    return Ok(Some(edit));
                }
            }
        }
        if let Some(tasks) = conversation["tasks"].as_array_mut() {
            for task in tasks {
                if matches!(task["status"].as_str(), Some("completed" | "failed")) {
                    if task
                        .as_object_mut()
                        .is_some_and(|t| t.remove("summary").is_some())
                    {
                        return Ok(Some(edit));
                    }
                } else if let Some(summary) = task["summary"].as_str() {
                    // 活动作业只在有工具事实时更新，不能被旧历史挤成无说明。
                    // 保留短动作/等待原因；仍超限交由已公开历史裁剪或明确报错。
                    if summary.encode_utf16().count() > 96 {
                        task["summary"] = json!(dto::bounded(summary, 96));
                        return Ok(Some(edit));
                    }
                }
            }
        }
        if let Some(events) = conversation["events"].as_array_mut() {
            for event in events.iter_mut() {
                // summary 是事件Schema必填字段；保留短说明而非移除字段。
                if let Some(text) = event["summary"].as_str() {
                    if text.encode_utf16().count() > 40 {
                        event["summary"] = json!(dto::bounded(text, 40));
                        return Ok(Some(edit));
                    }
                }
            }
            // 同一任务的旧 started/blocked/note 已有更晚状态，完整事实在账本。
            // 分派身份用于去重交接、每任务最新动态用于当前展示，两者绝不裁剪。
            let obsolete = events
                .iter()
                .enumerate()
                .filter(|(_, event)| event["kind"] != "task.assigned")
                .filter(|(_, event)| {
                    let Some(task) = event["taskId"].as_str() else {
                        return false;
                    };
                    let Some(time) = event["occurredAt"].as_str() else {
                        return false;
                    };
                    events.iter().any(|newer| {
                        newer["taskId"].as_str() == Some(task)
                            && newer["occurredAt"]
                                .as_str()
                                .is_some_and(|newer| newer > time)
                    })
                })
                .min_by_key(|(_, event)| (event["occurredAt"].as_str(), event["id"].as_str()))
                .map(|(index, _)| index);
            if let Some(index) = obsolete {
                events.remove(index);
                return Ok(Some(edit));
            }
        }
    }
    Ok(None)
}

/// 对完整事实快照使用最后预算边界；核心不能容纳仍明确失败。
fn fit_protected_snapshot_details(
    team: &str,
    snapshot: &mut Value,
    priorities: &BTreeMap<String, u64>,
) -> HostResult<bool> {
    let mut bytes =
        serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}))
            .map_err(HostError::storage)?
            .len();
    while bytes > OFFICE_MESSAGE_LIMIT {
        let Some(edit) = compact_protected_details(
            snapshot["conversations"]
                .as_array_mut()
                .expect("conversation array"),
            priorities,
        )?
        else {
            return Ok(false);
        };
        // 复用原UTF-8增量计量；大目录不为每个可选字段重新序列化整个办公室。
        bytes = message_size_after_edit(snapshot, bytes, edit)?;
    }
    Ok(snapshot_fits_office_message(team, snapshot))
}

// 仅从原委派、事实版本与上次持久快照计算，不跟踪动画进度。
#[derive(Default)]
struct PublishedHistory {
    anchors: BTreeSet<String>,
    terminal_tasks: BTreeSet<String>,
}

// 已公开事件才可退到账本查询；保留末次已见分派作为历史锚点。
// 锚点不是全队开工前缀，更不表示HTTP接受或动画完成。
fn compact_seen_history(
    snapshot: &mut Value,
    prior: &Value,
    priorities: &BTreeMap<String, u64>,
    published: &PublishedHistory,
) -> HostResult<Option<ConversationSizeEdit>> {
    let Some(conversations) = snapshot["conversations"].as_array_mut() else {
        return Ok(None);
    };
    let mut order: Vec<_> = (0..conversations.len()).collect();
    order.sort_by_key(|&i| {
        priorities
            .get(conversations[i]["id"].as_str().unwrap_or_default())
            .copied()
            .unwrap_or_default()
    });
    for index in order {
        let edit = conversation_size_edit(conversations, index)?;
        let conversation = &mut conversations[index];
        let Some(old) = prior["conversations"]
            .as_array()
            .and_then(|all| all.iter().find(|c| c["id"] == conversation["id"]))
        else {
            continue;
        };
        let seen: BTreeSet<_> = old["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["id"].as_str())
            .collect();
        let old_task_states: BTreeMap<_, _> = old["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["id"].as_str()?, t["status"].as_str()?)))
            .collect();
        // 重建会再次读到已归档事实；只按此前已公开的终态任务移除
        // 它们，游标由原Delegation顺序计算，随机ID/相同时间都不参与排序。
        // 只有已公开的终态任务事件可以退回历史查询。当前任务的等待、
        // 开始及最新动作不能仅因上次快照出现过就被当作旧历史。
        let terminal_ids: BTreeSet<_> = conversation["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| matches!(t["status"].as_str(), Some("completed" | "failed")))
            .filter_map(|t| t["id"].as_str().map(str::to_owned))
            .collect();
        // 当前人物窗口会主动移走终态卡；卡片缺席不代表其已公开事件仍在途。
        // 只采用原账本确认的终态集合，同时保护仍可见的活动卡与首次公开结果。
        let visible_ids: BTreeSet<_> = conversation["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["id"].as_str().map(str::to_owned))
            .collect();
        if let Some(events) = conversation["events"].as_array_mut() {
            if let Some(i) = events
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e["id"].as_str().is_some_and(|id| {
                        e["taskId"].as_str().is_some_and(|task| {
                            (terminal_ids.contains(task) || !visible_ids.contains(task))
                                && published.terminal_tasks.contains(task)
                        }) && (seen.contains(id)
                            || e["taskId"].as_str().is_some_and(|task| {
                                published.terminal_tasks.contains(task)
                                    && old_task_states.get(task).is_none_or(|state| {
                                        matches!(*state, "completed" | "failed")
                                    })
                            }))
                            && !published.anchors.contains(id)
                    })
                })
                .min_by_key(|(_, e)| {
                    (
                        // 已归档事件重建后的观察时间不是新业务事实；先移出这些
                        // 不在上次窗口中的旧事件，再按时间收缩仍展示的历史。
                        e["id"].as_str().is_some_and(|id| seen.contains(id)),
                        e["occurredAt"].as_str(),
                        e["id"].as_str(),
                    )
                })
                .map(|(i, _)| i)
            {
                events.remove(i);
                return Ok(Some(edit));
            }
        }
        // 长期持续Run的已完成卡片也必须有界。保留最近24项、全部非终态、
        // 白板引用和每位成员最新一项；较早成果仍由原成员历史/任务详情读取。
        let protected: BTreeSet<String> = conversation["whiteboard"]["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["id"].as_str().map(str::to_owned))
            .chain(
                conversation["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e["taskId"].as_str().map(str::to_owned)),
            )
            .collect();
        if let Some(tasks) = conversation["tasks"].as_array_mut() {
            let terminal: Vec<_> = tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t["status"].as_str(), Some("completed" | "failed")))
                .map(|(i, _)| i)
                .collect();
            if terminal.len() <= 24 {
                continue;
            }
            let recent: BTreeSet<_> = terminal.iter().rev().take(24).copied().collect();
            let latest_by_actor: BTreeSet<_> = tasks
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t["assigneeId"].as_str().map(|actor| (actor, i)))
                .collect::<BTreeMap<_, _>>()
                .into_values()
                .collect();
            if let Some(index) = terminal.into_iter().find(|i| {
                let t = &tasks[*i];
                !recent.contains(i)
                    && !latest_by_actor.contains(i)
                    && !protected.contains(t["id"].as_str().unwrap_or_default())
                    && !t["id"].as_str().is_some_and(|id| id.starts_with("run:"))
                    && t["id"]
                        .as_str()
                        .is_some_and(|id| published.terminal_tasks.contains(id))
            }) {
                tasks.remove(index);
                return Ok(Some(edit));
            }
        }
    }
    Ok(None)
}
// 新版阅读器的可选过程块重复了原任务事实，先释放这些字节，再收缩真实动态。
// 否则少量文件元数据就可能让当前团队的开始/完成动态先退场，而旧详情仍占预算。
fn compact_optional_board_history(
    snapshot: &mut Value,
    priorities: &BTreeMap<String, u64>,
) -> HostResult<Option<ConversationSizeEdit>> {
    let conversations = snapshot["conversations"]
        .as_array_mut()
        .expect("conversation array");
    let mut order: Vec<_> = (0..conversations.len()).collect();
    order.sort_by_key(|&i| {
        priorities
            .get(conversations[i]["id"].as_str().unwrap_or_default())
            .copied()
            .unwrap_or_default()
    });
    for index in order {
        let edit = conversation_size_edit(conversations, index)?;
        if compact_structured_board(&mut conversations[index]) {
            return Ok(Some(edit));
        }
    }
    Ok(None)
}
// 可选旧过程先退场，再收缩已公开终态；当前说明最后缩短，硬界不放宽。
fn compact_history_to_target(
    team: &str,
    snapshot: &mut Value,
    prior: Option<&Value>,
    priorities: &BTreeMap<String, u64>,
    published: &PublishedHistory,
) -> HostResult<()> {
    let mut bytes =
        serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}))
            .map_err(HostError::storage)?
            .len();
    while bytes > OFFICE_MESSAGE_TARGET {
        let edit = if let Some(edit) = compact_optional_board_history(snapshot, priorities)? {
            Some(edit)
        } else if let Some(prior) = prior {
            compact_seen_history(snapshot, prior, priorities, published)?
        } else {
            None
        };
        let Some(edit) = edit else {
            break;
        };
        bytes = message_size_after_edit(snapshot, bytes, edit)?;
    }
    // 最近终态卡片达到保留底线后，仍可收缩重复的历史说明；否则重启
    // 重建会恢复这些长摘要，虽未撞硬界却持续占满为新交接预留的空间。
    // 复用原受保护字段裁剪，当前动作、身份、状态和进度仍保留。
    while bytes > OFFICE_MESSAGE_TARGET {
        let Some(edit) = compact_protected_details(
            snapshot["conversations"]
                .as_array_mut()
                .expect("conversation array"),
            priorities,
        )?
        else {
            break;
        };
        bytes = message_size_after_edit(snapshot, bytes, edit)?;
    }
    Ok(())
}

fn fit_snapshot_with_history(
    team: &str,
    snapshot: &mut Value,
    prior: Option<&Value>,
    priorities: &BTreeMap<String, u64>,
    published: &PublishedHistory,
) -> HostResult<bool> {
    compact_history_to_target(team, snapshot, prior, priorities, published)?;
    if fit_protected_snapshot_details(team, snapshot, priorities)? {
        refill_whiteboard_details(team, snapshot, priorities)?;
        return Ok(true);
    }
    Ok(false)
}

// 裁掉已公开历史后可能重新空出预算。只扩展仍有详情的会话，优先最新，
// 不复活已归档入口；每次用真实JSON封套差额计量，给seq增长留足整数位。
fn refill_whiteboard_details(
    team: &str,
    snapshot: &mut Value,
    priorities: &BTreeMap<String, u64>,
) -> HostResult<()> {
    let mut bytes =
        serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot}))
            .map_err(HostError::storage)?
            .len();
    let mut order: Vec<_> = (0..snapshot["conversations"].as_array().map_or(0, Vec::len)).collect();
    order.sort_by_key(|&i| {
        std::cmp::Reverse(
            priorities
                .get(
                    snapshot["conversations"][i]["id"]
                        .as_str()
                        .unwrap_or_default(),
                )
                .copied()
                .unwrap_or_default(),
        )
    });
    for index in order {
        let conversation = &snapshot["conversations"][index];
        let Some(current) = conversation["whiteboard"]["detail"]
            .as_str()
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let (Some(title), Some(tasks)) = (
            conversation["whiteboard"]["title"].as_str(),
            whiteboard_tasks(conversation),
        ) else {
            continue;
        };
        let mut candidates = vec![whiteboard_detail(title, tasks)];
        for count in [12, 6, 3] {
            if let Some(detail) = bounded_whiteboard_detail(conversation, count) {
                candidates.push(detail);
            }
        }
        // 只回补本算法的短版；未裁剪的既有详情保持原业务归属与文字。
        if !candidates.iter().skip(1).any(|detail| detail == current) {
            continue;
        }
        let current = current.to_owned();
        let current_bytes = serde_json::to_vec(&current)
            .map_err(HostError::storage)?
            .len();
        for detail in candidates {
            if serde_json::to_vec(&detail)
                .map_err(HostError::storage)?
                .len()
                <= current_bytes
            {
                continue;
            }
            let conversations = snapshot["conversations"]
                .as_array_mut()
                .expect("conversation array");
            let edit = conversation_size_edit(conversations, index)?;
            conversations[index]["whiteboard"]["detail"] = json!(detail);
            let next_bytes = message_size_after_edit(snapshot, bytes, edit)?;
            if next_bytes <= OFFICE_MESSAGE_TARGET.saturating_sub(16) {
                bytes = next_bytes;
                break;
            }
            snapshot["conversations"][index]["whiteboard"]["detail"] = json!(current);
        }
    }
    Ok(())
}

/// 压缩最旧的可裁剪业务投影；已无可选字段时移除其展示入口。
/// 调用方先排除受保护、活动及新交接会话；原始账本不受影响。
/// 每次必须缩小目录，不能让已压缩到底的第一项阻塞后续历史。
fn compact_business_history(conversations: &mut Vec<Value>, index: usize) {
    if !compact_business_conversation(&mut conversations[index]) {
        conversations.remove(index);
    }
}

/// 判断终态Run是否已经属于此前成功投影的历史。
///
/// `None` 表示尚无此前投影的事实基线；不能据此归档新交接。
/// 该判断只用于压缩已公开历史，不表示浏览器已接收或播放动画。
fn is_historical_terminal(run_revision: u64, previous_source_revision: Option<u64>) -> bool {
    previous_source_revision.is_some_and(|revision| run_revision <= revision)
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
/// 状态：只投影真实任务与接手事实；不改变Run、工具或审批事实。
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
            .office_state
            .projection_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks
            .entry(team.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _projection_guard = projection_lock.lock().await;
    let employees:Vec<_>=crate::agents::ROLES.iter().map(|role|json!({"id":role.employee(),"name":role.name,"sceneActorId":role.actor(),"role":role.duty,"online":crate::agents::enabled(role,&host.inner.config)})).collect();
    let diagnostics_team = team.clone();
    let ui_busy = host.inner.office_state.ui_busy.clone();
    let terminal_runs = host.inner.office_state.terminal_runs.clone();
    let activity_window = config.activity_window;
    let collaboration = host
        .inner
        .config
        .agents
        .as_ref()
        .is_some_and(|agents| agents.enabled);
    let result = host.inner.store.call(move|db|{
        let diagnostics = std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some();
        let fact_started = std::time::Instant::now();
        let source_revision = db.revision()?;
        let previous_source_revision = db.office_projection_source.get(&team).copied();
        let reusable = match previous_source_revision {
            Some(previous) => previous == source_revision || !db.office_projection_changed(&team, previous)?,
            None => false,
        };
        if reusable {
            if let Some((snapshot, _)) = db.office_projection(&team)? {
                if snapshot_fits_office_message(&team, &snapshot) {
                    // 无关办公室的事实已检查，推进全局水位避免重复扫描；
                    // 本办公室的开始、工具、进度与终态仍使快照立即失效。
                    db.office_projection_source.insert(team.clone(), source_revision);
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
        // 新交接所在会话不可被历史目录预算裁掉，动画调度由Office负责。
        let mut handoff_conversations = BTreeSet::new();
        let mut published = PublishedHistory::default();
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
            let continuous=db.continuous_state(&r.run_id)?;
            // 无终点的主目标没有固定分母。保留原计划供详情核查，但不能让
            // 启动期未更新的步骤遮住后续事项；有限任务及子Run仍验原计划。
            let unbounded=continuous.as_ref().is_some_and(|c|c.started&&c.completion_condition.is_none());
            let plan=if unbounded {None}else{db.progress(&r.run_id)?};
            let (steps,mut percent)=crate::task_progress::project(plan.clone(),&r)?;
            let mut active_child_ids = BTreeSet::new();
            let mut all_child_failed = false;
            let mut all_child_blocked = false;
            let delegations=db.delegations(&r.run_id)?;
            let public_end = delegations.iter().rposition(|d|prior_conv
                .and_then(|c|c["events"].as_array()).is_some_and(|events|
                    events.iter().any(|e|e["id"]==format!("assign:{}",d.task_id))));
            if let Some(end)=public_end {
                published.anchors.insert(format!("assign:{}",delegations[end].task_id));
            }

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
            // 只合并被准确后续验收替代的终态尝试，包括未获验收的初稿。
            // 已分别验收的成果不能互相覆盖；相似标题不构成修订关系。
            // 同角色、同父、同步骤缺一不可，旧Run/JSONL/派发事件仍保留。
            for (attempt_index,d) in delegations.iter().enumerate() {
                let Some(index)=assignments.get(&d.call_id).copied() else {continue;};
                let Some(step)=plan.as_ref().and_then(|p|p["steps"].get(index)) else {continue;};
                if step["state"]!="completed" {continue;}
                if step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|id==&d.call_id)) {continue;}
                let ended=match read_run(&d.child_run) {Ok(c)=>c.state.is_terminal(),Err(e) if e.kind=="not_found"=>false,Err(e)=>return Err(e)};
                if !ended {continue;}
                for replacement in delegations.iter().skip(attempt_index+1) {
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
            // 人物身份唯一不代表任务身份唯一。同一角色顺序完成不同章节时，
            // 原成功成果必须保留；只有准确证据替代的失败尝试才不再占卡片。
            let display_calls:BTreeSet<_>=delegations.iter()
                .filter(|d|!recovered.contains(&d.call_id))
                .map(|d|d.call_id.clone()).collect();
            for (delegation_index,d) in delegations.iter().enumerate() {
                let role=crate::agents::role(&d.role_id).ok_or_else(||HostError::new("storage_unavailable","委派引用未知角色"))?;
                let show_card=display_calls.contains(&d.call_id);
                let child=match read_run(&d.child_run) {Ok(v)=>Some(v),Err(e) if e.kind=="not_found"=>None,Err(e)=>return Err(e)};
                let state=child.as_ref().map(|c| if !c.state.is_terminal() && (c.state==RunState::Cancelling || c.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource"|"awaiting_user"))) {"blocked"}else{status(c.state)}).unwrap_or(if r.state.is_terminal(){"failed"}else{"queued"});
                let superseded=recovered.contains(&d.call_id);
                let done=state=="completed";
                // 正式业务卡以父计划的准确委派验收为准；子执行计划是未验收时的阶段展示。
                // 文件验收仍由原Store核验实际子写入/路径，不能凭正文或终态跳过。
                let (child_steps_view, child_percent)=match &child {
                    Some(c)=>crate::query::RunFacts::read(db,c)?.execution()?,
                    None=>(Vec::new(),0),
                };
                let verified=child.as_ref().is_some_and(|c| crate::task_progress::delegation_verified(plan.as_ref(),&d.call_id,c,child_percent));
                let summary=if let Some(c)=&child {
                    if state=="queued" {"待执行：正在排队，尚未开始工作".into()}
                    else if done {dto::bounded(&c.text,180)} else if c.state.is_terminal() {super::turns::message(c)}
                    else if c.tools.iter().any(|t|t.state=="awaiting_user") {"等待用户补充信息，请在当前对话选择或填写其他需求".into()}
                    else if state=="blocked" {"等待确认、资源或清理".into()}
                    else if let Some(binding)=db.office_tool_activity(c)? {
                        format!("{}{}；已核验进度{child_percent}%",observed_tool_activity(&binding,c),child_steps_view.iter().find(|s|s["status"]=="running").and_then(|s|s["title"].as_str()).map(|title|format!("；正在{title}")).unwrap_or_default())
                    }
                    else if let Some(step)=child_steps_view.iter().find(|s|s["status"]=="running") {
                        format!("正在{}；已核验进度{child_percent}%",step["title"].as_str().unwrap_or("处理任务"))
                    } else if let Some(tool)=c.tools.iter().rev().find(|t|t.state=="running") {
                        match tool.tool_name.as_str() {
                            "web_search"=>"正在检索公开资料", "web_fetch"=>"正在核对来源正文",
                            "read_file"|"glob"|"grep"=>"正在读取工作区资料",
                            "write_file"|"edit_file"=>"正在保存文档", "think"=>"正在分析整理",
                            _=>"正在执行已授权操作",
                        }.into()
                    } else {"正在分析并组织下一步；尚无新增核验进度".into()}
                } else if r.state.is_terminal() {"委派中断，未接受执行；不会自动重放".into()} else {"已登记委派，等待接受执行".into()};
                // 子卡表示该同事的执行；父计划另行表示业务验收，不能因父漏报
                // 把已保存正文的有限执行降回0%。未核验的真实文件仍保留blocked。
                let execution_complete=done && (verified || child_percent==100);
                // 父可用当前业务步骤的真实独立工具证据验收，而不必重复引用每次委派。
                // 只有别人的委派证据仍不能替该同事验收；父计划独立取最小进度。
                let independent_acceptance=plan.as_ref().is_some_and(|p| {
                    p["steps"].as_array().into_iter().flatten().enumerate().any(|(index,step)|
                        assignments.get(&d.call_id).is_none_or(|assigned|*assigned==index)
                        && step["state"]=="completed"
                        && step["evidenceCallIds"].as_array().is_some_and(|ids|ids.iter().any(|id|
                            r.tools.iter().any(|tool|id==&tool.call_id && tool.tool_name!="delegate_task" && tool.state=="completed"))))
                });
                if show_card { completed_children += u64::from(execution_complete && (verified || independent_acceptance)); }
                let mut item=task(d.task_id.clone(),d.title.clone(),if done&&!execution_complete{"blocked"}else{state},if done&&!execution_complete{"独立执行已结束，文件步骤仍待核验".into()}else if execution_complete&&!verified{format!("执行已完成，父目标待验收；{summary}")}else{summary},&old,&now,if execution_complete{100}else{child_percent});
                item["assigneeId"]=json!(role.employee());
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
                let existing_assignment=prior_conv.and_then(|v|v["events"].as_array())
                    .and_then(|events|events.iter().find(|event|event["id"]==format!("assign:{}",d.task_id)));
                // 跨角色接手不是全队FIFO前缀。只有确切旧事件，或锚点之前
                // 已投影过的终态，才可归档；上次仍queued的卡不能据此被裁掉。
                let was_published=existing_assignment.is_some() || public_end.is_some_and(|end| delegation_index<=end)
                    && child.as_ref().is_some_and(|c| c.state.is_terminal()
                        && (is_historical_terminal(c.revision,previous_source_revision)
                            || previous_source_revision.is_none() && old.get(task_id.as_str().unwrap())
                                .is_none_or(|t|matches!(t["status"].as_str(),Some("completed"|"failed")))));
                if was_published && child.as_ref().is_some_and(|c|c.state.is_terminal()) {
                    published.terminal_tasks.insert(task_id.as_str().unwrap().to_owned());
                }

                if show_card && !superseded {
                    if child.as_ref().is_some_and(|c| !c.state.is_terminal()) || state=="queued" {
                        active_child_ids.insert(item["id"].as_str().unwrap().to_owned());
                    }
                    all_child_failed |= item["status"] == "failed";
                    all_child_blocked |= item["status"] == "blocked";
                }
                let mut lifecycle = Vec::new();
                if let Some(c)=&child {
                    // 生命周期动态复用原完整快照保存首次观察时间，不伪造错过的开始时刻。
                    // 业务核验与子Run结束分开；只有assignment能驱动交接动画。
                    let phases=[
                        ("started","task.started",c.state==RunState::Running,"观察到独立执行"),
                        ("blocked","task.blocked",!c.state.is_terminal()&&state=="blocked","曾观察到等待确认、资源或清理"),
                        ("completed","task.completed",!superseded&&item["status"]=="completed",if verified {"已核验完成"}else{"执行完成，父目标待验收"}),
                        ("result","note",done&&item["status"]!="completed","独立执行已结束；业务步骤须由主角色核验"),
                        ("incomplete","note",c.state.is_terminal()&&!done,"此尝试未完成；真实结果保留在成员历史"),
                    ];
                    for (phase,kind,observed,label) in phases {
                        let id=format!("activity:{}:{phase}",d.task_id);
                        let existing=prior_conv.and_then(|v|v["events"].as_array()).and_then(|events|events.iter().find(|event|event["id"]==id));
                        if !observed&&existing.is_none() {continue;}
                        lifecycle.push(json!({"id":id,"kind":kind,"summary":dto::bounded(&format!("{label}：{}",d.title),300),"occurredAt":existing.map(|event|event["occurredAt"].clone()).unwrap_or_else(||json!(now)),"taskId":task_id,"employeeId":role.employee()}));
                    }
                    child_events.extend(lifecycle);
                }
                if show_card && !superseded {
                    if let Some(index)=linked {child_steps.insert(index,item.clone());} else {child_tasks.push(item.clone());}
                }
                // Accepted仍在真实成员许可队列。排队卡已可见，但接手事件只在
                // 实际运行后发布；取消前未开工且无执行证据的任务不伪造交接。
                let mut started=child.as_ref().is_some_and(|c| matches!(c.state,RunState::Running|RunState::Completed)
                    || !c.text.is_empty() || !c.tools.is_empty());
                // 首次Provider请求失败也可能没有正文/工具；只有原账本的
                // RunStarted才能与未出队就取消/超时的任务区分。只在缺少
                // 现成执行证据且没有既有assignment的终态分支读取原Reader。
                if !started && existing_assignment.is_none() {
                    if let Some(c)=child.as_ref().filter(|c|c.state.is_terminal()) {
                        started=db.events(&c.session_id)?.iter().any(|event|matches!(event,
                            openpaw_core::SessionEvent::RunStarted{run_id} if run_id==&c.run_id));
                    }
                }
                if started || existing_assignment.is_some() {
                let existing=existing_assignment;
                let from=d.source_task_id.as_ref().and_then(|id|delegations.iter().find(|previous|&previous.task_id==id)).and_then(|previous|crate::agents::role(&previous.role_id)).map_or("office-lead",|role|role.employee());
                let mut assignment=existing.cloned().unwrap_or_else(||json!({"id":format!("assign:{}",d.task_id),"kind":"task.assigned","summary":dto::bounded(&format!("交给{}：{}",role.name,d.title),300),"occurredAt":now,"taskId":task_id,"employeeId":role.employee(),"fromEmployeeId":from}));
                // 精确业务证据后来把委派卡归并为唯一业务步骤时，事件仍是
                // 同一次交接，但必须指向当前稳定任务ID，不能挂回临时卡。
                // 事件ID和首次时间保持不变，因此不会重播拜访动画。
                assignment["taskId"]=task_id;
                if !was_published {handoff_conversations.insert(binding.scope.conversation_id.clone());}
                child_events.push(assignment);
                }
            }
            if !delegations.is_empty() {
                let required_children=display_calls.len() as u64;
                let delivered=r.state==RunState::Completed && completed_children==required_children;
                let child_percent=(completed_children+u64::from(delivered))*100/(required_children+1);
                percent=if steps.is_empty() {child_percent}else{percent.min(child_percent)};
            }
            if continuous.as_ref().is_some_and(|c|c.started) && !delegations.is_empty() && plan.is_none() {
                // 持续目标没有一个可预测的最终分母；只展示当前已有委派的
                // 真实核验比例。新事项会扩大分母，旧成果保留独立100%卡片。
                percent=completed_children*100/display_calls.len().max(1) as u64;
            }
            // 当前批次全部完成不等于主目标已结束；只有可靠Completed允许整体100%。
            if r.state != RunState::Completed {percent=percent.min(99);}
            let child_blocked=all_child_blocked || child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="blocked");
            let child_failed=all_child_failed || child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="failed");
            if r.state==RunState::Completed && child_blocked {percent=percent.min(99);}
            let active_tool_summary = db.office_tool_activity(&r)?.map(|binding| observed_tool_activity(&binding,&r));
            let summary=match r.state {
                RunState::Cancelled=>"已停止，实际操作已清理".to_owned(),
                RunState::Interrupted=>"处理被中断，已完成事项保留".to_owned(),
                RunState::TimedOut|RunState::Failed=>super::turns::message(&r),
                RunState::Cancelling=>"正在停止，等待实际操作清理".to_owned(),
                RunState::Completed if child_failed=>"回复已结束；仍有未恢复的子任务失败，已完成部分保留".to_owned(),
                RunState::Completed if percent<100=>"回复已结束；已完成部分保留，其余待核验或继续处理".to_owned(),
                RunState::Completed=>"本次目标已完成".to_owned(),
                _ if !unbounded && child_blocked && !child_tasks.iter().chain(child_steps.values()).any(|t|t["status"]=="running")=>"子任务等待确认、核验或可用资源".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_user")=>"等待用户补充信息，请选择或填写其他需求".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_approval")=>"等待你确认本次操作".to_owned(),
                _ if r.tools.iter().any(|t|t.state=="awaiting_resource")=>"等待可用资源".to_owned(),
                _=>child_tasks.iter().chain(child_steps.values()).find(|t|t["status"]=="running")
                    .map(|t|{
                        // 主卡展示团队概况，子卡仍展示本人动作；聚合时保留可信
                        // 负责人，避免把同事的读取/写入误显示为主角色亲自执行。
                        let owner=t["assigneeId"].as_str()
                            .and_then(|id|crate::agents::ROLES.iter().find(|role|role.employee()==id))
                            .map(|role|role.name).unwrap_or("同事");
                        format!("团队进展 · {owner}：{}；已核验总进度{percent}%",t["summary"].as_str().unwrap_or("正在处理"))
                    })
                    .or_else(||active_tool_summary.map(|summary|format!("{summary}；已核验进度{percent}%")))
                    .or_else(||steps.iter().find(|s|s["status"]=="running").map(|s|
                        format!("正在{}；已核验进度{percent}%",s["title"].as_str().unwrap_or("处理任务"))))
                    .unwrap_or_else(||"正在分析并组织下一步；尚无新增核验进度".to_owned()),
            };
            let main_state=if r.state==RunState::Completed && child_failed {"failed"} else if r.state==RunState::Completed && percent<100 || !r.state.is_terminal() && (!unbounded && child_blocked || r.tools.iter().any(|t|matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource"|"awaiting_user")) || r.state==RunState::Cancelling) {"blocked"}else{status(r.state)};
            let business_title = input.lines().find(|line| !line.trim().is_empty()).unwrap_or(&input).trim();
            let mut tasks=vec![task(main_id,dto::bounded(business_title,80),main_state,summary.clone(),&old,&now,percent)];
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
                if collaboration || !delegations.is_empty() {continue;} // 真实Agent保持Run归属，不能按工具类型虚构另一成员执行。
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
                    let mut displayed=task.clone();
                    if prior.get("summary").is_none() {displayed.as_object_mut().unwrap().remove("summary");}
                    if comparable==displayed {task["updatedAt"]=prior["updatedAt"].clone();}
                }
            }
            events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            let selected=&tasks[0];
            let board_steps:Vec<_>=if tasks.len()>1 {
                // 白板最多三个步骤（Office 官方 schema 限制）。优先保留当前步骤
                // 及其前一个已完成步骤，全部结束时保留最后三个结果；不能因为
                // 新步骤出现就把刚完成的成果全部挤掉。
                whiteboard_window(&tasks,&active_child_ids,unbounded).into_iter().map(|t| {
                    let label=match t["status"].as_str(){Some("completed")=>"已完成",Some("running")=>"处理中",Some("blocked")=>"等待确认",Some("failed")=>"未完成",_=>"待处理"};
                    json!({"id":t["id"],"title":dto::bounded(&format!("{} · {label}",dto::bounded(t["title"].as_str().unwrap(),24)),40),"progress":t["progress"]})
                }).collect()
            } else {vec![json!({"id":"goal","title":dto::bounded(&summary,80),"progress":percent})]};
            let completed=tasks.iter().skip(1).filter(|t|t["status"]=="completed").count();
            let description=if tasks.len()>1 {
                format!("已确认完成 {completed}/{} 步；{summary}",tasks.len()-1)
            } else {summary};
            let mut whiteboard=json!({"template":"task-board-v1","title":dto::bounded(selected["title"].as_str().unwrap_or(""),32),"status":selected["status"],"tagline":"","description":dto::bounded(&description,300),"summary":dto::bounded(&description,300),"detail":whiteboard_detail(selected["title"].as_str().unwrap_or(""),&tasks),"note":"","link":{"label":"","url":""},"steps":board_steps});
            structured_board(&mut whiteboard,&tasks);
            let artifact_warnings=db.artifact_warnings(&r.run_id)?;
            if !artifact_warnings.is_empty(){whiteboard["result"]["note"]=json!(dto::bounded(&format!("文件版本捕获警告：{}；原工作区写入保留，不提供缺失版本的预览/下载。",artifact_warnings.join("；")),300));}
            if run_id==board_id {retained_whiteboard=whiteboard;}
            if activity_window {
                // 先构造完整业务白板，再裁人物当前窗口。业务未验收不等于执行仍在途。
                tasks.retain(|t| if !delegations.is_empty() && t["assigneeId"]!="office-lead" {
                    active_child_ids.contains(t["id"].as_str().unwrap_or(""))
                } else {
                    !r.state.is_terminal() && !matches!(t["status"].as_str(),Some("completed"|"failed"))
                });
                // 原Office将title渲染为业务粗体、summary渲染为灰色动作。
                // 活动窗口只裁终态，不覆盖业务名称，也不把长目标挤到动作前面。
                for t in &mut tasks {
                    if let Some(previous)=old.get(t["id"].as_str().unwrap_or("")) {
                        let mut comparable=previous.clone();comparable["updatedAt"]=t["updatedAt"].clone();
                        if comparable==*t {t["updatedAt"]=previous["updatedAt"].clone();}
                    }
                }
            }
            retained_tasks.extend(tasks);
            retained_events.extend(events);
            }
            retained_events.sort_by(|a,b|b["occurredAt"].as_str().cmp(&a["occurredAt"].as_str()).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            priorities.insert(binding.scope.conversation_id.clone(),latest.revision);
            if latest.state.is_terminal() {terminal_conversations.insert(binding.scope.conversation_id.clone());}
            super::artifacts::project(db,&binding.scope,&mut retained_whiteboard)?;
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
        // 先主动收缩已公开终态历史，避免等到硬界才牺牲当前动作/白板。
        compact_history_to_target(&team, &mut snapshot, prior, &priorities, &published)?;
        // 整办公室硬预算；当前必要事实可以超过正常目标，不能超过完整封套硬界。
        // 历史增长时逐字段裁剪仍遵循原策略，但每步只序列化被修改的会话。
        // 完整目录只计量一次，避免每次删除一个字段都复制/编码整个办公室。
        let mut message_size=serde_json::to_vec(&json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot})).map_err(HostError::storage)?.len();
        let mut previous_edit=None;
        loop {
            if let Some(edit)=previous_edit.take() {
                message_size=message_size_after_edit(&snapshot,message_size,edit)?;
            }
            if message_size<=OFFICE_MESSAGE_LIMIT {break;}
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
                        // 窗口外已结束业务的交接属于历史：其锚点可能已随
                        // 上次裁剪退出目录，重建不能再把旧assignment钉成新交接。
                        // 当前活动、最近四组业务及最新会话已由protected集合排除。
                        && business_conversations.contains(c["id"].as_str().unwrap_or("")))
                    .min_by_key(|(_,c)| priorities.get(c["id"].as_str().unwrap_or(""))));
                let Some((index,_))=fallback else {
                    // 不能因最近窗口/活动会话均受保护就阻断审批菜单；只压缩
                    // 可选说明，保留全部任务、六成员和交接核心事实。
                    let conversation_count = convs.len();
                    if fit_snapshot_with_history(&team, &mut snapshot, prior, &priorities, &published)? { break; }
                    if diagnostics {
                        eprintln!("engine-host diagnostics projection clip exhausted team={} conversations={} terminal={} handoff={}", team, conversation_count, terminal_conversations.len(), handoff_conversations.len());
                    }
                    return Err(HostError::new("protocol_error","Office完整会话目录超限"));
                };
                previous_edit=Some(conversation_size_edit(convs,index)?);
                if business_conversations.contains(convs[index]["id"].as_str().unwrap_or("")) {
                    compact_business_history(convs, index);
                    continue;
                }
                let c=&mut convs[index];
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
            previous_edit=Some(conversation_size_edit(convs,i)?);
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
        refill_whiteboard_details(&team, &mut snapshot, &priorities)?;
        if prior==Some(&snapshot) {
            db.office_projection_source.insert(team.clone(), source_revision);
            return Ok(snapshot);
        }
        if let Some(prior)=prior {
            let next=prior["seq"].as_u64().filter(|n|*n<9007199254740991).ok_or_else(||HostError::new("sequence_conflict","Office序号已达安全整数上限"))?+1;
            snapshot["seq"]=json!(next);
        }
        let state=if previous.as_ref().is_some_and(|(_,s)|s=="halted"){ "halted" }else{"pending"};
        db.save_office_projection(&team,&snapshot,state)?;
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
    sync_internal(host, config, team, client, halted, address, false).await
}

/// 功能：为已接受的Office删除动作同步完整快照并核对回执。
/// 参数：host/config/team/client为既有可信服务，address为聊天监听地址。
/// 返回：真实applied、cached或ignored状态，不能当作浏览器已消费。
/// 错误：连接、认证、版本冲突和协议错误不转换为删除成功。
/// 状态：保留同一外发门与持久seq；删除后的快照先于UI确认。
/// 并发与取消：调用方持有UI优先标记，仅本次同步允许在该标记下扫描。
/// 权限：只供可信UI动作使用，不改变后台让行或会话范围校验。
/// 关联：REQ-OP-014、DES-OP-007/009；原删除动作回执契约。
pub(super) async fn sync_for_ui(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    client: &Client,
    address: std::net::SocketAddr,
) -> HostResult<String> {
    sync_internal(host, config, team, client, false, address, true).await
}

async fn sync_internal(
    host: &Host,
    config: &OfficeConfig,
    team: &str,
    client: &Client,
    halted: bool,
    address: std::net::SocketAddr,
    allow_ui_busy: bool,
) -> HostResult<String> {
    let gate = client.projection_gate(team).await;
    let _guard = gate.lock().await;
    if !super::workspaces::active(host, config, team).await? {
        return Ok("reserved".into());
    }
    // 发送失败后先重试同一持久版本，不能让下一次事实重建覆盖尚未发出的交接。
    // 可信UI同步仍须构建其删除/审批之后的准确状态，沿用原专用外发门。
    let pending = if allow_ui_busy {
        None
    } else {
        let team = team.to_owned();
        host.inner
            .store
            .call(move |db| {
                Ok(db
                    .office_projection(&team)?
                    .filter(|(snapshot, state)| {
                        state == "pending" && snapshot_fits_office_message(&team, snapshot)
                    })
                    .map(|(snapshot, _)| snapshot))
            })
            .await?
    };
    let snapshot = match pending {
        Some(snapshot) => snapshot,
        None => prepare_internal(host, config, team, allow_ui_busy).await?,
    };
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
    // 聊天地址是配置而非每帧业务数据。每个Client首次确认一次；Office
    // 重启丢失办公室时，下方精确404分支携带chat重新初始化，不能靠每秒全目录
    // 查询来侦测缓存。快照本身仍每轮发送并严格核对seq/状态，不降低刷新频率。
    client.register_chat(address).await?;
    let counts = snapshot["conversations"]
        .as_array()
        .into_iter()
        .flatten()
        .fold((0, 0), |(tasks, events), c| {
            (
                tasks + c["tasks"].as_array().map_or(0, Vec::len),
                events + c["events"].as_array().map_or(0, Vec::len),
            )
        });
    let request = json!({"type":"office.snapshot","teamId":team,"snapshot":snapshot});
    let bytes = serde_json::to_vec(&request)
        .map_err(HostError::storage)?
        .len();
    let receipt = match client.post(request.clone()).await {
        Err(error) if error.kind == "office_uninitialized" => {
            // 只有官方明确丢失此办公室才初始化；认证/网络/序号冲突不走此分支。
            // 初始化确认不是最终快照回执，仍重发完全相同的版本完成核对。
            client.post(json!({"type":"offices.initialize","offices":[snapshot],"chat":{"url":format!("http://{address}/office/chat")}})).await?;
            client.post(request).await
        }
        result => result,
    };
    if std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some() || receipt.is_err() {
        let result = receipt
            .as_ref()
            .map(|(_, r)| r["status"].as_str().unwrap_or("pending"))
            .unwrap_or_else(|e| e.kind.as_str());
        eprintln!("engine-host Office snapshot team={} seq={} bytes={} target={} limit={} tasks={} events={} result={}",
            team, snapshot["seq"],bytes,OFFICE_MESSAGE_TARGET,OFFICE_MESSAGE_LIMIT,counts.0,counts.1,result);
    }
    let (_, receipt) = receipt?;
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
            .office_state
            .projection_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(cache_team, snapshot.clone());
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn d142_old_terminal_handoffs_do_not_pin_unbounded_artifact_catalogues() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config: HostConfig = serde_json::from_value(json!({
            "home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},
            "provider":"echo","tools":["read_file","write_file"],"agents":{"enabled":true}
        }))
        .unwrap();
        let mut script = Vec::new();
        for _ in 0..7 {
            script.extend([
                MockMode::ToolUse { tool_name:"delegate_task".into(), input:json!({
                    "role_id":"research","title":"核验交付资料","instructions":"核验资料","acceptance":"返回结果"
                }),text:String::new() },
                MockMode::Fixed("核验完成".into()),
                MockMode::Fixed("汇总完成".into()),
            ]);
        }
        script.push(MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"pending.md","content":"等待原审批"}),
            text: String::new(),
        });
        script.push(MockMode::Fixed("保存完成".into()));
        let host = Host::with_provider(config, Arc::new(MockProvider::scripted(script)))
            .await
            .unwrap();
        let office = crate::test_support::office_config();
        let mut scopes = Vec::new();
        for index in 0..7 {
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: format!("history-{index}"),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            let run = host
                .submit_run(
                    &scope,
                    &binding.session_id,
                    format!("核验第{index}组交付资料"),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            let run_id = run.run_id.clone();
            // 本回归只测目录预算；Store发布真实blob作为有界版本夹具，不假称模型写作。
            host.inner
                .store
                .call(move |db| {
                    db.artifact_origin("http://127.0.0.1:12345")?;
                    for version in 0..20 {
                        let binding = openpaw_core::ToolCallBinding {
                            run_id: run_id.clone(),
                            call_id: format!("version-{version}"),
                            tool_name: "write_file".into(),
                            input: json!({}),
                        };
                        db.capture_artifact(
                            &binding,
                            &format!("outputs/history-{index}/file-{}.md", version % 8),
                            "text/markdown",
                            format!("第{version}版真实字节").as_bytes(),
                        )?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            let snapshot = prepare(&host, &office, "office-1")
                .await
                .expect("已结束旧交接不得阻塞后续产物快照");
            assert!(snapshot_fits_office_message("office-1", &snapshot));
            let current = snapshot["conversations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == scope.conversation_id)
                .unwrap();
            assert_eq!(
                current["whiteboard"]["result"]["artifacts"]
                    .as_array()
                    .unwrap()
                    .len(),
                8
            );
            assert_eq!(
                current["whiteboard"]["deliveries"]
                    .as_array()
                    .unwrap()
                    .len(),
                20
            );
            scopes.push(scope);
        }
        // 新活动会话的真实写入审批仍必须可见；不能以删除在途任务腾容量。
        let scope = ClientScope {
            conversation_id: "current".into(),
            ..scopes[0].clone()
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(&scope, &binding.session_id, "保存需人工确认的资料".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !host
                    .snapshot_and_subscribe(&scope)
                    .await
                    .unwrap()
                    .0
                    .pending_approvals
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for restart in [false, true] {
            if restart {
                host.inner
                    .store
                    .call(|db| {
                        db.office_projection_source.clear();
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let snapshot = prepare(&host, &office, "office-1")
                .await
                .expect("历史重建及恢复不能堵塞当前审批");
            assert!(snapshot_fits_office_message("office-1", &snapshot));
            let current = snapshot["conversations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == "current")
                .unwrap();
            assert_eq!(current["tasks"][0]["id"], format!("run:{}", run.run_id));
            assert_eq!(current["tasks"][0]["status"], "blocked");
            assert_eq!(current["employees"].as_array().unwrap().len(), 6);
            for scope in scopes
                .iter()
                .rev()
                .take(MAX_PROTECTED_BUSINESS_CONVERSATIONS)
            {
                assert!(snapshot["conversations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["id"] == scope.conversation_id));
            }
            for scope in &scopes {
                let scope = scope.clone();
                assert_eq!(
                    host.inner
                        .store
                        .call(move |db| Ok(db.artifacts(&scope)?.0.len()))
                        .await
                        .unwrap(),
                    20,
                    "退出窗口不能删除原版本"
                );
            }
        }
        assert!(!root.join("pending.md").exists());
        host.cancel_run(&scope, &run.run_id).await.unwrap();
        host.wait_idle().await;
        host.shutdown().await.unwrap();
    }

    #[test]
    fn d138_reader_uses_actual_execution_facts_without_fabricating_deliverables() {
        let tasks = vec![
            json!({"id":"goal","title":"总目标","status":"running","progress":50,"summary":"实际执行中","assigneeId":"office-lead"}),
            json!({"id":"done","title":"读取资料","status":"completed","progress":100,"summary":"读取成功","assigneeId":"data-agent"}),
            json!({"id":"active","title":"编写说明","status":"running","progress":25,"summary":"正在写入","assigneeId":"app-agent"}),
            json!({"id":"failed","title":"核查来源","status":"failed","progress":0,"summary":"请求失败","assigneeId":"code-agent"}),
        ];
        let mut board = json!({"title":"总目标","status":"running","summary":"实际执行中",
            "steps":[{"id":"done","progress":100},{"id":"active","progress":25},{"id":"failed","progress":0}]});
        structured_board(&mut board, &tasks);
        assert_eq!(board["steps"][0]["state"], "done");
        assert_eq!(board["steps"][1]["state"], "active");
        assert_eq!(board["steps"][2]["state"], "pending");
        assert_eq!(board["result"]["tasks"][0]["status"], "doing");
        assert_eq!(board["result"]["tasks"][1]["status"], "blocked");
        assert_eq!(board["result"]["tasks"][2]["progress"], 100);
        assert_eq!(board["result"]["status"], "draft");
        assert!(board["result"].get("artifacts").is_none());
        assert!(board.get("deliveries").is_none());
        assert_eq!(board["result"]["metrics"][0]["value"], "1");
        assert_eq!(board["detailSections"][0]["blocks"][0]["type"], "text");
        assert!(
            board["result"].get("sections").is_none(),
            "同一执行详情不在过程页重复两次"
        );
        let before = board.clone();
        structured_board(&mut board, &tasks);
        assert_eq!(before, board);
    }

    #[test]
    fn r30_budget_keeps_current_versions_and_never_mutates_history_records() {
        let artifacts = json!([{"id":"file","versionId":"version2","sha256":"b".repeat(64),"url":"http://127.0.0.1:1/v2"}]);
        let old = json!({"id":"delivery1","sequence":1,"createdAt":"2026-10-30T00:00:00Z","snapshotUrl":"http://127.0.0.1:1/d1"});
        let new = json!({"id":"delivery2","sequence":2,"createdAt":"2026-10-30T00:00:01Z","snapshotUrl":"http://127.0.0.1:1/d2"});
        let mut conversation = json!({"tasks":[{"id":"running","status":"running"}],"events":[{"id":"unpublished-handoff"}],"whiteboard":{"result":{"artifacts":artifacts,"sections":[{"large":"optional"}]},"deliveries":[new.clone(),old.clone()]}});
        while compact_structured_board(&mut conversation) {}
        assert_eq!(conversation["whiteboard"]["result"]["artifacts"], artifacts);
        assert_eq!(conversation["whiteboard"]["deliveries"], json!([new, old]));
        assert_eq!(conversation["tasks"][0]["id"], "running");
        assert_eq!(conversation["events"][0]["id"], "unpublished-handoff");
    }

    #[test]
    fn d138_optional_reader_content_yields_budget_before_core_tasks_and_events() {
        let tasks = vec![
            json!({"id":"goal","title":"目标","status":"completed","progress":100,"summary":"已执行","assigneeId":"office-lead"}),
        ];
        let mut board = json!({"title":"目标","status":"completed","summary":"已执行","steps":[]});
        structured_board(&mut board, &tasks);
        assert_eq!(
            board["result"]["status"], "draft",
            "执行完成不能冒充成果验收"
        );
        let mut conversation =
            json!({"id":"c","tasks":tasks,"events":[{"id":"untouched"}],"whiteboard":board});
        let facts = conversation.clone();
        assert!(compact_structured_board(&mut conversation));
        assert!(compact_structured_board(&mut conversation));
        assert!(!compact_structured_board(&mut conversation));
        assert_eq!(conversation["tasks"], facts["tasks"]);
        assert_eq!(conversation["events"], facts["events"]);
        assert_eq!(
            conversation["whiteboard"]["summary"],
            facts["whiteboard"]["summary"]
        );
    }

    #[test]
    fn d120_retained_terminal_cards_do_not_reserve_unbounded_explanations() {
        let tasks: Vec<_> = (0..24)
            .map(|i| {
                json!({
                    "id":format!("step:old:{i}"),"title":format!("第{i}项归档成果"),
                    "status":"completed","progress":100,"assigneeId":"code-agent",
                    "summary":"完整结果仍保留在原账本，重复摘要不应占满新交接空间。".repeat(40),
                    "updatedAt":"2026-10-28T00:00:00Z"
                })
            })
            .chain(std::iter::once(
                json!({"id":"run:current","title":"当前目标",
            "status":"running","progress":42,"assigneeId":"office-lead",
            "summary":"正在读取当前资料，等待同事完成核查"}),
            ))
            .collect();
        let prior = json!({"schemaVersion":"1.0","seq":1100,"workspace":{"id":"office-1"},
            "conversations":[{"id":"c","employees":[{"id":"office-lead","name":"王明"}],
            "tasks":tasks,"events":[],"whiteboard":{"title":"当前目标","status":"running",
                "detail":"当前目标详情","steps":[{"id":"run:current","title":"当前目标","progress":42}]}}]});
        let mut snapshot = prior.clone();
        compact_history_to_target(
            "office-1",
            &mut snapshot,
            Some(&prior),
            &BTreeMap::new(),
            &PublishedHistory::default(),
        )
        .unwrap();
        let bytes = serde_json::to_vec(
            &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot}),
        )
        .unwrap()
        .len();
        assert!(
            bytes <= OFFICE_MESSAGE_TARGET,
            "终态卡保留底线不应阻止可选说明收缩：{bytes}"
        );
        let current = &snapshot["conversations"][0];
        assert_eq!(current["tasks"].as_array().unwrap().len(), 25);
        assert_eq!(current["tasks"][24], prior["conversations"][0]["tasks"][24]);
        assert_eq!(current["employees"], prior["conversations"][0]["employees"]);
        assert_eq!(
            current["whiteboard"]["steps"],
            prior["conversations"][0]["whiteboard"]["steps"]
        );
        for task in current["tasks"].as_array().unwrap().iter().take(24) {
            assert_eq!(task["status"], "completed");
            assert_eq!(task["progress"], 100);
        }
    }

    #[test]
    fn d120_normal_budget_archives_history_before_current_explanations() {
        let conversations: Vec<_> = [35, 52].into_iter().enumerate().map(|(c, count)| {
            let tasks: Vec<_> = (0..count).map(|i| json!({
                "id":format!("task-{c}-{i}"),"title":format!("第{i}项独立材料整理"),
                "status":if i==count-1 {"running"} else {"completed"},
                "progress":if i==count-1 {42} else {100},"assigneeId":"code-agent",
                "summary":if i==count-1 {"正在读取当前资料；已核验42%"} else {"已有成果保留，可查看历史详情"},
                "updatedAt":format!("2026-10-28T00:{i:02}:00Z")
            })).collect();
            let events: Vec<_> = (0..count).flat_map(|i| ["task.assigned", "task.completed"].into_iter().map(move |kind| json!({
                "id":format!("{kind}:{c}:{i}"),"taskId":format!("task-{c}-{i}"),"kind":kind,
                "employeeId":"code-agent","fromEmployeeId":"office-lead",
                "summary":"已经公开的任务事件，完整成果继续保存在原始账本中",
                "occurredAt":format!("2026-10-28T00:{i:02}:00Z")
            }))).collect();
            json!({"id":format!("c-{c}"),"title":"多任务持续协作",
                "employees":[{"id":"code-agent","name":"李研","sceneActorId":"code-agent","role":"检索核验","online":true}],
                "tasks":tasks,"events":events,"whiteboard":{"title":"当前目标","summary":"当前目标说明必须先于旧事件保留","detail":"当前可读白板详情","steps":[{"id":format!("task-{c}-{}",count-1),"progress":42}]}})
        }).collect();
        let prior = json!({"schemaVersion":"1.0","seq":1099,"workspace":{"id":"office-1","name":"预算回归"},"conversations":conversations});
        let priorities = BTreeMap::from([("c-0".into(), 0), ("c-1".into(), 1)]);
        let published = PublishedHistory {
            anchors: ["task.assigned:0:34", "task.assigned:1:51"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            terminal_tasks: [35, 52]
                .into_iter()
                .enumerate()
                .flat_map(|(c, n)| (0..n - 1).map(move |i| format!("task-{c}-{i}")))
                .collect(),
        };
        let mut snapshot = prior.clone();
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            Some(&prior),
            &priorities,
            &published
        )
        .unwrap());
        let bytes = serde_json::to_vec(
            &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot}),
        )
        .unwrap()
        .len();
        assert!(
            bytes <= 48_000,
            "有足够旧终态历史时应保留正常预算，实际{bytes}"
        );
        for (index, c) in snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(c["employees"], prior["conversations"][index]["employees"]);
            assert_eq!(c["whiteboard"], prior["conversations"][index]["whiteboard"]);
            assert_eq!(
                c["tasks"].as_array().unwrap().last(),
                prior["conversations"][index]["tasks"]
                    .as_array()
                    .unwrap()
                    .last()
            );
        }
        let fitted = snapshot.clone();
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            Some(&fitted),
            &priorities,
            &published
        )
        .unwrap());
        assert_eq!(snapshot, fitted, "正常窗口不应每次继续裁剪");
        // 原账本重建会再次产生被归档事件；没有上次首次观察时间的旧结果
        // 不能凭新的观察时间挤掉保留下来的近期窗口。
        let mut rebuilt = prior.clone();
        for conversation in rebuilt["conversations"].as_array_mut().unwrap() {
            let seen = fitted["conversations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == conversation["id"])
                .unwrap();
            for event in conversation["events"].as_array_mut().unwrap() {
                if !seen["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["id"] == event["id"])
                {
                    event["occurredAt"] = json!("2026-10-28T23:59:59Z");
                }
            }
        }
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut rebuilt,
            Some(&fitted),
            &priorities,
            &published
        )
        .unwrap());
        assert_eq!(
            rebuilt, fitted,
            "已归档旧事件不能因新的观察时间重新进入窗口"
        );
    }

    #[test]
    fn d127_hidden_terminal_cards_do_not_pin_unbounded_events_after_stop() {
        let events: Vec<_> = (0..180)
            .flat_map(|i| {
                ["task.assigned", "task.completed"]
                    .into_iter()
                    .map(move |kind| {
                        json!({
                            "id":format!("{kind}:{i}"),"taskId":format!("old-{i}"),"kind":kind,
                            "employeeId":"code-agent","fromEmployeeId":"office-lead",
                            "summary":"已公开的真实成果，完整历史仍保存在原始账本中",
                            "occurredAt":"2026-10-29T05:00:00Z"
                        })
                    })
            })
            .collect();
        let prior = json!({"schemaVersion":"1.0","seq":1295,"workspace":{"id":"office-1","name":"停止后空闲"},
            "conversations":[{"id":"c","title":"持续协作","employees":[{"id":"office-lead","name":"王明"}],
            "tasks":[{"id":"run:parent","status":"running"}],"events":events,
            "whiteboard":{"title":"持续协作","detail":"真实成果与停止原因","steps":[]}}]});
        let mut snapshot = prior.clone();
        snapshot["conversations"][0]["tasks"] = json!([]);
        snapshot["conversations"][0]["whiteboard"]["status"] = json!("failed");
        let published = PublishedHistory {
            anchors: BTreeSet::from(["task.assigned:179".into()]),
            terminal_tasks: (0..180).map(|i| format!("old-{i}")).collect(),
        };
        let priorities = BTreeMap::new();
        assert!(
            fit_snapshot_with_history(
                "office-1",
                &mut snapshot,
                Some(&prior),
                &priorities,
                &published
            )
            .unwrap(),
            "人物当前窗口没有终态卡时，已公开历史也必须能归档，以发出停止终态"
        );
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        assert!(snapshot["conversations"][0]["tasks"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            snapshot["conversations"][0]["whiteboard"]["status"],
            "failed"
        );
        assert!(snapshot["conversations"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == "task.assigned:179"));
        let stable = snapshot.clone();
        compact_history_to_target(
            "office-1",
            &mut snapshot,
            Some(&stable),
            &priorities,
            &published,
        )
        .unwrap();
        assert_eq!(snapshot, stable, "已压缩窗口重复处理不得抖动或重放");
    }

    #[test]
    fn d127_hidden_first_terminal_and_visible_wait_remain_until_published() {
        let mut current = json!({"conversations":[{"id":"c","tasks":[{"id":"wait","status":"blocked"}],
            "events":[{"id":"done:new","taskId":"new","kind":"task.completed","occurredAt":"2026-10-29T05:01:00Z"},
            {"id":"wait:event","taskId":"wait","kind":"task.blocked","occurredAt":"2026-10-29T05:01:00Z"}],"whiteboard":{"steps":[]}}]});
        let mut old = current.clone();
        old["conversations"][0]["tasks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"new","status":"running"}));
        old["conversations"][0]["events"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let published = PublishedHistory {
            terminal_tasks: BTreeSet::from(["new".into()]),
            ..Default::default()
        };
        let before = current.clone();
        assert!(
            compact_seen_history(&mut current, &old, &BTreeMap::new(), &published)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            current, before,
            "终态首次公开前不能因人物卡已退场而丢失新结果"
        );
        let old = current.clone();
        assert!(
            compact_seen_history(&mut current, &old, &BTreeMap::new(), &published)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            current["conversations"][0]["events"],
            json!([old["conversations"][0]["events"][1]])
        );
    }

    #[test]
    fn d120_new_terminal_event_and_live_waits_are_not_old_history() {
        let mut current = json!({"conversations":[{"id":"c","tasks":[
            {"id":"new","status":"completed"},{"id":"wait","status":"blocked"}],"events":[
            {"id":"done:new","taskId":"new","kind":"task.completed","occurredAt":"2026-10-28T00:01:00Z"},
            {"id":"wait:event","taskId":"wait","kind":"task.blocked","occurredAt":"2026-10-28T00:01:00Z"}],"whiteboard":{"steps":[]}}]});
        let mut old = current.clone();
        old["conversations"][0]["tasks"][0]["status"] = json!("running");
        old["conversations"][0]["events"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let published = PublishedHistory {
            terminal_tasks: BTreeSet::from(["new".into()]),
            ..Default::default()
        };
        let priorities = BTreeMap::new();
        let before = current.clone();
        assert!(
            compact_seen_history(&mut current, &old, &priorities, &published)
                .unwrap()
                .is_none()
        );
        assert_eq!(current, before, "新终态结果及当前等待必须先公开");
        let old = current.clone();
        assert!(
            compact_seen_history(&mut current, &old, &priorities, &published)
                .unwrap()
                .is_some()
        );
        assert_eq!(current["conversations"][0]["events"][0]["id"], "wait:event");
    }

    #[test]
    fn d120_soft_target_yields_to_unpublished_core_but_hard_boundary_does_not() {
        let tasks:Vec<_>=(0..90).map(|i|json!({"id":format!("new-{i}"),"title":"当前任务",
            "status":"running","summary":"正在读取资料".repeat(30),"progress":42,"assigneeId":"code-agent"})).collect();
        let mut snapshot = json!({"schemaVersion":"1.0","seq":1,"workspace":{"id":"office-1"},
            "conversations":[{"id":"c","employees":[],"tasks":tasks,"events":[],"whiteboard":{}}]});
        let old = snapshot.clone();
        let priorities = BTreeMap::new();
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            Some(&old),
            &priorities,
            &PublishedHistory::default()
        )
        .unwrap());
        assert_eq!(
            snapshot["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .len(),
            90
        );
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        for t in snapshot["conversations"][0]["tasks"].as_array().unwrap() {
            assert!(!t["summary"].as_str().unwrap().is_empty());
            assert_eq!(t["progress"], 42);
        }
        snapshot["conversations"][0]["tasks"] = json!((0..10)
            .flat_map(|_| old["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .cloned())
            .collect::<Vec<_>>());
        assert!(
            !fit_snapshot_with_history(
                "office-1",
                &mut snapshot,
                Some(&old),
                &priorities,
                &PublishedHistory::default()
            )
            .unwrap(),
            "核心无法容纳时明确失败，不丢弃活动任务"
        );
    }

    #[test]
    fn d98_activity_targets_do_not_expose_urls_credentials_or_parent_paths() {
        let binding = |name: &str, input| openpaw_core::ToolCallBinding {
            run_id: "r".into(),
            call_id: "c".into(),
            tool_name: name.into(),
            input,
        };
        let text = super::public_tool_activity(&binding(
            "web_fetch",
            serde_json::json!({"url":"https://user:secret@example.org/private?token=DO_NOT_LOG"}),
        ));
        assert_eq!(text, "正在读取来源 example.org");
        for path in [
            "/private/person/report.md",
            "C:\\private\\person\\report.md",
        ] {
            assert_eq!(
                super::public_tool_activity(&binding(
                    "write_file",
                    serde_json::json!({"path":path,"content":"DO_NOT_LOG"})
                )),
                "正在保存 report.md"
            );
        }
    }

    #[test]
    fn d99_whiteboard_detail_preserves_actual_task_states_and_completed_child_progress() {
        let tasks = vec![
            serde_json::json!({"title":"制作图册","status":"failed","progress":60,"summary":"保存未完成","assigneeId":"office-lead"}),
            serde_json::json!({"title":"审稿","status":"completed","progress":100,"summary":"已核验","assigneeId":"review-agent"}),
            serde_json::json!({"title":"保存文件","status":"failed","progress":0,"summary":"尚未执行","assigneeId":"office-lead"}),
        ];
        let detail = super::whiteboard_detail("制作图册", &tasks);
        assert!(detail.contains("王明 · 未完成 · 60%"));
        assert!(detail.contains("刘市 · 已完成 · 100%"));
        assert!(detail.contains("王明 · 未完成 · 0%"));
        let mut conversation =
            serde_json::json!({"tasks":tasks,"whiteboard":{"summary":"简要目标","detail":detail}});
        let before = conversation["tasks"].clone();
        assert!(super::compact_business_conversation(&mut conversation));
        assert_eq!(conversation["tasks"], before);
        assert_eq!(conversation["whiteboard"]["detail"], "");
    }
    use super::*;
    use crate::{ClientScope, HostConfig};

    #[tokio::test]
    async fn d131_active_main_tool_keeps_its_real_owner() {
        for collaboration in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":collaboration}})).unwrap();
            let host =
                Host::with_provider(config, Arc::new(openpaw_llm::MockProvider::fixed("unused")))
                    .await
                    .unwrap();
            let binding = host
                .open_session(ClientScope {
                    namespace: "office".into(),
                    slot_id: "office-1".into(),
                    conversation_id: "ownership".into(),
                })
                .await
                .unwrap();
            let session_id = binding.session_id.clone();
            host.inner.store.call(move |db| {
                let offset = db.events(&binding.session_id)?.len();
                db.append(&binding.session_id, openpaw_core::SessionEvent::Message(openpaw_core::Message::user_text("核对工作资料")))?;
                db.append(&binding.session_id, openpaw_core::SessionEvent::RunStarted {run_id:"owner-run".into()})?;
                db.append(&binding.session_id, openpaw_core::SessionEvent::ToolCallStart {call_id:"owner-read".into(), tool_name:"read_file".into()})?;
                db.save_run(RunView {run_id:"owner-run".into(),session_id:binding.session_id,state:RunState::Running,revision:0,text:String::new(),error:None,tools:vec![],text_truncated:false}, offset)?;
                db.bump("tool_started", &json!({"run_id":"owner-run","call_id":"owner-read","tool_name":"read_file","input":{"path":"first.md"}}).to_string())?;
                Ok(())
            }).await.unwrap();
            let office = crate::test_support::office_config();
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let tasks = snapshot["conversations"][0]["tasks"].as_array().unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0]["status"], "running");
            assert_eq!(
                tasks[0]["assigneeId"],
                if collaboration {
                    "office-lead"
                } else {
                    "file-agent"
                },
                "协作模式不能因工具类型更换实际运行负责人"
            );
            if collaboration {
                assert!(
                    snapshot["conversations"][0]["events"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    "未委派不能制造另一成员接手事实"
                );
            }
            let details = crate::office::details::render(
                &host,
                &ClientScope {
                    namespace: "office".into(),
                    slot_id: "office-1".into(),
                    conversation_id: "ownership".into(),
                },
                &session_id,
                "owner-member",
                json!({"kind":"member","employeeId":"file-agent","actorId":"file-agent"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert!(
                details["content"]["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| {
                        f["label"] == "当前状态"
                            && f["value"]
                                == if collaboration {
                                    "尚无此成员的独立任务"
                                } else {
                                    "正在处理已关联的会话操作"
                                }
                    }),
                "成员详情必须与真实负责人及协作模式一致：{details}"
            );
            if collaboration {
                assert!(
                    !details["content"]["fields"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["label"] == "已关联操作"),
                    "不能把主Run工具计入未委派成员详情"
                );
            }
            host.shutdown().await.unwrap();
        }
    }

    #[test]
    fn r30_optional_reader_history_yields_before_observed_team_events() {
        let mut conversations: Vec<_> = (0..24).map(|i| json!({
            "id":format!("old-{i}"),"tasks":[],"events":[],
            "whiteboard":{"title":"旧目标","detailSections":[{"kind":"custom","id":"details","title":"过程","blocks":[{"type":"text","text":"旧过程说明".repeat(160)}]}]}
        })).collect();
        let tasks: Vec<_> = (0..5).map(|i| json!({"id":format!("t-{i}"),"assigneeId":format!("member-{i}"),"status":"completed","progress":100})).collect();
        let events: Vec<_> = (0..5).flat_map(|i| ["task.assigned","task.started","task.completed"].into_iter().enumerate().map(move |(phase,kind)| json!({"id":format!("{kind}:{i}"),"kind":kind,"taskId":format!("t-{i}"),"summary":"真实观察到的团队执行事实","occurredAt":format!("2026-10-30T08:00:0{phase}Z")}))).collect();
        conversations.push(
            json!({"id":"current","tasks":tasks,"events":events,"whiteboard":{"title":"团队目标"}}),
        );
        let mut snapshot = json!({"conversations":conversations});
        let original = snapshot.clone();
        let priorities: BTreeMap<_, _> = (0..24)
            .map(|i| (format!("old-{i}"), i))
            .chain(std::iter::once(("current".into(), 25)))
            .collect();
        let published = PublishedHistory {
            anchors: BTreeSet::new(),
            terminal_tasks: (0..5).map(|i| format!("t-{i}")).collect(),
        };
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot})
            )
            .unwrap()
            .len()
                > OFFICE_MESSAGE_TARGET
        );
        compact_history_to_target(
            "office-1",
            &mut snapshot,
            Some(&original),
            &priorities,
            &published,
        )
        .unwrap();
        assert!(
            serde_json::to_vec(
                &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot})
            )
            .unwrap()
            .len()
                <= OFFICE_MESSAGE_TARGET
        );
        assert_eq!(
            snapshot["conversations"][24], original["conversations"][24],
            "可选旧阅读器过程不应挤掉当前五人的已公开动态和任务"
        );
        assert_ne!(snapshot["conversations"][0], original["conversations"][0]);
    }

    #[test]
    fn d114_four_long_conversations_keep_markdown_under_snapshot_pressure() {
        let conversations: Vec<_> = (0..4).map(|c| {
            let tasks: Vec<_> = (0..12).map(|i| json!({
                "id":format!("task-{c}-{i}"),
                "title":format!("第{i}项{}", "核实资料并保存真实成果😀\"".repeat(3)),
                "status":if i==0 {"failed"} else {"completed"},
                "progress":if i==0 {37} else {100},
                "assigneeId":if i==0 {"office-lead"} else {"review-agent"},
                "summary":"真实说明与来源核验\n".repeat(40)
            })).collect();
            json!({"id":format!("c-{c}"),"title":"真实长会话", "employees":[],"tasks":tasks,
                "events":[{"id":format!("assignment-{c}"),"kind":"task.assigned","taskId":format!("task-{c}-0"),"summary":"尚未公开的交接"}],
                "whiteboard":{"template":"task-board-v1","title":"核实资料并保存成果","status":"failed","summary":"本轮未完成",
                    "detail":whiteboard_detail("核实资料并保存成果", &tasks),
                    "steps":[{"id":format!("task-{c}-0"),"title":"主任务","progress":37}]}})
        }).collect();
        let mut snapshot = json!({"schemaVersion":"1.0","seq":41,"workspace":{"id":"office-1"},"conversations":conversations});
        let original = snapshot.clone();
        let priorities = (0..4).map(|i| (format!("c-{i}"), i)).collect();
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            None,
            &priorities,
            &PublishedHistory::default()
        )
        .unwrap());
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        for (i, conversation) in snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let detail = conversation["whiteboard"]["detail"].as_str().unwrap();
            assert!(detail.starts_with("# "), "长会话详情不能被清空而回退旧白板");
            assert!(detail.contains("王明 · 未完成 · 37%"));
            assert!(detail.contains("刘市 · 已完成 · 100%"));
            assert_eq!(
                conversation["events"],
                original["conversations"][i]["events"]
            );
            assert_eq!(
                conversation["whiteboard"]["steps"],
                original["conversations"][i]["whiteboard"]["steps"]
            );
            for (a, b) in conversation["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .zip(original["conversations"][i]["tasks"].as_array().unwrap())
            {
                for key in ["id", "title", "status", "progress", "assigneeId"] {
                    assert_eq!(a[key], b[key]);
                }
            }
        }
        let fitted = snapshot.clone();
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            Some(&fitted),
            &priorities,
            &PublishedHistory::default()
        )
        .unwrap());
        assert_eq!(snapshot, fitted, "详情回补与重复拟合不能抖动");
    }

    #[test]
    fn d114_refill_uses_remaining_envelope_without_mixing_other_runs() {
        let mut tasks =
            vec![json!({"id":"run:chat","title":"核验成果","status":"completed","progress":100})];
        tasks.push(json!({"id":"run:business","title":"核验成果","status":"completed","progress":100,"assigneeId":"office-lead"}));
        tasks.extend((0..12).map(|i| json!({"id":format!("step:business:{i}"),"title":format!("成果第{i}部分"),"status":"completed","progress":100,"assigneeId":"review-agent","summary":"真实成果说明"})));
        tasks.push(json!({"id":"run:older","title":"无关的旧业务","status":"failed","progress":0}));
        let mut conversation = json!({"id":"current","tasks":tasks,"whiteboard":{"title":"核验成果","status":"completed","detail":"","steps":[{"id":"step:business:0"}]}});
        let short = bounded_whiteboard_detail(&conversation, 3).unwrap();
        assert!(
            short.contains("刘市 · 已完成 · 100%"),
            "同名普通追问不能取代步骤ID对应的业务Run"
        );
        assert!(!short.contains("无关的旧业务"));
        conversation["whiteboard"]["detail"] = json!(short);
        let mut snapshot = json!({"schemaVersion":"1.0","workspace":{"id":"office-1"},"seq":9007199254740990_u64,"conversations":[conversation,{"id":"archived","tasks":[],"whiteboard":{"title":"旧入口","detail":""}}]});
        let priorities = BTreeMap::from([("current".into(), 2), ("archived".into(), 1)]);
        let bytes = serde_json::to_vec(
            &json!({"type":"office.snapshot","teamId":"office-1","snapshot":snapshot}),
        )
        .unwrap()
        .len();
        snapshot["padding"] = json!("x".repeat(OFFICE_MESSAGE_LIMIT - bytes - 40));
        let before = snapshot.clone();
        refill_whiteboard_details("office-1", &mut snapshot, &priorities).unwrap();
        assert_eq!(snapshot, before, "余量不足不能挤掉其他内容");
        snapshot.as_object_mut().unwrap().remove("padding");
        refill_whiteboard_details("office-1", &mut snapshot, &priorities).unwrap();
        let detail = snapshot["conversations"][0]["whiteboard"]["detail"]
            .as_str()
            .unwrap();
        assert!(detail.contains("成果第0部分") && detail.contains("成果第11部分"));
        assert!(
            detail.contains("## 当前目标\n\n核验成果\n\n"),
            "说明被裁剪后，当前目标仍须可读"
        );
        assert!(!detail.contains("不要改变业务成果") && !detail.contains("无关的旧业务"));
        assert_eq!(
            snapshot["conversations"][0]["tasks"],
            before["conversations"][0]["tasks"]
        );
        assert_eq!(snapshot["conversations"][1]["whiteboard"]["detail"], "");
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        let restored = snapshot.clone();
        refill_whiteboard_details("office-1", &mut snapshot, &priorities).unwrap();
        assert_eq!(snapshot, restored);
    }

    #[test]
    fn d118_unbounded_wall_tracks_live_work_without_relabelling_old_blocked() {
        let tasks = vec![
            json!({"id":"parent","status":"running","progress":75}),
            json!({"id":"old-done","status":"completed","progress":100}),
            json!({"id":"old-unverified","status":"blocked","progress":50}),
            json!({"id":"recent-done","status":"completed","progress":100}),
            json!({"id":"live-writing","status":"running","progress":40}),
            json!({"id":"live-approval","status":"blocked","progress":25}),
            json!({"id":"queued","status":"queued","progress":0}),
        ];
        let original = tasks.clone();
        let active = ["live-writing", "live-approval", "queued"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let ids = |window: Vec<&Value>| {
            window
                .iter()
                .map(|t| t["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(whiteboard_window(&tasks, &active, true)),
            ["live-writing", "live-approval", "queued"]
        );
        assert_eq!(
            ids(whiteboard_window(&tasks, &active, false)),
            ["old-done", "old-unverified", "recent-done"],
            "有限计划不能改变原验收窗口"
        );
        let only_writer = ["live-writing".to_owned()].into_iter().collect();
        assert!(
            ids(whiteboard_window(&tasks, &only_writer, true)).contains(&"live-writing".to_owned())
        );
        assert_eq!(
            ids(whiteboard_window(&tasks, &BTreeSet::new(), true)),
            ["live-writing", "live-approval", "queued"],
            "无在途时保留最近三项原状态"
        );
        assert!(whiteboard_window(&tasks[..1], &active, true).is_empty());
        assert_eq!(tasks, original, "窗口选择不改历史失败/待核验和进度事实");
    }

    #[test]
    fn d117_snapshot_pressure_keeps_current_action_and_trims_terminal_detail() {
        let current = "正在核对来源正文 https://example.org/spec；已核验进度25%";
        let queued = "已登记委派，等待接受执行";
        let mut snapshot = json!({
            "schemaVersion":"1.0", "seq":1, "workspace":{"id":"office-1"},
            "conversations":[{
                "id":"current", "employees":[], "events":[], "whiteboard":{"title":"当前任务","detail":""},
                "tasks":[
                    {"id":"old","status":"completed","progress":100,"summary":"已归档历史详情".repeat(5000)},
                    {"id":"live","status":"running","progress":25,"summary":current},
                    {"id":"wait","status":"queued","progress":0,"summary":queued},
                    {"id":"approval","status":"blocked","progress":50,"summary":"等待对本次写入操作的确认😀".repeat(100)}
                ]
            }]
        });
        // 已公开的旧结果可以压缩，当前执行/等待说明不能因为可选而一起消失。
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        assert!(
            fit_protected_snapshot_details("office-1", &mut snapshot, &BTreeMap::new()).unwrap()
        );
        let conversations = snapshot["conversations"].as_array_mut().unwrap();
        while compact_protected_details(conversations, &BTreeMap::new())
            .unwrap()
            .is_some()
        {}
        let tasks = &snapshot["conversations"][0]["tasks"];
        assert!(tasks[0].get("summary").is_none());
        assert_eq!(tasks[1]["summary"], current, "不能裁掉正在执行的真实动作");
        assert_eq!(tasks[2]["summary"], queued, "排队不是无声卡死");
        let waiting = tasks[3]["summary"].as_str().expect("审批说明仍须可见");
        assert!(!waiting.is_empty());
        assert!(waiting.encode_utf16().count() <= 96);
        assert_eq!(tasks[1]["status"], "running");
        assert_eq!(tasks[1]["progress"], 25);
        assert!(snapshot_fits_office_message("office-1", &snapshot));
    }

    #[test]
    fn incremental_message_budget_matches_full_encoding_through_compaction_and_removal() {
        for count in [1, 2, 30] {
            let conversations: Vec<_> = (0..count).map(|index| json!({
                "id":format!("会话-{index}"), "title":"中文😀与转义\"标题", "events":[{"summary":"旧事件\n内容"}],
                "employees":[{"id":"person","role":"职责","online":true,"sceneActorId":"actor"}],
                "tasks":[{"id":"task","summary":"真实结果","status":"completed","progress":100}],
                "whiteboard":{"tagline":"标签","note":"说明","description":"中文\n结果", "link":{"label":"旧链接","url":"/old"},"steps":[]}
            })).collect();
            let mut snapshot = json!({"schemaVersion":"1.0","seq":1,"workspace":{"id":"团队"},"conversations":conversations});
            let full_size = |value: &Value| {
                serde_json::to_vec(
                    &json!({"type":"office.snapshot","teamId":"团队","snapshot":value}),
                )
                .unwrap()
                .len()
            };
            let mut size = full_size(&snapshot);
            let mut steps = 0;
            while !snapshot["conversations"].as_array().unwrap().is_empty() {
                let all = snapshot["conversations"].as_array_mut().unwrap();
                let edit = conversation_size_edit(all, 0).unwrap();
                if !compact_business_conversation(&mut all[0]) {
                    all.remove(0);
                }
                size = message_size_after_edit(&snapshot, size, edit).unwrap();
                assert_eq!(
                    size,
                    full_size(&snapshot),
                    "每一步必须等于完整UTF-8封套，包含删最后一项和数组逗号"
                );
                steps += 1;
                assert!(steps < 1000);
            }
        }
    }

    #[test]
    fn d104_equal_time_events_cannot_use_random_ids_as_causal_order() {
        let events = json!([
            {"id":"a-final","kind":"task.completed","taskId":"t","occurredAt":"2026-10-27T00:00:00Z","summary":"已完成"},
            {"id":"z-start","kind":"task.started","taskId":"t","occurredAt":"2026-10-27T00:00:00Z","summary":"执行中"},
            {"id":"other","kind":"task.completed","taskId":"other","occurredAt":"2026-10-27T00:00:01Z","summary":"另一任务"}
        ]);
        let mut conversations =
            vec![json!({"id":"c","events":events,"employees":[],"tasks":[],"whiteboard":null})];
        let before = conversations.clone();
        assert!(
            compact_protected_details(&mut conversations, &BTreeMap::new())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            conversations, before,
            "同毫秒事件无先后证明，随机ID不能证明过时；其他任务也不提供证明"
        );
    }

    #[test]
    fn d104_protected_catalogue_keeps_latest_and_assignment_events_under_pressure() {
        let conversations: Vec<_> = (0..4).map(|c| {
            let tasks: Vec<_> = (0..24).map(|i| json!({
                "id":format!("task-{c}-{i}"),"title":format!("第{i}项真实目标：{}","核验多人分工并保存交付文档".repeat(4)),
                "status":"completed","progress":100,"assigneeId":"code-agent"
            })).collect();
            let events: Vec<_> = (0..24).flat_map(|i| {
                ["task.assigned", "task.started", "task.blocked", "task.completed"].into_iter().enumerate().map(move |(phase, kind)| json!({
                    "id":format!("event-{c}-{i}-{phase}"),"taskId":format!("task-{c}-{i}"),
                    "kind":kind,"employeeId":"code-agent","fromEmployeeId":"office-lead",
                    "occurredAt":format!("2026-10-27T00:00:0{phase}Z"),"summary":"真实任务动态"
                }))
            }).collect();
            json!({"id":format!("c-{c}"),"title":"多轮真实任务","employees":[],"tasks":tasks,"events":events,
                "whiteboard":{"status":"completed","steps":[{"id":"goal","title":"本轮已交付","progress":100}]}})
        }).collect();
        let mut snapshot = json!({"schemaVersion":"1.0","seq":1,"workspace":{"id":"office-1"},"conversations":conversations});
        let original = snapshot.clone();
        let priorities = (0..4).map(|i| (format!("c-{i}"), i)).collect();
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        assert!(
            fit_protected_snapshot_details("office-1", &mut snapshot, &priorities).unwrap(),
            "已被新事实取代的动态不能阻断受保护会话审批"
        );
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        for (index, conversation) in snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let old = &original["conversations"][index];
            assert_eq!(conversation["tasks"], old["tasks"]);
            assert_eq!(conversation["employees"], old["employees"]);
            assert_eq!(conversation["whiteboard"], old["whiteboard"]);
            for event in
                old["events"].as_array().unwrap().iter().filter(|e| {
                    matches!(e["kind"].as_str(), Some("task.assigned" | "task.completed"))
                })
            {
                assert!(
                    conversation["events"].as_array().unwrap().contains(event),
                    "分派身份和每项最新事实必须保留"
                );
            }
        }
        let fitted = snapshot.clone();
        assert!(fit_protected_snapshot_details("office-1", &mut snapshot, &priorities).unwrap());
        assert_eq!(snapshot, fitted, "已符合预算不能继续丢弃动态");
    }

    #[test]
    fn d111_seen_history_cannot_block_six_new_handoffs_or_error_state() {
        let conversations:Vec<_>=(0..4).map(|c| {
            let tasks:Vec<_>=(0..32).map(|i|json!({"id":format!("task-{c}-{i}"),"title":format!("真实任务{i}"),"status":"completed","progress":100,"assigneeId":"code-agent","updatedAt":"2026-10-28T00:00:00Z"})).collect();
            let events:Vec<_>=(0..32).flat_map(|i| ["task.assigned","task.completed"].map(move |kind|json!({"id":format!("{kind}:{c}:{i}"),"taskId":format!("task-{c}-{i}"),"employeeId":"code-agent","fromEmployeeId":"office-lead","kind":kind,"occurredAt":format!("2026-10-28T00:{i:02}:00Z"),"summary":"已保存且可从原账本查询的真实任务结果"}))).collect();
            json!({"id":format!("c-{c}"),"title":"历史与当前共存","employees":[{"id":"code-agent","name":"李研","sceneActorId":"code-agent","online":true}],"tasks":tasks,"events":events,"whiteboard":{"steps":[]}})
        }).collect();
        let mut prior = json!({"schemaVersion":"1.0","seq":21,"workspace":{"id":"office-1","name":"真实测试"},"conversations":conversations});
        let mut snapshot = prior.clone();
        for i in 32..38 {
            snapshot["conversations"][3]["tasks"].as_array_mut().unwrap().push(json!({"id":format!("new-{i}"),"title":"尚未同步的交接","status":"queued","progress":0,"assigneeId":"app-agent"}));
            snapshot["conversations"][3]["events"].as_array_mut().unwrap().push(json!({"id":format!("assign:new-{i}"),"taskId":format!("new-{i}"),"kind":"task.assigned","employeeId":"app-agent","fromEmployeeId":"code-agent","occurredAt":format!("2026-10-28T01:{i}:00Z"),"summary":"新交接不能因历史占满而丢弃"}));
        }
        snapshot["conversations"][3]["tasks"].as_array_mut().unwrap().push(json!({"id":"run:failed","title":"网络故障真实终态","status":"failed","progress":31,"assigneeId":"office-lead"}));
        let published = PublishedHistory {
            anchors: (0..4).map(|c| format!("task.assigned:{c}:31")).collect(),
            terminal_tasks: (0..4)
                .flat_map(|c| (0..32).map(move |i| format!("task-{c}-{i}")))
                .collect(),
        };
        let rebuilt = snapshot.clone();
        for container in [&mut prior, &mut snapshot] {
            for conv in container["conversations"].as_array_mut().unwrap() {
                for event in conv["events"].as_array_mut().unwrap() {
                    event["occurredAt"] = json!("2026-10-28T00:00:00Z");
                }
                conv["events"]
                    .as_array_mut()
                    .unwrap()
                    .sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
            }
        }
        let priorities = (0..4).map(|i| (format!("c-{i}"), i)).collect();
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        assert!(
            fit_snapshot_with_history(
                "office-1",
                &mut snapshot,
                Some(&prior),
                &priorities,
                &published
            )
            .unwrap(),
            "已公开历史不能让新交接与错误终态停更"
        );
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        for id in &published.anchors {
            assert!(
                snapshot["conversations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|e| e["id"] == *id)),
                "原委派顺序的最后公开项必须保留，不受同时间/倒序ID影响"
            );
        }

        let current = &snapshot["conversations"][3];
        for i in 32..38 {
            assert!(current["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == format!("new-{i}") && t["status"] == "queued"));
            assert!(current["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == format!("assign:new-{i}")));
        }
        assert!(current["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "run:failed" && t["status"] == "failed"));
        for (i, c) in snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(c["employees"], prior["conversations"][i]["employees"]);
        }
        // 当前已经公开后作为下次裁剪依据，不能依赖删除账本或提升封套上限。
        prior = snapshot.clone();
        assert!(fit_snapshot_with_history(
            "office-1",
            &mut snapshot,
            Some(&prior),
            &priorities,
            &published
        )
        .unwrap());
        assert_eq!(snapshot, prior);
        for _ in 0..3 {
            let mut next = rebuilt.clone();
            assert!(
                fit_snapshot_with_history(
                    "office-1",
                    &mut next,
                    Some(&prior),
                    &priorities,
                    &published
                )
                .unwrap(),
                "已归档事实重建后不能再次堵塞"
            );
            assert!(snapshot_fits_office_message("office-1", &next));
            prior = next;
        }
    }

    #[test]
    fn d101_protected_multiturn_optional_details_cannot_block_approval_snapshot() {
        let members: Vec<_> = crate::agents::ROLES
            .iter()
            .map(|r| {
                json!({
                    "id":r.employee(),"name":r.name,"sceneActorId":r.actor(),"online":true,"role":r.duty
                })
            })
            .collect();
        let conversations: Vec<_> = (0..4).map(|c| {
            let tasks: Vec<_> = (0..24).map(|i| json!({
                "id":format!("task-{c}-{i}"),"title":format!("第{i}项真实目标"),
                "status":if i==0 {"blocked"} else {"completed"},"progress":if i==0 {42} else {100},
                "assigneeId":"code-agent","summary":"真实执行与验收说明".repeat(24)
            })).collect();
            json!({"id":format!("c-{c}"),"title":"多轮真实任务", "employees":members,"tasks":tasks,
                "events":[{"id":format!("assign-{c}"),"kind":"task.assigned","taskId":format!("task-{c}-0"),"employeeId":"code-agent","fromEmployeeId":"office-lead","occurredAt":"2026-10-27T00:00:00Z","summary":"执行分工交接"}],
                "whiteboard":{"template":"task-board-v1","title":"多轮真实任务","status":"blocked","summary":"等待确认","detail":"可选历史详情".repeat(600),"steps":[{"id":format!("task-{c}-0"),"title":"等待确认","progress":42}]}})
        }).collect();
        let mut snapshot = json!({"schemaVersion":"1.0","seq":1,"workspace":{"id":"office-1","name":"测试团队"},"conversations":conversations});
        let original = snapshot.clone();
        assert!(!snapshot_fits_office_message("office-1", &snapshot));
        let priorities = (0..4).map(|i| (format!("c-{i}"), i)).collect();
        let mut iterations = 0;
        while !snapshot_fits_office_message("office-1", &snapshot) {
            let before = serde_json::to_vec(&snapshot).unwrap().len();
            assert!(
                compact_protected_details(
                    snapshot["conversations"].as_array_mut().unwrap(),
                    &priorities
                )
                .unwrap()
                .is_some(),
                "活动/近期业务可选字段不得阻断审批同步"
            );
            assert!(serde_json::to_vec(&snapshot).unwrap().len() < before);
            iterations += 1;
            assert!(iterations < 1000);
        }
        for (index, conv) in snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let old = &original["conversations"][index];
            assert_eq!(conv["id"], old["id"]);
            assert_eq!(conv["whiteboard"]["steps"], old["whiteboard"]["steps"]);
            assert_eq!(conv["whiteboard"]["status"], old["whiteboard"]["status"]);
            for (a, b) in conv["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .zip(old["tasks"].as_array().unwrap())
            {
                for key in ["id", "title", "status", "progress", "assigneeId"] {
                    assert_eq!(a[key], b[key]);
                }
            }
            assert_eq!(conv["tasks"].as_array().unwrap().len(), 24);
            assert_eq!(conv["events"], old["events"], "不能移除或重写交接事件");
            for (a, b) in conv["employees"]
                .as_array()
                .unwrap()
                .iter()
                .zip(old["employees"].as_array().unwrap())
            {
                for key in ["id", "name", "sceneActorId", "online"] {
                    assert_eq!(a[key], b[key]);
                }
            }
        }
        while compact_protected_details(
            snapshot["conversations"].as_array_mut().unwrap(),
            &priorities,
        )
        .unwrap()
        .is_some()
        {}
        let minimal = snapshot.clone();
        assert!(compact_protected_details(
            snapshot["conversations"].as_array_mut().unwrap(),
            &priorities
        )
        .unwrap()
        .is_none());
        assert_eq!(snapshot, minimal, "没有可选预算时保持核心，不删除活动会话");
    }

    #[test]
    fn exhausted_old_business_does_not_block_handoff_budget() {
        let minimal = json!({"id":"oldest","title":"旧业务","events":[],"employees":[],"tasks":[],
            "whiteboard":{"tagline":"","note":"","description":"","link":{"label":"","url":""},"steps":[]}});
        let mut newer = minimal.clone();
        newer["id"] = json!("newer");
        newer["events"] = json!([{"id":"old-activity","summary":"可裁剪历史"}]);
        let protected = json!({"id":"current","tasks":[{"id":"real-current","status":"completed","progress":100}]});
        let mut conversations = vec![minimal.clone(), newer, protected.clone()];
        assert!(!compact_business_conversation(&mut minimal.clone()));
        let mut prior_size = serde_json::to_vec(&conversations).unwrap().len();
        let mut iterations = 0;
        while conversations.len() > 1 {
            compact_business_history(&mut conversations, 0);
            let size = serde_json::to_vec(&conversations).unwrap().len();
            assert!(size < prior_size, "每次必须释放真实预算");
            prior_size = size;
            iterations += 1;
            assert!(iterations < 10);
            assert_eq!(conversations.last(), Some(&protected));
        }
        assert_eq!(iterations, 3);
        assert_eq!(conversations, vec![protected]);
    }

    #[tokio::test]
    async fn completed_delegations_survive_new_revision_and_restart_without_replaying() {
        use openpaw_llm::mock::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
        let delegate = |role: &str| MockMode::ToolUse {
            tool_name: "delegate_task".into(),
            input: json!({"role_id":role,"title":format!("{role}核验"),"instructions":"核验现有信息","acceptance":"返回核验结果"}),
            text: String::new(),
        };
        let provider = std::sync::Arc::new(MockProvider::scripted(vec![
            delegate("research"),
            MockMode::Fixed("路线核验完成".into()),
            delegate("organize"),
            MockMode::Fixed("美食核验完成".into()),
            MockMode::Fixed("已汇总两份核验结果".into()),
        ]));
        let host = Host::with_provider(config, provider).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "trip".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(
                &scope,
                &binding.session_id,
                "两位同事分别核验路线和美食".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        assert_eq!(
            host.get_run(&scope, &run.run_id).await.unwrap().state,
            RunState::Completed
        );
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        let first = prepare(&host, &office, "office-1").await.unwrap();
        let trip = |snapshot: &Value| {
            snapshot["conversations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == "trip")
                .unwrap()
                .clone()
        };
        assert_eq!(trip(&first)["tasks"].as_array().unwrap().len(), 3);
        // 新绑定增加事实revision，但没有新的协作事实，不应重写交接。
        host.open_session(ClientScope {
            conversation_id: "unrelated".into(),
            ..scope.clone()
        })
        .await
        .unwrap();
        let rebuilt = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(trip(&rebuilt)["tasks"].as_array().unwrap().len(), 3);
        let final_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let final_trip = trip(&final_snapshot);
        assert_eq!(final_trip["tasks"][0]["status"], "completed");
        assert_eq!(
            final_trip["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "task.completed")
                .count(),
            2
        );
        // 模拟重启丢失进程内基线，持久快照中的旧交接保持身份。
        host.inner
            .store
            .call(|db| {
                db.office_projection_source.clear();
                Ok(())
            })
            .await
            .unwrap();
        let restored = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(trip(&restored)["tasks"][0]["status"], "completed");
        let assignments = |c: &Value| {
            c["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "task.assigned")
                .map(|e| e["id"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(assignments(&trip(&restored)), assignments(&final_trip));
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
    fn terminal_history_requires_an_actual_previous_fact_revision() {
        assert!(!is_historical_terminal(12, None));
        assert!(!is_historical_terminal(12, Some(11)));
        assert!(is_historical_terminal(12, Some(12)));
        assert!(is_historical_terminal(11, Some(12)));
    }

    #[tokio::test]
    async fn d123_queued_cards_are_visible_but_handoffs_follow_actual_execution() {
        d123_verify_execution_queue(["organize", "organize", "organize"], false).await;
    }

    #[tokio::test]
    async fn d123_cross_role_start_does_not_publish_another_members_queued_handoff() {
        d123_verify_execution_queue(["organize", "organize", "audit"], false).await;
    }

    #[tokio::test]
    async fn d123_cancelling_queued_work_does_not_invent_a_handoff() {
        d123_verify_execution_queue(["organize", "organize", "organize"], true).await;
    }

    async fn d123_verify_execution_queue(roles: [&str; 3], cancel: bool) {
        use openpaw_core::LlmProvider;
        use openpaw_llm::{MockMode, MockProvider};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Gate {
            parent: MockProvider,
            parent_requests: AtomicUsize,
            parent_release: tokio::sync::Semaphore,
            entered: [AtomicUsize; 3],
            release: [tokio::sync::Semaphore; 3],
        }
        #[async_trait::async_trait]
        impl LlmProvider for Gate {
            async fn complete(
                &self,
                request: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.parent.complete(request).await
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
                let child = request
                    .messages
                    .iter()
                    .flat_map(|m| &m.content)
                    .find_map(|block| {
                        let openpaw_core::ContentBlock::Text { text } = block else {
                            return None;
                        };
                        text.lines().find_map(|line| {
                            line.strip_prefix("要求：d123-child-")
                                .and_then(|i| i.parse::<usize>().ok())
                        })
                    });
                if let Some(index) = child {
                    self.entered[index].fetch_add(1, Ordering::SeqCst);
                    self.release[index].acquire().await.unwrap().forget();
                    return MockProvider::fixed(format!("独立成果{index}已完成"))
                        .stream(request)
                        .await;
                }
                if self.parent_requests.fetch_add(1, Ordering::SeqCst) == 3 {
                    self.parent_release.acquire().await.unwrap().forget();
                }
                self.parent.stream(request).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.parent.capabilities()
            }
            fn default_model(&self) -> &str {
                self.parent.default_model()
            }
            fn name(&self) -> &str {
                "d123-execution-gate"
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":[],"agents":{"enabled":true}})).unwrap();
        let provider = Arc::new(Gate {
            parent: MockProvider::scripted((0..3).map(|i| MockMode::ToolUse {
                tool_name: "delegate_task".into(),
                input: json!({"role_id":roles[i],"title":format!("独立待办{i}"),"instructions":format!("d123-child-{i}"),"acceptance":"返回真实有限成果","wait_for_completion":false}),
                text: String::new(),
            }).chain(std::iter::once(MockMode::Fixed("三份真实成果已收齐".into()))).collect()),
            parent_requests: AtomicUsize::new(0),
            parent_release: tokio::sync::Semaphore::new(0),
            entered: std::array::from_fn(|_| AtomicUsize::new(0)),
            release: std::array::from_fn(|_| tokio::sync::Semaphore::new(0)),
        });
        let host = Host::with_provider(config, provider.clone()).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "d123".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let parent = host
            .submit_run(&scope, &binding.session_id, "依次处理三个独立待办".into())
            .await
            .unwrap();
        let initial: BTreeSet<_> = roles
            .iter()
            .enumerate()
            .filter(|(index, role)| !roles[..*index].contains(role))
            .map(|(index, _)| index)
            .collect();
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider.parent_requests.load(Ordering::SeqCst) < 4
                || initial
                    .iter()
                    .any(|&index| provider.entered[index].load(Ordering::SeqCst) == 0)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        let conversation = |s: &Value| {
            s["conversations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == "d123")
                .unwrap()
                .clone()
        };
        let assignments = |c: &Value| {
            c["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "task.assigned")
                .map(|e| {
                    (
                        e["id"].as_str().unwrap().to_owned(),
                        e["occurredAt"].clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let first = prepare(&host, &office, "office-1").await.unwrap();
        let c = conversation(&first);
        assert_eq!(
            c["tasks"].as_array().unwrap().len(),
            4,
            "三个真实委派卡须立即可见，不能等待动画定时器"
        );
        assert_eq!(
            c["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["status"] == "queued")
                .count(),
            3 - initial.len()
        );
        let mut previous = assignments(&c);
        assert_eq!(
            previous.len(),
            initial.len(),
            "尚在成员FIFO等待的任务不能提前消费交接"
        );
        let unchanged = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(unchanged, first, "没有事实变化不能靠时间推进交接");
        // 在队列尚未出队时清掉重启可丢失的基线，也不能凭后续角色的事件
        // 推断中间排队任务已经交接。旧事件身份和时间必须保持。
        host.open_session(ClientScope {
            conversation_id: "d123-unrelated-active".into(),
            ..scope.clone()
        })
        .await
        .unwrap();
        host.inner
            .store
            .call(|db| {
                db.office_projection_source.clear();
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            assignments(&conversation(
                &prepare(&host, &office, "office-1").await.unwrap()
            )),
            previous
        );
        if cancel {
            host.cancel_run(&scope, &parent.run_id).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            let cancelled = prepare(&host, &office, "office-1").await.unwrap();
            assert_eq!(
                assignments(&conversation(&cancelled)),
                previous,
                "未实际开工的取消项不得生成交接"
            );
            assert_eq!(provider.entered[1].load(Ordering::SeqCst), 0);
            assert_eq!(provider.entered[2].load(Ordering::SeqCst), 0);
            host.shutdown().await.unwrap();
            return;
        }
        for index in 0..3 {
            provider.release[index].add_permits(1);
            let next = (index + 1..3).find(|&next| roles[next] == roles[index]);
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let id = parent.run_id.clone();
                    let done = host
                        .inner
                        .store
                        .call(move |db| {
                            let d = db.delegations(&id)?;
                            Ok(db.get_run_projection(&d[index].child_run)?.state
                                == RunState::Completed)
                        })
                        .await
                        .unwrap();
                    if done
                        && next.is_none_or(|next| provider.entered[next].load(Ordering::SeqCst) > 0)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let current = assignments(&conversation(&snapshot));
            let actual_started = provider
                .entered
                .iter()
                .filter(|count| count.load(Ordering::SeqCst) > 0)
                .count();
            assert_eq!(
                current.len(),
                actual_started,
                "交接必须逐项对应真实模型入口，不能根据委派前缀猜测"
            );
            for (id, time) in &previous {
                assert_eq!(current.get(id), Some(time), "已有交接身份及时间不能重写");
            }
            previous = current;
        }
        provider.parent_release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let final_snapshot = prepare(&host, &office, "office-1").await.unwrap();
        assert!(conversation(&final_snapshot)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "completed" && t["progress"] == 100));
        assert_eq!(assignments(&conversation(&final_snapshot)), previous);
        host.open_session(ClientScope {
            conversation_id: "unrelated-d123".into(),
            ..scope.clone()
        })
        .await
        .unwrap();
        let rebuilt = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(assignments(&conversation(&rebuilt)), previous);
        host.inner
            .store
            .call(|db| {
                db.office_projection_source.clear();
                Ok(())
            })
            .await
            .unwrap();
        let restored = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(assignments(&conversation(&restored)), previous);
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn d115_unbounded_work_does_not_reuse_initial_plan_or_rewrite_its_evidence() {
        use openpaw_llm::{MockMode, MockProvider};
        struct Gate {
            inner: MockProvider,
            requests: std::sync::atomic::AtomicUsize,
            entered: tokio::sync::Notify,
            release: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for Gate {
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
                if self
                    .requests
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    == 2
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
                "d115-stage-gate"
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":[],"agents":{"enabled":true}})).unwrap();
        let provider = std::sync::Arc::new(Gate {
            inner: MockProvider::scripted(vec![
                MockMode::ToolUse {
                    tool_name: "delegate_task".into(),
                    input: json!({"role_id":"organize","title":"整理已给定材料","instructions":"只整理本任务数据","acceptance":"有限文本"}),
                    text: String::new(),
                },
                MockMode::Fixed("已完成本项材料整理，不声称写文件".into()),
                MockMode::Fixed("下一阶段".into()),
            ]),
            requests: std::sync::atomic::AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let host = Host::with_provider(config, provider.clone()).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "d115-continuous".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(
                &scope,
                &binding.session_id,
                "持续处理独立事项直到停止".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let id = run.run_id.clone();
        // 模拟真实旧记录：启动期固定计划没有更新，但有限子执行已有可靠成果。
        // 不用模型最终文字或回写成功来“修好”这些待核验事实。
        host.inner.store.call(move |db| {
            db.bump("run_progress", &json!({"run_id":id,"steps":[{"id":"initial","title":"启动期固定步骤","state":"in_progress","evidenceCallIds":[]}]}).to_string())?;
            let state = crate::continuous::State { version:1, run_id:id, started:true, active:true, goal:"持续处理独立事项直到停止".into(), ..Default::default() };
            db.bump("continuous_task", &serde_json::to_string(&state).map_err(HostError::storage)?)?;
            Ok(())
        }).await.unwrap();
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        let snapshot = prepare(&host, &office, "office-1").await.unwrap();
        let tasks = snapshot["conversations"][0]["tasks"].as_array().unwrap();
        assert_eq!(
            tasks.len(),
            2,
            "无限主目标不能重新展示启动期固定步骤；真实子任务必须保留"
        );
        assert_eq!(
            tasks[0]["status"], "running",
            "阶段成果不等于整个持续目标完成"
        );
        assert_eq!(
            tasks[0]["progress"], 99,
            "保留已有子成果，主目标未结束不能提前100%"
        );
        assert_eq!(tasks[1]["status"], "completed");
        assert_eq!(tasks[1]["progress"], 100);
        let id = run.run_id.clone();
        host.inner.store.call(move |db| {
            let d = db.delegations(&id)?.remove(0);
            db.bump("run_progress", &json!({"run_id":d.child_run,"steps":[{"id":"unverified","title":"保存 outputs/unverified.md","state":"pending","evidenceCallIds":[]}]}).to_string())?;
            Ok(())
        }).await.unwrap();
        let blocked = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(
            blocked["conversations"][0]["tasks"][0]["status"], "running",
            "历史子计划待核验不能伪称整个持续主Run阻塞"
        );
        assert_eq!(
            blocked["conversations"][0]["tasks"][1]["status"], "blocked",
            "不能把缺少真实文件证据的子执行改为成功"
        );
        let id = run.run_id.clone();
        host.inner
            .store
            .call(move |db| {
                assert_eq!(
                    db.progress(&id)?.unwrap()["steps"][0]["state"],
                    "in_progress"
                );
                let mut state = db.continuous_state(&id)?.unwrap();
                state.completion_condition = Some("有限任务完成并验收".into());
                db.bump(
                    "continuous_task",
                    &serde_json::to_string(&state).map_err(HostError::storage)?,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let finite = prepare(&host, &office, "office-1").await.unwrap();
        assert!(
            finite["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["title"] == "启动期固定步骤"),
            "有限持续任务仍按原计划验收"
        );
        provider.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn sequential_member_work_keeps_real_progress_results_and_terminal_independent_of_animation(
    ) {
        use openpaw_llm::{MockMode, MockProvider};
        struct Gate {
            inner: MockProvider,
            requests: std::sync::atomic::AtomicUsize,
            entered: tokio::sync::Notify,
            release: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for Gate {
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
                if self
                    .requests
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    == 5
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
                "sequential-member-fixture"
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("brief.md"), "真实工作区资料").unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
        let call = |name: &str, input| MockMode::ToolUse {
            tool_name: name.into(),
            input,
            text: String::new(),
        };
        let provider = std::sync::Arc::new(Gate {
            inner: MockProvider::scripted(vec![
                call(
                    "delegate_task",
                    json!({"role_id":"research","title":"核验第一部分","instructions":"读取brief.md并整理","acceptance":"真实读取"}),
                ),
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"read","title":"读取资料"},{"id":"summary","title":"整理第一部分"}]}),
                ),
                call("read_file", json!({"path":"brief.md"})),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"read","state":"completed","evidenceCallIds":["call_2"]}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"summary","state":"in_progress"}),
                ),
                MockMode::Fixed("第一部分已核验完成".into()),
                call(
                    "delegate_task",
                    json!({"role_id":"research","title":"整理第二部分","instructions":"独立整理下一部分，不重复前项","acceptance":"实际正文"}),
                ),
                MockMode::Fixed("第二部分真实成果".into()),
                MockMode::Fixed("两个部分均已取得".into()),
            ]),
            requests: std::sync::atomic::AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let host = Host::with_provider(config, provider.clone()).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "sequential-member".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        host.submit_run(
            &scope,
            &binding.session_id,
            "请同一成员依次处理两个部分".into(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "PAWOFFICE_TEST_KEY".into(),
            allowed_origins: vec!["http://127.0.0.1:8765".into()],
            names: Default::default(),
            creation_slots: vec![],
        };
        let pending = prepare(&host, &office, "office-1").await.unwrap();
        let tasks = pending["conversations"][0]["tasks"].as_array().unwrap();
        let child = tasks
            .iter()
            .find(|t| t["assigneeId"] == "code-agent")
            .unwrap();
        assert_eq!(
            child["progress"], 33,
            "一项真实读取完成、整理尚在途，不能只报0或随时间增长"
        );
        assert!(child["summary"].as_str().unwrap().contains("整理第一部分"));
        assert!(
            tasks[0]["summary"]
                .as_str()
                .unwrap()
                .contains("团队进展 · 李研："),
            "主卡引用子动作必须标真实负责人，不能显示为王明本人执行：{}",
            tasks[0]["summary"]
        );
        provider.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        let ended = prepare(&host, &office, "office-1").await.unwrap();
        assert_eq!(
            ended["conversations"][0]["tasks"][0]["status"], "completed",
            "业务全部核验后不因视觉队列标为blocked"
        );
        assert_eq!(ended["conversations"][0]["tasks"][0]["progress"], 100);
        let final_snapshot = tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let s = prepare(&host, &office, "office-1").await.unwrap();
                if s["conversations"][0]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|t| t["assigneeId"] == "code-agent")
                    .count()
                    == 2
                {
                    break s;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        for task in final_snapshot["conversations"][0]["tasks"]
            .as_array()
            .unwrap()
        {
            assert_eq!(task["status"], "completed");
            assert_eq!(task["progress"], 100);
        }
        assert_eq!(
            final_snapshot["conversations"][0]["whiteboard"]["status"],
            "completed"
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn d139_parent_verified_plan_and_child_execution_have_separate_denominators() {
        use openpaw_llm::{MockMode, MockProvider};
        for confirm in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("brief.md"), "真实待核对资料").unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let mut script = vec![
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"accept","title":"核对两位同事结果及实际资料"}]}),
                ),
                call(
                    "delegate_task",
                    json!({"role_id":"organize","title":"核对第一部分","instructions":"读取brief.md","acceptance":"真实有限核对"}),
                ),
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("第一部分已实际读取并完成核对".into()),
                call(
                    "delegate_task",
                    json!({"role_id":"research","title":"核对第二部分","instructions":"读取brief.md","acceptance":"真实有限核对"}),
                ),
                call("read_file", json!({"path":"brief.md"})),
                MockMode::Fixed("第二部分已实际读取并完成核对".into()),
                call("read_file", json!({"path":"brief.md"})),
            ];
            if confirm {
                // 父计划以自身最终读回证据验收，不必将每个中间委派ID再重复登记。
                script.push(call("report_progress",json!({"type":"update","stepId":"accept","state":"completed","evidenceCallIds":["call_7"]})));
            }
            script.push(MockMode::Fixed("两项结果已收齐，实际资料核对完毕".into()));
            let host =
                Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
                    .await
                    .unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "d139".into(),
            };
            let session = host.open_session(scope.clone()).await.unwrap();
            let run = host
                .submit_run(&scope, &session.session_id, "核对两份独立结果".into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            assert_eq!(
                host.get_run(&scope, &run.run_id).await.unwrap().state,
                RunState::Completed
            );
            let mut office = OfficeConfig {
                activity_window: false,
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "UNUSED".into(),
                allowed_origins: vec![],
                names: Default::default(),
                creation_slots: vec![],
            };
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            assert_eq!(
                conv["tasks"][0]["status"],
                if confirm { "completed" } else { "blocked" },
                "子执行完成和父业务验收分别计数，confirm={confirm}"
            );
            assert_eq!(conv["tasks"][0]["progress"] == 100, confirm);
            for child in conv["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["assigneeId"] != "office-lead")
            {
                assert_eq!(child["status"], "completed");
                assert_eq!(child["progress"], 100);
            }
            assert_eq!(
                conv["whiteboard"]["status"],
                if confirm { "completed" } else { "blocked" }
            );
            // 生产活动窗口只隐藏已结束人物卡，不能改变白板的业务验收事实。
            office.activity_window = true;
            host.inner
                .store
                .call(|db| {
                    db.office_projection_source.clear();
                    Ok(())
                })
                .await
                .unwrap();
            let current = prepare(&host, &office, "office-1").await.unwrap();
            assert_eq!(
                current["conversations"][0]["whiteboard"]["status"],
                conv["whiteboard"]["status"]
            );
            assert!(current["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .is_empty());
            host.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn explicit_parent_acceptance_is_separate_from_finite_text_execution_and_file_gates() {
        use openpaw_llm::{MockMode, MockProvider};
        for (title, confirm, accepted) in [
            ("核对给定正文", true, true),
            ("核对给定正文", false, false),
            ("保存notes.md", true, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let mut script = vec![
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"accept","title":title}]}),
                ),
                call(
                    "delegate_task",
                    json!({"role_id":"organize","title":"检查给定正文","instructions":"对提供正文进行有边界的逻辑核验","acceptance":"返回实际核验结论","context":"给定正文及目标"}),
                ),
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"internal","title":"核定给定材料的约束"}]}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"internal","state":"in_progress"}),
                ),
                MockMode::Fixed("实际正文核验结果与限制".into()),
            ];
            if confirm {
                script.push(call("report_progress",json!({"type":"update","stepId":"accept","state":"completed","evidenceCallIds":["call_1"]})));
            }
            script.push(MockMode::Fixed("父任务已收到核验结果；文件未写入".into()));
            let host =
                Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
                    .await
                    .unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "accept-internal".into(),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            host.submit_run(
                &scope,
                &binding.session_id,
                "安排核验后按真实成果验收".into(),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                .await
                .unwrap();
            let office = OfficeConfig {
                activity_window: false,
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let snapshot = prepare(&host, &office, "office-1").await.unwrap();
            let conv = &snapshot["conversations"][0];
            assert_eq!(
                conv["tasks"][0]["status"],
                if accepted { "completed" } else { "blocked" },
                "{title}; confirm={confirm}"
            );
            assert_eq!(conv["tasks"][0]["progress"] == 100, accepted);
            let child = conv["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["assigneeId"] == "file-agent")
                .unwrap();
            assert_eq!(
                child["status"], "completed",
                "有限文本执行已经结束，父验收门禁由上面的父卡单独保持"
            );
            assert_eq!(child["progress"], 100);
            assert!(!root.join("notes.md").exists());
            host.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn accepted_revision_replaces_only_the_same_step_unaccepted_draft() {
        use openpaw_llm::{MockMode, MockProvider};
        for accept_both in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let provider = MockProvider::scripted(vec![
                call(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"draft","title":"输出短文"}]}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"draft","state":"in_progress"}),
                ),
                call(
                    "delegate_task",
                    json!({"role_id":"draft","title":"输出初稿","instructions":"独立撰写短文","acceptance":"有界正文"}),
                ),
                MockMode::Fixed("实际初稿，等待父角色核验".into()),
                call(
                    "delegate_task",
                    json!({"role_id":"draft","title":"压缩初稿","instructions":"缩短已返回的初稿","context":"实际初稿，等待父角色核验","acceptance":"有界修订正文"}),
                ),
                MockMode::Fixed("实际压缩后的修订正文".into()),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":"draft","state":"completed","evidenceCallIds":if accept_both {json!(["call_2","call_4"])}else{json!(["call_4"])}}),
                ),
                MockMode::Fixed("已核验实际交付".into()),
            ]);
            let provider = std::sync::Arc::new(provider);
            let host = Host::with_provider(config, provider.clone()).await.unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "revised-draft".into(),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            let run = host
                .submit_run(&scope, &binding.session_id, "先审初稿，再验收修订".into())
                .await
                .unwrap();
            if tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                .await
                .is_err()
            {
                let view =
                    tokio::time::timeout(Duration::from_secs(1), host.get_run(&scope, &run.run_id))
                        .await;
                panic!(
                    "等待真实清理超时：accept_both={accept_both}, provider_calls={}, run={view:?}",
                    provider.call_count()
                );
            }
            let office = OfficeConfig {
                activity_window: false,
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            let snapshot = tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    let s = prepare(&host, &office, "office-1").await.unwrap();
                    let count = s["conversations"][0]["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|t| t["assigneeId"] == "app-agent")
                        .count();
                    if count == if accept_both { 2 } else { 1 } {
                        break s;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap();
            let conv = &snapshot["conversations"][0];
            assert_eq!(
                conv["tasks"][0]["status"], "completed",
                "accept_both={accept_both}; {snapshot}"
            );
            assert_eq!(conv["tasks"][0]["progress"], 100);
            assert_eq!(conv["whiteboard"]["status"], "completed");
            assert!(conv["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["status"] == "completed" && t["progress"] == 100));
            assert_eq!(
                conv["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["kind"] == "task.assigned")
                    .count(),
                2,
                "原派发事实不删除"
            );
            host.inner
                .store
                .call(move |db| {
                    let ds = db.delegations(&run.run_id)?;
                    assert_eq!(ds.len(), 2, "不修改原委派账本");
                    for d in ds {
                        assert_eq!(db.get_run(&d.child_run)?.state, RunState::Completed);
                    }
                    Ok(())
                })
                .await
                .unwrap();
            host.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn child_generated_text_does_not_complete_an_unwritten_file_plan() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
        let provider = MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "delegate_task".into(),
                input: json!({"role_id":"draft","title":"整理记录","instructions":"生成有界记录","acceptance":"真实文件"}),
                text: String::new(),
            },
            MockMode::ToolUse {
                tool_name: "report_progress".into(),
                input: json!({"type":"create","steps":[{"id":"save","title":"保存notes.md"}]}),
                text: String::new(),
            },
            MockMode::Fixed("已生成正文，但没有写文件权限".into()),
            MockMode::Fixed("已收到正文".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(provider))
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "unwritten".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        host.submit_run(&scope, &binding.session_id, "整理并保存".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "PAWOFFICE_TEST_KEY".into(),
            allowed_origins: vec!["http://127.0.0.1:8765".into()],
            names: Default::default(),
            creation_slots: vec![],
        };
        let s = prepare(&host, &office, "office-1").await.unwrap();
        let tasks = s["conversations"][0]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        for task in tasks {
            assert_eq!(task["status"], "blocked");
            assert!(task["progress"].as_u64().unwrap() < 100);
        }
        assert_ne!(s["conversations"][0]["whiteboard"]["status"], "completed");
        host.shutdown().await.unwrap();
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
        for (second_evidence, expected_cards) in [
            ("grouped", 4),
            ("grouped-incremental", 4),
            ("call_2_1", 3),
            ("call_2_0", 5),
        ] {
            let grouped = second_evidence.starts_with("grouped");
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
                    json!({"type":"update","stepId":"research","state":"completed","evidenceCallIds":if second_evidence=="grouped" {json!(["call_2_0","call_2_1"])}else{json!(["call_2_0"])}}),
                ),
                call(
                    "report_progress",
                    json!({"type":"update","stepId":if grouped {"research"}else{"organize"},"state":"completed","evidenceCallIds":if second_evidence=="grouped" {json!(["call_2_0","call_2_1"])}else if grouped {json!(["call_2_1"])}else{json!([second_evidence])}}),
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
                activity_window: false,
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
            // 等待真实执行事实同步；不按动画时长推迟任务卡。
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
                    own["status"], "completed",
                    "子执行已完成，父业务步骤仍由下方未完成断言单独约束"
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
                        own[0]["status"], "completed",
                        "人物有限执行完成；父步骤仍由独立证据决定"
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
                activity_window: false,
                base_url: "http://127.0.0.1:9".into(),
                api_key_env: "PAWOFFICE_TEST_KEY".into(),
                allowed_origins: vec!["http://127.0.0.1:8765".into()],
                names: Default::default(),
                creation_slots: vec![],
            };
            // 未核验重试保留原失败尝试；等待独立成功卡按既有展示窗口出现。
            let snapshot = tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    let snapshot = prepare(&host, &office, "office-1").await.unwrap();
                    if snapshot["conversations"][0]["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|e| e["kind"] == "task.assigned")
                        .count()
                        >= if retry && !confirm { 2 } else { 1 }
                    {
                        break snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap();
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
                if confirm {
                    3
                } else if retry {
                    5
                } else {
                    4
                },
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
                assert_eq!(
                    conv["tasks"][0]["status"],
                    if retry { "failed" } else { "blocked" }
                );
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
                    assert_eq!(
                        attempts.len(),
                        2,
                        "无准确替代证据时保留两次真实任务，不抹去失败尝试"
                    );
                    assert!(
                        attempts.iter().any(|t| t["status"] == "completed"),
                        "后次真实执行完成，父业务仍须核验"
                    );
                    assert!(
                        attempts.iter().any(|t| t["status"] == "failed"),
                        "未明确替代的真实失败不能被抹去"
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
                    .position(|task| {
                        task["assigneeId"] == "code-agent" && task["status"] == "completed"
                    })
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
                1,
                "成功有限执行产生执行完成动态，摘要必须区分父验收"
            );
            if let Some(event) = completions.first() {
                assert_eq!(event["employeeId"], "code-agent");
                let completed_task = conv["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["id"] == event["taskId"])
                    .expect("完成动态必须引用真实任务");
                assert_eq!(completed_task["status"], "completed");
                assert_eq!(completed_task["assigneeId"], "code-agent");
                assert!(event["summary"].as_str().unwrap().contains(if confirm {
                    "已核验"
                } else {
                    "父目标待验收"
                }));
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
    async fn d101_protected_multiturn_catalogue_preserves_pending_approval() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["write_file"],"approval_timeout_secs":30})).unwrap();
        let steps:Vec<_>=(0..12).map(|i|json!({"id":format!("s{i}"),"title":format!("汇总第{i}部分{}","路线住宿资料与风险".repeat(4))})).collect();
        let plan = MockMode::ToolUse {
            tool_name: "report_progress".into(),
            input: json!({"type":"create","steps":steps}),
            text: String::new(),
        };
        let mut script = Vec::new();
        for _ in 0..12 {
            script.extend([
                plan.clone(),
                MockMode::Fixed("已整理实际资料与待核实事项".into()),
            ]);
        }
        script.extend([
            plan,
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"pending.md","content":"only after approval"}),
                text: String::new(),
            },
            MockMode::Fixed("完成".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
            .await
            .unwrap();
        let office = OfficeConfig {
            activity_window: false,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        let scope = |c| ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: format!("protected-{c}"),
        };
        for c in 0..4 {
            let b = host.open_session(scope(c)).await.unwrap();
            for turn in 0..3 {
                host.submit_run(
                    &scope(c),
                    &b.session_id,
                    format!("第{turn}轮：{}", "整理团队真实资料".repeat(10)),
                )
                .await
                .unwrap();
                tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                    .await
                    .unwrap();
            }
        }
        let b = host.open_session(scope(3)).await.unwrap();
        let run = host
            .submit_run(
                &scope(3),
                &b.session_id,
                "保留三轮任务后等待写入审批".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (v, mut events) = host.snapshot_and_subscribe(&scope(3)).await.unwrap();
                if !v.pending_approvals.is_empty() {
                    break;
                }
                events.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        let before = host.inner.store.call(|db| db.revision()).await.unwrap();
        let snapshot = prepare(&host, &office, "office-1")
            .await
            .expect("受保护多轮可选详情不得阻断审批");
        assert!(snapshot_fits_office_message("office-1", &snapshot));
        assert_eq!(snapshot["conversations"].as_array().unwrap().len(), 4);
        for c in snapshot["conversations"].as_array().unwrap() {
            assert_eq!(c["employees"].as_array().unwrap().len(), 6);
            assert!(
                c["whiteboard"]["detail"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("# ") && text.contains("## 任务进度")),
                "正式四会话模式必须保留可点击Markdown详情，不能仅验小目录"
            );
            assert_eq!(
                c["tasks"].as_array().unwrap().len(),
                39,
                "必须保留三轮完整任务身份和进度"
            );
        }
        let current = snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "protected-3")
            .unwrap();
        assert_eq!(current["tasks"][0]["status"], "blocked");
        assert_eq!(
            host.inner.store.call(|db| db.revision()).await.unwrap(),
            before,
            "压缩不能写入Run/进度/审批事实"
        );
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            snapshot,
            "压缩后的重复投影稳定"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        host.open_session(ClientScope {
            conversation_id: "unrelated-empty".into(),
            ..scope(3)
        })
        .await
        .unwrap();
        assert_eq!(
            prepare(&host, &office, "office-1").await.unwrap(),
            snapshot,
            "无关版本重建不能使压缩任务的updatedAt/seq每次抖动"
        );
        assert!(!root.join("pending.md").exists());
        assert_eq!(
            host.snapshot_and_subscribe(&scope(3))
                .await
                .unwrap()
                .0
                .pending_approvals
                .len(),
            1
        );
        host.cancel_run(&scope(3), &run.run_id).await.unwrap();
        host.wait_idle().await;
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn d130_business_title_and_live_detail_stay_separate_for_real_members() {
        use openpaw_llm::{MockMode, MockProvider};
        use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};
        struct Paused {
            inner: MockProvider,
            calls: AtomicUsize,
            ready: tokio::sync::Notify,
            release: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl openpaw_core::LlmProvider for Paused {
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
                if matches!(self.calls.fetch_add(1, Ordering::SeqCst), 2..=4) {
                    self.ready.notify_one();
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
                "d130-paused"
            }
        }
        for role in ["organize", "draft", "research", "review", "audit"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            for file in ["first.md", "second.md"] {
                std::fs::write(root.join(file), "可核对的真实资料").unwrap();
            }
            let call = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let provider = Arc::new(Paused {
                inner: MockProvider::scripted(vec![
                    call(
                        "delegate_task",
                        json!({"role_id":role,"title":"复核资料","instructions":"核对两份文件","acceptance":"有限核对结果"}),
                    ),
                    call("read_file", json!({"path":"first.md"})),
                    call("read_file", json!({"path":"second.md"})),
                    MockMode::Fixed("两份资料均已核对".into()),
                    MockMode::Fixed("汇总核对结果".into()),
                ]),
                calls: AtomicUsize::new(0),
                ready: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            });
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true}})).unwrap();
            let host = Host::with_provider(config, provider.clone()).await.unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "d130".into(),
            };
            let session = host.open_session(scope.clone()).await.unwrap();
            let parent = host
                .submit_run(
                    &scope,
                    &session.session_id,
                    "完成资料核对\n\n请在当前单一对话中持续工作，此段是操作约束而不是标题".into(),
                )
                .await
                .unwrap();
            let office = crate::test_support::office_config();
            let employee = crate::agents::role(role).unwrap().employee();
            let mut previous: Option<Value> = None;
            for file in ["first.md", "second.md"] {
                tokio::time::timeout(Duration::from_secs(5), provider.ready.notified())
                    .await
                    .unwrap();
                let snapshot = prepare(&host, &office, "office-1").await.unwrap();
                let conv = &snapshot["conversations"][0];
                let item = conv["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["assigneeId"] == employee)
                    .unwrap();
                assert_eq!(item["title"], "复核资料", "粗体业务标题不能变成工具动作");
                assert_eq!(
                    item["status"], "running",
                    "等待下一次模型响应仍是独立执行在途"
                );
                assert!(
                    item["summary"]
                        .as_str()
                        .unwrap()
                        .starts_with(&format!("刚完成：读取 {file}")),
                    "灰色说明须以真实最近工具开头"
                );
                assert_eq!(conv["tasks"][0]["title"], "完成资料核对");
                let shown = super::super::turns::display(
                    &host,
                    &host.get_run(&scope, &parent.run_id).await.unwrap(),
                )
                .await
                .unwrap();
                assert!(
                    shown.contains(crate::agents::role(role).unwrap().name)
                        && shown.contains("复核资料")
                        && shown.contains("正在执行"),
                    "正常团队节点必须进入聊天，不能等待主模型转述：{shown}"
                );
                if let Some(old) = &previous {
                    assert_eq!(old["conversations"][0]["tasks"][1]["id"], item["id"]);
                    let assignments = |v: &Value| {
                        v["conversations"][0]["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|e| e["kind"] == "task.assigned")
                            .cloned()
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(
                        assignments(old),
                        assignments(&snapshot),
                        "动作变化不新增或重播交接"
                    );
                }
                previous = Some(snapshot);
                provider.release.add_permits(1);
            }
            tokio::time::timeout(Duration::from_secs(5), provider.ready.notified())
                .await
                .unwrap();
            let parent_id = parent.run_id.clone();
            host.inner
                .store
                .call(move |db| {
                    let state = crate::continuous::State {
                        run_id: parent_id,
                        started: true,
                        active: true,
                        version: 1,
                        ..Default::default()
                    };
                    db.bump("continuous_task", &serde_json::to_string(&state).unwrap())?;
                    Ok(())
                })
                .await
                .unwrap();
            let waiting = prepare(&host, &office, "office-1").await.unwrap();
            let tasks = waiting["conversations"][0]["tasks"].as_array().unwrap();
            assert_eq!(tasks.len(), 1, "真实已结束的子执行应退场，主执行仍须保留");
            assert_eq!(tasks[0]["assigneeId"], "office-lead");
            assert_eq!(tasks[0]["status"], "running");
            assert!(
                tasks[0]["progress"].as_u64().unwrap() < 100,
                "持续目标仍在运行，不能把当前批次完成写成总目标100%"
            );
            provider.release.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
                .await
                .unwrap();
            let terminal = prepare(&host, &office, "office-1").await.unwrap();
            assert!(terminal["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(!terminal["conversations"][0]["whiteboard"]["detail"]
                .as_str()
                .unwrap()
                .is_empty());
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
            activity_window: false,
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
                assert!(members.iter().any(|member| member["id"] == role.employee()
                    && member["sceneActorId"] == role.actor()
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
