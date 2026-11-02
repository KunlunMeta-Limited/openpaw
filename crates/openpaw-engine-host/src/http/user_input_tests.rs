#[cfg(test)]
mod history_question_tests {
    use super::*;

    const TAIL: &str = "TAIL_CHECK_f7c908b2_末尾唯一校验";
    fn child_text() -> String {
        format!("{}\n{TAIL}", "实际子结果中英🙂|".repeat(12000))
    }
    fn response(tool: Option<(&str, serde_json::Value)>, text: &str, index: usize) -> Response {
        let delta = if let Some((name, args)) = tool {
            serde_json::json!({"tool_calls":[{"index":0,"id":format!("history-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        } else {
            serde_json::json!({"content":text})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if delta["tool_calls"].is_array(){"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }
    fn metadata(body: &serde_json::Value) -> Vec<serde_json::Value> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["content"].as_str())
            .flat_map(str::lines)
            .filter_map(|line| {
                serde_json::from_str(
                    line.strip_prefix("[Assistant] ")
                        .or_else(|| line.strip_prefix("[User] "))
                        .unwrap_or(line),
                )
                .ok()
            })
            .collect()
    }
    /// 功能：按真实历史模型输入生成本地压缩及原件回读响应。
    /// 参数：body为模型请求，gate为子结果交付的测试门闩。
    /// 返回：本地模型协议响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：只消费本地请求，必要时等待子结果放行。
    /// 并发与取消：异步等待可取消，不访问真实模型。
    /// 权限：不授予工具或原件读取权限。
    /// 关联：CHG-2026-0054、history_question_tests。
    pub(super) async fn model_response(
        body: &serde_json::Value,
        gate: &tokio::sync::Notify,
    ) -> Response {
        if body["stream"] != true || !body["tools"].is_array() {
            let mut previews = metadata(body)
                .into_iter()
                .filter(|v| v["reference"].is_string() && v["total_chars"].is_number())
                .map(|mut value| {
                    value["content"] = serde_json::json!(value["content"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(64)
                        .collect::<String>());
                    value
                })
                .collect::<Vec<_>>();
            if body["messages"]
                .to_string()
                .contains("[Imported Context Fact]")
            {
                previews.clear();
            }
            let observed = ["CHILD_VALUE_ONE", "CHILD_VALUE_TWO"]
                .into_iter()
                .filter(|value| body["messages"].to_string().contains(value))
                .collect::<Vec<_>>()
                .join(" ");
            let summary = format!("[Context Summary] Keep actual readable fact indexes; do not guess answers or tail values. Observed completed child values: {observed}\n{}\n", previews.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
            return if body["stream"] == true {
                response(None, &summary, 0)
            } else {
                Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":summary},"finish_reason":"stop"}]})).into_response()
            };
        }
        let all = body["messages"].as_array().unwrap();
        if let Some(source) = all.iter().rposition(|m| {
            m["role"] == "user"
                && m["content"].as_str().is_some_and(|s| {
                    s.starts_with("HISTORY_REUSE_")
                        || s.starts_with("HISTORY_VISIBILITY_PROBE")
                        || s.starts_with("HISTORY_READ_REF ")
                        || s.starts_with("HISTORY_QUERY_OLD ")
                })
        }) {
            let text = all[source]["content"].as_str().unwrap();
            let current = &all[source..];
            let tools = current
                .iter()
                .filter(|m| m["role"] == "tool")
                .collect::<Vec<_>>();
            if text.starts_with("HISTORY_VISIBILITY_PROBE") {
                if !tools.is_empty() {
                    return response(None, "可见性已按真实模型输入交付", tools.len());
                }
                let input = body["messages"].to_string();
                let observed = if input.contains("CHILD_VALUE_TWO") {
                    "VISIBLE_TWO"
                } else if input.contains("CHILD_VALUE_ONE") {
                    "VISIBLE_ONE"
                } else {
                    "VISIBLE_NONE"
                };
                return response(
                    Some((
                        "write_file",
                        serde_json::json!({"path":"visibility-check.txt","content":observed}),
                    )),
                    "",
                    0,
                );
            }
            if text.starts_with("HISTORY_READ_REF ") || text.starts_with("HISTORY_QUERY_OLD ") {
                if !tools.is_empty() {
                    return response(
                        None,
                        tools.last().unwrap()["content"].as_str().unwrap_or(""),
                        tools.len(),
                    );
                }
                let (name, args) = if let Some(reference) = text.strip_prefix("HISTORY_READ_REF ") {
                    (
                        "read_context_record",
                        serde_json::json!({"reference":reference,"start":0,"limit":512}),
                    )
                } else {
                    let identities = text
                        .strip_prefix("HISTORY_QUERY_OLD ")
                        .unwrap()
                        .split_whitespace()
                        .collect::<Vec<_>>();
                    (
                        "get_agent_status",
                        serde_json::json!({"session_id":identities[0],"turn_id":identities[1]}),
                    )
                };
                return response(Some((name, args)), "", 0);
            }
            let child = !body["tools"].to_string().contains("start_agent");
            if child {
                return if tools.is_empty() {
                    response(
                        Some((
                            "request_user_input",
                            serde_json::json!({"question":if text.ends_with("TWO"){"第二父来源的子问题"}else{"第一父来源的子问题"},"options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
                        )),
                        "",
                        0,
                    )
                } else {
                    response(
                        None,
                        if text.ends_with("TWO") {
                            "CHILD_VALUE_TWO"
                        } else {
                            "CHILD_VALUE_ONE"
                        },
                        1,
                    )
                };
            }
            if !tools.is_empty()
                || serde_json::to_string(current)
                    .unwrap()
                    .contains("CHILD_VALUE_")
            {
                return response(None, "父独立工作完成，按真实结果收尾", tools.len());
            }
            let mut args = serde_json::json!({"name":"来源校验员","task":if text.ends_with("TWO"){"HISTORY_REUSE_CHILD_TWO"}else{"HISTORY_REUSE_CHILD_ONE"}});
            if text.ends_with("TWO") {
                args["task_id"] = all[..source]
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .filter_map(|m| {
                        serde_json::from_str::<serde_json::Value>(m["content"].as_str()?).ok()
                    })
                    .find_map(|v| v["session_id"].as_str().map(str::to_owned))
                    .or_else(|| {
                        metadata(body).iter().find_map(|value| {
                            let key: serde_json::Value = serde_json::from_str(
                                value["reference"].as_str()?.strip_prefix("ctx:v1:")?,
                            )
                            .ok()?;
                            let call = key[3].as_str()?;
                            (call.starts_with("child-result/")
                                || call.starts_with("child-question/"))
                            .then(|| call.split('/').nth(1).unwrap().to_owned())
                        })
                    })
                    .map(serde_json::Value::String)
                    .expect("续办ID必须来自当前真实模型上下文");
            }
            return response(Some(("start_agent", args)), "", 0);
        }
        if all.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|text| text.contains("CHILD_VALUE_"))
        }) {
            return response(None, "已收尾真实子结果", 0);
        }
        if let Some(source) = all.iter().rposition(|m| {
            m["role"] == "user"
                && m["content"].as_str().is_some_and(|s| {
                    s.starts_with("HISTORY_ASK")
                        || s.starts_with("HISTORY_FACT_PROBE")
                        || s.starts_with("HISTORY_CHILD_FACT_PROBE")
                })
        }) {
            let current = &all[source..];
            let tools = current
                .iter()
                .filter(|m| m["role"] == "tool")
                .collect::<Vec<_>>();
            if all[source]["content"]
                .as_str()
                .unwrap()
                .starts_with("HISTORY_ASK")
            {
                return if tools.is_empty() {
                    response(
                        Some((
                            "request_user_input",
                            serde_json::json!({"question":"选择交付颜色","options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
                        )),
                        "",
                        0,
                    )
                } else {
                    response(None, "已接收真实答案", 1)
                };
            }
            if current
                .iter()
                .any(|m| m["tool_calls"].to_string().contains("write_file"))
            {
                return response(None, "已恢复并交付真实答案", tools.len());
            }
            let page_all = all[source]["content"]
                .as_str()
                .unwrap()
                .contains("PAGE_ALL");
            let pages = metadata(&serde_json::json!({"messages":current}));
            if let Some(page) = pages.iter().rev().find(|v| v["kind"] == "page") {
                if all[source]["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("HISTORY_CHILD_FACT_PROBE")
                {
                    return if let Some(tail) = page["content"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .find(|line| line.starts_with("TAIL_CHECK_"))
                    {
                        response(
                            Some((
                                "write_file",
                                serde_json::json!({"path":"fork-tail-check.txt","content":tail}),
                            )),
                            "",
                            tools.len(),
                        )
                    } else {
                        response(None, "原件末尾尚未读到，不交付", tools.len())
                    };
                }
                if page_all {
                    return if page["has_more"] == true {
                        response(
                            Some((
                                "read_context_record",
                                serde_json::json!({"reference":page["reference"],"start":page["next_start"],"limit":256}),
                            )),
                            "",
                            tools.len(),
                        )
                    } else {
                        response(
                            Some((
                                "write_file",
                                serde_json::json!({"path":"recovered-answer-tail.txt","content":page["content"]}),
                            )),
                            "",
                            tools.len(),
                        )
                    };
                }
                let record = page["content"]
                    .as_str()
                    .unwrap_or("")
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .find(|v| v["answer"]["label"].is_string());
                return if let Some(record) = record {
                    response(
                        Some((
                            "write_file",
                            serde_json::json!({"path":"recovered-answer.txt","content":record["answer"]["custom_text"].as_str().or_else(||record["answer"]["label"].as_str()).unwrap()}),
                        )),
                        "",
                        tools.len(),
                    )
                } else {
                    response(None, "原件未含有效答案", tools.len())
                };
            }
            let values = metadata(body);
            let fact = values
                .iter()
                .rev()
                .find(|v| v["kind"] == "message_text_preview")
                .or_else(|| {
                    values.iter().rev().find(|v| {
                        v["kind"] == "user_question_fact"
                            || v["kind"] == "child_result_preview"
                            || v["kind"] == "imported_context_fact"
                    })
                });
            return if let Some(fact) = fact {
                let start = if all[source]["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("HISTORY_CHILD_FACT_PROBE")
                {
                    fact["total_chars"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(128)
                } else {
                    0
                };
                response(
                    Some((
                        "read_context_record",
                        serde_json::json!({"reference":fact["reference"],"start":start,"limit":if page_all{256}else{2048}}),
                    )),
                    "",
                    tools.len(),
                )
            } else {
                response(None, "NO_RELIABLE_FACT", tools.len())
            };
        }
        let parent = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "start_agent");
        if !parent {
            gate.notified().await;
            return response(None, &child_text(), 0);
        }
        let messages = body["messages"].as_array().unwrap();
        let tools = messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let values = metadata(body);
        if messages.iter().any(|m| {
            m["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.iter().any(|c| c["function"]["name"] == "write_file"))
        }) {
            return response(None, "已按真实原件末尾交付", tools.len());
        }
        if let Some(page) = values.iter().rev().find(|v| v["kind"] == "page") {
            let text = page["content"].as_str().unwrap_or("");
            if let Some(tail) = text.lines().find(|line| line.starts_with("TAIL_CHECK_")) {
                return response(
                    Some((
                        "write_file",
                        serde_json::json!({"path":"tail-check.txt","content":tail}),
                    )),
                    "",
                    tools.len(),
                );
            }
            return response(None, "原件没有给出末尾校验，不交付", tools.len());
        }
        if let Some(preview) = values
            .iter()
            .rev()
            .find(|v| v["reference"].is_string() && v["total_chars"].is_number())
        {
            let start = preview["total_chars"].as_u64().unwrap().saturating_sub(128);
            return response(
                Some((
                    "read_context_record",
                    serde_json::json!({"reference":preview["reference"],"start":start,"limit":128}),
                )),
                "",
                tools.len(),
            );
        }
        if tools.is_empty() && !body.to_string().contains("可信子执行结果") {
            return response(
                Some((
                    "start_agent",
                    serde_json::json!({"task":"history-long-child","name":"长原件员工"}),
                )),
                "",
                0,
            );
        }
        response(None, "父已完成独立工作，真实子结果尚待处理", tools.len())
    }
    async fn send(app: Router, team: String) -> serde_json::Value {
        let response = app.oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":"history","turnId":"long-result","messages":[{"role":"user","content":"HISTORY_LONG_RESULT：启动一个子任务；先做独立工作后正常结束，收到真实结果后从原件末尾读取校验值并写入tail-check.txt。"}]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .expect("真实长子结果收尾必须有界")
        .unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }
    async fn send_in(
        app: &Router,
        team: &str,
        conversation: &str,
        turn: &str,
        text: &str,
    ) -> serde_json::Value {
        let history = child_test_json(app,"POST","/api/pawoffice/chat",serde_json::json!({"version":"1.0","type":"history","teamId":team,"conversationId":conversation})).await;
        let mut messages = history["data"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["state"] == "complete")
            .flat_map(|r| {
                [
                    serde_json::json!({"role":"user","content":r["user"]}),
                    serde_json::json!({"role":"assistant","content":r["assistant"]}),
                ]
            })
            .collect::<Vec<_>>();
        messages.push(serde_json::json!({"role":"user","content":text}));
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json").body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":team,"conversationId":conversation,"turnId":turn,"messages":messages}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }
    #[tokio::test]
    async fn main_history_answered_prefix_is_used_after_restart_without_reask() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-history-answer", "option-a").await;
        assert_eq!(
            send_in(
                &fixture.app,
                &fixture.team,
                "history-answer",
                "ask",
                "HISTORY_ASK 先取得答案"
            )
            .await["state"],
            "complete"
        );
        let owner = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "history-answer")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let path = fixture
            .home
            .path()
            .join("sessions")
            .join(format!("{owner}.jsonl"));
        let original = std::fs::read_to_string(&path).unwrap();
        let lines = original
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let index = lines
            .iter()
            .position(|v| v["type"] == "user_question_v1" && v["record"]["state"] == "answered")
            .unwrap();
        std::fs::write(
            &path,
            original
                .lines()
                .take(index + 1)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        fixture.model_seen.lock().await.clear();
        let result = send_in(
            &restarted,
            &fixture.team,
            "history-answer",
            "resume",
            "HISTORY_FACT_PROBE 使用已可靠回答并从原件确认后交付",
        )
        .await;
        assert_eq!(result["state"], "complete", "{result}");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("recovered-answer.txt"))
                .ok()
                .as_deref(),
            Some("方案A"),
            "必须从可靠原件回读后交付：{result}"
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|v| v["type"] == "decision.request")
                .count(),
            1,
            "重启不能重新问已有答案"
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("message/temporary"));
    }
    fn assert_complete_custom_pages(
        requests: &[serde_json::Value],
        owner: &str,
        delivered_tail: &str,
    ) {
        let pages = requests
            .iter()
            .filter(|request| request["stream"] == true && request["tools"].is_array())
            .flat_map(|request| request["messages"].as_array().unwrap())
            .filter(|message| message["role"] == "tool")
            .filter_map(|message| {
                serde_json::from_str::<serde_json::Value>(message["content"].as_str()?).ok()
            })
            .filter(|value| value["kind"] == "page")
            .collect::<Vec<_>>();
        assert!(pages.len() > 1, "完整答案必须经过真实工具分页进入模型输入");
        let reference = pages[0]["reference"].as_str().unwrap();
        openpaw_engine::context::parse_reference(reference, owner).unwrap();
        let total = pages[0]["total_chars"].as_u64().unwrap() as usize;
        let mut observed = vec![None; total];
        for page in &pages {
            assert_eq!(page["reference"], reference);
            assert_eq!(page["total_chars"], total);
            let start = page["start"].as_u64().unwrap() as usize;
            let content = page["content"].as_str().unwrap();
            let end = start + content.chars().count();
            assert_eq!(page["next_start"], end);
            assert_eq!(page["has_more"], end < total);
            for (offset, ch) in content.chars().enumerate() {
                let cell = &mut observed[start + offset];
                assert!(cell.is_none_or(|previous| previous == ch));
                *cell = Some(ch);
            }
        }
        let original = observed
            .into_iter()
            .map(|ch| ch.expect("完整原件不能有未进入真实模型输入的缺页"))
            .collect::<String>();
        let record = original
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|value| value["answer"]["custom_text"].is_string())
            .expect("必须从真实页面重建已接受回答原件");
        assert_eq!(record["state"], "answered");
        assert_eq!(record["answer"]["custom_text"], "补".repeat(1000));
        let last = pages
            .iter()
            .rfind(|page| page["has_more"] == false)
            .unwrap();
        assert_eq!(delivered_tail, last["content"].as_str().unwrap());
    }
    #[tokio::test]
    async fn main_history_max_custom_answer_survives_missing_result_fork_and_restart() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-history-answer", "max-custom").await;
        assert_eq!(
            send_in(
                &fixture.app,
                &fixture.team,
                "custom",
                "ask",
                "HISTORY_ASK 取得完整补充"
            )
            .await["state"],
            "complete"
        );
        let owner = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "custom")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let path = fixture
            .home
            .path()
            .join("sessions")
            .join(format!("{owner}.jsonl"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let index = raw
            .lines()
            .position(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["type"] == "user_question_v1" && value["record"]["state"] == "answered"
            })
            .unwrap();
        std::fs::write(
            &path,
            raw.lines().take(index + 1).collect::<Vec<_>>().join("\n") + "\n",
        )
        .unwrap();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let fork = child_test_json(
            &restarted,
            "POST",
            &format!("/session/{owner}/fork"),
            serde_json::json!({}),
        )
        .await;
        let fork_id = fork["id"].as_str().unwrap();
        fixture.model_seen.lock().await.clear();
        child_test_json(
            &restarted,
            "POST",
            &format!("/session/{fork_id}/message"),
            serde_json::json!({"text":"HISTORY_FACT_PROBE PAGE_ALL 逐页回读完整补充后交付末页"}),
        )
        .await;
        assert_complete_custom_pages(
            &fixture.model_seen.lock().await,
            fork_id,
            &std::fs::read_to_string(fixture.workspace.path().join("recovered-answer-tail.txt"))
                .unwrap(),
        );
        std::fs::remove_file(fixture.workspace.path().join("recovered-answer-tail.txt")).unwrap();
        fixture.model_seen.lock().await.clear();
        assert_eq!(
            send_in(
                &restarted,
                &fixture.team,
                "custom",
                "resume",
                "HISTORY_FACT_PROBE PAGE_ALL 继续逐页回读完整自定义补充后交付末页"
            )
            .await["state"],
            "complete"
        );
        assert_complete_custom_pages(
            &fixture.model_seen.lock().await,
            &owner,
            &std::fs::read_to_string(fixture.workspace.path().join("recovered-answer-tail.txt"))
                .unwrap(),
        );
        assert!(
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .any(
                    |request| request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| message["role"] == "system"
                            && message["content"]
                                .as_str()
                                .unwrap_or("")
                                .contains("answer_truncated"))
                ),
            "合法长答案也必须保留有界预览和原件引用"
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|v| v["type"] == "decision.request")
                .count(),
            1,
            "分支或重启均不能重问可靠答案"
        );
    }
    #[tokio::test]
    async fn main_history_long_child_end_turn_result_has_readable_tail_original() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-history-long", "option-a").await;
        let pending = tokio::spawn(send(fixture.app.clone(), fixture.team.clone()));
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                let child = requests.iter().any(|r| {
                    r["messages"].as_array().is_some_and(|messages| {
                        messages
                            .iter()
                            .any(|m| m["content"] == "history-long-child")
                    })
                });
                let parent = requests.iter().any(|r| {
                    r["tools"]
                        .as_array()
                        .is_some_and(|ts| ts.iter().any(|t| t["function"]["name"] == "start_agent"))
                        && r["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|m| m["role"] == "tool")
                });
                if child && parent {
                    break;
                }
                drop(requests);
                tokio::task::yield_now().await;
            }
        })
        .await;
        if ready.is_err() {
            let requests = fixture.model_seen.lock().await.iter().map(|r|serde_json::json!({"stream":r["stream"],"tool_count":r["tools"].as_array().map(Vec::len),"users":r["messages"].as_array().unwrap().iter().filter(|m|m["role"]=="user").map(|m|m["content"].clone()).collect::<Vec<_>>(),"tool_results":r["messages"].as_array().unwrap().iter().filter(|m|m["role"]=="tool").count()})).collect::<Vec<_>>();
            fixture.release_model.notify_one();
            let terminal = tokio::time::timeout(Duration::from_secs(3), pending)
                .await
                .ok()
                .map(Result::unwrap);
            panic!("父在子结束前应继续模型并准备正常EndTurn：requests={requests:?}, terminal={terminal:?}");
        }
        assert!(!pending.is_finished(), "父应保持执行归属等子真实结果");
        fixture.release_model.notify_one();
        let answer = pending.await.unwrap();
        let session = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "history")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let turns = child_test_json(
            &fixture.app,
            "GET",
            &format!("/session/{session}/turns"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            answer["state"], "complete",
            "长结果须有界并可读，不能超窗截断：{answer}, turns={turns}"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("tail-check.txt")).unwrap(),
            TAIL,
            "父必须从分页原件末尾取得值后才交付"
        );
        let requests = fixture.model_seen.lock().await;
        let parent = requests
            .iter()
            .filter(|r| {
                r["tools"]
                    .as_array()
                    .is_some_and(|ts| ts.iter().any(|t| t["function"]["name"] == "start_agent"))
            })
            .collect::<Vec<_>>();
        assert!(
            parent
                .iter()
                .any(|r| r["messages"].to_string().contains("可信子执行结果")),
            "必须覆盖正常EndTurn协调路径"
        );
        assert!(
            parent
                .iter()
                .any(|r| r["messages"].to_string().contains("read_context_record")),
            "必须实际分页原件"
        );
        assert!(
            parent.iter().all(|r| !r["messages"]
                .to_string()
                .contains(&"实际子结果中英🙂|".repeat(12000))),
            "模型只收有界副本"
        );
        assert!(
            !requests
                .iter()
                .any(|r| r["messages"].to_string().contains("wait_agent")
                    && r["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "assistant"
                            && m["tool_calls"].to_string().contains("wait_agent"))),
            "不得用显式wait替代正常收尾验证"
        );
        drop(requests);
        let fork = child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{session}/fork"),
            serde_json::json!({}),
        )
        .await;
        let fork_id = fork["id"].as_str().unwrap();
        child_test_json(&fixture.app,"POST",&format!("/session/{fork_id}/message"),serde_json::json!({"text":"HISTORY_CHILD_FACT_PROBE 从分支自己的原件末尾取得校验值后交付"})).await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("fork-tail-check.txt"))
                .ok()
                .as_deref(),
            Some(TAIL),
            "分支不能依赖旧owner引用，必须从本owner真实长Message原件末尾交付；turns={} model={}",
            child_test_json(
                &fixture.app,
                "GET",
                &format!("/session/{fork_id}/turns"),
                serde_json::json!({})
            )
            .await,
            serde_json::to_string(
                &fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
        let fork_read = openpaw_store::catalog::read(fixture.home.path(), fork_id).unwrap();
        assert!(!fork_read.events.iter().any(|event|matches!(event,openpaw_core::SessionEvent::Unknown(v) if matches!(v["type"].as_str(),Some("user_question_v1" | "child_execution_v1")))));
        for _ in 0..2 {
            child_test_json(
                &fixture.app,
                "POST",
                &format!("/session/{fork_id}/message"),
                serde_json::json!({"text":"/compact"}),
            )
            .await;
        }
        std::fs::remove_file(fixture.workspace.path().join("fork-tail-check.txt")).unwrap();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        child_test_json(&restarted,"POST",&format!("/session/{fork_id}/message"),serde_json::json!({"text":"HISTORY_CHILD_FACT_PROBE 多次压缩与重启后从本owner原件末尾重新核对"})).await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("fork-tail-check.txt")).unwrap(),
            TAIL
        );
        let read = openpaw_store::catalog::read(fixture.home.path(), fork_id).unwrap();
        let history = openpaw_store::transcript::replay(fork_id, &read.events).unwrap();
        assert!(
            history.model_contexts.len() >= 2,
            "需真实多次压缩而非仅提示文字"
        );
        for checkpoint in &history.model_contexts {
            assert!(
                checkpoint.source_ids.iter().all(|id| history
                    .messages
                    .iter()
                    .any(|message| message.uuid.as_ref() == Some(id))),
                "checkpoint只能引用真实raw消息"
            );
        }
    }
    #[tokio::test]
    async fn main_history_reused_child_facts_obey_each_parent_source_across_revert_fork_and_commit()
    {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-history-reuse", "option-a").await;
        for (turn, text) in [
            ("one", "HISTORY_REUSE_PARENT_ONE"),
            ("two", "HISTORY_REUSE_PARENT_TWO"),
        ] {
            let result = send_in(&fixture.app, &fixture.team, "sources", turn, text).await;
            let binding = fixture
                .state
                .main_office
                .conversation_record(&fixture.team, "sources")
                .unwrap()
                .unwrap()
                .session
                .unwrap();
            assert_eq!(
                result["state"],
                "complete",
                "{turn}: {result}, turns={}, recent={}",
                child_test_json(
                    &fixture.app,
                    "GET",
                    &format!("/session/{binding}/turns"),
                    serde_json::json!({})
                )
                .await,
                serde_json::to_string(&fixture.model_seen.lock().await.iter().rev().take(4).map(|request|serde_json::json!({"stream":request["stream"],"tools":request["tools"].as_array().map(Vec::len),"messages":request["messages"]})).collect::<Vec<_>>()).unwrap()
            );
        }
        let owner = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "sources")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let source = openpaw_store::catalog::read(fixture.home.path(), &owner).unwrap();
        let history = openpaw_store::transcript::replay(&owner, &source.events).unwrap();
        let second = history
            .messages
            .iter()
            .find(|m| m.role == openpaw_core::Role::User && m.text() == "HISTORY_REUSE_PARENT_TWO")
            .unwrap()
            .uuid
            .clone()
            .unwrap();
        let first_child = crate::runtime::restored_office_children(
            fixture.home.path(),
            &owner,
            fixture.workspace.path(),
        );
        assert_eq!(first_child.len(), 1, "两次必须续办同一真实员工");
        let child = openpaw_store::catalog::read(fixture.home.path(), &first_child[0].0).unwrap();
        let executions = crate::context::child_executions(&child.events)
            .unwrap()
            .into_iter()
            .filter(|r| r.stage == "result_saved")
            .collect::<Vec<_>>();
        assert_eq!(executions.len(), 2);
        assert_ne!(executions[0].child_turn_id, executions[1].child_turn_id);
        let hidden_ref = openpaw_engine::context::record_reference(
            &owner,
            &executions[1].parent_turn_id,
            executions[1].response_index,
            &format!(
                "child-result/{}/{}",
                executions[1].child_session_id, executions[1].child_turn_id
            ),
        );
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{owner}/revert"),
            serde_json::json!({"messageID":second}),
        )
        .await;
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        for expected in ["VISIBLE_ONE", "VISIBLE_TWO"] {
            if expected == "VISIBLE_TWO" {
                child_test_json(
                    &restarted,
                    "POST",
                    &format!("/session/{owner}/unrevert"),
                    serde_json::json!({}),
                )
                .await;
            }
            let fork = child_test_json(
                &restarted,
                "POST",
                &format!("/session/{owner}/fork"),
                serde_json::json!({}),
            )
            .await;
            let fork_id = fork["id"].as_str().unwrap();
            child_test_json(&restarted,"POST",&format!("/session/{fork_id}/message"),serde_json::json!({"text":"HISTORY_VISIBILITY_PROBE 实际读取当前模型可见结果后写入"})).await;
            assert_eq!(
                std::fs::read_to_string(fixture.workspace.path().join("visibility-check.txt"))
                    .unwrap(),
                expected,
                "创建父来源仍可见不能放行第二执行"
            );
        }
        child_test_json(
            &restarted,
            "POST",
            &format!("/session/{owner}/revert"),
            serde_json::json!({"messageID":second}),
        )
        .await;
        assert_eq!(
            send_in(
                &restarted,
                &fixture.team,
                "sources",
                "commit",
                "HISTORY_VISIBILITY_PROBE 提交裁剪后检查"
            )
            .await["state"],
            "complete"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("visibility-check.txt")).unwrap(),
            "VISIBLE_ONE"
        );
        let denied = send_in(
            &restarted,
            &fixture.team,
            "sources",
            "read-hidden",
            &format!("HISTORY_READ_REF {hidden_ref}"),
        )
        .await;
        assert!(
            !denied["text"].as_str().unwrap().contains("CHILD_VALUE_TWO"),
            "已提交隐藏来源的原件必须拒绝：{denied}"
        );
        assert!(
            denied["text"].as_str().unwrap().contains("不存在")
                || denied["text"].as_str().unwrap().contains("隐藏")
        );
        let old = &executions[0];
        let query = send_in(
            &restarted,
            &fixture.team,
            "sources",
            "query-old",
            &format!(
                "HISTORY_QUERY_OLD {} {}",
                old.child_session_id, old.child_turn_id
            ),
        )
        .await;
        assert!(
            !query["text"].as_str().unwrap().contains("CHILD_VALUE_ONE"),
            "旧结果事实不授予本轮查询许可：{query}"
        );
    }
    #[tokio::test]
    async fn main_history_question_projection_uses_actual_calls_and_visible_prefix() {
        use openpaw_core::user_input::UserQuestionState;
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-choice", "option-a").await;
        let result = send(fixture.app.clone(), fixture.team.clone()).await;
        assert_eq!(result["text"], "已采用方案A");
        let owner = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "history")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let events = openpaw_store::catalog::read(fixture.home.path(), &owner)
            .unwrap()
            .events;
        let history = openpaw_store::transcript::replay(&owner, &events).unwrap();
        let visible = history.visible_messages(&owner);
        let positions = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                openpaw_core::SessionEvent::Unknown(v) if v["type"] == "user_question_v1" => {
                    Some(index)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (position, state) in positions.iter().zip([
            UserQuestionState::Queued,
            UserQuestionState::Waiting,
            UserQuestionState::Answered,
        ]) {
            let projected =
                crate::context::validated_questions(&owner, &events[..=*position], visible);
            assert_eq!(projected.len(), 1, "真实提交前缀应保留合法问题");
            assert_eq!(projected[0].state, state);
        }
        assert!(
            crate::context::validated_questions(&owner, &events, &[]).is_empty(),
            "隐藏来源不得读取"
        );
        for field in ["answer", "scope", "question_id", "request", "display_id"] {
            let mut invalid = events.clone();
            let openpaw_core::SessionEvent::Unknown(value) =
                &mut invalid[*positions.last().unwrap()]
            else {
                unreachable!()
            };
            match field {
                "answer" => value["record"][field]["choice_id"] = serde_json::json!("forged"),
                "scope" => {
                    value["record"][field]["source_user_message_id"] = serde_json::json!("foreign")
                }
                "request" => value["record"][field]["question"] = serde_json::json!("另一个问题"),
                _ => value["record"][field] = serde_json::json!("forged"),
            }
            assert!(
                crate::context::validated_questions(&owner, &invalid, visible)
                    .iter()
                    .all(|record| record.state != UserQuestionState::Answered),
                "{field}错配不能成为已答事实"
            );
        }
    }
}

