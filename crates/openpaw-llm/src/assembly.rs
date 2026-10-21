//! 职责：共用Provider与模型装配，供CLI及后续Host复用。
//! 输入输出：显式配置到现有Provider实例。
//! 依赖：本crate已有Provider
//! 禁止职责：不读取宿主环境、不处理Office协议或保存密钥。

use crate::{AnthropicProvider, EchoProvider, MockProvider, OpenAiProvider};
use openpaw_core::{Error, LlmProvider, Result};
use std::sync::Arc;

/// 功能：解析显式模型优先于Provider默认模型。
/// 参数：provider为现有实现，explicit为可选显式模型。
/// 返回：有效的模型名称。
/// 错误：显式或默认模型为空则拒绝。
/// 状态：无。
/// 并发与取消：纯读取，无等待。
/// 权限：不访问密钥。
/// 关联：需求/设计 REQ-OP-003；DES-OP-002。
pub fn resolve_model(provider: &dyn LlmProvider, explicit: Option<&str>) -> Result<String> {
    let model = explicit.unwrap_or_else(|| provider.default_model()).trim();
    if model.is_empty() {
        return Err(Error::Config("model must not be empty".into()));
    }
    Ok(model.into())
}

/// 功能：用已有Provider实现完成无协议公共装配。
/// 参数：name为类型，model为显式模型，api_key由宿主注入，base_url为可选服务端点。
/// 返回：Provider及有效模型；离线实现仍使用既有确定性策略。
/// 错误：未知类型、空模型或缺少密钥返回Config，不包含密钥原文。
/// 状态：只构造客户端，不进行网络请求或持久化。
/// 并发与取消：实例可共享，Run必须独立装配引擎。
/// 权限：端点由已授权宿主配置，密钥不进入事件或工具环境。
/// 关联：需求/设计 REQ-OP-003；DES-OP-002。
pub fn assemble_provider(
    name: &str,
    model: Option<&str>,
    api_key: Option<String>,
    base_url: Option<&str>,
) -> Result<(Arc<dyn LlmProvider>, String)> {
    if let Some(url) = base_url {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| Error::Config("invalid provider base URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(Error::Config(
                "provider base URL must be HTTP(S) without embedded credentials, query or fragment"
                    .into(),
            ));
        }
    }
    let key = || {
        api_key
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::Config(format!("{name} API key is not set")))
    };
    let provider: Arc<dyn LlmProvider> = match name {
        "echo" => Arc::new(EchoProvider::new()),
        "mock" => Arc::new(MockProvider::counter("mock-reply-")),
        "anthropic" => {
            let mut provider = AnthropicProvider::new(key()?, model.unwrap_or("claude-sonnet-4"));
            if let Some(url) = base_url {
                provider = provider.with_base_url(url.trim_end_matches('/'));
            }
            Arc::new(provider)
        }
        "openai" => {
            let mut provider = OpenAiProvider::new(key()?, model.unwrap_or("gpt-4o"));
            if let Some(url) = base_url {
                provider = provider.with_base_url(url.trim_end_matches('/'));
            }
            Arc::new(provider)
        }
        _ => return Err(Error::Config(format!("unknown provider `{name}`"))),
    };
    let model = resolve_model(&*provider, model)?;
    Ok((provider, model))
}
