//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
use async_trait::async_trait;
use openpaw_core::{Capabilities, CompletionRequest, CompletionResponse, LlmProvider, StreamEvent};
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use serde_json::{json, Value};
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
        auto_approve_tools: Vec::new(),
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

#[tokio::test]
async fn office_hundred_by_hundred_bindings_keep_identity_and_reject_overflow_without_side_effects()
{
    fn tree(path: &std::path::Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let name = std::path::PathBuf::from(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                entries.push((name.clone(), None));
                entries.extend(
                    tree(&entry.path())
                        .into_iter()
                        .map(|(p, bytes)| (name.join(p), bytes)),
                );
            } else {
                entries.push((name, Some(std::fs::read(entry.path()).unwrap())));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(temp.path());
    std::fs::write(
        cfg.roots["project"].join("原有资料.txt"),
        "不得因创建空会话而修改",
    )
    .unwrap();
    cfg.slots = (0..100)
        .map(|i| (format!("office-{i:03}"), "project".to_owned()))
        .collect();
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("未调用")))
        .await
        .unwrap();
    // Host启动本来就创建.openpaw资源锁目录；核对其后每个目录/文件字节，不能误当原根为空。
    let before = tree(&cfg.roots["project"]);
    let root = cfg.roots["project"].clone();
    let started = std::time::Instant::now();
    let mut first_in_each = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for office in 0..100 {
        for conversation in 0..100 {
            let s = ClientScope {
                namespace: "office".into(),
                slot_id: format!("office-{office:03}"),
                conversation_id: format!("conversation-{conversation:03}"),
            };
            let binding = host.open_session(s.clone()).await.unwrap();
            assert_eq!(binding.scope, s);
            assert_eq!(binding.root_id, "project");
            assert!(
                ids.insert(binding.session_id.clone()),
                "不同办公室同名会话不能串线"
            );
            if conversation == 0 {
                first_in_each.push(binding);
            }
        }
        let overflow = ClientScope {
            namespace: "office".into(),
            slot_id: format!("office-{office:03}"),
            conversation_id: "overflow".into(),
        };
        assert_eq!(
            host.open_session(overflow).await.unwrap_err().kind,
            "capacity_exceeded"
        );
        if office % 20 == 19 {
            eprintln!("V08空绑定进度：{}办公室，各100会话", office + 1);
        }
    }
    assert_eq!(ids.len(), 10_000);
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let count = |table: &str| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    assert_eq!(count("host_bindings"), 10_000);
    assert_eq!(count("host_runs"), 0, "打开空会话不能伪造Run或调用Provider");
    assert_eq!(count("host_approvals"), 0);
    assert_eq!(
        tree(&root),
        before,
        "会话接受/超限不能改变任何工作区文件或既有资源目录"
    );
    drop(db);
    host.shutdown().await.unwrap();
    let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("仍未调用")))
        .await
        .unwrap();
    for binding in first_in_each {
        assert_eq!(
            host.open_session(binding.scope.clone())
                .await
                .unwrap()
                .session_id,
            binding.session_id
        );
    }
    host.shutdown().await.unwrap();
    assert_eq!(
        tree(&root),
        before,
        "重启和幂等查找不能新增或修改工作区内容"
    );
    eprintln!("100办公室×100真实空Session绑定、每间第101个拒绝、重启身份不变通过，耗时{:?}；不冒充10000个模型Run/网页任务容量验收", started.elapsed());
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
async fn d153_trusted_file_policy_is_audited_persistent_and_revocable() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    cfg.tools.push("edit_file".into());
    cfg.auto_approve_tools = vec!["write_file".into(), "edit_file".into()];
    let root = cfg.roots["project"].clone();
    let modes = vec![
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"report.txt","content":"revision-one"}),
            text: "save".into(),
        },
        MockMode::ToolUse {
            tool_name: "edit_file".into(),
            input: json!({"path":"report.txt","old_string":"one","new_string":"two"}),
            text: "revise".into(),
        },
        MockMode::ToolUse {
            tool_name: "edit_file".into(),
            input: json!({"path":"report.txt","old_string":"two","new_string":"three"}),
            text: "revise".into(),
        },
        MockMode::Fixed("done".into()),
    ];
    let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::scripted(modes)))
        .await
        .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "连续修订普通任务".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    let view = host.get_run(&s, &run.run_id).await.unwrap();
    assert_eq!(view.state, RunState::Completed, "{:?}", view.error);
    assert_eq!(view.tools.len(), 3);
    assert!(view.tools.iter().all(|t| t.state == "completed"));
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "revision-three"
    );
    assert!(host.pending_approvals(&s).await.unwrap().is_empty());
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let approvals: Vec<ApprovalView> = db
        .prepare("SELECT data FROM host_approvals WHERE run=?1")
        .unwrap()
        .query_map([&run.run_id], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect();
    assert_eq!(approvals.len(), 3);
    assert!(approvals
        .iter()
        .all(|a| a.status == "approved" && a.consumed));
    assert_eq!(
        approvals
            .iter()
            .map(|a| &a.binding.call_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    let audit: i64 = db.query_row("SELECT count(*) FROM host_events WHERE kind='approval_policy' AND json_extract(reference,'$.source')='trusted_tool_policy'", [], |r| r.get(0)).unwrap();
    assert_eq!(audit, 3);
    drop(db);
    assert!(
        host.decide_approval(
            &s,
            &approvals[0].approval_id,
            approvals[0].revision,
            approvals[0].binding.clone(),
            true
        )
        .await
        .is_err(),
        "已消费调用不能重放"
    );
    host.shutdown().await.unwrap();
    drop(host);
    // 同一持久配置重启仍生效；既有审批不会被重放。
    let host = Host::with_provider(cfg.clone(), writer()).await.unwrap();
    let second = host
        .submit_run(&s, &b.session_id, "重启后保存".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&s, &second.run_id).await.unwrap().state,
        RunState::Completed
    );
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "approved report"
    );
    host.shutdown().await.unwrap();
    drop(host);
    cfg.auto_approve_tools.clear();
    let host = Host::with_provider(cfg, writer()).await.unwrap();
    host.submit_run(&s, &b.session_id, "撤回预授权后保存".into())
        .await
        .unwrap();
    let a = pending(&host, &s).await;
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "approved report"
    );
    host.decide_approval(&s, &a.approval_id, a.revision, a.binding, false)
        .await
        .unwrap();
    host.wait_idle().await;
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "approved report"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn d153_unlisted_tool_and_cancelled_resource_wait_stay_restricted() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    cfg.tools.push("edit_file".into());
    cfg.auto_approve_tools = vec!["write_file".into()];
    let root = cfg.roots["project"].clone();
    std::fs::write(root.join("report.txt"), "before").unwrap();
    let host = Host::with_provider(
        cfg.clone(),
        Arc::new(MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"report.txt","old_string":"before","new_string":"forbidden"}),
                text: "edit".into(),
            },
            MockMode::Fixed("done".into()),
        ])),
    )
    .await
    .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    host.submit_run(&s, &b.session_id, "未授权的工具".into())
        .await
        .unwrap();
    let a = pending(&host, &s).await;
    assert_eq!(a.binding.tool_name, "edit_file");
    host.decide_approval(&s, &a.approval_id, a.revision, a.binding, false)
        .await
        .unwrap();
    host.wait_idle().await;
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "before"
    );
    host.shutdown().await.unwrap();
    drop(host);
    let host = Host::with_provider(cfg, writer()).await.unwrap();
    let lock = openpaw_sandbox::file_lock::try_lock_exclusive(
        &root.join(".openpaw/engine-host-resource.lock"),
    )
    .unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "资源等待期间停止".into())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    host.cancel_run(&s, &run.run_id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), host.wait_idle())
        .await
        .unwrap();
    drop(lock);
    assert_eq!(
        host.get_run(&s, &run.run_id).await.unwrap().state,
        RunState::Cancelled
    );
    assert_eq!(
        std::fs::read_to_string(root.join("report.txt")).unwrap(),
        "before"
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn d153_trusted_write_never_grants_outside_root_access() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    cfg.auto_approve_tools = vec!["write_file".into()];
    let outside = base.path().join("outside.txt");
    let host = Host::with_provider(
        cfg,
        Arc::new(MockProvider::scripted(vec![
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"../outside.txt","content":"must not write"}),
                text: "write".into(),
            },
            MockMode::Fixed("done".into()),
        ])),
    )
    .await
    .unwrap();
    let s = scope(1);
    let b = host.open_session(s.clone()).await.unwrap();
    let run = host
        .submit_run(&s, &b.session_id, "越界写入必须拒绝".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
        .await
        .unwrap();
    assert!(!outside.exists());
    assert!(host.pending_approvals(&s).await.unwrap().is_empty());
    let view = host.get_run(&s, &run.run_id).await.unwrap();
    assert!(view.tools.is_empty() || view.tools.iter().all(|t| t.state != "completed"));
    host.shutdown().await.unwrap();
}

