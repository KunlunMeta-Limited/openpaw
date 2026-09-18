//! `openpaw session ...` — list, show, and search persisted chat sessions.
//!
//! The SQLite index (`<home>/index.db`, FTS5-backed) is maintained
//! best-effort by `openpaw chat` next to the JSONL transcripts. The
//! transcripts remain the source of truth and are re-read on
//! `session show`.

use std::path::Path;

use openpaw_core::{Role, SessionEvent};
use openpaw_store::sqlite::SessionDb;
use openpaw_store::SessionReader;

/// Subcommands of `openpaw session`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List recent sessions from the index.
    List {
        /// Maximum number of sessions to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print an ordered message digest for one session id.
    Show {
        /// Session id as shown by `openpaw session list`.
        id: String,
    },
    /// Full-text search across indexed session messages.
    Search {
        /// Search query (tokens are matched via FTS5).
        query: String,
        /// Maximum number of hits to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// Truncate to `n` chars for tabular output; newlines become spaces.
fn truncate(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= n {
            out.push('…');
            break;
        }
        if c == '\n' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// `openpaw session list` — tabular summary of indexed sessions
/// (id / title / updated / message count).
pub fn run_list(home: &Path, limit: usize) -> i32 {
    let db_path = home.join("index.db");
    if !db_path.exists() {
        println!("(no session index yet — start a conversation with `openpaw chat`)");
        return 0;
    }
    let db = match SessionDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            report_error(
                format!("cannot open session index {}: {e}", db_path.display()),
                "the index file must be readable and a valid SQLite database.",
                "check the home path permissions; remove the index to rebuild it on the next chat.",
            );
            return 1;
        }
    };
    let sessions = match db.list_sessions() {
        Ok(sessions) => sessions,
        Err(e) => {
            report_error(
                format!("cannot list sessions: {e}"),
                "the sessions table could not be read.",
                "remove the index and re-run `openpaw chat` to rebuild it.",
            );
            return 1;
        }
    };
    if sessions.is_empty() {
        println!("(no sessions)");
        return 0;
    }
    println!(
        "{:<34} {:<26} {:<20} {:>8}",
        "id", "title", "updated", "turns"
    );
    for s in sessions.iter().take(limit) {
        let title = if s.title.is_empty() {
            "(untitled)"
        } else {
            s.title.as_str()
        };
        println!(
            "{:<34} {:<26} {:<20} {:>8}",
            truncate(&s.id, 32),
            truncate(title, 24),
            truncate(&s.updated_at, 19),
            s.message_count
        );
    }
    0
}

/// `openpaw session show <id>` — re-read the JSONL transcript behind the
/// indexed session and print a role-prefixed message digest.
pub fn run_show(home: &Path, id: &str) -> i32 {
    let db_path = home.join("index.db");
    if !db_path.exists() {
        report_error(
            format!("no session index at {}", db_path.display()),
            "`session show` resolves ids through the index built by `openpaw chat`.",
            "start a conversation with `openpaw chat`, then re-run.",
        );
        return 1;
    }
    let db = match SessionDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            report_error(
                format!("cannot open session index {}: {e}", db_path.display()),
                "the index file must be readable and a valid SQLite database.",
                "check the home path permissions; remove the index to rebuild it on the next chat.",
            );
            return 1;
        }
    };
    let transcript = match db.get_jsonl_path(id) {
        Ok(Some(path)) => path,
        Ok(None) => {
            report_error(
                format!("session `{id}` is not in the index"),
                "ids come from `openpaw session list`; this transcript was never indexed.",
                "run `openpaw session list` to see the indexed sessions.",
            );
            return 1;
        }
        Err(e) => {
            report_error(
                format!("cannot look up session `{id}`: {e}"),
                "the index lookup failed.",
                "remove the index and re-run `openpaw chat` to rebuild it.",
            );
            return 1;
        }
    };
    if !transcript.exists() {
        report_error(
            format!("transcript {} is missing", transcript.display()),
            "the JSONL file behind this session was moved or deleted.",
            "the index row is stale; remove the index and re-run `openpaw chat`.",
        );
        return 1;
    }
    let outcome = match SessionReader::read(&transcript) {
        Ok(outcome) => outcome,
        Err(e) => {
            report_error(
                format!("cannot parse transcript {}: {e}", transcript.display()),
                "the JSONL file is corrupted beyond its tolerated torn tail.",
                "sessions are plain JSONL; inspect the file manually to recover.",
            );
            return 1;
        }
    };
    println!("# {}", outcome.meta.id);
    println!("model:    {}", outcome.meta.model);
    println!("provider: {}", outcome.meta.provider);
    println!("created:  {}", outcome.meta.created_at.to_rfc3339());
    println!("file:     {}", transcript.display());
    if outcome.torn_tail_lines > 0 {
        println!(
            "(warning: {} torn tail line(s) from an interrupted write)",
            outcome.torn_tail_lines
        );
    }
    println!();
    let mut line = 0usize;
    for ev in &outcome.events {
        match ev {
            SessionEvent::Message(msg) => {
                line += 1;
                let role = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                println!("{:>3}. [{role:>9}] {}", line, truncate(&msg.text(), 100));
            }
            SessionEvent::ToolCallComplete {
                tool_name, success, ..
            } => {
                line += 1;
                let role = if *success { "tool" } else { "tool err" };
                println!("{:>3}. [{role:>9}] {}", line, truncate(tool_name, 100));
            }
            _ => {}
        }
    }
    0
}

/// `openpaw session search <query>` — FTS5 search over indexed messages,
/// printing session id, snippet, and relevance score per hit.
pub fn run_search(home: &Path, query: &str, limit: usize) -> i32 {
    let db_path = home.join("index.db");
    if !db_path.exists() {
        println!("(no session index yet — start a conversation with `openpaw chat`)");
        return 0;
    }
    let db = match SessionDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            report_error(
                format!("cannot open session index {}: {e}", db_path.display()),
                "the index file must be readable and a valid SQLite database.",
                "check the home path permissions; remove the index to rebuild it on the next chat.",
            );
            return 1;
        }
    };
    let results = match db.search(query, limit) {
        Ok(results) => results,
        Err(e) => {
            report_error(
                format!("search failed: {e}"),
                "the full-text query could not be executed against the index.",
                "simplify the query; punctuation-heavy queries are quoted automatically.",
            );
            return 1;
        }
    };
    if results.is_empty() {
        println!("(no results for \"{query}\")");
        return 0;
    }
    for r in &results {
        // FTS5 rank is negative; negate so a bigger score reads as a
        // better match. Prefer the highlighted snippet when present.
        let snippet = if r.snippet.is_empty() {
            truncate(&r.text_preview, 120)
        } else {
            truncate(&r.snippet, 120)
        };
        println!(
            "[{}:{}] score={:.3} {}",
            r.session_id, r.seq, -r.rank, snippet
        );
    }
    println!("({} result(s))", results.len());
    0
}
