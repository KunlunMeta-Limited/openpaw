//! MEMORY.md entrypoint index management.
//!
//! Scans a memory directory of `*.md` frontmatter files and builds a compact
//! Markdown index listing each memory's id, file name, and description.
//! The index is truncated to a maximum line count and byte size to keep the
//! entrypoint file (`MEMORY.md`) within LLM context budgets.
//!
//! Design notes:
//! - Reuses [`openpaw_store::frontmatter::read_doc`] for parsing, so BOM,
//!   CRLF, and 16 MB size-limit handling are shared with the rest of the
//!   codebase.
//! - Does **not** mutate the existing `store.rs` put/get path. Callers may
//!   invoke [`build_memory_index`] after writes to refresh the entrypoint.

use std::path::Path;

use openpaw_store::frontmatter::{read_doc, FrontmatterDoc};

use crate::memory::MemoryFrontmatter;

/// File name of the entrypoint index. Skipped while scanning a memory dir.
pub const ENTRYPOINT_NAME: &str = "MEMORY.md";

/// Hard limit on entrypoint line count. Mirrors the Claude Code reference.
pub const MAX_ENTRYPOINT_LINES: usize = 200;

/// Hard limit on entrypoint byte size (≈25 KB ≈ 6k tokens).
pub const MAX_ENTRYPOINT_BYTES: usize = 25_000;

/// Parse the frontmatter of a memory `.md` file.
///
/// Returns `None` if the file cannot be read or does not contain valid
/// frontmatter. This is intentionally lenient: a single corrupted file must
/// not abort an entire index rebuild.
fn parse_frontmatter(path: &Path) -> Option<MemoryFrontmatter> {
    let doc: FrontmatterDoc<MemoryFrontmatter> = read_doc(path).ok()?;
    Some(doc.frontmatter)
}

/// Build the `MEMORY.md` index content for a memory directory.
///
/// Scans `memory_dir` for `*.md` files (skipping `MEMORY.md` itself),
/// extracts each file's frontmatter, and produces a sorted Markdown list of
/// `- [id](file:name) — description` entries. The result is truncated to
/// both [`MAX_ENTRYPOINT_LINES`] lines and [`MAX_ENTRYPOINT_BYTES`] bytes
/// (whichever is hit first).
///
/// Errors are returned only for unrecoverable I/O failures (e.g. the
/// directory does not exist). Per-file parse failures are silently skipped
/// so a single bad file cannot break the entire index.
pub fn build_memory_index(memory_dir: &Path) -> std::io::Result<String> {
    let mut entries: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(memory_dir)? {
        let entry = entry?;
        let path = entry.path();

        // Only consider .md files.
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        // Skip the entrypoint itself to avoid self-reference.
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n == ENTRYPOINT_NAME)
            .unwrap_or(false)
        {
            continue;
        }

        if let Some(fm) = parse_frontmatter(&path) {
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<invalid>");
            let description = if fm.description.is_empty() {
                "(no description)"
            } else {
                fm.description.as_str()
            };
            entries.push(format!(
                "- [{}](file:{}) — {}",
                fm.id, file_name, description
            ));
        }
    }

    entries.sort();
    Ok(truncate_entrypoint(&entries.join("\n")))
}

/// Truncate an entrypoint body to the configured line and byte limits.
///
/// The limits are enforced in order: first by line count, then by byte
/// size. A truncation marker `... (truncated)` is appended when either
/// limit is hit so consumers can distinguish complete vs. partial indices.
pub fn truncate_entrypoint(content: &str) -> String {
    // 1. Line limit.
    let mut truncated: String = content
        .lines()
        .take(MAX_ENTRYPOINT_LINES)
        .collect::<Vec<&str>>()
        .join("\n");

    let line_truncated = truncated.len() < content.len();

    // 2. Byte limit. Walk by char boundary to avoid splitting UTF-8.
    let byte_truncated = if truncated.len() > MAX_ENTRYPOINT_BYTES {
        let mut end = MAX_ENTRYPOINT_BYTES;
        while end > 0 && !truncated.is_char_boundary(end) {
            end -= 1;
        }
        truncated.truncate(end);
        true
    } else {
        false
    };

    if line_truncated || byte_truncated {
        truncated.push_str("\n... (truncated)");
    }
    truncated
}

/// Test helpers scoped to this module's tests.
#[cfg(test)]
mod index_test_helpers {
    use std::path::Path;

    use openpaw_store::frontmatter::{write_doc_atomic, FrontmatterDoc};

    use crate::memory::{Confidence, MemoryFrontmatter, MemoryType, Source};

    pub fn write_memory(path: &Path, id: &str, description: &str) {
        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::High, Vec::new());
        fm.id = id.to_string();
        fm.description = description.to_string();
        fm.memory_type = MemoryType::Fact;
        let doc = FrontmatterDoc {
            frontmatter: fm,
            body: format!("Body for {id}"),
        };
        write_doc_atomic(path, &doc).expect("write_memory failed");
    }
}

