//! Read-only command detection.
//!
//! Determines whether a bash command is "read-only" (no side effects),
//! allowing it to bypass approval and run in parallel.
//!
//! # Strategy
//! 1. Parse command via `parse_for_security()`
//! 2. TooComplex/ParseUnavailable → not readonly (conservative)
//! 3. Check all sub-commands against readonly command sets
//! 4. Validate flags for commands with both readonly and mutating forms

use super::parser::{parse_for_security, ParseResult, Redirect, RedirectKind, SimpleCommand};
use std::collections::HashSet;

/// Whether a redirect writes to a real file (vs. reading input via `<`,
/// merging streams via `2>&1`, or discarding via `/dev/null`).
///
/// Any file-writing redirect makes a command non-readonly: `echo x > file`
/// can modify arbitrary paths and must not bypass approval (auto-approve
/// and the LLM classifier both gate on `is_readonly`).
fn is_file_write_redirect(r: &Redirect) -> bool {
    // fd duplication (`2>&1`, `>&2`) merges streams — not a file write.
    // /dev/null and /dev/std{out,err} targets discard or re-route output.
    let target = r.target.trim().trim_start_matches('&');
    if matches!(
        target,
        "1" | "2" | "/dev/null" | "/dev/stdout" | "/dev/stderr"
    ) {
        return false;
    }
    !matches!(r.kind, RedirectKind::Stdin)
}

/// True if the command contains any redirect that writes to a file.
fn has_file_write_redirect(cmd: &SimpleCommand) -> bool {
    cmd.redirects.iter().any(is_file_write_redirect)
}

/// Commands that are inherently read-only (no side effects).
///
/// These commands only read data and produce output — they never modify
/// the filesystem, network, or process state.
fn builtin_readonly_commands() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    // Listing / navigation
    for c in &[
        "ls", "tree", "pwd", "du", "df", "stat", "file", "realpath", "basename", "dirname",
    ] {
        set.insert(*c);
    }
    // Reading
    for c in &[
        "cat", "head", "tail", "less", "more", "wc", "nl", "strings", "xxd", "od",
    ] {
        set.insert(*c);
    }
    // Searching
    for c in &[
        "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
    ] {
        set.insert(*c);
    }
    // Processing (read-only)
    for c in &[
        "sort", "uniq", "cut", "tr", "paste", "column", "jq", "yq", "awk", "sed",
    ] {
        set.insert(*c);
    }
    // Diff
    for c in &["diff", "comm", "cmp"] {
        set.insert(*c);
    }
    // System info. `env` and `command` removed — they are wrappers
    // (semantics::SAFE_WRAPPERS) and must be stripped + re-inspected, not
    // whitelisted by name.
    for c in &[
        "uname", "whoami", "id", "hostname", "date", "uptime", "printenv", "tty", "arch",
    ] {
        set.insert(*c);
    }
    // Git read-only subcommands (handled separately for flag validation)
    for c in &["echo", "printf", "true", "false", ":", "test", "seq", "yes"] {
        set.insert(*c);
    }
    // Package managers (read-only queries)
    for c in &["cargo", "rustc", "rustup"] {
        set.insert(*c);
    }
    set
}

/// Commands that are "neutral" — neither readonly nor destructive.
/// Used for compound command analysis (e.g. `ls && echo done`).
fn neutral_commands() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    for c in &["echo", "printf", "true", "false", ":", "seq", "yes"] {
        set.insert(*c);
    }
    set
}

