//! 职责：真实HTTP验证Office聊天、只读工具、历史和取消边界。
//! 输入输出：隔离Host与确定性Provider到协议及账本断言。
//! 依赖：原AgentLoop/ToolRegistry和真实loopback监听。
//! 禁止职责：不访问用户凭据、不注入浏览器答案或伪造Run。
use async_trait::async_trait;
use futures::StreamExt;
use openpaw_core::{Capabilities, CompletionRequest, CompletionResponse, LlmProvider, StreamEvent};
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

#[test]
fn office_http_product_contract() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "office_child_suite", "--nocapture"])
        .env("OPENPAW_OFFICE_TEST_CHILD", "1")
        .env("OPENPAW_WEB_TEST_ALLOW_LOOPBACK", "1")
        .env("PAWOFFICE_TEST_KEY", "isolated-office-test-key")
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn office_child_suite() {
    if std::env::var("OPENPAW_OFFICE_TEST_CHILD").as_deref() != Ok("1") {
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(suite());
}
fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("项目 workspace");
    std::fs::create_dir_all(&root).unwrap();
    serde_json::from_value(json!({"home":base.join("home"),"roots":{"project":root},"slots":{"office-1":"project","office-2":"project"},"provider":"mock","tools":["read_file","grep","glob","todo_list","think"],"office":{"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"]}})).unwrap()
}
fn request(conversation: &str, input: &str) -> Value {
    json!({"type":"send","turnId":uuid::Uuid::new_v4().simple().to_string(),"version":"1.0","teamId":"office-1","conversationId":conversation,"messages":[{"role":"user","content":input}]})
}
fn post(client: &reqwest::Client, url: &str, body: &Value) -> reqwest::RequestBuilder {
    client
        .post(format!("{url}/office/chat"))
        .header("origin", "http://127.0.0.1:8765")
        .json(body)
}
async fn events(response: reqwest::Response) -> Vec<Value> {
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/x-ndjson"));
    let body = response.text().await.unwrap();
    body.lines()
        .map(|line| {
            assert!(line.encode_utf16().count() <= 350000);
            serde_json::from_str(line).unwrap()
        })
        .collect()
}
async fn suite() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    std::fs::write(
        cfg.roots["project"].join("README.md"),
        "真实只读标记：ALPHA",
    )
    .unwrap();
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "read_file".into(),
            input: json!({"path":"README.md"}),
            text: "读取文件。".into(),
        },
        MockMode::Fixed("# 读取结果\n\n已读取 ALPHA。".into()),
        MockMode::Fixed("记得 ALPHA。".into()),
    ]));
    let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let client = reqwest::Client::new();
    let e = events(
        post(&client, &url, &request("first", "读取README.md"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(completed(e.last().unwrap()));
    let text: String = e.iter().filter_map(|e| e["text"].as_str()).collect();
    assert_eq!(text, "读取文件。\n\n# 读取结果\n\n已读取 ALPHA。");
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "first".into(),
    };
    let (snapshot, _) = host.snapshot_and_subscribe(&scope).await.unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.runs[0].tools[0].state, "completed");
    let history = host
        .query(
            &scope,
            QueryRequest {
                session_id: snapshot.runs[0].session_id.clone(),
                kind: QueryKind::ToolResults,
                offset: 0,
                limit: 50,
            },
        )
        .await
        .unwrap();
    assert!(serde_json::to_string(&history).unwrap().contains("ALPHA"));
    let page_request = json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"first","limit":1});
    let page: Value = post(&client, &url, &page_request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["data"]["turns"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"]["turns"][0]["assistant"], text);
    let resume_first = json!({"type":"resume","version":"1.0","teamId":"office-1","conversationId":"first","turnId":page["data"]["turns"][0]["id"]});
    assert_eq!(
        events(post(&client, &url, &resume_first).send().await.unwrap()).await[0]["text"],
        text
    );
    let original_messages = host
        .get_session(&scope, &snapshot.runs[0].session_id)
        .await
        .unwrap()
        .messages;
    let original_text: Vec<_> = original_messages
        .iter()
        .filter(|m| m.role == openpaw_core::Role::Assistant)
        .map(|m| m.text())
        .collect();
    assert_eq!(
        original_text,
        vec!["读取文件。", "# 读取结果\n\n已读取 ALPHA。"]
    );
    assert_eq!(page["data"]["turns"][0]["user"], "读取README.md");
    // 原turn重试先从持久关联返回终态快照，不再调用Provider/工具。
    let turn = page["data"]["turns"][0]["id"].clone();
    let mut duplicate = request("first", "读取README.md");
    duplicate["turnId"] = turn;
    let replay = events(post(&client, &url, &duplicate).send().await.unwrap()).await;
    assert_eq!(replay[0]["type"], "snapshot");
    assert_eq!(replay[0]["state"], "complete");
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        1
    );
    duplicate["messages"][0]["content"] = json!("不同输入");
    let rejection = events(post(&client, &url, &duplicate).send().await.unwrap()).await;
    assert_eq!(
        rejection,
        vec![json!({"type":"error","message":"本轮标识已用于其他内容，请重新同步会话后重试"})]
    );
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        1
    );
    let empty =
        json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"empty"});
    let empty_page: Value = client
        .post(format!("{url}/office/chat"))
        .json(&empty)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty_page["data"]["turns"], json!([]));
    let follow = json!({"type":"send","turnId":uuid::Uuid::new_v4().simple().to_string(),"version":"1.0","teamId":"office-1","conversationId":"first","messages":[{"role":"user","content":"读取README.md"},{"role":"assistant","content":text},{"role":"user","content":"记得什么？"}]});
    let follow_events = events(post(&client, &url, &follow).send().await.unwrap()).await;
    assert!(completed(follow_events.last().unwrap()));
    let mut wrong = follow.clone();
    wrong["messages"][1]["content"] = json!("篡改历史");
    wrong["turnId"] = json!(uuid::Uuid::new_v4().simple().to_string());
    let rejection = events(post(&client, &url, &wrong).send().await.unwrap()).await;
    assert_eq!(
        rejection,
        vec![json!({"type":"error","message":"会话历史已更新，请点击重新同步后重试"})]
    );
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        2
    );
    let missing = json!({"type":"resume","version":"1.0","teamId":"office-1","conversationId":"first","turnId":"missing-turn"});
    assert_eq!(
        events(post(&client, &url, &missing).send().await.unwrap()).await,
        vec![json!({"type":"error","message":"未找到这轮对话，请重新同步会话"})]
    );
    let mut invalid = request("bad", "hello");
    invalid["runId"] = json!("not-v1");
    assert_eq!(
        post(&client, &url, &invalid).send().await.unwrap().status(),
        400
    );
    let mut unknown = request("bad", "hello");
    unknown["teamId"] = json!("office-unknown");
    assert_eq!(
        post(&client, &url, &unknown).send().await.unwrap().status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{url}/office/chat"))
            .header("origin", "http://evil.invalid")
            .json(&request("bad", "hello"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let preflight = client
        .request(reqwest::Method::OPTIONS, format!("{url}/office/chat"))
        .header("origin", "http://127.0.0.1:8765")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), 204);
    assert_eq!(
        preflight.headers()["access-control-allow-origin"],
        "http://127.0.0.1:8765"
    );
    let mut huge = request("bad", &"😀".repeat(2001));
    assert_eq!(
        post(&client, &url, &huge).send().await.unwrap().status(),
        400
    );
    huge["messages"] = Value::Null;
    assert_eq!(
        post(&client, &url, &huge).send().await.unwrap().status(),
        400
    );
    server.shutdown().await.unwrap();
    let mut writes = cfg.clone();
    writes.tools.push("write_file".into());
    assert!(writes.validate().is_ok());
    let restarted = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("不应执行")))
        .await
        .unwrap();
    let server = HostServer::bind(restarted, 0).await.unwrap();
    let restarted_url = format!("http://{}", server.address());
    let restored: Value = post(&client, &restarted_url, &page_request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 第一页limit=1为追问；resume仍能按原稳定turn读取工具前后同一展示文本。
    assert_eq!(restored["data"]["turns"][0]["assistant"], "记得 ALPHA。");
    assert_eq!(
        events(
            post(&client, &restarted_url, &resume_first)
                .send()
                .await
                .unwrap()
        )
        .await[0]["text"],
        text
    );
    let missing_control = json!({"type":"cancel","version":"1.0","teamId":"office-1","conversationId":"first","turnId":"missing-turn"});
    assert_eq!(
        post(&client, &restarted_url, &missing_control)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.shutdown().await.unwrap();
    let mut cfg = config(&tmp.path().join("slow"));
    cfg.max_active_runs = 3;
    let slow = Host::with_provider(cfg, Arc::new(Slow)).await.unwrap();
    let server = HostServer::bind(slow.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let response = post(&client, &url, &request("cancel", "慢任务"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        events(
            post(&client, &url, &request("cancel", "重复提交"))
                .send()
                .await
                .unwrap()
        )
        .await,
        vec![json!({"type":"error","message":"当前会话仍有任务运行，请等待完成或先停止"})]
    );
    let same_office = post(&client, &url, &request("parallel", "同办公室不同会话"))
        .send()
        .await
        .unwrap();
    assert_eq!(same_office.status(), 200);
    let mut other = request("independent", "另一个办公室");
    other["teamId"] = json!("office-2");
    let independent = post(&client, &url, &other).send().await.unwrap();
    assert_eq!(independent.status(), 200);
    assert_eq!(
        events(
            post(&client, &url, &request("capacity", "容量已满"))
                .send()
                .await
                .unwrap()
        )
        .await,
        vec![json!({"type":"error","message":"当前运行容量已满，请稍后重试"})]
    );
    let capacity_scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "capacity".into(),
    };
    assert!(slow
        .snapshot_and_subscribe(&capacity_scope)
        .await
        .unwrap()
        .0
        .runs
        .is_empty());
    let mut other_stream = independent.bytes_stream();
    assert!(other_stream.next().await.is_some());
    let mut stream = response.bytes_stream();
    assert!(stream.next().await.is_some());
    drop(stream);
    let control =
        json!({"version":"1.0","type":"history","teamId":"office-1","conversationId":"cancel"});
    let page: Value = post(&client, &url, &control)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["data"]["turns"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"]["turns"][0]["state"], "pending");
    let turn = page["data"]["activeTurnId"].clone();
    let resume = json!({"version":"1.0","type":"resume","teamId":"office-1","conversationId":"cancel","turnId":turn});
    let mut resumed = post(&client, &url, &resume)
        .send()
        .await
        .unwrap()
        .bytes_stream();
    let first = resumed.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("snapshot"));
    drop(resumed);
    let cancel = json!({"version":"1.0","type":"cancel","teamId":"office-1","conversationId":"cancel","turnId":turn});
    let stopped: Value = post(&client, &url, &cancel)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stopped["data"]["stopped"], true);
    let again: Value = post(&client, &url, &cancel)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["data"]["stopped"], true);
    let s = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "cancel".into(),
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (snap, _) = slow.snapshot_and_subscribe(&s).await.unwrap();
            if snap
                .runs
                .first()
                .is_some_and(|r| r.state == RunState::Cancelled)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let other_scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-2".into(),
        conversation_id: "independent".into(),
    };
    assert!(!slow
        .snapshot_and_subscribe(&other_scope)
        .await
        .unwrap()
        .0
        .runs[0]
        .state
        .is_terminal());
    drop(other_stream);
    drop(same_office);
    server.shutdown().await.unwrap();
    boundary_cases(tmp.path()).await;
    persistent_turn_cases(tmp.path()).await;
    history_encoded_byte_budget_pages_without_losing_turns(tmp.path()).await;
    web_cases(tmp.path()).await;
    unread_http_cases(tmp.path()).await;
    checkpoint_write_failure_cancels_real_stream_without_success_and_deletion_cannot_revive_it()
        .await;
}
async fn unread_http_cases(base: &std::path::Path) {
    // 保持真实HTTP响应存活且不消费正文，另外的控制请求仍须能完成和恢复终态。
    // 本用例不将操作系统缓冲区中尚有空间等同于已强制触发TCP背压。
    for cancel in [false, true] {
        let cfg = config(&base.join(format!("unread-http-{cancel}")));
        let provider: Arc<dyn LlmProvider> = if cancel {
            Arc::new(Slow)
        } else {
            Arc::new(MockProvider::stream_text("可靠文本。".repeat(4000), 200))
        };
        let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
        let server = HostServer::bind(host.clone(), 0).await.unwrap();
        let url = format!("http://{}", server.address());
        let client = reqwest::Client::new();
        let input = request("unread", "不读取订阅正文");
        let response = post(&client, &url, &input).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let resume = json!({"version":"1.0","type":"resume","teamId":"office-1","conversationId":"unread","turnId":input["turnId"]});
        let second = post(&client, &url, &resume).send().await.unwrap();
        assert_eq!(second.status(), 200);
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "unread".into(),
        };
        let (before, _) = host.snapshot_and_subscribe(&scope).await.unwrap();
        assert_eq!(before.runs.len(), 1);
        if cancel {
            assert!(!before.runs[0].state.is_terminal());
            let mut stop = resume.clone();
            stop["type"] = json!("cancel");
            let result: Value = tokio::time::timeout(Duration::from_secs(3), async {
                post(&client, &url, &stop)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap()
            })
            .await
            .expect("未消费的HTTP订阅不得阻塞取消控制请求");
            assert_eq!(result["data"]["stopped"], true);
        }
        let expected = if cancel {
            RunState::Cancelled
        } else {
            RunState::Completed
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = host.get_run(&scope, &before.runs[0].run_id).await.unwrap();
                if run.state == expected {
                    if !cancel {
                        assert_eq!(run.text, "可靠文本。".repeat(4000));
                    }
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("终态持久化不得等待HTTP消费者读取正文");
        // 两个正文仍未读取时，新HTTP查询必须读到可靠终态。
        let history =
            json!({"version":"1.0","type":"history","teamId":"office-1","conversationId":"unread"});
        let page: Value = post(&client, &url, &history)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page["data"]["turns"].as_array().unwrap().len(), 1);
        assert_eq!(
            page["data"]["turns"][0]["state"],
            if cancel { "stopped" } else { "complete" }
        );
        drop(response);
        drop(second);
        server.shutdown().await.unwrap();
        drop(host);
        let reopened = Host::with_provider(cfg, Arc::new(MockProvider::fixed("不得重放")))
            .await
            .unwrap();
        assert_eq!(
            reopened
                .get_run(&scope, &before.runs[0].run_id)
                .await
                .unwrap()
                .state,
            expected
        );
        reopened.shutdown().await.unwrap();
    }
}
async fn boundary_cases(base: &std::path::Path) {
    let client = reqwest::Client::new();
    for (index, text) in ["".to_owned(), "x".repeat(50001), "\"\n😀".repeat(1000)]
        .into_iter()
        .enumerate()
    {
        let cfg = config(&base.join(format!("boundary-{index}")));
        let host = Host::with_provider(
            cfg,
            Arc::new(if text.is_empty() {
                MockProvider::fixed("")
            } else {
                MockProvider::stream_text(text.clone(), 17)
            }),
        )
        .await
        .unwrap();
        let server = HostServer::bind(host.clone(), 0).await.unwrap();
        let url = format!("http://{}", server.address());
        let result = events(
            post(&client, &url, &request("bounds", "测试"))
                .send()
                .await
                .unwrap(),
        )
        .await;
        if index < 2 {
            assert!(
                result.last().unwrap()["type"] == "error"
                    || result.last().unwrap()["state"] == "error"
            );
            assert!(!result.iter().any(|v| v["type"] == "done"));
        } else {
            let actual: String = result.iter().filter_map(|v| v["text"].as_str()).collect();
            assert_eq!(actual, text);
            assert!(completed(result.last().unwrap()));
        }
        server.shutdown().await.unwrap();
    }
    let cfg = config(&base.join("import"));
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("新回答")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let imported = json!({"type":"send","turnId":uuid::Uuid::new_v4().simple().to_string(),"version":"1.0","teamId":"office-1","conversationId":"import","messages":[{"role":"user","content":"已有用户"},{"role":"assistant","content":"已有回复"},{"role":"user","content":"新输入"}]});
    assert!(completed(
        events(post(&client, &url, &imported).send().await.unwrap())
            .await
            .last()
            .unwrap()
    ));
    let import_page: Value = post(
        &client,
        &url,
        &json!({"version":"1.0","type":"history","teamId":"office-1","conversationId":"import"}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(import_page["data"]["turns"].as_array().unwrap().len(), 2);
    assert_eq!(import_page["data"]["turns"][0]["user"], "已有用户");
    assert_eq!(import_page["data"]["turns"][0]["assistant"], "已有回复");
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "import".into(),
    };
    let binding = host.open_session(scope.clone()).await.unwrap();
    let messages = host
        .get_session(&scope, &binding.session_id)
        .await
        .unwrap()
        .messages;
    assert_eq!(
        messages.iter().filter(|m| m.text() == "已有用户").count(),
        1
    );
    server.shutdown().await.unwrap();
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("重启追问")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let suffix = json!({"type":"send","turnId":uuid::Uuid::new_v4().simple().to_string(),"version":"1.0","teamId":"office-1","conversationId":"import","messages":[{"role":"user","content":"新输入"},{"role":"assistant","content":"新回答"},{"role":"user","content":"同文本也可以再次提问"}]});
    assert!(completed(
        events(post(&client, &url, &suffix).send().await.unwrap())
            .await
            .last()
            .unwrap()
    ));
    let oversized = client
        .post(format!("{url}/office/chat"))
        .header("origin", "http://127.0.0.1:8765")
        .header("content-type", "application/json")
        .body(vec![b' '; 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    // 存储故障在接受边界必须拒绝，不能启动推理后伪造成功。
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_new_fact BEFORE INSERT ON host_events BEGIN SELECT RAISE(ABORT,'injected-disk-failure'); END;").unwrap();
    assert_eq!(
        post(&client, &url, &request("storage-fault", "不能虚假成功"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    db.execute_batch("DROP TRIGGER fail_new_fact;").unwrap();
    drop(db);
    server.shutdown().await.unwrap();
}
struct Slow;
#[async_trait]
impl LlmProvider for Slow {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        MockProvider::fixed("慢任务完成").complete(req).await
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>>
    {
        tokio::time::sleep(Duration::from_secs(30)).await;
        MockProvider::fixed("慢任务完成").stream(req).await
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn name(&self) -> &str {
        "slow"
    }
    fn default_model(&self) -> &str {
        "slow-test"
    }
}

fn completed(event: &Value) -> bool {
    event["type"] == "done" || event["type"] == "snapshot" && event["state"] == "complete"
}

async fn persistent_turn_cases(base: &std::path::Path) {
    let cfg = config(&base.join("persistent-turns"));
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("稳定历史回答")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://{}", server.address());
    let first = request("pages", "第一页输入");
    let (a, b) = tokio::join!(
        post(&client, &url, &first).send(),
        post(&client, &url, &first).send()
    );
    assert!(completed(events(a.unwrap()).await.last().unwrap()));
    assert!(completed(events(b.unwrap()).await.last().unwrap()));
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "pages".into(),
    };
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        1
    );
    for input in ["第二页输入", "第三页输入"] {
        assert!(completed(
            events(
                post(&client, &url, &request("pages", input))
                    .send()
                    .await
                    .unwrap()
            )
            .await
            .last()
            .unwrap()
        ));
    }
    let page_request = json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"pages","limit":1});
    let page: Value = post(&client, &url, &page_request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["data"]["turns"][0]["user"], "第三页输入");
    assert_eq!(page["data"]["hasMore"], true);
    let mut older = page_request.clone();
    older["cursor"] = page["data"]["nextCursor"].clone();
    assert!(completed(
        events(
            post(&client, &url, &request("pages", "第四页输入"))
                .send()
                .await
                .unwrap()
        )
        .await
        .last()
        .unwrap()
    ));
    let prior: Value = post(&client, &url, &older)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prior["data"]["turns"][0]["user"], "第二页输入");
    older["conversationId"] = json!("other-scope");
    assert_eq!(
        post(&client, &url, &older).send().await.unwrap().status(),
        400
    );
    server.shutdown().await.unwrap();
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("不得重复执行")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let replay = events(post(&client, &url, &first).send().await.unwrap()).await;
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0]["state"], "complete");
    assert_eq!(replay[0]["text"], "稳定历史回答");
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        4
    );
    // 模拟升级前未保存turnId的原Run；历史与resume使用稳定legacy身份，不启动Provider。
    let old_scope = ClientScope {
        conversation_id: "old-session".into(),
        ..scope.clone()
    };
    let binding = host.open_session(old_scope.clone()).await.unwrap();
    let old = host
        .submit_run(&old_scope, &binding.session_id, "旧版用户输入".into())
        .await
        .unwrap();
    loop {
        if host
            .get_run(&old_scope, &old.run_id)
            .await
            .unwrap()
            .state
            .is_terminal()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let history = json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"old-session"});
    let old_page: Value = post(&client, &url, &history)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        old_page["data"]["turns"][0]["id"],
        format!("legacy:{}", old.run_id)
    );
    let resume = json!({"type":"resume","version":"1.0","teamId":"office-1","conversationId":"old-session","turnId":format!("legacy:{}",old.run_id)});
    assert_eq!(
        events(post(&client, &url, &resume).send().await.unwrap()).await[0]["state"],
        "complete"
    );
    server.shutdown().await.unwrap();
}

