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
    json!({"version":"1.0","teamId":"office-1","conversationId":conversation,"messages":[{"role":"user","content":input}]})
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
            assert!(line.encode_utf16().count() <= 32000);
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
        MockMode::Fixed("已读取 ALPHA。".into()),
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
    assert_eq!(e.last().unwrap(), &json!({"type":"done"}));
    let text: String = e.iter().filter_map(|e| e["text"].as_str()).collect();
    assert!(text.contains("ALPHA"));
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
    let follow = json!({"version":"1.0","teamId":"office-1","conversationId":"first","messages":[{"role":"user","content":"读取README.md"},{"role":"assistant","content":text},{"role":"user","content":"记得什么？"}]});
    let follow_events = events(post(&client, &url, &follow).send().await.unwrap()).await;
    assert_eq!(follow_events.last().unwrap()["type"], "done");
    let mut wrong = follow.clone();
    wrong["messages"][1]["content"] = json!("篡改历史");
    assert_eq!(
        post(&client, &url, &wrong).send().await.unwrap().status(),
        409
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
    assert!(writes.validate().is_err());
    let cfg = config(&tmp.path().join("slow"));
    let slow = Host::with_provider(cfg, Arc::new(Slow)).await.unwrap();
    let server = HostServer::bind(slow.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let response = post(&client, &url, &request("cancel", "慢任务"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        post(&client, &url, &request("duplicate", "重复提交"))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let mut other = request("independent", "另一个办公室");
    other["teamId"] = json!("office-2");
    let independent = post(&client, &url, &other).send().await.unwrap();
    assert_eq!(independent.status(), 200);
    let mut other_stream = independent.bytes_stream();
    assert!(other_stream.next().await.is_some());
    let mut stream = response.bytes_stream();
    assert!(stream.next().await.is_some());
    drop(stream);
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
    server.shutdown().await.unwrap();
    boundary_cases(tmp.path()).await;
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
            assert_eq!(result.last().unwrap()["type"], "error");
            assert!(!result.iter().any(|v| v["type"] == "done"));
        } else {
            let actual: String = result.iter().filter_map(|v| v["text"].as_str()).collect();
            assert_eq!(actual, text);
            assert_eq!(result.last().unwrap()["type"], "done");
        }
        server.shutdown().await.unwrap();
    }
    let cfg = config(&base.join("import"));
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("新回答")))
        .await
        .unwrap();
    let server = HostServer::bind(host.clone(), 0).await.unwrap();
    let url = format!("http://{}", server.address());
    let imported = json!({"version":"1.0","teamId":"office-1","conversationId":"import","messages":[{"role":"user","content":"已有用户"},{"role":"assistant","content":"已有回复"},{"role":"user","content":"新输入"}]});
    assert_eq!(
        events(post(&client, &url, &imported).send().await.unwrap())
            .await
            .last()
            .unwrap()["type"],
        "done"
    );
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
    let suffix = json!({"version":"1.0","teamId":"office-1","conversationId":"import","messages":[{"role":"user","content":"新输入"},{"role":"assistant","content":"新回答"},{"role":"user","content":"同文本也可以再次提问"}]});
    assert_eq!(
        events(post(&client, &url, &suffix).send().await.unwrap())
            .await
            .last()
            .unwrap()["type"],
        "done"
    );
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
