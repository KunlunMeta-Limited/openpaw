//! Semantic analysis — detect dangerous command names and structures.
//!
//! Corresponds to Claude Code's `checkSemantics()`.
//! Checks the first word (command name) against categorized denylists of
//! commands that enable arbitrary code execution or shell escaping.

use super::parser::SimpleCommand;

/// Result of semantic analysis.
#[derive(Debug, Clone)]
pub enum SemanticResult {
    /// Safe — passed semantic checks
    Ok,
    /// Dangerous — known dangerous command detected
    Dangerous(String),
    /// Too complex — cannot statically analyze (e.g. wrapper cannot be stripped),
    /// should fall back to manual approval
    TooComplex(String),
}

/// Builtins that execute their arguments as code — always flagged dangerous.
const EVAL_LIKE_BUILTINS: &[&str] = &[
    "eval",      // executes arbitrary string
    "source",    // executes script file
    ".",         // alias for source
    "exec",      // replaces current process
    "command",   // bypasses alias/function lookup
    "builtin",   // executes shell builtin
    "fc",        // fix command (can execute arbitrary commands)
    "coproc",    // coprocess (can execute arbitrary commands)
    "trap",      // sets signal handlers (can execute arbitrary commands)
    "enable",    // enables/disables builtins
    "mapfile",   // reads into array
    "readarray", // mapfile alias
    "hash",      // modifies command hash table
    "bind",      // modifies readline bindings
    "complete",  // modifies completion
    "compgen",   // generates completion
    "alias",     // defines aliases (can hide commands)
    "let",       // arithmetic evaluation
];

/// Zsh dangerous builtins.
const ZSH_DANGEROUS_BUILTINS: &[&str] = &[
    "zmodload",
    "emulate",
    "sysopen",
    "sysread",
    "syswrite",
    "zpty",
    "ztcp",
    "zsystem",
    "zprof",
    "zparseopts",
];

/// Safe wrapper commands — stripped before checking the inner command.
/// `command` added (same aliasing semantics as `time`/`nohup`).
const SAFE_WRAPPERS: &[&str] = &[
    "time", "nohup", "timeout", "nice", "env", "stdbuf", "command",
];

/// Commands that enable shell escaping — always flagged dangerous.
const SHELL_ESCAPE_COMMANDS: &[&str] = &[
    "eval",   // eval — executes arbitrary string as command
    "source", // source — executes script file
    ".",      // . — alias for source
    "exec",   // exec — replaces current process
    "bash",   // new bash shell
    "sh",     // new sh shell
    "zsh",    // new zsh shell
    "dash",   // new dash shell
    "xargs",  // xargs — argument injection vector
];

/// Check if a string is a valid timeout duration (pure digits or digits+unit).
fn is_valid_duration(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let digit_end = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digit_end == 0 {
        return false;
    }
    if digit_end == bytes.len() {
        return true; // pure digits
    }
    // Remaining must be a unit suffix
    matches!(&s[digit_end..], "s" | "m" | "h" | "d")
}

/// Check if a string is a valid stdbuf SIZE (0, digits, or digits+K/M/G).
fn is_valid_size(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    let bytes = s.as_bytes();
    let digit_end = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digit_end == 0 {
        return false;
    }
    if digit_end == bytes.len() {
        return true;
    }
    matches!(&s[digit_end..], "K" | "M" | "G")
}

/// Strip safe wrapper commands from the front of argv, returning the inner command's argv.
///
/// Uses a fail-closed strategy: only known-safe wrapper argument patterns are handled.
/// Any unknown wrapper argument → return `None` (caller should mark as TooComplex).
///
/// Handled patterns:
/// - `time CMD...` / `nohup CMD...` — strip directly; option flag → None
/// - `timeout DURATION CMD...` — strip timeout + duration (must be valid)
/// - `nice CMD...` — strip directly; option flag → None
/// - `env CMD...` — strip directly; option flag or VAR=value → None
/// - `stdbuf -iN -oN -eN CMD...` — strip stdbuf + known options with SIZE args
pub(crate) fn strip_safe_wrappers(argv: &[String]) -> Option<Vec<String>> {
    let mut result = argv.to_vec();

    loop {
        if result.is_empty() {
            return Some(result);
        }
        let first = result[0].as_str();

        if !SAFE_WRAPPERS.contains(&first) {
            return Some(result);
        }

        match first {
            "time" | "nohup" | "command" => {
                // time/nohup/command with an option flag (e.g. time -p,
                // command -v) → fail-closed
                if result.get(1).map(|s| s.starts_with('-')).unwrap_or(false) {
                    return None;
                }
                result.remove(0);
            }
            "timeout" => {
                result.remove(0);
                // Option flag → fail-closed (can't determine if option takes an argument)
                if result.first().map(|s| s.starts_with('-')).unwrap_or(false) {
                    return None;
                }
                // Skip DURATION argument (must be valid: digits or digits+unit);
                // unrecognized duration or missing arguments → fail-closed.
                match result.first() {
                    Some(dur) if is_valid_duration(dur) => {
                        result.remove(0);
                    }
                    _ => return None,
                }
            }
            "nice" => {
                // Any - option → fail-closed (-n5/-n 5 forms can't be reliably parsed)
                if result.get(1).map(|s| s.starts_with('-')).unwrap_or(false) {
                    return None;
                }
                result.remove(0);
            }
            "env" => {
                // - options or VAR=value → fail-closed (env -u VAR / env VAR=value patterns)
                if result
                    .get(1)
                    .map(|s| s.starts_with('-') || s.contains('='))
                    .unwrap_or(false)
                {
                    return None;
                }
                result.remove(0);
            }
            "stdbuf" => {
                result.remove(0);
                // Only skip known -i/-o/-e options (each followed by a SIZE argument)
                while result
                    .first()
                    .map(|s| s.starts_with("-i") || s.starts_with("-o") || s.starts_with("-e"))
                    .unwrap_or(false)
                {
                    result.remove(0);
                    // SIZE argument (e.g. 0, 1M, 4K) — if next isn't a valid SIZE → fail-closed
                    if result.first().map(|s| is_valid_size(s)).unwrap_or(false) {
                        result.remove(0);
                    } else {
                        return None;
                    }
                }
            }
            _ => return None,
        }
    }
}

