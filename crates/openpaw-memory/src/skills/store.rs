//! Filesystem-backed skill store.
//!
//! Layout under each scope root:
//! ```text
//! <root>/
//!   <skill-name>/
//!     SKILL.md
//!     scripts/        # optional, untouched by this store
//! ```
//!
//! `<root>` is `~/.openpaw/skills/` for [`Scope::User`] and
//! `./.openpaw/skills/` for [`Scope::Project`]. Project-scoped
//! skills override user-scoped skills with the same name.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::skills::skill::{LoadedSkill, Scope, SkillFrontmatter, SkillStatus};
use openpaw_store::{FrontmatterDoc, FrontmatterError};

const SKILL_FILE: &str = "SKILL.md";

pub struct BundledSkillDef {
    pub name: &'static str,
    pub description: &'static str,
    pub triggers: &'static str,
    pub priority: Option<u32>,
    pub tags: &'static str,
    pub author: Option<&'static str>,
    pub category: Option<&'static str>,
    pub when_to_use: Option<&'static str>,
}

pub const BUNDLED_SKILLS: &[BundledSkillDef] = &[
    BundledSkillDef { name: "code-style", description: "Enforce consistent code style and formatting", triggers: "rust,format,style", priority: Some(5), tags: "code-quality,formatting", author: None, category: Some("code-quality"), when_to_use: Some("User asks to format code or enforce consistent style") },
    BundledSkillDef { name: "error-handling", description: "Switch unwrap to proper error handling with anyhow/thiserror", triggers: "rust,error,anyhow", priority: Some(7), tags: "error-handling,robustness", author: None, category: Some("code-quality"), when_to_use: Some("User asks to improve error handling or fix unwrap calls") },
    BundledSkillDef { name: "test-first", description: "Write tests before implementing features (TDD)", triggers: "test,tdd,testing", priority: Some(6), tags: "testing,methodology", author: None, category: Some("testing"), when_to_use: Some("User asks to write tests or follow TDD methodology") },
    BundledSkillDef { name: "security-review", description: "Review code for security vulnerabilities", triggers: "security,review,vulnerability", priority: Some(8), tags: "security,review", author: None, category: Some("security"), when_to_use: Some("User asks to review code for security issues or vulnerabilities") },
    BundledSkillDef { name: "performance", description: "Identify and fix performance bottlenecks", triggers: "performance,optimization,speed", priority: Some(4), tags: "performance,optimization", author: None, category: Some("performance"), when_to_use: Some("User asks to optimize performance or fix slow code") },
    BundledSkillDef { name: "documentation", description: "Add or improve code documentation", triggers: "docs,documentation,comments", priority: Some(3), tags: "documentation,comments", author: None, category: Some("documentation"), when_to_use: Some("User asks to add or improve documentation and comments") },
    BundledSkillDef { name: "refactor", description: "Restructure code without changing behavior", triggers: "refactor,cleanup,restructure", priority: Some(4), tags: "refactoring,cleanup", author: None, category: Some("code-quality"), when_to_use: Some("User asks to refactor, clean up, or restructure code") },
    BundledSkillDef { name: "git-workflow", description: "Follow git best practices (commit messages, branching)", triggers: "git,commit,branch", priority: Some(3), tags: "git,workflow", author: None, category: Some("workflow"), when_to_use: Some("User asks about git workflow, commit messages, or branching strategy") },
    // Workflow orchestration knowledge skill
    BundledSkillDef { name: "workflow-orchestration", description: "Multi-agent workflow orchestration patterns and best practices", triggers: "workflow,pipeline,parallel,crosscheck,orchestration,multi-agent", priority: Some(6), tags: "workflow,orchestration,multi-agent,pipeline", author: None, category: Some("workflow"), when_to_use: Some("User asks about multi-agent workflows, pipeline orchestration, parallel execution, or cross-check verification") },
];

