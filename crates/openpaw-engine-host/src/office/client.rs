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
    // 仅确认此Client已登记的可信聊天地址，不缓存Office动态状态或聊天正文。
    chat_registration: tokio::sync::Mutex<Option<std::net::SocketAddr>>,
    gates: tokio::sync::Mutex<
        std::collections::BTreeMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>,
    >,
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
            chat_registration: Default::default(),
            gates: Default::default(),
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
        let timeout = match request["type"].as_str() {
            Some("ui.next") => Duration::from_secs(35),
            Some("decision.request") => Duration::from_millis(
                request["timeoutMs"]
                    .as_u64()
                    .filter(|v| (1000..=300000).contains(v))
                    .ok_or_else(|| HostError::new("invalid_input", "审批期限无效"))?
                    + 10000,
            ),
            _ => Duration::from_secs(15),
        };
        let response = self
            .http
            .post(&self.url)
            .timeout(timeout)
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
                Some("SNAPSHOT_REQUIRED") => "snapshot_required",
                Some("UI_REQUEST_EXPIRED" | "REQUEST_CLOSED") => "ui_expired",
                Some("REQUEST_TIMEOUT" | "DECISION_EXPIRED") => "decision_expired",
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
            let reason = data["reasonCode"]
                .as_str()
                .filter(|s| s.len() <= 64 && s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
                .unwrap_or("UNKNOWN");
            return Err(HostError::new(
                kind,
                format!("Office拒绝业务请求（{reason}），需核查同步状态"),
            ));
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
                if data["teamId"] != request["teamId"] {
                    return Err(HostError::new("protocol_error", "Office快照回执不合法"));
                }
                if data["seq"] != request["snapshot"]["seq"] {
                    return Err(HostError::new(
                        "sequence_conflict",
                        "Office快照回执序号与请求不一致",
                    ));
                }
                if !matches!(
                    (status, data["status"].as_str()),
                    (200, Some("applied" | "ignored")) | (202, Some("cached"))
                ) || data.as_object().is_none_or(|o| {
                    o.keys()
                        .any(|k| !matches!(k.as_str(), "teamId" | "seq" | "status" | "reasonCode"))
                }) {
                    return Err(HostError::new("protocol_error", "Office快照回执不合法"));
                }
            }
            Some("decision.request") => {
                let confirm = data["choiceId"] == "confirm";
                if status != 200
                    || data["teamId"] != request["teamId"]
                    || data["conversationId"] != request["conversationId"]
                    || data["decisionId"] != request["decision"]["id"]
                    || (!confirm && data["choiceId"] != "reject")
                    || data["label"]
                        != request["decision"][if confirm {
                            "confirmLabel"
                        } else {
                            "rejectLabel"
                        }]
                    || data.as_object().is_none_or(|o| o.len() != 5)
                {
                    return Err(HostError::new("protocol_error", "审批响应绑定不匹配"));
                }
            }
            Some("ui.next") => {
                if status != 200
                    || data.as_object().is_none_or(|o| o.len() != 1)
                    || data["actions"]
                        .as_array()
                        .is_none_or(|a| a.len() > 1 || a.iter().any(|v| !dto::ui_action(v)))
                {
                    return Err(HostError::new("protocol_error", "UI领取结果非法"));
                }
            }
            Some("ui.respond") => {
                if status != 200
                    || data["interactionId"] != request["interactionId"]
                    || data["delivered"] != true
                    || data.as_object().is_none_or(|o| o.len() != 2)
                {
                    return Err(HostError::new("protocol_error", "UI回执非法"));
                }
            }
            None if request.get("command").is_some() => {
                if status != 200
                    || !(data["forwarded"] == true
                        || data["forwarded"].as_u64().is_some_and(|n| n > 0))
                    || data.as_object().is_none_or(|o| o.len() != 1)
                {
                    return Err(HostError::new("protocol_error", "场景回执非法"));
                }
            }
            _ => return Err(HostError::new("unsupported", "不支持此Office操作")),
        }
        Ok((status, data))
    }
    /// 功能：确认同Client的聊天地址只注册一次，避免周期同步反复回传完整目录。
    /// 参数：address为Host可信监听地址，不能来自浏览器输入。
    /// 返回：原官方初始化回执已确认或同地址已成功登记。
    /// 错误：原post网络/认证/协议错误原样返回，不把失败写成成功。
    /// 状态：只保留进程内配置注册确认，不缓存聊天、审批或Office动态状态。
    /// 并发与取消：仅注册短门跨HTTP等待；失败或取消后未登记，下次可原样重试。
    /// 权限：复用固定Office端点与服务端密钥，原Host监听权限不变。
    /// 关联：需求REQ-OP-014/022；设计DES-OP-006/007；CHG-2026-0007第12.136节。
    pub(super) async fn register_chat(&self, address: std::net::SocketAddr) -> HostResult<()> {
        let mut registered = self.chat_registration.lock().await;
        if *registered != Some(address) {
            self.post(json!({"type":"offices.initialize","offices":[],"chat":{"url":format!("http://{address}/office/chat")}})).await?;
            *registered = Some(address);
        }
        Ok(())
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

impl Client {
    /// 功能：取得办公室的唯一快照外发门。
    /// 参数：team为可信办公室ID。
    /// 返回：可跨网络等待持有的办公室专用锁。
    /// 错误：无。
    /// 状态：不锁其他办公室或Run服务。
    /// 并发与取消：统一周期同步、审批屏障和删除屏障顺序。
    /// 权限：仅已配置办公室。
    /// 关联：REQ-OP-011/013/014；CHG-2026-0006。 需求/设计REQ-OP-014。
    pub(super) async fn projection_gate(
        &self,
        team: &str,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .await
            .entry(team.to_owned())
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn d150_sync_avoids_full_directory_and_recovers_office_loss_without_hiding_errors() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use std::sync::Arc;
        #[derive(Default)]
        struct Fixture {
            initialized: bool,
            denied: bool,
            stale: bool,
            requests: Vec<Value>,
        }
        async fn handle(
            State(state): State<Arc<tokio::sync::Mutex<Fixture>>>,
            Json(req): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            let mut state = state.lock().await;
            state.requests.push(req.clone());
            let (code, data) = if state.denied {
                (401, json!({"reasonCode":"AUTH_FAILED"}))
            } else {
                match req["type"].as_str().unwrap() {
                    "office.capabilities" => (
                        200,
                        json!({"scene":{"protocolVersion":"2.0","actors":[{"id":"office-lead"},{"id":"file-agent"},{"id":"data-agent"}]},"ui":{"whiteboardSchema":{}},"requests":{},"responses":{"success":{},"error":{}}}),
                    ),
                    "office.read" => (
                        200,
                        json!({"offices":if state.initialized {json!([{"id":"office-1"}])} else {json!([])},"sceneCommandProtocol":"2.0","actors":[],"supportedMessages":[],"ui":{}}),
                    ),
                    "offices.initialize" => {
                        if req["offices"].as_array().is_some_and(|a| !a.is_empty()) {
                            state.initialized = true;
                        }
                        (
                            202,
                            json!({"offices":if state.initialized {json!([{"id":"office-1"}])} else {json!([])},"sceneCommandProtocol":"2.0","actors":[],"supportedMessages":[],"ui":{}}),
                        )
                    }
                    "office.snapshot" if !state.initialized => {
                        (404, json!({"reasonCode":"OFFICE_NOT_INITIALIZED"}))
                    }
                    "office.snapshot" => (
                        if state.stale { 200 } else { 202 },
                        if state.stale {
                            json!({"teamId":req["teamId"],"seq":req["snapshot"]["seq"],"status":"ignored","reasonCode":"STALE_SEQ"})
                        } else {
                            json!({"teamId":req["teamId"],"seq":req["snapshot"]["seq"],"status":"cached"})
                        },
                    ),
                    _ => panic!("unexpected Office request"),
                }
            };
            (
                StatusCode::from_u16(code).unwrap(),
                Json(json!({"code":code,"msg":"fixture","data":data})),
            )
        }
        let state = Arc::new(tokio::sync::Mutex::new(Fixture::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/pawoffice/messages",
            listener.local_addr().unwrap()
        );
        let app = Router::new()
            .route("/api/pawoffice/messages", post(handle))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client {
            http: reqwest::Client::new(),
            url,
            authorization: reqwest::header::HeaderValue::from_static("Bearer fixture"),
            capabilities: Default::default(),
            chat_registration: Default::default(),
            gates: Default::default(),
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cfg:crate::HostConfig=serde_json::from_value(json!({"home":temp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"echo"})).unwrap();
        let host = crate::Host::start(cfg).await.unwrap();
        let office = OfficeConfig {
            base_url: "http://127.0.0.1:9".into(),
            api_key_env: "UNUSED".into(),
            allowed_origins: vec![],
            names: Default::default(),
            creation_slots: vec![],
            activity_window: true,
        };
        let address = "127.0.0.1:12345".parse().unwrap();
        // 先发现能力，确保下一次401确实发生在chat注册，而非能力读取。
        client.capabilities().await.unwrap();
        // 无缓存、认证失败不伪造注册成功或初始化完成。
        state.lock().await.denied = true;
        assert_eq!(
            super::super::projection::sync_once(
                &host, &office, "office-1", &client, false, address
            )
            .await
            .unwrap_err()
            .kind,
            "office_auth"
        );
        {
            let mut state = state.lock().await;
            assert_eq!(state.requests.last().unwrap()["type"], "offices.initialize");
            state.denied = false;
            state.requests.clear();
        }
        for _ in 0..8 {
            assert_eq!(
                super::super::projection::sync_once(
                    &host, &office, "office-1", &client, false, address
                )
                .await
                .unwrap(),
                "cached"
            );
        }
        {
            let state = state.lock().await;
            assert_eq!(
                state
                    .requests
                    .iter()
                    .filter(|r| r["type"] == "office.read")
                    .count(),
                0,
                "重复同步不得查全办公室及静态Schema"
            );
            assert_eq!(
                state
                    .requests
                    .iter()
                    .filter(|r| r["type"] == "offices.initialize")
                    .count(),
                2,
                "一次chat注册加一次确切未初始化恢复"
            );
            let snapshots: Vec<_> = state
                .requests
                .iter()
                .filter(|r| r["type"] == "office.snapshot")
                .collect();
            assert_eq!(snapshots.len(), 9, "仍每轮发快照，并在初始化后准确重试一次");
            assert!(
                snapshots
                    .iter()
                    .all(|r| r["snapshot"] == snapshots[0]["snapshot"]),
                "同seq完整内容不能因重试变化"
            );
        }
        // 模拟Office进程缓存清空：本地已注册不能阻止精确404后的重新初始化。
        state.lock().await.initialized = false;
        assert_eq!(
            super::super::projection::sync_once(
                &host, &office, "office-1", &client, false, address
            )
            .await
            .unwrap(),
            "cached"
        );
        assert_eq!(
            state
                .lock()
                .await
                .requests
                .iter()
                .filter(|r| r["type"] == "offices.initialize")
                .count(),
            3
        );
        // 新地址仅再注册一次，不把旧Client地址当作永久注册。
        let second = "127.0.0.1:12346".parse().unwrap();
        assert_eq!(
            super::super::projection::sync_once(&host, &office, "office-1", &client, false, second)
                .await
                .unwrap(),
            "cached"
        );
        assert_eq!(
            state
                .lock()
                .await
                .requests
                .iter()
                .filter(|r| r["type"] == "offices.initialize")
                .count(),
            4
        );
        state.lock().await.stale = true;
        assert_eq!(
            super::super::projection::sync_once(&host, &office, "office-1", &client, false, second)
                .await
                .unwrap_err()
                .kind,
            "sequence_conflict"
        );
        let before = state.lock().await.requests.len();
        assert_eq!(
            super::super::projection::sync_once(&host, &office, "office-1", &client, true, second)
                .await
                .unwrap(),
            "halted"
        );
        assert_eq!(
            state.lock().await.requests.len(),
            before,
            "外部序号冲突停写不能被初始化绕过"
        );
        state.lock().await.denied = true;
        assert_eq!(
            super::super::projection::sync_once(&host, &office, "office-1", &client, false, second)
                .await
                .unwrap_err()
                .kind,
            "office_auth"
        );
        assert_eq!(
            state.lock().await.requests.last().unwrap()["type"],
            "office.snapshot",
            "认证失败不尝试初始化掩盖错误"
        );
        host.shutdown().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn scene_receipts_accept_official_counts_and_reject_invalid_acknowledgements() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        let reply = std::sync::Arc::new(tokio::sync::Mutex::new((200u16, json!({"forwarded":1}))));
        async fn handle(
            State(reply): State<std::sync::Arc<tokio::sync::Mutex<(u16, Value)>>>,
        ) -> (StatusCode, Json<Value>) {
            let (status, data) = reply.lock().await.clone();
            (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"code":status,"msg":"fixture","data":data})),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/pawoffice/messages",
            listener.local_addr().unwrap()
        );
        let app = Router::new()
            .route("/api/pawoffice/messages", post(handle))
            .with_state(reply.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client {
            http: reqwest::Client::new(),
            url,
            authorization: reqwest::header::HeaderValue::from_static("Bearer local-fixture"),
            capabilities: Default::default(),
            chat_registration: Default::default(),
            gates: Default::default(),
        };
        for (status, data, valid) in [
            (200, json!({"forwarded":1}), true),
            (200, json!({"forwarded":3}), true),
            (200, json!({"forwarded":true}), true),
            (200, json!({"forwarded":0}), false),
            (200, json!({"forwarded":false}), false),
            (200, json!({"forwarded":-1}), false),
            (200, json!({"forwarded":1.5}), false),
            (200, json!({"forwarded":"1"}), false),
            (200, json!({"forwarded":1,"completed":true}), false),
            (200, json!({}), false),
            (202, json!({"forwarded":1}), false),
        ] {
            *reply.lock().await = (status, data.clone());
            let result = client.post(json!({"command":{}})).await;
            assert_eq!(result.is_ok(), valid, "status={status} data={data}");
            if !valid {
                assert_eq!(result.unwrap_err().kind, "protocol_error");
            }
        }
        server.abort();
    }
}
