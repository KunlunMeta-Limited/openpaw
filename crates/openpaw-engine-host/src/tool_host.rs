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
    /// 用途：共享Host句柄，执行仍经原运行注册表。
    /// 字段：crate::Host，共享Host句柄，执行仍经原运行注册表。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) host: crate::Host,
    /// 用途：可信角色及主子协作上下文。
    /// 字段：Option<crate::orchestration::Execution>，可信角色及主子协作上下文。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) execution: Option<crate::orchestration::Execution>,
    /// 用途：协作主角色普通工具执行预算，委派使用父期限。
    /// 字段：Option<std::time::Duration>，协作主角色普通工具执行预算，委派使用父期限。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) ordinary_timeout: Option<std::time::Duration>,
    /// 用途：仅当前Office绑定运行暴露进度元数据工具。
    /// 字段：bool，非权限凭据。
    /// 来源去向：可信Session绑定到原工具装配。
    /// 约束：无Office运行和CLI不注入，不放开真实写工具。
    /// 稳定性：T5内部字段。
    /// 关联：需求/设计REQ-OP-012；DES-OP-009。
    pub(crate) progress_enabled: bool,
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
            .call_priority(move |db| {
                let run = db.get_run(&binding.run_id)?;
                db.binding(&run.session_id)?; // 最后一道开始门禁与删除意图按Store序列排序。
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
        let mut specs: Vec<_> = self
            .registry
            .list_tools()
            .await?
            .into_iter()
            .filter(|s| self.allowed.contains(&s.name))
            .collect();
        if self.execution.as_ref().is_some_and(|e| e.role == "lead") {
            specs.push(crate::orchestration::spec());
        }
        if self.progress_enabled {
            specs.push(crate::task_progress::spec());
        }
        Ok(specs)
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
        let cancel = context.cancel();
        let call = self.invoke(name, args, context);
        tokio::pin!(call);
        if let Some(timeout) = self.ordinary_timeout.filter(|_| name != "delegate_task") {
            tokio::select! { biased;
                result=&mut call=>result,
                _=tokio::time::sleep(timeout)=> {
                    cancel.cancel();
                    let _=call.await;
                    self.control.fail(HostError::new("tool_timeout","操作执行时间超过预算，已等待实际清理"));
                    Err(Error::ToolHost("tool execution budget exceeded".into()))
                }
            }
        } else {
            call.await
        }
    }
}
#[async_trait]
impl ToolHostValidator for HostTools {
    async fn validate_input(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        if name == "delegate_task" && self.execution.as_ref().is_some_and(|e| e.role == "lead") {
            return crate::orchestration::validate(input).map_err(|e| e.message);
        }
        if self.progress_enabled && name == "report_progress" {
            return crate::task_progress::validate(input).map_err(|e| e.message);
        }
        if !self.allowed.iter().any(|s| s == name) {
            return Err("工具未在Host授权清单".into());
        }
        self.registry.validate_input(name, input).await
    }
}

impl HostTools {
    async fn invoke(
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
        if self.progress_enabled && name == "report_progress" {
            if context.cancel().is_cancelled() {
                return Err(Error::Cancelled);
            }
            let binding = context.binding().clone();
            let cancel = context.cancel();
            return self
                .store
                .call_priority(move |db| {
                    if cancel.is_cancelled() {
                        return Err(HostError::new("cancelled", "本轮正在停止"));
                    }
                    db.report_progress(&binding, &args)?;
                    crate::task_progress::accepted_plan_hint(db.progress(&binding.run_id)?)
                })
                .await
                .map(ToolCallOutcome::ok)
                .map_err(|e| {
                    if e.kind == "storage_unavailable" {
                        self.control.fail(e.clone());
                        Error::Store("业务进度无法可靠保存".into())
                    } else {
                        Error::ToolHost(e.to_string())
                    }
                });
        }
        if name == "delegate_task" {
            let execution = self
                .execution
                .as_ref()
                .filter(|e| e.role == "lead")
                .ok_or_else(|| Error::ToolHost("子角色不能委派".into()))?;
            let result = self
                .host
                .delegate(execution, context.binding().clone(), context.cancel())
                .await
                .map_err(|e| {
                    if e.kind == "storage_unavailable" {
                        self.control.fail(e.clone());
                        Error::Store(e.message)
                    } else if e.kind == "cancelled" {
                        Error::Cancelled
                    } else {
                        Error::ToolHost(e.to_string())
                    }
                })?;
            let completed = serde_json::from_str::<serde_json::Value>(&result)
                .ok()
                .is_some_and(|v| v["state"] == "completed");
            let mut outcome = ToolCallOutcome::ok(result);
            outcome.is_error = !completed;
            return Ok(crate::task_progress::with_execution_receipt(
                outcome,
                context.binding(),
                self.progress_enabled,
            ));
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
        let waiting_binding = context.binding().clone();
        if let Err(error) = self
            .store
            .call_priority(move |db| {
                let revision = db.bump(
                    "tool_waiting_resource",
                    &serde_json::to_string(&waiting_binding).map_err(HostError::storage)?,
                )?;
                let _ = db.notices.send(crate::HostNotice::Changed { revision });
                Ok(())
            })
            .await
        {
            self.control.fail(error);
            return Err(Error::Store("无法保存工具资源等待事实".into()));
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
        let receipt_binding = binding.clone();
        if let Err(e)=self.store.call_priority(move|db|{let rev=db.bump("tool_result",&serde_json::json!({"run_id":binding.run_id,"call_id":binding.call_id,"success":success}).to_string())?;let _=db.notices.send(crate::HostNotice::Changed{revision:rev});Ok(())}).await {self.control.fail(e);return Err(Error::Store("工具结果保存失败".into()));}
        // 回执在原结果可靠保存后追加；不提前宣称执行成功，也不把回执当作步骤完成。
        result.map(|outcome| {
            crate::task_progress::with_execution_receipt(
                outcome,
                &receipt_binding,
                self.progress_enabled,
            )
        })
    }
}
