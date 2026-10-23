//! Skills 侧基础层端到端验收（方案 §3.7）：
//! - 五态全迁移路径：以 `SkillStatus` 的五个状态（Active / Stale / Archived /
//!   Merged / Reactivated）为准，用 FsSkillStore（tempdir）+ SkillLifecycle
//!   走完全部合法迁移边，每条迁移断言状态落盘正确；
//! - 非法迁移被拒（落盘状态不变）；
//! - store 合同 + 失败路径；
//! - manifest / permissions 各至少 1 例。
use openpaw_memory::skills::lifecycle::SkillLifecycle;
use openpaw_memory::skills::manifest::{Compatibility, SkillManifest};
use openpaw_memory::skills::permissions::{
    check_skill_permission, skill_has_only_safe_properties, SkillPermissionDecision,
};
use openpaw_memory::skills::store::{FsSkillStore, SkillStore, SkillStoreError};
use openpaw_memory::skills::{LoadedSkill, Scope, SkillFrontmatter, SkillStatus};
use std::collections::HashMap;
fn fm(name: &str, desc: &str) -> SkillFrontmatter {
    SkillFrontmatter {
        name: name.to_string(),
        description: desc.to_string(),
        triggers: vec!["alpha".into(), name.into()],
        ..Default::default()
    }
}
fn put_skill(store: &FsSkillStore, name: &str, desc: &str) {
    store.put(Scope::User, fm(name, desc), "body").unwrap();
}
fn status_on_disk(store: &FsSkillStore, name: &str) -> SkillStatus {
    store
        .get(name)
        .unwrap()
        .expect("skill should exist on disk")
        .frontmatter
        .status
}
/// 五个状态 × 全部 8 条合法迁移边，每步断言落盘状态。
#[test]
fn five_state_full_migration_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsSkillStore::new(dir.path().to_path_buf(), None);
    // ── 主链：Active → Stale → Reactivated → Stale → Archived → Active ──
    put_skill(&store, "traveller", "visits every state");
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Active);
    // 1. Active → Stale
    store.mark_stale(Scope::User, "traveller").unwrap();
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Stale);
    // 2. Stale → Reactivated
    store.reactivate(Scope::User, "traveller").unwrap();
    assert_eq!(
        status_on_disk(&store, "traveller"),
        SkillStatus::Reactivated
    );
    // 3. Reactivated → Stale
    store.mark_stale(Scope::User, "traveller").unwrap();
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Stale);
    // 4. Stale → Archived
    store.archive(Scope::User, "traveller").unwrap();
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Archived);
    // 5. Archived → Active
    store.restore(Scope::User, "traveller").unwrap();
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Active);
    // 6. Active → Archived
    store.archive(Scope::User, "traveller").unwrap();
    assert_eq!(status_on_disk(&store, "traveller"), SkillStatus::Archived);
    // ── Merged 分支：merge 产生 Merged 源与 Active 合并体；Merged → Archived ──
    put_skill(&store, "source-a", "first source");
    put_skill(&store, "source-b", "second source");
    // 7. merge：源 skills 落盘为 Merged（且记录 superseded_by），合并体为 Active
    let merged_path = store
        .merge(
            Scope::User,
            &["source-a".to_string(), "source-b".to_string()],
            "combined",
            "merged body",
        )
        .unwrap();
    assert!(merged_path.ends_with("combined/SKILL.md"));
    assert_eq!(status_on_disk(&store, "source-a"), SkillStatus::Merged);
    assert_eq!(status_on_disk(&store, "source-b"), SkillStatus::Merged);
    assert_eq!(status_on_disk(&store, "combined"), SkillStatus::Active);
    let src = store.get("source-a").unwrap().unwrap();
    assert_eq!(src.frontmatter.superseded_by.as_deref(), Some("combined"));
    // 8. Merged → Archived
    store.archive(Scope::User, "source-a").unwrap();
    assert_eq!(status_on_disk(&store, "source-a"), SkillStatus::Archived);
    // can_transition 矩阵应与上面走过的 8 条合法边一致
    assert!(store.can_transition(SkillStatus::Active, SkillStatus::Stale));
    assert!(store.can_transition(SkillStatus::Stale, SkillStatus::Reactivated));
    assert!(store.can_transition(SkillStatus::Reactivated, SkillStatus::Stale));
    assert!(store.can_transition(SkillStatus::Stale, SkillStatus::Archived));
    assert!(store.can_transition(SkillStatus::Archived, SkillStatus::Active));
    assert!(store.can_transition(SkillStatus::Active, SkillStatus::Archived));
    assert!(store.can_transition(SkillStatus::Reactivated, SkillStatus::Archived));
    assert!(store.can_transition(SkillStatus::Merged, SkillStatus::Archived));
}
/// 非法迁移被拒，且落盘状态保持不变。
#[test]
fn illegal_transitions_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsSkillStore::new(dir.path().to_path_buf(), None);
    // 准备三个不同状态的 skill
    put_skill(&store, "stale-one", "stale");
    store.mark_stale(Scope::User, "stale-one").unwrap();
    put_skill(&store, "merged-one", "merged");
    put_skill(&store, "target", "merge target");
    store
        .merge(Scope::User, &["merged-one".to_string()], "target", "body")
        .unwrap();
    put_skill(&store, "archived-one", "archived");
    store.archive(Scope::User, "archived-one").unwrap();
    // Stale → Active（restore）非法
    let err = store.restore(Scope::User, "stale-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    assert_eq!(status_on_disk(&store, "stale-one"), SkillStatus::Stale);
    // Stale → Stale（重复 mark_stale）非法
    let err = store.mark_stale(Scope::User, "stale-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    assert_eq!(status_on_disk(&store, "stale-one"), SkillStatus::Stale);
    // Merged → Active / Merged → Stale / Merged → Reactivated 均非法
    let err = store.restore(Scope::User, "merged-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    let err = store.mark_stale(Scope::User, "merged-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    let err = store.reactivate(Scope::User, "merged-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    assert_eq!(status_on_disk(&store, "merged-one"), SkillStatus::Merged);
    // Archived → Stale / Archived → Reactivated 非法
    let err = store.mark_stale(Scope::User, "archived-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    let err = store.reactivate(Scope::User, "archived-one").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    assert_eq!(
        status_on_disk(&store, "archived-one"),
        SkillStatus::Archived
    );
    // Active → Reactivated 非法（Reactivated 只能从 Stale 进入）
    put_skill(&store, "fresh", "active");
    let err = store.reactivate(Scope::User, "fresh").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    assert_eq!(status_on_disk(&store, "fresh"), SkillStatus::Active);
    // 不存在的 skill：所有迁移入口都报 Config 错
    assert!(store.archive(Scope::User, "no-such").is_err());
    assert!(store.mark_stale(Scope::User, "no-such").is_err());
    assert!(store.reactivate(Scope::User, "no-such").is_err());
    assert!(store.restore(Scope::User, "no-such").is_err());
}
/// store 合同：创建/读取/frontmatter 往返/删除；失败路径返回错误或既定合同值。
#[test]
fn store_contract_and_failure_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsSkillStore::new(dir.path().to_path_buf(), None);
    // 创建 + 读取 + frontmatter 往返
    let mut front = fm("roundtrip", "full field roundtrip");
    front.version = Some("1.2.3".into());
    front.priority = Some(7);
    front.tags = vec!["tag-a".into(), "tag-b".into()];
    front.when_to_use = Some("User asks for roundtrip".into());
    front.pinned = true;
    front.status = SkillStatus::Active;
    let path = store.put(Scope::User, front, "hello body").unwrap();
    assert!(path.exists());
    let loaded: LoadedSkill = store.get("roundtrip").unwrap().expect("must exist");
    assert_eq!(loaded.name(), "roundtrip");
    assert_eq!(loaded.frontmatter.description, "full field roundtrip");
    assert_eq!(loaded.frontmatter.version.as_deref(), Some("1.2.3"));
    assert_eq!(loaded.frontmatter.priority, Some(7));
    assert_eq!(loaded.frontmatter.tags, vec!["tag-a", "tag-b"]);
    assert_eq!(
        loaded.frontmatter.when_to_use.as_deref(),
        Some("User asks for roundtrip")
    );
    assert!(loaded.frontmatter.pinned);
    assert_eq!(loaded.frontmatter.status, SkillStatus::Active);
    assert_eq!(loaded.body.trim(), "hello body");
    assert_eq!(loaded.scope, Scope::User);
    assert_eq!(store.list().unwrap().len(), 1);
    // 失败路径：读取不存在 → None
    assert!(store.get("ghost").unwrap().is_none());
    // 失败路径：删除不存在 → 合同约定返回 Ok(false)（"Returns true if it existed"）
    assert!(!store.delete(Scope::User, "ghost").unwrap());
    // 失败路径：删除存在的 skill → true，之后读取为空
    assert!(store.delete(Scope::User, "roundtrip").unwrap());
    assert!(store.get("roundtrip").unwrap().is_none());
    // 失败路径：非法名字 → InvalidName
    for bad in ["", "..", "a/b", ".hidden", "with space"] {
        let err = store.put(Scope::User, fm(bad, "x"), "y").unwrap_err();
        assert!(matches!(err, SkillStoreError::InvalidName(_)));
        assert!(store.get(bad).is_err());
    }
    // 失败路径：Project scope 未配置 root → Config 错
    let err = store.put(Scope::Project, fm("p", "x"), "y").unwrap_err();
    assert!(matches!(err, SkillStoreError::Config(_)));
    // 失败路径：空 root list → 空
    let empty = FsSkillStore::new(dir.path().join("missing"), None);
    assert!(empty.list().unwrap().is_empty());
}
#[test]
fn manifest_compatibility_and_roundtrip() {
    let manifest = SkillManifest {
        name: "mem-curator".into(),
        version: "0.3.0".into(),
        description: "Curate memory skills".into(),
        author: "openpaw".into(),
        license: Some("MIT".into()),
        homepage: None,
        repository: None,
        tags: vec!["memory".into()],
        triggers: vec!["memory".into()],
        categories: vec![],
        compatibility: Compatibility {
            openpaw_min: Some("0.1.0".into()),
            openpaw_max: Some("9.9.9".into()),
            platforms: vec![],
            features: vec![],
        },
        dependencies: vec![],
        scripts: HashMap::new(),
        checksum: None,
        size_bytes: None,
        installed_version: None,
        installed_at: None,
        source_hub: None,
    };
    assert!(manifest.is_compatible("0.2.0"));
    assert!(!manifest.is_compatible("0.0.9"));
    assert!(!manifest.is_compatible("10.0.0"));
    let json = serde_json::to_string(&manifest).unwrap();
    let back: SkillManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.name, "mem-curator");
    assert_eq!(back.version, "0.3.0");
}
#[test]
fn permission_rules_and_safe_properties() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsSkillStore::new(dir.path().to_path_buf(), None);
    store
        .put(Scope::User, fm("plain", "safe props only"), "body")
        .unwrap();
    let mut advanced = fm("advanced", "has tools");
    advanced.allowed_tools = vec!["read_file".into()];
    advanced.context = Some(openpaw_memory::skills::SkillContext::Fork);
    store.put(Scope::User, advanced, "body").unwrap();
    let plain = store.get("plain").unwrap().unwrap();
    assert!(skill_has_only_safe_properties(&plain));
    // deny 优先于一切
    let deny = vec!["plain".to_string()];
    assert!(matches!(
        check_skill_permission(&plain, &deny, &[]),
        SkillPermissionDecision::Deny { .. }
    ));
    // 白名单属性 + 无 deny/allow → 放行
    assert!(matches!(
        check_skill_permission(&plain, &[], &[]),
        SkillPermissionDecision::Allow
    ));
    let adv = store.get("advanced").unwrap().unwrap();
    assert!(!skill_has_only_safe_properties(&adv));
    // 高级属性无规则 → Ask
    assert!(matches!(
        check_skill_permission(&adv, &[], &[]),
        SkillPermissionDecision::Ask { .. }
    ));
    // allow 规则覆盖 Ask
    let allow = vec!["advanced".to_string()];
    assert!(matches!(
        check_skill_permission(&adv, &[], &allow),
        SkillPermissionDecision::Allow
    ));
}
// ============================================================================
// 编排层与工具入口验收（manager / dag / conditional / hot_reload / tool）
// ============================================================================
use openpaw_core::{ApprovalManager, AutoApproveManager};
use openpaw_memory::skills::conditional::{
    check_path_condition, has_path_condition, path_matches_skill, PathPatternCache,
};
use openpaw_memory::skills::dag::SkillDag;
use openpaw_memory::skills::hot_reload::{SkillSnapshot, SkillWatcher};
use openpaw_memory::skills::manager::SkillManager;
use openpaw_memory::skills::telemetry::{FsTelemetryStore, SkillProvenance};
use openpaw_memory::skills::tool;
fn make_fm(name: &str, desc: &str) -> SkillFrontmatter {
    SkillFrontmatter {
        name: name.to_string(),
        description: desc.to_string(),
        triggers: vec!["e2e".into()],
        ..Default::default()
    }
}
/// manager 合同：创建 → 加载 → 列表 → 搜索；以及三类失败路径。
#[test]
fn manager_contract_create_load_list_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SkillManager::new(dir.path());
    // 合同：创建 → 列表可见 → 搜索可命中
    mgr.create(
        Scope::User,
        &make_fm("alpha-mgr", "alpha manager skill"),
        "alpha body",
    )
    .unwrap();
    let active = mgr.list_active().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].name(), "alpha-mgr");
    let hits = mgr.search("alpha").unwrap();
    assert!(hits.iter().any(|s| s.frontmatter.name == "alpha-mgr"));
    // 失败路径：归档不存在的技能
    assert!(mgr.archive("ghost").is_err());
    // 失败路径：pin 不存在的技能
    assert!(mgr.pin("ghost").is_err());
    // 失败路径：对已归档技能 mark_stale（必须先 restore）
    mgr.create(Scope::User, &make_fm("arch-mgr", "to archive"), "body")
        .unwrap();
    mgr.archive("arch-mgr").unwrap();
    assert!(mgr.mark_stale("arch-mgr").is_err());
    assert_eq!(mgr.list_active().unwrap().len(), 1);
}
/// dag：母仓 dag 测试的离线移植 —— 环检测、悬空引用、学习路径拓扑序、
/// from_skills 构建。
#[test]
fn dag_cycles_dangling_and_learning_path() {
    // 环检测
    let mut dag = SkillDag::new();
    dag.add_skill("a", &["b".to_string()]);
    dag.add_skill("b", &["a".to_string()]);
    let result = dag.validate();
    assert!(!result.is_valid);
    assert!(!result.cycles.is_empty());
    // 悬空引用 + 不崩溃
    let mut dag = SkillDag::new();
    dag.add_skill("a", &["nonexistent".to_string()]);
    dag.add_skill("b", &["a".to_string()]);
    let result = dag.validate();
    assert!(result
        .dangling_references
        .contains(&"nonexistent".to_string()));
    assert!(result.cycles.is_empty());
    // 学习路径拓扑序：prerequisites 先于 dependent
    let mut dag = SkillDag::new();
    dag.add_skill("c", &["b".to_string()]);
    dag.add_skill("b", &["a".to_string()]);
    dag.add_skill("a", &[]);
    let path = dag.learning_path(&std::collections::HashSet::new());
    let names: Vec<&str> = path.steps.iter().map(|n| n.skill_name.as_str()).collect();
    let pos = |n: &str| names.iter().position(|&x| x == n).unwrap();
    assert!(pos("a") < pos("b"));
    assert!(pos("b") < pos("c"));
    // from_skills：由 LoadedSkill 的 prerequisites 建图
    let mut fm = make_fm("rust-async", "Async programming");
    fm.prerequisites = vec!["rust-basic".into()];
    let skill = LoadedSkill {
        frontmatter: fm,
        body: "body".into(),
        source: std::path::PathBuf::from("/tmp/rust-async/SKILL.md"),
        scope: Scope::User,
    };
    let dag = SkillDag::from_skills(&[skill]);
    assert_eq!(dag.len(), 1);
    assert!(dag
        .prerequisites("rust-async")
        .contains(&"rust-basic".to_string()));
}
/// conditional：paths 条件激活（glob 匹配、无条件放行、缓存）。
#[test]
fn conditional_activation_path_matching() {
    let with_paths = |paths: Vec<String>| LoadedSkill {
        frontmatter: SkillFrontmatter {
            name: "cond".into(),
            description: "conditional".into(),
            paths,
            ..Default::default()
        },
        body: String::new(),
        source: std::path::PathBuf::from("/dev/null"),
        scope: Scope::User,
    };
    // 无 paths → 无条件激活
    let unconditional = with_paths(vec![]);
    assert!(!has_path_condition(&unconditional));
    assert!(path_matches_skill(
        &unconditional,
        std::path::Path::new("/any/file.txt")
    ));
    // **/*.rs 匹配 rs 文件，不匹配 py
    let rust_only = with_paths(vec!["**/*.rs".into()]);
    assert!(has_path_condition(&rust_only));
    assert!(path_matches_skill(
        &rust_only,
        std::path::Path::new("/src/main.rs")
    ));
    assert!(path_matches_skill(
        &rust_only,
        std::path::Path::new("main.rs")
    ));
    assert!(!path_matches_skill(
        &rust_only,
        std::path::Path::new("/src/main.py")
    ));
    // 无条件 skill 的 check_path_condition 恒 Ok
    assert!(check_path_condition(&unconditional, "").is_ok());
    // PathPatternCache 汇总多技能模式
    let skills = vec![
        with_paths(vec!["**/*.rs".into()]),
        with_paths(vec!["**/*.py".into(), "**/*.toml".into()]),
        with_paths(vec![]),
    ];
    let cache = PathPatternCache::new(&skills);
    assert_eq!(cache.pattern_count(), 3);
    assert!(cache.matches_any(std::path::Path::new("/src/main.rs")));
    assert!(cache.matches_any(std::path::Path::new("/Cargo.toml")));
    assert!(!cache.matches_any(std::path::Path::new("/src/main.txt")));
}
/// hot_reload：轮询对比能检测新建（Created）与无变化两种情形。
#[test]
fn hot_reload_poll_detects_created_skills() {
    let dir = tempfile::tempdir().unwrap();
    // watcher 与写入方分别持有同一 root 的 store 实例（store 仅持有路径）
    let watcher = SkillWatcher::new(FsSkillStore::new(dir.path().to_path_buf(), None));
    let write_store = FsSkillStore::new(dir.path().to_path_buf(), None);
    let mut known: HashMap<String, SkillSnapshot> = HashMap::new();
    assert!(watcher.poll_changes(&mut known).is_empty());
    put_skill(&write_store, "hot-new", "hot reload");
    let events = watcher.poll_changes(&mut known);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert!(
        matches!(e, openpaw_memory::skills::hot_reload::SkillChangeEvent::Created { name, .. } if name == "hot-new"),
        "expected Created event, got {e:?}"
    );
    // 无变化
    assert!(watcher.poll_changes(&mut known).is_empty());
}
/// tool 入口合同 + 失败路径（经 run 直接调用，std-only block_on 驱动）。
#[test]
fn tool_run_contract_and_approval_gate() {
    // env 变量进程级共享，串行化本测试（文件内其余测试不读该变量）
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // ── 场景 1：无许可管理器的写操作 → fail-closed，不落盘 ──
    let dir1 = tempfile::tempdir().unwrap();
    std::env::set_var("OPENPAW_HOME", dir1.path());
    let out = block_on(tool::run(
        dir1.path(),
        serde_json::json!({
            "action": "create",
            "name": "denied-skill",
            "description": "should not persist",
            "body": "body"
        }),
        None,
    ))
    .expect("run must not return Err");
    assert!(out.is_error, "create without approval manager must fail");
    assert!(
        out.content.contains("fail-closed"),
        "content: {}",
        out.content
    );
    let denied_store = FsSkillStore::new(dir1.path().join("skills"), None);
    assert!(denied_store.get("denied-skill").unwrap().is_none());
    // ── 场景 2：AutoApproveManager 放行 → 创建成功、可读回、来源标记 Agent ──
    let dir2 = tempfile::tempdir().unwrap();
    std::env::set_var("OPENPAW_HOME", dir2.path());
    let approve = AutoApproveManager;
    let out = block_on(tool::run(
        dir2.path(),
        serde_json::json!({
            "action": "create",
            "name": "approved-skill",
            "description": "approved creation",
            "triggers": ["t1"],
            "body": "# Approved\nbody text"
        }),
        Some(&approve as &dyn ApprovalManager),
    ))
    .expect("run must not return Err");
    assert!(
        !out.is_error,
        "approved create must succeed: {}",
        out.content
    );
    // 读回：get 动作返回描述与正文
    let out = block_on(tool::run(
        dir2.path(),
        serde_json::json!({"action": "get", "name": "approved-skill"}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("approved creation"));
    assert!(out.content.contains("body text"));
    // 列表合同：list 动作（只读，无需许可）
    let out = block_on(tool::run(
        dir2.path(),
        serde_json::json!({"action": "list"}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("approved-skill"));
    // 来源标记：agent 创建的技能应记录 SkillProvenance::Agent
    let telemetry = FsTelemetryStore::new(dir2.path().join("skills"));
    let entry = telemetry
        .get("approved-skill")
        .unwrap()
        .expect("telemetry entry");
    assert_eq!(entry.created_by, SkillProvenance::Agent);
    // 清理 env，避免影响同进程其他测试
    std::env::remove_var("OPENPAW_HOME");
}
/// std-only 的最小 block_on：驱动仅在首次 poll 内即完成的 future
/// （tool::run 的许可管理器在测试中同步返回，不会真正挂起）。
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Flag(AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }
    let flag = Arc::new(Flag(AtomicBool::new(false)));
    let waker = Waker::from(flag.clone());
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        flag.0.store(false, Ordering::Release);
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        while !flag.0.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}
