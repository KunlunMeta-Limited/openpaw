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
    /// 是否启动兼容 OpenCode 的 engine-host（需要 `engine-host` feature）。
    #[cfg(feature = "engine-host")]
    #[arg(long)]
    pub engine_host: bool,
    /// engine-host 的回环监听地址；当前配置校验只允许 `127.0.0.1`。
    #[cfg(feature = "engine-host")]
    #[arg(long, default_value = "127.0.0.1")]
    pub engine_host_hostname: String,
    /// engine-host 的 TCP 端口；`0` 请求操作系统分配空闲端口。
    #[cfg(feature = "engine-host")]
    #[arg(long, default_value_t = 0)]
    pub engine_host_port: u16,
    /// Provider used by `--engine-host`.
    #[cfg(feature = "engine-host")]
    #[arg(long, default_value = "mock")]
    pub provider: String,
    /// `--engine-host` 使用的 OpenPaw home，会保存 session 等运行数据。
    #[cfg(feature = "engine-host")]
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// `--engine-host` 使用的工作区根目录，工具访问不得越界。
    #[cfg(feature = "engine-host")]
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

/// All top-level subcommands (the M5 selection).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start an echo-model conversation. No API key, no network — the M1
    /// smoke entry point.
    Hello,
    /// Start the agent loop conversation with tools and session
    /// persistence (offline providers; real providers plug into the same
    /// seam).
    Chat {
        /// Offline provider: `mock` (counter replies) or `echo`.
        #[arg(long, default_value = "mock")]
        provider: String,
        /// Workspace the tool host operates in (default: current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// OpenPaw home for session storage (default: OPENPAW_HOME or
        /// ~/.openpaw).
        #[arg(long)]
        home: Option<PathBuf>,
    },
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
    Doctor {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<PathBuf>,
        /// Workspace scanned by the network audit (default: current
        /// directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
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
    Init {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<PathBuf>,
    },
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
