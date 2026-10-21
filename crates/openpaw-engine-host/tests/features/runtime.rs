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
    let host = Host::with_provider(
        config(base.path()),
        Arc::new(MockProvider::stream_text("hello", 5)),
    )
    .await
    .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let (first, rx) = host.snapshot_and_subscribe(&s).await.unwrap();
    drop(rx);
    let run = host
        .submit_run(&s, &b.session_id, "input".into())
        .await
        .unwrap();
    host.wait_idle().await;
    let (last, _) = host.snapshot_and_subscribe(&s).await.unwrap();
    assert!(last.revision > first.revision);
    assert_eq!(last.runs[0].run_id, run.run_id);
    assert_eq!(last.runs[0].state, RunState::Completed);
    assert_eq!(last.runs[0].text, "hello");
    host.shutdown().await.unwrap();
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
