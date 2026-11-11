//! 职责：将共享待审事实显示为Office confirmation菜单。
//! 输入输出：原ApprovalView到官方decision.request和共享CAS。
//! 依赖：共享Host、唯一Client和快照同步屏障。
//! 禁止职责：不另建授权、不延长业务期限、不从未知结果批准。
use super::RolePresentation;
use super::{client::Client, dto, projection};
use crate::{ApprovalView, ClientScope, Host, HostResult};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// 功能：管理每个业务审批最多一个活动展示任务。
/// 参数：host/client/address为现有服务，refresh为恢复展示通知，stop为协调器关闭token。
/// 返回：所有展示请求已结束后返回。
/// 错误：存储或网络问题安全保留pending至到期，不批准。
/// 状态：新display ID区别于业务approval ID。
/// 并发与取消：JoinSet有界；关闭取消HTTP并等待所有任务。
/// 权限：只展示已配置Office范围。
/// 关联：REQ-OP-013；CHG-2026-0006第9.2节。 需求/设计REQ-OP-014。
pub(super) async fn run(
    host: Host,
    client: Arc<Client>,
    refresh: tokio::sync::broadcast::Sender<ClientScope>,
    address: std::net::SocketAddr,
    stop: CancellationToken,
) {
    let mut tasks = JoinSet::new();
    let mut active = BTreeSet::new();
    let mut last_revision = None;
    let mut last_scan = tokio::time::Instant::now() - Duration::from_secs(1);
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Ok(id) = result {
                active.remove(&id);
            }
        }
        if stop.is_cancelled() {
            break;
        }
        let revision = host.inner.store.call(|db| db.revision()).await.ok();
        let now = tokio::time::Instant::now();
        if revision == last_revision && now.duration_since(last_scan) < Duration::from_secs(1) {
            tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
            continue;
        }
        last_revision = revision;
        last_scan = now;
        let pending = host
            .inner
            .store
            .call(|db| Ok((db.bindings()?, db.approvals()?)))
            .await;
        if let Ok((bindings, approvals)) = pending {
            for a in approvals.into_iter().filter(|a| a.status == "pending") {
                if active.contains(&a.approval_id)
                    || active.len() >= host.inner.config.max_active_runs
                {
                    continue;
                }
                let run = a.binding.run_id.clone();
                let Ok(r) = host.inner.store.call(move |db| db.get_run(&run)).await else {
                    continue;
                };
                let display_scope = if let Some(b) = bindings
                    .iter()
                    .find(|b| b.session_id == r.session_id && b.scope.namespace == "office")
                {
                    Some(b.scope.clone())
                } else {
                    let run = r.run_id.clone();
                    host.inner
                        .store
                        .call(move |db| Ok(db.delegation_for_child(&run)?.map(|d| d.parent_scope)))
                        .await
                        .ok()
                        .flatten()
                };
                let Some(display_scope) =
                    display_scope.filter(|s| host.inner.config.slots.contains_key(&s.slot_id))
                else {
                    continue;
                };
                let id = a.approval_id.clone();
                active.insert(id.clone());
                let (h, c, updates, t, s) = (
                    host.clone(),
                    client.clone(),
                    refresh.clone(),
                    stop.clone(),
                    display_scope,
                );
                tasks.spawn(async move {
                    let _ = display(&h, &c, &updates, address, &s, &a, &t).await;
                    id
                });
            }
        }
        tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
    }
    while tasks.join_next().await.is_some() {}
}
async fn display(
    host: &Host,
    client: &Client,
    updates: &tokio::sync::broadcast::Sender<ClientScope>,
    address: std::net::SocketAddr,
    scope: &ClientScope,
    a: &ApprovalView,
    stop: &CancellationToken,
) -> HostResult<()> {
    // 先订阅再同步展示，避免恢复通知落在读取快照与发菜单之间。
    let mut refresh = updates.subscribe();
    let Some(config) = host.inner.config.office.as_ref() else {
        return Ok(());
    };
    let remaining = a.expires_at_ms - chrono::Utc::now().timestamp_millis();
    if remaining < 1000 {
        return Ok(());
    }
    let run = a.binding.run_id.clone();
    let owner = host
        .inner
        .store
        .call(move |db| db.delegation_for_child(&run))
        .await?;
    let role = owner.as_ref().and_then(|d| crate::agents::role(&d.role_id));
    let title = role
        .map(|r| format!("{}：确认本次 {}", r.name, a.binding.tool_name))
        .unwrap_or_else(|| format!("确认本次 {}", a.binding.tool_name));
    let actor = role.map(|r| r.actor()).unwrap_or("office-lead");
    let description = if let Some(d) = &owner {
        format!("子任务：{}\n{}", d.title, description(a))
    } else {
        description(a)
    };
    if dto::units(&description) > 2000 {
        host.decide_approval(scope, &a.approval_id, a.revision, a.binding.clone(), false)
            .await?;
        return Ok(());
    }
    tokio::select! {_=stop.cancelled()=>return Ok(()),r=projection::sync_once(host,config,&scope.slot_id,client,false,address)=>{r?;}}
    let (_, server) = client
        .post(json!({"type":"office.read","teamId":scope.slot_id}))
        .await?;
    if !server["snapshot"]["conversations"]
        .as_array()
        .is_some_and(|v| v.iter().any(|v| v["id"] == scope.conversation_id))
    {
        return Ok(());
    }
    let ms = (a.expires_at_ms - chrono::Utc::now().timestamp_millis()).min(300000);
    if ms < 1000 {
        return Ok(());
    }
    let display = uuid::Uuid::new_v4().simple().to_string();
    let (id, d, s, expiry) = (
        a.approval_id.clone(),
        display.clone(),
        scope.clone(),
        a.expires_at_ms,
    );
    host.inner
        .store
        .call(move |db| db.office_decision(&id, &d, &s, expiry, "active"))
        .await?;
    let result = {
        let request=client.post(json!({"type":"decision.request","teamId":scope.slot_id,"conversationId":scope.conversation_id,"timeoutMs":ms,
        "decision":{"id":display,"actorId":actor,"kind":"confirmation","title":title,"description":description,"confirmLabel":"允许本次","rejectLabel":"拒绝"}}));
        tokio::pin!(request);
        loop {
            tokio::select! {biased;
                _=stop.cancelled()=>break None,
                changed=refresh.recv()=>{
                    match changed {
                        Ok(s) if s != *scope => continue,
                        _ => break None,
                    }
                }
                r=&mut request=>break Some(r),
                _=tokio::time::sleep(Duration::from_millis(50))=>{
                    let current=host.pending_approvals(scope).await?;
                    if !current.iter().any(|v|v.approval_id==a.approval_id && v.revision==a.revision) {break None;}
                }
            }
        }
    }; // 在下一次展示之前drop旧HTTP；Office据连接关闭撤销旧display，绝不重置业务期限。
    let state = match result {
        Some(Ok((_, data))) => {
            // Client已严格核对作用域/display/choice/label；共享CAS仍重新检查取消与期限。
            host.decide_approval(
                scope,
                &a.approval_id,
                a.revision,
                a.binding.clone(),
                data["choiceId"] == "confirm",
            )
            .await?;
            "decided"
        }
        Some(Err(ref e)) if e.kind == "decision_expired" => {
            let _ = host
                .decide_approval(scope, &a.approval_id, a.revision, a.binding.clone(), false)
                .await;
            "expired"
        }
        Some(Err(e)) => {
            eprintln!("engine-host Office decision: {} {}", e.kind, e.message);
            "unknown"
        }
        _ => "unknown",
    };
    let (id, d, s, expiry) = (
        a.approval_id.clone(),
        display,
        scope.clone(),
        a.expires_at_ms,
    );
    host.inner
        .store
        .call(move |db| db.office_decision(&id, &d, &s, expiry, state))
        .await?;
    // 失败重试必须退避且沿用原业务期限；绝不从HTTP成功推断已执行。
    tokio::select! {_=stop.cancelled()=>{},_=tokio::time::sleep(Duration::from_secs(1))=>{}}
    Ok(())
}
fn description(a: &ApprovalView) -> String {
    let b = &a.binding;
    if b.tool_name == "bash" {
        format!("拟执行命令（本次允许）：\n{}\n普通本机执行，可访问当前用户权限内资源；工作区锁不是OS隔离。允许后仍校验输入和资源。",b.input["command"].as_str().unwrap_or("缺少命令，拒绝执行"))
    } else {
        let mut input = b.input.clone();
        if let Some(o) = input.as_object_mut() {
            for k in [
                "content",
                "old_text",
                "new_text",
                "old_string",
                "new_string",
            ] {
                if let Some(v) = o.get(k).and_then(|v| v.as_str()) {
                    let n = v.encode_utf16().count();
                    o.insert(
                        k.into(),
                        json!(format!("{n} UTF-16单元；完整拟执行内容见人物详情→工具结果")),
                    );
                }
            }
        }
        format!("拟执行 {}\n{}\n仅允许本次精确输入；拒绝不会修改文件。批准后再取得工作区锁并校验文件前置条件。",b.tool_name,input)
    }
}
