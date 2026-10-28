#[cfg(test)]
mod user_input_tests {
    use super::*;

    pub(super) fn model_response(body: &serde_json::Value, scenario: &str) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let result = messages
            .iter()
            .rev()
            .find(|message| message["role"] == "tool");
        let count = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count();
        let question_after_write = scenario == "question-after-write" && count == 1;
        let delta = if scenario == "question-after-write" && count == 0 {
            serde_json::json!({"tool_calls":[{"index":0,"id":"write-call","type":"function","function":{"name":"write_file","arguments":serde_json::json!({"path":"completed.txt","content":"已经可靠完成的副作用"}).to_string()}}]})
        } else if let Some(result) = result.filter(|_| !question_after_write) {
            let answer: serde_json::Value =
                serde_json::from_str(result["content"].as_str().unwrap()).unwrap_or_default();
            serde_json::json!({"content":if answer["answer"]["choice_id"] == "option-a" {"已采用方案A"}else if answer["answer"]["choice_id"] == "custom" {answer["answer"]["custom_text"].as_str().unwrap_or("空自定义")}else{"未取得有效答案"}})
        } else {
            let id = if scenario == "question-long-id" {
                "a".repeat(101)
            } else {
                "option-a".into()
            };
            let description = if scenario == "question-description" {
                "x".repeat(2000)
            } else {
                String::new()
            };
            serde_json::json!({"tool_calls":[{"index":0,"id":"question-call","type":"function","function":{"name":"request_user_input","arguments":serde_json::json!({"question":"请选择交付方式","description":description,"options":[{"id":id,"label":"方案A"},{"id":"option-b","label":"方案B"}]}).to_string()}}]})
        };
        let reason = if delta["tool_calls"].is_array() {
            "tool_calls"
        } else {
            "stop"
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":reason}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    pub(super) fn decision_response(body: &serde_json::Value, choice: &str) -> Response {
        assert!(
            body["decision"]["description"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .count()
                <= 2000,
            "提问者说明不能让合法输入超出既有协议"
        );
        if choice == "timeout" {
            return (StatusCode::REQUEST_TIMEOUT,Json(serde_json::json!({"code":408,"msg":"Expired","data":{"reasonCode":"DECISION_TIMEOUT","details":{},"issues":[]}}))).into_response();
        }
        let mut data = serde_json::json!({"teamId":body["teamId"],"conversationId":body["conversationId"],"decisionId":body["decision"]["id"],"choiceId":"option-a","label":"方案A"});
        match choice {
            "custom" | "empty-custom" | "long-custom" => {
                data["choiceId"] = serde_json::json!("custom");
                data["label"] = serde_json::json!("其他");
                data["customText"] = serde_json::json!(match choice {
                    "empty-custom" => "   ".into(),
                    "long-custom" => "🙂".repeat(501),
                    _ => "  我需要Markdown报告  ".into(),
                });
            }
            "mixed" => data["customText"] = serde_json::json!("混合回答"),
            "wrong-choice" => data["choiceId"] = serde_json::json!("unknown"),
            "wrong-label" => data["label"] = serde_json::json!("其他标签"),
            "wrong-conversation" => data["conversationId"] = serde_json::json!("different"),
            "wrong-display" => data["decisionId"] = serde_json::json!("different"),
            _ => {}
        }
        assert!(
            data["teamId"].is_string()
                && data["conversationId"].is_string()
                && data["decisionId"].is_string(),
            "local fixture must preserve original question identities: {data}"
        );
        Json(serde_json::json!({"code":200,"msg":"User selected","data":data})).into_response()
    }

    async fn send(fixture: &MainFixture) -> serde_json::Value {
        let response = fixture.app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":fixture.team,"conversationId":"question","turnId":"one","messages":[{"role":"user","content":"先问我，再按答案完成"}]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .expect("问答应真实完成")
        .unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn main_user_question_choice_returns_to_the_original_call() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-choice", "option-a").await;
        let answer = send(&fixture).await;
        assert_eq!(answer["state"], "complete", "{answer}");
        assert_eq!(answer["text"], "已采用方案A", "原调用必须收到真实答案");
        let requests = fixture.seen.lock().await;
        let question = requests
            .iter()
            .find(|message| message["type"] == "decision.request")
            .expect("既有面板收到问题");
        assert_eq!(question["decision"]["kind"], "choice");
        assert_eq!(question["timeoutMs"], 300000);
        assert_eq!(question["decision"]["options"][0]["id"], "option-a");
        let models = fixture.model_seen.lock().await;
        assert!(!models[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "clarify"));
        let result = models.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(result["tool_call_id"], "question-call");
        let result: serde_json::Value =
            serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["answer"]["choice_id"], "option-a");
        assert_eq!(result["tool_call_id"], "question-call");
        assert!(result["question_id"].is_string());
    }

