//! 审批决定必须控制真实工具副作用，headless 不消费下一条聊天作为审批。
use openpaw_cli::approval::{self, Answer, InputOwner};
use std::{
    io::{BufRead, Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn manual_headless_denies_tool_and_keeps_next_chat_prompt() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..3 {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => panic!("missing fixture request {index}"),
                }
            };
            // Windows 可继承 listener 的非阻塞模式；后续读写使用阻塞超时。
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
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
            requests.push(String::from_utf8(bytes).unwrap());
            let body = if index == 0 {
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"write-call\",\"type\":\"function\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"approved.txt\\\",\\\"content\\\":\\\"input-secret-sentinel\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
            } else {
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            };
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .args([
            "chat",
            "--provider",
            "openai",
            "--permissions",
            "manual",
            "--base-url",
            &format!("http://{address}/v1"),
            "--model",
            "fixture",
            "--home",
        ])
        .arg(home.path())
        .arg("--workspace")
        .arg(workspace.path())
        .env("OPENAI_API_KEY", "fixture-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"write a file\nyes\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.path().join("approved.txt").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected") && !stderr.contains("input-secret-sentinel"));
    let requests = server.join().unwrap();
    assert!(
        requests[2].contains("\"content\":\"yes\""),
        "{}",
        requests[2]
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("done")
            .count(),
        2
    );
}

#[tokio::test]
async fn terminal_decision_controls_file_side_effect_and_session_scope() {
    for (answer, allowed) in [
        ("once\n", true),
        ("session\n", true),
        ("reject\n", false),
        ("yes\n", false),
        ("\n", false),
        ("", false),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let input = InputOwner::new(std::io::Cursor::new(answer.as_bytes().to_vec()));
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        let serial = std::sync::atomic::AtomicU64::new(0);
        let gate = openpaw_engine::approval::ApprovalGate::new(
            Default::default(),
            move || {
                serial
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .to_string()
            },
            move |notice| {
                let _ = tx.send(notice);
            },
        );
        let mut tool_input =
            openpaw_core::ApprovalRequest::new("write_file", Default::default(), "write").input;
        tool_input["path"] = "approved.txt".into();
        tool_input["content"] = "safe content".into();
        let provider = openpaw_llm::MockProvider::scripted(vec![
            openpaw_llm::MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: tool_input,
                text: "write".into(),
            },
            openpaw_llm::MockMode::Fixed("done".into()),
        ]);
        let tools = openpaw_tools::arc_registry(workspace.path());
        let specs = tools.list_tools().await.unwrap();
        let agent = openpaw_engine::approval::configure(
            openpaw_engine::AgentLoop::new(Arc::new(provider)).with_tool_host(tools),
            true,
            gate.clone(),
        );
        let mut session = openpaw_core::Session::new("mock-1", "mock");
        session
            .messages
            .push(openpaw_core::Message::user_text("write a file"));
        let mut output = Vec::new();
        let mut sink = |_| {};
        let turn = agent.run_turn(&mut session, None, &specs, &mut sink);
        tokio::pin!(turn);
        loop {
            tokio::select! {
                result = &mut turn => { result.unwrap(); break; },
                Some(notice) = notices.recv() => {
                    if let openpaw_engine::approval::ApprovalNotice::Asked {id,request} = notice {
                        let decision = approval::ask(&input,&mut output,true,&request.tool_name).await;
                        assert_eq!(decision != Answer::Reject,allowed);
                        gate.reply(&id,decision != Answer::Reject,decision == Answer::Session,"decision".into());
                    }
                }
            }
        }
        assert_eq!(workspace.path().join("approved.txt").exists(), allowed);
        assert!(!String::from_utf8(output).unwrap().contains("safe content"));
    }
}
