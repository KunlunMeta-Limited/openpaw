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
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, Weak};
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

/// 功能：按可信主会话串行化交互展示，不处理答案或工具权限。
/// 字段/变体语义：lanes仅持有展示名额，pending仅保存可靠当前事实。
/// 数据来源：可信归属、唯一记录入口和时钟效果。
/// 数据去向：原问题调用、已有审批展示及有界观察恢复。
/// 约束：只协调长展示等待，不持有模型或工作区资源。
/// 稳定性：experimental。
/// 关联：CHG-2026-0054。
pub struct QuestionQueue {
    lanes: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    clock: Arc<dyn QuestionClock>,
    pending: Mutex<HashMap<String, UserQuestionRecord>>,
}
/// 功能：提供可信当前时间和原截止点等待，模型不能设置时钟。
/// 字段/变体语义：当前毫秒和截止等待必须使用同一时间源。
/// 数据来源：生产时钟或可信本地fixture。
/// 数据去向：问题原期限及有界观察恢复。
/// 约束：只影响问答期限，不增加执行预算。
/// 稳定性：experimental。
/// 关联：CHG-2026-0054。
#[async_trait]
pub trait QuestionClock: Send + Sync {
    /// 功能：读取当前Unix毫秒。
    /// 参数：无。
    /// 返回：可信毫秒值。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：不得阻塞。
    /// 权限：仅可信入口提供。
    /// 关联：问题原deadline；Stability: experimental。
    fn now_ms(&self) -> i64;
    /// 功能：等待到原截止点。
    /// 参数：deadline_ms为原Unix毫秒期限。
    /// 返回：期限已到。
    /// 错误：无。
    /// 状态：不改变问题或执行状态。
    /// 并发与取消：future可丢弃，时钟推进唤醒。
    /// 权限：不新建期限或执行许可。
    /// 关联：可控时间fixture；Stability: experimental。
    async fn wait_until(&self, deadline_ms: i64);
}
struct WallClock;
#[async_trait]
impl QuestionClock for WallClock {
    fn now_ms(&self) -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX)
    }
    async fn wait_until(&self, deadline_ms: i64) {
        let remaining = deadline_ms.saturating_sub(self.now_ms()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(remaining)).await;
    }
}
impl Default for QuestionQueue {
    fn default() -> Self {
        Self::new(Arc::new(WallClock))
    }
}
/// 功能：持有一次展示名额，丢弃时允许该主会话的后题晋升。
/// 字段/变体语义：内部guard只表示展示位置。
/// 数据来源：按可信主会话取得的公平名额。
/// 数据去向：问题或审批的当前展示等待。
/// 约束：不表示执行或工具授权。
/// 稳定性：experimental。
/// 关联：QuestionQueue。
pub struct PresentationPermit {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}
impl QuestionQueue {
    /// 功能：以可信时钟构造交互展示协调器。
    /// 参数：clock为当前时间和截止点等待效果。
    /// 返回：空协调器。
    /// 错误：无。
    /// 状态：不恢复展示或执行。
    /// 并发与取消：各主会话独立取得名额。
    /// 权限：不向模型开放配置。
    /// 关联：Host装配及本地fixture；Stability: experimental。
    pub fn new(clock: Arc<dyn QuestionClock>) -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            clock,
            pending: Mutex::new(HashMap::new()),
        }
    }
    /// 功能：读取同一可信时钟的当前毫秒。
    /// 参数：无。
    /// 返回：当前Unix毫秒。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：无等待。
    /// 权限：不创建期限。
    /// 关联：原deadline与重显剩余时间；Stability: experimental。
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }
    /// 功能：等待原问题截止点，不重建时间窗口。
    /// 参数：deadline_ms为可靠记录中的原截止点。
    /// 返回：期限已到。
    /// 错误：无。
    /// 状态：不改变执行或问题。
    /// 并发与取消：丢弃future取消等待。
    /// 权限：不延长预算。
    /// 关联：问答及有界观察恢复；Stability: experimental。
    pub async fn wait_until(&self, deadline_ms: i64) {
        self.clock.wait_until(deadline_ms).await;
    }
    /// 功能：读取指定主会话已可靠记录的当前待答事实。
    /// 参数：parent为实际主会话身份，入口还须核对当前父turn和实际子执行。
    /// 返回：Queued或Waiting的完整副本，含原期限与展示代次。
    /// 错误：无。
    /// 状态：只读，不包含已终结问题。
    /// 并发与取消：短锁复制，不访问模型历史。
    /// 权限：事实不授予执行或回答权限。
    /// 关联：观察恢复及原问题重显；Stability: experimental。
    pub fn pending(&self, parent: &str) -> Vec<UserQuestionRecord> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|record| record.scope.parent_session_id == parent)
            .cloned()
            .collect()
    }
    /// 功能：发布入口已经可靠保存的当前问题事实。
    /// 参数：record为原执行核验并提交后的记录。
    /// 返回：无。
    /// 错误：无；保存失败时不得调用。
    /// 状态：仅维护当前待答副本，终态移除。
    /// 并发与取消：短锁，不写持久记录。
    /// 权限：只有可信记录入口使用，不接受模型声明。
    /// 关联：唯一writer与展示代次；Stability: experimental。
    pub fn record_saved(&self, record: &UserQuestionRecord) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            record.state,
            UserQuestionState::Queued | UserQuestionState::Waiting
        ) {
            pending.insert(record.question_id.clone(), record.clone());
        } else {
            pending.remove(&record.question_id);
        }
    }
    /// 功能：按到达顺序等待指定主会话的展示名额。
    /// 参数：parent为入口核验的实际主会话身份。
    /// 返回：需持有至本题可靠结算的名额。
    /// 错误：无。
    /// 状态：不展示、不保存问题、不授予权限。
    /// 并发与取消：公平异步等待；丢弃future退出排队。
    /// 权限：调用方只传可信归属。
    /// 关联：问答与既有权限展示；Stability: experimental。
    pub async fn acquire(&self, parent: &str) -> PresentationPermit {
        let lane = {
            let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
            lanes.retain(|_, lane| lane.strong_count() > 0);
            lanes
                .get(parent)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let lane = Arc::new(tokio::sync::Mutex::new(()));
                    lanes.insert(parent.into(), Arc::downgrade(&lane));
                    lane
                })
        };
        PresentationPermit {
            _guard: lane.lock_owned().await,
        }
    }
}

