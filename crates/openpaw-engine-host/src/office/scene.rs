//! 职责：按真实生命周期发送短暂且可丢弃的场景反馈。
//! 输入输出：当前Run和审批状态到scene.say，真实子Run完成到office.visit汇报。
//! 依赖：原共享Host事实、Office能力与唯一Client。
//! 禁止职责：不补播历史，不重复前向分配，不将forwarded当动画或业务完成。
use super::client::Client;
use crate::Host;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
/// 功能：对观察期间新出现的生命周期发有限短句，并为真实完成子任务发送一次汇报。
/// 参数：host/client为共享服务，stop为关闭token。
/// 返回：关闭后结束。
/// 错误：临时反馈失败丢弃，不改变Run结果。
/// 状态：启动首帧作为基线，旧终态和离线历史不补播。
/// 并发与取消：每批每办公室最多32项；短句有效10秒、完成汇报120秒，HTTP等待可取消。
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
        let previous_revision = last_revision;
        let facts = host
            .inner
            .store
            .call(|db| Ok((db.revision()?, db.bindings()?, db.runs()?, db.approvals()?)))
            .await;
        if let Ok((revision, bindings, runs, approvals)) = facts {
            // 水位与元数据在同一Store调用读取，网络/查询失败不会吞掉新增完成事实。
            last_revision = Some(revision);
            if let Some(previous) = previous_revision {
                let mut completed: Vec<_> = runs
                    .iter()
                    .filter(|r| {
                        r.state == crate::RunState::Completed
                            && r.revision > previous
                            && bindings.iter().any(|b| {
                                b.session_id == r.session_id && b.scope.namespace == "host-agent"
                            })
                    })
                    .collect();
                completed.sort_by_key(|r| (r.revision, r.run_id.as_str()));
                completion_returns(
                    &host,
                    &client,
                    completed.into_iter().map(|r| r.run_id.clone()).collect(),
                    &stop,
                )
                .await;
            }
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

// 完成汇报使用独立场景指令；不改变task.assigned，不让终态卡过滤掉回交。
async fn completion_returns(
    host: &Host,
    client: &Client,
    children: Vec<String>,
    stop: &CancellationToken,
) {
    if children.is_empty() {
        return;
    }
    let observed = chrono::Utc::now();
    let expiry = observed.timestamp_millis() + 120_000;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let caps = tokio::select! {
        _ = stop.cancelled() => return,
        result = tokio::time::timeout_at(deadline, client.capabilities()) => result,
    };
    let mut counts = std::collections::BTreeMap::new();
    for child in children {
        if stop.is_cancelled() {
            return;
        }
        let event = format!("{child}:completion-return");
        let command = uuid::Uuid::new_v4().simple().to_string();
        let (id, c) = (child, command.clone());
        // 取完能力后再核对实际父子状态、范围及原回执；清理/删除后的任务不能回交。
        let delegation = match host
            .inner
            .store
            .call(move |db| db.claim_office_completion_return(&id, &c, expiry))
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(error) => {
                eprintln!(
                    "engine-host completion return lookup failed kind={}",
                    error.kind
                );
                continue;
            }
        };
        let role = crate::agents::role(&delegation.role_id).expect("Store已验证可信子角色");
        let lead = crate::agents::role("lead").expect("固定主角色");
        let count = counts
            .entry(delegation.parent_scope.slot_id.clone())
            .or_insert(0);
        let allowed = *count < 32;
        *count += 1;
        let send = async {
            if !allowed {
                return Ok("dropped");
            }
            let caps = match &caps {
                Ok(Ok(caps)) => caps,
                _ => return Ok("failed"),
            };
            let actors = caps["scene"]["actors"].as_array();
            if !caps["scene"]["capabilities"]
                .as_array()
                .is_some_and(|v| v.iter().any(|c| c["id"] == "office.visit"))
                || !actors.is_some_and(|v| {
                    v.iter().any(|a| a["id"] == role.actor)
                        && v.iter().any(|a| a["id"] == lead.actor)
                })
            {
                return Ok("dropped");
            }
            let team = &delegation.parent_scope.slot_id;
            let (_, receipt) = client.post(json!({"teamId":team,"conversationId":delegation.parent_scope.conversation_id,
                "command":{"protocolVersion":"2.0","sceneId":team,"commandId":command,
                    "type":"activity.start","capability":"office.visit",
                    "participants":[{"entityId":role.actor,"role":"visitor"},{"entityId":lead.actor,"role":"host"}],
                    "params":{"stops":[{"hostId":lead.actor,"message":format!("已处理：{}。",super::dto::bounded(&delegation.title,48))}]},
                    "busyPolicy":"queue","expiresAt":chrono::DateTime::from_timestamp_millis(expiry).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)}})).await?;
            // 只记录实际送达页面；完整播放仍需浏览器runtime记录证明。
            Ok::<_, crate::HostError>(
                if receipt["forwarded"].as_u64().is_some_and(|n| n > 0)
                    || receipt["forwarded"].as_bool() == Some(true)
                {
                    "forwarded"
                } else {
                    "dropped"
                },
            )
        };
        let state = tokio::select! {
            _ = stop.cancelled() => "dropped",
            result = tokio::time::timeout_at(deadline, send) => match result {
                Ok(Ok(state)) => state,
                Ok(Err(error)) => {eprintln!("engine-host completion return send failed kind={}",error.kind);"failed"},
                Err(_) => "expired",
            }
        };
        if state != "forwarded" {
            eprintln!(
                "engine-host completion return event={} delivery={}",
                event, state
            );
        }
        let _ = host
            .inner
            .store
            .call(move |db| db.office_scene(&event, &command, expiry, state))
            .await;
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
    async fn wait_terminal_receipt(home: &std::path::Path, run: &str, want: &str) -> String {
        use rusqlite::OptionalExtension;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let receipt = {
                    let db = rusqlite::Connection::open(home.join("engine-host/host.db")).unwrap();
                    db.query_row(
                        "SELECT command_id,status FROM host_office_scene WHERE event_id=?1",
                        [format!("{run}:terminal")],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .optional()
                    .unwrap()
                };
                if let Some((command, _)) = receipt.filter(|(_, state)| state == want) {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
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
        let first_command = wait_terminal_receipt(&cfg.home, &first.run_id, "forwarded").await;
        assert_eq!(
            state
                .lock()
                .await
                .commands
                .iter()
                .filter(|c| c["command"]["commandId"] == first_command)
                .count(),
            1
        );
        state.lock().await.offline = true;
        let second = host
            .submit_run(&scope, &b.session_id, "offline run".into())
            .await
            .unwrap();
        host.wait_idle().await;
        let second_command = wait_terminal_receipt(&cfg.home, &second.run_id, "failed").await;
        assert_eq!(
            state
                .lock()
                .await
                .commands
                .iter()
                .filter(|c| c["command"]["commandId"] == second_command)
                .count(),
            1
        );
        let sent_count = state.lock().await.commands.len();
        state.lock().await.offline = false;
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(
            state.lock().await.commands.len(),
            sent_count,
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
    #[test]
    fn completion_returns_follow_real_children_once_without_replay() {
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "office::scene::tests::completion_return_fixture_child",
                "--nocapture"
            ])
            .env("OPENPAW_COMPLETION_SCENE_CHILD", "1")
            .env("PAWOFFICE_SCENE_TEST_KEY", "return-fixture")
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn completion_return_fixture_child() {
        if std::env::var("OPENPAW_COMPLETION_SCENE_CHILD").as_deref() != Ok("1") {
            return;
        }
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(completion_scenario());
    }

    async fn wait_return_receipt(home: &std::path::Path, child: &str, status: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let exists = {
                    let db=rusqlite::Connection::open(home.join("engine-host/host.db")).unwrap();
                    db.query_row("SELECT EXISTS(SELECT 1 FROM host_office_scene WHERE event_id=?1 AND status=?2)",rusqlite::params![format!("{child}:completion-return"),status],|r|r.get::<_,bool>(0)).unwrap()
                };
                if exists { return; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap_or_else(|_| {
            let db = rusqlite::Connection::open(home.join("engine-host/host.db")).unwrap();
            let runs: Vec<String> = db.prepare("SELECT data FROM host_runs").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            let receipts: Vec<(String,String)> = db.prepare("SELECT event_id,status FROM host_office_scene").unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().map(Result::unwrap).collect();
            panic!("真实子Run完成必须产生且仅产生一次完成汇报回执 child={child} want={status} runs={runs:?} receipts={receipts:?}");
        });
    }

    async fn completion_scenario() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use openpaw_core::LlmProvider;
        use openpaw_llm::{MockMode, MockProvider};
        use serde_json::Value;
        use std::sync::atomic::{AtomicUsize, Ordering};
        const ROLES: [&str; 7] = [
            "research", "organize", "draft", "review", "audit", "draft", "audit",
        ];
        struct Provider {
            parent: MockProvider,
            parent_calls: AtomicUsize,
            parent_release: tokio::sync::Semaphore,
            entered: [AtomicUsize; 7],
            release: [tokio::sync::Semaphore; 7],
        }
        #[async_trait::async_trait]
        impl LlmProvider for Provider {
            async fn complete(
                &self,
                r: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
                self.parent.complete(r).await
            }
            async fn stream(
                &self,
                r: openpaw_core::CompletionRequest,
            ) -> openpaw_core::Result<
                futures::stream::BoxStream<
                    'static,
                    openpaw_core::Result<openpaw_core::StreamEvent>,
                >,
            > {
                let index = r.messages.iter().flat_map(|m| &m.content).find_map(|b| {
                    let openpaw_core::ContentBlock::Text { text } = b else {
                        return None;
                    };
                    text.lines().find_map(|line| {
                        line.strip_prefix("要求：return-child-")
                            .and_then(|s| s.parse::<usize>().ok())
                    })
                });
                if let Some(i) = index {
                    self.entered[i].fetch_add(1, Ordering::SeqCst);
                    self.release[i].acquire().await.unwrap().forget();
                    if i == 6 {
                        return Err(openpaw_core::Error::AuthFailed {
                            status: 401,
                            message: "controlled failure".into(),
                        });
                    }
                    return MockProvider::fixed("独立执行结束，成果待核验")
                        .stream(r)
                        .await;
                }
                // 主模型在全部异步派发后等待；若直接回复终态，原Host会正确取消未完成子任务。
                if self.parent_calls.fetch_add(1, Ordering::SeqCst) % 8 == 7 {
                    self.parent_release.acquire().await.unwrap().forget();
                }
                self.parent.stream(r).await
            }
            fn capabilities(&self) -> openpaw_core::Capabilities {
                self.parent.capabilities()
            }
            fn default_model(&self) -> &str {
                self.parent.default_model()
            }
            fn name(&self) -> &str {
                "completion-return-controlled"
            }
        }
        #[derive(Default)]
        struct Fixture {
            offline: bool,
            commands: Vec<Value>,
        }
        async fn handle(
            State(s): State<Arc<tokio::sync::Mutex<Fixture>>>,
            Json(r): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            if r["type"] == "office.capabilities" {
                return (
                    StatusCode::OK,
                    Json(
                        json!({"code":200,"msg":"fixture","data":{"scene":{"protocolVersion":"2.0","actors":crate::agents::ROLES.iter().map(|r|json!({"id":r.actor})).collect::<Vec<_>>(),"capabilities":[{"id":"scene.say"},{"id":"office.visit"}]},"ui":{"whiteboardSchema":{}},"requests":{},"responses":{"success":{},"error":{}}}}),
                    ),
                );
            }
            let command = &r["command"];
            assert_eq!(r["teamId"], command["sceneId"]);
            assert_eq!(r["conversationId"], "completion-scene");
            if command["capability"] == "office.visit" {
                assert_eq!(
                    command["participants"][1]["entityId"],
                    crate::agents::role("lead").unwrap().actor
                );
                assert_eq!(command["participants"][0]["role"], "visitor");
                assert_eq!(command["participants"][1]["role"], "host");
                assert_eq!(command["busyPolicy"], "queue");
                let message = command["params"]["stops"][0]["message"].as_str().unwrap();
                assert!(message.starts_with("已处理：独立待办"), "{message}");
                assert!(message.ends_with('。'));
                assert!(!message.contains("分配人") && !message.contains("请核验"));
                let expiry =
                    chrono::DateTime::parse_from_rfc3339(command["expiresAt"].as_str().unwrap())
                        .unwrap()
                        .timestamp_millis();
                assert!((1..=120000).contains(&(expiry - chrono::Utc::now().timestamp_millis())));
            } else {
                assert_eq!(command["capability"], "scene.say");
            }
            let mut s = s.lock().await;
            let offline = s.offline;
            s.commands.push(r);
            if offline {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"code":409,"msg":"offline","data":{"reasonCode":"NO_RECEIVER"}})),
                )
            } else {
                (
                    StatusCode::OK,
                    Json(json!({"code":200,"msg":"forwarded","data":{"forwarded":1}})),
                )
            }
        }
        let state = Arc::new(tokio::sync::Mutex::new(Fixture::default()));
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
        let cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":[],"max_active_runs":6,"agents":{"enabled":true,"max_active_children":5}})).unwrap();
        let script=(0..2).flat_map(|_|ROLES.into_iter().enumerate().map(|(i,role)|MockMode::ToolUse{tool_name:"delegate_task".into(),input:json!({"role_id":role,"title":format!("独立待办{i}"),"instructions":format!("return-child-{i}"),"acceptance":"返回真实有限成果","wait_for_completion":false}),text:String::new()}).chain(std::iter::once(MockMode::Fixed("有限分工已发出".into())))).collect();
        let provider = Arc::new(Provider {
            parent: MockProvider::scripted(script),
            parent_calls: AtomicUsize::new(0),
            parent_release: tokio::sync::Semaphore::new(0),
            entered: std::array::from_fn(|_| AtomicUsize::new(0)),
            release: std::array::from_fn(|_| tokio::sync::Semaphore::new(0)),
        });
        let host = Host::with_provider(cfg.clone(), provider.clone())
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "completion-scene".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let office:super::super::OfficeConfig=serde_json::from_value(json!({"base_url":base,"api_key_env":"PAWOFFICE_SCENE_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"]})).unwrap();
        let stop = CancellationToken::new();
        let worker = tokio::spawn(run(
            host.clone(),
            Arc::new(Client::new(&office).unwrap()),
            stop.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let parent = host
            .submit_run(&scope, &binding.session_id, "分工并按真实完成汇报".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider.entered[..5]
                .iter()
                .any(|e| e.load(Ordering::SeqCst) == 0)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let id = parent.run_id.clone();
                if host
                    .inner
                    .store
                    .call(move |db| db.delegations(&id))
                    .await
                    .unwrap()
                    .len()
                    == 7
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let id = parent.run_id.clone();
        let ds = host
            .inner
            .store
            .call(move |db| db.delegations(&id))
            .await
            .unwrap();
        assert_eq!(ds.len(), 7);
        assert_eq!(
            provider.entered[5].load(Ordering::SeqCst),
            0,
            "同人第二项必须仍在FIFO队列"
        );
        for i in 0..6 {
            if i == 4 {
                state.lock().await.offline = true;
            }
            provider.release[i].add_permits(1);
            wait_return_receipt(
                &cfg.home,
                &ds[i].child_run,
                if i == 4 { "failed" } else { "forwarded" },
            )
            .await;
            let s = state.lock().await;
            let visits: Vec<_> = s
                .commands
                .iter()
                .filter(|c| c["command"]["capability"] == "office.visit")
                .collect();
            assert_eq!(visits.len(), i + 1);
            assert_eq!(
                visits[i]["command"]["participants"][0]["entityId"],
                crate::agents::role(ROLES[i]).unwrap().actor
            );
            drop(s);
            let child = ds[i].child_run.clone();
            assert!(host
                .inner
                .store
                .call(move |db| db.claim_office_completion_return(
                    &child,
                    "duplicate",
                    chrono::Utc::now().timestamp_millis() + 120000
                ))
                .await
                .unwrap()
                .is_none());
            if i == 4 {
                state.lock().await.offline = false;
            }
        }
        provider.release[6].add_permits(1);
        provider.parent_release.add_permits(1);
        host.wait_idle().await;
        let count = state
            .lock()
            .await
            .commands
            .iter()
            .filter(|c| c["command"]["capability"] == "office.visit")
            .count();
        assert_eq!(count, 6, "失败子任务不得伪报完成，离线失败不得补播");
        // 任意无关事实变化不能重复回交；进程观察器重建不能补播旧终态。
        host.open_session(ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "unrelated".into(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        stop.cancel();
        worker.await.unwrap();
        let stop = CancellationToken::new();
        let worker = tokio::spawn(run(
            host.clone(),
            Arc::new(Client::new(&office).unwrap()),
            stop.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            state
                .lock()
                .await
                .commands
                .iter()
                .filter(|c| c["command"]["capability"] == "office.visit")
                .count(),
            6
        );
        // 第二次真实协作整体取消，不得为已取消/尚在队列的同事生成成功汇报。
        let second = host
            .submit_run(&scope, &binding.session_id, "整体取消".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider.entered[..5]
                .iter()
                .any(|e| e.load(Ordering::SeqCst) < 2)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        host.cancel_run(&scope, &second.run_id).await.unwrap();
        host.wait_idle().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            state
                .lock()
                .await
                .commands
                .iter()
                .filter(|c| c["command"]["capability"] == "office.visit")
                .count(),
            6,
            "停止清理不能生成成功回交"
        );
        stop.cancel();
        worker.await.unwrap();
        host.shutdown().await.unwrap();
        server_stop.cancel();
        server.await.unwrap();
    }
}
