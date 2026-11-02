#[cfg(test)]
mod goal_review_tests {
    use super::*;

    /// 功能：生成真实选择、按答案交付及拒绝提前完成的本地模型步骤。
    /// 参数：body为实际请求，scenario区分原有观察接线与新增选择约束。
    /// 返回：标准流式工具调用或简短用户回复。
    /// 错误：前置工具事实不符合脚本时断言失败。
    /// 状态：只生成模型响应，不替Host保存答案或验收。
    /// 并发与取消：无等待，真实提问等待由Host处理。
    /// 权限：只使用隔离fixture，绝不请求真实模型或天气。
    /// 关联：CHG-2026-0059；Stability: test-only。
    pub(super) fn choice_model_response(body: &serde_json::Value, scenario: &str) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let start = messages.iter().rposition(|m| m["role"] == "user" && m["content"].as_str().is_some_and(|s| s.starts_with("/goal ") || s == "确认执行")).unwrap();
        let tools: Vec<_> = messages[start..].iter().filter(|m| m["role"] == "tool").collect();
        let parsed = |m: &serde_json::Value| serde_json::from_str::<serde_json::Value>(m["content"].as_str().unwrap().split("\nGoal observation:").next().unwrap()).ok();
        let goal = tools.iter().rev().filter_map(|m| parsed(m)).find(|v| v["id"].is_string()).unwrap_or_default();
        let update = |action: &str, extra: serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":goal["id"],"expected_version":goal["plan_version"],"task_id":"display","reason":"核对用户真实选择及展示"});
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            Some(("goal_update", args))
        };
        let required = scenario.ends_with("required");
        if messages[start]["content"].as_str().unwrap().starts_with("/goal ") {
            let mut task = serde_json::json!({"id":"display","title":"选择城市后展示天气","expected_result":"choice.txt","acceptance_criteria":["按用户答案展示所选城市"],"dependencies":[]});
            if required { task["requires_user_input"] = serde_json::json!(true); }
            return goal_tests::goal_fixture_chunk(match tools.len() {
                0 => Some(("goal_read", serde_json::json!({}))),
                1 => update("set_plan", serde_json::json!({"acceptance_criteria":["所选内容可读"],"tasks":[task]})),
                _ => None,
            }, tools.len());
        }
        if required && tools.len() == 2 {
            return goal_tests::goal_fixture_chunk(update("record_result", serde_json::json!({"result":"武汉、长沙，请文字回复；尚未取得回答"})), tools.len());
        }
        let step = tools.len() - usize::from(required && tools.len() > 2);
        let city = tools.iter().filter_map(|m| parsed(m)).find(|v| v["question_id"].is_string()).map(|v| {
            if v["answer"]["choice_id"] == "custom" {v["answer"]["custom_text"].as_str().unwrap().to_owned()} else {v["answer"]["label"].as_str().unwrap().to_owned()}
        }).unwrap_or_default();
        let evidence = || {
            let observation = tools.last().unwrap()["content"].as_str().unwrap().rsplit_once("Goal observation: ").unwrap().1;
            serde_json::json!({"evidence":[{"criterion":0,"observation_id":observation,"conclusion":"实际读取与用户所选城市一致"}]})
        };
        let call = match step {
            0 => Some(("goal_read", serde_json::json!({}))),
            1 => update("start_task", serde_json::json!({})),
            2 => Some(("request_user_input", serde_json::json!({"question":"查看哪个城市的天气？","options":[{"id":"option-a","label":"武汉"},{"id":"option-b","label":"长沙"}]}))),
            3 => Some(("write_file", serde_json::json!({"path":"choice.txt","content":format!("所选城市：{city}；本地测试示例天气")}))),
            4 => update("record_result", serde_json::json!({"result":format!("已按选择展示{city}")})),
            5 | 7 => Some(("read_file", serde_json::json!({"path":"choice.txt"}))),
            6 => update("accept_task", evidence()),
            8 => update("accept_goal", evidence()),
            _ => None,
        };
        if call.is_none() {
            let chunk = serde_json::json!({"choices":[{"delta":{"content":format!("已展示{city}的示例天气。")},"finish_reason":"stop"}]});
            return ([(header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n")).into_response();
        }
        goal_tests::goal_fixture_chunk(call, tools.len())
    }

    #[tokio::test]
    async fn goal_business_choice_is_observed_before_acceptance_and_final_is_concise() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        let fixture = main_fixture("question-goal-choice-observe", "option-a").await;
        goal_tests::goal_send(&fixture.app,&fixture.team,"choice","plan","/goal 查询武汉长沙天气，然后弹菜单让我选择查看哪个城市").await;
        let answer = goal_tests::goal_send(&fixture.app,&fixture.team,"choice","execute","确认执行").await;
        assert_eq!(answer["state"],"complete","{answer}");
        let owner = fixture.state.main_office.conversation_record(&fixture.team,"choice").unwrap().unwrap().session.unwrap();
        let goal = crate::runtime::latest_goal(fixture.home.path(),&owner).unwrap().unwrap();
        let questions: Vec<_> = goal["observations"].as_array().unwrap().iter().filter(|v|v["tool"]=="request_user_input").collect();
        assert_eq!(questions.len(),1,"真实菜单答案必须经过Goal观察接线，不能仅保存为普通问答");
        assert_eq!(questions[0]["task_id"],"display");
        assert_eq!(goal["phase"],"completed");
        assert_eq!(std::fs::read_to_string(fixture.workspace.path().join("choice.txt")).unwrap(),"所选城市：武汉；本地测试示例天气");
        let text = answer["text"].as_str().unwrap();
        assert!(text.contains("武汉") && !text.contains("验收依据") && !text.contains("obs-"),"收尾正文应简洁：{text}");
        assert_eq!(fixture.state.get_typed(&owner).await.unwrap().turn_outcomes().len(),2,"仅有规划和用户明确确认两回合，业务菜单不得新增回合");
    }

    #[tokio::test]
    async fn goal_choice_requires_business_answer_and_delivers_selected_or_custom_city() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        for choice in ["option-a","custom","timeout"] {
            let fixture = main_fixture("question-goal-choice-required",choice).await;
            goal_tests::goal_send(&fixture.app,&fixture.team,"choice","plan","/goal 查武汉长沙天气，弹菜单选择后展示").await;
            let answer = goal_tests::goal_send(&fixture.app,&fixture.team,"choice","execute","确认执行").await;
            let owner = fixture.state.main_office.conversation_record(&fixture.team,"choice").unwrap().unwrap().session.unwrap();
            let goal = crate::runtime::latest_goal(fixture.home.path(),&owner).unwrap().unwrap();
            assert!(fixture.model_seen.lock().await.iter().flat_map(|body|body["messages"].as_array().unwrap()).any(|m|m["role"]=="tool" && m["content"].as_str().is_some_and(|s|s.starts_with("该任务必须先选中任务并调用request_user_input"))),"文字选项和固定计划回答不能提前完成任务");
            let seen = fixture.seen.lock().await;
            let menus: Vec<_> = seen.iter().filter(|m|m["type"]=="decision.request").collect();
            assert_eq!(menus.len(),2,"计划菜单与业务菜单必须独立存在");
            assert_eq!(menus[1]["decision"]["title"],"查看哪个城市的天气？");
            drop(seen);
            if choice == "timeout" {
                assert_eq!(goal["phase"],"paused","{answer}");
                assert_ne!(goal["tasks"][0]["status"],"accepted");
                assert!(!fixture.workspace.path().join("choice.txt").exists());
            } else {
                assert_eq!(goal["phase"],"completed","{answer}");
                let city = if choice == "custom" {"长沙"} else {"武汉"};
                assert_eq!(std::fs::read_to_string(fixture.workspace.path().join("choice.txt")).unwrap(),format!("所选城市：{city}；本地测试示例天气"));
                assert!(answer["text"].as_str().unwrap().contains(city));
                let read = openpaw_store::SessionReader::read(fixture.home.path().join("sessions").join(format!("{owner}.jsonl"))).unwrap();
                let history = openpaw_store::transcript::replay(&owner,&read.events).unwrap();
                assert_eq!(crate::context::validated_questions(&owner,&read.events,history.visible_messages(&owner)).len(),2,"可靠原件保留，正文精简不能丢失回答");
                assert!(!goal["tasks"][0]["evidence"].as_array().unwrap().is_empty());
            }
            assert_eq!(fixture.state.get_typed(&owner).await.unwrap().turn_outcomes().len(),2);
        }
    }

    #[tokio::test]
    async fn stopping_goal_business_choice_does_not_accept_late_answer() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        let fixture = main_fixture("question-goal-choice-required","held-answer").await;
        goal_tests::goal_send(&fixture.app,&fixture.team,"choice","plan","/goal 查两个城市后弹菜单选择").await;
        let owner = fixture.state.main_office.conversation_record(&fixture.team,"choice").unwrap().unwrap().session.unwrap();
        let (app,team) = (fixture.app.clone(),fixture.team.clone());
        let running = tokio::spawn(async move {goal_tests::goal_send(&app,&team,"choice","execute","确认执行").await});
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                if fixture.seen.lock().await.iter().any(|m|m["type"]=="decision.request" && m["decision"]["title"]=="查看哪个城市的天气？") {break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        child_test_json(&fixture.app,"POST",&format!("/session/{owner}/abort"),serde_json::json!({})).await;
        assert_eq!(tokio::time::timeout(Duration::from_secs(5),running).await.unwrap().unwrap()["state"],"stopped");
        fixture.release_model.notify_waiters();
        let goal = crate::runtime::latest_goal(fixture.home.path(),&owner).unwrap().unwrap();
        assert_ne!(goal["tasks"][0]["status"],"accepted");
        assert!(!fixture.workspace.path().join("choice.txt").exists());
        assert!(!goal["observations"].as_array().unwrap().iter().any(|o|o["question_definition"].is_array()));
    }
    /// 功能：模拟用户针对真实固定菜单提交的答案。
    /// 参数：body为实际菜单，choice为合成选择，count为本地展示次数。
    /// 返回：保留实际问题和会话身份的Office响应。
    /// 错误：超时沿原fixture返回408。
    /// 状态：不直接更改Goal。
    /// 并发与取消：同步；等待由Gateway路由控制。
    /// 权限：只供隔离测试，不调用真实模型。
    /// 关联：CHG-2026-0056；Stability: test-only。
    pub(super) fn decision_response(body: &serde_json::Value, choice: &str, count: usize) -> Response {
        if choice == "timeout" { return user_input_tests::decision_response(body,choice); }
        let selected = match choice {"custom-first" => if count == 1 {"custom"} else {"modify"},"execute-then-modify" => if count == 1 {"execute"} else {"modify"},"held-review"=>"execute",_=>choice};
        let label = match selected {"execute"=>"执行","cancel"=>"取消","custom"=>"其他",_=>"修改"};
        let mut data = serde_json::json!({"teamId":body["teamId"],"conversationId":body["conversationId"],"decisionId":body["decision"]["id"],"choiceId":selected,"label":label});
        if selected == "custom" {data["customText"] = serde_json::json!("请将标题改为用户补充的标题");}
        Json(serde_json::json!({"code":200,"msg":"User selected","data":data})).into_response()
    }
    /// 功能：生成固定审核菜单后的确定性本地模型响应。
    /// 参数：body为真实请求，scenario选择初始计划或重规划脚本。
    /// 返回：标准SSE模型响应。
    /// 错误：不符合步骤前提时测试失败。
    /// 状态：仅根据实际工具结果推进。
    /// 并发与取消：不等待或访问外部资源。
    /// 权限：工具调用均经实际Host，不自行验收。
    /// 关联：CHG-2026-0056；Stability: test-only。
    pub(super) fn model_response(body: &serde_json::Value, scenario: &str) -> Response {
        let messages = body["messages"].as_array().unwrap();
        let tools: Vec<_> = messages.iter().filter(|m|m["role"] == "tool").collect();
        let goal = tools.iter().rev().filter_map(|m|m["content"].as_str())
            .filter_map(|text|serde_json::from_str::<serde_json::Value>(text.split("\nGoal observation:").next().unwrap()).ok())
            .find(|value|value["id"].is_string()).unwrap_or_default();
        let update = |action:&str,extra:serde_json::Value| {
            let mut args = serde_json::json!({"action":action,"goal_id":goal["id"],"expected_version":goal["plan_version"],"task_id":"report","reason":"固定菜单真实回归"});
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            Some(("goal_update",args))
        };
        let plan = serde_json::json!({"acceptance_criteria":["文件可读"],"tasks":[{"id":"report","title":"保存测试文件","expected_result":"review-demo.txt","acceptance_criteria":["内容正确"],"dependencies":[]}]});
        let evidence = |index:usize|serde_json::json!({"evidence":[{"criterion":0,"observation_id":tools[index]["content"].as_str().unwrap().rsplit_once("Goal observation: ").unwrap().1,"conclusion":"已核对实际文件"}]});
        if scenario == "goal-menu-replan" && tools.len() == 3 {
            let mut revised = plan.clone();
            revised.as_object_mut().unwrap().remove("acceptance_criteria");
            revised["tasks"][0]["title"] = serde_json::json!("修订测试文件");
            return goal_tests::goal_fixture_chunk(update("replan",revised),tools.len());
        }
        let index = if scenario == "goal-menu-replan" && tools.len() >= 4 {tools.len() - 2} else {tools.len()};
        let call = match index {
            0 | 2 => Some(("goal_read",serde_json::json!({}))),
            1 => update("set_plan",plan.clone()),
            3 if goal["phase"] != "active" => update("set_plan",plan),
            3 => update("start_task",serde_json::json!({})),
            4 => Some(("write_file",serde_json::json!({"path":"review-demo.txt","content":"MENU_REVIEW_OK"}))),
            5 => update("record_result",serde_json::json!({"result":"文件已保存"})),
            6 | 8 => Some(("read_file",serde_json::json!({"path":"review-demo.txt"}))),
            7 => update("accept_task",evidence(if scenario == "goal-menu-replan" {8} else {6})),
            9 => update("accept_goal",evidence(if scenario == "goal-menu-replan" {10} else {8})),
            _ => None,
        };
        goal_tests::goal_fixture_chunk(call,tools.len())
    }
    #[tokio::test]
    async fn goal_review_execute_continues_same_turn_and_other_choices_do_not_deliver() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        for choice in ["execute","modify","cancel","timeout","custom-first"] {
            let fixture = main_fixture("goal-menu-choice",choice).await;
            let answer = tokio::time::timeout(Duration::from_secs(8),goal_tests::goal_send(&fixture.app,&fixture.team,"review","plan","/goal 保存可验收测试文件")).await.unwrap();
            assert_eq!(answer["state"],"complete","{choice}: {answer}");
            assert_eq!(fixture.workspace.path().join("review-demo.txt").exists(),choice == "execute","{choice}: {answer}");
            let seen = fixture.seen.lock().await;
            let decisions: Vec<_> = seen.iter().filter(|m|m["type"] == "decision.request" && m["decision"]["title"].as_str().is_some_and(|title|title.starts_with("是否执行 Goal"))).collect();
            assert_eq!(decisions.len(),if choice == "custom-first" {2} else {1},"{choice}");
            assert_eq!(decisions[0]["decision"]["options"].as_array().unwrap().iter().map(|v|v["id"].as_str().unwrap()).collect::<Vec<_>>(),["execute","modify","cancel"]);
            drop(seen);
            let owner = fixture.state.main_office.conversation_record(&fixture.team,"review").unwrap().unwrap().session.unwrap();
            let handle = fixture.state.get_typed(&owner).await.unwrap();
            assert_eq!(handle.turn_outcomes().len(),1,"菜单接续不得创建隐藏的新回合");
            let read = openpaw_store::SessionReader::read(fixture.home.path().join("sessions").join(format!("{owner}.jsonl"))).unwrap();
            let history = openpaw_store::transcript::replay(&owner,&read.events).unwrap();
            let questions = crate::context::validated_questions(&owner,&read.events,history.visible_messages(&owner));
            assert_eq!(questions.len(),decisions_count(choice),"审核必须保留可信问答原件");
            assert!(crate::context::validated_questions(&owner,&read.events,&[]).is_empty(),"隐藏来源不能恢复审核原件");
            let without_binding:Vec<_> = read.events.iter().filter(|event|!matches!(event,openpaw_core::SessionEvent::Unknown(value) if value["type"]=="session_goal_state_v1")).cloned().collect();
            assert!(crate::context::validated_questions(&owner,&without_binding,history.visible_messages(&owner)).is_empty(),"缺少真实版本审核绑定的goal_update不得伪装成提问");
            assert!(!fixture.model_seen.lock().await.iter().any(|body|body["messages"].to_string().contains("\"name\":\"request_user_input\"")),"固定菜单不依赖模型调用提问工具");
            if choice == "execute" {assert_eq!(crate::runtime::latest_goal(fixture.home.path(),&owner).unwrap().unwrap()["phase"],"completed");}
        }
    }
    fn decisions_count(choice:&str)->usize {if choice == "custom-first" {2}else{1}}

    #[tokio::test]
    async fn goal_plan_is_visible_before_approval_and_only_runs_after_answer() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        let fixture = main_fixture("goal-menu-preview", "held-review").await;
        let (app,team) = (fixture.app.clone(),fixture.team.clone());
        let running = tokio::spawn(async move {goal_tests::goal_send(&app,&team,"preview","plan","/goal 保存可验收测试文件").await});
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                if fixture.seen.lock().await.iter().any(|m|m["type"]=="decision.request") {break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let preview = tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let (_,read) = fixture.state.main_office.message(serde_json::json!({"type":"office.read","teamId":fixture.team}),Duration::from_secs(1)).await.unwrap();
                if let Some(view) = read["snapshot"]["conversations"].as_array().and_then(|rows|rows.iter().find(|row|row["id"]=="preview" && row["goal"]["planVersion"]==1)) {
                    break view.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await;
        if preview.is_err() {
            let owner = fixture.state.main_office.conversation_record(&fixture.team,"preview").unwrap().unwrap().session.unwrap();
            child_test_json(&fixture.app,"POST",&format!("/session/{owner}/abort"),serde_json::json!({})).await;
            running.await.unwrap();
        } else {
            let preview = preview.as_ref().unwrap();
            assert_eq!(preview["whiteboard"]["status"],"queued");
            assert_eq!(preview["whiteboard"]["steps"][0]["title"],"保存测试文件");
            assert_eq!(preview["whiteboard"]["steps"][0]["progress"],0);
            assert_eq!(preview["tasks"][0]["status"],"queued");
            assert_eq!(fixture.model_seen.lock().await.len(),2,"等待批准时不请求执行模型");
            assert!(!fixture.workspace.path().join("review-demo.txt").exists());
            fixture.release_model.notify_one();
            assert_eq!(tokio::time::timeout(Duration::from_secs(8),running).await.unwrap().unwrap()["state"],"complete");
            assert_eq!(std::fs::read_to_string(fixture.workspace.path().join("review-demo.txt")).unwrap(),"MENU_REVIEW_OK");
        }
        assert!(preview.is_ok(),"用户尚未回答审核菜单时，真实Office面板必须已经展示计划v1");
    }

    #[tokio::test]
    async fn goal_replan_is_visible_and_frozen_until_second_answer() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        let fixture = main_fixture("goal-menu-replan", "held-review").await;
        let (app,team) = (fixture.app.clone(),fixture.team.clone());
        let running = tokio::spawn(async move {goal_tests::goal_send(&app,&team,"preview","plan","/goal 保存可验收测试文件").await});
        for (version,title) in [(1,"保存测试文件"),(2,"修订测试文件")] {
            tokio::time::timeout(Duration::from_secs(5),async {
                loop {
                    if fixture.seen.lock().await.iter().filter(|m|m["type"]=="decision.request").count() >= version {break;}
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            let view = tokio::time::timeout(Duration::from_secs(3),async {
                loop {
                    let (_,read) = fixture.state.main_office.message(serde_json::json!({"type":"office.read","teamId":fixture.team}),Duration::from_secs(1)).await.unwrap();
                    if let Some(view) = read["snapshot"]["conversations"].as_array().and_then(|rows|rows.iter().find(|row|row["id"]=="preview" && row["goal"]["planVersion"]==version)) {break view.clone();}
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("审核期间必须显示当前计划版本");
            assert_eq!(view["whiteboard"]["status"],"queued","未批准的v{version}不能播放执行动画");
            assert!(view["whiteboard"]["tagline"].as_str().unwrap().contains("待批准"));
            assert_eq!(view["whiteboard"]["steps"][0]["title"],title);
            assert_eq!(view["whiteboard"]["steps"][0]["progress"],0);
            assert!(!fixture.workspace.path().join("review-demo.txt").exists());
            fixture.release_model.notify_one();
        }
        assert_eq!(tokio::time::timeout(Duration::from_secs(8),running).await.unwrap().unwrap()["state"],"complete");
        let owner = fixture.state.main_office.conversation_record(&fixture.team,"preview").unwrap().unwrap().session.unwrap();
        assert_eq!(fixture.state.get_typed(&owner).await.unwrap().turn_outcomes().len(),1,"两次审核沿同一回合继续");
        assert_eq!(std::fs::read_to_string(fixture.workspace.path().join("review-demo.txt")).unwrap(),"MENU_REVIEW_OK");
    }

    #[tokio::test]
    async fn goal_review_replan_requires_second_answer_and_stop_restart_never_replays_permission() {
        let _guard = PROVIDER_ENV_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
        for choice in ["execute","execute-then-modify"] {
            let fixture = main_fixture("goal-menu-replan",choice).await;
            let answer = tokio::time::timeout(Duration::from_secs(8),goal_tests::goal_send(&fixture.app,&fixture.team,"review","plan","/goal 保存可验收测试文件")).await.unwrap();
            assert_eq!(answer["state"],"complete","{choice}: {answer}");
            assert_eq!(fixture.workspace.path().join("review-demo.txt").exists(),choice=="execute","{answer}");
            let seen = fixture.seen.lock().await;
            let titles:Vec<_> = seen.iter().filter(|m|m["type"]=="decision.request").map(|m|m["decision"]["title"].as_str().unwrap()).collect();
            assert_eq!(titles,["是否执行 Goal 计划 v1？","是否执行 Goal 计划 v2？"]);
        }
        let fixture = main_fixture("goal-menu-stop","held-review").await;
        let (app,team) = (fixture.app.clone(),fixture.team.clone());
        let running = tokio::spawn(async move {goal_tests::goal_send(&app,&team,"review","plan","/goal 保存可验收测试文件").await});
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                if fixture.seen.lock().await.iter().any(|m|m["type"]=="decision.request") {break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(fixture.model_seen.lock().await.len(),2,"提交答案前不得请求执行模型");
        assert!(!fixture.workspace.path().join("review-demo.txt").exists());
        let owner = fixture.state.main_office.conversation_record(&fixture.team,"review").unwrap().unwrap().session.unwrap();
        let snapshot = restart_question_tests::snapshot(&fixture,&owner,|event|event["type"]=="user_question_v1" && event["record"]["state"]=="waiting");
        child_test_json(&fixture.app,"POST",&format!("/session/{owner}/abort"),serde_json::json!({})).await;
        assert_eq!(tokio::time::timeout(Duration::from_secs(5),running).await.unwrap().unwrap()["state"],"stopped");
        fixture.release_model.notify_one();
        let state = restart_question_tests::rebuild(&fixture,snapshot.path());
        assert_eq!(state.recover_orphaned_turns().await,1);
        let restored = state.get_typed(&owner).await.unwrap();
        assert!(restored.has_unfinished_goal());
        assert_eq!(crate::runtime::latest_goal(snapshot.path(),&owner).unwrap().unwrap()["confirmed_version"],0);
        assert_eq!(child_test_json(&build_router(state),"GET","/session/status",serde_json::json!({})).await,serde_json::json!({}));
        assert_eq!(fixture.model_seen.lock().await.len(),2,"恢复或迟到答案不得自动发起执行");
        assert!(!fixture.workspace.path().join("review-demo.txt").exists());
    }
}
