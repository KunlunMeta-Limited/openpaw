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
    for (phase, missing) in [
        ("deleting", false),
        ("cancelling", false),
        ("purging", false),
        ("purging", true),
    ] {
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
            "INSERT INTO host_session_deletions VALUES(?1,?2,?3,0)",
            rusqlite::params![
                serde_json::to_string(&a).unwrap(),
                binding.session_id,
                phase
            ],
        )
        .unwrap();
        drop(db);
        if missing {
            std::fs::remove_file(&ledger).unwrap();
        }
        let host = Host::start(cfg).await.unwrap();
        assert!(!ledger.exists());
        assert_eq!(host.delete_session(&a).await.unwrap().phase, "deleted");
        assert!(host.open_session(a).await.is_err());
        host.shutdown().await.unwrap();
    }
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn delete_running_command_waits_for_cleanup_and_preserves_other_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.tools.push("bash".into());
    let provider = Arc::new(MockProvider::scripted(vec![
        MockMode::ToolUse {
            tool_name: "bash".into(),
            input: json!({"command":"printf ready > command-ready; sleep 2; printf bad > late-side-effect"}),
            text: "拟执行慢命令".into(),
        },
        MockMode::Fixed("finished".into()),
    ]));
    let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
    let a = scope("running-delete");
    let b = scope("keep-running-delete");
    let sa = host.open_session(a.clone()).await.unwrap();
    let sb = host.open_session(b.clone()).await.unwrap();
    host.submit_run(&a, &sa.session_id, "run command".into())
        .await
        .unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(v) = host.pending_approvals(&a).await.unwrap().first() {
                break v.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.decide_approval(
        &a,
        &approval.approval_id,
        approval.revision,
        approval.binding,
        true,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !cfg.roots["r"].join("command-ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), host.delete_session(&a))
        .await
        .unwrap()
        .unwrap();
    assert!(!cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", sa.session_id))
        .exists());
    assert!(host.get_session(&b, &sb.session_id).await.is_ok());
    // 超过原命令的实际副作用时刻；这是检查未发生延迟写入，不用于猜清理结束。
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!cfg.roots["r"].join("late-side-effect").exists());
    assert!(host
        .submit_run(&a, &sa.session_id, "revive".into())
        .await
        .is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn metadata_purge_failure_after_ledger_removal_recovers_without_revival() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let host = Host::start(cfg.clone()).await.unwrap();
    let target = scope("metadata-failure");
    let keep = scope("unrelated");
    let binding = host.open_session(target.clone()).await.unwrap();
    let other = host.open_session(keep.clone()).await.unwrap();
    host.submit_run(
        &target,
        &binding.session_id,
        "remove private history".into(),
    )
    .await
    .unwrap();
    host.wait_idle().await;
    let ledger = cfg
        .home
        .join("sessions")
        .join(format!("{}.jsonl", binding.session_id));
    let db_path = cfg.home.join("engine-host/host.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("CREATE TRIGGER stop_purge BEFORE DELETE ON host_bindings BEGIN SELECT RAISE(FAIL,'injected metadata cleanup failure'); END;").unwrap();
    assert!(host.delete_session(&target).await.is_err());
    assert!(
        !ledger.exists(),
        "物理删除发生后，元数据事务失败不能声称回滚了历史"
    );
    assert!(host.open_session(target.clone()).await.is_err());
    let phase: String = db
        .query_row(
            "SELECT phase FROM host_session_deletions WHERE session=?1",
            [&binding.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(phase, "purging");
    host.shutdown().await.unwrap();
    db.execute_batch("DROP TRIGGER stop_purge").unwrap();
    drop(db);
    let host = Host::start(cfg.clone()).await.unwrap();
    assert_eq!(host.delete_session(&target).await.unwrap().phase, "deleted");
    assert!(host.open_session(target).await.is_err());
    assert_eq!(
        host.open_session(keep).await.unwrap().session_id,
        other.session_id
    );
    assert!(!ledger.exists());
    host.shutdown().await.unwrap();
}

#[test]
fn deletion_restart_recovers_each_real_durable_phase_after_process_kill() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    for (phase, team) in [
        ("deleting", false),
        ("cancelling", false),
        ("purging", false),
        ("deleting", true),
        ("cancelling", true),
        ("purging", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "deletion_crash_fixture_child", "--nocapture"])
            .env("OPENPAW_DELETE_CRASH_BASE", dir.path())
            .env("OPENPAW_DELETE_CRASH_PHASE", phase)
            .env("OPENPAW_DELETE_CRASH_TEAM", if team { "1" } else { "0" })
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if let Some(value) = line.strip_prefix("DELETE-READY ") {
                    let _ = tx.send(value.to_owned());
                    break;
                }
            }
        });
        let ready = rx.recv_timeout(Duration::from_secs(10));
        // 不走Host shutdown，实际杀进程，验证OS锁/writer释放和持久意图恢复。
        child.kill().unwrap();
        child.wait().unwrap();
        reader.join().unwrap();
        let ids: serde_json::Value = serde_json::from_str(&ready.unwrap()).unwrap();
        let cfg = config(dir.path());
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let actual: String = db
            .query_row(
                "SELECT phase FROM host_session_deletions WHERE session=?1",
                [ids["target"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(actual, phase);
        assert_eq!(ids["children"].as_array().unwrap().len(), usize::from(team));
        db.execute_batch("DROP TRIGGER interrupt_delete").unwrap();
        drop(db);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let host = Host::start(cfg.clone()).await.unwrap();
            assert_eq!(
                host.delete_session(&scope("crash-target"))
                    .await
                    .unwrap()
                    .phase,
                "deleted"
            );
            assert!(host.open_session(scope("crash-target")).await.is_err());
            assert_eq!(
                host.open_session(scope("crash-other"))
                    .await
                    .unwrap()
                    .session_id,
                ids["other"].as_str().unwrap()
            );
            assert!(!cfg
                .home
                .join("sessions")
                .join(format!("{}.jsonl", ids["target"].as_str().unwrap()))
                .exists());
            for child in ids["children"].as_array().unwrap() {
                let child_scope: ClientScope =
                    serde_json::from_value(child["scope"].clone()).unwrap();
                assert_eq!(
                    host.open_session(child_scope.clone())
                        .await
                        .unwrap_err()
                        .kind,
                    "session_deleted"
                );
                assert!(host
                    .get_run(&child_scope, child["run"].as_str().unwrap())
                    .await
                    .is_err());
                assert!(!cfg
                    .home
                    .join("sessions")
                    .join(format!("{}.jsonl", child["session"].as_str().unwrap()))
                    .exists());
                assert_eq!(
                    host.delete_session(&child_scope).await.unwrap().phase,
                    "deleted"
                );
            }
            let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
            let bindings: u64 = db
                .query_row("SELECT COUNT(*) FROM host_bindings", [], |r| r.get(0))
                .unwrap();
            let delegations: u64 = db
                .query_row(
                    "SELECT COUNT(*) FROM host_events WHERE kind='agent_delegation'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bindings, 1, "仅保留无关会话，父子均不可复活");
            assert_eq!(delegations, 0);
            assert_eq!(
                std::fs::read_to_string(cfg.roots["r"].join("keep.txt")).unwrap(),
                "workspace survives"
            );
            host.shutdown().await.unwrap();
        });
    }
}

#[test]
fn deletion_crash_fixture_child() {
    use std::io::Write;
    let Some(base) = std::env::var_os("OPENPAW_DELETE_CRASH_BASE") else {
        return;
    };
    let phase = std::env::var("OPENPAW_DELETE_CRASH_PHASE").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut cfg = config(std::path::Path::new(&base));
        let team = std::env::var("OPENPAW_DELETE_CRASH_TEAM").unwrap() == "1";
        let provider = if team {
            cfg.agents = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
            MockProvider::scripted(vec![
                MockMode::ToolUse { tool_name:"delegate_task".into(), input:json!({"role_id":"research","title":"核查项目说明","instructions":"读取keep.txt","acceptance":"返回文件事实"}), text:String::new() },
                MockMode::ToolUse { tool_name:"read_file".into(), input:json!({"path":"keep.txt"}), text:String::new() },
                MockMode::Fixed("已读取实际工作区事实".into()),
                MockMode::Fixed("主任务汇总实际子任务结果".into()),
            ])
        } else {
            MockProvider::fixed("private history")
        };
        std::fs::write(cfg.roots["r"].join("keep.txt"), "workspace survives").unwrap();
        let provider = Arc::new(provider);
        let host = Host::with_provider(cfg.clone(), provider.clone()).await.unwrap();
        let target = host.open_session(scope("crash-target")).await.unwrap();
        let other = host.open_session(scope("crash-other")).await.unwrap();
        host.submit_run(&scope("crash-target"), &target.session_id, "private history".into()).await.unwrap();
        host.wait_idle().await;
        std::fs::write(cfg.roots["r"].join("keep.txt"), "workspace survives").unwrap();
        let db = rusqlite::Connection::open(cfg.home.join("engine-host/host.db")).unwrap();
        let children: Vec<serde_json::Value> = {
            let mut q = db.prepare("SELECT reference FROM host_events WHERE kind='agent_delegation'").unwrap();
            q.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| {
                let v:serde_json::Value=serde_json::from_str(&r.unwrap()).unwrap();
                json!({"scope":v["child_scope"],"session":v["child_session"],"run":v["child_run"]})
            }).collect()
        };
        assert_eq!(children.len(), usize::from(team));
        assert_eq!(provider.call_count(), if team { 4 } else { 1 });
        for child in &children {
            let child_scope:ClientScope=serde_json::from_value(child["scope"].clone()).unwrap();
            assert_eq!(host.get_run(&child_scope, child["run"].as_str().unwrap()).await.unwrap().state, RunState::Completed);
            assert!(cfg.home.join("sessions").join(format!("{}.jsonl",child["session"].as_str().unwrap())).is_file());
        }
        let sql = match phase.as_str() {
            "deleting" => "CREATE TRIGGER interrupt_delete BEFORE UPDATE OF phase ON host_session_deletions WHEN NEW.phase='cancelling' BEGIN SELECT RAISE(FAIL,'crash boundary'); END;",
            "cancelling" => "CREATE TRIGGER interrupt_delete BEFORE UPDATE OF phase ON host_session_deletions WHEN NEW.phase='purging' BEGIN SELECT RAISE(FAIL,'crash boundary'); END;",
            "purging" => "CREATE TRIGGER interrupt_delete BEFORE DELETE ON host_bindings BEGIN SELECT RAISE(FAIL,'crash boundary'); END;",
            _ => panic!("unknown phase"),
        };
        db.execute_batch(sql).unwrap();
        assert!(host.delete_session(&scope("crash-target")).await.is_err());
        let actual: String = db.query_row("SELECT phase FROM host_session_deletions WHERE session=?1", [&target.session_id], |r| r.get(0)).unwrap();
        assert_eq!(actual, phase);
        println!("DELETE-READY {}", json!({"target":target.session_id,"other":other.session_id,"children":children}));
        std::io::stdout().flush().unwrap();
        std::future::pending::<()>().await;
    });
}