#[cfg(test)]
mod restart_question_tests {
    use super::*;

    fn response(call: Option<(&str, serde_json::Value)>, text: &str) -> Response {
        let delta = if let Some((name, args)) = call {
            serde_json::json!({"tool_calls":[{"index":0,"id":"restart-call","type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        } else {
            serde_json::json!({"content":text})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if delta["tool_calls"].is_array(){"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }
    /// 功能：按真实模型输入生成可靠重启事实验收脚本。
    /// 参数：body为本地模型收到的请求。
    /// 返回：对应场景的流式工具或文本响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：不直接修改会话或可靠记录。
    /// 并发与取消：同步生成响应，不持有执行资源。
    /// 权限：工具调用仍经真实Host校验。
    /// 关联：CHG-2026-0054、restart_question_tests。
    pub(super) fn model_response(body: &serde_json::Value) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let Some(source) = messages.iter().rposition(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("RESTART_"))
        }) else {
            return response(None, "真实执行已返回");
        };
        let text = messages[source]["content"].as_str().unwrap();
        let has_result = messages[source..].iter().any(|m| m["role"] == "tool");
        let parent = body["tools"].to_string().contains("start_agent");
        if (text == "RESTART_WRITE_FAIL" && has_result)
            || (text == "RESTART_START_FAIL"
                && messages[source..].iter().any(|m| {
                    m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("真实执行已返回"))
                }))
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if text == "RESTART_WRITE_FAIL" {
            return response(
                Some((
                    "edit_file",
                    serde_json::json!({"path":"restart-side-effect.txt","old_string":"原始","new_string":"已修改"}),
                )),
                "",
            );
        }
        if text.starts_with("RESTART_CONTINUE_CHILD") || text == "RESTART_CONTINUE_SIDE_EFFECT" {
            let expected = if text.ends_with("_ANSWER") {
                "option-a"
            } else if text.ends_with("SIDE_EFFECT") {
                "restart-side-effect.txt"
            } else if text.ends_with("_PAUSED") {
                "status=paused"
            } else if text.ends_with("_CANCELLED") {
                "status=cancelled"
            } else {
                "真实执行已返回"
            };
            if has_result {
                return response(
                    None,
                    if messages[source..].iter().any(|m| {
                        m["role"] == "tool"
                            && m["content"].as_str().is_some_and(|s| s.contains(expected))
                    }) {
                        if text.ends_with("_ANSWER") {
                            "CHILD_ANSWER_REUSED"
                        } else if text.ends_with("SIDE_EFFECT") {
                            "SIDE_EFFECT_REUSED"
                        } else if text.ends_with("_PAUSED") {
                            "CHILD_PAUSED"
                        } else if text.ends_with("_CANCELLED") {
                            "CHILD_CANCELLED"
                        } else {
                            "CHILD_RESULT_REUSED"
                        }
                    } else {
                        "NO_CHILD_ORIGINAL"
                    },
                );
            }
            let fact = messages
                .iter()
                .filter_map(|m| m["content"].as_str())
                .flat_map(str::lines)
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|v| {
                    v["kind"]
                        == if text.ends_with("_ANSWER") {
                            "user_question_fact"
                        } else if text.ends_with("SIDE_EFFECT") {
                            "tool_result_fact"
                        } else {
                            "child_result_preview"
                        }
                });
            if let Some(fact) = fact {
                return response(
                    Some((
                        "read_context_record",
                        serde_json::json!({"reference":fact["reference"],"start":0,"limit":4096}),
                    )),
                    "",
                );
            }
            return response(None, "NO_CHILD_FACT");
        }
        if has_result {
            return response(None, "真实执行已返回");
        }
        if text == "RESTART_CONTINUE_ANSWER"
            && messages
                .iter()
                .filter_map(|m| m["content"].as_str())
                .any(|s| s.contains("choice_id") && s.contains("option-a"))
        {
            return response(None, "ANSWER_REUSED");
        }
        if matches!(
            text,
            "RESTART_QUESTION" | "RESTART_CHILD_QUESTION" | "RESTART_CONTINUE_ANSWER"
        ) {
            return response(
                Some((
                    "request_user_input",
                    serde_json::json!({"question":"请选择交付方式","options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]}),
                )),
                "",
            );
        }
        if !parent {
            return response(
                Some((
                    "write_file",
                    serde_json::json!({"path":if text.ends_with("_SECOND"){"restart-second.txt"}else{"restart-first.txt"},"content":"真实子产物"}),
                )),
                "",
            );
        }
        let previous = messages[..source]
            .iter()
            .filter(|m| m["role"] == "tool")
            .find_map(|m| {
                serde_json::from_str::<serde_json::Value>(m["content"].as_str()?)
                    .ok()
                    .filter(|r| r["session_id"].is_string())
            });
        let mut args = serde_json::json!({"name":"重启测试员","task":"RESTART_CHILD"});
        if text == "RESTART_START_QUESTION" {
            args["task"] = serde_json::json!("RESTART_CHILD_QUESTION");
        }
        if text == "RESTART_RESUME" {
            args["task_id"] = previous.unwrap()["session_id"].clone();
            args["task"] = serde_json::json!("RESTART_CHILD_SECOND");
        }
        if let Some(id) = text.strip_prefix("RESTART_RESUME_ANSWER ") {
            args["task_id"] = serde_json::json!(id);
            args["task"] = serde_json::json!("RESTART_CONTINUE_ANSWER");
        }
        response(Some(("start_agent", args)), "")
    }
    async fn request(app: &Router, team: &str, operation: serde_json::Value) -> Response {
        let mut body =
            serde_json::json!({"version":"1.0","teamId":team,"conversationId":"restart"});
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
    async fn send(app: &Router, team: &str, turn: &str, text: &str) -> serde_json::Value {
        let history = request(app, team, serde_json::json!({"type":"history"})).await;
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(history.into_body(), 2_000_000).await.unwrap())
                .unwrap();
        let mut messages = Vec::new();
        for row in history["data"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["state"] == "complete")
        {
            messages.push(serde_json::json!({"role":"user","content":row["user"]}));
            messages.push(serde_json::json!({"role":"assistant","content":row["assistant"]}));
        }
        messages.push(serde_json::json!({"role":"user","content":text}));
        let result = request(
            app,
            team,
            serde_json::json!({"type":"send","turnId":turn,"messages":messages}),
        )
        .await;
        assert_eq!(result.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(result.into_body(), 2_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }
    fn copy_directory(source: &std::path::Path, target: &std::path::Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let to = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_directory(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    /// 功能：复制真实fixture后截取其可靠事件前缀。
    /// 参数：fixture为隔离源，id为原会话，cut为真实断点。
    /// 返回：新建的独立临时home。
    /// 错误：断点不存在或复制失败时测试失败。
    /// 状态：不修改原fixture、工作目录或生产记录。
    /// 并发与取消：同步复制已可靠事件，不恢复future。
    /// 权限：仅操作本测试临时目录。
    /// 关联：08冷加载与启动恢复测试共用。
    pub(super) fn snapshot(
        fixture: &MainFixture,
        id: &str,
        cut: impl Fn(&serde_json::Value) -> bool,
    ) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for dir in ["sessions", "offices"] {
            let source = fixture.home.path().join(dir);
            if source.exists() {
                copy_directory(&source, &home.path().join(dir));
            }
        }
        truncate_prefix(home.path(), id, cut);
        home
    }
    fn truncate_prefix(home: &std::path::Path, id: &str, cut: impl Fn(&serde_json::Value) -> bool) {
        let path = home.join("sessions").join(format!("{id}.jsonl"));
        let original = std::fs::read_to_string(&path).unwrap();
        let mut prefix = String::new();
        let mut found = false;
        for line in original.lines() {
            prefix.push_str(line);
            prefix.push('\n');
            if cut(&serde_json::from_str(line).unwrap()) {
                found = true;
                break;
            }
        }
        assert!(found, "必须截取真实已保存断点，不造业务事实");
        std::fs::write(path, prefix).unwrap();
    }
    /// 功能：以合成home重建实际Host进程状态。
    /// 参数：fixture提供原本地模型和Office端点，home为独立存储。
    /// 返回：尚未恢复执行的全新AppState。
    /// 错误：原fixture端点缺失时测试失败。
    /// 状态：不恢复旧future、模型请求或卡片。
    /// 并发与取消：新建运行时，不共享原会话锁和writer。
    /// 权限：沿原配置，不连接真实模型或15180。
    /// 关联：08服务重启fixture。
    pub(super) fn rebuild(fixture: &MainFixture, home: &std::path::Path) -> Arc<AppState> {
        let mut config = fixture.state.config.clone();
        config.home = home.into();
        let mut state = AppState::new(config.clone());
        let base = std::env::var("OPENPAW_OPENAI_BASE_URL").unwrap();
        Arc::get_mut(&mut state).unwrap().main_office = crate::office::MainOffice::new(
            home,
            &config.workspace,
            Some(base.trim_end_matches("/v1").into()),
        );
        state.office.attach_main(&state.main_office);
        state
    }
    fn parent_id(fixture: &MainFixture) -> String {
        fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "restart")
            .unwrap()
            .unwrap()
            .session
            .unwrap()
    }
    /// 功能：读取隔离journal中的实际持久问题记录。
    /// 参数：home为测试存储根，id为真实会话。
    /// 返回：按保存顺序排列的原记录。
    /// 错误：原journal不可读时测试失败。
    /// 状态：只读，不伪造已答或重启状态。
    /// 并发与取消：读取可靠快照，不获取执行许可。
    /// 权限：仅使用测试提供的真实owner。
    /// 关联：08公开行为及可靠前缀验证。
    pub(super) fn question_records(home: &std::path::Path, id: &str) -> Vec<serde_json::Value> {
        let read =
            openpaw_store::SessionReader::read(home.join("sessions").join(format!("{id}.jsonl")))
                .unwrap();
        read.events
            .iter()
            .filter_map(|event| match event {
                openpaw_core::SessionEvent::Unknown(v) if v["type"] == "user_question_v1" => {
                    Some(v["record"].clone())
                }
                _ => None,
            })
            .collect()
    }
    #[tokio::test]
    async fn main_restart_marks_queued_and_waiting_questions_interrupted_without_execution() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-restart-question", "option-a").await;
        assert_eq!(
            send(&fixture.app, &fixture.team, "question", "RESTART_QUESTION").await["state"],
            "complete"
        );
        let id = parent_id(&fixture);
        let calls = fixture.model_seen.lock().await.len();
        let cards = fixture
            .seen
            .lock()
            .await
            .iter()
            .filter(|m| m["type"] == "decision.request")
            .count();
        for phase in ["queued", "waiting"] {
            for startup in [false, true] {
                let home = snapshot(&fixture, &id, |event| {
                    event["type"] == "user_question_v1" && event["record"]["state"] == phase
                });
                let original = question_records(home.path(), &id).pop().unwrap();
                let state = rebuild(&fixture, home.path());
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, 1);
                }
                let app = build_router(state);
                let turns = child_test_json(
                    &app,
                    "GET",
                    &format!("/session/{id}/turns"),
                    serde_json::json!({}),
                )
                .await;
                assert_eq!(turns[0]["state"], "failed");
                assert_eq!(turns[0]["error"]["code"], "host_restarted");
                let record = question_records(home.path(), &id).pop().unwrap();
                assert_eq!(
                    record["state"], "interrupted",
                    "{phase}经过{startup}恢复必须结束旧问题"
                );
                for field in [
                    "question_id",
                    "display_id",
                    "generation",
                    "deadline_ms",
                    "tool_call_id",
                    "scope",
                ] {
                    assert_eq!(record[field], original[field]);
                }
                assert!(record["answer"].is_null());
                assert_eq!(
                    child_test_json(&app, "GET", "/session/status", serde_json::json!({})).await,
                    serde_json::json!({})
                );
            }
        }
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "重启不能自动请求模型"
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            cards,
            "重启不能重投卡片"
        );
    }
    #[tokio::test]
    async fn main_restart_reuses_durable_answer_before_missing_tool_result_and_after_another_restart(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-restart-question", "option-a").await;
        assert_eq!(
            send(&fixture.app, &fixture.team, "question", "RESTART_QUESTION").await["state"],
            "complete"
        );
        let id = parent_id(&fixture);
        let cards = fixture
            .seen
            .lock()
            .await
            .iter()
            .filter(|m| m["type"] == "decision.request")
            .count();
        for startup in [false, true] {
            let home = snapshot(&fixture, &id, |event| {
                event["type"] == "user_question_v1" && event["record"]["state"] == "answered"
            });
            let state = rebuild(&fixture, home.path());
            let calls = fixture.model_seen.lock().await.len();
            if startup {
                assert_eq!(state.recover_orphaned_turns().await, 1);
            }
            let app = build_router(state);
            let messages = child_test_json(
                &app,
                "GET",
                &format!("/session/{id}/message"),
                serde_json::json!({}),
            )
            .await;
            let question = messages
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|m| m["parts"].as_array().unwrap())
                .find(|part| part["tool"] == "request_user_input")
                .unwrap();
            assert_eq!(
                question["state"]["status"], "completed",
                "可靠接受答案必须先于unknown结果恢复"
            );
            assert!(question["state"].to_string().contains("option-a"));
            assert_eq!(
                fixture.model_seen.lock().await.len(),
                calls,
                "恢复本身不启动模型"
            );
            let result = send(&app, &fixture.team, "continue", "RESTART_CONTINUE_ANSWER").await;
            assert_eq!(result["state"], "complete");
            assert_eq!(
                result["text"], "ANSWER_REUSED",
                "用户明确继续时模型必须真正使用可靠答案"
            );
            let next = build_router(rebuild(&fixture, home.path()));
            assert_eq!(
                send(
                    &next,
                    &fixture.team,
                    "continue-again",
                    "RESTART_CONTINUE_ANSWER"
                )
                .await["text"],
                "ANSWER_REUSED"
            );
            assert_eq!(
                question_records(home.path(), &id)
                    .iter()
                    .filter(|q| q["state"] == "answered")
                    .count(),
                1
            );
        }
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            cards,
            "已知答案不能重启后重复提问"
        );
    }
    #[tokio::test]
    async fn main_restart_paused_or_cancelled_child_never_becomes_completed_or_accepts_late_answer()
    {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for stopped in [false, true] {
            let fixture = main_fixture(
                "question-restart-paused-child",
                if stopped {
                    "held-answer"
                } else {
                    "wrong-label"
                },
            )
            .await;
            if stopped {
                let app = fixture.app.clone();
                let team = fixture.team.clone();
                let pending = tokio::spawn(async move {
                    send(&app, &team, "question", "RESTART_START_QUESTION").await
                });
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if fixture
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
                let cancelled = request(
                    &fixture.app,
                    &fixture.team,
                    serde_json::json!({"type":"cancel","turnId":"question"}),
                )
                .await;
                assert_eq!(cancelled.status(), StatusCode::OK);
                assert_ne!(pending.await.unwrap()["state"], "complete");
            } else {
                assert_eq!(
                    send(
                        &fixture.app,
                        &fixture.team,
                        "question",
                        "RESTART_START_QUESTION"
                    )
                    .await["state"],
                    "complete"
                );
            }
            let id = parent_id(&fixture);
            let sessions =
                child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
            let child = sessions
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["parentID"] == id)
                .unwrap()["id"]
                .as_str()
                .unwrap();
            assert_eq!(
                question_records(fixture.home.path(), child).last().unwrap()["state"],
                if stopped { "cancelled" } else { "interrupted" }
            );
            let cards = fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count();
            for startup in [false, true] {
                let home = snapshot(&fixture, &id, |event| {
                    if stopped {
                        event["type"] == "turn_outcome" && event["state"] == "cancelled"
                    } else {
                        event["type"] == "execution_attempt_v1"
                            && event["kind"] == "tool_result"
                            && event["payload"]["name"] == "start_agent"
                    }
                });
                truncate_prefix(home.path(), child, |event| {
                    if stopped {
                        event["type"] == "turn_outcome" && event["state"] == "cancelled"
                    } else {
                        event["type"] == "turn_finish_commit_v1"
                    }
                });
                let state = rebuild(&fixture, home.path());
                let calls = fixture.model_seen.lock().await.len();
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, usize::from(!stopped));
                }
                let app = build_router(state);
                assert_eq!(
                    request(&app, &fixture.team, serde_json::json!({"type":"history"}))
                        .await
                        .status(),
                    StatusCode::OK
                );
                fixture.release_model.notify_waiters();
                tokio::task::yield_now().await;
                assert_eq!(
                    fixture.model_seen.lock().await.len(),
                    calls,
                    "恢复/迟到答案不得唤醒执行"
                );
                let read = openpaw_store::SessionReader::read(
                    home.path().join("sessions").join(format!("{child}.jsonl")),
                )
                .unwrap();
                let saved = crate::context::child_executions(&read.events).unwrap();
                assert_eq!(
                    saved.last().unwrap().status.as_deref(),
                    Some(if stopped { "cancelled" } else { "paused" })
                );
                assert!(saved.last().unwrap().outcome.as_ref().unwrap().is_error);
                let result = send(
                    &app,
                    &fixture.team,
                    "explicit-read",
                    if stopped {
                        "RESTART_CONTINUE_CHILD_CANCELLED"
                    } else {
                        "RESTART_CONTINUE_CHILD_PAUSED"
                    },
                )
                .await;
                assert_eq!(
                    result["text"],
                    if stopped {
                        "CHILD_CANCELLED"
                    } else {
                        "CHILD_PAUSED"
                    }
                );
                assert_eq!(
                    child_test_json(
                        &app,
                        "GET",
                        &format!("/session/{child}/turns"),
                        serde_json::json!({})
                    )
                    .await
                    .as_array()
                    .unwrap()
                    .len(),
                    1
                );
                assert!(!question_records(home.path(), child)
                    .iter()
                    .any(|q| q["state"] == "answered"));
                assert_eq!(
                    child_test_json(&app, "GET", "/session/status", serde_json::json!({})).await,
                    serde_json::json!({})
                );
            }
            assert_eq!(
                fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request")
                    .count(),
                cards
            );
        }
    }
    #[tokio::test]
    async fn main_restart_model_failure_keeps_known_side_effect_and_child_result_without_replay() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for (source, continuation, expected) in [
            (
                "RESTART_WRITE_FAIL",
                "RESTART_CONTINUE_SIDE_EFFECT",
                "SIDE_EFFECT_REUSED",
            ),
            (
                "RESTART_START_FAIL",
                "RESTART_CONTINUE_CHILD",
                "CHILD_RESULT_REUSED",
            ),
        ] {
            let fixture = main_fixture("question-restart-model-failure", "option-a").await;
            std::fs::write(
                fixture.workspace.path().join("restart-side-effect.txt"),
                "原始",
            )
            .unwrap();
            let result = send(&fixture.app, &fixture.team, source, source).await;
            assert_ne!(
                result["state"], "complete",
                "{source}此断点必须经过真实后续模型失败"
            );
            let id = parent_id(&fixture);
            for startup in [false, true] {
                let home = snapshot(&fixture, &id, |event| {
                    (event["type"] == "turn_outcome" && event["state"] == "failed")
                        || (event["type"] == "turn_finish_commit_v1"
                            && event["terminal"]["state"] == "failed")
                });
                let state = rebuild(&fixture, home.path());
                let calls = fixture.model_seen.lock().await.len();
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, 0);
                }
                let app = build_router(state);
                assert_eq!(
                    request(&app, &fixture.team, serde_json::json!({"type":"history"}))
                        .await
                        .status(),
                    StatusCode::OK
                );
                assert_eq!(fixture.model_seen.lock().await.len(), calls);
                assert_eq!(
                    send(&app, &fixture.team, "explicit-continue", continuation).await["text"],
                    expected,
                    "generated回滚不应丢失可靠工具/子执行事实"
                );
                if source == "RESTART_WRITE_FAIL" {
                    let messages = child_test_json(
                        &app,
                        "GET",
                        &format!("/session/{id}/message"),
                        serde_json::json!({}),
                    )
                    .await;
                    assert_eq!(
                        messages
                            .as_array()
                            .unwrap()
                            .iter()
                            .flat_map(|m| m["parts"].as_array().unwrap())
                            .filter(|p| p["tool"] == "edit_file")
                            .count(),
                        1,
                        "已知副作用不得重试"
                    );
                    assert_eq!(
                        std::fs::read_to_string(
                            fixture.workspace.path().join("restart-side-effect.txt")
                        )
                        .unwrap(),
                        "已修改"
                    );
                } else {
                    let sessions =
                        child_test_json(&app, "GET", "/session", serde_json::json!({})).await;
                    assert_eq!(
                        sessions
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|s| s["parentID"] == id)
                            .count(),
                        1
                    );
                    assert_eq!(
                        std::fs::read_to_string(fixture.workspace.path().join("restart-first.txt"))
                            .unwrap(),
                        "真实子产物"
                    );
                }
            }
        }
    }
    #[tokio::test]
    async fn main_restart_child_pending_and_accepted_answer_require_explicit_continuation() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-restart-child-question", "option-a").await;
        assert_eq!(
            send(
                &fixture.app,
                &fixture.team,
                "question",
                "RESTART_START_QUESTION"
            )
            .await["state"],
            "complete"
        );
        let id = parent_id(&fixture);
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"] == id)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let cards = fixture
            .seen
            .lock()
            .await
            .iter()
            .filter(|m| m["type"] == "decision.request")
            .count();
        for phase in ["queued", "waiting", "answered"] {
            for startup in [false, true] {
                let home = snapshot(&fixture, &id, |event| {
                    event["type"] == "execution_attempt_v1"
                        && event["kind"] == "tool_result"
                        && event["payload"]["name"] == "start_agent"
                });
                truncate_prefix(home.path(), child, |event| {
                    event["type"] == "user_question_v1" && event["record"]["state"] == phase
                });
                let original = question_records(home.path(), child).pop().unwrap();
                let calls = fixture.model_seen.lock().await.len();
                let state = rebuild(&fixture, home.path());
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, 2);
                }
                let app = build_router(state);
                let history =
                    request(&app, &fixture.team, serde_json::json!({"type":"history"})).await;
                assert_eq!(history.status(), StatusCode::OK);
                let saved = question_records(home.path(), child).pop().unwrap();
                assert_eq!(
                    saved["state"],
                    if phase == "answered" {
                        "answered"
                    } else {
                        "interrupted"
                    }
                );
                for key in [
                    "question_id",
                    "scope",
                    "tool_call_id",
                    "display_id",
                    "generation",
                    "deadline_ms",
                ] {
                    assert_eq!(saved[key], original[key]);
                }
                assert_eq!(
                    fixture.model_seen.lock().await.len(),
                    calls,
                    "恢复不得执行模型或工具"
                );
                assert_eq!(
                    child_test_json(&app, "GET", "/session/status", serde_json::json!({})).await,
                    serde_json::json!({})
                );
                let read = openpaw_store::SessionReader::read(
                    home.path().join("sessions").join(format!("{child}.jsonl")),
                )
                .unwrap();
                assert!(
                    !crate::context::child_executions(&read.events)
                        .unwrap()
                        .iter()
                        .any(|r| r.status.as_deref() == Some("completed")),
                    "已答不代表子任务交付"
                );
                if phase == "answered" {
                    assert_eq!(
                        send(
                            &app,
                            &fixture.team,
                            "parent-continue",
                            "RESTART_CONTINUE_CHILD_ANSWER"
                        )
                        .await["text"],
                        "CHILD_ANSWER_REUSED"
                    );
                    let resumed = send(
                        &app,
                        &fixture.team,
                        "child-continue",
                        &format!("RESTART_RESUME_ANSWER {child}"),
                    )
                    .await;
                    assert_eq!(resumed["state"], "complete");
                    let turns = child_test_json(
                        &app,
                        "GET",
                        &format!("/session/{child}/turns"),
                        serde_json::json!({}),
                    )
                    .await;
                    assert_eq!(
                        turns.as_array().unwrap().len(),
                        2,
                        "仅明确续办才创建新子turn"
                    );
                    let messages = child_test_json(
                        &app,
                        "GET",
                        &format!("/session/{child}/message"),
                        serde_json::json!({}),
                    )
                    .await;
                    assert!(
                        messages.to_string().contains("ANSWER_REUSED"),
                        "既有子会话自身也必须使用保存答案"
                    );
                    assert_eq!(
                        question_records(home.path(), child)
                            .iter()
                            .filter(|q| q["state"] == "answered")
                            .count(),
                        1
                    );
                }
            }
        }
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            cards,
            "重启与续办不重复已答题或恢复旧卡片"
        );
    }
    #[tokio::test]
    async fn main_restart_recovers_child_terminal_before_result_saved_and_parent_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-restart-source", "option-a").await;
        assert_eq!(
            send(&fixture.app, &fixture.team, "first", "RESTART_START").await["state"],
            "complete"
        );
        let id = parent_id(&fixture);
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let child = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["parentID"] == id)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        for (saved, accepted) in [(false, false), (false, true), (true, false), (true, true)] {
            for startup in [false, true] {
                let home = snapshot(&fixture, &id, |event| {
                    event["type"] == "execution_attempt_v1"
                        && event["kind"]
                            == if accepted {
                                "tool_result"
                            } else {
                                "tool_start"
                            }
                        && event["payload"]["name"] == "start_agent"
                });
                if !saved {
                    truncate_prefix(home.path(), child, |event| {
                        event["type"] == "turn_finish_commit_v1"
                    });
                }
                let state = rebuild(&fixture, home.path());
                let calls = fixture.model_seen.lock().await.len();
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, 1);
                }
                let app = build_router(state);
                let loaded =
                    request(&app, &fixture.team, serde_json::json!({"type":"history"})).await;
                assert_eq!(loaded.status(), StatusCode::OK);
                assert_eq!(
                    fixture.model_seen.lock().await.len(),
                    calls,
                    "父会话加载不得自动请求模型"
                );
                let result = send(&app, &fixture.team, "continue", "RESTART_CONTINUE_CHILD").await;
                assert_eq!(result["state"], "complete");
                assert_eq!(
                    result["text"], "CHILD_RESULT_REUSED",
                    "父入口必须直接复用子事实，不依赖先打开子详情"
                );
                let turns = child_test_json(
                    &app,
                    "GET",
                    &format!("/session/{child}/turns"),
                    serde_json::json!({}),
                )
                .await;
                assert_eq!(
                    turns.as_array().unwrap().len(),
                    1,
                    "续办只能读取旧子结果，不能重跑子turn"
                );
                assert_eq!(turns[0]["state"], "completed");
                let read = openpaw_store::SessionReader::read(
                    home.path().join("sessions").join(format!("{child}.jsonl")),
                )
                .unwrap();
                let records = read
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        openpaw_core::SessionEvent::Unknown(v)
                            if v["type"] == "child_execution_v1" =>
                        {
                            Some(v["record"].clone())
                        }
                        _ => None,
                    })
                    .filter(|r| r["stage"] == "result_saved")
                    .collect::<Vec<_>>();
                assert_eq!(
                    records.len(),
                    1,
                    "已提交真实子终态必须在重启后作为已知结果保留"
                );
                assert_eq!(records[0]["status"], "completed");
                assert!(records[0]["outcome"]["content"]
                    .as_str()
                    .unwrap()
                    .contains("真实执行已返回"));
                assert_eq!(
                    std::fs::read_to_string(fixture.workspace.path().join("restart-first.txt"))
                        .unwrap(),
                    "真实子产物"
                );
                assert_eq!(
                    child_test_json(&app, "GET", "/session", serde_json::json!({}))
                        .await
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|s| s["parentID"].is_string())
                        .count(),
                    1,
                    "不能通过新建执行恢复旧结果"
                );
            }
        }
    }
    #[tokio::test]
    async fn main_restart_retains_each_execution_source_and_saved_child_result() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-restart-source", "option-a").await;
        assert_eq!(
            send(&fixture.app, &fixture.team, "first", "RESTART_START").await["state"],
            "complete"
        );
        assert_eq!(
            send(&fixture.app, &fixture.team, "second", "RESTART_RESUME").await["state"],
            "complete"
        );
        let sessions =
            child_test_json(&fixture.app, "GET", "/session", serde_json::json!({})).await;
        let children = sessions
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["parentID"].is_string())
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 1, "续办必须是同一真实员工");
        let child = children[0];
        let read = openpaw_store::SessionReader::read(
            fixture
                .home
                .path()
                .join("sessions")
                .join(format!("{}.jsonl", child["id"].as_str().unwrap())),
        )
        .unwrap();
        let records = read
            .events
            .iter()
            .filter_map(|event| match event {
                openpaw_core::SessionEvent::Unknown(v) if v["type"] == "child_execution_v1" => {
                    Some(v["record"].clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let accepted = records
            .iter()
            .filter(|r| r["stage"] == "accepted")
            .collect::<Vec<_>>();
        let saved = records
            .iter()
            .filter(|r| r["stage"] == "result_saved")
            .collect::<Vec<_>>();
        assert_eq!(accepted.len(), 2, "每次真实子执行都必须可靠保存确切父来源");
        assert_eq!(saved.len(), 2, "结果通知前必须保存真实完整子结果");
        assert_ne!(
            accepted[0]["parent_user_message_id"],
            accepted[1]["parent_user_message_id"]
        );
        assert_ne!(accepted[0]["parent_turn_id"], accepted[1]["parent_turn_id"]);
        assert_ne!(accepted[0]["child_turn_id"], accepted[1]["child_turn_id"]);
        for record in saved {
            assert_eq!(record["parent_session_id"], child["parentID"]);
            assert_eq!(record["tool_call_id"], "restart-call");
            assert_eq!(record["status"], "completed");
            assert!(record["outcome"]["content"]
                .as_str()
                .unwrap()
                .contains("真实执行已返回"));
        }
        for file in ["restart-first.txt", "restart-second.txt"] {
            assert_eq!(
                std::fs::read_to_string(fixture.workspace.path().join(file)).unwrap(),
                "真实子产物"
            );
        }
    }
}

#[cfg(test)]
mod goal_question_tests {
    use super::*;

    fn task(id: &str, dependencies: &[&str]) -> serde_json::Value {
        serde_json::json!({"id":id,"title":format!("任务{id}"),"expected_result":format!("{id}.txt"),"acceptance_criteria":["文件可读"],"dependencies":dependencies})
    }
    fn tasks() -> serde_json::Value {
        serde_json::json!([task("a", &[]), task("b", &[]), task("c", &["a"])])
    }
    fn chunk(index: usize, tool: Option<(&str, serde_json::Value)>) -> Response {
        chunk_named(&format!("gq-{index}"), tool)
    }
    fn chunk_named(id: &str, tool: Option<(&str, serde_json::Value)>) -> Response {
        let delta = match tool {
            Some((name, args)) => {
                serde_json::json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
            }
            None => serde_json::json!({"content":"当前仍待确认或补充条件"}),
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta,"finish_reason":if delta["tool_calls"].is_array(){"tool_calls"}else{"stop"}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    }
    fn question() -> serde_json::Value {
        serde_json::json!({"question":"请选择交付方式","options":[{"id":"option-a","label":"方案A"},{"id":"option-b","label":"方案B"}]})
    }
    /// 功能：生成Goal提问及暂停边界的本地模型响应。
    /// 参数：body为真实请求，scenario选择脚本，questions为Office观察，_gate为共享门闩。
    /// 返回：与当前真实模型输入对应的流式响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：只生成模型效果，不自行验收Goal。
    /// 并发与取消：等待本地观察可取消，不持有工作区锁。
    /// 权限：问题回答不批准Goal或工具执行。
    /// 关联：CHG-2026-0054、goal_question_tests。
    pub(super) async fn model_response(
        body: &serde_json::Value,
        scenario: &str,
        questions: &tokio::sync::Mutex<Vec<serde_json::Value>>,
        _gate: &tokio::sync::Notify,
    ) -> Response {
        if scenario.starts_with("question-goal-choice") {
            return goal_review_tests::choice_model_response(body, scenario);
        }
        let messages = body["messages"].as_array().unwrap();
        let start = messages
            .iter()
            .rposition(|m| {
                m["role"] == "user"
                    && m["content"].as_str().is_some_and(|s| {
                        s.starts_with("/goal ") || s == "确认执行" || s.starts_with("goal-child-")
                    })
            })
            .unwrap();
        let text = messages[start]["content"].as_str().unwrap();
        let results = messages[start..]
            .iter()
            .filter(|m| {
                m["role"] == "tool"
                    && !m["tool_call_id"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("gq-extra-")
            })
            .collect::<Vec<_>>();
        let current = results.iter().find_map(|m| {
            serde_json::from_str::<serde_json::Value>(m["content"].as_str()?)
                .ok()
                .filter(|value| value["id"].is_string())
        });
        if text.starts_with("goal-child-") {
            return chunk(
                results.len(),
                match (text, results.len()) {
                    ("goal-child-a", 0) => Some(("request_user_input", question())),
                    ("goal-child-a", 1) => Some((
                        "bash",
                        serde_json::json!({"command":"printf 'A\\n' >> a.txt"}),
                    )),
                    ("goal-child-b", 0) => Some((
                        "write_file",
                        serde_json::json!({"path":"b.txt","content":"独立B已交付"}),
                    )),
                    ("goal-child-bypass", 0) => Some((
                        "write_file",
                        serde_json::json!({"path":"bypass.txt","content":"不应在超时后重派"}),
                    )),
                    _ => None,
                },
            );
        }
        if text == "确认执行" || text == "/goal continue" {
            let update = |action: &str, id: &str, extra: serde_json::Value| {
                let mut args = serde_json::json!({"action":action,"task_id":id,"goal_id":current.as_ref().unwrap()["id"],"expected_version":1,"reason":"按真实结果检查当前任务"});
                args.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                Some(("goal_update", args))
            };
            let evidence = |index: usize| {
                let id = results[index]["content"]
                    .as_str()
                    .unwrap()
                    .rsplit("Goal observation: ")
                    .next()
                    .unwrap()
                    .trim();
                serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"真实文件逐项检查通过"}]})
            };
            let reference = |index: usize| {
                serde_json::from_str::<serde_json::Value>(
                    results[index]["content"]
                        .as_str()
                        .unwrap()
                        .split("\nGoal observation:")
                        .next()
                        .unwrap(),
                )
                .unwrap()
            };
            // 显式等待可先返回waiting/running变化；继续事件等待到真实终态，再验收。
            let awaited = if text == "/goal continue" && results.len() == 5 {
                Some(("a", reference(3)))
            } else if text != "/goal continue" && !scenario.contains("root") && results.len() == 7 {
                Some(("a", reference(1)))
            } else if !scenario.contains("root")
                && ((scenario.ends_with("pause") && results.len() == 13)
                    || (!scenario.ends_with("pause") && results.len() == 11))
                && text != "/goal continue"
            {
                Some(("b", reference(3)))
            } else {
                None
            };
            if let Some((label, child)) = awaited {
                let terminal = messages[start..].iter().any(|m| {
                    m["role"] == "tool"
                        && m["content"]
                            .as_str()
                            .and_then(|s| {
                                serde_json::from_str::<serde_json::Value>(
                                    s.split("\nGoal observation:").next().unwrap(),
                                )
                                .ok()
                            })
                            .is_some_and(|r| {
                                r["session_id"] == child["session_id"]
                                    && r["turn_id"] == child["turn_id"]
                                    && matches!(
                                        r["status"].as_str(),
                                        Some("completed" | "paused" | "failed" | "cancelled")
                                    )
                            })
                });
                if !terminal {
                    return chunk_named(
                        &format!("gq-extra-{label}-{}", messages.len()),
                        Some((
                            "wait_agent",
                            serde_json::json!({"session_id":child["session_id"],"turn_id":child["turn_id"],"timeout_ms":60000}),
                        )),
                    );
                }
            }
            if scenario.contains("root") {
                if scenario.ends_with("-batch") {
                    if results.len() == 2 {
                        let calls = [
                            ("request_user_input", question()),
                            ("write_file", serde_json::json!({"path":"same-batch.txt","content":"必须取得答案后才能交付"})),
                        ].into_iter().enumerate().map(|(index,(name,args))|serde_json::json!({"index":index,"id":format!("gq-batch-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>();
                        let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
                        return (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    return chunk(
                        results.len(),
                        match results.len() {
                            0 => Some(("goal_read", serde_json::json!({}))),
                            1 => update("start_task", "a", serde_json::json!({})),
                            4 => update("block_task", "a", serde_json::json!({})),
                            5 => update("block_task", "b", serde_json::json!({})),
                            6 => update("block_task", "c", serde_json::json!({})),
                            _ => None,
                        },
                    );
                }
                if scenario.ends_with("-orphan") && results.len() == 2 {
                    std::future::pending::<()>().await;
                }
                return chunk(
                    results.len(),
                    match results.len() {
                        0 => Some(("goal_read", serde_json::json!({}))),
                        1 => Some(("request_user_input", question())),
                        2 => update("block_task", "a", serde_json::json!({})),
                        3 => update("block_task", "b", serde_json::json!({})),
                        4 => update("block_task", "c", serde_json::json!({})),
                        _ => None,
                    },
                );
            }
            if text == "/goal continue" {
                let call = match results.len() {
                    0 => Some(("goal_read", serde_json::json!({}))),
                    1 => Some(("glob", serde_json::json!({"pattern":"a.txt"}))),
                    2 => update(
                        "recheck_task",
                        "a",
                        serde_json::json!({"status":"pending","result":"真实glob确认A尚无交付","observation_id":results[1]["content"].as_str().unwrap().rsplit("Goal observation: ").next().unwrap().trim()}),
                    ),
                    3 => Some((
                        "start_agent",
                        serde_json::json!({"task":"goal-child-a","name":"继续需求员A","goal_task_id":"a"}),
                    )),
                    4 => Some((
                        "wait_agent",
                        serde_json::json!({"session_id":reference(3)["session_id"],"turn_id":reference(3)["turn_id"],"timeout_ms":60000}),
                    )),
                    5 => update("start_task", "a", serde_json::json!({})),
                    6 => Some(("read_file", serde_json::json!({"path":"a.txt"}))),
                    7 => update("accept_task", "a", evidence(6)),
                    8 => update("start_task", "c", serde_json::json!({})),
                    9 => Some((
                        "write_file",
                        serde_json::json!({"path":"c.txt","content":"A验收后C才交付"}),
                    )),
                    10 => update(
                        "record_result",
                        "c",
                        serde_json::json!({"result":"c.txt已实际交付，待验收"}),
                    ),
                    11 => Some(("read_file", serde_json::json!({"path":"c.txt"}))),
                    12 => update("accept_task", "c", evidence(11)),
                    13 => Some((
                        "bash",
                        serde_json::json!({"command":"cat a.txt b.txt c.txt"}),
                    )),
                    14 => update("accept_goal", "", evidence(13)),
                    _ => None,
                };
                return chunk(results.len(), call);
            }
            if results.len() == 5 {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if questions.lock().await.iter().any(|m| {
                            m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式" && m["decision"]["kind"] == "choice"
                        }) {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            let call = match results.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => Some((
                    "start_agent",
                    serde_json::json!({"task":"goal-child-a","name":"需求员A","goal_task_id":"a"}),
                )),
                2 => Some((
                    "start_agent",
                    serde_json::json!({"task":"goal-child-premature","name":"依赖C","goal_task_id":"c"}),
                )),
                3 => Some((
                    "start_agent",
                    serde_json::json!({"task":"goal-child-b","name":"资料员B","goal_task_id":"b"}),
                )),
                4 => Some(("goal_read", serde_json::json!({}))),
                5 => Some((
                    "get_agent_status",
                    serde_json::json!({"session_id":reference(1)["session_id"],"turn_id":reference(1)["turn_id"]}),
                )),
                6 => Some((
                    "wait_agent",
                    serde_json::json!({"session_id":reference(1)["session_id"],"turn_id":reference(1)["turn_id"],"timeout_ms":60000}),
                )),
                7 if scenario.ends_with("pause") => {
                    Some(("read_file", serde_json::json!({"path":"b.txt"})))
                }
                8 if scenario.ends_with("pause") => update(
                    "recheck_task",
                    "a",
                    serde_json::json!({"status":"pending","result":"试图同回合重派", "observation_id":results[7]["content"].as_str().unwrap().rsplit("Goal observation: ").next().unwrap().trim()}),
                ),
                9 if scenario.ends_with("pause") => Some((
                    "start_agent",
                    serde_json::json!({"task":"goal-child-bypass","name":"错误重派","goal_task_id":"a"}),
                )),
                10 if scenario.ends_with("pause") => update(
                    "replan",
                    "",
                    serde_json::json!({"tasks":[task("a",&[]),task("b",&[]),task("c",&[])]}),
                ),
                11 if scenario.ends_with("pause") => {
                    update("start_task", "c", serde_json::json!({}))
                }
                12 if scenario.ends_with("pause") => Some((
                    "write_file",
                    serde_json::json!({"path":"c.txt","content":"不应绕过依赖"}),
                )),
                13 if scenario.ends_with("pause") => {
                    update("start_task", "b", serde_json::json!({}))
                }
                14 if scenario.ends_with("pause") => {
                    Some(("read_file", serde_json::json!({"path":"b.txt"})))
                }
                15 if scenario.ends_with("pause") => update("accept_task", "b", evidence(14)),
                16 if scenario.ends_with("pause") => {
                    let mut equivalent = task("d", &[]);
                    equivalent["title"] = serde_json::json!("任务a");
                    update(
                        "replan",
                        "",
                        serde_json::json!({"tasks":[task("a",&[]),task("b",&[]),task("c",&["a"]),equivalent]}),
                    )
                }
                17 if scenario.ends_with("pause") => {
                    let mut replacement = task("d", &[]);
                    replacement["replaces"] = serde_json::json!(["a"]);
                    update(
                        "replan",
                        "",
                        serde_json::json!({"tasks":[task("a",&[]),task("b",&[]),task("c",&["a"]),replacement]}),
                    )
                }
                18 if scenario.ends_with("pause") => {
                    let mut repurposed = task("b", &[]);
                    repurposed["expected_result"] = serde_json::json!("a.txt");
                    update(
                        "replan",
                        "",
                        serde_json::json!({"tasks":[task("a",&[]),repurposed,task("c",&["a"])]}),
                    )
                }
                _ if scenario.ends_with("pause") => None,
                7 => update("start_task", "a", serde_json::json!({})),
                8 => update("accept_task", "a", evidence(5)),
                9 => Some(("read_file", serde_json::json!({"path":"a.txt"}))),
                10 => update("accept_task", "a", evidence(9)),
                11 => update("start_task", "b", serde_json::json!({})),
                12 => Some(("read_file", serde_json::json!({"path":"b.txt"}))),
                13 => update("accept_task", "b", evidence(12)),
                14 => update("start_task", "c", serde_json::json!({})),
                15 => Some((
                    "write_file",
                    serde_json::json!({"path":"c.txt","content":"A验收后C才交付"}),
                )),
                16 => update(
                    "record_result",
                    "c",
                    serde_json::json!({"result":"c.txt已实际交付"}),
                ),
                17 => Some(("read_file", serde_json::json!({"path":"c.txt"}))),
                18 => update("accept_task", "c", evidence(17)),
                19 => Some((
                    "bash",
                    serde_json::json!({"command":"cat a.txt b.txt c.txt"}),
                )),
                20 => update("accept_goal", "", evidence(19)),
                _ => None,
            };
            return chunk(results.len(), call);
        }
        if scenario != "question-goal-plan" {
            return chunk(
                results.len(),
                match results.len() {
                    0 => Some(("goal_read", serde_json::json!({}))),
                    1 => Some((
                        "goal_update",
                        serde_json::json!({"action":"set_plan","goal_id":current.as_ref().unwrap()["id"],"expected_version":0,"reason":"按独立产物拆解","acceptance_criteria":["全部文件可读"],"tasks":tasks()}),
                    )),
                    _ => None,
                },
            );
        }
        let call = match results.len() {
            0 if body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "request_user_input") =>
            {
                Some(("request_user_input", question()))
            }
            1 => Some(("goal_read", serde_json::json!({}))),
            2 => Some((
                "goal_update",
                serde_json::json!({"action":"set_plan","goal_id":current.as_ref().unwrap()["id"],"expected_version":0,"reason":"根据明确回答规划","acceptance_criteria":["全部文件可读"],"tasks":tasks()}),
            )),
            3 => Some((
                "write_file",
                serde_json::json!({"path":"premature.txt","content":"普通答案不能授权交付"}),
            )),
            _ => None,
        };
        chunk(results.len(), call)
    }
    async fn request(fixture: &MainFixture, operation: serde_json::Value) -> Response {
        let mut body = serde_json::json!({"version":"1.0","teamId":fixture.team,"conversationId":"goal-question"});
        body.as_object_mut()
            .unwrap()
            .extend(operation.as_object().unwrap().clone());
        fixture
            .app
            .clone()
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
    async fn send(fixture: &MainFixture, turn: &str, text: &str) -> Response {
        let response = request(fixture, serde_json::json!({"type":"history"})).await;
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap())
                .unwrap();
        let mut messages = Vec::new();
        for row in history["data"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["state"] == "complete")
        {
            messages.push(serde_json::json!({"role":"user","content":row["user"]}));
            messages.push(serde_json::json!({"role":"assistant","content":row["assistant"]}));
        }
        messages.push(serde_json::json!({"role":"user","content":text}));
        request(
            fixture,
            serde_json::json!({"type":"send","turnId":turn,"messages":messages}),
        )
        .await
    }
    async fn finish(response: Response) -> serde_json::Value {
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = tokio::time::timeout(
            Duration::from_secs(8),
            to_bytes(response.into_body(), 2_000_000),
        )
        .await
        .expect("Goal回合应有界结束")
        .unwrap();
        serde_json::from_str(std::str::from_utf8(&bytes).unwrap().lines().last().unwrap()).unwrap()
    }
    async fn goal(fixture: &MainFixture) -> serde_json::Value {
        wait_goal(fixture, |_| true).await
    }
    async fn wait_goal(
        fixture: &MainFixture,
        accepts: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = fixture.seen.lock().await.iter().rev().find_map(|m| {
                    let goal = m["snapshot"]["conversations"]
                        .as_array()
                        .and_then(|rows| rows.iter().find(|row| row["id"] == "goal-question"))
                        .map(|row| &row["goal"])
                        .unwrap_or(&m["snapshot"]["goal"]);
                    goal["id"].is_string().then(|| goal.clone())
                });
                if let Some(current) = current.filter(|value| accepts(value)) {
                    return current;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("主聊天应有界投影已提交Goal事实")
    }
    async fn wait_question(fixture: &MainFixture) {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式" && m["decision"]["kind"] == "choice")
                    && fixture.workspace.path().join("b.txt").exists()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("独立B和A真实待答应取得进展");
    }
    async fn wait_presented(fixture: &MainFixture) {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式" && m["decision"]["kind"] == "choice")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("执行中的Goal应已真实待答");
    }
    async fn wait_turn(fixture: &MainFixture, state: &str) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = request(fixture, serde_json::json!({"type":"history"})).await;
                let history: serde_json::Value = serde_json::from_slice(
                    &to_bytes(response.into_body(), 2_000_000).await.unwrap(),
                )
                .unwrap();
                if let Some(row) = history["data"]["turns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["id"] == "execute" && row["state"] == state)
                {
                    return row.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("原Goal回合应有界进入指定公开终态")
    }
    #[tokio::test]
    async fn main_restart_goal_question_preserves_facts_without_restoring_execution_permission() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-root-answer", "option-a").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        finish(send(&fixture, "execute", "确认执行").await).await;
        let id = fixture
            .state
            .main_office
            .conversation_record(&fixture.team, "goal-question")
            .unwrap()
            .unwrap()
            .session
            .unwrap();
        let original_goal = goal(&fixture).await;
        let calls = fixture.model_seen.lock().await.len();
        let cards = fixture
            .seen
            .lock()
            .await
            .iter()
            .filter(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式")
            .count();
        for phase in ["queued", "waiting", "answered"] {
            for startup in [false, true] {
                let home = restart_question_tests::snapshot(&fixture, &id, |event| {
                    event["type"] == "user_question_v1" && event["record"]["state"] == phase && event["record"]["request"]["question"] == "请选择交付方式"
                });
                let state = restart_question_tests::rebuild(&fixture, home.path());
                if startup {
                    assert_eq!(state.recover_orphaned_turns().await, 1);
                }
                let app = build_router(state.clone());
                child_test_json(&app,"POST","/api/pawoffice/chat",serde_json::json!({"version":"1.0","type":"history","teamId":fixture.team,"conversationId":"goal-question"})).await;
                let boards = state.main_office.directory().unwrap();
                let board = boards
                    .iter()
                    .find(|b| b["workspace"]["id"] == fixture.team)
                    .unwrap();
                let view = board["conversations"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|r| r["id"] == "goal-question"))
                    .unwrap_or(board);
                let goal = &view["goal"];
                assert_eq!(goal["id"], original_goal["id"]);
                assert_eq!(goal["phase"], "paused");
                assert_eq!(goal["acceptedTaskCount"], 0);
                assert!(!view["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["status"] == "running"));
                let records = restart_question_tests::question_records(home.path(), &id);
                assert_eq!(
                    records.last().unwrap()["state"],
                    if phase == "answered" {
                        "answered"
                    } else {
                        "interrupted"
                    }
                );
                assert_eq!(
                    child_test_json(&app, "GET", "/session/status", serde_json::json!({})).await,
                    serde_json::json!({})
                );
                assert_eq!(
                    fixture.model_seen.lock().await.len(),
                    calls,
                    "恢复Goal只能恢复事实不能执行"
                );
            }
        }
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式")
                .count(),
            cards
        );
    }
    #[tokio::test]
    async fn main_goal_question_same_batch_wait_requires_answer_before_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for (choice, expires, answered) in [
            ("held-answer", true, false),
            ("held-wrong-label", false, false),
            ("held-answer", false, true),
        ] {
            let fixture = main_fixture("question-goal-root-batch", choice).await;
            finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
            let pending = tokio::spawn(finish(send(&fixture, "execute", "确认执行").await));
            wait_presented(&fixture).await;
            assert!(!pending.is_finished(), "未提交问题时原执行仍在等待");
            assert!(
                !fixture.workspace.path().join("same-batch.txt").exists(),
                "题未提交时同一模型响应的依赖交付工具不得执行"
            );
            if expires {
                fixture.question_clock.as_ref().unwrap().advance(300000);
            } else {
                fixture.release_model.notify_one();
            }
            assert_eq!(pending.await.unwrap()["state"], "complete");
            assert_eq!(
                fixture.workspace.path().join("same-batch.txt").exists(),
                answered,
                "提问超时或答复无效后同一模型响应中的依赖交付工具不得执行：{choice}"
            );
            let phase = if answered { "blocked" } else { "paused" };
            assert_eq!(
                wait_goal(&fixture, |g| g["phase"] == phase).await["acceptedTaskCount"],
                0
            );
            if !answered {
                assert_eq!(
                    fixture.model_seen.lock().await.len(),
                    5,
                    "受控暂停后不请求下一轮模型"
                );
            }
        }
    }
    #[tokio::test]
    async fn main_goal_question_root_answer_only_resumes_original_question() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-root-answer", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let pending = tokio::spawn(finish(send(&fixture, "execute", "确认执行").await));
        wait_presented(&fixture).await;
        fixture.release_model.notify_one();
        assert_eq!(pending.await.unwrap()["state"], "complete");
        assert_eq!(
            wait_goal(&fixture, |g| g["phase"] == "blocked"
                || g["phase"] == "paused")
            .await["acceptedTaskCount"],
            0
        );
        assert!(!fixture.workspace.path().join("a.txt").exists());
        let models = fixture.model_seen.lock().await;
        assert!(models
            .iter()
            .any(|body| body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool"
                    && m["tool_call_id"] == "gq-1"
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("option-a")))));
    }
    #[tokio::test]
    async fn main_goal_question_unobserved_answer_ends_disconnect_exemption() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-root-orphan", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let first = send(&fixture, "execute", "确认执行").await;
        wait_presented(&fixture).await;
        drop(first);
        fixture.release_model.notify_one();
        wait_turn(&fixture, "stopped").await;
        assert_eq!(goal(&fixture).await["acceptedTaskCount"], 0);
        assert!(!fixture.workspace.path().join("a.txt").exists());
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式")
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn main_goal_question_explicit_stop_overrides_child_disconnect_recovery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-pause", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let first = send(&fixture, "execute", "确认执行").await;
        wait_question(&fixture).await;
        drop(first);
        let cancelled = request(
            &fixture,
            serde_json::json!({"type":"cancel","turnId":"execute"}),
        )
        .await;
        assert_eq!(cancelled.status(), StatusCode::OK);
        fixture.release_model.notify_one();
        wait_turn(&fixture, "stopped").await;
        assert!(!fixture.workspace.path().join("a.txt").exists());
        assert!(!fixture.workspace.path().join("c.txt").exists());
        assert!(!fixture.workspace.path().join("bypass.txt").exists());
        assert_eq!(
            wait_goal(&fixture, |g| g["phase"] == "paused").await["acceptedTaskCount"],
            0
        );
    }
    #[tokio::test]
    async fn main_goal_question_child_result_requires_actual_acceptance_before_dependencies() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-success", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let first = send(&fixture, "execute", "确认执行").await;
        wait_question(&fixture).await;
        assert_eq!(goal(&fixture).await["acceptedTaskCount"], 0);
        assert!(!fixture.workspace.path().join("a.txt").exists());
        assert!(!fixture.workspace.path().join("c.txt").exists());
        let resumed = request(
            &fixture,
            serde_json::json!({"type":"resume","turnId":"execute"}),
        )
        .await;
        drop(first);
        let pending = tokio::spawn(finish(resumed));
        fixture.release_model.notify_one();
        assert_eq!(pending.await.unwrap()["state"], "complete");
        assert_eq!(
            wait_goal(&fixture, |g| g["phase"] == "completed").await["phase"],
            "completed"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("a.txt")).unwrap(),
            "A\n"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(requests
            .iter()
            .any(|body| body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool"
                    && m["tool_call_id"] == "gq-8"
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("验收依据")))));
        assert!(!requests.iter().any(|body| body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == "goal-child-premature")));
    }
    #[tokio::test]
    async fn main_goal_question_explicit_continuation_rechecks_and_preserves_independent_acceptance(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-continue-pause", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let first = tokio::spawn(finish(send(&fixture, "execute", "确认执行").await));
        wait_question(&fixture).await;
        fixture.question_clock.as_ref().unwrap().advance(300000);
        first.await.unwrap();
        assert_eq!(
            wait_goal(&fixture, |g| g["phase"] == "paused").await["acceptedTaskCount"],
            1
        );
        let second = tokio::spawn(finish(send(&fixture, "continue", "/goal continue").await));
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| {
                        m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式" && m["decision"]["kind"] == "choice"
                    })
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
        fixture.release_model.notify_one();
        assert_eq!(second.await.unwrap()["state"], "complete");
        assert_eq!(
            wait_goal(&fixture, |g| g["phase"] == "completed").await["phase"],
            "completed"
        );
        assert_eq!(goal(&fixture).await["acceptedTaskCount"], 3);
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("a.txt")).unwrap(),
            "A\n",
            "明确继续只交付一次"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("b.txt")).unwrap(),
            "独立B已交付"
        );
    }
    #[tokio::test]
    async fn main_goal_question_observation_reconnect_keeps_original_deadline_and_rejects_old_guards(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for reconnect_first in [true, false] {
            let fixture = main_fixture("question-goal-root-observer", "held-answer").await;
            finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
            let first = send(&fixture, "execute", "确认执行").await;
            wait_presented(&fixture).await;
            let second = if reconnect_first {
                let second = request(
                    &fixture,
                    serde_json::json!({"type":"resume","turnId":"execute"}),
                )
                .await;
                drop(first);
                second
            } else {
                drop(first);
                request(
                    &fixture,
                    serde_json::json!({"type":"resume","turnId":"execute"}),
                )
                .await
            };
            fixture.question_clock.as_ref().unwrap().advance(300000);
            let result = finish(second).await;
            assert_eq!(
                result["state"], "complete",
                "旧观察者不能取消已重连同一回合，reconnect_first={reconnect_first}: {result}"
            );
            assert_eq!(goal(&fixture).await["phase"], "paused");
            assert_eq!(
                fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式")
                    .count(),
                1
            );
        }
    }
    #[tokio::test]
    async fn main_goal_question_expiry_keeps_independent_acceptance_and_rejects_same_turn_bypass() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-pause", "held-answer").await;
        finish(send(&fixture, "plan", "/goal 交付三个文件").await).await;
        let response = send(&fixture, "execute", "确认执行").await;
        let pending = tokio::spawn(finish(response));
        wait_question(&fixture).await;
        assert!(!fixture.workspace.path().join("a.txt").exists());
        assert!(!fixture.workspace.path().join("c.txt").exists());
        fixture.question_clock.as_ref().unwrap().advance(300000);
        let result = pending.await.unwrap();
        assert_eq!(result["state"], "complete", "超时应真实暂停并汇总");
        assert!(
            !fixture.workspace.path().join("bypass.txt").exists(),
            "未明确继续不能同回合重派"
        );
        assert!(
            !fixture.workspace.path().join("c.txt").exists(),
            "不能重规划移除暂停依赖"
        );
        let current = wait_goal(&fixture, |g| g["phase"] == "paused").await;
        assert_eq!(
            current["acceptedTaskCount"], 1,
            "独立B验收必须保留：{current}"
        );
        assert_eq!(current["phase"], "paused");
        assert_eq!(
            current["taskCount"], 3,
            "暂停分支不能通过等价新任务或replaces绕过"
        );
        let requests = fixture.model_seen.lock().await;
        for index in [10, 16, 17, 18] {
            assert!(
                requests
                    .iter()
                    .any(|body| body["messages"].as_array().unwrap().iter().any(|m| m
                        ["tool_call_id"]
                        == format!("gq-{index}")
                        && m["content"]
                            .as_str()
                            .is_some_and(|s| s.contains("不能通过重规划")))),
                "重规划绕过{index}必须明确拒绝"
            );
        }
    }
    #[tokio::test]
    async fn main_goal_question_planning_accepts_answer_without_authorizing_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-goal-plan", "option-a").await;
        assert_eq!(
            finish(send(&fixture, "plan", "/goal 交付三个文件").await).await["state"],
            "complete"
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request" && m["decision"]["title"] == "请选择交付方式")
                .count(),
            1,
            "规划阶段应真实提问"
        );
        let current = goal(&fixture).await;
        assert_eq!(current["phase"], "draft");
        assert_eq!(current["taskCount"], 3);
        assert!(!fixture.workspace.path().join("premature.txt").exists());
        let models = fixture.model_seen.lock().await;
        assert!(models
            .iter()
            .any(|body| body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("option-a")))));
    }
}

