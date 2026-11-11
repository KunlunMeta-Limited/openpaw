//! 职责：统一已授权Session的六类只读查询与有界分页。
//! 输入输出：中性查询到原存储的数据投影。
//! 依赖：原JSONL、Skill/Goal checked接口、Todo和有界文本读取。
//! 禁止职责：不创建知识/目标、不复制文件解析器、不触发调度。
use crate::{
    service::check_scope, ClientScope, Host, HostError, HostResult, QueryKind, QueryPage,
    QueryRequest,
};
use tokio_util::sync::CancellationToken;
/// 用途：同一次Store读取中的运行及业务计划事实，供各展示转换消费。
/// 字段：run沿用生命周期、revision、工具阶段、等待原因与结果引用；plan为原证据步骤。
/// 来源去向：原Db/RunView读取到Office投影与详情；不包含Office人物或文案映射。
/// 约束：执行完成与业务验收分别计算；不新增缓存、持久表或异步读取。
/// 稳定性：Host私有事实视图。
/// 关联：需求/设计：task_progress、projection、details。
pub(crate) struct RunFacts<'a> {
    run: &'a crate::RunView,
    plan: Option<serde_json::Value>,
}
impl<'a> RunFacts<'a> {
    /// 功能：在原Store序列内读取同一运行的进度事实。
    /// 参数：db为当前Store，run为已经授权的运行。
    /// 返回：引用原RunView的事实视图。
    /// 错误：原进度读取错误透传。
    /// 状态：只读原数据，无新事实或缓存。
    /// 并发与取消：不跨await，不改变原事务顺序。
    /// 权限：调用方先按原Session/Delegation校验scope。
    /// 关联：需求/设计：task_progress::execution_project。
    pub(crate) fn read(db: &crate::store::Db, run: &'a crate::RunView) -> HostResult<Self> {
        Ok(Self {
            run,
            plan: db.progress(&run.run_id)?,
        })
    }
    /// 功能：计算原子执行步骤进度，保留业务验收独立性。
    /// 参数：self为原事实视图。
    /// 返回：原步骤和原执行百分比。
    /// 错误：原计划解析错误。
    /// 状态：不写入、不补造证据。
    /// 并发与取消：同步纯转换。
    /// 权限：不扩大查询范围。
    /// 关联：需求/设计：task_progress::execution_project。
    pub(crate) fn execution(self) -> HostResult<(Vec<serde_json::Value>, u64)> {
        crate::task_progress::execution_project(self.plan, self.run)
    }
}
/// 功能：读取并校验委派绑定对应的子运行元数据。
/// 参数：db为原Store，assignment为已验证的持久委派。
/// 返回：原RunView元数据，不解析工具或正文。
/// 错误：找不到、存储错误、子会话不一致明确返回。
/// 状态：只读，不把缺失运行补造成完成。
/// 并发与取消：留在原Store有序读取中。
/// 权限：沿用delegations的父子scope校验，并核对子session。
/// 关联：Office聊天节点、原Delegation。
pub(crate) fn delegated_run_metadata(
    db: &crate::store::Db,
    assignment: &crate::orchestration::Delegation,
) -> HostResult<crate::RunView> {
    let child = db.run_metadata(&assignment.child_run)?;
    if child.session_id != assignment.child_session {
        return Err(HostError::new(
            "storage_unavailable",
            "子运行与持久委派所属不一致",
        ));
    }
    Ok(child)
}
impl Host {
    /// 功能：按作用域提供六类真实只读查询。
    /// 参数：scope为可信范围，request为会话、类型与分页。
    /// 返回：默认五项的有界查询页。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：原解析器错误上报；查询不写生命周期或创建数据。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn query(&self, scope: &ClientScope, request: QueryRequest) -> HostResult<QueryPage> {
        self.query_with_cancel(scope, request, &CancellationToken::new())
            .await
    }
    /// 功能：在UI期限内查询原存储，并允许取消资源等待。
    /// 参数：scope/request沿用原查询，cancel为操作token。
    /// 返回：原有界页。
    /// 错误：取消、删除、范围或原存储读取错误。
    /// 状态：只读，不创建原技能/目标/Todo。
    /// 并发与取消：根锁等待可取消；阻塞读取已经派发则等待join后返回。
    /// 权限：与原query一致，结束前再次检查删除墓碑。
    /// 关联：REQ-OP-014；API-HOST-QUERY。 需求/设计REQ-OP-014。
    pub(crate) async fn query_with_cancel(
        &self,
        scope: &ClientScope,
        request: QueryRequest,
        cancel: &CancellationToken,
    ) -> HostResult<QueryPage> {
        let file = matches!(&request.kind, QueryKind::File { .. });
        let mut page = self.query_detail_page(scope, request, cancel).await?.0;
        if !file {
            for item in &mut page.items {
                bound_json(item);
            }
        }
        Ok(page)
    }
    /// 功能：在同次原查询读取中取得稳定内容边界用于详情导航。
    /// 参数：scope/request/cancel沿用只读查询。
    /// 返回：查询页及只保存于导航的内容摘要。
    /// 错误：沿用原查询的权限、取消和数据错误。
    /// 状态：摘要不保存正文；数据改变必须重新打开详情。
    /// 并发与取消：摘要与本页来自同次读取，不跨页拼接不同版本。
    /// 权限：沿用完整作用域与文件根授权。
    /// 关联：REQ-OP-014；CHG-2026-0006第9.3节。 需求/设计REQ-OP-014。
    pub(crate) async fn query_detail_page(
        &self,
        scope: &ClientScope,
        request: QueryRequest,
        cancel: &CancellationToken,
    ) -> HostResult<(QueryPage, String)> {
        if cancel.is_cancelled() {
            return Err(HostError::new("cancelled", "查询已取消"));
        }
        let id = request.session_id.clone();
        let scope = scope.clone();
        let final_scope = scope.clone();
        let binding = self
            .inner
            .store
            .call(move |db| {
                db.check_live_scope(&scope)?;
                let b = db.binding(&id)?;
                check_scope(&b, &scope)?;
                Ok(b)
            })
            .await?;
        let root = self
            .inner
            .roots
            .get(&binding.root_id)
            .cloned()
            .ok_or_else(|| HostError::new("scope_denied", "查询根未授权"))?;
        let _guard = if matches!(&request.kind, QueryKind::History | QueryKind::ToolResults) {
            None
        } else {
            Some(root.acquire(false, cancel).await?)
        };
        let mut items = match request.kind {
            QueryKind::History | QueryKind::ToolResults => {
                let tools = matches!(request.kind, QueryKind::ToolResults);
                let id = binding.session_id;
                self.inner
                    .store
                    .call(move |db| {
                        db.events(&id)?
                            .into_iter()
                            .filter(|event| match event {
                                openpaw_core::SessionEvent::Message(m) => {
                                    !tools
                                        || m.content.iter().any(|b| {
                                            matches!(
                                                b,
                                                openpaw_core::ContentBlock::ToolResult { .. }
                                                    | openpaw_core::ContentBlock::ToolUse { .. }
                                            )
                                        })
                                }
                                _ => false,
                            })
                            .map(|mut event| {
                                if let openpaw_core::SessionEvent::Message(m) = &mut event {
                                    m.content.retain(|b| {
                                        !matches!(b, openpaw_core::ContentBlock::Thinking { .. })
                                    });
                                }
                                serde_json::to_value(event).map_err(HostError::storage)
                            })
                            .collect::<HostResult<Vec<_>>>()
                    })
                    .await?
            }
            QueryKind::File { path } => {
                let (session, original) = (binding.session_id.clone(), path.clone());
                let migrated = self
                    .inner
                    .store
                    .call(move |db| db.migrated_artifact(&session, &original))
                    .await?;
                let resolved = migrated
                    .as_ref()
                    .map_or(path.as_str(), |(relative, _)| relative);
                let text =
                    openpaw_tools::fs::read::read_text_bounded(&root.path, resolved, 256 * 1024)
                        .await
                        .map_err(|_| {
                            HostError::new("invalid_input", "文件越界、非文本、过大或不可读取")
                        })?;
                if let Some((relative, expected)) = migrated {
                    let target = root.path.join(relative);
                    crate::workspace_migration::verify_migrated_text(&target, &text, &expected)?;
                }
                vec![serde_json::json!({"path":path,"text":text})]
            }
            kind => {
                let home = self.inner.config.home.clone();
                tokio::task::spawn_blocking(move||{
                    // 在原Store扫描前校验显式目录，不能把根目录自身的链接当新的授权。
                    let values=match kind {
                        QueryKind::Skills=>{
                            openpaw_tools::safety::resolve(&home,"skills").map_err(|_|HostError::new("scope_denied","用户技能根越界"))?;
                            openpaw_tools::safety::resolve(&root.path,".openpaw/skills").map_err(|_|HostError::new("scope_denied","项目技能根越界"))?;
                            root.skills.list_readonly_checked().map_err(|_|HostError::new("invalid_input","技能数据损坏、越界或不可读取"))?.into_iter().map(|s|serde_json::json!({"frontmatter":s.frontmatter,"body":s.body})).collect()
                        },
                        QueryKind::Goals=>{
                            openpaw_tools::safety::resolve(&root.path,".openpaw/goals").map_err(|_|HostError::new("scope_denied","目标根越界"))?;
                            root.goals.list_readonly_checked().map_err(|_|HostError::new("invalid_input","目标数据损坏、越界或不可读取"))?.into_iter().map(|d|serde_json::json!({"frontmatter":d.frontmatter,"body":d.body})).collect()
                        },
                        QueryKind::Todo=>openpaw_tools::meta::todo::read(&root.path).map_err(|_|HostError::new("invalid_input","Todo数据损坏或不可读取"))?.into_iter().map(|t|serde_json::to_value(t).map_err(HostError::storage)).collect::<HostResult<Vec<_>>>()?,
                        _=>return Err(HostError::new("unsupported","未知查询类型")),
                    };Ok::<Vec<serde_json::Value>,HostError>(values)
                }).await.map_err(HostError::storage)??
            }
        };
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_vec(&items)
            .map_err(HostError::storage)?
            .hash(&mut hasher);
        let signature = format!("{:016x}", hasher.finish());
        let start = request.offset.min(items.len());
        let end = start
            .saturating_add(if request.limit == 0 {
                5
            } else {
                request.limit.min(50)
            })
            .min(items.len());
        let next_cursor = (end < items.len()).then_some(end);
        items = items.drain(start..end).collect();
        self.inner
            .store
            .call(move |db| db.check_live_scope(&final_scope))
            .await?;
        if cancel.is_cancelled() {
            return Err(HostError::new("cancelled", "查询已取消"));
        }
        Ok((QueryPage { items, next_cursor }, signature))
    }
}
fn bounded(s: &str) -> String {
    s.chars().take(32 * 1024).collect()
}
fn bound_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => *s = bounded(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(bound_json),
        serde_json::Value::Object(o) => o.values_mut().for_each(bound_json),
        _ => {}
    }
}
