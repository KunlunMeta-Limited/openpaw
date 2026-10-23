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

/// 功能：沿用原入口展示有效配置及脱敏密钥状态。
/// 参数：home为默认模型配置目录。
/// 返回：有效为零，读取或装配失败为一。
/// 错误：不输出原始文件或凭据。
/// 状态：只读，不发模型请求。
/// 并发与取消：无后台任务，不修改全局环境。
/// 权限：只展示本机配置。
/// 关联：需求/设计REQ-OP-003；DES-OP-002。
pub fn run_show(home: &Path) -> i32 {
    run_show_with_env(home, None)
}
/// 功能：展示有效模型和配置来源，密钥仅显示有无。
/// 参数：home为默认配置目录；env_file为可选显式文件。
/// 返回：有效配置为零，读取或装配错误为一。
/// 错误：不输出配置原始行、秘密值或包含凭据的非法URL。
/// 状态：只读，不连接模型服务或创建文件。
/// 并发与取消：启动诊断无后台任务，无共享可变环境。
/// 权限：只展示本机信息，不向Office发送。
/// 关联：需求/设计REQ-OP-003；DES-OP-002。
pub fn run_show_with_env(home: &Path, env_file: Option<&Path>) -> i32 {
    let settings = match openpaw_llm::model_config::ModelEnvironment::load(home, env_file) {
        Ok(settings) => settings,
        Err(_) => {
            eprintln!("模型.env读取失败，文件缺失、不可读或格式错误（内容已隐藏）");
            return 1;
        }
    };
    let state = if home.is_dir() { "present" } else { "missing" };
    println!("openpaw home: {} ({state})", home.display());
    let (file, loaded) = settings.file();
    println!(
        "model env file: {} ({})",
        file.display(),
        if loaded {
            "loaded"
        } else {
            "missing; using existing defaults"
        }
    );
    for name in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY"] {
        let present = settings.value(name).is_some_and(|s| !s.trim().is_empty());
        println!(
            "{name}: {} [{}]",
            if present {
                "set (value hidden)"
            } else {
                "not set"
            },
            settings.source(name)
        );
    }
    let (provider, model) = match settings.assemble(None, None, None, None, "mock") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("模型配置不合法或缺少凭据，请检查Provider、模型、端点和密钥（内容已隐藏）");
            return 1;
        }
    };
    println!(
        "default provider: {} [{}]",
        provider.name(),
        settings.source("OPENPAW_PROVIDER")
    );
    println!("model: {model} [{}]", settings.source("OPENPAW_MODEL"));
    // 端点在原装配器校验成功后才显示，避免嵌入式凭据泄漏。
    println!(
        "base URL: {} [{}]",
        settings
            .value("OPENPAW_BASE_URL")
            .unwrap_or("provider default"),
        settings.source("OPENPAW_BASE_URL")
    );
    0
}

/// `openpaw config doctor` — straight alias of `openpaw doctor`.
pub fn run_doctor(home: &Path, workspace: &Path) -> i32 {
    doctor::run(home, workspace)
}