#[cfg(test)]
mod question_recovery_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// 功能：模拟单个活动页面的待答卡片及提交效果。
    /// 字段/变体语义：visible记录卡片，replies保存答复通道，observations保留读取事实，read字段控制读取门闩。
    /// 数据来源：本地Office请求和测试显式刷新／提交。
    /// 数据去向：原版协议响应及真实Host恢复流程。
    /// 约束：不修改问题期限或直接写可靠答案。
    /// 稳定性：test-only。
    /// 关联：CHG-2026-0054、question_recovery_tests。
    #[derive(Default)]
    pub(super) struct Display {
        visible: std::sync::Mutex<HashSet<String>>,
        replies: std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<&'static str>>>,
        observations: std::sync::Mutex<Vec<Vec<String>>>,
        hold_read: std::sync::atomic::AtomicBool,
        read_waiting: tokio::sync::Notify,
        release_read: tokio::sync::Notify,
    }
    /// 功能：绑定一次展示请求的本地可见生命周期。
    /// 字段/变体语义：持有页面效果与本次展示身份。
    /// 数据来源：Display::track。
    /// 数据去向：Drop移除原展示身份。
    /// 约束：旧POST丢弃只移除本代卡片，不提交答案。
    /// 稳定性：test-only。
    /// 关联：CHG-2026-0054、Display。
    pub(super) struct Visible(Arc<Display>, String);
    impl Drop for Visible {
        fn drop(&mut self) {
            self.0.visible.lock().unwrap().remove(&self.1);
        }
    }
    impl Display {
        /// 功能：在选定的Office读取边界挂起本地响应。
        /// 参数：body为本地Office请求。
        /// 返回：读取获准继续。
        /// 错误：无。
        /// 状态：通知读取已进入，等待测试显式放行。
        /// 并发与取消：等待可取消，不持有互斥锁。
        /// 权限：不更改卡片、答案或执行许可。
        /// 关联：CHG-2026-0054、缺失观察与答案竞争测试。
        pub(super) async fn before_read(&self, body: &serde_json::Value) {
            if body["type"] == "office.read"
                && body["teamId"].is_string()
                && self.hold_read.load(std::sync::atomic::Ordering::SeqCst)
            {
                self.read_waiting.notify_one();
                self.release_read.notified().await;
            }
        }
        /// 功能：登记本次提问在本地页面可见。
        /// 参数：body为原Office请求。
        /// 返回：提问请求的可见性guard，其他消息返回None。
        /// 错误：缺少展示身份时断言失败。
        /// 状态：插入本代展示身份。
        /// 并发与取消：短同步锁，guard丢弃移除本代。
        /// 权限：不代表用户已作答。
        /// 关联：CHG-2026-0054、Visible。
        pub(super) fn track(self: &Arc<Self>, body: &serde_json::Value) -> Option<Visible> {
            if body["type"] != "decision.request" {
                return None;
            }
            let id = body["decision"]["id"].as_str().unwrap().to_owned();
            self.visible.lock().unwrap().insert(id.clone());
            Some(Visible(self.clone(), id))
        }
        /// 功能：返回本地页面当前可见的待答身份。
        /// 参数：无。
        /// 返回：当前可见身份集合的快照。
        /// 错误：无。
        /// 状态：追加本次读取观察记录。
        /// 并发与取消：短同步锁，不等待网络。
        /// 权限：只反映页面展示，不确认答案。
        /// 关联：CHG-2026-0054、office.read恢复fixture。
        pub(super) fn pending(&self) -> Vec<String> {
            let pending = self
                .visible
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            self.observations.lock().unwrap().push(pending.clone());
            pending
        }
        fn refresh(&self) {
            self.visible.lock().unwrap().clear();
        }
        fn submit(&self, id: &str, choice: &'static str) {
            self.replies
                .lock()
                .unwrap()
                .remove(id)
                .unwrap()
                .send(choice)
                .ok();
        }
        /// 功能：等待本地用户提交并生成原版Office问答响应。
        /// 参数：body为具有真实归属的提问请求。
        /// 返回：明确答复、临时离线或断流效果。
        /// 错误：通道关闭按fixture超时处理。
        /// 状态：保存本代答复发送端，等待测试显式提交。
        /// 并发与取消：异步等待可丢弃，不持锁等待。
        /// 权限：不绕过真实Host答案和来源校验。
        /// 关联：CHG-2026-0054、question_recovery_tests。
        pub(super) async fn response(&self, body: &serde_json::Value) -> Response {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.replies
                .lock()
                .unwrap()
                .insert(body["decision"]["id"].as_str().unwrap().into(), sender);
            let choice = receiver.await.unwrap_or("timeout");
            if choice == "offline" {
                return (StatusCode::CONFLICT, Json(serde_json::json!({"code":409,"msg":"No online page","data":{"reasonCode":"NO_RECEIVER","details":{},"issues":[]}}))).into_response();
            }
            if choice == "disconnect" {
                return Response::new(Body::from_stream(futures::stream::iter([
                    Err::<String, _>(std::io::Error::other("fixture response disconnected")),
                ])));
            }
            user_input_tests::decision_response(body, choice)
        }
    }
    async fn start(fixture: &MainFixture) -> tokio::task::JoinHandle<axum::body::Bytes> {
        start_in(fixture, "question").await
    }
    async fn start_in(
        fixture: &MainFixture,
        conversation: &str,
    ) -> tokio::task::JoinHandle<axum::body::Bytes> {
        let response = fixture
            .app
            .clone()
            .oneshot(Request::builder().method("POST").uri("/api/pawoffice/chat").header("content-type","application/json")
                .body(Body::from(serde_json::json!({"version":"1.0","type":"send","teamId":fixture.team,"conversationId":conversation,"turnId":"one","messages":[{"role":"user","content":"先问我，再按答案完成"}]}).to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::spawn(async move { to_bytes(response.into_body(), 2_000_000).await.unwrap() })
    }
    async fn card(fixture: &MainFixture, count: usize) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let cards = fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .filter(|m| m["type"] == "decision.request")
                    .cloned()
                    .collect::<Vec<_>>();
                if cards.len() >= count {
                    let card = &cards[count - 1];
                    if fixture
                        .question_display
                        .replies
                        .lock()
                        .unwrap()
                        .contains_key(card["decision"]["id"].as_str().unwrap())
                    {
                        return card.clone();
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("实际Office对端应收到第{count}次展示请求：{error}"))
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
    async fn main_question_recovery_replays_missing_card_on_original_deadline_and_call() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-refresh", "controlled").await;
        let turn = start(&fixture).await;
        let original = card(&fixture, 1).await;
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(60_000);
        let replay = card(&fixture, 2).await;
        assert_ne!(original["decision"]["id"], replay["decision"]["id"]);
        assert_eq!(replay["timeoutMs"], 240000, "刷新不能重建五分钟期限");
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            1,
            "恢复展示不调用模型"
        );
        let before = records(&fixture);
        assert_eq!(before.last().unwrap()["generation"], 2);
        assert_eq!(before.last().unwrap()["deadline_ms"], 301000);
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "option-a");
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("已采用方案A"));
        let after = records(&fixture);
        let answered = after.last().unwrap();
        assert_eq!(answered["state"], "answered");
        assert_eq!(answered["question_id"], before[0]["question_id"]);
        assert_eq!(answered["generation"], 2, "终态不能写回旧展示代次");
        assert_eq!(answered["display_id"], replay["decision"]["id"]);
        assert_eq!(fixture.model_seen.lock().await.len(), 2, "只接续原调用一次");
    }

    #[tokio::test]
    async fn main_question_recovery_temporary_offline_reasks_without_guessing_an_answer() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-offline", "controlled").await;
        let turn = start(&fixture).await;
        let original = card(&fixture, 1).await;
        fixture
            .question_display
            .submit(original["decision"]["id"].as_str().unwrap(), "offline");
        fixture.question_clock.as_ref().unwrap().advance(1_000);
        let replay = card(&fixture, 2).await;
        assert_eq!(replay["timeoutMs"], 299000);
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            1,
            "离线不能猜测用户答案"
        );
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "custom");
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("我需要Markdown报告"));
        assert_eq!(records(&fixture).last().unwrap()["generation"], 2);
    }

    #[tokio::test]
    async fn main_question_recovery_keeps_visible_draft_and_never_reasks_a_saved_answer() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-visible", "controlled").await;
        let turn = start(&fixture).await;
        let original = card(&fixture, 1).await;
        let id = original["decision"]["id"].as_str().unwrap();
        fixture.question_clock.as_ref().unwrap().advance(60_000);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .question_display
                    .observations
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|snapshot| snapshot.iter().any(|seen| seen == id))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("实际查询确认原卡片仍存在");
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1,
            "不能定时替换正常填写中的卡片"
        );
        fixture.question_display.submit(id, "option-a");
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("已采用方案A"));
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(60_000);
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1,
            "已保存答案不再重建展示"
        );
        assert_eq!(
            records(&fixture)
                .iter()
                .filter(|record| record["state"] == "answered")
                .count(),
            1
        );
        assert_eq!(fixture.model_seen.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn main_question_recovery_unknown_stream_response_and_late_old_answer_use_only_new_generation(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-disconnect", "controlled").await;
        let turn = start(&fixture).await;
        let original = card(&fixture, 1).await;
        fixture
            .question_display
            .submit(original["decision"]["id"].as_str().unwrap(), "disconnect");
        fixture.question_clock.as_ref().unwrap().advance(60_000);
        let replay = card(&fixture, 2).await;
        assert_eq!(replay["timeoutMs"], 240000);
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            1,
            "响应丢失未确认答案时不能接续"
        );
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(1_000);
        let newest = card(&fixture, 3).await;
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "option-a");
        fixture
            .question_display
            .submit(newest["decision"]["id"].as_str().unwrap(), "custom");
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("我需要Markdown报告"), "{text}");
        let saved = records(&fixture);
        let terminal = saved.last().unwrap();
        assert_eq!(terminal["generation"], 3);
        assert_eq!(terminal["display_id"], newest["decision"]["id"]);
        assert_eq!(
            terminal["answer"]["choice_id"], "custom",
            "迟到旧回答不能替换新代答案"
        );
        assert_eq!(
            saved
                .iter()
                .filter(|record| record["state"] == "answered")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn main_question_recovery_less_than_interface_minimum_expires_without_extending_deadline()
    {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-small-window", "controlled").await;
        let turn = start(&fixture).await;
        card(&fixture, 1).await;
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(299_500);
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("已暂停"));
        let saved = records(&fixture);
        assert_eq!(saved.last().unwrap()["state"], "expired");
        assert_eq!(saved.last().unwrap()["deadline_ms"], 301000);
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
        assert_eq!(fixture.model_seen.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn main_question_recovery_stop_keeps_current_generation_and_rejects_late_answer() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-stop", "controlled").await;
        let turn = start(&fixture).await;
        card(&fixture, 1).await;
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(1000);
        let replay = card(&fixture, 2).await;
        let session = records(&fixture)[0]["scope"]["asking_session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        child_test_json(
            &fixture.app,
            "POST",
            &format!("/session/{session}/abort"),
            serde_json::json!({}),
        )
        .await;
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "option-a");
        fixture.question_clock.as_ref().unwrap().advance(60000);
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("stopped"));
        let saved = records(&fixture);
        assert_eq!(saved.last().unwrap()["state"], "cancelled");
        assert_eq!(
            saved.last().unwrap()["generation"],
            2,
            "Drop取消必须保留最新可靠代次"
        );
        assert_eq!(
            saved.last().unwrap()["display_id"],
            replay["decision"]["id"]
        );
        assert!(!saved.iter().any(|record| record["state"] == "answered"));
        assert_eq!(fixture.model_seen.lock().await.len(), 1);
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn main_question_recovery_save_failure_never_publishes_unsaved_generation() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-save", "controlled").await;
        let turn = start(&fixture).await;
        card(&fixture, 1).await;
        let path = std::fs::read_dir(fixture.home.path().join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained).unwrap();
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(1000);
        let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
        std::fs::rename(&retained, &path).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"state\":\"error\""), "{text}");
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            1,
            "保存失败不能发出新代请求"
        );
        assert_eq!(fixture.model_seen.lock().await.len(), 1);
        assert!(records(&fixture)
            .iter()
            .all(|record| record["generation"] == 1 && record["state"] != "answered"));
    }

    #[tokio::test]
    async fn main_question_recovery_child_replays_into_same_parent_while_independent_work_continues(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-child", "controlled").await;
        let turn = start(&fixture).await;
        let original = card(&fixture, 1).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.workspace.path().join("B-independent.txt").exists()
                    && fixture.workspace.path().join("C-independent.txt").exists()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("子待答时父与独立兄弟继续完成产物");
        assert!(!fixture.workspace.path().join("A-dependent.txt").exists());
        fixture.question_display.refresh();
        fixture.question_clock.as_ref().unwrap().advance(60000);
        let replay = card(&fixture, 2).await;
        assert_eq!(original["conversationId"], replay["conversationId"]);
        assert_eq!(
            original["decision"]["actorId"],
            replay["decision"]["actorId"]
        );
        assert_eq!(replay["timeoutMs"], 240000);
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "custom");
        let bytes = tokio::time::timeout(Duration::from_secs(8), turn)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("协作完成"));
        assert!(fixture.workspace.path().join("A-dependent.txt").exists());
        let saved = records(&fixture);
        let answered = saved
            .iter()
            .find(|record| record["state"] == "answered")
            .unwrap();
        assert_ne!(
            answered["scope"]["asking_session_id"],
            answered["scope"]["parent_session_id"]
        );
        assert_eq!(answered["generation"], 2);
        assert_eq!(answered["answer"]["choice_id"], "custom");
    }
    #[tokio::test]
    async fn main_question_recovery_invalid_replayed_answer_does_not_interrupt_another_conversation(
    ) {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-recovery-isolation", "controlled").await;
        let a = start_in(&fixture, "a").await;
        let first = card(&fixture, 1).await;
        let b = start_in(&fixture, "b").await;
        let second = card(&fixture, 2).await;
        fixture
            .question_display
            .visible
            .lock()
            .unwrap()
            .remove(first["decision"]["id"].as_str().unwrap());
        fixture.question_clock.as_ref().unwrap().advance(60000);
        let replay = card(&fixture, 3).await;
        assert_eq!(replay["conversationId"], "a");
        fixture
            .question_display
            .submit(replay["decision"]["id"].as_str().unwrap(), "wrong-display");
        fixture
            .question_display
            .submit(second["decision"]["id"].as_str().unwrap(), "custom");
        let a = tokio::time::timeout(Duration::from_secs(5), a)
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(Duration::from_secs(5), b)
            .await
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&a).unwrap().contains("已暂停"));
        assert!(std::str::from_utf8(&b)
            .unwrap()
            .contains("我需要Markdown报告"));
        let saved = records(&fixture);
        assert_eq!(
            saved
                .iter()
                .filter(|record| record["state"] == "answered")
                .count(),
            1
        );
        assert_eq!(
            saved
                .iter()
                .filter(|record| record["state"] == "interrupted")
                .count(),
            1
        );
        assert_eq!(
            fixture
                .seen
                .lock()
                .await
                .iter()
                .filter(|m| m["type"] == "decision.request")
                .count(),
            3,
            "错误归属不应无限重投"
        );
    }
    #[tokio::test]
    async fn main_question_recovery_answer_or_stop_during_missing_observation_does_not_reissue() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for answer in [true, false] {
            let fixture = main_fixture("question-recovery-observation-race", "controlled").await;
            let turn = start(&fixture).await;
            let original = card(&fixture, 1).await;
            fixture
                .question_display
                .hold_read
                .store(true, std::sync::atomic::Ordering::SeqCst);
            fixture.question_display.refresh();
            fixture.question_clock.as_ref().unwrap().advance(1000);
            tokio::time::timeout(
                Duration::from_secs(5),
                fixture.question_display.read_waiting.notified(),
            )
            .await
            .expect("缺失观察已在外部对端等待回复");
            if answer {
                fixture
                    .question_display
                    .submit(original["decision"]["id"].as_str().unwrap(), "option-a");
            } else {
                let session = records(&fixture)[0]["scope"]["asking_session_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                child_test_json(
                    &fixture.app,
                    "POST",
                    &format!("/session/{session}/abort"),
                    serde_json::json!({}),
                )
                .await;
            }
            let bytes = tokio::time::timeout(Duration::from_secs(5), turn)
                .await
                .unwrap()
                .unwrap();
            fixture
                .question_display
                .hold_read
                .store(false, std::sync::atomic::Ordering::SeqCst);
            fixture.question_display.release_read.notify_waiters();
            fixture.question_clock.as_ref().unwrap().advance(1000);
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.contains(if answer {
                    "已采用方案A"
                } else {
                    "stopped"
                }),
                "{text}"
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
                "已接受回答或停止不能被缺失观察复活"
            );
            assert_eq!(
                records(&fixture).last().unwrap()["state"],
                if answer { "answered" } else { "cancelled" }
            );
            assert_eq!(records(&fixture).last().unwrap()["generation"], 1);
        }
    }
}

