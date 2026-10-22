//! 职责：管理独立AgentLoop的接受、容量、取消、恢复视图与关闭。
//! 输入输出：可信作用域到共享Session/Run和可等待终态。
//! 依赖：原引擎、原Provider装配、Host Store和工具包装。
//! 禁止职责：不实现客户端协议、不在注册表锁内等待推理或磁盘。
use crate::{
    approval::{HostApproval, RunControl},
    config::valid_id,
    resource::{self, Root},
    store::Store,
    tool_host::HostTools,
    types::*,
    HostConfig,
};
use async_trait::async_trait;
use futures::FutureExt;
use openpaw_core::{LlmProvider, Message, Session, SessionEvent, ToolExecutor};
use openpaw_engine::{AgentEvent, AgentLoop, RunContext, RunObserver, RunStatus};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{broadcast, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
/// 用途：两个客户端将共同使用的协议无关宿主。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct Host {
    /// 用途：共享服务所有权，Run与订阅复用同一底座。
    /// 字段：Arc<Inner>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) inner: Arc<Inner>,
}
/// 用途：共享运行服务的唯一资源注册表。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) struct Inner {
    /// 用途：已校验本机配置，不能由HTTP请求覆写。
    /// 字段：HostConfig，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) config: HostConfig,
    /// 用途：允许根标识到既存绝对目录，同一真实根合并资源。
    /// 字段：BTreeMap<String, Arc<Root>>，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) roots: BTreeMap<String, Arc<Root>>,
    /// 用途：Host单写存储命令入口，调用方等待写入完成。
    /// 字段：Store，宿主类型字段。
    /// 来源去向：共享Host内部与可信Rust适配器。
    /// 约束：只在已授权会话及配置根内使用。
    /// 稳定性：T3新增中性字段。
    /// 关联：需求/设计DES-OP-006。
    pub(crate) store: Store,
    provider: Arc<dyn LlmProvider>,
    model: String,
    state: Mutex<Registry>,
    unavailable: AtomicBool,
    shutdown_result: tokio::sync::Mutex<Option<HostResult<()>>>,
    idle: Notify,
    office: tokio::sync::Mutex<Option<crate::office::Coordinator>>,
    notices: broadcast::Sender<HostNotice>,
    live: Arc<Mutex<HashMap<String, (String, bool)>>>,
}
struct Active {
    session: String,
    scope: ClientScope,
    control: RunControl,
}
#[derive(Default)]
struct Registry {
    closing: bool,
    active: HashMap<String, Active>,
}
struct Observer {
    store: Store,
    session_id: String,
    control: RunControl,
}
#[async_trait]
impl RunObserver for Observer {
    async fn record(&mut self, mut event: SessionEvent) -> openpaw_core::Result<()> {
        if let SessionEvent::RunFinished {
            ref mut status,
            ref mut error,
            ..
        } = event
        {
            if self
                .control
                .fault
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                *status = "failed".into();
                *error = Some("宿主关键事实保存失败".into());
            }
        }
        let id = self.session_id.clone();
        if let Err(e) = self.store.call(move |db| db.append(&id, event)).await {
            self.control.fail(e);
            return Err(openpaw_core::Error::Store("运行事实保存失败".into()));
        }
        Ok(())
    }
}
impl Host {
    /// 功能：调用原Provider装配启动共享Host。
    /// 参数：config为显式已授权本机配置。
    /// 返回：完成恢复的Host。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不通过修改进程环境装配Provider，密钥不进入事件。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn start(config: HostConfig) -> HostResult<Self> {
        config.validate()?;
        let key = config
            .api_key_env
            .as_ref()
            .and_then(|name| std::env::var(name).ok());
        let (provider, _) = openpaw_llm::assembly::assemble_provider(
            &config.provider,
            config.model.as_deref(),
            key,
            config.base_url.as_deref(),
        )
        .map_err(|_| HostError::new("invalid_input", "Provider配置不合法或缺少凭据"))?;
        Self::with_provider(config, provider).await
    }
    /// 功能：用指定原Provider启动Host，供确定性验收及装配复用。
    /// 参数：config为本机配置，provider为原LlmProvider实例。
    /// 返回：已恢复的共享Host。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：Provider可共享，每Run必须新建AgentLoop；取得home所有权。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn with_provider(
        config: HostConfig,
        provider: Arc<dyn LlmProvider>,
    ) -> HostResult<Self> {
        config.validate()?;
        let model = openpaw_llm::assembly::resolve_model(&*provider, config.model.as_deref())
            .map_err(|_| HostError::new("invalid_input", "模型不能为空"))?;
        let cfg = config.clone();
        let (home, roots) = tokio::task::spawn_blocking(move || resource::prepare(&cfg))
            .await
            .map_err(HostError::storage)??;
        let (notices, _) = broadcast::channel(256);
        let live = Arc::new(Mutex::new(HashMap::new()));
        let store = Store::open(
            home,
            roots
                .iter()
                .map(|(id, r)| (id.clone(), r.path.clone()))
                .collect(),
            notices.clone(),
            live.clone(),
        )
        .await?;
        // 持久绑定不可随新配置悄悄换根；重新启动也须遵守原绑定。
        let bindings = store.call(|db| db.bindings()).await?;
        for b in bindings {
            if config.slots.get(&b.scope.slot_id) != Some(&b.root_id) {
                store.close().await?;
                return Err(HostError::new("scope_denied", "配置改变了既有会话工作根"));
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                roots,
                store,
                provider,
                model,
                state: Mutex::new(Registry::default()),
                unavailable: AtomicBool::new(false),
                shutdown_result: tokio::sync::Mutex::new(None),
                idle: Notify::new(),
                office: tokio::sync::Mutex::new(None),
                notices,
                live,
            }),
        })
    }
    /// 功能：按可信范围返回或创建固定Session绑定。
    /// 参数：scope为完整命名空间、槽和会话。
    /// 返回：原Session的固定绑定。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只由配置决定工作根；不能覆盖已有范围。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn open_session(&self, scope: ClientScope) -> HostResult<SessionBinding> {
        valid_scope(&scope)?;
        if self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closing
        {
            return Err(HostError::new("cancelled", "宿主正在关闭"));
        }
        let root = self
            .inner
            .config
            .slots
            .get(&scope.slot_id)
            .cloned()
            .ok_or_else(|| HostError::new("scope_denied", "办公室未授权"))?;
        let model = self.inner.model.clone();
        let provider = self.inner.config.provider.clone();
        self.inner
            .store
            .call(move |db| {
                if let Some(binding) = db.bindings()?.into_iter().find(|b| b.scope == scope) {
                    return Ok(binding);
                }
                let mut session = Session::new(model, provider);
                session.meta.workspace_id = Some(root.clone());
                let binding = SessionBinding {
                    scope,
                    session_id: session.id().into(),
                    root_id: root,
                };
                db.create_session(binding.clone(), session)?;
                Ok(binding)
            })
            .await
    }
    /// 功能：校验作用域并恢复原Session上下文。
    /// 参数：scope为可信范围，id为会话ID。
    /// 返回：原core Session。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读，不重放工具或改工作根。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn get_session(&self, scope: &ClientScope, id: &str) -> HostResult<Session> {
        let scope = scope.clone();
        let id = id.to_owned();
        self.inner
            .store
            .call(move |db| {
                check_scope(&db.binding(&id)?, &scope)?;
                db.session(&id)
            })
            .await
    }
    /// 功能：可靠接受输入后启动独立Run。
    /// 参数：scope/session_id为固定范围，input为本次用户文本。
    /// 返回：持久accepted Run视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：预留槽/会话/容量后保存输入；失败不启动工具。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn submit_run(
        &self,
        scope: &ClientScope,
        session_id: &str,
        input: String,
    ) -> HostResult<RunView> {
        self.submit_with_history(scope, session_id, input, None)
            .await
    }
    /// 功能：在同一运行预留内核对或导入客户端完整文本历史。
    /// 参数：scope/session_id为固定范围，input为新输入，history为可选外部历史。
    /// 返回：输入与历史可靠保存后的accepted Run。
    /// 错误：冲突、容量、历史不一致或持久化错误。
    /// 状态：复用原接受流程与JSONL；外部文本不产生工具事实。
    /// 并发与取消：先预留会话和槽，再核对，禁止首请求交错导入。
    /// 权限：只允许配置范围，不从历史恢复任何授权。
    /// 关联：需求/设计REQ-OP-010；DES-OP-003/007。
    pub(crate) async fn submit_with_history(
        &self,
        scope: &ClientScope,
        session_id: &str,
        input: String,
        history: Option<Vec<Message>>,
    ) -> HostResult<RunView> {
        if self.inner.unavailable.load(Ordering::Acquire) {
            return Err(HostError::new(
                "storage_unavailable",
                "宿主保存故障已锁存，请修复并重启",
            ));
        }
        if input.trim().is_empty() || input.len() > 1024 * 1024 {
            return Err(HostError::new("invalid_input", "输入为空或超过1MiB"));
        }
        let sid = session_id.to_owned();
        let s = scope.clone();
        let binding = self
            .inner
            .store
            .call(move |db| {
                let b = db.binding(&sid)?;
                check_scope(&b, &s)?;
                Ok(b)
            })
            .await?;
        let root = self
            .inner
            .roots
            .get(&binding.root_id)
            .cloned()
            .ok_or_else(|| HostError::new("scope_denied", "会话工作根不再授权"))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let control = RunControl {
            run_id: id.clone(),
            cancel: CancellationToken::new(),
            fault: Arc::new(Mutex::new(None)),
        };
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closing {
                return Err(HostError::new("cancelled", "宿主正在关闭"));
            }
            if state.active.values().any(|a| a.session == session_id) {
                return Err(HostError::new("session_busy", "会话已有活动运行"));
            }
            if state
                .active
                .values()
                .any(|a| a.scope.namespace == scope.namespace && a.scope.slot_id == scope.slot_id)
            {
                return Err(HostError::new("slot_busy", "办公室已有活动运行"));
            }
            if state.active.len() >= self.inner.config.max_active_runs {
                return Err(HostError::new("capacity_exceeded", "宿主运行容量已满"));
            }
            state.active.insert(
                id.clone(),
                Active {
                    session: session_id.into(),
                    scope: scope.clone(),
                    control: control.clone(),
                },
            );
        }
        let view = RunView {
            run_id: id.clone(),
            session_id: session_id.into(),
            state: RunState::Accepting,
            revision: 0,
            text: String::new(),
            error: None,
            tools: Vec::new(),
            text_truncated: false,
        };
        let accepted = self
            .inner
            .store
            .call(move |db| {
                if let Some(history) = history {
                    db.reconcile_history(&view.session_id, &history)?;
                }
                let offset = db.events(&view.session_id)?.len();
                let mut view = db.save_run(view, offset)?;
                db.append(
                    &view.session_id,
                    SessionEvent::Message(Message::user_text(input)),
                )?;
                view.state = RunState::Accepted;
                let view = db.save_run(view, offset)?;
                let session = db.session(&view.session_id)?;
                Ok((view, session))
            })
            .await;
        let (view, session) = match accepted {
            Ok(v) => v,
            Err(e) => {
                if e.kind == "storage_unavailable" {
                    self.inner.unavailable.store(true, Ordering::Release);
                }
                let rid = id.clone();
                let error = e.clone();
                let _ = self
                    .inner
                    .store
                    .call(move |db| {
                        if let Ok(mut r) = db.get_run(&rid) {
                            r.state = RunState::Failed;
                            r.error = Some(error);
                            db.save_run(r, 0)?;
                        }
                        Ok(())
                    })
                    .await;
                self.release(&id);
                return Err(e);
            }
        };
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (String::new(), false));
        let host = self.clone();
        let run = view.clone();
        tokio::spawn(async move {
            let future = host.execute(run.clone(), session, root, control.clone());
            if std::panic::AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .is_err()
            {
                control.fail(HostError::new("interrupted", "运行任务异常，禁止自动重放"));
                let mut failed = run;
                failed.state = RunState::Failed;
                failed.error = Some(HostError::new("interrupted", "运行任务异常"));
                let _ = host
                    .inner
                    .store
                    .call(move |db| {
                        db.save_run(failed, 0)?;
                        Ok(())
                    })
                    .await;
            }
            host.inner
                .live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            host.release(&id);
        });
        Ok(view)
    }
    async fn execute(
        &self,
        mut view: RunView,
        mut session: Session,
        root: Arc<Root>,
        control: RunControl,
    ) {
        let result = async {
            view.state = RunState::Running;
            let v = view.clone();
            self.inner.store.call(move |db| db.save_run(v, 0)).await?;
            let approval = Arc::new(HostApproval::new(
                self.inner.store.clone(),
                root.clone(),
                control.clone(),
                Duration::from_secs(self.inner.config.approval_timeout_secs),
            ));
            let tools = Arc::new(HostTools {
                registry: openpaw_tools::ToolRegistry::new(&root.path),
                allowed: self.inner.config.tools.clone(),
                root: root.clone(),
                approval: approval.clone(),
                control: control.clone(),
                store: self.inner.store.clone(),
            });
            let specs = tools
                .list_tools()
                .await
                .map_err(|_| HostError::new("invalid_input", "工具装配失败"))?;
            let engine = AgentLoop::new(self.inner.provider.clone())
                .with_model(&self.inner.model)
                .with_tool_host(tools)
                .with_approval_manager(approval)
                .with_parallel_tools(false)
                .with_stream_timeout(Duration::from_secs(self.inner.config.stream_idle_secs));
            let context = RunContext {
                run_id: view.run_id.clone(),
                cancel: control.cancel.clone(),
                total_timeout: Some(Duration::from_secs(self.inner.config.run_timeout_secs)),
                tool_timeout: Some(Duration::from_secs(self.inner.config.tool_timeout_secs)),
            };
            let mut observer = Observer {
                store: self.inner.store.clone(),
                session_id: view.session_id.clone(),
                control: control.clone(),
            };
            let id = view.run_id.clone();
            let inner = self.inner.clone();
            let outcome = engine
                .run_with_context(
                    &mut session,
                    Some(format!(
                        "Work only inside the authorized workspace: {}",
                        root.path.display()
                    )),
                    &specs,
                    &context,
                    &mut observer,
                    &mut move |event| {
                        if let AgentEvent::TextDelta { text } = event {
                            let mut live = inner.live.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(buffer) = live.get_mut(&id) {
                                if !buffer.1 && buffer.0.len() + text.len() <= 4 * 1024 * 1024 {
                                    buffer.0.push_str(&text);
                                } else {
                                    buffer.1 = true;
                                }
                            }
                            let _ = inner.notices.send(HostNotice::Text);
                        }
                    },
                )
                .await;
            view.state = match outcome.status {
                RunStatus::Completed => RunState::Completed,
                RunStatus::Failed => RunState::Failed,
                RunStatus::Cancelled => RunState::Cancelled,
                RunStatus::TimedOut => RunState::TimedOut,
            };
            if outcome.error.is_some() {
                view.error = Some(HostError::new(
                    match outcome.status {
                        RunStatus::Cancelled => "cancelled",
                        RunStatus::TimedOut => "timeout",
                        _ => "interrupted",
                    },
                    "运行未正常完成；详情见会话事实",
                ));
            }
            Ok::<(), HostError>(())
        }
        .await;
        if let Err(e) = result {
            control.fail(e);
        }
        if let Some(error) = control
            .fault
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            if error.kind == "storage_unavailable" {
                self.inner.unavailable.store(true, Ordering::Release);
            }
            view.state = RunState::Failed;
            view.error = Some(error);
        }
        let id = view.run_id.clone();
        if let Err(error) = self
            .inner
            .store
            .call(move |db| {
                for mut a in db.approvals()? {
                    if a.binding.run_id == id
                        && (a.status == "pending" || a.status == "approved" && !a.consumed)
                    {
                        a.status = "cancelled".into();
                        db.save_approval(a)?;
                    }
                }
                db.save_run(view, 0)?;
                Ok(())
            })
            .await
        {
            self.inner.unavailable.store(true, Ordering::Release);
            control.fail(error);
        }
    }
    fn release(&self, id: &str) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(id);
        self.inner.idle.notify_waiters();
    }
    /// 功能：校验会话范围后读取Run视图。
    /// 参数：scope为可信范围，id为稳定RunID。
    /// 返回：真实状态及展示文本/工具视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读；无法可靠保存关键状态时明确错误。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn get_run(&self, scope: &ClientScope, id: &str) -> HostResult<RunView> {
        let scope = scope.clone();
        let id = id.to_owned();
        let view = self
            .inner
            .store
            .call(move |db| {
                let run = db.get_run(&id)?;
                check_scope(&db.binding(&run.session_id)?, &scope)?;
                Ok(run)
            })
            .await?;
        if self.inner.unavailable.load(Ordering::Acquire) && !view.state.is_terminal() {
            return Err(HostError::new(
                "storage_unavailable",
                "运行关键状态未能可靠保存",
            ));
        }
        Ok(view)
    }
    /// 功能：持久记录取消请求并通知本Run。
    /// 参数：scope为可信范围，id为RunID。
    /// 返回：取消请求后的视图。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：返回cancelling不代表清理完毕；终态不回退。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn cancel_run(&self, scope: &ClientScope, id: &str) -> HostResult<RunView> {
        let view = self.get_run(scope, id).await?;
        if view.state.is_terminal() {
            return Ok(view);
        }
        let control = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .get(id)
            .map(|a| a.control.clone());
        let id = id.to_owned();
        let updated = self
            .inner
            .store
            .call(move |db| {
                let mut run = db.get_run(&id)?;
                if !run.state.is_terminal() {
                    run.state = RunState::Cancelling;
                    db.save_run(run, 0)
                } else {
                    Ok(run)
                }
            })
            .await;
        if let Some(control) = control {
            control.cancel.cancel();
        }
        updated
    }
    /// 功能：在同一协调序列取得视图和后续通知。
    /// 参数：scope为可信完整范围。
    /// 返回：一致快照及有界通知接收器。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：落后从快照重取；断开订阅不默认取消Run。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn snapshot_and_subscribe(
        &self,
        scope: &ClientScope,
    ) -> HostResult<(HostSnapshot, broadcast::Receiver<HostNotice>)> {
        let scope = scope.clone();
        self.inner
            .store
            .call(move |db| {
                let bindings = db.bindings()?;
                let mut runs = Vec::new();
                for r in db.runs()? {
                    if bindings
                        .iter()
                        .any(|b| b.session_id == r.session_id && b.scope == scope)
                    {
                        runs.push(db.get_run(&r.run_id)?);
                    }
                }
                let approvals = db
                    .approvals()?
                    .into_iter()
                    .filter(|a| {
                        a.status == "pending" && runs.iter().any(|r| r.run_id == a.binding.run_id)
                    })
                    .collect();
                // 在同一单写协调序列订阅，快照取得到订阅之间没有事实提交窗口。
                Ok((
                    HostSnapshot {
                        revision: db.revision()?,
                        runs,
                        pending_approvals: approvals,
                    },
                    db.notices.subscribe(),
                ))
            })
            .await
    }
    /// 功能：恢复指定范围当前待决审批。
    /// 参数：scope为可信完整范围。
    /// 返回：已保存pending审批列表。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读，不触发菜单或默认批准。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn pending_approvals(&self, scope: &ClientScope) -> HostResult<Vec<ApprovalView>> {
        Ok(self
            .snapshot_and_subscribe(scope)
            .await?
            .0
            .pending_approvals)
    }
    /// 功能：校验完整绑定及revision后原子决定审批。
    /// 参数：scope/id/revision/binding为准确待审对象，approve为本次决定。
    /// 返回：已提交审批状态。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：重复同决定不重复执行；相反、过期或取消冲突拒绝。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn decide_approval(
        &self,
        scope: &ClientScope,
        id: &str,
        revision: u64,
        binding: openpaw_core::ToolCallBinding,
        approve: bool,
    ) -> HostResult<ApprovalView> {
        let scope = scope.clone();
        let id = id.to_owned();
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closing
                || state
                    .active
                    .get(&binding.run_id)
                    .is_none_or(|a| a.control.cancel.is_cancelled())
            {
                return Err(HostError::new("approval_conflict", "运行已取消或不存在"));
            }
        }
        self.inner
            .store
            .call(move |db| {
                let mut a = db
                    .approvals()?
                    .into_iter()
                    .find(|a| a.approval_id == id)
                    .ok_or_else(|| HostError::new("not_found", "审批不存在"))?;
                let run = db.get_run(&a.binding.run_id)?;
                check_scope(&db.binding(&run.session_id)?, &scope)?;
                let target = if approve { "approved" } else { "denied" };
                if a.binding != binding
                    || run.state.is_terminal()
                    || run.state == RunState::Cancelling
                    || chrono::Utc::now().timestamp_millis() >= a.expires_at_ms
                {
                    return Err(HostError::new(
                        "approval_conflict",
                        "审批范围、运行或有效期不匹配",
                    ));
                }
                if a.status == target {
                    return Ok(a);
                }
                if a.status != "pending" || a.revision != revision {
                    return Err(HostError::new("approval_conflict", "审批已决定或版本失效"));
                }
                a.status = target.into();
                db.save_approval(a)
            })
            .await
    }
    /// 功能：等待所有预留和活动Run完成实际清理。
    /// 参数：self为共享Host。
    /// 返回：无返回值，返回即活动注册表为空。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不取消运行；Notify与注册表检查避免丢唤醒。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                .is_empty()
            {
                return;
            }
            notified.await;
        }
    }
    /// 功能：拒新Run、取消并等待清理后关闭存储。
    /// 参数：self为共享Host。
    /// 返回：关闭完成结果。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：不能丢弃活动工具future；存储最后关闭并释放所有权。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn shutdown(&self) -> HostResult<()> {
        let mut completed = self.inner.shutdown_result.lock().await;
        if let Some(result) = &*completed {
            return result.clone();
        }
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            for a in state.active.values() {
                a.control.cancel.cancel();
            }
        }
        self.wait_idle().await;
        let projection = if let Some(office) = self.inner.office.lock().await.take() {
            office.finish(self).await
        } else {
            Ok(())
        };
        let result = projection.and(self.inner.store.close().await);
        *completed = Some(result.clone());
        result
    }

    /// 功能：启动可选Office同步，在原shutdown中统一回收。
    /// 参数：self为已恢复的共享Host。
    /// 返回：启动成功或已启动时成功。
    /// 错误：Office凭据/客户端装配错误。
    /// 状态：不重复启动办公室协调器。
    /// 并发与取消：异步生命周期锁避免两个监听器重复装配。
    /// 权限：只使用已验证本机配置。
    /// 关联：需求/设计REQ-OP-011；DES-OP-007/009。
    pub(crate) async fn start_office(&self) -> HostResult<()> {
        let mut owner = self.inner.office.lock().await;
        if owner.is_none() {
            if let Some(config) = &self.inner.config.office {
                *owner = Some(crate::office::Coordinator::start(
                    self.clone(),
                    config.clone(),
                )?);
            }
        }
        Ok(())
    }

    /// 功能：读取已恢复且可继续接受运行的就绪状态。
    /// 参数：self为共享Host。
    /// 返回：未关闭、未锁存存储故障时为真。
    /// 错误：无。
    /// 状态：只读，不触发网络或磁盘操作。
    /// 并发与取消：短注册表锁；关闭开始即返回假。
    /// 权限：仅提供无业务数据健康信息。
    /// 关联：需求/设计REQ-OP-009；DES-OP-006。
    pub(crate) fn ready(&self) -> bool {
        !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closing
            && !self.inner.unavailable.load(Ordering::Acquire)
    }
}
/// 功能：逐字段验证原绑定和可信范围。
/// 参数：binding为持久记录，scope为可信调用方范围。
/// 返回：相等返回成功。
/// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
/// 状态：只读；ID不是认证凭据。
/// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
/// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub(crate) fn check_scope(binding: &SessionBinding, scope: &ClientScope) -> HostResult<()> {
    if binding.scope == *scope {
        Ok(())
    } else {
        Err(HostError::new("scope_denied", "无权访问此会话"))
    }
}
fn valid_scope(scope: &ClientScope) -> HostResult<()> {
    valid_id(&scope.namespace)?;
    valid_id(&scope.slot_id)?;
    valid_id(&scope.conversation_id)
}
