//! AST-argv destructive command verdicts (audit remediation).
//!
//! Destructive command detection is upgraded from regex substring matching
//! to AST argv semantics (regex is blind to long/multiple options:
//! `rm --recursive --force /` and `rm -rf --no-preserve-root /` both
//! bypassed the old regex).
//!
//! This module provides the authoritative verdict; callers (the engine's
//! DestructiveCommandGuardrail) fall back to the core regex check when the
//! AST is unavailable (ParseUnavailable/TooComplex).

use super::parser::{parse_for_security, ParseResult};

/// Verdict whether a command is a destructive rm (recursive / root deletion).
///
/// Returns `Some((pattern_name, matched_snippet))`:
/// - `"rm_root"` — deletes the filesystem root (Deny level);
/// - `"rm_recursive"` — recursive deletion (Confirm level).
///
/// Semantics (argv-based, not substring matching):
/// - a recursive flag after `rm`: `-r` / `-R` / a short-flag cluster
///   containing r/R (`-rf`, `-fr`) or the long option `--recursive`;
/// - **rm_root**: any non-flag argument resolves to the filesystem root
///   (`/`, `C:\`, `C:/`), with or without a recursive flag (`rm /` is
///   equally fatal);
/// - **rm_recursive**: a recursive flag is present and at least one
///   non-flag argument exists;
/// - `rm` with no target (`rm -r`) gets no verdict — no operand, no
///   destruction surface;
/// - a `sudo` prefix is stripped (`sudo rm -rf /` == `rm -rf /`);
/// - quoted targets are already expanded by the parser (`rm "/"` == `rm /`).
///
/// Returns `None` when the AST is unavailable (caller falls back to regex).
pub fn destructive_rm_verdict(command: &str) -> Option<(&'static str, String)> {
    let parsed = parse_for_security(command);
    let commands = match parsed {
        ParseResult::Simple { commands } => commands,
        // TooComplex / ParseUnavailable / ParseAborted → caller falls back to regex
        _ => return None,
    };

    for cmd in &commands {
        // sudo prefix stripping: `sudo rm ...` / `sudo -u root rm ...`?
        // Only a bare `sudo` is handled (argument forms like `-u root`
        // cannot be statically distinguished from targets — keep them).
        let mut idx = 0usize;
        if cmd.argv.first().map(|a| a == "sudo").unwrap_or(false) {
            idx = 1;
        }
        if cmd.argv.get(idx).map(|a| a.as_str()) != Some("rm") {
            continue;
        }
        let args = &cmd.argv[idx + 1..];

        let mut recursive = false;
        let mut targets: Vec<&str> = Vec::new();
        let mut end_of_options = false;
        for a in args {
            if !end_of_options && a == "--" {
                // end-of-options separator: everything after it is a target
                // (`rm -- -rf` deletes a file named "-rf").
                end_of_options = true;
                continue;
            }
            if !end_of_options && a.starts_with('-') && a.len() > 1 {
                if a == "--recursive" {
                    recursive = true;
                } else if a.starts_with("--") {
                    // Other long options (--no-preserve-root / --dry-run /
                    // -x=y): no per-character analysis — look-alike letters
                    // (the r in dry-run) do not constitute a recursive flag.
                } else {
                    // Short-flag cluster (`-rf` / `-fr` / `-R`): any r/R
                    // means recursive.
                    let body = &a[1..];
                    if !body.contains('=') && body.chars().any(|c| c == 'r' || c == 'R') {
                        recursive = true;
                    }
                }
            } else {
                targets.push(a.as_str());
            }
        }

        let matched = cmd.argv.join(" ");
        // rm_root: any target resolves to the root (no recursive flag needed
        // — `rm /` is equally fatal).
        if targets.iter().any(|t| is_root_path(t)) {
            return Some(("rm_root", matched));
        }
        if recursive && !targets.is_empty() {
            return Some(("rm_recursive", matched));
        }
    }
    None
}

/// Whether an (already quote-expanded) target is a filesystem root.
fn is_root_path(target: &str) -> bool {
    let t = target.trim();
    if t == "/" {
        return true;
    }
    // Windows drive root: `C:\` / `C:/` (case-insensitive).
    let bytes = t.as_bytes();
    if bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict_name(cmd: &str) -> Option<&'static str> {
        destructive_rm_verdict(cmd).map(|(n, _)| n)
    }

    // === T1-4 acceptance: long-option forms the old regex was blind to ===

    #[test]
    fn a06_rm_recursive_long_options_root_is_deny() {
        assert_eq!(verdict_name("rm --recursive --force /"), Some("rm_root"));
        assert_eq!(verdict_name("rm -rf --no-preserve-root /"), Some("rm_root"));
    }

    #[test]
    fn a06_rm_recursive_long_options_dir_is_confirm() {
        assert_eq!(verdict_name("rm --recursive dir"), Some("rm_recursive"));
        assert_eq!(
            verdict_name("rm --recursive --force build/"),
            Some("rm_recursive")
        );
        assert_eq!(verdict_name("rm -fr build/"), Some("rm_recursive"));
        assert_eq!(verdict_name("rm -Rf build/"), Some("rm_recursive"));
    }

    #[test]
    fn a06_rm_root_without_flags() {
        assert_eq!(verdict_name("rm /"), Some("rm_root"));
        assert_eq!(verdict_name("rm -f /"), Some("rm_root"));
        assert_eq!(verdict_name("rm / C:\\Users"), Some("rm_root"));
    }

    #[test]
    fn a06_windows_drive_root() {
        // Note: a bare `C:\` in bash has `\` as an escape character (the
        // target degrades to `C:`) — real Windows root deletion is written
        // quoted or with forward slashes.
        assert_eq!(verdict_name("rm -rf 'C:\\'"), Some("rm_root"));
        assert_eq!(verdict_name("rm -rf c:/"), Some("rm_root"));
        // Non-root path is not root-level
        assert_eq!(verdict_name("rm -rf C:/tmp"), Some("rm_recursive"));
    }

    #[test]
    fn a06_sudo_prefix_stripped() {
        assert_eq!(verdict_name("sudo rm -rf /"), Some("rm_root"));
        assert_eq!(
            verdict_name("sudo rm --recursive dir"),
            Some("rm_recursive")
        );
    }

    // === Green anchors: no false positives ===

    #[test]
    fn a06_non_recursive_rm_not_flagged() {
        assert_eq!(verdict_name("rm file.txt"), None);
        assert_eq!(verdict_name("rm -f file.txt"), None);
        assert_eq!(verdict_name("rm -- file.txt"), None);
    }

    #[test]
    fn a06_recursive_without_target_not_flagged() {
        // `rm -r` with no target = usage error, no destruction surface.
        assert_eq!(verdict_name("rm -r"), None);
        assert_eq!(verdict_name("rm --recursive"), None);
    }

    #[test]
    fn a06_other_commands_not_flagged() {
        assert_eq!(verdict_name("ls -r"), None);
        assert_eq!(verdict_name("grep -r pattern dir"), None);
        assert_eq!(verdict_name("rm -i file"), None);
        // Look-alike flag: the r in --dry-run is not a recursive flag
        // (long options are not split per character).
        assert_eq!(verdict_name("rm --dry-run file"), None);
    }

    #[test]
    fn a06_unparseable_returns_none_for_regex_fallback() {
        // Command substitution → TooComplex → None (caller falls back to regex).
        assert_eq!(destructive_rm_verdict("rm -rf $(compute_target)"), None);
    }
}
