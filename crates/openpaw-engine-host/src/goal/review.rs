//! 功能：将真实计划审核接到现有问答及中立回合交互。
//! 边界：只读原件，持久转换经过Goal和runtime唯一writer；不直接修改Session。
//! 禁止承担：不伪造用户回合、不创建网络通道或预算；关联CHG-2026-0056。
use crate::runtime::SessionHandle;
use openpaw_core::{Error, NullToolHost, ToolInvocationContext, ToolSpec};
use openpaw_engine::turn_execution::{
    ToolBatchCoordination, TurnInteraction, TurnInteractionResolution,
};
use openpaw_engine::user_input::{wrap_tools, QuestionContext};
use openpaw_main_goal::GoalContext;
use std::{path::PathBuf, sync::Arc};

/// 功能：为原活动回合组装固定计划审核协调器。
/// 参数：handle为可信会话，goal/question为原上下文，path为原件路径，base为不含Goal的原系统提示。
/// 返回：无待审核时无动作的可选交互准备回调。
/// 错误：展示、记录或审核失效时原回合安全暂停或失败。
/// 状态：规范计划由Engine先发布，问答由唯一writer保存。
/// 并发与取消：等待复用原取消，未持有Session锁外的长期锁。
/// 权限：只使用后端固定问题及本次真实保存的答案。
/// 关联：TurnOptions；Stability: experimental。
pub(crate) fn coordination(
    handle: Arc<SessionHandle>,
    goal: Arc<GoalContext>,
    question: Arc<QuestionContext>,
    path: PathBuf,
    base: Option<String>,
) -> ToolBatchCoordination {
    Arc::new(move || {
        let review = goal.pending_review()?;
        let (handle, goal, question, path, base) = (
            handle.clone(),
            goal.clone(),
            question.clone(),
            path.clone(),
            base.clone(),
        );
        Some(TurnInteraction {
            presentation: review.plan.clone(),
            resolve: Box::pin(async move {
                goal.review_displayed(&review).map_err(Error::Session)?;
                let context = ToolInvocationContext {
                    turn_id: Some(review.turn_id.clone()),
                    response_index: review.response_index,
                    tool_call_id: review.tool_call_id.clone(),
                };
                let outcome = wrap_tools(Arc::new(NullToolHost), question)
                    .call_with_context("request_user_input", review.request.clone(), &context)
                    .await?;
                let record = if outcome.is_error {
                    None
                } else {
                    let read = openpaw_store::SessionReader::read(&path)
                        .map_err(|_| Error::session("无法核对已保存的计划审核回答"))?;
                    let visible = handle
                        .message_cache
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    crate::context::validated_questions(&handle.id, &read.events, &visible)
                        .into_iter()
                        .find(|record| {
                            record.turn_id == review.turn_id
                                && record.response_index == review.response_index
                                && record.tool_call_id == review.tool_call_id
                        })
                };
                if !outcome.is_error && record.is_none() {
                    return Err(Error::session("审核回答缺少可靠来源，未批准执行"));
                }
                let action = goal
                    .settle_review(&review, record.as_ref())
                    .map_err(Error::Session)?;
                if matches!(action.as_str(), "execute" | "custom") {
                    let mut tools: Vec<ToolSpec> = handle.tools.clone();
                    tools.extend(openpaw_main_goal::plan_tools());
                    goal.filter_tools(&mut tools);
                    let instruction = if action == "execute" {
                        "用户已明确批准刚展示的当前计划版本。先goal_read，再按当前执行约束继续；委派从pending直接start_agent，不要先start_task。".to_owned()
                    } else {
                        format!("用户通过该版本审核提出修改要求，尚未批准交付：{}。据此修改完整计划，新版本会再次等待确认。",record.as_ref().and_then(|r|r.answer.as_ref()).and_then(|a|a.custom_text.as_deref()).unwrap_or(""))
                    };
                    let system = [base, goal.prompt()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Ok(TurnInteractionResolution::Continue {
                        instruction,
                        tools,
                        system: Some(system),
                    })
                } else {
                    Ok(TurnInteractionResolution::Pause(
                        match action.as_str() {
                            "modify" => {
                                "计划尚未执行。请在原聊天补充修改要求，更新后会再次弹出确认菜单。"
                            }
                            "cancel" => "已取消并结束当前Goal，已有计划和执行事实保留。",
                            _ => "计划审核已暂停，尚未批准执行。请在原聊天明确继续并核对计划。",
                        }
                        .into(),
                    ))
                }
            }),
        })
    })
}
