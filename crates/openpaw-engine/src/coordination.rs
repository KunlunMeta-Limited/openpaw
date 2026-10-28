//! 功能：提供中立子执行等待及实际模型请求的并发资源所有权。
//! 边界：入口提供已受理身份与执行结果；不读写会话，不认识Host、Office或Goal。

use async_trait::async_trait;
use futures::{future::BoxFuture, stream::BoxStream, StreamExt};
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, StreamEvent,
    ToolCallOutcome,
};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 功能：保存已经受理的确切子执行身份及其独立结果等待，不表示执行完成。
/// Stability: experimental。
pub struct StartedExecution {
    /// 真实子会话身份，由可信执行入口分配。
    pub session_id: String,
    /// 本次子回合身份，不与同会话后续执行混用。
    pub turn_id: String,
    result: BoxFuture<'static, Result<ToolCallOutcome, String>>,
}

impl StartedExecution {
    /// 功能：绑定已受理执行身份与真实完成等待。
    /// 参数：session_id、turn_id来自入口，result只等待该执行的持久结果。
    /// 返回：尚未等待结果的执行引用。
    /// 错误：无；身份与准入由入口校验。
    /// 状态：不启动执行或模型请求。
    /// 并发与取消：实际执行及其并发名额不由此等待对象拥有。
    /// 权限：不授予其他会话的访问或执行许可。
    /// 关联：Host同步委派的受理与等待分离。Stability: experimental。
    pub fn new(
        session_id: String,
        turn_id: String,
        result: BoxFuture<'static, Result<ToolCallOutcome, String>>,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            result,
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
    pub async fn wait(self) -> Result<ToolCallOutcome, String> {
        self.result.await
    }
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
