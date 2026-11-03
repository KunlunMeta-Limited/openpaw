//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    HostConfig {
        agents: None,
        office: None,
        home: base.join("home"),
        roots: [("project".into(), root)].into(),
        slots: [("office-1".into(), "project".into())].into(),
        max_active_runs: 4,
        provider: "mock".into(),
        model: None,
        base_url: None,
        api_key_env: None,
        basic_username_env: None,
        basic_password_env: None,
        tools: vec!["write_file".into(), "sleep".into()],
        run_timeout_secs: 60,
        stream_idle_secs: 10,
        tool_timeout_secs: 60,
        approval_timeout_secs: 30,
    }
}
fn scope() -> ClientScope {
    ClientScope {
        namespace: "recovery".into(),
        slot_id: "office-1".into(),
        conversation_id: "conversation".into(),
    }
}
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(base: &std::path::Path, mode: &str) -> (Child, String) {
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_fixture", "--nocapture"])
            .env("OPENPAW_T3_FIXTURE", mode)
            .env("OPENPAW_T3_FIXTURE_BASE", base)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(pos) = line.find("READY ") {
                let _ = tx.send(line[pos..].to_string());
                return;
            }
        }
    });
    let line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("fixture should reach real persisted boundary");
    (child, line)
}
#[test]
fn child_fixture() {
    let Ok(mode) = std::env::var("OPENPAW_T3_FIXTURE") else {
        return;
    };
    let base = std::path::PathBuf::from(std::env::var_os("OPENPAW_T3_FIXTURE_BASE").unwrap());
    if mode.contains("-shell-") {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(host_shell_fixture(
                &base,
                mode.ends_with("cancel"),
                mode.starts_with("team-"),
            ));
        return;
    }
    if mode == "lock-exclusive" || mode == "lock-shared" {
        let cfg = config(&base);
        let meta = cfg.roots["project"].join(".openpaw");
        std::fs::create_dir_all(&meta).unwrap();
        let _guard = if mode == "lock-exclusive" {
            openpaw_sandbox::file_lock::try_lock_exclusive(&meta.join("engine-host-resource.lock"))
                .unwrap()
        } else {
            openpaw_sandbox::file_lock::try_lock_shared(&meta.join("engine-host-resource.lock"))
                .unwrap()
        };
        println!("READY resource");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut cfg = config(&base);
        let mut scope = scope();
        if mode == "text" {
            cfg.tools.clear();
            scope.namespace="office".into();
            cfg.office=Some(serde_json::from_value(json!({"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_CHECKPOINT_KEY","allowed_origins":["http://127.0.0.1:8765"]})).unwrap());
        }
        if ["accepted", "text", "completed"].contains(&mode.as_str()) {
            let provider: Arc<dyn openpaw_core::LlmProvider> = if mode == "completed" {
                Arc::new(MockProvider::fixed("已可靠提交的终态文本"))
            } else {
                Arc::new(CrashBoundaryProvider { emit_text: mode == "text", requests: Default::default() })
            };
            let host = Host::with_provider(cfg, provider).await.unwrap();
            let binding = host.open_session(scope.clone()).await.unwrap();
            let run = host.submit_run(&scope, &binding.session_id, "边界输入已保存".into()).await.unwrap();
            loop {
                let current = host.get_run(&scope, &run.run_id).await.unwrap();
                let text_committed = if mode == "text" {
                    // 独立SQLite读取屏障：live可见不能证明展示尾部已经提交。
                    let db = rusqlite::Connection::open(config(&base).home.join("engine-host/host.db")).unwrap();
                    let data: String = db.query_row("SELECT data FROM host_runs WHERE id=?1",[&run.run_id],|r|r.get(0)).unwrap();
                    let persisted: RunView = serde_json::from_str(&data).unwrap();
                    persisted.text == "硬停前已落盘的部分文本"
                } else { false };
                if mode == "accepted" || mode == "text" && current.text == "硬停前已落盘的部分文本" && text_committed || mode == "completed" && current.state == RunState::Completed {
                    let session = host.get_session(&scope, &binding.session_id).await.unwrap();
                    assert!(session.messages.iter().any(|message| message.text() == "边界输入已保存"));
                    println!("READY {} {}", run.run_id, binding.session_id);
                    std::io::stdout().flush().unwrap();
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let team = mode.starts_with("team-");
        if team {
            scope.namespace = "office".into();
            cfg.agents = Some(AgentsConfig {enabled:true,tools:[("draft".into(),cfg.tools.clone())].into(),..Default::default()});
        }
        let mut responses = vec![
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"report.txt","content":"once"}),
                text: "writing".into(),
            },
            MockMode::ToolUse {
                tool_name: "sleep".into(),
                input: json!({"duration_ms":50_000}),
                text: "waiting".into(),
            },
            MockMode::Fixed("done".into()),
        ];
        if team {responses.insert(0,MockMode::ToolUse {tool_name:"delegate_task".into(),input:json!({"role_id":"draft","title":"保存真实验收报告","instructions":"写report.txt并等待下一步","acceptance":"记录真实写入结果"}),text:"分配任务".into()});}
        let provider=Arc::new(MockProvider::scripted(responses));
        let host = Host::with_provider(cfg, provider).await.unwrap();
        let b = host.open_session(scope.clone()).await.unwrap();
        let r = host
            .submit_run(&scope, &b.session_id, "write".into())
            .await
            .unwrap();
        loop {
            let (snapshot, mut rx) = host.snapshot_and_subscribe(&scope).await.unwrap();
            if let Some(a) = snapshot.pending_approvals.first() {
                if mode.ends_with("pending") {
                    println!("READY {} {}", r.run_id, b.session_id);
                    std::io::stdout().flush().unwrap();
                    std::future::pending::<()>().await;
                }
                host.decide_approval(
                    &scope,
                    &a.approval_id,
                    a.revision,
                    a.binding.clone(),
                    true,
                )
                .await
                .unwrap();
                break;
            }
            let _ = rx.recv().await;
        }
        loop {
            let (snapshot, mut rx) = host.snapshot_and_subscribe(&scope).await.unwrap();
            if mode.ends_with("locked") {
                let db = rusqlite::Connection::open(config(&base).home.join("engine-host/host.db")).unwrap();
                let waiting: u64 = db.query_row("SELECT COUNT(*) FROM host_events WHERE kind='tool_waiting_resource' AND json_extract(reference,'$.tool_name')='write_file'", [], |r| r.get(0)).unwrap();
                let started: u64 = db.query_row("SELECT COUNT(*) FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.tool_name')='write_file'", [], |r| r.get(0)).unwrap();
                if waiting == 1 {
                    assert_eq!(started, 0);
                    assert!(!config(&base).roots["project"].join("report.txt").exists());
                    println!("READY {} {}", r.run_id, b.session_id);
                    std::io::stdout().flush().unwrap();
                    std::future::pending::<()>().await;
                }
            }
            let sleeping = if team {
                let db=rusqlite::Connection::open(config(&base).home.join("engine-host/host.db")).unwrap();
                let active:u64=db.query_row("SELECT COUNT(*) FROM host_events WHERE kind='tool_started' AND json_extract(reference,'$.tool_name')='sleep'",[],|r|r.get(0)).unwrap();
                active==1
            } else { snapshot.runs.iter().any(|r| {
                r.tools
                    .iter()
                    .any(|t| t.tool_name == "sleep" && t.state == "running")
            }) };
            if sleeping {
                println!("READY {} {}", r.run_id, b.session_id);
                std::io::stdout().flush().unwrap();
                std::future::pending::<()>().await;
            }
            let _ = rx.recv().await;
        }
    });
}

