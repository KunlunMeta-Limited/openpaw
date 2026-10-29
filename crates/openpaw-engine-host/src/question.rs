//! 功能：把中立问题映射到既有Office问答协议，可靠记录交由runtime。
//! 边界：不创建writer、路由、客户端或新网络目的地，不处理工具审批。
//! 关联：CHG-2026-0054。
use crate::{office::MainOffice, runtime::SessionHandle};
use openpaw_core::{user_input::*, ToolInvocationContext};
use openpaw_engine::user_input::QuestionRuntime;
use serde_json::json;
use std::sync::{Arc, Weak};
use std::time::Duration;

struct Adapter {
    handle: Weak<SessionHandle>,
    office: Arc<MainOffice>,
}
/// 功能：创建只持有弱会话引用的问答入口。
/// 参数：handle为实际会话，office为既有固定出站通道。
/// 返回：中立协调器可调用的薄适配器。
/// 错误：调用时归属或通道失效返回安全错误。
/// 状态：不发送请求或开启writer。
/// 并发与取消：不形成会话引用环。
/// 权限：不接收模型指定的地址或身份。
/// 关联：runtime会话装配。
pub(crate) fn adapter(
    handle: &Arc<SessionHandle>,
    office: Arc<MainOffice>,
) -> Arc<dyn QuestionRuntime> {
    Arc::new(Adapter {
        handle: Arc::downgrade(handle),
        office,
    })
}

#[async_trait::async_trait]
impl QuestionRuntime for Adapter {
    fn scope(&self, invocation: &ToolInvocationContext) -> Result<UserQuestionScope, String> {
        self.handle
            .upgrade()
            .ok_or("提问会话已经结束")?
            .question_scope(invocation)
    }
    fn record(&self, record: &UserQuestionRecord) -> Result<(), String> {
        self.handle
            .upgrade()
            .ok_or("提问会话已经结束")?
            .commit_question(record)
    }
    async fn display(&self, record: &UserQuestionRecord) -> Result<UserQuestionAnswer, String> {
        let handle = self.handle.upgrade().ok_or("提问会话已经结束")?;
        let binding = self
            .office
            .directory()?
            .into_iter()
            .find_map(|snapshot| {
                let team = snapshot["workspace"]["id"].as_str()?;
                self.office
                    .session_record(team, &record.scope.parent_session_id)
                    .map(|binding| (team.to_owned(), binding))
            })
            .ok_or("问题没有有效的主会话绑定")?;
        let (team, binding) = binding;
        let conversation = binding.conversation.ok_or("问题没有有效的对话绑定")?;
        let employee = binding.snapshot["employees"]
            .as_array()
            .and_then(|employees| {
                employees
                    .iter()
                    .find(|employee| employee["id"] == handle.question_employee_id())
            });
        let actor = employee
            .and_then(|employee| employee["sceneActorId"].as_str())
            .unwrap_or("");
        let mut decision = json!({"id":record.display_id,"kind":"choice","actorId":actor,"title":record.request.question,"description":record.request.description,"options":record.request.options,"allowCustom":true});
        if let Some(name) = employee.and_then(|employee| employee["name"].as_str()) {
            let description = format!("{name}：{}", record.request.description);
            let mut units = 0;
            decision["description"] = json!(description
                .chars()
                .take_while(|ch| {
                    units += ch.len_utf16();
                    units <= 2000
                })
                .collect::<String>());
        }
        let deadline = record.deadline_ms.ok_or("问题没有原截止时间")?;
        let prefix = record
            .display_id
            .rsplit_once('-')
            .ok_or("问题展示身份无效")?
            .0;
        let mut current = record.clone();
        loop {
            let now = handle.question_queue.now_ms();
            let remaining = deadline.saturating_sub(now);
            let retry_at = now.saturating_add(1000);
            if remaining < 1000 {
                return Err("问题已超时，请在原聊天明确继续".into());
            }
            decision["id"] = json!(current.display_id);
            let request = json!({"type":"decision.request","teamId":team,"conversationId":conversation,"timeoutMs":remaining.min(300000),"decision":decision});
            // 先轮询原回答；页面读到缺失与有效回答同时可用时，不丢弃已知回答。
            let response = {
                let response = self
                    .office
                    .message(request, Duration::from_millis(remaining as u64 + 5000));
                tokio::pin!(response);
                tokio::select! { biased;
                    result = &mut response => Some(result),
                    _ = self.missing(&handle, &team, &current.display_id) => None,
                }
            }; // 重显前丢弃旧POST；它的迟到响应不能交给新代。
            if let Some(response) = response {
                match response {
                    Ok((status, data))
                        if status == 409 && data["reasonCode"] == "NO_RECEIVER"
                            || status == 503
                                && matches!(
                                    data["reasonCode"].as_str(),
                                    Some("RECEIVER_UNAVAILABLE" | "SERVER_CLOSING")
                                ) => {}
                    Ok((status, data)) => {
                        return parse_answer(
                            status,
                            data,
                            &team,
                            &conversation,
                            &current.display_id,
                        )
                    }
                    Err(reason)
                        if matches!(reason.as_str(), "main 请求结果不明" | "main 响应中断") =>
                        {}
                    Err(reason) => return Err(reason),
                }
                handle.question_queue.wait_until(retry_at).await;
            }
            if deadline.saturating_sub(handle.question_queue.now_ms()) < 1000 {
                return Err("问题已超时，请在原聊天明确继续".into());
            }
            current.generation = current
                .generation
                .checked_add(1)
                .ok_or("问题展示代次耗尽")?;
            current.display_id = format!("{prefix}-{}", current.generation);
            // 同逻辑题保留原名额及deadline；唯一writer成功后才允许新展示。
            handle.commit_question(&current)?;
        }
    }
}

impl Adapter {
    async fn missing(&self, handle: &SessionHandle, team: &str, display: &str) {
        loop {
            handle
                .question_queue
                .wait_until(handle.question_queue.now_ms().saturating_add(1000))
                .await;
            let observed = self
                .office
                .message(
                    json!({"type":"office.read","teamId":team}),
                    Duration::from_secs(2),
                )
                .await;
            if let Ok((200, data)) = observed {
                if data["pendingDecisions"]
                    .as_array()
                    .is_some_and(|pending| !pending.iter().any(|id| id == display))
                {
                    return;
                }
            }
        }
    }
}
fn parse_answer(
    status: u16,
    data: serde_json::Value,
    team: &str,
    conversation: &str,
    display: &str,
) -> Result<UserQuestionAnswer, String> {
    if status == 408 {
        return Err("问题已超时，请在原聊天明确继续".into());
    }
    if status != 200
        || data["teamId"] != team
        || data["conversationId"] != conversation
        || data["decisionId"] != display
    {
        return Err("问题回答归属无效或展示失败".into());
    }
    Ok(UserQuestionAnswer {
        choice_id: data["choiceId"].as_str().ok_or("答案缺少选项身份")?.into(),
        label: data["label"].as_str().ok_or("答案缺少标签")?.into(),
        custom_text: data
            .get("customText")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("自定义答案格式无效")
            })
            .transpose()?,
    })
}
