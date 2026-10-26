//! `openpaw memory ...` — list / search / show / delete persistent memories.
//!
//! Memories are frontmatter markdown files under `<home>/memories/` (user
//! scope) and, when `--project <dir>` is given, `<dir>/.openpaw/memories/`
//! (project scope).

use std::path::Path;

use openpaw_memory::{FsMemoryStore, MemoryStore, Scope};

/// Subcommands of `openpaw memory`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List active memories (id / type / zone / first line).
    List {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw/memories` is searched too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// Relevance-ranked memory search.
    Search {
        /// Search query (words; ranking is TF-IDF based).
        query: String,
        /// Maximum number of hits to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw/memories` is searched too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// Print one memory in full.
    Show {
        /// Memory id as shown by `openpaw memory list`.
        id: String,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw/memories` is searched too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// Delete a memory by id (user scope first, then project scope).
    Delete {
        /// Memory id as shown by `openpaw memory list`.
        id: String,
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Project directory whose `.openpaw/memories` is searched too.
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
}

/// Build the memory store for an OpenPaw home and optional project
/// directory. The user scope is always `<home>/memories`; the project
/// scope (when `--project` is given) is `<dir>/.openpaw/memories`,
/// matching the standard project layout.
fn build_store(home: &Path, project: Option<&Path>) -> FsMemoryStore {
    let user_root = home.join("memories");
    let project_root = project.map(|dir| dir.join(".openpaw").join("memories"));
    FsMemoryStore::new(user_root, project_root)
}

/// First non-empty trimmed line of a memory body, for one-line summaries.
fn first_line(body: &str) -> &str {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// `openpaw memory list` — one line per active memory.
pub fn run_list(home: &Path, project: Option<&Path>) -> i32 {
    let store = build_store(home, project);
    let items = match MemoryStore::list_active(&store) {
        Ok(items) => items,
        Err(e) => {
            report_error(
                format!("cannot list memories: {e}"),
                "the memories directories must be readable.",
                "check the home path (or --project directory) permissions and re-run.",
            );
            return 1;
        }
    };
    if items.is_empty() {
        println!("(no memories)");
        return 0;
    }
    for m in items {
        println!(
            "{}  [{:?}]  {}  {}",
            m.frontmatter.id,
            m.frontmatter.memory_type,
            m.frontmatter.zone,
            first_line(&m.body)
        );
    }
    0
}

/// `openpaw memory search <query>` — relevance-ranked hits with scores.
pub fn run_search(home: &Path, project: Option<&Path>, query: &str, limit: usize) -> i32 {
    let store = build_store(home, project);
    let results = match store.search_memories(query, limit) {
        Ok(results) => results,
        Err(e) => {
            report_error(
                format!("cannot search memories: {e}"),
                "the memories directories must be readable.",
                "check the home path (or --project directory) permissions and re-run.",
            );
            return 1;
        }
    };
    if results.is_empty() {
        println!("(no memories matching '{query}')");
        return 0;
    }
    for (m, score) in results {
        println!(
            "{}  score={score:.3}  [{:?}]  {}",
            m.frontmatter.id,
            m.frontmatter.memory_type,
            first_line(&m.body)
        );
    }
    0
}

/// `openpaw memory show <id>` — full frontmatter and body of one memory.
pub fn run_show(home: &Path, project: Option<&Path>, id: &str) -> i32 {
    let store = build_store(home, project);
    let memory = match MemoryStore::get(&store, id) {
        Ok(Some(m)) => m,
        Ok(None) => {
            report_error(
                format!("no memory with id `{id}`"),
                "ids are exact matches; the memory may live in the other scope or was deleted.",
                "run `openpaw memory list` to find current ids, or pass --project for the project scope.",
            );
            return 1;
        }
        Err(e) => {
            report_error(
                format!("cannot read memory `{id}`: {e}"),
                "the memory file may be malformed or unreadable.",
                "fix or remove the offending file; malformed files are skipped by `memory list`.",
            );
            return 1;
        }
    };
    println!(
        "# {}  [{:?}]",
        memory.frontmatter.id, memory.frontmatter.memory_type
    );
    println!("scope:      {:?}", memory.scope);
    println!("source:     {}", memory.source_path.display());
    println!(
        "created:    {}",
        memory.frontmatter.created.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!("confidence: {:?}", memory.frontmatter.confidence);
    println!("origin:     {:?}", memory.frontmatter.source);
    println!("pinned:     {}", memory.frontmatter.pinned);
    if !memory.frontmatter.tags.is_empty() {
        println!("tags:       {}", memory.frontmatter.tags.join(", "));
    }
    println!();
    println!("{}", memory.body.trim());
    0
}

/// `openpaw memory delete <id>` — remove a memory file by id.
pub fn run_delete(home: &Path, project: Option<&Path>, id: &str) -> i32 {
    let store = build_store(home, project);
    // Try the user scope first, then the project scope when configured.
    let removed = match MemoryStore::delete(&store, Scope::User, id) {
        Ok(true) => Some(Scope::User),
        Ok(false) => {
            if project.is_some() {
                MemoryStore::delete(&store, Scope::Project, id)
                    .ok()
                    .filter(|ok| *ok)
                    .map(|_| Scope::Project)
            } else {
                None
            }
        }
        Err(e) => {
            report_error(
                format!("cannot delete memory `{id}`: {e}"),
                "the memory file must be writable to be removed.",
                "check file permissions and re-run.",
            );
            return 1;
        }
    };
    match removed {
        Some(scope) => {
            println!("removed {id} from {scope:?}");
            0
        }
        None => {
            report_error(
                format!("memory `{id}` not found"),
                "deletion looks up the id in both scopes when --project is given; nothing matched.",
                "run `openpaw memory list` to find current ids.",
            );
            1
        }
    }
}
