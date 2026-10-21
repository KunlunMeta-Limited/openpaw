//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
use openpaw_memory::skills::{
    skill::{Scope, SkillFrontmatter},
    store::{FsSkillStore, SkillStore},
};
#[test]
fn strict_skill_reader_is_readonly_reports_damage_and_preserves_legacy_tolerance() {
    let base = tempfile::tempdir().unwrap();
    let user = base.path().join("skills");
    let store = FsSkillStore::new(user.clone(), None);
    assert!(store.list_readonly_checked().unwrap().is_empty());
    assert!(!user.exists());
    store
        .put(
            Scope::User,
            SkillFrontmatter {
                name: "alpha".into(),
                description: "test".into(),
                ..Default::default()
            },
            "body",
        )
        .unwrap();
    let file = user.join("alpha/SKILL.md");
    let before = std::fs::read(&file).unwrap();
    assert_eq!(store.list_readonly_checked().unwrap().len(), 1);
    assert_eq!(std::fs::read(&file).unwrap(), before);
    std::fs::write(&file, "---\n[bad: yaml\n---\nbody").unwrap();
    assert!(store.list_readonly_checked().is_err());
    assert!(store.list().unwrap().is_empty());
}
#[cfg(unix)]
#[test]
fn strict_skill_reader_rejects_escaping_links() {
    let base = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(base.path().join("skills")).unwrap();
    std::os::unix::fs::symlink(outside.path(), base.path().join("skills/escape")).unwrap();
    assert!(FsSkillStore::new(base.path().join("skills"), None)
        .list_readonly_checked()
        .is_err());
}
