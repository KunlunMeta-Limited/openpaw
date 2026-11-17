//! 职责：验证真实媒体HTTP、实际文件提交、输入转义与取消。
//! 输入输出：隔离临时工作区和本地HTTP响应到文件字节断言。
//! 依赖：原媒体工具；不读取用户图片凭据。
//! 禁止职责：不把本地HTTP夹具称为真实商业生图验收。
use openpaw_tools::media::{run, ImageConfig, ImageService};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn svg_is_real_file_escapes_labels_and_rejects_invalid_sources_and_paths() {
    let temp = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let args = json!({"path":"assets/flow.svg","kind":"diagram","title":"架构<&>","nodes":[{"id":"a","label":"<script>alert(1)</script>"},{"id":"b","label":"核查"},{"id":"c","label":"存档"}],"edges":[{"from":"a","to":"b","label":"交接"},{"from":"b","to":"c","label":"可重试"}]});
    let result = run(
        temp.path(),
        None,
        "render_visual",
        args.clone(),
        token.clone(),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let svg = std::fs::read_to_string(temp.path().join("assets/flow.svg")).unwrap();
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("&lt;script&gt;"));
    assert!(!svg.contains("<script>"));
    assert!(svg.contains("marker-end="));
    let mut bad = args;
    bad["path"] = json!("../escape.svg");
    assert!(run(temp.path(), None, "render_visual", bad, token.clone())
        .await
        .is_err());
    let mut chart = json!({"path":"assets/bars.svg","kind":"bar_chart","title":"实测","data":[{"label":"甲","value":1.5},{"label":"乙","value":3}],"source":"隔离测试实测数据"});
    run(
        temp.path(),
        None,
        "render_visual",
        chart.clone(),
        token.clone(),
    )
    .await
    .unwrap();
    assert!(std::fs::read_to_string(temp.path().join("assets/bars.svg"))
        .unwrap()
        .contains("1.5"));
    chart["source"] = json!("");
    assert!(
        run(temp.path(), None, "render_visual", chart, token.clone())
            .await
            .is_err()
    );
    token.cancel();
    assert!(matches!(run(temp.path(),None,"render_visual",json!({"path":"cancelled.svg","kind":"diagram","title":"取消","nodes":[{"id":"a","label":"a"}]}),token).await,Err(openpaw_core::Error::Cancelled)));
    assert!(!temp.path().join("cancelled.svg").exists());
}

#[tokio::test]
async fn image_http_writes_actual_bytes_and_never_saves_failed_or_fake_results() {
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = tempfile::tempdir().unwrap();
    // 仅该测试使用的非凭据变量；不读写用户模型密钥。
    let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
    for (i,(status,body,expected)) in [
        ("200 OK",json!({"data":[{"b64_json":base64::engine::general_purpose::STANDARD.encode(&png)}]}).to_string(),true),
        ("200 OK",json!({"data":[{"url":"https://example.invalid/placeholder.png"}]}).to_string(),false),
        ("500 Internal Server Error","SECRET-SERVICE-BODY".into(),false),
        ("200 OK",json!({"data":[{"b64_json":base64::engine::general_purpose::STANDARD.encode(b"not an image")}]}).to_string(),false),
    ].into_iter().enumerate() {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let server=tokio::spawn(async move {
            let (mut stream,_)=listener.accept().await.unwrap();
            let mut request=vec![0;65536];
            let n=stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("POST /v1/images/generations"));
            let response=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let config=ImageConfig {base_url:format!("http://{address}/v1"),model:"fixture-image".into(),api_key_env:"OPENPAW_MEDIA_CONTRACT_KEY".into(),response_format:None};
        let path=format!("assets/{i}.png");
        let result=run(temp.path(),Some(&ImageService::new(config,"fixture-only-not-a-real-key".into()).unwrap()),"generate_image",json!({"path":path,"prompt":"真实HTTP契约图片"}),CancellationToken::new()).await;
        server.await.unwrap();
        assert_eq!(result.is_ok(),expected);
        assert_eq!(temp.path().join(&path).exists(),expected);
        if expected {assert_eq!(std::fs::read(temp.path().join(path)).unwrap(),png);} else {assert!(!result.unwrap_err().to_string().contains("SECRET-SERVICE-BODY"));}
    }
}

#[test]
fn image_service_requires_dedicated_secret_and_redacts_debug() {
    let cfg = ImageConfig {
        base_url: "https://images.example.invalid/v1".into(),
        model: "image-only".into(),
        api_key_env: "IMAGE_KEY".into(),
        response_format: None,
    };
    assert!(ImageService::new(cfg.clone(), String::new()).is_err());
    let service = ImageService::new(cfg.clone(), "secret-do-not-log".into()).unwrap();
    assert!(!format!("{service:?}").contains("secret-do-not-log"));
    let mut bad = cfg.clone();
    bad.base_url = "http://images.example.invalid/v1".into();
    assert!(ImageService::new(bad, "fixture".into()).is_err());
    let mut bad = cfg;
    bad.base_url = "https://user:secret@images.example.invalid/v1".into();
    assert!(ImageService::new(bad, "fixture".into()).is_err());
}

#[tokio::test]
async fn image_body_wait_is_cancelled_and_never_commits_partial_output() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 65536];
        let _ = stream.read(&mut buffer).await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{\"data\":").await.unwrap();
        ready.send(()).unwrap();
        // The client must release this actual connection on cancellation.
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buffer))
                .await
                .unwrap()
                .is_err()
        )
    });
    let service = ImageService::new(
        ImageConfig {
            base_url: format!("http://{address}/v1"),
            model: "fixture".into(),
            api_key_env: "ISOLATED".into(),
            response_format: None,
        },
        "fixture-only".into(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let operation = run(
        root.path(),
        Some(&service),
        "generate_image",
        json!({"path":"outputs/incomplete.png","prompt":"取消网络等待"}),
        cancel.clone(),
    );
    tokio::pin!(operation);
    tokio::select! {r=&mut operation=>panic!("should remain pending: {r:?}"),_=started=>{}}
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(3), operation)
            .await
            .unwrap(),
        Err(openpaw_core::Error::Cancelled)
    ));
    server.await.unwrap();
    assert!(!root.path().join("outputs/incomplete.png").exists());
}
