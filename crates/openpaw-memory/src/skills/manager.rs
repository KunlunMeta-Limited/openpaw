//! 统一的技能管理门面：组合 SkillStore + SkillLifecycle + FsTelemetryStore。

use std::path::Path;

use crate::skills::{
    FsSkillStore, FsTelemetryStore, LoadedSkill, Scope, SkillFrontmatter, SkillLifecycle,
    SkillStatus, SkillStore, SkillStoreError, SkillTelemetryState,
};

type StoreResult<T> = std::result::Result<T, SkillStoreError>;

pub struct SkillManager {
    store: FsSkillStore,
    telemetry: FsTelemetryStore,
}

impl SkillManager {
    /// Bind the manager to an arbitrary root: both the store and the
    /// telemetry file live under `<root>/.openpaw/skills/`.
    pub fn new(root: &Path) -> Self {
        let skills_root = root.join(".openpaw").join("skills");
        let store = FsSkillStore::new(skills_root.clone(), None);
        let telemetry = FsTelemetryStore::new(skills_root);
        Self { store, telemetry }
    }

    /// Bind the manager to a workspace: the user scope is the global
    /// `~/.openpaw/skills/` directory, the project scope is the
    /// workspace-local `<workspace>/.openpaw/skills/` (enabled only when
    /// the directory exists). Telemetry follows the project scope when
    /// present, otherwise the user scope.
    pub fn for_workspace(workspace: &Path) -> StoreResult<Self> {
        let user_root = openpaw_core::openpaw_home().join("skills");
        let project_dir = workspace.join(".openpaw").join("skills");
        let project_root = if project_dir.exists() {
            Some(project_dir)
        } else {
            None
        };
        let store = FsSkillStore::new(user_root.clone(), project_root.clone());
        let telemetry_root = project_root.unwrap_or(user_root);
        let telemetry = FsTelemetryStore::new(telemetry_root);
        Ok(Self { store, telemetry })
    }

    // -- CRUD --

    pub fn create(&self, scope: Scope, fm: &SkillFrontmatter, body: &str) -> StoreResult<()> {
        self.store.put(scope, fm.clone(), body)?;
        Ok(())
    }

