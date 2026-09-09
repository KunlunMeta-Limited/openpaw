//! 技能子系统。
//!
//! 基础层（skill/store/manifest/permissions/provenance/telemetry/lifecycle）
//! 与编排层（manager/dag/conditional/hot_reload/relevance/tool）均自母仓
//! 同名模块移植，按 OpenPaw 约定做机械适配：
//! telemetry 为裁剪版（数据结构 + FsTelemetryStore 轻量持久化，
//! hub/批量分析面剥离），embed 分支剥离，路径约定 `~/.openpaw`。

// ── 基础层 ──
pub mod lifecycle;
pub mod manifest;
pub mod permissions;
pub mod provenance;
pub mod skill;
pub mod store;
pub mod telemetry;

// ── 编排层与工具入口 ──
pub mod conditional;
pub mod dag;
pub mod hot_reload;
pub mod manager;
pub mod relevance;
pub mod tool;

// ── 基础层 re-export ──
pub use lifecycle::{DuplicateGroup, SkillLifecycle};
pub use manifest::{Compatibility, SkillDependency, SkillManifest};
pub use permissions::{
    check_skill_permission, skill_has_only_safe_properties, SkillPermissionDecision,
    SAFE_SKILL_PROPERTIES,
};
pub use provenance::ProvenanceGuard;
pub use skill::{LoadedSkill, Scope, SkillContext, SkillFrontmatter, SkillStatus};
pub use store::{FsSkillStore, SkillStore, SkillStoreError};
pub use telemetry::{FsTelemetryStore, SkillProvenance, SkillTelemetry, SkillTelemetryState};

// ── 编排层 re-export ──
pub use conditional::{
    check_path_condition, has_path_condition, path_matches_skill, PathPatternCache,
};
pub use dag::{DagValidationResult, LearningPath, LearningPathNode, SkillDag};
pub use hot_reload::{SkillChangeEvent, SkillWatcher};
pub use manager::SkillManager;
pub use relevance::{match_for_query, match_for_query_with_effectiveness};
pub use tool::{run as skill_manage_run, spec as skill_manage_spec};