struct CrashBoundaryProvider {
    emit_text: bool,
    requests: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl openpaw_core::LlmProvider for CrashBoundaryProvider {
    async fn complete(
        &self,
        _: openpaw_core::CompletionRequest,
    ) -> openpaw_core::Result<openpaw_core::CompletionResponse> {
        unreachable!("Host应复用原流式执行入口")
    }
    async fn stream(
        &self,
        _: openpaw_core::CompletionRequest,
    ) -> openpaw_core::Result<
        futures::stream::BoxStream<'static, openpaw_core::Result<openpaw_core::StreamEvent>>,
    > {
        use futures::StreamExt;
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut events = vec![Ok(openpaw_core::StreamEvent::MessageStart)];
        if self.emit_text {
            events.push(Ok(openpaw_core::StreamEvent::TextDelta {
                index: 0,
                text: "硬停前已落盘的部分文本".into(),
            }));
        }
        Ok(Box::pin(
            futures::stream::iter(events).chain(futures::stream::pending()),
        ))
    }
    fn capabilities(&self) -> openpaw_core::Capabilities {
        openpaw_core::Capabilities::default()
    }
    fn name(&self) -> &str {
        "crash-boundary"
    }
    fn default_model(&self) -> &str {
        "crash-boundary"
    }
}

async fn host_shell_fixture(base: &std::path::Path, cancel: bool, team: bool) {
    let mut cfg = config(base);
    cfg.tools = vec!["bash".into()];
    let mut scope = scope();
    if team {
        scope.namespace = "office".into();
        cfg.agents = Some(AgentsConfig {
            enabled: true,
            tools: [("research".into(), vec!["bash".into()])].into(),
            ..Default::default()
        });
    }
    cfg.api_key_env = Some("OPENPAW_T3_MODEL_SECRET".into());
    cfg.basic_username_env = Some("OPENPAW_T3_BASIC_USER".into());
    cfg.basic_password_env = Some("OPENPAW_T3_BASIC_SECRET".into());
    let root = cfg.roots["project"].clone();
    std::fs::write(root.join("t3-host-worker-marker"), "isolated fixture").unwrap();
    let command = format!(
        "\"{}\" --exact host_tool_worker --nocapture",
        std::env::current_exe().unwrap().display()
    );
    let mut responses = vec![
        MockMode::ToolUse {
            tool_name: "bash".into(),
            input: json!({"command":command}),
            text: "proposed shell".into(),
        },
        MockMode::Fixed("done".into()),
    ];
    if team {
        responses.insert(0, MockMode::ToolUse {tool_name:"delegate_task".into(),input:json!({"role_id":"research","title":"核验隔离子进程","instructions":"运行已提供的受控进程测试命令","acceptance":"取消或关闭后真实进程树退出"}),text:"安排独立同事".into()});
    }
    let provider = Arc::new(MockProvider::scripted(responses));
    let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
    let b = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope.clone(), &b.session_id, "run approved fixture".into())
        .await
        .unwrap();
    let a = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (snapshot, mut rx) = host.snapshot_and_subscribe(&scope.clone()).await.unwrap();
            if let Some(a) = snapshot.pending_approvals.into_iter().next() {
                break a;
            }
            rx.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    if team {
        assert_ne!(a.binding.run_id, run.run_id, "必须是实际子Run的原审批");
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let count:u64=db.query_row("SELECT COUNT(*) FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=?1 AND json_extract(reference,'$.child_run')=?2",rusqlite::params![&run.run_id,&a.binding.run_id],|r|r.get(0)).unwrap();
        assert_eq!(count, 1, "子工具必须经持久父子所属验证");
    }
    assert!(!root.join("worker.pid").exists(), "批准前不能创建进程");
    host.decide_approval(&scope.clone(), &a.approval_id, a.revision, a.binding, true)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.join("worker-ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let view = host.get_run(&scope.clone(), &run.run_id).await.unwrap();
    assert_eq!(view.tools[0].state, "running");
    let lock_path = root.join(".openpaw/engine-host-resource.lock");
    assert_eq!(
        openpaw_sandbox::file_lock::try_lock_exclusive(&lock_path)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    if cancel {
        assert_eq!(
            host.cancel_run(&scope.clone(), &run.run_id)
                .await
                .unwrap()
                .state,
            RunState::Cancelling
        );
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        assert_eq!(
            host.get_run(&scope.clone(), &run.run_id)
                .await
                .unwrap()
                .state,
            RunState::Cancelled
        );
    } else {
        tokio::time::timeout(Duration::from_secs(10), host.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
    // 冲突锁只能在工具实际退出、结果已保存后取得；不能仅凭取消请求释放。
    let guard = openpaw_sandbox::file_lock::try_lock_exclusive(&lock_path).unwrap();
    for file in ["worker.pid", "grandchild.pid"] {
        let pid = std::fs::read_to_string(root.join(file))
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(!host_test_process_running(pid), "Host返回后仍残留进程{pid}");
    }
    assert_eq!(
        std::fs::read_to_string(root.join("secret-check")).unwrap(),
        "isolated"
    );
    drop(guard);
    if cancel {
        let (snapshot, _) = host.snapshot_and_subscribe(&scope.clone()).await.unwrap();
        let history = host
            .get_session(&scope.clone(), &b.session_id)
            .await
            .unwrap();
        let displayed = format!(
            "{}{}",
            serde_json::to_string(&snapshot).unwrap(),
            serde_json::to_string(&history).unwrap()
        );
        for secret in ["T3-test-model-value", "T3-test-basic-value"] {
            assert!(!displayed.contains(secret));
        }
        host.shutdown().await.unwrap();
    }
    // 即使旧Host克隆仍存在，关闭确认后数据目录也必须可重新独占。
    let reopened = Host::start(cfg).await.unwrap();
    assert_eq!(
        reopened
            .get_run(&scope.clone(), &run.run_id)
            .await
            .unwrap()
            .state,
        RunState::Cancelled
    );
    reopened.shutdown().await.unwrap();
}

#[test]
fn h10_h11_host_shell_credentials_tree_cleanup_and_resource_release() {
    for mode in [
        "host-shell-cancel",
        "host-shell-shutdown",
        "team-shell-cancel",
        "team-shell-shutdown",
    ] {
        let base = tempfile::tempdir().unwrap();
        let log_path = base.path().join("fixture.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let mut process = Child(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_fixture", "--nocapture"])
                .env("OPENPAW_T3_FIXTURE", mode)
                .env("OPENPAW_T3_FIXTURE_BASE", base.path())
                .env("OPENPAW_T3_MODEL_SECRET", "T3-test-model-value")
                .env("OPENPAW_T3_BASIC_USER", "T3-test-user")
                .env("OPENPAW_T3_BASIC_SECRET", "T3-test-basic-value")
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "Host进程验收超时");
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = std::fs::read_to_string(log_path).unwrap();
        for secret in ["T3-test-model-value", "T3-test-basic-value"] {
            assert!(!output.contains(secret));
        }
        assert!(status.success(), "Host原生进程测试失败：{output}");
    }
}

#[test]
fn host_tool_worker() {
    if !std::path::Path::new("t3-host-worker-marker").exists() {
        return;
    }
    for name in [
        "OPENPAW_T3_MODEL_SECRET",
        "OPENPAW_T3_BASIC_USER",
        "OPENPAW_T3_BASIC_SECRET",
    ] {
        assert!(std::env::var_os(name).is_none(), "工具环境不可继承凭据");
    }
    std::fs::write("secret-check", "isolated").unwrap();
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host_tool_grandchild", "--nocapture"])
            .spawn()
            .unwrap(),
    );
    std::fs::write("worker.pid", std::process::id().to_string()).unwrap();
    std::fs::write("grandchild.pid", child.0.id().to_string()).unwrap();
    std::fs::write("worker-ready", "ready").unwrap();
    child.0.wait().unwrap();
}
#[test]
fn host_tool_grandchild() {
    if !std::path::Path::new("t3-host-worker-marker").exists() {
        return;
    }
    loop {
        std::thread::park();
    }
}
#[cfg(target_os = "linux")]
fn host_test_process_running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("无法核查测试进程：{e}"),
    }
}
#[cfg(windows)]
fn host_test_process_running(pid: u32) -> bool {
    // 仅检查测试自身的数字PID；不增加另一套生产进程管理或Windows依赖。
    let result = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
        &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 3 }}")]).status().unwrap();
    assert!(result.code() == Some(0) || result.code() == Some(3));
    result.success()
}
#[tokio::test]
async fn h08_real_process_crash_interrupts_without_replaying_and_invalidates_approval() {
    if std::env::var("OPENPAW_CHECKPOINT_HTTP_CHILD").as_deref() != Ok("1") {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "h08_real_process_crash_interrupts_without_replaying_and_invalidates_approval",
                "--nocapture",
            ])
            .env("OPENPAW_CHECKPOINT_HTTP_CHILD", "1")
            .env(
                "PAWOFFICE_CHECKPOINT_KEY",
                "isolated-checkpoint-fixture-key",
            )
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }

    for mode in [
        "accepted",
        "completed",
        "written",
        "pending",
        "team-written",
        "team-pending",
        "approved-locked",
        "team-approved-locked",
        "text",
    ] {
        eprintln!("验证硬停边界：{mode}");
        let base = tempfile::tempdir().unwrap();
        let resource = if mode.ends_with("locked") {
            let meta = config(base.path()).roots["project"].join(".openpaw");
            std::fs::create_dir_all(&meta).unwrap();
            Some(
                // 共享锁允许审批读取文件前置条件，只阻止批准后的独占写入。
                openpaw_sandbox::file_lock::try_lock_shared(
                    &meta.join("engine-host-resource.lock"),
                )
                .unwrap(),
            )
        } else {
            None
        };
        let (mut process, line) = child(base.path(), mode);
        let ids: Vec<_> = line.split_whitespace().collect();
        let mut cfg = config(base.path());
        if mode == "text" {
            cfg.tools.clear();
            cfg.office=Some(serde_json::from_value(json!({"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_CHECKPOINT_KEY","allowed_origins":["http://127.0.0.1:8765"]})).unwrap());
        }
        let approvals: Vec<ApprovalView> = {
            let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
            let mut q = db.prepare("SELECT data FROM host_approvals").unwrap();
            q.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
                .collect()
        };
        assert!(Host::start(cfg.clone()).await.is_err());
        process.0.kill().unwrap();
        process.0.wait().unwrap();
        drop(resource);
        let report = cfg.roots["project"].join("report.txt");
        let before = std::fs::read(&report).ok();
        assert_eq!(before.is_some(), mode.ends_with("written"));
        // 独立计数恢复后的Provider请求，不用模型固定回答掩盖意外重放。
        let recovered_provider = Arc::new(CrashBoundaryProvider {
            emit_text: false,
            requests: Default::default(),
        });
        let host = Host::with_provider(cfg.clone(), recovered_provider.clone())
            .await
            .unwrap();
        let mut scope = scope();
        if mode.starts_with("team-") || mode == "text" {
            scope.namespace = "office".into();
        }
        let run = host.get_run(&scope, ids[1]).await.unwrap();
        let expected = if mode == "completed" {
            RunState::Completed
        } else {
            RunState::Interrupted
        };
        assert_eq!(run.state, expected);
        if mode == "text" {
            assert_eq!(run.text, "硬停前已落盘的部分文本");
        }
        if mode == "completed" {
            assert_eq!(run.text, "已可靠提交的终态文本");
        }
        assert!(host.pending_approvals(&scope).await.unwrap().is_empty());
        for approval in approvals {
            assert!(
                host.decide_approval(
                    &scope,
                    &approval.approval_id,
                    approval.revision,
                    approval.binding,
                    true
                )
                .await
                .is_err(),
                "硬停前的批准/待审批回执不得在恢复后复用"
            );
        }
        assert_eq!(std::fs::read(&report).ok(), before);
        assert_eq!(
            host.open_session(scope.clone()).await.unwrap().session_id,
            ids[2]
        );
        if mode == "text" {
            let server = HostServer::bind(host.clone(), 0).await.unwrap();
            let url = format!("http://{}/office/chat", server.address());
            let client = reqwest::Client::new();
            let history:serde_json::Value=client.post(&url).header("origin","http://127.0.0.1:8765").json(&json!({"type":"history","version":"1.0","teamId":"office-1","conversationId":"conversation"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
            assert_eq!(
                history["data"]["turns"][0]["assistant"],
                "硬停前已落盘的部分文本\n\n> 本轮未完成：宿主重启，未完成运行不自动重放"
            );
            assert_eq!(
                run.text, "硬停前已落盘的部分文本",
                "错误说明只投影展示，不能写入原模型正文"
            );
            assert_eq!(history["data"]["turns"][0]["state"], "error");
            let body=client.post(&url).header("origin","http://127.0.0.1:8765").json(&json!({"type":"resume","version":"1.0","teamId":"office-1","conversationId":"conversation","turnId":history["data"]["turns"][0]["id"]})).send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
            let snapshot: serde_json::Value =
                serde_json::from_str(body.lines().next().unwrap()).unwrap();
            assert_eq!(snapshot["type"], "snapshot");
            assert_eq!(snapshot["text"], history["data"]["turns"][0]["assistant"]);
            assert_eq!(snapshot["state"], "error");
            server.shutdown().await.unwrap();
        }
        host.shutdown().await.unwrap();
        assert_eq!(
            recovered_provider
                .requests
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "恢复不允许重发模型请求"
        );
        // 可重建索引实际删除再重启，不删Host权威元数据。
        std::fs::remove_file(cfg.home.join("index.db")).unwrap();
        let host = Host::start(cfg).await.unwrap();
        assert_eq!(host.get_run(&scope, ids[1]).await.unwrap().state, expected);
        assert_eq!(std::fs::read(&report).ok(), before);
        host.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn h08_corrupt_middle_fails_start_torn_tail_repairs_and_unknown_schema_refuses() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let b = host.open_session(scope()).await.unwrap();
    host.submit_run(&scope(), &b.session_id, "hello".into())
        .await
        .unwrap();
    host.wait_idle().await;
    host.shutdown().await.unwrap();
    let path = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", b.session_id));
    let valid = std::fs::read(&path).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{torn")
        .unwrap();
    let host = Host::start(cfg.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), valid);
    let mut damaged = b"{bad}\n".to_vec();
    damaged.extend_from_slice(&valid);
    std::fs::write(&path, damaged).unwrap();
    assert!(Host::start(cfg.clone()).await.is_err());
    std::fs::write(&path, &valid).unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(Host::start(cfg).await.is_err());
}

#[tokio::test]
async fn h05_cross_process_shared_reads_and_cancellable_exclusive_wait() {
    for mode in ["lock-shared", "lock-exclusive"] {
        let base = tempfile::tempdir().unwrap();
        let cfg = config(base.path());
        std::fs::write(cfg.roots["project"].join("read.txt"), "source").unwrap();
        let (mut child, _) = child(base.path(), mode);
        let mut cfg = cfg;
        cfg.tools = vec!["read_file".into()];
        let provider = Arc::new(MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: json!({"path":"read.txt"}),
                text: "read".into(),
            },
            MockMode::Fixed("done".into()),
        ]));
        let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
        let b = host.open_session(scope()).await.unwrap();
        let r = host
            .submit_run(&scope(), &b.session_id, "read".into())
            .await
            .unwrap();
        if mode == "lock-exclusive" {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (snap, mut rx) = host.snapshot_and_subscribe(&scope()).await.unwrap();
                    if snap.runs.iter().any(|r| !r.tools.is_empty()) {
                        break;
                    }
                    let _ = rx.recv().await;
                }
            })
            .await
            .unwrap();
            assert!(host
                .get_run(&scope(), &r.run_id)
                .await
                .unwrap()
                .tools
                .iter()
                .all(|t| t.state != "running"));
            host.cancel_run(&scope(), &r.run_id).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), host.wait_idle())
            .await
            .unwrap();
        assert_eq!(
            host.get_run(&scope(), &r.run_id).await.unwrap().state,
            if mode == "lock-shared" {
                RunState::Completed
            } else {
                RunState::Cancelled
            }
        );
        // 不同home使用同根仍会受同一OS锁协调。
        cfg.home = base.path().join("second-home");
        let second = Host::start(cfg).await.unwrap();
        second.shutdown().await.unwrap();
        host.shutdown().await.unwrap();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
    }
}
#[tokio::test]
async fn h08_corrupt_index_rebuild_preserves_ledger_and_root_cannot_change() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let b = host.open_session(scope()).await.unwrap();
    host.submit_run(&scope(), &b.session_id, "hello".into())
        .await
        .unwrap();
    host.wait_idle().await;
    host.shutdown().await.unwrap();
    let ledger = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", b.session_id));
    let before = std::fs::read(&ledger).unwrap();
    std::fs::write(cfg.home.join("index.db"), "not sqlite").unwrap();
    let host = Host::start(cfg.clone()).await.unwrap();
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    assert!(std::fs::read_dir(&cfg.home).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("index-corrupt-")));
    host.shutdown().await.unwrap();
    let another = base.path().join("another");
    std::fs::create_dir(&another).unwrap();
    cfg.roots.insert("project".into(), another);
    assert!(Host::start(cfg).await.is_err());
}

