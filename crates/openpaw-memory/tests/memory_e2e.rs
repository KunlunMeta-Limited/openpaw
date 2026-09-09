//! M3 core acceptance: nine MemoryType × frontmatter snapshot roundtrip
//! through FsMemoryStore, dual-domain (User vs Project) isolation, search by
//! query/category/limit, and deletion.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use openpaw_core::{MemoryProvider, MemorySearchQuery};
use openpaw_memory::{
    Confidence, FsMemoryStore, MemoryFrontmatter, MemoryStore, MemoryType, Scope, Source,
};
use openpaw_store::FrontmatterDoc;

fn block_on<F: Future>(fut: F) -> F::Output {
    // FsMemoryStore's async trait methods never actually park (no internal
    // await points), so the standard no-op waker plus a yield loop suffices
    // without pulling a runtime dependency into dev-deps.
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn memory_type_slug(t: MemoryType) -> &'static str {
    match t {
        MemoryType::Fact => "fact",
        MemoryType::Procedure => "procedure",
        MemoryType::Preference => "preference",
        MemoryType::Episode => "episode",
        MemoryType::Context => "context",
        MemoryType::Insight => "insight",
        MemoryType::Reasoning => "reasoning",
        MemoryType::Goal => "goal",
        MemoryType::Intention => "intention",
    }
}

fn source_slug(s: Source) -> &'static str {
    match s {
        Source::Reflection => "reflection",
        Source::User => "user",
        Source::Imported => "imported",
        Source::Agent => "agent",
    }
}

fn confidence_slug(c: Confidence) -> &'static str {
    match c {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
    }
}