/// Detect /proc/*/environ access — may leak process environment variables.
fn check_proc_environ(commands: &[SimpleCommand]) -> Option<String> {
    for cmd in commands {
        for arg in &cmd.argv {
            if arg.contains("/proc/") && arg.contains("environ") {
                return Some(format!("access to /proc/*/environ detected: {arg}"));
            }
        }
    }
    None
}

/// Detect jq system() calls and dangerous flags.
fn check_jq_system(commands: &[SimpleCommand]) -> Option<String> {
    for cmd in commands {
        if cmd.argv.first().map(|s| s == "jq").unwrap_or(false) {
            for arg in &cmd.argv {
                if arg.contains("system(") || arg.contains("system (") {
                    return Some(format!("jq system() call detected: {arg}"));
                }
                // Dangerous flags: -e (exit code), --rawfile (read file into variable)
                if arg == "-e" || arg.starts_with("--rawfile") {
                    return Some(format!("jq dangerous flag: {arg}"));
                }
            }
        }
    }
    None
}

/// Detect interpreters executing inline code (`python -c`, `node -e`, ...).
/// Script-file invocation (`python3 script.py`) stays unaffected.
///
/// Audit remediation: `env python3 -c ...` bypassed approval because `env` was a
/// whitelisted readonly command and the stripped inner command (`python3`)
/// was unknown-but-not-dangerous to the semantic check.
fn check_interpreter_inline_code(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    let is_interpreter = base.starts_with("python")
        || matches!(
            base,
            "node"
                | "nodejs"
                | "deno"
                | "bun"
                | "php"
                | "ruby"
                | "perl"
                | "lua"
                | "luajit"
                | "Rscript"
                | "ir"
        );
    if !is_interpreter {
        return None;
    }
    let inline = argv
        .iter()
        .skip(1)
        .any(|a| a == "-c" || a == "-e" || a == "--eval" || a.starts_with("--eval="));
    if inline {
        Some(format!("interpreter inline code execution: {first}"))
    } else {
        None
    }
}

/// Check if a string is a shell reserved word used as a command name.
fn is_shell_keyword(s: &str) -> bool {
    matches!(
        s,
        "if" | "then"
            | "elif"
            | "else"
            | "fi"
            | "while"
            | "until"
            | "for"
            | "in"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "function"
            | "select"
            | "time"
            | "{"
            | "}"
            | "!"
            | "[["
    )
}

