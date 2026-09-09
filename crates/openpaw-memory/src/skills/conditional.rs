//! 条件激活管理器 — 带 paths 的 skill 仅在文件路径匹配时激活。
//!
//! 条件激活检查在 skill 调用入口内部执行，
//! 不修改 ContextSources 或 skill 索引区。

use std::path::Path;

use crate::skills::LoadedSkill;

/// 极简 glob 匹配：支持 `**`（跨目录段）、`*`（段内任意字符）、`?`
/// （单字符）。openpaw-memory 依赖清单中没有 glob crate，这里以约
/// 五十行实现覆盖 paths 字段实际用到的 gitignore 风格子集。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let t: Vec<&str> = text.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&p, &t)
}

fn match_segments(p: &[&str], t: &[&str]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some(&"**") => (0..=t.len()).any(|n| match_segments(&p[1..], &t[n..])),
        Some(pat) => {
            if t.is_empty() {
                return false;
            }
            match_segment(pat, t[0]) && match_segments(&p[1..], &t[1..])
        }
    }
}

fn match_segment(pat: &str, text: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    let tc: Vec<char> = text.chars().collect();
    match_chars(&pc, &tc)
}

fn match_chars(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') => (0..=t.len()).any(|n| match_chars(&p[1..], &t[n..])),
        Some('?') => !t.is_empty() && match_chars(&p[1..], &t[1..]),
        Some(&c) => !t.is_empty() && t[0] == c && match_chars(&p[1..], &t[1..]),
    }
}

/// 检查 skill 是否有条件激活限制（即声明了 paths）
pub fn has_path_condition(skill: &LoadedSkill) -> bool {
    !skill.frontmatter.paths.is_empty()
}

/// 检查给定路径是否匹配 skill 的 paths 模式
///
/// 返回 true 如果：
/// - skill 没有 paths 声明（无条件激活，始终可用）
/// - skill 有 paths 声明且路径匹配其中任一模式
pub fn path_matches_skill(skill: &LoadedSkill, path: &Path) -> bool {
    if skill.frontmatter.paths.is_empty() {
        return true; // 无条件激活
    }
    let path_str = path.to_string_lossy().replace('\\', "/");
    for pattern_str in &skill.frontmatter.paths {
        if glob_match(pattern_str, &path_str) {
            return true;
        }
        // 也尝试匹配路径的文件名部分
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            if glob_match(pattern_str, filename) {
                return true;
            }
        }
    }
    false
}