/// Check if a single parsed command is read-only.
///
/// Handles flag validation for commands with both readonly and mutating forms
/// (e.g. `find -delete`, `sed -i`, `git push`).
fn is_command_readonly(cmd: &SimpleCommand) -> bool {
    // Any file-writing redirect (e.g. `echo x > file`) can modify arbitrary
    // paths — never treat such a command as readonly. Checked before the
    // argv lookup so a bare `> file` is also caught.
    if has_file_write_redirect(cmd) {
        return false;
    }

    // Audit remediation: strip safe wrappers BEFORE the whitelist lookup.
    // `env python3 -c ...` used to hit the `env` whitelist entry and was
    // auto-approved (whitelist + LLM classifier) — free-form RCE. The semantics
    // layer strips wrappers fail-closed: any unknown wrapper argument
    // (`env -u/-i/VAR=x`, `time -p`, `command -v`, ...) yields None →
    // not readonly. Bare `env` strips to an empty argv → trivially readonly
    // (it only prints the environment).
    let argv = match super::semantics::strip_safe_wrappers(&cmd.argv) {
        Some(argv) => argv,
        None => return false, // fail-closed: unknown wrapper argument
    };

    let first = match argv.first() {
        Some(s) => s.as_str(),
        None => return true, // bare `env` — trivially readonly
    };

    // Git special handling (parameterized subcommands).
    if first == "git" {
        return is_git_readonly(&argv);
    }

    // sed special handling: -i / -iSUFFIX / --in-place[=SUFFIX] make it
    // non-readonly (the old prefix check missed the long form).
    if first == "sed" {
        return !argv
            .iter()
            .skip(1)
            .any(|a| a == "-i" || a.starts_with("-i") || a.starts_with("--in-place"));
    }

    // find special handling: mutating actions make it non-readonly.
    // -fprint/-fprintf/-fprint0/-fls write to arbitrary files.
    if first == "find" {
        return !argv.iter().skip(1).any(|a| {
            a == "-delete"
                || a == "-exec"
                || a == "-execdir"
                || a == "-ok"
                || a == "-okdir"
                || a.starts_with("-fprint")
                || a.starts_with("-fls")
        });
    }

    // awk/gawk: the script body can execute arbitrary commands via system()
    // or "| getline" (previously only jq was checked).
    if first == "awk" || first == "gawk" {
        return !argv
            .iter()
            .skip(1)
            .any(|a| a.contains("system(") || a.contains("system (") || a.contains("| getline"));
    }

    // sort: -o writes to file
    if first == "sort" {
        return !argv.iter().any(|a| a == "-o" || a.starts_with("-o"));
    }

    // tee: writes to file
    if first == "tee" {
        return false;
    }

    // cp/mv/rm/mkdir/rmdir/chmod/chown/ln/touch: all have side effects
    if matches!(
        first,
        "cp" | "mv"
            | "rm"
            | "mkdir"
            | "rmdir"
            | "chmod"
            | "chown"
            | "ln"
            | "touch"
            | "dd"
            | "mkfs"
            | "mount"
            | "umount"
            | "kill"
            | "killall"
            | "pkill"
            | "install"
            | "strip"
            | "patch"
            | "rsync"
            | "scp"
            | "sftp"
    ) {
        return false;
    }

    // cargo: some subcommands are readonly, others aren't
    if first == "cargo" {
        if let Some(subcmd) = argv.get(1) {
            return matches!(
                subcmd.as_str(),
                "check" | "tree" | "metadata" | "search" | "locate-project"
            );
        }
        return false;
    }

    // rustc: --version etc are readonly
    if first == "rustc" {
        return argv
            .iter()
            .any(|a| a == "--version" || a == "-V" || a == "--print");
    }

    // Check builtin readonly set
    builtin_readonly_commands().contains(first)
}

