//! 职责：将共享六类查询映射到有范围和期限的detail-v1导航。
//! 输入输出：已绑定动作到字段、分页和返回按钮。
//! 依赖：原Host::query、原账本工具事实和Store导航元数据。
//! 禁止职责：不解析Skill/Goal/Todo文件、不保存正文副本、不接受任意文件路径。
use super::dto;
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
        .call(move |db| {
            db.check_live_scope(&s)?;
            crate::service::check_scope(&db.binding(&sid)?, &s)
        })
        .await?;
    let agents_enabled = host.inner.config.agents.as_ref().is_some_and(|a| a.enabled);
    let member_child = if let Some(role) = member.filter(|r| r.id != "lead" && agents_enabled) {
        let (s, role_id, wanted) = (
            scope.clone(),
            role.id.to_owned(),
            context["childTaskId"].as_str().map(str::to_owned),
        );
        host.inner
            .store
            .call(move |db| {
                let binding = db
                    .bindings()?
                    .into_iter()
                    .find(|b| b.scope == s)
                    .ok_or_else(|| HostError::new("scope_denied", "成员会话不存在"))?;
                let parent = db
                    .runs()?
                    .into_iter()
                    .filter(|r| r.session_id == binding.session_id)
                    .max_by_key(|r| r.revision);
                let Some(parent) = parent else {
                    return Ok(None);
                };
                let choices = db.delegations(&parent.run_id)?;
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
                    choices.into_iter().rev().find(|d| d.role_id == role_id)
                };
                match choice {
                    None => Ok(None),
                    Some(d) => match db.get_run(&d.child_run) {
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
                "{}。下方历史、Skill、Goal、Todo和文件入口为当前会话范围，点击查看不会启动模型。",
                role.duty
            );
            let (snapshot, _) = host.snapshot_and_subscribe(scope).await?;
            let current = snapshot.runs.iter().max_by_key(|run| run.revision);
            let active = current.filter(|run| !run.state.is_terminal());
            let state = if let Some((_, run)) = &member_child {
                match run.state {
                    crate::RunState::Completed => "子任务已完成",
                    crate::RunState::Cancelling => "正在清理子任务",
                    crate::RunState::Failed
                    | crate::RunState::TimedOut
                    | crate::RunState::Interrupted
                    | crate::RunState::Cancelled => "子任务未完成，实际结果可查询",
                    _ if run.tools.iter().any(|t| t.state == "awaiting_approval") => {
                        "子任务等待人工批准"
                    }
                    _ => "正在独立处理分配的任务",
                }
            } else if active.is_some() && role.id == "lead" {
                "正在处理会话任务"
            } else if active.is_some_and(|r| {
                r.tools.iter().any(|t| {
                    super::projection::actor(&t.tool_name).0 == role.employee
                        && matches!(
                            t.state.as_str(),
                            "running" | "awaiting_approval" | "awaiting_resource"
                        )
                })
            }) {
                "正在处理已关联的会话操作"
            } else if agents_enabled {
                "尚无此成员的独立任务"
            } else if matches!(role.employee, "assistant" | "files" | "search") {
                "空闲，尚无分配给此成员的活动任务"
            } else {
                "当前会话未启用此功能角色，无活动任务"
            };
            fields.extend([
                json!({"label":"姓名","value":role.name}),
                json!({"label":"职责","value":role.duty}),
                json!({"label":"角色标识","value":role.id}),
                json!({"label":"运行模式","value":if agents_enabled {"已启用独立主子协作；只有真实派发才产生子运行"}else{"共享会话功能角色；当前未启用独立子Agent"}}),
                json!({"label":"当前状态","value":state}),
            ]);
            if let Some((d, run)) = &member_child {
                fields.extend([
                    json!({"label":"当前任务","value":d.title}),
                    json!({"label":"独立执行引用","value":format!("角色 {} · Run {} · Session {}",d.role_id,d.child_run,d.child_session)}),
                    json!({"label":"最近结果","value":dto::bounded(if run.text.is_empty(){"尚无可靠回复正文"}else{&run.text},1800)}),
                ]);
            }
            if let Some(run) = current.filter(|_| member_child.is_none()) {
                let tools: Vec<_> = run
                    .tools
                    .iter()
                    .filter(|tool| super::projection::actor(&tool.tool_name).0 == role.employee)
                    .collect();
                fields.push(json!({"label":"已关联操作","value":format!("最近一次会话运行中有 {} 次工具操作；这是执行记录，不代表独立Agent。",tools.len())}));
                if role.id == "lead" {
                    fields.push(json!({"label":"最近结果","value":dto::bounded(if run.text.is_empty() {"尚无回复正文"} else {&run.text},1800)}));
                }
            }
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
        for value in options.values_mut() {
            value["employeeId"] = json!(role.employee);
            value["actorId"] = json!(role.actor);
            if let Some((d, _)) = &member_child {
                value["childTaskId"] = json!(d.task_id);
            }
            if value["kind"] == "menu" {
                value["kind"] = json!("member");
            }
        }
    }
    let (id, scope) = (id.to_owned(), scope.clone());
    host.inner
        .store
        .call(move |db| {
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
                                if matches!(name.as_str(), "write_file" | "edit_file") =>
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
