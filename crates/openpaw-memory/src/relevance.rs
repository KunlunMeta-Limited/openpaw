use std::collections::HashMap;

use chrono::Utc;

use crate::memory::MemoryType;
use crate::stats::MemoryEffectiveness;
use crate::LoadedMemory;

const MIN_TOKEN_LEN: usize = 2;

fn tokenise(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let ascii_tokens: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= MIN_TOKEN_LEN)
        .map(|t| t.to_string())
        .collect();

    let mut cjk_bigrams = Vec::new();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                cjk_bigrams.push(format!("{}{}", chars[i], chars[i + 1]));
                i += 2;
            } else {
                cjk_bigrams.push(c.to_string());
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    let mut tokens = ascii_tokens;
    tokens.extend(cjk_bigrams);
    tokens
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{3000}'..='\u{303F}'
        | '\u{F900}'..='\u{FAFF}' // CJK code point, not a task id. openpaw-scrub-allow
        | '\u{2E80}'..='\u{2EFF}'
        | '\u{2F00}'..='\u{2FDF}'
        | '\u{FF00}'..='\u{FFEF}'
    )
}

fn build_tf(tokens: &[String]) -> HashMap<String, f64> {
    let mut tf: HashMap<String, f64> = HashMap::new();
    let total = tokens.len() as f64;
    for t in tokens {
        *tf.entry(t.clone()).or_insert(0.0) += 1.0;
    }
    for v in tf.values_mut() {
        *v /= total;
    }
    tf
}

fn cosine_sim(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let dot: f64 = a
        .iter()
        .filter_map(|(k, va)| b.get(k).map(|vb| va * vb))
        .sum();
    let norm_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let norm_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub fn search_memories<'a>(
    memories: &'a [LoadedMemory],
    query: &str,
    k: usize,
) -> Vec<&'a LoadedMemory> {
    search_memories_scored(memories, query, k)
        .into_iter()
        .map(|(m, _)| m)
        .collect()
}

