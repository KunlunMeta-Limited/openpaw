//! Second-batch acceptance: SM-2 interval boundaries, relevance scoring
//! (CJK bigram + TF-IDF + effectiveness re-ranking), context injection with
//! the drift-defense preamble, and SM-2 review wiring on the store.
use chrono::{TimeZone, Utc};
use openpaw_memory::{
    event_to_quality, inject_memories_with_drift_defense, sm2_calculate, Confidence, FsMemoryStore,
    InjectionConfig, LoadedMemory, MemoryEffectiveness, MemoryEvent, MemoryFrontmatter,
    MemoryStore, Scope, Source, DRIFT_DEFENSE_SECTION,
};
use std::collections::HashMap;
fn fixed_now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 7, 0, 0, 0).unwrap()
}
fn fm(tags: Vec<&str>) -> MemoryFrontmatter {
    MemoryFrontmatter::new(
        Source::User,
        Confidence::Medium,
        tags.into_iter().map(String::from).collect(),
    )
}
// === SM-2 interval boundaries ===
/// First successful review schedules the next review 1 day out.
#[test]
fn sm2_first_review_schedules_one_day() {
    let result = sm2_calculate(0, 2.5, 0, 4, fixed_now());
    assert_eq!(result.repetition, 1);
    assert_eq!(result.interval_days, 1);
    assert_eq!(
        result.next_review_at,
        fixed_now() + chrono::Duration::days(1)
    );
}
/// Consecutive correct recalls multiply the interval by the ease factor.
#[test]
fn sm2_consecutive_successes_scale_interval() {
    let r1 = sm2_calculate(0, 2.5, 0, 4, fixed_now());
    assert_eq!(r1.interval_days, 1);
    let r2 = sm2_calculate(
        r1.repetition,
        r1.ease_factor,
        r1.interval_days,
        4,
        fixed_now(),
    );
    assert_eq!(r2.interval_days, 6);
    let r3 = sm2_calculate(
        r2.repetition,
        r2.ease_factor,
        r2.interval_days,
        4,
        fixed_now(),
    );
    // interval = ceil(6 * ease) with ease ≥ 2.4 after two quality-4 reviews.
    assert!(
        r3.interval_days > 6,
        "interval must grow past 6: {}",
        r3.interval_days
    );
    assert!(r3.interval_days >= (6.0 * r2.ease_factor).ceil() as u32);
}
/// Repeated barely-successful recalls (quality 3) must never drive the ease
/// factor below 1.3.
#[test]
fn sm2_ease_factor_floor_is_clamped() {
    let mut ef = 2.5f64;
    for _ in 0..50 {
        let r = sm2_calculate(0, ef, 1, 3, fixed_now());
        ef = r.ease_factor;
        assert!(ef >= 1.3, "ease factor below floor: {ef}");
    }
    assert!((ef - 1.3).abs() < 1e-9);
}
/// A lapse (quality ≤ 2) resets repetition and interval but keeps ease.
#[test]
fn sm2_lapse_resets_repetition_and_interval() {
    let result = sm2_calculate(4, 2.2, 30, 1, fixed_now());
    assert_eq!(result.repetition, 0);
    assert_eq!(result.interval_days, 1);
    assert!((result.ease_factor - 2.2).abs() < 1e-9);
    assert_eq!(
        result.next_review_at,
        fixed_now() + chrono::Duration::days(1)
    );
}
/// The interval cap keeps long-running memories reviewable.
#[test]
fn sm2_interval_is_capped() {
    let mut rep = 0u32;
    let mut ef = 2.5f64;
    let mut interval = 0u32;
    for _ in 0..100 {
        let r = sm2_calculate(rep, ef, interval, 5, fixed_now());
        rep = r.repetition;
        ef = r.ease_factor;
        interval = r.interval_days;
    }
    assert!(interval <= 365, "interval {interval} exceeds the cap");
}
/// MemoryEvent → recall-quality mapping stays stable (used by review wiring).
#[test]
fn event_to_quality_mapping_is_stable() {
    assert_eq!(event_to_quality(MemoryEvent::Accessed), 5);
    assert_eq!(event_to_quality(MemoryEvent::Referenced), 4);
    assert_eq!(event_to_quality(MemoryEvent::Loaded), 3);
    assert_eq!(event_to_quality(MemoryEvent::ReviewFailed), 2);
    assert_eq!(event_to_quality(MemoryEvent::ContextInjected), 2);
}
// === Relevance ===
fn make_mem(id: &str, body: &str) -> LoadedMemory {
    let mut m = fm(vec![]);
    m.id = id.to_string();
    LoadedMemory {
        frontmatter: m,
        body: body.to_string(),
        source_path: std::path::PathBuf::new(),
        scope: Scope::User,
    }
}
/// Chinese queries match through CJK bigram tokenisation.
#[test]
fn relevance_cjk_bigram_hit() {
    let memories = vec![
        make_mem("m1", "用户偏好深色主题"),
        make_mem("m2", "Python scripting language"),
    ];
    let results = openpaw_memory::search_memories(&memories, "偏好深色", 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].frontmatter.id, "m1");
}
/// English queries match through cosine/TF-IDF similarity.
#[test]
fn relevance_english_tfidf_hit() {
    let memories = vec![
        make_mem("m1", "I prefer dark mode in my editor"),
        make_mem("m2", "Python is my favorite scripting language"),
    ];
    let results = openpaw_memory::search_memories(&memories, "dark theme", 2);
    assert!(!results.is_empty());
    assert_eq!(results[0].frontmatter.id, "m1");
    let scored = openpaw_memory::search_memories_scored(&memories, "dark theme", 2);
    assert!(scored[0].1 > 0.0);
}
/// Effectiveness re-ranking demotes a low-effectiveness memory even when the
/// base similarity is identical.
#[test]
fn relevance_effectiveness_reranks() {
    let memories = vec![
        make_mem("low_eff", "rust ownership rules"),
        make_mem("fresh", "rust ownership rules"),
    ];
    let mut eff = HashMap::new();
    eff.insert(
        "low_eff".to_string(),
        MemoryEffectiveness {
            loaded: 10,
            referenced: 0,
            ..Default::default()
        },
    );
    let results =
        openpaw_memory::search_memories_with_effectiveness(&memories, "rust rules", 2, Some(&eff));
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].0.frontmatter.id, "fresh",
        "fresh memory must outrank the demoted one"
    );
    assert!(results[0].1 > results[1].1);
}
/// Empty query yields no results; zero k yields no results.
#[test]
fn relevance_empty_query_and_zero_k() {
    let memories = vec![make_mem("m1", "test body")];
    assert!(openpaw_memory::search_memories(&memories, "", 5).is_empty());
    assert!(openpaw_memory::search_memories(&memories, "test", 0).is_empty());
}
// === Injection with drift defense ===
/// With memories present, the injection starts with the drift-defense
/// preamble followed by the relevant-memories block.
#[test]
fn injection_includes_drift_defense_section() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().to_path_buf(), None);
    let mut m = fm(vec!["rust"]);
    m.description = "Use Rust for performance-critical code".to_string();
    m.pinned = true;
    let id = m.id.clone();
    MemoryStore::put(
        &store,
        Scope::User,
        m,
        "Use Rust for performance-critical code",
    )
    .unwrap();
    let config = InjectionConfig::default();
    let result = inject_memories_with_drift_defense(&store, &Scope::User, "Rust", &config);
    assert!(result.starts_with(DRIFT_DEFENSE_SECTION));
    assert!(result.contains("---"));
    assert!(result.contains("## Relevant Memories"));
    assert!(result.contains(&id));
}
/// An empty store produces no injection at all — no dangling defense block.
#[test]
fn injection_empty_store_yields_empty_output() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().to_path_buf(), None);
    let config = InjectionConfig::default();
    let result = inject_memories_with_drift_defense(&store, &Scope::User, "anything", &config);
    assert!(result.is_empty());
}
// === Store + SM-2 review wiring ===
/// review_memory advances the SM-2 fields and keeps the body intact.
#[test]
fn store_review_memory_advances_sm2_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().to_path_buf(), None);
    let m = fm(vec!["rust"]);
    let id = m.id.clone();
    MemoryStore::put(&store, Scope::User, m, "use anyhow for errors\n").unwrap();
    store.review_memory(&id, 4).unwrap();
    let updated = MemoryStore::get(&store, &id).unwrap().unwrap();
    assert_eq!(updated.frontmatter.sm2_repetition, 1);
    assert!(updated.frontmatter.sm2_ease_factor > 2.4);
    assert!(updated.frontmatter.sm2_next_review.is_some());
    assert!(updated.body.contains("anyhow"));
    // Second successful review moves to the 6-day interval.
    store.review_memory(&id, 4).unwrap();
    let again = MemoryStore::get(&store, &id).unwrap().unwrap();
    assert_eq!(again.frontmatter.sm2_repetition, 2);
    assert_eq!(again.frontmatter.sm2_interval_days, 6);
}
/// Reviewing an unknown id fails with a config error.
#[test]
fn store_review_memory_unknown_id_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().to_path_buf(), None);
    assert!(store.review_memory("mem_missing", 4).is_err());
}
/// Non-empty queries return relevance-ordered hits from the store.
#[test]
fn store_search_orders_by_relevance() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().to_path_buf(), None);
    let mut a = fm(vec!["rust"]);
    a.description = "prefer borrow checker friendly APIs".to_string();
    MemoryStore::put(
        &store,
        Scope::User,
        a,
        "the borrow checker explains lifetimes\n",
    )
    .unwrap();
    let mut b = fm(vec!["python"]);
    b.description = "python scripting notes".to_string();
    MemoryStore::put(&store, Scope::User, b, "python is great for glue scripts\n").unwrap();
    let hits = store.search_memories("borrow checker", 5).unwrap();
    assert!(!hits.is_empty());
    assert!(hits[0].0.body.contains("borrow checker"));
    assert!(hits[0].1 > 0.0);
}
// === Palace zones ===
#[test]
fn palace_groups_and_rejects_bad_zone_names() {
    use openpaw_memory::{build_palace_index_simple, group_by_zone, save_zone_summary};
    let mut core = fm(vec![]);
    core.zone = "core".into();
    let core_mem = LoadedMemory {
        frontmatter: core,
        body: "core fact".into(),
        source_path: std::path::PathBuf::new(),
        scope: Scope::User,
    };
    let memories = [core_mem];
    let grouped = group_by_zone(&memories);
    assert_eq!(grouped.get("core").map(|v| v.len()), Some(1));
    let index = build_palace_index_simple(&[]);
    assert!(index.contains("## Memory Palace"));
    assert!(index.contains("### core (0 items)"));
    // Failure path: traversal / separators are rejected before any write.
    assert!(save_zone_summary("../evil", "x").is_err());
    assert!(save_zone_summary("a/b", "x").is_err());
    assert!(save_zone_summary("", "x").is_err());
}