async fn history_encoded_byte_budget_pages_without_losing_turns(base: &std::path::Path) {
    let cfg = config(&base.join("history-byte-budget"));
    // 控制字符序列在JSON中占六倍字节，确保触发传输预算而非仅验证轮次数。
    let answer = "\u{0001}".repeat(50_000);
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed(answer.clone())))
        .await
        .unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "byte-budget".into(),
    };
    let binding = host.open_session(scope.clone()).await.unwrap();
    let mut expected = std::collections::BTreeSet::new();
    for i in 0..50 {
        let run = host
            .submit_run(&scope, &binding.session_id, format!("真实轮次{i:02}"))
            .await
            .unwrap();
        host.wait_idle().await;
        let run = host.get_run(&scope, &run.run_id).await.unwrap();
        assert_eq!(run.state, RunState::Completed);
        assert_eq!(run.text, answer);
        expected.insert(format!("legacy:{}", run.run_id));
    }
    host.shutdown().await.unwrap();
    let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("查询不应调用")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://{}", server.address());
    let mut request = json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"byte-budget","limit":50});
    let mut seen = std::collections::BTreeSet::new();
    let mut cursors = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        let response = post(&client, &url, &request).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.unwrap();
        assert!(bytes.len() <= 8 * 1024 * 1024);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let turns = body["data"]["turns"].as_array().unwrap();
        assert!(!turns.is_empty());
        if pages == 0 {
            assert!(turns.len() < 50, "确实触发字节裁剪，不能仅测limit分页");
        }
        for turn in turns {
            assert_eq!(turn["assistant"], answer);
            assert_eq!(turn["state"], "complete");
            assert!(
                seen.insert(turn["id"].as_str().unwrap().to_owned()),
                "翻页不得重复轮次"
            );
        }
        pages += 1;
        if body["data"]["hasMore"] == false {
            assert_eq!(body["data"]["nextCursor"], "");
            break;
        }
        let cursor = body["data"]["nextCursor"].as_str().unwrap().to_owned();
        assert!(cursors.insert(cursor.clone()), "有后页时游标必须前进");
        assert!(pages <= 50);
        request["cursor"] = json!(cursor);
    }
    assert_eq!(seen, expected, "重启后字节预算分页保留全部50轮");
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        50
    );
    server.shutdown().await.unwrap();
    eprintln!("V08: 50个真实完成Run重启后按8MiB编码字节预算分为{pages}页，内容/稳定ID完整且无重复，无新增模型Run");
}

