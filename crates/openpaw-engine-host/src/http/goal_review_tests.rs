#[cfg(test)]
mod goal_review_tests {
    use super::*;
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
