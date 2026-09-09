//! `openpaw doctor` — local environment checks and the network-audit
//! report (M5 exit gate).
//!
//! Every check reports PASS or WARN; warnings never fail the command
//! (exit code stays 0) so the doctor is safe to run in setup scripts.
//! The network audit is a zero-telemetry, purely local static scan: it
//! lists workspace `.rs` files whose source mentions the network call
//! surface (`reqwest::` / `http_client` / `web_fetch`). No network I/O
//! is performed. Directory traversal uses plain `std::fs` recursion (no
//! extra dependencies).

use std::path::{Path, PathBuf};

use openpaw_store::sqlite::SessionDb;

/// Outcome of one doctor check: pass with a detail line, or warn with a
/// three-segment explanation (what / why / next step).
enum Check {
    /// Check succeeded; the string is the detail after the `[PASS]` tag.
    Pass(String),
    /// Check warns; fields are detail, why, and next step.
    Warn(String, String, String),
}

impl Check {
    /// Constructor accepting plain string literals for the warn fields.
    fn warn(detail: impl Into<String>, why: impl Into<String>, next: impl Into<String>) -> Self {
        Self::Warn(detail.into(), why.into(), next.into())
    }
}

/// Directory names skipped by the source scan (build artifacts and VCS
/// internals are never user source code).
const SKIP_DIRS: [&str; 2] = [".git", "target"];

/// Upper bounds keeping the scan bounded on huge trees.
const MAX_SCAN_DEPTH: usize = 24;
const MAX_SCAN_FILES: usize = 20_000;

/// Emit one check result and update the pass/warn counters.
fn emit(check: Check, pass: &mut usize, warn: &mut usize) {
    match check {
        Check::Pass(detail) => {
            *pass += 1;
            println!("[PASS] {detail}");
        }
        Check::Warn(detail, why, next) => {
            *warn += 1;
            println!("[WARN] {detail}");
            println!("       why: {why}");
            println!("       next: {next}");
        }
    }
}

/// `openpaw doctor` — run all checks, print the summary, exit 0.
pub fn run(home: &Path, workspace: &Path) -> i32 {
    let mut pass = 0usize;
    let mut warn = 0usize;

    // (a) OPENPAW_HOME writability (probe file, removed right after).
    emit(check_home_writable(home), &mut pass, &mut warn);

    // (b) Provider key presence in the environment (values are never
    // printed — only set/not set).
    for name in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY"] {
        emit(check_env_key(name), &mut pass, &mut warn);
    }

    // (c) index.db opens and reports its schema version.
    emit(check_index_db(home), &mut pass, &mut warn);

    // (d) memories / skills directory state.
    emit(
        check_knowledge_dir("memories", &home.join("memories"), false),
        &mut pass,
        &mut warn,
    );
    emit(
        check_knowledge_dir("skills", &home.join("skills"), true),
        &mut pass,
        &mut warn,
    );

    // (e) Network-audit report over the workspace source tree.
    let mut scanned = 0usize;
    let mut hits = Vec::new();
    let audit = network_audit(workspace, &mut scanned, &mut hits);
    println!("network audit report (workspace: {}):", workspace.display());
    if scanned > 0 {
        println!("  scanned: {scanned} rust file(s)");
        println!("  network call sites: {} files", hits.len());
        for path in &hits {
            println!("    {}", relative(path, workspace));
        }
        if hits.is_empty() {
            println!("  (no network call sites found)");
        }
        println!(
            "  (zero telemetry: local static scan for `reqwest::` / `http_client` / \
             `web_fetch`; no network I/O is performed)"
        );
    }
    emit(audit, &mut pass, &mut warn);

    println!();
    println!("doctor summary: {pass} pass, {warn} warn (warnings do not fail; exit 0)");
    0
}

// ── individual checks ────────────────────────────────────────────────────

