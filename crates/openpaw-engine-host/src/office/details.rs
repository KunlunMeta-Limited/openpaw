//! 职责：将共享六类查询映射到有范围和期限的detail-v1导航。
//! 输入输出：已绑定动作到字段、分页和返回按钮。
//! 依赖：原Host::query、原账本工具事实和Store导航元数据。
//! 禁止职责：不解析Skill/Goal/Todo文件、不保存正文副本、不接受任意文件路径。
use super::dto;
use crate::{store::Db, RunView};
use crate::{ClientScope, Host, HostError, HostResult, QueryKind, QueryRequest};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
const CATEGORIES: [(&str, &str); 6] = [
    ("history", "历史"),
    ("tools", "工具结果 / 拟执行"),
    ("skills", "Skill"),
    ("goals", "Goal"),
    ("todo", "Todo"),
    ("files", "文件产物"),
];

fn read_cached_run(
    db: &Db,
    cache: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, RunView>>>,
    id: &str,
) -> HostResult<RunView> {
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
    {
        return Ok(cached);
    }
    let loaded = db.get_run_projection(id)?;
    if loaded.state.is_terminal() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned(), loaded.clone());
    }
    Ok(loaded)
}

/// 功能：按已验证导航生成详情并登记本次按钮上下文。
/// 参数：host/scope/session为可信绑定，id为新动作ID，context为服务端导航，cancel为操作期限。
/// 返回：官方dialog展示。
/// 错误：查询、作用域、过期或数据变化错误。
/// 状态：仅保存控制上下文，字段来自每次真实只读查询。
/// 并发与取消：所有根锁等待使用操作token；派发读取等待实际结束。
/// 权限：文件只从本会话成功工具产物白名单选择。
/// 关联：REQ-OP-014；CHG-2026-0006第9.3节。 需求/设计REQ-OP-014。
pub(super) async fn render(
    host: &Host,
    scope: &ClientScope,
    session: &str,
    id: &str,
    context: Value,
    cancel: &CancellationToken,
) -> HostResult<Value> {
    let kind = context["kind"].as_str().unwrap_or("menu");
    let mut options = serde_json::Map::new();
    let mut buttons = Vec::new();
    let mut fields = Vec::new();
    let title;
    let description;
    let member = context["employeeId"]
        .as_str()
        .map(|employee| {
            crate::agents::ROLES
                .iter()
                .find(|role| {
                    role.employee == employee && context["actorId"].as_str() == Some(role.actor)
                })
                .ok_or_else(|| HostError::new("scope_denied", "成员身份与人物绑定不匹配"))
        })
        .transpose()?;
    // 每次导航重新验证原会话，不能让旧按钮绕过删除或重绑定。
    let (s, sid) = (scope.clone(), session.to_owned());
    host.inner
        .store
        .call_priority(move |db| {
            db.check_live_scope(&s)?;
            crate::service::check_scope(&db.binding(&sid)?, &s)
        })
        .await?;
    let agents_enabled = host.inner.config.agents.as_ref().is_some_and(|a| a.enabled);
    let terminal_runs = host.inner.office_terminal_runs.clone();
    let member_child = if let Some(role) = member.filter(|r| r.id != "lead" && agents_enabled) {
        let (s, role_id, wanted) = (
            scope.clone(),
            role.id.to_owned(),
            context["childTaskId"].as_str().map(str::to_owned),
        );
        host.inner
            .store
            .call_ui(move |db| {
                let binding = db
                    .bindings()?
                    .into_iter()
                    .find(|b| b.scope == s)
                    .ok_or_else(|| HostError::new("scope_denied", "成员会话不存在"))?;
                // 同一会话的新普通问答不会删除同事上次的真实委派；按接受顺序
                // 找角色结果，不用运行结束revision把旧任务错误排到新任务前面。
                let parents = db.runs_for_session(&binding.session_id)?;
                let mut choices = Vec::new();
                for parent in parents {
                    choices.extend(db.delegations(&parent.run_id)?);
                }
                let choice = if let Some(wanted) = wanted {
                    Some(
                        choices
                            .into_iter()
                            .find(|d| d.role_id == role_id && d.task_id == wanted)
                            .ok_or_else(|| {
                                HostError::new(
                                    "snapshot_changed",
                                    "成员任务已变化，请重新打开人物详情",
                                )
                            })?,
                    )
                } else {
                    let mut candidates = Vec::new();
                    for d in choices.into_iter().filter(|d| d.role_id == role_id) {
                        match db.run_metadata(&d.child_run) {
                            Ok(run) => candidates.push((d, run)),
                            Err(e) if e.kind == "not_found" => {}
                            Err(e) => return Err(e),
                        }
                    }
                    // 同人已接受的新待办不能遮住正在执行的旧项；同状态按真实接受顺序。
                    let index = candidates
                        .iter()
                        .position(|(_, r)| {
                            matches!(
                                r.state,
                                crate::RunState::Running | crate::RunState::Cancelling
                            )
                        })
                        .or_else(|| candidates.iter().position(|(_, r)| !r.state.is_terminal()))
                        .or_else(|| candidates.len().checked_sub(1));
                    index.map(|i| candidates.remove(i).0)
                };
                match choice {
                    None => Ok(None),
                    Some(d) => match read_cached_run(db, &terminal_runs, &d.child_run) {
                        Ok(run) => Ok(Some((d, run))),
                        Err(e) if e.kind == "not_found" => Ok(None),
                        Err(e) => Err(e),
                    },
                }
            })
            .await?
    } else {
        None
    };
    let member_scope = member_child
        .as_ref()
        .map(|(d, _)| &d.child_scope)
        .unwrap_or(scope);
    let member_session = member_child
        .as_ref()
        .map(|(d, _)| d.child_session.as_str())
        .unwrap_or(session);
    let independent_empty =
        agents_enabled && member.is_some_and(|r| r.id != "lead") && member_child.is_none();
    if kind == "menu" || kind == "member" {
        if kind == "member" {
            let role = member.ok_or_else(|| HostError::new("scope_denied", "缺少成员身份"))?;
            title = format!("{} · 成员详情", role.name);
            description = format!(
                "{}。查看分工、进度与结果；运行中可刷新状态，查看详情不会重新执行任务。",
                role.duty
            );
            // 首页只用主角色当前Run；同事详情不需要解析全部父Run及其余同事正文。
            let current_run = if role.id == "lead" || !agents_enabled {
                let (s, sid, cache) = (
                    scope.clone(),
                    session.to_owned(),
                    host.inner.office_terminal_runs.clone(),
                );
                host.inner
                    .store
                    .call_ui(move |db| {
                        db.check_live_scope(&s)?;
                        crate::service::check_scope(&db.binding(&sid)?, &s)?;
                        db.runs_for_session(&sid)?
                            .into_iter()
                            .max_by_key(|r| r.revision)
                            .map(|r| read_cached_run(db, &cache, &r.run_id))
                            .transpose()
                    })
                    .await?
            } else {
                None
            };
            let current = current_run.as_ref();
            let active = current.filter(|run| !run.state.is_terminal());
            let state = if let Some((_, run)) = &member_child {
                match run.state {
                    crate::RunState::Completed => "空闲；最近子任务已完成",
                    crate::RunState::Accepted | crate::RunState::Accepting => {
                        "已接受任务，排队等待执行"
                    }
                    crate::RunState::Cancelling => "正在清理子任务",
                    crate::RunState::Failed
                    | crate::RunState::TimedOut
                    | crate::RunState::Interrupted
                    | crate::RunState::Cancelled => "空闲；最近子任务未完成，实际结果可查询",
                    _ if run.tools.iter().any(|t| t.state == "awaiting_approval") => {
                        "子任务等待人工批准"
                    }
                    _ => "正在独立处理分配的任务",
                }
            } else if role.id == "lead" {
                // 主角色拥有会话Run；不能把已完成的主Run误报成“没有独立任务”。
                match current.map(|run| run.state) {
                    None => "空闲，尚无会话任务",
                    Some(crate::RunState::Completed) => {
                        "空闲；本轮运行已完成，业务进度以任务与白板为准"
                    }
                    Some(crate::RunState::Cancelled) => "空闲；本轮任务已停止",
                    Some(crate::RunState::Cancelling) => "正在停止本轮任务，等待实际清理",
                    Some(
                        crate::RunState::Failed
                        | crate::RunState::TimedOut
                        | crate::RunState::Interrupted,
                    ) => "空闲；本轮任务未完成，实际结果可查询",
                    _ => "正在处理会话任务",
                }
            } else if !agents_enabled
                && active.is_some_and(|r| {
                    r.tools.iter().any(|t| {
                        super::projection::actor(&t.tool_name).0 == role.employee
                            && matches!(
                                t.state.as_str(),
                                "running" | "awaiting_approval" | "awaiting_resource"
                            )
                    })
                })
            {
                "正在处理已关联的会话操作"
            } else if agents_enabled {
                "尚无此成员的独立任务"
            } else if crate::agents::enabled(role, &host.inner.config) {
                "空闲，尚无分配给此成员的活动任务"
            } else {
                "当前会话未启用此功能角色，无活动任务"
            };
            fields.extend([
                json!({"label":"姓名","value":role.name}),
                json!({"label":"职责","value":role.duty}),
                json!({"label":"当前状态","value":state}),
            ]);
            let focus = member_child
                .as_ref()
                .map(|(d, _)| (d.run_id.clone(), Some(d.task_id.clone())))
                .or_else(|| current.map(|r| (r.run_id.clone(), None)));
            let tasks = if let Some(focus) = focus {
                task_views_focused(host, scope, session, Some(role.employee), Some(focus)).await?
            } else {
                Vec::new()
            };
            let business = tasks.iter().find(|(task, run, child)| {
                if let Some((d, _)) = &member_child {
                    run.run_id == d.child_run && child.as_deref() == Some(d.task_id.as_str())
                } else {
                    role.id == "lead"
                        && current.is_some_and(|r| r.run_id == run.run_id)
                        && task["id"] == format!("run:{}", run.run_id)
                }
            });
            if let Some((task, _, _)) = business {
                fields.extend([
                    json!({"label":if member_child.as_ref().is_some_and(|(_,r)|r.state.is_terminal()) || role.id=="lead" && current.is_some_and(|r|r.state.is_terminal()) {"最近任务"}else{"当前任务"},"value":task["title"]}),
                    json!({"label":"业务状态","value":task_status(task["status"].as_str().unwrap_or("queued"))}),
                    json!({"label":"已确认进度","value":format!("{}%",task["progress"].as_u64().unwrap_or(0))}),
                    json!({"label":"进度说明","value":dto::bounded(task["summary"].as_str().unwrap_or("暂无说明"),1000)}),
                ]);
            } else {
                fields.extend([
                    json!({"label":"当前任务","value":member_child.as_ref().map(|(d,_)|d.title.as_str()).unwrap_or("暂无已分配任务")}),
                    json!({"label":"已确认进度","value":if member_child.is_some(){"当前显示窗口未包含该任务，不估算进度"}else{"暂无任务，不估算进度"}}),
                ]);
            }
            if let Some((d, run)) = &member_child {
                fields.extend([
                    json!({"label":"独立执行引用","value":format!("角色 {} · Run {} · Session {}",d.role_id,d.child_run,d.child_session)}),
                    json!({"label":"最近结果","value":dto::bounded(if run.text.is_empty(){"尚无可靠回复正文"}else{&run.text},1800)}),
                ]);
            }
            if let Some(run) =
                current.filter(|_| member_child.is_none() && (!agents_enabled || role.id == "lead"))
            {
                let tools: Vec<_> = run
                    .tools
                    .iter()
                    .filter(|tool| {
                        agents_enabled
                            || super::projection::actor(&tool.tool_name).0 == role.employee
                    })
                    .collect();
                fields.push(json!({"label":"已关联操作","value":format!("最近一次会话运行中有 {} 次工具操作；这是执行记录，不代表独立Agent。",tools.len())}));
                if role.id == "lead" {
                    fields.push(json!({"label":"最近结果","value":dto::bounded(if run.text.is_empty() {"尚无回复正文"} else {&run.text},1800)}));
                }
            }
            fields.extend([
                json!({"label":"角色标识","value":role.id}),
                json!({"label":"运行模式","value":if agents_enabled {"独立协作；派发后各自执行"}else{"共享会话功能角色；未启用独立子Agent"}}),
            ]);
        } else {
            title = "OpenPaw 会话详情".to_owned();
            description =
                "同一引擎的真实账本和只读数据；工具拟执行输入与实际结果分开识别。".to_owned();
        }
        for (key, label) in CATEGORIES {
            button(
                &mut buttons,
                &mut options,
                key,
                label,
                json!({"kind":key,"offset":0,"chunk":0}),
            );
        }
        button(
            &mut buttons,
            &mut options,
            "tasks",
            "任务进度与结果",
            json!({"kind":"tasks","offset":0}),
        );
        if kind == "member" {
            button(
                &mut buttons,
                &mut options,
                "refresh",
                "刷新任务状态",
                json!({"kind":"member"}),
            );
        }
    } else if kind == "tasks" || kind == "task" {
        let focus = (kind == "task")
            .then(|| {
                member_child
                    .as_ref()
                    .map(|(d, _)| (d.run_id.clone(), Some(d.task_id.clone())))
            })
            .flatten();
        let tasks = if let Some(focus) = focus {
            task_views_focused(
                host,
                scope,
                session,
                member.map(|role| role.employee),
                Some(focus),
            )
            .await?
        } else {
            task_views(host, scope, session, member.map(|role| role.employee)).await?
        };
        if kind == "task" {
            let (task, run, _) = tasks
                .iter()
                .find(|(task, _, child)| {
                    task["id"] == context["taskId"]
                        && context["childTaskId"]
                            .as_str()
                            .is_none_or(|wanted| child.as_deref() == Some(wanted))
                })
                .ok_or_else(|| {
                    HostError::new(
                        "snapshot_changed",
                        "任务已不在当前展示范围，请重新打开任务列表",
                    )
                })?;
            title = dto::bounded(task["title"].as_str().unwrap_or("任务详情"), 160);
            description = "当前任务使用同步快照；历史任务按原委派和计划查询。执行状态与结果来自真实运行，结束回复不代表全部业务已核验。".into();
            let owner = crate::agents::ROLES
                .iter()
                .find(|r| task["assigneeId"] == r.employee)
                .map(|r| r.name)
                .unwrap_or("未分配");
            fields.extend([
                json!({"label":"负责人","value":owner}),
                json!({"label":"业务状态","value":task_status(task["status"].as_str().unwrap_or("queued"))}),
                json!({"label":"已确认进度","value":format!("{}%",task["progress"].as_u64().unwrap_or(0))}),
                json!({"label":"进度说明","value":dto::bounded(task["summary"].as_str().unwrap_or("暂无说明"),1800)}),
                json!({"label":"实际执行状态","value":match run.state {crate::RunState::Accepting=>"正在保存请求",crate::RunState::Accepted=>"已接受，等待执行",crate::RunState::Running=>"正在执行",crate::RunState::Cancelling=>"正在停止，等待清理",crate::RunState::Completed=>"执行已结束",crate::RunState::Failed=>"执行失败",crate::RunState::TimedOut=>"执行超时",crate::RunState::Cancelled=>"已停止并清理",crate::RunState::Interrupted=>"执行被中断"}}),
                json!({"label":"执行引用","value":format!("Run {} · Session {}",run.run_id,run.session_id)}),
                json!({"label":"实际回复","value":dto::bounded(if run.text.is_empty(){"尚无可靠回复正文"}else{&run.text},1800)}),
            ]);
            button(
                &mut buttons,
                &mut options,
                "back",
                "返回任务列表",
                json!({"kind":"tasks","offset":context["offset"].as_u64().unwrap_or(0)}),
            );
        } else {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for (task, run, _) in &tasks {
                task.to_string().hash(&mut hasher);
                run.revision.hash(&mut hasher);
            }
            let signature = format!("{:016x}", hasher.finish());
            if context["signature"]
                .as_str()
                .is_some_and(|s| s != signature)
            {
                return Err(HostError::new(
                    "snapshot_changed",
                    "任务已变化，请重新打开列表，避免分页错读",
                ));
            }
            let offset = context["offset"].as_u64().unwrap_or(0) as usize;
            title = "任务进度与结果".into();
            description = format!(
                "当前及历史任务共{}项，显示{}—{}；移出场景窗口的终态委派仍可查看真实结果。",
                tasks.len(),
                offset.min(tasks.len()),
                (offset + 5).min(tasks.len())
            );
            for (index, (task, _, child)) in tasks.iter().skip(offset).take(5).enumerate() {
                fields.push(json!({"label":dto::bounded(task["title"].as_str().unwrap_or("任务"),160),"value":format!("{} · 已确认{}%\n{}",task_status(task["status"].as_str().unwrap_or("queued")),task["progress"].as_u64().unwrap_or(0),dto::bounded(task["summary"].as_str().unwrap_or(""),1000))}));
                let mut target = json!({"kind":"task","taskId":task["id"],"offset":offset});
                if let Some(child) = child {
                    target["childTaskId"] = json!(child);
                }
                button(
                    &mut buttons,
                    &mut options,
                    &format!("task-{index}"),
                    &format!(
                        "查看 {}",
                        dto::bounded(task["title"].as_str().unwrap_or("任务"), 80)
                    ),
                    target,
                );
            }
            if offset > 0 {
                button(
                    &mut buttons,
                    &mut options,
                    "prev",
                    "上一页",
                    json!({"kind":"tasks","offset":offset.saturating_sub(5),"signature":signature}),
                );
            }
            if offset + 5 < tasks.len() {
                button(
                    &mut buttons,
                    &mut options,
                    "next",
                    "下一页",
                    json!({"kind":"tasks","offset":offset+5,"signature":signature}),
                );
            }
            button(
                &mut buttons,
                &mut options,
                "back",
                "返回目录",
                json!({"kind":"menu"}),
            );
        }
    } else if kind == "files" {
        title = "真实文件产物".to_owned();
        let files = if independent_empty {
            vec![]
        } else {
            artifacts(host, member_scope, member_session).await?
        };
        let offset = context["offset"].as_u64().unwrap_or(0) as usize;
        let page: Vec<_> = files.iter().skip(offset).take(5).collect();
        description = format!(
            "共{}个本会话成功写入/编辑文件；显示{}—{}。没有产物不等于项目为空。",
            files.len(),
            offset.min(files.len()),
            (offset + page.len()).min(files.len())
        );
        for (i, path) in page.iter().enumerate() {
            let key = format!("file-{i}");
            fields.push(
                json!({"label":format!("产物 {}",offset+i+1),"value":dto::bounded(path,2000)}),
            );
            button(
                &mut buttons,
                &mut options,
                &key,
                &format!("打开 {}", dto::bounded(path, 100)),
                json!({"kind":"file","path":path,"offset":0,"chunk":0}),
            );
        }
        if offset > 0 {
            button(
                &mut buttons,
                &mut options,
                "prev",
                "上一页",
                json!({"kind":"files","offset":offset.saturating_sub(5)}),
            );
        }
        if offset + 5 < files.len() {
            button(
                &mut buttons,
                &mut options,
                "next",
                "下一页",
                json!({"kind":"files","offset":offset+5}),
            );
        }
        button(
            &mut buttons,
            &mut options,
            "back",
            "返回目录",
            json!({"kind":"menu"}),
        );
    } else {
        let offset = context["offset"].as_u64().unwrap_or(0) as usize;
        let chunk = context["chunk"].as_u64().unwrap_or(0) as usize;
        let query = match kind {
            "history" => QueryKind::History,
            "tools" => QueryKind::ToolResults,
            "skills" => QueryKind::Skills,
            "goals" => QueryKind::Goals,
            "todo" => QueryKind::Todo,
            "file" => {
                let path = context["path"]
                    .as_str()
                    .ok_or_else(|| HostError::new("scope_denied", "无效文件引用"))?;
                if independent_empty
                    || !artifacts(host, member_scope, member_session)
                        .await?
                        .iter()
                        .any(|p| p == path)
                {
                    return Err(HostError::new("scope_denied", "文件不属于本会话成功产物"));
                }
                QueryKind::File { path: path.into() }
            }
            _ => return Err(HostError::new("unsupported", "未知详情类型")),
        };
        let private_kind = matches!(kind, "history" | "tools" | "file");
        let (page, signature) = if independent_empty && private_kind {
            (
                crate::QueryPage {
                    items: Vec::new(),
                    next_cursor: None,
                },
                "no-child-task".to_owned(),
            )
        } else {
            host.query_detail_page(
                if private_kind { member_scope } else { scope },
                QueryRequest {
                    session_id: if private_kind {
                        member_session
                    } else {
                        session
                    }
                    .into(),
                    kind: query,
                    offset,
                    limit: 5,
                },
                cancel,
            )
            .await?
        };
        if context["signature"]
            .as_str()
            .is_some_and(|s| s != signature)
        {
            return Err(HostError::new(
                "snapshot_changed",
                "数据已变化，请重新打开详情，避免分页错读",
            ));
        }
        let mut pieces = Vec::new();
        for (i, item) in page.items.iter().enumerate() {
            let text = if kind == "file" {
                item["text"].as_str().unwrap_or("").to_owned()
            } else {
                serde_json::to_string_pretty(item).map_err(HostError::storage)?
            };
            for (part, value) in split(&text, 1800).into_iter().enumerate() {
                pieces.push(
                    json!({"label":format!("记录 {} / 文本段 {}",offset+i+1,part+1),"value":value}),
                );
            }
        }
        fields.extend(pieces.iter().skip(chunk).take(5).cloned());
        title = if kind == "file" {
            format!(
                "文件 {}",
                dto::bounded(context["path"].as_str().unwrap_or(""), 120)
            )
        } else {
            CATEGORIES
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, t)| *t)
                .unwrap_or("详情")
                .into()
        };
        description = if page.items.is_empty() {
            "当前范围暂无数据；读取错误不会显示成空列表。".into()
        } else {
            format!("本页{}条记录，文本段{}—{}/{}。历史工具字段为原账本投影；tool_use表示拟执行，tool_result表示实际结果。",page.items.len(),chunk+1,(chunk+fields.len()).min(pieces.len()),pieces.len())
        };
        let mut current = context.clone();
        current["signature"] = json!(signature);
        if chunk > 0 {
            let mut prev = current.clone();
            prev["chunk"] = json!(chunk.saturating_sub(5));
            button(&mut buttons, &mut options, "prev", "上一段", prev);
        } else if offset > 0 {
            let mut prev = context.clone();
            prev["offset"] = json!(offset.saturating_sub(5));
            prev["chunk"] = json!(0);
            prev["signature"] = json!(signature);
            button(&mut buttons, &mut options, "prev", "上一页", prev);
        }
        if chunk + 5 < pieces.len() {
            let mut next = current.clone();
            next["chunk"] = json!(chunk + 5);
            button(&mut buttons, &mut options, "next", "下一段", next);
        } else if let Some(next) = page.next_cursor {
            let mut next_page = context.clone();
            next_page["offset"] = json!(next);
            next_page["chunk"] = json!(0);
            next_page["signature"] = json!(signature);
            button(&mut buttons, &mut options, "next", "下一页", next_page);
        }
        button(
            &mut buttons,
            &mut options,
            "back",
            "返回目录",
            json!({"kind":"menu"}),
        );
    }
    if let Some(role) = member {
        for (key, value) in options.iter_mut() {
            value["employeeId"] = json!(role.employee);
            value["actorId"] = json!(role.actor);
            if let Some((d, _)) = member_child.as_ref().filter(|_| key != "refresh") {
                if value.get("childTaskId").is_none() {
                    value["childTaskId"] = json!(d.task_id);
                }
            }
            if value["kind"] == "menu" {
                value["kind"] = json!("member");
            }
        }
    }
    let (id, scope) = (id.to_owned(), scope.clone());
    host.inner
        .store
        .call_priority(move |db| {
            db.check_live_scope(&scope)?;
            db.office_action(
                &id,
                &scope,
                "navigation",
                "open",
                chrono::Utc::now().timestamp_millis() + 600000,
                &json!({"options":options}),
            )?;
            Ok(())
        })
        .await?;
    Ok(
        json!({"target":"dialog","content":{"template":"detail-v1","title":title,"description":description,"fields":fields,"actions":buttons}}),
    )
}
fn task_status(status: &str) -> &str {
    match status {
        "completed" => "已核验完成",
        "failed" => "未完成（执行失败或中断）",
        "blocked" => "等待确认、核验或继续处理",
        "running" => "正在处理",
        _ => "待处理",
    }
}
/// 功能：复用当前投影与原委派枚举任务，补充显示窗口外的已公开终态历史。
/// 参数：host为共享服务，scope/session为当前可信父会话，employee为可选角色过滤。
/// 返回：任务、真实Run与可选独立子任务引用，不另建任务库。
/// 错误：存储损坏或引用失效时明确报错。
/// 状态：只读事实与最近同步投影；历史进度复用原计划核验，不推进进度、不执行工具。
/// 并发与取消：原Store单线程读取，删除后不能复活导航。
/// 权限：仅同办公室和同父会话，子运行必须有原委派事实。
/// 关联：CHG-2026-0007第12.21节；REQ-OP-014/021。
async fn task_views(
    host: &Host,
    scope: &ClientScope,
    session: &str,
    employee: Option<&str>,
) -> HostResult<Vec<(Value, crate::RunView, Option<String>)>> {
    task_views_focused(host, scope, session, employee, None).await
}

