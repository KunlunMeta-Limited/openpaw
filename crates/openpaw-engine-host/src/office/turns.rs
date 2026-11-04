//! 职责：将Office稳定轮次、历史页和恢复控制映射到原共享Run及JSONL。
//! 输入输出：已校验完整作用域、轮次请求到原Run视图或历史页。
//! 依赖：Host接受/查询/取消、Store原账本和幂等关联。
//! 禁止职责：不复制会话账本、不执行工具、不从历史重放副作用。
use super::dto::{self, ChatRequest, HistoryRequest};
use crate::{ClientScope, Host, HostError, HostResult, RunState, RunView};
use serde_json::{json, Value};

// 在轮次服务统一核对创建事实；不能让猜到预留槽ID的请求先于office.create建会话。
async fn check_active(host: &Host, scope: &ClientScope) -> HostResult<()> {
    let config = host
        .inner
        .config
        .office
        .as_ref()
        .ok_or_else(|| HostError::new("scope_denied", "Office未启用"))?;
    if !super::workspaces::active(host, config, &scope.slot_id).await? {
        return Err(HostError::new("scope_denied", "办公室尚未创建"));
    }
    Ok(())
}

/// 功能：可靠接受官方send或返回同一轮次的原Run。
/// 参数：host为共享服务，scope为已校验范围，request为send载荷。
/// 返回：新接受或已存在的Run。
/// 错误：范围、幂等、历史、容量或存储错误。
/// 状态：原JSONL记录消息；元数据仅保存幂等关联。
/// 并发与取消：Host短接受锁防并发重试重复执行；脱离HTTP不取消执行。
/// 权限：工作根只来自Host配置，不能由请求指定。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。 需求/设计REQ-OP-014。
pub(super) async fn accept(
    host: &Host,
    scope: &ClientScope,
    request: ChatRequest,
) -> HostResult<RunView> {
    check_active(host, scope).await?;
    if request.turn_id.starts_with("legacy:") {
        return Err(HostError::new("invalid_input", "legacy:为升级历史保留前缀"));
    }
    let canonical = serde_json::to_string(&request.messages).map_err(HostError::storage)?;
    let binding = host.open_session(scope.clone()).await?;
    let mut messages = request.messages;
    let input = messages
        .pop()
        .ok_or_else(|| HostError::new("invalid_input", "缺少用户输入"))?
        .content;
    let history = messages
        .into_iter()
        .map(|m| {
            if m.role == "user" {
                openpaw_core::Message::user_text(m.content)
            } else {
                openpaw_core::Message::assistant_text(m.content)
            }
        })
        .collect();
    host.submit_idempotent(
        scope,
        &binding.session_id,
        input,
        history,
        request.turn_id,
        canonical,
    )
    .await
}
/// 功能：只读查找完整会话范围内的轮次。
/// 参数：host为共享服务，scope为完整范围，turn为客户端轮次ID。
/// 返回：原Run视图。
/// 错误：不存在、越界或存储错误。
/// 状态：不创建Session或Run；活动轮次仅通知原作用域重展示审批，不改变业务状态。
/// 并发与取消：在Store单写序列读取；取消另走共享服务。
/// 权限：逐项匹配namespace/team/conversation。
/// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
pub(super) async fn find(host: &Host, scope: &ClientScope, turn: &str) -> HostResult<RunView> {
    check_active(host, scope).await?;
    let lookup_scope = scope.clone();
    let turn = turn.to_owned();
    let run = host
        .inner
        .store
        .call(move |db| {
            let binding = db
                .bindings()?
                .into_iter()
                .find(|b| b.scope == lookup_scope)
                .ok_or_else(|| HostError::new("not_found", "会话不存在"))?;
            for run in db
                .runs()?
                .into_iter()
                .filter(|r| r.session_id == binding.session_id)
            {
                if db.request_key(&run.run_id)? == turn {
                    return db.get_run(&run.run_id);
                }
            }
            Err(HostError::new("not_found", "轮次不存在"))
        })
        .await?;
    if !run.state.is_terminal() {
        host.refresh_office_ui(scope).await;
    }
    Ok(run)
}
/// 功能：从Run输入边界和原JSONL投影稳定分页历史。
/// 参数：host为共享服务，scope为完整范围，request为已校验分页请求。
/// 返回：官方历史页，不含工具结果伪用户轮次。
/// 错误：非法游标、账本或存储错误。
/// 状态：空会话返回空页；成功读取仅通知重展示审批，不创建绑定、启动模型或改变期限。
/// 并发与取消：单一Store读取序列；游标绑定既存Run身份，新追加不移动边界。
/// 权限：只读取指定且已配置的会话范围。
/// 关联：CHG-2026-0005第9.7节；DES-OP-003/007。 需求/设计REQ-OP-014。
pub(super) async fn history(
    host: &Host,
    scope: &ClientScope,
    request: HistoryRequest,
) -> HostResult<Value> {
    let total = std::time::Instant::now();
    check_active(host, scope).await?;
    let active_ms = total.elapsed().as_millis();
    let history_scope = scope.clone();
    let queued = std::time::Instant::now();
    let page = host.inner.store.call(move |db| {
        let queue_ms = queued.elapsed().as_millis();
        let mut cache = crate::store::LedgerReadCache::default();
        let result = history_in(db, &history_scope, request, &mut cache);
        if std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some() {
            let (reads, bytes, read_time, runs) = cache.stats();
            eprintln!("engine-host diagnostics chat_history active_ms={active_ms} queue_ms={queue_ms} total_ms={} ledger_reads={} ledger_bytes={} ledger_ms={} chat_run_reads={runs} ok={}", total.elapsed().as_millis(), reads, bytes, read_time.as_millis(), result.is_ok());
        }
        result
    }).await?;
    host.refresh_office_ui(scope).await;
    Ok(page)
}