#[cfg(test)]
mod child_question_tests {
    use super::*;

    fn call(index: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"index":index,"id":format!("child-question-{name}-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
    }

    /// 功能：生成父子提问和独立兄弟继续执行的本地模型脚本。
    /// 参数：body为真实模型请求，questions为Office观察，gate控制独立任务交付。
    /// 返回：当前父子场景的流式响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：只生成模型效果，实际子执行仍由Host调度。
    /// 并发与取消：本地门闩等待可取消，不暂停其他子任务。
    /// 权限：不创建额外执行许可或默认答案。
    /// 关联：CHG-2026-0054、child_question_tests。
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

    /// 功能：生成单题问答及答案接续的本地模型响应。
    /// 参数：body为真实模型请求，scenario选择输入边界脚本。
    /// 返回：流式提问、工具调用或最终文本。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：不直接写答案或问题状态。
    /// 并发与取消：同步构造响应，不持执行资源。
    /// 权限：真实工具执行仍走Host权限入口。
    /// 关联：CHG-2026-0054、user_input_tests。
    pub(super) fn model_response(body: &serde_json::Value, scenario: &str) -> Response {
        if scenario == "question-original" && body["stream"] == true {
            return goal_tests::original_model_response(body);
        }
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

    /// 功能：按原Office归属构造明确答案或到期响应。
    /// 参数：body为真实提问请求，choice选择正常或非法答复脚本。
    /// 返回：既有Office问答协议响应。
    /// 错误：不满足fixture归属或长度前提时断言失败。
    /// 状态：不直接保存可靠答案或放行执行。
    /// 并发与取消：同步构造响应，迟到答复由真实Host处理。
    /// 权限：非法脚本只用于验证Host拒绝边界。
    /// 关联：CHG-2026-0054、user_input_tests与恢复fixture。
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
        if choice == "display-failure" {
            return (StatusCode::INTERNAL_SERVER_ERROR,Json(serde_json::json!({"code":500,"msg":"Display failed","data":{"reasonCode":"DISPLAY_FAILED","details":{},"issues":[]}}))).into_response();
        }
        let mut data = serde_json::json!({"teamId":body["teamId"],"conversationId":body["conversationId"],"decisionId":body["decision"]["id"],"choiceId":"option-a","label":"方案A"});
        match choice {
            "custom" | "empty-custom" | "long-custom" | "max-custom" => {
                data["choiceId"] = serde_json::json!("custom");
                data["label"] = serde_json::json!("其他");
                data["customText"] = serde_json::json!(match choice {
                    "empty-custom" => "   ".into(),
                    "long-custom" => "🙂".repeat(501),
                    "max-custom" => "补".repeat(1000),
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
        if body["decision"]["title"] == "查看哪个城市的天气？" {
            if data["choiceId"] == "custom" {
                data["customText"] = serde_json::json!("长沙");
            } else if data["choiceId"] == "option-a" {
                data["label"] = serde_json::json!("武汉");
            }
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
    async fn main_attached_model_request_defaults_to_nonblocking_collaboration() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("question-choice", "option-a").await;
        assert_eq!(send(&fixture).await["state"], "complete");
        let requests = fixture.model_seen.lock().await;
        let system = requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            system.contains("优先通过 start_agent"),
            "默认应启动后继续独立工作"
        );
        assert!(system.contains("get_agent_status"));
        assert!(system.contains("wait_agent"));
        assert!(system.contains("delegate_task 仅作为同步等待的兼容入口"));
        assert!(system.contains("明确要求多 Agent、子 Agent、多空间协作"));
        assert!(system.contains("必须先调用 start_agent"));
        assert!(system.contains("不能静默退化为单 Agent"));
        assert!(!system.contains("必须先调用 delegate_task"));
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
        assert!(
            question["timeoutMs"]
                .as_u64()
                .is_some_and(|ms| (1000..=300000).contains(&ms)),
            "首次投递也应扣除原deadline已经消耗的时间"
        );
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
            "display-failure",
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
            let path = std::fs::read_dir(fixture.home.path().join("sessions"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let history = std::fs::read_to_string(path).unwrap();
            let terminal = history
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|event| event["type"] == "user_question_v1")
                .next_back()
                .unwrap();
            assert_eq!(
                terminal["record"]["state"],
                if choice == "timeout" {
                    "expired"
                } else {
                    "interrupted"
                },
                "{choice}的可靠终态必须来自可信失败种类"
            );
            assert!(terminal["record"]["answer"].is_null());
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

    /// 功能：生成非阻塞派发和可信子终态的本地模型脚本。
    /// 参数：body为真实模型请求，gate控制子任务释放。
    /// 返回：与当前父子输入对应的流式响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：只生成模型效果，不直接登记受理或终态。
    /// 并发与取消：门闩等待可取消，父子资源仍由真实Host管理。
    /// 权限：启动回执不表示任务完成或验收。
    /// 关联：CHG-2026-0054、async_delegation_tests。
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
                || (first_user == "父最后预算结束" && tools < 128))
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

    /// 功能：提供问题队列与截止等待共享的可控时钟。
    /// 字段/变体语义：watch发送端保存当前Unix毫秒，默认1000。
    /// 数据来源：测试显式advance。
    /// 数据去向：真实QuestionQueue及原deadline等待。
    /// 约束：只推进问题时钟，不修改预算或重新设置期限。
    /// 稳定性：test-only。
    /// 关联：CHG-2026-0054、question_queue_tests及恢复组合。
    pub(super) struct Clock(tokio::sync::watch::Sender<i64>);
    impl Default for Clock {
        fn default() -> Self {
            Self(tokio::sync::watch::channel(1000).0)
        }
    }
    impl Clock {
        /// 功能：推进共享的可信问题时钟并唤醒截止等待。
        /// 参数：ms为测试指定的推进毫秒。
        /// 返回：无。
        /// 错误：无。
        /// 状态：增加当前时间，保留既有问题deadline。
        /// 并发与取消：同步更新watch，等待方可取消。
        /// 权限：不授予执行许可或接受答案。
        /// 关联：CHG-2026-0054、QuestionClock。
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

    /// 功能：生成FIFO问题、共享展示名额与暂停接续的本地模型脚本。
    /// 参数：body为真实请求，questions为Office观察，gate控制独立子任务交付。
    /// 返回：当前队列场景的流式响应。
    /// 错误：不满足fixture前提时断言失败。
    /// 状态：只生成请求，队列事实仍由真实协调器保存。
    /// 并发与取消：本地等待可取消，不持展示名额或模型配额。
    /// 权限：不以模型文字设置可信暂停或用户答案。
    /// 关联：CHG-2026-0054、question_queue_tests。
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
