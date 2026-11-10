//! 职责：用原Host Session/Run实现一层真实委派和主子总预算。
//! 输入输出：可信主Run的delegate_task到原子Run终态与账本引用。
//! 依赖：共享注册表、原Provider、原Store；不执行第二套推理循环。
//! 禁止职责：不持Root锁等子Run，不赋予角色额外权限，不把模型结果提升为system。
use crate::{
    approval::RunControl, AgentsConfig, ClientScope, Host, HostError, HostResult, RunState,
};
use openpaw_core::{ToolCallBinding, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// 用途：共享协作配额，不包含另一套Run或Session注册表。
/// 字段：父Run、绝对期限、调用/输出预算和有界子任务门。
/// 来源去向：可信主运行接受到原Provider与委派工具。
/// 约束：主子共用同一对象；不能延长父期限。
/// 稳定性：Host内部T6实现。
/// 关联：需求/设计REQ-OP-022；DES-OP-012。
pub(crate) struct Group {
    /// 用途：所属主Run的稳定标识。
    /// 字段：String，所属主Run的稳定标识。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) parent: String,
    /// 用途：不可延长的父运行绝对期限。
    /// 字段：Instant，不可延长的父运行绝对期限。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) deadline: Option<Instant>,
    /// 用途：启动时已校验的协作配置。
    /// 字段：AgentsConfig，启动时已校验的协作配置。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) config: AgentsConfig,
    /// 用途：与父运行同源的取消token。
    /// 字段：CancellationToken，与父运行同源的取消token。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) cancel: CancellationToken,
    /// 用途：单组活动子任务有界门。
    /// 字段：Semaphore，单组活动子任务有界门。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) gate: Arc<Semaphore>,
    /// 用途：同一父组的持续控制副本，原host_events是持久来源。
    /// 字段：Mutex<State>，主子共享的持续控制副本。
    /// 来源去向：主角色工具到执行/审批装配。
    /// 约束：停止后不再授权，子角色不能修改。
    /// 稳定性：Host内部。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) continuous: Mutex<crate::continuous::State>,
    /// 用途：串行化可靠意图与原子Run接受之间的短窗口。
    /// 字段：异步互斥门，不覆盖工具执行或等待结果。
    /// 来源去向：同一组并发delegate调用到原Store/Run接受。
    /// 约束：持门不得等待角色、资源或模型；防止下一调用误读尚在接受的意图。
    /// 稳定性：Host内部调度同步。
    /// 关联：CHG-2026-0007第9.14节D113。
    pub(crate) accepting: tokio::sync::Mutex<()>,
    queue: Mutex<std::collections::BTreeMap<String, std::collections::VecDeque<String>>>,
    queue_changed: tokio::sync::Notify,
    role_gates: std::collections::BTreeMap<&'static str, Arc<Semaphore>>,
    budget: Mutex<(u64, u64, u64)>,
}
impl Group {
    /// 功能：创建一组主子共享配额。
    /// 参数：parent/config/control为可信父运行，seconds为原总预算。
    /// 返回：唯一共享Group。
    /// 错误：无；配置已在启动时校验。
    /// 状态：初始化配额，不创建子Run。
    /// 并发与取消：预算互斥、子门有界、取消与父同源。
    /// 权限：仅原Host接受流程使用。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn new(
        parent: String,
        config: AgentsConfig,
        control: &RunControl,
        seconds: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            parent,
            deadline: (seconds != 0)
                .then(|| Instant::now() + std::time::Duration::from_secs(seconds)),
            gate: Arc::new(Semaphore::new(config.max_active_children)),
            budget: Mutex::new((0, 0, 0)),
            continuous: Mutex::new(crate::continuous::State::default()),
            accepting: tokio::sync::Mutex::new(()),
            queue: Mutex::new(Default::default()),
            queue_changed: tokio::sync::Notify::new(),
            role_gates: crate::agents::ROLES
                .iter()
                .filter(|r| r.id != "lead")
                .map(|r| (r.id, Arc::new(Semaphore::new(1))))
                .collect(),
            config,
            cancel: control.cancel.clone(),
        })
    }
    /// 功能：按真实接受顺序登记成员待办索引。
    /// 参数：role为固定角色，id为原注册表的子Run ID。
    /// 返回：成功入队，无另建任务事实。
    /// 错误：每人64项或组内256项未清理待办时明确拒绝。
    /// 状态：仅进程内调度索引，原Run和Delegation为持久来源。
    /// 并发与取消：调用方持原注册表短锁确定接受顺序；不等待模型。
    /// 权限：仅可信子Run可登记，不能增加角色或工作根。
    /// 关联：CHG-2026-0007第9.14节D113。
    pub(crate) fn enqueue(&self, role: &str, id: &str) -> HostResult<()> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.values().map(|q| q.len()).sum::<usize>() >= 256
            || queue.get(role).is_some_and(|q| q.len() >= 64)
        {
            return Err(HostError::new(
                "capacity_exceeded",
                "成员待办队列已满，请先收取已有任务结果",
            ));
        }
        queue.entry(role.into()).or_default().push_back(id.into());
        self.queue_changed.notify_waiters();
        Ok(())
    }

    /// 功能：实际Run清理后移除调度索引，唤醒下一项。
    /// 参数：role为可信角色，id为原子Run ID。
    /// 返回：无，重复清理无副作用。
    /// 错误：无。
    /// 状态：不删除账本、成果或交接事实。
    /// 并发与取消：可移除队中已取消项，执行中的队首保留至实际清理。
    /// 权限：仅原注册表release使用。
    /// 关联：CHG-2026-0007第9.14节D113。
    pub(crate) fn dequeue(&self, role: &str, id: &str) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(items) = queue.get_mut(role) {
            items.retain(|item| item != id);
        }
        self.queue_changed.notify_waiters();
    }

    /// 功能：只读返回本组真实角色FIFO位置及前驱。
    /// 参数：role和id为原委派身份。
    /// 返回：排位及前驱Run，尚未入队返回空，不推断开工。
    /// 错误：无；互斥锁毒化时读取原数据。
    /// 状态：不改变队列、许可或执行顺序。
    /// 并发与取消：短锁快照，返回后可能变化，不持锁等待。
    /// 权限：只在已鉴别主Run的delegation_status使用。
    /// 关联：CHG-2026-0007第12.160节D172；REQ-OP-022。
    pub(crate) fn queue_position(&self, role: &str, id: &str) -> (Option<usize>, Option<String>) {
        let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let Some(items) = queue.get(role) else {
            return (None, None);
        };
        let Some(index) = items.iter().position(|item| item == id) else {
            return (None, None);
        };
        (
            Some(index + 1),
            index.checked_sub(1).and_then(|i| items.get(i)).cloned(),
        )
    }

    /// 功能：等待本人成为队首后取得真实角色及组执行额度。
    /// 参数：role和id来自原接受流程，cancel为子Run取消上下文。
    /// 返回：持有到真实执行和清理结束的两枚许可。
    /// 错误：取消、父期限届满或角色无效时不执行任务。
    /// 状态：等待期间原Run保持Accepted，不伪装running，不占工作区锁。
    /// 并发与取消：先订阅再检查FIFO；取消队中项由注册表统一清理。
    /// 权限：不改变全局并发或角色工具范围。
    /// 关联：CHG-2026-0007第9.14节D113。
    pub(crate) async fn acquire(
        &self,
        role: &str,
        id: &str,
        cancel: &CancellationToken,
    ) -> HostResult<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let wait = async {
            loop {
                let changed = self.queue_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(role)
                    .and_then(|q| q.front())
                    .is_some_and(|head| head == id)
                {
                    break;
                }
                changed.await;
            }
            let gate = self
                .role_gates
                .get(role)
                .ok_or_else(|| HostError::new("scope_denied", "未知可信角色"))?;
            let role = gate
                .clone()
                .acquire_owned()
                .await
                .map_err(HostError::storage)?;
            let group = self
                .gate
                .clone()
                .acquire_owned()
                .await
                .map_err(HostError::storage)?;
            Ok((role, group))
        };
        tokio::select! { biased;
            _=cancel.cancelled()=>Err(HostError::new("cancelled","成员待办已取消，未执行操作")),
            _=self.cancel.cancelled()=>Err(HostError::new("cancelled","主任务已取消，未执行待办")),
            _=self.wait_deadline()=>Err(HostError::new("run_timeout","协作期限已到，未执行待办")),
            result=wait=>result,
        }
    }

    /// 功能：在实际Provider请求之前原子预留一次调用及输出额度。
    /// 参数：tokens为本请求实际max_tokens。
    /// 返回：预留成功。
    /// 错误：父取消、期限届满或额度不足返回Provider错误。
    /// 状态：调用次数不退还；未知用量保留全部预留。
    /// 并发与取消：短互斥锁，同组竞争最后份额只能一方成功。
    /// 权限：不能通过子Session重新取得整份预算。
    /// 关联：需求/设计REQ-OP-022；CHG-2026-0007第9.7.1节。
    pub(crate) fn reserve(&self, tokens: u32) -> openpaw_core::Result<()> {
        if self.cancel.is_cancelled() {
            return Err(openpaw_core::Error::Cancelled);
        }
        let mut remaining = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        if self.deadline.is_some_and(|d| Instant::now() >= d)
            || (self.config.max_model_requests != 0
                && remaining.0 >= self.config.max_model_requests)
            || (self.config.max_output_tokens != 0
                && remaining.1.saturating_add(u64::from(tokens)) > self.config.max_output_tokens)
        {
            return Err(openpaw_core::Error::Provider(
                "协作总模型调用或输出预算不足".into(),
            ));
        }
        remaining.0 = remaining.0.saturating_add(1);
        remaining.1 = remaining.1.saturating_add(u64::from(tokens));
        remaining.2 = remaining.2.saturating_add(1);
        Ok(())
    }
    /// 功能：仅按成功完整响应中可信usage退回未用输出额度。
    /// 参数：reserved为原请求预留，usage为原Provider完整响应。
    /// 返回：无。
    /// 错误：无；缺失用量的全零响应保留预留，不当作零成本。
    /// 状态：只退未用额度；调用次数和不完整流不退。
    /// 并发与取消：短预算锁，不等待模型。
    /// 权限：不信任模型正文自报用量。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn settle(&self, reserved: u32, usage: openpaw_core::Usage) {
        if usage.input_tokens == 0 && usage.output_tokens == 0 {
            return;
        }
        let mut remaining = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        remaining.2 = remaining.2.saturating_sub(1);
        remaining.1 = remaining
            .1
            .saturating_sub(u64::from(reserved.saturating_sub(usage.output_tokens)))
            .saturating_add(u64::from(usage.output_tokens.saturating_sub(reserved)));
    }
    /// 功能：生成父子共享预算的可靠终态控制摘要。
    /// 参数：self为唯一共享配额。
    /// 返回：调用次数、已用或保留输出份额和未知用量标志。
    /// 错误：无；不完整流不会被当作零用量。
    /// 状态：只读，不退还份额、不复制消息正文。
    /// 并发与取消：短预算锁，无外部等待。
    /// 权限：只包含数值和父Run ID，不含密钥。
    /// 关联：需求/设计REQ-OP-022；DES-OP-012。
    pub(crate) fn summary(&self) -> Value {
        let remaining = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        json!({"version":1,"run_id":self.parent,"provider_calls":remaining.0,"output_used_or_reserved":remaining.1,"usage_incomplete":remaining.2>0,"unknown_requests":remaining.2})
    }

    /// 功能：等待可选总期限，无期限时仅由外围取消分支唤醒。
    /// 参数：self为可信协作组。
    /// 返回：配置期限到达后返回。
    /// 错误：无。
    /// 状态：不延长或重置期限。
    /// 并发与取消：调用方必须与取消信号一起select。
    /// 权限：模型不能修改期限。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) async fn wait_deadline(&self) {
        if let Some(deadline) = self.deadline {
            tokio::time::sleep_until(deadline).await;
        } else {
            futures::future::pending::<()>().await;
        }
    }
}

