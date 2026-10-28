//! 功能：校验单题、关联原调用并在可信入口保存后交回答案。
//! 边界：展示及持久化由入口注入，不访问Office、会话文件或网络。
//! 关联：CHG-2026-0054；共享队列及重显沿用同一逻辑问题。
use async_trait::async_trait;
use openpaw_core::user_input::*;
use openpaw_core::{
    Result, ToolCallOutcome, ToolExecutor, ToolHost, ToolHostValidator, ToolInvocationContext,
    ToolSpec,
};
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 功能：可信入口提供的问题归属、可靠保存和展示效果。
#[async_trait]
pub trait QuestionRuntime: Send + Sync {
    /// 功能：核对真实回合并确定问题归属。
    /// 参数：invocation为引擎提供的原调用。
    /// 返回：不可由模型指定的会话与来源。
    /// 错误：执行失效或归属未知时拒绝。
    /// 状态：只读。
    /// 并发与取消：同步核对当前执行。
    /// 权限：不授予工具权限。
    /// 关联：ToolInvocationContext。
    fn scope(
        &self,
        invocation: &ToolInvocationContext,
    ) -> std::result::Result<UserQuestionScope, String>;
    /// 功能：通过入口唯一writer可靠保存问题事实。
    /// 参数：record为完整中立问题记录。
    /// 返回：成功表示事实已经提交。
    /// 错误：存储失败或执行已经取消时拒绝接受答案。
    /// 状态：追加记录，不改模型历史。
    /// 并发与取消：入口与停止线性化，短同步事务。
    /// 权限：只保存本次真实归属。
    /// 关联：user_question_v1。
    fn record(&self, record: &UserQuestionRecord) -> std::result::Result<(), String>;
    /// 功能：展示一题并取得用户明确提交的答案。
    /// 参数：record包含展示身份和原期限。
    /// 返回：原始结构化回答，仍须协调器校验。
    /// 错误：展示失败或未答时返回脱敏原因。
    /// 状态：不自行接受或保存答案。
    /// 并发与取消：future丢弃关闭展示等待。
    /// 权限：不把问题回答视为工具审批。
    /// 关联：既有交互入口的薄适配。
    async fn display(
        &self,
        record: &UserQuestionRecord,
    ) -> std::result::Result<UserQuestionAnswer, String>;
}

/// 功能：只接收可信问答协调器的回合暂停原因。
pub type PauseCheck = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// 功能：持有已装配的可信问答入口及本次回合暂停事实。
#[derive(Default)]
pub struct QuestionContext {
    runtime: Mutex<Option<Arc<dyn QuestionRuntime>>>,
    paused: Mutex<Option<String>>,
}

