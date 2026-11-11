//! `openpaw evolve ...` — the self-evolution surface (M4).
//!
//! `evolve status` reports how much of the persisted knowledge (memories +
//! skills, user and project domains) still carries the reflection-time
//! `unverified: true` review flag, plus a goal-profile overview pulled from
//! the goal ledger's fs store and event history. `evolve reset` clears that
//! flag: each flagged artifact is read with its domain frontmatter type, the
//! key is dropped from the catch-all `extra` mapping, and the file is
//! rewritten through the openpaw-store atomic frontmatter codec — flagged
//! files are never deleted, only rewritten in place.

use std::path::{Path, PathBuf};

use openpaw_goal_ledger::{
    last_next_action, FsGoalStore, GoalStore, RunOutcome, RUN_SUMMARY_EVENT,
};
use openpaw_reflect_lite::scan_unverified;
use openpaw_store::write_doc_atomic;

/// Frontmatter key carrying the reflection review gate.
const UNVERIFIED_KEY: &str = "unverified";

/// Subcommands of `openpaw evolve`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Unverified-artifact ratios (memories/skills, user + project scopes)
    /// and a goal-profile overview. Exit code 0 even when nothing exists.
    Status {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw` knowledge is scanned too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// Clear the `unverified` flag on reflection artifacts (atomic
    /// frontmatter rewrite; files are kept). Requires `--yes`.
    Reset {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw` knowledge is reset too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
        /// Acknowledge the rewrite (there is no interactive confirmation).
        #[arg(long)]
        yes: bool,
    },
}

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

// ── evolve status ────────────────────────────────────────────────────────

/// Knowledge roots scanned by `evolve status` / `evolve reset`.
///
/// User domain: `<home>/memories` + `<home>/skills`; project domain (when
/// `--project` is given): `<dir>/.openpaw/memories` + `<dir>/.openpaw/skills`
/// — the same layout the memory/skill stores write.
fn knowledge_roots(home: &Path, project: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut roots = vec![
        ("user memories", home.join("memories")),
        ("user skills", home.join("skills")),
    ];
    if let Some(dir) = project {
        roots.push(("project memories", dir.join(".openpaw").join("memories")));
        roots.push(("project skills", dir.join(".openpaw").join("skills")));
    }
    roots
}

/// `openpaw evolve status` — ratio table + goal overview. Exit 0 unless a
/// scan fails hard (unreadable directory), which is a three-segment error.
/// 功能：统计知识审查状态并列出目标账本进度。
/// 参数：home为数据根，project为可选项目根。
/// 返回：成功为0，目录或账本读取失败为1。
/// 错误：报告原读取原因，不将错误当作空历史。
/// 状态：只读，不启动AgentLoop或修改知识与目标。
/// 并发与取消：沿原同步本地查询，无新增等待锁。
/// 权限：仅查询调用者已选择的数据根。
/// 关联：账本错误修复；需求REQ-OP-005；设计DES-OP-004；能力与错误边界修复。
pub fn run_status(home: &Path, project: Option<&Path>) -> i32 {
    println!("unverified knowledge artifacts:");
    let mut total_files = 0usize;
    for (label, root) in knowledge_roots(home, project) {
        let stats = match scan_unverified(&root) {
            Ok(stats) => stats,
            Err(e) => {
                report_error(
                    format!("cannot scan {label} at {}: {e}", root.display()),
                    "the knowledge directory must be readable for the ratio scan.",
                    "check the path permissions and re-run.",
                );
                return 1;
            }
        };
        total_files += stats.files;
        println!(
            "  {:<18} {:>5} total  {:>5} unverified  {:>6.1}%",
            label,
            stats.files,
            stats.unverified,
            stats.ratio() * 100.0
        );
    }
    if total_files == 0 {
        println!(
            "  (no knowledge artifacts yet - reflection output lands in \
             <home>/memories and <home>/skills)"
        );
    }

    println!();
    println!("goal profiles ({}):", home.join("goals").display());
    let store = FsGoalStore::new(home.join("goals"));
    let goals = match GoalStore::list_goals(&store) {
        Ok(goals) => goals,
        Err(e) => {
            report_error(
                format!("cannot list goals: {e}"),
                "the goals directory must be readable.",
                "check the home path permissions and re-run.",
            );
            return 1;
        }
    };
    if goals.is_empty() {
        println!("  (no goals yet - goal profiles appear under <home>/goals)");
    }
    for doc in &goals {
        let fm = &doc.frontmatter;
        let (progress, next) = match run_digest(&store, &fm.id) {
            Ok(digest) => digest,
            Err(e) => {
                report_error(
                    format!("cannot read goal ledger `{}`: {e}", fm.id),
                    "ledger read errors cannot be treated as empty history.",
                    "check the goal event file and permissions before retrying.",
                );
                return 1;
            }
        };
        println!("  {}  [{:?}]  progress={progress}", fm.id, fm.status);
        if !fm.objective.is_empty() {
            println!("      objective: {}", fm.objective);
        }
        if let Some(next) = next {
            println!("      next: {next}");
        }
    }
    0
}

