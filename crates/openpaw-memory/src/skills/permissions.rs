//! Skill 执行权限模型 — Safe Properties 白名单 + deny/allow 规则。
//!
//! 权限检查在 skill_invoke::run() 中集成。

use crate::skills::LoadedSkill;

/// 安全属性白名单 — 仅含这些属性的 skill 可自动放行
pub const SAFE_SKILL_PROPERTIES: &[&str] = &[
    "name",
    "description",
    "when_to_use",
    "version",
    "triggers",
    "tags",
    "category",
    "author",
    "license",
    "priority",
    "pinned",
    "source",
    "status",
    "scope",
    "argument_hint",
    "arguments",
    "user_invocable",
    "prerequisites",
    "parent",
    "difficulty",
    "concepts",
    "superseded_by",
];

/// 权限决策
#[derive(Debug, Clone)]
pub enum SkillPermissionDecision {
    /// 自动放行
    Allow,
    /// 拒绝执行
    Deny { reason: String },
    /// 需要用户确认
    Ask { reason: String },
}

/// 检查 skill 是否仅含安全属性（无高级执行属性）
pub fn skill_has_only_safe_properties(skill: &LoadedSkill) -> bool {
    let has_allowed_tools = !skill.frontmatter.allowed_tools.is_empty();
    let has_context = skill.frontmatter.context.is_some();
    let has_agent = skill.frontmatter.agent.is_some();
    let has_disable_model = skill.frontmatter.disable_model_invocation;
    let has_paths = !skill.frontmatter.paths.is_empty();

    !(has_allowed_tools || has_context || has_agent || has_disable_model || has_paths)
}

/// 权限检查
///
/// 检查顺序：
/// 1. deny 规则匹配 → Deny
/// 2. allow 规则匹配 → Allow
/// 3. Safe Properties 白名单 → Allow
/// 4. 默认 → Ask
pub fn check_skill_permission(
    skill: &LoadedSkill,
    deny_rules: &[String],
    allow_rules: &[String],
) -> SkillPermissionDecision {
    let name = &skill.frontmatter.name;

    // 1. deny 规则匹配（支持精确匹配和 prefix:* 前缀）
    for rule in deny_rules {
        if matches_rule(rule, name) {
            return SkillPermissionDecision::Deny {
                reason: format!("Skill '{}' blocked by deny rule '{}'", name, rule),
            };
        }
    }

    // 2. allow 规则匹配
    for rule in allow_rules {
        if matches_rule(rule, name) {
            return SkillPermissionDecision::Allow;
        }
    }

    // 3. Safe Properties 白名单
    if skill_has_only_safe_properties(skill) {
        return SkillPermissionDecision::Allow;
    }

    // 4. 默认 ask
    SkillPermissionDecision::Ask {
        reason: format!(
            "Skill '{}' has advanced properties (allowed_tools/context/agent/paths). Confirm execution?",
            name
        ),
    }
}

/// 规则匹配（支持精确和 prefix:* 通配）
fn matches_rule(rule: &str, name: &str) -> bool {
    if rule == name {
        return true;
    }
    if let Some(prefix) = rule.strip_suffix(":*") {
        return name.starts_with(prefix);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::skill::{LoadedSkill, Scope, SkillContext, SkillFrontmatter};
    use std::path::PathBuf;

    fn make_skill(name: &str) -> LoadedSkill {
        LoadedSkill {
            frontmatter: SkillFrontmatter {
                name: name.to_string(),
                description: "test".to_string(),
                ..Default::default()
            },
            body: String::new(),
            source: PathBuf::from("/dev/null"),
            scope: Scope::User,
        }
    }

    fn make_advanced_skill(name: &str) -> LoadedSkill {
        let mut skill = make_skill(name);
        skill.frontmatter.allowed_tools = vec!["read_file".into()];
        skill.frontmatter.context = Some(SkillContext::Fork);
        skill
    }

    #[test]
    fn test_matches_rule_exact() {
        assert!(matches_rule("code-review", "code-review"));
        assert!(!matches_rule("code-review", "code-review-v2"));
    }

    #[test]
    fn test_matches_rule_prefix() {
        assert!(matches_rule("code:*", "code-review"));
        assert!(matches_rule("code:*", "code-audit"));
        assert!(!matches_rule("code:*", "config-review"));
    }

    #[test]
    fn test_safe_skill_allowed() {
        let skill = make_skill("simple");
        let result = check_skill_permission(&skill, &[], &[]);
        assert!(matches!(result, SkillPermissionDecision::Allow));
    }

    #[test]
    fn test_advanced_skill_asks() {
        let skill = make_advanced_skill("advanced");
        let result = check_skill_permission(&skill, &[], &[]);
        assert!(matches!(result, SkillPermissionDecision::Ask { .. }));
    }

    #[test]
    fn test_deny_rule_blocks() {
        let skill = make_skill("dangerous");
        let deny = vec!["dangerous".to_string()];
        let result = check_skill_permission(&skill, &deny, &[]);
        assert!(matches!(result, SkillPermissionDecision::Deny { .. }));
    }

    #[test]
    fn test_allow_rule_overrides() {
        let skill = make_advanced_skill("trusted");
        let allow = vec!["trusted".to_string()];
        let result = check_skill_permission(&skill, &[], &allow);
        assert!(matches!(result, SkillPermissionDecision::Allow));
    }

    #[test]
    fn test_deny_overrides_allow() {
        let skill = make_skill("conflict");
        let deny = vec!["conflict".to_string()];
        let allow = vec!["conflict".to_string()];
        let result = check_skill_permission(&skill, &deny, &allow);
        assert!(matches!(result, SkillPermissionDecision::Deny { .. }));
    }

    #[test]
    fn test_prefix_deny_rule() {
        let skill = make_skill("test-dangerous");
        let deny = vec!["test:*".to_string()];
        let result = check_skill_permission(&skill, &deny, &[]);
        assert!(matches!(result, SkillPermissionDecision::Deny { .. }));
    }

    #[test]
    fn test_skill_has_only_safe_properties_true() {
        let skill = make_skill("simple");
        assert!(skill_has_only_safe_properties(&skill));
    }

    #[test]
    fn test_skill_has_only_safe_properties_false_with_allowed_tools() {
        let mut skill = make_skill("with-tools");
        skill.frontmatter.allowed_tools = vec!["read_file".into()];
        assert!(!skill_has_only_safe_properties(&skill));
    }

    #[test]
    fn test_skill_has_only_safe_properties_false_with_paths() {
        let mut skill = make_skill("with-paths");
        skill.frontmatter.paths = vec!["**/*.rs".into()];
        assert!(!skill_has_only_safe_properties(&skill));
    }

    #[test]
    fn test_skill_has_only_safe_properties_false_with_context() {
        let mut skill = make_skill("fork-skill");
        skill.frontmatter.context = Some(SkillContext::Fork);
        assert!(!skill_has_only_safe_properties(&skill));
    }
}
