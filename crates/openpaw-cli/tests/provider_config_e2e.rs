//! 配置诊断必须与实际 Provider 配置一致且不泄露密钥。
use std::process::Command;

#[test]
fn configured_models_and_safe_diagnostics() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["model", "list"],
        vec!["config", "show"],
        vec![
            "doctor",
            "--home",
            home.path().to_str().unwrap(),
            "--workspace",
            home.path().to_str().unwrap(),
        ],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(args)
            .env("OPENPAW_HOME", home.path())
            .env("OPENAI_API_KEY", "private-key-sentinel")
            .env("OPENPAW_OPENAI_MODEL", "custom-model-sentinel")
            .env("OPENPAW_OPENAI_BASE_URL", "http://127.0.0.1:1/custom/v2")
            .env("OPENPAW_MAX_OUTPUT_TOKENS", "73")
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("custom-model-sentinel"), "{text}");
        assert!(!text.contains("private-key-sentinel"));
    }
}

#[test]
fn invalid_chat_configuration_is_safe_and_exits_two() {
    for (name, value) in [
        (
            "OPENPAW_OPENAI_BASE_URL",
            "http://user:url-secret@localhost/v1",
        ),
        ("OPENAI_API_KEY", "   "),
        ("OPENPAW_OPENAI_MODEL", "  "),
        ("OPENPAW_MAX_OUTPUT_TOKENS", "0"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_openpaw"))
            .args(["chat", "--provider", "openai", "--home"])
            .arg(home.path())
            .env("OPENAI_API_KEY", "private-key-sentinel")
            .env("OPENPAW_OPENAI_MODEL", "model")
            .env("OPENPAW_OPENAI_BASE_URL", "http://127.0.0.1:1/v1")
            .env("OPENPAW_MAX_OUTPUT_TOKENS", "73")
            .env(name, value)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(text.contains("why:") && text.contains("next:"));
        assert!(!text.contains("url-secret") && !text.contains("private-key-sentinel"));
        assert!(!home.path().join("sessions").exists());
    }
}
