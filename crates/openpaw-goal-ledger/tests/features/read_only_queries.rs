//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
use openpaw_goal_ledger::{FsGoalStore, GoalStore};
#[test]
fn strict_goal_reader_does_not_create_events_and_reports_bad_documents() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("goals");
    let store = FsGoalStore::new(root.clone());
    assert!(store.list_readonly_checked().unwrap().is_empty());
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("bad.md"), "---\n[broken\n---\nbody").unwrap();
    let before = std::fs::read(root.join("bad.md")).unwrap();
    assert!(store.list_readonly_checked().is_err());
    assert!(store.list_goals().unwrap().is_empty());
    assert_eq!(std::fs::read(root.join("bad.md")).unwrap(), before);
    assert!(!root.join(".events").exists());
}
#[cfg(unix)]
#[test]
fn strict_goal_reader_rejects_external_file_link() {
    let base = tempfile::tempdir().unwrap();
    let out = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(out.path(), base.path().join("escape.md")).unwrap();
    assert!(FsGoalStore::new(base.path().to_path_buf())
        .list_readonly_checked()
        .is_err());
}
