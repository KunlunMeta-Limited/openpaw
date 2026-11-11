//! `openpaw model ...` — static provider/model table and the default
//! provider placeholder.

/// Print a three-segment error (what happened / why / what to do next).
fn report_error(what: String, why: &str, next: &str) {
    eprintln!("error: {what}");
    eprintln!("why: {why}");
    eprintln!("next: {next}");
}

/// The built-in provider list with default models (static table; matches
/// what `openpaw chat --provider` accepts and what openpaw-llm ships).
const PROVIDERS: [(&str, &str, &str); 4] = [
    (
        "anthropic",
        "claude-sonnet-4",
        "network; requires ANTHROPIC_API_KEY",
    ),
    ("openai", "gpt-4o", "network; requires OPENAI_API_KEY"),
    ("mock", "mock-1", "offline; deterministic counter replies"),
    ("echo", "echo-1", "offline; zero-config echo"),
];

/// Subcommands of `openpaw model`.
/// 用途：保持原命令/参数在当前入口的准确语义。
/// 字段：沿用声明中的参数或成员。
/// 来源去向：原调用方到当前装配入口。
/// 约束：不新增隐式权限、持久化或不支持的能力保证。
/// 稳定性：保持原接口，行为边界见本轮记录。
/// 关联：需求/设计：REQ-OP-003；DES-OP-002。
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print the built-in providers and their default models.
    List,
    /// Report that persisting a default provider is unsupported; use --provider or .env.
    /// 用途：保持原命令/参数在当前入口的准确语义。
    /// 字段：沿用声明中的参数或成员。
    /// 来源去向：原调用方到当前装配入口。
    /// 约束：不新增隐式权限、持久化或不支持的能力保证。
    /// 稳定性：保持原接口，行为边界见本轮记录。
    /// 关联：需求/设计：REQ-OP-003；DES-OP-002。
    Default {
        /// Provider name as shown by `openpaw model list`.
        name: String,
    },
}

/// `openpaw model list` — static provider table.
pub fn run_list() -> i32 {
    println!("{:<10} {:<18} notes", "provider", "default model");
    for (name, model, notes) in PROVIDERS {
        println!("{name:<10} {model:<18} {notes}");
    }
    0
}

/// 功能：明确拒绝尚未实现的默认模型持久化命令。
/// 参数：name为原支持的Provider名称。
/// 返回：非零退出码，不伪报配置成功。
/// 错误：未知Provider或尚未支持写入均说明原因与替代入口。
/// 状态：不读写配置。
/// 并发与取消：无等待或共享状态。
/// 权限：不新增配置写权限，不请求模型服务。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub fn run_default(name: &str) -> i32 {
    if !PROVIDERS.iter().any(|(known, _, _)| *known == name) {
        report_error(
            format!("unknown provider `{name}`"),
            "`model default` accepts one of anthropic, openai, mock, echo.",
            "re-run with one of those names, or run `openpaw model list`.",
        );
        return 1;
    }
    report_error(
        "unsupported: persisting a default provider is not implemented".into(),
        "no configuration was changed.",
        "use `openpaw chat --provider <name>` or configure OPENPAW_PROVIDER in your .env.",
    );
    1
}
