//! 功能：提供中立子执行等待及实际模型请求的并发资源所有权。
//! 输入：可信入口提供已受理身份、执行阶段、终态结果及取消信号。
//! 输出：AgentLoop取得有界等待结果，模型请求按共享名额取得并释放资源。
//! 边界：不读写会话，不认识Host、Office或Goal，不由模型正文决定可信终态或执行权限。
//! 关联：CHG-2026-0054；AgentLoop接续、runtime子执行及user_input可信等待。

use async_trait::async_trait;
use futures::{future::BoxFuture, stream::BoxStream, FutureExt, StreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, StreamEvent,
    ToolCallOutcome,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;

/// 功能：可信入口报告子执行当前阶段，不等于任务验收。Stability: experimental。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPhase {
    /// 已接收，尚未取得实际执行名额。
    Queued,
    /// 实际执行正在推进。
    Running,
    /// 原问题已可靠保存，正在等待明确回答。
    WaitingInput,
    /// 未取得有效答案，当前分支已经受控暂停。
    Paused,
}
impl ExecutionPhase {
    fn status(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Paused => "paused",
        }
    }
}

/// 功能：保存已经受理的确切子执行身份及其独立结果等待，不表示执行完成。
/// Stability: experimental。
#[derive(Clone)]
pub struct StartedExecution {
    /// 真实子会话身份，由可信执行入口分配。
    pub session_id: String,
    /// 本次子回合身份，不与同会话后续执行混用。
    pub turn_id: String,
    phase: watch::Receiver<ExecutionPhase>,
    result: watch::Receiver<Option<Result<ToolCallOutcome, String>>>,
}

impl StartedExecution {
    /// 功能：绑定已受理执行身份与真实完成等待。
    /// 参数：session_id、turn_id来自入口，phase表示可信执行阶段，result只等待该执行的持久结果。
    /// 返回：尚未等待结果的执行引用。
    /// 错误：无；身份与准入由入口校验。
    /// 状态：不启动执行或模型请求。
    /// 并发与取消：实际执行及其并发名额不由此等待对象拥有。
    /// 权限：不授予其他会话的访问或执行许可。
    /// 关联：Host同步委派的受理与等待分离。Stability: experimental。
    pub fn new(
        session_id: String,
        turn_id: String,
        phase: watch::Receiver<ExecutionPhase>,
        result: BoxFuture<'static, Result<ToolCallOutcome, String>>,
    ) -> Self {
        let (complete, receiver) = watch::channel(None);
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(result)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("子执行结果整理异常".into()));
            complete.send_replace(Some(result));
        });
        Self {
            session_id,
            turn_id,
            phase,
            result: receiver,
        }
    }

    /// 功能：等待同一已受理执行的真实结果。
    /// 参数：self为入口已校验的执行引用。
    /// 返回：真实工具结果。
    /// 错误：执行异常或可靠终态缺失由原入口返回。
    /// 状态：只等待，不重新启动子回合或重置预算。
    /// 并发与取消：丢弃等待不释放实际执行持有的名额，取消由原执行所有者管理。
    /// 权限：不改变既有执行授权。
    /// 关联：StartedExecution::new。Stability: experimental。
    pub async fn wait(mut self) -> Result<ToolCallOutcome, String> {
        loop {
            if let Some(result) = self.result.borrow_and_update().clone() {
                return result;
            }
            self.result
                .changed()
                .await
                .map_err(|_| "子执行结果通知中断".to_string())?;
        }
    }

    fn pending_status(&self) -> &'static str {
        self.phase.borrow().status()
    }

    async fn changed(mut self, observed: ExecutionPhase) {
        if self.result.borrow_and_update().is_some() {
            return;
        }
        if *self.phase.borrow_and_update() != observed {
            return;
        }
        tokio::select! {
            _ = self.result.changed() => {},
            _ = self.phase.changed() => {},
        }
    }
}

/// 功能：当前父回合内可信子执行的查询、事件等待和结果交付去重。
/// 边界：只登记入口已受理引用，不创建会话、写历史或恢复执行许可。
/// Stability: experimental。
#[derive(Default)]
pub struct ExecutionRegistry(Mutex<Vec<TrackedExecution>>);

struct TrackedExecution {
    execution: StartedExecution,
    delivered: bool,
    observed_phase: Option<ExecutionPhase>,
}

