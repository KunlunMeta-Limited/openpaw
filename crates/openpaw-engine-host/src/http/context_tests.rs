#[cfg(test)]
mod context_tests {
    use super::*;

    pub(super) fn model_response(body: &serde_json::Value) -> Response {
        let parent = body["tools"]
            .as_array()
            .is_some_and(|a| a.iter().any(|t| t["function"]["name"] == "delegate_task"));
        if parent && body.to_string().contains("DELEGATE_LOOP") {
            let done = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool");
            let delta = if done {
                serde_json::json!({"content":"子会话已完成六步。"})
            } else {
                serde_json::json!({"tool_calls":[{"index":0,"id":"delegate-loop","type":"function","function":{"name":"delegate_task","arguments":serde_json::json!({"name":"上下文测试","task":"LOOP_PROBE 严格顺序完成六步"}).to_string()}}]})
            };
            let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if done {"stop"} else {"tool_calls"}}]});
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        if body.to_string().contains("LOOP_PROBE") && !body.to_string().contains("GOAL_LOOP") {
            return loop_response(body);
        }
        if body.to_string().contains("GOAL_LOOP") {
            let latest = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|m| m["role"] == "user")
                .unwrap()["content"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            if !latest.starts_with("/goal ") {
                return goal_loop_response(body);
            }
        }
        if body["stream"] != true {
            return Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"[Context Summary] 保留Goal任务、人工阻塞、原件引用；不恢复执行。"},"finish_reason":"stop"}]})).into_response();
        }
        let messages = body["messages"].as_array().unwrap();
        let user = messages.iter().rposition(|m| m["role"] == "user").unwrap();
        let requested = messages[user]["content"]
            .as_str()
            .unwrap_or("")
            .strip_prefix("READ_ORIGINAL ")
            .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap());
        let results: Vec<_> = messages[user..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        let text = messages[user]["content"].as_str().unwrap_or("");
        let goal = body["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == "goal_read"));
        if goal && requested.is_none() {
            return goal_response(text, &results);
        }
        let call = if results.is_empty() {
            if let Some(args) = &requested {
                Some(("read_context_record", args.clone()))
            } else {
                Some(("read_file", serde_json::json!({"path":"large.txt"})))
            }
        } else if requested.is_some() || body.to_string().contains("SMALL_WINDOW_PREVIEW") {
            None
        } else {
            let last: serde_json::Value =
                serde_json::from_str(results.last().unwrap()["content"].as_str().unwrap())
                    .unwrap_or_default();
            if last["reference"].is_string()
                && (last["kind"] == "tool_result_preview" || last["has_more"] == true)
            {
                Some((
                    "read_context_record",
                    serde_json::json!({"reference":last["reference"],"start":if last["kind"] == "tool_result_preview" {0}else{last["next_start"].as_u64().unwrap()},"limit":4096}),
                ))
            } else {
                None
            }
        };
        let has_call = call.is_some();
        let delta = if let Some((name, args)) = call {
            serde_json::json!({"tool_calls":[{"index":0,"id":format!("context-{}",results.len()),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        } else {
            serde_json::json!({"content":if requested.is_some(){results.last().unwrap()["content"].as_str().unwrap()}else{"已读取原件。"}})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if has_call{"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    pub(super) fn completed_step(body: &serde_json::Value) -> usize {
        body.to_string()
            .split("STEP_DONE_")
            .skip(1)
            .filter_map(|s| {
                s.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<usize>()
                    .ok()
            })
            .max()
            .unwrap_or(0)
    }

    fn loop_response(body: &serde_json::Value) -> Response {
        let step = completed_step(body);
        if body["stream"] != true {
            return Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":format!("[Context Summary] LOOP_PROBE; STEP_DONE_{step}; 已完成步骤保留，继续下一步；不得重复工具；保持用户约束。")},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":30,"total_tokens":130}})).into_response();
        }
        let delta = if step < 6 {
            let next = step + 1;
            let command = format!("python3 loop_step.py {next}");
            serde_json::json!({"tool_calls":[{"index":0,"id":format!("loop-{next}"),"type":"function","function":{"name":"bash","arguments":serde_json::json!({"command":command}).to_string()}}]})
        } else {
            serde_json::json!({"content":"六步已完成。"})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if step<6{"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    fn goal_response(text: &str, results: &[&serde_json::Value]) -> Response {
        let current: serde_json::Value = results
            .first()
            .and_then(|r| r["content"].as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let update = |action: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"真实工具结果测试","task_id":"one"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            ("goal_update", args)
        };
        let call = if text.starts_with("/goal ") {
            match results.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => Some(update(
                    "set_plan",
                    serde_json::json!({"acceptance_criteria":["准确核对"],"tasks":[{"id":"one","title":"读取资料","expected_result":"已核对的资料","acceptance_criteria":["原件完整"],"dependencies":[]}]}),
                )),
                _ => None,
            }
        } else if text == "确认执行" {
            match results.len() {
                0 | 3 => Some(("goal_read", serde_json::json!({}))),
                1 => Some(update("start_task", serde_json::json!({}))),
                2 => Some(("read_file", serde_json::json!({"path":"large.txt"}))),
                4 => Some(update(
                    "block_task",
                    serde_json::json!({"reason":"等待用户核对"}),
                )),
                _ => None,
            }
        } else if results.is_empty() {
            Some(("goal_read", serde_json::json!({})))
        } else {
            None
        };
        let has_call = call.is_some();
        let delta = if let Some((name, args)) = call {
            serde_json::json!({"tool_calls":[{"index":0,"id":format!("context-{}",results.len()),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        } else {
            serde_json::json!({"content":"整理完毕。"})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if has_call{"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    fn goal_loop_response(body: &serde_json::Value) -> Response {
        if body["stream"] != true {
            return loop_response(body);
        }
        let messages = body["messages"].as_array().unwrap();
        let step = completed_step(body);
        let last = messages.last().unwrap();
        let state: serde_json::Value = last["content"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let call = if (step == 0 || step == 6) && state["tasks"].is_null() {
            Some(("goal_read", serde_json::json!({})))
        } else if step == 0 && state["tasks"][0]["status"] != "running" {
            Some((
                "goal_update",
                serde_json::json!({"action":"start_task","goal_id":state["id"],"expected_version":state["plan_version"],"task_id":"one","reason":"执行测试"}),
            ))
        } else if step < 6 {
            Some((
                "bash",
                serde_json::json!({"command":format!("python3 loop_step.py {}", step+1)}),
            ))
        } else if state["tasks"][0]["status"] != "blocked" {
            Some((
                "goal_update",
                serde_json::json!({"action":"block_task","goal_id":state["id"],"expected_version":state["plan_version"],"task_id":"one","reason":"等待人工核对"}),
            ))
        } else {
            None
        };
        let delta = if let Some((name, args)) = &call {
            serde_json::json!({"tool_calls":[{"index":0,"id":format!("goal-loop-{}-{step}", messages.len()),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        } else {
            serde_json::json!({"content":"六步执行后保持人工阻塞。"})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if call.is_some() {"tool_calls"} else {"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }

    async fn send(
        app: &Router,
        team: &str,
        conversation: &str,
        turn: &str,
        text: &str,
    ) -> serde_json::Value {
        let history=request(app,"POST","/api/pawoffice/chat",serde_json::json!({"version":"1.0","type":"history","teamId":team,"conversationId":conversation})).await;
        let mut messages = Vec::new();
        for row in history["data"]["turns"].as_array().unwrap() {
            if row["state"] == "complete" {
                messages.push(serde_json::json!({"role":"user","content":row["user"]}));
                messages.push(serde_json::json!({"role":"assistant","content":row["assistant"]}));
            }
        }
        messages.push(serde_json::json!({"role":"user","content":text}));
        let response=app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat")
            .header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":conversation,"turnId":turn,"messages":messages}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }

    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{method} {path}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn main_context_large_tool_result_is_bounded_and_exactly_readable() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "中英🙂long-unbroken".repeat(1200),
        )
        .unwrap();
        let answer = send(
            &fixture.app,
            &fixture.team,
            "original",
            "read",
            "读取large.txt并分页取得完整原件",
        )
        .await;
        assert_eq!(answer["state"], "complete", "{answer}");
        let requests = fixture.model_seen.lock().await;
        let last = requests.last().unwrap();
        let results: Vec<_> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        assert!(
            results[0]["content"].as_str().unwrap().chars().count() <= 6500,
            "模型应只收到有界工具预览"
        );
        let preview: serde_json::Value =
            serde_json::from_str(results[0]["content"].as_str().unwrap())
                .expect("预览提供可读引用");
        assert!(preview["reference"].is_string());
        let pages: Vec<serde_json::Value> = results[1..]
            .iter()
            .map(|m| serde_json::from_str(m["content"].as_str().unwrap()).unwrap())
            .collect();
        assert!(!pages.is_empty());
        let reconstructed: String = pages
            .iter()
            .map(|p| p["content"].as_str().unwrap())
            .collect();
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "original")
            .unwrap()
            .unwrap();
        let history = request(
            &fixture.app,
            "GET",
            &format!("/session/{}/message", binding.session.as_ref().unwrap()),
            serde_json::json!({}),
        )
        .await;
        let original = history
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["parts"].as_array().unwrap())
            .find(|p| p["tool"] == "read_file")
            .unwrap()["state"]["output"]
            .as_str()
            .unwrap();
        assert_eq!(
            reconstructed, original,
            "分页应精确重建公开历史中的完整结果"
        );
        assert_eq!(pages.last().unwrap()["has_more"], false);
    }

    #[tokio::test]
    async fn main_context_original_reference_survives_restart_and_respects_revert_and_session() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "原件without-newlines".repeat(800),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "original",
                "read",
                "读取large.txt"
            )
            .await["state"],
            "complete"
        );
        let reference = {
            let requests = fixture.model_seen.lock().await;
            let result = requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "tool")
                .unwrap();
            serde_json::from_str::<serde_json::Value>(result["content"].as_str().unwrap()).unwrap()
                ["reference"]
                .clone()
        };
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let read = format!(
            "READ_ORIGINAL {}",
            serde_json::json!({"reference":reference,"start":0,"limit":10})
        );
        let answer = send(&restarted, &fixture.team, "original", "page", &read).await;
        let page: serde_json::Value =
            serde_json::from_str(answer["text"].as_str().unwrap()).unwrap();
        assert_eq!(page["start"], 0);
        assert_eq!(page["end"], 10);
        let foreign = send(&restarted, &fixture.team, "foreign", "page", &read).await;
        assert!(foreign["text"].as_str().unwrap().contains("不属于当前会话"));
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "original")
            .unwrap()
            .unwrap();
        let id = binding.session.as_ref().unwrap();
        let history = request(
            &restarted,
            "GET",
            &format!("/session/{id}/message"),
            serde_json::json!({}),
        )
        .await;
        let first = &history[0]["info"]["id"];
        request(
            &restarted,
            "POST",
            &format!("/session/{id}/revert"),
            serde_json::json!({"messageID":first}),
        )
        .await;
        let hidden = send(&restarted, &fixture.team, "original", "hidden", &read).await;
        assert!(hidden["text"].as_str().unwrap().contains("不存在或已隐藏"));
        let requests = fixture.model_seen.lock().await;
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .is_some_and(|m| m["role"] == "user" && m["content"] == "读取large.txt"))
                .count(),
            1,
            "原始读取只请求一次"
        );
    }

    #[tokio::test]
    async fn main_context_goal_reads_and_updates_do_not_repeat_observation_bodies() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-goal", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "EXACT_ORIGINAL_MARKER".repeat(1300),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "goal",
                "plan",
                "/goal 核对大型原件"
            )
            .await["state"],
            "complete"
        );
        assert_eq!(
            send(&fixture.app, &fixture.team, "goal", "execute", "确认执行").await["state"],
            "complete"
        );
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "goal",
                "inspect",
                "只读取Goal状态"
            )
            .await["state"],
            "complete"
        );
        let (reference, execution_reference) = {
            let requests = fixture.model_seen.lock().await;
            let last = requests.last().unwrap();
            let system = last["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "system")
                .unwrap()["content"]
                .as_str()
                .unwrap();
            assert!(system.len() < 12000, "Goal系统提示不应重复携带观察正文");
            let goal: serde_json::Value = serde_json::from_str(
                last["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(goal["tasks"][0]["status"], "blocked");
            assert_eq!(goal["tasks"][0]["acceptance_criteria"][0], "原件完整");
            assert!(goal["observations"][0].get("output").is_none());
            let fact = goal["execution_attempts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["kind"] == "tool_result" && f["name"] == "read_file")
                .expect("精简执行事实必须保留名称和可回读引用");
            (
                goal["observations"][0]["reference"].clone(),
                fact["reference"].clone(),
            )
        };
        assert!(reference.is_string());
        for (index, reference) in [reference, execution_reference].into_iter().enumerate() {
            let reply = send(
                &fixture.app,
                &fixture.team,
                "goal",
                &format!("original-{index}"),
                &format!(
                    "READ_ORIGINAL {}",
                    serde_json::json!({"reference":reference,"start":6,"limit":21})
                ),
            )
            .await;
            let page: serde_json::Value =
                serde_json::from_str(reply["text"].as_str().unwrap().lines().next().unwrap())
                    .unwrap();
            assert!(page["content"]
                .as_str()
                .unwrap()
                .starts_with("EXACT_ORIGINAL_MARKER"));
        }
    }

    #[tokio::test]
    async fn main_context_compacts_completed_batches_inside_one_turn_without_repeating_tools() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop", "confirm").await;
        std::fs::write(fixture.workspace.path().join("loop_step.py"),"from pathlib import Path\nimport sys\np=Path('counter.txt')\nn=int(p.read_text())+1 if p.exists() else 1\np.write_text(str(n))\nassert n==int(sys.argv[1])\nprint('STEP_DONE_'+str(n))\nprint('证据'*2000)\n").unwrap();
        let intent = "LOOP_PROBE 请严格按顺序运行六步，保留中文证据，不重复已完成操作。";
        let answer = send(&fixture.app, &fixture.team, "loop", "six", intent).await;
        assert_eq!(answer["state"], "complete", "同回合压缩后应继续：{answer}");
        assert!(
            fixture.workspace.path().join("counter.txt").exists(),
            "实际工具未创建计数文件：{:?}",
            fixture.model_seen.lock().await.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "tool")
                .map(|m| m["content"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .take(300)
                    .collect::<String>())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6",
            "工具不能因压缩重复执行"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(
            requests.iter().any(|r| r["stream"] != true),
            "同一回合应真正请求摘要"
        );
        for request in requests.iter().filter(|r| r["stream"] == true) {
            let messages = request["messages"].as_array().unwrap();
            assert!(
                messages
                    .iter()
                    .any(|m| m["role"] == "user" && m["content"] == intent),
                "最新用户意图须原样保留"
            );
            let mut calls = std::collections::HashSet::new();
            for message in messages {
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    calls.insert(call["id"].clone().to_string());
                }
                if message["role"] == "tool" {
                    assert!(
                        calls.contains(&message["tool_call_id"].to_string()),
                        "工具结果必须有对应调用"
                    );
                }
            }
        }
    }

    fn loop_script(fixture: &MainFixture) {
        std::fs::write(fixture.workspace.path().join("loop_step.py"),"from pathlib import Path\nimport sys\np=Path('counter.txt')\nn=int(p.read_text())+1 if p.exists() else 1\np.write_text(str(n))\nassert n==int(sys.argv[1])\nprint('STEP_DONE_'+str(n))\nprint('证据'*2000)\n").unwrap();
    }

    #[tokio::test]
    async fn main_context_goal_compacts_mid_turn_and_retains_actual_manual_block() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop-goal", "confirm").await;
        loop_script(&fixture);
        send(
            &fixture.app,
            &fixture.team,
            "goal-loop",
            "plan",
            "/goal GOAL_LOOP 核对六步结果",
        )
        .await;
        let answer = send(
            &fixture.app,
            &fixture.team,
            "goal-loop",
            "execute",
            "确认执行",
        )
        .await;
        assert_eq!(answer["state"], "complete", "{answer}");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(requests.iter().any(|r| r["stream"] != true));
        assert!(requests.last().unwrap().to_string().contains("blocked"));
        drop(requests);
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "goal-loop")
            .unwrap()
            .unwrap();
        let read = openpaw_store::SessionReader::read(
            fixture
                .home
                .path()
                .join("sessions")
                .join(format!("{}.jsonl", binding.session.unwrap())),
        )
        .unwrap();
        let goal = read
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                openpaw_core::SessionEvent::Unknown(v) if v["type"] == "session_goal_state_v1" => {
                    Some(v)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(goal["state"]["records"][0]["tasks"][0]["status"], "blocked");
    }

    #[tokio::test]
    async fn main_context_escaped_preview_and_invalid_pages_remain_bounded() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "\"\\\u{0001}x中🙂".repeat(1800),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "escaped",
                "read",
                "读取large.txt"
            )
            .await["state"],
            "complete"
        );
        let reference = {
            let requests = fixture.model_seen.lock().await;
            let preview = requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "tool")
                .unwrap()["content"]
                .as_str()
                .unwrap();
            assert!(
                preview.chars().count() <= 6000 && preview.len() <= 24 * 1024,
                "序列化转义后的整个预览也须有界，实际字符数{}",
                preview.chars().count()
            );
            serde_json::from_str::<serde_json::Value>(preview).unwrap()["reference"].clone()
        };
        for (index, args) in [
            serde_json::json!({"reference":reference,"start":-1}),
            serde_json::json!({"reference":reference,"limit":0}),
            serde_json::json!({"reference":reference,"limit":4097}),
            serde_json::json!({"reference":reference,"start":999999}),
            serde_json::json!({"reference":"ctx:v1:[]"}),
        ]
        .iter()
        .enumerate()
        {
            let answer = send(
                &fixture.app,
                &fixture.team,
                "escaped",
                &format!("invalid-{index}"),
                &format!("READ_ORIGINAL {args}"),
            )
            .await;
            assert_eq!(answer["state"], "complete");
            assert!(!answer["text"]
                .as_str()
                .unwrap()
                .contains("\"kind\":\"page\""));
        }
    }

    #[tokio::test]
    async fn main_context_checkpoint_respects_revert_unrevert_fork_and_uncommitted_tail() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop", "confirm").await;
        loop_script(&fixture);
        send(
            &fixture.app,
            &fixture.team,
            "visibility",
            "six",
            "LOOP_PROBE 六步",
        )
        .await;
        send(
            &fixture.app,
            &fixture.team,
            "visibility",
            "hidden",
            "HIDDEN_SECRET_456",
        )
        .await;
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "visibility")
            .unwrap()
            .unwrap();
        let id = binding.session.unwrap();
        let history = request(
            &fixture.app,
            "GET",
            &format!("/session/{id}/message"),
            serde_json::json!({}),
        )
        .await;
        let boundary = history
            .as_array()
            .unwrap()
            .iter()
            .find(|m| {
                m["info"]["role"] == "user" && m["parts"].to_string().contains("HIDDEN_SECRET_456")
            })
            .unwrap()["info"]["id"]
            .clone();
        request(
            &fixture.app,
            "POST",
            &format!("/session/{id}/revert"),
            serde_json::json!({"messageID":boundary}),
        )
        .await;
        request(
            &fixture.app,
            "POST",
            &format!("/session/{id}/unrevert"),
            serde_json::json!({}),
        )
        .await;
        let restored = request(
            &fixture.app,
            "GET",
            &format!("/session/{id}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(restored.to_string().contains("HIDDEN_SECRET_456"));
        let fork = request(
            &fixture.app,
            "POST",
            &format!("/session/{id}/fork"),
            serde_json::json!({"messageID":boundary}),
        )
        .await;
        fixture.model_seen.lock().await.clear();
        request(
            &fixture.app,
            "POST",
            &format!("/session/{}/message", fork["id"].as_str().unwrap()),
            serde_json::json!({"text":"只检查当前可见历史"}),
        )
        .await;
        assert!(fixture
            .model_seen
            .lock()
            .await
            .iter()
            .all(|r| !r.to_string().contains("HIDDEN_SECRET_456")));
        request(
            &fixture.app,
            "POST",
            &format!("/session/{id}/revert"),
            serde_json::json!({"messageID":boundary}),
        )
        .await;
        let path = fixture
            .home
            .path()
            .join("sessions")
            .join(format!("{id}.jsonl"));
        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(
            append,
            "{}",
            serde_json::json!({"type":"turn_finish_prepare_v1","turnID":"torn-context"})
        )
        .unwrap();
        let read = openpaw_store::SessionReader::read(&path).unwrap();
        let mut poison = read
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                openpaw_core::SessionEvent::Unknown(v)
                    if v["type"] == "model_context_checkpoint_v1" =>
                {
                    Some((**v).clone())
                }
                _ => None,
            })
            .unwrap();
        poison["messages"] = serde_json::json!([{"role":"user","content":[{"type":"text","text":"POISON_CHECKPOINT"}]}]);
        writeln!(append, "{poison}").unwrap();
        write!(append, "{{\"type\":").unwrap();
        drop(append);
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        fixture.model_seen.lock().await.clear();
        let answer = send(
            &restarted,
            &fixture.team,
            "visibility",
            "visible",
            "检查有效历史",
        )
        .await;
        assert_eq!(answer["state"], "complete", "{answer}");
        assert!(fixture
            .model_seen
            .lock()
            .await
            .iter()
            .all(|r| !r.to_string().contains("HIDDEN_SECRET_456")
                && !r.to_string().contains("POISON_CHECKPOINT")));
        let visible = request(
            &restarted,
            "GET",
            &format!("/session/{id}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(!visible.to_string().contains("HIDDEN_SECRET_456"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6"
        );
    }

    #[tokio::test]
    async fn main_context_native_overflow_compacts_once_and_retries_without_repeating_tools() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for scenario in ["context-loop-overflow-once", "context-loop-overflow-twice"] {
            let fixture = main_fixture(scenario, "confirm").await;
            loop_script(&fixture);
            let answer = send(
                &fixture.app,
                &fixture.team,
                "overflow",
                "six",
                "LOOP_PROBE 六步",
            )
            .await;
            assert_eq!(
                answer["state"],
                if scenario.ends_with("once") {
                    "complete"
                } else {
                    "error"
                },
                "{scenario}: {answer}"
            );
            let requests = fixture.model_seen.lock().await;
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r["stream"] == true && completed_step(r) == 2)
                    .count(),
                2,
                "同一逻辑请求只恢复一次"
            );
            assert_eq!(
                std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
                if scenario.ends_with("once") { "6" } else { "2" }
            );
        }
    }

    #[tokio::test]
    async fn main_context_real_delegated_child_compacts_and_restores_its_own_history() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop-child", "confirm").await;
        loop_script(&fixture);
        let answer = send(
            &fixture.app,
            &fixture.team,
            "parent",
            "child",
            "DELEGATE_LOOP",
        )
        .await;
        assert_eq!(answer["state"], "complete", "{answer}");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(requests.iter().any(|r| r["stream"] != true));
        let parent_request = requests.last().unwrap().to_string();
        assert!(
            !parent_request.contains("STEP_DONE_1"),
            "子会话内部工具正文不能逐条进入父模型"
        );
        drop(requests);
        let sessions = request(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"].is_string())
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let history = request(
            &restarted,
            "GET",
            &format!("/session/{child}/message"),
            serde_json::json!({}),
        )
        .await;
        assert!(
            history.to_string().contains("STEP_DONE_1"),
            "子会话完整历史必须保留"
        );
    }

    #[tokio::test]
    async fn main_context_manual_compact_is_deterministic_and_does_not_resume_goal() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-goal", "confirm").await;
        let empty = send(&fixture.app, &fixture.team, "empty", "compact", "/compact").await;
        assert_eq!(empty["state"], "complete");
        assert!(empty["text"].as_str().unwrap().contains("无需整理"));
        assert!(
            fixture.model_seen.lock().await.is_empty(),
            "无历史的命令不能交给业务模型"
        );
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "证据".repeat(4000),
        )
        .unwrap();
        send(
            &fixture.app,
            &fixture.team,
            "goal-manual",
            "plan",
            "/goal 核对原件",
        )
        .await;
        send(
            &fixture.app,
            &fixture.team,
            "goal-manual",
            "execute",
            "确认执行",
        )
        .await;
        fixture.model_seen.lock().await.clear();
        let compact = send(
            &fixture.app,
            &fixture.team,
            "goal-manual",
            "compact",
            "/compact",
        )
        .await;
        assert_eq!(compact["state"], "complete", "{compact}");
        assert!(compact["text"].as_str().unwrap().contains("已整理"));
        assert!(
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .all(|r| r["stream"] != true && r["tools"].as_array().is_none_or(|a| a.is_empty())),
            "命令只能请求无工具摘要"
        );
        fixture.model_seen.lock().await.clear();
        send(
            &fixture.app,
            &fixture.team,
            "goal-manual",
            "inspect",
            "只读取Goal状态",
        )
        .await;
        let requests = fixture.model_seen.lock().await;
        let goal: serde_json::Value = serde_json::from_str(
            requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(goal["tasks"][0]["status"], "blocked");
    }

    #[tokio::test]
    async fn main_context_checkpoint_restores_handoff_and_suffix_without_resummarizing() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let mut fixture = main_fixture("context-loop", "confirm").await;
        loop_script(&fixture);
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "persist",
                "six",
                "LOOP_PROBE 六步"
            )
            .await["state"],
            "complete"
        );
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "persist",
                "suffix",
                "保留后续消息 SUFFIX_MARKER"
            )
            .await["state"],
            "complete"
        );
        let config = fixture.state.config.clone();
        fixture.app = Router::new();
        fixture.state = AppState::new(config.clone());
        let restarted = build_router(fixture.state.clone());
        fixture.model_seen.lock().await.clear();
        assert_eq!(
            send(
                &restarted,
                &fixture.team,
                "persist",
                "next",
                "接着回答 AFTER_RESTART"
            )
            .await["state"],
            "complete"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(
            requests.iter().all(|r| r["stream"] == true),
            "重启应使用已保存检查点，不能再次摘要"
        );
        let body = requests.first().unwrap().to_string();
        assert!(body.contains("[Context Summary]"));
        assert!(body.contains("SUFFIX_MARKER"));
        assert!(
            !body.contains("STEP_DONE_1\\n"),
            "被替代的旧工具正文不应重新注入"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6"
        );
    }

    #[tokio::test]
    async fn main_context_invalid_summary_stops_without_replaying_completed_tools() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop-invalid", "confirm").await;
        loop_script(&fixture);
        let answer = send(
            &fixture.app,
            &fixture.team,
            "invalid",
            "six",
            "LOOP_PROBE 六步",
        )
        .await;
        assert_eq!(answer["state"], "error", "无效摘要必须明确失败：{answer}");
        let count: usize = std::fs::read_to_string(fixture.workspace.path().join("counter.txt"))
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..6).contains(&count));
        let requests = fixture.model_seen.lock().await;
        assert_eq!(requests.iter().filter(|r| r["stream"] != true).count(), 1);
        assert_ne!(
            requests.last().unwrap()["stream"],
            true,
            "摘要失败后不能继续业务请求"
        );
    }

    #[tokio::test]
    async fn main_context_model_change_rebuilds_for_the_smaller_actual_window() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop", "confirm").await;
        loop_script(&fixture);
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "model-change",
                "six",
                "LOOP_PROBE 六步"
            )
            .await["state"],
            "complete"
        );
        std::env::set_var(
            "OPENPAW_MODEL_CAPABILITIES",
            r#"{"openai":{"smaller-model":{"input":["text"],"context_window":16384}}}"#,
        );
        fixture.model_seen.lock().await.clear();
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "model-change")
            .unwrap()
            .unwrap();
        // 聊天沿用会话模型；模型切换通过既有REST覆盖入口，不伪造环境变量热切换。
        let id = binding.session.unwrap();
        request(&fixture.app, "POST", &format!("/session/{id}/message"), serde_json::json!({"text":"保留所有用户约束 MODEL_SWITCH","model":{"providerID":"openai","modelID":"smaller-model"}})).await;
        let turns = request(
            &fixture.app,
            "GET",
            &format!("/session/{id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            turns.as_array().unwrap().last().unwrap()["state"],
            "completed",
            "{turns}"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(requests.iter().all(|r| r["model"] == "smaller-model"));
        assert!(
            requests.iter().any(|r| r["stream"] != true),
            "模型不兼容的旧检查点应失效，按新窗口重新整理原件"
        );
        assert!(requests
            .iter()
            .filter(|r| r["stream"] == true)
            .all(|r| r["messages"].to_string().contains("MODEL_SWITCH")));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            "6"
        );
    }

    #[tokio::test]
    async fn main_context_finish_failure_does_not_publish_the_summary_candidate() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-manual-cancel", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "原件保存".repeat(2000),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "finish-failure",
                "read",
                "读取large.txt"
            )
            .await["state"],
            "complete"
        );
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "finish-failure")
            .unwrap()
            .unwrap();
        let id = binding.session.unwrap();
        let handle = fixture
            .state
            .sessions
            .read()
            .await
            .get(&id)
            .unwrap()
            .clone();
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let sending = tokio::spawn(async move {
            send(&app, &team, "finish-failure", "compact", "/compact").await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .any(|r| r["stream"] != true)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // 仅在已确认的持久化故障注入边界关闭writer；模型请求仍由真实主聊天驱动。
        handle.writer.lock().unwrap().take();
        fixture.release_model.notify_waiters();
        let answer = tokio::time::timeout(Duration::from_secs(3), sending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(answer["state"], "error", "{answer}");
        assert!(handle.session.lock().await.model_context.is_none());
        let read = openpaw_store::SessionReader::read(
            fixture
                .home
                .path()
                .join("sessions")
                .join(format!("{id}.jsonl")),
        )
        .unwrap();
        assert!(read.events.iter().all(|e| !matches!(e, openpaw_core::SessionEvent::Unknown(v) if v["type"] == "model_context_checkpoint_v1")));
        assert!(read.events.iter().any(|e| matches!(e, openpaw_core::SessionEvent::Message(message) if message.content.iter().any(|block| matches!(block, openpaw_core::ContentBlock::ToolResult { content, .. } if content.contains("原件保存"))))));
    }

    #[tokio::test]
    async fn main_context_cancel_pending_summary_prevents_further_tools() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-loop-cancel", "confirm").await;
        loop_script(&fixture);
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let sending =
            tokio::spawn(
                async move { send(&app, &team, "cancel", "six", "LOOP_PROBE 六步").await },
            );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .any(|r| r["stream"] != true)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let before = std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap();
        let status = request(
            &fixture.app,
            "GET",
            "/session/status",
            serde_json::json!({}),
        )
        .await;
        assert!(
            status
                .as_object()
                .unwrap()
                .values()
                .any(|v| v["activity"]["task"] == "正在整理上下文"),
            "摘要等待须显示现有整理状态：{status}"
        );
        request(&fixture.app, "POST", "/api/pawoffice/chat", serde_json::json!({"version":"1.0","type":"cancel","teamId":fixture.team,"conversationId":"cancel","turnId":"six"})).await;
        let answer = tokio::time::timeout(Duration::from_secs(3), sending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(answer["state"], "stopped");
        fixture.release_model.notify_waiters();
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("counter.txt")).unwrap(),
            before
        );
        assert_ne!(
            fixture.model_seen.lock().await.last().unwrap()["stream"],
            true
        );
    }

    #[tokio::test]
    async fn main_context_manual_compact_busy_and_cancel_keep_existing_admission_rules() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-manual-cancel", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "可回读资料".repeat(2000),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "manual-cancel",
                "read",
                "读取large.txt"
            )
            .await["state"],
            "complete"
        );
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let sending =
            tokio::spawn(
                async move { send(&app, &team, "manual-cancel", "compact", "/compact").await },
            );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .any(|r| r["stream"] != true)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let busy = fixture.app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":fixture.team,"conversationId":"manual-cancel","turnId":"another","messages":[{"role":"user","content":"/compact"}]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        request(&fixture.app, "POST", "/api/pawoffice/chat", serde_json::json!({"version":"1.0","type":"cancel","teamId":fixture.team,"conversationId":"manual-cancel","turnId":"compact"})).await;
        let answer = tokio::time::timeout(Duration::from_secs(3), sending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(answer["state"], "stopped");
        assert_eq!(
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .filter(|r| r["stream"] != true)
                .count(),
            1
        );
        fixture.release_model.notify_waiters();
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "manual-cancel")
            .unwrap()
            .unwrap();
        let read = openpaw_store::SessionReader::read(
            fixture
                .home
                .path()
                .join("sessions")
                .join(format!("{}.jsonl", binding.session.unwrap())),
        )
        .unwrap();
        assert!(read.events.iter().all(|e| !matches!(e, openpaw_core::SessionEvent::Unknown(v) if v["type"] == "model_context_checkpoint_v1")), "取消的手动候选不能发布");
    }

    #[tokio::test]
    async fn main_context_fixed_user_input_overflow_is_rejected_without_model_or_tools() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-fixed-input", "confirm").await;
        let answer = send(
            &fixture.app,
            &fixture.team,
            "too-large",
            "first",
            &format!("LOOP_PROBE {}", "不可遗漏约束".repeat(600)),
        )
        .await;
        assert_eq!(answer["state"], "error");
        assert!(fixture.model_seen.lock().await.is_empty());
        assert!(!fixture.workspace.path().join("counter.txt").exists());
    }

    #[tokio::test]
    async fn main_context_legacy_messages_without_uuid_have_exact_visible_originals() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(fixture.workspace.path().join("large.txt"), "seed").unwrap();
        send(
            &fixture.app,
            &fixture.team,
            "legacy",
            "seed",
            "读取large.txt",
        )
        .await;
        let binding = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "legacy")
            .unwrap()
            .unwrap();
        let id = binding.session.unwrap();
        let path = fixture
            .home
            .path()
            .join("sessions")
            .join(format!("{id}.jsonl"));
        let read = openpaw_store::SessionReader::read(&path).unwrap();
        let count = openpaw_store::transcript::replay(&id, &read.events)
            .unwrap()
            .messages
            .len();
        let original = "LEGACY_RAW_旧正文🙂".repeat(1500);
        let mut writer = openpaw_store::SessionWriter::open_append(&path).unwrap();
        writer
            .append(&openpaw_core::SessionEvent::Message(
                openpaw_core::Message {
                    role: openpaw_core::Role::Assistant,
                    content: vec![openpaw_core::ContentBlock::ToolUse {
                        id: "legacy-read".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({"path":"历史原件，不应重新读取"}),
                    }],
                    ..Default::default()
                },
            ))
            .unwrap();
        writer
            .append(&openpaw_core::SessionEvent::Message(
                openpaw_core::Message {
                    role: openpaw_core::Role::User,
                    content: vec![openpaw_core::ContentBlock::ToolResult {
                        tool_use_id: "legacy-read".into(),
                        content: original.clone(),
                        is_error: false,
                        name: Some("read_file".into()),
                        protected: false,
                        media: vec![],
                    }],
                    ..Default::default()
                },
            ))
            .unwrap();
        drop(writer);
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let reference = openpaw_engine::context::record_reference(
            &id,
            &format!("message/legacy/{}", count + 1),
            0,
            "legacy-read",
        );
        let answer = send(
            &restarted,
            &fixture.team,
            "legacy",
            "page",
            &format!(
                "READ_ORIGINAL {}",
                serde_json::json!({"reference":reference,"start":7,"limit":33})
            ),
        )
        .await;
        assert_eq!(answer["state"], "complete");
        let page: serde_json::Value =
            serde_json::from_str(answer["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            page["content"],
            original.chars().skip(7).take(33).collect::<String>()
        );
        assert_eq!(page["total_chars"], original.chars().count());
        assert!(fixture.model_seen.lock().await.last().unwrap()["messages"]
            .to_string()
            .contains("tool_result_preview"));
    }

    #[tokio::test]
    async fn main_context_unrevert_after_restart_restores_uncompacted_history_to_model() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let mut fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "ordinary result",
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "unrevert",
                "first",
                "HISTORY_BEFORE_RESTART_FIRST"
            )
            .await["state"],
            "complete"
        );
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "unrevert",
                "second",
                "HISTORY_BEFORE_RESTART_SECOND"
            )
            .await["state"],
            "complete"
        );
        let id = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "unrevert")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let history = request(
            &fixture.app,
            "GET",
            &format!("/session/{id}/message"),
            serde_json::json!({}),
        )
        .await;
        request(
            &fixture.app,
            "POST",
            &format!("/session/{id}/revert"),
            serde_json::json!({"messageID":history[0]["info"]["id"]}),
        )
        .await;
        let config = fixture.state.config.clone();
        fixture.app = Router::new();
        fixture.state = AppState::new(config);
        let restarted = build_router(fixture.state.clone());
        request(
            &restarted,
            "POST",
            &format!("/session/{id}/unrevert"),
            serde_json::json!({}),
        )
        .await;
        fixture.model_seen.lock().await.clear();
        assert_eq!(
            send(
                &restarted,
                &fixture.team,
                "unrevert",
                "next",
                "继续读取此前历史"
            )
            .await["state"],
            "complete"
        );
        let requests = fixture.model_seen.lock().await;
        let actual = requests.first().unwrap()["messages"].to_string();
        assert!(
            actual.contains("HISTORY_BEFORE_RESTART_FIRST")
                && actual.contains("HISTORY_BEFORE_RESTART_SECOND"),
            "取消回退后的模型应收到恢复的可见原历史：{actual}"
        );
    }

    #[tokio::test]
    async fn main_context_large_result_preview_shrinks_for_an_actual_small_window() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-small-window", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "小窗口完整证据".repeat(2000),
        )
        .unwrap();
        let answer = send(
            &fixture.app,
            &fixture.team,
            "small-window",
            "read",
            "SMALL_WINDOW_PREVIEW",
        )
        .await;
        let id = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "small-window")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let turns = request(
            &fixture.app,
            "GET",
            &format!("/session/{id}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            answer["state"],
            "complete",
            "首次结果也应按实际窗口缩小：{answer}, turns={turns}, requests={}",
            fixture.model_seen.lock().await.len()
        );
        let requests = fixture.model_seen.lock().await;
        let preview = requests
            .iter()
            .filter(|r| r["stream"] == true)
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .find_map(|m| {
                let content = m["content"].as_str()?;
                let value: serde_json::Value = serde_json::from_str(content).ok()?;
                (value["kind"] == "tool_result_preview").then_some(content)
            })
            .expect("模型应收到小窗口预览");
        assert!(preview.chars().count() <= 1024 && preview.len() <= 4096);
    }

    #[tokio::test]
    async fn main_context_record_pages_follow_the_overridden_model_window() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("context-original", "confirm").await;
        std::fs::write(
            fixture.workspace.path().join("large.txt"),
            "分页中文原件".repeat(1500),
        )
        .unwrap();
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "page-model",
                "read",
                "SMALL_WINDOW_PREVIEW"
            )
            .await["state"],
            "complete"
        );
        let reference = {
            let requests = fixture.model_seen.lock().await;
            let tool = requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "tool")
                .unwrap();
            serde_json::from_str::<serde_json::Value>(tool["content"].as_str().unwrap()).unwrap()
                ["reference"]
                .clone()
        };
        let id = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "page-model")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        std::env::set_var(
            "OPENPAW_MODEL_CAPABILITIES",
            r#"{"openai":{"smaller-model":{"input":["text"],"context_window":16384}}}"#,
        );
        fixture.model_seen.lock().await.clear();
        request(&fixture.app, "POST", &format!("/session/{id}/message"), serde_json::json!({"text":format!("READ_ORIGINAL {}",serde_json::json!({"reference":reference,"start":5,"limit":4096})),"model":{"providerID":"openai","modelID":"smaller-model"}})).await;
        let requests = fixture.model_seen.lock().await;
        let page = requests
            .iter()
            .filter(|r| r["stream"] == true)
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| serde_json::from_str::<serde_json::Value>(m["content"].as_str()?).ok())
            .find(|v| v["kind"] == "page")
            .expect("新模型应实际收到原件分页");
        let chars = page["content"].as_str().unwrap().chars().count();
        assert!(
            chars <= 2048,
            "分页必须跟随本回合模型而非会话创建窗口，实际{chars}"
        );
        assert_eq!(page["end"].as_u64().unwrap(), 5 + chars as u64);
        assert_eq!(page["next_start"], page["end"]);
        assert_eq!(page["has_more"], true);
    }
}
