//! 职责：包装原ToolRegistry的授权、资源协调及可靠执行事实。
//! 输入输出：原引擎最终调用到原工具结果。
//! 依赖：原注册表、Environment、真实开始观察器和Host Store。
//! 禁止职责：不复制工具主体，不在资源清理前释放锁。
use crate::{
    approval::{HostApproval, RunControl},
    resource::Root,
    store::Store,
    HostError,
};
use async_trait::async_trait;
use openpaw_core::{
    Error, Result, ToolCallBinding, ToolCallContext, ToolCallOutcome, ToolExecutionObserver,
    ToolExecutor, ToolHostValidator, ToolSpec,
};
use std::sync::Arc;
/// 用途：原ToolRegistry的授权及资源包装。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct HostTools {
    /// 用途：复用原ToolRegistry，执行前后仅包裹宿主责任。
    /// 字段：openpaw_tools::ToolRegistry，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) registry: openpaw_tools::ToolRegistry,
    /// 用途：显式工具白名单，写工具仍需每次审批。
    /// 字段：Vec<String>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) allowed: Vec<String>,
    /// 用途：本Run固定的规范工作根与协作锁。
    /// 字段：Arc<Root>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) root: Arc<Root>,
    /// 用途：同一Run共享的原ApprovalManager实现，精确一次消费。
    /// 字段：Arc<HostApproval>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) approval: Arc<HostApproval>,
    /// 用途：本Run独立取消及故障锁存上下文。
    /// 字段：RunControl，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) control: RunControl,
    /// 用途：Host单写存储命令入口，调用方等待写入完成。
    /// 字段：Store，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) store: Store,
}
struct Started {
    store: Store,
    control: RunControl,
}
#[async_trait]
impl ToolExecutionObserver for Started {
    async fn started(&self, binding: &ToolCallBinding) -> Result<()> {
        let binding = binding.clone();
        if let Err(e) = self
            .store
            .call(move |db| {
                let rev = db.bump(
                    "tool_started",
                    &serde_json::to_string(&binding).map_err(HostError::storage)?,
                )?;
                let _ = db
                    .notices
                    .send(crate::HostNotice::Changed { revision: rev });
                Ok(())
            })
            .await
        {
            self.control.fail(e);
            return Err(Error::Store("无法保存真实工具开始".into()));
        }
        Ok(())
    }
}
fn writing(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "notebook_edit" | "todo_add" | "todo_update" | "bash"
    )
}
#[async_trait]
impl ToolExecutor for HostTools {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(self
            .registry
            .list_tools()
            .await?
            .into_iter()
            .filter(|s| self.allowed.contains(&s.name))
            .collect())
    }
    async fn call(&self, _: &str, _: serde_json::Value) -> Result<ToolCallOutcome> {
        Err(Error::ToolHost("Host工具必须携带Run上下文".into()))
    }
    async fn call_with_context(
        &self,
        name: &str,
        args: serde_json::Value,
        context: ToolCallContext,
    ) -> Result<ToolCallOutcome> {
        self.validate_input(name, &args)
            .await
            .map_err(Error::ToolHost)?;
        if context.binding().run_id != self.control.run_id
            || context.binding().tool_name != name
            || context.binding().input != args
        {
            return Err(Error::ToolHost("工具绑定不匹配".into()));
        }
        let write = writing(name);
        if write {
            if !context.is_approved_for(name, &args) {
                return Err(Error::ToolHost("工具缺少精确批准".into()));
            }
            self.approval
                .consume(context.binding().clone())
                .await
                .map_err(|e| Error::ToolHost(e.to_string()))?;
        }
        let _guard = self
            .root
            .acquire(write, &context.cancel())
            .await
            .map_err(|e| {
                if e.kind == "cancelled" {
                    Error::Cancelled
                } else {
                    Error::ToolHost(e.to_string())
                }
            })?;
        self.validate_input(name, &args)
            .await
            .map_err(Error::ToolHost)?;
        if write {
            self.approval
                .revalidate(context.binding())
                .await
                .map_err(|e| Error::ToolHost(e.to_string()))?;
        }
        let binding = context.binding().clone();
        let context = context.with_observer(Arc::new(Started {
            store: self.store.clone(),
            control: self.control.clone(),
        }));
        // 必须等待原调用返回；context取消不能通过drop绕过进程树、管道及原子写入清理。
        let result = self.registry.call_with_context(name, args, context).await;
        let success = result.as_ref().is_ok_and(|r| !r.is_error);
        if let Err(e)=self.store.call(move|db|{let rev=db.bump("tool_result",&serde_json::json!({"run_id":binding.run_id,"call_id":binding.call_id,"success":success}).to_string())?;let _=db.notices.send(crate::HostNotice::Changed{revision:rev});Ok(())}).await {self.control.fail(e);return Err(Error::Store("工具结果保存失败".into()));}
        result
    }
}
#[async_trait]
impl ToolHostValidator for HostTools {
    async fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        if !self.allowed.iter().any(|s| s == name) {
            return Err("工具未在Host授权清单".into());
        }
        self.registry.validate_input(name, input).await
    }
}
