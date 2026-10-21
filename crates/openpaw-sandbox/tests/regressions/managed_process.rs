//! 职责：原生进程与隔离回归。
//! 输入输出：临时进程到退出/输出/取消证据。
//! 依赖：原Sandbox和系统shell。
//! 禁止职责：不以交叉编译声明Windows运行通过。
//! 原生进程/管道行为回归；测试数据只在临时目录。
use openpaw_sandbox::{Sandbox, SandboxPolicy};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn d08_both_output_pipes_are_drained_past_retention_limit() {
    let command = if cfg!(windows) {
        "for /L %i in (1,1,40000) do @(echo 0123456789012345678901234567890123456789 & echo 0123456789012345678901234567890123456789 1>&2)"
    } else {
        "i=0; while [ $i -lt 40000 ]; do printf '0123456789012345678901234567890123456789\\n'; printf '0123456789012345678901234567890123456789\\n' >&2; i=$((i+1)); done"
    };
    let sandbox = Sandbox::new(SandboxPolicy::None).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(45), sandbox.exec(command, None))
        .await
        .expect("must not deadlock")
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("output truncated"));
    assert!(result.stderr.contains("output truncated"));
    assert!(result.stdout.len() < 1024 * 1024 + 100);
    assert!(result.stderr.len() < 1024 * 1024 + 100);
}
#[tokio::test]
async fn d08_cancel_reaps_a_child_tree_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("t2-worker-marker"), "isolated test fixture").unwrap();
    let command = format!(
        "\"{}\" --exact t2_child_process --nocapture",
        std::env::current_exe().unwrap().display()
    );
    let sandbox = Sandbox::new(SandboxPolicy::None).unwrap();
    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let task = {
        let cancel = cancel.clone();
        let root = dir.path().to_owned();
        tokio::spawn(async move {
            sandbox
                .exec_cancellable(&command, Some(&root), cancel)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !dir.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("tree cleanup must finish")
        .unwrap();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    for file in ["worker.pid", "grandchild.pid"] {
        let pid = std::fs::read_to_string(dir.path().join(file))
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(
            !process_is_running(pid),
            "process {pid} remains alive after cleanup"
        );
    }
}
#[tokio::test]
async fn d09_requested_domain_isolation_is_never_an_environment_hint() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::new(SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![dir.path().to_owned()],
        allowed_domains: vec!["example.com".into()],
    })
    .unwrap();
    let result = sandbox
        .exec("echo escaped > escaped", Some(dir.path()))
        .await;
    assert!(result.is_err());
    assert!(!dir.path().join("escaped").exists());
}
#[tokio::test]
async fn d08_pre_cancelled_command_has_no_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = Sandbox::new(SandboxPolicy::None)
        .unwrap()
        .exec_cancellable("echo bad > side-effect", Some(dir.path()), cancel)
        .await;
    assert!(result.is_err());
    assert!(!dir.path().join("side-effect").exists());
}
#[tokio::test]
async fn d08_local_cwd_supports_spaces_and_unicode() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("工作 空格");
    std::fs::create_dir(&root).unwrap();
    let result = Sandbox::new(SandboxPolicy::None)
        .unwrap()
        .exec("echo hello > result.txt", Some(&root))
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(std::fs::read_to_string(root.join("result.txt"))
        .unwrap()
        .contains("hello"));
}

// 供上述真实进程树测试复用本测试二进制；常规套件没有隔离marker时不启动子进程。
#[test]
fn t2_child_process() {
    if !Path::new("t2-worker-marker").exists() {
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "t2_grandchild_process", "--nocapture"])
        .spawn()
        .unwrap();
    std::fs::write("worker.pid", std::process::id().to_string()).unwrap();
    std::fs::write("grandchild.pid", child.id().to_string()).unwrap();
    std::fs::write("ready", "ready").unwrap();
    let _ = child.wait().unwrap();
}
#[test]
fn t2_grandchild_process() {
    if !Path::new("t2-worker-marker").exists() {
        return;
    }
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}
use std::path::Path;
#[cfg(target_os = "linux")]
fn process_is_running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z')),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("cannot inspect process {pid}: {e}"),
    }
}
#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows::Win32::{Foundation::*, System::Threading::*};
    // SAFETY：只查询测试自己创建的pid；取得的句柄在本函数退出前关闭。
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => handle,
            Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => return false,
            Err(e) => panic!("cannot inspect test process {pid}: {e}"),
        };
        let mut code = 0;
        let result = GetExitCodeProcess(handle, &mut code);
        let _ = CloseHandle(handle);
        result.unwrap();
        code == 259
    }
}

#[tokio::test]
async fn d34_builder_literal_arguments_cannot_start_another_command() {
    let dir = tempfile::tempdir().unwrap();
    let argument = if cfg!(windows) {
        "x&echo>injected"
    } else {
        "x;echo>injected"
    };
    let result = openpaw_sandbox::SandboxedCommand::new("echo", SandboxPolicy::None)
        .arg(argument)
        .cwd(dir.path())
        .exec()
        .await;
    if cfg!(windows) {
        assert!(result.is_err());
    } else {
        assert!(result.unwrap().stdout.contains(argument));
    }
    assert!(!dir.path().join("injected").exists());
}
#[tokio::test]
async fn d34_unsupported_builder_environment_is_explicitly_rejected() {
    let result = openpaw_sandbox::SandboxedCommand::new("echo", SandboxPolicy::None)
        .env("TEST_FIXTURE", "test-value")
        .exec()
        .await;
    assert!(result.unwrap_err().to_string().contains("env overrides"));
}
#[cfg(unix)]
#[tokio::test]
async fn d34_quotes_and_command_substitution_remain_literal_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let argument = "O'Reilly $(echo bad > injected)";
    let result = openpaw_sandbox::SandboxedCommand::new("echo", SandboxPolicy::None)
        .arg(argument)
        .cwd(dir.path())
        .exec()
        .await
        .unwrap();
    assert!(result.stdout.contains(argument));
    assert!(!dir.path().join("injected").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn d10_unc_cwd_is_rejected_instead_of_cmd_directory_fallback() {
    for root in [
        r"\\unavailable-server\unavailable-share",
        r"\\?\UNC\unavailable-server\unavailable-share",
    ] {
        let result = Sandbox::new(SandboxPolicy::None)
            .unwrap()
            .exec("echo should-not-run", Some(Path::new(root)))
            .await;
        assert!(result.unwrap_err().to_string().contains("UNC/device"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn d08_builder_timeout_cleans_delayed_descendant_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let result = openpaw_sandbox::SandboxedCommand::new(
        "(sleep 0.3; echo leaked > leaked) & wait",
        SandboxPolicy::None,
    )
    .cwd(dir.path())
    .timeout(Duration::from_millis(30))
    .exec()
    .await;
    assert!(matches!(
        result,
        Err(openpaw_sandbox::SandboxError::Timeout(_))
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !dir.path().join("leaked").exists(),
        "timed-out descendant still wrote a file"
    );
}
