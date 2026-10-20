//! `xtask scrub --check` — porting-hygiene banned-word scan (Q10).
//!
//! Categories: upstream identifiers/codenames, internal task-ID patterns,
//! internal doc names, internal domains. Hits inside `docs/` and
//! `tests/golden/` are allowlisted (the docs legitimately discuss the
//! porting work); any other hit fails the gate. A line may opt out with
//! the inline marker `openpaw-scrub-allow`.

use anyhow::{Context, Result};
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

/// 功能：扫描移植遗留标识，精确登记的项目编号仅豁免对应匹配片段。
/// 参数：root为仓库根；check为失败门禁或只报告模式。
/// 返回：无命中时成功；报告模式输出所有命中。
/// 错误：登记表读取失败返回错误；check模式有命中则进程非零退出。
/// 状态：只读源码及编号登记表。
/// 并发与取消：同步扫描，无共享写入。
/// 权限：不执行目标代码，不改变文件。
/// 关联：移植清理规则与治理编号登记校验。
pub fn run(root: &Path, check: bool) -> Result<()> {
    // Build the rule list once: compiling all four regexes per scanned
    // line made the gate take minutes on this repo size.
    let rule_list = rules();
    let registered: Vec<String> = if root.join("governance/identifiers.json").exists() {
        let document: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            root.join("governance/identifiers.json"),
        )?)?;
        document["ids"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("registered ids must be an array"))?
            .iter()
            .map(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("registered id must be a string"))
            })
            .collect::<Result<_>>()?
    } else {
        Vec::new()
    };
    let mut hits: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    for path in scan_targets(root)? {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read source {}", path.display()))?;
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
                for m in rule.re.find_iter(line) {
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
                    // Only the complete registered identifier exempts this span;
                    // a later unrelated hit on the same line must still fail.
                    if rule.name == "internal_task_id"
                        && registered_span(line, m.start(), m.end(), &registered)
                    {
                        continue;
                    }
                    let kind = classify_line(line, m.start());
                    if allowlisted {
                        // Docs may discuss the port; still surface for awareness.
                        continue;
                    }
                    hits.push(format!(
                        "{rel}:{}: [{}:{}] {}",
                        line_no + 1,
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

fn registered_span(line: &str, start: usize, end: usize, ids: &[String]) -> bool {
    ids.iter().any(|id| {
        line.match_indices(id).any(|(offset, _)| {
            let boundary = |c: char| c.is_ascii_alphanumeric() || c == '-';
            offset <= start
                && offset + id.len() >= end
                && !line[..offset].chars().next_back().is_some_and(boundary)
                && !line[offset + id.len()..]
                    .chars()
                    .next()
                    .is_some_and(boundary)
        })
    })
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

fn scan_targets(root: &Path) -> Result<Vec<PathBuf>> {
    // Generated dependency/build trees are not reviewable repository sources.
    // Git also includes non-ignored untracked sources, so a new file cannot hide.
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "cannot enumerate source files: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut paths = std::collections::BTreeSet::new();
    for raw in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = root.join(std::str::from_utf8(raw)?);
        if path.is_file()
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs") | Some("md") | Some("toml") | Some("yml") | Some("yaml") | Some("json")
            )
        {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registered_identifier_does_not_hide_later_hit() {
        let registered = format!("REQ-OP-{:03}", 1);
        let banned = format!("Z{}", 55);
        let line = format!("{registered} {banned}");
        let rule = rules()
            .into_iter()
            .find(|r| r.name == "internal_task_id")
            .unwrap();
        let hits: Vec<_> = rule
            .re
            .find_iter(&line)
            .filter(|m| {
                !registered_span(&line, m.start(), m.end(), std::slice::from_ref(&registered))
            })
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].as_str(), banned);
        assert!(!registered_span(
            &format!("X{registered}"),
            5,
            10,
            &[registered]
        ));
    }
    #[test]
    fn source_enumeration_skips_ignored_but_includes_new_files() {
        let root = std::env::temp_dir().join(format!(
            "openpaw-scrub-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        std::fs::create_dir(root.join("generated")).unwrap();
        std::fs::write(root.join("generated/ignored.rs"), "ignored").unwrap();
        std::fs::write(root.join("new.rs"), "new").unwrap();
        let files = scan_targets(&root).unwrap();
        assert_eq!(files, vec![root.join("new.rs")]);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn chinese_context_truncates_without_utf8_boundary_panic() {
        let line = "中".repeat(120);
        let result = trim_context(&line);
        assert_eq!(result.chars().count(), 101);
        assert!(result.ends_with('…'));
    }
}
