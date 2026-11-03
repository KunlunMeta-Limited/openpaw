//! 职责：协调共享会话删除的持久意图、运行清理和账本清除。
//! 输入输出：可信完整范围到最小删除结果。
//! 依赖：原Host注册表、Store、JSONL与SQLite索引。
//! 禁止职责：不删除项目文件，不复制消息解析器，不等待全Host空闲。
use crate::{ClientScope, DeleteView, Host, HostError, HostResult};
impl Host {
    /// 功能：持久登记删除并等待目标Run清理后移除历史。
    /// 参数：scope为可信完整范围。
    /// 返回：已删除的最小视图；重复删除返回同一墓碑。
    /// 错误：不存在或存储删除失败；失败保留意图供重试和恢复。
    /// 状态：不回滚已发生工具副作用，不重新执行旧Run。
    /// 并发与取消：登记后的删除不随HTTP断开中止，串行Store防接受竞争。
    /// 权限：只处理匹配scope的原会话，工作区文件保留。
    /// 关联：REQ-OP-014；CHG-2026-0006第9.4节。 需求/设计REQ-OP-014。
    pub async fn delete_session(&self, scope: &ClientScope) -> HostResult<DeleteView> {
        let scope = scope.clone();
        let host = self.clone();
        let guard = self.reserve_deletion()?;
        // 删除是持久生命周期；调用future被丢弃也必须走完清理或保留可恢复意图。
        tokio::spawn(async move {
            let _guard = guard;
            let s = scope.clone();
            let view = host.inner.store.call(move |db| db.begin_delete(&s)).await?;
            if view.phase == "deleted" {
                return Ok(view);
            }
            host.quiesce_session(&view.session_id).await;
            let session = view.session_id.clone();
            let removed = {
                let target = session.clone();
                host.inner
                    .store
                    .call(move |db| {
                        Ok(db
                            .runs()?
                            .into_iter()
                            .filter(|run| run.session_id == target)
                            .map(|run| run.run_id)
                            .collect::<Vec<_>>())
                    })
                    .await?
            };
            let key = serde_json::to_string(&scope).map_err(HostError::storage)?;
            host.inner
                .store
                .call(move |db| db.purge_session(&key, &session))
                .await?;
            host.inner
                .office_projection_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&scope.slot_id);
            // 删除后的Run标识不可复活；清掉终态视图，避免人物详情在墓碑之后
            // 命中旧内存数据。不同办公室的缓存仍保留。
            host.inner
                .office_terminal_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|id, _| !removed.iter().any(|removed_id| removed_id == id));
            Ok(DeleteView {
                phase: "deleted".into(),
                ..view
            })
        })
        .await
        .map_err(HostError::storage)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn shutdown_waits_for_accepted_deletion_and_rejects_new_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cfg = serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = Host::start(cfg).await.unwrap();
        let guard = host.reserve_deletion().unwrap();
        let h = host.clone();
        let shutdown = tokio::spawn(async move { h.shutdown().await });
        // shutdown在原注册表设置closing后必须等待持有的删除生命周期。
        let mut idle = Box::pin(host.wait_idle());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut idle)
                .await
                .is_err()
        );
        assert!(host.reserve_deletion().is_err());
        assert!(!shutdown.is_finished());
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
