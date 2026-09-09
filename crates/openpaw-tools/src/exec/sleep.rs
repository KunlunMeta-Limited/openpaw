//! `sleep` — pause agent execution for a bounded duration.
//!
//! Useful for polling scenarios (CI status, service readiness). The duration
//! is capped at [`MAX_DURATION_MS`] to prevent runaway waits; values above
//! the cap are silently truncated.

use std::path::Path;
use std::time::Duration;

use openpaw_core::{Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};

/// Hard upper bound on a single sleep: 5 minutes. Longer waits should be
/// split into multiple polls so the agent stays responsive to interrupts.
pub const MAX_DURATION_MS: u64 = 300_000;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "sleep".into(),
        description: "Sleep for a specified duration (capped at 5 minutes). \
            Useful for polling scenarios such as waiting for CI or service readiness. \
            The agent is inactive during the sleep and should not be used to delay \
            user-facing responses."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "duration_ms": {
                    "type": "integer",
                    "description": "Duration in milliseconds (max 300000 = 5 minutes)",
                    "minimum": 0,
                    "maximum": 300000
                }
            },
            "required": ["duration_ms"]
        }),
        category: Some(ToolCategory::ReadOnly),
        concurrency: Some(ToolConcurrency::ParallelSafe),
        is_destructive: Some(false),
        ..Default::default()
    }
}

pub async fn run(_workspace: &Path, args: serde_json::Value) -> Result<ToolCallOutcome> {
    let requested = args
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let capped = requested.min(MAX_DURATION_MS);

    if capped > 0 {
        tokio::time::sleep(Duration::from_millis(capped)).await;
    }

    let content = if capped == requested {
        format!("slept {capped}ms")
    } else {
        format!("slept {capped}ms (requested {requested}ms, capped at {MAX_DURATION_MS}ms)")
    };

    Ok(ToolCallOutcome::ok(content))
}
