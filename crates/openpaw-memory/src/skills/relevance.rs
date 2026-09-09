//! Skill relevance matcher.
//!
//! v1: deterministic token overlap. No LLM, no embedding. Cheap enough to
//! re-run on every user turn.
//!
//! Scoring rules:
//! - Tokenise the query on non-alphanumeric boundaries, lowercase, drop
//!   tokens shorter than 3 chars.
//! - For each skill build a token bag from `triggers ∪ name ∪ description`,
//!   same normalisation.
//! - Score = number of distinct query tokens that appear in the skill bag.
//! - Tie-break: more triggers > fewer triggers (trigger-rich skills are
//!   more intentional).
//! - Floor: skills with score 0 are dropped.
//!
//! 移植适配：母仓的 embed 混合排序分支随 embed 特性剥离，本文件只保留
//! 确定性的 Token/双列匹配路径；telemetry 依赖为裁剪版数据结构。

use std::collections::HashSet;

use chrono::Utc;

use crate::skills::{LoadedSkill, SkillTelemetry};

const MIN_TOKEN_LEN: usize = 3;

/// 判断字符是否为 CJK 统一表意文字或日文假名
fn is_cjk(c: char) -> bool {
    let code = c as u32;
    matches!(code,
        0x4E00..=0x9FFF   // CJK 统一表意文字
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0xF900..=0xFAFF // CJK 兼容表意文字
        | 0x3040..=0x309F // 平假名
        | 0x30A0..=0x30FF // 片假名
    )
}

/// 英文停用词集合
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does",
    "did", "will", "would", "could", "should", "may", "might", "shall", "can", "this", "that",
    "these", "those", "i", "you", "he", "she", "it", "we", "they", "what", "which", "who", "when",
    "where", "why", "how", "all", "each", "every", "some", "any", "no", "not", "as", "if", "than",
    "then", "so", "just", "about", "into", "out", "up", "down",
];

