//! CLI surface shared by the binary and the help snapshot tests.
//!
//! `Cli` lives in the library so integration tests can render help text via
//! `Cli::command()` without spawning the binary. The binary (`main.rs`)
//! matches on [`Command`] and dispatches to the per-command `run_*`
//! functions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{config_cmd, evolve_cmd, goal_cmd, memory_cmd, model_cmd, session_cmd, tool_cmd};

/// 用途：OpenPaw命令行入口及全局配置选项。
/// 字段：子命令、可选.env和feature控制的sidecar参数。
/// 来源去向：本机参数到原CLI及Host装配。
/// 约束：显式文件只作用于模型配置相关入口，不修改进程环境。
/// 稳定性：新增可选参数，既有子命令保留。
/// 关联：需求/设计REQ-OP-002、REQ-OP-003；DES-OP-002。
#[derive(Debug, Parser)]
#[command(
    name = "openpaw",
    version,
    about = "OpenPaw — an open, evolvable personal AI agent",
    after_help = "Run `openpaw hello` for a zero-config echo conversation, `openpaw chat` for the full agent loop (local tools are automatically approved; Host policy applies only to engine-host), `openpaw doctor` for environment checks, and `openpaw init` to create the home skeleton."
)]
pub struct Cli {
    /// 用途：显式选择模型.env，覆盖OpenPaw数据目录默认文件。
    /// 字段：可选本机路径，支持中文、空格和Windows原生路径。
    /// 来源去向：命令行到共用ModelEnvironment。
    /// 约束：缺失或错误返回非零；不修改进程环境。
    /// 稳定性：新增全局可选参数；不影响无关子命令。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    #[arg(long, global = true)]
    pub env_file: Option<PathBuf>,
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
}

