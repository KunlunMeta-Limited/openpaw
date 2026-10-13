//! `openpaw model ...`：显示共享配置目录中的默认与自定义模型。
//!
//! 目录来自 openpaw-llm::config，不连接上游或展示密钥；default 子命令
//! 保留既有未持久化提示，不承担配置写入。关联 CHG-2026-0023。

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// `openpaw model` 的模型目录与默认协议校验子命令。
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// 列出内置协议的默认模型及环境配置中的自定义模型。
    List,
    /// Record the default provider choice (placeholder: no file is
    /// written yet).
    Default {
        /// Provider name as shown by `openpaw model list`.
        name: String,
    },
}

/// 功能：列出共享目录中的默认和配置模型。
/// 参数：无；读取当前进程的模型环境配置。
/// 返回：退出码 0。
/// 错误：无；目录不代表上游认证或连通性验证。
/// 状态：只打印目录，不写配置或会话。
/// 并发与取消：同步使用本次环境快照，无异步等待。
/// 权限：不访问网络，不展示密钥。
/// 关联：CHG-2026-0023 共享配置/目录。
pub fn run_list() -> i32 {
    println!("{:<10} {:<18} notes", "provider", "default model");
    let config = openpaw_llm::config::ConfigSnapshot::from_env();
    for (name, _, notes) in openpaw_llm::config::PROVIDERS {
        for model in config.models(name) {
            println!("{name:<10} {model:<18} {notes}");
        }
    }
    0
}

/// 功能：校验支持协议并保持既有未持久化提示。
/// 参数：name 为共享目录中的协议名称。
/// 返回：已知协议返回 0，未知协议返回 1。
/// 错误：未知协议打印三段式错误提示。
/// 状态：只校验并提示，不持久化默认值。
/// 并发与取消：同步调用，无共享可变状态。
/// 权限：不访问网络或读取密钥。
/// 关联：CHG-2026-0023 共享配置/目录。
pub fn run_default(name: &str) -> i32 {
    if !openpaw_llm::config::PROVIDERS
        .iter()
        .any(|(known, _, _)| *known == name)
    {
        report_error(
            format!("unknown provider `{name}`"),
            "`model default` accepts one of anthropic, openai, mock, echo.",
            "re-run with one of those names, or run `openpaw model list`.",
        );
        return 1;
    }
    println!("default provider `{name}` accepted (config file persistence lands in M5b).");
    println!("why: no config file is read or written yet, so the choice is not persisted.");
    println!(
        "next: re-run after M5b to persist the default; `openpaw chat --provider` works today."
    );
    0
}