/// 从 skill 调用参数中提取路径
///
/// args 可能是：
/// - 纯文件路径："/path/to/file.rs"
/// - 带描述的文本："review /path/to/file.rs"
///   我们尝试提取看起来像文件路径的部分
pub fn extract_path_from_args(args: &str) -> Option<std::path::PathBuf> {
    if args.is_empty() {
        return None;
    }
    // 尝试将整个 args 当作路径
    let trimmed = args.trim();
    let candidate = Path::new(trimmed);
    if candidate.is_file() || candidate.is_dir() {
        return Some(candidate.to_path_buf());
    }
    // 尝试从 args 中提取路径样式的 token
    for token in trimmed.split_whitespace() {
        let token = token.trim_matches(|c: char| c == '\'' || c == '"' || c == '`' || c == ',');
        let candidate = Path::new(token);
        if candidate.is_file() || candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        // 如果 token 包含 / 或 \ 且看起来像路径，也接受
        if (token.contains('/') || token.contains('\\'))
            && !token.contains("://") // 排除 URL
            && token.len() > 2
        {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

/// 检查 skill 是否满足条件激活要求
///
/// 返回 Ok(()) 如果可以执行，返回 Err(reason) 如果不满足条件
pub fn check_path_condition(skill: &LoadedSkill, args: &str) -> Result<(), String> {
    if !has_path_condition(skill) {
        return Ok(());
    }

    // 尝试从 args 中提取路径
    if let Some(path) = extract_path_from_args(args) {
        if path_matches_skill(skill, &path) {
            return Ok(());
        }
        return Err(format!(
            "Skill '{}' requires paths matching {:?}, but the provided path '{}' does not match",
            skill.frontmatter.name,
            skill.frontmatter.paths,
            path.display()
        ));
    }

    // args 中没有路径 — 检查是否允许无条件调用
    // 设计决策：如果 skill 有 paths 但 args 中没有路径，仍允许调用
    // （skill 可能在 body 中自行处理路径，不依赖 args）。
    Ok(())
}

/// 获取所有 paths 模式编译后的缓存（用于批量检查）
pub struct PathPatternCache {
    patterns: Vec<String>,
}

impl PathPatternCache {
    pub fn new(skills: &[LoadedSkill]) -> Self {
        let mut patterns = Vec::new();
        for skill in skills {
            for p in &skill.frontmatter.paths {
                patterns.push(p.clone());
            }
        }
        Self { patterns }
    }

    /// 检查路径是否匹配任一模式
    pub fn matches_any(&self, path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let path_str = path.to_string_lossy().replace('\\', "/");
        self.patterns.iter().any(|p| glob_match(p, &path_str))
    }

    /// 所有已收集的模式数量
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{LoadedSkill, Scope, SkillFrontmatter};
    use std::path::PathBuf;

    fn make_skill(name: &str, paths: Vec<String>) -> LoadedSkill {
        LoadedSkill {
            frontmatter: SkillFrontmatter {
                name: name.to_string(),
                description: "test".to_string(),
                paths,
                ..Default::default()
            },
            body: String::new(),
            source: PathBuf::from("/dev/null"),
            scope: Scope::User,
        }
    }

    #[test]
    fn test_has_path_condition() {
        let with_paths = make_skill("a", vec!["**/*.rs".into()]);
        let without_paths = make_skill("b", vec![]);
        assert!(has_path_condition(&with_paths));
        assert!(!has_path_condition(&without_paths));
    }

    #[test]
    fn test_path_matches_rs_files() {
        let skill = make_skill("rust-review", vec!["**/*.rs".into()]);
        assert!(path_matches_skill(&skill, Path::new("/src/main.rs")));
        assert!(path_matches_skill(&skill, Path::new("main.rs")));
        assert!(!path_matches_skill(&skill, Path::new("/src/main.py")));
    }

    #[test]
    fn test_path_matches_no_paths_always_true() {
        let skill = make_skill("general", vec![]);
        assert!(path_matches_skill(&skill, Path::new("/any/file.txt")));
    }

    #[test]
    fn test_path_matches_multiple_patterns() {
        let skill = make_skill("multi", vec!["**/*.rs".into(), "**/*.toml".into()]);
        assert!(path_matches_skill(&skill, Path::new("/Cargo.toml")));
        assert!(path_matches_skill(&skill, Path::new("/src/lib.rs")));
        assert!(!path_matches_skill(&skill, Path::new("/src/lib.py")));
    }

    #[test]
    fn test_check_path_condition_no_paths() {
        let skill = make_skill("general", vec![]);
        assert!(check_path_condition(&skill, "").is_ok());
        assert!(check_path_condition(&skill, "anything").is_ok());
    }

    #[test]
    fn test_check_path_condition_with_matching_path() {
        let skill = make_skill("rust-review", vec!["**/*.rs".into()]);
        // 由于实际文件可能不存在，extract_path_from_args 可能返回 None
        // 但 check_path_condition 仍应返回 Ok（允许调用）
        let result = check_path_condition(&skill, "src/main.rs");
        assert!(
            result.is_ok(),
            "should allow even if path doesn't exist on disk"
        );
    }

    #[test]
    fn test_extract_path_from_args_empty() {
        assert!(extract_path_from_args("").is_none());
    }

    #[test]
    fn test_extract_path_from_args_url_rejected() {
        assert!(extract_path_from_args("https://example.com").is_none());
    }

    #[test]
    fn test_path_pattern_cache() {
        let skills = vec![
            make_skill("a", vec!["**/*.rs".into()]),
            make_skill("b", vec!["**/*.py".into(), "**/*.toml".into()]),
            make_skill("c", vec![]),
        ];
        let cache = PathPatternCache::new(&skills);
        assert_eq!(cache.pattern_count(), 3); // 1 + 2 + 0
        assert!(cache.matches_any(Path::new("/src/main.rs")));
        assert!(cache.matches_any(Path::new("/src/main.py")));
        assert!(!cache.matches_any(Path::new("/src/main.txt")));
    }

    #[test]
    fn test_windows_path_normalization() {
        // Windows 路径中的 \ 应被标准化为 / 后匹配
        let skill = make_skill("win-skill", vec!["src/**/*.rs".into()]);
        // path_matches_skill 内部将 \ 转为 /
        assert!(path_matches_skill(&skill, Path::new("src\\sub\\main.rs")));
    }

    #[test]
    fn test_path_matches_filename_only() {
        // 仅文件名也能匹配
        let skill = make_skill("test-skill", vec!["*.test.rs".into()]);
        assert!(path_matches_skill(
            &skill,
            Path::new("/some/dir/my.test.rs")
        ));
    }

    #[test]
    fn test_glob_match_double_star_zero_segments() {
        // ** 可以匹配零个目录段
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(glob_match("**", "anything/else/here"));
        assert!(!glob_match("**/*.rs", "main.py"));
    }
}
