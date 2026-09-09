//! `openpaw init` — create the OpenPaw home skeleton.
//!
//! Creates the standard subdirectories (memories / skills / goals /
//! sessions), self-checks that index.db opens, and prints the welcome
//! with the next steps. Safe to re-run: existing directories are kept.

use std::path::Path;

use openpaw_store::sqlite::SessionDb;

/// Standard subdirectories of the home skeleton.
const SKELETON_DIRS: [&str; 4] = ["memories", "skills", "goals", "sessions"];

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// `openpaw init [--home]` — skeleton + index.db self-check + welcome.
pub fn run(home: &Path) -> i32 {
    for dir in SKELETON_DIRS {
        let path = home.join(dir);
        if let Err(e) = std::fs::create_dir_all(&path) {
            report_error(
                format!("cannot create {}: {e}", path.display()),
                "the OpenPaw home must be writable to initialize the skeleton.",
                "check the path permissions or pass a writable --home.",
            );
            return 1;
        }
    }

    // index.db self-check: open (creates when missing) and read the
    // schema version back.
    let db_path = home.join("index.db");
    let version = match SessionDb::open(&db_path) {
        Ok(db) => match db.schema_version() {
            Ok(version) => version,
            Err(e) => {
                report_error(
                    format!("index.db opened but the schema query failed: {e}"),
                    "the schema_version table should be readable right after open.",
                    "remove index.db and re-run `openpaw init`.",
                );
                return 1;
            }
        },
        Err(e) => {
            report_error(
                format!("cannot open/create {}: {e}", db_path.display()),
                "index.db backs session list/show/search and must be a valid SQLite file.",
                "check the home permissions and re-run `openpaw init`.",
            );
            return 1;
        }
    };

    println!("OpenPaw initialized at {}", home.display());
    println!(
        "directories: {} (index.db self-check ok, schema version {version})",
        SKELETON_DIRS.join(", ")
    );
    println!();
    println!("next steps:");
    println!("  openpaw chat            start a conversation (offline by default)");
    println!("  openpaw evolve status   inspect the self-evolution surface");
    0
}
