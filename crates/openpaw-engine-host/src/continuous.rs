//! 职责：管理一次用户请求中的通用持续意图、阶段检查点和有界展示。
//! 输入输出：可信配置/主Run工具调用到原host_events和接续上下文。
//! 依赖：原AgentLoop、Group、Store、Session和审批管线。
//! 禁止职责：不实现文章工作流、不递归创建主Run、不伪造用户输入或重放工具。
use crate::{Host, HostError, HostResult};
use openpaw_core::{SessionEvent, ToolCallBinding, ToolSpec};
use openpaw_engine::RunObserver;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// 用途：本机显式启用的持续任务与产物授权策略。
/// 字段：是否启用、产物子目录、自动审批开关和阶段间隔。
/// 来源去向：可信配置到Host，不接受模型修改。
/// 约束：只在主模型调用start后生效，不改变普通问答。
/// 稳定性：默认关闭，旧配置兼容。
/// 关联：CHG-2026-0007第9.12节。
pub struct ContinuousConfig {
    /// 用途：限制同一持续任务中同时开放的业务事项数。
    /// 字段：usize，默认2，合法范围1至16。
    /// 来源去向：可信配置到open_item原子校验。
    /// 约束：不是子Agent并发数，不按文章类型判断。
    /// 稳定性：可选配置，旧配置默认2。
    /// 关联：CHG-2026-0007第9.13节D107。
    pub max_open_items: usize,
    /// 用途：可信配置启用持续模式。
    /// 字段：bool；默认false。
    /// 来源去向：本机TOML到主Run。
    /// 约束：不改变普通问答结束语义。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub enabled: bool,
    /// 用途：自动批准的产物子目录。
    /// 字段：String；默认outputs。
    /// 来源去向：本机配置到路径授权校验。
    /// 约束：只允许非空相对子目录，不含父目录。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub output_dir: String,
    /// 用途：在持续任务范围内自动批准产物写入。
    /// 字段：bool；默认true。
    /// 来源去向：可信配置到原审批服务。
    /// 约束：仅活动持续任务、准确输入及产物目录内生效。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub auto_approve: bool,
    /// 用途：阶段之间的可取消等待。
    /// 字段：u64；默认250毫秒。
    /// 来源去向：本机配置到阶段协调。
    /// 约束：最大60000，不替代工具空闲超时。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub stage_pause_ms: u64,
}
impl Default for ContinuousConfig {
    fn default() -> Self {
        Self {
            max_open_items: 2,
            enabled: false,
            output_dir: "outputs".into(),
            auto_approve: true,
            stage_pause_ms: 250,
        }
    }
}
impl ContinuousConfig {
    /// 功能：校验产物子目录、接续间隔和同时开放事项数。
    /// 参数：self为本机配置。
    /// 返回：合法返回成功。
    /// 错误：绝对/逃逸/空目录、异常间隔或事项数越界返回invalid_input。
    /// 状态：只读，不创建目录。
    /// 并发与取消：无等待。
    /// 权限：禁止把整个工作区或父目录作为自动授权子目录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) fn validate(&self) -> HostResult<()> {
        let path = Path::new(&self.output_dir);
        if self.output_dir.trim().is_empty()
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
            || path.components().count() == 0
            || self.stage_pause_ms > 60000
            || !(1..=16).contains(&self.max_open_items)
        {
            return Err(HostError::new(
                "invalid_input",
                "持续任务需要非空工作区相对子目录、不超过60秒阶段间隔及1至16个同时事项",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
/// 用途：原主Run的版本化持续控制记录。
/// 字段：run_id、启停、原目标、下一步和有界显示文本。
/// 来源去向：真实工具调用到原host_events；内存仅保存同一记录副本。
/// 约束：不存密钥、不替代JSONL模型事实，重启不自动执行。
/// 稳定性：内部控制payload v1。
/// 关联：CHG-2026-0007第9.12节。
pub(crate) struct State {
    /// 用途：记录本次持续Run已开放且尚未关闭的业务事项。
    /// 字段：有界事项ID列表，旧控制记录默认空。
    /// 来源去向：原持续工具到原host_events和委派校验。
    /// 约束：不保存另一套Run状态，不解释业务类型或文件路径。
    /// 稳定性：v1兼容可选字段。
    /// 关联：CHG-2026-0007第9.13节D107。
    #[serde(default)]
    pub(crate) open_items: Vec<String>,
    /// 用途：持久控制payload版本。
    /// 字段：u8；写入1。
    /// 来源去向：持续工具到原host_events。
    /// 约束：不建立另一任务数据库。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) version: u8,
    /// 用途：所属原主Run标识。
    /// 字段：String，所属主运行的稳定标识。
    /// 来源去向：已绑定工具调用到控制事实。
    /// 约束：不能跨Run控制。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) run_id: String,
    /// 用途：是否曾启动持续展示模式。
    /// 字段：bool，记录是否已进入持续模式。
    /// 来源去向：持久事实到投影。
    /// 约束：finish后仍保留历史摘要。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) started: bool,
    /// 用途：是否自动接续下一阶段。
    /// 字段：bool，表示本次运行是否继续接续。
    /// 来源去向：可信启用且主模型start到运行协调。
    /// 约束：停止或故障不自动恢复。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) active: bool,
    /// 用途：本次持续任务的原始目标摘要。
    /// 字段：String；最多4000字。
    /// 来源去向：start输入到下一阶段上下文。
    /// 约束：作为任务数据，不提升权限。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) goal: String,
    /// 用途：下一阶段待办检查点。
    /// 字段：String；最多4000字。
    /// 来源去向：真实checkpoint到接续上下文。
    /// 约束：不得重放已完成副作用。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) next: String,
    /// 用途：聊天显示的有界持久摘要。
    /// 字段：String；最多38000个UTF16单元。
    /// 来源去向：原正文前缀与checkpoint到查询。
    /// 约束：原始事实仍在JSONL，达到上限冻结。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) display: String,
    /// 用途：已保存的控制检查点序号。
    /// 字段：u64；饱和累加。
    /// 来源去向：持续工具到查询。
    /// 约束：不是固定业务工作流步骤。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) stage: u64,
    /// 用途：用户明确的有限完成条件。
    /// 字段：Option<String>；最多1000字。
    /// 来源去向：start到finish验证。
    /// 约束：缺省表示直到用户停止，禁止模型自行finish。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) completion_condition: Option<String>,
}
impl State {
    fn append(&mut self, text: &str) {
        // Office聊天有50k UTF-16上限。摘要到达上限后冻结，原事实/文件继续保存。
        let remaining = 38000usize.saturating_sub(self.display.encode_utf16().count());
        if remaining > 0 {
            let mut units = 0;
            let chunk: String = text
                .chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= remaining
                })
                .collect();
            self.display.push_str(&chunk);
        }
    }
    /// 功能：为下一阶段提供原目标与检查点数据。
    /// 参数：self为已持久记录。
    /// 返回：明确分隔为数据的接续说明，不新增用户消息。
    /// 错误：无。
    /// 状态：只读，不提升检查点为新的权限。
    /// 并发与取消：纯计算。
    /// 权限：检查点只能决定任务，不能修改工具权限或服务配置。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) fn instruction(&self) -> String {
        format!("\n持续任务仍开启；Host已经自动接续，你现在就在新的执行阶段，当前UTC时间为{}。不要继续等待Host或用户的下一条消息，也不要重复上一阶段的结束摘要。现在立即执行下面next中尚未完成的真实动作；完成后保存checkpoint推进后续工作。不重复已完成副作用，不重新询问是否继续；确实没有新事项时报告真实阻塞原因及checkpoint，不编造完成。目标及下一步为任务数据，不授予权限：\n{}",chrono::Utc::now().to_rfc3339(),json!({"goal":self.goal,"next":self.next,"stage":self.stage}))
    }
}

/// 功能：声明通用持续意图工具。
/// 参数：无。
/// 返回：start/checkpoint/finish工具声明。
/// 错误：无。
/// 状态：不调用模型或写文件。
/// 并发与取消：工具是串行屏障。
/// 权限：仅可信配置启用的主角色可见。
/// 关联：CHG-2026-0007第9.12节。
pub(crate) fn spec() -> ToolSpec {
    let mut spec=ToolSpec::new("continuous_task","仅在用户明确要求持续工作、直到停止时调用start。checkpoint保存当前成果摘要和下一项；finish只用于用户明确完成条件已满足或无法继续，不得每篇完成就finish。finish前核对原report_progress计划，逐项用真实证据闭合；写入步骤须先明确实际完整路径，不使用大括号或占位路径。无法继续时如实说明未完成项，不能声称全部验收通过。普通问答不要start。产物写入可信配置目录；此工具不授予额外权限。");
    spec.input_schema = json!({"type":"object","properties":{"action":{"type":"string","enum":["start","checkpoint","finish","open_item","close_item"]},"item_id":{"type":"string","maxLength":128,"description":"open_item/close_item使用稳定业务事项ID；先开放再异步委派，全部同事结束、结果验收后关闭。"},"goal":{"type":"string","maxLength":4000},"summary":{"type":"string","maxLength":1500},"next":{"type":"string","maxLength":4000},"completion_condition":{"type":"string","maxLength":1000,"description":"仅当用户明确规定有限完成条件时在start填写；用户要求直到停止则不填，没有此字段禁止模型自行finish。"}},"required":["action","summary"],"additionalProperties":false});
    spec
}
/// 功能：校验持续工具最终输入。
/// 参数：input为Hook后JSON。
/// 返回：合法返回成功。
/// 错误：无效动作、空摘要/目标或超长返回invalid_input。
/// 状态：只读。
/// 并发与取消：纯计算。
/// 权限：不从输入接受配置、目录或身份。
/// 关联：CHG-2026-0007第9.12节。
pub(crate) fn validate(input: &Value) -> HostResult<()> {
    let obj = input
        .as_object()
        .ok_or_else(|| HostError::new("invalid_input", "持续工具需要对象"))?;
    if obj.keys().any(|k| {
        !matches!(
            k.as_str(),
            "action" | "goal" | "summary" | "next" | "completion_condition" | "item_id"
        )
    }) {
        return Err(HostError::new("invalid_input", "持续工具存在未知字段"));
    }
    let action = input["action"].as_str().unwrap_or("");
    if !matches!(
        action,
        "start" | "checkpoint" | "finish" | "open_item" | "close_item"
    ) || input["summary"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty() || s.chars().count() > 1500)
        || (action == "start" && input["goal"].as_str().is_none_or(|s| s.trim().is_empty()))
        || ["goal", "next"].iter().any(|k| {
            input
                .get(k)
                .is_some_and(|v| v.as_str().is_none_or(|s| s.chars().count() > 4000))
        })
    {
        return Err(HostError::new(
            "invalid_input",
            "持续任务动作、摘要或目标不合法",
        ));
    }
    if matches!(action, "open_item" | "close_item") {
        if input["item_id"].as_str().is_none_or(|id| {
            id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_control)
        }) {
            return Err(HostError::new(
                "invalid_input",
                "开放或关闭事项需要不超过128字节的item_id",
            ));
        }
    } else if input.get("item_id").is_some() {
        return Err(HostError::new(
            "invalid_input",
            "item_id仅用于open_item/close_item",
        ));
    }
    if input.get("completion_condition").is_some_and(|v| {
        v.as_str()
            .is_none_or(|s| s.trim().is_empty() || s.chars().count() > 1000)
    }) || (action != "start" && input.get("completion_condition").is_some())
    {
        return Err(HostError::new(
            "invalid_input",
            "完成条件仅在start指定，须为不超过1000字的非空文本",
        ));
    }
    Ok(())
}
impl Host {
    /// 功能：在原单写Store中提交持续控制事实，再更新共享Group。
    /// 参数：execution为可信主角色，binding为原最终工具输入。
    /// 返回：实际模式、阶段和产物子目录。
    /// 错误：越权、取消、状态冲突或持久化失败。
    /// 状态：先保存后发布；有限收尾保留原计划并透传未核验提醒，不建立另一套任务数据库。
    /// 并发与取消：在Store序列中检查Run仍运行和同源取消。
    /// 权限：子角色不能启停主任务，也不能修改配置。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) async fn continuous_update(
        &self,
        execution: &crate::orchestration::Execution,
        binding: ToolCallBinding,
    ) -> HostResult<String> {
        validate(&binding.input)?;
        let config = execution
            .group
            .config
            .continuous
            .as_ref()
            .filter(|c| c.enabled)
            .ok_or_else(|| HostError::new("scope_denied", "未启用持续任务"))?;
        if execution.role != "lead" || binding.run_id != execution.group.parent {
            return Err(HostError::new("scope_denied", "只有主角色可管理持续任务"));
        }
        let output_dir = config.output_dir.clone();
        let max_open_items = config.max_open_items;
        let group = execution.group.clone();
        self.inner.store.call_priority(move |db| {
            let run=db.get_run(&binding.run_id)?;
            if run.state.is_terminal() || group.cancel.is_cancelled() {return Err(HostError::new("cancelled","持续任务已停止"));}
            db.binding(&run.session_id)?;
            let bound=db.events(&run.session_id)?.into_iter().any(|event|matches!(event,SessionEvent::Message(m) if m.content.iter().any(|c|matches!(c,openpaw_core::ContentBlock::ToolUse{id,name,input} if id==&binding.call_id && name=="continuous_task" && input==&binding.input))));
            if !bound {return Err(HostError::new("scope_denied","持续工具输入未绑定原调用事实"));}
            let mut state=group.continuous.lock().unwrap_or_else(|e|e.into_inner());
            let action=binding.input["action"].as_str().unwrap_or("");
            let mut next=state.clone();
            if action=="start" {
                if state.started {return Err(HostError::new("request_conflict","持续任务已启动，后续使用checkpoint"));}
                next=State {version:1,run_id:binding.run_id.clone(),started:true,active:true,goal:binding.input["goal"].as_str().unwrap_or("").into(),display:run.text,completion_condition:binding.input["completion_condition"].as_str().filter(|s|!s.trim().is_empty()).map(str::to_owned),..Default::default()};
                next.append("\n持续任务已启动。完整成果保存到文件；聊天仅保留有界摘要。\n");
            } else if !state.active {return Err(HostError::new("request_conflict","没有活动持续任务"));}
            if action=="open_item" {
                let id=binding.input["item_id"].as_str().unwrap().to_owned();
                if !next.open_items.contains(&id) {
                    if next.open_items.len()>=max_open_items {return Err(HostError::new("capacity_exceeded","同时开放的业务事项已达配置上限，请先完成并关闭原事项"));}
                    // 旧事项不重新开放，避免同名目录/工作被误当新一轮而重放。
                    if db.delegations(&binding.run_id)?.iter().any(|d|d.work_item.as_ref()==Some(&id)) {return Err(HostError::new("request_conflict","已结束的事项ID不能重新开放，请使用新ID"));}
                    next.open_items.push(id);
                }
            }
            if action=="close_item" {
                let id=binding.input["item_id"].as_str().unwrap();
                if !next.open_items.iter().any(|item|item==id) {return Err(HostError::new("request_conflict","事项尚未开放或已经关闭"));}
                for d in db.delegations(&binding.run_id)?.iter().filter(|d|d.work_item.as_deref()==Some(id)) {
                    if !db.get_run(&d.child_run)?.state.is_terminal() {return Err(HostError::new("request_conflict","事项仍有执行中的同事，不能关闭"));}
                }
                next.open_items.retain(|item|item!=id);
            }
            if action=="finish" && next.completion_condition.is_none() {return Err(HostError::new("request_conflict","本任务要求持续到用户停止，模型不能自行finish；请保存checkpoint后继续"));}
            if action=="finish" && !next.open_items.is_empty() {return Err(HostError::new("request_conflict","仍有开放事项，请先收齐结果并关闭"));}
            next.active=action!="finish";
            next.stage=next.stage.saturating_add(1);
            if let Some(value)=binding.input["next"].as_str() {next.next=value.into();}
            next.append(&format!("\n{}\n",binding.input["summary"].as_str().unwrap_or("")));
            if action=="finish" {
                let unfinished=db.progress(&binding.run_id)?.as_ref().and_then(|p|p["steps"].as_array()).into_iter().flatten()
                    .filter(|step|step["state"]!="completed")
                    .map(|step|format!("{}：{}",step["id"].as_str().unwrap_or(""),step["title"].as_str().unwrap_or("")))
                    .collect::<Vec<_>>();
                if !unfinished.is_empty() {
                    // finish也允许明确无法继续的有限收尾；不能因此伪造步骤完成，
                    // 也不能让模型“全部完成”的摘要盖掉Store仍未核验的事实。
                    next.append(&format!("\n收尾核验提醒：执行已结束，父计划仍有{}项未核验，不能视为全部验收通过。未核验步骤：{}。已保存成果保留；文件步骤须用与原计划一致的实际完整写入路径及真实成功调用核验，不自动重做。\n",unfinished.len(),unfinished.join("；")));
                }
            }
            let revision=db.bump("continuous_task",&serde_json::to_string(&next).map_err(HostError::storage)?)?;
            *state=next.clone();
            let _=db.notices.send(crate::HostNotice::Changed {revision});
            Ok(json!({"active":next.active,"stage":next.stage,"open_items":next.open_items,"max_open_items":max_open_items,"output_dir":output_dir,"saved":true}).to_string())
        }).await
    }
}

