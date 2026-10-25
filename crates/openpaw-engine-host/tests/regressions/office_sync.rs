//! 职责：验证旧Host元数据事务迁移及未知版本拒绝。
//! 输入输出：隔离v1数据库到原Host恢复结果。
//! 依赖：真实SQLite和Host，不依赖用户环境。
//! 禁止职责：不把模拟Office回执写作浏览器验收。
use openpaw_engine_host::*;
use serde_json::json;
fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    serde_json::from_value(json!({"home":base.join("home"),"roots":{"root":root},"slots":{"office-1":"root"},"provider":"echo"})).unwrap()
}
#[tokio::test]
async fn v1_metadata_upgrades_atomically_and_future_schema_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    let path = cfg.home.join("engine-host/host.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE host_office_projection; DROP TABLE host_context_import; DROP TABLE host_office_calls; DROP TABLE host_requests; DROP TABLE host_session_deletions; DROP TABLE host_office_decisions; DROP TABLE host_office_actions; DROP TABLE host_office_scene; PRAGMA user_version=1;").unwrap();
    drop(db);
    let host = Host::start(cfg.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        4
    );
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(Host::start(cfg).await.is_err());
}

#[tokio::test]
async fn real_v3_without_new_tables_upgrades_without_rewriting_existing_history() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "v3-existing".into(),
    };
    let binding = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope, &binding.session_id, "existing v3 history".into())
        .await
        .unwrap();
    host.wait_idle().await;
    host.shutdown().await.unwrap();
    let path = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", binding.session_id));
    let before = std::fs::read(&path).unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.execute_batch("DROP TABLE host_session_deletions; DROP TABLE host_office_decisions; DROP TABLE host_office_actions; DROP TABLE host_office_scene; PRAGMA user_version=3;").unwrap();
    drop(db);
    let host = Host::start(cfg.clone()).await.unwrap();
    assert_eq!(
        host.get_run(&scope, &run.run_id).await.unwrap().state,
        RunState::Completed
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    host.delete_session(&scope).await.unwrap();
    assert!(!path.exists());
    host.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    assert_eq!(
        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        4
    );
}

