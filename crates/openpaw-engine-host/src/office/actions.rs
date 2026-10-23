//! 职责：唯一ui.next领取、持久登记、有界分派及官方响应。
//! 输入输出：原Office动作到共享删除或只读详情。
//! 依赖：唯一Client、共享Host、原完整快照协调器。
//! 禁止职责：不模拟任务暂停/改派，不竞争领取，不以丢失响应回滚删除。
use super::{client::Client, details, projection, workspaces, OfficeConfig};
use crate::{ClientScope, Host, HostError, HostResult};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

/// 功能：领取后立即登记并交给最多16个处理任务。
/// 参数：host/client/config/address为已有服务，stop为关闭token。
/// 返回：领取和处理任务全部join后返回。
/// 错误：网络失败退避；动作失败返回明确结果。
/// 状态：队列最多128；领取协议无ACK，登记前断线可能丢失一次操作。
/// 并发与取消：关闭取消长轮询和读取，删除仍完成持久生命周期。
/// 权限：完整team/conversation和具体成员/任务验证。
/// 关联：REQ-OP-014；CHG-2026-0006第9.3节。 需求/设计REQ-OP-014。
pub(super) async fn run(
    host: Host,
    client: Arc<Client>,
    config: OfficeConfig,
    address: std::net::SocketAddr,
    stop: CancellationToken,
) {
    let (tx, mut rx) = mpsc::channel(128);
    let (h, c, t) = (host.clone(), client.clone(), stop.clone());
    let poller = tokio::spawn(async move {
        loop {
            let permit = tokio::select! {_=t.cancelled()=>break,p=tx.reserve()=>match p{Ok(p)=>p,Err(_)=>break}};
            let response = tokio::select! {_=t.cancelled()=>break,r=c.post(json!({"type":"ui.next","timeoutMs":30000}))=>r};
            match response {
                Ok((_, data)) => {
                    for action in data["actions"].as_array().into_iter().flatten() {
                        let action = action.clone();
                        let scope = scope(&action);
                        let id = action["id"].as_str().unwrap().to_owned();
                        let kind = action["action"].as_str().unwrap().to_owned();
                        let expiry = chrono::Utc::now().timestamp_millis() + 50000;
                        let saved = h
                            .inner
                            .store
                            .call(move |db| {
                                db.office_action(&id, &scope, &kind, "claimed", expiry, &json!({}))
                            })
                            .await;
                        if saved == Ok(true) {
                            permit.send((action, expiry));
                            break;
                        }
                    }
                }
                Err(_) => {
                    drop(permit);
                    tokio::select! {_=t.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                }
            }
        }
    });
    let mut tasks = JoinSet::new();
    loop {
        if stop.is_cancelled() {
            break;
        }
        while tasks.len() >= 16 {
            tokio::select! {_=stop.cancelled()=>break,_=tasks.join_next()=>{}}
            if stop.is_cancelled() {
                break;
            }
        }
        tokio::select! {_=stop.cancelled()=>break,item=rx.recv()=>{
            let Some((action,expiry))=item else {break;};let(h,c,cfg,t)=(host.clone(),client.clone(),config.clone(),stop.clone());
            tasks.spawn(async move {let _=handle(&h,&c,&cfg,address,&action,expiry,&t).await;});
        },_=tasks.join_next(),if !tasks.is_empty()=>{}}
    }
    drop(rx);
    let _ = poller.await;
    while tasks.join_next().await.is_some() {}
}
fn scope(action: &Value) -> ClientScope {
    ClientScope {
        namespace: "office".into(),
        slot_id: action["teamId"].as_str().unwrap_or("").into(),
        conversation_id: action["conversationId"].as_str().unwrap_or("").into(),
    }
}
async fn handle(
    host: &Host,
    client: &Client,
    config: &OfficeConfig,
    address: std::net::SocketAddr,
    action: &Value,
    expiry: i64,
    stop: &CancellationToken,
) -> HostResult<()> {
    if stop.is_cancelled() || chrono::Utc::now().timestamp_millis() >= expiry {
        return Err(HostError::new(
            "ui_expired",
            "领取操作在处理前已过期，未执行业务",
        ));
    }
    let token = stop.child_token();
    let (deadline, t) = (expiry, token.clone());
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(
            (deadline - chrono::Utc::now().timestamp_millis()).max(0) as u64,
        ))
        .await;
        t.cancel();
    });
    let _guard = token.clone().drop_guard();
    // 新建动作的team/conversation按官方协议为空，不进入普通会话作用域分派。
    // 槽发布门必须跨ui.respond持有，否则同步线程会提前注册办公室导致官方409。
    let mut creation_guard = None;
    let dispatched = if action["action"] == "office.create" {
        workspaces::create(host, client, config, action, &token)
            .await
            .map(|(presentation, guard)| {
                creation_guard = Some(guard);
                presentation
            })
    } else {
        dispatch(host, client, config, address, action, &token).await
    };
    let presentation = dispatched.unwrap_or_else(
        |e| json!({"target":"result","success":false,"message":format!("{}：{}",e.kind,e.message)}),
    );
    let reply = if token.is_cancelled() {
        Err(HostError::new(
            "ui_expired",
            "操作已过期或关闭，未确认UI送达",
        ))
    } else {
        tokio::select! {_=token.cancelled()=>Err(HostError::new("ui_expired","操作未确认送达")),r=client.post(json!({"type":"ui.respond","interactionId":action["id"],"presentation":presentation}))=>r.map(|_|())}
    };
    drop(creation_guard);
    timer.abort();
    let _ = timer.await;
    let (id, s) = (
        action["id"].as_str().unwrap_or("").to_owned(),
        scope(action),
    );
    let status = if reply.is_ok() {
        "delivered"
    } else {
        "unconfirmed"
    };
    host.inner
        .store
        .call(move |db| db.office_action_state(&id, &s, status))
        .await?;
    if reply.is_err() {
        eprintln!("engine-host Office UI: 响应未确认送达，已提交业务不回滚");
    }
    reply
}
async fn dispatch(
    host: &Host,
    client: &Client,
    config: &OfficeConfig,
    address: std::net::SocketAddr,
    action: &Value,
    cancel: &CancellationToken,
) -> HostResult<Value> {
    let scope = scope(action);
    if !host.inner.config.slots.contains_key(&scope.slot_id) {
        return Err(HostError::new("scope_denied", "办公室未授权"));
    }
    if action["action"] == "conversation.delete" {
        // 业务删除不随UI60秒期限回滚；成功响应必须在服务端最新快照移除后。
        host.delete_session(&scope).await?;
        let sync = projection::sync_once(host, config, &scope.slot_id, client, false, address);
        tokio::select! {_=cancel.cancelled()=>return Err(HostError::new("ui_expired","已删除，UI未确认")),r=sync=>{r?;}}
        let (_, read) = client
            .post(json!({"type":"office.read","teamId":scope.slot_id}))
            .await?;
        if read["snapshot"]["conversations"]
            .as_array()
            .is_none_or(|v| v.iter().any(|c| c["id"] == scope.conversation_id))
        {
            return Err(HostError::new(
                "snapshot_required",
                "已删除，等待服务端完整快照移除",
            ));
        }
        return Ok(
            json!({"target":"result","success":true,"message":"会话已删除；运行已清理，工作区文件保留。"}),
        );
    }
    let s = scope.clone();
    let binding = host
        .inner
        .store
        .call(move |db| {
            db.check_live_scope(&s)?;
            db.bindings()?
                .into_iter()
                .find(|b| b.scope == s)
                .ok_or_else(|| HostError::new("not_found", "此会话尚未接入Host"))
        })
        .await?;
    let snapshot = projection::prepare(host, config, &scope.slot_id).await?;
    let conv = snapshot["conversations"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["id"] == scope.conversation_id))
        .ok_or_else(|| HostError::new("not_found", "会话尚未产生可展示事实"))?;
    let context = match action["action"].as_str().unwrap_or("") {
        "actor.inspect" => {
            if conv["employees"]
                .as_array()
                .is_none_or(|v| !v.iter().any(|v| v["sceneActorId"] == action["actorId"]))
            {
                return Err(HostError::new("scope_denied", "该人物未绑定此会话成员"));
            }
            let member = conv["employees"]
                .as_array()
                .and_then(|v| v.iter().find(|v| v["sceneActorId"] == action["actorId"]))
                .unwrap();
            json!({"kind":"member","employeeId":member["id"],"actorId":member["sceneActorId"]})
        }
        "employee.inspect" => {
            if conv["employees"]
                .as_array()
                .is_none_or(|v| !v.iter().any(|v| v["id"] == action["employeeId"]))
            {
                return Err(HostError::new("scope_denied", "成员不属于当前会话"));
            }
            let member = conv["employees"]
                .as_array()
                .and_then(|v| v.iter().find(|v| v["id"] == action["employeeId"]))
                .unwrap();
            json!({"kind":"member","employeeId":member["id"],"actorId":member["sceneActorId"]})
        }
        "dialog.action" => {
            let (s, id) = (
                scope.clone(),
                action["interactionId"].as_str().unwrap_or("").to_owned(),
            );
            let nav = host
                .inner
                .store
                .call(move |db| db.office_navigation(&id, &s))
                .await?;
            nav["options"]
                .get(action["optionId"].as_str().unwrap_or(""))
                .cloned()
                .ok_or_else(|| HostError::new("scope_denied", "按钮不属于当前详情"))?
        }
        "task.start" | "task.block" | "task.complete" | "task.assign" => {
            if conv["tasks"]
                .as_array()
                .is_none_or(|v| !v.iter().any(|v| v["id"] == action["taskId"]))
            {
                return Err(HostError::new("scope_denied", "任务不属于当前会话"));
            }
            if action["action"] == "task.assign"
                && conv["employees"]
                    .as_array()
                    .is_none_or(|v| !v.iter().any(|v| v["id"] == action["assigneeId"]))
            {
                return Err(HostError::new("scope_denied", "目标成员不属于当前会话"));
            }
            return Ok(
                json!({"target":"result","success":false,"message":"尚不支持任务重放、暂停、手动完成或改派执行。请从聊天提出新请求；停止运行请使用聊天停止按钮。真实运行状态未改变。"}),
            );
        }
        _ => return Err(HostError::new("unsupported", "不支持此操作")),
    };
    details::render(
        host,
        &scope,
        &binding.session_id,
        action["id"].as_str().unwrap_or(""),
        context,
        cancel,
    )
    .await
}