/// 用途：持续主Run每阶段复用原引擎的观察器适配。
/// 字段：原可靠观察器引用。
/// 来源去向：原引擎事实到原观察器。
/// 约束：阶段生命周期由外层合并为一次主Run；其他事实原样保存。
/// 稳定性：Host私有适配。
/// 关联：CHG-2026-0007第9.12节。
pub(crate) struct StageObserver<'a>(
    /// 用途：可靠事实写入委托。
    /// 字段：原RunObserver可变引用。
    /// 来源去向：主Run到原Store。
    /// 约束：只过滤阶段生命周期，其余事实原样保存。
    /// 稳定性：内部适配。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) &'a mut dyn RunObserver,
);
#[async_trait::async_trait]
impl RunObserver for StageObserver<'_> {
    async fn record(&mut self, event: SessionEvent) -> openpaw_core::Result<()> {
        if matches!(
            event,
            SessionEvent::RunStarted { .. } | SessionEvent::RunFinished { .. }
        ) {
            return Ok(());
        }
        self.0.record(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn d141_finite_finish_preserves_unverified_plan_warning_without_inventing_success() {
        use crate::{ClientScope, HostConfig, RunState};
        use openpaw_llm::{MockMode, MockProvider};
        for mode in ["pending", "verified", "no-plan"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("brief.md"), "真实资料").unwrap();
            let config: HostConfig = serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo","tools":["read_file"],"agents":{"enabled":true,"continuous":{"enabled":true}}})).unwrap();
            let tool = |name: &str, input| MockMode::ToolUse {
                tool_name: name.into(),
                input,
                text: String::new(),
            };
            let mut script = vec![tool(
                "continuous_task",
                json!({"action":"start","goal":"有限核对","summary":"开始","completion_condition":"核对结束或明确无法继续"}),
            )];
            if mode != "no-plan" {
                script.push(tool(
                    "report_progress",
                    json!({"type":"create","steps":[{"id":"check","title":"核对真实资料"}]}),
                ));
            }
            if mode == "verified" {
                script.push(tool("read_file", json!({"path":"brief.md"})));
                script.push(tool("report_progress", json!({"type":"update","stepId":"check","state":"completed","evidenceCallIds":["call_2"]})));
            }
            script.push(tool(
                "continuous_task",
                json!({"action":"finish","summary":"模型自称已全部完成"}),
            ));
            script.push(MockMode::Fixed("有限执行结束".into()));
            let host =
                Host::with_provider(config, std::sync::Arc::new(MockProvider::scripted(script)))
                    .await
                    .unwrap();
            let scope = ClientScope {
                namespace: "office".into(),
                slot_id: "office-1".into(),
                conversation_id: mode.into(),
            };
            let binding = host.open_session(scope.clone()).await.unwrap();
            let run = host
                .submit_run(&scope, &binding.session_id, "有限核对".into())
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
                .await
                .unwrap();
            assert_eq!(
                host.get_run(&scope, &run.run_id).await.unwrap().state,
                RunState::Completed
            );
            let (state, plan) = host
                .inner
                .store
                .call(move |db| {
                    Ok((
                        db.continuous_state(&run.run_id)?.unwrap(),
                        db.progress(&run.run_id)?,
                    ))
                })
                .await
                .unwrap();
            assert!(
                !state.active,
                "允许明确的有限失败收尾，不靠无限接续冒充恢复"
            );
            assert_eq!(state.display.contains("收尾核验提醒"), mode == "pending");
            if mode == "pending" {
                assert!(state.display.contains("check：核对真实资料"));
                assert!(state.display.contains("不能视为全部验收通过"));
                assert_eq!(plan.unwrap()["steps"][0]["state"], "pending");
            }
            host.shutdown().await.unwrap();
        }
    }
    #[test]
    fn control_schema_rejects_invalid_finish_conditions_and_scope_roots() {
        assert!(validate(&json!({"action":"start","goal":"持续目标","summary":"开始"})).is_ok());
        for value in [json!(5), json!(""), json!("x".repeat(1001))] {
            assert!(validate(&json!({"action":"start","goal":"目标","summary":"开始","completion_condition":value})).is_err());
        }
        assert!(validate(&json!({"action":"checkpoint","summary":"继续","completion_condition":"不能临时自定结束"})).is_err());
        for path in ["", ".", "..", "../other", "/tmp", "outputs/../other"] {
            assert!(ContinuousConfig {
                output_dir: path.into(),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }
}