/// 用途：原Run关联的可信协作上下文。
/// 字段：共享配额、固定角色ID；lead为唯一可委派角色。
/// 来源去向：Host接受流程到本Run装配与注册表。
/// 约束：不接受浏览器指定子Run上下文。
/// 稳定性：Host内部。
/// 关联：需求/设计REQ-OP-022；DES-OP-012。
#[derive(Clone)]
pub(crate) struct Execution {
    /// 用途：本主子组唯一预算和期限所有者。
    /// 字段：Arc<Group>，本主子组唯一预算和期限所有者。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) group: Arc<Group>,
    /// 用途：本Run的可信固定角色ID。
    /// 字段：String，本Run的可信固定角色ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) role: String,
}

/// 用途：固定版本的持久委派意图，不复制会话正文。
/// 字段：父范围/Run/call、角色、稳定任务和子Session/Run引用及标题。
/// 来源去向：原host_events控制记录到恢复、查询和Office投影。
/// 约束：运行及工具事实仍从原JSONL读取；版本不匹配拒绝。
/// 稳定性：控制payload版本1，不增加数据库表。
/// 关联：需求/设计REQ-OP-022/023；DES-OP-012。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Delegation {
    /// 用途：记录本次真实交接的前序任务。
    /// 字段：可选同父稳定任务ID，旧记录默认None。
    /// 来源去向：已完成前序的Store校验到Office发起人物投影。
    /// 约束：不接受任意人物ID，不允许跨父或未完成依赖。
    /// 稳定性：v1向后兼容可选字段。
    /// 关联：CHG-2026-0007第9.13节D106。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_task_id: Option<String>,
    /// 用途：关联同一个有界业务事项。
    /// 字段：可选主Run内事项标识，旧记录默认None。
    /// 来源去向：已开放事项到委派控制事实。
    /// 约束：不作为路径、权限或业务类型解释。
    /// 稳定性：v1向后兼容可选字段。
    /// 关联：CHG-2026-0007第9.13节D107。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_item: Option<String>,
    /// 用途：持久控制payload版本，当前为1。
    /// 字段：u32，持久控制payload版本，当前为1。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) version: u32,
    /// 用途：原父Run ID，供删除和查询追踪。
    /// 字段：String，原父Run ID，供删除和查询追踪。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) run_id: String,
    /// 用途：原父Session的稳定ID。
    /// 字段：String，原父Session的稳定ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) parent_session: String,
    /// 用途：原Office父会话的完整范围。
    /// 字段：ClientScope，原Office父会话的完整范围。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) parent_scope: ClientScope,
    /// 用途：原父模型真实工具调用ID。
    /// 字段：String，原父模型真实工具调用ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) call_id: String,
    /// 用途：目标固定子角色ID。
    /// 字段：String，目标固定子角色ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) role_id: String,
    /// 用途：至多80个Unicode字符的业务任务目标。
    /// 字段：String，至多80个Unicode字符的业务任务目标。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) title: String,
    /// 用途：有界要求摘要；精确输入仍核对原父JSONL。
    /// 字段：String，有界要求摘要；精确输入仍核对原父JSONL。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) input_summary: String,
    /// 用途：稳定业务任务ID，与Run/call分别管理。
    /// 字段：String，稳定业务任务ID，与Run/call分别管理。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) task_id: String,
    /// 用途：独立内部子会话的完整范围。
    /// 字段：ClientScope，独立内部子会话的完整范围。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) child_scope: ClientScope,
    /// 用途：真实子Session的稳定ID。
    /// 字段：String，真实子Session的稳定ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) child_session: String,
    /// 用途：真实子Run的预先分配ID。
    /// 字段：String，真实子Run的预先分配ID。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) child_run: String,
    /// 用途：意图可靠保存时的UTC时间。
    /// 字段：String，意图可靠保存时的UTC时间。
    /// 来源去向：可信Host装配、原Store与本次运行。
    /// 约束：不得由浏览器或模型覆盖权限及所属关系。
    /// 稳定性：T6宿主字段，旧默认运行保持兼容。
    /// 关联：需求/设计REQ-OP-021/022；DES-OP-012。
    pub(crate) created_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    role_id: String,
    #[serde(default)]
    candidate_roles: Option<Vec<String>>,
    #[serde(default)]
    required_tools: Option<Vec<String>>,
    title: String,
    instructions: String,
    acceptance: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default = "wait_default")]
    wait_for_completion: bool,
    #[serde(default)]
    source_task_id: Option<String>,
    #[serde(default)]
    work_item: Option<String>,
}
fn wait_default() -> bool {
    true
}
impl Input {
    fn parse(value: &Value) -> HostResult<Self> {
        let input: Self = serde_json::from_value(value.clone()).map_err(|_| {
            let missing: Vec<_> = ["role_id", "title", "instructions", "acceptance"]
                .into_iter()
                .filter(|field| value.get(*field).is_none())
                .collect();
            let message = if missing.is_empty() {
                "委派字段类型或未知字段不合法；需要role_id/title/instructions/acceptance，不允许覆盖根或模型".to_owned()
            } else {
                format!("委派缺少必填字段：{}。补齐同一项任务的准确输入后再提交；未创建任务。", missing.join(", "))
            };
            HostError::new("invalid_input", message)
        })?;
        if input.role_id != "auto"
            && (input.candidate_roles.is_some() || input.required_tools.is_some())
        {
            return Err(HostError::new(
                "invalid_input",
                "固定role_id不能附candidate_roles/required_tools。未指定人员的独立任务请保留候选和必需工具、将role_id改为auto；确需固定人员则删除这两个仅auto字段。不会自动改派或创建任务。",
            ));
        }
        if input.role_id == "auto"
            && (input.candidate_roles.as_ref().is_none_or(|roles| {
                roles.is_empty()
                    || roles.len() > 5
                    || roles
                        .iter()
                        .any(|r| r == "lead" || crate::agents::role(r).is_none())
                    || roles
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != roles.len()
            }) || input.required_tools.as_ref().is_none_or(|tools| {
                tools.len() > 64
                    || tools.iter().any(|t| {
                        t.is_empty()
                            || t.len() > 80
                            || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
            }))
        {
            return Err(HostError::new("invalid_input", "auto模式必须同时提供candidate_roles（1—5个不同且合法的子角色）和required_tools（最多64项合法工具名）；纯文字任务也须显式required_tools=[]。不能以更换固定角色绕过缺失参数；未创建任务。"));
        }
        if input.role_id == "lead"
            || (input.role_id != "auto" && crate::agents::role(&input.role_id).is_none())
            || input.title.trim().is_empty()
            || input.title.chars().count() > 80
            || input.instructions.trim().is_empty()
            || input.acceptance.trim().is_empty()
            || [&input.source_task_id, &input.work_item]
                .into_iter()
                .flatten()
                .any(|s| s.trim().is_empty() || s.len() > 128 || s.chars().any(char::is_control))
            || serde_json::to_vec(value).map_err(HostError::storage)?.len() > 16 * 1024
        {
            return Err(HostError::new(
                "invalid_input",
                "委派角色、标题或16KiB输入范围不合法",
            ));
        }
        Ok(input)
    }

    // 调用方持原accepting门；选择后在释放门前提交原意图和Run，
    // 后一并发调用看到已入FIFO的前项，不另建预留或调度器。
    fn resolve_role(
        &mut self,
        group: &Group,
        host_tools: &[String],
        prior: &[Delegation],
    ) -> HostResult<()> {
        if self.role_id != "auto" {
            return Ok(());
        }
        let queue = group.queue.lock().unwrap_or_else(|e| e.into_inner());
        let candidates = self.candidate_roles.as_ref().expect("validated candidates");
        let required = self.required_tools.as_ref().expect("validated tools");
        self.role_id = candidates
            .iter()
            .filter(|role| {
                let allowed = group.config.allowed(role, host_tools);
                required.iter().all(|tool| allowed.contains(tool))
            })
            // 已完成任务已退出FIFO；同为空闲时仍参考原意图接受次数，
            // 避免稍晚补派又挤回刚结束的人，其他合格成员从未参与。
            // 历史均衡只用于队列等长，不能把真实空闲者让给繁忙者。
            .min_by_key(|role| {
                (
                    queue.get(role.as_str()).map_or(0, |q| q.len()),
                    prior.iter().filter(|d| &d.role_id == *role).count(),
                )
            })
            .cloned()
            .ok_or_else(|| {
                HostError::new(
                    "scope_denied",
                    "候选成员均不具备所列必需工具；请调整任务或候选，不会自动授予权限",
                )
            })?;
        Ok(())
    }
}
/// 功能：提供主角色唯一的薄委派工具定义。
/// 参数：无。
/// 返回：原ToolSpec，不注册第二套工具系统。
/// 错误：无。
/// 状态：不创建任务。
/// 并发与取消：只读。
/// 权限：仅可信协作主Run暴露；执行仍验证上下文。
/// 关联：需求/设计REQ-OP-022；DES-OP-012。
pub(crate) fn spec() -> ToolSpec {
    let mut spec = ToolSpec::new("delegate_task", "交给真实独立同事。每次必填role_id/title/instructions/acceptance；auto另必填candidate_roles和required_tools，纯文字也写required_tools=[]。存在多个就绪且互不依赖的实质交付物时，在同一响应发出多个本工具调用并明确wait_for_completion=false，先派齐再等结果；只有单项或确需结果才能推进时才同步等待。未指定执行人且无成果归属的独立工作使用role_id=auto，而不是固定常见专家；同时列candidate_roles及required_tools（纯文字用空数组），Host按真实工具和本组FIFO负载选择，回执role_id才是实际执行人。用户指定人员/已有明确归属时仍填固定role_id，不静默改派。默认等待结果；wait_for_completion=false只返回接受，须用delegation_status取得真实结果。source_task_id引用同父已完成前序task_id表达交接；context带真实资料；持续异步须已open_item的work_item。同角色串行，不覆盖权限。auto模式必须role_id=auto并同时带candidate_roles/required_tools；固定role_id禁止带这两个字段。用户要求不同同事/并行时不要对独立任务反复固定同一角色；无固定归属时用auto。");
    spec.input_schema = json!({"type":"object","properties":{"role_id":{"type":"string","enum":["auto","research","organize","draft","review","audit"]},"candidate_roles":{"type":"array","items":{"type":"string","enum":["research","organize","draft","review","audit"]},"minItems":1,"maxItems":5,"uniqueItems":true,"description":"仅auto：列出全部实际合格成员；先按FIFO长度，再按同父已接受任务数较少者，仍相同按此顺序。纯文字成果通常五位均可承担，不因角色简介只列一两人而让其他合格空闲者被排除。不能加入工具能力不足的成员，不在标题或指令写死姓名。"},"required_tools":{"type":"array","items":{"type":"string"},"maxItems":64,"description":"仅auto且必填：执行所必需的实际工具名；纯文字交付填[]。不会授予新权限。"},"title":{"type":"string","maxLength":80},"instructions":{"type":"string"},"acceptance":{"type":"string"},"context":{"type":"string"},"wait_for_completion":{"type":"boolean","default":true},"source_task_id":{"type":"string","maxLength":128},"work_item":{"type":"string","maxLength":128}},"required":["role_id","title","instructions","acceptance"],"additionalProperties":false});
    spec.concurrency = Some(openpaw_core::ToolConcurrency::ParallelSafe);
    spec
}
/// 功能：验证最终委派输入，不授权工作工具。
/// 参数：value为Hook后的最终输入。
/// 返回：结构与范围合法。
/// 错误：未知字段、角色或超限明确拒绝。
/// 状态：不写入，不创建子Run。
/// 并发与取消：纯计算。
/// 权限：固定一层角色，无递归和配置覆盖。
/// 关联：需求/设计REQ-OP-022；DES-OP-012。
pub(crate) fn validate(value: &Value) -> HostResult<()> {
    Input::parse(value).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusInput {
    task_ids: Vec<String>,
    #[serde(default = "no_wait")]
    wait_for: String,
}
fn no_wait() -> String {
    "none".into()
}
fn status_input(value: &Value) -> HostResult<StatusInput> {
    let input: StatusInput = serde_json::from_value(value.clone())
        .map_err(|_| HostError::new("invalid_input", "需要task_ids和可选wait_for=none/any/all"))?;
    if input.task_ids.is_empty()
        || input.task_ids.len() > 16
        || input
            .task_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 128)
        || !matches!(input.wait_for.as_str(), "none" | "any" | "all")
    {
        return Err(HostError::new(
            "invalid_input",
            "查询1至16个真实任务，wait_for须为none/any/all",
        ));
    }
    Ok(input)
}
/// 功能：声明查询或等待真实同事执行结果的原Host工具。
/// 参数：无。
/// 返回：原ToolSpec，不创建调度器。
/// 错误：无。
/// 状态：只读工具声明。
/// 并发与取消：等待不持工作区锁。
/// 权限：仅可信主Run可调用。
/// 关联：CHG-2026-0007第9.13节D107。
pub(crate) fn status_spec() -> ToolSpec {
    let mut spec=ToolSpec::new("delegation_status","查询已接受委派的真实结果。task_ids用delegate_task返回的task_id；wait_for=any等待其中至少一项终态，all等待全部，none立即读取。终态可能失败，不能只看工具成功。按返回title/role_id/work_item核对任务；下一轮只等待pending_task_ids，已终态项留在成果表，不再混入any反复唤醒。available_tools是实际权限，不授予新工具。每条结果含原delegate_call_id，state=completed只代表执行结束，业务验收还须核对真实成果。");
    spec.input_schema = json!({"type":"object","properties":{"task_ids":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string","maxLength":128}},"wait_for":{"type":"string","enum":["none","any","all"],"default":"none"}},"required":["task_ids"],"additionalProperties":false});
    spec
}
/// 功能：校验同事结果查询的最终参数。
/// 参数：value为Hook后的工具输入。
/// 返回：结构合法返回成功。
/// 错误：空列表、越界或未知字段返回invalid_input。
/// 状态：只读，不授予任务作用域。
/// 并发与取消：纯计算。
/// 权限：实际所属仍由Store核验。
/// 关联：CHG-2026-0007第9.13节D107。
pub(crate) fn validate_status(value: &Value) -> HostResult<()> {
    status_input(value).map(|_| ())
}

// 纠错只能引用调用方同父、同事项的真实意图，不能猜测依赖或自动替换输入。
fn source_reference_error(
    source: &str,
    intents: &[Delegation],
    item: &Option<String>,
) -> HostError {
    let hint = intents
        .iter()
        .find(|d| d.child_run == source && &d.work_item == item)
        .map(|d| format!("；你填写的是child_run_id，若该记录确为所需前序，请改用其task_id={}，仍须等待其成功完成", d.task_id))
        .unwrap_or_else(|| "；请从本事项delegate_task实际返回的task_id重新选择，不要使用child_run_id、其他事项或其他主任务的标识；不要通过删除source_task_id绕过成果依赖".into());
    HostError::new(
        "scope_denied",
        format!("交接来源必须是同一主任务的真实前序任务{hint}"),
    )
}

impl Host {
    /// 功能：读取同父真实子Run并可取消等待终态。
    /// 参数：execution为可信主角色，value为已绑定输入，cancel为本次调用取消。
    /// 返回：真实结果和原委派call ID，不把接受回执当完成。
    /// 错误：跨父、无任务、存储、取消或父期限错误。
    /// 状态：只读原Store，不重放或启动子Run。
    /// 并发与取消：先订阅再查询，无后台等待任务，不持资源锁。
    /// 权限：仅同主Run所属任务，不能用其他人的task ID读结果。
    /// 关联：CHG-2026-0007第9.13节D107。
    pub(crate) async fn delegation_status(
        &self,
        execution: &Execution,
        value: Value,
        cancel: CancellationToken,
    ) -> HostResult<String> {
        if execution.role != "lead" {
            return Err(HostError::new("scope_denied", "只有主角色可读取委派结果"));
        }
        let input = status_input(&value)?;
        loop {
            let notified = self.run_change();
            let parent = execution.group.parent.clone();
            let ids = input.task_ids.clone();
            let tools = self.inner.config.tools.clone();
            let config = execution.group.config.clone();
            let group = execution.group.clone();
            let (results,pending,finished,participants)=self.inner.store.call_priority(move |db| {
                let owner=db.get_run(&parent)?;
                let binding=db.binding(&owner.session_id)?;
                db.check_live_scope(&binding.scope)?;
                let intents=db.delegations(&parent)?;
                let mut results=Vec::new();let mut pending=Vec::new();let mut finished=0;
                for id in &ids {
                    let d=intents.iter().find(|d|&d.task_id==id).ok_or_else(||HostError::new("scope_denied","任务不属于本主Run"))?;
                    let run=match db.get_run(&d.child_run) {
                        Ok(r)=>r,
                        Err(e) if e.kind=="not_found"=>return Err(HostError::new("interrupted","只有未接受的持久委派意图，禁止自动重放")),
                        Err(e)=>return Err(e),
                    };
                    finished+=usize::from(run.state.is_terminal());
                    if !run.state.is_terminal() {pending.push(d.task_id.clone());}
                    let (position, predecessor)=group.queue_position(&d.role_id,&d.child_run);
                    let phase=if run.state.is_terminal() {"terminal"}
                        else if run.state==crate::RunState::Cancelling {"cancelling"}
                        else if run.tools.iter().any(|t|t.state=="awaiting_user") {"awaiting_user"}
                        else if run.tools.iter().any(|t|t.state=="awaiting_approval") {"awaiting_approval"}
                        else if run.tools.iter().any(|t|t.state=="awaiting_resource") {"awaiting_resource"}
                        else if run.state==crate::RunState::Accepted && predecessor.is_some() {"role_fifo"}
                        else if run.state==crate::RunState::Accepted {"awaiting_execution"}
                        else if run.tools.iter().any(|t|t.state=="running") {"tool_running"}
                        else {"running_next_action_unknown"};
                    results.push(json!({"task_id":d.task_id,"delegate_call_id":d.call_id,"role_id":d.role_id,"title":d.title,"work_item":d.work_item,"source_task_id":d.source_task_id,"available_tools":config.allowed(&d.role_id,&tools),"state":run.state,"summary":run.text,"error":run.error,"child_run_id":d.child_run,"scheduling":{"phase":phase,"role_queue_position":position,"predecessor_run_id":predecessor,"note":"队位是读取瞬间的事实；awaiting_execution不能据此区分角色许可、组容量或全局容量。"}}));
                }
                let participants=intents.iter().map(|d|d.role_id.clone()).collect::<std::collections::BTreeSet<_>>();
                Ok((results,pending,finished,participants))
            }).await?;
            if input.wait_for == "none"
                || input.wait_for == "any" && finished > 0
                || finished == input.task_ids.len()
            {
                return Ok(json!({"tasks":results,"pending_task_ids":pending,"participating_role_ids":participants,"participation_note":"真实受派子角色去重；不含主负责人，不代表所有任务已验收。","next_wait_hint":"仅等待pending_task_ids；已结束结果按task_id收取一次，有真实依赖的后续立即推进。"}).to_string());
            }
            tokio::select! {biased;
                _=cancel.cancelled()=>return Err(HostError::new("cancelled","委派结果等待已取消")),
                _=execution.group.cancel.cancelled()=>return Err(HostError::new("cancelled","主任务已取消")),
                _=execution.group.wait_deadline()=>return Err(HostError::new("run_timeout","协作期限已到")),
                _=notified=>{}
            }
        }
    }
    /// 功能：将一个真实主工具调用编排为原Host子Run并等待实际清理。
    /// 参数：execution为可信主运行上下文，binding为原最终输入，cancel为本次调用token。
    /// 返回：有界真实结果及稳定子Run/任务引用。
    /// 错误：取消、额度、冲突及持久化失败；不自动重放中断意图。
    /// 状态：先保存意图，再接受子Run；所有执行事实仍走原引擎。
    /// 并发与取消：不持Root锁等待；取消后等待子Run退出注册表。
    /// 权限：只允许lead、一层、同根、角色工具子集。
    /// 关联：需求/设计REQ-OP-022；CHG-2026-0007第9.7/9.8节。
    pub(crate) async fn delegate(
        &self,
        execution: &Execution,
        binding: ToolCallBinding,
        cancel: CancellationToken,
    ) -> HostResult<String> {
        if execution.role != "lead" || binding.run_id != execution.group.parent {
            return Err(HostError::new("scope_denied", "只有协作主Run可委派"));
        }
        let mut input = Input::parse(&binding.input)?;
        let parent_input = binding.input.clone();
        // 依赖无效时必须在等待执行名额之前拒绝，否则主角色会被一个
        // 尚未就绪的交接堵住，无法收取正在工作的同事结果。保存时仍复验。
        if let Some(source) = input.source_task_id.clone() {
            let parent = binding.run_id.clone();
            let item = input.work_item.clone();
            self.inner
                .store
                .call_priority(move |db| {
                    let intents = db.delegations(&parent)?;
                    let d = intents
                        .iter()
                        .find(|d| d.task_id == source)
                        .ok_or_else(|| source_reference_error(&source, &intents, &item))?;
                    if d.work_item != item {
                        return Err(HostError::new(
                            "scope_denied",
                            "交接来源与目标不属于同一事项",
                        ));
                    }
                    if db.get_run(&d.child_run)?.state != RunState::Completed {
                        return Err(HostError::new(
                            "request_conflict",
                            "前序尚未成功完成，不能交接其成果",
                        ));
                    }
                    Ok(())
                })
                .await?;
        }
        // 工具批次也可并发进入这里；先把意图与Run接受成对提交，避免
        // 下一项把这个极短的接受窗口误判为历史中断。门在等待执行前释放。
        let accepting = tokio::select! { biased;
            _=cancel.cancelled()=>return Err(HostError::new("cancelled","委派接受已取消")),
            _=execution.group.cancel.cancelled()=>return Err(HostError::new("cancelled","主任务已取消")),
            _=execution.group.wait_deadline()=>return Err(HostError::new("run_timeout","协作期限已到")),
            guard=execution.group.accepting.lock()=>guard,
        };
        let b = binding.clone();
        let limit = execution.group.config.max_delegations;
        let wait = input.wait_for_completion;
        let group = execution.group.clone();
        let host_tools = self.inner.config.tools.clone();
        let intent = self.inner.store.call_priority(move |db| {
            let parent = db.get_run(&b.run_id)?;
            let owner = db.binding(&parent.session_id)?;
            db.check_live_scope(&owner.scope)?;
            if owner.scope.namespace != "office" || parent.state.is_terminal() || parent.state == RunState::Cancelling { return Err(HostError::new("scope_denied", "主任务已结束或不是Office协作")); }
            // 幂等输入与原工具事实逐字段比对，不依赖易碰撞的展示摘要。
            let actual = db.events(&parent.session_id)?.into_iter().any(|e| matches!(e,openpaw_core::SessionEvent::Message(m) if m.content.iter().any(|c| matches!(c,openpaw_core::ContentBlock::ToolUse {id,name,input} if id==&b.call_id && name=="delegate_task" && input==&b.input))));
            if !actual { return Err(HostError::new("request_conflict", "委派与原账本最终工具输入不一致")); }
            let prior = db.delegations(&b.run_id)?;
            if let Some(d) = prior.iter().find(|d| d.call_id == b.call_id) {
                db.check_delegation_input(&b)?;
                return Ok((d.clone(), false));
            }
            // 必须在幂等读取之后选人：同call重取不能因负载变化换成员。
            input.resolve_role(&group, &host_tools, &prior)?;
            if limit != 0 && prior.len() >= limit { return Err(HostError::new("capacity_exceeded", "本轮委派次数已满")); }
            if group.cancel.is_cancelled() {return Err(HostError::new("cancelled","主任务已停止"));}
            {
                let state=group.continuous.lock().unwrap_or_else(|e|e.into_inner());
                if state.active && (!input.wait_for_completion || input.work_item.is_some())
                    && !input.work_item.as_ref().is_some_and(|id|state.open_items.contains(id)) {
                    return Err(HostError::new("request_conflict","持续异步委派必须使用continuous_task open_item已开放的work_item"));
                }
            }
            if let Some(source)=&input.source_task_id {
                let previous=prior.iter().find(|d|&d.task_id==source)
                    .ok_or_else(||source_reference_error(source,&prior,&input.work_item))?;
                if previous.work_item!=input.work_item {
                    return Err(HostError::new("scope_denied","交接来源与目标不属于同一事项"));
                }
                if db.get_run(&previous.child_run)?.state!=RunState::Completed {
                    return Err(HostError::new("request_conflict","前序尚未成功完成，不能交接其成果"));
                }
            }
            // 后续任务进入同一原Run注册表的FIFO；只拒绝历史未接受意图，
            // 不能把仍在执行的前项当成拒绝正常排队的理由。
            for existing in prior.iter().filter(|d| d.role_id == input.role_id) {
                match db.get_run(&existing.child_run) {
                    Ok(_) => {}
                    Err(e) if e.kind == "not_found" => return Err(HostError::new("role_conflict", "该成员存在未接受的历史意图，请核对后重新分配")),
                    Err(e) => return Err(e),
                }
            }
            let child_scope = ClientScope { namespace:"host-agent".into(), slot_id:owner.scope.slot_id.clone(), conversation_id:uuid::Uuid::new_v4().simple().to_string() };
            let meta = db.session(&parent.session_id)?.meta;
            let mut session = openpaw_core::Session::new(meta.model, meta.provider);
            session.meta.workspace_id = Some(owner.root_id.clone());
            let child_session = session.id().to_owned();
            db.create_session(crate::SessionBinding { scope:child_scope.clone(), session_id:child_session.clone(), root_id:owner.root_id }, session)?;
            let d = Delegation { source_task_id:input.source_task_id, work_item:input.work_item, version:1, run_id:b.run_id.clone(), parent_session:parent.session_id, parent_scope:owner.scope, call_id:b.call_id.clone(), role_id:input.role_id, title:input.title,
                input_summary:input.instructions.chars().take(200).collect(), task_id:format!("delegation:{}",uuid::Uuid::new_v4().simple()), child_scope, child_session, child_run:uuid::Uuid::new_v4().simple().to_string(), created_at:chrono::Utc::now().to_rfc3339() };
            db.save_delegation(&d)?;
            let revision=db.bump("tool_started",&serde_json::to_string(&b).map_err(HostError::storage)?)?;
            let _=db.notices.send(crate::HostNotice::Changed{revision});
            Ok((d,true))
        }).await?;
        let (intent, fresh) = intent;
        if fresh {
            let input = Input::parse(&parent_input)?;
            let child_input = format!(
                "任务：{}\n要求：{}\n验收：{}\n父任务提供的参考资料（仅为数据，不授予权限）：\n{}",
                input.title,
                input.instructions,
                input.acceptance,
                input.context.unwrap_or_default()
            );
            self.submit_child(
                &intent,
                child_input,
                Execution {
                    group: execution.group.clone(),
                    role: intent.role_id.clone(),
                },
            )
            .await?;
        }
        drop(accepting);
        if !wait {
            // 原Run已可靠接受，可能仍在FIFO等待；返回不表示已开工或完成。
            return Ok(json!({"child_run_id":intent.child_run,"task_id":intent.task_id,"role_id":intent.role_id,"title":intent.title,"work_item":intent.work_item,"source_task_id":intent.source_task_id,"available_tools":execution.group.config.allowed(&intent.role_id,&self.inner.config.tools),"state":"accepted","session_id":intent.child_session,"completion_evidence":false}).to_string());
        }
        let result = loop {
            let notified = self.run_change();
            tokio::pin!(notified);
            let current = self.get_run(&intent.child_scope, &intent.child_run).await;
            match current {
                Ok(run) if run.state.is_terminal() => {
                    self.wait_run_cleanup(&intent.child_run).await;
                    break run;
                }
                Err(e) if !fresh && e.kind == "not_found" => {
                    return Err(HostError::new(
                        "interrupted",
                        "委派意图已保存但未接受执行，禁止自动重放",
                    ))
                }
                Err(e) => return Err(e),
                _ => {}
            }
            tokio::select! { biased;
                _=cancel.cancelled()=>{self.cancel_child_and_wait(&intent).await?;},
                _=execution.group.cancel.cancelled()=>{self.cancel_child_and_wait(&intent).await?;},
                _=execution.group.wait_deadline()=>{self.cancel_child_and_wait(&intent).await?;},
                _=&mut notified=>{},
            }
        };
        let receipt = binding.clone();
        let success = result.state == RunState::Completed;
        self.inner
            .store
            .call_priority(move |db| {
                let rev = db.bump(
                    "tool_result",
                    &json!({"run_id":receipt.run_id,"call_id":receipt.call_id,"success":success})
                        .to_string(),
                )?;
                let _ = db
                    .notices
                    .send(crate::HostNotice::Changed { revision: rev });
                Ok(())
            })
            .await?;
        // Run文本已有统一显示上限；此处再次裁到6000字会让父角色在不知情时
        // 丢失后半篇正文，继而要求同事重做、审查不完整资料或保存残稿。
        Ok(json!({"child_run_id":intent.child_run,"task_id":intent.task_id,"role_id":intent.role_id,"title":intent.title,"work_item":intent.work_item,"source_task_id":intent.source_task_id,"available_tools":execution.group.config.allowed(&intent.role_id,&self.inner.config.tools),"state":result.state,"summary":result.text,"session_id":intent.child_session,"error":result.error}).to_string())
    }
    async fn cancel_child_and_wait(&self, d: &Delegation) -> HostResult<()> {
        self.cancel_run(&d.child_scope, &d.child_run).await?;
        self.wait_run_cleanup(&d.child_run).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn d176_auto_selection_uses_capabilities_and_original_fifo_without_changing_fixed_role() {
        let g = group();
        let host = vec!["read_file".into(), "web_search".into()];
        let value = json!({"role_id":"auto","candidate_roles":["research","organize","review"],"required_tools":["web_search"],"title":"独立来源核对","instructions":"按原始输入核对","acceptance":"返回真实来源"});
        let mut first = Input::parse(&value).unwrap();
        first.resolve_role(&g, &host, &[]).unwrap();
        assert_eq!(first.role_id, "research");
        g.enqueue("research", "first").unwrap();
        let mut second = Input::parse(&value).unwrap();
        second.resolve_role(&g, &host, &[]).unwrap();
        assert_eq!(second.role_id, "organize");
        g.enqueue("organize", "second").unwrap();
        let mut third = Input::parse(&value).unwrap();
        third.resolve_role(&g, &host, &[]).unwrap();
        assert_eq!(third.role_id, "review");
        let mut no_tools = value.clone();
        no_tools["required_tools"] = json!(["write_file"]);
        assert_eq!(
            Input::parse(&no_tools)
                .unwrap()
                .resolve_role(&g, &host, &[])
                .unwrap_err()
                .kind,
            "scope_denied"
        );
        let accepted = |role: &str, index: usize| Delegation {
            version: 1,
            source_task_id: None,
            work_item: None,
            run_id: "p".into(),
            parent_session: "parent".into(),
            parent_scope: ClientScope {
                namespace: "office".into(),
                slot_id: "team".into(),
                conversation_id: "conversation".into(),
            },
            call_id: format!("call-{role}-{index}"),
            role_id: role.into(),
            title: "原有真实委派".into(),
            input_summary: "有限成果".into(),
            task_id: format!("task-{role}-{index}"),
            child_scope: ClientScope {
                namespace: "host-agent".into(),
                slot_id: "team".into(),
                conversation_id: format!("child-{role}-{index}"),
            },
            child_session: format!("session-{role}-{index}"),
            child_run: format!("run-{role}-{index}"),
            created_at: "2026-11-05T00:00:00Z".into(),
        };
        let prior = [
            accepted("research", 1),
            accepted("research", 2),
            accepted("organize", 3),
        ];
        g.dequeue("research", "first");
        g.dequeue("organize", "second");
        let mut later = Input::parse(&value).unwrap();
        later.resolve_role(&g, &host, &prior).unwrap();
        assert_eq!(
            later.role_id, "review",
            "已完成离队后，优先同为空闲但尚未参与的合格成员"
        );
        g.enqueue("review", "occupied").unwrap();
        let mut idle_first = Input::parse(&value).unwrap();
        idle_first.resolve_role(&g, &host, &prior).unwrap();
        assert_eq!(
            idle_first.role_id, "organize",
            "真实FIFO优先于历史均衡，等空闲再比较接受次数"
        );
        g.enqueue("research", "first").unwrap();
        for changes in [
            json!({"role_id":"lead"}),
            json!({"candidate_roles":["lead"]}),
            json!({"candidate_roles":["research","research"]}),
            json!({"required_tools":null}),
            json!({"candidate_roles":[]}),
        ] {
            let mut bad = value.clone();
            for (k, v) in changes.as_object().unwrap() {
                bad[k] = v.clone();
            }
            assert!(Input::parse(&bad).is_err());
        }
        let mut fixed=Input::parse(&json!({"role_id":"research","title":"固定责任","instructions":"必须原执行人","acceptance":"实际结果"})).unwrap();
        fixed.resolve_role(&g, &host, &[]).unwrap();
        assert_eq!(fixed.role_id, "research");
        let mut mixed = value.clone();
        mixed["role_id"] = json!("research");
        let error = Input::parse(&mixed).err().unwrap();
        assert_eq!(error.kind, "invalid_input");
        assert!(error.message.contains("固定role_id不能附"));
        assert!(error.message.contains("将role_id改为auto"));
        assert_eq!(g.queue_position("research", "first").0, Some(1));
    }
    #[test]
    fn d116_source_hint_distinguishes_task_from_run_without_cross_item_disclosure() {
        let scope = json!({"namespace":"office","slot_id":"office-1","conversation_id":"c"});
        let d: Delegation = serde_json::from_value(json!({"version":1,"run_id":"parent","parent_session":"ps","parent_scope":scope,"call_id":"call","role_id":"organize","title":"真实有限成果","input_summary":"实际资料","task_id":"delegation:trusted","child_scope":scope,"child_session":"cs","child_run":"child-run","created_at":"2026-10-28T00:00:00Z","work_item":"a"})).unwrap();
        let local =
            source_reference_error("child-run", std::slice::from_ref(&d), &Some("a".into()));
        assert_eq!(local.kind, "scope_denied", "纠错仍拒绝执行，不能自动换输入");
        assert!(local.message.contains("task_id=delegation:trusted"));
        assert!(local.message.contains("仍须等待其成功完成"));
        for (source, item) in [
            ("child-run", Some("b".into())),
            ("unknown", Some("a".into())),
            ("child-run", None),
        ] {
            let rejected = source_reference_error(source, std::slice::from_ref(&d), &item);
            assert_eq!(rejected.kind, "scope_denied");
            assert!(!rejected.message.contains("delegation:trusted"));
            assert!(rejected.message.contains("不要通过删除source_task_id"));
        }
    }

    fn group() -> Arc<Group> {
        let control = RunControl {
            run_id: "p".into(),
            cancel: CancellationToken::new(),
            fault: Arc::new(Mutex::new(None)),
        };
        Group::new(
            "p".into(),
            AgentsConfig {
                max_model_requests: 2,
                max_output_tokens: 100,
                ..Default::default()
            },
            &control,
            60,
        )
    }
    #[test]
    fn two_children_compete_for_last_output_reservation_without_overspend() {
        let g = group();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let g = g.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    g.reserve(80).is_ok()
                })
            })
            .collect();
        barrier.wait();
        let won = tasks
            .into_iter()
            .filter_map(|t| t.join().ok())
            .filter(|v| *v)
            .count();
        assert_eq!(won, 1);
        assert_eq!(g.summary()["usage_incomplete"], true);
        g.settle(
            80,
            openpaw_core::Usage {
                input_tokens: 10,
                output_tokens: 30,
                ..Default::default()
            },
        );
        assert_eq!(g.summary()["usage_incomplete"], false);
        assert!(g.reserve(60).is_ok());
        assert!(g.reserve(1).is_err());
    }
    #[test]
    fn missing_usage_keeps_reserved_share_and_invalid_delegate_input_is_rejected() {
        let g = group();
        g.reserve(90).unwrap();
        g.settle(90, Default::default());
        assert!(g.reserve(11).is_err());
        assert_eq!(g.summary()["unknown_requests"], 1);
        let missing = json!({"role_id":"auto","title":"整理","instructions":"文字"});
        let error = Input::parse(&missing).err().unwrap();
        assert!(error.message.contains("acceptance") && error.message.contains("未创建任务"));
        assert!(!error.message.contains("整理"));
        let auto = json!({"role_id":"auto","candidate_roles":["draft"],"title":"整理","instructions":"文字","acceptance":"完成正文"});
        let error = Input::parse(&auto).err().unwrap();
        assert!(error.message.contains("required_tools=[]"));
        let mut complete = auto.clone();
        complete["required_tools"] = json!([]);
        assert!(validate(&complete).is_ok());
        let valid = json!({"role_id":"research","title":"核对资料","instructions":"只读取","acceptance":"有来源"});
        assert!(validate(&valid).is_ok());
        for (field, value) in [
            ("root", json!("/")),
            ("role_id", json!("lead")),
            ("provider", json!("x")),
            ("context", json!("长".repeat(6000))),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            assert!(validate(&bad).is_err(), "{field}");
        }
    }
}