impl ExecutionRegistry {
    /// 功能：开始新的父回合时清除上轮内存执行许可。
    /// 参数：无。
    /// 返回：无。
    /// 错误：无。
    /// 状态：旧事实仍由入口的持久记录保存。
    /// 并发与取消：仅在旧回合已经清理后调用，不取消实际执行。
    /// 权限：不恢复旧执行许可。
    /// 关联：父回合准入。Stability: experimental。
    pub fn begin_turn(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// 功能：登记已受理的子执行并返回真实身份。
    /// 参数：execution来自可信受理入口，模型不能构造此引用。
    /// 返回：只含身份和运行状态的启动结果。
    /// 错误：无；受理错误由入口处理。
    /// 状态：登记当前回合的查询许可，启动不计完成。
    /// 并发与取消：实际执行继续拥有名额，不由该表释放。
    /// 权限：仅授予当前父回合访问该确切执行。
    /// 关联：inspect、coordinate。Stability: experimental。
    pub fn register(&self, execution: StartedExecution) -> ToolCallOutcome {
        let outcome = execution_status(&execution, execution.pending_status(), false);
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TrackedExecution {
                execution,
                delivered: false,
                observed_phase: None,
            });
        outcome
    }

    /// 功能：读取或有界等待当前回合有权访问的确切子执行。
    /// 参数：input含session_id、turn_id及可选timeout_ms；wait选择显式等待，cancel沿用父令牌。
    /// 返回：真实结果或超时时的当前状态；已交付结果只返回状态。
    /// 错误：非法参数和非本回合身份返回工具错误。
    /// 状态：不启动回合、不增加预算，真实结果只领取一次。
    /// 并发与取消：重复无变化查询转为事件等待；超时不取消执行。
    /// 权限：查找必须同时匹配入口登记的会话和回合身份。
    /// 关联：start_agent、wait_agent。Stability: experimental。
    pub async fn inspect(
        &self,
        input: serde_json::Value,
        wait: bool,
        cancel: CancellationToken,
    ) -> ToolCallOutcome {
        if let Err(error) = Self::validate(&input) {
            return ToolCallOutcome::error(error);
        }
        let session_id = input["session_id"].as_str().unwrap();
        let turn_id = input["turn_id"].as_str().unwrap();
        let (execution, repeated, phase) = {
            let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = entries.iter_mut().find(|entry| {
                entry.execution.session_id == session_id && entry.execution.turn_id == turn_id
            }) else {
                return ToolCallOutcome::error("执行身份不属于当前父回合，不能查询或等待");
            };
            let phase = *entry.execution.phase.borrow();
            let repeated = entry.observed_phase == Some(phase);
            entry.observed_phase = Some(phase);
            (entry.execution.clone(), repeated, phase)
        };
        if (wait || repeated) && execution.result.borrow().is_none() {
            let duration =
                std::time::Duration::from_millis(input["timeout_ms"].as_u64().unwrap_or(30_000));
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return execution_status(&execution, "cancelled", false),
                _ = tokio::time::timeout(duration, execution.clone().changed(phase)) => {}
            }
        }
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.iter_mut().find(|entry| {
            entry.execution.session_id == session_id && entry.execution.turn_id == turn_id
        }) else {
            return ToolCallOutcome::error("父回合已经结束");
        };
        let result = execution.result.borrow().clone();
        if let Some(result) = result {
            let result = result.unwrap_or_else(ToolCallOutcome::error);
            if entry.delivered {
                let status = result
                    .structured
                    .as_ref()
                    .and_then(|value| value["status"].as_str())
                    .unwrap_or(if result.is_error {
                        "failed"
                    } else {
                        "completed"
                    });
                return execution_status(&execution, status, true);
            }
            entry.delivered = true;
            result
        } else {
            entry.observed_phase = Some(*execution.phase.borrow());
            execution_status(&execution, execution.pending_status(), false)
        }
    }

    /// 功能：严格校验查询与等待参数。
    /// 参数：input为模型提供的JSON对象。
    /// 返回：合法参数返回空值。
    /// 错误：缺失、未知字段或越界期限返回说明。
    /// 状态：不读写执行状态。
    /// 并发与取消：同步无等待。
    /// 权限：格式校验不代替可信归属登记。
    /// 关联：inspect、ToolHostValidator。Stability: experimental。
    pub fn validate(input: &serde_json::Value) -> Result<(), String> {
        let object = input.as_object().ok_or("参数必须是对象")?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "session_id" | "turn_id" | "timeout_ms"))
            || ["session_id", "turn_id"].iter().any(|key| {
                !input[*key]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
            })
            || object.get("timeout_ms").is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|ms| (1000..=60_000).contains(&ms))
            })
        {
            return Err("需要session_id、turn_id；timeout_ms可选，范围1000..60000".into());
        }
        Ok(())
    }

    /// 功能：正常结束前等待活动执行并领取尚未处理的真实结果。
    /// 参数：无；取消由AgentLoop外层沿原令牌处理。
    /// 返回：供原父执行安全接入的新事实，全部已处理时为空。
    /// 错误：子异常作为真实失败结果交付，不伪称成功。
    /// 状态：仅领取，不自行写父历史或唤醒新父回合。
    /// 并发与取消：事件等待不持锁、模型名额或工作区资源。
    /// 权限：只访问本轮登记的执行。
    /// 关联：TurnOptions::end_turn_coordination。Stability: experimental。
    pub async fn coordinate(&self) -> Option<String> {
        loop {
            let pending = {
                let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
                let mut results = Vec::new();
                for entry in entries.iter_mut().filter(|entry| !entry.delivered) {
                    if let Some(result) = entry.execution.result.borrow().clone() {
                        entry.delivered = true;
                        let result = result.unwrap_or_else(ToolCallOutcome::error);
                        results.push(format!(
                            "session_id={} turn_id={}\n{}",
                            entry.execution.session_id, entry.execution.turn_id, result.content
                        ));
                    }
                }
                if !results.is_empty() {
                    return Some(format!(
                        "可信子执行结果（请处理真实结果后再汇总）：\n{}",
                        results.join("\n\n")
                    ));
                }
                entries
                    .iter()
                    .filter(|entry| !entry.delivered)
                    .map(|entry| Box::pin(entry.execution.clone().wait()))
                    .collect::<Vec<_>>()
            };
            if pending.is_empty() {
                return None;
            }
            let _ = futures::future::select_all(pending).await;
        }
    }
}