/// (a) The home directory must be creatable and writable. Probed with a
/// throwaway file that is removed immediately.
fn check_home_writable(home: &Path) -> Check {
    if let Err(e) = std::fs::create_dir_all(home) {
        return Check::warn(
            format!("openpaw home not usable: {} ({e})", home.display()),
            "the home directory must be creatable for sessions, memories, and goals.",
            "check the path permissions or pass a writable --home.",
        );
    }
    let probe = home.join(".openpaw-doctor-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::Pass(format!("openpaw home writable: {}", home.display()))
        }
        Err(e) => Check::warn(
            format!("openpaw home not writable: {} ({e})", home.display()),
            "session transcripts and the index are written into the home directory.",
            "check the directory permissions or pass a writable --home.",
        ),
    }
}

/// (b) One provider key from the environment. Presence only — the value
/// is never printed.
fn check_env_key(name: &str) -> Check {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Check::Pass(format!("{name}: set (value hidden)")),
        _ => Check::warn(
            format!("{name}: not set"),
            "the anthropic/openai providers read their key from the environment; \
             offline providers (mock/echo) work without it.",
            format!("export {name} to enable that provider; the value is never printed."),
        ),
    }
}

/// (c) index.db must open as a SQLite database and report its schema
/// version. Opening creates the file when missing (same as `openpaw chat`).
fn check_index_db(home: &Path) -> Check {
    let db_path = home.join("index.db");
    match SessionDb::open(&db_path) {
        Ok(db) => match db.schema_version() {
            Ok(version) => Check::Pass(format!("index.db opens (schema version {version})")),
            Err(e) => Check::warn(
                format!("index.db opens but the schema query failed: {e}"),
                "the schema_version table should be readable right after open.",
                "remove index.db and let the next chat rebuild it.",
            ),
        },
        Err(e) => Check::warn(
            format!("index.db cannot be opened: {e}"),
            "the index must be a readable SQLite database for session list/show/search.",
            "check the home permissions; remove index.db to rebuild it on the next chat.",
        ),
    }
}

/// (d) One knowledge directory. Memories are flat `*.md`; skills are
/// `<name>/SKILL.md` — the layout the stores write.
fn check_knowledge_dir(label: &str, dir: &Path, skills_layout: bool) -> Check {
    if !dir.is_dir() {
        return Check::warn(
            format!("{label} dir: missing ({})", dir.display()),
            "knowledge artifacts live under this directory once reflection or \
             `openpaw init` has run.",
            "run `openpaw init` to create the home skeleton.",
        );
    }
    let count = count_artifacts(dir, skills_layout);
    Check::Pass(format!("{label} dir: present ({count} artifact(s))"))
}

/// Count artifacts in one knowledge root: flat `*.md` files, or
/// `<name>/SKILL.md` pairs when `skills_layout` is set.
fn count_artifacts(dir: &Path, skills_layout: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if skills_layout {
            if path.is_dir() && path.join("SKILL.md").is_file() {
                count += 1;
            }
        } else if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            count += 1;
        }
    }
    count
}

// ── (e) network audit ────────────────────────────────────────────────────

/// Scan the workspace for `.rs` files whose source mentions the network
/// call surface. Purely local: file reads only, no network I/O.
fn network_audit(workspace: &Path, scanned: &mut usize, hits: &mut Vec<PathBuf>) -> Check {
    if !workspace.is_dir() {
        return Check::warn(
            format!("workspace not found: {}", workspace.display()),
            "the network audit scans the workspace source tree.",
            "pass an existing --workspace directory to audit.",
        );
    }
    let mut files = Vec::new();
    collect_rs_files(workspace, &mut files, 0);
    *scanned = files.len();
    for path in &files {
        if has_network_call_site(path) {
            hits.push(path.clone());
        }
    }
    Check::Pass(format!(
        "network audit: {} network call-site file(s) across {scanned} scanned rust file(s)",
        hits.len()
    ))
}

/// Recursively collect `.rs` files, skipping build/VCS directories.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_SCAN_DEPTH || out.len() >= MAX_SCAN_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                let name = entry.file_name();
                if SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                collect_rs_files(&path, out, depth + 1);
            }
            Ok(ft) if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") => {
                out.push(path);
            }
            _ => {}
        }
    }
}

/// Whether one source file mentions the network call surface.
fn has_network_call_site(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            text.contains("reqwest::") || text.contains("http_client") || text.contains("web_fetch")
        }
        Err(_) => false,
    }
}

/// Workspace-relative display path (falls back to the full path).
fn relative(path: &Path, workspace: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .display()
        .to_string()
}