/// 功能：从原目标账本生成状态摘要并保留读取错误。
/// 参数：store为原存储，id为准确目标标识。
/// 返回：进度文字和下一步；合法空历史返回横线。
/// 错误：账本读取失败向调用者返回，不当作空历史。
/// 状态：只读原账本，不生成执行决定。
/// 并发与取消：同步有界本地读取，无共享写锁。
/// 权限：沿调用者已选择的数据根。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub(crate) fn run_digest(
    store: &FsGoalStore,
    id: &str,
) -> openpaw_goal_ledger::Result<(String, Option<String>)> {
    let events = GoalStore::query_events(store, id)?;
    let summary_text = events
        .iter()
        .rev()
        .find(|e| e.event_type == RUN_SUMMARY_EVENT)
        .and_then(|e| e.payload.get("summary"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let progress = if summary_text.is_empty() {
        "-".to_string()
    } else {
        let (outcome, value) = RunOutcome::parse_from_summary(summary_text);
        match value {
            Some(p) => format!("{:.0}%", p * 100.0),
            None => format!("{outcome:?}").to_lowercase(),
        }
    };
    Ok((progress, last_next_action(&events)))
}

// ── evolve reset ─────────────────────────────────────────────────────────

/// What one knowledge-root sweep produced.
#[derive(Default)]
struct ResetOutcome {
    /// Files rewritten without the flag.
    cleared: Vec<PathBuf>,
    /// Files skipped: present in the store layout but not parseable as the
    /// domain frontmatter type (left untouched, never deleted).
    skipped: usize,
}

/// `openpaw evolve reset` — clear the unverified review flag. Without
/// `--yes` it refuses with a three-segment hint (exit 1); there is no
/// interactive confirmation on this command.
pub fn run_reset(home: &Path, project: Option<&Path>, yes: bool) -> i32 {
    if !yes {
        eprintln!("error: `evolve reset` requires explicit confirmation");
        eprintln!(
            "why: it rewrites the frontmatter of every artifact flagged `unverified: true` \
             (user and project memories/skills); the flag is the human-review gate for \
             reflection output, so clearing it must be deliberate, and there is no \
             interactive prompt on this command."
        );
        eprintln!(
            "next: re-run as `openpaw evolve reset --yes` (repeat the --home/--project \
             used for `evolve status`)."
        );
        return 1;
    }

    let mut outcome = ResetOutcome::default();
    for (label, root) in knowledge_roots(home, project) {
        if let Err(e) = clear_root(&root, &mut outcome) {
            report_error(
                format!("cannot reset {label} under {}: {e}", root.display()),
                "flagged artifact files must be readable and writable for the atomic rewrite.",
                "check file permissions and re-run; flagged files are never deleted.",
            );
            return 1;
        }
    }

    if outcome.cleared.is_empty() && outcome.skipped == 0 {
        println!("(nothing to reset - no artifacts carry `unverified: true`)");
        return 0;
    }
    for path in &outcome.cleared {
        println!("cleared: {}", path.display());
    }
    if outcome.skipped > 0 {
        println!(
            "skipped {} artifact file(s) that do not parse as store frontmatter \
             (left untouched)",
            outcome.skipped
        );
    }
    println!(
        "reset done: unverified flag cleared on {} artifact(s)",
        outcome.cleared.len()
    );
    0
}

/// Sweep one knowledge root. Layout mirrors reflect-lite's scanner:
/// memories are flat `*.md`, skills are `<name>/SKILL.md`.
fn clear_root(root: &Path, outcome: &mut ResetOutcome) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(root).map_err(|e| e.to_string())?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                reset_skill(&skill_md, outcome)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            reset_memory(&path, outcome)?;
        }
    }
    Ok(())
}

/// Rewrite one memory artifact without its `unverified` key, if (and only
/// if) the key is present with value `true`. Unparseable files are counted
/// as skipped, not touched. The rewrite goes through the openpaw-store
/// atomic codec (temp file + rename), so a crash never truncates a file.
fn reset_memory(path: &Path, outcome: &mut ResetOutcome) -> Result<(), String> {
    let mut doc = match openpaw_store::read_doc::<openpaw_memory::MemoryFrontmatter>(path) {
        Ok(doc) => doc,
        Err(_) => {
            outcome.skipped += 1;
            return Ok(());
        }
    };
    let flagged = doc
        .frontmatter
        .extra
        .iter()
        .any(|(k, v)| k.as_str() == Some(UNVERIFIED_KEY) && v.as_bool() == Some(true));
    if !flagged {
        return Ok(());
    }
    doc.frontmatter
        .extra
        .retain(|k, _| k.as_str() != Some(UNVERIFIED_KEY));
    write_doc_atomic(path, &doc).map_err(|e| e.to_string())?;
    outcome.cleared.push(path.to_path_buf());
    Ok(())
}

/// Same flag-clearing rewrite for one `SKILL.md` artifact.
fn reset_skill(path: &Path, outcome: &mut ResetOutcome) -> Result<(), String> {
    let mut doc = match openpaw_store::read_doc::<openpaw_memory::skills::SkillFrontmatter>(path) {
        Ok(doc) => doc,
        Err(_) => {
            outcome.skipped += 1;
            return Ok(());
        }
    };
    let flagged = doc
        .frontmatter
        .extra
        .iter()
        .any(|(k, v)| k.as_str() == Some(UNVERIFIED_KEY) && v.as_bool() == Some(true));
    if !flagged {
        return Ok(());
    }
    doc.frontmatter
        .extra
        .retain(|k, _| k.as_str() != Some(UNVERIFIED_KEY));
    write_doc_atomic(path, &doc).map_err(|e| e.to_string())?;
    outcome.cleared.push(path.to_path_buf());
    Ok(())
}