fn execution_status(
    execution: &StartedExecution,
    status: &str,
    delivered: bool,
) -> ToolCallOutcome {
    let value = serde_json::json!({"session_id":execution.session_id,"turn_id":execution.turn_id,"status":status,"result_delivered":delivered});
    ToolCallOutcome {
        content: value.to_string(),
        structured: Some(value),
        is_error: false,
        media: Vec::new(),
    }
}

/// 功能：提供中立执行状态查询与有界等待工具声明。
/// 参数：无。
/// 返回：仅描述真实执行引用的两项工具。
/// 错误：无。
/// 状态：不启动任务。
/// 并发与取消：允许并行，等待不占执行名额。
/// 权限：声明不授予执行访问，调用仍需可信登记校验。
/// 关联：ExecutionRegistry::inspect。Stability: experimental。
pub fn execution_tool_specs() -> Vec<openpaw_core::ToolSpec> {
    [("get_agent_status","查询本轮确切子执行；重复无变化时等待。"),
     ("wait_agent","有界等待子结果；超时不取消或重启。timeout_ms默认30000，范围1000..60000。")]
    .into_iter().map(|(name,description)|openpaw_core::ToolSpec {
        name:name.into(),description:description.into(),
        input_schema:serde_json::json!({"type":"object","additionalProperties":false,"required":["session_id","turn_id"],"properties":{"session_id":{"type":"string"},"turn_id":{"type":"string"},"timeout_ms":{"type":"integer"}}}),
        category:Some(openpaw_core::ToolCategory::Communication), concurrency:Some(openpaw_core::ToolConcurrency::ParallelSafe),max_result_chars:Some(24_000),..Default::default()
    }).collect()
}

/// 功能：让模型请求与返回的实际流持有同一共享并发名额。
/// 参数：inner为实际provider，slots为可信入口提供的共享信号量。
/// 返回：模型能力透明的provider包装。
/// 错误：构造无错误；额度关闭由请求返回既有provider错误。
/// 状态：不发送请求、不改变模型声明。
/// 并发与取消：complete覆盖实际请求，stream覆盖整个返回流；丢弃释放名额。
/// 权限：只限制并发，不增加模型或工具权限。
/// 关联：父子执行共用模型额度。Stability: experimental。
pub fn limit_provider(inner: Arc<dyn LlmProvider>, slots: Arc<Semaphore>) -> Arc<dyn LlmProvider> {
    Arc::new(LimitedProvider { inner, slots })
}

struct LimitedProvider {
    inner: Arc<dyn LlmProvider>,
    slots: Arc<Semaphore>,
}

#[async_trait]
impl LlmProvider for LimitedProvider {
    async fn complete(&self, req: CompletionRequest) -> openpaw_core::Result<CompletionResponse> {
        let _permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Provider("模型请求调度器已关闭".into()))?;
        self.inner.complete(req).await
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> openpaw_core::Result<BoxStream<'static, openpaw_core::Result<StreamEvent>>> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Provider("模型请求调度器已关闭".into()))?;
        let stream = self.inner.stream(req).await?;
        Ok(Box::pin(futures::stream::unfold(
            (stream, permit),
            |(mut stream, permit)| async move {
                stream.next().await.map(|event| (event, (stream, permit)))
            },
        )))
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn model_capabilities(&self, model: &str) -> Capabilities {
        self.inner.model_capabilities(model)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }
}
