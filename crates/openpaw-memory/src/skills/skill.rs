//! Skill data types. Aligned with Anthropic's `SKILL.md` convention so
//! skills can be exchanged between this agent and Claude Code skills.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    #[default]
    Active,
    Stale,
    Archived,
    Merged,
    Reactivated,
}

/// Skill execution context — inline (default) or fork (sub-agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillContext {
    /// Execute inline — skill body returned as tool result (default).
    #[default]
    Inline,
    /// Execute in a forked sub-agent via delegate_task.
    Fork,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,

    pub description: String,

    #[serde(default)]
    pub triggers: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,

    #[serde(default)]
    pub status: SkillStatus,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,

    #[serde(default)]
    pub priority: Option<u32>,

    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default)]
    pub author: Option<String>,

    #[serde(default)]
    pub category: Option<String>,

    #[serde(default)]
    pub pinned: bool,

    #[serde(default)]
    pub source: Option<String>,

    #[serde(default)]
    pub prerequisites: Vec<String>,

    #[serde(default)]
    pub parent: Option<String>,

    #[serde(default)]
    pub difficulty: Option<f64>,

    #[serde(default)]
    pub concepts: Vec<String>,

    // ── 新增：执行控制字段 ──
    /// AI 自动匹配依据，注入 SkillTool prompt 列表。
    /// Claude Code 兼容别名：`when-to-use`。
    #[serde(
        default,
        alias = "when-to-use",
        skip_serializing_if = "Option::is_none"
    )]
    pub when_to_use: Option<String>,

    /// 参数提示，显示在 skill_invoke 工具描述中。
    /// Claude Code 兼容别名：`argument-hint`。
    #[serde(
        default,
        alias = "argument-hint",
        skip_serializing_if = "Option::is_none"
    )]
    pub argument_hint: Option<String>,

    /// 声明式参数名，用于 $ARGUMENTS 替换。
    #[serde(default, alias = "arguments", skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,

    // ── 新增：权限与执行模式字段 ──
    /// 工具白名单，执行时注入权限上下文。
    /// Claude Code 兼容别名：`allowed-tools`。
    #[serde(
        default,
        alias = "allowed-tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_tools: Vec<String>,

    /// 执行模式：inline（默认）或 fork。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<SkillContext>,

    /// fork 模式下指定的 Agent 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// 是否禁止 LLM 自主调用（仅用户可触发）。
    /// Claude Code 兼容别名：`disable-model-invocation`。
    #[serde(default, alias = "disable-model-invocation")]
    pub disable_model_invocation: bool,

    /// 是否用户可调用（默认 true）。
    /// Claude Code 兼容别名：`user-invocable`。
    #[serde(
        default = "default_true",
        alias = "user-invocable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub user_invocable: bool,

    /// 条件激活的文件路径模式（gitignore 风格）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    #[serde(flatten)]
    pub extra: serde_yml::Mapping,
}

impl Default for SkillFrontmatter {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            triggers: Vec::new(),
            version: None,
            license: None,
            status: SkillStatus::Active,
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
            when_to_use: None,
            argument_hint: None,
            arguments: Vec::new(),
            allowed_tools: Vec::new(),
            context: None,
            agent: None,
            disable_model_invocation: false,
            user_invocable: true,
            paths: Vec::new(),
            extra: serde_yml::Mapping::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
    pub source: PathBuf,
    pub scope: Scope,
}

