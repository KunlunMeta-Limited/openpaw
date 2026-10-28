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
                    .find(|employee| employee["id"] == record.scope.asking_session_id)
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
        let request = json!({"type":"decision.request","teamId":team,"conversationId":conversation,"timeoutMs":300000,"decision":decision});
        let (status, data) = self
            .office
            .message(request, Duration::from_secs(305))
            .await?;
        if status == 408 {
            return Err("问题已超时，请在原聊天明确继续".into());
        }
        if status != 200
            || data["teamId"] != team
            || data["conversationId"] != conversation
            || data["decisionId"] != record.display_id
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
}