/// One memory per MemoryType, persisted then read back; every frontmatter
/// field must round-trip exactly, and the on-disk YAML key set must match the
/// expected snapshot (skipped-empty fields excluded).
#[test]
fn nine_memory_types_frontmatter_snapshot_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().join("user"), None);

    // (type, source, confidence, zone, description, pinned, tags)
    type Spec = (
        MemoryType,
        Source,
        Confidence,
        &'static str,
        &'static str,
        bool,
        Vec<&'static str>,
    );
    let specs: [Spec; 9] = [
        (
            MemoryType::Fact,
            Source::Reflection,
            Confidence::High,
            "conventions",
            "use anyhow at app layer",
            true,
            vec!["rust"],
        ),
        (
            MemoryType::Procedure,
            Source::User,
            Confidence::Medium,
            "workflows",
            "run cargo test then clippy",
            false,
            vec!["build"],
        ),
        (
            MemoryType::Preference,
            Source::User,
            Confidence::High,
            "style",
            "prefers concise answers",
            true,
            vec![],
        ),
        (
            MemoryType::Episode,
            Source::Agent,
            Confidence::Low,
            "episodes",
            "debugged flaky path case",
            false,
            vec![],
        ),
        (
            MemoryType::Context,
            Source::Reflection,
            Confidence::Medium,
            "environment",
            "windows host, powershell shell",
            false,
            vec!["env"],
        ),
        (
            MemoryType::Insight,
            Source::Reflection,
            Confidence::High,
            "insights",
            "flaky tests cluster near time",
            false,
            vec![],
        ),
        (
            MemoryType::Reasoning,
            Source::User,
            Confidence::Medium,
            "reasoning",
            "divide and conquer refactors",
            false,
            vec!["method"],
        ),
        (
            MemoryType::Goal,
            Source::Agent,
            Confidence::Medium,
            "goals",
            "land M3 memory core",
            false,
            vec!["m3"],
        ),
        (
            MemoryType::Intention,
            Source::Agent,
            Confidence::Low,
            "goals",
            "write snapshot test",
            false,
            vec![],
        ),
    ];

    for (i, (memory_type, source, confidence, zone, description, pinned, tags)) in
        specs.iter().enumerate()
    {
        let mut fm = MemoryFrontmatter::new(
            *source,
            *confidence,
            tags.iter().map(|t| t.to_string()).collect(),
        );
        fm.memory_type = *memory_type;
        fm.zone = zone.to_string();
        fm.description = description.to_string();
        fm.pinned = *pinned;
        // The last record also carries optional fields so the snapshot covers
        // both the omitted-empty and the present cases.
        if i == 8 {
            fm.task_id = Some("task-42".into());
            fm.goal_id = Some("goal-7".into());
            fm.supersedes = vec!["mem_prev_0001".into()];
        }
        let (id, created) = (fm.id.clone(), fm.created);
        let body = format!("memory body {i}: {description}\n");
        MemoryStore::put(&store, Scope::User, fm, &body).unwrap();

        let loaded = MemoryStore::get(&store, &id).unwrap().unwrap();
        assert_eq!(loaded.id(), id);
        assert_eq!(loaded.scope, Scope::User);
        assert_eq!(loaded.body, body);

        let got = &loaded.frontmatter;
        assert_eq!(got.id, id);
        assert_eq!(
            got.created, created,
            "created must survive the yaml roundtrip"
        );
        assert_eq!(got.memory_type, *memory_type);
        assert_eq!(got.source, *source);
        assert_eq!(got.confidence, *confidence);
        assert_eq!(got.zone, *zone);
        assert_eq!(got.description, *description);
        assert_eq!(got.pinned, *pinned);
        let expected_tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        assert_eq!(got.tags, expected_tags);
        assert_eq!(got.sm2_repetition, 0);
        assert!((got.sm2_ease_factor - 2.5).abs() < f64::EPSILON);
        assert_eq!(got.sm2_interval_days, 0);
        assert!(got.sm2_next_review.is_none());
        assert!(got.extra.is_empty());

        // Snapshot: exact on-disk frontmatter key set and serialized values.
        let raw: FrontmatterDoc<serde_json::Value> =
            openpaw_store::read_doc(&loaded.source_path).unwrap();
        let obj = raw.frontmatter.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();

        let mut expected_keys: Vec<&str> = vec![
            "confidence",
            "created",
            "description",
            "id",
            "memory_type",
            "pinned",
            "sm2_ease_factor",
            "sm2_interval_days",
            "sm2_next_review",
            "sm2_repetition",
            "source",
            "tags",
            "zone",
        ];
        if i == 8 {
            expected_keys.extend(["goal_id", "supersedes", "task_id"]);
        }
        expected_keys.sort_unstable();
        assert_eq!(
            keys, expected_keys,
            "frontmatter key-set snapshot mismatch for {memory_type:?}"
        );

        assert_eq!(obj["memory_type"], memory_type_slug(*memory_type));
        assert_eq!(obj["source"], source_slug(*source));
        assert_eq!(obj["confidence"], confidence_slug(*confidence));
        assert_eq!(obj["pinned"], *pinned);
        assert_eq!(obj["zone"], *zone);
        assert_eq!(obj["description"], *description);
        assert_eq!(obj["tags"], serde_json::json!(expected_tags));
        assert_eq!(obj["sm2_ease_factor"], serde_json::json!(2.5));
        assert_eq!(obj["sm2_repetition"], serde_json::json!(0));
        assert_eq!(obj["sm2_interval_days"], serde_json::json!(0));
        assert!(obj["sm2_next_review"].is_null());
        if i == 8 {
            assert_eq!(obj["task_id"], "task-42");
            assert_eq!(obj["goal_id"], "goal-7");
            assert_eq!(obj["supersedes"], serde_json::json!(["mem_prev_0001"]));
        }
    }
}

/// User and Project memories live in disjoint directories; a scope-scoped
/// delete cannot reach the other domain, and a store without a project root
/// never sees project-domain records.
#[test]
fn dual_domain_user_project_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let user_root = dir.path().join("user-domain");
    let project_root = dir.path().join("project-domain");
    let store = FsMemoryStore::new(user_root.clone(), Some(project_root.clone()));

    let mut mu = MemoryFrontmatter::new(Source::User, Confidence::High, vec!["user".into()]);
    mu.zone = "user-zone".into();
    let id_u = mu.id.clone();
    MemoryStore::put(&store, Scope::User, mu, "user domain body\n").unwrap();

    let mut mp = MemoryFrontmatter::new(Source::Agent, Confidence::Low, vec!["project".into()]);
    mp.zone = "project-zone".into();
    let id_p = mp.id.clone();
    MemoryStore::put(&store, Scope::Project, mp, "project domain body\n").unwrap();

    // Files live in disjoint directories and carry the right scope tag.
    let lu = MemoryStore::get(&store, &id_u).unwrap().unwrap();
    let lp = MemoryStore::get(&store, &id_p).unwrap().unwrap();
    assert!(lu.source_path.starts_with(&user_root));
    assert!(lp.source_path.starts_with(&project_root));
    assert_eq!(lu.scope, Scope::User);
    assert_eq!(lp.scope, Scope::Project);

    // A User-scope delete cannot remove a Project-domain record.
    assert!(!MemoryStore::delete(&store, Scope::User, &id_p).unwrap());
    assert!(MemoryStore::get(&store, &id_p).unwrap().is_some());

    // A store bound to the user domain only cannot see the project domain.
    let user_only = FsMemoryStore::new(user_root, None);
    assert!(MemoryStore::get(&user_only, &id_p).unwrap().is_none());
    assert!(MemoryStore::list(&user_only)
        .unwrap()
        .iter()
        .all(|m| m.scope == Scope::User));
}