// 先确定真实游标边界，只有入选页才能读取正文；不将工具详情附带读取到聊天。
fn history_in(
    db: &crate::store::Db,
    scope: &ClientScope,
    request: HistoryRequest,
    cache: &mut crate::store::LedgerReadCache,
) -> HostResult<Value> {
    let selection = std::time::Instant::now();
    let Some(binding) = db.binding_for_scope(scope)? else {
        if !request.cursor.is_empty() {
            return Err(HostError::new("invalid_input", "游标不属于此会话"));
        }
        return Ok(json!({"turns":[],"nextCursor":"","hasMore":false,"activeTurnId":""}));
    };
    enum Source {
        Imported(Value),
        Run(RunView, String),
    }
    let mut rows = Vec::new();
    for (offset, user, assistant) in db.imported_turns_cached(&binding.session_id, cache)? {
        let key = format!("import:{}:{offset}", binding.session_id);
        rows.push((offset, key.clone(), Source::Imported(json!({"id":format!("legacy:{key}"),"user":dto::bounded(&user,4000),"assistant":dto::bounded(&assistant,50000),"state":"complete","error":""}))));
    }
    for run in db.runs_for_session(&binding.session_id)? {
        let (offset, input) = db.run_input_cached(&run, cache)?;
        if !input.is_empty() {
            rows.push((offset, run.run_id.clone(), Source::Run(run, input)));
        }
    }
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let end = if request.cursor.is_empty() {
        rows.len()
    } else {
        let key = request
            .cursor
            .strip_prefix("before:")
            .ok_or_else(|| HostError::new("invalid_input", "游标格式无效"))?;
        rows.iter()
            .position(|(_, id, _)| id == key)
            .ok_or_else(|| HostError::new("invalid_input", "游标不属于此会话"))?
    };
    let mut start = end.saturating_sub(request.limit);
    let select_time = selection.elapsed();
    let building = std::time::Instant::now();
    let mut node_time = std::time::Duration::ZERO;
    let mut serialize_time = std::time::Duration::ZERO;
    let mut child_reads = 0usize;
    let mut turns = Vec::new();
    let mut sizes = Vec::new();
    let mut encoded_bytes = 2usize; // JSON数组方括号；逐项大小已含UTF-8/转义。
    for (_, _, source) in &rows[start..end] {
        let turn = match source {
            Source::Imported(turn) => turn.clone(),
            Source::Run(run, input) => {
                let run = db.get_chat_run(&run.run_id, cache)?;
                let node_start = std::time::Instant::now();
                let (text, reads) = display_counted(db, &run, cache)?;
                node_time += node_start.elapsed();
                child_reads += reads;
                json!({"id":db.request_key(&run.run_id)?,"user":dto::bounded(input,4000),"assistant":text,"state":state(&run),"error":message(&run)})
            }
        };
        let serialization = std::time::Instant::now();
        let bytes = serde_json::to_vec(&turn).map_err(HostError::storage)?.len();
        serialize_time += serialization.elapsed();
        encoded_bytes += bytes + usize::from(!turns.is_empty());
        sizes.push(bytes);
        turns.push(turn);
    }
    let build_time = building.elapsed();
    let mut removed = 0;
    while encoded_bytes > 8 * 1024 * 1024 - 1024 {
        if turns.len() - removed <= 1 {
            return Err(HostError::new("protocol_error", "单轮历史超过传输上限"));
        }
        encoded_bytes -= sizes[removed] + 1;
        removed += 1;
        start += 1;
    }
    turns.drain(..removed);
    if std::env::var_os("OPENPAW_HOST_DIAGNOSTICS").is_some() {
        eprintln!("engine-host diagnostics chat_history_page selection_ms={} build_ms={} nodes_ms={} serialize_ms={} candidates={} returned={} child_metadata_reads={child_reads}", select_time.as_millis(), build_time.as_millis(), node_time.as_millis(), serialize_time.as_millis(), rows.len(), turns.len());
    }
    let active = turns
        .last()
        .filter(|t| t["state"] == "pending")
        .and_then(|t| t["id"].as_str())
        .unwrap_or("");
    Ok(
        json!({"turns":turns,"nextCursor":if start>0{format!("before:{}",rows[start].1)}else{String::new()},"hasMore":start>0,"activeTurnId":active}),
    )
}
/// 功能：映射官方聊天状态。
/// 参数：run为真实Run视图。
/// 返回：官方轮次状态pending、complete、stopped或error。
/// 错误：无。
/// 状态：只读，不把中断伪装恢复执行。
/// 并发与取消：取消清理完成前保持pending。
/// 权限：无。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。 需求/设计REQ-OP-014。
pub(super) fn state(run: &RunView) -> &'static str {
    if run.text_truncated || dto::units(&run.text) > 50000 {
        return "error";
    }
    match run.state {
        RunState::Completed if run.error.is_none() && !run.text.trim().is_empty() => "complete",
        RunState::Cancelled => "stopped",
        s if !s.is_terminal() => "pending",
        _ => "error",
    }
}
/// 功能：提供有界、无秘密的轮次错误说明。
/// 参数：run为真实Run视图。
/// 返回：正常为空字符串，错误为本地固定说明。
/// 错误：无。
/// 状态：不泄露Provider响应和配置密钥。
/// 并发与取消：纯计算。
/// 权限：无。
/// 关联：CHG-2026-0005第9.7节；DES-OP-007。 需求/设计REQ-OP-014。
pub(super) fn message(run: &RunView) -> String {
    if state(run) != "error" {
        return String::new();
    }
    if let Some(error) = &run.error {
        return error.message.clone();
    }
    match run.state {
        RunState::Interrupted => "Host重启中断，未自动重放工具",
        RunState::TimedOut => "运行预算到期，清理已完成",
        RunState::Completed if run.text.trim().is_empty() => "模型没有返回可显示文本",
        _ if run.text_truncated || dto::units(&run.text) > 50000 => "模型回复超过显示上限",
        _ => "运行失败，完整诊断保存在OpenPaw账本",
    }
    .into()
}

