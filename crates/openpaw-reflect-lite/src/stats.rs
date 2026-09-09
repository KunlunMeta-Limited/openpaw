//! Unverified-ratio statistics over persisted knowledge artifacts.
//!
//! Data source for the `evolve status` surface: scan a knowledge root
//! (memories layout: flat `*.md`; skills layout: `<name>/SKILL.md`) and
//! report what share of the artifacts carries `unverified: true` in its
//! frontmatter. The check is deliberately schema-agnostic: both store
//! frontmatter types expose the same flattened `extra` mapping, so the flag
//! is read from a generic YAML mapping instead of a concrete type.

use std::path::Path;

use serde_yml::Value;

/// Snapshot of one scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnverifiedStats {
    /// Artifacts scanned (files with a parseable frontmatter block).
    pub files: usize,
    /// Artifacts whose frontmatter contains `unverified: true`.
    pub unverified: usize,
}

impl UnverifiedStats {
    /// Share of unverified artifacts in `[0.0, 1.0]`; `0.0` when empty.
    pub fn ratio(&self) -> f64 {
        if self.files == 0 {
            0.0
        } else {
            self.unverified as f64 / self.files as f64
        }
    }
}

/// Scan one knowledge root. Files are counted when they match the store
/// layout (`.md` at depth one, or `SKILL.md` one directory deep); a file
/// whose frontmatter cannot be parsed counts towards `files` but never
/// towards `unverified`.
pub fn scan_unverified(root: &Path) -> std::io::Result<UnverifiedStats> {
    let mut stats = UnverifiedStats::default();
    if !root.exists() {
        return Ok(stats);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skills layout: <root>/<skill-name>/SKILL.md
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                stats.files += 1;
                if file_is_unverified(&skill_md) {
                    stats.unverified += 1;
                }
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            // Memory layout: flat markdown files.
            stats.files += 1;
            if file_is_unverified(&path) {
                stats.unverified += 1;
            }
        }
    }
    Ok(stats)
}

fn file_is_unverified(path: &Path) -> bool {
    match openpaw_store::read_doc::<serde_yml::Mapping>(path) {
        Ok(doc) => {
            doc.frontmatter.get(Value::String("unverified".into())) == Some(&Value::Bool(true))
        }
        Err(_) => false,
    }
}
