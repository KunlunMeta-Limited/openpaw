#[cfg(test)]
mod delegation_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn main_sync_delegation_waits_for_the_real_child_terminal() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("委派读取资料后汇总"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 500_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                if requests.iter().any(|body| {
                    body["tools"].is_array() && !body["tools"].to_string().contains("delegate_task")
                }) {
                    break;
                }
                drop(requests);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("子模型请求应已到达外部fixture并等待放行");
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["parentID"].is_string())
            .unwrap();
        let child_id = child["id"].as_str().unwrap();
        let parent_id = child["parentID"].as_str().unwrap();
        let running = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(running[0]["state"], "running");
        assert!(!stream.is_finished(), "同步委派不能将受理当作真实完成");
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("parent integrated"));
        let terminal = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(terminal[0]["turnID"], running[0]["turnID"]);
        assert_eq!(terminal[0]["state"], "completed");
        let parent_history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{parent_id}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(parent_history.to_string().contains(child_id));
        assert!(parent_history.to_string().contains("status=completed"));
    }
}
