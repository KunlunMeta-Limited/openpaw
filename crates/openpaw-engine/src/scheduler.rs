//! 功能：提供与传输协议无关的跨会话有界 FIFO 调度。
//! 输入输出边界：接收容量配置与取消令牌，返回只可消费一次的运行凭据。
//! 禁止承担：不读环境、不写会话、不发布 HTTP、SSE 或 Office 事件。
//! 关联：CHG-2026-0025。
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// 功能：表示调用方明确提供的运行与等待容量。
#[derive(Debug, Clone, Copy)]
pub struct SchedulerLimits {
    /// 同时执行的回合上限，必须大于零。
    pub max_concurrent_turns: usize,
    /// 等待运行槽位的回合上限，零表示不排队。
    pub max_queued_turns: usize,
}
struct SchedulerState {
    running: usize,
    next_ticket: u64,
    queued: VecDeque<Arc<QueuedEntry>>,
}
struct QueuedEntry {
    ticket: u64,
    promoted: AtomicBool,
    notify: Notify,
}
/// 功能：提供跨会话有界 FIFO 调度，以同步锁线性化接收、取消和 permit 交接。
pub struct TurnScheduler {
    config: SchedulerLimits,
    state: std::sync::Mutex<SchedulerState>,
}
impl TurnScheduler {
    /// 功能：用显式容量创建调度器。
    /// 参数：config 提供正数运行容量及非负等待容量。
    /// 返回：可共享的空调度器。
    /// 错误：运行容量为零时 panic；调用方须先校验配置。
    /// 状态：初始化零个运行项及空队列，不读取环境变量。
    /// 并发与取消：初始化同步完成，不创建后台任务或取消令牌。
    /// 权限：不执行工具，不访问文件或网络。
    /// 关联：CHG-2026-0025。
    pub fn new(config: SchedulerLimits) -> Arc<Self> {
        assert!(
            config.max_concurrent_turns > 0,
            "concurrent capacity must be positive"
        );
        Arc::new(Self {
            config,
            state: std::sync::Mutex::new(SchedulerState {
                running: 0,
                next_ticket: 0,
                queued: VecDeque::new(),
            }),
        })
    }
    /// 功能：原子接收一个回合或报告队列已满。
    /// 参数：self 为共享调度器。
    /// 返回：立即可运行的 permit 或拥有 FIFO 票据的等待凭据。
    /// 错误：运行和等待容量均耗尽时返回 QueueFull。
    /// 状态：原子占用一个槽位或添加一个队列项。
    /// 并发与取消：同步锁保护容量；丢弃返回的凭据自动归还容量。
    /// 权限：不执行工具，不访问文件或网络。
    /// 关联：CHG-2026-0025。
    pub fn admit(self: &Arc<Self>) -> Result<TurnAdmission, ScheduleError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running < self.config.max_concurrent_turns {
            state.running += 1;
            return Ok(TurnAdmission::Running(TurnPermit {
                scheduler: self.clone(),
                released: false,
            }));
        }
        if state.queued.len() >= self.config.max_queued_turns {
            return Err(ScheduleError::QueueFull);
        }
        state.next_ticket = state.next_ticket.saturating_add(1);
        let entry = Arc::new(QueuedEntry {
            ticket: state.next_ticket,
            promoted: AtomicBool::new(false),
            notify: Notify::new(),
        });
        state.queued.push_back(entry.clone());
        Ok(TurnAdmission::Queued(QueuedTurn {
            scheduler: self.clone(),
            entry,
            armed: true,
        }))
    }
    /// 功能：查询等待凭据的当前一基队列位置。
    /// 参数：ticket 为等待凭据的不透明票号。
    /// 返回：一基队列位置；已晋升、取消或未知票号返回 None。
    /// 错误：无。
    /// 状态：只读当前队列。
    /// 并发与取消：同步锁下读取快照，位置可能随后改变。
    /// 权限：不读取会话正文或访问外部资源。
    /// 关联：CHG-2026-0025。
    pub fn queue_position(&self, ticket: u64) -> Option<usize> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .queued
            .iter()
            .position(|entry| entry.ticket == ticket)
            .map(|index| index + 1)
    }
    fn cancel_queued(&self, ticket: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = state.queued.iter().position(|entry| entry.ticket == ticket) else {
            return false;
        };
        state.queued.remove(index);
        true
    }
    fn release_running(&self) {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(next) = state.queued.pop_front() {
                next.promoted.store(true, Ordering::Release);
                Some(next)
            } else {
                state.running = state.running.saturating_sub(1);
                None
            }
        };
        if let Some(next) = next {
            next.notify.notify_one();
        }
    }
}
/// 功能：表示中立调度接收失败的稳定原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// 运行与等待容量均已耗尽。
    QueueFull,
}
/// 功能：持有运行槽位或 FIFO 等待凭据；wait 同时监听晋升与调用方取消。
pub enum TurnAdmission {
    /// 已占用运行槽位。
    Running(TurnPermit),
    /// 正在等待运行槽位。
    Queued(QueuedTurn),
}
impl TurnAdmission {
    /// 功能：判断凭据是否正在等待运行槽位。
    /// 参数：self 为尚未消费的接收凭据。
    /// 返回：凭据为等待类型时返回 true；是否已晋升须通过 wait 确认。
    /// 错误：无。
    /// 状态：只读凭据类型。
    /// 并发与取消：同步无锁，不影响取消或晋升。
    /// 权限：不访问会话或外部资源。
    /// 关联：CHG-2026-0025。
    pub fn is_queued(&self) -> bool {
        matches!(self, Self::Queued(_))
    }
    /// 功能：读取等待凭据的不透明票号。
    /// 参数：self 为尚未消费的接收凭据。
    /// 返回：等待类型返回票号，立即运行类型返回 None。
    /// 错误：无。
    /// 状态：只读票号，不查询当前队列位置。
    /// 并发与取消：同步无锁，不影响取消或晋升。
    /// 权限：不访问会话或外部资源。
    /// 关联：CHG-2026-0025。
    pub fn queue_ticket(&self) -> Option<u64> {
        match self {
            Self::Running(_) => None,
            Self::Queued(queued) => Some(queued.entry.ticket),
        }
    }
    /// 功能：消费凭据并等待运行槽位或取消。
    /// 参数：self 为只消费一次的凭据，cancel 为调用方取消令牌。
    /// 返回：运行 permit；仍在队列中且被取消时返回 None。
    /// 错误：无。
    /// 状态：取消移除队列项；晋升转为运行 permit。
    /// 并发与取消：晋升先完成时仍返回 permit，由调用方检查取消；丢弃 future 自动释放容量。
    /// 权限：不反向取消调用方，不执行工具或访问外部资源。
    /// 关联：CHG-2026-0025。
    pub async fn wait(self, cancel: CancellationToken) -> Option<TurnPermit> {
        match self {
            Self::Running(permit) => Some(permit),
            Self::Queued(queued) => queued.wait(cancel).await,
        }
    }
}
/// 功能：独占一个运行槽位；释放时自动晋升 FIFO 下一项。
pub struct TurnPermit {
    scheduler: Arc<TurnScheduler>,
    released: bool,
}
impl Drop for TurnPermit {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            self.scheduler.release_running();
        }
    }
}
/// 功能：拥有一个等待票据；丢弃时移除等待项或归还已晋升槽位。
pub struct QueuedTurn {
    scheduler: Arc<TurnScheduler>,
    entry: Arc<QueuedEntry>,
    armed: bool,
}
impl QueuedTurn {
    async fn wait(mut self, cancel: CancellationToken) -> Option<TurnPermit> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                if self.scheduler.cancel_queued(self.entry.ticket) {
                    self.armed = false;
                    None
                } else if self.entry.promoted.load(Ordering::Acquire) {
                    self.armed = false;
                    Some(TurnPermit { scheduler: self.scheduler.clone(), released: false })
                } else {
                    self.armed = false;
                    None
                }
            }
            _ = self.entry.notify.notified() => {
                self.armed = false;
                Some(TurnPermit { scheduler: self.scheduler.clone(), released: false })
            }
        }
    }
}
impl Drop for QueuedTurn {
    fn drop(&mut self) {
        if self.armed
            && !self.scheduler.cancel_queued(self.entry.ticket)
            && self.entry.promoted.load(Ordering::Acquire)
        {
            // release_running 已把活动槽位交给本票据；若等待 future 此时被丢弃，
            // 必须继续把槽位交给下一项或归还容量，不能让并发额度永久泄漏。
            self.scheduler.release_running();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fifo_is_bounded_and_cancelled_waiter_releases_its_ticket() {
        let scheduler = TurnScheduler::new(SchedulerLimits {
            max_concurrent_turns: 1,
            max_queued_turns: 2,
        });
        let first = scheduler.admit().unwrap();
        let second = scheduler.admit().unwrap();
        let third = scheduler.admit().unwrap();
        let second_ticket = second.queue_ticket().unwrap();
        let third_ticket = third.queue_ticket().unwrap();
        assert!(matches!(scheduler.admit(), Err(ScheduleError::QueueFull)));
        assert_eq!(scheduler.queue_position(second_ticket), Some(1));
        assert_eq!(scheduler.queue_position(third_ticket), Some(2));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(second.wait(cancel).await.is_none());
        assert_eq!(scheduler.queue_position(third_ticket), Some(1));
        drop(first);
        let promoted = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            third.wait(CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(scheduler.admit().unwrap().is_queued());
        drop(promoted);
        assert!(!scheduler.admit().unwrap().is_queued());
    }

    #[test]
    fn dropping_promoted_waiter_returns_capacity() {
        let scheduler = TurnScheduler::new(SchedulerLimits {
            max_concurrent_turns: 1,
            max_queued_turns: 1,
        });
        let running = scheduler.admit().unwrap();
        let queued = scheduler.admit().unwrap();
        drop(running);
        drop(queued);
        assert!(matches!(
            scheduler.admit().unwrap(),
            TurnAdmission::Running(_)
        ));
    }
}