/// 用途：顶层CLI命令
/// 字段：各子命令及参数
/// 来源去向：命令行到各模块
/// 约束：聊天显式模型优先、端点不得含密钥
/// 稳定性：只新增可选参数
/// 关联：需求/设计 REQ-OP-002、REQ-OP-003。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// 用途：离线预检或显式确认团队工作区迁移。
    /// 字段：plan/apply子命令，只有apply允许写入。
    /// 来源去向：可信本机命令行到共享Host迁移协调器。
    /// 约束：不启动HTTP、不装配模型，不能与sidecar同时运行。
    /// 稳定性：engine-host可选功能，旧命令不变。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013。
    #[cfg(feature = "engine-host")]
    EngineHostWorkspace {
        /// 用途：选择只读计划或确认应用。
        /// 字段：本机迁移子命令及显式配置参数。
        /// 来源去向：命令行到原Host迁移入口。
        /// 约束：apply必须携带完整计划摘要和全新备份位置。
        /// 稳定性：新增可选命令。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[command(subcommand)]
        command: WorkspaceMigrationCommand,
    },
    /// 用途：启动可选共享Host服务。
    /// 字段：显式配置文件和可选本机端口。
    /// 来源去向：命令行到Host配置和启动。
    /// 约束：仅loopback，不能与sidecar同时启动。
    /// 稳定性：新增可选feature命令，默认CLI不变。
    /// 关联：需求/设计REQ-OP-002；DES-OP-006。
    #[cfg(feature = "engine-host")]
    EngineHost {
        /// 用途：加载Host配置。
        /// 字段：本机TOML文件路径。
        /// 来源去向：命令行到HostConfig。
        /// 约束：配置明确授权home、根和工具。
        /// 稳定性：新增。
        /// 关联：REQ-OP-008；DES-OP-006。
        #[arg(long)]
        config: PathBuf,
        /// 用途：选择监听端口。
        /// 字段：可选无符号端口。
        /// 来源去向：命令行到loopback listener。
        /// 约束：缺省或零采用系统随机端口。
        /// 稳定性：新增。
        /// 关联：REQ-OP-009；DES-OP-006。
        #[arg(long)]
        port: Option<u16>,
    },
    /// Start an echo-model conversation. No API key, no network — the M1
    /// smoke entry point.
    Hello,
    /// 用途：启动原引擎聊天。
    /// 字段：Provider、显式模型、端点、工作区和数据根。
    /// 来源去向：CLI到共享装配与引擎。
    /// 约束：保留离线入口，显式模型非空，端点不得含密钥。
    /// 稳定性：新增可选参数。
    /// 关联：需求REQ-OP-003、REQ-OP-004。
    #[command(
        long_about = "Run the shared agent loop. Local CLI tools are automatically approved within --workspace; this is not an OS sandbox. Host tool allowlists and interactive approval policy apply only to engine-host. Unsupported plan-mode and clarify tools are not advertised by this CLI."
    )]
    Chat {
        /// 模型Provider；显式参数优先，其次.env，无配置默认mock。
        #[arg(long)]
        provider: Option<String>,
        /// 用途：覆盖Provider默认模型
        /// 字段：可选模型
        /// 来源去向：CLI到引擎
        /// 约束：非空
        /// 稳定性：新增
        /// 关联：需求/设计 REQ-OP-003。
        #[arg(long)]
        model: Option<String>,
        /// 用途：配置Provider端点
        /// 字段：URL
        /// 来源去向：CLI到Provider
        /// 约束：禁止密钥嵌入
        /// 稳定性：新增
        /// 关联：需求/设计 DES-OP-002。
        #[arg(long)]
        base_url: Option<String>,
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

#[cfg(feature = "engine-host")]
#[derive(Debug, clap::Subcommand)]
/// 用途：声明仅本机工作区迁移的审核参数。
/// 字段：Plan只读配置，Apply精确绑定计划、摘要和备份目录。
/// 来源去向：本机管理员到Host离线迁移服务。
/// 约束：不使用模型.env，不开放远程或工具迁移权限。
/// 稳定性：engine-host可选功能，Linux/Windows同一参数。
/// 关联：需求/设计REQ-OP-025；DES-OP-013。
pub enum WorkspaceMigrationCommand {
    /// 用途：只读输出迁移JSON计划及SHA256，供人工审核。
    /// 字段：旧配置、目标配置和明确团队列表。
    /// 来源去向：本机配置/元数据到stdout。
    /// 约束：不创建输出文件或目录，不调用模型和网络。
    /// 稳定性：新增本机管理命令。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013。
    Plan {
        /// 用途：指定现行配置。
        /// 字段：绝对或本机相对TOML路径。
        /// 来源去向：命令行到只读HostConfig。
        /// 约束：原数据目录必须已存在且可独占。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        config: PathBuf,
        /// 用途：指定目标团队根配置。
        /// 字段：本机TOML路径。
        /// 来源去向：管理员到迁移审核计划。
        /// 约束：不自动覆盖原配置；只允许所选团队变根。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        target_config: PathBuf,
        /// 用途：明确需要迁移的团队。
        /// 字段：可重复或逗号分隔的稳定团队ID。
        /// 来源去向：管理员到原绑定范围筛选。
        /// 约束：不得为空或重复，不接受任意SessionID。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long, required = true, value_delimiter = ',')]
        team: Vec<String>,
    },
    /// 用途：按已确认摘要离线复制并原子重绑。
    /// 字段：旧/目标配置、计划文件、确认摘要及全新备份目录。
    /// 来源去向：本机人工审核结果到原Store事务。
    /// 约束：预检漂移或未清理运行/审批拒绝执行，不改原工作文件。
    /// 稳定性：新增本机管理命令。
    /// 关联：需求/设计REQ-OP-025；DES-OP-013。
    Apply {
        /// 用途：指定原配置。
        /// 字段：本机TOML路径。
        /// 来源去向：管理员到迁移协调器。
        /// 约束：必须与计划内容摘要相符。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        config: PathBuf,
        /// 用途：指定目标配置。
        /// 字段：本机TOML路径。
        /// 来源去向：管理员到迁移协调器。
        /// 约束：必须与计划相符，不自动覆盖原配置。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        target_config: PathBuf,
        /// 用途：读取人工审核过的计划。
        /// 字段：plan输出JSON文件路径。
        /// 来源去向：本机审核文件到摘要校验。
        /// 约束：不是授权任意路径或SQL的脚本。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        plan: PathBuf,
        /// 用途：明确确认准确计划。
        /// 字段：64位十六进制SHA256。
        /// 来源去向：人工审核到本机提交门禁。
        /// 约束：不允许省略或自动接受新计划。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        confirm_sha256: String,
        /// 用途：保存完整原数据/配置和迁移回执。
        /// 字段：全新且不在源/目标/home中的备份目录。
        /// 来源去向：管理员到可回退备份。
        /// 约束：不覆盖既有目录，不删除原文件。
        /// 稳定性：新增。
        /// 关联：需求/设计REQ-OP-025；DES-OP-013。
        #[arg(long)]
        backup: PathBuf,
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