#[test]
fn d153_trusted_policy_config_rejects_unknown_duplicate_and_disabled_tools() {
    let base = tempfile::tempdir().unwrap();
    let mut cfg = config(base.path());
    for names in [
        vec!["bash"],
        vec!["write_file", "write_file"],
        vec!["render_visual"],
        vec!["invented_tool"],
    ] {
        cfg.auto_approve_tools = names.into_iter().map(str::to_owned).collect();
        assert_eq!(cfg.validate().unwrap_err().kind, "invalid_input");
    }
    cfg.auto_approve_tools = vec!["write_file".into()];
    cfg.validate().unwrap();
    let value = serde_json::to_value(&cfg).unwrap();
    let mut old = value.clone();
    old.as_object_mut().unwrap().remove("auto_approve_tools");
    assert!(serde_json::from_value::<HostConfig>(old)
        .unwrap()
        .auto_approve_tools
        .is_empty());
    assert_eq!(
        serde_json::from_value::<HostConfig>(value)
            .unwrap()
            .auto_approve_tools,
        cfg.auto_approve_tools
    );
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
async fn d102_actual_request_matches_whole_file_and_patch_capabilities() {
    for tools in [
        vec!["read_file"],
        vec!["read_file", "write_file"],
        vec!["read_file", "edit_file"],
        vec!["read_file", "write_file", "edit_file"],
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = config(tmp.path());
        cfg.tools = tools.iter().map(|s| (*s).to_owned()).collect();
        if tools.len() == 3 {
            cfg.auto_approve_tools = vec!["write_file".into(), "edit_file".into()];
        }
        let capture = Arc::new(ContextProvider(std::sync::Mutex::new(Vec::new())));
        let host = Host::with_provider(cfg, capture.clone()).await.unwrap();
        let s = scope(1);
        let b = host.open_session(s.clone()).await.unwrap();
        host.submit_run(&s, &b.session_id, "继续完善已存在的长报告并保存".into())
            .await
            .unwrap();
        host.wait_idle().await;
        {
            let requests = capture.0.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let r = &requests[0];
            assert_eq!(
                r.tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>(),
                tools.iter().copied().collect(),
                "上下文不能偷偷添加写入/编辑工具"
            );
            let context = r.system.as_deref().unwrap();
            assert_eq!(
                context.contains("本轮可信文件工具预授权：write_file, edit_file"),
                tools.len() == 3
            );
            if tools.len() == 3 {
                assert!(context.contains("不要求用户再次点击或在聊天批准"));
            }
            assert_eq!(
                context.contains("仅有整文件写入能力"),
                tools.contains(&"write_file") && !tools.contains(&"edit_file")
            );
            assert_eq!(
                context.contains("使用现有edit_file做有界精确替换"),
                tools.contains(&"edit_file")
            );
            let can_revise = tools.contains(&"write_file") || tools.contains(&"edit_file");
            assert_eq!(context.contains("先汇总本轮有依据的修订意见"), can_revise);
            if can_revise {
                assert!(context.contains("同一变更不得换表述重新申请"));
                assert!(context.contains("审核结论不能代替事实依据"));
            }
            if tools.contains(&"write_file") && !tools.contains(&"edit_file") {
                assert!(context.contains("独立章节文件与入口索引"));
                assert!(context.contains("不要重写整篇"));
            }
        }
        host.shutdown().await.unwrap();
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
        let host = Host::with_provider(cfg.clone(), provider.clone())
            .await
            .unwrap();
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
    replay_first: Option<bool>,
    entered: tokio::sync::Notify,
}
impl TeamProvider {
    fn new(hold_children: bool, write_child: bool) -> Self {
        Self {
            requests: std::sync::Mutex::new(Vec::new()),
            hold_children,
            write_child,
            replay_first: None,
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
            let raw = calls.iter().filter(|c| c.starts_with("delegate-")).count();
            let n = raw - usize::from(self.replay_first.is_some() && raw > 1);
            if self.write_child && !calls.contains(&"team-plan") {
                vec![ContentBlock::ToolUse {
                    id: "team-plan".into(),
                    name: "report_progress".into(),
                    input: json!({"type":"create","steps":[{"id":"save","title":"保存 team-report.txt"}]}),
                }]
            } else if self.write_child && n == roles.len() && !calls.contains(&"team-confirm") {
                vec![ContentBlock::ToolUse {
                    id: "team-confirm".into(),
                    name: "report_progress".into(),
                    input: json!({"type":"update","stepId":"save","state":"completed","evidenceCallIds":["delegate-draft"]}),
                }]
            } else if n < roles.len() {
                let repeat = self.replay_first.is_some() && raw == 1;
                let role = roles[if repeat { 0 } else { n }];
                let title = if repeat && self.replay_first == Some(true) {
                    "不同的委派目标".into()
                } else {
                    format!("{role}核对项目说明")
                };
                vec![ContentBlock::ToolUse {
                    id: format!("delegate-{role}"),
                    name: "delegate_task".into(),
                    input: json!({"role_id":role,"title":title,"instructions":"读取README.md并核对项目背景，明确真实证据和限制","acceptance":"有实际文件读取和角色独立结果"}),
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

struct ContinuousFixture {
    lead_calls: std::sync::atomic::AtomicUsize,
    children: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    barrier: tokio::sync::Barrier,
    finish: bool,
}
#[async_trait]
impl LlmProvider for ContinuousFixture {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> openpaw_core::Result<CompletionResponse> {
        use openpaw_core::{ContentBlock, StopReason, Usage};
        use std::sync::atomic::Ordering::SeqCst;
        if request.tools.is_empty() {
            return Ok(CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: "已处理首批资料并保存产物，继续后续批次；保留原始目标与下一步检查点。"
                        .into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 20,
                    ..Default::default()
                },
            });
        }
        let lead = request
            .system
            .as_deref()
            .is_some_and(|s| s.contains("（lead）"));
        let text = |s: String| vec![ContentBlock::Text { text: s }];
        let tool = |id: &str, name: &str, input: serde_json::Value| {
            vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }]
        };
        let content = if !lead {
            let active = self.children.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(active, SeqCst);
            tokio::time::timeout(Duration::from_secs(5), self.barrier.wait())
                .await
                .expect("五子Run必须真实同时进入Provider，串行或容量不足应使回归失败");
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.children.fetch_sub(1, SeqCst);
            text("独立成员已实际完成本次核查".into())
        } else {
            let n = self.lead_calls.fetch_add(1, SeqCst);
            if self.finish && n == 4 {
                let dynamic = &request
                    .system_blocks
                    .as_ref()
                    .expect("接续阶段必须保留原动态系统块")
                    .dynamic;
                assert!(dynamic.contains("Host已经自动接续") && dynamic.contains("当前UTC时间"));
                assert_eq!(
                    request
                        .messages
                        .iter()
                        .filter(|m| m.role == openpaw_core::Role::User
                            && m.content
                                .iter()
                                .any(|c| matches!(c, ContentBlock::Text { .. })))
                        .count(),
                    1,
                    "接续不能伪造新用户输入"
                );
            }
            if n == 0 {
                let mut input = json!({"action":"start","goal":"持续处理资料直到停止","summary":"启动连续协作","next":"安排独立任务"});
                if self.finish {
                    input["completion_condition"] = json!("验收三批真实工作后停止");
                }
                tool("continuous-start", "continuous_task", input)
            } else if !self.finish {
                if n == 1 {
                    tool(
                        "forbidden-finish",
                        "continuous_task",
                        json!({"action":"finish","summary":"模型尝试在用户停止前自行结束，必须被拒绝"}),
                    )
                } else {
                    text(format!("第{n}阶段已完成，持续目标尚未停止"))
                }
            } else {
                match n {
                    1|9|10=>["research","organize","draft","review","audit"].into_iter().map(|role|ContentBlock::ToolUse{id:format!("batch-{n}-{role}"),name:"delegate_task".into(),input:json!({"role_id":role,"title":format!("{role}第{n}批资料核查"),"instructions":"只处理当前给定资料，不写文件","acceptance":"返回本角色准确结果"})}).collect(),
                    2|7=>tool(&format!("checkpoint-{n}"),"continuous_task",json!({"action":"checkpoint","summary":format!("已核验第{n}批真实成果"),"next":"继续处理后续独立工作"})),
                    3=>text("本阶段结束，但仍应自动推进下一阶段".into()),
                    4=>tool("auto-write","write_file",json!({"path":"outputs/result.md","content":"自动批准的真实产物"})),
                    5=>tool("outside-write","write_file",json!({"path":"outside.md","content":"越界写入必须仍等待审批"})),
                    6=>tool("auto-svg","render_visual",json!({"path":"outputs/flow.svg","kind":"diagram","title":"实际图示","nodes":[{"id":"a","label":"核查"},{"id":"b","label":"交付"}],"edges":[{"from":"a","to":"b"}]})),
                    8=>text("长正文不应直接拼进持续聊天摘要".repeat(5000)),
                    11=>tool("continuous-finish","continuous_task",json!({"action":"finish","summary":"完成验收指定的三批工作，正常关闭持续任务"})),
                    _=>text("所有验收工作已完成".into()),
                }
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
    fn name(&self) -> &str {
        "continuous-contract"
    }
    fn default_model(&self) -> &str {
        "continuous-contract"
    }
}
fn continuous_config(base: &std::path::Path) -> HostConfig {
    let mut cfg = team_config(base);
    cfg.run_timeout_secs = 0;
    cfg.max_active_runs = 6;
    cfg.tools.push("render_visual".into());
    let agents = cfg.agents.as_mut().unwrap();
    agents.max_active_children = 5;
    agents.max_delegations = 0;
    agents.max_model_requests = 0;
    agents.max_output_tokens = 0;
    agents.continuous = Some(ContinuousConfig {
        enabled: true,
        stage_pause_ms: 1,
        ..Default::default()
    });
    cfg
}
fn continuous_fixture(finish: bool) -> Arc<ContinuousFixture> {
    Arc::new(ContinuousFixture {
        lead_calls: std::sync::atomic::AtomicUsize::new(0),
        children: std::sync::atomic::AtomicUsize::new(0),
        peak: std::sync::atomic::AtomicUsize::new(0),
        barrier: tokio::sync::Barrier::new(5),
        finish,
    })
}

#[tokio::test]
async fn d116_actual_continuous_request_explains_fifo_and_exact_source_identifiers() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = continuous_config(tmp.path());
    let capture = Arc::new(ContextProvider(std::sync::Mutex::new(Vec::new())));
    let host = Host::with_provider(cfg, capture.clone()).await.unwrap();
    let s = team_scope("d116-guidance");
    let b = host.open_session(s.clone()).await.unwrap();
    host.submit_run(&s, &b.session_id, "先说明当前协作能力".into())
        .await
        .unwrap();
    host.wait_idle().await;
    {
        let requests = capture.0.lock().unwrap();
        assert_eq!(requests.len(), 1, "未启动持续工具的普通问题仍只请求一次");
        let context = requests[0].system.as_deref().unwrap();
        assert!(context.contains("FIFO待办"));
        assert!(!context.contains("角色忙时不重复派发同角色"));
        assert!(!context.contains("每位成员同时只能有一个在途任务"));
        assert!(context.contains("不是child_run_id或delegate_call_id"));
        assert!(context.contains("不能删掉source_task_id"));
        assert!(context.contains("近似要求不能自行升级成精确硬阈值"));
    }
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn continuous_one_input_resumes_five_parallel_agents_over_twelve_delegations_and_scoped_writes(
) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = continuous_config(tmp.path());
    let provider = continuous_fixture(true);
    let host = Host::with_provider(cfg.clone(), provider.clone())
        .await
        .unwrap();
    let scope = team_scope("continuous-contract");
    let session = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(
            &scope,
            &session.session_id,
            "持续处理资料，使用真实多人协作".into(),
        )
        .await
        .unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let pending = host.pending_approvals(&scope).await.unwrap();
            if let Some(approval) = pending.into_iter().next() {
                break approval;
            }
            let current = host.get_run(&scope, &run.run_id).await.unwrap();
            assert!(
                !current.state.is_terminal(),
                "提前终止：{:?}",
                current.error
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(approval.binding.input["path"], "outside.md");
    assert_eq!(
        std::fs::read_to_string(cfg.roots["project"].join("outputs/result.md")).unwrap(),
        "自动批准的真实产物"
    );
    assert!(!cfg.roots["project"].join("outside.md").exists());
    host.decide_approval(
        &scope,
        &approval.approval_id,
        approval.revision,
        approval.binding,
        false,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), host.wait_idle())
        .await
        .unwrap();
    let final_run = host.get_run(&scope, &run.run_id).await.unwrap();
    assert_eq!(
        final_run.state,
        RunState::Completed,
        "{:?}",
        final_run.error
    );
    assert_eq!(provider.peak.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert_eq!(delegated(&cfg.home, &run.run_id).len(), 15);
    assert!(cfg.roots["project"].join("outputs/flow.svg").is_file());
    assert!(final_run.text.contains("第7批真实成果"));
    assert!(final_run.text.encode_utf16().count() < 50000);
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let auto: i64 = db
        .query_row(
            "SELECT count(*) FROM host_events WHERE kind='approval_policy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(auto, 2, "自动写入和媒体都要留下原审批审计");
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn continuous_can_be_cancelled_after_end_turn_and_ordinary_chat_still_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = continuous_config(tmp.path());
    let provider = continuous_fixture(false);
    let host = Host::with_provider(cfg.clone(), provider.clone())
        .await
        .unwrap();
    let scope = team_scope("continuous-cancel");
    let session = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope, &session.session_id, "持续推进直到我停止".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider
            .lead_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            < 4
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let active = host.get_run(&scope, &run.run_id).await.unwrap();
    assert!(!active.state.is_terminal(), "未获用户停止，不得自行结束");
    assert!(active
        .tools
        .iter()
        .any(|t| t.call_id == "forbidden-finish" && t.state == "failed"));
    host.cancel_run(&scope, &run.run_id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&scope, &run.run_id).await.unwrap().state,
        RunState::Cancelled
    );
    let count = provider
        .lead_calls
        .load(std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        provider
            .lead_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        count
    );
    let stopped = host.get_run(&scope, &run.run_id).await.unwrap();
    host.shutdown().await.unwrap();
    let restored = Host::with_provider(cfg, provider.clone()).await.unwrap();
    let historical = restored.get_run(&scope, &run.run_id).await.unwrap();
    assert_eq!(historical.text, stopped.text);
    assert_eq!(historical.state, RunState::Cancelled);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        provider
            .lead_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        count,
        "重启不自动接续或重放工具"
    );
    restored.shutdown().await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = continuous_config(tmp.path());
    let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("普通问答正常结束")))
        .await
        .unwrap();
    let session = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope, &session.session_id, "你好".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&scope, &run.run_id).await.unwrap().state,
        RunState::Completed
    );
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn continuous_same_role_queues_and_readonly_env_assembles_image_credentials() {
    use openpaw_core::{ContentBlock, StopReason, Usage};
    struct Queued {
        lead: std::sync::atomic::AtomicUsize,
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl LlmProvider for Queued {
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> openpaw_core::Result<CompletionResponse> {
            use std::sync::atomic::Ordering::SeqCst;
            let lead = request
                .system
                .as_deref()
                .is_some_and(|s| s.contains("（lead）"));
            let content = if lead && self.lead.fetch_add(1, SeqCst) == 0 {
                (0..3).map(|i|ContentBlock::ToolUse{id:format!("queued-{i}"),name:"delegate_task".into(),input:json!({"role_id":"research","title":format!("顺序资料{i}"),"instructions":"本次独立资料核查","acceptance":"返回实际结果"})}).collect()
            } else {
                if !lead {
                    let n = self.active.fetch_add(1, SeqCst) + 1;
                    self.peak.fetch_max(n, SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    self.active.fetch_sub(1, SeqCst);
                }
                vec![ContentBlock::Text {
                    text: "已完成本次核查".into(),
                }]
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
                usage: Usage::default(),
            })
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn name(&self) -> &str {
            "queued-role"
        }
        fn default_model(&self) -> &str {
            "queued-role"
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = continuous_config(tmp.path());
    cfg.agents.as_mut().unwrap().images = Some(openpaw_tools::media::ImageConfig {
        base_url: "http://127.0.0.1:23456/v1".into(),
        model: "isolated-image-model".into(),
        api_key_env: "OPENPAW_READONLY_IMAGE_FIXTURE".into(),
        response_format: None,
    });
    let env_file = tmp.path().join("models.env");
    std::fs::write(
        &env_file,
        "OPENPAW_READONLY_IMAGE_FIXTURE=fixture-image-only\n",
    )
    .unwrap();
    let settings =
        openpaw_llm::model_config::ModelEnvironment::load(tmp.path(), Some(&env_file)).unwrap();
    assert!(std::env::var("OPENPAW_READONLY_IMAGE_FIXTURE").is_err());
    // Process-only assembly must not silently reuse a chat key or fall back to a provider.
    assert_eq!(
        Host::with_provider(cfg.clone(), Arc::new(MockProvider::fixed("unused")))
            .await
            .err()
            .unwrap()
            .kind,
        "invalid_input"
    );
    let provider = Arc::new(Queued {
        lead: 0.into(),
        active: 0.into(),
        peak: 0.into(),
    });
    let host = Host::with_provider_and_environment(cfg.clone(), provider.clone(), &settings)
        .await
        .unwrap();
    assert!(std::env::var("OPENPAW_READONLY_IMAGE_FIXTURE").is_err());
    let scope = team_scope("queued-role");
    let session = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(
            &scope,
            &session.session_id,
            "给同一个成员依次处理三个独立小任务".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    let current = host.get_run(&scope, &run.run_id).await.unwrap();
    assert_eq!(current.state, RunState::Completed, "{:?}", current.error);
    assert_eq!(provider.peak.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(delegated(&cfg.home, &run.run_id).len(), 3);
    host.shutdown().await.unwrap();
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
async fn d90_empty_child_fails_and_explicit_retry_preserves_role_and_budget_rules() {
    use openpaw_core::{ContentBlock, StopReason, Usage};
    struct EmptyThenRetry {
        children: std::sync::atomic::AtomicUsize,
        requests: std::sync::Mutex<Vec<CompletionRequest>>,
    }
    #[async_trait]
    impl LlmProvider for EmptyThenRetry {
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> openpaw_core::Result<CompletionResponse> {
            self.requests.lock().unwrap().push(request.clone());
            let lead = request
                .system
                .as_deref()
                .is_some_and(|s| s.contains("（lead）"));
            let n = request
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                .count();
            let (content, stop_reason) = if lead && n < 3 {
                (
                    vec![ContentBlock::ToolUse {
                        id: format!("audit-attempt-{n}"),
                        name: "delegate_task".into(),
                        input: json!({"role_id":"audit","title":"赵审核对路线","instructions":"用三句核对给定路线，不联网","acceptance":"有实际正文"}),
                    }],
                    StopReason::ToolUse,
                )
            } else if lead {
                (
                    vec![ContentBlock::Text {
                        text: "已取得赵审两次真实复核正文".into(),
                    }],
                    StopReason::EndTurn,
                )
            } else if self
                .children
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                (vec![], StopReason::MaxTokens)
            } else {
                (
                    vec![ContentBlock::Text {
                        text: format!(
                            "明确给出实际核验结果{}尾部完整标记",
                            "可核验正文".repeat(1500)
                        ),
                    }],
                    StopReason::EndTurn,
                )
            };
            Ok(CompletionResponse {
                content,
                stop_reason,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 4096,
                    ..Default::default()
                },
            })
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn default_model(&self) -> &str {
            "retry-fixture"
        }
        fn name(&self) -> &str {
            "retry-fixture"
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = team_config(tmp.path());
    let provider = Arc::new(EmptyThenRetry {
        children: std::sync::atomic::AtomicUsize::new(0),
        requests: std::sync::Mutex::new(vec![]),
    });
    let host = Host::with_provider(cfg.clone(), provider.clone())
        .await
        .unwrap();
    let scope = team_scope("empty-retry");
    let session = host.open_session(scope.clone()).await.unwrap();
    let parent = host
        .submit_run(
            &scope,
            &session.session_id,
            "委派赵审，空结果后明确重试".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    let final_run = host.get_run(&scope, &parent.run_id).await.unwrap();
    assert_eq!(final_run.state, RunState::Completed);
    let delegations = delegated(&cfg.home, &parent.run_id);
    assert_eq!(
        delegations.len(),
        3,
        "失败可显式重试，成功后可顺序分配不同任务"
    );
    for (i, d) in delegations.iter().enumerate() {
        let run = host
            .get_run(
                &serde_json::from_value(d["child_scope"].clone()).unwrap(),
                d["child_run"].as_str().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            run.state,
            if i == 0 {
                RunState::Failed
            } else {
                RunState::Completed
            }
        );
        if i == 0 {
            assert_eq!(run.error.unwrap().kind, "provider_output_limit");
        } else {
            assert!(run.text.contains("实际核验结果"));
        }
    }
    {
        let requests = provider.requests.lock().unwrap();
        let results: Vec<_> = requests
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    content,
                    ..
                } = b
                {
                    Some((tool_use_id.as_str(), *is_error, content.as_str()))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            results.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>(),
            vec![
                ("audit-attempt-0", true),
                ("audit-attempt-1", false),
                ("audit-attempt-2", false)
            ]
        );
        assert!(results[0].2.contains("输出额度"));
        assert!(results[1].2.contains("尾部完整标记"));
        assert!(results[2].2.contains("尾部完整标记"));
        assert_eq!(
            provider.children.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    }
    host.shutdown().await.unwrap();
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
    for request in provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.system.as_deref().is_some_and(|s| s.contains("（lead）")))
    {
        assert_eq!(
            request.max_tokens,
            provider.capabilities().max_output_tokens.min(8192)
        );
        let system = request.system.as_ref().unwrap();
        assert!(
            system.contains("子角色看不到父会话历史") && system.contains("context"),
            "真实Provider必须获知独立子角色的资料交接方式，不能让审查者凭空核验前序结果"
        );
        assert!(
            system.contains("不要把草稿和依赖它的审查放在同一批工具调用里")
                && system.contains("已有前序产物时，review和audit必须实际检查该产物")
                && system.contains("引用实际返回的原句"),
            "主角色必须按真实结果的依赖顺序交接，前期验收清单不能代替对稿核验"
        );
        for mapping in [
            "王明 -> lead",
            "李研 -> research",
            "周理 -> organize",
            "陈书 -> draft",
            "刘市 -> review",
            "赵审 -> audit",
        ] {
            assert!(
                system.contains(mapping),
                "主角色的实际Provider请求必须收到准确人物映射：{mapping}"
            );
        }
        assert!(
            system.contains("本轮各子角色实际工具白名单")
                && system.contains("赵审（audit）：read_file")
                && system.contains(
                    "需要新建或修改文件的任务只能交给白名单含write_file或edit_file的角色"
                ),
            "主角色必须看到每个子角色的实际工具能力，避免把写入任务派给只读角色"
        );
    }
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
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    let plan:String=db.query_row("SELECT reference FROM host_events WHERE kind='run_progress' AND json_extract(reference,'$.run_id')=?1 ORDER BY revision DESC LIMIT 1",[&parent.run_id],|r|r.get(0)).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(
        plan["steps"][0]["state"], "completed",
        "父步骤只能凭准确委派及真实子文件成功确认"
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

#[tokio::test]
async fn p04_repeated_delegate_reuses_child_and_changed_input_is_rejected() {
    for changed in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = team_config(tmp.path());
        let mut provider = TeamProvider::new(false, false);
        provider.replay_first = Some(changed);
        let provider = Arc::new(provider);
        let host = Host::with_provider(cfg.clone(), provider.clone())
            .await
            .unwrap();
        let scope = team_scope("repeated");
        let session = host.open_session(scope.clone()).await.unwrap();
        let parent = host
            .submit_run(&scope, &session.session_id, "测试真实委派重试".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let children = delegated(&cfg.home, &parent.run_id);
        assert_eq!(children.len(), 5, "同call不得再建子Session/Run");
        let requests = provider.requests.lock().unwrap().clone();
        let errors: Vec<_> = requests
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|c| match c {
                openpaw_core::ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } if tool_use_id == "delegate-research" => Some((content.clone(), *is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 2);
        assert!(!errors[0].1);
        assert_eq!(errors[1].1, changed);
        if changed {
            assert!(errors[1].0.contains("不能替换"));
        }
        drop(requests);
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn p04_group_limits_and_deadline_are_shared_by_real_children() {
    for mode in ["limit", "budget", "deadline"] {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = team_config(tmp.path());
        let agents = cfg.agents.as_mut().unwrap();
        match mode {
            "limit" => agents.max_delegations = 2,
            "budget" => agents.max_model_requests = 1,
            "deadline" => cfg.run_timeout_secs = 1,
            _ => unreachable!(),
        }
        let host = Host::with_provider(
            cfg.clone(),
            Arc::new(TeamProvider::new(mode == "deadline", false)),
        )
        .await
        .unwrap();
        let scope = team_scope(mode);
        let session = host.open_session(scope.clone()).await.unwrap();
        let parent = host
            .submit_run(&scope, &session.session_id, "实际协作限制".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let final_run = host.get_run(&scope, &parent.run_id).await.unwrap();
        let children = delegated(&cfg.home, &parent.run_id);
        assert_eq!(children.len(), if mode == "limit" { 2 } else { 1 });
        for child in &children {
            let scope: ClientScope = serde_json::from_value(child["child_scope"].clone()).unwrap();
            let run = host
                .get_run(&scope, child["child_run"].as_str().unwrap())
                .await
                .unwrap();
            assert!(run.state.is_terminal());
            if mode != "limit" {
                assert_ne!(run.state, RunState::Completed);
            }
        }
        if mode == "deadline" {
            assert_eq!(final_run.state, RunState::TimedOut);
        }
        if mode == "budget" {
            assert_eq!(final_run.state, RunState::Failed);
        }
        if mode == "limit" {
            assert!(final_run
                .tools
                .iter()
                .any(|t| t.tool_name == "delegate_task" && t.state == "failed"));
        }
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn p07_restart_validates_agent_versions_and_never_replays_children() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = team_config(tmp.path());
    let provider = Arc::new(TeamProvider::new(false, false));
    let host = Host::with_provider(cfg.clone(), provider.clone())
        .await
        .unwrap();
    let scope = team_scope("versions");
    let session = host.open_session(scope.clone()).await.unwrap();
    host.submit_run(&scope, &session.session_id, "真实协作恢复".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    let before = provider.requests.lock().unwrap().len();
    host.shutdown().await.unwrap();
    drop(host);
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    for kind in ["agent_group", "agent_delegation", "agent_budget"] {
        db.execute(
            "UPDATE host_events SET reference=json_set(reference,'$.version',2) WHERE kind=?1",
            [kind],
        )
        .unwrap();
        match Host::with_provider(cfg.clone(), provider.clone()).await {
            Ok(h) => {
                h.shutdown().await.unwrap();
                panic!("未知{kind}版本不得静默接收");
            }
            Err(e) => assert_eq!(e.kind, "storage_unavailable"),
        }
        assert_eq!(provider.requests.lock().unwrap().len(), before);
        db.execute(
            "UPDATE host_events SET reference=json_set(reference,'$.version',1) WHERE kind=?1",
            [kind],
        )
        .unwrap();
    }
    let reopened = Host::with_provider(cfg, provider.clone()).await.unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), before);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn p04_long_but_bounded_group_reaches_final_answer_and_still_honors_shared_limit() {
    for (limit, expected, expected_calls) in
        [(24, RunState::Completed, 21), (10, RunState::Failed, 10)]
    {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = team_config(tmp.path());
        let agents = cfg.agents.as_mut().unwrap();
        agents.max_model_requests = limit;
        agents.max_output_tokens = 262144;
        let mut responses = vec![
            MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: json!({"path":"README.md"}),
                text: String::new(),
            };
            20
        ];
        responses.push(MockMode::Fixed("逐次核对完成，提供最终答复".into()));
        let provider = Arc::new(MockProvider::scripted(responses));
        let host = Host::with_provider(cfg, provider.clone()).await.unwrap();
        let scope = team_scope("long-bounded-group");
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(
                &scope,
                &binding.session_id,
                "完整处理已配置额度内的逐项检查".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let result = host.get_run(&scope, &run.run_id).await.unwrap();
        assert_eq!(
            result.state,
            expected,
            "不能在已配置预算尚有额度时提前结束；实际Provider调用={}：{:?}",
            provider.call_count(),
            result.error
        );
        assert_eq!(provider.call_count(), expected_calls);
        if expected == RunState::Completed {
            assert!(result.text.contains("最终答复"));
        }
        host.shutdown().await.unwrap();
    }
}

struct AsyncPipeline {
    next: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    release: tokio::sync::Semaphore,
    waiting: tokio::sync::Notify,
    cancel_test: bool,
}
#[async_trait]
impl LlmProvider for AsyncPipeline {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> openpaw_core::Result<CompletionResponse> {
        use openpaw_core::{ContentBlock, StopReason, Usage};
        use std::sync::atomic::Ordering::SeqCst;
        let tool = |id: &str, name: &str, input| {
            vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }]
        };
        let results: std::collections::BTreeMap<_, _> = request
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|c| {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = c
                {
                    Some((tool_use_id.as_str(), (content.as_str(), *is_error)))
                } else {
                    None
                }
            })
            .collect();
        let task_id = |call: &str| -> String {
            serde_json::from_str::<serde_json::Value>(results[call].0).unwrap()["task_id"]
                .as_str()
                .unwrap()
                .into()
        };
        let lead = request
            .system
            .as_deref()
            .is_some_and(|s| s.contains("（lead）"));
        let content = if !lead {
            let child_calls = results.len();
            if child_calls == 0 {
                tool(
                    "child-plan",
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"text","title":"整理给定材料"}]}),
                )
            } else {
                let active = self.active.fetch_add(1, SeqCst) + 1;
                self.peak.fetch_max(active, SeqCst);
                self.release.acquire().await.unwrap().forget();
                self.active.fetch_sub(1, SeqCst);
                vec![ContentBlock::Text {
                    text: "真实有限文本成果；不声称已写文件".into(),
                }]
            }
        } else {
            let n = self.next.fetch_add(1, SeqCst);
            match n {
                0 => tool(
                    "start",
                    "continuous_task",
                    json!({"action":"start","goal":"两事项真实交错协作","summary":"开始","completion_condition":"完成两事项与结果核验"}),
                ),
                1 => tool(
                    "open-a",
                    "continuous_task",
                    json!({"action":"open_item","item_id":"a","summary":"开放事项A"}),
                ),
                2 => tool(
                    "open-b",
                    "continuous_task",
                    json!({"action":"open_item","item_id":"b","summary":"开放事项B"}),
                ),
                3 => tool(
                    "open-c",
                    "continuous_task",
                    json!({"action":"open_item","item_id":"c","summary":"第三事项必须拒绝"}),
                ),
                4 => {
                    assert!(
                        results["open-c"].1 && results["open-c"].0.contains("capacity_exceeded")
                    );
                    tool(
                        "plan",
                        "report_progress",
                        json!({"type":"create","steps":[{"id":"result","title":"核对交接成果"}]}),
                    )
                }
                5 | 6 => {
                    let (role, item) = if n == 5 {
                        ("organize", "a")
                    } else {
                        ("research", "b")
                    };
                    tool(
                        item,
                        "delegate_task",
                        json!({"role_id":role,"title":format!("{item}独立任务"),"instructions":"整理已给定资料，不写文件","acceptance":"实际有限文本","wait_for_completion":false,"work_item":item}),
                    )
                }
                7 => tool(
                    "premature",
                    "report_progress",
                    json!({"type":"update","stepId":"result","state":"completed","evidenceCallIds":["a"]}),
                ),
                8 => {
                    assert!(results["premature"].1, "接受回执不能完成业务步骤");
                    tool(
                        "bad-source",
                        "delegate_task",
                        json!({"role_id":"draft","title":"前序未完成必须拒绝","instructions":"只整理","acceptance":"正文","source_task_id":task_id("a"),"work_item":"a","wait_for_completion":false}),
                    )
                }
                9 => {
                    assert!(
                        results["bad-source"].1 && results["bad-source"].0.contains("前序尚未")
                    );
                    tool(
                        "close-early",
                        "continuous_task",
                        json!({"action":"close_item","item_id":"a","summary":"在途事项必须拒绝"}),
                    )
                }
                10 => {
                    assert!(results["close-early"].1);
                    tool(
                        "third",
                        "delegate_task",
                        json!({"role_id":"audit","title":"组容量排队","instructions":"有限资料核对","acceptance":"真实文本","work_item":"a","wait_for_completion":false}),
                    )
                }
                11 => {
                    // 到下一轮才证明第三项真实回执已持久接受，而非仅准备发出工具请求。
                    assert!(!results["third"].1);
                    self.waiting.notify_one();
                    tool(
                        "wait",
                        "delegation_status",
                        json!({"task_ids":[task_id("a"),task_id("b"),task_id("third")],"wait_for":"all"}),
                    )
                }
                12 => {
                    assert!(!self.cancel_test);
                    let v: serde_json::Value = serde_json::from_str(results["wait"].0).unwrap();
                    assert!(v["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|r| r["state"] == "completed"));
                    self.release.add_permits(1);
                    tool(
                        "handoff",
                        "delegate_task",
                        json!({"role_id":"draft","title":"前序负责人交接","instructions":"根据前序真实结果整理","acceptance":"有界正文","context":"真实有限文本成果；不声称已写文件","source_task_id":task_id("a"),"work_item":"a"}),
                    )
                }
                13 => tool(
                    "confirm",
                    "report_progress",
                    json!({"type":"update","stepId":"result","state":"completed","evidenceCallIds":["a","b","handoff"]}),
                ),
                14 => tool(
                    "close-a",
                    "continuous_task",
                    json!({"action":"close_item","item_id":"a","summary":"已核对事项A成果"}),
                ),
                15 => tool(
                    "close-b",
                    "continuous_task",
                    json!({"action":"close_item","item_id":"b","summary":"已核对事项B成果"}),
                ),
                16 => tool(
                    "finish",
                    "continuous_task",
                    json!({"action":"finish","summary":"两事项验收完成"}),
                ),
                _ => vec![ContentBlock::Text {
                    text: "两项有限目标已实际完成".into(),
                }],
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
    fn name(&self) -> &str {
        "async-pipeline-fixture"
    }
    fn default_model(&self) -> &str {
        "fixture"
    }
}

#[tokio::test]
async fn async_delegation_retains_real_slots_rejects_premature_evidence_and_cancels_waits() {
    use std::sync::atomic::Ordering::SeqCst;
    for cancel_test in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = continuous_config(tmp.path());
        cfg.agents.as_mut().unwrap().max_active_children = 2;
        let provider = Arc::new(AsyncPipeline {
            next: 0.into(),
            active: 0.into(),
            peak: 0.into(),
            release: tokio::sync::Semaphore::new(0),
            waiting: tokio::sync::Notify::new(),
            cancel_test,
        });
        let host = Host::with_provider(cfg.clone(), provider.clone())
            .await
            .unwrap();
        let scope = team_scope("async-pipeline");
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(
                &scope,
                &binding.session_id,
                "两事项交错执行并真实交接".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), provider.waiting.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider.active.load(SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            provider.active.load(SeqCst),
            2,
            "第三异步委派必须等待仍由子Run持有的组许可"
        );
        assert_eq!(provider.peak.load(SeqCst), 2);
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let rows:Vec<serde_json::Value>=db.prepare("SELECT reference FROM host_events WHERE kind='continuous_task' ORDER BY revision DESC LIMIT 1").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().map(|r|serde_json::from_str(&r.unwrap()).unwrap()).collect();
        assert_eq!(rows[0]["open_items"], json!(["a", "b"]));
        let queued_count: i64 = db
            .query_row(
                "SELECT count(*) FROM host_runs WHERE json_extract(data,'$.state')='accepted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            queued_count, 1,
            "第三项已可靠接受但尚未开工，不能阻塞主Agent继续调度"
        );
        if cancel_test {
            host.cancel_run(&scope, &run.run_id).await.unwrap();
        } else {
            provider.release.add_permits(3);
        }
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        assert_eq!(
            host.get_run(&scope, &run.run_id).await.unwrap().state,
            if cancel_test {
                RunState::Cancelled
            } else {
                RunState::Completed
            }
        );
        assert_eq!(provider.peak.load(SeqCst), 2);
        let ds: Vec<serde_json::Value> = db
            .prepare(
                "SELECT reference FROM host_events WHERE kind='agent_delegation' ORDER BY revision",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        assert_eq!(ds.len(), if cancel_test { 3 } else { 4 });
        if cancel_test {
            let queued = host
                .get_run(
                    &serde_json::from_value(ds[2]["child_scope"].clone()).unwrap(),
                    ds[2]["child_run"].as_str().unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                queued.state,
                RunState::Cancelled,
                "排队项取消不得执行或丢失真实终态"
            );
        }
        if !cancel_test {
            assert_eq!(ds[3]["source_task_id"], ds[0]["task_id"]);
            assert_eq!(ds[3]["work_item"], "a");
        }
        drop(db);
        host.shutdown().await.unwrap();
    }
}

struct FifoMembers {
    next: std::sync::atomic::AtomicUsize,
    started: std::sync::Mutex<Vec<String>>,
    research_active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    accepted: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
#[async_trait]
impl LlmProvider for FifoMembers {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> openpaw_core::Result<CompletionResponse> {
        use openpaw_core::{ContentBlock, StopReason, Usage};
        use std::sync::atomic::Ordering::SeqCst;
        let lead = request
            .system
            .as_deref()
            .is_some_and(|s| s.contains("（lead）"));
        let content = if lead {
            let n = self.next.fetch_add(1, SeqCst);
            if n < 4 {
                vec![ContentBlock::ToolUse {
                    id: format!("fifo-{n}"),
                    name: "delegate_task".into(),
                    input: json!({"role_id":if n<3 {"research"}else{"draft"},"title":format!("FIFO-{n}"),"instructions":format!("仅整理给定资料 FIFO-{n}"),"acceptance":"实际有限文本","wait_for_completion":false}),
                }]
            } else if n == 4 {
                let ids: Vec<Value> = request
                    .messages
                    .iter()
                    .flat_map(|m| &m.content)
                    .filter_map(|c| match c {
                        ContentBlock::ToolResult {
                            content,
                            is_error: false,
                            ..
                        } => serde_json::from_str::<Value>(content).ok().and_then(|v| {
                            let id = v.get("task_id")?.clone();
                            let index = v["title"]
                                .as_str()
                                .expect("接受回执须携真实任务标题")
                                .strip_prefix("FIFO-")
                                .unwrap()
                                .parse::<usize>()
                                .unwrap();
                            assert_eq!(v["role_id"], if index < 3 { "research" } else { "draft" });
                            assert!(v["available_tools"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|t| t == "read_file"));
                            Some(id)
                        }),
                        _ => None,
                    })
                    .collect();
                assert_eq!(ids.len(), 4, "忙成员不能阻止后续委派被可靠接受");
                vec![ContentBlock::ToolUse {
                    id: "inspect-fifo".into(),
                    name: "delegation_status".into(),
                    input: json!({"task_ids":ids,"wait_for":"none"}),
                }]
            } else if n == 5 || n == 6 {
                let result = request
                    .messages
                    .iter()
                    .flat_map(|m| &m.content)
                    .rev()
                    .find_map(|c| match c {
                        ContentBlock::ToolResult {
                            content,
                            is_error: false,
                            ..
                        } => serde_json::from_str::<Value>(content).ok(),
                        _ => None,
                    })
                    .unwrap();
                let tasks = result["tasks"].as_array().unwrap();
                let pending = result["pending_task_ids"]
                    .as_array()
                    .expect("同次查询须给出剩余待办集合");
                for (i, task) in tasks.iter().enumerate() {
                    assert_eq!(task["title"], format!("FIFO-{i}"));
                    assert_eq!(task["role_id"], if i < 3 { "research" } else { "draft" });
                    assert!(task["source_task_id"].is_null());
                    assert!(task["available_tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|t| t == "read_file"));
                    let terminal = matches!(
                        task["state"].as_str(),
                        Some("completed" | "cancelled" | "failed" | "timed_out" | "interrupted")
                    );
                    assert_eq!(pending.contains(&task["task_id"]), !terminal);
                }
                if n == 5 {
                    assert!(pending.len() >= 3, "三个未清理research任务均须保留");
                    self.accepted.notify_one();
                    vec![ContentBlock::ToolUse {
                        id: "wait-fifo".into(),
                        name: "delegation_status".into(),
                        input: json!({"task_ids":tasks.iter().map(|t|t["task_id"].clone()).collect::<Vec<_>>(),"wait_for":"all"}),
                    }]
                } else {
                    assert!(pending.is_empty());
                    vec![ContentBlock::Text {
                        text: "已收取所有任务的真实终态，取消不等于完成".into(),
                    }]
                }
            } else {
                vec![ContentBlock::Text {
                    text: "已收取所有任务的真实终态，取消不等于完成".into(),
                }]
            }
        } else {
            let title = request
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .find_map(|c| match c {
                    ContentBlock::Text { text } => text
                        .lines()
                        .find_map(|l| l.strip_prefix("任务："))
                        .map(str::to_owned),
                    _ => None,
                })
                .unwrap();
            self.started.lock().unwrap().push(title.clone());
            if title != "FIFO-3" {
                let active = self.research_active.fetch_add(1, SeqCst) + 1;
                self.peak.fetch_max(active, SeqCst);
                self.release.acquire().await.unwrap().forget();
                self.research_active.fetch_sub(1, SeqCst);
            }
            vec![ContentBlock::Text {
                text: format!("{title}实际成果"),
            }]
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
                input_tokens: 20,
                output_tokens: 20,
                ..Default::default()
            },
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn name(&self) -> &str {
        "fifo-member-fixture"
    }
    fn default_model(&self) -> &str {
        "fifo-member-fixture"
    }
}
#[tokio::test]
async fn member_fifo_accepts_three_tasks_without_blocking_other_member_and_cancels_middle() {
    use std::sync::atomic::Ordering::SeqCst;
    for cancel_middle in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = continuous_config(dir.path());
        cfg.agents.as_mut().unwrap().max_active_children = 2;
        let provider = Arc::new(FifoMembers {
            next: 0.into(),
            started: Default::default(),
            research_active: 0.into(),
            peak: 0.into(),
            accepted: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let host = Host::with_provider(cfg.clone(), provider.clone())
            .await
            .unwrap();
        let scope = team_scope("fifo-members");
        let binding = host.open_session(scope.clone()).await.unwrap();
        let parent = host
            .submit_run(
                &scope,
                &binding.session_id,
                "同一成员三项依次处理，其他成员独立工作".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.accepted.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if provider.started.lock().unwrap().contains(&"FIFO-3".into()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(provider.peak.load(SeqCst), 1);
        assert!(!provider
            .started
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "FIFO-1" || s == "FIFO-2"));
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let ds: Vec<Value> = db
            .prepare(
                "SELECT reference FROM host_events WHERE kind='agent_delegation' ORDER BY revision",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        assert_eq!(ds.len(), 4);
        for d in &ds[1..3] {
            let run = host
                .get_run(
                    &serde_json::from_value(d["child_scope"].clone()).unwrap(),
                    d["child_run"].as_str().unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(run.state, RunState::Accepted);
        }
        if cancel_middle {
            host.cancel_run(
                &serde_json::from_value(ds[1]["child_scope"].clone()).unwrap(),
                ds[1]["child_run"].as_str().unwrap(),
            )
            .await
            .unwrap();
        }
        provider.release.add_permits(3);
        tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        assert_eq!(
            host.get_run(&scope, &parent.run_id).await.unwrap().state,
            RunState::Completed
        );
        let actual: Vec<_> = provider
            .started
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.as_str() != "FIFO-3")
            .cloned()
            .collect();
        assert_eq!(
            actual,
            if cancel_middle {
                vec!["FIFO-0", "FIFO-2"]
            } else {
                vec!["FIFO-0", "FIFO-1", "FIFO-2"]
            }
        );
        assert_eq!(provider.peak.load(SeqCst), 1, "同一人实际执行不得重叠");
        let ids: std::collections::BTreeSet<_> =
            ds.iter().map(|d| d["task_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 4);
        drop(db);
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn d124_explicit_response_limit_reaches_real_parent_and_child_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = team_config(tmp.path());
    cfg.agents.as_mut().unwrap().max_response_tokens = Some(16384);
    let provider = Arc::new(TeamProvider::new(false, false));
    let host = Host::with_provider(cfg, provider.clone()).await.unwrap();
    let scope = team_scope("response-limit");
    let binding = host.open_session(scope.clone()).await.unwrap();
    let run = host
        .submit_run(&scope, &binding.session_id, "分工核对项目说明".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&scope, &run.run_id).await.unwrap().state,
        RunState::Completed
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|r| r.system.as_deref().is_some_and(|s| s.contains("（lead）"))));
        assert!(requests.iter().any(|r| r
            .system
            .as_deref()
            .is_some_and(|s| s.contains("（research）"))));
        assert!(requests.iter().all(|r| r.max_tokens == 16384));
    }
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn d124_response_limit_cannot_bypass_shared_group_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = team_config(tmp.path());
    cfg.agents.as_mut().unwrap().max_response_tokens = Some(16384);
    cfg.agents.as_mut().unwrap().max_output_tokens = 8192;
    let provider = Arc::new(TeamProvider::new(false, false));
    let host = Host::with_provider(cfg, provider.clone()).await.unwrap();
    let scope = team_scope("response-reservation");
    let b = host.open_session(scope.clone()).await.unwrap();
    let r = host
        .submit_run(&scope, &b.session_id, "不能超过共享预算".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        host.get_run(&scope, &r.run_id).await.unwrap().state,
        RunState::Failed
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "预留不足必须在Provider调用前拒绝"
    );
    host.shutdown().await.unwrap();
    for invalid in [0, 1_048_577] {
        let mut cfg = team_config(tmp.path());
        cfg.agents.as_mut().unwrap().max_response_tokens = Some(invalid);
        assert!(cfg.validate().is_err());
    }
}