/// 功能：为真实失败回复追加可见说明，统一流、恢复和历史的显示口径。
/// 参数：run为原账本或活动缓存恢复的真实Run。
/// 返回：不超过官方50000个UTF-16单元的正文，失败时保留有界原文并附原因。
/// 错误：无，正文只用已安全分类的本地说明，不回显上游原始响应。
/// 状态：仅展示投影，不新建消息、不修改Run终态、不重放已完成操作。
/// 并发与取消：纯计算；取消保留原文与stopped状态，不附失败通知。
/// 权限：沿用调用方原scope校验，不读取配置或凭据。
/// 关联：CHG-2026-0007第9.14节D112；REQ-OP-014；DES-OP-007。
pub(super) fn display_text(run: &RunView) -> String {
    if state(run) != "error" {
        return dto::bounded(&run.text, 50000);
    }
    let notice = format!("\n\n> 本轮未完成：{}", message(run));
    let original = dto::bounded(&run.text, 50000usize.saturating_sub(dto::units(&notice)));
    format!("{original}{notice}")
}

/// 功能：将同一父轮次的真实子错误直接投影为聊天可见说明。
/// 参数：host为原共享服务，run为调用方已校验scope的父Run。
/// 返回：有界原文和最近八条子错误；不依赖主模型转述。
/// 错误：父子作用域、存储读取失败原样上报，不能静默省略错误。
/// 状态：只读原Delegation及Run元数据，不追加消息或修改执行状态。
/// 并发与取消：原Store读取序列；取消子Run不当作失败，刷新不重放。
/// 权限：沿用已校验父scope，委派读取逐字段校验父子所属和工作根。
/// 关联：CHG-2026-0007第12.115节D129；REQ-OP-014；DES-OP-007。
pub(super) async fn display(host: &Host, run: &RunView) -> HostResult<String> {
    let run = run.clone();
    host.inner.store.call(move |db| display_in(db, &run)).await
}
/// 功能：从原账本生成聊天展示及历史精确核对共用的安全正文。
/// 参数：db为原Store读取序列，run为已核对所属的父Run。
/// 返回：原正文、有界真实团队节点与子错误，结果不作为新的模型消息。
/// 错误：委派所属或元数据读取失败时明确返回。
/// 状态：仅展示，不写入数据库、消息或执行进度。
/// 并发与取消：同步只读，停止与失败严格区分。
/// 权限：沿原父子范围校验；历史调用仅接受同位置精确匹配。
/// 关联：CHG-2026-0007第12.115节D129；REQ-OP-014；DES-OP-007。
pub(crate) fn display_in(db: &crate::store::Db, run: &RunView) -> HostResult<String> {
    display_counted(db, run, &mut crate::store::LedgerReadCache::default()).map(|(text, _)| text)
}
fn display_counted(
    db: &crate::store::Db,
    run: &RunView,
    cache: &mut crate::store::LedgerReadCache,
) -> HostResult<(String, usize)> {
    let artifact_failures: Vec<String> = db
        .artifact_warnings(&run.run_id)?
        .into_iter()
        .map(|message| {
            format!(
                "\n> 文件已写入，但不可变版本未保存：{}",
                dto::bounded(&message, 700)
            )
        })
        .collect();
    let mut failures = Vec::new();
    let mut running = Vec::new();
    let mut queued = Vec::new();
    let mut ended = Vec::new();
    let mut child_reads = 0;
    for d in db.delegations(&run.run_id)?.into_iter().rev() {
        // 原delegations已验证父子scope/根；只按主键读生命周期，不解析子正文或工具。
        child_reads += 1;
        let mut child = match db.run_metadata(&d.child_run) {
            Ok(child) => child,
            Err(error) if error.kind == "not_found" => continue,
            Err(error) => return Err(error),
        };
        if child.session_id != d.child_session {
            return Err(HostError::new(
                "storage_unavailable",
                "子运行与持久委派所属不一致",
            ));
        }
        // 旧记录可能只持有通用终态错误；复用原文本恢复来细分账本原因，
        // 仍不构建工具或递归子任务。新明确分类沿元数据，不能被旧账本覆盖。
        if matches!(
            child.state,
            RunState::Failed | RunState::TimedOut | RunState::Interrupted
        ) && child.error.as_ref().is_none_or(|e| {
            matches!(
                e.kind.as_str(),
                "interrupted" | "run_failed" | "timeout" | "cancelled"
            )
        }) {
            child = db.get_chat_run(&d.child_run, cache)?;
        }
        let role = crate::agents::role(&d.role_id)
            .ok_or_else(|| HostError::new("storage_unavailable", "子任务角色不合法"))?;
        let title = dto::bounded(
            &d.title
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>(),
            160,
        );
        let phase = match child.state {
            RunState::Accepting | RunState::Accepted => "排队中，尚未开始",
            RunState::Running => "正在执行",
            RunState::Cancelling => "正在停止，等待实际清理",
            RunState::Completed => "本阶段执行结束",
            RunState::Cancelled => "已停止",
            RunState::Failed | RunState::TimedOut | RunState::Interrupted => "本阶段未完成",
        };
        let nodes = match child.state {
            RunState::Running | RunState::Cancelling => &mut running,
            RunState::Accepting | RunState::Accepted => &mut queued,
            _ => &mut ended,
        };
        if nodes.len() < 8 {
            nodes.push(format!("\n> {} ·「{}」：{phase}", role.name, title));
        }
        if failures.len() < 8
            && matches!(
                child.state,
                RunState::Failed | RunState::TimedOut | RunState::Interrupted
            )
        {
            let reason = dto::bounded(&message(&child), 700);
            failures.push(format!("\n> {} ·「{}」：{}", role.name, title, reason));
        }
    }
    failures.reverse();
    let heading = if run.state.is_terminal() {
        "子任务异常记录（历史失败保留，后续处理见回复）"
    } else {
        "子任务异常记录（主任务仍在运行）"
    };
    let mut notice = if failures.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n> {heading}{}\n> 仅展示最近8项；完整记录可从人物任务详情查询。",
            failures.join("")
        )
    };
    if !artifact_failures.is_empty() {
        notice.push_str(&format!(
            "\n\n> 文件版本保存异常（原工作区写入成功，不自动重做）{}",
            artifact_failures.join("")
        ));
    }
    let body = dto::bounded(
        &display_text(run),
        50000usize.saturating_sub(dto::units(&notice) + if notice.is_empty() { 0 } else { 2 }),
    );
    // 正常节点只占正文和错误说明之外的余量，不让长历史因新增节点再次丢字。
    let mut budget = 50000usize.saturating_sub(
        dto::units(&body) + dto::units(&notice) + if notice.is_empty() { 0 } else { 2 },
    );
    let label = "\n\n> 团队执行节点（阶段结束不代表整体目标已验收；活动优先，最多8项）";
    let mut nodes = String::new();
    if budget >= dto::units(label) + 2 {
        budget -= dto::units(label) + 2;
        for line in running.into_iter().chain(queued).chain(ended).take(8) {
            let units = dto::units(&line);
            if units > budget {
                break;
            }
            nodes.push_str(&line);
            budget -= units;
        }
        if !nodes.is_empty() {
            nodes = format!("{label}{nodes}\n\n");
        }
    }
    // 展示前缀的状态变化由既有官方snapshot替换，不当成模型正文回退或重放。
    Ok((
        format!(
            "{nodes}{notice}{}{body}",
            if notice.is_empty() { "" } else { "\n\n" }
        ),
        child_reads,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn d149_history_selects_page_before_text_and_preserves_import_cursor_append_and_scope() {
        use openpaw_core::{Message, SessionEvent};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:crate::HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r","office-2":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "history-page".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let saved_scope = scope.clone();
        host.inner.store.call(move |db| {
            let session=&binding.session_id;
            db.reconcile_history(session,&[Message::user_text("导入用户"),Message::assistant_text("导入答案")])?;
            // 这个从未提交的旧失败不能借用后续真实用户输入。
            let offset=db.events(session)?.len();
            db.save_run(RunView{run_id:"uncommitted".into(),session_id:session.clone(),state:RunState::Failed,revision:0,text:String::new(),tools:vec![],error:None,text_truncated:false},offset)?;
            for i in 0..22 {
                let offset=db.events(session)?.len();let id=format!("run-{i:02}");
                db.append(session,SessionEvent::Message(Message::user_text(format!("用户{i}"))))?;
                db.append(session,SessionEvent::RunStarted{run_id:id.clone()})?;
                db.append(session,SessionEvent::Message(Message::assistant_text(format!("可靠答案{i}"))))?;
                db.save_run(RunView{run_id:id.clone(),session_id:session.clone(),state:RunState::Completed,revision:0,text:String::new(),tools:vec![],error:None,text_truncated:false},offset)?;
            }
            let request=|cursor:&str,limit|serde_json::from_value::<HistoryRequest>(json!({"version":"1.0","teamId":"office-1","conversationId":"history-page","cursor":cursor,"limit":limit})).unwrap();
            let before=db.revision()?;let mut cache=crate::store::LedgerReadCache::default();
            let first=history_in(db,&saved_scope,request("",20),&mut cache)?;
            assert_eq!(first["turns"].as_array().unwrap().len(),20);assert_eq!(first["turns"][0]["user"],"用户2");assert_eq!(first["turns"][19]["user"],"用户21");
            assert_eq!(cache.stats().3,20,"23个候选只构建入选20轮，不为分页外恢复正文");assert_eq!(cache.reads(),1,"导入、旧输入与20轮正文只解析同一账本一次");
            let cursor=first["nextCursor"].as_str().unwrap().to_owned();assert_eq!(cursor,"before:run-02");
            let offset=db.events(session)?.len();db.append(session,SessionEvent::Message(Message::user_text("追加用户")))?;db.append(session,SessionEvent::RunStarted{run_id:"new-run".into()})?;db.append(session,SessionEvent::Message(Message::assistant_text("追加答案")))?;
            db.save_run(RunView{run_id:"new-run".into(),session_id:session.clone(),state:RunState::Completed,revision:0,text:String::new(),tools:vec![],error:None,text_truncated:false},offset)?;
            let mut older_cache=crate::store::LedgerReadCache::default();let older=history_in(db,&saved_scope,request(&cursor,20),&mut older_cache)?;
            assert_eq!(older["turns"].as_array().unwrap().len(),3);assert_eq!(older["turns"][0]["user"],"导入用户");assert_eq!(older["turns"][1]["user"],"用户0");assert_eq!(older["turns"][2]["user"],"用户1");assert_eq!(older["hasMore"],false);assert_eq!(older["nextCursor"],"");assert_eq!(older_cache.stats().3,2);
            let mut cache=crate::store::LedgerReadCache::default();let latest=history_in(db,&saved_scope,request("",1),&mut cache)?;assert_eq!(latest["turns"][0]["user"],"追加用户");assert_eq!(cache.stats().3,1);
            assert_eq!(history_in(db,&saved_scope,request("invalid",1),&mut Default::default()).unwrap_err().kind,"invalid_input");
            let mut other=saved_scope.clone();other.slot_id="office-2".into();assert_eq!(history_in(db,&other,request(&cursor,1),&mut Default::default()).unwrap_err().kind,"invalid_input");assert!(history_in(db,&other,request("",1),&mut Default::default())?["turns"].as_array().unwrap().is_empty());
            assert!(db.revision()?>before,"只有显式追加新轮次才改变revision");
            Ok(())
        }).await.unwrap();
        host.delete_session(&scope).await.unwrap();
        let deleted = scope.clone();
        host.inner
            .store
            .call(move |db| {
                assert_eq!(
                    db.binding_for_scope(&deleted).unwrap_err().kind,
                    "session_deleted"
                );
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn d149_legacy_child_failure_keeps_ledger_classification_without_tools() {
        use openpaw_core::{Message, SessionEvent};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:crate::HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(config).await.unwrap();
        let parent = host
            .open_session(ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: "parent".into(),
            })
            .await
            .unwrap();
        let child = host
            .open_session(ClientScope {
                namespace: "host-agent".into(),
                slot_id: "office-1".into(),
                conversation_id: "child".into(),
            })
            .await
            .unwrap();
        host.inner
            .store
            .call(move |db| {
                db.append(
                    &parent.session_id,
                    SessionEvent::RunStarted {
                        run_id: "parent".into(),
                    },
                )?;
                db.append(
                    &parent.session_id,
                    SessionEvent::Message(Message::assistant_text("父回复")),
                )?;
                db.save_run(
                    RunView {
                        run_id: "parent".into(),
                        session_id: parent.session_id.clone(),
                        state: RunState::Completed,
                        revision: 0,
                        text: String::new(),
                        error: None,
                        tools: vec![],
                        text_truncated: false,
                    },
                    0,
                )?;
                db.append(
                    &child.session_id,
                    SessionEvent::RunStarted {
                        run_id: "child".into(),
                    },
                )?;
                db.append(
                    &child.session_id,
                    SessionEvent::RunFinished {
                        run_id: "child".into(),
                        status: "failed".into(),
                        error: Some("provider error: http 402: SECRET_MUST_NOT_LEAK".into()),
                    },
                )?;
                db.save_run(
                    RunView {
                        run_id: "child".into(),
                        session_id: child.session_id.clone(),
                        state: RunState::Failed,
                        revision: 0,
                        text: String::new(),
                        error: Some(HostError::new("run_failed", "旧通用错误")),
                        tools: vec![],
                        text_truncated: false,
                    },
                    0,
                )?;
                db.save_delegation(&crate::orchestration::Delegation {
                    version: 1,
                    run_id: "parent".into(),
                    parent_session: parent.session_id.clone(),
                    parent_scope: parent.scope.clone(),
                    call_id: "call".into(),
                    role_id: "draft".into(),
                    title: "真实写作".into(),
                    input_summary: "验证".into(),
                    task_id: "task".into(),
                    child_scope: child.scope.clone(),
                    child_session: child.session_id.clone(),
                    child_run: "child".into(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    source_task_id: None,
                    work_item: None,
                })?;
                let parent = db.get_run("parent")?;
                let full_child = db.get_run("child")?;
                assert_eq!(full_child.error.as_ref().unwrap().kind, "provider_quota");
                let shown = display_in(db, &parent)?;
                assert!(
                    shown.contains(&message(&full_child)),
                    "团队节点必须保留账本细分的旧错误"
                );
                assert!(!shown.contains("SECRET_MUST_NOT_LEAK"));
                // 已确定的新分类直接沿元数据；不能被旧账本中的不同原因覆盖。
                let mut child = db.run_metadata("child")?;
                child.error = Some(HostError::new("storage_unavailable", "保存失败"));
                db.save_run(child, 0)?;
                assert!(display_in(db, &parent)?.contains("保存失败"));
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }

    #[test]
    fn d119_provider_stream_read_failure_reaches_chat_without_raw_body() {
        let raw = "provider error: stream read: error decoding response body SECRET_TOKEN https://private.invalid";
        let run = RunView {
            run_id: "original".into(),
            session_id: "original-session".into(),
            state: RunState::Failed,
            revision: 1,
            text: "已保存第一份成果".into(),
            error: Some(crate::service::safe_run_error(raw, RunState::Failed)),
            tools: vec![],
            text_truncated: false,
        };
        assert_eq!(run.error.as_ref().unwrap().kind, "provider_stream");
        assert!(message(&run).contains("响应流读取失败"));
        let displayed = display_text(&run);
        assert!(displayed.starts_with(&run.text));
        assert!(displayed.contains("本轮未完成"));
        assert!(displayed.contains("不会自动重做"));
        assert!(!displayed.contains("SECRET_TOKEN"));
        assert!(!displayed.contains("private.invalid"));
        assert_eq!(
            crate::service::safe_run_error(raw, RunState::Cancelled).kind,
            "cancelled"
        );
        assert_eq!(
            crate::service::safe_run_error(
                "tool error: stream read: arbitrary user data",
                RunState::Failed
            )
            .kind,
            "run_failed"
        );
    }

    #[test]
    fn d112_failure_notice_is_bounded_idempotent_and_never_added_to_cancellation() {
        let mut run = RunView {
            run_id: "r".into(),
            session_id: "s".into(),
            state: RunState::Failed,
            revision: 1,
            text: "😀".repeat(25000),
            error: Some(crate::service::safe_run_error(
                "http send: SECRET",
                RunState::Failed,
            )),
            tools: vec![],
            text_truncated: false,
        };
        let shown = display_text(&run);
        assert!(dto::units(&shown) <= 50000);
        assert!(shown.contains("本轮未完成：连接模型服务失败"));
        assert!(!shown.contains("SECRET"));
        assert_eq!(shown, display_text(&run));
        run.state = RunState::Cancelled;
        assert_eq!(display_text(&run), run.text);
        run.state = RunState::Completed;
        run.error = None;
        assert_eq!(display_text(&run), run.text);
    }
}