async fn task_views_focused(
    host: &Host,
    scope: &ClientScope,
    session: &str,
    employee: Option<&str>,
    focus: Option<(String, Option<String>)>,
) -> HostResult<Vec<(Value, crate::RunView, Option<String>)>> {
    let (scope, session, employee) = (
        scope.clone(),
        session.to_owned(),
        employee.map(str::to_owned),
    );
    let terminal_runs = host.inner.office_terminal_runs.clone();
    let activity_window = host
        .inner
        .config
        .office
        .as_ref()
        .is_some_and(|o| o.activity_window);
    let read = move |db: &mut Db| {
        db.check_live_scope(&scope)?;
        crate::service::check_scope(&db.binding(&session)?, &scope)?;
        let Some((projection, _)) = db.office_projection(&scope.slot_id)? else {
            return Ok(vec![]);
        };
        let Some(conv) = projection["conversations"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["id"] == scope.conversation_id))
        else {
            return Ok(vec![]);
        };
        let parents = db
            .runs_for_session(&session)?
            .into_iter()
            .filter(|p| focus.as_ref().is_none_or(|(parent, _)| &p.run_id == parent))
            .map(|p| {
                let delegations = db.delegations(&p.run_id)?;
                Ok((p, delegations))
            })
            .collect::<HostResult<Vec<_>>>()?;
        let mut result = Vec::new();
        for task in conv["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| employee.as_ref().is_none_or(|e| t["assigneeId"] == *e))
        {
            let mut selected = None;
            for (parent, delegations) in &parents {
                let delegated = delegations.iter().rev().find(|d| {
                    task["id"] == d.task_id
                        || conv["events"].as_array().into_iter().flatten().any(|e| {
                            e["id"] == format!("assign:{}", d.task_id)
                                && e["taskId"] == task["id"]
                                && e["employeeId"] == task["assigneeId"]
                        })
                });
                if let Some(d) = delegated {
                    if focus
                        .as_ref()
                        .is_some_and(|(_, child)| child.as_ref() != Some(&d.task_id))
                    {
                        continue;
                    }
                    selected = Some((
                        read_cached_run(db, &terminal_runs, &d.child_run)?,
                        Some(d.task_id.clone()),
                    ));
                    break;
                }
                let step_owned = task["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with(&format!("step:{}:", parent.run_id)));
                if task["id"] == format!("run:{}", parent.run_id) || step_owned {
                    if focus.as_ref().is_some_and(|(_, child)| child.is_some()) {
                        continue;
                    }
                    selected = Some((read_cached_run(db, &terminal_runs, &parent.run_id)?, None));
                    break;
                }
            }
            let (run, child) = match selected {
                Some(selected) => selected,
                None if focus.is_some() => continue,
                None => {
                    return Err(HostError::new(
                        "snapshot_changed",
                        "任务执行引用已失效，请重新同步",
                    ))
                }
            };
            result.push((task.clone(), run, child));
        }
        // 快照只是显示窗口，不是历史任务库。只补已经公开且执行结束的
        // 委派；当前卡仍优先，不把在途执行当终态，也不按回复猜完成。
        let mut covered: std::collections::BTreeSet<_> = result
            .iter()
            .filter_map(|(_, _, child)| child.clone())
            .collect();
        for (parent, delegations) in &parents {
            let continuous = db.continuous_state(&parent.run_id)?;
            let plan = if continuous.is_some_and(|c| c.started && c.completion_condition.is_none())
            {
                None
            } else {
                db.progress(&parent.run_id)?
            };
            if activity_window && employee.as_ref().is_none_or(|e| e == "office-lead") {
                let run = read_cached_run(db, &terminal_runs, &parent.run_id)?;
                let (steps, percent) = crate::task_progress::project(plan.clone(), &run)?;
                let root_id = format!("run:{}", run.run_id);
                // 多Agent总进度还依赖委派验收；父模型结束不能补造100%。
                // 这类成果按下方真实子任务恢复，目标仍由原白板/历史呈现。
                if delegations.is_empty()
                    && run.state.is_terminal()
                    && !result.iter().any(|(t, _, _)| t["id"] == root_id)
                {
                    let (_, input) = db.run_input(parent)?;
                    let state = if run.state == crate::RunState::Completed {
                        if percent == 100 {
                            "completed"
                        } else {
                            "blocked"
                        }
                    } else {
                        "failed"
                    };
                    result.push((json!({"id":root_id,"title":dto::bounded(&input,80),"assigneeId":"office-lead","status":state,"progress":percent,"summary":"历史运行；业务计划与实际结果可查询"}),run.clone(),None));
                }
                for step in steps {
                    if !result.iter().any(|(t, _, _)| t["id"] == step["id"])
                        && matches!(step["status"].as_str(), Some("completed" | "failed"))
                    {
                        let mut item = step;
                        item["assigneeId"] = json!("office-lead");
                        result.push((item, run.clone(), None));
                    }
                }
            }
            for d in delegations {
                if focus
                    .as_ref()
                    .is_some_and(|(_, child)| child.as_ref() != Some(&d.task_id))
                {
                    continue;
                }
                let role = crate::agents::role(&d.role_id)
                    .ok_or_else(|| HostError::new("storage_unavailable", "历史委派引用未知角色"))?;
                if covered.contains(&d.task_id)
                    || employee.as_ref().is_some_and(|e| e != role.employee)
                {
                    continue;
                }
                let child = match read_cached_run(db, &terminal_runs, &d.child_run) {
                    Ok(run) if run.state.is_terminal() => run,
                    Ok(_) => continue,
                    Err(e) if e.kind == "not_found" => continue,
                    Err(e) => return Err(e),
                };
                // 导航仅接受原委派中的可信子绑定，不能用浏览器提供的任务ID跨会话读取。
                crate::service::check_scope(&db.binding(&d.child_session)?, &d.child_scope)?;
                let (_, percent) =
                    crate::task_progress::execution_project(db.progress(&child.run_id)?, &child)?;
                let verified = child.state == crate::RunState::Completed
                    && plan
                        .as_ref()
                        .map(|p| {
                            p["steps"].as_array().into_iter().flatten().any(|step| {
                                step["state"] == "completed"
                                    && step["evidenceCallIds"]
                                        .as_array()
                                        .is_some_and(|ids| ids.iter().any(|id| id == &d.call_id))
                            })
                        })
                        .unwrap_or(percent == 100);
                let complete =
                    child.state == crate::RunState::Completed && (verified || percent == 100);
                let state = if complete {
                    "completed"
                } else if child.state == crate::RunState::Completed {
                    "blocked"
                } else {
                    "failed"
                };
                let summary = if complete {
                    "历史终态任务；实际回复与成果可查询".to_owned()
                } else if state == "blocked" {
                    "独立执行已结束，业务步骤仍待核验".to_owned()
                } else {
                    super::turns::message(&child)
                };
                let task = json!({"id":d.task_id,"title":d.title,"assigneeId":role.employee,"status":state,
                        "progress":if complete {100}else{percent},"summary":summary,"updatedAt":d.created_at});
                covered.insert(d.task_id.clone());
                result.push((task, child, Some(d.task_id.clone())));
            }
        }
        Ok(result)
    };
    host.inner.store.call_ui(read).await
}
fn button(
    buttons: &mut Vec<Value>,
    options: &mut serde_json::Map<String, Value>,
    id: &str,
    label: &str,
    context: Value,
) {
    buttons.push(json!({"id":id,"label":label}));
    options.insert(id.into(), context);
}
fn split(text: &str, limit: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut n = 0;
    for c in text.chars() {
        if n + c.len_utf16() > limit {
            parts.push(std::mem::take(&mut current));
            n = 0;
        }
        current.push(c);
        n += c.len_utf16();
    }
    if !current.is_empty() || parts.is_empty() {
        parts.push(current);
    }
    parts
}
async fn artifacts(host: &Host, scope: &ClientScope, session: &str) -> HostResult<Vec<String>> {
    let (s, id) = (scope.clone(), session.to_owned());
    host.inner
        .store
        .call(move |db| {
            db.check_live_scope(&s)?;
            crate::service::check_scope(&db.binding(&id)?, &s)?;
            let mut pending = std::collections::BTreeMap::new();
            let mut paths = std::collections::BTreeSet::new();
            for event in db.events(&id)? {
                if let openpaw_core::SessionEvent::Message(m) = event {
                    for b in m.content {
                        match b {
                            openpaw_core::ContentBlock::ToolUse { id, name, input }
                                if crate::task_progress::is_file_writer(&name) =>
                            {
                                if let Some(path) = input["path"].as_str() {
                                    pending.insert(id, path.to_owned());
                                }
                            }
                            openpaw_core::ContentBlock::ToolResult {
                                tool_use_id,
                                is_error: false,
                                ..
                            } => {
                                if let Some(path) = pending.remove(&tool_use_id) {
                                    paths.insert(path);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(paths.into_iter().collect())
        })
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_chunks_preserve_all_text_and_utf16_bounds() {
        let text = "a😀中文\n".repeat(2000);
        let parts = split(&text, 1800);
        assert_eq!(parts.concat(), text);
        assert!(parts.iter().all(|s| dto::units(s) <= 1800));
    }
    #[tokio::test]
    async fn d121_archived_delegation_remains_readable_without_a_snapshot_card() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":[],"agents":{"enabled":true},"office":{"activity_window":true,"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_UNUSED","allowed_origins":["http://127.0.0.1:9"]}})).unwrap();
        let provider = MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "delegate_task".into(),
                input: json!({"role_id":"organize","title":"整理已给定资料","instructions":"只整理已给定文本","acceptance":"有限文字成果"}),
                text: String::new(),
            },
            MockMode::Fixed("真实子成果已完成".into()),
            MockMode::Fixed("本轮已汇总".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(provider))
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "d121-archive".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let accepted = host
            .submit_run(&scope, &binding.session_id, "整理有限资料".into())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if host
                    .get_run(&scope, &accepted.run_id)
                    .await
                    .unwrap()
                    .state
                    .is_terminal()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let office = super::super::OfficeConfig {
            activity_window: true,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        let projected = super::super::projection::prepare(&host, &office, "office-1")
            .await
            .unwrap();
        assert!(
            projected["conversations"][0]["tasks"]
                .as_array()
                .unwrap()
                .is_empty(),
            "终态不常驻人物当前窗口"
        );
        assert!(
            !projected["conversations"][0]["whiteboard"]["detail"]
                .as_str()
                .unwrap()
                .is_empty(),
            "人物空闲不能清空业务白板"
        );
        let member = render(
            &host,
            &scope,
            &binding.session_id,
            "idle-member",
            json!({"kind":"member","employeeId":"file-agent","actorId":"file-agent"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(member["content"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["label"] == "当前状态"
                && f["value"].as_str().unwrap_or("").starts_with("空闲")));
        let before = task_views(&host, &scope, &binding.session_id, Some("file-agent"))
            .await
            .unwrap();
        assert_eq!(before.len(), 1);
        let child = before[0].2.clone().unwrap();
        host.inner
            .store
            .call(|db| {
                let (mut snapshot, state) = db.office_projection("office-1")?.unwrap();
                snapshot["conversations"][0]["tasks"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|t| t["assigneeId"] != "file-agent");
                db.save_office_projection("office-1", &snapshot, &state)
            })
            .await
            .unwrap();
        let count = host
            .snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len();
        let archived = task_views(&host, &scope, &binding.session_id, Some("file-agent"))
            .await
            .unwrap();
        assert_eq!(archived.len(), 1, "移出快照的真实终态任务仍须可以查看");
        assert_eq!(archived[0].2.as_deref(), Some(child.as_str()));
        assert_eq!(archived[0].0["status"], before[0].0["status"]);
        assert_eq!(archived[0].0["progress"], before[0].0["progress"]);
        assert_eq!(archived[0].1.text, "真实子成果已完成");
        let all = task_views(&host, &scope, &binding.session_id, None)
            .await
            .unwrap();
        assert_eq!(
            all.len(),
            1,
            "归档只恢复真实子任务，不用父模型结束补造多Agent总进度"
        );
        assert!(all.iter().all(|(_, _, child)| child.is_some()));
        let detail = render(
            &host,
            &scope,
            &binding.session_id,
            "archived-detail",
            json!({"kind":"task","taskId":child,"childTaskId":child}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(detail["content"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["label"] == "实际回复" && f["value"] == "真实子成果已完成"));
        assert_eq!(
            host.snapshot_and_subscribe(&scope)
                .await
                .unwrap()
                .0
                .runs
                .len(),
            count
        );
        let mut foreign = scope.clone();
        foreign.conversation_id = "other".into();
        assert!(task_views(&host, &foreign, &binding.session_id, None)
            .await
            .is_err());
        host.delete_session(&scope).await.unwrap();
        assert!(task_views(&host, &scope, &binding.session_id, None)
            .await
            .is_err());
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn d144_member_reads_selected_execution_without_unrelated_history() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":[],"agents":{"enabled":true},"office":{"activity_window":true,"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_UNUSED","allowed_origins":["http://127.0.0.1:9"]}})).unwrap();
        let provider = MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "delegate_task".into(),
                input: json!({"role_id":"organize","title":"整理已给定资料","instructions":"只整理已给定文本","acceptance":"有限文字成果"}),
                text: String::new(),
            },
            MockMode::Fixed("真实子成果已完成".into()),
            MockMode::Fixed("本轮已汇总".into()),
            MockMode::ToolUse {
                tool_name: "delegate_task".into(),
                input: json!({"role_id":"audit","title":"另项独立审核","instructions":"独立审核另一份给定文字","acceptance":"报告依据"}),
                text: String::new(),
            },
            MockMode::Fixed("另一位成员结果".into()),
            MockMode::Fixed("第二轮结束".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(provider))
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "d144-bounded".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let accepted = host
            .submit_run(&scope, &binding.session_id, "整理有限资料".into())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if host
                    .get_run(&scope, &accepted.run_id)
                    .await
                    .unwrap()
                    .state
                    .is_terminal()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // 终态先落盘，外层任务随后释放注册表；下一轮须等真实清理，不能将
        // get_run 的终态可见性误认为提交许可，也不能提前释放产品资源。
        tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let second = host
            .submit_run(&scope, &binding.session_id, "另一项审核".into())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let office = super::super::OfficeConfig {
            activity_window: true,
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
        };
        super::super::projection::prepare(&host, &office, "office-1")
            .await
            .unwrap();
        let run_id = second.run_id.clone();
        let other = host
            .inner
            .store
            .call(move |db| Ok(db.delegations(&run_id)?.remove(0)))
            .await
            .unwrap();
        let path = temp
            .path()
            .join("home/sessions")
            .join(format!("{}.jsonl", other.child_session));
        let original = std::fs::read(&path).unwrap();
        // 当前人物不应为获取自己的状态而读取另一角色的损坏长历史；
        // 真正查询那位角色时仍必须透传错误，不能将损坏吞成“暂无数据”。
        std::fs::write(&path, b"{broken-json\n").unwrap();
        host.inner.office_terminal_runs.lock().unwrap().clear();
        let selected = render(
            &host,
            &scope,
            &binding.session_id,
            "selected-member",
            json!({"kind":"member","employeeId":"file-agent","actorId":"file-agent"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(selected["content"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["label"] == "最近结果" && f["value"] == "真实子成果已完成"));
        assert!(
            host.get_run(&scope, &second.run_id).await.is_err(),
            "完整Run读取仍暴露真实账本损坏"
        );
        let audit = crate::agents::role("audit").unwrap();
        assert!(render(
            &host,
            &scope,
            &binding.session_id,
            "broken-member",
            json!({"kind":"member","employeeId":audit.employee,"actorId":audit.actor}),
            &CancellationToken::new()
        )
        .await
        .is_err());
        std::fs::write(&path, original).unwrap();
        let all = task_views(&host, &scope, &binding.session_id, None)
            .await
            .unwrap();
        assert_eq!(all.len(), 2, "首页按需读取不裁掉历史列表");
        assert!(all.iter().any(|(_, r, _)| r.text == "另一位成员结果"));
        let metadata = host
            .inner
            .store
            .call(move |db| db.run_metadata(&second.run_id))
            .await
            .unwrap();
        assert!(metadata.state.is_terminal());
        assert_eq!(
            host.snapshot_and_subscribe(&scope)
                .await
                .unwrap()
                .0
                .runs
                .len(),
            2,
            "只读详情不新增执行"
        );
        host.delete_session(&scope).await.unwrap();
        assert!(render(
            &host,
            &scope,
            &binding.session_id,
            "deleted-member",
            json!({"kind":"member","employeeId":"file-agent","actorId":"file-agent"}),
            &CancellationToken::new()
        )
        .await
        .is_err());
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn task_navigation_pages_real_facts_and_refuses_wrong_or_deleted_scope() {
        use openpaw_llm::{MockMode, MockProvider};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let config:crate::HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let steps: Vec<_> = (0..6)
            .map(|i| json!({"id":format!("s{i}"),"title":format!("核对资料第{}项",i+1)}))
            .collect();
        let provider = MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "report_progress".into(),
                input: json!({"type":"create","steps":steps}),
                text: String::new(),
            },
            MockMode::Fixed("仅给出说明，资料尚未核验".into()),
        ]);
        let host = Host::with_provider(config, std::sync::Arc::new(provider))
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "tasks".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let accepted = host
            .submit_run(&scope, &binding.session_id, "核对六项资料".into())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), host.wait_idle())
            .await
            .unwrap();
        let office:crate::OfficeConfig=serde_json::from_value(json!({"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"]})).unwrap();
        super::super::projection::prepare(&host, &office, "office-1")
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let member = render(
            &host,
            &scope,
            &binding.session_id,
            "member-overview",
            json!({"kind":"member","employeeId":"office-lead","actorId":"office-lead"}),
            &stop,
        )
        .await
        .unwrap();
        assert!(
            member["content"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["label"] == "已确认进度"),
            "人物首页直接显示业务进度"
        );
        assert!(
            member["content"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["id"] == "refresh"),
            "详情能主动刷新而不启动新执行"
        );
        let navigation_scope = scope.clone();
        let navigation = host
            .inner
            .store
            .call(move |db| db.office_navigation("member-overview", &navigation_scope))
            .await
            .unwrap();
        let refreshed = render(
            &host,
            &scope,
            &binding.session_id,
            "member-refreshed",
            navigation["options"]["refresh"].clone(),
            &stop,
        )
        .await
        .unwrap();
        assert_eq!(refreshed["content"], member["content"]);
        assert!(refreshed["content"]["actions"].as_array().unwrap().len() <= 8);
        let first = render(
            &host,
            &scope,
            &binding.session_id,
            "task-list",
            json!({"kind":"tasks"}),
            &stop,
        )
        .await
        .unwrap();
        assert_eq!(first["content"]["fields"].as_array().unwrap().len(), 5);
        assert!(first["content"]["actions"].as_array().unwrap().len() <= 8);
        let s = scope.clone();
        let nav = host
            .inner
            .store
            .call(move |db| db.office_navigation("task-list", &s))
            .await
            .unwrap();
        let second = render(
            &host,
            &scope,
            &binding.session_id,
            "task-page2",
            nav["options"]["next"].clone(),
            &stop,
        )
        .await
        .unwrap();
        assert_eq!(second["content"]["fields"].as_array().unwrap().len(), 2);
        let detail = render(
            &host,
            &scope,
            &binding.session_id,
            "task-detail",
            nav["options"]["task-0"].clone(),
            &stop,
        )
        .await
        .unwrap();
        let fields = detail["content"]["fields"].as_array().unwrap();
        assert!(fields
            .iter()
            .any(|f| f["label"] == "实际执行状态" && f["value"] == "执行已结束"));
        assert!(fields
            .iter()
            .any(|f| f["label"] == "业务状态" && f["value"] == "等待确认、核验或继续处理"));
        let actual = host.get_run(&scope, &accepted.run_id).await.unwrap();
        assert!(actual.text.contains("仅给出说明，资料尚未核验"));
        assert_eq!(
            fields.iter().find(|f| f["label"] == "实际回复").unwrap()["value"],
            dto::bounded(&actual.text, 1800)
        );
        let mut wrong = scope.clone();
        wrong.conversation_id = "foreign".into();
        assert!(render(
            &host,
            &wrong,
            &binding.session_id,
            "wrong",
            json!({"kind":"tasks"}),
            &stop
        )
        .await
        .is_err());
        assert!(render(
            &host,
            &scope,
            &binding.session_id,
            "wrong-task",
            json!({"kind":"task","taskId":"run:foreign"}),
            &stop
        )
        .await
        .is_err());
        assert!(render(
            &host, &scope, &binding.session_id, "changed-task-owner",
            json!({"kind":"task","taskId":format!("run:{}",accepted.run_id),"childTaskId":"previous-child-task"}), &stop,
        ).await.is_err(),"旧子任务按钮不能悄悄打开相同卡片ID对应的父执行");
        host.delete_session(&scope).await.unwrap();
        assert!(render(
            &host,
            &scope,
            &binding.session_id,
            "deleted",
            nav["options"]["task-0"].clone(),
            &stop
        )
        .await
        .is_err());
        host.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn later_navigation_refuses_changed_dataset_and_cross_scope_context() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cfg=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(cfg).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "details".into(),
        };
        let b = host.open_session(scope.clone()).await.unwrap();
        let id = b.session_id.clone();
        host.inner
            .store
            .call(move |db| {
                db.append(
                    &id,
                    openpaw_core::SessionEvent::Message(openpaw_core::Message::user_text(
                        "😀中文".repeat(6000),
                    )),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let page = render(
            &host,
            &scope,
            &b.session_id,
            "first",
            json!({"kind":"history","offset":0,"chunk":0}),
            &stop,
        )
        .await
        .unwrap();
        assert!(page["content"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| dto::units(f["value"].as_str().unwrap()) <= 1800));
        let s = scope.clone();
        let nav = host
            .inner
            .store
            .call(move |db| db.office_navigation("first", &s))
            .await
            .unwrap();
        let next = nav["options"]["next"].clone();
        assert!(next.is_object());
        let id = b.session_id.clone();
        host.inner
            .store
            .call(move |db| {
                db.append(
                    &id,
                    openpaw_core::SessionEvent::Message(openpaw_core::Message::user_text(
                        "new data",
                    )),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(render(&host, &scope, &b.session_id, "second", next, &stop)
            .await
            .is_err());
        let mut wrong = scope.clone();
        wrong.conversation_id = "another".into();
        assert!(host
            .inner
            .store
            .call(move |db| db.office_navigation("first", &wrong))
            .await
            .is_err());
        host.shutdown().await.unwrap();
    }
}
