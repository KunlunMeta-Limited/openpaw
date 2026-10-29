#[cfg(test)]
mod child_question_tests {
    use super::*;

    fn call(index: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"index":index,"id":format!("child-question-{name}-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
    }

    pub(super) async fn model_response(
        body: &serde_json::Value,
        questions: &tokio::sync::Mutex<Vec<serde_json::Value>>,
        gate: &tokio::sync::Notify,
    ) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let latest = messages.iter().rposition(|m| m["role"] == "user").unwrap();
        let orphan = messages[latest]["content"] == "无父许可仍提问";
        let task = messages
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("");
        let task = if orphan { "child-A-needs-answer" } else { task };
        let results = messages[if orphan { latest } else { 0 }..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let parent = body["tools"].to_string().contains("start_agent");
        if task == "child-B-delayed" && results.is_empty() {
            gate.notified().await;
        }
        let calls = if parent && task.starts_with("父自己提问") {
            match results.len() {
                0 => vec![call(
                    0,
                    "start_agent",
                    serde_json::json!({"task":if task=="父自己提问后暂停"{"child-B-delayed"}else{"child-B-independent"},"name":"独立资料员"}),
                )],
                1 => vec![call(
                    0,
                    "request_user_input",
                    serde_json::json!({"question":"请选择交付方式","options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
                )],
                _ => Vec::new(),
            }
        } else if parent {
            match results.len() {
                0 => vec![
                    call(
                        0,
                        "start_agent",
                        serde_json::json!({"task":"child-A-needs-answer","name":"需求员A"}),
                    ),
                    call(
                        1,
                        "start_agent",
                        serde_json::json!({"task":"child-B-independent","name":"资料员B"}),
                    ),
                ],
                2 => vec![call(
                    0,
                    "write_file",
                    serde_json::json!({"path":"C-independent.txt","content":"父独立工作已完成"}),
                )],
                3 => {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if questions
                                .lock()
                                .await
                                .iter()
                                .any(|m| m["type"] == "decision.request")
                            {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("子A应已进入真实待答");
                    let reference: serde_json::Value =
                        serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
                    vec![call(
                        0,
                        "get_agent_status",
                        serde_json::json!({"session_id":reference["session_id"],"turn_id":reference["turn_id"]}),
                    )]
                }
                _ => Vec::new(),
            }
        } else if task == "child-A-needs-answer" {
            match results.len() {
                0 => vec![call(
                    0,
                    "request_user_input",
                    serde_json::json!({"question":"请选择交付方式","options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
                )],
                1 => vec![call(
                    0,
                    "write_file",
                    serde_json::json!({"path":"A-dependent.txt","content":"取得明确答案后才执行"}),
                )],
                _ => Vec::new(),
            }
        } else if results.is_empty() {
            vec![call(
                0,
                "write_file",
                serde_json::json!({"path":"B-independent.txt","content":"兄弟独立工作已完成"}),
            )]
        } else {
            Vec::new()
        };
        let delta = if calls.is_empty() {
            serde_json::json!({"content":if parent {
                if task=="子问题失败如实暂停" {
                    if messages.iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("status=paused"))) {"A已暂停，B与C已完成"} else {"错误地认为A已完成"}
                } else if task.starts_with("父自己提问") && messages.iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("B实际完成"))) {"父按答案继续并汇总B"} else if messages.iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("A实际完成"))) {"协作完成"} else {"尝试提前汇总"}
            } else if task=="child-A-needs-answer" {"A实际完成"}else{"B实际完成"}})
        } else {
            serde_json::json!({"tool_calls":calls})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if delta["tool_calls"].is_array(){"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    #[tokio::test]
    async fn main_child_question_allows_sibling_and_parent_progress_with_owned_waiting_status() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-progress", "held-answer").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("A提问时让B和父独立工作继续"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if fixture.workspace.path().join("B-independent.txt").exists()
                    && fixture.workspace.path().join("C-independent.txt").exists()
                    && fixture.model_seen.lock().await.iter().any(|body| {
                        body["messages"].as_array().unwrap().iter().any(|m| {
                            m["role"] == "tool"
                                && m["content"]
                                    .as_str()
                                    .is_some_and(|s| s.contains("waiting_input"))
                        })
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A待答时B和父独立C应取得进展，父能查询真实waiting_input");
        assert!(
            !fixture.workspace.path().join("A-dependent.txt").exists(),
            "答案未提交前依赖工作不能提前执行"
        );
        assert!(!stream.is_finished(), "父不能因普通汇总而取消有效待答孩子");
        let card = fixture
            .seen
            .lock()
            .await
            .iter()
            .find(|m| m["type"] == "decision.request")
            .unwrap()
            .clone();
        assert_eq!(card["conversationId"], "fixture");
        assert!(
            !card["decision"]["actorId"].as_str().unwrap().is_empty(),
            "子问题应有真实展示人物"
        );
        assert!(
            card["decision"]["description"]
                .as_str()
                .unwrap()
                .contains("需求员A"),
            "用户应识别真实提问子成员"
        );
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("协作完成"));
        assert!(fixture.workspace.path().join("A-dependent.txt").exists());
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["title"] == "需求员A")
            .unwrap();
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{}/message", child["id"].as_str().unwrap()),
            serde_json::json!({}),
        )
        .await;
        assert!(
            history.to_string().contains("option-a"),
            "答案只交回真实A的原调用"
        );
    }

    #[tokio::test]
    async fn main_child_question_without_answer_is_paused_instead_of_completed() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-paused", "timeout").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("子问题失败如实暂停"))
            .await
            .unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(8),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            text.contains("A已暂停，B与C已完成"),
            "父必须区分未取得答案的暂停与成功：{text}"
        );
        assert!(!fixture.workspace.path().join("A-dependent.txt").exists());
        assert!(fixture.workspace.path().join("B-independent.txt").exists());
        assert!(fixture.workspace.path().join("C-independent.txt").exists());
        let bodies = fixture.model_seen.lock().await;
        assert!(
            bodies.iter().any(|body| body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("status=paused") && s.contains("超时")))),
            "父模型应收到本次子执行的真实暂停原因"
        );
    }

    #[tokio::test]
    async fn main_parent_question_pause_keeps_started_independent_child_owned_until_finish() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-parent-pause", "timeout").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("父自己提问后暂停"))
            .await
            .unwrap();
        let mut stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let question = fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|m| m["type"] == "decision.request");
                let child = fixture.model_seen.lock().await.iter().any(|b| {
                    b["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "user" && m["content"] == "child-B-delayed")
                });
                if question && child {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("父真实提问和独立子模型均已到达外部门控");
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut stream)
                .await
                .is_err(),
            "父自己的受控暂停不能误取消已启动独立孩子"
        );
        fixture.release_model.notify_one();
        let bytes = tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("执行已暂停"));
        assert!(
            std::str::from_utf8(&bytes).unwrap().contains("B实际完成")
                && std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("status=completed"),
            "用户应看见独立孩子的实际完成汇总"
        );
        assert!(
            fixture.workspace.path().join("B-independent.txt").exists(),
            "独立子实际工具必须执行成功"
        );
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let parent = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| !s["parentID"].is_string())
            .unwrap();
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{}/message", parent["id"].as_str().unwrap()),
            serde_json::json!({}),
        )
        .await;
        assert!(
            history.to_string().contains("B实际完成"),
            "父暂停也要保留实际收齐的子结果事实"
        );
        assert_eq!(
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .filter(|body| body["tools"].to_string().contains("start_agent"))
                .count(),
            2,
            "父暂停收尾不能续跑依赖模型"
        );
    }

    #[tokio::test]
    async fn main_child_question_cannot_reuse_display_ownership_after_parent_turn_ends() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-ownership", "option-a").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("先完成本轮协作"))
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["title"] == "需求员A")
            .unwrap();
        let path = format!("/session/{}/message", child["id"].as_str().unwrap());
        child_test_json(
            &fixture.app,
            "POST",
            &path,
            serde_json::json!({"text":"无父许可仍提问"}),
        )
        .await;
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1,
            "旧子会话关系不能为新的未登记执行取得父会话提问许可"
        );
    }

    #[tokio::test]
    async fn main_parent_question_waits_while_started_independent_child_makes_progress() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-parent-wait", "held-answer").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("父自己提问时独立孩子继续"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.workspace.path().join("B-independent.txt").exists()
                    && fixture
                        .seen
                        .lock()
                        .await
                        .iter()
                        .any(|m| m["type"] == "decision.request")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("父待答期间独立孩子仍须实际工作");
        assert!(!stream.is_finished(), "父须等明确回答");
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("父按答案继续并汇总B"));
    }

    fn conversation_chat(team: &str, conversation: &str) -> Request<Body> {
        Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":conversation,"turnId":"one","messages":[{"role":"user","content":format!("独立协作会话{conversation}")}]}).to_string())).unwrap()
    }

    #[tokio::test]
    async fn main_child_question_stop_closes_owned_tree_and_rejects_late_answer_only() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-child-stop", "held-answer").await;
        let a = fixture
            .app
            .clone()
            .oneshot(conversation_chat(&fixture.team, "a"))
            .await
            .unwrap();
        let b = fixture
            .app
            .clone()
            .oneshot(conversation_chat(&fixture.team, "b"))
            .await
            .unwrap();
        let a = tokio::spawn(async move { to_bytes(a.into_body(), 2_000_000).await.unwrap() });
        let b = tokio::spawn(async move { to_bytes(b.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request")
                    .count()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("两个真实主会话应各有自己的子问题");
        let response=fixture.app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"cancel","teamId":fixture.team,"conversationId":"a","turnId":"one"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let a = tokio::time::timeout(Duration::from_secs(5), a)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&a)
            .unwrap()
            .contains("\"state\":\"stopped\""));
        assert!(!b.is_finished(), "停止a不影响b的有效待答");
        fixture.release_model.notify_waiters();
        let b = tokio::time::timeout(Duration::from_secs(8), b)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&b).unwrap().contains("协作完成"));
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let mut stopped_parent = None;
        for session in sessions
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| !s["parentID"].is_string())
        {
            let history = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{}/message", session["id"].as_str().unwrap()),
                serde_json::json!({}),
            )
            .await;
            if history.to_string().contains("独立协作会话a") {
                stopped_parent = Some(session["id"].clone());
            }
        }
        let stopped_parent = stopped_parent.unwrap();
        let stopped_child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"] == stopped_parent && s["title"] == "需求员A")
            .unwrap();
        let child_id = stopped_child["id"].as_str().unwrap();
        let turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(turns[0]["state"], "cancelled", "迟到回答不能复活子执行");
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(
            !history.to_string().contains("A-dependent.txt"),
            "停止孩子不能取得答案后再执行依赖工具"
        );
    }
}

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

