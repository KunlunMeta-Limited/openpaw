//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
use async_trait::async_trait;
use openpaw_core::{Capabilities, CompletionRequest, CompletionResponse, LlmProvider, StreamEvent};
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use serde_json::json;
use std::{sync::Arc, time::Duration};

fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("项目 workspace");
    std::fs::create_dir_all(&root).unwrap();
    HostConfig {
        agents: None,
        office: None,
        home: base.join("home"),
        roots: [("project".into(), root)].into(),
        slots: (1..=5)
            .map(|i| (format!("office-{i}"), "project".into()))
            .collect(),
        max_active_runs: 4,
        provider: "mock".into(),
        model: None,
        base_url: None,
        api_key_env: None,
        basic_username_env: None,
        basic_password_env: None,
        tools: vec![
            "read_file".into(),
            "write_file".into(),
            "bash".into(),
            "sleep".into(),
        ],
        run_timeout_secs: 15,
        stream_idle_secs: 10,
        tool_timeout_secs: 10,
        approval_timeout_secs: 5,
    }
}
fn scope(slot: usize) -> ClientScope {
    ClientScope {
        namespace: "test".into(),
        slot_id: format!("office-{slot}"),
        conversation_id: "conversation".into(),
    }
}
async fn pending(host: &Host, s: &ClientScope) -> ApprovalView {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (snapshot, mut receiver) = host.snapshot_and_subscribe(s).await.unwrap();
            if let Some(a) = snapshot.pending_approvals.first() {
                return a.clone();
            }
            let _ = receiver.recv().await;
        }
    })
    .await
    .expect("真实引擎应产生持久审批")
}
fn writer() -> Arc<dyn LlmProvider> {
    Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"report.txt","content":"approved report"}),
            text: "writing".into(),
        },
        MockMode::Fixed("done".into()),
    ]))
}
struct Slow;
#[async_trait]
impl LlmProvider for Slow {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        MockProvider::fixed("done").complete(req).await
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>>
    {
        tokio::time::sleep(Duration::from_secs(2)).await;
        MockProvider::stream_text("slow complete", 3)
            .stream(req)
            .await
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn default_model(&self) -> &str {
        "test-slow"
    }
    fn name(&self) -> &str {
        "slow"
    }
}
struct BurstyWriter(MockProvider);
#[async_trait]
impl LlmProvider for BurstyWriter {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        self.0.complete(req).await
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>>
    {
        let mut response = self.complete(req).await?;
        let mut events = vec![Ok(StreamEvent::MessageStart)];
        if response
            .content
            .iter()
            .any(|part| matches!(part, openpaw_core::ContentBlock::ToolUse { .. }))
        {
            // 与同一次tool_use回复一同流出文本，不能用已EndTurn的另一回复假造后续工具。
            response.content.insert(
                0,
                openpaw_core::ContentBlock::Text {
                    text: "流".repeat(640),
                },
            );
            for _ in 0..640 {
                events.push(Ok(StreamEvent::TextDelta {
                    index: 0,
                    text: "流".into(),
                }));
            }
        }
        events.push(Ok(StreamEvent::Final(Box::new(response))));
        Ok(Box::pin(futures::stream::iter(events)))
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn default_model(&self) -> &str {
        "test-burst"
    }
    fn name(&self) -> &str {
        "burst"
    }
}
#[tokio::test]
async fn h01_h02_parallel_capacity_and_cancel_are_isolated() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let host = Host::with_provider(cfg, Arc::new(Slow)).await.unwrap();
    let mut runs = Vec::new();
    for i in 1..=4 {
        let b = host.open_session(scope(i)).await.unwrap();
        let r = host
            .submit_run(&scope(i), &b.session_id, format!("input {i}"))
            .await
            .unwrap();
        runs.push((b, r));
    }
    let busy = host
        .submit_run(&scope(1), &runs[0].0.session_id, "duplicate".into())
        .await
        .unwrap_err();
    assert_eq!(busy.kind, "session_busy");
    let fifth = host.open_session(scope(5)).await.unwrap();
    assert_eq!(
        host.submit_run(&scope(5), &fifth.session_id, "capacity".into())
            .await
            .unwrap_err()
            .kind,
        "capacity_exceeded"
    );
    host.cancel_run(&scope(1), &runs[0].1.run_id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&scope(1), &runs[0].1.run_id)
            .await
            .unwrap()
            .state,
        RunState::Cancelled
    );
    for (i, (_, run)) in runs.iter().enumerate().skip(1) {
        let r = host.get_run(&scope(i + 1), &run.run_id).await.unwrap();
        assert_eq!(r.state, RunState::Completed);
        assert_eq!(r.text, "slow complete");
    }
    assert_eq!(
        host.get_run(&scope(2), &runs[0].1.run_id)
            .await
            .unwrap_err()
            .kind,
        "scope_denied"
    );
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn h03_exact_approval_duplicate_and_real_tool_states() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let root = cfg.roots["project"].clone();
    let host = Host::with_provider(cfg, writer()).await.unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "write report".into())
        .await
        .unwrap();
    let a = pending(&host, &s).await;
    assert!(!root.join("report.txt").exists());
    let v = host.get_run(&s, &run.run_id).await.unwrap();
    assert_eq!(v.tools[0].state, "awaiting_approval");
    let mut wrong = a.binding.clone();
    wrong.input["content"] = json!("not approved");
    assert_eq!(
        host.decide_approval(&s, &a.approval_id, a.revision, wrong, true)
            .await
            .unwrap_err()
            .kind,
        "approval_conflict"
    );
    host.decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), true)
        .await
        .unwrap();
    // 并发重复决定不能变成第二次执行；可能已结束时应明确冲突。
    let duplicate = host
        .decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), true)
        .await;
    assert!(duplicate.is_ok() || duplicate.unwrap_err().kind == "approval_conflict");
    host.wait_idle().await;
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "approved report"
    );
    let view = host.get_run(&s, &run.run_id).await.unwrap();
    assert_eq!(view.state, RunState::Completed);
    assert_eq!(view.tools[0].state, "completed");
    assert!(host
        .decide_approval(&s, &a.approval_id, a.revision, a.binding, false)
        .await
        .is_err());
    let history = host
        .query(
            &s,
            QueryRequest {
                session_id: b.session_id,
                kind: QueryKind::ToolResults,
                offset: 0,
                limit: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(history.items.len(), 2);
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn h04_denial_expiry_and_cancel_never_write() {
    for mode in ["denied", "expired", "cancelled"] {
        let base = tempfile::tempdir().unwrap();
        let mut cfg = config(base.path());
        cfg.approval_timeout_secs = 1;
        let root = cfg.roots["project"].clone();
        let host = Host::with_provider(cfg, writer()).await.unwrap();
        let s = scope(1);
        let b = host.open_session(s.clone()).await.unwrap();
        let run = host
            .submit_run(&s, &b.session_id, "write".into())
            .await
            .unwrap();
        let a = pending(&host, &s).await;
        match mode {
            "denied" => {
                host.decide_approval(&s, &a.approval_id, a.revision, a.binding, false)
                    .await
                    .unwrap();
            }
            "cancelled" => {
                host.cancel_run(&s, &run.run_id).await.unwrap();
            }
            _ => {}
        }
        tokio::time::timeout(Duration::from_secs(4), host.wait_idle())
            .await
            .unwrap();
        assert!(!root.join("report.txt").exists());
        assert!(host.pending_approvals(&s).await.unwrap().is_empty());
        host.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn h05_cancel_resource_wait_and_h06_changed_file_invalidates_approval() {
    for change in [false, true] {
        let base = tempfile::tempdir().unwrap();
        let cfg = config(base.path());
        let root = cfg.roots["project"].clone();
        std::fs::write(root.join("report.txt"), "before").unwrap();
        let host = Host::with_provider(cfg, writer()).await.unwrap();
        let s = scope(1);
        let b = host.open_session(s.clone()).await.unwrap();
        let run = host
            .submit_run(&s, &b.session_id, "write".into())
            .await
            .unwrap();
        let a = pending(&host, &s).await;
        if change {
            std::fs::write(root.join("report.txt"), "external edit").unwrap();
        }
        let lock = if change {
            None
        } else {
            Some(
                openpaw_sandbox::file_lock::try_lock_exclusive(
                    &root.join(".openpaw/engine-host-resource.lock"),
                )
                .unwrap(),
            )
        };
        host.decide_approval(&s, &a.approval_id, a.revision, a.binding, true)
            .await
            .unwrap();
        if !change {
            // 历史不获取工作根锁，外部独占不应阻塞历史与停止。
            tokio::time::timeout(
                Duration::from_secs(1),
                host.query(
                    &s,
                    QueryRequest {
                        session_id: b.session_id,
                        kind: QueryKind::History,
                        offset: 0,
                        limit: 5,
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();
            host.cancel_run(&s, &run.run_id).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), host.wait_idle())
            .await
            .unwrap();
        drop(lock);
        assert_eq!(
            std::fs::read_to_string(root.join("report.txt")).unwrap(),
            if change { "external edit" } else { "before" }
        );
        host.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn h07_six_queries_do_not_create_data_and_paginate() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let root = cfg.roots["project"].clone();
    let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("done")))
        .await
        .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    for _ in 0..4 {
        host.submit_run(&s, &b.session_id, "hello".into())
            .await
            .unwrap();
        host.wait_idle().await;
    }
    for kind in [QueryKind::Skills, QueryKind::Goals, QueryKind::Todo] {
        assert!(host
            .query(
                &s,
                QueryRequest {
                    session_id: b.session_id.clone(),
                    kind,
                    offset: 0,
                    limit: 0
                }
            )
            .await
            .unwrap()
            .items
            .is_empty());
    }
    assert!(!root.join(".openpaw/skills").exists());
    assert!(!root.join(".openpaw/goals").exists());
    assert!(!root.join(".openpaw/todos.jsonl").exists());
    let page = host
        .query(
            &s,
            QueryRequest {
                session_id: b.session_id.clone(),
                kind: QueryKind::History,
                offset: 0,
                limit: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 5);
    assert_eq!(page.next_cursor, Some(5));
    std::fs::write(root.join("text.txt"), "汉".repeat(80_000)).unwrap();
    let page = host
        .query(
            &s,
            QueryRequest {
                session_id: b.session_id.clone(),
                kind: QueryKind::File {
                    path: "text.txt".into(),
                },
                offset: 0,
                limit: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items[0]["text"].as_str().unwrap().len(), 240_000);
    std::fs::write(root.join("large"), vec![b'a'; 256 * 1024 + 1]).unwrap();
    assert!(host
        .query(
            &s,
            QueryRequest {
                session_id: b.session_id,
                kind: QueryKind::File {
                    path: "large".into()
                },
                offset: 0,
                limit: 0
            }
        )
        .await
        .is_err());
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn h09_subscription_lag_recovers_committed_terminal_and_detach_does_not_cancel() {
    let base = tempfile::tempdir().unwrap();
    let text = "流".repeat(640);
    let host = Host::with_provider(
        config(base.path()),
        Arc::new(BurstyWriter(MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"report.txt","content":"after lag"}),
                text: "writing".into(),
            },
            MockMode::Fixed("done".into()),
        ]))),
    )
    .await
    .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let (first, mut lagged) = host.snapshot_and_subscribe(&s).await.unwrap();
    let (_, detached) = host.snapshot_and_subscribe(&s).await.unwrap();
    drop(detached);
    let run = host
        .submit_run(&s, &b.session_id, "input".into())
        .await
        .unwrap();
    let a = pending(&host, &s).await;
    // 故意不读640次文本通知，真实超过256容量；审批仍必须能从可靠视图恢复。
    assert!(matches!(
        lagged.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) if n > 0
    ));
    let (recovered, _) = host.snapshot_and_subscribe(&s).await.unwrap();
    assert!(recovered.revision > first.revision);
    assert_eq!(recovered.pending_approvals.len(), 1);
    assert_eq!(recovered.pending_approvals[0].approval_id, a.approval_id);
    assert!(recovered.runs[0].text.starts_with(&text));
    host.decide_approval(&s, &a.approval_id, a.revision, a.binding, true)
        .await
        .unwrap();
    host.wait_idle().await;
    let (last, _) = host.snapshot_and_subscribe(&s).await.unwrap();
    assert!(last.revision > first.revision);
    assert_eq!(last.runs[0].run_id, run.run_id);
    assert_eq!(last.runs[0].state, RunState::Completed);
    assert!(last.runs[0].text.starts_with(&text));
    assert!(last.runs[0].text.ends_with("done"));
    assert!(last.pending_approvals.is_empty());
    assert_eq!(
        std::fs::read_to_string(base.path().join("项目 workspace/report.txt")).unwrap(),
        "after lag"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn h04_competing_decisions_and_cancel_have_one_effective_outcome() {
    for cancel in [false, true] {
        let base = tempfile::tempdir().unwrap();
        let cfg = config(base.path());
        let root = cfg.roots["project"].clone();
        let host = Host::with_provider(cfg, writer()).await.unwrap();
        let s = scope(1);
        let b = host.open_session(s.clone()).await.unwrap();
        let run = host
            .submit_run(&s, &b.session_id, "write".into())
            .await
            .unwrap();
        let a = pending(&host, &s).await;
        // 已取得审批前视图后持外部独占锁，保证即使批准先到也无法先写文件。
        let guard = openpaw_sandbox::file_lock::try_lock_exclusive(
            &root.join(".openpaw/engine-host-resource.lock"),
        )
        .unwrap();
        if cancel {
            let (approved, cancelled) = tokio::join!(
                host.decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), true),
                host.cancel_run(&s, &run.run_id),
            );
            assert!(approved.is_ok() || approved.unwrap_err().kind == "approval_conflict");
            assert_eq!(cancelled.unwrap().state, RunState::Cancelling);
        } else {
            let (approved, denied) = tokio::join!(
                host.decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), true),
                host.decide_approval(&s, &a.approval_id, a.revision, a.binding.clone(), false),
            );
            assert_ne!(approved.is_ok(), denied.is_ok(), "相反决定只能有一个生效");
            let rejected = if let Err(error) = approved {
                error
            } else {
                denied.unwrap_err()
            };
            assert_eq!(rejected.kind, "approval_conflict");
            host.cancel_run(&s, &run.run_id).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), host.wait_idle())
            .await
            .unwrap();
        drop(guard);
        let final_run = host.get_run(&s, &run.run_id).await.unwrap();
        assert!(final_run.state.is_terminal());
        if cancel {
            assert_eq!(final_run.state, RunState::Cancelled);
        }
        assert!(!root.join("report.txt").exists());
        assert!(final_run.tools.iter().all(|tool| tool.state != "running"));
        assert!(host.pending_approvals(&s).await.unwrap().is_empty());
        assert!(host
            .decide_approval(&s, &a.approval_id, a.revision, a.binding, true)
            .await
            .is_err());
        host.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn h10_configuration_scope_and_root_aliases() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    cfg.roots
        .insert("alias".into(), cfg.roots["project"].clone());
    cfg.slots.insert("office-2".into(), "alias".into());
    let host = Host::start(cfg.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    cfg.roots
        .insert("nested".into(), cfg.roots["project"].join("nested"));
    std::fs::create_dir_all(&cfg.roots["nested"]).unwrap();
    assert!(Host::start(cfg).await.is_err());
    let mut cfg = config(base.path());
    cfg.home = cfg.roots["project"].join("home");
    assert!(Host::start(cfg).await.is_err());
}

#[tokio::test]
async fn h02_simultaneous_submissions_and_same_slot_different_conversation_conflict() {
    let base = tempfile::tempdir().unwrap();
    let host = Host::with_provider(config(base.path()), Arc::new(Slow))
        .await
        .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let (left, right) = tokio::join!(
        host.submit_run(&s, &b.session_id, "first".into()),
        host.submit_run(&s, &b.session_id, "second".into()),
    );
    let (run, conflict) = match (left, right) {
        (Ok(run), Err(error)) | (Err(error), Ok(run)) => (run, error),
        _ => panic!("同一会话并发提交只能接受一次"),
    };
    assert_eq!(conflict.kind, "session_busy");
    let mut other = s.clone();
    other.conversation_id = "different-conversation".into();
    let b2 = host.open_session(other.clone()).await.unwrap();
    assert_eq!(
        host.submit_run(&other, &b2.session_id, "same office".into())
            .await
            .unwrap_err()
            .kind,
        "slot_busy"
    );
    host.cancel_run(&s, &run.run_id).await.unwrap();
    host.wait_idle().await;
    // 终态后同槽名额可再次使用，不把冲突预留永久留在注册表。
    let next = host
        .submit_run(&other, &b2.session_id, "after release".into())
        .await
        .unwrap();
    host.cancel_run(&other, &next.run_id).await.unwrap();
    host.wait_idle().await;
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn h10_real_directory_alias_and_escape_link_are_checked() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    let root = cfg.roots["project"].clone();
    let outside = base.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("private.txt"), "outside content").unwrap();
    let alias = base.path().join("root-alias");
    let escape = root.join("escape");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
    }
    #[cfg(windows)]
    {
        // 目录junction不要求管理员或开发者模式；此处验证真实重解析点而非字符串大小写。
        for (link, target) in [(&alias, &root), (&escape, &outside)] {
            let output = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction创建失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let upper = root.to_string_lossy().to_uppercase();
        assert!(same_file::is_same_file(&root, std::path::Path::new(&upper)).unwrap());
        cfg.roots.insert("case-alias".into(), upper.into());
    }
    cfg.roots.insert("alias".into(), alias);
    cfg.slots.insert("office-2".into(), "alias".into());
    let host = Host::start(cfg).await.unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let result = host
        .query(
            &s,
            QueryRequest {
                session_id: b.session_id.clone(),
                kind: QueryKind::File {
                    path: "escape/private.txt".into(),
                },
                offset: 0,
                limit: 0,
            },
        )
        .await;
    assert!(result.is_err(), "链接不可绕过授权根");
    assert_eq!(
        host.query(
            &scope(2),
            QueryRequest {
                session_id: b.session_id,
                kind: QueryKind::History,
                offset: 0,
                limit: 0,
            }
        )
        .await
        .unwrap_err()
        .kind,
        "scope_denied"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn h08_storage_failure_latches_failed_and_no_tool_side_effect() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let db_path = cfg.home.join("engine-host/host.db");
    let root = cfg.roots["project"].clone();
    let host = Host::with_provider(cfg, writer()).await.unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_approval BEFORE INSERT ON host_approvals BEGIN SELECT RAISE(FAIL,'test storage failure'); END;").unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "write".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), host.wait_idle())
        .await
        .unwrap();
    let run = host.get_run(&s, &run.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.error.unwrap().kind, "storage_unavailable");
    assert!(!root.join("report.txt").exists());
    assert_eq!(
        host.submit_run(&s, &b.session_id, "must refuse".into())
            .await
            .unwrap_err()
            .kind,
        "storage_unavailable"
    );
    let session = host.get_session(&s, &b.session_id).await.unwrap();
    assert!(session
        .messages
        .iter()
        .any(|m| m.role == openpaw_core::Role::User));
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn h08_terminal_storage_failure_is_not_reported_as_success() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let db_path = cfg.home.join("engine-host/host.db");
    let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("done")))
        .await
        .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_terminal BEFORE UPDATE ON host_runs WHEN json_extract(NEW.data,'$.state')='completed' BEGIN SELECT RAISE(FAIL,'test terminal storage failure'); END;").unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "hello".into())
        .await
        .unwrap();
    host.wait_idle().await;
    assert_eq!(
        host.get_run(&s, &run.run_id).await.unwrap_err().kind,
        "storage_unavailable"
    );
    assert_eq!(
        host.submit_run(&s, &b.session_id, "retry".into())
            .await
            .unwrap_err()
            .kind,
        "storage_unavailable"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn h08_acceptance_failure_does_not_start_tools_or_replay_input() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    let root = cfg.roots["project"].clone();
    let host = Host::with_provider(cfg.clone(), writer()).await.unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_accept BEFORE UPDATE ON host_runs WHEN json_extract(NEW.data,'$.state')='accepted' BEGIN SELECT RAISE(FAIL,'test accept failure'); END;").unwrap();
    assert_eq!(
        host.submit_run(&s, &b.session_id, "must not start".into())
            .await
            .unwrap_err()
            .kind,
        "storage_unavailable"
    );
    tokio::time::timeout(Duration::from_secs(1), host.wait_idle())
        .await
        .unwrap();
    assert!(!root.join("report.txt").exists());
    assert!(host.pending_approvals(&s).await.unwrap().is_empty());
    let (before, _) = host.snapshot_and_subscribe(&s).await.unwrap();
    assert_eq!(before.runs.len(), 1);
    assert_eq!(before.runs[0].state, RunState::Failed);
    db.execute_batch("DROP TRIGGER reject_accept").unwrap();
    drop(db);
    host.shutdown().await.unwrap();
    let reopened = Host::start(cfg).await.unwrap();
    let (after, _) = reopened.snapshot_and_subscribe(&s).await.unwrap();
    assert_eq!(after.runs[0].state, RunState::Failed);
    assert!(!root.join("report.txt").exists());
    let session = reopened.get_session(&s, &b.session_id).await.unwrap();
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|m| m.role == openpaw_core::Role::User)
            .count(),
        1
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn tool_waiting_resource_is_distinct_from_execution_and_can_be_cancelled() {
    let base = tempfile::tempdir().unwrap();
    let cfg = config(base.path());
    std::fs::write(cfg.roots["project"].join("read.txt"), "not-yet-read").unwrap();
    let provider = MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "read_file".into(),
            input: json!({"path":"read.txt"}),
            text: "读取".into(),
        },
        MockMode::Fixed("done".into()),
    ]);
    let host = Host::with_provider(cfg.clone(), Arc::new(provider))
        .await
        .unwrap();
    let guard = openpaw_sandbox::file_lock::try_lock_exclusive(
        &cfg.roots["project"].join(".openpaw/engine-host-resource.lock"),
    )
    .unwrap();
    let s = scope(1);
    let binding = host.open_session(s.clone()).await.unwrap();
    let run = host
        .submit_run(&s, &binding.session_id, "读取read.txt".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let view = host.get_run(&s, &run.run_id).await.unwrap();
            if view.tools.iter().any(|t| t.state == "awaiting_resource") {
                assert!(!view.tools.iter().any(|t| t.state == "running"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    host.cancel_run(&s, &run.run_id).await.unwrap();
    host.wait_idle().await;
    assert_eq!(
        host.get_run(&s, &run.run_id).await.unwrap().state,
        RunState::Cancelled
    );
    drop(guard);
    host.shutdown().await.unwrap();
}

struct ContextProvider(std::sync::Mutex<Vec<CompletionRequest>>);
#[async_trait]
impl LlmProvider for ContextProvider {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        MockProvider::fixed("context checked").complete(req).await
    }
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<futures::stream::BoxStream<'static, openpaw_core::Result<StreamEvent>>>
    {
        self.0.lock().unwrap().push(req.clone());
        MockProvider::fixed("context checked").stream(req).await
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn default_model(&self) -> &str {
        "context-fixture"
    }
    fn name(&self) -> &str {
        "context-fixture"
    }
}
#[tokio::test]
async fn r7_actual_model_request_has_trusted_time_and_only_configured_web_tools() {
    for enabled in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let original = config(tmp.path());
        let mut cfg: HostConfig = serde_json::from_value(
            json!({"home":original.home,"roots":original.roots,"slots":original.slots}),
        )
        .unwrap();
        assert_eq!(
            cfg.tools,
            vec!["read_file", "glob", "grep", "todo_list", "think"]
        );
        if enabled {
            cfg.tools.extend(["web_search".into(), "web_fetch".into()]);
        }
        let provider = Arc::new(ContextProvider(std::sync::Mutex::new(Vec::new())));
        let before = chrono::Utc::now();
        let host = Host::with_provider(cfg, provider.clone()).await.unwrap();
        let s = scope(1);
        let binding = host.open_session(s.clone()).await.unwrap();
        host.submit_run(&s, &binding.session_id, "what is today?".into())
            .await
            .unwrap();
        host.wait_idle().await;
        let after = chrono::Utc::now();
        let records = provider.0.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        let system = records[0].system.as_ref().unwrap();
        let tools: Vec<_> = records[0].tools.iter().map(|t| t.name.clone()).collect();
        let timestamp = |prefix: &str| {
            chrono::DateTime::parse_from_rfc3339(
                system.lines().find_map(|l| l.strip_prefix(prefix)).unwrap(),
            )
            .unwrap()
        };
        let utc = timestamp("Current UTC time: ");
        let local = timestamp("Host local time: ");
        assert_eq!(utc.timestamp_millis(), local.timestamp_millis());
        assert!(utc >= before && utc <= after);
        assert_eq!(tools.iter().any(|x| x == "web_search"), enabled);
        assert_eq!(tools.iter().any(|x| x == "web_fetch"), enabled);
        assert!(system.contains(if enabled {
            "fresh=true"
        } else {
            "Network tools are not enabled"
        }));
        drop(records);
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn d43_reopened_session_uses_current_capabilities_without_rewriting_history() {
    let tmp = tempfile::tempdir().unwrap();
    let original = config(tmp.path());
    let cfg: HostConfig = serde_json::from_value(
        json!({"home":original.home,"roots":original.roots,"slots":original.slots}),
    )
    .unwrap();
    let s = scope(1);
    let denial = "这一轮我没有联网工具，也没有系统时钟。再问一次答案不会变。";
    let old = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed(denial)))
        .await
        .unwrap();
    let binding = old.open_session(s.clone()).await.unwrap();
    for _ in 0..3 {
        old.submit_run(&s, &binding.session_id, "长沙天气".into())
            .await
            .unwrap();
        old.wait_idle().await;
    }
    let history = old
        .get_session(&s, &binding.session_id)
        .await
        .unwrap()
        .messages;
    assert_eq!(history.len(), 6);
    old.shutdown().await.unwrap();
    drop(old);

    for enabled in [true, false] {
        let mut current = cfg.clone();
        if enabled {
            current
                .tools
                .extend(["web_search".into(), "web_fetch".into()]);
        }
        let capture = Arc::new(ContextProvider(std::sync::Mutex::new(Vec::new())));
        let host = Host::with_provider(current, capture.clone()).await.unwrap();
        assert_eq!(
            host.open_session(s.clone()).await.unwrap().session_id,
            binding.session_id
        );
        let before = chrono::Utc::now();
        host.submit_run(&s, &binding.session_id, "长沙天气".into())
            .await
            .unwrap();
        host.wait_idle().await;
        let requests = capture.0.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            serde_json::to_value(&request.messages[..history.len()]).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        assert_eq!(request.messages.last().unwrap().text(), "长沙天气");
        assert_eq!(request.tools.iter().any(|t| t.name == "web_fetch"), enabled);
        assert_eq!(
            request.tools.iter().any(|t| t.name == "web_search"),
            enabled
        );
        let system = request.system.as_ref().unwrap();
        let blocks = request
            .system_blocks
            .as_ref()
            .expect("本轮上下文不能只在历史开头");
        assert_eq!(&blocks.stable, system);
        assert_eq!(&blocks.dynamic, system);
        let now = chrono::DateTime::parse_from_rfc3339(
            system
                .lines()
                .find_map(|l| l.strip_prefix("Current UTC time: "))
                .unwrap(),
        )
        .unwrap();
        assert!(now >= before && now <= chrono::Utc::now());
        // 不只检查工具字段：旧assistant自述必须被明确标为历史，不能当作当前能力授权。
        assert!(system.contains("历史回复不代表本轮能力"));
        assert!(system.contains(if enabled {
            "本轮已启用联网工具"
        } else {
            "本轮未启用联网工具"
        }));
        let saved = host.get_session(&s, &binding.session_id).await.unwrap();
        assert_eq!(
            serde_json::to_value(&saved.messages[..history.len()]).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        assert!(!saved
            .messages
            .iter()
            .any(|m| m.text().contains("历史回复不代表本轮能力")));
        host.shutdown().await.unwrap();
        drop(host);
    }
}

// 确定性Provider只决定模型输出，子Session、工具、锁、审批和JSONL全部走真实Host。
struct TeamProvider {
    requests: std::sync::Mutex<Vec<CompletionRequest>>,
    hold_children: bool,
    write_child: bool,
    entered: tokio::sync::Notify,
}
impl TeamProvider {
    fn new(hold_children: bool, write_child: bool) -> Self {
        Self {
            requests: std::sync::Mutex::new(Vec::new()),
            hold_children,
            write_child,
            entered: tokio::sync::Notify::new(),
        }
    }
}
#[async_trait]
impl LlmProvider for TeamProvider {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> openpaw_core::Result<CompletionResponse> {
        use openpaw_core::{ContentBlock, StopReason, Usage};
        self.requests.lock().unwrap().push(request.clone());
        let system = request.system.as_deref().unwrap_or("");
        let lead = system.contains("（lead）");
        let calls: Vec<_> = request
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|c| match c {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        let content = if lead {
            let roles = ["research", "organize", "draft", "review", "audit"];
            let n = calls.iter().filter(|c| c.starts_with("delegate-")).count();
            if n < roles.len() {
                let role = roles[n];
                vec![ContentBlock::ToolUse {
                    id: format!("delegate-{role}"),
                    name: "delegate_task".into(),
                    input: json!({"role_id":role,"title":format!("{role}核对项目说明"),"instructions":"读取README.md并核对项目背景，明确真实证据和限制","acceptance":"有实际文件读取和角色独立结果"}),
                }]
            } else {
                vec![ContentBlock::Text {
                    text: "五位同事已分别实际核对项目说明；主角色汇总完成".into(),
                }]
            }
        } else {
            self.entered.notify_one();
            if self.hold_children {
                std::future::pending::<()>().await;
            }
            let write = self.write_child && system.contains("（draft）");
            if calls.is_empty() {
                vec![ContentBlock::ToolUse {
                    id: "child-work".into(),
                    name: if write { "write_file" } else { "read_file" }.into(),
                    input: if write {
                        json!({"path":"team-report.txt","content":"独立子角色真实审批产物"})
                    } else {
                        json!({"path":"README.md"})
                    },
                }]
            } else {
                vec![ContentBlock::Text {
                    text: format!(
                        "已完成本角色核查。证据是本子会话的真实{}结果。",
                        if write { "write_file" } else { "read_file" }
                    ),
                }]
            }
        };
        let stop_reason = if content
            .iter()
            .any(|c| matches!(c, ContentBlock::ToolUse { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(CompletionResponse {
            content,
            stop_reason,
            usage: Usage {
                input_tokens: 30,
                output_tokens: 20,
                ..Default::default()
            },
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn default_model(&self) -> &str {
        "team-deterministic"
    }
    fn name(&self) -> &str {
        "team-fixture"
    }
}
fn team_scope(conversation: &str) -> ClientScope {
    ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: conversation.into(),
    }
}
fn team_config(base: &std::path::Path) -> HostConfig {
    let mut cfg = config(base);
    cfg.tools = vec!["read_file".into(), "write_file".into()];
    cfg.agents = Some(AgentsConfig {
        enabled: true,
        ..Default::default()
    });
    std::fs::write(
        cfg.roots["project"].join("README.md"),
        "可信项目说明：复用原引擎与会话账本",
    )
    .unwrap();
    cfg
}
fn delegated(home: &std::path::Path, parent: &str) -> Vec<serde_json::Value> {
    let db = rusqlite::Connection::open(home.join("engine-host/host.db")).unwrap();
    let mut q=db.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation' AND json_extract(reference,'$.run_id')=?1 ORDER BY revision").unwrap();
    q.query_map([parent], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}
#[tokio::test]
async fn p04_five_real_children_have_distinct_runs_sessions_requests_and_original_ledgers() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = team_config(tmp.path());
    let provider = Arc::new(TeamProvider::new(false, false));
    let host = Host::with_provider(cfg.clone(), provider.clone())
        .await
        .unwrap();
    let scope = team_scope("five");
    let b = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope, &b.session_id, "王明，请安排五位同事实际协作".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    let final_run = host.get_run(&scope, &run.run_id).await.unwrap();
    assert_eq!(
        final_run.state,
        RunState::Completed,
        "{:?}",
        final_run.error
    );
    let children = delegated(&cfg.home, &run.run_id);
    assert_eq!(children.len(), 5);
    let mut sessions = std::collections::BTreeSet::new();
    let mut runs = std::collections::BTreeSet::new();
    for child in &children {
        let child_scope: ClientScope =
            serde_json::from_value(child["child_scope"].clone()).unwrap();
        let id = child["child_run"].as_str().unwrap();
        let sid = child["child_session"].as_str().unwrap();
        assert!(sessions.insert(sid.to_owned()));
        assert!(runs.insert(id.to_owned()));
        assert_ne!(sid, b.session_id);
        let r = host.get_run(&child_scope, id).await.unwrap();
        assert_eq!(r.state, RunState::Completed);
        assert!(r
            .tools
            .iter()
            .any(|t| t.tool_name == "read_file" && t.state == "completed"));
        let s = host.get_session(&child_scope, sid).await.unwrap();
        assert!(!s.messages.is_empty());
        assert_eq!(
            host.get_run(&scope, id).await.unwrap_err().kind,
            "scope_denied"
        );
    }
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        16,
        "主6次+五子各2次，不能用单模型角色对白替代"
    );
    assert_eq!(
        host.snapshot_and_subscribe(&scope)
            .await
            .unwrap()
            .0
            .runs
            .len(),
        1,
        "子会话不污染Office会话历史"
    );
    host.delete_session(&scope).await.unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let left: u64 = db
        .query_row("SELECT COUNT(*) FROM host_bindings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0, "父删除必须清掉所有子账本/绑定");
    let left: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM host_events WHERE kind='agent_delegation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    assert!(cfg.roots["project"].join("README.md").is_file());
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn p06_child_write_uses_exact_parent_visible_approval_and_other_conversation_cannot_approve()
{
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = team_config(tmp.path());
    cfg.agents
        .as_mut()
        .unwrap()
        .tools
        .insert("draft".into(), vec!["write_file".into()]);
    let host = Host::with_provider(cfg.clone(), Arc::new(TeamProvider::new(false, true)))
        .await
        .unwrap();
    let scope = team_scope("approval");
    let b = host.open_session(scope.clone()).await.unwrap();
    let parent = host
        .submit_run(&scope, &b.session_id, "真实协作并保存子任务产物".into())
        .await
        .unwrap();
    let approval = pending(&host, &scope).await;
    let path = cfg.roots["project"].join("team-report.txt");
    assert!(!path.exists());
    assert_ne!(
        approval.binding.run_id, parent.run_id,
        "审批保留准确子Run绑定"
    );
    let other = team_scope("other");
    host.open_session(other.clone()).await.unwrap();
    assert_eq!(
        host.decide_approval(
            &other,
            &approval.approval_id,
            approval.revision,
            approval.binding.clone(),
            true
        )
        .await
        .unwrap_err()
        .kind,
        "scope_denied"
    );
    host.decide_approval(
        &scope,
        &approval.approval_id,
        approval.revision,
        approval.binding,
        true,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "独立子角色真实审批产物"
    );
    assert_eq!(
        host.get_run(&scope, &parent.run_id).await.unwrap().state,
        RunState::Completed
    );
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn p07_cancel_and_capacity_reservations_do_not_orphan_real_children() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = team_config(tmp.path());
    let provider = Arc::new(TeamProvider::new(true, false));
    let host = Host::with_provider(cfg, provider.clone()).await.unwrap();
    let a = team_scope("a");
    let b = team_scope("b");
    let c = team_scope("c");
    let ab = host.open_session(a.clone()).await.unwrap();
    let bb = host.open_session(b.clone()).await.unwrap();
    let cb = host.open_session(c.clone()).await.unwrap();
    let ar = host
        .submit_run(&a, &ab.session_id, "协作A".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), provider.entered.notified())
        .await
        .unwrap();
    let br = host
        .submit_run(&b, &bb.session_id, "协作B".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), provider.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        host.submit_run(&c, &cb.session_id, "协作C".into())
            .await
            .unwrap_err()
            .kind,
        "capacity_exceeded"
    );
    host.cancel_run(&a, &ar.run_id).await.unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let v = host.get_run(&a, &ar.run_id).await.unwrap();
            if v.state.is_terminal() {
                break v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal.state, RunState::Cancelled);
    assert!(
        !host
            .get_run(&b, &br.run_id)
            .await
            .unwrap()
            .state
            .is_terminal(),
        "取消A不应取消B"
    );
    host.shutdown().await.unwrap();
    let rows = delegated(&host_config_home(tmp.path()), &br.run_id);
    assert_eq!(rows.len(), 1);
}
fn host_config_home(base: &std::path::Path) -> std::path::PathBuf {
    base.join("home")
}
#[test]
fn p04_agent_configuration_rejects_overrides_and_preserves_old_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = team_config(tmp.path());
    cfg.validate().unwrap();
    cfg.max_active_runs = 1;
    assert!(cfg.validate().is_err());
    cfg.max_active_runs = 4;
    for role in ["unknown", "draft"] {
        cfg.agents
            .as_mut()
            .unwrap()
            .tools
            .insert(role.into(), vec!["bash".into()]);
        assert!(cfg.validate().is_err());
        cfg.agents.as_mut().unwrap().tools.clear();
    }
    let mut value = serde_json::to_value(&cfg).unwrap();
    value["agents"]["provider"] = json!("override");
    assert!(serde_json::from_value::<HostConfig>(value).is_err());
}
