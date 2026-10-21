use openpaw_core::{ToolExecutor, ToolHostValidator};
use openpaw_tools::ToolRegistry;
use serde_json::json;

#[tokio::test]
async fn media_reads_follow_workspace_rules_and_session_cache_lifecycle() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let png = b"\x89PNG\r\n\x1a\nfixture";
    tokio::fs::write(workspace.path().join("shot.png"), png)
        .await
        .unwrap();
    tokio::fs::write(workspace.path().join("extensionless"), png)
        .await
        .unwrap();
    tokio::fs::write(outside.path().join("shot.png"), png)
        .await
        .unwrap();
    tokio::fs::write(workspace.path().join("notes.txt"), "hello\nworld")
        .await
        .unwrap();
    let host = ToolRegistry::new(workspace.path());
    host.validate_input("read_file", &json!({"path":"shot.png"}))
        .await
        .unwrap();
    for path in ["shot.png", "extensionless"] {
        let outcome = host.call("read_file", json!({"path":path})).await.unwrap();
        assert!(!outcome.is_error);
        assert_eq!(outcome.media[0].bytes().unwrap(), png);
        let id = outcome.media[0].id.clone();
        assert!(
            host.call("read_attachment", json!({"attachmentID":id}))
                .await
                .unwrap()
                .is_error
        );
        host.set_media_context(outcome.media.clone());
        let reread = host
            .call("read_attachment", json!({"attachmentID":id}))
            .await
            .unwrap();
        assert_eq!(reread.media, outcome.media);
        host.set_media_context(Vec::new());
        assert!(
            host.call("read_attachment", json!({"attachmentID":id}))
                .await
                .unwrap()
                .is_error
        );
    }
    let text = host
        .call("read_file", json!({"path":"notes.txt"}))
        .await
        .unwrap();
    assert!(text.content.contains("hello") && text.media.is_empty());
    assert!(
        host.call("read_file", json!({"path":outside.path().join("shot.png")}))
            .await
            .unwrap()
            .is_error
    );
    assert!(
        host.call("read_file", json!({"path":"../escape.png"}))
            .await
            .unwrap()
            .is_error
    );
    tokio::fs::write(workspace.path().join("bad.png"), b"not an image")
        .await
        .unwrap();
    assert!(
        host.call("read_file", json!({"path":"bad.png"}))
            .await
            .unwrap()
            .is_error
    );
}
