//! 职责：协议无关的运行上下文、结果与可靠事件观察。
//! 输入输出：调用方上下文到运行终态和JSONL事件。
//! 依赖：复用core事件与取消token。
//! 禁止职责：不解析Office协议，不另建引擎或存储。

use async_trait::async_trait;
use openpaw_core::{Error, Result, SessionEvent, StopReason, Usage};
use std::{future::Future, time::Duration};
use tokio_util::sync::CancellationToken;

/// 用途：一次运行的独立取消和预算
/// 字段：标识、父token、总/工具时限。
/// 来源去向：CLI/Host到AgentLoop
/// 约束：每个Run独立，不复用引擎并发。
/// 稳定性：新增接口
/// 关联：需求/设计 REQ-OP-003；DES-OP-002。
#[derive(Clone, Debug)]
pub struct RunContext {
    /// 用途：账本关联
    /// 字段：运行标识
    /// 来源去向：调用方到事件
    /// 约束：非空且唯一
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub run_id: String,
    /// 用途：外部取消
    /// 字段：父token
    /// 来源去向：宿主到引擎
    /// 约束：引擎不替换或取消父token
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub cancel: CancellationToken,
    /// 用途：Run总预算
    /// 字段：可选时长
    /// 来源去向：配置到引擎
    /// 约束：含等待但不提前结束清理
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub total_timeout: Option<Duration>,
    /// 用途：单次工具时间
    /// 字段：可选时长
    /// 来源去向：配置到执行器
    /// 约束：到期通知取消仍等待清理
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub tool_timeout: Option<Duration>,
}
impl Default for RunContext {
    fn default() -> Self {
        Self {
            run_id: openpaw_core::session::SessionId::new().to_string(),
            cancel: CancellationToken::new(),
            total_timeout: None,
            tool_timeout: None,
        }
    }
}

