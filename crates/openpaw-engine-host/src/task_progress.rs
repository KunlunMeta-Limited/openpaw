//! 职责：验证Office运行内业务步骤与真实工具证据，生成有界进度。
//! 输入输出：模型元数据报告、原Run及已开始调用到host_events派生事实。
//! 依赖：原工具声明、调用绑定和单写Store；不另建任务数据库。
//! 禁止职责：不执行项目工具、不批准写入、不把模型自报当成真实执行。
use crate::{HostError, HostResult, RunState, RunView};
use openpaw_core::{ToolCallBinding, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const NAME: &str = "report_progress";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    id: String,
    title: String,
    state: String,
    #[serde(rename = "evidenceCallIds")]
    evidence: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    run_id: String,
    steps: Vec<Step>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    title: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Report {
    #[serde(rename = "create")]
    Create { steps: Vec<Definition> },
    #[serde(rename = "update")]
    Update {
        #[serde(rename = "stepId")]
        step_id: String,
        state: String,
        #[serde(rename = "evidenceCallIds", default)]
        evidence: Vec<String>,
    },
}
fn invalid(message: &str) -> HostError {
    HostError::new("invalid_progress", message)
}
fn parse(value: &Value) -> HostResult<Report> {
    if serde_json::to_vec(value).map_err(HostError::storage)?.len() > 8192 {
        return Err(invalid("步骤报告超过8KiB"));
    }
    serde_json::from_value(value.clone()).map_err(|_| invalid("步骤报告字段不符合约定"))
}
fn needs_write(title: &str) -> bool {
    ["保存", "写入", "落盘", "记录", "生成文件", "导出"]
        .iter()
        .any(|s| title.contains(s))
}

/// 功能：声明仅Office运行内使用的元数据工具。
/// 参数：无。
/// 返回：复用原ToolSpec的严格输入声明。
/// 错误：无。
/// 状态：不创建计划或文件。
/// 并发与取消：无共享可变状态，执行时仍检查Run取消。
/// 权限：不授予项目工具权限，不需文件写审批。
/// 关联：REQ-OP-012；CHG-2026-0006第9.9节。
pub(crate) fn spec() -> ToolSpec {
    let definition = json!({"type":"object","additionalProperties":false,"required":["id","title"],"properties":{"id":{"type":"string","minLength":1,"maxLength":80},"title":{"type":"string","minLength":1,"maxLength":160}}});
    serde_json::from_value(json!({"name":NAME,"description":"报告本轮用户目标的业务步骤及进度，不执行任何项目操作。多步骤任务先create固定计划，再update。保存/记录步骤标题须说明产物路径，completed必须引用本轮已成功的真实调用ID；文件步骤须引用write_file/edit_file。无工具的整理步骤保持in_progress，最终交付由Host确认。不能用此工具代替真实联网、文件写入或审批。","input_schema":{"type":"object","oneOf":[{"type":"object","additionalProperties":false,"required":["type","steps"],"properties":{"type":{"const":"create"},"steps":{"type":"array","minItems":1,"maxItems":12,"items":definition}}},{"type":"object","additionalProperties":false,"required":["type","stepId","state"],"properties":{"type":{"const":"update"},"stepId":{"type":"string"},"state":{"enum":["pending","in_progress","completed"]},"evidenceCallIds":{"type":"array","maxItems":32,"items":{"type":"string"}}}}]}})).expect("固定工具声明合法")
}

/// 功能：校验报告的结构，实际证据在Store序列内再次校验。
/// 参数：value为引擎最终输入。
/// 返回：合法时为空。
/// 错误：未知字段、null或超大输入明确拒绝。
/// 状态：不写入数据。
/// 并发与取消：无等待，不取工作区锁。
/// 权限：不作为批准凭据。
/// 关联：REQ-OP-012；CHG-2026-0006第9.9节。
pub(crate) fn validate(value: &Value) -> HostResult<()> {
    parse(value).map(|_| ())
}

/// 功能：将模型报告与本Run真实执行事实绑定后生成下一份计划。
/// 参数：previous为已存计划，value为报告，run为当前事实，started为原执行绑定。
/// 返回：有界规范计划；重复报告产生相同内容。
/// 错误：终态、计划替换、无证据完成或跨运行引用均拒绝。
/// 状态：计划分母固定、已完成步骤不可回退，写入步骤必须有成功写入证据。
/// 并发与取消：调用方在原Store单写命令中执行；不持工作区资源。
/// 权限：仅Office绑定可调用，可信run_id不取模型输入。
/// 关联：REQ-OP-012；CHG-2026-0006第9.9节。
pub(crate) fn update(
    previous: Option<Value>,
    value: &Value,
    run: &RunView,
    started: &[ToolCallBinding],
) -> HostResult<Value> {
    if run.state.is_terminal() || run.state == RunState::Cancelling {
        return Err(invalid("本轮已结束或正在停止，不再接受步骤报告"));
    }
    let mut plan = previous
        .map(serde_json::from_value::<Plan>)
        .transpose()
        .map_err(HostError::storage)?;
    match parse(value)? {
        Report::Create { steps } => {
            if steps.is_empty() || steps.len() > 12 {
                return Err(invalid("计划须包含1—12项步骤"));
            }
            let mut ids = BTreeSet::new();
            for step in &steps {
                if step.id.is_empty()
                    || step.id.len() > 80
                    || !step
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                    || !ids.insert(&step.id)
                    || step.title.trim().is_empty()
                    || step.title.encode_utf16().count() > 160
                {
                    return Err(invalid(
                        "步骤ID须唯一且为ASCII标识，标题须为1—160个UTF16单元",
                    ));
                }
            }
            if let Some(old) = &plan {
                if old.steps.len() != steps.len()
                    || old
                        .steps
                        .iter()
                        .zip(&steps)
                        .any(|(a, b)| a.id != b.id || a.title != b.title)
                {
                    return Err(invalid("本轮计划已固定，不能替换步骤或改变分母"));
                }
            } else {
                plan = Some(Plan {
                    run_id: run.run_id.clone(),
                    steps: steps
                        .into_iter()
                        .map(|s| Step {
                            id: s.id,
                            title: s.title,
                            state: "pending".into(),
                            evidence: vec![],
                        })
                        .collect(),
                });
            }
        }
        Report::Update {
            step_id,
            state,
            evidence,
        } => {
            let plan = plan
                .as_mut()
                .ok_or_else(|| invalid("请先声明本轮固定步骤"))?;
            if !matches!(state.as_str(), "pending" | "in_progress" | "completed")
                || evidence.len() > 32
            {
                return Err(invalid("步骤状态或证据数量不合法"));
            }
            let step = plan
                .steps
                .iter_mut()
                .find(|s| s.id == step_id)
                .ok_or_else(|| invalid("步骤不属于本轮计划"))?;
            if step.state == "completed" && (state != step.state || evidence != step.evidence) {
                return Err(invalid("已确认步骤不能回退或替换证据"));
            }
            let mut unique = BTreeSet::new();
            let mut write = false;
            for id in &evidence {
                if !unique.insert(id) {
                    return Err(invalid("证据调用ID不能重复"));
                }
                let tool = run
                    .tools
                    .iter()
                    .find(|t| &t.call_id == id && t.tool_name != NAME && t.state == "completed")
                    .ok_or_else(|| invalid("完成证据必须来自本轮成功的真实调用"))?;
                let binding = started
                    .iter()
                    .find(|b| {
                        &b.call_id == id && b.run_id == run.run_id && b.tool_name == tool.tool_name
                    })
                    .ok_or_else(|| invalid("证据缺少本轮实际执行事实"))?;
                if matches!(tool.tool_name.as_str(), "write_file" | "edit_file")
                    && binding
                        .input
                        .get("path")
                        .or_else(|| binding.input.get("file_path"))
                        .and_then(Value::as_str)
                        .is_some_and(|path| step.title.contains(path))
                {
                    write = true;
                }
            }
            if state == "completed" && (evidence.is_empty() || needs_write(&step.title) && !write) {
                return Err(invalid(
                    "尚无可确认的执行证据；记录文件必须先真实写入，无工具的整理步骤由最终交付确认",
                ));
            }
            step.state = state;
            step.evidence = evidence;
        }
    }
    let plan = plan.expect("create或update已形成计划");
    if plan.run_id != run.run_id {
        return Err(invalid("计划不属于本轮运行"));
    }
    let result = serde_json::to_value(plan).map_err(HostError::storage)?;
    if serde_json::to_vec(&result)
        .map_err(HostError::storage)?
        .len()
        > 8192
    {
        return Err(invalid("计划与证据总量超过8KiB"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolView;
    fn run() -> RunView {
        RunView {
            run_id: "r".into(),
            session_id: "s".into(),
            state: RunState::Running,
            revision: 1,
            text: String::new(),
            error: None,
            tools: vec![],
            text_truncated: false,
        }
    }
    fn plan(run: &RunView) -> Value {
        update(None,&json!({"type":"create","steps":[{"id":"lookup","title":"查询当天资料"},{"id":"record","title":"保存 weather.md"},{"id":"answer","title":"整理答复"}]}),run,&[]).unwrap()
    }
    #[test]
    fn false_file_completion_and_cross_run_evidence_are_rejected() {
        let mut r = run();
        let original = plan(&r);
        let report = json!({"type":"update","stepId":"record","state":"completed","evidenceCallIds":["write"]});
        assert!(update(Some(original.clone()), &report, &r, &[]).is_err());
        r.tools.push(ToolView {
            call_id: "write".into(),
            tool_name: "write_file".into(),
            state: "completed".into(),
        });
        let mut binding = ToolCallBinding {
            run_id: "another".into(),
            call_id: "write".into(),
            tool_name: "write_file".into(),
            input: json!({"path":"weather.md","content":"fact"}),
        };
        assert!(update(Some(original.clone()), &report, &r, &[binding.clone()]).is_err());
        binding.run_id = "r".into();
        binding.input["path"] = json!("other.md");
        assert!(
            update(Some(original.clone()), &report, &r, &[binding.clone()]).is_err(),
            "另一文件写入不能确认当前产物"
        );
        binding.input["path"] = json!("weather.md");
        let confirmed = update(Some(original), &report, &r, &[binding]).unwrap();
        let (_, percent) = project(Some(confirmed.clone()), &r).unwrap();
        assert_eq!(percent, 25);
        r.state = RunState::Failed;
        assert_eq!(
            project(Some(confirmed.clone()), &r).unwrap().1,
            25,
            "失败保留真实完成度"
        );
        assert!(update(Some(confirmed), &report, &r, &[]).is_err());
    }
    #[test]
    fn fixed_plan_and_strict_boundaries_prevent_progress_inflation() {
        let r = run();
        let p = plan(&r);
        assert!(update(
            Some(p.clone()),
            &json!({"type":"create","steps":[{"id":"only","title":"一项"}]}),
            &r,
            &[]
        )
        .is_err());
        for value in [
            json!({"type":"create","steps":[]}),
            json!({"type":"create","steps":[{"id":"a","title":"a"},{"id":"a","title":"b"}]}),
            json!({"type":"update","stepId":"lookup","state":"completed"}),
            json!({"type":"update","stepId":"lookup","state":"running"}),
            json!({"type":"update","stepId":"other","state":"pending"}),
            json!({"type":"update","stepId":"lookup","state":"pending","extra":true}),
            json!({"type":"create","steps":null}),
        ] {
            assert!(
                update(Some(p.clone()), &value, &r, &[]).is_err(),
                "非法报告应拒绝：{value}"
            );
        }
        assert!(
            validate(&json!({"type":"create","steps":[{"id":"x","title":"x".repeat(9000)}]}))
                .is_err()
        );
        let (rows, percent) = project(Some(p), &r).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(percent, 0);
        assert!(rows.iter().all(|v| v["id"].as_str().unwrap().len() <= 100));
    }
    #[test]
    fn no_tool_answer_is_confirmed_only_by_reliable_normal_finish() {
        let mut r = run();
        let p = update(
            None,
            &json!({"type":"create","steps":[{"id":"answer","title":"整理答复"}]}),
            &r,
            &[],
        )
        .unwrap();
        r.text = "最终文字".into();
        assert_eq!(project(Some(p.clone()), &r).unwrap().1, 0);
        r.state = RunState::Cancelled;
        assert_eq!(project(Some(p.clone()), &r).unwrap().1, 0);
        r.state = RunState::Completed;
        assert_eq!(project(Some(p), &r).unwrap().1, 100);
    }
}

/// 功能：生成业务步骤和可信计数进度，不暴露内部调用标识。
/// 参数：value为已校验派生事实，run为真实终态及工具事实。
/// 返回：步骤数组和整体百分比。
/// 错误：损坏计划明确上报，不伪造空计划。
/// 状态：终态前保留最终交付分母；失败不显示100%。
/// 并发与取消：纯读取；终态后的报告不会改变数据。
/// 权限：不输出执行输入、路径正文或调用ID。
/// 关联：REQ-OP-012；CHG-2026-0006第9.9节。
pub(crate) fn project(value: Option<Value>, run: &RunView) -> HostResult<(Vec<Value>, u64)> {
    let Some(value) = value else {
        return Ok((
            vec![],
            if run.state == RunState::Completed {
                100
            } else {
                0
            },
        ));
    };
    let plan: Plan = serde_json::from_value(value).map_err(HostError::storage)?;
    let mut completed = 0;
    let steps=plan.steps.iter().enumerate().map(|(index,s)| {
        // 纯整理步骤没有工具证据，只能在最终文本已可靠保存且Run正常结束后确认。
        let answer = ["整理","总结","答复","交付"].iter().any(|v|s.title.contains(v));
        let done=s.state=="completed" || run.state==RunState::Completed && !run.text.trim().is_empty() && !needs_write(&s.title) && answer;
        if done {completed+=1;}
        let waiting=s.evidence.iter().any(|id|run.tools.iter().any(|t|&t.call_id==id && matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource")));
        let state=if done {"completed"} else if run.state.is_terminal() {"failed"} else if waiting || run.state==RunState::Cancelling {"blocked"} else if s.state=="in_progress" {"running"} else {"queued"};
        json!({"id":format!("step:{}:{}",run.run_id,index),"title":s.title,"status":state,"progress":if done{100}else{0},"summary":if done{"步骤已确认"}else if run.state.is_terminal(){"本步骤未完成"}else if waiting{"等待确认或资源"}else if s.state=="in_progress"{"正在处理"}else{"待处理"}})
    }).collect::<Vec<_>>();
    let delivered = run.state == RunState::Completed && completed == steps.len();
    let percent = (completed + usize::from(delivered)) * 100 / (steps.len() + 1);
    Ok((steps, percent as u64))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::{ClientScope, Host, HostConfig};
    use openpaw_llm::{MockMode, MockProvider};
    use std::sync::Arc;
    #[tokio::test]
    async fn actual_engine_progress_uses_original_tools_persists_and_is_deleted_with_run() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("资料.txt"), "真实读取标记").unwrap();
        let cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"mock","tools":["read_file"]})).unwrap();
        let step = |input| MockMode::ToolUse {
            tool_name: NAME.into(),
            input,
            text: String::new(),
        };
        let provider = Arc::new(MockProvider::scripted(vec![
            step(
                json!({"type":"create","steps":[{"id":"read","title":"查询资料"},{"id":"answer","title":"整理答复"}]}),
            ),
            step(json!({"type":"update","stepId":"read","state":"in_progress"})),
            MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: json!({"path":"资料.txt"}),
                text: String::new(),
            },
            step(
                json!({"type":"update","stepId":"read","state":"completed","evidenceCallIds":["call_2"]}),
            ),
            step(json!({"type":"update","stepId":"answer","state":"in_progress"})),
            MockMode::Fixed("读取标记并整理完毕".into()),
        ]));
        let host = Host::with_provider(cfg.clone(), provider).await.unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "plan".into(),
        };
        let binding = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(&scope, &binding.session_id, "查询资料并整理".into())
            .await
            .unwrap();
        let view = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let v = host.get_run(&scope, &run.run_id).await.unwrap();
                if v.state.is_terminal() {
                    break v;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(view.state, RunState::Completed);
        let id = run.run_id.clone();
        let plan = host
            .inner
            .store
            .call(move |db| {
                assert_eq!(db.office_task_assignments(&id)?.get("call_2"), Some(&0));
                db.progress(&id)
            })
            .await
            .unwrap();
        assert_eq!(
            plan.as_ref().unwrap()["steps"][0]["state"],
            "completed",
            "原引擎结果应能确认本轮真实调用"
        );
        assert_eq!(project(plan.clone(), &view).unwrap().1, 100);
        host.shutdown().await.unwrap();
        let host = Host::with_provider(cfg, Arc::new(MockProvider::fixed("不应重放")))
            .await
            .unwrap();
        let id = run.run_id.clone();
        assert_eq!(
            host.inner
                .store
                .call(move |db| db.progress(&id))
                .await
                .unwrap(),
            plan
        );
        host.delete_session(&scope).await.unwrap();
        let id = run.run_id;
        host.inner
            .store
            .call(move |db| {
                assert!(db.progress(&id).is_err());
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("资料.txt")).unwrap(),
            "真实读取标记"
        );
        host.shutdown().await.unwrap();
    }
}