#[cfg(test)]
mod async_delegation_tests {
    use super::*;

    pub(super) async fn model_response(
        body: &serde_json::Value,
        gate: &tokio::sync::Notify,
    ) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let first_user = messages
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("");
        let terminal_case = matches!(
            first_user,
            "父预算耗尽" | "父最后预算结束" | "父模型截断" | "父模型失败"
        );
        let parent = body["tools"].to_string().contains("delegate_task") || terminal_case;
        let tools = messages.iter().filter(|m| m["role"] == "tool").count();
        if !parent && first_user == "失败子任务" {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if terminal_case && tools > 0 {
            if first_user == "父模型失败" {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let chunk = if (first_user == "父预算耗尽"
                || (first_user == "父最后预算结束" && tools < 25))
                && !body["tools"].as_array().unwrap().is_empty()
            {
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("budget-{tools}"),"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]},"finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"delta":{"content":"本轮未完成，保留实际进度"},"finish_reason":if first_user == "父模型截断" {"length"} else {"stop"}}]})
            };
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        if parent
            && tools == 0
            && messages
                .iter()
                .any(|m| m["content"] == "满额后仍可独立工作")
        {
            let calls = (0..5).map(|index|serde_json::json!({"index":index,"id":format!("capacity-async-{index}"),"type":"function","function":{"name":"start_agent","arguments":serde_json::json!({"task":format!("capacity-child-{index}")}).to_string()}})).collect::<Vec<_>>();
            let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        let capacity = messages
            .iter()
            .any(|m| m["content"] == "满额后仍可独立工作");
        if parent
            && messages
                .iter()
                .any(|m| m["content"] == "显式等待与结果去重")
        {
            let reference = messages
                .iter()
                .find(|m| m["role"] == "tool")
                .and_then(|m| {
                    serde_json::from_str::<serde_json::Value>(m["content"].as_str()?).ok()
                })
                .unwrap_or_default();
            let args = serde_json::json!({"session_id":reference["session_id"],"turn_id":if tools == 1 {serde_json::json!("wrong-turn")} else {reference["turn_id"].clone()},"timeout_ms":if tools == 2 {1000} else {60000}});
            let call = match tools {
                0 => Some(("start_agent", serde_json::json!({"task":"独立检索资料"}))),
                1 | 5 => Some(("get_agent_status", args)),
                2 | 4 => Some(("wait_agent", args)),
                3 => Some(("read_file", serde_json::json!({"path":"README.md"}))),
                _ => None,
            };
            let chunk = match call {
                Some((name, args)) => {
                    serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("explicit-{tools}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
                }
                None => {
                    serde_json::json!({"choices":[{"delta":{"content":"explicit parent integrated"},"finish_reason":"stop"}]})
                }
            };
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        let chunk = if !parent {
            gate.notified().await;
            serde_json::json!({"choices":[{"delta":{"content":"async-child-result"},"finish_reason":"stop"}]})
        } else if tools < if capacity { 6 } else { 2 } {
            let (name, args) = if tools == 0 {
                (
                    "start_agent",
                    serde_json::json!({"task":if first_user == "子失败后如实汇总" {"失败子任务"} else {"独立检索资料"},"name":"检索员"}),
                )
            } else {
                ("read_file", serde_json::json!({"path":"README.md"}))
            };
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("async-{tools}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
        } else {
            let integrated = messages.iter().any(|m| {
                m["role"] == "user"
                    && m["content"].as_str().is_some_and(|s| {
                        s.contains("async-child-result") || s.contains("status=failed")
                    })
            });
            serde_json::json!({"choices":[{"delta":{"content":if integrated {"async parent integrated"} else {"premature parent summary"}},"finish_reason":"stop"}]})
        };
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    #[tokio::test]
    async fn main_async_dispatch_allows_independent_work_and_waits_before_finalizing() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations-async", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("独立工作与子任务最后汇总"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 500_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|body| {
                    body["tools"].to_string().contains("delegate_task")
                        && body["messages"].as_array().unwrap().iter().any(|m| {
                            m["role"] == "tool"
                                && m["content"]
                                    .as_str()
                                    .is_some_and(|s| s.contains("actual tool content"))
                        })
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("父模型在子执行未完成时应已收到独立读取结果");
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"].is_string())
            .expect("start_agent应真实创建子会话");
        let child_id = child["id"].as_str().unwrap();
        let parent_id = child["parentID"].as_str().unwrap();
        let turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(turns[0]["state"], "running");
        assert!(!stream.is_finished(), "父正常EndTurn必须等待活动子执行");
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("async parent integrated"));
        let turns_after = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(turns_after[0]["turnID"], turns[0]["turnID"]);
        assert_eq!(turns_after[0]["state"], "completed");
        let history = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{parent_id}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(
            history.to_string().contains("async-child-result"),
            "真实子结果应可靠保存到父历史"
        );
    }

    #[tokio::test]
    async fn main_explicit_wait_checks_exact_identity_times_out_without_cancelling_and_deduplicates(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations-async", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("显式等待与结果去重"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 500_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                if requests.iter().any(|body| {
                    body["messages"].as_array().unwrap().iter().any(|m| {
                        m["role"] == "tool"
                            && m["content"]
                                .as_str()
                                .is_some_and(|s| s.contains("actual tool content"))
                    })
                }) {
                    break;
                }
                drop(requests);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("有界等待超时后父仍应能处理独立工作");
        let requests = fixture.model_seen.lock().await;
        let parent = requests
            .iter()
            .rev()
            .find(|body| body["tools"].to_string().contains("delegate_task"))
            .unwrap();
        let results = parent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        assert!(results[1]["content"]
            .as_str()
            .unwrap()
            .contains("执行身份不属于当前父回合"));
        assert!(results[2]["content"]
            .as_str()
            .unwrap()
            .contains("\"status\":\"running\""));
        let reference: serde_json::Value =
            serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
        drop(requests);
        let child_id = reference["session_id"].as_str().unwrap();
        let before = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(before.as_array().unwrap().len(), 1);
        assert_eq!(before[0]["state"], "running");
        fixture.release_model.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        let requests = fixture.model_seen.lock().await;
        let parent = requests
            .iter()
            .rev()
            .find(|body| body["tools"].to_string().contains("delegate_task"))
            .unwrap();
        let results = parent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|m| m["content"]
                    .as_str()
                    .unwrap()
                    .contains("async-child-result"))
                .count(),
            1
        );
        let repeated: serde_json::Value =
            serde_json::from_str(results[5]["content"].as_str().unwrap()).unwrap();
        assert_eq!(repeated["status"], "completed", "重复领取仍应显示真实终态");
        assert_eq!(repeated["result_delivered"], true);
        let after = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{child_id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            after.as_array().unwrap().len(),
            1,
            "查询和等待不能创建新子回合"
        );
        assert_eq!(after[0]["turnID"], before[0]["turnID"]);
    }

    #[tokio::test]
    async fn main_full_child_capacity_queues_without_holding_parent_tools_or_releasing_slots() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations-async-capacity", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("满额后仍可独立工作"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 500_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|body| {
                    body["messages"].as_array().unwrap().iter().any(|m| {
                        m["role"] == "tool"
                            && m["content"]
                                .as_str()
                                .is_some_and(|s| s.contains("actual tool content"))
                    })
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("四个子执行占满名额后，第五个应排队受理，父仍能读取文件");
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let children = sessions
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["parentID"].is_string())
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 5);
        let mut running = 0;
        let mut queued = 0;
        for child in children {
            let turns = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{}/turns", child["id"].as_str().unwrap()),
                serde_json::json!({}),
            )
            .await;
            running += usize::from(turns[0]["state"] == "running");
            queued += usize::from(turns[0]["state"] == "queued");
        }
        assert_eq!((running, queued), (4, 1), "名额必须由真实子执行持有");
        assert!(!stream.is_finished());
        fixture.release_model.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .filter(|body| !body["tools"].to_string().contains("delegate_task"))
                    .count()
                    == 5
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("已完成孩子释放实际名额后，排队子执行应到达模型fixture");
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("async parent integrated"));
    }

    #[tokio::test]
    async fn main_parent_terminal_paths_cancel_owned_children_without_waiting_for_results() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for input in ["父预算耗尽", "父最后预算结束", "父模型截断", "父模型失败"]
        {
            let fixture = main_fixture("delegate-conversations-async", "confirm").await;
            let response = fixture
                .app
                .clone()
                .oneshot(fixture.chat(input))
                .await
                .unwrap();
            let bytes = tokio::time::timeout(
                Duration::from_secs(5),
                to_bytes(response.into_body(), 500_000),
            )
            .await
            .expect("终止路径不得进入普通EndTurn子结果等待")
            .unwrap();
            assert!(!std::str::from_utf8(&bytes)
                .unwrap()
                .contains("async parent integrated"));
            let sessions =
                child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
            let child = sessions
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["parentID"].is_string())
                .unwrap();
            let turns = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{}/turns", child["id"].as_str().unwrap()),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(
                turns[0]["state"], "cancelled",
                "{input}应清理所属实际子执行"
            );
        }
    }

    #[tokio::test]
    async fn main_async_child_failure_is_delivered_for_parent_summary() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations-async", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("子失败后如实汇总"))
            .await
            .unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 500_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("async parent integrated"));
        let requests = fixture.model_seen.lock().await;
        let parent = requests
            .iter()
            .rev()
            .find(|body| body["tools"].to_string().contains("delegate_task"))
            .unwrap();
        assert!(parent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("status=failed"))));
    }

    #[tokio::test]
    async fn main_stop_cancels_event_wait_and_child_in_the_original_turn() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("delegate-conversations-async", "confirm").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("独立工作与子任务最后汇总"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 500_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|body| {
                    body["tools"].to_string().contains("delegate_task")
                        && body["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|m| m["role"] == "tool")
                            .count()
                            == 2
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"].is_string())
            .unwrap();
        let parent = child["parentID"].as_str().unwrap();
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{parent}/abort"),
            serde_json::json!({}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), stream)
            .await
            .expect("停止应取消事件等待")
            .unwrap();
        for id in [parent, child["id"].as_str().unwrap()] {
            let turns = child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{id}/turns"),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(turns[0]["state"], "cancelled");
        }
    }
}
#[cfg(test)]
mod question_queue_tests {
    use super::*;

