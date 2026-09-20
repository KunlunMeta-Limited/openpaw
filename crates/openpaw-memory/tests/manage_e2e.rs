//! End-to-end acceptance for the unified `manage` entry and the standalone
//! memory / skill tool entries:
//! - domain routing: memory put→get roundtrip, skill create→list;
//! - fail-closed: write actions without an approval manager (or with a
//!   denying one) return an error outcome and persist nothing;
//! - unknown domain / unknown action → error outcome;
//! - one contract each for the standalone `memory_tool::run` and
//!   `skills::tool::run` entries.
use openpaw_core::{ApprovalManager, AutoApproveManager, AutoDenyManager};
use openpaw_memory::manage;
use openpaw_memory::memory_tool;
use openpaw_memory::skills::store::{FsSkillStore, SkillStore};
use openpaw_memory::skills::tool as skill_tool;
use openpaw_memory::{FsMemoryStore, MemoryStore};
/// std-only minimal block_on: drives futures that complete within a few
/// polls (the approval managers used here return synchronously).
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
/// OPENPAW_HOME is process-global: serialise every test that touches it.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn set_home(dir: &std::path::Path) {
    std::env::set_var("OPENPAW_HOME", dir);
}
fn clear_home() {
    std::env::remove_var("OPENPAW_HOME");
}
/// Extract the memory id from a successful `set` outcome
/// ("Memory '<id>' created in zone '<zone>' at <path>").
fn extract_created_id(content: &str) -> String {
    let start = content.find("Memory '").expect("created message") + "Memory '".len();
    let end = content[start..].find('\'').expect("closing quote") + start;
    content[start..end].to_string()
}
// ============================================================================
// manage 域路由：memory put→get 往返
// ============================================================================
#[test]
fn manage_routes_memory_put_get_roundtrip() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_home(dir.path());
    let approve = AutoApproveManager;
    // put（写动作，经许可门放行）
    let out = block_on(manage::run(
        dir.path(),
        serde_json::json!({
            "domain": "memory",
            "action": "set",
            "content": "openpaw keeps facts in markdown",
            "zone": "facts"
        }),
        Some(&approve as &dyn ApprovalManager),
    ))
    .expect("manage::run must not return Err");
    assert!(
        !out.is_error,
        "managed memory set must succeed: {}",
        out.content
    );
    let id = extract_created_id(&out.content);
    // get（只读，无需许可）→ 正文与来源可读回
    let out = block_on(manage::run(
        dir.path(),
        serde_json::json!({"domain": "memory", "action": "get", "id": id}),
        None,
    ))
    .unwrap();
    assert!(
        !out.is_error,
        "managed memory get must succeed: {}",
        out.content
    );
    assert!(out.content.contains("openpaw keeps facts in markdown"));
    assert!(out.content.contains("Source: Agent"), "{}", out.content);
    assert!(out.content.contains("Zone: facts"));
    // 落盘位置：user 域 = <OPENPAW_HOME>/memories（与 cli 域规则一致）
    let store = FsMemoryStore::new(dir.path().join("memories"), None);
    assert_eq!(MemoryStore::list_active(&store).unwrap().len(), 1);
    clear_home();
}
// ============================================================================
// manage 域路由：skill create→list
// ============================================================================
#[test]
fn manage_routes_skill_create_list() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_home(dir.path());
    let approve = AutoApproveManager;
    let out = block_on(manage::run(
        dir.path(),
        serde_json::json!({
            "domain": "skill",
            "action": "create",
            "name": "manage-made",
            "description": "created through the manage entry",
            "triggers": ["manage"],
            "body": "# Manage made\nbody text"
        }),
        Some(&approve as &dyn ApprovalManager),
    ))
    .expect("manage::run must not return Err");
    assert!(
        !out.is_error,
        "managed skill create must succeed: {}",
        out.content
    );
    let out = block_on(manage::run(
        dir.path(),
        serde_json::json!({"domain": "skill", "action": "list"}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("manage-made"), "{}", out.content);
    clear_home();
}
// ============================================================================
// fail-closed：写动作无许可管理器 / 许可被拒 → error outcome 且不落盘
// ============================================================================
#[test]
fn manage_write_actions_fail_closed_without_approval_manager() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // ── memory 域：无许可管理器 → fail-closed，不落盘 ──
    let dir1 = tempfile::tempdir().unwrap();
    set_home(dir1.path());
    let out = block_on(manage::run(
        dir1.path(),
        serde_json::json!({
            "domain": "memory",
            "action": "set",
            "content": "must not persist",
            "zone": "general"
        }),
        None,
    ))
    .expect("manage::run must not return Err");
    assert!(out.is_error, "memory set without manager must fail");
    assert!(
        out.content.contains("fail-closed"),
        "content: {}",
        out.content
    );
    let store = FsMemoryStore::new(dir1.path().join("memories"), None);
    assert!(
        MemoryStore::list_active(&store).unwrap().is_empty(),
        "no memory must be written on fail-closed denial"
    );
    // ── memory 域：许可被拒 → error outcome，不落盘 ──
    let dir2 = tempfile::tempdir().unwrap();
    set_home(dir2.path());
    let deny = AutoDenyManager;
    let out = block_on(manage::run(
        dir2.path(),
        serde_json::json!({
            "domain": "memory",
            "action": "set",
            "content": "denied write",
            "zone": "general"
        }),
        Some(&deny as &dyn ApprovalManager),
    ))
    .unwrap();
    assert!(out.is_error, "denied memory set must be an error outcome");
    assert!(out.content.contains("denied"), "content: {}", out.content);
    let store = FsMemoryStore::new(dir2.path().join("memories"), None);
    assert!(MemoryStore::list_active(&store).unwrap().is_empty());
    // ── skill 域：无许可管理器 → fail-closed，不落盘 ──
    let dir3 = tempfile::tempdir().unwrap();
    set_home(dir3.path());
    let out = block_on(manage::run(
        dir3.path(),
        serde_json::json!({
            "domain": "skill",
            "action": "create",
            "name": "denied-skill",
            "description": "should not persist",
            "body": "body"
        }),
        None,
    ))
    .expect("manage::run must not return Err");
    assert!(out.is_error, "skill create without manager must fail");
    assert!(
        out.content.contains("fail-closed"),
        "content: {}",
        out.content
    );
    let denied_store = FsSkillStore::new(dir3.path().join("skills"), None);
    assert!(denied_store.get("denied-skill").unwrap().is_none());
    clear_home();
}
// ============================================================================
// 错误 domain / 错误 action → error outcome
// ============================================================================
#[test]
fn manage_rejects_unknown_domain_and_action() {
    // 缺失 domain
    let out = block_on(manage::run(
        std::path::Path::new("."),
        serde_json::json!({"action": "list"}),
        None,
    ))
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("domain"), "content: {}", out.content);
    // 未知 domain
    let out = block_on(manage::run(
        std::path::Path::new("."),
        serde_json::json!({"domain": "notes", "action": "list"}),
        None,
    ))
    .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("unknown domain"),
        "content: {}",
        out.content
    );
    // memory 域下的未知 action（由 memory 半边报错）
    let out = block_on(manage::run(
        std::path::Path::new("."),
        serde_json::json!({"domain": "memory", "action": "frobnicate"}),
        None,
    ))
    .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("unknown action"),
        "content: {}",
        out.content
    );
}
// ============================================================================
// memory_tool 独立入口合同：set → get → review → remove → get
// ============================================================================
#[test]
fn memory_tool_standalone_contract() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_home(dir.path());
    let approve = AutoApproveManager;
    // set（写动作，经许可门）
    let out = block_on(memory_tool::run(
        dir.path(),
        serde_json::json!({
            "action": "set",
            "content": "use anyhow at the app layer",
            "zone": "conventions",
            "pinned": true
        }),
        Some(&approve as &dyn ApprovalManager),
    ))
    .expect("memory_tool::run must not return Err");
    assert!(!out.is_error, "set must succeed: {}", out.content);
    let id = extract_created_id(&out.content);
    // get（只读）
    let out = block_on(memory_tool::run(
        dir.path(),
        serde_json::json!({"action": "get", "id": id}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("use anyhow at the app layer"));
    assert!(out.content.contains("Source: Agent"));
    // review（写动作：重写 SM-2 字段，经许可门）→ SM-2 进度落盘
    let out = block_on(memory_tool::run(
        dir.path(),
        serde_json::json!({"action": "review", "id": id, "quality": 4}),
        Some(&approve as &dyn ApprovalManager),
    ))
    .unwrap();
    assert!(!out.is_error, "review must succeed: {}", out.content);
    let store = FsMemoryStore::new(dir.path().join("memories"), None);
    let reviewed = MemoryStore::get(&store, &id)
        .unwrap()
        .expect("memory after review");
    assert_eq!(reviewed.frontmatter.sm2_repetition, 1);
    assert!(reviewed.frontmatter.sm2_next_review.is_some());
    // remove（写动作，经许可门）
    let out = block_on(memory_tool::run(
        dir.path(),
        serde_json::json!({"action": "remove", "id": id}),
        Some(&approve as &dyn ApprovalManager),
    ))
    .unwrap();
    assert!(!out.is_error, "remove must succeed: {}", out.content);
    assert!(out.content.contains("deleted successfully"));
    // get → not found
    let out = block_on(memory_tool::run(
        dir.path(),
        serde_json::json!({"action": "get", "id": id}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("not found"));
    clear_home();
}
// ============================================================================
// skills::tool 独立入口合同：create（许可放行）→ get
// ============================================================================
#[test]
fn skill_tool_standalone_contract() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_home(dir.path());
    let approve = AutoApproveManager;
    let out = block_on(skill_tool::run(
        dir.path(),
        serde_json::json!({
            "action": "create",
            "name": "standalone-contract",
            "description": "created via the standalone skill entry",
            "body": "skill body"
        }),
        Some(&approve as &dyn ApprovalManager),
    ))
    .expect("skill_tool::run must not return Err");
    assert!(!out.is_error, "create must succeed: {}", out.content);
    let out = block_on(skill_tool::run(
        dir.path(),
        serde_json::json!({"action": "get", "name": "standalone-contract"}),
        None,
    ))
    .unwrap();
    assert!(!out.is_error);
    assert!(out
        .content
        .contains("created via the standalone skill entry"));
    assert!(out.content.contains("skill body"));
    clear_home();
}