#[cfg(test)]
mod tests {
    use super::index_test_helpers::write_memory;
    use super::*;

    /// Verify a basic index with three memories is rendered correctly.
    #[test]
    fn builds_index_from_memory_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(
            &dir.path().join("a.md"),
            "mem_a",
            "First memory description",
        );
        write_memory(
            &dir.path().join("b.md"),
            "mem_b",
            "Second memory description",
        );
        write_memory(
            &dir.path().join("c.md"),
            "mem_c",
            "Third memory description",
        );

        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.contains("mem_a"), "index should list mem_a: {index}");
        assert!(index.contains("mem_b"), "index should list mem_b: {index}");
        assert!(index.contains("mem_c"), "index should list mem_c: {index}");
        assert!(
            index.contains("First memory description"),
            "index should include description: {index}"
        );
        // No truncation marker expected for small inputs.
        assert!(
            !index.contains("(truncated)"),
            "no truncation expected: {index}"
        );
    }

    /// The entrypoint file itself must not appear in the index.
    #[test]
    fn skips_entrypoint_file() {
        let dir = tempfile::tempdir().unwrap();
        // Place a hand-written MEMORY.md in the dir — must be skipped.
        std::fs::write(dir.path().join(ENTRYPOINT_NAME), "# Memory Index\n").unwrap();
        write_memory(&dir.path().join("a.md"), "mem_a", "desc");

        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.contains("mem_a"));
        // The entrypoint's body ("# Memory Index") must not leak in.
        assert!(!index.contains("Memory Index"));
    }

    /// Non-markdown files are ignored.
    #[test]
    fn ignores_non_markdown_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        write_memory(&dir.path().join("a.md"), "mem_a", "desc");

        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.contains("mem_a"));
        assert!(!index.contains("ignore me"));
    }

    /// A corrupted .md file is silently skipped; the rest of the index is
    /// still produced.
    #[test]
    fn skips_corrupted_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.md"), "no frontmatter here").unwrap();
        write_memory(&dir.path().join("good.md"), "mem_good", "good desc");

        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.contains("mem_good"));
        assert!(!index.contains("no frontmatter"));
    }

    /// An empty directory yields an empty (but valid) index.
    #[test]
    fn empty_dir_yields_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.is_empty(), "expected empty index, got: {index:?}");
    }

    /// Line truncation kicks in at [`MAX_ENTRYPOINT_LINES`].
    #[test]
    fn truncates_by_line_count() {
        let dir = tempfile::tempdir().unwrap();
        // Write more memories than the line limit.
        for i in 0..(MAX_ENTRYPOINT_LINES + 50) {
            write_memory(
                &dir.path().join(format!("m{i}.md")),
                &format!("mem_{i}"),
                "desc",
            );
        }
        let index = build_memory_index(dir.path()).unwrap();
        assert!(
            index.contains("(truncated)"),
            "expected truncation marker: {index}"
        );
        // Line count (excluding the marker line) should be at most the limit.
        let line_count = index.lines().count();
        assert!(
            line_count <= MAX_ENTRYPOINT_LINES + 1,
            "expected ≤ {MAX_ENTRYPOINT_LINES}+1 lines, got {line_count}"
        );
    }

    /// Byte truncation kicks in at [`MAX_ENTRYPOINT_BYTES`].
    #[test]
    fn truncates_by_byte_size() {
        let dir = tempfile::tempdir().unwrap();
        // A few memories with very long descriptions.
        let long_desc = "x".repeat(10_000);
        for i in 0..10 {
            write_memory(
                &dir.path().join(format!("m{i}.md")),
                &format!("mem_{i}"),
                &long_desc,
            );
        }
        let index = build_memory_index(dir.path()).unwrap();
        assert!(index.contains("(truncated)"));
        assert!(
            index.len() <= MAX_ENTRYPOINT_BYTES + 32,
            "expected ≤ {}+32 bytes, got {}",
            MAX_ENTRYPOINT_BYTES,
            index.len()
        );
    }

    /// `truncate_entrypoint` is a pure function — empty input yields empty
    /// output, no marker.
    #[test]
    fn truncate_empty_no_marker() {
        let out = truncate_entrypoint("");
        assert!(out.is_empty());
    }

    /// `truncate_entrypoint` does not split UTF-8 codepoints.
    #[test]
    fn truncate_respects_char_boundary() {
        // Each '🎉' is 4 bytes; ensure we never cut mid-codepoint.
        let s = "🎉".repeat(10_000);
        let out = truncate_entrypoint(&s);
        assert!(out.len() <= MAX_ENTRYPOINT_BYTES + 32);
        // The output should be valid UTF-8 (String guarantees this, but the
        // assertion documents the intent).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
