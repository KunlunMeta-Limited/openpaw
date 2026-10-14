//! 功能：为可信登记的 AI PC 会话追加上下文工具。
//! 输入输出边界：Basic 登记保存内存凭据；工具参数不得指定身份或权限。
//! 禁止承担：不修改默认工具注册表、不落盘凭据、不绕过桌面授权。
//! 关联：CHG-2026-0026。
use async_trait::async_trait;
use openpaw_core::{
    ToolCallOutcome, ToolCategory, ToolConcurrency, ToolExecutor, ToolHost, ToolHostValidator,
    ToolSpec,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

/// 功能：由受认证请求提交的桥接凭据；禁止 Debug 与持久化。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrationInput {
    /// 功能：限于宿主机的固定 /ai-pc 地址。
    pub(crate) bridge_url: String,
    /// 功能：每个受管会话独立的随机令牌。
    pub(crate) token: String,
}
/// 功能：内存登记记录，不向模型或日志暴露。
#[derive(Clone)]
pub(crate) struct Registration {
    url: reqwest::Url,
    token: String,
    client: reqwest::Client,
}
/// 功能：登记接口的稳定错误分类，不含凭据。
pub(crate) enum RegistrationError {
    AuthenticationRequired,
    NotFound,
    Busy,
    Invalid,
}
/// 功能：每个 Host 实例独立的会话登记表；重启不恢复。
pub(crate) type Registrations = Arc<RwLock<HashMap<String, Registration>>>;
impl RegistrationInput {
    /// 功能：验证宿主机边界并创建无重定向的客户端。
    /// 参数：self 为已解析的登记请求。
    /// 返回：可放入内存表的登记记录。
    /// 错误：非法地址、令牌或客户端初始化失败返回 Invalid。
    /// 状态：不写文件或登记表。
    /// 并发与取消：同步验证，无共享状态。
    /// 权限：只接受指定宿主机地址与固定路径。
    /// 关联：CHG-2026-0026。
    pub(crate) fn checked(self) -> Result<Registration, RegistrationError> {
        let url = reqwest::Url::parse(&self.bridge_url).map_err(|_| RegistrationError::Invalid)?;
        if self.bridge_url.len() > 2048
            || !matches!(url.scheme(), "http" | "https")
            || !matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "host.docker.internal")
            )
            || url.path() != "/ai-pc"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !(32..=256).contains(&self.token.len())
            || !self
                .token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(RegistrationError::Invalid);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| RegistrationError::Invalid)?;
        Ok(Registration {
            url,
            token: self.token,
            client,
        })
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    keyword: Option<String>,
    application: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
}
fn validate(name: &str, args: &Value) -> Result<(), String> {
    match name {
        "ai_pc_current_context" if args.as_object().is_some_and(|v| v.is_empty()) => Ok(()),
        "ai_pc_history_search" => {
            let a: HistoryArgs =
                serde_json::from_value(args.clone()).map_err(|_| "历史参数无效")?;
            if a.keyword.as_ref().is_some_and(|s| s.chars().count() > 256)
                || a.application
                    .as_ref()
                    .is_some_and(|s| s.chars().count() > 256)
                || a.from_ms.is_some_and(|t| t < 0)
                || a.to_ms.is_some_and(|t| t < 0)
                || matches!((a.from_ms,a.to_ms),(Some(f),Some(t)) if f>t)
            {
                return Err("历史范围无效".into());
            }
            Ok(())
        }
        _ => Err("当前上下文工具只接受空对象".into()),
    }
}
/// 功能：生成不含凭据的两个工具声明。
/// 参数：无。
/// 返回：当前上下文与历史检索声明。
/// 错误：无。
/// 状态：无。
/// 并发与取消：无共享可变状态。
/// 权限：声明不能自报服务、会话或模式。
/// 关联：CHG-2026-0026。
pub(crate) fn specs() -> Vec<ToolSpec> {
    [("ai_pc_current_context", "查询获准的当前 Windows 上下文、采集/分析状态与可引用来源；暂停时不会恢复采集。",json!({"type":"object","properties":{},"additionalProperties":false})),
    ("ai_pc_history_search", "按中文关键词、应用和 Unix 毫秒范围检索获准桌面历史。引用结果中的 source_id。返回 ok/empty/denied 状态。",json!({"type":"object","properties":{"keyword":{"type":"string","maxLength":256},"application":{"type":"string","maxLength":256},"from_ms":{"type":"integer","minimum":0},"to_ms":{"type":"integer","minimum":0}},"additionalProperties":false}))]
        .into_iter().map(|(name,description,input_schema)| ToolSpec { name:name.into(),description:description.into(),input_schema,category:Some(ToolCategory::ReadOnly),concurrency:Some(ToolConcurrency::ParallelSafe), ..Default::default() }).collect()
}
struct DesktopTools {
    session_id: String,
    inner: Arc<dyn ToolHost>,
    registrations: Registrations,
}
/// 功能：包装内置工具，绑定运行时提供的真实会话身份。
/// 参数：inner 为默认工具、session_id 为可信身份、registrations 为进程内登记表。
/// 返回：兼容原 ToolHost 合同的包装器。
/// 错误：无。
/// 状态：不自动登记任何会话。
/// 并发与取消：登记读锁不跨越 await。
/// 权限：仅已登记会话可调用桌面工具。
/// 关联：CHG-2026-0026。
pub(crate) fn wrap(
    inner: Arc<dyn ToolHost>,
    session_id: &str,
    registrations: Registrations,
) -> Arc<dyn ToolHost> {
    Arc::new(DesktopTools {
        session_id: session_id.into(),
        inner,
        registrations,
    })
}
impl DesktopTools {
    fn registration(&self) -> Option<Registration> {
        self.registrations
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.session_id)
            .cloned()
    }
    async fn desktop_call(
        &self,
        name: &str,
        args: Value,
        registration: Registration,
    ) -> Result<ToolCallOutcome, String> {
        validate(name, &args)?;
        let path = if name == "ai_pc_current_context" {
            "current"
        } else {
            "history"
        };
        let mut url = registration.url;
        url.set_path(&format!("/ai-pc/{path}"));
        let mut response = registration
            .client
            .post(url)
            .bearer_auth(registration.token)
            .json(&json!({"session_id":self.session_id,"args":args}))
            .send()
            .await
            .map_err(|_| "桌面桥接不可用")?;
        if !response.status().is_success() {
            return Err(format!(
                "桌面桥接拒绝请求 HTTP {}",
                response.status().as_u16()
            ));
        }
        let mut body = vec![];
        while let Some(chunk) = response.chunk().await.map_err(|_| "桌面响应传输失败")? {
            if body.len().saturating_add(chunk.len()) > 512 * 1024 {
                return Err("桌面响应超过容量".into());
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| "桌面响应格式无效")?;
        let status = value["status"].as_str().ok_or("桌面响应缺少状态")?;
        if !matches!(status, "ok" | "empty" | "denied") {
            return Err("桌面响应状态无效".into());
        }
        let content = serde_json::to_string(&value).map_err(|_| "桌面响应格式无效")?;
        Ok(ToolCallOutcome {
            content,
            is_error: status == "denied",
            structured: Some(value),
        })
    }
}
#[async_trait]
impl ToolExecutor for DesktopTools {
    async fn list_tools(&self) -> openpaw_core::Result<Vec<ToolSpec>> {
        let mut tools = self.inner.list_tools().await?;
        if self.registration().is_some() {
            tools.extend(specs());
        }
        Ok(tools)
    }
    async fn call(&self, name: &str, args: Value) -> openpaw_core::Result<ToolCallOutcome> {
        let registration = self.registration();
        if matches!(name, "ai_pc_current_context" | "ai_pc_history_search") {
            return Ok(match registration {
                Some(r) => self
                    .desktop_call(name, args, r)
                    .await
                    .unwrap_or_else(ToolCallOutcome::error),
                None => ToolCallOutcome::error("本会话未登记桌面授权"),
            });
        }
        if registration.is_some() && name == "session_search" {
            return Ok(ToolCallOutcome::error(
                "AI PC 会话须使用 ai_pc_history_search 授权检索",
            ));
        }
        self.inner.call(name, args).await
    }
}
#[async_trait]
impl ToolHostValidator for DesktopTools {
    async fn validate_input(&self, name: &str, input: &Value) -> Result<(), String> {
        if matches!(name, "ai_pc_current_context" | "ai_pc_history_search") {
            validate(name, input)
        } else {
            self.inner.validate_input(name, input).await
        }
    }
    fn should_cascade_on_error(&self, name: &str, outcome: &ToolCallOutcome) -> bool {
        self.inner.should_cascade_on_error(name, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn authenticated_registration_is_idle_scoped_and_not_inherited_or_restored() {
        use crate::{build_router, AppState, HostConfig};
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use base64::Engine;
        use tower::ServiceExt;
        let home = tempfile::tempdir().unwrap();
        let config = HostConfig {
            home: home.path().into(),
            provider: "echo".into(),
            username: Some("fixture".into()),
            password: Some("secret".into()),
            ..Default::default()
        };
        let state = AppState::new(config.clone());
        let handle = state.create_session(None, None, None, None).await.unwrap();
        let app = build_router(state.clone());
        let request = |id: &str, method: &str, body: Value, authenticated: bool| {
            let mut builder = Request::builder()
                .method(method)
                .uri(format!("/session/{id}/ai-pc"))
                .header("content-type", "application/json");
            if authenticated {
                builder = builder.header(
                    "authorization",
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode("fixture:secret")
                    ),
                );
            }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let body = json!({"bridge_url":"http://127.0.0.1:1/ai-pc","token":"a".repeat(64)});
        assert_eq!(
            app.clone()
                .oneshot(request(&handle.id, "PUT", body.clone(), false))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(request("unknown", "PUT", body.clone(), true))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(request(
                    &handle.id,
                    "PUT",
                    json!({"bridge_url":"http://example.com/ai-pc","token":"a".repeat(64)}),
                    true
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.clone()
                .oneshot(request(&handle.id, "PUT", body.clone(), true))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(state.ai_pc_tools(&handle).len(), 23);
        assert!(handle.reserve_turn());
        assert_eq!(
            app.clone()
                .oneshot(request(&handle.id, "DELETE", json!({}), true))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        handle.release_turn();
        let fork = state.fork_session(&handle.id, None).await.unwrap();
        assert_eq!(state.ai_pc_tools(&fork).len(), 21);
        assert!(AppState::new(config).ai_pc.read().unwrap().is_empty());
        assert_eq!(
            app.clone()
                .oneshot(request(&handle.id, "DELETE", json!({}), true))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(state.ai_pc_tools(&handle).len(), 21);
        assert_eq!(
            app.oneshot(request(&handle.id, "PUT", body, true))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        state.delete_session(&handle.id).await.unwrap();
        assert!(!state.ai_pc.read().unwrap().contains_key(&handle.id));
        let unprotected = AppState::new(HostConfig {
            home: tempfile::tempdir().unwrap().keep(),
            provider: "echo".into(),
            username: None,
            password: None,
            ..Default::default()
        });
        let id = unprotected
            .create_session(None, None, None, None)
            .await
            .unwrap()
            .id
            .clone();
        assert_eq!(
            build_router(unprotected)
                .oneshot(request(&id, "DELETE", json!({}), false))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    #[tokio::test]
    async fn desktop_http_injects_the_bound_session_without_exposing_credentials() {
        use axum::{extract::Json, http::HeaderMap, routing::post, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/ai-pc/history",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(
                    headers["authorization"],
                    format!("Bearer {}", "a".repeat(64))
                );
                assert_eq!(body["session_id"], "trusted");
                assert_eq!(body["args"]["keyword"], "中文");
                Json(json!({"status":"ok","items":[{"source_id":"source","text":"中文公开"}]}))
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registrations = Registrations::default();
        registrations.write().unwrap().insert(
            "trusted".into(),
            RegistrationInput {
                bridge_url: format!("http://{address}/ai-pc"),
                token: "a".repeat(64),
            }
            .checked()
            .ok()
            .unwrap(),
        );
        let tools = wrap(
            Arc::new(openpaw_core::NullToolHost),
            "trusted",
            registrations,
        );
        let result = tools
            .call("ai_pc_history_search", json!({"keyword":"中文"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("中文公开"));
        assert!(!result.content.contains(&"a".repeat(64)));
        task.abort();
    }
    #[tokio::test]
    async fn unregistered_tools_and_forged_identity_are_denied() {
        let registrations = Registrations::default();
        let tools = wrap(
            openpaw_tools::arc_registry(std::path::Path::new(".")),
            "owned",
            registrations.clone(),
        );
        assert_eq!(tools.list_tools().await.unwrap().len(), 21);
        assert!(
            tools
                .call("ai_pc_current_context", json!({}))
                .await
                .unwrap()
                .is_error
        );
        let input = RegistrationInput {
            bridge_url: "http://127.0.0.1:1/ai-pc".into(),
            token: "a".repeat(64),
        };
        registrations
            .write()
            .unwrap()
            .insert("owned".into(), input.checked().ok().unwrap());
        assert_eq!(tools.list_tools().await.unwrap().len(), 23);
        assert!(
            tools
                .call("ai_pc_current_context", json!({"mode":"open"}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tools
                .call("ai_pc_history_search", json!({"session_id":"other"}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tools
                .call("session_search", json!({"query":"private"}))
                .await
                .unwrap()
                .is_error
        );
        let other = wrap(
            openpaw_tools::arc_registry(std::path::Path::new(".")),
            "other",
            registrations,
        );
        assert_eq!(other.list_tools().await.unwrap().len(), 21);
        assert!(
            other
                .call("ai_pc_current_context", json!({}))
                .await
                .unwrap()
                .is_error
        );
    }
    #[test]
    fn registration_rejects_external_addresses_and_secret_bearing_urls() {
        for url in [
            "https://example.com/ai-pc",
            "http://user@localhost/ai-pc",
            "http://localhost/ai-pc?token=x",
            "http://localhost/other",
            "file:///ai-pc",
        ] {
            assert!(RegistrationInput {
                bridge_url: url.into(),
                token: "a".repeat(64)
            }
            .checked()
            .is_err());
        }
    }
}
