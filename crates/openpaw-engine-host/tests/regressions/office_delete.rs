//! 职责：删除账本、审批、派生元数据及崩溃恢复的真实回归。
//! 输入输出：隔离Host/SQLite/JSONL到文件与行为断言。
//! 依赖：原Host、Provider和SessionReader/Writer。
//! 禁止职责：不删除用户数据，不以UI成功代替物理删除证据。
use openpaw_engine_host::*;
use openpaw_llm::{MockMode, MockProvider};
use serde_json::json;
use std::{sync::Arc, time::Duration};
fn config(base: &std::path::Path) -> HostConfig {
    let root = base.join("项目 root");
    std::fs::create_dir_all(&root).unwrap();
    serde_json::from_value(json!({"home":base.join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file","write_file"],"approval_timeout_secs":10})).unwrap()
}
fn scope(id: &str) -> ClientScope {
    ClientScope {
        namespace: "office".into(),
        slot_id: "office-1".into(),
        conversation_id: id.into(),
    }
}
#[tokio::test]
async fn delete_removes_history_backups_and_derived_text_but_preserves_other_session_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let a = scope("delete");
    let b = scope("keep");
    let ba = host.open_session(a.clone()).await.unwrap();
    let bb = host.open_session(b.clone()).await.unwrap();
    let r = host
        .submit_run(&a, &ba.session_id, "private original input".into())
        .await
        .unwrap();
    host.wait_idle().await;
    let other = host
        .submit_run(&b, &bb.session_id, "other session survives".into())
        .await
        .unwrap();
    host.wait_idle().await;
    std::fs::write(cfg.roots["r"].join("keep.txt"), "workspace artifact").unwrap();
    let ledger = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", ba.session_id));
    let keep = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", bb.session_id));
    let original = std::fs::read(&keep).unwrap();
    let backup = ledger.with_file_name(format!("{}.jsonl.torn-123-0.bak", ba.session_id));
    std::fs::write(&backup, "private torn content").unwrap();
    let user_backup = ledger.with_file_name(format!("{}.jsonl.user.bak", ba.session_id));
    std::fs::write(&user_backup, "user backup").unwrap();
    let deleted = host.delete_session(&a).await.unwrap();
    assert_eq!(deleted.phase, "deleted");
    assert!(!ledger.exists());
    assert!(!backup.exists());
    assert!(user_backup.exists());
    assert_eq!(std::fs::read(&keep).unwrap(), original);
    assert!(cfg.roots["r"].join("keep.txt").exists());
    assert_eq!(
        host.open_session(a.clone()).await.unwrap_err().kind,
        "session_deleted"
    );
    assert!(host.get_run(&a, &r.run_id).await.is_err());
    assert!(host.get_session(&a, &ba.session_id).await.is_err());
    assert!(host
        .submit_run(&a, &ba.session_id, "revive".into())
        .await
        .is_err());
    assert_eq!(host.delete_session(&a).await.unwrap(), deleted);
    assert_eq!(
        host.get_run(&b, &other.run_id).await.unwrap().state,
        RunState::Completed
    );
    host.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    for table in [
        "host_runs",
        "host_requests",
        "host_approvals",
        "host_office_actions",
    ] {
        let n:i64=db.query_row(&format!("SELECT count(*) FROM {table} WHERE CAST({} AS TEXT) LIKE '%private original input%'",if table=="host_runs"||table=="host_approvals"{"data"}else if table=="host_requests"{"request"}else{"context"}),[],|r|r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
    let remaining: i64 = db
        .query_row(
            "SELECT count(*) FROM host_bindings WHERE id=?1",
            [&ba.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
    drop(db);
    let restarted = Host::start(cfg).await.unwrap();
    assert_eq!(
        restarted.open_session(a).await.unwrap_err().kind,
        "session_deleted"
    );
    assert_eq!(
        restarted.get_run(&b, &other.run_id).await.unwrap().state,
        RunState::Completed
    );
    restarted.shutdown().await.unwrap();
}
#[tokio::test]
async fn delete_pending_approval_cancels_only_target_and_old_approval_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "write_file".into(),
            input: json!({"path":"forbidden.txt","content":"bad"}),
            text: "拟写入".into(),
        },
        MockMode::Fixed("结束".into()),
    ]));
    let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
    let a = scope("pending");
    let binding = host.open_session(a.clone()).await.unwrap();
    host.submit_run(&a, &binding.session_id, "write".into())
        .await
        .unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(a) = host.pending_approvals(&a).await.unwrap().first() {
                break a.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.delete_session(&a))
        .await
        .unwrap()
        .unwrap();
    assert!(!cfg.roots["r"].join("forbidden.txt").exists());
    assert!(host
        .decide_approval(
            &a,
            &approval.approval_id,
            approval.revision,
            approval.binding,
            true
        )
        .await
        .is_err());
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn pending_delete_intent_is_recovered_before_reopening_or_reading_a_corrupt_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let a = scope("crash");
    let binding = host.open_session(a.clone()).await.unwrap();
    host.shutdown().await.unwrap();
    let ledger = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", binding.session_id));
    std::fs::write(&ledger, "unreadable intermediate corruption\n").unwrap();
    let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
    db.execute(
        "INSERT INTO host_session_deletions VALUES(?1,?2,'deleting',0)",
        rusqlite::params![serde_json::to_string(&a).unwrap(), binding.session_id],
    )
    .unwrap();
    drop(db);
    let host = Host::start(cfg).await.unwrap();
    assert!(!ledger.exists());
    assert_eq!(host.delete_session(&a).await.unwrap().phase, "deleted");
    assert!(host.open_session(a).await.is_err());
    host.shutdown().await.unwrap();
}
#[tokio::test]
async fn deletion_io_failure_keeps_durable_intent_and_retries_after_problem_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let a = scope("io");
    let binding = host.open_session(a.clone()).await.unwrap();
    let ledger = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", binding.session_id));
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    assert!(host.delete_session(&a).await.is_err());
    assert!(host.open_session(a.clone()).await.is_err());
    std::fs::remove_dir(&ledger).unwrap();
    assert_eq!(host.delete_session(&a).await.unwrap().phase, "deleted");
    host.shutdown().await.unwrap();
}
