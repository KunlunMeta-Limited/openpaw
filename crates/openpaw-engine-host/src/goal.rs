//! 功能：将主聊天 Goal module 接到既有 runtime 的五项同步效果。
//! 输入输出边界：只转交已校验快照与当前会话事实，不持有 session 强引用。
//! 禁止承担：不实现 Goal 转换、第二 writer、前台驱动或新的 Office 协议。
//! 关联：CHG-2026-0050 第11节已批准的 Goal module 提取。

use crate::runtime::SessionHandle;
use openpaw_main_goal::GoalRuntime;
pub(crate) use openpaw_main_goal::{plan_tools, GoalContext, GoalToolHost};
use serde_json::Value;
use std::sync::{Arc, Weak};

struct HostGoalRuntime {
    handle: Weak<SessionHandle>,
}

/// 功能：为已组装会话构造 Goal 同步效果 adapter。
/// 参数：handle 为当前会话的真实 runtime handle。
/// 返回：内部只保存弱引用的共享 adapter。
/// 错误：无。
/// 状态：不读写 Goal、journal 或执行状态。
/// 并发与取消：仅组装时安装，每次效果读取原 handle 的当前事实。
/// 权限：不创建执行许可，不替换预算或取消令牌。
/// 关联：runtime::build_session_handle、GoalContext::attach。
pub(crate) fn runtime_adapter(handle: &Arc<SessionHandle>) -> Arc<dyn GoalRuntime> {
    Arc::new(HostGoalRuntime {
        handle: Arc::downgrade(handle),
    })
}

impl GoalRuntime for HostGoalRuntime {
    fn commit(&self, event: Value) -> Result<(), String> {
        self.handle
            .upgrade()
            .ok_or_else(|| "Goal 会话不可用".to_string())?
            .commit_goal(event)
    }

    fn is_cancelled(&self) -> bool {
        self.handle
            .upgrade()
            .is_none_or(|handle| handle.main_cancel_token().is_cancelled())
    }

    fn execution_facts(&self) -> Option<Value> {
        self.handle
            .upgrade()
            .map(|handle| handle.goal_execution_facts())
    }

    fn cancel_children(&self) {
        if let Some(handle) = self.handle.upgrade() {
            handle.cancel_goal_children();
        }
    }

    fn project(&self, goal: Value) {
        if let Some(handle) = self.handle.upgrade() {
            handle.office.project_goal(&handle.id, goal);
        }
    }
}
