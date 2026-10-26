#[cfg(test)]
mod goal_tests {
    use super::*;

    #[tokio::test]
    async fn main_goal_stopped_user_can_change_requirements_without_resuming_delivery() {
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
        let stream = lifecycle_start(&fixture, "execute").await;
        wait_report(&fixture).await;
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
        let restarted = build_router(AppState::new(config));
        let calls = fixture.model_seen.lock().await.len();
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
                4 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                5 => update(
                    "recheck_task",
                    "report",
                    serde_json::json!({"status":"produced","result":"已读取中断前的真实报告，无需重复写入","observation_id":observation(4)}),
                ),
                6 => update("accept_task", "report", evidence(4)),
                7 => Some(("read_file", serde_json::json!({"path":"report.txt"}))),
                8 => update("accept_goal", "", evidence(7)),
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
