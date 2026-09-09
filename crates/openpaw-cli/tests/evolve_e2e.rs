//! E2E: `openpaw evolve status/reset` — the ratio table, the goal overview,
//! and the atomic frontmatter rewrite that clears the review flag.

use std::path::Path;

use openpaw_cli::evolve_cmd;
use openpaw_goal_ledger::{FsGoalStore, GoalStore};

/// Write one flat memory artifact with (or without) the review flag.
fn write_memory(root: &Path, file: &str, unverified: bool) {
    std::fs::create_dir_all(root).unwrap();
    let id = file.trim_end_matches(".md");
    let flag = if unverified { "unverified: true\n" } else { "" };
    std::fs::write(
        root.join(file),
        format!(
            "---\nid: {id}\ncreated: 2026-08-31T00:00:00Z\nsource: reflection\nconfidence: medium\n{flag}---\nbody of {id}\n"
        ),
    )
    .unwrap();
}

/// Write one `<name>/SKILL.md` artifact with (or without) the review flag.
fn write_skill(root: &Path, name: &str, unverified: bool) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let flag = if unverified { "unverified: true\n" } else { "" };
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: demo skill\n{flag}---\nskill body\n"),
    )
    .unwrap();
}

#[test]
fn reset_requires_yes_then_clears_flags_atomically() {
    let home = tempfile::tempdir().unwrap();
    let hp = home.path();
    write_memory(&hp.join("memories"), "mem_flagged.md", true);
    write_memory(&hp.join("memories"), "mem_verified.md", false);
    write_skill(&hp.join("skills"), "fmt-skill", true);
    // A file that parses as generic frontmatter but not as the memory
    // schema: reset must skip it (never delete); status still counts it.
    let broken = hp.join("memories").join("broken.md");
    std::fs::write(
        &broken,
        "---\nunverified: true\n---\nmissing required fields\n",
    )
    .unwrap();
    let verified_before =
        std::fs::read_to_string(hp.join("memories").join("mem_verified.md")).unwrap();

    // Without --yes the reset refuses and nothing changes.
    assert_eq!(evolve_cmd::run_reset(hp, None, false), 1);
    let flagged = std::fs::read_to_string(hp.join("memories").join("mem_flagged.md")).unwrap();
    assert!(
        flagged.contains("unverified: true"),
        "refusal must not touch files"
    );

    // With --yes the flagged artifacts are rewritten in place: flag gone,
    // frontmatter and body preserved; the verified file is byte-identical.
    assert_eq!(evolve_cmd::run_reset(hp, None, true), 0);
    let flagged_after =
        std::fs::read_to_string(hp.join("memories").join("mem_flagged.md")).unwrap();
    assert!(
        !flagged_after.contains("unverified"),
        "flag removed: {flagged_after}"
    );
    assert!(
        flagged_after.starts_with("---\nid: mem_flagged\n"),
        "frontmatter preserved: {flagged_after}"
    );
    assert!(
        flagged_after.contains("body of mem_flagged"),
        "body preserved"
    );
    let skill_after =
        std::fs::read_to_string(hp.join("skills").join("fmt-skill").join("SKILL.md")).unwrap();
    assert!(
        !skill_after.contains("unverified") && skill_after.contains("name: fmt-skill"),
        "skill flag removed, frontmatter preserved: {skill_after}"
    );
    let verified_after =
        std::fs::read_to_string(hp.join("memories").join("mem_verified.md")).unwrap();
    assert_eq!(
        verified_after, verified_before,
        "non-flagged file must stay byte-identical"
    );
    assert!(
        std::fs::read_to_string(&broken)
            .unwrap()
            .contains("unverified: true"),
        "unparseable file skipped, not deleted"
    );

    // A second reset finds nothing flagged anymore and still exits 0.
    assert_eq!(evolve_cmd::run_reset(hp, None, true), 0);
}

#[test]
fn status_lists_domains_and_goals_and_project_scope() {
    let home = tempfile::tempdir().unwrap();
    let hp = home.path();
    write_memory(&hp.join("memories"), "mem_x.md", true);

    // One goal profile via the goal-ledger store; status prints its
    // id/status/progress digest and exits 0.
    let store = FsGoalStore::new(hp.join("goals"));
    GoalStore::create_goal(
        &store,
        openpaw_goal_ledger::GoalFrontmatter::new("ship the demo"),
        "goal body\n",
    )
    .unwrap();
    assert_eq!(evolve_cmd::run_status(hp, None), 0);

    // The project domain is scanned too when --project is given.
    let project = tempfile::tempdir().unwrap();
    write_memory(
        &project.path().join(".openpaw").join("memories"),
        "proj_mem.md",
        false,
    );
    assert_eq!(evolve_cmd::run_status(hp, Some(project.path())), 0);
}
