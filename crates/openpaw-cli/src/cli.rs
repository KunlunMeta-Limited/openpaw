//! CLI surface shared by the binary and the help snapshot tests.
//!
//! `Cli` lives in the library so integration tests can render help text via
//! `Cli::command()` without spawning the binary. The binary (`main.rs`)
//! matches on [`Command`] and dispatches to the per-command `run_*`
//! functions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{config_cmd, evolve_cmd, goal_cmd, memory_cmd, model_cmd, session_cmd, tool_cmd};

/// openpaw — an open, evolvable personal AI agent.
#[derive(Debug, Parser)]
#[command(
    name = "openpaw",
    version,
    about = "OpenPaw — an open, evolvable personal AI agent",
    after_help = "Run `openpaw hello` for a zero-config echo conversation, `openpaw chat` for the full agent loop, `openpaw doctor` for environment checks, and `openpaw init` to create the home skeleton."
)]
pub struct Cli {
    /// Subcommand to run; omit it to print help.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Offline provider: `mock`, `echo`, `anthropic`, or `openai`. Global so
    /// `chat` and `--engine-host` share one resolution order.
    #[arg(long, global = true)]
    pub provider: Option<String>,
    /// Workspace the tool host operates in (default: current directory).
    #[arg(long, global = true)]
    pub workspace: Option<PathBuf>,
    /// OpenPaw home for session storage (default: OPENPAW_HOME or
    /// ~/.openpaw).
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    #[cfg(feature = "engine-host")]
    #[arg(long)]
    pub engine_host: bool,
    #[cfg(feature = "engine-host")]
    #[arg(long, default_value = "127.0.0.1")]
    pub engine_host_hostname: String,
    #[cfg(feature = "engine-host")]
    #[arg(long, default_value_t = 0)]
    pub engine_host_port: u16,
    /// Serve the MCP sidecar tools in-process instead of running a
    /// subcommand (feature `sidecar`).
    #[cfg(feature = "sidecar")]
    #[arg(long)]
    pub sidecar: bool,
    /// Sidecar transport: `http` (default; binds 127.0.0.1 on a dynamic
    /// port and prints the URL to stdout) or `stdio` (JSON-RPC lines on
    /// stdin/stdout).
    #[cfg(feature = "sidecar")]
    #[arg(long, default_value = "http")]
    pub sidecar_transport: String,
}

/// All top-level subcommands (the M5 selection).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start an echo-model conversation. No API key, no network — the M1
    /// smoke entry point.
    Hello,
    /// Start the agent loop conversation with tools and session
    /// persistence (offline providers; real providers plug into the same
    /// seam). Provider, workspace and home come from the global flags.
    Chat,
    /// List, show, and search persisted chat sessions.
    Session {
        #[command(subcommand)]
        cmd: session_cmd::Command,
    },
    /// List, search, show, and delete persistent memories.
    Memory {
        #[command(subcommand)]
        cmd: memory_cmd::Command,
    },
    /// Inspect the self-evolution loop: unverified-knowledge ratios, goal
    /// profiles, and resetting unreviewed reflection artifacts.
    Evolve {
        #[command(subcommand)]
        cmd: evolve_cmd::Command,
    },
    /// Run local environment checks and the network-audit report.
    Doctor,
    /// Create and inspect goal profiles; `goal resume` prints the
    /// single-step resume decision without driving an agent loop.
    Goal {
        #[command(subcommand)]
        cmd: goal_cmd::Command,
    },
    /// Show the resolved configuration or forward to the doctor checks.
    Config {
        #[command(subcommand)]
        cmd: config_cmd::Command,
    },
    /// List the built-in providers and their default models.
    Model {
        #[command(subcommand)]
        cmd: model_cmd::Command,
    },
    /// List the tools registered in the tool host.
    Tool {
        #[command(subcommand)]
        cmd: tool_cmd::Command,
    },
    /// Print shell-completion generation status (clap_complete lands
    /// later; nothing is written yet).
    Completions {
        /// Target shell (bash, zsh, fish, powershell, ...).
        shell: String,
    },
    /// Create the OpenPaw home directory skeleton and self-check index.db.
    Init,
    /// Explain the in-process MCP sidecar model (no daemon; informational).
    #[cfg(feature = "sidecar")]
    Sidecar {
        #[command(subcommand)]
        cmd: SidecarCommand,
    },
}

/// Subcommands of `openpaw sidecar` — informational only: the sidecar runs
/// in-process with `--sidecar`, so there is no daemon to query or stop.
#[cfg(feature = "sidecar")]
#[derive(Debug, Subcommand)]
pub enum SidecarCommand {
    /// Explain the in-process sidecar model.
    Status,
    /// Explain how to stop the in-process sidecar.
    Stop,
}