/// MemoryProvider::search over FsMemoryStore: query text, category (zone)
/// filter, and limit semantics (total counted before truncation).
#[test]
fn provider_search_by_query_category_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().join("user"), None);

    let mut a = MemoryFrontmatter::new(Source::User, Confidence::High, vec![]);
    a.zone = "rust".into();
    a.description = "borrow checker deep dive".into();
    MemoryStore::put(
        &store,
        Scope::User,
        a,
        "the borrow checker explains lifetimes\n",
    )
    .unwrap();

    let mut b = MemoryFrontmatter::new(Source::Reflection, Confidence::Medium, vec![]);
    b.zone = "rust".into();
    MemoryStore::put(&store, Scope::User, b, "prefer slices over vec arguments\n").unwrap();

    let mut c = MemoryFrontmatter::new(Source::Agent, Confidence::Low, vec![]);
    c.zone = "go".into();
    MemoryStore::put(&store, Scope::User, c, "goroutines leak without cancel\n").unwrap();

    // query text match.
    let hits = block_on(MemoryProvider::search(
        &store,
        &MemorySearchQuery {
            query: "borrow".into(),
            category: None,
            limit: None,
        },
    ));
    assert_eq!(hits.entries.len(), 1);
    assert!(hits.entries[0].content.contains("borrow checker"));

    // category maps to zone; empty query matches everything in the category.
    let hits = block_on(MemoryProvider::search(
        &store,
        &MemorySearchQuery {
            query: String::new(),
            category: Some("rust".into()),
            limit: None,
        },
    ));
    assert_eq!(hits.total, 2);
    assert_eq!(hits.entries.len(), 2);

    // limit truncates entries but keeps the pre-truncation total.
    let hits = block_on(MemoryProvider::search(
        &store,
        &MemorySearchQuery {
            query: String::new(),
            category: None,
            limit: Some(1),
        },
    ));
    assert_eq!(hits.total, 3);
    assert_eq!(hits.entries.len(), 1);

    // query miss.
    let hits = block_on(MemoryProvider::search(
        &store,
        &MemorySearchQuery {
            query: "no-such-token-xyz".into(),
            category: None,
            limit: None,
        },
    ));
    assert_eq!(hits.total, 0);
    assert!(hits.entries.is_empty());
}

/// Deletion removes the file and is reflected by every read path, including
/// the async provider bridge that tries both scopes.
#[test]
fn delete_roundtrip_and_provider_remove() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsMemoryStore::new(dir.path().join("user"), None);

    let m = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec!["t".into()]);
    let id = m.id.clone();
    let path = MemoryStore::put(&store, Scope::User, m, "to be deleted\n").unwrap();
    assert!(path.exists());
    assert!(MemoryStore::get(&store, &id).unwrap().is_some());

    assert!(MemoryStore::delete(&store, Scope::User, &id).unwrap());
    assert!(MemoryStore::get(&store, &id).unwrap().is_none());
    assert!(!path.exists());
    assert!(!MemoryStore::delete(&store, Scope::User, &id).unwrap());

    // Provider-side removal across both scopes.
    let m2 = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
    let id2 = m2.id.clone();
    MemoryStore::put(&store, Scope::User, m2, "provider delete\n").unwrap();
    assert!(block_on(MemoryProvider::remove(&store, &id2)));
    assert!(MemoryStore::get(&store, &id2).unwrap().is_none());
    assert!(!block_on(MemoryProvider::remove(&store, &id2)));
}
