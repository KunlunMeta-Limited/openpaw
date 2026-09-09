//! `bash` — run a shell command in the workspace directory.
//!
//! Commands are analyzed by the crate's bash AST safety chain before they
//! enter an [`Environment`]: `Dangerous` is blocked, `RequiresApproval` is
//! refused with an approval hint, `Safe` executes through the environment —
//! [`LocalEnvironment`] (`cmd /C` on Windows, `sh -c` elsewhere) by default,
//! or a caller-provided environment such as `SandboxedEnvironment` via
//! [`run_with_env`]. Output is capped and truncated; the process is killed
//! on timeout.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::Deserialize;

use super::environment::{Environment, LocalEnvironment};
use crate::bashast::{analyze_command_safety, destructive_rm_verdict, CommandSafety};

/// Default timeout for a single command: 120s.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Hard upper bound for a single command: 10 minutes.
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Max characters of combined output before truncation.
const MAX_OUTPUT_CHARS: usize = 30_000;

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "bash".into(),
        description: "Run a shell command in the workspace directory. Prefer specialized tools (read_file, write_file, edit_file, glob, grep) over bash for file operations — they are safer and produce fewer output tokens. Use bash for tasks those tools cannot handle (e.g. running tests, installing packages, git operations). Commands time out after 120s by default; set timeout_ms for longer runs. Do not use for file creation or editing — use write_file or edit_file instead.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute"},
                "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds (default 120000, max 600000)"}
            },
            "required": ["command"]
        }),
        category: Some(ToolCategory::Destructive),
        concurrency: Some(ToolConcurrency::NeverParallel),
        requires_approval: true,
        is_destructive: Some(true),
        ..Default::default()
    }
    .with_description_fn(Arc::new(|input: &serde_json::Value| {
        input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| format!("Running {}", truncate_chars(cmd, 60)))
            .unwrap_or_else(|| "Running".to_string())
    }))
}

pub async fn run(workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let env = LocalEnvironment;
    run_with_env(workspace, args, &env).await
}

/// Executes `args` through the given [`Environment`]. The AST safety chain
/// runs before the command reaches the environment (judge first, then enter).
pub async fn run_with_env(
    workspace: &Path,
    args: serde_json::Value,
    env: &dyn Environment,
) -> Result<ToolCallOutcome> {
    let a: Args = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return Ok(ToolCallOutcome::error(format!("bash: bad args: {e}"))),
    };

    // Bash AST safety chain: block outright, demand approval, or proceed.
    // The per-subcommand predicate is itself AST-driven (semantic rm
    // verdict) — no regex fallback from the upstream guardrail.
    let safety = analyze_command_safety(&a.command, |sub| destructive_rm_verdict(sub).is_some());
    match safety {
        CommandSafety::Safe => {}
        CommandSafety::Dangerous(reason) => {
            return Ok(ToolCallOutcome::error(format!("blocked: {reason}")))
        }
        CommandSafety::RequiresApproval(reason) => {
            return Ok(ToolCallOutcome::error(format!(
                "requires approval: {reason}"
            )))
        }
    }

    execute_in_env(&a.command, workspace, a.timeout_ms, env).await
}

async fn execute_in_env(
    command: &str,
    workspace: &Path,
    timeout_ms: u64,
    env: &dyn Environment,
) -> Result<ToolCallOutcome> {
    let timeout = Duration::from_millis(timeout_ms.min(MAX_TIMEOUT_MS));

    let result = tokio::time::timeout(timeout, env.exec(command, Some(workspace))).await;

    match result {
        Ok(env_result) => {
            let mut out = String::new();
            if !env_result.stdout.is_empty() {
                out.push_str(&env_result.stdout);
            }
            if !env_result.stderr.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[stderr]\n");
                out.push_str(&env_result.stderr);
            }
            if out.is_empty() {
                out.push_str("(no output)");
            }
            out.push_str(&format!("\n[exit code: {}]", env_result.exit_code));

            let content = truncate_output(out);
            if env_result.exit_code != 0 {
                Ok(ToolCallOutcome::error(content))
            } else {
                Ok(ToolCallOutcome::ok(content))
            }
        }
        Err(_) => Ok(ToolCallOutcome::error(format!(
            "bash timeout after {}ms",
            timeout.as_millis()
        ))),
    }
}

/// Truncate a string to at most `max` characters on a char boundary.
fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Cap combined command output, appending a notice when truncated.
fn truncate_output(out: String) -> String {
    if out.chars().count() <= MAX_OUTPUT_CHARS {
        return out;
    }
    let truncated: String = out.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{truncated}\n... (output truncated at {MAX_OUTPUT_CHARS} characters)")
}
