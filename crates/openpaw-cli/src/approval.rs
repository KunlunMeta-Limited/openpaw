//! 功能：通过唯一输入所有者适配终端人工审批。
//! 参数：可注入的按行输入与提示输出。
//! 返回：once/session/reject 决定。
//! 错误：空白、无效、EOF 和非 TTY 均拒绝。
//! 状态：输入按请求读取，不预取下一条聊天。
//! 并发与取消：丢弃等待不会把旧行交给新的请求。
//! 权限：仅展示工具名，不展示参数或原始原因。
//! 关联：共享 Engine ApprovalGate。
use std::io::{BufRead, Write};
use tokio::sync::oneshot;

/// 功能：通过一个后台所有者按需读取输入行。
pub struct InputOwner {
    tx: std::sync::mpsc::Sender<oneshot::Sender<Option<String>>>,
}
impl InputOwner {
    /// 功能：创建唯一按需输入所有者。
    /// 参数：拥有所有权的 BufRead 输入，可用 Cursor 注入测试。
    /// 返回：输入请求句柄。
    /// 错误：线程创建异常由标准库报告。
    /// 状态：后台只按请求读取一行，无预取。
    /// 并发与取消：所有读者串行请求；读取线程不持审批锁。
    /// 权限：不消费无请求的下一条聊天。
    /// 关联：chat prompt 与审批输入。
    pub fn new(mut reader: impl BufRead + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<oneshot::Sender<Option<String>>>();
        std::thread::spawn(move || {
            for response in rx {
                if response.is_closed() {
                    continue;
                }
                let mut line = String::new();
                let value = match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => Some(line),
                };
                let _ = response.send(value);
            }
        });
        Self { tx }
    }
    /// 功能：请求下一行终端输入。
    /// 参数：无。
    /// 返回：一行文本；EOF、读失败或线程退出为 None。
    /// 错误：读失败安全结束。
    /// 状态：一次请求对应一次读取。
    /// 并发与取消：丢弃 future 后旧回复不转交后续请求。
    /// 权限：调用方区分聊天和审批阶段。
    /// 关联：InputOwner 后台读取线程。
    pub async fn read_line(&self) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(tx).ok()?;
        rx.await.ok().flatten()
    }
}
/// 功能：表达终端批准一次、session 授权或拒绝。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Once,
    Session,
    Reject,
}

/// 功能：解析终端审批决定。
/// 参数：可选输入行。
/// 返回：once/session 或 Reject。
/// 错误：空白、无效和 EOF 均 Reject。
/// 状态：纯解析。
/// 并发与取消：同步无锁。
/// 权限：不将 yes 等模糊输入视为批准。
/// 关联：ask 与终端回归。
pub fn parse_answer(line: Option<&str>) -> Answer {
    match line.map(str::trim) {
        Some("once") => Answer::Once,
        Some("session" | "always") => Answer::Session,
        _ => Answer::Reject,
    }
}

/// 功能：安全显示工具名并取得终端审批。
/// 参数：唯一输入所有者、输出、TTY 标记和工具名。
/// 返回：Once、Session 或 Reject。
/// 错误：EOF/无效/非 TTY 拒绝；提示失败拒绝。
/// 状态：非 TTY 不读取输入。
/// 并发与取消：等待期间不持 Engine gate 锁。
/// 权限：不显示完整参数或原始原因。
/// 关联：chat 审批通知 adapter。
pub async fn ask(
    input: &InputOwner,
    output: &mut impl Write,
    interactive: bool,
    tool_name: &str,
) -> Answer {
    let safe_name: String = tool_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(80)
        .collect();
    if !interactive {
        let _ = writeln!(
            output,
            "approval: tool {safe_name} rejected; manual approval requires an interactive terminal"
        );
        return Answer::Reject;
    }
    let displayed = write!(
        output,
        "approval: tool {safe_name}; once / session / reject [reject]: "
    );
    if displayed.is_err() || output.flush().is_err() {
        return Answer::Reject;
    }
    let line = input.read_line().await;
    parse_answer(line.as_deref())
}