#[test]
fn sync_retry_cache_conflict_and_restart() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sync_fixture_child", "--nocapture"])
        .env("OPENPAW_OFFICE_SYNC_CHILD", "1")
        .env("PAWOFFICE_TEST_KEY", "fixture-key")
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn sync_fixture_child() {
    if std::env::var("OPENPAW_OFFICE_SYNC_CHILD").as_deref() != Ok("1") {
        return;
    }
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(sync_scenario());
}
#[derive(Default)]
struct Fixture {
    initialized: bool,
    mode: u8,
    requests: Vec<serde_json::Value>,
    snapshot: Option<serde_json::Value>,
    drop_next_snapshot_reply: bool,
    dropped_replies: usize,
    snapshot_bytes: Vec<Vec<u8>>,
}
async fn sync_scenario() {
    use axum::response::IntoResponse;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};
    let fixture = Arc::new(Mutex::new(Fixture::default()));
    async fn handle(
        State(shared): State<Arc<Mutex<Fixture>>>,
        headers: HeaderMap,
        bytes: axum::body::Bytes,
    ) -> axum::response::Response {
        let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(headers["authorization"], "Bearer fixture-key");
        if request["type"] == "ui.next" {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            return (
                axum::http::StatusCode::OK,
                Json(json!({"code":200,"msg":"fixture","data":{"actions":[]}})),
            )
                .into_response();
        }
        let mut s = shared.lock().unwrap();
        s.requests.push(request.clone());
        let mut code = 200;
        let ui = json!({"slots":[],"actions":[],"actionSchema":{},"presentationSchema":{},"whiteboardSchema":{}});
        let actors:Vec<_>=["office-lead","file-agent","data-agent"].iter().map(|id|json!({"id":id,"name":id,"templateId":id,"position":{"x":1,"y":1},"presentation":{"status":"idle","title":"","sourceRevision":0}})).collect();
        let directory = |s: &Fixture| json!({"offices":if s.initialized{vec![json!({"id":"office-1","name":"office-1","seq":s.snapshot.as_ref().unwrap()["seq"],"runtimeReady":false,"taskCount":0})]}else{vec![]},"actors":[{"id":"office-lead","name":"lead"}],"supportedMessages":["offices.initialize","office.snapshot","office.read","office.capabilities"],"sceneCommandProtocol":"2.0","ui":ui});
        let data = match request["type"].as_str().unwrap() {
            "office.capabilities" => {
                json!({"scene":{"protocolVersion":"2.0","sceneId":"office-1","command":{},"batch":{},"actors":actors,"objects":[],"capabilities":[]},"ui":ui,"requests":{},"responses":{"success":{},"error":{}}})
            }
            "office.read" => directory(&s),
            "offices.initialize" => {
                s.initialized = true;
                if let Some(first) = request["offices"].as_array().and_then(|a| a.first()) {
                    s.snapshot = Some(first.clone());
                }
                code = 202;
                directory(&s)
            }
            "office.snapshot" => {
                s.snapshot_bytes.push(bytes.to_vec());
                if s.drop_next_snapshot_reply {
                    s.drop_next_snapshot_reply = false;
                    s.dropped_replies += 1;
                    s.snapshot = Some(request["snapshot"].clone());
                    // 请求已应用后真实中断响应体，不能假定失败等同于服务端回滚。
                    let body = axum::body::Body::from_stream(futures::stream::once(async {
                        Err::<axum::body::Bytes, _>(std::io::Error::other(
                            "controlled lost receipt after apply",
                        ))
                    }));
                    return axum::response::Response::new(body);
                }
                if s.mode == 2 {
                    code = 409;
                    json!({"reasonCode":"SEQ_CONFLICT","details":{},"issues":[]})
                } else {
                    s.snapshot = Some(request["snapshot"].clone());
                    code = if s.mode == 0 { 202 } else { 200 };
                    let seq = request["snapshot"]["seq"].as_u64().unwrap();
                    json!({"teamId":request["teamId"],"seq":if s.mode==4{seq-1}else{seq},"status":if s.mode==0{"cached"}else{"applied"}})
                }
            }
            "ui.next" => json!({"actions":[]}),
            _ => panic!("unexpected operation"),
        };
        (
            axum::http::StatusCode::from_u16(code).unwrap(),
            Json(json!({"code":code,"msg":"fixture","data":data})),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    let signal = token.clone();
    let app = Router::new()
        .route("/api/pawoffice/messages", post(handle))
        .with_state(fixture.clone());
    let serving = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.office = Some(OfficeConfig {
        base_url: format!("http://{address}"),
        api_key_env: "PAWOFFICE_TEST_KEY".into(),
        allowed_origins: vec!["http://127.0.0.1:8765".into()],
        names: Default::default(),
        creation_slots: vec![],
    });
    let host = Host::start(cfg.clone()).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    async fn wait(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !ready() {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
    }
    wait(|| {
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .count()
            >= 2
    })
    .await;
    let original = fixture.lock().unwrap().snapshot.clone().unwrap();
    {
        let s = fixture.lock().unwrap();
        let sent: Vec<_> = s
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .collect();
        assert_eq!(sent[0]["snapshot"], sent[1]["snapshot"]);
    }
    let path = cfg.home.join("engine-host/host.db");
    let state = || {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.query_row(
            "SELECT sync_state FROM host_office_projection WHERE team='office-1'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
    };
    assert_eq!(state(), "cached");
    fixture.lock().unwrap().drop_next_snapshot_reply = true;
    wait(|| fixture.lock().unwrap().dropped_replies == 1).await;
    assert_ne!(state(), "applied", "确认丢失不能被视为页面应用成功");
    wait(|| {
        let s = fixture.lock().unwrap();
        s.snapshot_bytes.len() >= 4
    })
    .await;
    {
        let s = fixture.lock().unwrap();
        assert!(
            s.snapshot_bytes.windows(2).all(|pair| pair[0] == pair[1]),
            "丢失回执后同seq必须重发完全相同字节，不能重生成时间或内容"
        );
        assert_eq!(s.snapshot.as_ref().unwrap(), &original);
    }
    fixture.lock().unwrap().mode = 1;
    wait(|| state() == "applied").await;
    server.shutdown().await.unwrap();
    fixture.lock().unwrap().initialized = false;
    let count = fixture
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r["type"] == "offices.initialize")
        .count();
    let host = Host::start(cfg.clone()).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    wait(|| {
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "offices.initialize")
            .count()
            > count
    })
    .await;
    assert_eq!(
        fixture.lock().unwrap().snapshot.as_ref().unwrap(),
        &original
    );
    fixture.lock().unwrap().mode = 2;
    wait(|| state() == "halted").await;
    let count = fixture
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r["type"] == "office.snapshot")
        .count();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .count(),
        count
    );
    server.shutdown().await.unwrap();
    assert_eq!(
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .count(),
        count
    );
    let host = Host::start(cfg).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    wait(|| {
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .count()
            > count
    })
    .await;
    assert_eq!(state(), "halted", "重启后应重试旧halted并重新确认外部冲突");
    let retried = fixture
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r["type"] == "office.snapshot")
        .count();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(
        fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["type"] == "office.snapshot")
            .count(),
        retried,
        "确认外部seq冲突后仍应保持本进程锁存"
    );
    server.shutdown().await.unwrap();
    // 独立数据目录验证旧seq回执，保留前面的真实409锁存场景。
    *fixture.lock().unwrap() = Fixture {
        mode: 4,
        ..Default::default()
    };
    let another = tempfile::tempdir().unwrap();
    let mut stale_cfg = config(another.path());
    stale_cfg.office = Some(OfficeConfig {
        base_url: format!("http://{address}"),
        api_key_env: "PAWOFFICE_TEST_KEY".into(),
        allowed_origins: vec!["http://127.0.0.1:8765".into()],
        names: Default::default(),
        creation_slots: vec![],
    });
    let stale_db = stale_cfg.home.join("engine-host/host.db");
    let host = Host::start(stale_cfg).await.unwrap();
    let server = HostServer::bind(host, 0).await.unwrap();
    wait(|| {
        let db = rusqlite::Connection::open(&stale_db).unwrap();
        db.query_row(
            "SELECT sync_state FROM host_office_projection WHERE team='office-1'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
            == "halted"
    })
    .await;
    server.shutdown().await.unwrap();
    token.cancel();
    serving.await.unwrap();
}

#[tokio::test]
async fn schema_migration_failure_rolls_back_without_erasing_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    let path = cfg.home.join("engine-host/host.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE host_office_projection; DROP TABLE host_office_calls; DROP TABLE host_context_import; CREATE VIEW host_office_projection AS SELECT 1 AS wrong_column; PRAGMA user_version=1;").unwrap();
    drop(db);
    assert!(Host::start(cfg).await.is_err());
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        1
    );
    let tables: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='host_context_import'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}
