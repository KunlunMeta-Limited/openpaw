//! 实际 Host 进程被 kill 后恢复子身份和未知工具结果，不以 AppState drop 代替进程中断。
#![cfg(all(feature = "engine-host", unix))]
use std::{
    io::{BufRead, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(port: u16, method: &str, path: &str, body: &str) -> serde_json::Value {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(socket,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.1 204"),
        "{response}"
    );
    let text = response.split_once("\r\n\r\n").unwrap().1;
    if text.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(text).unwrap()
    }
}
fn launch(port: u16, provider: u16, home: &std::path::Path, workspace: &std::path::Path) -> Host {
    let host = Host(
        Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args([
                "--engine-host",
                "--engine-host-port",
                &port.to_string(),
                "--provider",
                "openai",
                "--home",
            ])
            .arg(home)
            .arg("--workspace")
            .arg(workspace)
            .env("OPENAI_API_KEY", "isolated-fixture")
            .env(
                "OPENPAW_OPENAI_BASE_URL",
                format!("http://127.0.0.1:{provider}/v1"),
            )
            .env(
                "OPENPAW_OFFICE_ACTIONS_URL",
                format!("http://127.0.0.1:{provider}/actions"),
            )
            .env("OPENPAW_ENGINE_PERMISSIONS", "auto")
            .env_remove("OPENPAW_MAIN_URL")
            .env_remove("OPENCODE_SERVER_USERNAME")
            .env_remove("OPENCODE_SERVER_PASSWORD")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    host
}
#[test]
fn killed_host_keeps_child_unknown_once_without_reexecuting_tools_or_replaying_actions() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    assert!(Command::new("mkfifo")
        .arg(workspace.path().join("release"))
        .status()
        .unwrap()
        .success());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let provider = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let actions = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (seen, activity, done) = (calls.clone(), actions.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let model = line.contains("/chat/completions");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            drop(reader);
            if !model {
                activity.fetch_add(1, Ordering::AcqRel);
                let _ = write!(
                    socket,
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                );
                continue;
            }
            seen.fetch_add(1, Ordering::AcqRel);
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let messages = body["messages"].as_array().unwrap();
            let child = !body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "delegate_task");
            let prompt = messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
                .and_then(|m| m["content"].as_str())
                .unwrap_or("");
            let frame = if child && prompt == "resume original child" {
                assert!(messages
                    .iter()
                    .any(|message| message["content"] == "child interrupted work"));
                serde_json::json!({"choices":[{"delta":{"content":"resumed same child"},"finish_reason":"stop"}]})
            } else if !child
                && messages
                    .last()
                    .is_some_and(|message| message["role"] == "tool")
            {
                serde_json::json!({"choices":[{"delta":{"content":"parent integrated"},"finish_reason":"stop"}]})
            } else {
                let (id, name, args) = if child {
                    (
                        "actual-child-write",
                        "bash",
                        serde_json::json!({"command":"printf actual-side-effect > effect.txt; cat release"}),
                    )
                } else {
                    ("actual-delegate","delegate_task",serde_json::from_str(prompt).unwrap_or_else(|_|serde_json::json!({"task":"child interrupted work","name":"persistent-worker"})))
                };
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,"function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
            };
            let payload = format!("data: {frame}\n\ndata: [DONE]\n\n");
            let _=write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len());
        }
    });
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut host = launch(port, provider, home.path(), workspace.path());
    let parent = request(port, "POST", "/session", "{}")["id"]
        .as_str()
        .unwrap()
        .to_string();
    request(
        port,
        "POST",
        &format!("/session/{parent}/prompt_async"),
        r#"{"text":"delegate interrupted work"}"#,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !workspace.path().join("effect.txt").exists() {
        assert!(
            Instant::now() < deadline,
            "child actual bash side effect not reached: sessions={}, parentHistory={}, calls={}",
            request(port, "GET", "/session", ""),
            request(port, "GET", &format!("/session/{parent}/message"), ""),
            calls.load(Ordering::Acquire)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let sessions = request(port, "GET", "/session", "");
    let child = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["parentID"] == parent)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    host.0.kill().unwrap();
    let killed = host.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        killed.signal(),
        Some(9),
        "fixture must stop the Host process by SIGKILL"
    );
    let release = workspace.path().join("release");
    let release_writer = std::thread::spawn(move || {
        let _ = std::fs::write(release, b"release");
    });
    std::thread::sleep(Duration::from_millis(100));
    let count = calls.load(Ordering::Acquire);
    let action_count = actions.load(Ordering::Acquire);
    let host = launch(port, provider, home.path(), workspace.path());
    let history = request(port, "GET", &format!("/session/{child}/message"), "");
    let tool = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["parts"].as_array().unwrap())
        .find(|part| part["callID"] == "actual-child-write")
        .unwrap();
    assert_eq!(tool["state"]["disposition"], "unknown");
    assert!(tool["state"]["error"]
        .as_str()
        .unwrap()
        .contains("Host 重启"));
    let turns = request(port, "GET", &format!("/session/{child}/turns"), "");
    assert_eq!(turns[0]["state"], "failed");
    assert_eq!(turns[0]["error"]["code"], "host_restarted");
    assert_eq!(
        request(port, "GET", &format!("/session/{child}"), "")["parentID"],
        parent
    );
    let path = home.path().join("sessions").join(format!("{child}.jsonl"));
    let journal = std::fs::read(&path).unwrap();
    drop(host);
    let _host = launch(port, provider, home.path(), workspace.path());
    assert_eq!(
        request(port, "GET", &format!("/session/{child}/message"), ""),
        history
    );
    assert_eq!(std::fs::read(path).unwrap(), journal);
    assert_eq!(calls.load(Ordering::Acquire), count);
    assert_eq!(actions.load(Ordering::Acquire), action_count);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("effect.txt")).unwrap(),
        "actual-side-effect"
    );
    request(port,"POST",&format!("/session/{parent}/message"),&serde_json::json!({"text":serde_json::json!({"task":"resume original child","task_id":child}).to_string()}).to_string());
    let continued = request(port, "GET", &format!("/session/{child}/message"), "");
    assert!(continued.to_string().contains("resumed same child"));
    assert_eq!(
        request(port, "GET", "/session", "")
            .as_array()
            .unwrap()
            .iter()
            .filter(|session| session["parentID"] == parent)
            .count(),
        1
    );
    request(port, "DELETE", &format!("/session/{parent}"), "");
    assert_eq!(
        request(port, "GET", &format!("/session/{child}/message"), ""),
        continued
    );
    println!(
        "crash recovery evidence: {}",
        serde_json::json!({"parentID":parent,"childID":child,"killSignal":killed.signal(),"restarts":2,"recoveryDisposition":"unknown","providerCallsBeforeRecovery":count,"providerCallsAfterReadonlyRecovery":count,"actionsBeforeRecovery":action_count,"actionsAfterReadonlyRecovery":action_count,"sameChildContinued":true,"parentDeletePreservedChild":true,"effect":"actual-side-effect"})
    );
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    release_writer.join().unwrap();
}