/// Check if a git command is read-only based on its subcommand.
///
/// Audit remediation: polymorphic subcommands (`branch`, `tag`, `remote`,
/// `stash`, `reflog`) have both read-only and mutating forms — the old
/// first-word lookup marked the mutating forms readonly. Rule: a polymorphic
/// subcommand is readonly only in an explicitly read-only argument form;
/// anything unrecognized fails closed (approval).
fn is_git_readonly(argv: &[String]) -> bool {
    let Some(subcmd) = argv.get(1) else {
        return false; // bare `git` — no subcommand
    };
    let sub = subcmd.as_str();
    let rest = &argv[2..];

    // `--output=<file>` writes a file even for inspection subcommands.
    if rest.iter().any(|a| a.starts_with("--output")) {
        return false;
    }

    // Always-readonly inspection subcommands.
    if matches!(
        sub,
        "status"
            | "log"
            | "diff"
            | "show"
            | "blame"
            | "shortlog"
            | "describe"
            | "rev-parse"
            | "ls-files"
            | "ls-tree"
            | "name-rev"
            | "grep"
    ) {
        return true;
    }

    // `git config` — readonly only in explicit get/list forms.
    if sub == "config" {
        return !rest.iter().any(|a| {
            a == "--global"
                || a == "--local"
                || a == "--system"
                || a == "--unset"
                || a == "--replace-all"
        }) && rest
            .iter()
            .any(|a| a == "--get" || a == "--get-all" || a == "--list" || a == "-l");
    }

    // Polymorphic subcommands.
    match sub {
        "branch" => {
            // `git branch` bare or listing flags → readonly; creation (-c),
            // copy, delete (-D/-d), rename (-m/-M), set-upstream → mutating.
            rest.is_empty()
                || rest.iter().all(|a| {
                    matches!(
                        a.as_str(),
                        "-a" | "-r"
                            | "-v"
                            | "-vv"
                            | "--all"
                            | "--remotes"
                            | "--list"
                            | "--show-current"
                    )
                })
        }
        "tag" => rest.is_empty() || rest.iter().all(|a| matches!(a.as_str(), "-l" | "--list")),
        "remote" => {
            // `git remote` bare lists remotes; -v/--verbose/get-url/show read.
            // add/set-url/rename/remove/prune are mutating.
            rest.is_empty()
                || matches!(
                    rest.first().map(|s| s.as_str()),
                    Some("-v") | Some("--verbose") | Some("get-url") | Some("show")
                )
        }
        "stash" => {
            // `git stash` bare = save = mutating.
            matches!(
                rest.first().map(|s| s.as_str()),
                Some("list") | Some("show")
            )
        }
        "reflog" => {
            // `git reflog` bare = show; delete/expire mutate history.
            rest.is_empty() || matches!(rest.first().map(|s| s.as_str()), Some("show"))
        }
        _ => false, // unknown/mutating subcommand → fail closed
    }
}

