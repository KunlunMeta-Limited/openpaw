//! 职责：统一原执行后端的进程组、输出上限与取消回收生命周期。
//! 输入输出：配置好的Command和调用token到真实输出/退出码/审计。
//! 依赖：原tokio进程、Linux进程组及Windows原Job实现。
//! 禁止职责：不解释工具协议、不建立新的沙箱策略或降级限制。
#[cfg(unix)]
use crate::{audit::AuditLog, SandboxError, SandboxResult};
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

/// 用途：每管道最多保留的字节数
/// 字段：每条管道保留1MiB
/// 来源去向：子进程输出到结果
/// 约束：超限继续排空，内存有界
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
pub(crate) const OUTPUT_LIMIT: usize = 1024 * 1024;
/// 功能：从允许列表构造工具继承环境
/// 参数：无
/// 返回：必要系统环境键值
/// 错误：缺失键跳过
/// 状态：只读当前进程环境
/// 并发与取消：无等待
/// 权限：不继承模型/Office密钥或任意业务环境
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub(crate) fn tool_environment() -> Vec<(String, String)> {
    [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "LC_ALL",
        "TERM",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
    .collect()
}

/// 功能：保留有界字节并持续读取到EOF
/// 参数：reader为一条管道
/// 返回：UTF8有损解码的文本和截断提示
/// 错误：真实读取错误透传
/// 状态：读取并丢弃超限输出
/// 并发与取消：始终读到EOF；进程所有者负责取消杀树
/// 权限：只接收工具输出，不改变权限
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
#[cfg(unix)]
async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut retained = Vec::new();
    let mut chunk = [0; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(OUTPUT_LIMIT - retained.len());
        retained.extend_from_slice(&chunk[..keep]);
        truncated |= keep < count;
    }
    let mut text = String::from_utf8_lossy(&retained).into_owned();
    if truncated {
        text.push_str("\n[output truncated; remaining bytes drained]");
    }
    Ok(text)
}

#[cfg(unix)]
struct ProcessGroup {
    pid: i32,
    armed: bool,
}
#[cfg(unix)]
impl ProcessGroup {
    fn terminate(&self) -> std::io::Result<()> {
        // SAFETY：正pid来自本函数spawn的独立进程组；负pid仅指向该组。
        if unsafe { libc::kill(-self.pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(e)
        }
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.terminate();
        }
    }
}

/// 功能：执行配置命令并持有进程组直到实际回收
/// 参数：cmd为后端参数；command/cwd/policy/backend为审计；cancel为调用取消
/// 返回：有界输出、真实退出码和审计
/// 错误：创建/管道/回收失败返回错误，取消以明确错误返回
/// 状态：创建独立进程组并终止遗留后台后代
/// 并发与取消：双管道同时排空；取消杀组后等待leader和EOF，不提前返回
/// 权限：不降低后端配置的限制；仅继承必要系统环境
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
#[cfg(unix)]
pub(crate) async fn run_command(
    mut cmd: tokio::process::Command,
    command: &str,
    cwd: Option<&Path>,
    policy: &str,
    backend: &str,
    cancel: CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    use std::process::Stdio;
    let start = std::time::Instant::now();
    if cancel.is_cancelled() {
        return Err(SandboxError::ExecutionFailed(
            "command cancelled before spawn".into(),
        ));
    }
    cmd.process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env_clear().envs(tool_environment());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn()?;
    let mut group = ProcessGroup {
        pid: child
            .id()
            .ok_or_else(|| SandboxError::ExecutionFailed("missing child pid".into()))?
            as i32,
        armed: true,
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SandboxError::ExecutionFailed("missing stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SandboxError::ExecutionFailed("missing stderr".into()))?;
    let wait = async {
        let (status, cancelled) = tokio::select! {
            biased;
            _=cancel.cancelled()=> { group.terminate()?; (child.wait().await?,true) },
            status=child.wait()=>(status?,false),
        };
        // leader正常退出也结束其后台后代，防止继承的管道永远不EOF。
        group.terminate()?;
        Ok::<_, std::io::Error>((status, cancelled))
    };
    // join而非try_join：一条管道失败也不能drop另一条和正在回收的进程。
    let (status, stdout, stderr) = tokio::join!(wait, drain(stdout), drain(stderr));
    let (status, cancelled) = status?;
    // 已显式杀组、回收leader并排空两管道；Drop不再向可能复用的组号发信号。
    group.armed = false;
    let stdout = stdout?;
    let stderr = stderr?;
    if cancelled || cancel.is_cancelled() {
        return Err(SandboxError::ExecutionFailed(
            "command cancelled; process group reaped".into(),
        ));
    }
    use std::os::unix::process::ExitStatusExt;
    let exit_code = status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1));
    let mut audit = AuditLog::new(command, cwd, policy, backend);
    audit.duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    audit.exit_code = exit_code;
    Ok(SandboxResult {
        stdout,
        stderr,
        exit_code,
        audit,
    })
}
