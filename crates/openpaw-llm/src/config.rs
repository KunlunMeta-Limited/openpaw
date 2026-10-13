//! 功能：共享 Provider 配置、目录与工厂。
//! 参数：环境快照和入口覆盖。
//! 返回：校验后的配置或脱敏错误。
//! 错误：非法配置在网络请求前失败。
//! 状态：只持有内存配置。
//! 并发与取消：快照不可变，不读取网络。
//! 权限：密钥仅用于认证头。
//! 关联：CLI chat 和 Engine Host。

use openpaw_core::LlmProvider;
use std::{collections::HashMap, sync::Arc};

/// 功能：提供支持协议、默认模型和显示名称的唯一目录。
pub const PROVIDERS: [(&str, &str, &str); 4] = [
    ("mock", "mock-1", "OpenPaw mock"),
    ("echo", "echo-1", "OpenPaw echo"),
    ("anthropic", "claude-sonnet-4", "Anthropic"),
    ("openai", "gpt-4o", "OpenAI"),
];

/// 功能：入口显式配置覆盖。
#[derive(Default)]
pub struct Overrides {
    /// 功能：覆盖服务地址。参数：可选 URL。返回：无。错误：解析时校验。状态：内存。并发与取消：无。权限：禁止嵌入凭据。关联：resolve。
    pub base_url: Option<String>,
    /// 功能：覆盖模型。参数：可选名称。返回：无。错误：解析时校验。状态：内存。并发与取消：无。权限：无。关联：resolve。
    pub model: Option<String>,
    /// 功能：覆盖生成上限。参数：可选正整数文本。返回：无。错误：解析时校验。状态：内存。并发与取消：无。权限：无。关联：resolve。
    pub max_output_tokens: Option<String>,
}

/// 功能：封装不可变环境快照。
#[derive(Clone, Default)]
pub struct ConfigSnapshot {
    values: HashMap<String, String>,
}

/// 功能：保存校验后的运行配置。
pub struct ResolvedConfig {
    /// 功能：协议标识。参数：无。返回：名称。错误：无。状态：内存。并发与取消：只读。权限：无。关联：工厂。
    pub provider: String,
    /// 功能：有效模型。参数：无。返回：名称。错误：无。状态：内存。并发与取消：只读。权限：无。关联：会话和 AgentLoop。
    pub model: String,
    /// 功能：有效服务地址。参数：无。返回：可选地址。错误：无。状态：内存。并发与取消：只读。权限：已排除凭据。关联：工厂。
    pub base_url: Option<String>,
    /// 功能：有效输出上限。参数：无。返回：正整数。错误：无。状态：内存。并发与取消：只读。权限：无。关联：AgentLoop。
    pub max_output_tokens: u32,
    key: Option<String>,
}

impl ConfigSnapshot {
    /// 功能：读取登记的进程环境配置。
    /// 参数：无。
    /// 返回：独立快照。
    /// 错误：非 Unicode 值按无效配置保留为空值。
    /// 状态：不修改环境。
    /// 并发与取消：应在入口一次捕获。
    /// 权限：不读取配置文件或网络。
    /// 关联：from_pairs。
    pub fn from_env() -> Self {
        let names = [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENPAW_OPENAI_BASE_URL",
            "OPENPAW_ANTHROPIC_BASE_URL",
            "OPENPAW_OPENAI_MODEL",
            "OPENPAW_ANTHROPIC_MODEL",
            "OPENPAW_MAX_OUTPUT_TOKENS",
        ];
        Self::from_pairs(names.into_iter().filter_map(|name| {
            std::env::var_os(name)
                .map(|value| (name.to_string(), value.into_string().unwrap_or_default()))
        }))
    }

