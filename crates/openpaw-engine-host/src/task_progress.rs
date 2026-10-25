//! 职责：验证Office运行内业务步骤与真实工具证据，生成有界进度。
//! 输入输出：模型元数据报告、原Run及已开始调用到host_events派生事实。
//! 依赖：原工具声明、调用绑定和单写Store；不另建任务数据库。
//! 禁止职责：不执行项目工具、不批准写入、不把模型自报当成真实执行。
use crate::{HostError, HostResult, RunState, RunView};
use openpaw_core::{ToolCallBinding, ToolCallOutcome, ToolSpec};
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
    serde_json::from_value(value.clone()).map_err(|_| invalid("步骤报告字段不符合约定。更新必须包含type=update、stepId、state；例如{\"type\":\"update\",\"stepId\":\"原步骤ID\",\"state\":\"in_progress\"}。completed还须提供evidenceCallIds，不能用工具名、路径或时间戳代替真实调用ID。"))
}
fn needs_write(title: &str) -> bool {
    // “查看已保存的记录文件位置”是读取事实；不能把过去动作或名词当成新写入。
    // 显式“核对后保存/并记录到…”仍命中，新产物仍须相符的真实写入证据。
    let action = ["保存", "写入", "落盘", "记录", "生成文件", "导出"]
        .iter()
        .fold(title.to_owned(), |text, verb| {
            text.replace(&format!("已{verb}"), "")
        });
    ["保存", "写入", "落盘", "生成文件", "导出"]
        .iter()
        .any(|s| action.contains(s))
        || action.trim_start().starts_with("记录")
        || [
            "请记录",
            "并记录",
            "再记录",
            "后记录",
            "及记录",
            "记录到",
            "记录至",
        ]
        .iter()
        .any(|s| action.contains(s))
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
    serde_json::from_value(json!({"name":NAME,"description":"报告本轮用户目标的业务步骤及进度，不执行任何项目操作。多步骤任务先create固定计划，再update。保存/记录步骤标题须说明产物路径，completed必须引用本轮已成功的真实调用ID；文件步骤须引用write_file/edit_file；真实协作也可引用父delegate_task调用ID，Host再核验该成功子Run的实际写入和相同产物路径。不能把子调用ID当父Run证据。无工具的整理步骤保持in_progress，最终交付由Host确认。不能用此工具代替真实联网、文件写入或审批。","input_schema":{"type":"object","oneOf":[{"type":"object","additionalProperties":false,"required":["type","steps"],"properties":{"type":{"const":"create"},"steps":{"type":"array","minItems":1,"maxItems":12,"items":definition}}},{"type":"object","additionalProperties":false,"required":["type","stepId","state"],"properties":{"type":{"const":"update"},"stepId":{"type":"string"},"state":{"enum":["pending","in_progress","completed"]},"evidenceCallIds":{"type":"array","maxItems":32,"items":{"type":"string"}}}}]}})).expect("固定工具声明合法")
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

/// 功能：在Office真实工具成功结果后补充精确执行回执，避免遗漏进度报告。
/// 参数：outcome为原工具结果，binding为实际执行绑定，enabled为Office进度开关。
/// 返回：保留原正文、错误及结构化结果的回执文本。
/// 错误：无；失败结果和未启用进度的调用原样返回。
/// 状态：调用方仅在原工具返回且执行结果可靠保存后使用，不写入或自动确认步骤。
/// 并发与取消：纯转换，取消和实际清理仍由原执行入口负责。
/// 权限：回执不是审批凭据，不含输入正文；取回网页不代表内容已经核验。
/// 关联：需求REQ-OP-012；设计DES-OP-006/007；缺陷D48。
pub(crate) fn with_execution_receipt(
    mut outcome: ToolCallOutcome,
    binding: &ToolCallBinding,
    enabled: bool,
) -> ToolCallOutcome {
    if enabled && !outcome.is_error && binding.tool_name != NAME {
        let receipt = json!({"callId":binding.call_id,"tool":binding.tool_name});
        // 委派的执行证据属于父调用，真实子写入仍由原Store校验；不能要求父复制子工具调用。
        let evidence = if binding.tool_name == "delegate_task" {
            "这是父delegate_task的执行回执。若子Run成功且其结果满足步骤，可引用上述父delegate_task调用ID；保存步骤由Host核验该子Run真实成功的write_file/edit_file及相同路径，不需要也不能把子调用ID冒充父证据。子Run内部仍走同一Host审批与执行管线；父模型未看到UI点击，不等于写入未审批。子文本不是审批凭据，工具成功不代表内容验收或步骤自动完成。"
        } else {
            "保存步骤仍须引用对应路径的实际write_file/edit_file回执。"
        };
        outcome.content.push_str(&format!("\n\n[Host执行回执]\n{receipt}\n以上仅确认本次工具执行成功，不代表资料内容有效或业务步骤已完成。核对结果符合对应步骤要求后，在进入下一步骤或最终回复前调用report_progress：{{\"type\":\"update\",\"stepId\":\"当前计划中对应步骤的id\",\"state\":\"completed\",\"evidenceCallIds\":[{}]}}。必须替换stepId，逐字使用此callId；若内容无效先继续检索，不得报告完成。{evidence}\n[/Host执行回执]", json!(binding.call_id)));
    }
    outcome
}

/// 功能：回显已接受的有界计划，使模型持续看到尚未确认的步骤。
/// 参数：value为原Store刚读取的本轮计划。
/// 返回：步骤ID、标题和状态，不复制工具输入或结果正文。
/// 错误：已存计划损坏时明确报错，不伪造成功清单。
/// 状态：只读，不改变分母、证据或进度。
/// 并发与取消：在报告原单写序列内读取，取消门禁仍由原调用方处理。
/// 权限：仅反馈当前已授权Office Run的计划，不授予执行权限。
/// 关联：需求REQ-OP-012；设计DES-OP-006/007；缺陷D48。
pub(crate) fn accepted_plan_hint(value: Option<Value>) -> HostResult<String> {
    let plan: Plan = serde_json::from_value(value.ok_or_else(|| invalid("已记录计划缺失"))?)
        .map_err(HostError::storage)?;
    let steps = plan
        .steps
        .iter()
        .map(|s| json!({"id":s.id,"title":s.title,"state":s.state}))
        .collect::<Vec<_>>();
    Ok(format!("业务步骤已记录；真实操作和文件仍须通过原工具及审批执行。当前步骤：{}。in_progress尚未完成；核验真实操作结果后，用对应Host执行回执的callId报告completed，再处理下一步。最终答复前核对清单，无工具的整理答复由最终可靠结束确认。",json!(steps)))
}

/// 功能：为无效进度报告提供本轮真实成功调用的有界恢复提示。
/// 参数：run为当前运行视图，started为原Store读取的实际执行绑定。
/// 返回：最多16项精确调用ID及工具名；不含工具输入、正文或凭据。
/// 错误：无；无合格调用时返回空列表，不制造完成依据。
/// 状态：只读，不批准、执行或修改任何步骤。
/// 并发与取消：在报告原单写序列内计算，取消与终态校验仍由update负责。
/// 权限：只提示同Run已成功且有实际started记录的调用，失败/待审/其他Run不入选。
/// 关联：需求REQ-OP-012；设计DES-OP-006/007。
pub(crate) fn recovery_hint(run: &RunView, started: &[ToolCallBinding]) -> String {
    let calls = run
        .tools
        .iter()
        .rev()
        .filter(|tool| {
            tool.state == "completed"
                && tool.tool_name != NAME
                && tool.call_id.len() <= 256
                && started.iter().any(|b| {
                    b.run_id == run.run_id
                        && b.call_id == tool.call_id
                        && b.tool_name == tool.tool_name
                })
        })
        .take(16)
        .map(|tool| json!({"callId":tool.call_id,"tool":tool.tool_name}))
        .collect::<Vec<_>>();
    format!("。仅本轮已成功且实际执行的可用证据：{}。evidenceCallIds须逐字复制匹配操作的callId；先核实任务要求和来源，再用{{\"type\":\"update\",\"stepId\":\"原步骤ID\",\"state\":\"completed\",\"evidenceCallIds\":[\"真实callId\"]}}重试。文件步骤仍须匹配真实写入路径；无工具的整理步骤用in_progress，不伪造证据。",json!(calls))
}

/// 功能：将模型报告与本Run真实执行事实绑定后生成下一份计划。
/// 参数：previous为已存计划，value为报告，run为当前事实，started为原执行绑定。
/// 返回：有界规范计划；重复报告产生相同内容。
/// 错误：终态、计划替换、无证据完成或跨运行引用均拒绝。
/// 状态：计划分母固定、已完成步骤不可回退，写入步骤必须有成功写入证据。
/// 并发与取消：调用方在原Store单写命令中执行；不持工作区资源。
/// 权限：仅Office绑定可调用，可信run_id不取模型输入。
/// 关联：REQ-OP-012；CHG-2026-0006第9.9节。
#[cfg(test)]
pub(crate) fn update(
    previous: Option<Value>,
    value: &Value,
    run: &RunView,
    started: &[ToolCallBinding],
) -> HostResult<Value> {
    update_with_delegations(previous, value, run, started, &[])
}

/// 用途：经原Store逐字段校验的子Run成功文件证据。
/// 字段：准确父委派call ID及真实子工具成功写入路径。
/// 来源去向：原子JSONL/host_events到父业务进度校验。
/// 约束：不是伪造父写工具；子拒绝/失败不能产生证据。
/// 稳定性：Host内部，不改变report_progress协议。
/// 关联：需求/设计REQ-OP-022/023；DES-OP-012。
pub(crate) struct DelegatedWrite {
    /// 用途：父Run真正执行的delegate_task调用ID。
    /// 字段：String，独立于子工具call ID。
    /// 来源去向：可信持久委派引用到原进度校验。
    /// 约束：调用者必须先验证子Run归属及终态。
    /// 稳定性：Host内部证据字段。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) call_id: String,
    /// 用途：该委派真实子Run成功写入的文件路径。
    /// 字段：Vec<String>，只包含原started及成功ToolResult对应的路径。
    /// 来源去向：原工具事实到父业务步骤文件匹配。
    /// 约束：不能由模型或浏览器声明；不包含文件正文。
    /// 稳定性：Host内部证据字段。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) paths: Vec<String>,
}

/// 功能：在原单Run进度规则中验证额外的可信委派文件证据。
/// 参数：previous/value/run/started沿用原校验；delegated由原Store从真实子Run构造。
/// 返回：验证后的固定计划，不替模型自动推进步骤。
/// 错误：未执行、失败、错父/错路径或未知证据仍拒绝。
/// 状态：纯校验，实际保存仍走原Store。
/// 并发与取消：在原单写序列内读取一致事实。
/// 权限：父只可引用自己的delegate call，不能借其他子Run的工具ID。
/// 关联：需求/设计REQ-OP-022/023；DES-OP-012。
pub(crate) fn update_with_delegations(
    previous: Option<Value>,
    value: &Value,
    run: &RunView,
    started: &[ToolCallBinding],
    delegated: &[DelegatedWrite],
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
            write |= evidence.iter().any(|id| {
                run.tools.iter().any(|t| {
                    t.call_id == *id && t.tool_name == "delegate_task" && t.state == "completed"
                }) && delegated.iter().any(|d| {
                    d.call_id == *id && d.paths.iter().any(|path| step.title.contains(path))
                })
            });
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
        let answer = ["整理","总结","汇总","答复","交付"].iter().any(|v|s.title.contains(v));
        let done=s.state=="completed" || run.state==RunState::Completed && !run.text.trim().is_empty() && !needs_write(&s.title) && answer;
        if done {completed+=1;}
        let waiting=s.evidence.iter().any(|id|run.tools.iter().any(|t|&t.call_id==id && matches!(t.state.as_str(),"awaiting_approval"|"awaiting_resource")));
        // 正常回复结束与业务核验是不同事实，不能将漏报或待继续步骤伪报为执行失败。
        let state=if done {"completed"} else if run.state==RunState::Completed {"blocked"} else if run.state.is_terminal() {"failed"} else if waiting || run.state==RunState::Cancelling {"blocked"} else if s.state=="in_progress" {"running"} else {"queued"};
        json!({"id":format!("step:{}:{}",run.run_id,index),"title":s.title,"status":state,"progress":if done{100}else{0},"summary":if done{"步骤已确认"}else if run.state==RunState::Completed{"回复已结束；本步骤待核验或继续处理"}else if run.state.is_terminal(){"本步骤未完成"}else if waiting{"等待确认或资源"}else if s.state=="in_progress"{"正在处理"}else{"待处理"}})
    }).collect::<Vec<_>>();
    let delivered = run.state == RunState::Completed && completed == steps.len();
    let percent = (completed + usize::from(delivered)) * 100 / (steps.len() + 1);
    Ok((steps, percent as u64))
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
    fn normal_reply_with_unverified_work_is_blocked_without_false_failure_or_completion() {
        let mut r = run();
        let original = plan(&r);
        r.state = RunState::Completed;
        r.text = "已给出当前答复，但未执行保存".into();
        let (steps, percent) = project(Some(original.clone()), &r).unwrap();
        assert_eq!(steps[0]["status"], "blocked");
        assert_eq!(steps[1]["status"], "blocked");
        assert_eq!(steps[2]["status"], "completed");
        assert!(percent < 100, "回复结束不能代替未执行的业务步骤");
        r.state = RunState::Failed;
        let (steps, percent) = project(Some(original), &r).unwrap();
        assert_eq!(steps[0]["status"], "failed");
        assert_eq!(steps[1]["status"], "failed");
        assert!(percent < 100, "真实失败不能被隐藏或伪报完成");
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
    fn delegated_file_completion_requires_exact_parent_call_and_child_artifact() {
        let mut r = run();
        let p = plan(&r);
        r.tools.push(ToolView {
            call_id: "delegate-draft".into(),
            tool_name: "delegate_task".into(),
            state: "completed".into(),
        });
        let b = ToolCallBinding {
            run_id: r.run_id.clone(),
            call_id: "delegate-draft".into(),
            tool_name: "delegate_task".into(),
            input: json!({"role_id":"draft"}),
        };
        let report = json!({"type":"update","stepId":"record","state":"completed","evidenceCallIds":["delegate-draft"]});
        for (call, path) in [
            ("other-parent", "weather.md"),
            ("delegate-draft", "other.md"),
        ] {
            let evidence = [DelegatedWrite {
                call_id: call.into(),
                paths: vec![path.into()],
            }];
            assert!(update_with_delegations(
                Some(p.clone()),
                &report,
                &r,
                std::slice::from_ref(&b),
                &evidence
            )
            .is_err());
        }
        let evidence = [DelegatedWrite {
            call_id: b.call_id.clone(),
            paths: vec!["weather.md".into()],
        }];
        assert!(
            update_with_delegations(Some(p.clone()), &report, &r, &[], &evidence).is_err(),
            "没有真实父started不能确认"
        );
        r.tools[0].state = "failed".into();
        assert!(
            update_with_delegations(
                Some(p.clone()),
                &report,
                &r,
                std::slice::from_ref(&b),
                &evidence
            )
            .is_err(),
            "委派失败不能确认子产物"
        );
        r.tools[0].state = "completed".into();
        assert!(update_with_delegations(Some(p), &report, &r, &[b], &evidence).is_ok());
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
    fn recovery_lists_only_this_runs_successful_started_calls_without_inputs() {
        let mut r = run();
        let mut started = Vec::new();
        for i in 0..25 {
            let call = format!("actual_{i}");
            r.tools.push(ToolView {
                call_id: call.clone(),
                tool_name: "read_file".into(),
                state: "completed".into(),
            });
            started.push(ToolCallBinding {
                run_id: r.run_id.clone(),
                call_id: call,
                tool_name: "read_file".into(),
                input: json!({"path":"private-input-must-not-leak"}),
            });
        }
        for (id, state, binding_run) in [
            ("unstarted", "completed", "other"),
            ("failed-call", "failed", "r"),
            ("pending-call", "awaiting_approval", "r"),
        ] {
            r.tools.push(ToolView {
                call_id: id.into(),
                tool_name: "write_file".into(),
                state: state.into(),
            });
            started.push(ToolCallBinding {
                run_id: binding_run.into(),
                call_id: id.into(),
                tool_name: "write_file".into(),
                input: json!({"content":"secret"}),
            });
        }
        let hint = recovery_hint(&r, &started);
        assert_eq!(hint.matches("\"callId\":").count(), 16);
        assert!(hint.contains("actual_24"));
        for forbidden in [
            "unstarted",
            "failed-call",
            "pending-call",
            "private-input-must-not-leak",
            "secret",
        ] {
            assert!(!hint.contains(forbidden));
        }
        let e = validate(&json!({"stepId":"read","state":"completed"})).unwrap_err();
        assert!(e.message.contains("type=update") && e.message.contains("evidenceCallIds"));
    }
    #[test]
    fn execution_receipt_keeps_original_result_and_never_confirms_a_step() {
        let binding = ToolCallBinding {
            run_id: "r".into(),
            call_id: "actual-call".into(),
            tool_name: "write_file".into(),
            input: json!({"content":"private-input"}),
        };
        let mut outcome = ToolCallOutcome::ok("wrote original bytes");
        outcome.structured = Some(json!({"original":true}));
        let result = with_execution_receipt(outcome.clone(), &binding, true);
        assert!(result.content.starts_with(&outcome.content));
        assert!(result.content.contains("\"callId\":\"actual-call\""));
        assert!(result.content.contains("evidenceCallIds") && result.content.contains("不代表"));
        assert!(!result.content.contains("private-input"));
        assert_eq!(result.structured, outcome.structured);
        assert!(!result.is_error);
        assert_eq!(
            with_execution_receipt(outcome.clone(), &binding, false).content,
            outcome.content
        );
        let mut delegated = binding.clone();
        delegated.tool_name = "delegate_task".into();
        let child = ToolCallOutcome::ok("child execution completed; content still needs review");
        let delegated_result = with_execution_receipt(child.clone(), &delegated, true);
        assert!(
            spec().description.contains("delegate_task"),
            "委派证据说明必须与原校验一致"
        );
        assert!(
            delegated_result.content.contains("子Run")
                && delegated_result.content.contains("父delegate_task")
        );
        assert!(!delegated_result
            .content
            .contains("保存步骤仍须引用对应路径的实际write_file/edit_file回执"));
        assert!(result
            .content
            .contains("保存步骤仍须引用对应路径的实际write_file/edit_file回执"));
        assert_eq!(
            with_execution_receipt(child.clone(), &delegated, false).content,
            child.content
        );
        let failed_delegate = with_execution_receipt(
            ToolCallOutcome::error("child actually failed"),
            &delegated,
            true,
        );
        assert!(failed_delegate.is_error);
        assert_eq!(failed_delegate.content, "child actually failed");
        let failed = ToolCallOutcome::error("actual failure");
        let result = with_execution_receipt(failed, &binding, true);
        assert!(result.is_error);
        assert_eq!(result.content, "actual failure");
        let r = run();
        let p = plan(&r);
        let hint = accepted_plan_hint(Some(p.clone())).unwrap();
        assert!(hint.contains("\"id\":\"record\"") && hint.contains("pending"));
        assert_eq!(
            project(Some(p), &r).unwrap().1,
            0,
            "回执/清单均不能推进进度"
        );
        assert!(accepted_plan_hint(Some(json!({"steps":[]}))).is_err());
    }
    #[test]
    fn checking_saved_artifact_location_requires_query_evidence_not_another_write() {
        let mut r = run();
        r.tools.push(ToolView {
            call_id: "lookup".into(),
            tool_name: "glob".into(),
            state: "completed".into(),
        });
        let binding = ToolCallBinding {
            run_id: r.run_id.clone(),
            call_id: "lookup".into(),
            tool_name: "glob".into(),
            input: json!({"pattern":"reports/*.md"}),
        };
        let title = "核对本地日期与已保存的长沙天气记录文件位置";
        let p = update(
            None,
            &json!({"type":"create","steps":[{"id":"check","title":title}]}),
            &r,
            &[],
        )
        .unwrap();
        let report = json!({"type":"update","stepId":"check","state":"completed","evidenceCallIds":["lookup"]});
        assert!(
            update(Some(p.clone()), &report, &r, &[]).is_err(),
            "查询也必须有真实started"
        );
        let confirmed = update(Some(p), &report, &r, std::slice::from_ref(&binding)).unwrap();
        r.state = RunState::Completed;
        assert_eq!(project(Some(confirmed), &r).unwrap().1, 100);
        r.state = RunState::Running;
        for title in [
            "保存 reports/new.md",
            "记录天气到 reports/new.md",
            "核对已保存的文件后写入 reports/new.md",
            "查询记录并保存 reports/new.md",
            "核对日期并记录到 reports/new.md",
        ] {
            let p = update(
                None,
                &json!({"type":"create","steps":[{"id":"check","title":title}]}),
                &r,
                &[],
            )
            .unwrap();
            assert!(
                update(Some(p), &report, &r, std::slice::from_ref(&binding)).is_err(),
                "读证据不能证明新的写操作：{title}"
            );
        }
        for title in [
            "读取历史记录",
            "查询已保存的文件路径",
            "核对已写入的文件位置",
        ] {
            assert!(!needs_write(title), "只读语境：{title}");
        }
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
        let cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"mock","tools":["read_file","glob"]})).unwrap();
        let step = |input| MockMode::ToolUse {
            tool_name: NAME.into(),
            input,
            text: String::new(),
        };
        let provider = Arc::new(MockProvider::scripted(vec![
            step(
                json!({"type":"create","steps":[{"id":"read","title":"查询资料"},{"id":"check","title":"核对已保存的资料记录文件位置"},{"id":"answer","title":"整理答复"}]}),
            ),
            step(json!({"type":"update","stepId":"read","state":"in_progress"})),
            MockMode::ToolUse {
                tool_name: "read_file".into(),
                input: json!({"path":"资料.txt"}),
                text: String::new(),
            },
            step(
                json!({"type":"update","stepId":"read","state":"completed","evidenceCallIds":["read_file:资料.txt"]}),
            ),
            step(
                json!({"type":"update","stepId":"read","state":"completed","evidenceCallIds":["call_2"]}),
            ),
            MockMode::ToolUse {
                tool_name: "glob".into(),
                input: json!({"pattern":"资料.txt"}),
                text: String::new(),
            },
            step(
                json!({"type":"update","stepId":"check","state":"completed","evidenceCallIds":["call_5"]}),
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
        assert!(
            view.tools
                .iter()
                .any(|t| t.tool_name == NAME && t.state == "failed"),
            "无效证据仍须拒绝，不能为恢复直接批准"
        );
        let events = host
            .inner
            .store
            .call({
                let id = binding.session_id.clone();
                move |db| db.events(&id)
            })
            .await
            .unwrap();
        assert!(events.iter().any(|e| matches!(e, openpaw_core::SessionEvent::Message(m) if m.content.iter().any(|block| matches!(block,openpaw_core::ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("\"callId\":\"call_2\"") && content.contains("evidenceCallIds"))))), "失败结果须向模型提供本轮真实证据，随后正确重试才能完成");
        assert!(events.iter().any(|e| matches!(e, openpaw_core::SessionEvent::Message(m) if m.content.iter().any(|block| matches!(block,openpaw_core::ContentBlock::ToolResult { content, is_error: false, tool_use_id, .. } if tool_use_id=="call_2" && content.contains("真实读取标记") && content.contains("[Host执行回执]") && content.contains("\"callId\":\"call_2\""))))), "成功正文须携带本次真实回执，不能等错误才提示");
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
