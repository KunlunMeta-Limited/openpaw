//! 职责：实现原ApprovalManager的持久、精确绑定与一次消费。
//! 输入输出：原引擎请求到pending事实及人工决定。
//! 依赖：Host单写Store、原资源锁与调用上下文。
//! 禁止职责：不建立永久授权，不在等待用户期间持根锁。
use crate::{resource::Root, store::Store, types::*};
use async_trait::async_trait;
use openpaw_core::{ApprovalManager, ApprovalRequest, ApprovalResponse, ToolCallBinding};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
/// 用途：每Run独立的取消和存储故障锁存。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct RunControl {
    /// 用途：Host分配的稳定Run标识，每次调用独立。
    /// 字段：String，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) run_id: String,
    /// 用途：本Run独立取消token，不能取消其他Session。
    /// 字段：CancellationToken，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) cancel: CancellationToken,
    /// 用途：本Run关键存储故障锁存，优先于模型完成文本。
    /// 字段：Arc<Mutex<Option<HostError>>>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) fault: Arc<Mutex<Option<HostError>>>,
}
impl RunControl {
    /// 功能：锁存关键运行错误并通知取消。
    /// 参数：error为安全宿主错误。
    /// 返回：无返回值。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：故障优先于模型完成文本；取消后仍等待工具清理。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn fail(&self, error: HostError) {
        *self.fault.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        self.cancel.cancel();
    }
}
#[derive(Clone, PartialEq)]
struct Precondition {
    path: std::path::PathBuf,
    content: Option<String>,
}
/// 用途：原ApprovalManager的持久宿主实现。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct HostApproval {
    /// 用途：Host单写存储命令入口，调用方等待写入完成。
    /// 字段：Store，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) store: Store,
    /// 用途：本Run固定的规范工作根与协作锁。
    /// 字段：Arc<Root>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) root: Arc<Root>,
    /// 用途：本Run独立取消及故障锁存上下文。
    /// 字段：RunControl，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) control: RunControl,
    /// 用途：人工审批等待预算，到期拒绝。
    /// 字段：Duration，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) timeout: Duration,
    before: Mutex<HashMap<String, Option<Precondition>>>,
    continuous: Option<Arc<crate::orchestration::Group>>,
}
impl HostApproval {
    /// 功能：装配每Run的原审批trait实现。
    /// 参数：store/root/control/timeout为既有宿主能力及审批预算。
    /// 返回：独立HostApproval。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不创建永久批准；共享根锁不在用户等待期间持有。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) fn new(
        store: Store,
        root: Arc<Root>,
        control: RunControl,
        timeout: Duration,
    ) -> Self {
        Self {
            store,
            root,
            control,
            timeout,
            before: Mutex::new(HashMap::new()),
            continuous: None,
        }
    }
    /// 功能：关联本Run的可信持续任务授权策略。
    /// 参数：group为原父组或None。
    /// 返回：原审批器，不另建批准系统。
    /// 错误：无。
    /// 状态：只注入关联，start前不自动放行。
    /// 并发与取消：主子共享启停事实，停止后不新授权。
    /// 权限：仅配置的产物子目录及媒体/文件工具。
    /// 关联：CHG-2026-0007第9.12节。
    pub(crate) fn with_continuous(
        mut self,
        group: Option<Arc<crate::orchestration::Group>>,
    ) -> Self {
        self.continuous = group;
        self
    }
    fn automatic(&self, b: &ToolCallBinding) -> bool {
        let Some(group) = &self.continuous else {
            return false;
        };
        let Some(config) = group
            .config
            .continuous
            .as_ref()
            .filter(|c| c.enabled && c.auto_approve)
        else {
            return false;
        };
        if group.cancel.is_cancelled()
            || !group
                .continuous
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
            || !matches!(
                b.tool_name.as_str(),
                "write_file" | "edit_file" | "notebook_edit" | "generate_image" | "render_visual"
            )
        {
            return false;
        }
        let Some(path) = b.input["path"].as_str() else {
            return false;
        };
        let Ok(target) = openpaw_tools::safety::resolve(&self.root.path, path) else {
            return false;
        };
        let Ok(output) = openpaw_tools::safety::resolve(&self.root.path, &config.output_dir) else {
            return false;
        };
        target != output && target.starts_with(output)
    }
    async fn capture(&self, b: &ToolCallBinding) -> HostResult<Option<Precondition>> {
        let path = match b.tool_name.as_str() {
            "write_file" | "edit_file" | "notebook_edit" | "generate_image" | "render_visual" => b
                .input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HostError::new("invalid_input", "写工具缺少path"))?,
            "todo_add" | "todo_update" => ".openpaw/todos.jsonl",
            _ => return Ok(None),
        };
        let resolved = openpaw_tools::safety::resolve(&self.root.path, path)
            .map_err(|_| HostError::new("scope_denied", "工具路径越界"))?;
        let content = match tokio::fs::symlink_metadata(&resolved).await {
            Ok(meta) if matches!(b.tool_name.as_str(), "generate_image" | "render_visual") => {
                if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
                    return Err(HostError::new(
                        "invalid_input",
                        "已有媒体不是受支持大小的普通文件",
                    ));
                }
                let bytes = tokio::fs::read(&resolved)
                    .await
                    .map_err(HostError::storage)?;
                Some(format!(
                    "{:?}",
                    ring::digest::digest(&ring::digest::SHA256, &bytes)
                ))
            }
            Ok(_) => Some(
                openpaw_tools::fs::read::read_text_bounded(&self.root.path, path, 256 * 1024)
                    .await
                    .map_err(|_| {
                        HostError::new("invalid_input", "前置文件非文本、过大或不可读取")
                    })?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(HostError::new("invalid_input", "无法校验前置文件")),
        };
        Ok(Some(Precondition {
            path: resolved,
            content,
        }))
    }
    /// 功能：在单写协调序列原子领取一次批准。
    /// 参数：b为原core完整最终输入绑定。
    /// 返回：领取成功后的允许执行。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：过期、重复或绑定不一致拒绝；消费先于副作用。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) async fn consume(&self, b: ToolCallBinding) -> HostResult<()> {
        let run = self.control.run_id.clone();
        self.store
            .call_priority(move |db| {
                let current = db.get_run(&run)?;
                db.binding(&current.session_id)?; // 同一Store序列检查删除意图，旧批准不能跨删除消费。
                let mut a = db
                    .approvals()?
                    .into_iter()
                    .find(|a| a.binding.run_id == run && a.binding.call_id == b.call_id)
                    .ok_or_else(|| HostError::new("approval_conflict", "缺少持久审批"))?;
                if a.binding != b
                    || a.status != "approved"
                    || a.consumed
                    || chrono::Utc::now().timestamp_millis() >= a.expires_at_ms
                {
                    return Err(HostError::new(
                        "approval_conflict",
                        "授权不匹配、过期或已消费",
                    ));
                }
                a.consumed = true;
                db.save_approval(a)?;
                Ok(())
            })
            .await
    }
    /// 功能：获锁后检查审批前文件前置条件。
    /// 参数：b为原最终输入绑定。
    /// 返回：内容与规范路径未变时成功。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：变化则旧批准失效，不自动修改或覆盖外部内容。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub(crate) async fn revalidate(&self, b: &ToolCallBinding) -> HostResult<()> {
        let before = self
            .before
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&b.call_id)
            .cloned()
            .ok_or_else(|| HostError::new("approval_conflict", "缺少批准前视图"))?;
        if before != self.capture(b).await? {
            return Err(HostError::new(
                "approval_conflict",
                "文件在审批后变化，请重新确认",
            ));
        }
        Ok(())
    }
    async fn wait(&self, req: ApprovalRequest) -> HostResult<ApprovalResponse> {
        let binding = req
            .binding
            .ok_or_else(|| HostError::new("invalid_input", "审批缺少调用绑定"))?;
        if binding.run_id != self.control.run_id
            || binding.tool_name != req.tool_name
            || binding.input != req.input
        {
            return Err(HostError::new("scope_denied", "审批绑定无效"));
        }
        let mut rx = self.store.call(|db| Ok(db.notices.subscribe())).await?;
        let before = {
            let _guard = self.root.acquire(false, &self.control.cancel).await?;
            self.capture(&binding).await?
        };
        self.before
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(binding.call_id.clone(), before);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let query_id = id.clone();
        let timeout_ms = i64::try_from(self.timeout.as_millis()).unwrap_or(i64::MAX);
        let automatic = self.automatic(&binding);
        let auto_group = self.continuous.clone();
        self.store
            .call_priority(move |db| {
                let run = db.get_run(&binding.run_id)?;
                db.binding(&run.session_id)?;
                if db.approvals()?.iter().any(|a| {
                    a.binding.run_id == binding.run_id && a.binding.call_id == binding.call_id
                }) {
                    return Err(HostError::new("approval_conflict", "重复工具调用ID"));
                }
                let automatic=automatic && auto_group.as_ref().is_some_and(|g|!g.cancel.is_cancelled()&&g.continuous.lock().unwrap_or_else(|e|e.into_inner()).active);
                if automatic {
                    db.bump("approval_policy",&serde_json::json!({"version":1,"run_id":binding.run_id,"call_id":binding.call_id,"decision":"approved","source":"continuous_output_policy"}).to_string())?;
                }
                db.save_approval(ApprovalView {
                    approval_id: id,
                    binding,
                    status: if automatic {"approved"} else {"pending"}.into(),
                    consumed: false,
                    revision: 0,
                    expires_at_ms: chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_add(timeout_ms),
                })?;
                Ok(())
            })
            .await?;
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);
        loop {
            let id = query_id.clone();
            let state = self
                .store
                .call(move |db| {
                    db.approvals()?
                        .into_iter()
                        .find(|a| a.approval_id == id)
                        .ok_or_else(|| HostError::new("not_found", "审批不存在"))
                })
                .await?;
            if state.status != "pending" {
                return Ok(if state.status == "approved" {
                    ApprovalResponse::Approved
                } else {
                    ApprovalResponse::Denied {
                        reason: state.status,
                    }
                });
            }
            let status = tokio::select! {biased;_=self.control.cancel.cancelled()=>Some("cancelled"),_=&mut deadline=>Some("expired"),_=rx.recv()=>None};
            if let Some(status) = status {
                let id = query_id.clone();
                self.store
                    .call_priority(move |db| {
                        if let Some(mut a) =
                            db.approvals()?.into_iter().find(|a| a.approval_id == id)
                        {
                            if a.status == "pending" {
                                a.status = status.into();
                                db.save_approval(a)?;
                            }
                        }
                        Ok(())
                    })
                    .await?;
                return Ok(ApprovalResponse::Denied {
                    reason: status.into(),
                });
            }
        }
    }
}
#[async_trait]
impl ApprovalManager for HostApproval {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        match self.wait(req).await {
            Ok(result) => result,
            Err(error) => {
                if error.kind == "storage_unavailable" {
                    self.control.fail(error);
                }
                ApprovalResponse::Denied {
                    reason: "审批未获有效批准".into(),
                }
            }
        }
    }
}