    #[tokio::test]
    async fn main_user_question_rejects_option_ids_outside_the_existing_protocol() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-long-id", "option-a").await;
        let answer = send(&fixture).await;
        assert!(
            answer["text"].as_str().unwrap().contains("已暂停"),
            "{answer}"
        );
        assert!(
            !fixture
                .seen
                .lock()
                .await
                .iter()
                .any(|message| message["type"] == "decision.request"),
            "非法参数不应进入原版前端"
        );
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            1,
            "无有效答案不能接续依赖模型"
        );
    }

    #[tokio::test]
    async fn main_user_question_custom_answer_returns_trimmed_explicit_text() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-custom", "custom").await;
        let answer = send(&fixture).await;
        assert_eq!(answer["text"], "我需要Markdown报告");
        let models = fixture.model_seen.lock().await;
        let tool = models.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        let result: serde_json::Value =
            serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            result["answer"],
            serde_json::json!({"choice_id":"custom","label":"其他","custom_text":"我需要Markdown报告"})
        );
    }

    #[tokio::test]
    async fn main_user_question_keeps_asker_description_within_existing_protocol() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-description", "option-a").await;
        let answer = send(&fixture).await;
        assert_eq!(answer["text"], "已采用方案A", "{answer}");
        let records = fixture.seen.lock().await;
        let card = records
            .iter()
            .find(|message| message["type"] == "decision.request")
            .unwrap();
        assert!(card["decision"]["description"]
            .as_str()
            .unwrap()
            .starts_with("王明："));
    }

    #[tokio::test]
    async fn main_user_question_invalid_and_unowned_answers_pause_without_dependent_requests() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for choice in [
            "empty-custom",
            "long-custom",
            "mixed",
            "wrong-choice",
            "wrong-label",
            "wrong-conversation",
            "wrong-display",
            "timeout",
        ] {
            let fixture = main_fixture("question-invalid", choice).await;
            let answer = send(&fixture).await;
            assert!(
                answer["text"].as_str().unwrap().contains("已暂停"),
                "{choice}: {answer}"
            );
            assert_eq!(
                fixture.model_seen.lock().await.len(),
                1,
                "{choice}不能作为有效答案接续"
            );
        }
    }

    async fn wait_for_question(fixture: &MainFixture) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|message| message["type"] == "decision.request")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("问题应到达真实Office边界");
    }

    #[tokio::test]
    async fn main_user_question_timeout_preserves_settled_tool_facts() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-after-write", "timeout").await;
        let answer = send(&fixture).await;
        assert!(
            answer["text"].as_str().unwrap().contains("已暂停"),
            "{answer}"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("completed.txt")).unwrap(),
            "已经可靠完成的副作用"
        );
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            2,
            "超时后不调用依赖模型"
        );
        let files: Vec<_> = std::fs::read_dir(fixture.home.path().join("sessions"))
            .unwrap()
            .map(|file| file.unwrap().path())
            .collect();
        let history = std::fs::read_to_string(&files[0]).unwrap();
        assert!(history.contains("write-call") && history.contains("question-call"));
        let events: Vec<serde_json::Value> = history
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let records: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "user_question_v1")
            .collect();
        assert_eq!(records.last().unwrap()["record"]["state"], "expired");
        assert!(records.last().unwrap()["record"]["answer"].is_null());
        let messages: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "message")
            .collect();
        assert!(
            messages
                .iter()
                .any(|event| event.to_string().contains("write-call")),
            "受控暂停保存已结算模型事实"
        );
    }

    #[tokio::test]
    async fn main_user_question_answer_is_not_accepted_if_journal_is_unavailable() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-save-failure", "held-answer").await;
        let task = tokio::spawn({
            let app = fixture.app.clone();
            let team = fixture.team.clone();
            async move {
                let response=app.oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":"question","turnId":"one","messages":[{"role":"user","content":"先问我"}]}).to_string())).unwrap()).await.unwrap();
                to_bytes(response.into_body(), 2_000_000).await.unwrap()
            }
        });
        wait_for_question(&fixture).await;
        let path = std::fs::read_dir(fixture.home.path().join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained).unwrap();
        fixture.release_model.notify_one();
        let bytes = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("保存失败应安全收尾")
            .unwrap();
        std::fs::rename(&retained, &path).unwrap();
        let answer: serde_json::Value = serde_json::from_slice(
            std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .last()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(answer["state"], "error", "{answer}");
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            1,
            "保存失败不接续模型"
        );
        assert!(!std::fs::read_to_string(path)
            .unwrap()
            .contains("\"state\":\"answered\""));
    }

    async fn send_in_conversation(
        app: Router,
        team: String,
        conversation: &str,
    ) -> serde_json::Value {
        let response=app.oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":conversation,"turnId":"one","messages":[{"role":"user","content":"先问我"}]}).to_string())).unwrap()).await.unwrap();
        let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn main_user_question_stop_rejects_late_answer_and_preserves_other_conversation() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-stop", "held-answer").await;
        let a = tokio::spawn(send_in_conversation(
            fixture.app.clone(),
            fixture.team.clone(),
            "a",
        ));
        let b = tokio::spawn(send_in_conversation(
            fixture.app.clone(),
            fixture.team.clone(),
            "b",
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|message| message["type"] == "decision.request")
                    .count()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("不同主会话各自提问");
        let response=fixture.app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"cancel","teamId":fixture.team,"conversationId":"a","turnId":"one"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        fixture.release_model.notify_waiters();
        let (a, b) = tokio::time::timeout(Duration::from_secs(5), async {
            (a.await.unwrap(), b.await.unwrap())
        })
        .await
        .expect("停止一会话不能阻塞另一会话");
        assert_eq!(a["state"], "stopped", "{a}");
        assert_eq!(b["text"], "已采用方案A", "{b}");
        let mut ended = Vec::new();
        for file in std::fs::read_dir(fixture.home.path().join("sessions")).unwrap() {
            let history = std::fs::read_to_string(file.unwrap().path()).unwrap();
            let records: Vec<serde_json::Value> = history
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|event: &serde_json::Value| event["type"] == "user_question_v1")
                .collect();
            ended.push(
                records.last().unwrap()["record"]["state"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            assert!(
                records
                    .iter()
                    .filter(|record| record["record"]["state"] == "answered")
                    .count()
                    <= 1,
                "回答只可靠接受一次"
            );
        }
        ended.sort();
        assert_eq!(ended, vec!["answered", "cancelled"]);
    }
}
