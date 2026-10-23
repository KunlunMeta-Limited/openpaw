//! 职责：以真实Rust入口演示T3并行、审批、查询和取消。
//! 输入输出：调用方提供的全新隔离目录与可核对JSON/文件。
//! 依赖：原MockProvider、AgentLoop、工具、JSONL和Host服务。
//! 禁止职责：不开放调试HTTP，不读取用户项目配置，不冒充Office联调。
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use std::{sync::Arc, time::Duration};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("请传入新的隔离目录")?;
    if base.exists() && std::fs::read_dir(&base)?.next().is_some() {
        return Err("只允许全新或空的隔离目录".into());
    }
    std::fs::create_dir_all(&base)?;
    let base = std::fs::canonicalize(base)?;
    let root = base.join("workspace");
    std::fs::create_dir_all(&root)?;
    let config = HostConfig {
        office: None,
        home: base.join("home"),
        roots: [("project".into(), root.clone())].into(),
        slots: [
            ("office-1".into(), "project".into()),
            ("office-2".into(), "project".into()),
        ]
        .into(),
        max_active_runs: 4,
        provider: "mock".into(),
        model: None,
        base_url: None,
        api_key_env: None,
        basic_username_env: None,
        basic_password_env: None,
        tools: vec!["read_file".into(), "write_file".into(), "sleep".into()],
        run_timeout_secs: 30,
        stream_idle_secs: 10,
        tool_timeout_secs: 10,
        approval_timeout_secs: 10,
    };
    let provider = MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: serde_json::json!({"path":"report.txt","content":"approved once"}),
            text: "proposed".into(),
        },
        MockMode::ToolUse {
            tool_name: "sleep".into(),
            input: serde_json::json!({"duration_ms":5_000}),
            text: "other session".into(),
        },
        MockMode::Fixed("completed".into()),
    ]);
    let host = Host::with_provider(config, Arc::new(provider)).await?;
    let a = ClientScope {
        namespace: "smoke".into(),
        slot_id: "office-1".into(),
        conversation_id: "report".into(),
    };
    let b = ClientScope {
        slot_id: "office-2".into(),
        conversation_id: "cancel".into(),
        ..a.clone()
    };
    let sa = host.open_session(a.clone()).await?;
    let sb = host.open_session(b.clone()).await?;
    let ra = host
        .submit_run(&a, &sa.session_id, "Write a report after approval".into())
        .await?;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (view, mut rx) = host.snapshot_and_subscribe(&a).await?;
            if let Some(approval) = view.pending_approvals.first() {
                break Ok::<_, HostError>(approval.clone());
            }
            let _ = rx.recv().await;
        }
    })
    .await??;
    assert!(!root.join("report.txt").exists());
    let rb = host
        .submit_run(&b, &sb.session_id, "Run independently".into())
        .await?;
    host.cancel_run(&b, &rb.run_id).await?;
    host.decide_approval(
        &a,
        &approval.approval_id,
        approval.revision,
        approval.binding,
        true,
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(10), host.wait_idle()).await?;
    let ra = host.get_run(&a, &ra.run_id).await?;
    let rb = host.get_run(&b, &rb.run_id).await?;
    assert_eq!(ra.state, RunState::Completed);
    assert_eq!(rb.state, RunState::Cancelled);
    let file = host
        .query(
            &a,
            QueryRequest {
                session_id: sa.session_id.clone(),
                kind: QueryKind::File {
                    path: "report.txt".into(),
                },
                offset: 0,
                limit: 0,
            },
        )
        .await?;
    let history = host
        .query(
            &a,
            QueryRequest {
                session_id: sa.session_id,
                kind: QueryKind::History,
                offset: 0,
                limit: 0,
            },
        )
        .await?;
    println!(
        "{}",
        serde_json::json!({"report_run":ra,"cancelled_run":rb,"file":file,"history_page":history})
    );
    host.shutdown().await?;
    Ok(())
}
