//! 真实 Host 写入与实际 CLI 读取同一个 home。
#![cfg(feature = "engine-host")]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(port: u16, method: &str, path: &str, body: &str) -> String {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(socket, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.1 204"),
        "{response}"
    );
    response.split_once("\r\n\r\n").unwrap().1.to_string()
}

fn cli(home: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("session")
        .args(args)
        .arg("--home")
        .arg(home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn provider_connected_uses_local_configuration_and_preserves_unavailable_default() {
    for (variable, value, expected) in [
        ("OPENAI_API_KEY", "", "[\"mock\",\"echo\"]"),
        ("OPENAI_API_KEY", "   ", "[\"mock\",\"echo\"]"),
        (
            "OPENPAW_OPENAI_BASE_URL",
            "http://user:url-secret@host/v1",
            "[\"mock\",\"echo\"]",
        ),
        ("OPENPAW_OPENAI_MODEL", " ", "[\"mock\",\"echo\"]"),
        ("OPENPAW_MAX_OUTPUT_TOKENS", "0", "[]"),
        (
            "OPENPAW_OPENAI_MODEL",
            "configured-model",
            "[\"mock\",\"echo\",\"openai\"]",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut command = Command::new(env!("CARGO_BIN_EXE_openpaw"));
        command
            .args([
                "--engine-host",
                "--engine-host-port",
                &port.to_string(),
                "--provider",
                "openai",
                "--home",
            ])
            .arg(home.path())
            .env_remove("OPENCODE_SERVER_USERNAME")
            .env_remove("OPENCODE_SERVER_PASSWORD")
            .env_remove("ANTHROPIC_API_KEY")
            .env("OPENAI_API_KEY", "key-secret")
            .env("OPENPAW_OPENAI_BASE_URL", "http://127.0.0.1:1/v1")
            .env("OPENPAW_OPENAI_MODEL", "configured-model")
            .env("OPENPAW_MAX_OUTPUT_TOKENS", "73")
            .env(variable, value)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _host = Host(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let body = request(port, "GET", "/provider", "");
        assert!(
            body.contains(&format!("\"connected\":{expected}")),
            "{body}"
        );
        assert!(body.contains("\"default\":\"openai\""));
        for protocol in ["mock", "echo", "anthropic", "openai"] {
            assert!(body.contains(&format!("\"id\":\"{protocol}\"")));
        }
        assert!(!body.contains("key-secret") && !body.contains("url-secret"));
    }
}

#[test]
fn cli_reads_an_active_host_turn_without_recovering_or_cancelling_it() {
    let home = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let fixture = TcpListener::bind("127.0.0.1:0").unwrap();
    let fixture_address = fixture.local_addr().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        use std::io::BufRead;
        fixture.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            if let Ok((socket, _)) = fixture.accept() {
                break socket;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
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
            if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = n.trim().parse().unwrap();
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let body="data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"streamed response\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let _host = Host(
        Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args([
                "--engine-host",
                "--engine-host-port",
                &port.to_string(),
                "--provider",
                "openai",
                "--home",
            ])
            .arg(home.path())
            .env("OPENAI_API_KEY", "fixture-key")
            .env(
                "OPENPAW_OPENAI_BASE_URL",
                format!("http://{fixture_address}/v1"),
            )
            .env("OPENPAW_ENGINE_PERMISSIONS", "auto")
            .env_remove("OPENCODE_SERVER_USERNAME")
            .env_remove("OPENCODE_SERVER_PASSWORD")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let session = request(port, "POST", "/session", "{}");
    let id = session
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    request(
        port,
        "POST",
        &format!("/session/{id}/prompt_async"),
        "{\"text\":\"active user sentinel\"}",
    );
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let show = cli(home.path(), &["show", id]);
    assert!(show.contains("] active user sentinel"), "{show}");
    assert!(
        !show.contains("[assistant]"),
        "uncommitted reply exposed: {show}"
    );
    assert!(cli(home.path(), &["search", "sentinel"]).contains(id));
    release_tx.send(()).unwrap();
    server.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let turns = request(port, "GET", &format!("/session/{id}/turns"), "");
        assert!(
            !turns.contains("interrupted") && !turns.contains("cancelled"),
            "{turns}"
        );
        if turns.contains("completed") {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cli(home.path(), &["show", id]).contains("streamed response"));
}

#[test]
fn host_transcript_is_discovered_without_a_cli_index() {
    let home = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_openpaw"));
    command
        .args([
            "--engine-host",
            "--engine-host-port",
            &port.to_string(),
            "--provider",
            "echo",
            "--home",
        ])
        .arg(home.path())
        .arg("--workspace")
        .arg(home.path())
        .env_remove("OPENCODE_SERVER_USERNAME")
        .env_remove("OPENCODE_SERVER_PASSWORD")
        .env("OPENPAW_MAX_OUTPUT_TOKENS", "4096")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _host = Host(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "Host startup timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    let session = request(port, "POST", "/session", "{}");
    let id = session
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    request(
        port,
        "POST",
        &format!("/session/{id}/message"),
        "{\"text\":\"host zebra parity\"}",
    );
    assert!(!home.path().join("index.db").exists());
    let path = home.path().join("sessions").join(format!("{id}.jsonl"));
    let before = std::fs::read(&path).unwrap();
    assert!(cli(home.path(), &["list"]).contains(id));
    let show = cli(home.path(), &["show", id]);
    assert!(
        show.contains("user") && show.contains("assistant") && show.contains("host zebra parity"),
        "{show}"
    );
    assert!(cli(home.path(), &["search", "zebra"]).contains(id));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let home = home.path();
            scope.spawn(move || {
                for _ in 0..3 {
                    assert!(cli(home, &["search", "zebra"]).contains(id));
                }
            });
        }
    });
    let db = openpaw_store::SessionDb::open(home.path().join("index.db")).unwrap();
    assert_eq!(db.get_session_messages(id, 100).unwrap().len(), 2);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "CLI reads must not modify Host JSONL"
    );
    let data = openpaw_store::SessionReader::read(&path).unwrap();
    let user_id = data
        .events
        .iter()
        .find_map(|event| {
            if let openpaw_core::SessionEvent::Unknown(value) = event {
                if value["type"] == "turn_accept_v1" {
                    return value["message"]["uuid"].as_str().map(str::to_string);
                }
            }
            None
        })
        .unwrap();
    request(
        port,
        "POST",
        &format!("/session/{id}/revert"),
        &format!("{{\"messageID\":\"{user_id}\"}}"),
    );
    assert!(!cli(home.path(), &["show", id]).contains("] host zebra parity"));
    assert!(!cli(home.path(), &["search", "zebra"]).contains(id));
    request(port, "POST", &format!("/session/{id}/unrevert"), "{}");
    assert!(cli(home.path(), &["search", "zebra"]).contains(id));
    request(
        port,
        "POST",
        &format!("/session/{id}/revert"),
        &format!("{{\"messageID\":\"{user_id}\"}}"),
    );
    request(
        port,
        "POST",
        &format!("/session/{id}/message"),
        "{\"text\":\"replacement otter\"}",
    );
    let show = cli(home.path(), &["show", id]);
    assert!(
        show.contains("] replacement otter") && !show.contains("] host zebra parity"),
        "{show}"
    );
    assert!(!cli(home.path(), &["search", "zebra"]).contains(id));
    request(
        port,
        "PATCH",
        &format!("/session/{id}"),
        "{\"title\":\"Renamed parity session\"}",
    );
    let fork = request(port, "POST", &format!("/session/{id}/fork"), "{}");
    let fork_id = fork
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_ne!(fork_id, id);
    assert!(cli(home.path(), &["show", fork_id]).contains("replacement otter"));
    request(
        port,
        "PATCH",
        &format!("/session/{id}"),
        "{\"time\":{\"archived\":123}}",
    );
    assert!(cli(home.path(), &["show", id]).contains("archived: true"));
    assert!(cli(home.path(), &["list"]).contains("Renamed parity"));
}