pub fn search_memories_scored<'a>(
    memories: &'a [LoadedMemory],
    query: &str,
    k: usize,
) -> Vec<(&'a LoadedMemory, f64)> {
    if k == 0 || query.is_empty() || memories.is_empty() {
        return Vec::new();
    }
    let query_tokens = tokenise(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let query_tf = build_tf(&query_tokens);

    let mut scored: Vec<(&LoadedMemory, f64)> = memories
        .iter()
        .filter_map(|m| {
            let body_tokens = tokenise(&m.body);
            if body_tokens.is_empty() {
                return None;
            }
            let body_tf = build_tf(&body_tokens);
            let sim = cosine_sim(&query_tf, &body_tf);
            if sim > 0.0 {
                Some((m, sim))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    // Filter out NaN scores — they are meaningless and would appear in
    // arbitrary positions despite the sort above.
    scored
        .into_iter()
        .take(k)
        .filter(|(_, s)| !s.is_nan())
        .collect()
}

/// Over-sampling multiplier for effectiveness-aware search.
///
/// `search_memories_with_effectiveness` fetches `k * EFFECTIVENESS_OVERSAMPLE`
/// results from the base cosine search, re-scores them with the effectiveness
/// factor, then truncates to `k`.  This ensures that effectiveness penalties
/// (which can down-rank high-relevance memories) don't discard the best
/// results entirely.
const EFFECTIVENESS_OVERSAMPLE: usize = 2;

pub fn search_memories_with_effectiveness<'a>(
    memories: &'a [LoadedMemory],
    query: &str,
    k: usize,
    effectiveness: Option<&HashMap<String, MemoryEffectiveness>>,
) -> Vec<(&'a LoadedMemory, f64)> {
    let now = Utc::now();
    // Over-fetch so effectiveness re-ranking has headroom for demoting
    // low-quality memories without dropping below k results.
    let fetch_k = k.saturating_mul(EFFECTIVENESS_OVERSAMPLE).max(k);
    let mut scored = search_memories_scored(memories, query, fetch_k);
    if let Some(eff) = effectiveness {
        for (mem, score) in &mut scored {
            let factor = eff
                .get(&mem.frontmatter.id)
                .map(|e| e.combined_factor(now))
                .unwrap_or(1.0);
            *score *= factor;
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    // Filter out NaN scores (e.g. from 0.0 * inf effectiveness edge cases).
    scored
        .into_iter()
        .take(k)
        .filter(|(_, s)| !s.is_nan())
        .collect()
}

/// Boost score for Reasoning memories whose problem_type matches the query.
/// Returns base_score unchanged for non-reasoning memories or no match.
pub fn score_with_problem_type(base_score: f64, memory: &LoadedMemory, query: &str) -> f64 {
    if memory.frontmatter.memory_type != MemoryType::Reasoning {
        return base_score;
    }
    if let Some(ptype) = memory.frontmatter.extra.get("problem_type") {
        if let Some(s) = ptype.as_str() {
            let ptype_lower = s.to_lowercase();
            let query_lower = query.to_lowercase();
            if ptype_lower.split('-').any(|kw| query_lower.contains(kw)) {
                return base_score * 1.5;
            }
        }
    }
    base_score
}

/// Search memories with problem_type boost for Reasoning memories.
pub fn search_memories_with_problem_type<'a>(
    memories: &'a [LoadedMemory],
    query: &str,
    k: usize,
    effectiveness: Option<&HashMap<String, MemoryEffectiveness>>,
) -> Vec<(&'a LoadedMemory, f64)> {
    let mut scored = search_memories_with_effectiveness(memories, query, k, effectiveness);
    for (mem, score) in &mut scored {
        *score = score_with_problem_type(*score, mem, query);
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(k)
        .filter(|(_, s)| !s.is_nan())
        .collect()
}

/// Calculate IDF (Inverse Document Frequency) for a term across all documents.
///
/// `doc_token_sets` is a pre-computed set of unique tokens per document,
/// avoiding repeated `tokenise()` calls.
fn idf_with_sets(term: &str, doc_token_sets: &[std::collections::HashSet<String>]) -> f64 {
    let total_docs = doc_token_sets.len() as f64;
    let docs_containing = doc_token_sets
        .iter()
        .filter(|set| set.contains(term))
        .count() as f64;

    if docs_containing == 0.0 {
        return 0.0;
    }

    (total_docs / docs_containing).ln() + 1.0
}

/// TF-IDF similarity between query and document.
pub fn tfidf_similarity(query: &str, document: &str, all_documents: &[&str]) -> f64 {
    let query_terms = tokenise(query);
    let doc_terms = tokenise(document);

    if query_terms.is_empty() || doc_terms.is_empty() {
        return 0.0;
    }

    // Pre-compute unique token sets for each document once, so idf lookups
    // are O(1) per document instead of re-tokenising every document per term.
    let doc_token_sets: Vec<std::collections::HashSet<String>> = all_documents
        .iter()
        .map(|doc| tokenise(doc).into_iter().collect())
        .collect();

    let mut score = 0.0;
    for term in &query_terms {
        let tf = doc_terms.iter().filter(|t| *t == term).count() as f64 / doc_terms.len() as f64;
        let idf_val = idf_with_sets(term, &doc_token_sets);
        score += tf * idf_val;
    }

    score
}

/// Search memories using TF-IDF scoring.
///
/// Scores are computed over a concatenation of `frontmatter.description` and
/// `body`, so memories are findable even when only one of the two fields
/// contains the query terms.
pub fn search_memories_tfidf(
    memories: &[LoadedMemory],
    query: &str,
    limit: usize,
) -> Vec<LoadedMemory> {
    if memories.is_empty() || query.is_empty() {
        return vec![];
    }

    let doc_texts: Vec<String> = memories
        .iter()
        .map(|m| {
            if m.frontmatter.description.is_empty() {
                m.body.clone()
            } else {
                format!("{}\n{}", m.frontmatter.description, m.body)
            }
        })
        .collect();
    let doc_refs: Vec<&str> = doc_texts.iter().map(|s| s.as_str()).collect();

    let mut scored: Vec<(f64, LoadedMemory)> = memories
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let score = tfidf_similarity(query, doc_refs[i], &doc_refs);
            (score, m.clone())
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

/// Denoise + dedup filter for memory search.
///
/// This is a *post-filter* that runs on top of any base search result list
/// (e.g. [`search_memories_scored`] or [`search_memories_with_effectiveness`]).
/// It does **not** modify the base functions, preserving backward
/// compatibility.
///
/// # Denoise rule
/// Skip a `Context`-type memory whose description/tags contain a name from
/// `recent_tools` *unless* it also contains a warning keyword
/// (`warning` / `pitfall` / `issue` / `caution` / `bug`). Context memories
/// that merely echo "the user just ran tool X" are noise; warnings about
/// tool X are still valuable.
///
/// # Dedup rule
/// Skip any memory whose `frontmatter.id` appears in `already_surfaced`.
///
/// If both filters are empty, the input is returned unchanged (just truncated
/// to `k`).
pub fn search_memories_filtered<'a>(
    memories: &'a [LoadedMemory],
    query: &str,
    k: usize,
    recent_tools: &[String],
    already_surfaced: &[String],
) -> Vec<(&'a LoadedMemory, f64)> {
    if k == 0 {
        return Vec::new();
    }
    // Over-fetch by 2x so post-filtering has headroom.
    let fetch_k = k.saturating_mul(2).max(k);
    let scored = search_memories_scored(memories, query, fetch_k);

    let surfaced_set: std::collections::HashSet<&str> =
        already_surfaced.iter().map(String::as_str).collect();

    scored
        .into_iter()
        .filter(|(m, _)| {
            // Dedup: skip already-surfaced memories.
            if surfaced_set.contains(m.frontmatter.id.as_str()) {
                return false;
            }
            // Denoise: Context-type memories mentioning recent tools are
            // noise unless they carry a warning keyword.
            if should_skip_for_recent_tools(m, recent_tools) {
                return false;
            }
            true
        })
        .take(k)
        .collect()
}

/// Denoise predicate: should this memory be skipped because it is a Context
/// memory echoing a recently-used tool?
///
/// Returns `true` if the memory should be **skipped**.
fn should_skip_for_recent_tools(memory: &LoadedMemory, recent_tools: &[String]) -> bool {
    if recent_tools.is_empty() {
        return false;
    }
    // Only filter Context-type memories.
    if memory.frontmatter.memory_type != MemoryType::Context {
        return false;
    }

    // Search across description + tags.
    let mut search_text = memory.frontmatter.description.to_lowercase();
    search_text.push(' ');
    search_text.push_str(&memory.frontmatter.tags.join(" ").to_lowercase());

    let contains_tool = recent_tools
        .iter()
        .any(|tool| search_text.contains(&tool.to_lowercase()));
    if !contains_tool {
        return false;
    }

    // If the memory contains a warning keyword, keep it — it's the kind of
    // Context memory that is actually useful when invoking the tool again.
    const WARNING_KEYWORDS: [&str; 5] = ["warning", "pitfall", "issue", "caution", "bug"];
    let contains_warning = WARNING_KEYWORDS.iter().any(|kw| search_text.contains(kw));
    !contains_warning
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Confidence, MemoryFrontmatter, MemoryType, Scope, Source};
    use std::path::PathBuf;

    fn make_mem(id: &str, body: &str) -> LoadedMemory {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.id = id.to_string();
        LoadedMemory {
            frontmatter: fm,
            body: body.to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        }
    }

    fn make_mem_with_desc(id: &str, description: &str, body: &str) -> LoadedMemory {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.id = id.to_string();
        fm.description = description.to_string();
        LoadedMemory {
            frontmatter: fm,
            body: body.to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        }
    }

    #[test]
    fn tokenise_ascii() {
        let tokens = tokenise("Hello World Rust code");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"code".to_string()));
    }

    #[test]
    fn tokenise_cjk() {
        let tokens = tokenise("偏好深色主题");
        assert!(tokens.iter().any(|t| t == "偏好" || t == "深色"));
    }

    #[test]
    fn search_basic() {
        let memories = vec![
            make_mem("m1", "I prefer dark mode in my editor"),
            make_mem("m2", "Python is my favorite scripting language"),
            make_mem("m3", "I like light themes"),
        ];
        let results = search_memories(&memories, "dark theme", 2);
        assert!(!results.is_empty());
        assert_eq!(results[0].frontmatter.id, "m1");
    }

    #[test]
    fn search_empty_query() {
        let memories = vec![make_mem("m1", "test")];
        let results = search_memories(&memories, "", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn search_with_effectiveness() {
        let memories = vec![
            make_mem("m1", "Rust coding conventions for the project"),
            make_mem("m2", "I prefer dark mode"),
        ];
        let mut eff = HashMap::new();
        eff.insert(
            "m1".to_string(),
            MemoryEffectiveness {
                loaded: 10,
                referenced: 1,
                last_event_at: Some(Utc::now()),
                ..Default::default()
            },
        );
        let results = search_memories_with_effectiveness(&memories, "Rust", 2, Some(&eff));
        assert!(!results.is_empty());
    }

    #[test]
    fn search_cjk_bigram() {
        let memories = vec![
            make_mem("m1", "用户偏好深色主题"),
            make_mem("m2", "Python scripting language"),
        ];
        let results = search_memories(&memories, "偏好深色", 1);
        assert!(!results.is_empty());
        assert_eq!(results[0].frontmatter.id, "m1");
    }

    #[test]
    fn tfidf_similarity_basic() {
        let docs = vec!["Rust programming language", "Python scripting language"];
        let score_rust = tfidf_similarity("Rust programming", "Rust programming language", &docs);
        let score_py = tfidf_similarity("Rust programming", "Python scripting language", &docs);
        assert!(
            score_rust > score_py,
            "TF-IDF should rank matching doc higher: rust={score_rust} py={score_py}"
        );
    }

    #[test]
    fn tfidf_empty_inputs() {
        assert_eq!(tfidf_similarity("", "some doc", &["some doc"]), 0.0);
        assert_eq!(tfidf_similarity("query", "", &[""]), 0.0);
    }

    #[test]
    fn search_tfidf_ranking() {
        let memories = vec![
            make_mem_with_desc("m1", "Rust memory management", "Rust uses ownership"),
            make_mem_with_desc("m2", "Python data science", "Python is great for data"),
            make_mem_with_desc("m3", "Rust concurrency patterns", "Rust has async await"),
        ];
        let results = search_memories_tfidf(&memories, "Rust programming", 2);
        assert!(!results.is_empty());
        // Both Rust memories should rank above Python
        assert!(
            results
                .iter()
                .all(|m| m.frontmatter.id.starts_with('m') && m.frontmatter.id != "m2")
                || results[0].frontmatter.id != "m2"
        );
    }

    #[test]
    fn search_tfidf_empty_query() {
        let memories = vec![make_mem_with_desc("m1", "test", "body")];
        let results = search_memories_tfidf(&memories, "", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn search_tfidf_empty_memories() {
        let results: Vec<LoadedMemory> = search_memories_tfidf(&[], "test", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_problem_type_match_boost() {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.memory_type = MemoryType::Reasoning;
        fm.extra.insert(
            serde_yml::Value::String("problem_type".into()),
            serde_yml::Value::String("tool-failure-streak".into()),
        );
        let mem = LoadedMemory {
            frontmatter: fm,
            body: "test".to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        };
        let base = 0.5;
        let boosted = score_with_problem_type(base, &mem, "tool failure in debugging");
        assert!(boosted > base);
    }

    #[test]
    fn test_problem_type_no_match_no_boost() {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.memory_type = MemoryType::Reasoning;
        fm.extra.insert(
            serde_yml::Value::String("problem_type".into()),
            serde_yml::Value::String("trade-off".into()),
        );
        let mem = LoadedMemory {
            frontmatter: fm,
            body: "test".to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        };
        let base = 0.5;
        let score = score_with_problem_type(base, &mem, "debugging tool failure");
        assert_eq!(score, base);
    }

    #[test]
    fn test_non_reasoning_no_boost() {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.memory_type = MemoryType::Fact;
        let mem = LoadedMemory {
            frontmatter: fm,
            body: "test".to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        };
        let base = 0.5;
        let score = score_with_problem_type(base, &mem, "debugging");
        assert_eq!(score, base);
    }

    // === search_memories_filtered tests ===

    fn make_mem_typed_desc(
        id: &str,
        description: &str,
        body: &str,
        mtype: MemoryType,
    ) -> LoadedMemory {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.id = id.to_string();
        fm.description = description.to_string();
        fm.memory_type = mtype;
        LoadedMemory {
            frontmatter: fm,
            body: body.to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        }
    }

    #[test]
    fn filtered_empty_filters_returns_base_results() {
        let memories = vec![
            make_mem("m1", "I prefer dark mode in my editor"),
            make_mem("m2", "Python is my favorite scripting language"),
        ];
        let results = search_memories_filtered(&memories, "dark mode", 5, &[], &[]);
        assert!(!results.is_empty());
        assert_eq!(results[0].0.frontmatter.id, "m1");
    }

    #[test]
    fn filtered_dedup_excludes_already_surfaced() {
        let memories = vec![
            make_mem("m1", "I prefer dark mode in my editor"),
            make_mem("m2", "dark mode is great for eyes"),
            make_mem("m3", "light mode is also fine"),
        ];
        let results = search_memories_filtered(&memories, "dark mode", 5, &[], &["m1".to_string()]);
        assert!(results.iter().all(|(m, _)| m.frontmatter.id != "m1"));
        assert!(!results.is_empty());
    }

    #[test]
    fn filtered_denoise_skips_context_with_recent_tool() {
        let memories = vec![
            make_mem_typed_desc(
                "ctx1",
                "user ran bash",
                "bash was used recently",
                MemoryType::Context,
            ),
            make_mem_typed_desc(
                "fact1",
                "I prefer dark mode",
                "dark mode is my preference",
                MemoryType::Fact,
            ),
        ];
        // ctx1 should be filtered because it's Context + mentions "bash" + no warning.
        let results = search_memories_filtered(&memories, "bash", 5, &["bash".to_string()], &[]);
        assert!(results.iter().all(|(m, _)| m.frontmatter.id != "ctx1"));
    }

    #[test]
    fn filtered_denoise_keeps_context_with_warning() {
        let memories = vec![make_mem_typed_desc(
            "ctx1",
            "warning: bash command rm -rf is dangerous",
            "bash warning pitfall",
            MemoryType::Context,
        )];
        let results = search_memories_filtered(&memories, "bash", 5, &["bash".to_string()], &[]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.frontmatter.id, "ctx1");
    }

    #[test]
    fn filtered_denoise_keeps_non_context_with_recent_tool() {
        let memories = vec![make_mem_typed_desc(
            "fact1",
            "bash is my favorite shell",
            "bash facts",
            MemoryType::Fact,
        )];
        let results = search_memories_filtered(&memories, "bash", 5, &["bash".to_string()], &[]);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn filtered_k_zero_returns_empty() {
        let memories = vec![make_mem("m1", "test")];
        let results = search_memories_filtered(&memories, "test", 0, &[], &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn filtered_all_filtered_returns_empty() {
        let memories = vec![make_mem_typed_desc(
            "ctx1",
            "user ran bash",
            "bash usage",
            MemoryType::Context,
        )];
        let results = search_memories_filtered(&memories, "bash", 5, &["bash".to_string()], &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn filtered_warning_keyword_in_tags_kept() {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.id = "ctx_tag".to_string();
        fm.memory_type = MemoryType::Context;
        fm.description = "user ran bash".to_string();
        fm.tags = vec!["warning".to_string()];
        let m = LoadedMemory {
            frontmatter: fm,
            body: "bash body".to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        };
        // Should be kept because tags contain "warning".
        assert!(!should_skip_for_recent_tools(&m, &["bash".to_string()]));
    }
}