pub fn install_bundled_skills(
    store: &dyn SkillStore,
    scope: Scope,
) -> Vec<std::result::Result<PathBuf, SkillStoreError>> {
    let mut results = Vec::new();
    for def in BUNDLED_SKILLS {
        if store.get(def.name).unwrap_or(None).is_some() {
            continue;
        }
        let fm = SkillFrontmatter {
            name: def.name.to_string(),
            description: def.description.to_string(),
            triggers: def.triggers.split(',').map(|s| s.to_string()).collect(),
            version: Some("1.0.0".into()),
            license: Some("MIT".into()),
            status: SkillStatus::Active,
            priority: def.priority,
            tags: def.tags.split(',').map(|s| s.to_string()).collect(),
            author: def.author.map(|s| s.to_string()),
            category: def.category.map(|s| s.to_string()),
            source: Some("bundled".into()),
            when_to_use: def.when_to_use.map(|s| s.to_string()),
            ..Default::default()
        };
        let body = bundled_skill_body(def.name, def.description, def.triggers);
        results.push(store.put(scope, fm, &body));
    }
    results
}

/// Returns the body content for a bundled skill.
/// Most skills use a simple template; the workflow-orchestration skill
/// has a rich body with orchestration patterns and best practices.
fn bundled_skill_body(name: &str, description: &str, triggers: &str) -> String {
    if name == "workflow-orchestration" {
        return WORKFLOW_ORCHESTRATION_SKILL_BODY.to_string();
    }
    format!(
        "# {}\n\n{}\n\n## Triggers\n{}\n",
        name,
        description,
        triggers
            .split(',')
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Rich body content for the workflow-orchestration skill.
const WORKFLOW_ORCHESTRATION_SKILL_BODY: &str = r#"# Workflow Orchestration

Multi-agent workflow orchestration patterns and best practices.

## When to Use Workflows
- Task can be decomposed into 3+ independent subtasks
- Multi-perspective cross-validation is needed
- A single context window cannot hold all information
- Auditable, recoverable execution is required

## When NOT to Use Workflows
- Simple sequential operations (use delegate_task directly)
- Subtasks have strong dependencies and cannot parallelize
- Single-turn tasks with sufficient context

## Orchestration Primitives

| Primitive | Use Case | OpenPaw StepAction |
|-----------|----------|---------------------|
| Pipeline | Multi-item × multi-stage pipeline | `Pipeline` |
| Parallel | Independent subtasks in parallel | `Parallel` |
| CrossCheck | Adversarial cross-validation | `CrossCheck` |
| SubAgent | Single sub-agent dispatch | `SubAgent` |
| Loop | Iterate until condition met | `Loop` |
| DynamicPlan | LLM dynamically generates steps | `DynamicPlan` |

## Quality Pattern Library

### adversarial-verify (Adversarial Verification)
Producer generates claims → Reviewer independently votes → Synthesizer consolidates surviving claims.
Best for: Code review, security analysis, fact-checking.

### judge-panel (Review Panel)
Multiple independent agents each give judgment → Majority vote.
Best for: Design decisions, approach selection.

### loop-until-dry (Loop Until No Findings)
Loop step repeatedly executes agent → Until output is empty or max_iterations reached.
Best for: Progressive fixes, iterative optimization.

### completeness-critic (Completeness Critic)
One agent produces → Another agent checks for gaps → Supplements.
Best for: Requirements coverage, test coverage.

## Concurrency Control
- Default `max_agent_concurrency=3`
- IO-intensive tasks can go up to 8-10
- API rate-limited scenarios keep 3-5
- Absolute maximum is 16

## Resume and Budget
- Workflows support crash recovery (WorkflowRecovery)
- Agent result caching avoids re-execution on resume
- `budget.total` controls the token hard cap
- When exhausted, agent calls throw errors

## Named Workflows
- Place in `.openpaw/workflows/<name>.json`
- Automatically registered as `/<name>` slash command
- Arguments can be passed through the command

## Pipeline Pattern
Pipeline allows items to flow through stages without barriers between items:
- Item 1 can be in stage 2 while item 2 is still in stage 1
- `failure_mode: "skip_item"` continues on single item failure
- `failure_mode: "abort_all"` fails the entire pipeline on any error
- Use `${item.field}` and `${prev}` for template interpolation

## Triggers
- workflow
- pipeline
- parallel
- crosscheck
- orchestration
- multi-agent
"#;

/// Install bundled skills and register them with the provenance guard.
pub fn install_bundled_skills_with_provenance(
    store: &dyn SkillStore,
    scope: Scope,
    provenance_root: &std::path::Path,
) -> Vec<std::result::Result<PathBuf, SkillStoreError>> {
    let results = install_bundled_skills(store, scope);
    let mut guard = crate::skills::provenance::ProvenanceGuard::new(provenance_root);
    for def in BUNDLED_SKILLS {
        // Register provenance for every bundled skill that exists on disk
        // (whether just installed or pre-existing). The previous logic
        // inverted this check — it skipped registration when the skill
        // existed, which is exactly when registration is needed.
        if store.get(def.name).unwrap_or(None).is_none() {
            continue;
        }
        let _ = guard.register_bundled(def.name, provenance_root);
    }
    results
}

#[derive(Debug, Error)]
pub enum SkillStoreError {
    #[error("invalid skill name {0:?}: must match [A-Za-z0-9_-]+ and contain no path separators")]
    InvalidName(String),

    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, SkillStoreError>;

pub trait SkillStore: Send + Sync {
    /// All skills, with project scope taking precedence on name collisions.
    fn list(&self) -> Result<Vec<LoadedSkill>>;

    /// Look up a single skill by name. Project beats user on collision.
    fn get(&self, name: &str) -> Result<Option<LoadedSkill>>;

    /// Persist a skill to the chosen scope. Returns the written path.
    fn put(&self, scope: Scope, frontmatter: SkillFrontmatter, body: &str) -> Result<PathBuf>;

    /// Remove a skill. Returns true if it existed.
    fn delete(&self, scope: Scope, name: &str) -> Result<bool>;
}

/// Filesystem implementation backed by two scope roots.
pub struct FsSkillStore {
    user_root: PathBuf,
    project_root: Option<PathBuf>,
}

impl FsSkillStore {
    pub fn new(user_root: PathBuf, project_root: Option<PathBuf>) -> Self {
        Self {
            user_root,
            project_root,
        }
    }

    /// Conventional locations: `~/.openpaw/skills` (user) and
    /// `./.openpaw/skills` (project, if a `./.openpaw`
    /// directory exists).
    ///
    /// **Deprecated:** Prefer explicit roots via [`FsSkillStore::new`].
    /// This method uses CWD-based project scope which does not respect
    /// workspace boundaries.
    #[deprecated(since = "0.1.0", note = "use `FsSkillStore::new` with explicit roots")]
    pub fn standard() -> Result<Self> {
        let user = standard_user_root()?;
        let project = standard_project_root();
        Ok(Self::new(user, project))
    }

    /// 功能：严格只读列出显式根的技能，项目同名项覆盖用户项。
    /// 参数：self为已有显式根Store。
    /// 返回：按技能名排序的原LoadedSkill。
    /// 错误：目录读取、坏frontmatter、根外链接返回原存储错误。
    /// 状态：不创建目录、不更新遥测或生命周期。
    /// 并发与取消：同步读取；宿主在阻塞任务内调用并等待完成。
    /// 权限：每个文件规范化后必须留在对应显式技能根内。
    /// 关联：需求/设计REQ-OP-014；DES-OP-006。
    pub fn list_readonly_checked(&self) -> Result<Vec<LoadedSkill>> {
        let mut result = std::collections::BTreeMap::new();
        for (root, scope) in std::iter::once((&self.user_root, Scope::User))
            .chain(self.project_root.as_ref().map(|r| (r, Scope::Project)))
        {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(SkillStoreError::Io {
                        path: root.clone(),
                        source,
                    })
                }
            };
            let canonical = std::fs::canonicalize(root).map_err(|source| SkillStoreError::Io {
                path: root.clone(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| SkillStoreError::Io {
                    path: root.clone(),
                    source,
                })?;
                let path = entry.path();
                let resolved =
                    std::fs::canonicalize(&path).map_err(|source| SkillStoreError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if !resolved.starts_with(&canonical) {
                    return Err(SkillStoreError::Config("skill path escapes root".into()));
                }
                if !resolved.is_dir() {
                    continue;
                }
                let file = path.join(SKILL_FILE);
                let resolved = match std::fs::canonicalize(&file) {
                    Ok(p) => p,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(source) => return Err(SkillStoreError::Io { path: file, source }),
                };
                if !resolved.starts_with(&canonical) {
                    return Err(SkillStoreError::Config("skill file escapes root".into()));
                }
                let skill = Self::load_one(&resolved, scope)?;
                result.insert(skill.name().to_owned(), skill);
            }
        }
        Ok(result.into_values().collect())
    }

    fn root_for(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::User => Some(&self.user_root),
            Scope::Project => self.project_root.as_deref(),
        }
    }

    fn skill_path(&self, scope: Scope, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        let root = self.root_for(scope).ok_or_else(|| {
            SkillStoreError::Config("project scope requested but no project root configured".into())
        })?;
        Ok(root.join(name).join(SKILL_FILE))
    }

    fn load_one(path: &Path, scope: Scope) -> Result<LoadedSkill> {
        let doc: FrontmatterDoc<SkillFrontmatter> = openpaw_store::read_doc(path)?;
        Ok(LoadedSkill {
            frontmatter: doc.frontmatter,
            body: doc.body,
            source: path.to_path_buf(),
            scope,
        })
    }

    fn list_scope(root: &Path, scope: Scope) -> Result<Vec<LoadedSkill>> {
        if !root.exists() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(root).map_err(|source| SkillStoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| SkillStoreError::Io {
                path: root.to_path_buf(),
                source,
            })?;
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let skill_md = dir.join(SKILL_FILE);
            if !skill_md.is_file() {
                continue;
            }
            if let Ok(s) = Self::load_one(&skill_md, scope) {
                out.push(s);
            }
        }
        Ok(out)
    }
}

impl SkillStore for FsSkillStore {
    fn list(&self) -> Result<Vec<LoadedSkill>> {
        let mut user = Self::list_scope(&self.user_root, Scope::User)?;
        let project = match &self.project_root {
            Some(root) => Self::list_scope(root, Scope::Project)?,
            None => Vec::new(),
        };
        // Project overrides user on name collision.
        let project_names: std::collections::HashSet<&str> =
            project.iter().map(|s| s.name()).collect();
        user.retain(|s| !project_names.contains(s.name()));
        user.extend(project);
        user.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(user)
    }

    fn get(&self, name: &str) -> Result<Option<LoadedSkill>> {
        validate_name(name)?;
        if let Some(root) = &self.project_root {
            let p = root.join(name).join(SKILL_FILE);
            if p.is_file() {
                return Ok(Some(Self::load_one(&p, Scope::Project)?));
            }
        }
        let p = self.user_root.join(name).join(SKILL_FILE);
        if p.is_file() {
            return Ok(Some(Self::load_one(&p, Scope::User)?));
        }
        Ok(None)
    }

    fn put(&self, scope: Scope, frontmatter: SkillFrontmatter, body: &str) -> Result<PathBuf> {
        if frontmatter.name.is_empty() {
            return Err(SkillStoreError::InvalidName(frontmatter.name.clone()));
        }
        let path = self.skill_path(scope, &frontmatter.name)?;
        let doc = FrontmatterDoc {
            frontmatter,
            body: body.to_string(),
        };
        openpaw_store::write_doc_atomic(&path, &doc)?;
        Ok(path)
    }

    fn delete(&self, scope: Scope, name: &str) -> Result<bool> {
        validate_name(name)?;
        let root = self.root_for(scope).ok_or_else(|| {
            SkillStoreError::Config("project scope requested but no project root configured".into())
        })?;
        let dir = root.join(name);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(SkillStoreError::Io { path: dir, source }),
        }
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(SkillStoreError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// `~/.openpaw/skills`.
pub fn standard_user_root() -> Result<PathBuf> {
    Ok(openpaw_core::openpaw_home().join("skills"))
}

/// `./.openpaw/skills` if the project dir exists; else `None`.
pub fn standard_project_root() -> Option<PathBuf> {
    let p = PathBuf::from(".openpaw").join("skills");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yml::Mapping;

    fn fm(name: &str, desc: &str) -> SkillFrontmatter {
        SkillFrontmatter {
            name: name.to_string(),
            description: desc.to_string(),
            triggers: vec!["foo".into(), "bar".into()],
            version: Some("0.1.0".into()),
            license: None,
            status: crate::skills::skill::SkillStatus::Active,
            superseded_by: None,
            priority: None,
            tags: Vec::new(),
            author: None,
            category: None,
            pinned: false,
            source: None,
            prerequisites: Vec::new(),
            parent: None,
            difficulty: None,
            concepts: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn put_get_list_delete_user_scope() {
        let user = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(user.path().to_path_buf(), None);

        let path = store
            .put(Scope::User, fm("alpha", "first"), "Body of alpha.\n")
            .unwrap();
        assert!(path.ends_with("alpha/SKILL.md"));

        let loaded = store.get("alpha").unwrap().expect("alpha should exist");
        assert_eq!(loaded.name(), "alpha");
        assert_eq!(loaded.frontmatter.description, "first");
        assert_eq!(loaded.body.trim(), "Body of alpha.");
        assert_eq!(loaded.scope, Scope::User);

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name(), "alpha");

        assert!(store.delete(Scope::User, "alpha").unwrap());
        assert!(store.get("alpha").unwrap().is_none());
        assert!(!store.delete(Scope::User, "alpha").unwrap());
    }

    #[test]
    fn project_overrides_user() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(
            user.path().to_path_buf(),
            Some(project.path().to_path_buf()),
        );

        store
            .put(Scope::User, fm("shared", "from user"), "user body")
            .unwrap();
        store
            .put(Scope::Project, fm("shared", "from project"), "project body")
            .unwrap();
        store
            .put(Scope::User, fm("only-user", "u"), "u body")
            .unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);

        let shared = store.get("shared").unwrap().unwrap();
        assert_eq!(shared.scope, Scope::Project);
        assert_eq!(shared.frontmatter.description, "from project");
    }

    #[test]
    fn rejects_bad_names() {
        let user = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(user.path().to_path_buf(), None);

        for bad in ["..", "../etc", "a/b", ".hidden", "", "with space", "weird?"] {
            let err = store.put(Scope::User, fm(bad, "x"), "y").unwrap_err();
            assert!(
                matches!(err, SkillStoreError::InvalidName(_)),
                "name {bad:?} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn preserves_unknown_frontmatter_keys() {
        let user = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(user.path().to_path_buf(), None);

        let mut extra = Mapping::new();
        extra.insert(
            serde_yml::Value::String("model".into()),
            serde_yml::Value::String("claude-haiku-4-5-20251001".into()),
        );

        let mut fm = fm("with-extras", "test");
        fm.extra = extra;
        store.put(Scope::User, fm, "body").unwrap();

        let loaded = store.get("with-extras").unwrap().unwrap();
        assert_eq!(
            loaded
                .frontmatter
                .extra
                .get(serde_yml::Value::String("model".into())),
            Some(&serde_yml::Value::String(
                "claude-haiku-4-5-20251001".into()
            ))
        );
    }

    #[test]
    fn project_scope_requires_project_root() {
        let user = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(user.path().to_path_buf(), None);

        let err = store.put(Scope::Project, fm("x", "y"), "z").unwrap_err();
        assert!(matches!(err, SkillStoreError::Config(_)));
    }

    #[test]
    fn empty_root_returns_empty_list() {
        let user = tempfile::tempdir().unwrap();
        let store = FsSkillStore::new(user.path().join("does-not-exist"), None);
        let listed = store.list().unwrap();
        assert!(listed.is_empty());
    }
}
