//! 功能：实际 CLI 进程共享执行事实写入，并保持成功上下文与旧读取语义。
use std::{
    io::{BufRead, Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn failed_cli_turn_preserves_real_effect_without_committing_incomplete_model_messages() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(size) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = size.trim().parse().unwrap()
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            if index == 0 {
                let frame = serde_json::json!({"choices":[{"delta":{"content":"saving CLI result","tool_calls":[{"index":0,"id":"cli-write","function":{"name":"write_file","arguments":"{\"path\":\"cli.txt\",\"content\":\"persisted effect\"}"}}]},"finish_reason":"tool_calls"}]});
                let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            } else {
                assert!(String::from_utf8_lossy(&body).contains("cli-write"));
                write!(
                    socket,
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .args([
            "chat",
            "--provider",
            "openai",
            "--permissions",
            "auto",
            "--home",
        ])
        .arg(home.path())
        .arg("--workspace")
        .arg(workspace.path())
        .env("OPENAI_API_KEY", "fixture")
        .env("OPENPAW_OPENAI_BASE_URL", format!("http://{address}/v1"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"write CLI evidence\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("cli.txt")).unwrap(),
        "persisted effect"
    );
    let path = std::fs::read_dir(home.path().join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let read = openpaw_store::SessionReader::read(path).unwrap();
    let transcript = openpaw_store::transcript::replay(&read.meta.id, &read.events).unwrap();
    assert_eq!(transcript.messages.len(), 1);
    let result = transcript
        .execution_attempts
        .iter()
        .find(|r| r.kind == "tool_result")
        .expect("CLI actual result is independently durable");
    assert_eq!(result.payload["call_id"], "cli-write");
    assert_eq!(result.payload["disposition"], "completed");
    let show = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .args(["session", "show", &read.meta.id, "--home"])
        .arg(home.path())
        .output()
        .unwrap();
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("write CLI evidence"));
}
