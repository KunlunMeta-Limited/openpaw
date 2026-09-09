//! `xtask telemetry` — zero-telemetry gate (Q3).
//!
//! Two purely local scan surfaces:
//! 1. `Cargo.lock` package names, matched case-insensitively against a
//!    telemetry-SDK feature list (`sentry`, `mixpanel`, `segment-io`,
//!    ...). A bare `segment` would false-positive on unrelated crates,
//!    so the SDK's actual crate name `segment-io` is listed instead.
//! 2. Workspace `.rs` sources, scanned for telemetry call-site symbols:
//!    telemetry SDK path roots (SDK crate name followed by the path
//!    separator) and the quoted `telemetry` configuration key.
//!    Deliberately conservative — plain prose words never match.
//!
//! `docs/` and `tests/golden/` are exempt (same allowlist style as the
//! scrub gate); any line may opt out with the inline marker
//! `openpaw-telemetry-allow`. Check semantics are the default: any hit
//! fails the gate with exit code 1.

use anyhow::{Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};

/// Telemetry-SDK name fragments, matched as case-insensitive substrings
/// of Cargo.lock package names. `segment` is deliberately represented by
/// its SDK crate name `segment-io`: a bare `segment` would also match
/// unrelated crates (segment trees, text segmentation, ...).
const BANNED_DEPS: &[&str] = &[
    "sentry",
    "mixpanel",
    "segment-io",
    "analytics",
    "posthog",
    "datadog",
    "amplitude",
    "opentelemetry",
    "otel",
    "appinsights",
    "newrelic",
    "statsd",
];

/// Inline opt-out marker (same mechanism as the scrub gate).
const ALLOW_MARKER: &str = "openpaw-telemetry-allow";

pub fn run(root: &Path) -> Result<()> {
    let deps = lock_package_names(root)?;
    let rules = symbol_rules();

    let mut dep_hits: Vec<String> = Vec::new();
    for name in &deps {
        if let Some(frag) = dep_hit(name) {
            dep_hits.push(format!("{name} (telemetry SDK feature \"{frag}\")"));
        }
    }

    let mut symbol_hits: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    for path in scan_targets(root) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        scanned += 1;
        if is_exempt(&path) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        for (line_no, line) in content.lines().enumerate() {
            if line_hits(line, &rules) {
                symbol_hits.push(format!("{rel}:{}: {}", line_no + 1, trim_context(line)));
            }
        }
    }

    if dep_hits.is_empty() && symbol_hits.is_empty() {
        println!(
            "telemetry: {} dependencies scanned, {} symbols scanned — clean",
            deps.len(),
            scanned
        );
        Ok(())
    } else {
        for h in &dep_hits {
            eprintln!("BANNED dep: {h}");
        }
        for h in &symbol_hits {
            eprintln!("BANNED symbol: {h}");
        }
        eprintln!(
            "telemetry: {} dep hit(s), {} symbol hit(s) — zero-telemetry policy violated",
            dep_hits.len(),
            symbol_hits.len()
        );
        std::process::exit(1);
    }
}

/// Case-insensitive telemetry-SDK fragment match on one crate name;
/// returns the fragment that matched.
fn dep_hit(crate_name: &str) -> Option<&'static str> {
    let lower = crate_name.to_ascii_lowercase();
    BANNED_DEPS
        .iter()
        .copied()
        .find(|frag| lower.contains(frag))
}

/// Whether one source line carries a telemetry call-site symbol, with the
/// inline opt-out marker applied first.
fn line_hits(line: &str, rules: &[Regex]) -> bool {
    if line.contains(ALLOW_MARKER) {
        return false;
    }
    rules.iter().any(|re| re.is_match(line))
}

/// Telemetry call-site symbol rules for Rust source. The quoted config
/// key is assembled from parts so this scanner's own source never
/// carries the literal and self-flags.
fn symbol_rules() -> Vec<Regex> {
    vec![
        // Telemetry SDK path roots (`<sdk-name>::`).
        Regex::new(r"\b(sentry|mixpanel|posthog|opentelemetry|appinsights|newrelic)::").unwrap(),
        // The quoted `telemetry` configuration key.
        Regex::new(&format!(r#"{}telemetry{}"#, '"', '"')).unwrap(),
    ]
}

/// Package names parsed from Cargo.lock at the workspace root (line-level
/// `[[package]]` / `name = "..."` extraction — no toml dependency).
fn lock_package_names(root: &Path) -> Result<Vec<String>> {
    let lock = root.join("Cargo.lock");
    let content =
        std::fs::read_to_string(&lock).with_context(|| format!("read {}", lock.display()))?;
    let mut names = Vec::new();
    let mut in_package = false;
    for line in content.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_package = true;
        } else if line.starts_with('[') {
            in_package = false;
        } else if in_package && line.starts_with("name") {
            let value = line
                .split_once('=')
                .map(|(_, v)| v.trim().trim_matches('"'))
                .unwrap_or("");
            if !value.is_empty() {
                names.push(value.to_string());
            }
        }
    }
    anyhow::ensure!(
        !names.is_empty(),
        "no [[package]] names parsed from {} — is this a Cargo lockfile?",
        lock.display()
    );
    Ok(names)
}

/// Docs and golden samples may discuss telemetry policy itself.
fn is_exempt(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "docs" || s == "tests" && path.to_string_lossy().contains("golden")
    })
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
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect()
}

fn trim_context(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() > 100 {
        let cut: String = t.chars().take(100).collect();
        format!("{cut}…")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_feature_list_matches() {
        assert_eq!(dep_hit("sentry-actix"), Some("sentry"));
        assert_eq!(dep_hit("opentelemetry_sdk"), Some("opentelemetry"));
        assert_eq!(dep_hit("otel-collector"), Some("otel"));
        assert_eq!(dep_hit("Segment-IO"), Some("segment-io"));
        assert_eq!(dep_hit("serde"), None);
        // Bare `segment` must not match; only the SDK crate name does.
        assert_eq!(dep_hit("segment-tree"), None);
    }

    #[test]
    fn symbol_rules_match_call_sites_only() {
        let rules = symbol_rules();
        // SDK path roots match...
        assert!(line_hits("use sentry::init;", &rules)); // openpaw-telemetry-allow
        assert!(line_hits("let c = posthog::client();", &rules)); // openpaw-telemetry-allow
                                                                  // ...the quoted config key matches (assembled to avoid self-flag)...
        let config_key = format!("let t = cfg.get({}telemetry{});", '"', '"');
        assert!(line_hits(&config_key, &rules));
        // ...but plain module paths and prose words do not.
        assert!(!line_hits("crate::telemetry::run();", &rules));
        assert!(!line_hits("mod telemetry;", &rules));
        assert!(!line_hits("// analytics discussion in a comment", &rules));
    }

    #[test]
    fn inline_marker_opts_out() {
        let rules = symbol_rules();
        assert!(!line_hits(
            "use sentry::init; // openpaw-telemetry-allow",
            &rules
        ));
    }

    #[test]
    fn docs_and_golden_are_exempt() {
        assert!(is_exempt(Path::new("/repo/docs/plan.md")));
        assert!(is_exempt(Path::new("/repo/tests/golden/frontmatter/a.md")));
        assert!(!is_exempt(Path::new(
            "/repo/crates/openpaw-core/src/lib.rs"
        )));
        assert!(!is_exempt(Path::new("/repo/tests/common/mod.rs")));
    }
}