/// 用途：可靠运行终态
/// 变体：正常、失败、取消、超时。
/// 来源去向：引擎到CLI/Host
/// 约束：工具清理后返回，持久化失败为失败。
/// 稳定性：新增
/// 关联：需求/设计 REQ-OP-003。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// 用途：正常完成
    /// 变体：无
    /// 来源去向：引擎到客户端
    /// 约束：事实已保存
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    Completed,
    /// 用途：运行失败
    /// 变体：无
    /// 来源去向：引擎到客户端
    /// 约束：不能假成功
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    Failed,
    /// 用途：外部取消
    /// 变体：无
    /// 来源去向：引擎到客户端
    /// 约束：实际清理后
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    Cancelled,
    /// 用途：超过预算
    /// 变体：无
    /// 来源去向：引擎到客户端
    /// 约束：与取消区分
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    TimedOut,
}
impl RunStatus {
    /// 功能：转换为账本稳定终态名称。
    /// 参数：self为终态。
    /// 返回：静态状态标识。
    /// 错误：无。
    /// 状态：无。
    /// 并发与取消：只读，无等待。
    /// 权限：无。
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

/// 用途：一次运行结果
/// 字段：终态、停止原因、用量和错误。
/// 来源去向：引擎到宿主
/// 约束：没有Provider用量时不推测token数。
/// 稳定性：新增
/// 关联：需求/设计 REQ-OP-003。
#[derive(Debug)]
pub struct RunOutcome {
    /// 用途：运行终态
    /// 字段：枚举
    /// 来源去向：引擎到调用方
    /// 约束：与错误一致
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub status: RunStatus,
    /// 用途：正常模型停止原因
    /// 字段：可选原因
    /// 来源去向：Provider到调用方
    /// 约束：失败时为空
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub stop_reason: Option<StopReason>,
    /// 用途：本Run累计已知用量
    /// 字段：用量Usage
    /// 来源去向：Provider到宿主
    /// 约束：不使用Session饱和计数差
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-002。
    pub usage: Usage,
    /// 用途：具体诊断
    /// 字段：可选Error
    /// 来源去向：引擎到调用方
    /// 约束：不吞存储错误
    /// 稳定性：新增
    /// 关联：需求/设计 DES-OP-003。
    pub error: Option<Error>,
}

/// 用途：关键生命周期的可等待持久化边界
/// 字段：无。
/// 来源去向：引擎到宿主Store
/// 约束：record失败即停止后续副作用，不能在同步显示回调中替代。
/// 稳定性：新增
/// 关联：需求/设计 REQ-OP-004；DES-OP-003。
#[async_trait]
pub trait RunObserver: Send {
    /// 功能：可靠接收一个事实、快照或终态事件。
    /// 参数：event为按发生顺序生成的事件。
    /// 返回：持久化完成后的成功结果。
    /// 错误：写入失败必须返回错误，不得伪报成功。
    /// 状态：实现可追加账本；终态失败不能撤销已发生副作用。
    /// 并发与取消：引擎顺序等待；不可因取消中途丢弃写入。
    /// 权限：实现负责已授权的数据目录，不得记录密钥。
    /// 关联：需求/设计 REQ-OP-004；DES-OP-003。
    async fn record(&mut self, event: SessionEvent) -> Result<()>;
}

/// 用途：旧入口的空观察器
/// 字段：无
/// 来源去向：run_turn到运行接口
/// 约束：不提供持久化保证
/// 稳定性：内部
/// 关联：需求/设计 DES-OP-003。
pub(crate) struct NoopObserver;
#[async_trait]
impl RunObserver for NoopObserver {
    async fn record(&mut self, _: SessionEvent) -> Result<()> {
        Ok(())
    }
}

/// 用途：运行内部共享取消与截止条件
/// 字段：父/子token、总截止及工具时限
/// 来源去向：运行到等待阶段
/// 约束：只通知取消不提前释放工具
/// 稳定性：内部
/// 关联：需求/设计 DES-OP-002。
#[derive(Clone)]
pub(crate) struct RunControl {
    /// 用途：调用授权的运行关联
    /// 字段：运行标识run_id
    /// 来源去向：RunContext到工具
    /// 约束：每次独立运行保持稳定
    /// 稳定性：新增协议无关接口
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    pub(crate) run_id: String,
    /// 用途：本Run子取消信号
    /// 字段：运行配置
    /// 来源去向：上下文到工具
    /// 约束：不能提前结束实际清理
    /// 稳定性：内部
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) cancel: CancellationToken,
    /// 用途：兼容旧入口的外部父信号
    /// 字段：运行配置
    /// 来源去向：上下文到工具
    /// 约束：不能提前结束实际清理
    /// 稳定性：内部
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) parent: Option<CancellationToken>,
    /// 用途：本Run总截止时间
    /// 字段：运行配置
    /// 来源去向：上下文到工具
    /// 约束：不能提前结束实际清理
    /// 稳定性：内部
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) deadline: Option<tokio::time::Instant>,
    /// 用途：单次工具执行时限
    /// 字段：运行配置
    /// 来源去向：上下文到工具
    /// 约束：不能提前结束实际清理
    /// 稳定性：内部
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) tool_timeout: Option<Duration>,
    /// 用途：工具超时共享通知
    /// 字段：运行配置
    /// 来源去向：上下文到工具
    /// 约束：不能提前结束实际清理
    /// 稳定性：内部
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) expired: CancellationToken,
}
impl RunControl {
    /// 功能：检查外部取消、工具超时和Run总预算。
    /// 参数：self为运行控制。
    /// 返回：未中断时成功。
    /// 错误：取消或超时返回明确错误。
    /// 状态：只读检查。
    /// 并发与取消：取消优先于总截止，工具超时独立表达。
    /// 权限：无。
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) fn check(&self) -> Result<()> {
        if self.expired.is_cancelled() {
            return Err(Error::Timeout("tool execution budget exceeded".into()));
        }
        if self.cancel.is_cancelled()
            || self
                .parent
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(Error::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|d| tokio::time::Instant::now() >= d)
        {
            return Err(Error::Timeout("run total budget exceeded".into()));
        }
        Ok(())
    }
    /// 功能：等待任一取消或超时条件。
    /// 参数：self为本次运行控制。
    /// 返回：具体取消或超时错误。
    /// 错误：结果本身即中断原因。
    /// 状态：不修改父token。
    /// 并发与取消：无条件时等待取消，不能替代工具清理。
    /// 权限：无。
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) async fn interrupted(&self) -> Error {
        tokio::select! {
            biased;
            _ = self.expired.cancelled() => Error::Timeout("tool execution budget exceeded".into()),
            _ = self.cancel.cancelled() => Error::Cancelled,
            _ = async { match &self.parent { Some(t) => t.cancelled().await, None => futures::future::pending().await } } => Error::Cancelled,
            _ = async {
                match self.deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => futures::future::pending().await,
                }
            } => Error::Timeout("run total budget exceeded".into()),
        }
    }
    /// 功能：可取消地等待无副作用请求或查询。
    /// 参数：future为允许安全丢弃的等待。
    /// 返回：原future结果。
    /// 错误：取消或总预算超时。
    /// 状态：不用于工具执行和可靠持久化。
    /// 并发与取消：中断时丢弃传入future，实际工具用专门清理等待。
    /// 权限：不得用于可能仍产生副作用的操作。
    /// 关联：需求/设计 DES-OP-002。
    pub(crate) async fn wait<T>(&self, future: impl Future<Output = T>) -> Result<T> {
        self.check()?;
        tokio::select! { biased; e = self.interrupted() => Err(e), v = future => Ok(v) }
    }
}
