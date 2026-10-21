//! 职责：验证共用模型配置的格式、默认和秘密隔离边界。
//! 输入输出：临时.env到真实装配及断言。
//! 依赖：原ModelEnvironment和Provider，不复制解析器。
//! 禁止职责：不读取用户配置，不修改全局环境，不调用付费接口。
use openpaw_llm::model_config::ModelEnvironment;

#[test]
fn absent_default_is_optional_but_explicit_missing_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let environment = ModelEnvironment::load(dir.path(), None).unwrap();
    assert!(!environment.file().1);
    assert!(ModelEnvironment::load(dir.path(), Some(&dir.path().join("missing.env"))).is_err());
}
#[test]
fn quoted_crlf_unicode_and_export_are_parsed_without_setting_process_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("模型 配置.env");
    let name = format!("OPENPAW_TEST_FILE_ONLY_{}", std::process::id());
    assert!(std::env::var_os(&name).is_none());
    std::fs::write(
        &file,
        format!("\u{feff}# 注释\r\nexport {name}='中文 空格 # literal'\r\nOPENPAW_MODEL=fixture-model\r\nOPENPAW_TEST_CUSTOM_MODEL_KEY=custom-file-fixture\r\n"),
    )
    .unwrap();
    let environment = ModelEnvironment::load(dir.path(), Some(&file)).unwrap();
    assert_eq!(environment.value(&name), Some("中文 空格 # literal"));
    assert_eq!(environment.source(&name), ".env");
    assert!(std::env::var_os(name).is_none());
    let (_, model) = environment
        .assemble(Some("mock"), Some("explicit-model"), None, None, "echo")
        .unwrap();
    assert_eq!(model, "explicit-model");
    assert!(environment
        .assemble(
            Some("openai"),
            Some("fixture-model"),
            Some("https://example.com/v1"),
            Some("OPENPAW_TEST_CUSTOM_MODEL_KEY"),
            "mock"
        )
        .is_ok());
}
#[test]
fn malformed_duplicate_non_utf8_and_oversize_files_are_rejected_without_secret_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(".env");
    for bytes in [
        b"OPENAI_API_KEY='never-log-this-secret\n".to_vec(),
        b"KEY=one\nKEY=never-log-this-secret\n".to_vec(),
        vec![0xff],
        vec![b'a'; 1024 * 1024 + 1],
    ] {
        std::fs::write(&file, bytes).unwrap();
        let error = match ModelEnvironment::load(dir.path(), None) {
            Ok(_) => panic!("bad configuration accepted"),
            Err(e) => e.to_string(),
        };
        assert!(!error.contains("never-log-this-secret"));
    }
}
#[test]
fn explicit_empty_model_and_embedded_url_credentials_are_not_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "OPENPAW_MODEL=\n").unwrap();
    let environment = ModelEnvironment::load(dir.path(), None).unwrap();
    assert!(environment
        .assemble(Some("mock"), Some(""), None, None, "mock")
        .is_err());
    let error = environment
        .assemble(
            Some("mock"),
            Some("m"),
            Some("https://user:never-log-this-secret@example.com/v1"),
            None,
            "mock",
        )
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("never-log-this-secret"));
}
