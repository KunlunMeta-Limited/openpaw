//! `openpaw config ...` — show the resolved configuration, or forward to
//! the doctor checks.
//!
//! Keys are only ever reported as set/not set; their values never reach
//! the output (masked).

use std::path::Path;

use crate::doctor;

/// Subcommands of `openpaw config`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the resolved OpenPaw home and provider key presence (masked).
    Show,
    /// Alias of `openpaw doctor` (same checks, same report).
    Doctor,
}

/// `openpaw config show` — resolved home + provider key presence.
pub fn run_show(home: &Path) -> i32 {
    let state = if home.is_dir() { "present" } else { "missing" };
    println!("openpaw home: {} ({state})", home.display());
    println!("ANTHROPIC_API_KEY: {}", key_state("ANTHROPIC_API_KEY"));
    println!("OPENAI_API_KEY: {}", key_state("OPENAI_API_KEY"));
    println!("default provider: mock (offline)");
    0
}

/// Masked presence state of one environment key.
fn key_state(name: &str) -> &'static str {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => "set (value hidden)",
        _ => "not set",
    }
}

/// `openpaw config doctor` — straight alias of `openpaw doctor`.
pub fn run_doctor(home: &Path, workspace: &Path) -> i32 {
    doctor::run(home, workspace)
}
