#[cfg(test)]
mod goal_tests {
    use super::*;

    #[tokio::test]
    async fn main_goal_shell_verifies_produced_and_whole_results_without_unbinding_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-review", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "review",
            "plan",
            "/goal 审查验证脚本",
        )
        .await;
        let answer = goal_send(&fixture.app, &fixture.team, "review", "execute", "确认执行").await;
        assert!(answer["text"].as_str().unwrap().contains("整体验收已通过"));
        assert!(!fixture
            .workspace
            .path()
            .join("unbound-produced.txt")
            .exists());
        assert!(!fixture.workspace.path().join("unbound-whole.txt").exists());
        let requests = fixture.model_seen.lock().await;
        let results = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>();
        assert!(results
            .iter()
            .any(|text| text.contains("shell verified") && text.contains("Goal observation:")));
        assert!(
            results
                .iter()
                .filter(|text| text.contains("交付工具必须绑定"))
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn main_goal_long_block_reason_keeps_whiteboard_within_receiving_contract() {
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
            "review",
            "plan",
            "/goal 审查验证脚本长原因投影",
        )
        .await;
        goal_send(&fixture.app, &fixture.team, "review", "execute", "确认执行").await;
        let (snapshot, conversation) = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (_, read) = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap();
                if let Some(conversation) =
                    read["snapshot"]["conversations"]
                        .as_array()
                        .and_then(|rows| {
                            rows.iter().find(|row| {
                                row["id"] == "review" && row["goal"]["phase"] == "blocked"
                            })
                        })
                {
                    break (read["snapshot"].clone(), conversation.clone());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("真实Gateway应收到带长原因的阻塞快照");
        if let Ok(path) = std::env::var("GOAL_REVIEW_PROJECTION_FILE") {
            std::fs::write(path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();
        }
        assert!(
            conversation["goal"]["reason"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .count()
                > 40
        );
        assert!(conversation["tasks"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("外部地区资料"));
        for (field, limit) in [
            ("title", 160),
            ("tagline", 40),
            ("description", 300),
            ("note", 160),
        ] {
            assert!(
                conversation["whiteboard"][field]
                    .as_str()
                    .unwrap()
                    .encode_utf16()
                    .count()
                    <= limit,
                "白板字段 {field} 超出接收端合同"
            );
        }
    }

    #[tokio::test]
    async fn main_goal_plan_changes_require_fresh_overall_check_after_replan_and_user_edit() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        for mode in ["重规划", "用户编辑"] {
            let fixture = main_fixture("goal-review", "confirm").await;
            goal_send(
                &fixture.app,
                &fixture.team,
                "proof",
                "plan",
                &format!("/goal 审查旧整体验收依据{mode}"),
            )
            .await;
            let mut answer =
                goal_send(&fixture.app, &fixture.team, "proof", "execute", "确认执行").await;
            if mode == "用户编辑" {
                assert!(!answer["text"].as_str().unwrap().contains("整体验收已通过"));
                goal_send(
                    &fixture.app,
                    &fixture.team,
                    "proof",
                    "edit",
                    "整体标准改为调整后的资料准确",
                )
                .await;
                answer = goal_send(
                    &fixture.app,
                    &fixture.team,
                    "proof",
                    "continue",
                    "/goal continue",
                )
                .await;
            }
            assert!(
                answer["text"].as_str().unwrap().contains("整体验收已通过"),
                "变更后完成真实新检查才能通过：{mode}"
            );
            let requests = fixture.model_seen.lock().await;
            let results = requests
                .iter()
                .flat_map(|r| r["messages"].as_array().unwrap())
                .filter(|m| m["role"] == "tool")
                .filter_map(|m| m["content"].as_str())
                .collect::<Vec<_>>();
            assert!(
                results
                    .iter()
                    .any(|text| text.starts_with("整体验收需要任务全部通过后的独立检查")),
                "旧整体观察必须拒绝：{mode}"
            );
            if mode == "用户编辑" {
                assert!(results.iter().any(|text| text.contains("交付工具必须绑定")));
                assert!(results.iter().any(|text| text.contains("计划尚待确认")));
                assert!(!results.iter().any(|text| text.starts_with("paused-bypass")));
            }
        }
    }

    fn review_proof_model_response(messages: &[serde_json::Value]) -> Response {
        let latest = messages
            .iter()
            .rposition(|m| {
                m["role"] == "user" && !m["content"].as_str().unwrap_or("").starts_with("Goal 仍有")
            })
            .unwrap();
        let text = messages[latest]["content"].as_str().unwrap_or("");
        let tools = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let current = tools
            .iter()
            .rev()
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|g| g["id"].is_string())
            })
            .unwrap_or_default();
        let editing = messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("审查旧整体验收依据用户编辑"))
        });
        let definition = |id: &str| serde_json::json!({"id":id,"title":format!("交付{id}"),"expected_result":format!("{id}.txt"),"acceptance_criteria":["资料为七"],"dependencies":[]});
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"task_id":task,"reason":"核对变更后的整体资料"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let evidence = |index: usize| {
            let id = tools[index]["content"]
                .as_str()
                .unwrap_or("")
                .rsplit("Goal observation: ")
                .next()
                .unwrap_or("")
                .trim();
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"实际资料为七"}]})
        };
        let read = |id: &str| Some(("read_file", serde_json::json!({"path":format!("{id}.txt")})));
        let call = if text.starts_with("/goal ") && text != "/goal continue" {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update(
                    "set_plan",
                    "",
                    serde_json::json!({"acceptance_criteria":["整体资料准确"],"tasks":[definition("a")]}),
                ),
                _ => None,
            }
        } else if text.starts_with("整体标准改为") {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update(
                    "set_plan",
                    "",
                    serde_json::json!({"acceptance_criteria":["调整后的资料准确"],"tasks":[definition("a")],"user_excerpt":text}),
                ),
                2 => Some((
                    "bash",
                    serde_json::json!({"command":"printf paused-bypass"}),
                )),
                _ => None,
            }
        } else if text == "/goal continue" {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => {
                    let old = current["observations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["whole_goal"] == true)
                        .unwrap();
                    update(
                        "accept_goal",
                        "",
                        serde_json::json!({"evidence":[{"criterion":0,"observation_id":old["id"],"conclusion":"旧检查不能用于新验收"}]}),
                    )
                }
                2 => read("a"),
                3 => update("accept_goal", "", evidence(2)),
                _ => None,
            }
        } else {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update("start_task", "a", serde_json::json!({})),
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"a.txt","content":"七"}),
                )),
                3 => update(
                    "record_result",
                    "a",
                    serde_json::json!({"result":"a资料已交付"}),
                ),
                4 => read("a"),
                5 => update("accept_task", "a", evidence(4)),
                6 => read("a"),
                7 if editing => update(
                    "replan",
                    "",
                    serde_json::json!({"objective":"请求用户调整整体标准"}),
                ),
                8 if editing => Some((
                    "bash",
                    serde_json::json!({"command":"printf paused-bypass"}),
                )),
                _ if editing => None,
                7 => update(
                    "replan",
                    "",
                    serde_json::json!({"tasks":[definition("a"),definition("b")]}),
                ),
                8 => update("start_task", "b", serde_json::json!({})),
                9 => Some((
                    "write_file",
                    serde_json::json!({"path":"b.txt","content":"七"}),
                )),
                10 => update(
                    "record_result",
                    "b",
                    serde_json::json!({"result":"b资料已交付"}),
                ),
                11 => read("b"),
                12 => update("accept_task", "b", evidence(11)),
                13 => update("accept_goal", "", evidence(6)),
                14 => read("b"),
                15 => update("accept_goal", "", evidence(14)),
                _ => None,
            }
        };
        goal_fixture_chunk(call, tools.len())
    }

    fn review_model_response(messages: &[serde_json::Value]) -> Response {
        let latest = messages
            .iter()
            .rposition(|m| {
                m["role"] == "user" && !m["content"].as_str().unwrap_or("").starts_with("Goal 仍有")
            })
            .unwrap();
        let tools = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let current = tools
            .first()
            .and_then(|m| m["content"].as_str())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or_default();
        let long_reason = messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("长原因投影"))
        });
        let reason = if long_reason {
            format!("需要补充外部地区资料与截止日期：{}", "🔎".repeat(30))
        } else {
            "审查真实shell检查与交付权限".into()
        };
        let update = |action: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":reason,"task_id":"script"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let evidence = |index: usize| {
            let id = tools[index]["content"]
                .as_str()
                .unwrap_or("")
                .rsplit("Goal observation: ")
                .next()
                .unwrap_or("")
                .trim();
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"shell输出通过"}]})
        };
        let executing = messages[latest]["content"] == "确认执行";
        let call = if !executing {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update(
                    "set_plan",
                    serde_json::json!({"acceptance_criteria":["shell输出通过"],"tasks":[{"id":"script","title":"交付检查脚本","expected_result":"check.py","acceptance_criteria":["shell输出通过"],"dependencies":[]}]}),
                ),
                _ => None,
            }
        } else if long_reason {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update("block_task", serde_json::json!({})),
                _ => None,
            }
        } else {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update("start_task", serde_json::json!({})),
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"check.py","content":"print('shell verified')\n"}),
                )),
                3 => update(
                    "record_result",
                    serde_json::json!({"result":"检查脚本已交付"}),
                ),
                4 | 7 => Some(("bash", serde_json::json!({"command":"python3 check.py"}))),
                5 => Some((
                    "write_file",
                    serde_json::json!({"path":"unbound-produced.txt","content":"不能重开交付"}),
                )),
                6 => update("accept_task", evidence(4)),
                8 => Some((
                    "write_file",
                    serde_json::json!({"path":"unbound-whole.txt","content":"不能重开交付"}),
                )),
                9 => update("accept_goal", evidence(7)),
                10 if !tools.last().unwrap()["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("\"phase\":\"completed\"") =>
                {
                    update(
                        "replan",
                        serde_json::json!({"objective":"停止失败的审查fixture"}),
                    )
                }
                _ => None,
            }
        };
        goal_fixture_chunk(call, tools.len())
    }

    #[tokio::test]
    async fn main_goal_block_rechecks_keep_shortage_and_stopped_conditions_need_explicit_continue()
    {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-blocks", "confirm").await;
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
            "blocks",
            "plan",
            "/goal 执行阻塞方案",
        )
        .await;
        goal_send(&fixture.app, &fixture.team, "blocks", "execute", "确认执行").await;
        let insufficient = goal_send(
            &fixture.app,
            &fixture.team,
            "blocks",
            "insufficient",
            "/goal continue",
        )
        .await;
        assert!(insufficient["text"]
            .as_str()
            .unwrap()
            .contains("仍缺地区资料，请补充地区"));
        assert!(!fixture.workspace.path().join("data.txt").exists());
        let edited = goal_send(
            &fixture.app,
            &fixture.team,
            "blocks",
            "conditions",
            "地区已补充为华东，请核对条件，暂不启动执行",
        )
        .await;
        assert!(edited["text"].as_str().unwrap().contains("/goal continue"));
        assert!(!fixture
            .workspace
            .path()
            .join("condition-bypass.txt")
            .exists());
        assert!(!fixture.workspace.path().join("data.txt").exists());
        let requests = fixture.model_seen.lock().await;
        let condition_request = requests
            .iter()
            .rposition(|r| {
                r["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["content"] == "地区已补充为华东，请核对条件，暂不启动执行")
            })
            .unwrap();
        let edited_fact = requests[condition_request]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .filter(|m| m["role"] == "tool")
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|g| g["id"].is_string())
            })
            .unwrap();
        assert_eq!(edited_fact["phase"], "paused");
        assert_eq!(edited_fact["tasks"][0]["status"], "pending");
        assert_eq!(edited_fact["tasks"][0]["block_reason"], "");
        assert_eq!(edited_fact["tasks"][2]["status"], "accepted");
        assert!(edited_fact["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "条件仍不足，仍缺地区资料，请补充地区"));
        assert!(requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .any(|s| s.contains("只能等待条件或恢复 pending")));
        drop(requests);
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        goal_request(
            &restarted,
            &fixture.team,
            "blocks",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        assert!(!fixture.workspace.path().join("data.txt").exists());
        let completed = goal_send(
            &restarted,
            &fixture.team,
            "blocks",
            "resume",
            "/goal continue",
        )
        .await;
        assert!(completed["text"]
            .as_str()
            .unwrap()
            .contains("整体验收已通过"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("data.txt")).unwrap(),
            "地区：华东"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("dependent.txt")).unwrap(),
            "报告引用地区：华东"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("independent.txt")).unwrap(),
            "独立资料已交付"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(!requests[calls..]
            .iter()
            .flat_map(|r| {
                let messages = r["messages"].as_array().unwrap();
                let latest = messages
                    .iter()
                    .rposition(|m| m["role"] == "user" && m["content"] == "/goal continue")
                    .unwrap();
                &messages[latest..]
            })
            .filter(|m| m["role"] == "assistant")
            .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
            .any(|c| c["function"]["name"] == "write_file"
                && c["function"]["arguments"]
                    .as_str()
                    .is_some_and(|s| s.contains("independent.txt"))));
    }

    #[tokio::test]
    async fn main_goal_execution_blocks_wait_dependencies_and_keep_independent_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-blocks", "confirm").await;
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
            "blocks",
            "plan",
            "/goal 执行阻塞方案",
        )
        .await;
        let answer = goal_send(&fixture.app, &fixture.team, "blocks", "execute", "确认执行").await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("independent.txt")).unwrap(),
            "独立资料已交付"
        );
        assert!(!fixture.workspace.path().join("dependent.txt").exists());
        assert!(!fixture.workspace.path().join("unbound.txt").exists());
        assert!(answer["text"]
            .as_str()
            .unwrap()
            .contains("缺少地区资料，请补充地区"));
        let requests = fixture.model_seen.lock().await;
        let results: Vec<_> = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect();
        let facts: Vec<serde_json::Value> = results
            .iter()
            .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .filter(|g| g["id"].is_string())
            .collect();
        let blocked = facts
            .iter()
            .find(|g| g["tasks"][0]["status"] == "blocked")
            .expect("模型可在没有工具尝试门槛时报告阻塞");
        assert_eq!(blocked["observations"], serde_json::json!([]));
        let pristine = facts
            .iter()
            .find(|g| g["plan_version"] == 1 && g["history"].as_array().unwrap().len() == 1)
            .unwrap();
        assert!(pristine["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "pending"));
        for message in ["归属", "任务不属于", "原因", "复核", "依赖尚未验收"] {
            assert!(
                results.iter().any(|s| s.contains(message)),
                "缺少拒绝结果：{message}"
            );
        }
        let final_fact = facts
            .iter()
            .rev()
            .find(|g| g["phase"] == "blocked")
            .expect("无可执行工作必须保存等待状态")
            .clone();
        assert_eq!(
            final_fact["tasks"][0]["block_reason"],
            "缺少地区资料，请补充地区"
        );
        assert_eq!(final_fact["tasks"][1]["status"], "pending");
        assert_eq!(final_fact["tasks"][2]["status"], "accepted");
        assert_eq!(final_fact["tasks"][2]["block_reason"], "");
        drop(requests);
        // 允许原有5秒目录重同步及其2秒请求窗口；不要求尽力队列中的每条投影均送达。
        let snapshot = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (_, read) = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap();
                if read["snapshot"]["goal"]["phase"] == "blocked" {
                    break read["snapshot"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "等待阻塞投影超时：{:?}",
                fixture.seen.try_lock().map(|messages| messages
                    .iter()
                    .filter(|m| m["type"] == "office.snapshot")
                    .map(|m| observed_goal(m, "blocks").clone())
                    .collect::<Vec<_>>())
            )
        });
        assert_eq!(snapshot["goal"]["progress"], 33);
        assert!(snapshot["goal"]["reason"]
            .as_str()
            .unwrap()
            .contains("缺少地区资料"));
        assert_eq!(snapshot["tasks"][0]["status"], "blocked");
        assert!(snapshot["tasks"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("缺少地区资料"));
        assert_eq!(snapshot["tasks"][1]["status"], "blocked");
        assert!(snapshot["tasks"][1]["summary"]
            .as_str()
            .unwrap()
            .contains("等待依赖"));
        assert_eq!(snapshot["whiteboard"]["status"], "blocked");
        assert!(snapshot["whiteboard"]["tagline"]
            .as_str()
            .unwrap()
            .contains("请补充地区"));
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let response = goal_request(
            &restarted,
            &fixture.team,
            "blocks",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        goal_send(
            &restarted,
            &fixture.team,
            "blocks",
            "inspect",
            "查看执行阻塞事实",
        )
        .await;
        let requests = fixture.model_seen.lock().await;
        let restored = requests[calls..]
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|g| g["phase"] == "blocked")
            })
            .unwrap();
        assert_eq!(restored["tasks"], final_fact["tasks"]);
        assert_eq!(restored["history"], final_fact["history"]);
        assert_eq!(restored["reason"], final_fact["reason"]);
    }

    fn block_model_response(messages: &[serde_json::Value]) -> Response {
        let latest = messages.iter().rposition(|m| m["role"] == "user").unwrap();
        let text = messages[latest]["content"].as_str().unwrap();
        let tools: Vec<_> = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        let current: serde_json::Value = tools
            .iter()
            .rev()
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|g| g["id"].is_string())
            })
            .unwrap_or_default();
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"task_id":task,"reason":"核对执行阻塞"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        if text == "查看执行阻塞事实" {
            return goal_fixture_chunk(
                if tools.is_empty() {
                    Some(("goal_read", serde_json::json!({})))
                } else {
                    None
                },
                tools.len(),
            );
        }
        if text == "/goal continue" || text.contains("地区已补充") {
            let evidence = |index: usize| serde_json::json!({"evidence":[{"criterion":0,"observation_id":tools[index]["content"].as_str().unwrap().rsplit("Goal observation: ").next().unwrap().trim(),"conclusion":"实际交付内容与地区一致"}]});
            let resolving = text.contains("地区已补充");
            let blocked = current["tasks"][0]["status"] == "blocked";
            let call = if resolving {
                match tools.len() {
                    0 | 4 => Some(("goal_read", serde_json::json!({}))),
                    1 => update(
                        "recheck_task",
                        "data",
                        serde_json::json!({"status":"pending","reason":"用户补充华东地区，模型确认条件已满足"}),
                    ),
                    2 => update("start_task", "data", serde_json::json!({})),
                    3 => Some((
                        "write_file",
                        serde_json::json!({"path":"condition-bypass.txt","content":"不得自动执行"}),
                    )),
                    _ => None,
                }
            } else if blocked {
                match tools.len() {
                    0 | 4 => Some(("goal_read", serde_json::json!({}))),
                    1 => update("start_task", "data", serde_json::json!({})),
                    2 => update(
                        "recheck_task",
                        "data",
                        serde_json::json!({"status":"produced","reason":"不能用阻塞重检伪造产出"}),
                    ),
                    3 => update(
                        "recheck_task",
                        "data",
                        serde_json::json!({"status":"blocked","reason":"条件仍不足，仍缺地区资料，请补充地区"}),
                    ),
                    _ => None,
                }
            } else {
                match tools.len() {
                    0 => Some(("goal_read", serde_json::json!({}))),
                    1 | 7 => update(
                        "start_task",
                        if tools.len() == 1 { "data" } else { "report" },
                        serde_json::json!({}),
                    ),
                    2 => Some((
                        "write_file",
                        serde_json::json!({"path":"data.txt","content":"地区：华东"}),
                    )),
                    3 => Some(("read_file", serde_json::json!({"path":"data.txt"}))),
                    4 | 10 => update(
                        "record_result",
                        if tools.len() == 4 { "data" } else { "report" },
                        serde_json::json!({"result":"真实资料已交付，待核对"}),
                    ),
                    5 => update(
                        "block_task",
                        "data",
                        serde_json::json!({"reason":"不得把未验收产物绕过复核变成待执行"}),
                    ),
                    6 | 11 => update(
                        "accept_task",
                        if tools.len() == 6 { "data" } else { "report" },
                        evidence(if tools.len() == 6 { 3 } else { 9 }),
                    ),
                    8 => Some((
                        "write_file",
                        serde_json::json!({"path":"dependent.txt","content":"报告引用地区：华东"}),
                    )),
                    9 | 12 => Some(("read_file", serde_json::json!({"path":"dependent.txt"}))),
                    13 => update("accept_goal", "", evidence(12)),
                    _ => None,
                }
            };
            return goal_fixture_chunk(call, tools.len());
        }
        let call = if text.starts_with("/goal ") && text != "/goal continue" {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update(
                    "set_plan",
                    "",
                    serde_json::json!({"acceptance_criteria":["地区、依赖报告和独立资料均可核对"],"tasks":[
                        {"id":"data","title":"地区资料","expected_result":"地区资料文件","acceptance_criteria":["地区字段完整"],"dependencies":[]},
                        {"id":"report","title":"依赖报告","expected_result":"引用地区的报告","acceptance_criteria":["引用地区资料"],"dependencies":["data"]},
                        {"id":"independent","title":"独立资料","expected_result":"独立资料文件","acceptance_criteria":["内容完整"],"dependencies":[]}
                    ]}),
                ),
                _ => None,
            }
        } else {
            match tools.len() {
                0 | 5 | 15 => Some(("goal_read", serde_json::json!({}))),
                1 => update(
                    "block_task",
                    "data",
                    serde_json::json!({"goal_id":"other-goal","reason":"不得污染当前目标"}),
                ),
                2 => update(
                    "block_task",
                    "foreign-task",
                    serde_json::json!({"reason":"其他目标任务"}),
                ),
                3 => update("block_task", "data", serde_json::json!({"reason":"  "})),
                4 => update(
                    "recheck_task",
                    "data",
                    serde_json::json!({"status":"pending","reason":"未阻塞不能假装解除"}),
                ),
                6 => update(
                    "block_task",
                    "data",
                    serde_json::json!({"reason":"缺少地区资料，请补充地区"}),
                ),
                7 => update("start_task", "report", serde_json::json!({})),
                8 => Some((
                    "write_file",
                    serde_json::json!({"path":"unbound.txt","content":"不得交付"}),
                )),
                9 => update("start_task", "independent", serde_json::json!({})),
                10 => Some((
                    "write_file",
                    serde_json::json!({"path":"independent.txt","content":"独立资料已交付"}),
                )),
                11 => Some(("read_file", serde_json::json!({"path":"independent.txt"}))),
                12 => update(
                    "record_result",
                    "independent",
                    serde_json::json!({"result":"独立资料已交付，待核对"}),
                ),
                13 => update(
                    "accept_task",
                    "independent",
                    serde_json::json!({"evidence":[{"criterion":0,"observation_id":tools[11]["content"].as_str().unwrap().rsplit("Goal observation: ").next().unwrap().trim(),"conclusion":"实际文件内容完整"}]}),
                ),
                14 => update(
                    "block_task",
                    "independent",
                    serde_json::json!({"reason":"不能撤销已验收事实"}),
                ),
                _ => None,
            }
        };
        goal_fixture_chunk(call, tools.len())
    }

    #[tokio::test]
    async fn main_goal_manual_pause_records_real_user_source_without_resuming_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-manual", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "manual",
            "plan",
            "/goal 人工约束下交付资料",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running = tokio::spawn(async move {
            goal_send(&app, &team, "manual", "execute", "确认执行").await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|request| {
                    let messages = request["messages"].as_array().unwrap();
                    messages
                        .iter()
                        .rposition(|m| m["role"] == "user" && m["content"] == "确认执行")
                        .is_some_and(|latest| {
                            messages[latest..]
                                .iter()
                                .filter(|m| m["role"] == "tool")
                                .count()
                                == 1
                        })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            goal_request(
                &fixture.app,
                &fixture.team,
                "manual",
                serde_json::json!({"type":"cancel","turnId":"execute"})
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(running.await.unwrap()["state"], "stopped");
        let answer = goal_send(
            &fixture.app,
            &fixture.team,
            "manual",
            "pause",
            "供应商那份资料先搁着，等我点头再碰。其余可以照做。",
        )
        .await;
        let text = answer["text"].as_str().unwrap();
        assert!(
            text.contains("manual_blocked"),
            "人工暂停应持久显示：{text}"
        );
        assert!(text.contains("供应商那份资料先搁着"));
        assert!(!fixture.workspace.path().join("edit-write.txt").exists());
        let requests = fixture.model_seen.lock().await;
        let goal = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| serde_json::from_str::<serde_json::Value>(m["content"].as_str()?).ok())
            .find(|g| g["tasks"][0]["status"] == "manual_blocked")
            .unwrap();
        assert_eq!(
            goal["tasks"][1]["status"], "manual_blocked",
            "原依赖后继也受人工约束"
        );
        assert_eq!(goal["tasks"][2]["status"], "pending");
        assert!(goal["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["action"] == "manual_pause"
                && entry["user_message_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && entry["user_excerpt"]
                    .as_str()
                    .is_some_and(|s| s.contains("先搁着"))));
        drop(requests);
        let continued = goal_send(
            &fixture.app,
            &fixture.team,
            "manual",
            "continue",
            "/goal continue",
        )
        .await;
        assert!(
            !fixture.workspace.path().join("bypass.txt").exists(),
            "拆分来源或删除依赖边不能绕过人工约束"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("independent.txt")).unwrap(),
            "独立清单"
        );
        assert!(continued["text"]
            .as_str()
            .unwrap()
            .contains("manual_blocked"));
        assert!(
            continued["text"].as_str().unwrap().contains("等待条件"),
            "只剩人工暂停任务时说明等待原因"
        );
        let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap()
                    .1;
                if snapshot["snapshot"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["id"].as_str().unwrap_or("").ends_with("-held-a"))
                {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("异步当前计划投影应到达既有任务区");
        let held = snapshot["snapshot"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"].as_str().unwrap_or("").ends_with("-held-a"))
            .unwrap();
        assert_eq!(held["status"], "blocked", "既有任务区应显示人工暂停");
        assert!(
            held["summary"].as_str().unwrap().contains("先搁着"),
            "既有摘要应说明人工来源"
        );
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        assert_eq!(
            goal_request(
                &restarted,
                &fixture.team,
                "manual",
                serde_json::json!({"type":"history"})
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "重载人工约束不能自行执行"
        );
        let restored = goal_send(
            &restarted,
            &fixture.team,
            "manual",
            "restore",
            "供应商已经允许，之前搁置的那份现在可以接着办。",
        )
        .await;
        assert!(restored["text"]
            .as_str()
            .unwrap()
            .contains("/goal continue"));
        assert!(
            !fixture.workspace.path().join("held-a.txt").exists(),
            "用户恢复决定仍不授予编辑回合交付权限"
        );
        assert!(!fixture.workspace.path().join("edit-write.txt").exists());
        let finished = goal_send(
            &restarted,
            &fixture.team,
            "manual",
            "continued-restored",
            "/goal continue",
        )
        .await;
        assert!(
            finished["text"].as_str().unwrap().contains("已验收 2/4"),
            "明确恢复及continue后保留独立结果并推进原Goal：{finished}"
        );
        assert!(fixture.workspace.path().join("held-a.txt").exists());
        assert!(
            !fixture.workspace.path().join("dependent.txt").exists(),
            "恢复后仍须等待任务依赖验收"
        );
    }

    fn manual_tasks() -> serde_json::Value {
        serde_json::json!([
            {"id":"held","title":"供应商资料","expected_result":"供应商资料文件","acceptance_criteria":["资料完整"],"dependencies":[]},
            {"id":"dependent","title":"资料报告","expected_result":"依据资料的报告","acceptance_criteria":["引用供应商资料"],"dependencies":["held"]},
            {"id":"independent","title":"本地清单","expected_result":"独立本地清单","acceptance_criteria":["清单可读取"],"dependencies":[]}
        ])
    }

    fn manual_model_response(messages: &[serde_json::Value]) -> Response {
        let latest = messages
            .iter()
            .rposition(|m| {
                m["role"] == "user" && !m["content"].as_str().unwrap_or("").starts_with("Goal 仍有")
            })
            .unwrap();
        let text = messages[latest]["content"].as_str().unwrap_or("");
        let tools = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let current: serde_json::Value = tools
            .iter()
            .rev()
            .find(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .is_some_and(|goal| goal["id"].is_string() && goal["plan_version"].is_number())
            })
            .and_then(|m| m["content"].as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let update = |action: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"尊重本轮用户控制"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let call = if tools.is_empty() {
            Some(("goal_read", serde_json::json!({})))
        } else if text.starts_with("/goal ") && text != "/goal continue" {
            if tools.len() == 1 {
                update(
                    "set_plan",
                    serde_json::json!({"acceptance_criteria":["三份资料完整可核对"],"tasks":manual_tasks()}),
                )
            } else {
                None
            }
        } else if text.contains("先搁着") {
            match tools.len() {
                1 => update(
                    "manual_pause",
                    serde_json::json!({"task_id":"held","user_excerpt":text}),
                ),
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"edit-write.txt","content":"禁止编辑回合执行"}),
                )),
                3 => Some(("goal_read", serde_json::json!({}))),
                _ => None,
            }
        } else if text.contains("之前搁置") {
            match tools.len() {
                1 => update(
                    "manual_resume",
                    serde_json::json!({"task_id":"held","user_excerpt":"不存在的用户原文"}),
                ),
                2 => update(
                    "manual_resume",
                    serde_json::json!({"task_id":"held","user_excerpt":text}),
                ),
                3 => Some((
                    "write_file",
                    serde_json::json!({"path":"edit-write.txt","content":"编辑恢复不能交付"}),
                )),
                4 => Some(("goal_read", serde_json::json!({}))),
                _ => None,
            }
        } else if text == "/goal continue"
            && current["tasks"].as_array().is_some_and(|tasks| {
                tasks
                    .iter()
                    .any(|task| task["current"] != false && task["status"] == "manual_blocked")
            })
        {
            let mut tasks = manual_tasks().as_array().unwrap().clone();
            let held = tasks.remove(0);
            tasks[0]["dependencies"] = serde_json::json!([]);
            for id in ["held-a", "held-b"] {
                let mut split = held.clone();
                split["id"] = serde_json::json!(id);
                split["replaces"] = serde_json::json!(["held"]);
                tasks.push(split);
            }
            match tools.len() {
                1 => update(
                    "manual_resume",
                    serde_json::json!({"task_id":"held","user_excerpt":"用户允许恢复"}),
                ),
                2 => update("start_task", serde_json::json!({"task_id":"held"})),
                3 => update("replan", serde_json::json!({"tasks":tasks})),
                4 => update("start_task", serde_json::json!({"task_id":"held-a"})),
                5 => Some((
                    "write_file",
                    serde_json::json!({"path":"bypass.txt","content":"不能绕过用户暂停"}),
                )),
                6 => update("start_task", serde_json::json!({"task_id":"dependent"})),
                7 => update(
                    "recheck_task",
                    serde_json::json!({"task_id":"held-a","status":"pending","reason":"模型自行恢复"}),
                ),
                8 => {
                    tasks[1]["expected_result"] = serde_json::json!("独立本地清单，可重新安排");
                    update("replan", serde_json::json!({"tasks":tasks}))
                }
                9 => update("start_task", serde_json::json!({"task_id":"independent"})),
                10 => Some((
                    "write_file",
                    serde_json::json!({"path":"independent.txt","content":"独立清单"}),
                )),
                11 => update(
                    "record_result",
                    serde_json::json!({"task_id":"independent","result":"独立清单已产出"}),
                ),
                12 => Some(("read_file", serde_json::json!({"path":"independent.txt"}))),
                13 => {
                    let content = tools
                        .get(12)
                        .and_then(|m| m["content"].as_str())
                        .unwrap_or("");
                    update(
                        "accept_task",
                        serde_json::json!({"task_id":"independent","evidence":[{"criterion":0,"observation_id":content.rsplit("Goal observation: ").next().unwrap_or("").trim(),"conclusion":"独立清单读取通过"}]}),
                    )
                }
                _ => None,
            }
        } else if text == "/goal continue" {
            let current_tasks = current["tasks"].as_array().unwrap();
            let tasks = current_tasks
                .iter()
                .filter(|task| task["current"] != false)
                .map(|task| {
                    let mut def = serde_json::Map::new();
                    for key in [
                        "id",
                        "title",
                        "expected_result",
                        "acceptance_criteria",
                        "dependencies",
                        "replaces",
                    ] {
                        if let Some(value) = task.get(key) {
                            def.insert(key.into(), value.clone());
                        }
                    }
                    if task["id"] == "dependent" {
                        def.insert(
                            "dependencies".into(),
                            serde_json::json!(["held-a", "held-b"]),
                        );
                    }
                    serde_json::Value::Object(def)
                })
                .collect::<Vec<_>>();
            let evidence = |index: usize, task: &str| {
                let content = tools
                    .get(index)
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or("");
                serde_json::json!({"task_id":task,"evidence":[{"criterion":0,"observation_id":content.rsplit("Goal observation: ").next().unwrap_or("").trim(),"conclusion":"实际读取结果通过"}]})
            };
            match tools.len() {
                1 => update("replan", serde_json::json!({"tasks":tasks})),
                2 => update("start_task", serde_json::json!({"task_id":"dependent"})),
                3 => update("start_task", serde_json::json!({"task_id":"held-a"})),
                4 => Some((
                    "write_file",
                    serde_json::json!({"path":"held-a.txt","content":"供应商资料已核对"}),
                )),
                5 => update(
                    "record_result",
                    serde_json::json!({"task_id":"held-a","result":"实际文件已产出"}),
                ),
                6 => Some(("read_file", serde_json::json!({"path":"held-a.txt"}))),
                7 => update("accept_task", evidence(6, "held-a")),
                _ => None,
            }
        } else {
            None
        };
        goal_fixture_chunk(call, tools.len())
    }

    #[tokio::test]
    async fn main_goal_cancel_joins_bound_children_without_late_delivery_or_restart() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-delegate", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "cancel-parallel",
            "plan",
            "/goal 并行交付甲乙并整合",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running = tokio::spawn(async move {
            goal_send(&app, &team, "cancel-parallel", "execute", "确认执行").await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .model_seen
                    .lock()
                    .await
                    .iter()
                    .filter(|r| {
                        r["messages"].as_array().unwrap().iter().any(|m| {
                            matches!(
                                m["content"].as_str(),
                                Some("goal-child-a" | "/goal child-b")
                            )
                        })
                    })
                    .count()
                    == 2
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("等待两个在途子请求");
        let response = goal_request(
            &fixture.app,
            &fixture.team,
            "cancel-parallel",
            serde_json::json!({"type":"cancel","turnId":"execute"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let answer = tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(answer["state"], "stopped");
        let calls = fixture.model_seen.lock().await.len();
        fixture.release_model.notify_waiters();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let response = goal_request(
            &restarted,
            &fixture.team,
            "cancel-parallel",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "停止后的迟到模型响应与历史加载均不能重新执行"
        );
        for file in ["a.txt", "b.txt", "report.txt"] {
            assert!(!fixture.workspace.path().join(file).exists());
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (_, read) = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap();
                if read["snapshot"]["goal"]["phase"] == "paused" {
                    assert_eq!(read["snapshot"]["goal"]["acceptedTaskCount"], 0);
                    let tasks = read["snapshot"]["tasks"].as_array().unwrap();
                    assert_eq!(tasks.len(), 3);
                    assert_eq!(
                        tasks
                            .iter()
                            .filter(|task| task["status"] == "blocked"
                                && matches!(task["title"].as_str(), Some("甲" | "乙")))
                            .count(),
                        2
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("取消后的 Goal 与子任务中断事实应投影");
    }

    #[tokio::test]
    async fn main_goal_delegates_independent_children_and_parent_verifies_before_dependencies() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-delegate", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "parallel",
            "plan",
            "/goal 并行交付甲乙并整合",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running = tokio::spawn(async move {
            goal_send(&app, &team, "parallel", "execute", "确认执行").await
        });
        let overlap = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let requests = fixture.model_seen.lock().await;
                let children = requests
                    .iter()
                    .filter(|request| {
                        request["messages"].as_array().unwrap().iter().any(|m| {
                            m["role"] == "user"
                                && matches!(
                                    m["content"].as_str(),
                                    Some("goal-child-a" | "/goal child-b")
                                )
                        })
                    })
                    .count();
                if children == 2 {
                    break;
                }
                drop(requests);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            overlap.is_ok(),
            "两个真实子模型请求必须重叠等待：{:?}",
            fixture
                .model_seen
                .lock()
                .await
                .iter()
                .map(|r| r["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .take(2)
                    .map(|m| (m["role"].clone(), m["content"].clone()))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        let read = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (_, read) = fixture
                    .state
                    .main_office
                    .message(
                        serde_json::json!({"type":"office.read","teamId":fixture.team}),
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap();
                if read["snapshot"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|task| task["status"] == "running")
                    .count()
                    == 2
                {
                    break read;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("异步 Office 投影应同步真实子任务归属");
        let tasks = read["snapshot"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 3, "Goal 委派不生成重复 exec-* 任务");
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task["status"] == "running")
                .count(),
            2
        );
        let assignees: Vec<_> = tasks
            .iter()
            .filter(|task| task["status"] == "running")
            .map(|task| task["assigneeId"].as_str().unwrap())
            .collect();
        assert_ne!(assignees[0], assignees[1]);
        assert!(!fixture.workspace.path().join("report.txt").exists());
        fixture.release_model.notify_one();
        fixture.release_model.notify_one();
        // 子结果已返回、主模型在验收前等待：比例仍为0，依赖任务仍不能启动。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|r| {
                    r["messages"].as_array().unwrap().iter().any(|m| {
                        m["role"] == "tool"
                            && m["content"]
                                .as_str()
                                .is_some_and(|s| s.contains("尚未验收"))
                    })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!fixture.workspace.path().join("report.txt").exists());
        let (_, read) = fixture
            .state
            .main_office
            .message(
                serde_json::json!({"type":"office.read","teamId":fixture.team}),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(
            read["snapshot"]["goal"]["acceptedTaskCount"], 0,
            "子返回不等于验收"
        );
        fixture.release_model.notify_one();
        let answer = running.await.unwrap();
        assert_eq!(answer["state"], "complete");
        assert!(answer["text"].as_str().unwrap().contains("整体验收已通过"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("a.txt")).unwrap(),
            "甲"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("b.txt")).unwrap(),
            "乙"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("report.txt")).unwrap(),
            "甲乙"
        );
        let requests = fixture.model_seen.lock().await;
        assert!(
            !requests.iter().any(|r| r["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "user" && m["content"] == "terminal-child")),
            "整体验收后同一回合不能退化为普通聊天继续启动子执行"
        );
        let results: Vec<_> = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(results.iter().any(|s| s.contains("依赖")));
        assert!(results
            .iter()
            .any(|s| s.contains("重复") || s.contains("已被")));
        assert!(results.iter().any(|s| s.contains("验收依据")));
        assert!(results.iter().any(|s| s.contains("Goal 交付工具必须绑定")));
        assert!(
            results
                .iter()
                .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .any(|goal| goal["plan_version"] == 2
                    && goal["tasks"].as_array().is_some_and(|tasks| tasks
                        .iter()
                        .filter(|task| task["assignee_id"].is_string())
                        .count()
                        == 2)),
            "重规划保留未改定义任务的真实子归属"
        );
    }

    /// 功能：通过外部模型边界驱动真实父子委派和父验收。
    /// 参数：body 为真实请求，gate 为测试控制的外部响应屏障。
    /// 返回：模型 SSE 响应。
    /// 错误：fixture 格式错误通过断言暴露。
    /// 状态：不修改 Host、Goal 或会话。
    /// 并发与取消：子初始请求与主验收请求等待独立通知，取消由真实运行时处理。
    /// 权限：仅测试模型边界。
    /// 关联：主聊天 Goal 并行集成测试。
    pub(super) async fn delegation_model_response(
        body: &serde_json::Value,
        gate: &tokio::sync::Notify,
    ) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let latest = messages.iter().rposition(|m| m["role"] == "user").unwrap();
        let tools: Vec<_> = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        let text = messages[latest]["content"].as_str().unwrap();
        if matches!(text, "goal-child-a" | "/goal child-b") {
            assert!(
                !body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| matches!(
                        tool["function"]["name"].as_str(),
                        Some("goal_read" | "goal_update" | "delegate_task")
                    )),
                "委派文本不是用户 Goal 命令，子会话没有目标控制权限"
            );
            if tools.is_empty() {
                gate.notified().await;
                return goal_fixture_chunk(
                    Some((
                        "goal_update",
                        serde_json::json!({"action":"accept_goal","goal_id":"parent","expected_version":1,"reason":"child must not control Goal"}),
                    )),
                    0,
                );
            }
            return goal_fixture_chunk(
                if tools.len() == 1 {
                    Some((
                        "write_file",
                        serde_json::json!({"path":if text.ends_with('a') {"a.txt"} else {"b.txt"},"content":if text.ends_with('a') {"甲"} else {"乙"}}),
                    ))
                } else {
                    None
                },
                tools.len(),
            );
        }
        let current: serde_json::Value = tools
            .iter()
            .rev()
            .filter_map(|m| m["content"].as_str())
            .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .find(|goal| goal["id"].is_string())
            .unwrap_or_default();
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"核对真实委派结果","task_id":task});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let evidence = |index: usize| {
            let id = tools[index]["content"]
                .as_str()
                .unwrap()
                .rsplit("Goal observation: ")
                .next()
                .unwrap()
                .trim();
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"实际文件符合验收条件"}]})
        };
        if text != "确认执行" {
            return goal_fixture_chunk(
                match tools.len() {
                    0 => Some(("goal_read", serde_json::json!({}))),
                    1 => Some((
                        "goal_update",
                        serde_json::json!({"action":"set_plan","goal_id":current["id"],"expected_version":current["plan_version"],"reason":"独立交付后整合","acceptance_criteria":["报告为甲乙"],"tasks":[
                            {"id":"a","title":"甲","expected_result":"a.txt","acceptance_criteria":["文件为甲"],"dependencies":[]},
                            {"id":"b","title":"乙","expected_result":"b.txt","acceptance_criteria":["文件为乙"],"dependencies":[]},
                            {"id":"report","title":"整合","expected_result":"report.txt","acceptance_criteria":["文件为甲乙"],"dependencies":["a","b"]}
                        ]}),
                    )),
                    _ => None,
                },
                tools.len(),
            );
        }
        if tools.len() == 1 {
            let calls: Vec<_> = [("a","goal-child-a"),("b","/goal child-b"),("a","duplicate-child"),("report","premature-child")].into_iter().enumerate().map(|(i,(id,task))| serde_json::json!({"index":i,"id":format!("delegate-{i}"),"type":"function","function":{"name":"delegate_task","arguments":serde_json::json!({"task":task,"name":id,"goal_task_id":id}).to_string()}})).collect();
            let mut calls = calls;
            calls.push(serde_json::json!({"index":4,"id":"unbound-parent","type":"function","function":{"name":"sleep","arguments":"{\"duration_ms\":0}"}}));
            let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]});
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        if tools.len() == 6 {
            gate.notified().await;
        }
        goal_fixture_chunk(
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                6 => update(
                    "replan",
                    "",
                    serde_json::json!({"tasks":[
                        {"id":"a","title":"甲","expected_result":"a.txt","acceptance_criteria":["文件为甲"],"dependencies":[]},
                        {"id":"b","title":"乙","expected_result":"b.txt","acceptance_criteria":["文件为乙"],"dependencies":[]},
                        {"id":"report","title":"整合","expected_result":"report.txt","acceptance_criteria":["文件为甲乙"],"dependencies":["a","b"]}
                    ]}),
                ),
                7 => update("start_task", "report", serde_json::json!({})),
                8 => update("start_task", "a", serde_json::json!({})),
                9 => update(
                    "accept_task",
                    "a",
                    serde_json::json!({"evidence":[{"criterion":0,"observation_id":"child_unverified","conclusion":"子返回不算验收"}]}),
                ),
                10 => Some(("read_file", serde_json::json!({"path":"a.txt"}))),
                11 => update("accept_task", "a", evidence(10)),
                12 => update("start_task", "b", serde_json::json!({})),
                13 => Some(("read_file", serde_json::json!({"path":"b.txt"}))),
                14 => update("accept_task", "b", evidence(13)),
                15 => update("start_task", "report", serde_json::json!({})),
                16 => Some((
                    "write_file",
                    serde_json::json!({"path":"report.txt","content":"甲乙"}),
                )),
                17 => update(
                    "record_result",
                    "report",
                    serde_json::json!({"result":"report.txt 已产出"}),
                ),
                18 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                19 => update("accept_task", "report", evidence(18)),
                20 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                21 => update("accept_goal", "", evidence(20)),
                22 => Some((
                    "delegate_task",
                    serde_json::json!({"task":"terminal-child","name":"结束后越权"}),
                )),
                _ => None,
            },
            tools.len(),
        )
    }

    #[tokio::test]
    async fn main_goal_replans_after_real_failure_retains_acceptance_and_replacement_history() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-replan", "confirm").await;
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
            "replan",
            "plan",
            "/goal 重规划交付四份资料",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running = tokio::spawn(async move {
            goal_send(&app, &team, "replan", "execute", "确认执行").await
        });
        for (progress, version) in [(50, 1), (40, 2)] {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if fixture.seen.lock().await.iter().any(|m| {
                        observed_goal(m, "replan")["progress"] == progress
                            && observed_goal(m, "replan")["planVersion"] == version
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("重规划必须投影真实的2/4到2/5比例");
            fixture.release_model.notify_one();
        }
        let answer = running.await.unwrap();
        assert!(answer["text"]
            .as_str()
            .unwrap()
            .contains("缺失文件，拆分修复并补充复核"));
        assert!(answer["text"].as_str().unwrap().contains("修订汇总"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("repair.txt")).unwrap(),
            "修复已交付"
        );
        assert!(
            !fixture.workspace.path().join("bypass.txt").exists(),
            "目标越界暂停之后不能继续交付"
        );
        let requests = fixture.model_seen.lock().await;
        let results: Vec<_> = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(
            results.iter().any(|s| s.contains("依赖尚未验收")),
            "改写后的依赖必须生效"
        );
        assert!(
            results
                .iter()
                .any(|s| s.contains("计划定义") || s.contains("执行状态")),
            "不能注入accepted状态"
        );
        assert!(
            results
                .iter()
                .any(|s| s.contains("整体目标") && s.contains("停止")),
            "执行模型不得改变目标"
        );
        assert!(
            results
                .iter()
                .any(|s| s.contains("替换来源必须退出当前计划")),
            "保留原任务时不能伪造替换关系"
        );
        assert!(
            results
                .iter()
                .any(|s| s.contains("历史任务身份不可重新使用")),
            "历史身份不能重生覆盖执行事实"
        );
        let latest: serde_json::Value = results
            .iter()
            .rev()
            .find_map(|s| {
                serde_json::from_str::<serde_json::Value>(s)
                    .ok()
                    .filter(|v| v["phase"] == "paused")
            })
            .expect("越界请求应持久暂停");
        assert_eq!(latest["objective"], "重规划交付四份资料");
        assert_eq!(
            latest["acceptance_criteria"],
            serde_json::json!(["四份资料一致且完整"])
        );
        let tasks = latest["tasks"].as_array().unwrap();
        assert_eq!(tasks.iter().filter(|t| t["current"] == true).count(), 5);
        assert_eq!(
            tasks.iter().find(|t| t["id"] == "one").unwrap()["status"],
            "accepted"
        );
        assert_eq!(
            tasks.iter().find(|t| t["id"] == "two").unwrap()["status"],
            "accepted"
        );
        assert_eq!(
            tasks.iter().find(|t| t["id"] == "failed").unwrap()["current"],
            false
        );
        assert_eq!(
            tasks.iter().find(|t| t["id"] == "repair").unwrap()["replaces"],
            serde_json::json!(["failed"])
        );
        assert!(latest["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["is_error"] == true
                && o["task_id"] == "failed"
                && o["input"]["path"] == "missing.txt"));
        assert!(latest["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["action"] == "replan"
                && entry["reason"] == "缺失文件，拆分修复并补充复核"
                && entry["tasks"].as_array().is_some_and(|ts| ts
                    .iter()
                    .any(|t| t["id"] == "failed" && t["current"] == true))));
        drop(requests);
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let response = goal_request(
            &restarted,
            &fixture.team,
            "replan",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        assert!(history.to_string().contains("拆分修复并补充复核"));
        goal_send(
            &restarted,
            &fixture.team,
            "replan",
            "inspect",
            "查看重规划事实",
        )
        .await;
        let requests = fixture.model_seen.lock().await;
        let restored = requests[calls..]
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .find_map(|s| {
                serde_json::from_str::<serde_json::Value>(s)
                    .ok()
                    .filter(|g| g["phase"] == "paused")
            })
            .expect("重载后通过工具读取持久事实");
        assert_eq!(restored["history"], latest["history"]);
        assert_eq!(restored["observations"], latest["observations"]);
        assert_eq!(restored["tasks"], latest["tasks"]);
    }

    fn replan_tasks(revised: bool) -> serde_json::Value {
        let task = |id: &str, title: &str, deps: &[&str], replaces: &[&str]| serde_json::json!({"id":id,"title":title,"expected_result":title,"acceptance_criteria":["文件内容可复核"],"dependencies":deps,"replaces":replaces});
        let mut tasks = vec![
            task("one", "资料一", &[], &[]),
            task("two", "资料二", &[], &[]),
        ];
        if revised {
            tasks.extend([
                task("repair", "修复资料", &[], &["failed"]),
                task("review", "补充复核", &["repair"], &["failed"]),
                task("summary", "修订汇总", &["review"], &[]),
            ]);
        } else {
            tasks.extend([
                task("failed", "原始资料", &[], &[]),
                task("summary", "原汇总", &["failed"], &[]),
            ]);
            for task in &mut tasks {
                task.as_object_mut().unwrap().remove("replaces");
            }
        }
        serde_json::json!(tasks)
    }

    fn replan_model_response(messages: &[serde_json::Value]) -> Response {
        let latest = messages.iter().rposition(|m| m["role"] == "user").unwrap();
        let tools: Vec<_> = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        let current: serde_json::Value = tools
            .iter()
            .rev()
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|v| v["id"].is_string())
            })
            .unwrap_or_default();
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"核对实际结果","task_id":task});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        if messages[latest]["content"] == "查看重规划事实" {
            return goal_fixture_chunk(
                if tools.is_empty() {
                    Some(("goal_read", serde_json::json!({})))
                } else {
                    None
                },
                tools.len(),
            );
        }
        if messages[latest]["content"] != "确认执行" {
            return goal_fixture_chunk(
                match tools.len() {
                    0 => Some(("goal_read", serde_json::json!({}))),
                    1 => update(
                        "set_plan",
                        "",
                        serde_json::json!({"acceptance_criteria":["四份资料一致且完整"],"tasks":replan_tasks(false)}),
                    ),
                    _ => None,
                },
                tools.len(),
            );
        }
        let evidence = |index: usize| {
            let id = tools[index]["content"]
                .as_str()
                .unwrap()
                .rsplit("Goal observation: ")
                .next()
                .unwrap()
                .trim();
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"实际读取内容一致"}]})
        };
        let call = match tools.len() {
            0 => Some(("goal_read", serde_json::json!({}))),
            1 | 6 => update(
                "start_task",
                if tools.len() == 1 { "one" } else { "two" },
                serde_json::json!({}),
            ),
            2 | 7 => Some((
                "write_file",
                serde_json::json!({"path":if tools.len()==2 {"one.txt"} else {"two.txt"},"content":"真实资料"}),
            )),
            3 | 8 => Some((
                "read_file",
                serde_json::json!({"path":if tools.len()==3 {"one.txt"} else {"two.txt"}}),
            )),
            4 | 9 => update(
                "record_result",
                if tools.len() == 4 { "one" } else { "two" },
                serde_json::json!({"result":"实际资料已写入"}),
            ),
            5 | 10 => update(
                "accept_task",
                if tools.len() == 5 { "one" } else { "two" },
                evidence(if tools.len() == 5 { 3 } else { 8 }),
            ),
            11 => update("start_task", "failed", serde_json::json!({})),
            12 => Some(("read_file", serde_json::json!({"path":"missing.txt"}))),
            13 => update(
                "replan",
                "",
                serde_json::json!({"reason":"缺失文件，拆分修复并补充复核","tasks":replan_tasks(true)}),
            ),
            14 => update("start_task", "summary", serde_json::json!({})),
            15 => {
                let mut tasks = replan_tasks(true);
                tasks[2]["status"] = serde_json::json!("accepted");
                update("replan", "", serde_json::json!({"tasks":tasks}))
            }
            16 => {
                let mut tasks = replan_tasks(true);
                let mut clone = tasks[0].clone();
                clone["id"] = serde_json::json!("clone");
                clone["replaces"] = serde_json::json!(["one"]);
                tasks.as_array_mut().unwrap().push(clone);
                update("replan", "", serde_json::json!({"tasks":tasks}))
            }
            17 => {
                let mut tasks = replan_tasks(true);
                let mut clone = tasks[0].clone();
                clone["id"] = serde_json::json!("failed");
                tasks.as_array_mut().unwrap().push(clone);
                update("replan", "", serde_json::json!({"tasks":tasks}))
            }
            18 => update("start_task", "repair", serde_json::json!({})),
            19 => Some((
                "write_file",
                serde_json::json!({"path":"repair.txt","content":"修复已交付"}),
            )),
            20 => update(
                "replan",
                "",
                serde_json::json!({"objective":"放弃原目标","acceptance_criteria":["任意回复即通过"],"tasks":replan_tasks(true)}),
            ),
            21 => Some((
                "write_file",
                serde_json::json!({"path":"bypass.txt","content":"不得写入"}),
            )),
            22 => Some(("goal_read", serde_json::json!({}))),
            _ => None,
        };
        goal_fixture_chunk(call, tools.len())
    }

    #[tokio::test]
    async fn main_goal_stopped_user_can_change_requirements_without_resuming_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-resume-produced", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let stream = lifecycle_start(&fixture, "execute").await;
        wait_report(&fixture).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|request| {
                    let messages = request["messages"].as_array().unwrap();
                    messages
                        .iter()
                        .rposition(|m| m["role"] == "user" && m["content"] == "确认执行")
                        .is_some_and(|index| {
                            messages[index..]
                                .iter()
                                .filter(|m| m["role"] == "tool")
                                .count()
                                == 9
                        })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(stream);
        wait_goal_turn(&fixture, "execute", "stopped").await;
        let edited = goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "edit",
            "目标改为复核七，整体条件改为报告准确；继续前先只修改要求。",
        )
        .await;
        let text = edited["text"].as_str().unwrap();
        assert!(text.contains("目标：复核七"));
        assert!(
            text.contains("/goal continue"),
            "停止后编辑仍须显式继续，不能提示重新初始确认"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("report.txt")).unwrap(),
            "原始报告：七"
        );
        let finished = goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "continue",
            "/goal continue",
        )
        .await;
        assert!(finished["text"]
            .as_str()
            .unwrap()
            .contains("整体验收已通过"));
        let requests = fixture.model_seen.lock().await;
        assert!(
            requests
                .iter()
                .flat_map(|r| r["messages"].as_array().unwrap())
                .filter(|m| m["role"] == "tool")
                .filter_map(|m| m["content"].as_str())
                .any(|s| s.contains("\"phase\":\"paused\"")
                    && s.contains("\"status\":\"accepted\"")
                    && s.contains("复核七")),
            "用户编辑保留有效验收事实并保持停止"
        );
    }

    #[tokio::test]
    async fn main_goal_stream_drop_stops_exact_turn_and_unpolled_close_cannot_start_work() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-resume-late", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let stream = lifecycle_start(&fixture, "execute").await;
        wait_report(&fixture).await;
        let old_observer = goal_request(
            &fixture.app,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"resume","turnId":"execute"}),
        )
        .await;
        drop(stream);
        wait_goal_turn(&fixture, "execute", "stopped").await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let continued = tokio::spawn(async move {
            goal_send(&app, &team, "resume", "continue", "/goal continue").await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture.model_seen.lock().await.iter().any(|r| {
                    r["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "user" && m["content"] == "/goal continue")
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(old_observer);
        wait_goal_turn(&fixture, "continue", "pending").await;
        fixture.release_model.notify_one();
        assert_eq!(
            continued.await.unwrap()["state"],
            "complete",
            "迟到的旧观察流不能取消新回合"
        );
        let early = main_fixture("goal-resume", "confirm").await;
        goal_send(
            &early.app,
            &early.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let before = early.model_seen.lock().await.len();
        drop(lifecycle_start(&early, "close-before-loop").await);
        wait_goal_turn(&early, "close-before-loop", "stopped").await;
        assert!(!early.workspace.path().join("data.txt").exists());
        assert_eq!(
            early.model_seen.lock().await.len(),
            before,
            "未轮询响应体便关闭也不能启动模型"
        );
    }

    async fn wait_goal_turn(fixture: &MainFixture, turn: &str, state: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = goal_request(
                    &fixture.app,
                    &fixture.team,
                    "resume",
                    serde_json::json!({"type":"history"}),
                )
                .await;
                let page: serde_json::Value = serde_json::from_slice(
                    &to_bytes(response.into_body(), 1_000_000).await.unwrap(),
                )
                .unwrap();
                if page["data"]["turns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == turn && row["state"] == state)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("回合 {turn} 应为 {state}"));
    }

    #[tokio::test]
    async fn main_goal_continue_rechecks_interrupted_output_without_repeating_it() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-resume", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let response = lifecycle_start(&fixture, "execute").await;
        wait_report(&fixture).await;
        // 捕获真实运行中的原始 home；不改写 Goal/Turn 事件，模拟进程丢失后的冷加载。
        let captured = tempfile::tempdir().unwrap();
        copy_lifecycle_home(fixture.home.path(), captured.path());
        let mut config = fixture.state.config.clone();
        config.home = captured.path().into();
        let published = fixture.seen.lock().await.len();
        let mut reloaded_state = AppState::new(config.clone());
        Arc::get_mut(&mut reloaded_state).unwrap().main_office = crate::office::MainOffice::new(
            &config.home,
            &config.workspace,
            Some(
                std::env::var("OPENPAW_OPENAI_BASE_URL")
                    .unwrap()
                    .trim_end_matches("/v1")
                    .to_string(),
            ),
        );
        reloaded_state
            .office
            .attach_main(&reloaded_state.main_office);
        let restarted = build_router(reloaded_state);
        let calls = fixture.model_seen.lock().await.len();
        let overview = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(snapshot) = fixture.seen.lock().await[published..]
                    .iter()
                    .filter(|message| message["type"] == "offices.initialize")
                    .filter_map(|message| message["offices"].as_array())
                    .flatten()
                    .find(|snapshot| {
                        snapshot["workspace"]["id"] == fixture.team
                            && snapshot["goal"]["phase"] == "paused"
                    })
                    .cloned()
                {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("打开聊天历史前，冷启动的真实办公室快照必须显示暂停");
        assert_eq!(overview["tasks"][0]["status"], "completed");
        assert_eq!(overview["tasks"][1]["status"], "blocked");
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "办公室恢复不启动模型"
        );
        let loaded = goal_request(
            &restarted,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"history"}),
        )
        .await;
        let loaded: serde_json::Value =
            serde_json::from_slice(&to_bytes(loaded.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        assert!(
            loaded["data"]["turns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == "execute" && row["state"] == "error"),
            "真实运行日志冷加载应显示中断"
        );
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "冷加载 active/running 事实不能恢复执行许可"
        );
        let cancelled = goal_request(
            &fixture.app,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"cancel","turnId":"execute"}),
        )
        .await;
        assert_eq!(cancelled.status(), StatusCode::OK);
        drop(response);
        let calls = fixture.model_seen.lock().await.len();
        let history = goal_request(
            &restarted,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(history.status(), StatusCode::OK);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        let other = goal_send(
            &restarted,
            &fixture.team,
            "other",
            "continue-other",
            "/goal continue",
        )
        .await;
        assert!(other["text"].as_str().unwrap().contains("当前聊天没有"));
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "其他聊天不能接管或启动模型"
        );
        let finished = goal_send(
            &restarted,
            &fixture.team,
            "resume",
            "continue",
            "/goal continue",
        )
        .await;
        assert_eq!(finished["state"], "complete");
        assert!(finished["text"]
            .as_str()
            .unwrap()
            .contains("整体验收已通过"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("report.txt")).unwrap(),
            "原始报告：七"
        );
        let requests = fixture.model_seen.lock().await;
        let results = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>();
        assert!(
            results.iter().any(|s| s.contains("复核")),
            "重复启动必须被拒绝"
        );
        assert!(
            results
                .iter()
                .any(|s| s.contains("accepted") && s.contains("interrupted")),
            "恢复前保留验收和中断事实"
        );
    }

    async fn lifecycle_start(fixture: &MainFixture, turn: &str) -> Response {
        let history = goal_request(
            &fixture.app,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"history"}),
        )
        .await;
        let history: serde_json::Value =
            serde_json::from_slice(&to_bytes(history.into_body(), 1_000_000).await.unwrap())
                .unwrap();
        let mut messages = main_conversation_context(history["data"]["turns"].as_array().unwrap())
            .into_iter()
            .map(|m| serde_json::json!({"role":m.role,"content":m.content}))
            .collect::<Vec<_>>();
        messages.push(serde_json::json!({"role":"user","content":"确认执行"}));
        goal_request(
            &fixture.app,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"send","turnId":turn,"messages":messages}),
        )
        .await
    }

    async fn wait_report(fixture: &MainFixture) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.workspace.path().join("report.txt").exists()
                    && fixture.model_seen.lock().await.iter().any(|request| {
                        let messages = request["messages"].as_array().unwrap();
                        messages
                            .iter()
                            .rposition(|m| m["role"] == "user" && m["content"] == "确认执行")
                            .is_some_and(|index| {
                                messages[index..]
                                    .iter()
                                    .filter(|m| m["role"] == "tool")
                                    .count()
                                    == 8
                            })
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("中断前真实产物已存在");
    }

    fn copy_lifecycle_home(source: &std::path::Path, target: &std::path::Path) {
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir(&destination).unwrap();
                copy_lifecycle_home(&entry.path(), &destination);
            } else {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn main_goal_agent_level_error_pauses_without_automatic_retry() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-agent-error", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let response = lifecycle_start(&fixture, "execute").await;
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"state\":\"error\""));
        assert_lifecycle_paused(&fixture).await;
        let calls = fixture.model_seen.lock().await.len();
        let restored = build_router(AppState::new(fixture.state.config.clone()));
        let history = goal_request(
            &restored,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(history.status(), StatusCode::OK);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        assert!(!fixture.workspace.path().join("late.txt").exists());
    }

    async fn assert_lifecycle_paused(fixture: &MainFixture) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture.seen.lock().await.iter().any(|m| {
                    observed_goal(m, "resume")["phase"] == "paused"
                        && observed_goal(m, "resume")["acceptedTaskCount"] == 1
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Goal 应保存暂停和验收事实");
    }

    #[tokio::test]
    #[ignore = "真实等待原 main 180 秒执行期限，独立运行"]
    async fn main_goal_original_180_second_deadline_saves_pause_and_prevents_later_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-deadline", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "resume",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let started = std::time::Instant::now();
        let response = lifecycle_start(&fixture, "execute").await;
        let bytes = tokio::time::timeout(
            Duration::from_secs(185),
            to_bytes(response.into_body(), 1_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(179));
        let final_line = std::str::from_utf8(&bytes).unwrap().lines().last().unwrap();
        let final_snapshot: serde_json::Value = serde_json::from_str(final_line).unwrap();
        assert_ne!(final_snapshot["state"], "pending");
        wait_goal_turn(&fixture, "execute", "stopped").await;
        assert_lifecycle_paused(&fixture).await;
        fixture.release_model.notify_one();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("data.txt")).unwrap(),
            "七"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("report.txt")).unwrap(),
            "原始报告：七"
        );
        assert!(!fixture.workspace.path().join("late.txt").exists());
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let history = goal_request(
            &restarted,
            &fixture.team,
            "resume",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(history.status(), StatusCode::OK);
        assert_eq!(fixture.model_seen.lock().await.len(), calls);
        eprintln!(
            "Goal original deadline evidence: {}",
            serde_json::json!({"elapsedSeconds":started.elapsed().as_secs(),"originalDeadlineSeconds":180,"terminal":final_snapshot,"acceptedDataKept":true,"interruptedReportKept":true,"lateDeliveryAbsent":true,"reloadModelCalls":0})
        );
    }

    /// 功能：从模型 HTTP 边界提供停止及继续的确定性响应。
    /// 参数：body 为实际请求。
    /// 返回：本地 SSE 模型响应。
    /// 错误：fixture 输入错误由测试暴露。
    /// 状态：不操作 Host 内部状态。
    /// 并发与取消：由外部模型等待控制中断窗口。
    /// 权限：仅测试。
    /// 关联：Goal 生命周期集成测试。
    pub(super) fn lifecycle_model_response(body: &serde_json::Value) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let latest = messages
            .iter()
            .rposition(|m| {
                m["role"] == "user" && !m["content"].as_str().unwrap_or("").starts_with("Goal 仍有")
            })
            .unwrap();
        let text = messages[latest]["content"].as_str().unwrap_or("");
        if text.starts_with("/goal ") && text != "/goal continue" {
            return goal_model_response(body);
        }
        let tools = messages[latest..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect::<Vec<_>>();
        let current: serde_json::Value = tools
            .first()
            .and_then(|m| m["content"].as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"task_id":task,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"依据真实产物继续"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let observation = |index: usize| {
            tools
                .get(index)
                .and_then(|m| m["content"].as_str())
                .unwrap_or("")
                .rsplit("Goal observation: ")
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let evidence = |index| serde_json::json!({"evidence":[{"criterion":0,"observation_id":observation(index),"conclusion":"实际内容与条件一致"}]});
        if text.starts_with("目标改为复核七") {
            let call = match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => {
                    let definitions = current["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|task| {
                            let mut definition = serde_json::Map::new();
                            for name in [
                                "id",
                                "title",
                                "expected_result",
                                "acceptance_criteria",
                                "dependencies",
                            ] {
                                definition.insert(name.into(), task[name].clone());
                            }
                            serde_json::Value::Object(definition)
                        })
                        .collect::<Vec<_>>();
                    update(
                        "set_plan",
                        "",
                        serde_json::json!({"objective":"复核七","acceptance_criteria":["报告准确"],"tasks":definitions,"user_excerpt":text}),
                    )
                }
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"report.txt","content":"错误重复"}),
                )),
                _ => None,
            };
            return goal_fixture_chunk(call, tools.len());
        }
        if text == "/goal continue" && tools.len() == 3 {
            assert!(
                tools[2]["content"]
                    .as_str()
                    .unwrap()
                    .contains("不能重复启动"),
                "停止前的 produced 也必须先复核，不能沿用旧验收授权"
            );
        }
        if text == "/goal continue" && tools.len() == 5 {
            assert!(
                tools[4]["content"]
                    .as_str()
                    .unwrap()
                    .contains("交付工具必须绑定"),
                "未复核中断结果不能取得产出后的shell验收许可"
            );
        }
        let call = if text == "确认执行" {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update("start_task", "data", serde_json::json!({})),
                2 => Some((
                    "write_file",
                    serde_json::json!({"path":"data.txt","content":"七"}),
                )),
                3 => update(
                    "record_result",
                    "data",
                    serde_json::json!({"result":"数据已产出"}),
                ),
                4 => Some(("read_file", serde_json::json!({"path":"data.txt"}))),
                5 => update("accept_task", "data", evidence(4)),
                6 => update("start_task", "report", serde_json::json!({})),
                7 => Some((
                    "write_file",
                    serde_json::json!({"path":"report.txt","content":"原始报告：七"}),
                )),
                8 => update(
                    "record_result",
                    "report",
                    serde_json::json!({"result":"报告已产出，尚未验收"}),
                ),
                9 => Some((
                    "write_file",
                    serde_json::json!({"path":"late.txt","content":"must not execute after stop"}),
                )),
                _ => None,
            }
        } else {
            match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => Some((
                    "write_file",
                    serde_json::json!({"path":"report.txt","content":"错误重复"}),
                )),
                2 => update("start_task", "report", serde_json::json!({})),
                3 => update(
                    "recheck_task",
                    "report",
                    serde_json::json!({"status":"produced","result":"未经复核不得采用","observation_id":"invented"}),
                ),
                4 => Some((
                    "bash",
                    serde_json::json!({"command":"printf interrupted-bypass"}),
                )),
                5 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                6 => update(
                    "recheck_task",
                    "report",
                    serde_json::json!({"status":"produced","result":"已读取中断前的真实报告，无需重复写入","observation_id":observation(5)}),
                ),
                7 => update("accept_task", "report", evidence(5)),
                8 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                9 => update("accept_goal", "", evidence(8)),
                _ => None,
            }
        };
        goal_fixture_chunk(call, tools.len())
    }
    fn observed_goal<'a>(
        message: &'a serde_json::Value,
        conversation: &str,
    ) -> &'a serde_json::Value {
        message["snapshot"]["conversations"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == conversation))
            .map(|row| &row["goal"])
            .unwrap_or(&message["snapshot"]["goal"])
    }
    #[tokio::test]
    async fn main_goal_cancel_keeps_accepted_work_and_never_starts_dependent_delivery() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-execute", "confirm").await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "stop",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running =
            tokio::spawn(
                async move { goal_send(&app, &team, "stop", "execute", "确认执行").await },
            );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|m| observed_goal(m, "stop")["progress"] == 50)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let response = goal_request(
            &fixture.app,
            &fixture.team,
            "stop",
            serde_json::json!({"type":"cancel","turnId":"execute"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let answer = running.await.unwrap();
        assert_eq!(answer["state"], "stopped");
        assert!(fixture.workspace.path().join("data.txt").exists());
        assert!(!fixture.workspace.path().join("report.txt").exists());
        let calls = fixture.model_seen.lock().await.len();
        let restarted = build_router(AppState::new(fixture.state.config.clone()));
        let response = goal_request(
            &restarted,
            &fixture.team,
            "stop",
            serde_json::json!({"type":"history"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            fixture.model_seen.lock().await.len(),
            calls,
            "读取保存事实不授予执行许可"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture.seen.lock().await.iter().any(|m| {
                    observed_goal(m, "stop")["phase"] == "paused"
                        && observed_goal(m, "stop")["acceptedTaskCount"] == 1
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("停止保留已验收事实并显示暂停");
    }
    #[tokio::test]
    async fn main_goal_confirm_executes_dependencies_checks_evidence_and_accepts_whole_goal() {
        let _guard = PROVIDER_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let fixture = main_fixture("goal-execute", "confirm").await;
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
            "execute",
            "plan",
            "/goal 分析销售并交付报告",
        )
        .await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "execute",
            "edit",
            "把报告改成一页摘要。",
        )
        .await;
        goal_send(
            &fixture.app,
            &fixture.team,
            "other",
            "other-plan",
            "/goal 分析库存并交付报告",
        )
        .await;
        let app = fixture.app.clone();
        let team = fixture.team.clone();
        let running = tokio::spawn(async move {
            goal_send(&app, &team, "execute", "execute", "确认执行").await
        });
        for progress in [50, 100] {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if fixture.seen.lock().await.iter().any(|m| {
                        observed_goal(m, "execute")["progress"] == progress
                            && observed_goal(m, "execute")["phase"] == "active"
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("模型在验收边界等待时应同步精确比例");
            fixture.release_model.notify_one();
        }
        let answer = running.await.unwrap();
        assert_eq!(answer["state"], "complete");
        assert!(answer["text"].as_str().unwrap().contains("整体验收已通过"));
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("data.txt")).unwrap(),
            "七"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("report.txt")).unwrap(),
            "根据数据：七"
        );
        assert!(!fixture.workspace.path().join("unbound.txt").exists());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .any(|m| observed_goal(m, "execute")["phase"] == "completed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("整体完成事实应投影");
        let snapshots = fixture.seen.lock().await;
        assert!(snapshots
            .iter()
            .any(|m| observed_goal(m, "execute")["progress"] == 50));
        assert!(snapshots
            .iter()
            .any(|m| observed_goal(m, "execute")["progress"] == 100
                && observed_goal(m, "execute")["phase"] == "active"));
        assert!(snapshots
            .iter()
            .any(|m| observed_goal(m, "execute")["phase"] == "completed"));
        drop(snapshots);
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
        let other = read["snapshot"]["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "other")
            .unwrap();
        assert_eq!(other["goal"]["phase"], "draft");
        assert_eq!(other["goal"]["acceptedTaskCount"], 0);
        let requests = fixture.model_seen.lock().await;
        let results: Vec<_> = requests
            .iter()
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(results.iter().any(|s| s.contains("依赖")));
        assert!(results.iter().any(|s| s.contains("验收依据")));
        assert!(results.iter().any(|s| s.contains("整体验收")));
        let current = results
            .iter()
            .filter_map(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .find(|goal| goal["phase"] == "active")
            .unwrap();
        assert_eq!(current["plan_version"], 2, "只能执行最近展示的摘要计划");
        assert_eq!(current["tasks"][1]["title"], "摘要报告");
    }
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
        if messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("审查旧整体验收依据"))
        }) {
            return review_proof_model_response(messages);
        }
        if messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("审查验证脚本"))
        }) {
            return review_model_response(messages);
        }
        if messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("执行阻塞方案"))
        }) {
            return block_model_response(messages);
        }
        if messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("人工约束下交付资料"))
        }) {
            return manual_model_response(messages);
        }
        if messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("重规划交付"))
        }) {
            return replan_model_response(messages);
        }
        if let Some(confirm) = messages
            .iter()
            .rposition(|message| message["role"] == "user" && message["content"] == "确认执行")
        {
            return execute_model_response(&messages[confirm..]);
        }
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

    fn execute_model_response(messages: &[serde_json::Value]) -> Response {
        let tools: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
        let current: serde_json::Value = tools
            .first()
            .and_then(|m| m["content"].as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let update = |action: &str, task: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":current["id"],"expected_version":current["plan_version"],"reason":"按真实检查推进","task_id":task});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let evidence = |index: usize| {
            let content = tools
                .get(index)
                .and_then(|m| m["content"].as_str())
                .unwrap_or("");
            let id = content
                .rsplit("Goal observation: ")
                .next()
                .unwrap_or("")
                .trim();
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":id,"conclusion":"逐项检查通过"}]})
        };
        let call = match tools.len() {
            0 => Some(("goal_read", serde_json::json!({}))),
            1 => Some((
                "write_file",
                serde_json::json!({"path":"unbound.txt","content":"must not write"}),
            )),
            2 => update("start_task", "report", serde_json::json!({})),
            3 => update("start_task", "data", serde_json::json!({})),
            4 => Some((
                "write_file",
                serde_json::json!({"path":"data.txt","content":"七"}),
            )),
            5 => update(
                "record_result",
                "data",
                serde_json::json!({"result":"data.txt 已产出，待核对"}),
            ),
            6 => update(
                "accept_task",
                "data",
                serde_json::json!({"evidence":[{"criterion":0,"observation_id":"invented","conclusion":"done"}]}),
            ),
            7 => Some(("read_file", serde_json::json!({"path":"data.txt"}))),
            8 => update("accept_task", "data", evidence(7)),
            9 if !messages.iter().any(|m| {
                m["content"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Goal 仍有"))
            }) =>
            {
                None
            }
            9 => Some(("read_file", serde_json::json!({"path":"data.txt"}))),
            10 => update("start_task", "report", serde_json::json!({})),
            11 => Some((
                "write_file",
                serde_json::json!({"path":"report.txt","content":"根据数据：七"}),
            )),
            12 => update(
                "record_result",
                "report",
                serde_json::json!({"result":"report.txt 已产出，待核对"}),
            ),
            13 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
            14 => update("accept_task", "report", evidence(13)),
            15 => update("accept_goal", "", evidence(9)),
            16 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
            17 => update("accept_goal", "", evidence(16)),
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
            if row["state"] != "complete" {
                continue;
            }
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
        let progress = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(goal) = fixture
                    .seen
                    .lock()
                    .await
                    .iter()
                    .rev()
                    .find(|message| message["snapshot"]["goal"]["phase"] == "ended")
                    .map(|message| message["snapshot"]["goal"].clone())
                {
                    break goal["progress"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(progress, -1, "未确认的旧目标结束也不能产生已执行比例");
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
