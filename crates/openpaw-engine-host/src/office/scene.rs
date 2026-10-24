//! 职责：按真实生命周期发送短暂且可丢弃的场景反馈。
//! 输入输出：当前Run和审批状态到受支持的scene.say。
//! 依赖：原共享Host事实、Office能力与唯一Client。
//! 禁止职责：不播放历史交接，不重复发送visit，不将forwarded当业务完成。
use super::client::Client;
use crate::Host;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
/// 功能：对观察期间新出现的真实生命周期发有限短句。
/// 参数：host/client为共享服务，stop为关闭token。
/// 返回：关闭后结束。
/// 错误：临时反馈失败丢弃，不改变Run结果。
/// 状态：启动首帧作为基线，旧终态和离线历史不补播。
/// 并发与取消：串行发送、单队列最多32/办公室、TTL10秒；可取消HTTP。
/// 权限：只发送固定公开摘要，不含文件或命令正文。
/// 关联：REQ-OP-015；CHG-2026-0006第9.6节。 需求/设计REQ-OP-014。
pub(super) async fn run(host: Host, client: Arc<Client>, stop: CancellationToken) {
    let mut seen = BTreeSet::new();
    let mut first = true;
    let mut last_revision = None;
    loop {
        if stop.is_cancelled() {
            return;
        }
        // 场景反馈只由Host事实变化驱动；空闲时不要每200ms反序列化所有Run
        // 和审批，这会与人物详情共享Store队列并放大点击延迟。
        let revision = host.inner.store.call(|db| db.revision()).await.ok();
        if revision == last_revision {
            tokio::select! {_=stop.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(200))=>{}}
            continue;
        }
        last_revision = revision;
        let facts = host
            .inner
            .store
            .call(|db| Ok((db.bindings()?, db.runs()?, db.approvals()?)))
            .await;
        if let Ok((bindings, runs, approvals)) = facts {
            let mut counts = std::collections::BTreeMap::new();
            let observed = chrono::Utc::now();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            // 只观察各会话最新Run；去重集随当前范围收敛，不随终生历史无限增长。
            let latest: std::collections::BTreeMap<_, _> =
                runs.iter()
                    .fold(std::collections::BTreeMap::new(), |mut m, r| {
                        let entry = m.entry(r.session_id.clone()).or_insert(r);
                        if r.revision > entry.revision {
                            *entry = r;
                        }
                        m
                    });
            let current: BTreeSet<_> = latest
                .values()
                .flat_map(|r| {
                    ["received", "approval", "terminal"].map(|s| format!("{}:{s}", r.run_id))
                })
                .collect();
            seen.retain(|id| current.contains(id));
            for run in latest.into_values() {
                let Some(binding) = bindings
                    .iter()
                    .find(|b| b.session_id == run.session_id && b.scope.namespace == "office")
                else {
                    continue;
                };
                let stage = if run.state.is_terminal() {
                    "terminal"
                } else if approvals
                    .iter()
                    .any(|a| a.binding.run_id == run.run_id && a.status == "pending")
                {
                    "approval"
                } else {
                    "received"
                };
                let event = format!("{}:{stage}", run.run_id);
                if !seen.insert(event.clone()) || first {
                    continue;
                }
                let count = counts.entry(binding.scope.slot_id.clone()).or_insert(0);
                if *count >= 32 {
                    continue;
                }
                *count += 1;
                let command = uuid::Uuid::new_v4().simple().to_string();
                let expiry = observed.timestamp_millis() + 10000;
                let (e, c) = (event.clone(), command.clone());
                if host
                    .inner
                    .store
                    .call(move |db| db.office_scene(&e, &c, expiry, "queued"))
                    .await
                    .is_err()
                {
                    continue;
                }
                // 同一批观察共用期限：前一办公室的网络等待不能让后续旧反馈获得新TTL。
                let send = async {
                    let caps = client.capabilities().await?;
                    if !caps["scene"]["capabilities"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v["id"] == "scene.say"))
                        || !caps["scene"]["actors"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v["id"] == "office-lead"))
                    {
                        return Ok("dropped");
                    }
                    let team = &binding.scope.slot_id;
                    let text = match stage {
                        "approval" => "需要确认后才能执行本次操作",
                        "terminal" => "本次运行已结束，详情请查看任务和会话",
                        _ => "已收到请求，开始处理",
                    };
                    client.post(json!({"teamId":team,"conversationId":binding.scope.conversation_id,"command":{"protocolVersion":"2.0","sceneId":team,"commandId":command,"type":"activity.start","capability":"scene.say","participants":[{"entityId":"office-lead","role":"actor"}],"params":{"text":text,"durationMs":2000},"expiresAt":chrono::DateTime::from_timestamp_millis(expiry).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"timeoutMs":3000,"busyPolicy":"reject"}})).await?;
                    Ok::<_, crate::HostError>("forwarded")
                };
                let state = tokio::select! {
                    _=stop.cancelled()=>"dropped",
                    result=tokio::time::timeout_at(deadline,send)=>match result {
                        Ok(Ok(state))=>state,
                        Ok(Err(_))=>"failed",
                        Err(_)=>"expired",
                    }
                };
                // forwarded只代表Fastify发给在线连接，不能写成动画完成；失败不重试补播。
                let _ = host
                    .inner
                    .store
                    .call(move |db| db.office_scene(&event, &command, expiry, state))
                    .await;
            }
            first = false;
        }
        tokio::select! {_=stop.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(200))=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientScope, HostConfig};
    #[test]
    fn scene_delivery_is_bounded_and_never_replays_offline_history() {
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "office::scene::tests::scene_fixture_child",
                "--nocapture"
            ])
            .env("OPENPAW_SCENE_CHILD", "1")
            .env("PAWOFFICE_SCENE_TEST_KEY", "scene-fixture")
            .status()
            .unwrap()
            .success());
    }
    #[test]
    fn scene_fixture_child() {
        if std::env::var("OPENPAW_SCENE_CHILD").as_deref() != Ok("1") {
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(scenario());
    }
    async fn scenario() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use serde_json::Value;
        #[derive(Default)]
        struct Fixture {
            offline: bool,
            commands: Vec<Value>,
        }
        let state = Arc::new(tokio::sync::Mutex::new(Fixture::default()));
        async fn handle(
            State(s): State<Arc<tokio::sync::Mutex<Fixture>>>,
            Json(r): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            if r["type"] == "office.capabilities" {
                return (
                    StatusCode::OK,
                    Json(
                        json!({"code":200,"msg":"fixture","data":{"scene":{"protocolVersion":"2.0","actors":[{"id":"office-lead"}],"capabilities":[{"id":"scene.say"}]},"ui":{"whiteboardSchema":{}},"requests":{},"responses":{"success":{},"error":{}}}}),
                    ),
                );
            }
            assert_eq!(r["command"]["capability"], "scene.say");
            assert_eq!(r["teamId"], r["command"]["sceneId"]);
            assert_eq!(r["conversationId"], "scene");
            assert_eq!(r["command"]["busyPolicy"], "reject");
            assert_eq!(r["command"]["timeoutMs"], 3000);
            let expiry =
                chrono::DateTime::parse_from_rfc3339(r["command"]["expiresAt"].as_str().unwrap())
                    .unwrap()
                    .timestamp_millis();
            assert!((1..=10000).contains(&(expiry - chrono::Utc::now().timestamp_millis())));
            let mut s = s.lock().await;
            s.commands.push(r);
            if s.offline {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"code":409,"msg":"offline","data":{"reasonCode":"NO_RECEIVER"}})),
                )
            } else {
                (
                    StatusCode::OK,
                    Json(json!({"code":200,"msg":"forwarded","data":{"forwarded":true}})),
                )
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server_stop = CancellationToken::new();
        let token = server_stop.clone();
        let s = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/pawoffice/messages", post(handle))
                    .with_state(s),
            )
            .with_graceful_shutdown(token.cancelled_owned())
            .await
            .unwrap();
        });
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let office:super::super::OfficeConfig=serde_json::from_value(json!({"base_url":base,"api_key_env":"PAWOFFICE_SCENE_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"]})).unwrap();
        let host = Host::start(cfg.clone()).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "scene".into(),
        };
        let b = host.open_session(scope.clone()).await.unwrap();
        host.submit_run(&scope, &b.session_id, "historical run".into())
            .await
            .unwrap();
        host.wait_idle().await;
        let stop = CancellationToken::new();
        let worker = tokio::spawn(run(
            host.clone(),
            Arc::new(Client::new(&office).unwrap()),
            stop.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(state.lock().await.commands.is_empty(), "启动不能补播旧终态");
        let first = host
            .submit_run(&scope, &b.session_id, "new run".into())
            .await
            .unwrap();
        host.wait_idle().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state.lock().await.commands.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        state.lock().await.offline = true;
        let second = host
            .submit_run(&scope, &b.session_id, "offline run".into())
            .await
            .unwrap();
        host.wait_idle().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state.lock().await.commands.len() == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        state.lock().await.offline = false;
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(
            state.lock().await.commands.len(),
            2,
            "联网恢复不能补播失败指令"
        );
        assert_eq!(
            host.get_run(&scope, &second.run_id).await.unwrap().state,
            crate::RunState::Completed
        );
        stop.cancel();
        worker.await.unwrap();
        host.shutdown().await.unwrap();
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        for (r, want) in [(first.run_id, "forwarded"), (second.run_id, "failed")] {
            let actual: String = db
                .query_row(
                    "SELECT status FROM host_office_scene WHERE event_id=?1",
                    [format!("{r}:terminal")],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual, want);
        }
        server_stop.cancel();
        server.await.unwrap();
    }
}
