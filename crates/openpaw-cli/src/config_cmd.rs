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
    Doctor {
        /// OpenPaw home (default: OPENPAW_HOME or ~/.openpaw).
        #[arg(long)]
        home: Option<std::path::PathBuf>,
        /// Workspace scanned by the network audit (default: current
        /// directory).
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
    },
}

/// `openpaw config show` — resolved home + provider key presence.
/// 功能：显示同一共享快照的脱敏 Provider 配置。
/// 参数：见签名；入口路径/名称或显式配置。
/// 返回：见签名的退出码、Provider 或摘要。
/// 错误：配置和读取失败脱敏处理；doctor WARN 不失败。
/// 状态：诊断不连接模型；摘要不写 JSONL。
/// 并发与取消：使用已有运行时和锁，未新增调度或恢复决策。
/// 权限：密钥不进入输出或摘要。
/// 关联：CHG-2026-0023 共享配置/目录。
pub fn run_show(home: &Path) -> i32 {
    let state = if home.is_dir() { "present" } else { "missing" };
    println!("openpaw home: {} ({state})", home.display());
    println!("ANTHROPIC_API_KEY: {}", key_state("ANTHROPIC_API_KEY"));
    println!("OPENAI_API_KEY: {}", key_state("OPENAI_API_KEY"));
    println!("chat default provider: mock (offline)");
    let config = openpaw_llm::config::ConfigSnapshot::from_env();
    for (name, _, _) in openpaw_llm::config::PROVIDERS {
        match config.resolve(name, Default::default()) {
            Ok(c) => println!(
                "{name}: model={}, base URL={}, max output tokens={}, locally configured",
                c.model,
                c.base_url.as_deref().unwrap_or("offline"),
                c.max_output_tokens
            ),
            Err(error) => println!("{name}: unavailable ({error})"),
        }
    }
    0
}

/// Masked presence state of one environment key.
fn key_state(name: &str) -> &'static str {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => "set (value hidden)",
        _ => "not set",
    }
}

/// `openpaw config doctor` — straight alias of `openpaw doctor`.
pub fn run_doctor(home: &Path, workspace: &Path) -> i32 {
    doctor::run(home, workspace)
}
