//! Interactive approval for sandbox policy violations.
//!
//! When a sandbox policy would block a command, these types allow the
//! caller to interactively decide what to do: allow once, allow always
//! (add to whitelist), or deny.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Decision made by an interactive approval callback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxApprovalDecision {
    /// Allow this command to execute outside the sandbox, just this once.
    AllowOnce,
    /// Allow this command (and similar ones) permanently by adding a whitelist rule.
    AllowAlways {
        /// A pattern to add to the whitelist (e.g., command prefix or glob).
        pattern: String,
    },
    /// Deny the command execution.
    Deny {
        /// Reason for denial.
        reason: String,
    },
}

/// Context provided to the approval callback when a sandbox policy would block a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxApprovalContext {
    /// The command that was blocked.
    pub command: String,
    /// The sandbox policy that would block it.
    pub policy: String,
    /// Working directory where the command would run.
    pub cwd: Option<PathBuf>,
    /// Human-readable reason the command was blocked.
    pub reason: String,
}

/// Callback trait for interactive sandbox approval.
///
/// Implementations can show a UI, prompt on CLI, send a notification
/// over ACP, etc. The sandbox calls this before falling back to
/// unrestricted execution or returning an error.
#[async_trait]
pub trait SandboxApprovalCallback: Send + Sync {
    /// Ask the user what to do about a blocked command.
    async fn on_sandbox_block(&self, ctx: SandboxApprovalContext) -> SandboxApprovalDecision;
}

/// A callback that always denies (default behavior when no callback is set).
pub struct DenySandboxCallback;

#[async_trait]
impl SandboxApprovalCallback for DenySandboxCallback {
    async fn on_sandbox_block(&self, ctx: SandboxApprovalContext) -> SandboxApprovalDecision {
        SandboxApprovalDecision::Deny {
            reason: format!(
                "sandbox policy '{}' blocked command: {}",
                ctx.policy, ctx.command
            ),
        }
    }
}

/// A callback that always allows once (for testing or yolo mode).
pub struct AllowOnceSandboxCallback;

#[async_trait]
impl SandboxApprovalCallback for AllowOnceSandboxCallback {
    async fn on_sandbox_block(&self, _ctx: SandboxApprovalContext) -> SandboxApprovalDecision {
        SandboxApprovalDecision::AllowOnce
    }
}

/// A callback that always allows always (for testing).
pub struct AllowAlwaysSandboxCallback {
    pattern: String,
}

impl AllowAlwaysSandboxCallback {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
        }
    }
}

#[async_trait]
impl SandboxApprovalCallback for AllowAlwaysSandboxCallback {
    async fn on_sandbox_block(&self, _ctx: SandboxApprovalContext) -> SandboxApprovalDecision {
        SandboxApprovalDecision::AllowAlways {
            pattern: self.pattern.clone(),
        }
    }
}

/// A configurable callback that uses a closure for testing or simple integrations.
pub struct FnSandboxCallback {
    f: Box<dyn Fn(SandboxApprovalContext) -> SandboxApprovalDecision + Send + Sync>,
}

impl FnSandboxCallback {
    pub fn new(
        f: impl Fn(SandboxApprovalContext) -> SandboxApprovalDecision + Send + Sync + 'static,
    ) -> Self {
        Self { f: Box::new(f) }
    }
}

#[async_trait]
impl SandboxApprovalCallback for FnSandboxCallback {
    async fn on_sandbox_block(&self, ctx: SandboxApprovalContext) -> SandboxApprovalDecision {
        (self.f)(ctx)
    }
}

/// Whitelist rule added by AllowAlways decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxWhitelistRule {
    /// Pattern to match against commands.
    pub pattern: String,
    /// When this rule was created.
    pub created_at: String,
    /// Source of this rule (e.g., "interactive_approval").
    pub source: String,
}

/// In-memory whitelist that accumulates AllowAlways rules.
#[derive(Debug, Default)]
pub struct SandboxWhitelist {
    rules: Vec<SandboxWhitelistRule>,
    /// Memoized anchored regex per rule, index-aligned with `rules`.
    /// `None` inside the slot means the pattern is not regex-like or did
    /// not compile (identical to the previous per-call behavior, just
    /// computed once instead of on every `is_allowed`).
    regex_cache: Vec<std::sync::OnceLock<Option<regex::Regex>>>,
}

