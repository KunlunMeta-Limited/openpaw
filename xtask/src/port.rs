//! `xtask port` — semi-automatic crate porting helper.
//!
//! Copies `.rs` files from an upstream source directory into the target
//! crate, applies the mechanical renames, then runs the banned-word
//! precheck on the copied files (report-only — semantic fixes are a human
//! decision).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

fn apply_renames(content: &str) -> String {
    // The rename table is the one legitimate carrier of these literals;
    // each row carries the inline scan allowance marker.
    content
        .replace("FREEAGENT", "OPENPAW") // openpaw-scrub-allow
        .replace("FreeAgent", "OpenPaw") // openpaw-scrub-allow
        .replace("freeagent_", "openpaw_") // openpaw-scrub-allow
        .replace("freeagent-", "openpaw-") // openpaw-scrub-allow
        .replace("~/.freeagent", "~/.openpaw") // openpaw-scrub-allow
        .replace("./.freeagent", "./.openpaw") // openpaw-scrub-allow
        .replace(".freeagent", ".openpaw") // openpaw-scrub-allow
        .replace("freeagent", "openpaw") // openpaw-scrub-allow
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let get = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let Some(crate_name) = get("--crate") else {
        bail!("missing --crate <name>");
    };
    let Some(from) = get("--from") else {
        bail!("missing --from <dir>");
    };
    let files_filter: Option<String> = get("--files");

    let from_dir = PathBuf::from(&from);
    if !from_dir.is_dir() {
        bail!("--from must be a directory: {from}");
    }
    let dest_dir = root.join("crates").join(&crate_name).join("src");
    if !dest_dir.parent().is_some_and(|p| p.is_dir()) {
        bail!("unknown crate: {crate_name}");
    }

    let mut copied = 0usize;
    let mut precheck_hits: Vec<String> = Vec::new();

    // Precheck patterns, compiled once (same rules as `scrub --check`,
    // minus the internal-doc-name rule which cannot appear in copied code).
    let re_codename = regex::Regex::new(r"(?i)\bsimon\w*")?;
    let re_task_id = regex::Regex::new(r"\b[A-Z]-?\d{2,}\b")?;

    for entry in walkdir::WalkDir::new(&from_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let rel = path.strip_prefix(&from_dir).unwrap_or(path);
        let rel_str = rel.to_string_lossy().to_string();
        if let Some(f) = &files_filter {
            let re = regex::Regex::new(f)?;
            if !re.is_match(&rel_str) {
                continue;
            }
        }
        let original =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let converted = apply_renames(&original);
        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &converted)?;

        // Precheck report (report-only).
        for (idx, line) in converted.lines().enumerate() {
            if line.contains("openpaw-scrub-allow") {
                continue;
            }
            let task_hit = re_task_id.find(line).is_some_and(|m| {
                let s = m.as_str();
                !(s.starts_with('Q') || s.starts_with('P')) || s.contains('-')
            });
            if re_codename.is_match(line) || task_hit {
                precheck_hits.push(format!("{}:{}: {}", rel_str, idx, line.trim()));
            }
        }
        copied += 1;
        println!("ported: {rel_str} -> crates/{crate_name}/src/{rel_str}");
    }

    println!("port: {copied} file(s) into {crate_name}");
    if precheck_hits.is_empty() {
        println!("port precheck: clean (still run `xtask scrub --check`)");
    } else {
        println!(
            "port precheck: {} suspicious line(s) — human review required:",
            precheck_hits.len()
        );
        for h in &precheck_hits {
            println!("  {h}");
        }
    }
    Ok(())
}
