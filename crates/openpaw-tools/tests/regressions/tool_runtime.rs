//! 职责：工具授权、执行观察和Todo回归。
//! 输入输出：临时工作区到可验证副作用与错误。
//! 依赖：原ToolRegistry/Environment/Todo。
//! 禁止职责：不使用用户配置或业务数据。
//! 原工具/执行环境/Todo的真实行为回归；临时工作区不使用用户配置。
use openpaw_tools::{
    exec::environment::{create_environment, EnvironmentKind, SandboxPolicy},
    meta::todo,
};
use serde_json::json;
#[tokio::test]
async fn d11_corrupt_todo_is_not_an_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".openpaw")).unwrap();
    std::fs::write(dir.path().join(".openpaw/todos.jsonl"), "{broken\n").unwrap();
    let result = todo::run(dir.path(), "todo_list", json!({})).await;
    assert!(
        result.is_err() || result.as_ref().unwrap().is_error,
        "corruption must be reported"
    );
}
#[tokio::test]
async fn d09_environment_factory_does_not_panic_in_current_thread_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let env = create_environment(
        EnvironmentKind::Sandbox(SandboxPolicy::Restricted),
        dir.path().to_owned(),
    );
    assert_ne!(
        env.name(),
        "local",
        "requested restriction must never downgrade"
    );
}