/// Check if a list of parsed commands contains dangerous command names or structures.
///
/// # Returns
/// - `SemanticResult::Ok` — no dangerous commands found
/// - `SemanticResult::Dangerous(reason)` — found dangerous command or structure
/// - `SemanticResult::TooComplex(reason)` — wrapper cannot be stripped, needs approval
pub fn check_semantics(commands: &[SimpleCommand]) -> SemanticResult {
    for cmd in commands {
        // Strip safe wrappers from argv (None → TooComplex)
        let stripped = match strip_safe_wrappers(&cmd.argv) {
            Some(argv) => argv,
            None => {
                return SemanticResult::TooComplex(format!(
                    "failed to strip wrappers: {}",
                    cmd.argv.join(" ")
                ));
            }
        };

        if let Some(first) = stripped.first() {
            let lower = first.to_lowercase();

            // Eval-like builtins check
            if EVAL_LIKE_BUILTINS.contains(&lower.as_str()) {
                return SemanticResult::Dangerous(format!(
                    "dangerous command (eval-like): {first}"
                ));
            }

            // Zsh dangerous builtins check
            if ZSH_DANGEROUS_BUILTINS.contains(&lower.as_str()) {
                return SemanticResult::Dangerous(format!("dangerous zsh builtin: {first}"));
            }

            // Shell escape commands check
            if SHELL_ESCAPE_COMMANDS.contains(&lower.as_str()) {
                return SemanticResult::Dangerous(format!(
                    "dangerous command (shell escape): {first}"
                ));
            }

            // Shell reserved word as command name — parse anomaly
            if is_shell_keyword(&lower) {
                return SemanticResult::Dangerous(format!("shell keyword as command: {first}"));
            }

            // Interpreter inline-code check: `env python3 -c '...'`
            // executes arbitrary code; script-file invocation
            // (`python3 script.py`) stays unaffected.
            if let Some(reason) = check_interpreter_inline_code(&stripped) {
                return SemanticResult::Dangerous(reason);
            }
        }
    }

    // /proc/*/environ detection
    if let Some(reason) = check_proc_environ(commands) {
        return SemanticResult::Dangerous(reason);
    }

    // jq system() detection
    if let Some(reason) = check_jq_system(commands) {
        return SemanticResult::Dangerous(reason);
    }

    SemanticResult::Ok
}

#[cfg(test)]
mod tests {
    use super::super::parser::{parse_for_security, ParseResult};
    use super::*;

    fn get_commands(cmd: &str) -> Vec<SimpleCommand> {
        match parse_for_security(cmd) {
            ParseResult::Simple { commands } => commands,
            _ => vec![],
        }
    }

    #[test]
    fn test_eval_dangerous() {
        let cmds = get_commands("eval 'rm -rf /'");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(r) if r.contains("eval")));
    }

    #[test]
    fn test_source_dangerous() {
        let cmds = get_commands("source /tmp/evil.sh");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_dot_dangerous() {
        let cmds = get_commands(". /tmp/evil.sh");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_exec_dangerous() {
        let cmds = get_commands("exec bash");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_bash_dangerous() {
        let cmds = get_commands("bash -c 'rm -rf /'");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_xargs_dangerous() {
        let cmds = get_commands("echo /home | xargs rm -rf");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_safe_command() {
        let cmds = get_commands("ls -la");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
    }

    #[test]
    fn test_safe_git_status() {
        let cmds = get_commands("git status");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
    }

    #[test]
    fn test_safe_compound() {
        let cmds = get_commands("ls && git status");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
    }

    #[test]
    fn test_dangerous_in_compound() {
        let cmds = get_commands("ls && eval 'rm -rf /'");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    // === New tests for extended semantic checks ===

    #[test]
    fn test_command_builtin() {
        // Post-fix semantics: `command` is a safe wrapper
        // (alias/function bypass does not change the danger verdict of the
        // inner command) — judged by the stripped inner command.
        // Safe inner → Ok (no longer blanket Dangerous).
        let cmds = get_commands("command ls");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
        // Option flag fail-closed → TooComplex (needs manual approval).
        let cmds = get_commands("command -v ls");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::TooComplex(_)));
        // A wrapper must not hide a dangerous inner command.
        let cmds = get_commands("command eval 'rm -rf /'");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_trap() {
        let cmds = get_commands("trap 'echo hi' INT");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_wrapper_time() {
        let cmds = vec![SimpleCommand {
            text: "time ls".into(),
            argv: vec!["time".into(), "ls".into()],
            env_vars: vec![],
            redirects: vec![],
        }];
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
    }

    #[test]
    fn test_wrapper_timeout() {
        let cmds = vec![SimpleCommand {
            text: "timeout 10 ls".into(),
            argv: vec!["timeout".into(), "10".into(), "ls".into()],
            env_vars: vec![],
            redirects: vec![],
        }];
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Ok));
    }

    #[test]
    fn test_wrapper_unknown_flag() {
        let cmds = vec![SimpleCommand {
            text: "time -p ls".into(),
            argv: vec!["time".into(), "-p".into(), "ls".into()],
            env_vars: vec![],
            redirects: vec![],
        }];
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::TooComplex(_)));
    }

    #[test]
    fn test_proc_environ() {
        let cmds = get_commands("cat /proc/1/environ");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(r) if r.contains("/proc")));
    }

    #[test]
    fn test_jq_system() {
        let cmds = get_commands("jq 'system(\"id\")'");
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(_)));
    }

    #[test]
    fn test_shell_keyword_as_cmd() {
        let cmds = vec![SimpleCommand {
            text: "if".into(),
            argv: vec!["if".into()],
            env_vars: vec![],
            redirects: vec![],
        }];
        let result = check_semantics(&cmds);
        assert!(matches!(result, SemanticResult::Dangerous(r) if r.contains("shell keyword")));
    }
}
