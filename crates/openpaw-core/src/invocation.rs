//! 职责：提供不可变的最终工具输入绑定和执行开始观察接口。
//! 输入输出：引擎审批结果转为一次调用上下文，工具校验后通知真实开始。
//! 依赖：既有core错误、serde和取消token。
//! 禁止职责：不实现审批状态机、工作区调度或Office协议。
//! Stability: unstable.
use crate::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 用途：审批与执行共享的精确输入绑定
/// 字段：Run、call、工具及完整JSON
/// 来源去向：引擎到审批器/执行观察器
/// 约束：完整内容比较，不以截断展示或有碰撞摘要授权
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallBinding {
    /// 用途：独立运行标识
    /// 字段：独立运行标识
    /// 来源去向：引擎到可信审批/执行层
    /// 约束：只读绑定；不是远程认证凭据
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub run_id: String,
    /// 用途：模型工具调用标识
    /// 字段：模型工具调用标识
    /// 来源去向：引擎到可信审批/执行层
    /// 约束：只读绑定；不是远程认证凭据
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub call_id: String,
    /// 用途：最终工具名
    /// 字段：最终工具名
    /// 来源去向：引擎到可信审批/执行层
    /// 约束：只读绑定；不是远程认证凭据
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub tool_name: String,
    /// 用途：最终完整输入
    /// 字段：最终完整输入
    /// 来源去向：引擎到可信审批/执行层
    /// 约束：只读绑定；不是远程认证凭据
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub input: Value,
}

/// 用途：可等待的真实工具开始边界
/// 字段：started方法
/// 来源去向：工具主体前到宿主可靠存储
/// 约束：失败必须阻止执行，不使用提议事件冒充running
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
#[async_trait]
pub trait ToolExecutionObserver: Send + Sync {
    /// 功能：可靠记录工具即将进入主体
    /// 参数：binding为已校验最终调用
    /// 返回：存储成功
    /// 错误：存储或状态错误必须向上传播
    /// 状态：实现可以保存running事实
    /// 并发与取消：调用方等待完成；不能drop可靠写入
    /// 权限：不授予授权，也不得改输入
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    async fn started(&self, binding: &ToolCallBinding) -> Result<()>;
}

/// 用途：一次工具调用上下文
/// 字段：私有不可变绑定、批准标记、token、可选观察器
/// 来源去向：引擎到ToolHost
/// 约束：由可信宿主构造；批准只对当前绑定有效，不建立永久规则
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
#[derive(Clone)]
pub struct ToolCallContext {
    binding: ToolCallBinding,
    approved: bool,
    cancel: CancellationToken,
    observer: Option<Arc<dyn ToolExecutionObserver>>,
}
impl ToolCallContext {
    /// 功能：创建未获人工授权的调用上下文
    /// 参数：run_id/call_id/name/input为最终调用，cancel为调用token
    /// 返回：新的独立上下文
    /// 错误：无
    /// 状态：复制最终输入，尚未批准
    /// 并发与取消：token仅用于本调用
    /// 权限：构造不授予人工权限
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn new(
        run_id: impl Into<String>,
        call_id: impl Into<String>,
        name: impl Into<String>,
        input: &Value,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            binding: ToolCallBinding {
                run_id: run_id.into(),
                call_id: call_id.into(),
                tool_name: name.into(),
                input: input.clone(),
            },
            approved: false,
            cancel,
            observer: None,
        }
    }
    /// 功能：在可信审批器确认后标记本次绑定已获批
    /// 参数：self为已定输入上下文
    /// 返回：持有本次批准的上下文
    /// 错误：无
    /// 状态：只修改本次实例标记
    /// 并发与取消：不共享或创建永久批准
    /// 权限：仅引擎/宿主在审批成功后调用，不能信任远程布尔参数
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn approve(mut self) -> Self {
        self.approved = true;
        self
    }
    /// 功能：绑定可等待的真实执行观察器
    /// 参数：observer为宿主实现
    /// 返回：更新后上下文
    /// 错误：无
    /// 状态：只设置本次观察器
    /// 并发与取消：观察器需线程安全
    /// 权限：观察器不能修改输入或授权
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn with_observer(mut self, observer: Arc<dyn ToolExecutionObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
    /// 功能：读取本次输入绑定
    /// 参数：self为上下文
    /// 返回：不可变绑定引用
    /// 错误：无
    /// 状态：只读
    /// 并发与取消：可共享读取
    /// 权限：不是远程凭证
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn binding(&self) -> &ToolCallBinding {
        &self.binding
    }
    /// 功能：取得本次取消信号
    /// 参数：self为上下文
    /// 返回：同一调用token的克隆
    /// 错误：无
    /// 状态：不取消信号
    /// 并发与取消：取消后仍等待实际清理
    /// 权限：只作用于本调用
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn cancel(&self) -> CancellationToken {
        self.cancel.clone()
    }
    /// 功能：检查本次批准是否覆盖给定最终输入
    /// 参数：name/input为待执行内容
    /// 返回：精确相等且已批准且未取消时为真
    /// 错误：无
    /// 状态：只读
    /// 并发与取消：取消与批准竞争时不允许已取消调用
    /// 权限：完整Value比较防止摘要碰撞，不能修改后复用批准
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub fn is_approved_for(&self, name: &str, input: &Value) -> bool {
        self.approved
            && !self.cancel.is_cancelled()
            && self.binding.tool_name == name
            && self.binding.input == *input
    }
    /// 功能：在最终校验和资源获取后记录真正开始
    /// 参数：name/input为马上执行的内容
    /// 返回：允许进入工具主体
    /// 错误：绑定不符、取消或观察器失败返回错误
    /// 状态：可调用宿主可靠观察器
    /// 并发与取消：等待观察器完成后再检查取消，不drop持久化
    /// 权限：不替代分类审批；调用方必须在锁后重验
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    pub async fn execution_started(&self, name: &str, input: &Value) -> Result<()> {
        if self.binding.tool_name != name || self.binding.input != *input {
            return Err(Error::ToolHost(
                "tool input changed after authorization".into(),
            ));
        }
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(observer) = &self.observer {
            observer
                .started(&self.binding)
                .await
                .map_err(|e| Error::Store(format!("cannot persist tool execution start: {e}")))?;
        }
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}
