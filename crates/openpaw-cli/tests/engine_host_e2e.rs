//! 职责：验证T3共享Host及原存储的真实行为。
//! 输入输出：隔离临时目录、确定性Provider与断言结果。
//! 依赖：原引擎、原存储及受测共享服务。
//! 禁止职责：不读取用户项目配置，不以模拟成功替代副作用断言。
#![cfg(feature = "engine-host")]
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Command, Stdio},
    time::Duration,
};
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn h11_cli_health_loopback_configuration_and_same_home_ownership() {
    let base = tempfile::tempdir().unwrap();
    let workspace = base.path().join("workspace 中文");
    std::fs::create_dir(&workspace).unwrap();
    let home = base.path().join("home");
    let config = base.path().join("host.toml");
    // JSON字符串也是TOML双引号字符串，兼容Windows盘符及反斜杠。
    let body = format!(
        "home = {}\nprovider = \"echo\"\n[roots]\nproject = {}\n[slots]\noffice-1 = \"project\"\n",
        serde_json::to_string(&home.to_string_lossy()).unwrap(),
        serde_json::to_string(&workspace.to_string_lossy()).unwrap()
    );
    std::fs::write(&config, body).unwrap();
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .arg("engine-host")
            .arg("--config")
            .arg(&config)
            .args(["--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let addr = line
        .trim()
        .strip_prefix("openpaw engine host listening on http://")
        .unwrap();
    assert!(addr.starts_with("127.0.0.1:"));
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("\"status\":\"ready\""));
    assert!(!response.contains("workspace"));
    let second = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("engine-host")
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("storage_unavailable"));
    #[cfg(unix)]
    {
        // 外部原生命令只发送TERM；清理仍由CLI自己的优雅停止入口完成。
        assert!(Command::new("kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(windows)]
    {
        child.0.kill().unwrap();
        child.0.wait().unwrap();
    }
}
#[test]
fn invalid_config_and_sidecar_conflict_do_not_start() {
    let base = tempfile::tempdir().unwrap();
    let cfg = base.path().join("bad.toml");
    std::fs::write(&cfg, "api_key = 'never-log-this-value'").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_openpaw"))
        .arg("engine-host")
        .arg("--config")
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("never-log-this-value"));
    #[cfg(feature = "sidecar")]
    {
        let out = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(["--sidecar", "engine-host", "--config"])
            .arg(&cfg)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("不能与--sidecar"));
    }
}

#[test]
fn office_configuration_enables_real_v1_chat_and_cors() {
    let base = tempfile::tempdir().unwrap();
    let workspace = base.path().join("workspace 中文");
    std::fs::create_dir(&workspace).unwrap();
    let home = base.path().join("home");
    let config = base.path().join("office.toml");
    let body=format!("home = {}\nprovider = \"echo\"\n[roots]\nproject = {}\n[slots]\noffice-1 = \"project\"\n[office]\nbase_url = \"http://127.0.0.1:9\"\napi_key_env = \"PAWOFFICE_TEST_KEY\"\nallowed_origins = [\"http://127.0.0.1:8765\"]\n",serde_json::to_string(&home.to_string_lossy()).unwrap(),serde_json::to_string(&workspace.to_string_lossy()).unwrap());
    std::fs::write(&config, body).unwrap();
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(["engine-host", "--config"])
            .arg(&config)
            .args(["--port", "0"])
            .env("OPENPAW_HOME", base.path().join("model-env-home"))
            .env("PAWOFFICE_TEST_KEY", "fixture-not-real")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = line
        .trim()
        .strip_prefix("openpaw engine host listening on http://")
        .unwrap();
    let payload = r#"{"version":"1.0","teamId":"office-1","conversationId":"binary-test","messages":[{"role":"user","content":"真实HTTP回归"}]}"#;
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(socket,"POST /office/chat HTTP/1.1\r\nHost: {address}\r\nOrigin: http://127.0.0.1:8765\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.contains("200 OK"));
    assert!(response
        .to_ascii_lowercase()
        .contains("access-control-allow-origin: http://127.0.0.1:8765"));
    assert!(response.contains("application/x-ndjson"));
    assert!(response.contains("真实HTTP回归"));
    assert!(response.contains("\"type\":\"done\""));
    #[cfg(unix)]
    {
        assert!(Command::new("kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        let until = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
