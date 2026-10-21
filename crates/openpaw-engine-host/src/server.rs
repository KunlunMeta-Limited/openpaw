//! 职责：loopback健康监听、端口发现与可靠停止。
//! 输入输出：已恢复的Host到本机HTTP健康接口。
//! 依赖：可选axum和Host关闭服务。
//! 禁止职责：本阶段不开放执行、审批或客户端协议路由。
use crate::{Host, HostError, HostResult};
use axum::{routing::get, Json, Router};
use std::net::{Ipv4Addr, SocketAddr};
use tokio_util::sync::CancellationToken;
/// 用途：loopback健康服务及优雅关闭所有者。
/// 字段：下列成员或状态由宿主统一管理。
/// 来源去向：可信适配器、共享运行服务及原存储。
/// 约束：不作为远程授权凭据，不复制核心执行逻辑。
/// 稳定性：T3新增，协议无关Rust接口。
/// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
pub struct HostServer {
    addr: SocketAddr,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<HostResult<()>>,
    host: Host,
}
impl HostServer {
    /// 功能：为已恢复Host监听loopback健康服务。
    /// 参数：host为共享服务，port零表示随机端口。
    /// 返回：实际监听地址及停止所有者。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：仅GET /health；不开放调试执行和审批。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn bind(host: Host, port: u16) -> HostResult<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| HostError::new("invalid_input", "无法监听本机端口"))?;
        let addr = listener.local_addr().map_err(HostError::storage)?;
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let health = host.clone();
        let app = Router::new().route("/health",get(move||{let health=health.clone();async move{
            let ready=health.ready();
            (if ready{axum::http::StatusCode::OK}else{axum::http::StatusCode::SERVICE_UNAVAILABLE},Json(serde_json::json!({"status":if ready{"ready"}else{"unavailable"},"schemaVersion":1})))
        }}));
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await
                .map_err(|_| HostError::new("interrupted", "健康服务异常停止"))
        });
        Ok(Self {
            addr,
            stop,
            task,
            host,
        })
    }
    /// 功能：返回已绑定的实际本机地址。
    /// 参数：self为启动成功的监听器。
    /// 返回：本机loopback SocketAddr。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：只读，无状态改变。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub fn address(&self) -> SocketAddr {
        self.addr
    }
    /// 功能：等待Host工具清理后停止健康监听。
    /// 参数：self为健康服务所有者。
    /// 返回：完整停止结果。
    /// 错误：非法范围、状态冲突或存储故障明确上报；无失败分支的读取不产生错误。
    /// 状态：先Host后listener，错误也不能跳过实际清理。
    /// 并发与取消：共享状态经协调器/锁访问；取消不提前释放实际执行资源。
    /// 权限：仅本机可信调用方；作用域和允许根仍须在服务入口校验。
    /// 关联：需求/设计REQ-OP-008、REQ-OP-009；DES-OP-006。
    pub async fn shutdown(self) -> HostResult<()> {
        // 先等待所有工具清理，再终止listener；服务错误也不能跳过Host关闭。
        let result = self.host.shutdown().await;
        self.stop.cancel();
        let serving = self.task.await.map_err(HostError::storage)?;
        result.and(serving)
    }
}