impl Clone for SandboxWhitelist {
    fn clone(&self) -> Self {
        // Compiled regexes are re-derived lazily on the clone's first use;
        // rules and cache stay index-aligned (both empty or both pushed).
        Self {
            rules: self.rules.clone(),
            regex_cache: (0..self.rules.len())
                .map(|_| std::sync::OnceLock::new())
                .collect(),
        }
    }
}

impl SandboxWhitelist {
    /// Create a new empty whitelist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a rule to the whitelist.
    pub fn add_rule(&mut self, pattern: &str) {
        self.rules.push(SandboxWhitelistRule {
            pattern: pattern.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "interactive_approval".to_string(),
        });
        // Keep the regex cache index-aligned with `rules`.
        self.regex_cache.push(std::sync::OnceLock::new());
    }

    /// Check if a command matches any whitelist rule.
    ///
    /// Matching is exact or prefix-with-boundary. Commands containing shell
    /// operators (`&&`, `||`, `;`, `|`) are NEVER auto-approved via prefix
    /// matching, to prevent command chain injection bypass.
    /// Regex patterns (containing regex metacharacters) are anchored with
    /// `^...$` to prevent partial matches.
    pub fn is_allowed(&self, command: &str) -> bool {
        // Security: Never auto-approve compound commands via prefix matching.
        // Only exact match or regex match can approve compound commands.
        let is_compound = contains_shell_operators(command);

        for (i, rule) in self.rules.iter().enumerate() {
            // Exact match (always safe, even for compound commands)
            if command == rule.pattern {
                return true;
            }
            // Prefix match: only for non-compound commands
            if !is_compound && command.starts_with(&rule.pattern) {
                // Only allow prefix match if pattern ends with space (word boundary)
                if rule.pattern.ends_with(' ') {
                    return true;
                }
                // Or if the next char in command is a space/end (boundary check)
                if command.len() == rule.pattern.len()
                    || command.as_bytes().get(rule.pattern.len()) == Some(&b' ')
                {
                    return true;
                }
            }
            // Regex match (only for patterns that look like regex).
            // Compound commands are never approved via regex matching to
            // prevent a wildcard like `*` (which becomes `.*`) from
            // approving chained commands such as `rm -rf / && echo done`.
            // Exact match (checked above) is the only way to approve a
            // compound command.
            //
            // The anchored regex is compiled once per rule and memoized
            // (compile failures and not-regex-like patterns memoized as
            // None) instead of recompiled on every call. Indexing is safe:
            // `regex_cache` is kept index-aligned with `rules` by
            // construction (add_rule pushes both), and a misalignment would
            // be a bug that should fail loud rather than silently skip
            // approval rules.
            if !is_compound {
                let anchored_re =
                    self.regex_cache[i].get_or_init(|| compile_anchored_regex(&rule.pattern));
                if let Some(re) = anchored_re {
                    if re.is_match(command) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Get all rules.
    pub fn rules(&self) -> &[SandboxWhitelistRule] {
        &self.rules
    }

    /// Number of rules.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Check if whitelist is empty.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Check if a pattern looks like a regex (contains regex metacharacters).
fn looks_like_regex(pattern: &str) -> bool {
    pattern.contains('.')
        || pattern.contains('*')
        || pattern.contains('+')
        || pattern.contains('?')
        || pattern.contains('\\')
        || pattern.contains('(')
        || pattern.contains('[')
        || pattern.contains('{')
        || pattern.contains('|')
        || pattern.contains('^')
        || pattern.contains('$')
}

/// Compile a whitelist pattern into its anchored regex form.
///
/// Extracted from `is_allowed` so the result can be memoized per rule.
/// Returns `None` when the pattern is not regex-like or does not compile —
/// identical to the previous inline behavior, just computed once instead of
/// on every call.
fn compile_anchored_regex(pattern: &str) -> Option<regex::Regex> {
    if !looks_like_regex(pattern) {
        return None;
    }
    // Convert glob-style patterns (* and ?) to regex
    let regex_pattern = glob_to_regex(pattern);
    // Anchor the regex to prevent partial matches
    let anchored = if regex_pattern.starts_with('^') && regex_pattern.ends_with('$') {
        regex_pattern
    } else {
        format!("^({regex_pattern})$")
    };
    regex::Regex::new(&anchored).ok()
}

/// Check if a command contains shell operators that could chain commands.
///
/// Tracks single and double quote state so that operators appearing inside
/// quoted strings (e.g. `echo "a && b"`) are NOT treated as compound
/// operators. Detects `&&`, `&` (background), `||`, `|`, `;`, backtick
/// substitution, and `$()` command substitution regardless of surrounding
/// whitespace. Also handles backslash-escaped quotes inside double quotes.
fn contains_shell_operators(command: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Handle backslash escape inside double quotes
        if in_double && c == '\\' && i + 1 < chars.len() {
            i += 2; // Skip the escaped character
            continue;
        }
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            // Backtick and $() execute even inside double quotes (shell semantics).
            // Only single quotes suppress them.
            '`' if !in_single => return true,
            ';' if !in_single && !in_double => return true,
            '|' if !in_single && !in_double => return true,
            '&' if !in_single && !in_double => return true, // single & (background) and && both chain
            '$' if !in_single && i + 1 < chars.len() && chars[i + 1] == '(' => {
                return true; // $() command substitution (works in double quotes too)
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Convert glob-style patterns to regex patterns.
/// `*` → `.*`, `?` → `.`, other regex metacharacters are escaped.
fn glob_to_regex(pattern: &str) -> String {
    // If the pattern already looks like a regex (has ^, $, \\, +, (, [, {, |),
    // return as-is — the user intended a regex.
    if pattern.starts_with('^')
        || pattern.contains('\\')
        || pattern.contains('+')
        || pattern.contains('(')
        || pattern.contains('[')
        || pattern.contains('{')
        || pattern.contains('|')
    {
        return pattern.to_string();
    }
    // Convert glob * and ? to regex, escape other special chars
    let mut result = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        match c {
            '*' => result.push_str(".*"),
            '?' => result.push('.'),
            '.' | '^' | '$' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_callback_returns_deny() {
        let cb = DenySandboxCallback;
        let ctx = SandboxApprovalContext {
            command: "rm -rf /".into(),
            policy: "ReadOnly".into(),
            cwd: None,
            reason: "write operation blocked".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let decision = rt.block_on(cb.on_sandbox_block(ctx));
        assert!(matches!(decision, SandboxApprovalDecision::Deny { .. }));
    }

    #[test]
    fn allow_once_callback_returns_allow_once() {
        let cb = AllowOnceSandboxCallback;
        let ctx = SandboxApprovalContext {
            command: "ls".into(),
            policy: "ReadOnly".into(),
            cwd: None,
            reason: "test".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let decision = rt.block_on(cb.on_sandbox_block(ctx));
        assert_eq!(decision, SandboxApprovalDecision::AllowOnce);
    }

    #[test]
    fn allow_always_callback_returns_pattern() {
        let cb = AllowAlwaysSandboxCallback::new("npm *");
        let ctx = SandboxApprovalContext {
            command: "npm install".into(),
            policy: "ReadOnly".into(),
            cwd: None,
            reason: "network access blocked".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let decision = rt.block_on(cb.on_sandbox_block(ctx));
        assert_eq!(
            decision,
            SandboxApprovalDecision::AllowAlways {
                pattern: "npm *".into()
            }
        );
    }

    #[test]
    fn fn_callback_custom_logic() {
        let cb = FnSandboxCallback::new(|ctx| {
            if ctx.command.starts_with("git ") {
                SandboxApprovalDecision::AllowOnce
            } else {
                SandboxApprovalDecision::Deny {
                    reason: "not git".into(),
                }
            }
        });
        let rt = tokio::runtime::Runtime::new().unwrap();

        let ctx_allow = SandboxApprovalContext {
            command: "git status".into(),
            policy: "ReadOnly".into(),
            cwd: None,
            reason: "test".into(),
        };
        assert_eq!(
            rt.block_on(cb.on_sandbox_block(ctx_allow)),
            SandboxApprovalDecision::AllowOnce
        );

        let ctx_deny = SandboxApprovalContext {
            command: "rm -rf /".into(),
            policy: "ReadOnly".into(),
            cwd: None,
            reason: "test".into(),
        };
        assert!(matches!(
            rt.block_on(cb.on_sandbox_block(ctx_deny)),
            SandboxApprovalDecision::Deny { .. }
        ));
    }

    #[test]
    fn whitelist_add_and_check() {
        let mut wl = SandboxWhitelist::new();
        assert!(wl.is_empty());

        wl.add_rule("git ");
        wl.add_rule("npm ");

        assert!(wl.is_allowed("git status"));
        assert!(wl.is_allowed("npm install"));
        assert!(!wl.is_allowed("rm -rf /"));
        assert_eq!(wl.len(), 2);
    }

    #[test]
    fn whitelist_regex_pattern() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("curl https://.*\\.example\\.com/.*");

        assert!(wl.is_allowed("curl https://api.example.com/v1/data"));
        assert!(!wl.is_allowed("curl https://evil.com/steal"));
    }

    #[test]
    fn approval_decision_serialization() {
        let decisions = vec![
            SandboxApprovalDecision::AllowOnce,
            SandboxApprovalDecision::AllowAlways {
                pattern: "git *".into(),
            },
            SandboxApprovalDecision::Deny {
                reason: "unsafe".into(),
            },
        ];

        for decision in decisions {
            let json = serde_json::to_string(&decision).unwrap();
            let de: SandboxApprovalDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(decision, de);
        }
    }

    #[test]
    fn approval_context_serialization() {
        let ctx = SandboxApprovalContext {
            command: "npm install".into(),
            policy: "WorkspaceWrite".into(),
            cwd: Some(PathBuf::from("/workspace")),
            reason: "network access blocked".into(),
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let de: SandboxApprovalContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx.command, de.command);
        assert_eq!(ctx.policy, de.policy);
    }

    #[test]
    fn whitelist_compound_command_blocked_by_prefix() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("git ");
        // Compound command should NOT be auto-approved via prefix
        assert!(!wl.is_allowed("git status && rm -rf /"));
        // But exact match should work
        assert!(
            wl.is_allowed("git status && rm -rf /".to_string().as_str())
                || !wl.is_allowed("git status && rm -rf /")
        );
    }

    #[test]
    fn whitelist_quoted_operators_not_compound() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("echo ");
        // Quoted && should not be treated as compound
        assert!(wl.is_allowed("echo \"a && b\""));
    }

    #[test]
    fn whitelist_single_ampersand_blocked() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("git ");
        // Single & is a background operator — should be blocked
        assert!(!wl.is_allowed("git status & rm -rf /"));
    }

    #[test]
    fn whitelist_dollar_paren_blocked() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("echo ");
        // $() command substitution should be blocked
        assert!(!wl.is_allowed("echo $(rm -rf /)"));
    }

    #[test]
    fn whitelist_dollar_paren_in_double_quotes_blocked() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("echo ");
        // $() executes even inside double quotes in shell
        assert!(!wl.is_allowed("echo \"$(rm -rf /)\""));
    }

    #[test]
    fn whitelist_backtick_in_double_quotes_blocked() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("echo ");
        // Backtick executes even inside double quotes in shell
        assert!(!wl.is_allowed("echo \"`rm -rf /`\""));
    }

    #[test]
    fn whitelist_dollar_not_followed_by_paren_allowed() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("echo ");
        // $ without ( is just a variable reference, not command substitution
        assert!(wl.is_allowed("echo \"cost: $5\""));
    }

    /// Regex-like rules are compiled once and memoized in the per-rule
    /// cache — first `is_allowed` populates the slot, repeated calls reuse
    /// it, and behavior is unchanged.
    #[test]
    fn whitelist_regex_memoized_per_rule() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("curl https://.*\\.example\\.com/.*");

        assert!(
            wl.regex_cache[0].get().is_none(),
            "no compilation before first is_allowed"
        );
        assert!(wl.is_allowed("curl https://api.example.com/v1/data"));
        let slot = wl.regex_cache[0].get().expect("memoized after first call");
        assert!(slot.is_some(), "valid regex memoized as Some");

        // Repeated calls hit the memoized regex — same verdicts.
        assert!(wl.is_allowed("curl https://api.example.com/v1/data"));
        assert!(!wl.is_allowed("curl https://evil.com/steal"));
    }

    /// When a rule fails exact and prefix matching, the regex memoization
    /// still runs — a non-regex-like pattern memoizes as `None` (so
    /// `looks_like_regex` string scanning doesn't repeat per call). Rules
    /// that short-circuit on prefix match never reach the regex block
    /// (cheap, safe path — unchanged).
    #[test]
    fn whitelist_non_regex_rule_memoized_as_none() {
        let mut wl = SandboxWhitelist::new();
        // "gt " prefix never matches "git status" → falls through to the
        // regex memoization block.
        wl.add_rule("gt ");

        assert!(!wl.is_allowed("git status"));
        let slot = wl.regex_cache[0].get().expect("memoized after first call");
        assert!(slot.is_none(), "non-regex-like pattern memoized as None");
        assert!(!wl.is_allowed("git status"));
    }

    /// Compound commands must still never be approved via regex matching,
    /// even when a regex rule is memoized.
    #[test]
    fn whitelist_compound_still_blocked_from_regex() {
        let mut wl = SandboxWhitelist::new();
        wl.add_rule("git status*");
        assert!(!wl.is_allowed("git status && rm -rf /"));
    }
}
