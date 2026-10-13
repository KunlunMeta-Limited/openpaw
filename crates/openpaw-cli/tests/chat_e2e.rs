//! E2E: `openpaw chat` — two-turn conversation with session persistence.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn chat_openai_custom_endpoint_model_and_tokens() {
    network_case("openai", "/custom/v2/", true);
}

#[test]
fn chat_anthropic_root_and_versioned_base_urls_use_environment_configuration() {
    for prefix in ["", "/", "/v1", "/v1/", "/custom/v1/"] {
        network_case("anthropic", prefix, false);
    }
}

fn network_case(provider: &'static str, prefix: &str, explicit: bool) {
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => panic!("CLI did not request the configured endpoint"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            let text = String::from_utf8_lossy(&request);
            if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")?
                            .parse()
                            .ok()
                    })
                    .unwrap();
                if body.len() >= length {
                    break;
                }
            }
        }
        let body = if provider == "anthropic" {
            concat!("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"fixture\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"fixture-model\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"custom reply\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
        } else {
            "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"custom reply\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
        };
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        String::from_utf8(request).unwrap()
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_openpaw"));
    command
        .args(["chat", "--provider", provider, "--home"])
        .arg(home.path())
        .env("OPENAI_API_KEY", "fixture-key")
        .env("ANTHROPIC_API_KEY", "fixture-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let base = format!("http://{address}{prefix}");
    if explicit {
        command
            .args([
                "--base-url",
                &base,
                "--model",
                "fixture-model",
                "--max-output-tokens",
                "73",
            ])
            .env("OPENPAW_OPENAI_MODEL", "wrong-env-model")
            .env("OPENPAW_MAX_OUTPUT_TOKENS", "42");
    } else {
        command
            .env("OPENPAW_ANTHROPIC_BASE_URL", &base)
            .env("OPENPAW_ANTHROPIC_MODEL", "fixture-model")
            .env("OPENPAW_MAX_OUTPUT_TOKENS", "73");
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nexit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("custom reply"));
    let request = server.join().unwrap();
    let expected = if provider == "anthropic" {
        if prefix.starts_with("/custom") {
            "/custom/v1/messages"
        } else {
            "/v1/messages"
        }
    } else {
        "/custom/v2/chat/completions"
    };
    assert!(
        request.starts_with(&format!("POST {expected} ")),
        "{request}"
    );
    let auth = if provider == "anthropic" {
        "x-api-key: fixture-key"
    } else {
        "authorization: bearer fixture-key"
    };
    assert!(request.to_ascii_lowercase().contains(auth));
    assert!(request.contains("\"model\":\"fixture-model\""));
    assert!(request.contains("\"max_tokens\":73"));
    let path = std::fs::read_dir(home.path().join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let transcript = openpaw_store::SessionReader::read(path).unwrap();
    assert_eq!(transcript.meta.model, "fixture-model");
}

#[test]
fn chat_two_turns_and_session_persisted() {
    let home = tempfile::tempdir().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("chat")
        .arg("--provider")
        .arg("mock")
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openpaw chat");
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(b"first question\nsecond question\nexit\n")
            .expect("write stdin");
    }
    let output = child.wait_with_output().expect("wait");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mock-reply-0") && stdout.contains("mock-reply-1"),
        "two counter replies expected, got: {stdout}"
    );

    // Session transcript exists under the given home and reparses.
    let sessions = PathBuf::from(home.path()).join("sessions");
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&sessions).expect("sessions dir") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "jsonl") {
            found = Some(p);
        }
    }
    let transcript = found.expect("session transcript file");
    let outcome = openpaw_store::SessionReader::read(&transcript).expect("parse transcript");
    assert_eq!(outcome.meta.provider, "mock");
    let message_events = outcome
        .events
        .iter()
        .filter(|e| matches!(e, openpaw_core::SessionEvent::Message(_)))
        .count();
    // 2 user + 2 assistant messages persisted.
    assert!(
        message_events >= 4,
        "expected ≥4 message events, got {message_events}"
    );
}

#[test]
fn chat_unknown_provider_fails_with_three_segment_error() {
    let home = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("chat")
        .arg("--provider")
        .arg("hologram")
        .arg("--home")
        .arg(home.path())
        .output()
        .expect("run chat");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown provider `hologram`"));
    assert!(stderr.contains("why:"));
    assert!(stderr.contains("next:"));
}
