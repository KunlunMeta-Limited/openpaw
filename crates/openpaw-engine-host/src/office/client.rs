//! 职责：Office官方messages客户端及明确回执语义。
//! 输入输出：完整业务请求到经过封套和操作类型检查的结果。
//! 依赖：workspace reqwest；所有调用在宿主异步任务中。
//! 禁止职责：不访问Office内部SSE/reply/ui，不输出密钥或原始错误体。
use super::{dto, OfficeConfig};
use crate::{HostError, HostResult};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// 用途：只存在服务端的Office HTTP客户端。
/// 字段：禁重定向连接池、端点和敏感请求头。
/// 来源去向：系统Office凭据到Fastify消息API。
/// 约束：不实现Debug、不将密钥放入事件或工具环境。
/// 稳定性：T4内部接口。
/// 关联：需求/设计REQ-OP-011；DES-OP-007。
pub(super) struct Client {
    http: reqwest::Client,
    url: String,
    authorization: reqwest::header::HeaderValue,
    capabilities: tokio::sync::Mutex<Option<Value>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    code: u16,
    msg: String,
    data: Value,
}
impl Client {
    /// 功能：读取Office专用凭据并创建禁止重定向的客户端。
    /// 参数：config为已审核连接配置。
    /// 返回：可复用HTTP客户端。
    /// 错误：缺少凭据或非法请求头；不回显值。
    /// 状态：此处不进行网络请求。
    /// 并发与取消：连接池可共享，网络总超时15秒。
    /// 权限：只使用PAWOFFICE_专用变量，不读取模型密钥。
    /// 关联：需求/设计REQ-OP-011；CHG-2026-0005。
    pub(super) fn new(config: &OfficeConfig) -> HostResult<Self> {
        let key = std::env::var(&config.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| HostError::new("invalid_input", "缺少Office服务凭据"))?;
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| HostError::new("invalid_input", "Office凭据格式不合法"))?;
        authorization.set_sensitive(true);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| HostError::new("invalid_input", "Office客户端初始化失败"))?;
        Ok(Self {
            http,
            url: format!(
                "{}/api/pawoffice/messages",
                config.base_url.trim_end_matches('/')
            ),
            authorization,
            capabilities: tokio::sync::Mutex::new(None),
        })
    }
    /// 功能：调用官方消息并核对HTTP封套与操作返回值。
    /// 参数：request为无null且不超过64KiB的官方请求。
    /// 返回：HTTP状态与data对象，202仍保留cached含义。
    /// 错误：网络、认证、协议或SEQ_CONFLICT；不输出响应原文。
    /// 状态：失败不假定服务端回滚；调用方负责读核对和同版本重试。
    /// 并发与取消：单次有限超时，future可取消；不持资源锁。
    /// 权限：密钥只发固定端点，禁止重定向。
    /// 关联：需求/设计REQ-OP-011；API-OFFICE-MESSAGES。
    pub(super) async fn post(&self, request: Value) -> HostResult<(u16, Value)> {
        let bytes = serde_json::to_vec(&request).map_err(HostError::storage)?;
        if bytes.len() > 65536 || !dto::protocol_value(&request) {
            return Err(HostError::new(
                "protocol_error",
                "Office请求不符合容量或值约束",
            ));
        }
        let response = self
            .http
            .post(&self.url)
            .header(reqwest::header::AUTHORIZATION, self.authorization.clone())
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|_| HostError::new("office_unavailable", "Office消息连接或等待失败"))?;
        let status = response.status().as_u16();
        let mut chunks = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk =
                chunk.map_err(|_| HostError::new("office_unavailable", "Office响应中断"))?;
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err(HostError::new("protocol_error", "Office响应超过上限"));
            }
            body.extend_from_slice(&chunk);
        }
        let envelope: Envelope = serde_json::from_slice(&body)
            .map_err(|_| HostError::new("protocol_error", "Office响应封套无效"))?;
        if envelope.code != status
            || !envelope.data.is_object()
            || !dto::protocol_value(&envelope.data)
            || envelope.msg.len() > 8192
        {
            return Err(HostError::new(
                "protocol_error",
                "Office状态与响应封套不一致",
            ));
        }
        let data = envelope.data;
        if status >= 300 {
            let kind = match data["reasonCode"].as_str() {
                Some("SEQ_CONFLICT") => "sequence_conflict",
                Some("OFFICE_NOT_INITIALIZED") => "office_uninitialized",
                _ => {
                    if status == 401 || status == 403 {
                        "office_auth"
                    } else if status >= 500 || status == 408 {
                        "office_unavailable"
                    } else {
                        "protocol_error"
                    }
                }
            };
            return Err(HostError::new(kind, "Office拒绝业务请求，需核查同步状态"));
        }
        match request["type"].as_str() {
            Some("office.capabilities") => {
                if status != 200
                    || data["scene"]["protocolVersion"] != "2.0"
                    || !data["scene"]["actors"].is_array()
                    || !data["ui"]["whiteboardSchema"].is_object()
                    || !data["requests"].is_object()
                    || !data["responses"]["success"].is_object()
                    || !data["responses"]["error"].is_object()
                {
                    return Err(HostError::new("protocol_error", "Office能力协议不兼容"));
                }
            }
            Some("offices.initialize") | Some("office.read") => {
                if request.get("teamId").is_some() {
                    if !data["snapshot"].is_object()
                        || !data["scene"].is_object()
                        || !data["pendingDecisions"].is_array()
                    {
                        return Err(HostError::new("protocol_error", "Office读取结果不合法"));
                    }
                } else if !data["offices"].is_array()
                    || data["sceneCommandProtocol"] != "2.0"
                    || !data["actors"].is_array()
                    || !data["supportedMessages"].is_array()
                    || !data["ui"].is_object()
                    || (status != 200 && status != 202)
                {
                    return Err(HostError::new("protocol_error", "Office目录结果不合法"));
                }
            }
            Some("office.snapshot") => {
                if data["teamId"] != request["teamId"]
                    || data["seq"] != request["snapshot"]["seq"]
                    || !matches!(
                        (status, data["status"].as_str()),
                        (200, Some("applied" | "ignored")) | (202, Some("cached"))
                    )
                    || data.as_object().is_none_or(|o| {
                        o.keys().any(|k| {
                            !matches!(k.as_str(), "teamId" | "seq" | "status" | "reasonCode")
                        })
                    })
                {
                    return Err(HostError::new("protocol_error", "Office快照回执不合法"));
                }
            }
            _ => return Err(HostError::new("unsupported", "T4客户端不支持此操作")),
        }
        Ok((status, data))
    }
    /// 功能：发现真实场景人物与白板协议。
    /// 参数：self为固定Office客户端。
    /// 返回：官方能力数据。
    /// 错误：连接或协议异常。
    /// 状态：只读。
    /// 并发与取消：有限超时，可由宿主关闭取消。
    /// 权限：复用服务端凭据。
    /// 关联：需求/设计REQ-OP-011/012。
    pub(super) async fn capabilities(&self) -> HostResult<Value> {
        let mut cache = self.capabilities.lock().await;
        if let Some(data) = &*cache {
            return Ok(data.clone());
        }
        let data = self.post(json!({"type":"office.capabilities"})).await?.1;
        *cache = Some(data.clone());
        Ok(data)
    }
}