async fn web_cases(base: &std::path::Path) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web = format!("http://{}", listener.local_addr().unwrap());
    let (ready, started) = tokio::sync::oneshot::channel();
    let fixture = tokio::spawn(async move {
        for slow in [false, true] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 8192];
            let _ = stream.read(&mut buf).await.unwrap();
            if !slow {
                let body = "<h1>WEB-ACTUAL-MARKER</h1>";
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            } else {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await
                    .unwrap();
                let _ = ready.send(());
                let closed = stream.read(&mut buf).await;
                assert!(
                    matches!(closed, Ok(0))
                        || closed.as_ref().err().is_some_and(|e| matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        )),
                    "{closed:?}"
                );
                break;
            }
        }
    });
    let tmp = base.join("web-cases");
    let mut cfg = config(&tmp);
    cfg.tools.extend(["web_search".into(), "web_fetch".into()]);
    cfg.validate().unwrap();
    let mut invalid = cfg.clone();
    invalid.tools.push("notebook_edit".into());
    assert!(invalid.validate().is_err());
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "web_fetch".into(),
            input: json!({"url":format!("{web}/fresh"),"fresh":true}),
            text: "读取真实网页。".into(),
        },
        MockMode::Fixed("已取得 WEB-ACTUAL-MARKER。".into()),
        MockMode::ToolUse {
            tool_name: "web_fetch".into(),
            input: json!({"url":format!("{web}/slow"),"fresh":true}),
            text: "等待网页正文。".into(),
        },
        MockMode::Fixed("另一会话仍可工作。".into()),
    ]));
    let host = Host::with_provider(cfg, provider).await.unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let client = reqwest::Client::new();
    let req = request("web-result", "读取公开网页");
    let e = events(post(&client, &url, &req).send().await.unwrap()).await;
    assert!(completed(e.last().unwrap()));
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "web-result".into(),
    };
    let snapshot = host.snapshot_and_subscribe(&scope).await.unwrap().0;
    let run = &snapshot.runs[0];
    assert_eq!(run.tools[0].tool_name, "web_fetch");
    assert_eq!(run.tools[0].state, "completed");
    let query = host
        .query(
            &scope,
            QueryRequest {
                session_id: run.session_id.clone(),
                kind: QueryKind::ToolResults,
                offset: 0,
                limit: 5,
            },
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&query).unwrap();
    assert!(text.contains("WEB-ACTUAL-MARKER") && text.contains("retrieved_at_unix_ms:"));
    let slow = request("web-slow", "取消网页等待");
    let response = post(&client, &url, &slow).send().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    let cancel = json!({"type":"cancel","version":"1.0","teamId":"office-1","conversationId":"web-slow","turnId":slow["turnId"]});
    let cancelled =
        tokio::time::timeout(Duration::from_secs(3), post(&client, &url, &cancel).send())
            .await
            .unwrap()
            .unwrap();
    assert!(cancelled.status().is_success());
    let _ = events(response).await;
    let scope = ClientScope {
        conversation_id: "web-slow".into(),
        ..scope
    };
    assert_eq!(
        host.snapshot_and_subscribe(&scope).await.unwrap().0.runs[0].state,
        RunState::Cancelled
    );
    let next = events(
        post(&client, &url, &request("web-after", "继续工作"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(completed(next.last().unwrap()));
    tokio::time::timeout(Duration::from_secs(3), fixture)
        .await
        .unwrap()
        .unwrap();
    server.shutdown().await.unwrap();
}

struct PartialCheckpoint;
#[async_trait]
impl LlmProvider for PartialCheckpoint {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        MockProvider::fixed("不应走非流入口").complete(req).await
    }
    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> openpaw_core::Result<futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>>
    {
        Ok(futures::stream::iter([
            Ok(StreamEvent::MessageStart),
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "未完成的真实流文本".into(),
            }),
        ])
        .chain(futures::stream::pending())
        .boxed())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn name(&self) -> &str {
        "partial-checkpoint"
    }
    fn default_model(&self) -> &str {
        "partial-checkpoint"
    }
}
async fn checkpoint_write_failure_cancels_real_stream_without_success_and_deletion_cannot_revive_it(
) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let host = Host::with_provider(cfg.clone(), Arc::new(PartialCheckpoint))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let client = reqwest::Client::new();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_display_checkpoint BEFORE UPDATE ON host_runs WHEN json_extract(NEW.data,'$.text') != '' BEGIN SELECT RAISE(ABORT,'injected-checkpoint-write-failure'); END;").unwrap();
    let response = post(
        &client,
        &url,
        &request("checkpoint-failure", "测试真实流检查点故障"),
    )
    .send()
    .await
    .unwrap();
    let body = tokio::time::timeout(Duration::from_secs(6), response.text())
        .await
        .unwrap()
        .unwrap();
    assert!(!body
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["type"] == "done"));
    assert!(body.contains("error"));
    host.wait_idle().await;
    let data: String = db
        .query_row("SELECT data FROM host_runs", [], |r| r.get(0))
        .unwrap();
    let raw: RunView = serde_json::from_str(&data).unwrap();
    assert_eq!(raw.state, RunState::Failed);
    assert_eq!(raw.error.unwrap().kind, "storage_unavailable");
    db.execute_batch("DROP TRIGGER fail_display_checkpoint;")
        .unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: "checkpoint-failure".into(),
    };
    host.delete_session(&scope).await.unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM host_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    server.shutdown().await.unwrap();
    let host = Host::start(cfg).await.unwrap();
    assert_eq!(
        host.open_session(scope).await.unwrap_err().kind,
        "session_deleted"
    );
    host.shutdown().await.unwrap();
}