#[tokio::test]
async fn workspace_migration_preserves_real_parent_child_ledger_and_isolates_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut old = config(tmp.path());
    old.slots.insert("office-2".into(), "project".into());
    old.tools = vec!["read_file".into(), "write_file".into()];
    old.agents = Some(AgentsConfig {
        enabled: true,
        tools: [("draft".into(), old.tools.clone())].into(),
        ..Default::default()
    });
    let source = old.roots["project"].clone();
    let absolute = source.join("report.txt").to_string_lossy().to_string();
    std::fs::write(source.join("brief.md"), "shared input").unwrap();
    let mut script = Vec::new();
    for _ in 0..2 {
        script.extend([
            MockMode::ToolUse { tool_name: "delegate_task".into(), input: json!({"role_id":"draft","title":"保存迁移前报告","instructions":"写原报告，等待本次人工审批","acceptance":"真实落盘"}), text:"分配真实任务".into() },
            MockMode::ToolUse { tool_name: "write_file".into(), input: json!({"path":absolute,"content":"shared report"}), text:"保存报告".into() },
            MockMode::Fixed("子任务完成".into()), MockMode::Fixed("主任务完成".into()),
        ]);
    }
    let host = Host::with_provider(old.clone(), Arc::new(MockProvider::scripted(script)))
        .await
        .unwrap();
    let mut bindings = Vec::new();
    let mut runs = Vec::new();
    for team in ["office-1", "office-2"] {
        let s = ClientScope {
            namespace: "office".into(),
            slot_id: team.into(),
            conversation_id: "existing".into(),
        };
        let b = host.open_session(s.clone()).await.unwrap();
        let r = host
            .submit_run(&s, &b.session_id, "保留真实父子任务".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (view, mut rx) = host.snapshot_and_subscribe(&s).await.unwrap();
                if let Some(a) = view.pending_approvals.first() {
                    host.decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), true)
                        .await
                        .unwrap();
                    break;
                }
                rx.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        host.wait_idle().await;
        assert_eq!(
            host.get_run(&s, &r.run_id).await.unwrap().state,
            RunState::Completed
        );
        bindings.push(b);
        runs.push(r.run_id);
    }
    host.shutdown().await.unwrap();
    let old_path = tmp.path().join("old.toml");
    std::fs::write(&old_path, toml::to_string(&old).unwrap()).unwrap();
    let destination = tmp.path().join("团队二 workspace");
    std::fs::create_dir(&destination).unwrap();
    let mut next = old.clone();
    next.roots.insert("project-2".into(), destination.clone());
    next.slots.insert("office-2".into(), "project-2".into());
    let next_path = tmp.path().join("target.toml");
    std::fs::write(&next_path, toml::to_string(&next).unwrap()).unwrap();
    std::fs::write(old.home.join(".env"), "FIXTURE_SECRET=never-copy").unwrap();
    assert!(
        plan_workspaces(&old_path, &next_path, &["office-2".into()]).is_err(),
        "不能为完整业务备份复制混放在home中的模型配置"
    );
    assert_eq!(
        std::fs::read_to_string(old.home.join(".env")).unwrap(),
        "FIXTURE_SECRET=never-copy"
    );
    std::fs::remove_file(old.home.join(".env")).unwrap();
    let snapshot =
        |path: &std::path::Path| -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
            let mut result = std::collections::BTreeMap::new();
            let mut todo = vec![path.to_owned()];
            while let Some(dir) = todo.pop() {
                for e in std::fs::read_dir(dir).unwrap() {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        todo.push(p)
                    } else {
                        result.insert(
                            p.strip_prefix(path).unwrap().to_owned(),
                            std::fs::read(p).unwrap(),
                        );
                    }
                }
            }
            result
        };
    let before = snapshot(tmp.path());
    let plan: serde_json::Value = serde_json::from_str(
        &plan_workspaces(&old_path, &next_path, &["office-2".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot(tmp.path()),
        before,
        "只读预检不能创建WAL/SHM/输出或改业务文件"
    );
    assert_eq!(
        plan["payload"]["moves"][0]["sessions"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "团队主子绑定须完整进入原计划"
    );
    assert_eq!(plan["payload"]["moves"][0]["counts"]["sessions"], 2);
    assert_eq!(plan["payload"]["moves"][0]["counts"]["runs"], 2);
    assert_eq!(plan["payload"]["moves"][0]["counts"]["children"], 1);
    assert_eq!(plan["payload"]["moves"][0]["counts"]["approvals"], 1);
    let plan_path = tmp.path().join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
    let confirm = plan["sha256"].as_str().unwrap();
    assert!(apply_workspaces(
        &old_path,
        &next_path,
        &plan_path,
        &"0".repeat(64),
        &tmp.path().join("wrong-backup")
    )
    .is_err());
    assert!(!tmp.path().join("wrong-backup").exists());
    // 实际源漂移与目标冲突均在备份/绑定提交前拒绝，不能自动接受新的摘要。
    std::fs::write(source.join("brief.md"), "changed").unwrap();
    assert!(apply_workspaces(
        &old_path,
        &next_path,
        &plan_path,
        confirm,
        &tmp.path().join("stale-backup")
    )
    .is_err());
    assert!(!tmp.path().join("stale-backup").exists());
    std::fs::write(source.join("brief.md"), "shared input").unwrap();
    std::fs::write(destination.join("brief.md"), "conflict").unwrap();
    assert!(plan_workspaces(&old_path, &next_path, &["office-2".into()]).is_err());
    std::fs::remove_file(destination.join("brief.md")).unwrap();
    let original_source = snapshot(&source);
    let original_ledgers = snapshot(&old.home.join("sessions"));
    let backup = tmp.path().join("backup");
    let receipt: serde_json::Value = serde_json::from_str(
        &apply_workspaces(&old_path, &next_path, &plan_path, confirm, &backup).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["phase"], "committed");
    assert_eq!(snapshot(&source), original_source);
    assert_eq!(
        snapshot(&old.home.join("sessions")),
        original_ledgers,
        "原JSONL逐字节不变"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("report.txt")).unwrap(),
        "shared report"
    );
    assert!(backup.join("home/engine-host/host.db").is_file());
    assert!(backup.join("committed.json").is_file());
    assert!(
        apply_workspaces(
            &old_path,
            &next_path,
            &plan_path,
            confirm,
            &tmp.path().join("repeat")
        )
        .is_err(),
        "旧计划不能重复重绑"
    );
    assert!(
        Host::with_provider(
            old.clone(),
            Arc::new(MockProvider::fixed("must not replay"))
        )
        .await
        .is_err(),
        "迁移后不能用原配置悄悄回到共享根"
    );
    let host = Host::with_provider(
        next.clone(),
        Arc::new(MockProvider::fixed("must not replay")),
    )
    .await
    .unwrap();
    for (binding, run) in bindings.iter().zip(&runs) {
        let current = host.open_session(binding.scope.clone()).await.unwrap();
        assert_eq!(current.session_id, binding.session_id);
        assert_eq!(
            host.get_run(&binding.scope, run).await.unwrap().state,
            RunState::Completed
        );
        let history = host
            .get_session(&binding.scope, &binding.session_id)
            .await
            .unwrap();
        assert_eq!(
            history.meta.workspace_id.as_deref(),
            Some("project"),
            "旧JSONL保留历史工作根事实"
        );
    }
    let query = |index: usize, path: String| QueryRequest {
        session_id: bindings[index].session_id.clone(),
        kind: QueryKind::File { path },
        offset: 0,
        limit: 5,
    };
    let page = host
        .query(&bindings[1].scope, query(1, absolute.clone()))
        .await
        .unwrap();
    assert_eq!(page.items[0]["text"], "shared report");
    std::fs::write(source.join("report.txt"), "team one only").unwrap();
    assert_eq!(
        host.query(&bindings[0].scope, query(0, "report.txt".into()))
            .await
            .unwrap()
            .items[0]["text"],
        "team one only"
    );
    assert_eq!(
        host.query(&bindings[1].scope, query(1, absolute.clone()))
            .await
            .unwrap()
            .items[0]["text"],
        "shared report"
    );
    assert!(host
        .query(
            &bindings[0].scope,
            query(
                0,
                destination
                    .join("report.txt")
                    .to_string_lossy()
                    .into_owned()
            )
        )
        .await
        .is_err());
    std::fs::write(destination.join("report.txt"), "team two only").unwrap();
    assert!(
        host.query(&bindings[1].scope, query(1, absolute))
            .await
            .is_err(),
        "旧产物映射发现内容变化须明确失败"
    );
    assert_eq!(
        host.query(&bindings[1].scope, query(1, "report.txt".into()))
            .await
            .unwrap()
            .items[0]["text"],
        "team two only"
    );
    host.shutdown().await.unwrap();
    // 损坏的迁移源路径不能成为历史产物的可信别名，清单摘要损坏也必须拒绝启动。
    let database = old.home.join("engine-host/host.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    let original: String = conn
        .query_row(
            "SELECT reference FROM host_events WHERE kind='workspace_rebind'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    for field in ["old_path", "manifest_sha256"] {
        let mut corrupted: serde_json::Value = serde_json::from_str(&original).unwrap();
        corrupted[field] = if field == "old_path" {
            json!(tmp.path().join("unregistered-original-root"))
        } else {
            json!("0".repeat(64))
        };
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute(
            "UPDATE host_events SET reference=?1 WHERE kind='workspace_rebind'",
            [serde_json::to_string(&corrupted).unwrap()],
        )
        .unwrap();
        drop(conn);
        assert!(
            Host::with_provider(
                next.clone(),
                Arc::new(MockProvider::fixed("must not replay"))
            )
            .await
            .is_err(),
            "损坏的{field}不得继续启动或读取历史产物"
        );
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute(
            "UPDATE host_events SET reference=?1 WHERE kind='workspace_rebind'",
            [&original],
        )
        .unwrap();
    }
    // 完整home回退后原配置和稳定绑定仍有效；不删除新根后来产生的文件。
    let saved_new_home = tmp.path().join("migrated-home");
    std::fs::rename(&old.home, &saved_new_home).unwrap();
    fn copy_directory(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.path().is_dir() {
                copy_directory(&e.path(), &dest)
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }
    copy_directory(&backup.join("home"), &old.home);
    let restored = Host::with_provider(
        old.clone(),
        Arc::new(MockProvider::fixed("must not replay")),
    )
    .await
    .unwrap();
    for binding in &bindings {
        assert_eq!(
            restored
                .open_session(binding.scope.clone())
                .await
                .unwrap()
                .root_id,
            "project"
        );
    }
    restored.shutdown().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(destination.join("report.txt")).unwrap(),
        "team two only"
    );
}

#[tokio::test]
async fn workspace_migration_transaction_failure_preserves_old_binding_and_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut old = config(tmp.path());
    old.slots.insert("office-2".into(), "project".into());
    let source = old.roots["project"].clone();
    std::fs::write(source.join("input.txt"), "original").unwrap();
    let scope = ClientScope {
        namespace: "office".into(),
        slot_id: "office-2".into(),
        conversation_id: "existing".into(),
    };
    let host = Host::with_provider(old.clone(), Arc::new(MockProvider::fixed("no execution")))
        .await
        .unwrap();
    let binding = host.open_session(scope.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    let database = old.home.join("engine-host/host.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_migration BEFORE UPDATE ON host_bindings BEGIN SELECT RAISE(ABORT,'fixture SQL failure'); END; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(conn);
    let old_path = tmp.path().join("old.toml");
    std::fs::write(&old_path, toml::to_string(&old).unwrap()).unwrap();
    let mut target = old.clone();
    let destination = tmp.path().join("new");
    std::fs::create_dir(&destination).unwrap();
    target.roots.insert("new".into(), destination.clone());
    target.slots.insert("office-2".into(), "new".into());
    let target_path = tmp.path().join("target.toml");
    std::fs::write(&target_path, toml::to_string(&target).unwrap()).unwrap();
    assert!(plan_workspaces(&old_path, &target_path, &[]).is_err());
    assert!(plan_workspaces(&old_path, &target_path, &["unknown".into()]).is_err());
    assert!(plan_workspaces(
        &old_path,
        &target_path,
        &["office-2".into(), "office-2".into()]
    )
    .is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source.join("input.txt"), source.join("linked.txt")).unwrap();
        assert!(plan_workspaces(&old_path, &target_path, &["office-2".into()]).is_err());
        std::fs::remove_file(source.join("linked.txt")).unwrap();
    }
    let plan: serde_json::Value = serde_json::from_str(
        &plan_workspaces(&old_path, &target_path, &["office-2".into()]).unwrap(),
    )
    .unwrap();
    let plan_path = tmp.path().join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    let backup = tmp.path().join("backup");
    assert!(apply_workspaces(
        &old_path,
        &target_path,
        &plan_path,
        plan["sha256"].as_str().unwrap(),
        &backup
    )
    .is_err());
    assert!(backup.join("home/engine-host/host.db").exists());
    assert!(backup.join("backed-up.json").exists());
    assert!(!backup.join("committed.json").exists());
    let conn = rusqlite::Connection::open(&database).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT data FROM host_bindings WHERE id=?1",
            [&binding.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<SessionBinding>(&raw)
            .unwrap()
            .root_id,
        "project"
    );
    let count: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM host_events WHERE kind='workspace_rebind'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    conn.execute_batch("DROP TRIGGER reject_migration").unwrap();
    drop(conn);
    assert_eq!(
        std::fs::read_to_string(source.join("input.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("input.txt")).unwrap(),
        "original",
        "保留失败复制清单，不删除可能已被用户编辑的新文件"
    );
    let host = Host::with_provider(old, Arc::new(MockProvider::fixed("no replay")))
        .await
        .unwrap();
    assert_eq!(
        host.open_session(scope).await.unwrap().session_id,
        binding.session_id
    );
    host.shutdown().await.unwrap();
}
