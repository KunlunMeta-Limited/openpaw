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
        let terminal_case = matches!(first_user, "父预算耗尽" | "父模型截断" | "父模型失败");
        let parent = body["tools"].to_string().contains("delegate_task") || terminal_case;
        let tools = messages.iter().filter(|m| m["role"] == "tool").count();
        if !parent && first_user == "失败子任务" {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if terminal_case && tools > 0 {
            if first_user == "父模型失败" {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let chunk = if first_user == "父预算耗尽"
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
        for input in ["父预算耗尽", "父模型截断", "父模型失败"] {
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
