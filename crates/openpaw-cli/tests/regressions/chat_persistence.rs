//! 职责：T1真实行为回归。
//! 输入输出：确定性输入、故障到断言及账本证据。
//! 依赖：原引擎、Provider或Store公共入口。
//! 禁止职责：不复制生产逻辑，不冒充Office或真实模型服务验收。

use openpaw_core::{ContentBlock, SessionEvent};
use openpaw_store::SessionReader;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    other => panic!("CLI未发出预期请求: {other:?}"),
                }
            };
            // Windows会继承监听socket的非阻塞模式；请求体读取必须阻塞且有界超时。
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            let end = loop {
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let length: usize = String::from_utf8_lossy(&bytes[..end])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < end + length {
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            requests.push(serde_json::from_slice(&bytes[end..end + length]).unwrap());
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (base, handle)
}
fn frame(delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":7,"completion_tokens":4}})
    )
}
fn run_chat(
    home: &std::path::Path,
    workspace: &std::path::Path,
    base: &str,
    model: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openpaw"));
    command
        .args(["chat", "--provider", "openai", "--base-url", base])
        .arg("--home")
        .arg(home)
        .arg("--workspace")
        .arg(workspace)
        .env("OPENAI_API_KEY", "fixture-key")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"read the file\nexit\n")
        .unwrap();
    child.wait_with_output().unwrap()
}
fn transcript(home: &std::path::Path) -> openpaw_store::session::ReadOutcome {
    let path = std::fs::read_dir(home.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    SessionReader::read(path).unwrap()
}

#[test]
fn actual_http_requests_use_default_and_explicit_model() {
    for explicit in [None, Some("chosen-model")] {
        let home = tempfile::tempdir().unwrap();
        let (base, server) = server(vec![frame(json!({"content":"hello"}), json!("stop"))]);
        let output = run_chat(home.path(), home.path(), &base, explicit);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["model"], explicit.unwrap_or("gpt-4o"));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("hello"),
            "真实流文本必须进入CLI和最终消息"
        );
        let out = transcript(home.path());
        assert_eq!(out.meta.model, explicit.unwrap_or("gpt-4o"));
        assert_eq!(
            out.events
                .iter()
                .filter(|e| matches!(e, SessionEvent::Usage(_)))
                .count(),
            1
        );
        assert!(
            matches!(out.events.last(), Some(SessionEvent::RunFinished { status, .. }) if status == "completed")
        );
    }
}

#[test]
fn tool_results_and_call_ids_survive_later_provider_failure() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("事实.txt"), "persist this result").unwrap();
    let tool_delta = json!({"tool_calls":[{"index":0,"id":"call-real-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"事实.txt\"}"}}]});
    // 第二次响应有文本但没有终止帧，必须失败且不能重放第一步工具。
    let (base, server) = server(vec![
        frame(tool_delta, json!("tool_calls")),
        format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"incomplete"},"finish_reason":null}]})
        ),
    ]);
    let output = run_chat(home.path(), home.path(), &base, None);
    assert!(!output.status.success());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call-real-1"));
    let out = transcript(home.path());
    assert!(out.events.iter().any(
        |e| matches!(e, SessionEvent::ToolCallStart { call_id, .. } if call_id == "call-real-1")
    ));
    assert!(out.events.iter().any(|e| matches!(e, SessionEvent::ToolCallComplete { call_id, success: true, duration_ms: Some(_), .. } if call_id == "call-real-1")));
    assert!(out.events.iter().any(|e| matches!(e, SessionEvent::Message(m) if m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "call-real-1" && content.contains("persist this result"))))));
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, SessionEvent::Message(m) if m.text() == "incomplete")));
    assert!(
        matches!(out.events.last(), Some(SessionEvent::RunFinished { status, .. }) if status == "failed")
    );
}

#[test]
fn invalid_model_and_embedded_credentials_fail_before_connecting() {
    for args in [
        vec!["--model", " "],
        vec!["--base-url", "http://user:secret@127.0.0.1:1"],
    ] {
        let home = tempfile::tempdir().unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(["chat", "--provider", "openai"])
            .args(args)
            .arg("--home")
            .arg(home.path())
            .env("OPENAI_API_KEY", "fixture-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
        assert!(!home.path().join("sessions").exists());
    }
}

#[test]
fn successful_tool_turn_persists_user_result_and_matching_ids() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("fact.txt"), "persist this result").unwrap();
    let tool_delta = json!({"tool_calls":[{"index":0,"id":"call-real-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"fact.txt\"}"}}]});
    let (base, server) = server(vec![
        frame(tool_delta, json!("tool_calls")),
        frame(json!({"content":"done"}), json!("stop")),
    ]);
    let output = run_chat(home.path(), home.path(), &base, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.join().unwrap().len(), 2);
    let out = transcript(home.path());
    assert!(out.events.iter().any(|e| matches!(e, SessionEvent::Message(m) if m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, content, .. } if tool_use_id == "call-real-1" && content.contains("persist this result"))))), "成功工具的User/tool_result不能按角色过滤");
    assert!(out.events.iter().any(
        |e| matches!(e, SessionEvent::ToolCallComplete { call_id, .. } if call_id == "call-real-1")
    ));
}