use async_trait::async_trait;
use openpaw_core::{
    Error, Result, ToolCallBinding, ToolCallContext, ToolExecutionObserver, ToolExecutor,
};
use openpaw_tools::{
    exec::environment::{Environment, EnvironmentResult},
    ToolRegistry,
};
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_util::sync::CancellationToken;
struct CountingEnvironment(AtomicUsize);
#[async_trait]
impl Environment for CountingEnvironment {
    async fn exec(&self, _: &str, _: Option<&Path>) -> EnvironmentResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        EnvironmentResult {
            stdout: "injected environment".into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
    async fn push_file(&self, _: &Path, _: &Path) -> Result<()> {
        Ok(())
    }
    async fn pull_file(&self, _: &Path, _: &Path) -> Result<()> {
        Ok(())
    }
    async fn health_check(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "counting"
    }
}
#[tokio::test]
async fn d07_approval_is_bound_to_one_exact_call_and_uses_injected_environment() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(CountingEnvironment(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(dir.path()).with_environment(env.clone());
    let args = json!({"command":"echo $(whoami)"});
    let context = ToolCallContext::new("run", "call", "bash", &args, CancellationToken::new());
    assert!(registry
        .call_with_context("bash", args.clone(), context.clone())
        .await
        .is_err());
    assert_eq!(env.0.load(Ordering::SeqCst), 0);
    assert!(
        !registry
            .call_with_context("bash", args.clone(), context.clone().approve())
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(env.0.load(Ordering::SeqCst), 1);
    assert!(registry
        .call_with_context("bash", json!({"command":"echo changed"}), context.approve())
        .await
        .is_err());
    assert_eq!(env.0.load(Ordering::SeqCst), 1);
    // 新一次没有批准的调用不能继承前一次的允许状态。
    assert!(registry.call("bash", args).await.unwrap().is_error);
    assert_eq!(env.0.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn d07_approved_dangerous_command_is_still_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let env = Arc::new(CountingEnvironment(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(dir.path()).with_environment(env.clone());
    let args = json!({"command":"rm -rf /"});
    let context =
        ToolCallContext::new("run", "call", "bash", &args, CancellationToken::new()).approve();
    assert!(
        registry
            .call_with_context("bash", args, context)
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(env.0.load(Ordering::SeqCst), 0);
}
struct Observer {
    calls: AtomicUsize,
    fail: bool,
}
#[async_trait]
impl ToolExecutionObserver for Observer {
    async fn started(&self, binding: &ToolCallBinding) -> Result<()> {
        assert!(!binding.call_id.is_empty());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(Error::Store("injected durable observation failure".into()))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn d05_running_observation_failure_prevents_file_write() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report"), "before").unwrap();
    let observer = Arc::new(Observer {
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let registry = ToolRegistry::new(dir.path()).with_execution_observer(observer.clone());
    let args = json!({"path":"report","content":"after"});
    let context =
        ToolCallContext::new("run", "call", "write_file", &args, CancellationToken::new())
            .approve();
    assert!(registry
        .call_with_context("write_file", args, context)
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("report")).unwrap(),
        "before"
    );
    assert_eq!(observer.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn d11_structured_read_of_missing_todos_has_no_writes() {
    let dir = tempfile::tempdir().unwrap();
    assert!(todo::read(dir.path()).unwrap().is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn d11_concurrent_registries_preserve_all_updates_and_unique_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut tasks = vec![];
    for i in 0..48 {
        let root = dir.path().to_owned();
        tasks.push(tokio::spawn(async move {
            ToolRegistry::new(root)
                .call("todo_add", json!({"title":format!("task-{i}")}))
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        assert!(!task.await.unwrap().is_error);
    }
    let todos = todo::read(dir.path()).unwrap();
    assert_eq!(todos.len(), 48);
    let ids: std::collections::HashSet<_> = todos.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 48);
}
#[tokio::test]
async fn d11_lock_wait_cancels_without_entering_tool_body() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".openpaw")).unwrap();
    let _guard =
        openpaw_sandbox::try_lock_exclusive(&dir.path().join(".openpaw/todos.lock")).unwrap();
    let observer = Arc::new(Observer {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let registry = ToolRegistry::new(dir.path()).with_execution_observer(observer.clone());
    let cancel = CancellationToken::new();
    let args = json!({"title":"must not write"});
    let context = ToolCallContext::new("run", "call", "todo_add", &args, cancel.clone()).approve();
    let call = registry.call_with_context("todo_add", args, context);
    tokio::pin!(call);
    tokio::select! {biased;_= &mut call=>panic!("lock held, cannot finish"),_=tokio::task::yield_now()=>{}}
    assert_eq!(observer.calls.load(Ordering::SeqCst), 0);
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), call)
            .await
            .unwrap(),
        Err(Error::Cancelled)
    ));
    assert!(todo::read(dir.path()).unwrap().is_empty());
    assert_eq!(observer.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn d11_invalid_jsonl_never_gets_overwritten_by_add() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".openpaw")).unwrap();
    let path = dir.path().join(".openpaw/todos.jsonl");
    for content in ["{bad\n", "{\"id\":1,\"title\":\"a\",\"status\":\"unknown\"}\n", "{\"id\":1,\"title\":\"a\",\"status\":\"done\"}\n{\"id\":1,\"title\":\"b\",\"status\":\"done\"}\n"] {
        std::fs::write(&path,content).unwrap();assert!(todo::run(dir.path(),"todo_add",json!({"title":"new"})).await.is_err());assert_eq!(std::fs::read_to_string(&path).unwrap(),content);
    }
}
#[cfg(unix)]
#[tokio::test]
async fn d11_link_to_another_workspace_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join(".openpaw")).unwrap();
    assert!(todo::read(dir.path()).is_err());
    assert!(todo::run(dir.path(), "todo_add", json!({"title":"escape"}))
        .await
        .is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
#[cfg(windows)]
#[tokio::test]
async fn d11_windows_file_without_delete_share_keeps_old_data_on_replace_failure() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    todo::run(dir.path(), "todo_add", json!({"title":"old"}))
        .await
        .unwrap();
    let path = dir.path().join(".openpaw/todos.jsonl");
    let before = std::fs::read(&path).unwrap();
    let _occupied = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(todo::run(dir.path(), "todo_add", json!({"title":"new"}))
        .await
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")));
}

#[cfg(windows)]
#[tokio::test]
async fn d11_windows_drive_case_alias_and_verbatim_paths_share_todos() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("大小写 Mixed Case");
    std::fs::create_dir(&root).unwrap();
    todo::run(&root, "todo_add", json!({"title":"first"}))
        .await
        .unwrap();
    let canonical = std::fs::canonicalize(&root).unwrap();
    let lower = std::path::PathBuf::from(root.to_string_lossy().to_lowercase());
    for alias in [&canonical, &lower] {
        assert_eq!(todo::read(alias).unwrap()[0].title, "first");
        let outcome = todo::run(alias, "todo_add", json!({"title":"alias"}))
            .await
            .unwrap();
        assert!(!outcome.is_error);
    }
    assert_eq!(todo::read(&root).unwrap().len(), 3);
}
#[cfg(windows)]
#[tokio::test]
async fn d11_windows_junction_to_other_workspace_is_rejected() {
    use std::os::windows::process::CommandExt;
    let dir = tempfile::Builder::new()
        .prefix("junction 工作区 ")
        .tempdir()
        .unwrap();
    let outside = tempfile::Builder::new()
        .prefix("outside 外部 ")
        .tempdir()
        .unwrap();
    let junction = dir.path().join(".openpaw");
    // /J目录junction不需要symlink权限；只操作本测试临时目录，不配置系统共享。
    let script = format!(
        "mklink /J \"{}\" \"{}\"",
        junction.display(),
        outside.path().display()
    );
    let output = std::process::Command::new("cmd.exe")
        // cmd不使用MSCRT的反斜杠引号规则，脚本必须保留原生双引号。
        .args(["/D", "/V:OFF", "/S", "/C"])
        .raw_arg(format!("\"{script}\""))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture failed: {:?}",
        output
    );
    assert!(todo::read(dir.path()).is_err());
    assert!(todo::run(dir.path(), "todo_add", json!({"title":"escape"}))
        .await
        .is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    // 显式移除junction自身，避免TempDir递归清理依赖平台实现。
    std::fs::remove_dir(junction).unwrap();
}

#[tokio::test]
async fn d11_oversized_update_cannot_make_the_existing_store_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    todo::run(dir.path(), "todo_add", json!({"title":"before"}))
        .await
        .unwrap();
    let old = std::fs::read(dir.path().join(".openpaw/todos.jsonl")).unwrap();
    assert!(todo::run(
        dir.path(),
        "todo_add",
        json!({"title":"x".repeat(2*1024*1024)})
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(dir.path().join(".openpaw/todos.jsonl")).unwrap(),
        old
    );
    assert_eq!(todo::read(dir.path()).unwrap()[0].title, "before");
}
#[cfg(unix)]
#[tokio::test]
async fn d11_permission_error_is_not_an_empty_list() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    todo::run(dir.path(), "todo_add", json!({"title":"protected"}))
        .await
        .unwrap();
    let path = dir.path().join(".openpaw/todos.jsonl");
    let original = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
    let result = todo::read(dir.path());
    std::fs::set_permissions(&path, original).unwrap();
    assert!(
        result.is_err(),
        "test must execute as ordinary user with real access denial"
    );
}

#[tokio::test]
async fn t3_bounded_preview_preserves_read_file_lines_and_rejects_binary_large_escape() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("中文 space.txt");
    std::fs::write(&file, "第一行\nsecond\n").unwrap();
    let plain =
        openpaw_tools::fs::read::read_text_bounded(root.path(), "中文 space.txt", 256 * 1024)
            .await
            .unwrap();
    assert_eq!(plain, "第一行\nsecond\n");
    let read =
        openpaw_tools::fs::read::run(root.path(), serde_json::json!({"path":"中文 space.txt"}))
            .await
            .unwrap();
    assert!(read.content.contains("    1\t第一行\n"));
    std::fs::write(&file, vec![b'a'; 256 * 1024]).unwrap();
    assert_eq!(
        openpaw_tools::fs::read::read_text_bounded(root.path(), "中文 space.txt", 256 * 1024)
            .await
            .unwrap()
            .len(),
        256 * 1024
    );
    std::fs::write(&file, vec![b'a'; 256 * 1024 + 1]).unwrap();
    assert!(
        openpaw_tools::fs::read::read_text_bounded(root.path(), "中文 space.txt", 256 * 1024)
            .await
            .is_err()
    );
    std::fs::write(&file, b"binary\0text").unwrap();
    assert!(
        openpaw_tools::fs::read::read_text_bounded(root.path(), "中文 space.txt", 256 * 1024)
            .await
            .is_err()
    );
    assert!(
        openpaw_tools::fs::read::read_text_bounded(root.path(), "../escape", 256 * 1024)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn d40_precancelled_web_invocation_has_no_start_or_request() {
    let dir = tempfile::tempdir().unwrap();
    let observer = Arc::new(Observer {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let registry = ToolRegistry::new(dir.path()).with_execution_observer(observer.clone());
    for (name, args) in [
        ("web_fetch", json!({"url":"http://127.0.0.1:9/"})),
        ("web_search", json!({"query":"cancelled before start"})),
    ] {
        let token = CancellationToken::new();
        token.cancel();
        let context = ToolCallContext::new("web-run", "web-call", name, &args, token);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            registry.call_with_context(name, args, context),
        )
        .await;
        assert!(
            matches!(result, Ok(Err(Error::Cancelled))),
            "pre-cancelled network call must be Cancelled: {result:?}"
        );
    }
    assert_eq!(observer.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn d40_inflight_web_wait_is_cancelled_before_http_timeout() {
    if std::env::var("OPENPAW_D40_CHILD").as_deref() != Ok("1") {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "d40_inflight_web_wait_is_cancelled_before_http_timeout",
                "--nocapture",
            ])
            .env("OPENPAW_D40_CHILD", "1")
            .env("OPENPAW_WEB_TEST_ALLOW_LOOPBACK", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        let (started, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n")
                .await
                .unwrap();
            let _ = started.send(());
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(dir.path());
        let args = json!({"url":url});
        let cancel = CancellationToken::new();
        let context = ToolCallContext::new("run", "call", "web_fetch", &args, cancel.clone());
        let mut call =
            tokio::spawn(
                async move { registry.call_with_context("web_fetch", args, context).await },
            );
        tokio::time::timeout(std::time::Duration::from_secs(3), ready)
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut call).await;
        if result.is_err() {
            call.abort();
            let _ = call.await;
        }
        server.abort();
        let _ = server.await;
        assert!(
            matches!(result, Ok(Ok(Err(Error::Cancelled)))),
            "network wait must observe call token: {result:?}"
        );
    });
}
