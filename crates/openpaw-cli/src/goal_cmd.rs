//! `openpaw goal ...` — create and inspect goal profiles (M5).
//!
//! Thin surface over the goal ledger's fs store: `new` writes a
//! [`GoalFrontmatter`] profile, `status` lists the overview (reusing the
//! evolve command's progress digest), and `resume` prints the
//! single-step resume decision taken from the ledger's newest run
//! summary. Driving an agent loop is deliberately out of scope here —
//! this command only reports the decision; execution lands on the
//! execution side in a later phase.

use std::path::Path;

use openpaw_goal_ledger::{
    decide_resume, Confidence, FsGoalStore, GoalFrontmatter, GoalStore, ResumeDecision, Source,
};

use crate::evolve_cmd::run_digest;

/// Subcommands of `openpaw goal`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Create a goal profile with an auto-generated id.
    New {
        /// What the goal should achieve.
        objective: String,
        /// Project directory; the profile lands in `<dir>/.openpaw/goals`.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// List goal profiles (id / status / progress).
    Status,
    /// Print the resume decision for one goal from its ledger (no agent
    /// loop is driven).
    Resume {
        /// Goal id as shown by `openpaw goal status`.
        id: String,
    },
}

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// `openpaw goal new <objective>` — write a goal profile into
/// `goals_dir` (user-sourced, medium confidence by default).
pub fn run_new(goals_dir: &Path, objective: &str) -> i32 {
    let frontmatter =
        GoalFrontmatter::with_source_confidence(objective, Source::User, Confidence::Medium);
    let id = frontmatter.id.clone();
    let store = FsGoalStore::new(goals_dir.to_path_buf());
    let body = format!("objective: {objective}\n");
    match store.create_goal(frontmatter, &body) {
        Ok(path) => {
            println!("goal created: {id}");
            println!("file: {}", path.display());
            println!("status: created");
            println!("next: run `openpaw goal resume {id}` to see the resume decision.");
            0
        }
        Err(e) => {
            report_error(
                format!("cannot create goal under {}: {e}", goals_dir.display()),
                "the goals directory must be writable and the generated id must be unused.",
                "check the directory permissions and re-run.",
            );
            1
        }
    }
}

/// `openpaw goal status` — one line per goal profile with its ledger
/// progress digest (same overview shape as `openpaw evolve status`).
pub fn run_status(home: &Path) -> i32 {
    let goals_dir = home.join("goals");
    let store = FsGoalStore::new(goals_dir.clone());
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
    println!("goal profiles ({}):", goals_dir.display());
    if goals.is_empty() {
        println!("  (no goals yet - create one with `openpaw goal new <objective>`)");
        return 0;
    }
    for doc in &goals {
        let fm = &doc.frontmatter;
        let (progress, next) = run_digest(&store, &fm.id);
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

/// `openpaw goal resume <id>` — read the ledger's newest run summary and
/// print the resume decision. No agent loop is driven; the printed
/// decision is the seam the execution side will consume later.
pub fn run_resume(home: &Path, id: &str) -> i32 {
    let goals_dir = home.join("goals");
    let store = FsGoalStore::new(goals_dir);
    let doc = match GoalStore::load_goal(&store, id) {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            report_error(
                format!("no goal with id `{id}`"),
                "ids are exact matches as shown by `openpaw goal status`.",
                "run `openpaw goal status` to list current ids.",
            );
            return 1;
        }
        Err(e) => {
            report_error(
                format!("cannot read goal `{id}`: {e}"),
                "the goal file must be readable frontmatter markdown.",
                "check the goals directory permissions and re-run.",
            );
            return 1;
        }
    };
    let events = GoalStore::query_events(&store, id).unwrap_or_default();
    let fm = &doc.frontmatter;
    println!("goal: {id}  [{:?}]", fm.status);
    match decide_resume(fm, &events) {
        ResumeDecision::Run { next_action } => {
            println!("resume decision: run one bounded turn");
            if next_action.is_empty() {
                println!("next action: (none recorded - will start from the objective)");
            } else {
                println!("next action: {next_action}");
            }
        }
        ResumeDecision::WaitForUser { question } => {
            println!("resume decision: wait for user");
            println!("question: {question}");
        }
        ResumeDecision::AlreadyFinished { status } => {
            println!("resume decision: already finished (status: {status:?})");
        }
    }
    println!(
        "note: the agent loop is not driven here; execution lands on the \
         execution side in a later phase."
    );
    0
}
