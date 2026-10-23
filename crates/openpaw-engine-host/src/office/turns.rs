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
    check_active(host, scope).await?;
    let history_scope = scope.clone();
    let page = host.inner.store.call(move|db|{
        let scope = history_scope;
        db.check_live_scope(&scope)?;
        let Some(binding)=db.bindings()?.into_iter().find(|b|b.scope==scope) else {
            if !request.cursor.is_empty(){return Err(HostError::new("invalid_input","游标不属于此会话"));}
            return Ok(json!({"turns":[],"nextCursor":"","hasMore":false,"activeTurnId":""}));
        };
        let mut rows=Vec::new();
        for (offset,user,assistant) in db.imported_turns(&binding.session_id)? {
            let key=format!("import:{}:{offset}",binding.session_id);
            rows.push((offset,key.clone(),json!({"id":format!("legacy:{key}"),"user":dto::bounded(&user,4000),"assistant":dto::bounded(&assistant,50000),"state":"complete","error":""})));
        }
        for r in db.runs()?.into_iter().filter(|r|r.session_id==binding.session_id){
            let (offset,input)=db.run_input(&r)?;
            if input.is_empty(){continue;}
            let r=db.get_run(&r.run_id)?;
            rows.push((offset,r.run_id.clone(),json!({"id":db.request_key(&r.run_id)?,"user":dto::bounded(&input,4000),"assistant":dto::bounded(&r.text,50000),"state":state(&r),"error":message(&r)})));
        }
        rows.sort_by_key(|(offset,key,_)|(*offset,key.clone()));
        let end=if request.cursor.is_empty(){rows.len()} else {
            let key=request.cursor.strip_prefix("before:").ok_or_else(||HostError::new("invalid_input","游标格式无效"))?;
            rows.iter().position(|(_,id,_)|id==key).ok_or_else(||HostError::new("invalid_input","游标不属于此会话"))?
        };
        let mut start=end.saturating_sub(request.limit);
        let mut turns=Vec::new();
        for (_,_,turn) in &rows[start..end] {turns.push(turn.clone());}
        // 按传输字节限制整页，保留最近内容且游标仍指向真实Run。
        while serde_json::to_vec(&turns).map_err(HostError::storage)?.len()>8*1024*1024-1024 {
            if turns.len()<=1{return Err(HostError::new("protocol_error","单轮历史超过传输上限"));}
            turns.remove(0);start+=1;
        }
        let active=turns.last().filter(|t|t["state"]=="pending").and_then(|t|t["id"].as_str()).unwrap_or("");
        Ok(json!({"turns":turns,"nextCursor":if start>0{format!("before:{}",rows[start].1)}else{String::new()},"hasMore":start>0,"activeTurnId":active}))
    }).await?;
    host.refresh_office_ui(scope).await;
    Ok(page)
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