/// 功能：持有已装配的可信问答入口及本次回合暂停事实。
#[derive(Default)]
pub struct QuestionContext {
    runtime: Mutex<Option<Arc<dyn QuestionRuntime>>>,
    paused: Mutex<Option<String>>,
    queue: Arc<QuestionQueue>,
}

impl QuestionContext {
    /// 功能：装配根与子会话共享的展示协调器。
    /// 参数：queue为可信入口共享的协调器。
    /// 返回：拥有独立暂停事实的问答上下文。
    /// 错误：无。
    /// 状态：不开始提问。
    /// 并发与取消：共享队列但不共享回合暂停。
    /// 权限：不继承其他会话答案或权限。
    /// 关联：Host会话装配；Stability: experimental。
    pub fn new(queue: Arc<QuestionQueue>) -> Self {
        Self {
            queue,
            ..Self::default()
        }
    }
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
        self.queue.record_saved(&record);
        let mut guard = PendingQuestion {
            runtime: runtime.clone(),
            record: record.clone(),
            settled: false,
            _slot: None,
            queue: self.queue.clone(),
        };
        guard._slot = Some(self.queue.acquire(&record.scope.parent_session_id).await);
        let deadline = self.queue.now_ms().saturating_add(300_000);
        record.deadline_ms = Some(deadline);
        record.state = UserQuestionState::Waiting;
        if guard.save(&record).is_err() {
            return self.pause("问题保存失败，尚未取得有效答案".into());
        }
        let answer = tokio::select! {
            biased;
            _ = self.queue.wait_until(deadline) => Err("问题已超时，请在原聊天明确继续".into()),
            result = runtime.display(&record) => result.and_then(|answer|validate_answer(&record.request,answer)),
        };
        let answer = if self.queue.now_ms() >= deadline {
            Err("问题已超时，请在原聊天明确继续".into())
        } else {
            answer
        };
        if let Some(current) = self
            .queue
            .pending(&record.scope.parent_session_id)
            .into_iter()
            .find(|current| current.question_id == record.question_id)
        {
            record = current;
        }
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
        if guard.save(&record).is_err() {
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
    _slot: Option<PresentationPermit>,
    queue: Arc<QuestionQueue>,
}
impl PendingQuestion {
    fn save(&mut self, record: &UserQuestionRecord) -> std::result::Result<(), String> {
        self.runtime.record(record)?;
        self.queue.record_saved(record);
        self.record = record.clone();
        Ok(())
    }
}
impl Drop for PendingQuestion {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(current) = self
                .queue
                .pending(&self.record.scope.parent_session_id)
                .into_iter()
                .find(|current| current.question_id == self.record.question_id)
            {
                self.record = current;
            }
            self.record.state = UserQuestionState::Cancelled;
            self.record.reason = Some("原执行已取消或中断，未接受答案".into());
            let _ = self.runtime.record(&self.record);
            self.queue.record_saved(&self.record);
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
