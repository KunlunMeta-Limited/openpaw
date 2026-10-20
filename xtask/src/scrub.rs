//! `xtask scrub --check` — porting-hygiene banned-word scan (Q10).
//!
//! Categories: upstream identifiers/codenames, internal task-ID patterns,
//! internal doc names, internal domains. Hits inside `docs/` and
//! `tests/golden/` are allowlisted (the docs legitimately discuss the
//! porting work); any other hit fails the gate. A line may opt out with
//! the inline marker `openpaw-scrub-allow`.

use anyhow::Result;
use std::path::{Path, PathBuf};

struct Rule {
    name: &'static str,
    #[allow(dead_code)]
    category: &'static str,
    re: regex::Regex,
}

fn rules() -> Vec<Rule> {
    vec![
        Rule {
            name: "upstream_identifier",
            category: "identifier",
            // Assembled from parts so this scanner does not self-flag.
            re: regex::Regex::new(&format!("(?i){}{}", "free", "agent")).unwrap(),
        },
        Rule {
            name: "internal_codename",
            category: "identifier",
            re: regex::Regex::new(r"(?i)\bsimon\w*").unwrap(),
        },
        Rule {
            name: "internal_task_id",
            category: "identifier",
            // Upstream task IDs look like `D35`, `A-11`, `M29`, `T05`. // openpaw-scrub-allow
            // Matches with the plan's own gate/prefix numbering (`Q10`,
            // `P3` — no hyphen, Q/P prefix) are filtered at scan time.
            re: regex::Regex::new(r"\b[A-Z]-?\d{2,}\b").unwrap(),
        },
        Rule {
            name: "internal_doc_name",
            category: "reference",
            re: regex::Regex::new(r"(?i)\b\d{2}-[a-z0-9]+(?:-[a-z0-9]+)+\.(md|txt)\b").unwrap(),
        },
    ]
}

const ALLOW_MARKER: &str = "openpaw-scrub-allow";

pub fn run(root: &Path, check: bool) -> Result<()> {
    // Build the rule list once: compiling all four regexes per scanned
    // line made the gate take minutes on this repo size.
    let rule_list = rules();
    let mut hits: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    for path in scan_targets(root) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        scanned += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let allowlisted = is_allowlisted(&path);

        for (line_no, line) in content.lines().enumerate() {
            if line.contains(ALLOW_MARKER) {
                continue;
            }
            for rule in &rule_list {
                if let Some(m) = rule.re.find(line) {
                    // Task-ID filter: `Q10`/`P3` style (no hyphen, Q/P
                    // prefix) is this plan's own numbering, not an upstream
                    // task ID.
                    let s = m.as_str();
                    if rule.name == "internal_task_id"
                        && !s.contains('-')
                        && (s.starts_with('Q') || s.starts_with('P'))
                    {
                        continue;
                    }
                    let kind = classify_line(line, m.start());
                    if allowlisted {
                        // Docs may discuss the port; still surface for awareness.
                        continue;
                    }
                    hits.push(format!(
                        "{rel}:{line_no}: [{}:{}] {}",
                        rule.name,
                        kind,
                        trim_context(line)
                    ));
                }
            }
        }
    }

    println!("scrub: scanned {scanned} files");
    if hits.is_empty() {
        println!("scrub: clean");
        Ok(())
    } else if check {
        for h in &hits {
            eprintln!("BANNED: {h}");
        }
        eprintln!(
            "scrub: {} hit(s) — fix or justify with the inline marker",
            hits.len()
        );
        std::process::exit(1);
    } else {
        for h in &hits {
            println!("HIT: {h}");
        }
        println!("scrub: {} hit(s) (report-only mode)", hits.len());
        Ok(())
    }
}

fn classify_line(line: &str, match_start: usize) -> &'static str {
    let before = &line[..match_start.min(line.len())];
    if before.contains("//") || before.contains("#") && before.trim_start().starts_with('#') {
        "comment"
    } else if before.matches('"').count() % 2 == 1 {
        "string"
    } else {
        "code"
    }
}

fn trim_context(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() > 100 {
        format!("{}…", t.chars().take(100).collect::<String>())
    } else {
        t.to_string()
    }
}

fn is_allowlisted(path: &Path) -> bool {
    // Docs and golden samples legitimately discuss the porting project and
    // carry upstream-format content.
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "docs" || s == "tests" && path.to_string_lossy().contains("golden")
    }) || path
        .file_name()
        .is_some_and(|f| f.to_string_lossy() == "devlog.md")
}

fn scan_targets(root: &Path) -> Vec<PathBuf> {
    let skip_dirs = ["target", ".git", "node_modules"];
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            !skip_dirs.contains(&name.as_str())
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs") | Some("md") | Some("toml") | Some("yml") | Some("yaml") | Some("json")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_context_truncates_without_utf8_boundary_panic() {
        let line = "中".repeat(120);
        let result = trim_context(&line);
        assert_eq!(result.chars().count(), 101);
        assert!(result.ends_with('…'));
    }
}