    pub fn edit(&self, scope: Scope, fm: &SkillFrontmatter, body: &str) -> StoreResult<()> {
        self.store.put(scope, fm.clone(), body)?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> StoreResult<()> {
        // Try user scope first, then project
        if self.store.delete(Scope::User, name)? {
            return Ok(());
        }
        self.store.delete(Scope::Project, name)?;
        Ok(())
    }

    // -- Lifecycle --

    /// Detect the scope a skill lives in. Falls back to User if not found
    /// (the caller will get a "not found" error from the lifecycle method).
    fn scope_of(&self, name: &str) -> StoreResult<Scope> {
        Ok(self
            .store
            .get(name)?
            .map(|s| s.scope)
            .unwrap_or(Scope::User))
    }

    pub fn archive(&self, name: &str) -> StoreResult<()> {
        let scope = self.scope_of(name)?;
        self.store.archive(scope, name)
    }

    pub fn restore(&self, name: &str) -> StoreResult<()> {
        let scope = self.scope_of(name)?;
        self.store.restore(scope, name)
    }

    pub fn mark_stale(&self, name: &str) -> StoreResult<()> {
        let scope = self.scope_of(name)?;
        self.store.mark_stale(scope, name)
    }

    pub fn reactivate(&self, name: &str) -> StoreResult<()> {
        let scope = self.scope_of(name)?;
        self.store.reactivate(scope, name)
    }

    // -- Pin management --

    pub fn pin(&self, name: &str) -> StoreResult<()> {
        let mut skill = self
            .store
            .get(name)?
            .ok_or_else(|| SkillStoreError::Config(format!("skill '{name}' not found")))?;
        skill.frontmatter.pinned = true;
        self.store
            .put(skill.scope, skill.frontmatter, &skill.body)?;
        // 遥测更新失败不影响主流程
        let _ = self.telemetry.set_pinned(name, true);
        Ok(())
    }

    pub fn unpin(&self, name: &str) -> StoreResult<()> {
        let mut skill = self
            .store
            .get(name)?
            .ok_or_else(|| SkillStoreError::Config(format!("skill '{name}' not found")))?;
        skill.frontmatter.pinned = false;
        self.store
            .put(skill.scope, skill.frontmatter, &skill.body)?;
        // 遥测更新失败不影响主流程
        let _ = self.telemetry.set_pinned(name, false);
        Ok(())
    }

    pub fn is_pinned(&self, name: &str) -> bool {
        self.store
            .get(name)
            .ok()
            .flatten()
            .map(|s| s.frontmatter.pinned)
            .unwrap_or(false)
    }

    // -- List / Search --

    pub fn list_active(&self) -> StoreResult<Vec<LoadedSkill>> {
        // 惰性触发 30/90 天 Stale/Archived 自动生命周期转换（裁剪版
        // telemetry 保留了该轻量入口；每天最多执行一次，由 telemetry
        // store 内部用日期标记文件节流）。转换失败由入口自行吞掉，
        // 不影响技能列表的正常返回。
        let _ = self.telemetry.apply_automatic_transitions_daily(30, 90);
        let all = self.store.list()?;
        Ok(all
            .into_iter()
            .filter(|s| {
                matches!(
                    s.frontmatter.status,
                    SkillStatus::Active | SkillStatus::Reactivated
                )
            })
            .collect())
    }

    pub fn search(&self, query: &str) -> StoreResult<Vec<LoadedSkill>> {
        let active = self.list_active()?;
        let effectiveness = self.telemetry.load().ok();

        // telemetry 已 Archived（长期未用自动转换）的技能不再参与推荐，
        // 避免过期技能一直以 Active 身份被 search 命中。
        // 无 telemetry 记录的技能不受影响。
        let active: Vec<LoadedSkill> = active
            .into_iter()
            .filter(|s| {
                !matches!(
                    effectiveness
                        .as_ref()
                        .and_then(|e| e.get(&s.frontmatter.name)),
                    Some(t) if t.state == SkillTelemetryState::Archived
                )
            })
            .collect();

        let matches = match effectiveness {
            Some(ref eff) => crate::skills::relevance::match_for_query_with_effectiveness(
                &active,
                query,
                active.len(),
                Some(eff),
            ),
            None => crate::skills::relevance::match_for_query(&active, query, active.len()),
        };
        Ok(matches.into_iter().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillTelemetry;

    fn make_fm(name: &str, desc: &str) -> SkillFrontmatter {
        SkillFrontmatter {
            name: name.to_string(),
            description: desc.to_string(),
            triggers: vec!["test".into()],
            ..Default::default()
        }
    }

    #[test]
    fn create_and_list_active() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("my-skill", "A test skill");
        mgr.create(Scope::User, &fm, "body").unwrap();

        let active = mgr.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name(), "my-skill");
    }

    #[test]
    fn pin_unpin() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("pin-skill", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();

        assert!(!mgr.is_pinned("pin-skill"));
        mgr.pin("pin-skill").unwrap();
        assert!(mgr.is_pinned("pin-skill"));
        mgr.unpin("pin-skill").unwrap();
        assert!(!mgr.is_pinned("pin-skill"));
    }

    #[test]
    fn archive_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("arch-skill", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();

        mgr.archive("arch-skill").unwrap();
        let active = mgr.list_active().unwrap();
        assert!(active.is_empty());

        mgr.restore("arch-skill").unwrap();
        let active = mgr.list_active().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn pin_prevents_archive() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("protected", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();
        mgr.pin("protected").unwrap();

        let result = mgr.archive("protected");
        assert!(result.is_err());
    }

    #[test]
    fn mark_stale_then_reactivate() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("stale-skill", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();

        mgr.mark_stale("stale-skill").unwrap();
        let active = mgr.list_active().unwrap();
        assert!(
            active.is_empty(),
            "stale skill should not be in active list"
        );

        mgr.reactivate("stale-skill").unwrap();
        let active = mgr.list_active().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn mark_stale_on_archived_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("arch-first", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();
        mgr.archive("arch-first").unwrap();

        // Cannot mark an archived skill as stale — must restore first
        let result = mgr.mark_stale("arch-first");
        assert!(result.is_err());
    }

    /// list_active 应触发 30/90 天自动生命周期转换。
    #[test]
    fn list_active_triggers_automatic_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("aging-skill", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();

        // 写入一条 35 天未用的 telemetry 记录
        let mut data = std::collections::HashMap::new();
        data.insert(
            "aging-skill".to_string(),
            SkillTelemetry {
                last_used_at: Some(chrono::Utc::now() - chrono::Duration::days(35)),
                ..Default::default()
            },
        );
        mgr.telemetry.save(&data).unwrap();

        // list_active 惰性触发每日转换
        let _ = mgr.list_active().unwrap();

        let t = mgr.telemetry.get("aging-skill").unwrap().unwrap();
        assert_eq!(
            t.state,
            SkillTelemetryState::Stale,
            "list_active should have triggered the 30/90-day transition"
        );
    }

    /// search 不再返回 telemetry 已 Archived 的技能（过期技能
    /// 不应一直以 Active 被推荐）。
    #[test]
    fn search_excludes_telemetry_archived_skills() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("rust-coding", "Rust programming");
        mgr.create(Scope::User, &fm, "Rust best practices").unwrap();

        // 标记为 telemetry Archived（模拟长期未用自动转换后的状态）
        let mut data = std::collections::HashMap::new();
        data.insert(
            "rust-coding".to_string(),
            SkillTelemetry {
                state: SkillTelemetryState::Archived,
                ..Default::default()
            },
        );
        mgr.telemetry.save(&data).unwrap();

        // frontmatter 仍是 Active，但 search 不应推荐它
        let results = mgr.search("rust").unwrap();
        assert!(
            results.iter().all(|s| s.frontmatter.name != "rust-coding"),
            "telemetry-archived skill should not be recommended by search"
        );

        // list_active 仍返回它（frontmatter 状态未变，由 curator/用户决定归档）
        let active = mgr.list_active().unwrap();
        assert!(active.iter().any(|s| s.frontmatter.name == "rust-coding"));
    }

    /// 验证 SkillManager::search 通过 Token 匹配工作（无 embed 依赖）。
    #[test]
    fn e2e_search_works_without_embed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm1 = make_fm("rust-error", "Rust error handling");
        mgr.create(Scope::User, &fm1, "Use anyhow for error handling")
            .unwrap();
        let fm2 = make_fm("python-test", "Python testing");
        mgr.create(Scope::User, &fm2, "Use pytest for testing")
            .unwrap();

        let results = mgr.search("rust error").unwrap();
        assert!(!results.is_empty(), "search should return results");
        assert!(
            results.iter().any(|s| s.frontmatter.name == "rust-error"),
            "search should find rust-error"
        );
    }

    /// 验证 SkillManager::search 空查询返回空结果。
    #[test]
    fn e2e_search_empty_query_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("test-skill", "test");
        mgr.create(Scope::User, &fm, "body").unwrap();

        let results = mgr.search("").unwrap();
        assert!(results.is_empty(), "empty query should return no results");
    }

    /// 验证 SkillManager::search 无技能时返回空。
    #[test]
    fn e2e_search_no_skills_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let results = mgr.search("anything").unwrap();
        assert!(results.is_empty(), "no skills should return no results");
    }

    /// 验证 search 无 embed 时多次调用稳定（Token 匹配降级安全）。
    #[test]
    fn e2e_search_fallback_safe() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new(dir.path());

        let fm = make_fm("rust-coding", "Rust programming");
        mgr.create(Scope::User, &fm, "Rust best practices").unwrap();

        for _ in 0..3 {
            let results = mgr.search("rust").unwrap();
            assert!(!results.is_empty(), "search should be stable");
        }
    }
}
