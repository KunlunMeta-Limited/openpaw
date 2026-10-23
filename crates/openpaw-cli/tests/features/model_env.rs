//! 职责：验证CLI及Host真实二进制使用同一.env模型配置。
//! 输入输出：隔离目录、loopback HTTP与进程退出结果。
//! 依赖：原CLI、Provider、Host及临时文件，不复制生产解析。
//! 禁止职责：不读取用户密钥、不修改Office、不发付费模型请求。
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn command(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_openpaw"));
    for name in [
        "OPENPAW_PROVIDER",
        "OPENPAW_MODEL",
        "OPENPAW_BASE_URL",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
    ] {
        c.env_remove(name);
    }
    c.env("OPENPAW_HOME", home)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    c
}
fn file(home: &Path, contents: &str) {
    std::fs::write(home.join(".env"), contents).unwrap();
}
fn run_chat(mut command: Command, home: &Path) -> std::process::Output {
    command
        .arg("--home")
        .arg(home)
        .arg("--workspace")
        .arg(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nexit\n")
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn default_env_selects_provider_and_explicit_arguments_override_it() {
    let dir = tempfile::tempdir().unwrap();
    file(
        dir.path(),
        "OPENPAW_PROVIDER=mock\nOPENPAW_MODEL=file-model\n",
    );
    let mut c = command(dir.path());
    c.arg("chat");
    let out = run_chat(c, dir.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("mock-reply-0"));
    let mut c = command(dir.path());
    c.args(["chat", "--provider", "echo", "--model", "explicit-model"])
        .env("OPENPAW_PROVIDER", "invalid-system-provider");
    let out = run_chat(c, dir.path());
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
}
#[test]
fn explicit_file_system_priority_empty_values_and_errors_are_observable() {
    let dir = tempfile::tempdir().unwrap();
    file(dir.path(), "broken='never-log-this-secret\n");
    let chosen = dir.path().join("指定 配置.env");
    std::fs::write(&chosen, "OPENPAW_PROVIDER=mock\n").unwrap();
    let mut c = command(dir.path());
    c.arg("chat").arg("--env-file").arg(&chosen);
    assert!(run_chat(c, dir.path()).status.success());
    let out = command(dir.path())
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("never-log-this-secret"));
    file(
        dir.path(),
        "OPENPAW_PROVIDER=mock\nOPENPAW_MODEL=file-model\n",
    );
    let out = command(dir.path())
        .args(["config", "show"])
        .env("OPENPAW_MODEL", "system-model")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("system-model [系统环境]"));
    let out = command(dir.path())
        .args(["config", "show"])
        .env("OPENPAW_MODEL", "")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = command(dir.path())
        .args(["chat", "--env-file"])
        .arg(dir.path().join("missing.env"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    // 与模型无关的命令不因坏.env失效。
    file(dir.path(), "bad='never-log-this-secret\n");
    assert!(command(dir.path())
        .args(["model", "list"])
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn default_without_env_remains_offline_and_config_diagnostics_hide_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = command(dir.path());
    c.arg("chat");
    assert!(run_chat(c, dir.path()).status.success());
    file(dir.path(),"OPENPAW_PROVIDER=openai\nOPENPAW_MODEL=deepseek-chat\nOPENPAW_BASE_URL=https://api.deepseek.com/v1\nOPENAI_API_KEY=never-log-this-secret\n");
    let out = command(dir.path())
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("deepseek-chat"));
    assert!(text.contains("set (value hidden) [.env]"));
    assert!(!text.contains("never-log-this-secret"));
    file(dir.path(),"OPENPAW_PROVIDER=openai\nOPENPAW_BASE_URL=https://user:never-log-this-secret@example.com/v1\nOPENAI_API_KEY=never-log-this-secret\n");
    let out = command(dir.path())
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .contains("never-log-this-secret"));
}
#[test]
fn actual_provider_http_uses_file_credentials_endpoint_and_selected_model() {
    for use_override in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no provider request");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                headers.push_str(&line);
            }
            assert!(length > 0 && length < 1024 * 1024);
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":{"content":"model-connected"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}})
            );
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            (headers, serde_json::from_slice::<Value>(&bytes).unwrap())
        });
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(),&format!("OPENPAW_PROVIDER=openai\nOPENPAW_MODEL=file-model\nOPENPAW_BASE_URL=http://{address}/v1\nOPENAI_API_KEY=file-fixture-secret\n"));
        let mut c = command(dir.path());
        c.arg("chat");
        if use_override {
            c.args(["--model", "cli-model"])
                .env("OPENPAW_MODEL", "system-model")
                .env("OPENAI_API_KEY", "system-fixture-secret");
        }
        let out = run_chat(c, dir.path());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("model-connected"));
        let (headers, request) = server.join().unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions "));
        assert!(headers.to_ascii_lowercase().contains(if use_override {
            "authorization: bearer system-fixture-secret"
        } else {
            "authorization: bearer file-fixture-secret"
        }));
        assert_eq!(
            request["model"],
            if use_override {
                "cli-model"
            } else {
                "file-model"
            }
        );
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!output.contains("fixture-secret"));
    }
}

#[cfg(feature = "engine-host")]
#[test]
fn host_loads_same_env_and_preserves_explicit_toml_and_original_loader() {
    use openpaw_engine_host::HostConfig;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for explicit in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("项目 空格");
        std::fs::create_dir(&workspace).unwrap();
        file(dir.path(),"OPENPAW_PROVIDER=openai\nOPENPAW_MODEL=deepseek-chat\nOPENPAW_BASE_URL=https://api.deepseek.com/v1\nOPENAI_API_KEY=file-fixture-secret\n");
        let config = dir.path().join("host.toml");
        std::fs::write(
            &config,
            format!(
                "home={}\n{}[roots]\nproject={}\n[slots]\noffice-1='project'\n",
                serde_json::to_string(&dir.path().join("data").to_string_lossy()).unwrap(),
                if explicit {
                    "provider='echo'\nmodel='explicit-model'\nbase_url='https://example.com/v1'\n"
                } else {
                    ""
                },
                serde_json::to_string(&workspace.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let environment =
            openpaw_llm::model_config::ModelEnvironment::load(dir.path(), None).unwrap();
        let merged = HostConfig::load_with_model_environment(&config, &environment).unwrap();
        assert_eq!(merged.provider, if explicit { "echo" } else { "openai" });
        assert_eq!(
            merged.model.as_deref(),
            Some(if explicit {
                "explicit-model"
            } else {
                "deepseek-chat"
            })
        );
        assert_eq!(
            merged.base_url.as_deref(),
            Some(if explicit {
                "https://example.com/v1"
            } else {
                "https://api.deepseek.com/v1"
            })
        );
        assert_eq!(HostConfig::load(&config).unwrap().provider, "echo");
        let mut c = command(dir.path());
        c.arg("engine-host")
            .arg("--config")
            .arg(&config)
            .args(["--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // openai没有系统密钥时也必须通过文件凭据成功装配，echo不需要密钥。
        let mut child = Child(c.spawn().unwrap());
        let stdout = child.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line).unwrap();
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(Duration::from_secs(15)).unwrap();
        let address = line
            .trim()
            .strip_prefix("openpaw engine host listening on http://")
            .unwrap();
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        socket.read_to_string(&mut body).unwrap();
        assert!(body.contains("200 OK"));
    }
}
