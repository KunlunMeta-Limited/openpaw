//! Cross-platform shell type detection.
//!
//! On Windows, commands may be executed via `cmd.exe` or PowerShell,
//! which tree-sitter-bash cannot parse. This module detects the shell
//! type so the parser can gracefully return `ParseUnavailable` for
//! non-bash syntax.

/// Detected shell type for a command string.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellType {
    /// Bash/sh syntax — can be parsed by tree-sitter-bash
    Bash,
    /// cmd.exe syntax (Windows)
    Cmd,
    /// PowerShell syntax
    PowerShell,
    /// Cannot determine shell type
    Unknown,
}

/// Detect the shell type of a command string.
///
/// # Detection Rules
/// - `%VAR%` syntax, `>nul` redirect, cmd-specific commands → `Cmd`
/// - `$env:`, `Verb-Noun` patterns, `@()` → `PowerShell`
/// - `&&`, `||`, `2>`, `/dev/null`, `export`, `${}` → `Bash`
/// - On Windows with no bash features → `Cmd` (Windows default)
/// - On Unix with no cmd/powershell features → `Bash` (Unix default)
pub fn detect_shell_type(command: &str) -> ShellType {
    // Check for explicit cmd features first
    if has_cmd_features(command) {
        return ShellType::Cmd;
    }
    // Check for explicit PowerShell features
    if has_powershell_features(command) {
        return ShellType::PowerShell;
    }
    // If no cmd/powershell features detected, attempt Bash parsing.
    // This is safe because:
    // 1. Many Windows dev tools (git, cargo) use bash syntax
    // 2. If parse fails, we get ParseUnavailable (fail-safe)
    // 3. We only return Cmd for explicit cmd syntax
    ShellType::Bash
}

fn has_cmd_features(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    // %VAR% syntax — two % signs with content between
    if cmd.contains('%') {
        let bytes = cmd.as_bytes();
        let mut first_pct = None;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'%' {
                if first_pct.is_some() {
                    // Found closing %
                    return true;
                }
                first_pct = Some(i);
            }
        }
    }
    // >nul redirect (cmd-specific)
    if lower.contains(">nul") || lower.contains("2>nul") {
        return true;
    }
    // cmd-specific commands
    let first_word = cmd.split_whitespace().next().unwrap_or("").to_lowercase();
    matches!(
        first_word.as_str(),
        "dir" | "set" | "cls" | "ver" | "vol" | "type" | "copy" | "move" | "del"
    )
}

fn has_powershell_features(cmd: &str) -> bool {
    // Case-insensitivity — PowerShell is case-insensitive end to end
    // (`$ENV:PATH`, `get-childitem` previously fell through to Bash analysis
    // while pwsh executed them, i.e. bash semantics on PowerShell commands).
    let lower = cmd.to_lowercase();
    // Launcher / pure-PS alias first words — `powershell -ec <b64>` and
    // bare `iex '...'` previously detected as Bash, so the PS -EncodedCommand
    // / alias checks never ran (analysis/execution shell mismatch).
    let first_word = lower.split_whitespace().next().unwrap_or("");
    if matches!(
        first_word,
        "powershell" | "pwsh" | "powershell_ise" | "iex" | "iwr" | "sal"
    ) {
        return true;
    }
    // $env: syntax
    if lower.contains("$env:") {
        return true;
    }
    // PowerShell cmdlet pattern: Verb-Noun
    const PATTERNS: &[&str] = &[
        "write-",
        "get-",
        "invoke-",
        "set-",
        "new-",
        "remove-",
        "select-",
        "where-",
        "foreach-",
        "out-",
        "convertto-",
        "start-",
        "stop-",
        "test-",
        "add-",
        "import-",
        "export-",
    ];
    for p in PATTERNS {
        if lower.contains(p) {
            return true;
        }
    }
    // PowerShell array subexpression
    if lower.contains("@(") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_bash_simple() {
        assert_eq!(detect_shell_type("ls -la"), ShellType::Bash);
    }

    #[test]
    fn detect_bash_compound() {
        assert_eq!(detect_shell_type("ls && echo done"), ShellType::Bash);
    }

    #[test]
    fn detect_cmd_dir() {
        assert_eq!(detect_shell_type("dir"), ShellType::Cmd);
    }

    #[test]
    fn detect_cmd_var() {
        assert_eq!(detect_shell_type("echo %PATH%"), ShellType::Cmd);
    }

    #[test]
    fn detect_cmd_nul() {
        assert_eq!(detect_shell_type("echo hello >nul"), ShellType::Cmd);
    }

    #[test]
    fn detect_powershell_env() {
        assert_eq!(detect_shell_type("echo $env:PATH"), ShellType::PowerShell);
    }

    #[test]
    fn detect_powershell_cmdlet() {
        assert_eq!(
            detect_shell_type("Get-ChildItem -Path C:\\"),
            ShellType::PowerShell
        );
    }

    #[test]
    fn detect_powershell_array() {
        assert_eq!(detect_shell_type("@(1,2,3)"), ShellType::PowerShell);
    }

    #[test]
    fn detect_bash_stderr_redirect() {
        assert_eq!(detect_shell_type("ls 2>/dev/null"), ShellType::Bash);
    }

    #[test]
    fn detect_bash_command_substitution() {
        assert_eq!(detect_shell_type("echo $(whoami)"), ShellType::Bash);
    }

    // === case-insensitive detection + launcher first words ===

    #[test]
    fn a05_lowercase_cmdlet_detected_as_powershell() {
        assert_eq!(
            detect_shell_type("get-childitem -path C:\\"),
            ShellType::PowerShell
        );
        assert_eq!(
            detect_shell_type("remove-item -recurse -force C:\\x"),
            ShellType::PowerShell
        );
    }

    #[test]
    fn a05_uppercase_env_detected_as_powershell() {
        assert_eq!(detect_shell_type("echo $ENV:PATH"), ShellType::PowerShell);
    }

    #[test]
    fn a05_powershell_launcher_first_word() {
        assert_eq!(
            detect_shell_type("powershell -ec SGVsbG8="),
            ShellType::PowerShell
        );
        assert_eq!(
            detect_shell_type("PWSH -NoProfile -Command ls"),
            ShellType::PowerShell
        );
    }

    #[test]
    fn a05_pure_ps_alias_first_word() {
        assert_eq!(detect_shell_type("IEX 'evil'"), ShellType::PowerShell);
        assert_eq!(detect_shell_type("iwr http://x"), ShellType::PowerShell);
    }
}