impl QuestionContext {
    /// 功能：绑定实际入口效果，不启动提问。
    /// 参数：runtime为具有真实执行所有权的适配器。
    /// 返回：无。
    /// 错误：无。
    /// 状态：替换弱会话适配入口。
    /// 并发与取消：短同步锁。
    /// 权限：仅入口组装使用。
    /// 关联：Host会话建立。
    pub fn attach(&self, runtime: Arc<dyn QuestionRuntime>) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(runtime);
    }
    /// 功能：清理已结束回合的临时暂停标志。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：不删除持久问题或答案。
    /// 并发与取消：入口在接受新执行后调用。
    /// 权限：不创建执行许可或预算。
    /// 关联：TurnOptions。
    pub fn begin(&self) {
        *self.paused.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    /// 功能：取得本回合可信暂停事实。
    /// 参数：无。
    /// 返回：无有效答案时的暂停原因。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：短同步锁。
    /// 权限：工具正文不能设置此事实。
    /// 关联：AgentLoop批次结算。
    pub fn pause_reason(&self) -> Option<String> {
        self.paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn pause(&self, reason: String) -> ToolCallOutcome {
        *self.paused.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
        ToolCallOutcome::error(reason)
    }
    async fn ask(&self, args: Value, invocation: &ToolInvocationContext) -> ToolCallOutcome {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(runtime) = runtime else {
            return self.pause("用户提问入口尚未就绪".into());
        };
        let request = match request(args) {
            Ok(request) => request,
            Err(reason) => return self.pause(reason),
        };
        let scope = match runtime.scope(invocation) {
            Ok(scope) => scope,
            Err(reason) => return self.pause(reason),
        };
        let Some(turn_id) = invocation.turn_id.clone().filter(|id| !id.is_empty()) else {
            return self.pause("提问缺少真实执行身份".into());
        };
        if invocation.tool_call_id.is_empty() {
            return self.pause("提问缺少原调用身份".into());
        }
        let question_id = format!(
            "{}/{}/{}/{}",
            scope.asking_session_id, turn_id, invocation.response_index, invocation.tool_call_id
        );
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        question_id.hash(&mut hash);
        let mut record = UserQuestionRecord {
            question_id,
            scope,
            turn_id,
            response_index: invocation.response_index,
            tool_call_id: invocation.tool_call_id.clone(),
            request,
            state: UserQuestionState::Queued,
            generation: 1,
            display_id: format!("uq-{:016x}-1", hash.finish()),
            deadline_ms: None,
            answer: None,
            reason: None,
        };
        if runtime.record(&record).is_err() {
            return self.pause("问题保存失败，尚未取得有效答案".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        record.deadline_ms = Some(
            i64::try_from(now)
                .unwrap_or(i64::MAX)
                .saturating_add(300_000),
        );
        record.state = UserQuestionState::Waiting;
        if runtime.record(&record).is_err() {
            return self.pause("问题保存失败，尚未取得有效答案".into());
        }
        let mut guard = PendingQuestion {
            runtime: runtime.clone(),
            record: record.clone(),
            settled: false,
        };
        let answer = match tokio::time::timeout(
            std::time::Duration::from_secs(300),
            runtime.display(&record),
        )
        .await
        {
            Ok(Ok(answer)) => validate_answer(&record.request, answer),
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err("问题已超时，请在原聊天明确继续".into()),
        };
        match answer {
            Ok(answer) => {
                record.state = UserQuestionState::Answered;
                record.answer = Some(answer);
            }
            Err(reason) => {
                record.state = if reason.contains("超时") {
                    UserQuestionState::Expired
                } else {
                    UserQuestionState::Interrupted
                };
                record.reason = Some(reason);
            }
        }
        if runtime.record(&record).is_err() {
            return self.pause("答案尚未可靠保存，执行已暂停".into());
        }
        guard.settled = true;
        if let Some(reason) = record.reason {
            return self.pause(reason);
        }
        ToolCallOutcome::ok(json!({"question_id":record.question_id,"tool_call_id":record.tool_call_id,"scope":record.scope,"answer":record.answer}).to_string())
    }
}

struct PendingQuestion {
    runtime: Arc<dyn QuestionRuntime>,
    record: UserQuestionRecord,
    settled: bool,
}
impl Drop for PendingQuestion {
    fn drop(&mut self) {
        if !self.settled {
            self.record.state = UserQuestionState::Cancelled;
            self.record.reason = Some("原执行已取消或中断，未接受答案".into());
            let _ = self.runtime.record(&self.record);
        }
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.encode_utf16().count() <= max
}
fn request(args: Value) -> std::result::Result<UserQuestionRequest, String> {
    let request: UserQuestionRequest =
        serde_json::from_value(args).map_err(|_| "问题参数格式无效")?;
    if !text(&request.question, 160)
        || request.description.encode_utf16().count() > 2000
        || !(2..=8).contains(&request.options.len())
    {
        return Err("问题或选项数量无效".into());
    }
    let mut ids = std::collections::HashSet::new();
    for option in &request.options {
        if option.id == "custom"
            || option.id.is_empty()
            || option.id.len() > 100
            || !option
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
            || !ids.insert(&option.id)
            || !text(&option.label, 160)
            || option.description.encode_utf16().count() > 500
        {
            return Err("问题选项身份或文字无效".into());
        }
    }
    Ok(request)
}
fn validate_answer(
    request: &UserQuestionRequest,
    mut answer: UserQuestionAnswer,
) -> std::result::Result<UserQuestionAnswer, String> {
    let valid = if answer.choice_id == "custom" {
        answer.custom_text = answer.custom_text.map(|text| text.trim().to_owned());
        answer.label == "其他"
            && answer
                .custom_text
                .as_deref()
                .is_some_and(|value| text(value, 1000))
    } else {
        answer.custom_text.is_none()
            && request
                .options
                .iter()
                .any(|option| option.id == answer.choice_id && option.label == answer.label)
    };
    if valid {
        Ok(answer)
    } else {
        Err("未取得合法的明确答案，执行已暂停".into())
    }
}

struct QuestionToolHost {
    inner: Arc<dyn ToolHost>,
    context: Arc<QuestionContext>,
}
/// 功能：把真实提问接入原工具链，保留其他工具的权限与身份。
/// 参数：inner为原宿主，context为入口装配的可信协调器。
/// 返回：共享工具宿主。
/// 错误：调用时通过原结果返回，未答另设可信暂停事实。
/// 状态：不立即展示或写盘。
/// 并发与取消：future丢弃关闭原等待。
/// 权限：普通回答不批准工具或Goal。
/// 关联：Host根及子会话装配。
pub fn wrap_tools(inner: Arc<dyn ToolHost>, context: Arc<QuestionContext>) -> Arc<dyn ToolHost> {
    Arc::new(QuestionToolHost { inner, context })
}
#[async_trait]
impl ToolExecutor for QuestionToolHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let mut tools = self.inner.list_tools().await?;
        tools.retain(|tool| tool.name != "clarify");
        let mut spec = ToolSpec::new("request_user_input","需要用户决定或补齐关键条件时提问一个问题，等待明确回答后继续；已有答案不要重复提问。回答不代表工具授权或Goal计划批准。");
        spec.input_schema = json!({"type":"object","required":["question","options"],"properties":{"question":{"type":"string","minLength":1,"maxLength":160},"description":{"type":"string","maxLength":2000},"options":{"type":"array","minItems":2,"maxItems":8,"items":{"type":"object","required":["id","label"],"properties":{"id":{"type":"string"},"label":{"type":"string","maxLength":160},"description":{"type":"string","maxLength":500}},"additionalProperties":false}}},"additionalProperties":false});
        spec.category = Some(openpaw_core::ToolCategory::ReadOnly);
        spec.concurrency = Some(openpaw_core::ToolConcurrency::NeverParallel);
        tools.push(spec);
        Ok(tools)
    }
    async fn call(&self, name: &str, args: Value) -> Result<ToolCallOutcome> {
        self.call_with_context(name, args, &ToolInvocationContext::default())
            .await
    }
    async fn call_with_context(
        &self,
        name: &str,
        args: Value,
        context: &ToolInvocationContext,
    ) -> Result<ToolCallOutcome> {
        if name == "request_user_input" {
            Ok(self.context.ask(args, context).await)
        } else {
            self.inner.call_with_context(name, args, context).await
        }
    }
    fn set_media_context(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.set_media_context(media);
    }
    fn remember_media(&self, media: Vec<openpaw_core::InlineMedia>) {
        self.inner.remember_media(media);
    }
}
#[async_trait]
impl ToolHostValidator for QuestionToolHost {
    async fn validate_input(&self, name: &str, args: &Value) -> std::result::Result<(), String> {
        if name == "request_user_input" {
            Ok(())
        } else {
            self.inner.validate_input(name, args).await
        }
    }
    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        self.inner.should_cascade_on_error(name, outcome)
    }
}