impl LoadedSkill {
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_frontmatter_fields_default_on_deserialize() {
        let yaml = r#"
name: test-skill
description: "A test"
triggers:
  - foo
status: active
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.name, "test-skill");
        assert!(fm.priority.is_none());
        assert!(fm.tags.is_empty());
        assert!(fm.author.is_none());
        assert!(fm.category.is_none());
        assert!(!fm.pinned);
        assert!(fm.source.is_none());
        assert!(fm.prerequisites.is_empty());
        assert!(fm.parent.is_none());
        assert!(fm.difficulty.is_none());
        assert!(fm.concepts.is_empty());
    }

    #[test]
    fn new_frontmatter_fields_roundtrip() {
        let fm = SkillFrontmatter {
            name: "advanced-rust".to_string(),
            description: "Advanced Rust".to_string(),
            triggers: vec!["rust".into()],
            version: Some("2.0.0".into()),
            license: Some("MIT".into()),
            status: SkillStatus::Active,
            priority: Some(10),
            tags: vec!["rust".into(), "advanced".into()],
            author: Some("alice".into()),
            category: Some("programming".into()),
            pinned: true,
            source: Some("hub".into()),
            prerequisites: vec!["rust-basic".into()],
            parent: Some("rust".into()),
            difficulty: Some(0.8),
            concepts: vec!["ownership".into(), "lifetimes".into()],
            when_to_use: Some("User asks about advanced Rust".into()),
            argument_hint: Some("<topic>".into()),
            allowed_tools: vec!["read_file".into(), "grep".into()],
            context: Some(SkillContext::Fork),
            disable_model_invocation: false,
            user_invocable: true,
            paths: vec!["**/*.rs".into()],
            ..Default::default()
        };
        let yaml = serde_yml::to_string(&fm).unwrap();
        let back: SkillFrontmatter = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(back.priority, Some(10));
        assert_eq!(back.tags, vec!["rust", "advanced"]);
        assert_eq!(back.author, Some("alice".into()));
        assert_eq!(back.category, Some("programming".into()));
        assert!(back.pinned);
        assert_eq!(back.source, Some("hub".into()));
        assert_eq!(back.prerequisites, vec!["rust-basic"]);
        assert_eq!(back.parent, Some("rust".into()));
        assert_eq!(back.difficulty, Some(0.8));
        assert_eq!(back.concepts, vec!["ownership", "lifetimes"]);
        // 新字段 roundtrip
        assert_eq!(
            back.when_to_use.as_deref(),
            Some("User asks about advanced Rust")
        );
        assert_eq!(back.argument_hint.as_deref(), Some("<topic>"));
        assert_eq!(back.allowed_tools, vec!["read_file", "grep"]);
        assert_eq!(back.context, Some(SkillContext::Fork));
        assert_eq!(back.paths, vec!["**/*.rs"]);
    }

    #[test]
    fn parse_claude_code_style_frontmatter() {
        let yaml = r#"
name: code-review
description: Systematic code review
when-to-use: "User asks to review code"
allowed-tools:
  - read_file
  - grep
  - glob
context: fork
argument-hint: "<file-or-directory>"
disable-model-invocation: false
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.name, "code-review");
        assert_eq!(fm.when_to_use.as_deref(), Some("User asks to review code"));
        assert_eq!(fm.allowed_tools, vec!["read_file", "grep", "glob"]);
        assert_eq!(fm.context, Some(SkillContext::Fork));
        assert_eq!(fm.argument_hint.as_deref(), Some("<file-or-directory>"));
        assert!(!fm.disable_model_invocation);
        assert!(fm.user_invocable); // 默认 true
    }

    #[test]
    fn default_impl_values() {
        let fm = SkillFrontmatter::default();
        assert!(fm.name.is_empty());
        assert!(fm.when_to_use.is_none());
        assert!(fm.argument_hint.is_none());
        assert!(fm.arguments.is_empty());
        assert!(fm.allowed_tools.is_empty());
        assert!(fm.context.is_none());
        assert!(fm.agent.is_none());
        assert!(!fm.disable_model_invocation);
        assert!(fm.user_invocable);
        assert!(fm.paths.is_empty());
    }

    #[test]
    fn parse_snake_case_frontmatter() {
        let yaml = r#"
name: test-skill
description: A test
when_to_use: "When testing"
allowed_tools:
  - read_file
context: inline
argument_hint: "<arg>"
disable_model_invocation: true
user_invocable: false
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.when_to_use.as_deref(), Some("When testing"));
        assert_eq!(fm.allowed_tools, vec!["read_file"]);
        assert_eq!(fm.context, Some(SkillContext::Inline));
        assert_eq!(fm.argument_hint.as_deref(), Some("<arg>"));
        assert!(fm.disable_model_invocation);
        assert!(!fm.user_invocable);
    }

    #[test]
    fn parse_mixed_case_frontmatter() {
        // 混合 kebab-case 和 snake_case
        let yaml = r#"
name: mixed-skill
description: Mixed case test
when-to-use: "Mixed case"
allowed_tools:
  - grep
paths:
  - "**/*.rs"
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.when_to_use.as_deref(), Some("Mixed case"));
        assert_eq!(fm.allowed_tools, vec!["grep"]);
        assert_eq!(fm.paths, vec!["**/*.rs"]);
    }

    #[test]
    fn parse_minimal_frontmatter() {
        let yaml = r#"
name: minimal
description: Minimal skill
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.name, "minimal");
        assert_eq!(fm.description, "Minimal skill");
        assert!(fm.when_to_use.is_none());
        assert!(fm.allowed_tools.is_empty());
        assert!(fm.context.is_none());
        assert!(fm.paths.is_empty());
        assert!(!fm.disable_model_invocation);
        assert!(fm.user_invocable);
    }

    #[test]
    fn parse_unknown_fields_preserved_in_extra() {
        let yaml = r#"
name: test
description: test
model: claude-sonnet-4
effort: high
custom_field: value
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.name, "test");
        // 未知字段应保留在 extra 中
        assert!(fm.extra.contains_key("model"));
        assert!(fm.extra.contains_key("effort"));
        assert!(fm.extra.contains_key("custom_field"));
    }

    #[test]
    fn default_context_is_inline() {
        let fm = SkillFrontmatter::default();
        assert_eq!(fm.context, None);
        assert_eq!(fm.context.unwrap_or_default(), SkillContext::Inline);
    }

    #[test]
    fn parse_context_fork() {
        let yaml = r#"
name: fork-skill
description: Fork skill
context: fork
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.context, Some(SkillContext::Fork));
    }

    #[test]
    fn parse_context_inline() {
        let yaml = r#"
name: inline-skill
description: Inline skill
context: inline
"#;
        let fm: SkillFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.context, Some(SkillContext::Inline));
    }
}