/// Return the top-`k` skills relevant to `query`, highest score first.
/// An empty `query` or no matches yields an empty Vec.
pub fn match_for_query<'a>(
    skills: &'a [LoadedSkill],
    query: &str,
    k: usize,
) -> Vec<&'a LoadedSkill> {
    if k == 0 {
        return Vec::new();
    }
    let q = tokenise(query);
    if q.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, usize, &LoadedSkill)> = skills
        .iter()
        .filter_map(|s| {
            let bag = skill_bag(s);
            let score = q.iter().filter(|t| bag.contains(t.as_str())).count();
            if score == 0 {
                None
            } else {
                Some((score, s.frontmatter.triggers.len(), s))
            }
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    scored.into_iter().take(k).map(|(_, _, s)| s).collect()
}

pub fn match_for_query_with_effectiveness<'a>(
    skills: &'a [LoadedSkill],
    query: &str,
    k: usize,
    effectiveness: Option<&std::collections::HashMap<String, SkillTelemetry>>,
) -> Vec<&'a LoadedSkill> {
    if k == 0 {
        return Vec::new();
    }
    let q = tokenise(query);
    if q.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(&LoadedSkill, f64)> = skills
        .iter()
        .filter_map(|s| {
            let bag = skill_bag(s);
            let base_score = q.iter().filter(|t| bag.contains(t.as_str())).count();
            if base_score == 0 {
                None
            } else {
                let eff_factor = effectiveness
                    .and_then(|e| e.get(&s.frontmatter.name))
                    .map(|t| t.factor() * t.decay_factor(Utc::now()))
                    .unwrap_or(1.0);
                let boost = priority_boost(s);
                Some((s, base_score as f64 * eff_factor * boost))
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(k).map(|(s, _)| s).collect()
}

fn priority_boost(skill: &LoadedSkill) -> f64 {
    skill
        .frontmatter
        .priority
        .map_or(1.0, |p| 1.0 + (p as f64) * 0.1)
}

fn tokenise(s: &str) -> HashSet<String> {
    let lower = s.to_lowercase();
    let mut tokens = HashSet::new();

    let mut cjk_buf = String::new();
    let mut ascii_buf = String::new();

    for c in lower.chars() {
        if is_cjk(c) {
            flush_ascii(&mut ascii_buf, &mut tokens);
            cjk_buf.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_buf, &mut tokens);
            ascii_buf.push(c);
        } else {
            flush_cjk(&mut cjk_buf, &mut tokens);
            flush_ascii(&mut ascii_buf, &mut tokens);
        }
    }
    flush_cjk(&mut cjk_buf, &mut tokens);
    flush_ascii(&mut ascii_buf, &mut tokens);

    // 过滤停用词
    tokens.retain(|t| !STOP_WORDS.contains(&t.as_str()));
    tokens
}

/// 将 CJK buffer 转为 bigram tokens
fn flush_cjk(buf: &mut String, tokens: &mut HashSet<String>) {
    if buf.is_empty() {
        return;
    }
    let chars: Vec<char> = buf.chars().collect();
    if chars.len() == 1 {
        // 单个 CJK 字符也作为 token
        tokens.insert(chars[0].to_string());
    } else {
        for window in chars.windows(2) {
            tokens.insert(format!("{}{}", window[0], window[1]));
        }
    }
    buf.clear();
}

/// 将 ASCII buffer 转为 token（需满足最小长度）
fn flush_ascii(buf: &mut String, tokens: &mut HashSet<String>) {
    if buf.is_empty() {
        return;
    }
    if buf.chars().count() >= MIN_TOKEN_LEN {
        tokens.insert(buf.clone());
    }
    buf.clear();
}

fn skill_bag(s: &LoadedSkill) -> HashSet<String> {
    let mut bag = HashSet::new();
    for t in &s.frontmatter.triggers {
        bag.extend(tokenise(t));
    }
    bag.extend(tokenise(&s.frontmatter.name));
    bag.extend(tokenise(&s.frontmatter.description));
    // when_to_use 作为 AI 自动匹配依据，加入匹配 bag
    if let Some(ref wtu) = s.frontmatter.when_to_use {
        bag.extend(tokenise(wtu));
    }
    bag
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{LoadedSkill, Scope, SkillFrontmatter, SkillStatus};
    use std::path::PathBuf;

    fn skill(name: &str, desc: &str, triggers: &[&str]) -> LoadedSkill {
        LoadedSkill {
            frontmatter: SkillFrontmatter {
                name: name.to_string(),
                description: desc.to_string(),
                triggers: triggers.iter().map(|s| s.to_string()).collect(),
                status: SkillStatus::Active,
                ..Default::default()
            },
            body: String::new(),
            source: PathBuf::from("/dev/null"),
            scope: Scope::User,
        }
    }

    #[test]
    fn ranks_by_token_overlap() {
        let skills = vec![
            skill(
                "rust-error-handling",
                "Switch unwrap to anyhow",
                &["rust", "anyhow", "unwrap"],
            ),
            skill(
                "python-type-hints",
                "Add type hints to functions",
                &["python", "typing"],
            ),
        ];
        let hits = match_for_query(&skills, "please refactor my Rust unwrap to use anyhow", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].frontmatter.name, "rust-error-handling");
    }

    #[test]
    fn empty_query_returns_empty() {
        let skills = vec![skill("rust-error-handling", "x", &["rust"])];
        assert!(match_for_query(&skills, "", 3).is_empty());
        assert!(match_for_query(&skills, "  !! ", 3).is_empty());
    }

    #[test]
    fn no_matches_returns_empty() {
        let skills = vec![skill("rust-x", "x", &["rust"])];
        let hits = match_for_query(&skills, "javascript debugging", 3);
        assert!(hits.is_empty());
    }

    #[test]
    fn k_zero_returns_empty() {
        let skills = vec![skill("rust-x", "rust", &["rust"])];
        let hits = match_for_query(&skills, "rust", 0);
        assert!(hits.is_empty());
    }

    #[test]
    fn top_k_caps_results() {
        let skills = vec![
            skill("a", "rust", &["rust"]),
            skill("b", "rust", &["rust"]),
            skill("c", "rust", &["rust"]),
            skill("d", "rust", &["rust"]),
        ];
        let hits = match_for_query(&skills, "rust", 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn drops_short_tokens() {
        // "go" is 2 chars, below MIN_TOKEN_LEN=3, so a query of just "go"
        // produces no tokens and matches nothing.
        let skills = vec![skill("go-formatting", "go fmt", &["go"])];
        let hits = match_for_query(&skills, "go", 3);
        assert!(hits.is_empty());
    }

    #[test]
    fn tie_breaks_on_trigger_count() {
        // Both score 1 on "rust"; sk2 has more triggers and should win.
        let sk1 = skill("a", "rust thing", &["rust"]);
        let sk2 = skill("b", "rust", &["rust", "ownership", "borrow"]);
        let skills = vec![sk1, sk2];
        let hits = match_for_query(&skills, "rust", 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].frontmatter.name, "b");
    }

    #[test]
    fn priority_boost_applied() {
        let mut sk_high = skill("high-priority", "rust", &["rust"]);
        sk_high.frontmatter.priority = Some(10);
        let sk_low = skill("low-priority", "rust", &["rust"]);
        let skills = vec![sk_low, sk_high];
        let hits = match_for_query_with_effectiveness(&skills, "rust", 2, None);
        assert_eq!(hits[0].frontmatter.name, "high-priority");
    }

    #[test]
    fn priority_boost_function_values() {
        let mut sk = skill("test", "test", &["test"]);
        sk.frontmatter.priority = None;
        assert!((priority_boost(&sk) - 1.0).abs() < f64::EPSILON);
        sk.frontmatter.priority = Some(5);
        assert!((priority_boost(&sk) - 1.5).abs() < f64::EPSILON);
        sk.frontmatter.priority = Some(10);
        assert!((priority_boost(&sk) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn effectiveness_decay_prefers_recently_used() {
        // 使用遥测数据时，最近使用的技能应排在陈旧技能之前。
        let skills = vec![
            skill("old-skill", "rust programming", &["rust"]),
            skill("new-skill", "rust coding", &["rust"]),
        ];
        let mut telemetry = std::collections::HashMap::new();
        telemetry.insert(
            "old-skill".to_string(),
            SkillTelemetry {
                last_used_at: Some(Utc::now() - chrono::Duration::days(90)),
                ..Default::default()
            },
        );
        telemetry.insert(
            "new-skill".to_string(),
            SkillTelemetry {
                last_used_at: Some(Utc::now()),
                ..Default::default()
            },
        );
        let hits = match_for_query_with_effectiveness(&skills, "rust", 2, Some(&telemetry));
        assert_eq!(hits[0].frontmatter.name, "new-skill");
    }

    #[test]
    fn when_to_use_participates_in_matching() {
        // when_to_use 字段应参与 skill_bag 匹配
        let mut sk = skill("code-review", "review", &[]);
        sk.frontmatter.when_to_use = Some("User asks to review code or audit code quality".into());
        let skills = vec![sk];
        // "audit" 只出现在 when_to_use 中，不在 name/description/triggers 中
        let hits = match_for_query(&skills, "please audit my code quality", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].frontmatter.name, "code-review");
    }

    // ===== CJK Bigram 分词测试 =====

    #[test]
    fn test_cjk_bigram() {
        let tokens = tokenise("帮我优化代码");
        assert!(tokens.contains("帮我"));
        assert!(tokens.contains("我优"));
        assert!(tokens.contains("优化"));
        assert!(tokens.contains("化代"));
        assert!(tokens.contains("代码"));
        assert!(tokens.len() >= 4);
    }

    #[test]
    fn test_mixed_cjk_ascii() {
        let tokens = tokenise("帮我 optimize database");
        assert!(tokens.contains("帮我"));
        assert!(tokens.contains("optimize"));
        assert!(tokens.contains("database"));
    }

    #[test]
    fn test_stop_words_filtered() {
        let tokens = tokenise("the database is optimize");
        assert!(!tokens.contains("the"));
        assert!(!tokens.contains("is"));
        assert!(tokens.contains("database"));
        assert!(tokens.contains("optimize"));
    }

    #[test]
    fn test_single_cjk_char() {
        let tokens = tokenise("查");
        assert!(tokens.contains("查"));
    }

    #[test]
    fn test_pure_ascii_unchanged() {
        let tokens = tokenise("optimize database performance");
        assert!(tokens.contains("optimize"));
        assert!(tokens.contains("database"));
        assert!(tokens.contains("performance"));
    }

    #[test]
    fn test_empty_string() {
        let tokens = tokenise("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_japanese_kana() {
        let tokens = tokenise("データベース");
        // 片假名也应生成 bigram
        assert!(!tokens.is_empty());
    }

    #[test]
    fn test_cjk_with_punctuation() {
        let tokens = tokenise("帮我，优化代码！");
        assert!(tokens.contains("帮我"));
        assert!(tokens.contains("优化"));
        assert!(tokens.contains("代码"));
    }

    #[test]
    fn test_cjk_query_matches_chinese_skill() {
        let sk = skill("db-optimize", "优化数据库查询性能", &["数据库", "优化"]);
        let skills = vec![sk];
        let matches = match_for_query(&skills, "帮我优化数据库", 5);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].frontmatter.name, "db-optimize");
    }

    #[test]
    fn test_match_for_query_english_unchanged() {
        // 回归测试：英文匹配行为不退化
        let skills = vec![skill(
            "rust-error-handling",
            "Switch unwrap to anyhow",
            &["rust", "anyhow", "unwrap"],
        )];
        let hits = match_for_query(&skills, "please refactor my Rust unwrap to use anyhow", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].frontmatter.name, "rust-error-handling");
    }
}