    /// 功能：从显式键值构建快照。
    /// 参数：键值迭代器。
    /// 返回：独立快照。
    /// 错误：解析时校验。
    /// 状态：不修改进程环境。
    /// 并发与取消：可用于并行测试。
    /// 权限：调用者提供内存凭据。
    /// 关联：resolve。
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            values: pairs.into_iter().collect(),
        }
    }

    /// 功能：解析通用生成上限。
    /// 参数：入口显式覆盖。
    /// 返回：正 u32。
    /// 错误：脱敏字段错误。
    /// 状态：无修改。
    /// 并发与取消：同步无阻塞。
    /// 权限：无。
    /// 关联：CLI 和 Host。
    pub fn max_output_tokens(&self, explicit: Option<&str>) -> Result<u32, String> {
        let value = explicit
            .or_else(|| {
                self.values
                    .get("OPENPAW_MAX_OUTPUT_TOKENS")
                    .map(String::as_str)
            })
            .unwrap_or("4096");
        value
            .trim()
            .parse::<std::num::NonZeroU32>()
            .map(|n| n.get())
            .map_err(|_| "OPENPAW_MAX_OUTPUT_TOKENS must be a positive u32".into())
    }

    /// 功能：解析有效模型，不依赖密钥。
    /// 参数：协议和显式模型。
    /// 返回：唯一有效模型。
    /// 错误：未知协议或空模型。
    /// 状态：无修改。
    /// 并发与取消：同步只读。
    /// 权限：不连接上游。
    /// 关联：幂等指纹和模型目录。
    pub fn model(&self, name: &str, explicit: Option<&str>) -> Result<String, String> {
        let (_, default, _) = PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == name)
            .ok_or_else(|| format!("unknown provider `{name}`"))?;
        let variable = format!("OPENPAW_{}_MODEL", name.to_ascii_uppercase());
        let configured = self.values.get(&variable).map(String::as_str);
        nonempty(explicit.or(configured).unwrap_or(default), &variable)
    }

    /// 功能：统一校验并解析运行配置。
    /// 参数：协议名称和显式覆盖。
    /// 返回：有效配置。
    /// 错误：仅回显字段名称，不回显非法值。
    /// 状态：无修改。
    /// 并发与取消：不发网络请求。
    /// 权限：离线协议忽略网络字段，网络密钥仅保存在私有字段。
    /// 关联：ResolvedConfig::build。
    pub fn resolve(&self, name: &str, overrides: Overrides) -> Result<ResolvedConfig, String> {
        let model = self.model(name, overrides.model.as_deref())?;
        let max_output_tokens = self.max_output_tokens(overrides.max_output_tokens.as_deref())?;
        let mut config = ResolvedConfig {
            provider: name.into(),
            model,
            max_output_tokens,
            base_url: None,
            key: None,
        };
        if matches!(name, "openai" | "anthropic") {
            let prefix = name.to_ascii_uppercase();
            let key_var = format!("{prefix}_API_KEY");
            let key = nonempty(
                self.values.get(&key_var).map(String::as_str).unwrap_or(""),
                &key_var,
            )?;
            reqwest::header::HeaderValue::from_str(&key)
                .map_err(|_| format!("{key_var} is not a valid authentication value"))?;
            let url_var = format!("OPENPAW_{prefix}_BASE_URL");
            let default = if name == "openai" {
                "https://api.openai.com/v1"
            } else {
                "https://api.anthropic.com"
            };
            let value = overrides
                .base_url
                .as_deref()
                .or_else(|| self.values.get(&url_var).map(String::as_str))
                .unwrap_or(default)
                .trim();
            let url = reqwest::Url::parse(value).map_err(|_| {
                format!(
                    "{url_var} must be an http(s) base URL without credentials, query or fragment"
                )
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(format!(
                    "{url_var} must be an http(s) base URL without credentials, query or fragment"
                ));
            }
            config.base_url = Some(value.trim_end_matches('/').into());
            config.key = Some(key);
        }
        Ok(config)
    }

    /// 功能：列出协议默认和有效配置模型。
    /// 参数：协议标识。
    /// 返回：去重模型列表。
    /// 错误：无效配置模型不会进入目录。
    /// 状态：无修改。
    /// 并发与取消：只读。
    /// 权限：无网络。
    /// 关联：model list 与 /provider。
    pub fn models(&self, name: &str) -> Vec<String> {
        let mut models: Vec<String> = PROVIDERS
            .iter()
            .filter(|(id, _, _)| *id == name)
            .map(|(_, model, _)| model.to_string())
            .collect();
        if let Ok(model) = self.model(name, None) {
            if !models.contains(&model) {
                models.push(model);
            }
        }
        models
    }
}

impl ResolvedConfig {
    /// 功能：构造共享 Provider。
    /// 参数：已校验配置。
    /// 返回：已有 LlmProvider trait 对象。
    /// 错误：无；配置先行校验。
    /// 状态：Provider 持有内存认证信息。
    /// 并发与取消：实际请求由已有实现负责。
    /// 权限：构造不连接上游。
    /// 关联：AgentLoop。
    pub fn build(&self) -> Arc<dyn LlmProvider> {
        match self.provider.as_str() {
            "openai" => Arc::new(
                crate::OpenAiProvider::new(self.key.clone().unwrap_or_default(), &self.model)
                    .with_base_url(self.base_url.clone().unwrap_or_default()),
            ),
            "anthropic" => Arc::new(
                crate::AnthropicProvider::new(self.key.clone().unwrap_or_default(), &self.model)
                    .with_base_url(self.base_url.clone().unwrap_or_default()),
            ),
            "echo" => Arc::new(crate::EchoProvider::new()),
            _ => Arc::new(crate::MockProvider::counter("mock-reply-")),
        }
    }
}

fn nonempty(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{field} must be set and nonempty"))
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(pairs: &[(&str, &str)]) -> ConfigSnapshot {
        ConfigSnapshot::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }
    #[test]
    fn invalid_configuration_never_exposes_credentials_and_offline_is_independent() {
        for url in [
            "",
            " ",
            "file:///tmp",
            "http://user:secret-url@host/v1",
            "https://host/v1?q=secret-url",
            "https://host/v1#secret-url",
        ] {
            let s = snapshot(&[
                ("OPENAI_API_KEY", "private-key"),
                ("OPENPAW_OPENAI_BASE_URL", url),
            ]);
            let error = s.resolve("openai", Default::default()).err().unwrap();
            assert!(!error.contains("private-key") && !error.contains("secret-url"));
            assert!(s.resolve("mock", Default::default()).is_ok());
            assert!(s.resolve("echo", Default::default()).is_ok());
        }
        for tokens in ["", "0", "-1", "4294967296", "invalid"] {
            assert!(snapshot(&[("OPENPAW_MAX_OUTPUT_TOKENS", tokens)])
                .resolve("mock", Default::default())
                .is_err());
        }
        for key in ["", "   ", "bad\nkey"] {
            assert!(snapshot(&[("OPENAI_API_KEY", key)])
                .resolve("openai", Default::default())
                .is_err());
        }
        let s = snapshot(&[
            ("OPENAI_API_KEY", "private-key"),
            ("OPENPAW_OPENAI_MODEL", " "),
            ("OPENPAW_OPENAI_BASE_URL", "https://host/custom/"),
        ]);
        assert!(s.resolve("openai", Default::default()).is_err());
        let c = s
            .resolve(
                "openai",
                Overrides {
                    model: Some(" explicit-model ".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(c.model, "explicit-model");
        assert_eq!(c.base_url.as_deref(), Some("https://host/custom"));
        assert_eq!(c.max_output_tokens, 4096);
    }
}
