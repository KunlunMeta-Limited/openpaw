//! Assertions over the demo example's core path: everything the demo
//! persists must carry `unverified: true` (100% ratio in both domains).
//!
//! The example file is included as a module so the demo logic has a single
//! source of truth; its `main` is compiled out under `cfg(test)`.
#[path = "../examples/demo_reflect_loop.rs"]
mod demo_reflect_loop;
use demo_reflect_loop::run_demo;
#[test]
fn demo_persists_exactly_one_unverified_artifact_per_domain() {
    let home = tempfile::tempdir().unwrap();
    let report = run_demo(home.path());
    assert!(report.memory_path.is_file(), "memory artifact must exist");
    assert!(report.skill_path.is_file(), "skill artifact must exist");
    for (label, stats) in [
        ("memories", &report.memory_stats),
        ("skills", &report.skill_stats),
    ] {
        assert_eq!(stats.files, 1, "{label}: exactly one artifact scanned");
        assert_eq!(stats.unverified, 1, "{label}: the artifact is unverified");
        assert!(
            (stats.ratio() - 1.0).abs() < 1e-9,
            "{label}: ratio must be 100%"
        );
    }
    for path in [&report.memory_path, &report.skill_path] {
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(
            raw.contains("unverified: true"),
            "{} must carry `unverified: true`:\n{raw}",
            path.display()
        );
    }
}
