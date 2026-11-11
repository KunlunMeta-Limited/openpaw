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
    Status {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
    },
    /// Print the resume decision for one goal from its ledger (no agent
    /// loop is driven).
    Resume {
        /// Goal id as shown by `openpaw goal status`.
        id: String,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
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
/// 功能：列出目标档案及真实账本进度。
/// 参数：home为调用者已解析的数据根。
/// 返回：成功为0，目录或账本读取失败为1。
/// 错误：报告原读取原因，不将错误当作空历史。
/// 状态：只读，不启动AgentLoop或修改知识与目标。
/// 并发与取消：沿原同步本地查询，无新增等待锁。
/// 权限：仅查询调用者已选择的数据根。
/// 关联：账本错误修复；需求REQ-OP-005；设计DES-OP-004；能力与错误边界修复。
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

/// `openpaw goal resume <id>` — read the ledger's newest run summary and
/// print the resume decision. No agent loop is driven; the printed
/// decision is the seam the execution side will consume later.
/// 功能：从准确目标的原账本计算恢复建议。
/// 参数：home为数据根，id为精确目标标识。
/// 返回：成功为0，目录或账本读取失败为1。
/// 错误：报告原读取原因，不将错误当作空历史。
/// 状态：只读，不启动AgentLoop或修改知识与目标。
/// 并发与取消：沿原同步本地查询，无新增等待锁。
/// 权限：仅查询调用者已选择的数据根。
/// 关联：账本错误修复；需求REQ-OP-005；设计DES-OP-004；能力与错误边界修复。
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
    let events = match GoalStore::query_events(&store, id) {
        Ok(events) => events,
        Err(e) => {
            report_error(
                format!("cannot read goal ledger `{id}`: {e}"),
                "the resume decision requires readable history; no decision was made.",
                "check the goal event file and permissions before retrying.",
            );
            return 1;
        }
    };
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
