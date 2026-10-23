//! 职责：将共享待审事实显示为Office confirmation菜单。
//! 输入输出：原ApprovalView到官方decision.request和共享CAS。
//! 依赖：共享Host、唯一Client和快照同步屏障。
//! 禁止职责：不另建授权、不延长业务期限、不从未知结果批准。
use super::{client::Client, dto, projection, OfficeConfig};
use crate::{ApprovalView, ClientScope, Host, HostResult};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// 功能：管理每个业务审批最多一个活动展示任务。
/// 参数：host/client/config/address为现有服务，stop为协调器关闭token。
/// 返回：所有展示请求已结束后返回。
/// 错误：存储或网络问题安全保留pending至到期，不批准。
/// 状态：新display ID区别于业务approval ID。
/// 并发与取消：JoinSet有界；关闭取消HTTP并等待所有任务。
/// 权限：只展示已配置Office范围。
/// 关联：REQ-OP-013；CHG-2026-0006第9.2节。 需求/设计REQ-OP-014。
pub(super) async fn run(
    host: Host,
    client: Arc<Client>,
    config: OfficeConfig,
    address: std::net::SocketAddr,
    stop: CancellationToken,
) {
    let mut tasks = JoinSet::new();
    let mut active = BTreeSet::new();
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Ok(id) = result {
                active.remove(&id);
            }
        }
        if stop.is_cancelled() {
            break;
        }
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
                let Some(b) = bindings.iter().find(|b| {
                    b.session_id == r.session_id
                        && b.scope.namespace == "office"
                        && host.inner.config.slots.contains_key(&b.scope.slot_id)
                }) else {
                    continue;
                };
                let id = a.approval_id.clone();
                active.insert(id.clone());
                let (h, c, cfg, t, s) = (
                    host.clone(),
                    client.clone(),
                    config.clone(),
                    stop.clone(),
                    b.scope.clone(),
                );
                tasks.spawn(async move {
                    let _ = display(&h, &c, &cfg, address, &s, &a, &t).await;
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
    config: &OfficeConfig,
    address: std::net::SocketAddr,
    scope: &ClientScope,
    a: &ApprovalView,
    stop: &CancellationToken,
) -> HostResult<()> {
    let remaining = a.expires_at_ms - chrono::Utc::now().timestamp_millis();
    if remaining < 1000 {
        return Ok(());
    }
    let description = description(a);
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
    let request=client.post(json!({"type":"decision.request","teamId":scope.slot_id,"conversationId":scope.conversation_id,"timeoutMs":ms,
        "decision":{"id":display,"actorId":"office-lead","kind":"confirmation","title":format!("确认本次 {}",a.binding.tool_name),"description":description,"confirmLabel":"允许本次","rejectLabel":"拒绝"}}));
    tokio::pin!(request);
    let result = loop {
        tokio::select! {biased;
            _=stop.cancelled()=>break None,
            r=&mut request=>break Some(r),
            _=tokio::time::sleep(Duration::from_millis(50))=>{
                let current=host.pending_approvals(scope).await?;
                if !current.iter().any(|v|v.approval_id==a.approval_id && v.revision==a.revision) {break None;}
            }
        }
    };
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