/// Comprehensive read-only judgment for a command string.
///
/// # Rules
/// 1. Parse via `parse_for_security()`
/// 2. `TooComplex` / `ParseUnavailable` → `false` (conservative)
/// 3. All sub-commands must be readonly or neutral
/// 4. Each command's flags must be validated
///
/// # Examples
/// ```
/// use openpaw_tools::bashast::is_readonly;
/// assert!(is_readonly("ls -la"));
/// assert!(is_readonly("cat file | grep pattern"));
/// assert!(!is_readonly("rm file"));
/// assert!(!is_readonly("ls && rm file"));
/// ```
pub fn is_readonly(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }

    let parsed = parse_for_security(command);
    match parsed {
        ParseResult::Simple { commands } => {
            if commands.is_empty() {
                // e.g. a bare `> file` — the statement has no command the
                // redirect could be attributed to; treat it conservatively
                // as non-readonly (it may truncate/create a file).
                return false;
            }
            // All commands must be readonly or neutral. Neutral commands
            // (echo/printf/...) lose their neutral status when they carry a
            // file-writing redirect (`echo hacked > ~/.bashrc`).
            commands.iter().all(|cmd| {
                let is_ro = is_command_readonly(cmd);
                let is_neutral = cmd
                    .argv
                    .first()
                    .map(|s| neutral_commands().contains(s.as_str()))
                    .unwrap_or(false)
                    && !has_file_write_redirect(cmd);
                is_ro || is_neutral
            })
        }
        ParseResult::TooComplex { .. } => false, // Conservative
        ParseResult::ParseUnavailable => false,  // Conservative
        ParseResult::ParseAborted { .. } => false, // Conservative
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Basic readonly ===

    #[test]
    fn ls_is_readonly() {
        assert!(is_readonly("ls"));
    }
    #[test]
    fn ls_la_is_readonly() {
        assert!(is_readonly("ls -la"));
    }
    #[test]
    fn cat_is_readonly() {
        assert!(is_readonly("cat file"));
    }
    #[test]
    fn grep_is_readonly() {
        assert!(is_readonly("grep pattern file"));
    }
    #[test]
    fn find_name_is_readonly() {
        assert!(is_readonly("find . -name '*.rs'"));
    }
    #[test]
    fn git_status_is_readonly() {
        assert!(is_readonly("git status"));
    }
    #[test]
    fn git_log_is_readonly() {
        assert!(is_readonly("git log --oneline -5"));
    }
    #[test]
    fn git_diff_is_readonly() {
        assert!(is_readonly("git diff"));
    }

    // === Non-readonly ===

    #[test]
    fn rm_not_readonly() {
        assert!(!is_readonly("rm file"));
    }
    #[test]
    fn mv_not_readonly() {
        assert!(!is_readonly("mv a b"));
    }
    #[test]
    fn mkdir_not_readonly() {
        assert!(!is_readonly("mkdir dir"));
    }
    #[test]
    fn chmod_not_readonly() {
        assert!(!is_readonly("chmod 755 file"));
    }
    #[test]
    fn git_push_not_readonly() {
        assert!(!is_readonly("git push"));
    }
    #[test]
    fn git_commit_not_readonly() {
        assert!(!is_readonly("git commit -m 'test'"));
    }

    // === Flag validation ===

    #[test]
    fn find_delete_not_readonly() {
        assert!(!is_readonly("find . -delete"));
    }
    #[test]
    fn find_exec_not_readonly() {
        assert!(!is_readonly("find . -exec rm {} \\;"));
    }
    #[test]
    fn sed_inplace_not_readonly() {
        assert!(!is_readonly("sed -i 's/old/new/g' file"));
    }
    #[test]
    fn sed_readonly() {
        assert!(is_readonly("sed 's/old/new/g' file"));
    }
    #[test]
    fn sort_output_not_readonly() {
        assert!(!is_readonly("sort -o output.txt input.txt"));
    }

    // === Compound commands ===

    #[test]
    fn compound_readonly() {
        assert!(is_readonly("ls && echo done"));
    }
    #[test]
    fn compound_not_readonly() {
        assert!(!is_readonly("ls && rm file"));
    }
    #[test]
    fn pipeline_readonly() {
        assert!(is_readonly("cat file | grep pattern | head -10"));
    }
    #[test]
    fn command_substitution_not_readonly() {
        assert!(!is_readonly("echo $(whoami)"));
    }

    // === Edge cases ===

    #[test]
    fn empty_is_readonly() {
        assert!(is_readonly(""));
    }
    #[test]
    fn env_var_prefix_readonly() {
        assert!(is_readonly("FOO=bar ls"));
    }

    // === Redirect awareness (C1: output redirects must not bypass approval) ===

    #[test]
    fn stdout_redirect_not_readonly() {
        assert!(!is_readonly("echo hacked > ~/.ssh/authorized_keys"));
    }
    #[test]
    fn append_redirect_not_readonly() {
        assert!(!is_readonly("echo alias x >> ~/.bashrc"));
    }
    #[test]
    fn stderr_redirect_not_readonly() {
        assert!(!is_readonly("ls 2> err.txt"));
    }
    #[test]
    fn both_redirect_to_file_not_readonly() {
        assert!(!is_readonly("ls &> out.txt"));
    }
    #[test]
    fn bare_redirect_not_readonly() {
        assert!(!is_readonly("> file"));
    }
    #[test]
    fn stdin_redirect_still_readonly() {
        assert!(is_readonly("cat < input.txt"));
    }
    #[test]
    fn stream_merge_still_readonly() {
        assert!(is_readonly("cat file 2>&1 | grep pattern"));
    }
    #[test]
    fn devnull_redirect_still_readonly() {
        assert!(is_readonly("ls > /dev/null 2>&1"));
    }
    #[test]
    fn compound_with_redirect_not_readonly() {
        assert!(!is_readonly("ls && echo hacked > file"));
    }
}