    pub(super) struct Clock(tokio::sync::watch::Sender<i64>);
    impl Default for Clock {
        fn default() -> Self {
            Self(tokio::sync::watch::channel(1000).0)
        }
    }
    impl Clock {
        pub(super) fn advance(&self, ms: i64) {
            self.0.send_modify(|now| *now += ms);
        }
    }
    #[async_trait::async_trait]
    impl openpaw_engine::user_input::QuestionClock for Clock {
        fn now_ms(&self) -> i64 {
            *self.0.borrow()
        }
        async fn wait_until(&self, deadline_ms: i64) {
            let mut receiver = self.0.subscribe();
            loop {
                if *receiver.borrow_and_update() >= deadline_ms {
                    return;
                }
                receiver.changed().await.unwrap();
            }
        }
    }

    fn call(index: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"index":index,"id":format!("queue-{name}-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
    }

    pub(super) async fn model_response(
        body: &serde_json::Value,
        questions: &tokio::sync::Mutex<Vec<serde_json::Value>>,
        gate: &tokio::sync::Notify,
    ) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let parent = body["tools"].to_string().contains("start_agent");
        let results = messages.iter().filter(|m| m["role"] == "tool").count();
        let task = messages.iter().find(|m| m["role"] == "user").unwrap()["content"]
            .as_str()
            .unwrap();
        let question = |title: &str| {
            call(
                0,
                "request_user_input",
                serde_json::json!({"question":title,"options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
            )
        };
        if task == "迟到的独立孩子" && results == 0 {
            gate.notified().await;
        }
        if task == "审批写入" && results == 0 {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if questions.lock().await.iter().any(|m| {
                        m["type"] == "decision.request" && m["decision"]["kind"] == "choice"
                    }) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        let calls = if parent && task == "先写入再提问" {
            let continuation = messages
                .iter()
                .rposition(|m| m["role"] == "user" && m["content"] == "明确继续");
            let current = continuation.map_or(results, |index| {
                messages[index..]
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .count()
            });
            if !messages
                .iter()
                .any(|m| m["role"] == "tool" && m["tool_call_id"] == "queue-bash-0")
            {
                vec![call(
                    0,
                    "bash",
                    serde_json::json!({"command":"printf 'done\\n' >> completed-once.txt"}),
                )]
            } else if current == usize::from(continuation.is_none()) {
                vec![question("继续前请选择交付方式")]
            } else if continuation.is_some() && current == 1 {
                vec![call(
                    0,
                    "write_file",
                    serde_json::json!({"path":"after-answer.txt","content":"明确继续并作答后完成"}),
                )]
            } else {
                Vec::new()
            }
        } else if parent && task == "父子都提问" {
            if results == 0 {
                vec![call(
                    0,
                    "start_agent",
                    serde_json::json!({"task":"问题A","name":"子提问员"}),
                )]
            } else if results == 1 {
                vec![question("父的问题")]
            } else {
                Vec::new()
            }
        } else if parent && task == "父提问到期而孩子继续" {
            if results == 0 {
                vec![call(
                    0,
                    "start_agent",
                    serde_json::json!({"task":"迟到的独立孩子","name":"独立资料员"}),
                )]
            } else if results == 1 {
                vec![question("父的问题")]
            } else {
                Vec::new()
            }
        } else if task == "迟到的独立孩子" && results == 0 {
            vec![call(
                0,
                "write_file",
                serde_json::json!({"path":"late-independent.txt","content":"父问题暂停后独立孩子仍完成"}),
            )]
        } else if results == 0 && parent {
            vec![
                call(
                    0,
                    "start_agent",
                    serde_json::json!({"task":"问题A","name":"提问员A"}),
                ),
                call(
                    1,
                    "start_agent",
                    serde_json::json!({"task":if task=="审批与提问"{"审批写入"}else{"问题B"},"name":"提问员B"}),
                ),
            ]
        } else if results == 0 && task == "审批写入" {
            vec![call(
                0,
                "write_file",
                serde_json::json!({"path":"approved.txt","content":"仅原审批允许后写入"}),
            )]
        } else if results == 0 {
            vec![call(
                0,
                "request_user_input",
                serde_json::json!({"question":task,"options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
            )]
        } else {
            Vec::new()
        };
        let delta = if calls.is_empty() {
            serde_json::json!({"content":if parent {if messages.iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("status=paused"))){"一个分支已暂停，仍待补充答案"}else{"父汇总两个真实回答"}}else{"已按本题答案完成"}})
        } else {
            serde_json::json!({"tool_calls":calls})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if delta["tool_calls"].is_array(){"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    fn records(fixture: &MainFixture) -> Vec<serde_json::Value> {
        std::fs::read_dir(fixture.home.path().join("sessions"))
            .unwrap()
            .flat_map(|file| {
                std::fs::read_to_string(file.unwrap().path())
                    .unwrap()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter(|event| event["type"] == "user_question_v1")
                    .map(|event| event["record"].clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn main_question_queue_presents_one_question_then_advances_to_the_next() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-two", "held-answer").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("两位子员工分别提问后汇总"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if records(&fixture)
                    .iter()
                    .filter(|r| r["state"] == "queued")
                    .count()
                    == 2
                    && fixture
                        .seen
                        .lock()
                        .await
                        .iter()
                        .any(|m| m["type"] == "decision.request")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("两个子问题已经可靠受理且首题开始展示");
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|r| r["state"] == "waiting")
                .count(),
            1,
            "排队问题不应提前开始作答期限"
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1,
            "同主会话一次只能投递一个长展示请求"
        );
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request")
                    .count()
                    == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("回答首题后才晋升后题");
        fixture.release_model.notify_one();
        let bytes = tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("父汇总两个真实回答"));
        let answered = records(&fixture)
            .into_iter()
            .filter(|r| r["state"] == "answered")
            .collect::<Vec<_>>();
        assert_eq!(answered.len(), 2);
        assert_ne!(
            answered[0]["scope"]["asking_session_id"],
            answered[1]["scope"]["asking_session_id"]
        );
    }

    #[tokio::test]
    async fn main_question_queue_grants_full_window_at_promotion_and_expires_without_default_answer(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-clock-expiry", "held-answer").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("两位子员工分别提问后汇总"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if records(&fixture)
                    .iter()
                    .filter(|r| r["state"] == "queued")
                    .count()
                    == 2
                    && fixture
                        .seen
                        .lock()
                        .await
                        .iter()
                        .any(|m| m["type"] == "decision.request")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            records(&fixture)
                .iter()
                .find(|r| r["state"] == "waiting")
                .unwrap()["deadline_ms"],
            301000,
            "首题在可信时间1000开始五分钟"
        );
        fixture.question_clock.as_ref().unwrap().advance(240000);
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if records(&fixture)
                    .iter()
                    .filter(|r| r["state"] == "waiting")
                    .count()
                    == 2
                    && fixture
                        .seen
                        .lock()
                        .await
                        .iter()
                        .filter(|m| m["type"] == "decision.request")
                        .count()
                        == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let waiting = records(&fixture)
            .into_iter()
            .filter(|r| r["state"] == "waiting")
            .collect::<Vec<_>>();
        assert!(
            waiting.iter().any(|r| r["deadline_ms"] == 541000),
            "后题在晋升时获得完整窗口，不扣排队四分钟"
        );
        fixture.question_clock.as_ref().unwrap().advance(300000);
        let bytes = tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .expect("可推进期限应使未答分支暂停并完成父汇总")
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("仍待补充答案"));
        let saved = records(&fixture);
        assert_eq!(saved.iter().filter(|r| r["state"] == "answered").count(), 1);
        assert_eq!(saved.iter().filter(|r| r["state"] == "expired").count(), 1);
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            2,
            "到期后不重复弹题或替用户选择"
        );
    }

    #[tokio::test]
    async fn main_question_queue_coordinates_permission_display_without_turning_answers_into_approval(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-approval", "held-answer").await;
        let response = fixture
            .app
            .clone()
            .oneshot(fixture.chat("审批与提问"))
            .await
            .unwrap();
        let stream =
            tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let permissions =
                    child_test_json(&fixture.app, "GET", "/permission", serde_json::json!({}))
                        .await;
                if permissions
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["permission"] == "write_file")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("真实写入审批已受理但尚未回答");
        let premature = tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if fixture.seen.lock().await.iter().any(|m| {
                    m["type"] == "decision.request"
                        && m["decision"]["title"]
                            .as_str()
                            .unwrap_or("")
                            .contains("write_file")
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(premature.is_err(), "原问题未结束时不得并行投递权限展示");
        assert!(!fixture.workspace.path().join("approved.txt").exists());
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.seen.lock().await.iter().any(|m| {
                    m["type"] == "decision.request"
                        && m["decision"]["title"]
                            .as_str()
                            .unwrap_or("")
                            .contains("write_file")
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("问题结束后原审批再展示");
        assert!(
            !fixture.workspace.path().join("approved.txt").exists(),
            "普通问题答案不能批准待处理写入"
        );
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("approved.txt")).unwrap(),
            "仅原审批允许后写入"
        );
    }

    async fn wait_queued(fixture: &MainFixture, count: usize) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if records(fixture)
                    .iter()
                    .filter(|r| r["state"] == "queued")
                    .count()
                    == count
                    && fixture
                        .seen
                        .lock()
                        .await
                        .iter()
                        .any(|m| m["type"] == "decision.request")
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn stream(
        fixture: &MainFixture,
        text: &str,
    ) -> tokio::task::JoinHandle<axum::body::Bytes> {
        let history=child_test_json(&fixture.app,"POST","/api/pawoffice/chat",serde_json::json!({"version":"1.0","type":"history","teamId":fixture.team,"conversationId":"fixture"})).await;
        let mut messages = history["data"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|turn| {
                [
                    serde_json::json!({"role":"user","content":turn["user"]}),
                    serde_json::json!({"role":"assistant","content":turn["assistant"]}),
                ]
            })
            .collect::<Vec<_>>();
        messages.push(serde_json::json!({"role":"user","content":text}));
        let response = fixture
            .app
            .clone()
            .oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":fixture.team,"conversationId":"fixture","turnId":if text=="明确继续"{"two"}else{"one"},"messages":messages}).to_string())).unwrap())
            .await
            .unwrap();
        if response.status() != StatusCode::OK {
            let status = response.status();
            let body = to_bytes(response.into_body(), 2_000_000).await.unwrap();
            panic!(
                "本地用例{text}接收失败{status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() })
    }
    async fn wait_presented(fixture: &MainFixture, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request")
                    .count()
                    == count
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "等待第{count}个展示超时，模型请求={:?}",
                fixture.model_seen.try_lock().ok().as_deref()
            )
        });
    }

    #[tokio::test]
    async fn main_question_queue_root_and_child_share_the_same_parent_lane() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-root-child", "held-answer").await;
        let stream = stream(&fixture, "父子都提问").await;
        wait_queued(&fixture, 2).await;
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|r| r["state"] == "waiting")
                .count(),
            1
        );
        fixture.release_model.notify_one();
        wait_presented(&fixture, 2).await;
        fixture.release_model.notify_one();
        tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|r| r["state"] == "answered")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn main_question_queue_expired_first_question_promotes_next_without_reasking() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-clock-first-expiry", "held-answer").await;
        let stream = stream(&fixture, "两个孩子分别提问").await;
        wait_queued(&fixture, 2).await;
        fixture.question_clock.as_ref().unwrap().advance(300000);
        wait_presented(&fixture, 2).await;
        assert!(records(&fixture)
            .iter()
            .any(|r| r["state"] == "waiting" && r["deadline_ms"] == 601000));
        fixture.release_model.notify_waiters();
        let bytes = tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("仍待补充答案"));
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|r| r["state"] == "expired")
                .count(),
            1
        );
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|r| r["state"] == "answered")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn main_question_queue_stop_settles_queued_and_waiting_without_late_answer_reviving() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-stop", "held-answer").await;
        let stream = stream(&fixture, "两个孩子分别提问").await;
        wait_queued(&fixture, 2).await;
        let stopped=fixture.app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"cancel","teamId":fixture.team,"conversationId":"fixture","turnId":"one"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(stopped.status(), StatusCode::OK);
        fixture.release_model.notify_waiters();
        tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        let saved = records(&fixture);
        assert_eq!(
            saved.iter().filter(|r| r["state"] == "cancelled").count(),
            2
        );
        assert_eq!(saved.iter().filter(|r| r["state"] == "answered").count(), 0);
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn main_question_queue_expiry_preserves_effects_until_explicit_user_continuation() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-clock-resume", "held-answer").await;
        let first = stream(&fixture, "先写入再提问").await;
        wait_queued(&fixture, 1).await;
        fixture.question_clock.as_ref().unwrap().advance(300000);
        let bytes = tokio::time::timeout(Duration::from_secs(8), first)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("已暂停"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("completed-once.txt")).unwrap(),
            "done\n"
        );
        assert!(!fixture.workspace.path().join("after-answer.txt").exists());
        let second = stream(&fixture, "明确继续").await;
        wait_presented(&fixture, 2).await;
        assert!(!fixture.workspace.path().join("after-answer.txt").exists());
        fixture.release_model.notify_waiters();
        tokio::time::timeout(Duration::from_secs(8), second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("completed-once.txt")).unwrap(),
            "done\n",
            "已知成功的副作用不能重做"
        );
        assert!(fixture.workspace.path().join("after-answer.txt").exists());
        let saved = records(&fixture);
        assert_eq!(saved.iter().filter(|r| r["state"] == "expired").count(), 1);
        assert_eq!(saved.iter().filter(|r| r["state"] == "answered").count(), 1);
    }

    #[tokio::test]
    async fn main_question_queue_root_expiry_still_collects_independent_child_result() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-queue-clock-root-expiry", "never-answer").await;
        let stream = stream(&fixture, "父提问到期而孩子继续").await;
        wait_queued(&fixture, 1).await;
        fixture.question_clock.as_ref().unwrap().advance(300000);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if records(&fixture).iter().any(|r| r["state"] == "expired") {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !stream.is_finished(),
            "父受控暂停仍需收齐已经启动的独立孩子"
        );
        fixture.release_model.notify_one();
        let bytes = tokio::time::timeout(Duration::from_secs(8), stream)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("已暂停"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("late-independent.txt")).unwrap(),
            "父问题暂停后独立孩子仍完成"
        );
    }
}
