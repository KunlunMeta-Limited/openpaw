#[cfg(test)]
mod goal_tests {
    use super::*;
    /// 功能：提供 Goal 主聊天测试的外部模型响应。
    /// 参数：body 为真实模型 HTTP 请求。
    /// 返回：OpenAI SSE 响应。
    /// 错误：非法 fixture 请求由断言暴露。
    /// 状态：不直接操作 Goal 或 session。
    /// 并发与取消：无内部共享状态。
    /// 权限：只用于测试模型边界。
    /// 关联：主聊天 HTTP 测试。
    pub(super) fn goal_model_response(body: &serde_json::Value) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let latest = messages
            .iter()
            .rposition(|message| message["role"] == "user")
            .unwrap();
        let tools: Vec<_> = messages[latest..]
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        if messages[latest]["content"] == "普通回合，请写下七。" {
            return goal_fixture_chunk(
                if tools.is_empty() {
                    Some((
                        "write_file",
                        serde_json::json!({"path":"ordinary.txt","content":"七"}),
                    ))
                } else {
                    None
                },
                tools.len(),
            );
        }
        let call = match tools.len() {
            0 => Some(("goal_read", serde_json::json!({}))),
            1 => {
                let current: serde_json::Value =
                    serde_json::from_str(tools[0]["content"].as_str().unwrap())
                        .unwrap_or(serde_json::Value::Null);
                let text = messages[latest]["content"].as_str().unwrap();
                if text.contains("作罢") {
                    return goal_fixture_chunk(
                        Some((
                            "goal_update",
                            serde_json::json!({"action":"end","goal_id":current["id"],"expected_version":current["plan_version"],"reason":"用户明确终结旧目标","user_excerpt":text}),
                        )),
                        tools.len(),
                    );
                }
                if text.contains("摘要") {
                    return goal_fixture_chunk(
                        Some((
                            "goal_update",
                            serde_json::json!({
                                "action":"set_plan","goal_id":current["id"],"expected_version":current["plan_version"],"reason":"用户要求先交付一页摘要",
                                "acceptance_criteria":["摘要与数据逐项一致"],"tasks":[
                                    {"id":"data","title":"整理数据","expected_result":"可复核的数据表","acceptance_criteria":["总数与原始资料一致"],"dependencies":[]},
                                    {"id":"report","title":"摘要报告","expected_result":"一页摘要报告","acceptance_criteria":["引用数据表且不超过一页"],"dependencies":["data"]}
                                ]
                            }),
                        )),
                        tools.len(),
                    );
                }
                Some((
                    "goal_update",
                    serde_json::json!({
                        "action":"set_plan", "goal_id":current["id"], "expected_version":current["plan_version"],
                        "reason":"按独立交付结果拆解", "acceptance_criteria":["报告与数据逐项一致"],
                        "tasks":[
                            {"id":"data","title":"整理数据","expected_result":format!("可复核的数据表：{}末项核对结束","核对资料。".repeat(220)),"acceptance_criteria":["总数与原始资料一致"],"dependencies":[]},
                            {"id":"report","title":"撰写报告","expected_result":"完整分析报告","acceptance_criteria":["引用数据表并解释结论"],"dependencies":["data"]}
                        ]
                    }),
                ))
            }
            // 故意绕过工具列表，证明确认前由 ToolHost 强制拒绝交付写入。
            2 => Some((
                "write_file",
                serde_json::json!({"path":"premature.txt","content":"must not execute"}),
            )),
            _ => None,
        };
        goal_fixture_chunk(call, tools.len())
    }

    fn goal_fixture_chunk(call: Option<(&str, serde_json::Value)>, index: usize) -> Response {
        let chunk = if let Some((name, args)) = call {
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("goal-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
        } else {
            serde_json::json!({"choices":[{"delta":{"content":"计划已整理。"},"finish_reason":"stop"}]})
        };
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    async fn goal_request(
        app: &Router,
        team: &str,
        conversation: &str,
        operation: serde_json::Value,
    ) -> Response {
        let mut body =
            serde_json::json!({"version":"1.0","teamId":team,"conversationId":conversation});
        body.as_object_mut()
            .unwrap()
            .extend(operation.as_object().unwrap().clone());
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pawoffice/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn goal_send(
        app: &Router,
        team: &str,
        conversation: &str,
        turn: &str,
        text: &str,
    ) -> serde_json::Value {
        let response = goal_request(
            app,
            team,
            conversation,
            serde_json::json!({"type":"history"}),
        )
        .await;
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        let mut messages = Vec::new();
        for row in history["data"]["turns"].as_array().unwrap() {
            messages.push(serde_json::json!({"role":"user","content":row["user"]}));
            messages.push(serde_json::json!({"role":"assistant","content":row["assistant"]}));
        }
        messages.push(serde_json::json!({"role":"user","content":text}));
        let response = goal_request(
            app,
            team,
            conversation,
            serde_json::json!({"type":"send","turnId":turn,"messages":messages}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let final_line = std::str::from_utf8(&bytes).unwrap().lines().last().unwrap();
        serde_json::from_str(final_line).unwrap()
    }

    #[tokio::test]
    async fn main_goal_draft_displays_complete_model_plan_without_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-plan", "confirm").await;
        let answer = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "draft-1",
            "/goal 分析销售并交付报告",
        )
        .await;
        assert_eq!(answer["state"], "complete");
        let text = answer["text"].as_str().unwrap();
        for required in [
            "分析销售并交付报告",
            "报告与数据逐项一致",
            "可复核的数据表",
            "总数与原始资料一致",
            "完整分析报告",
            "引用数据表并解释结论",
            "data",
            "确认执行",
            "末项核对结束",
        ] {
            assert!(
                text.contains(required),
                "完整规范计划缺少 {required}: {text}"
            );
        }
        assert!(!fixture.workspace.path().join("premature.txt").exists());
        let requests = fixture.model_seen.lock().await;
        assert!(requests.iter().any(|request| request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"].as_str().unwrap_or("").contains("确认"))));
    }

    #[tokio::test]
    async fn main_goal_draft_projects_real_tasks_and_unknown_whiteboard_progress() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-plan", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "draft-1",
            "/goal 分析销售并交付报告",
        )
        .await;
        let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(snapshot) = fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .rev()
                    .find(|message| message["snapshot"]["goal"]["planVersion"] == 1)
                    .map(|message| message["snapshot"].clone())
                {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("待确认计划应由 Gateway 收到真实任务投影");
        assert_eq!(snapshot["goal"]["phase"], "draft");
        assert_eq!(snapshot["goal"]["taskCount"], 2);
        assert_eq!(snapshot["goal"]["acceptedTaskCount"], 0);
        assert_eq!(snapshot["goal"]["progress"], -1);
        let tasks = snapshot["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["goalId"], snapshot["goal"]["id"]);
        assert_eq!(tasks[0]["progress"], -1);
        assert!(tasks[0]["summary"].as_str().unwrap().encode_utf16().count() <= 1000);
        assert!(tasks[0]["summary"]
            .as_str()
            .unwrap()
            .contains("完整条件与依赖见聊天计划"));
        assert!(tasks[1]["summary"].as_str().unwrap().contains("依赖：data"));
        assert_eq!(snapshot["whiteboard"]["progressMode"], "reported");
        assert_eq!(snapshot["whiteboard"]["steps"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["whiteboard"]["steps"][0]["progress"], -1);
    }

    #[tokio::test]
    async fn main_goal_duplicate_requires_explicit_end_without_unlocking_delivery_in_edit_turn() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-plan", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "create",
            "/goal 分析销售并交付报告",
        )
        .await;
        let calls = fixture.model_seen.lock().await.len();
        let duplicate = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "duplicate",
            "/goal 另一个目标",
        )
        .await;
        assert!(duplicate["text"]
            .as_str()
            .unwrap()
            .contains("先明确结束旧目标"));
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "重复创建不应交由模型覆盖"
        );
        let ended = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "end",
            "这份计划先作罢，请终结它，保留已讨论的安排。",
        )
        .await;
        assert!(ended["text"].as_str().unwrap().contains("未标记为验收完成"));
        assert!(
            !fixture.workspace.path().join("premature.txt").exists(),
            "结束 Goal 不应给当前编辑回合解锁交付工具"
        );
        let replacement = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "replacement",
            "/goal 另一个目标",
        )
        .await;
        assert!(replacement["text"]
            .as_str()
            .unwrap()
            .contains("目标：另一个目标"));
        let history = goal_request(
            &fixture.app,
            &fixture.team,
            "draft",
            serde_json::json!({"type":"history"}),
        )
        .await;
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(history.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        assert!(history["data"]["turns"][0]["assistant"]
            .as_str()
            .unwrap()
            .contains("分析销售并交付报告"));
    }

    #[tokio::test]
    async fn main_goal_natural_language_edit_and_restart_keep_latest_draft_and_history() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-plan", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "create",
            "/goal 分析销售并交付报告",
        )
        .await;
        let edited = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "edit",
            "把报告改成一页摘要，先给我能核对的要点。",
        )
        .await;
        assert!(edited["text"]
            .as_str()
            .unwrap()
            .contains("完整计划（版本 2）"));
        assert!(edited["text"].as_str().unwrap().contains("一页摘要报告"));
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let response = goal_request(
            &restarted,
            &fixture.team,
            "draft",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        assert!(history["data"]["turns"][1]["assistant"]
            .as_str()
            .unwrap()
            .contains("一页摘要报告"));
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "重载历史不能自动启动模型"
        );
        let next = goal_send(
            &restarted,
            &fixture.team,
            "draft",
            "edit-again",
            "保留这个摘要版本，补充它的核对要求。",
        )
        .await;
        assert!(next["text"]
            .as_str()
            .unwrap()
            .contains("完整计划（版本 3）"));
        let requests = fixture.model_seen.lock().await;
        let read = requests[calls..]
            .iter()
            .flat_map(|request| request["messages"].as_array().unwrap())
            .find(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("\"displayed_version\":2"))
            })
            .expect("恢复后模型必须读到原聊天最新已展示版本");
        let goal: serde_json::Value =
            serde_json::from_str(read["content"].as_str().unwrap()).unwrap();
        assert_eq!(goal["plan_version"], 2);
        assert!(goal["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["tasks"].as_array().is_some_and(|tasks| tasks
                .iter()
                .any(|task| task["expected_result"] == "完整分析报告"))));
        assert!(!fixture.workspace.path().join("premature.txt").exists());
    }

    #[tokio::test]
    async fn main_goal_same_workspace_conversations_keep_plan_identity_and_projection_isolated() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-isolation", "confirm").await;
        fixture
            .state
            .main_office
            .message(
                serde_json::json!({"type":"office.capabilities"}),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        goal_send(
            &fixture.app,
            &fixture.team,
            "a",
            "a-create",
            "/goal 分析销售并交付报告",
        )
        .await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "b",
            "b-create",
            "/goal 分析库存并交付报告",
        )
        .await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "a",
            "a-edit",
            "把它改成一页摘要。",
        )
        .await;
        let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let read = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap()
                    .1;
                if read["snapshot"]["conversations"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row["id"] == "a" && row["goal"]["planVersion"] == 2)
                    })
                {
                    break read["snapshot"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let conversations = snapshot["conversations"].as_array().unwrap();
        let a = conversations.iter().find(|row| row["id"] == "a").unwrap();
        let b = conversations.iter().find(|row| row["id"] == "b").unwrap();
        assert_ne!(a["goal"]["id"], b["goal"]["id"]);
        assert_eq!(b["goal"]["planVersion"], 1);
        assert_ne!(a["tasks"][0]["id"], b["tasks"][0]["id"]);
        assert_eq!(a["tasks"][0]["goalId"], a["goal"]["id"]);
        assert_eq!(b["tasks"][0]["goalId"], b["goal"]["id"]);
        assert_eq!(a["tasks"][1]["title"], "摘要报告");
        assert_eq!(b["tasks"][1]["title"], "撰写报告");
        assert_ne!(
            a["whiteboard"]["steps"][0]["id"],
            b["whiteboard"]["steps"][0]["id"]
        );
        assert!(!fixture.workspace.path().join("premature.txt").exists());
    }

    #[tokio::test]
    async fn main_goal_ended_keeps_later_ordinary_turn_tools_and_reply_unchanged() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-plan", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "create",
            "/goal 分析销售并交付报告",
        )
        .await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "end",
            "这份计划先作罢。",
        )
        .await;
        let answer = goal_send(
            &fixture.app,
            &fixture.team,
            "draft",
            "ordinary",
            "普通回合，请写下七。",
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("ordinary.txt")).unwrap(),
            "七"
        );
        assert!(
            !answer["text"].as_str().unwrap().contains("完整计划"),
            "结束后的普通回合不应自动附加旧 Goal 计划"
        );
    }
}
