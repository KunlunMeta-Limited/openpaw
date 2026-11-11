//! 职责：保留补全命令入口并明确报告尚未实现。
//! 输入输出：shell名称到非零退出与原因。
//! 依赖：无。
//! 禁止职责：不输出可被误当成补全脚本的成功说明。

/// 功能：拒绝尚未实现的补全生成并校验shell名称。
/// 参数：shell为原命令参数。
/// 返回：失败退出码1。
/// 错误：未知shell与尚未实现分别报告。
/// 状态：不生成脚本或修改用户配置。
/// 并发与取消：无。
/// 权限：不新增依赖和写权限。
/// 关联：需求/设计：CHG-2026-0007 §12.169；能力与错误边界修复。
pub fn run(shell: &str) -> i32 {
    if !matches!(shell, "bash" | "zsh" | "fish" | "powershell" | "elvish") {
        eprintln!("error: unknown shell `{shell}`\nwhy: expected bash, zsh, fish, powershell, or elvish.\nnext: use `openpaw --help` to inspect available commands.");
    } else {
        eprintln!("error: unsupported: shell completion generation is not implemented\nwhy: no completion script was generated.\nnext: use `openpaw --help` and subcommand --help; do not source this command's output.");
    }
    1
}
