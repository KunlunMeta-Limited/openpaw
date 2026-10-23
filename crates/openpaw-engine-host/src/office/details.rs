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
    if kind == "menu" {
        title = "OpenPaw 会话详情".to_owned();
        description = "同一引擎的真实账本和只读数据；工具拟执行输入与实际结果分开识别。".to_owned();
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
        let files = artifacts(host, scope, session).await?;
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
                if !artifacts(host, scope, session)
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
        let (page, signature) = host
            .query_detail_page(
                scope,
                QueryRequest {
                    session_id: session.into(),
                    kind: query,
                    offset,
                    limit: 5,
                },
                cancel,
            )
            .await?;
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
