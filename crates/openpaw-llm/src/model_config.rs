//! 职责：只读加载模型.env，与系统环境合并后复用原Provider装配。
//! 输入输出：默认数据目录、可选文件、系统环境到模型配置与来源。
//! 依赖：dotenvy负责格式，assembly负责模型及端点验证。
//! 禁止职责：不修改全局环境、不查找项目祖先目录、不记录密钥。
use crate::assemble_provider;
use openpaw_core::{Error, LlmProvider, Result};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// 用途：保留配置来源的只读模型环境，不提供可能泄漏秘密的Debug输出。
/// 字段：选定文件路径、文件内容及进程环境快照；均为私有。
/// 来源去向：可信本地启动配置到原Provider，禁止进入工具环境。
/// 约束：系统环境优先；文件错误不回显原始行；不改变进程环境。
/// 稳定性：新增可选配置能力，原Provider接口保持兼容。
/// 关联：需求/设计REQ-OP-003；DES-OP-002。
pub struct ModelEnvironment {
    path: PathBuf,
    file: BTreeMap<String, String>,
    process: BTreeMap<String, String>,
    loaded: bool,
}
impl ModelEnvironment {
    /// 功能：读取明确文件或默认OpenPaw数据目录的.env，并保存环境快照。
    /// 参数：home为原OpenPaw配置目录；explicit为可选显式路径。
    /// 返回：只读配置；默认文件不存在时返回空文件配置。
    /// 错误：显式缺失、不可读、超过1MiB、非UTF8、重复字段或格式错误；不含原始配置行。
    /// 状态：只读取文件和环境，不创建目录或更改全局环境。
    /// 并发与取消：启动时同步读取有界文件，无后台任务或共享可变环境。
    /// 权限：仅可信本地文件，不搜索工作区父目录。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn load(home: &Path, explicit: Option<&Path>) -> Result<Self> {
        let process = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        Self::read(home, explicit, process)
    }
    fn read(
        home: &Path,
        explicit: Option<&Path>,
        process: BTreeMap<String, String>,
    ) -> Result<Self> {
        let path = explicit
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".env"));
        let mut values = BTreeMap::new();
        let file = match std::fs::File::open(&path) {
            Ok(file) => Some(file),
            Err(e) if explicit.is_none() && e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(Error::Config("无法读取模型.env配置文件".into())),
        };
        let loaded = file.is_some();
        if let Some(file) = file {
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::Config("无法读取模型.env配置文件".into()))?;
            if bytes.len() > 1024 * 1024 || std::str::from_utf8(&bytes).is_err() {
                return Err(Error::Config("模型.env必须是UTF-8文本且不超过1MiB".into()));
            }
            // 只迭代解析；dotenv()/from_path()会修改进程环境并使工具继承凭据。
            for pair in
                dotenvy::from_read_iter(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            {
                let (key, value) =
                    pair.map_err(|_| Error::Config("模型.env格式错误（原始内容已隐藏）".into()))?;
                if values.insert(key, value).is_some() {
                    return Err(Error::Config(
                        "模型.env存在重复字段（原始内容已隐藏）".into(),
                    ));
                }
            }
        }
        Ok(Self {
            path,
            file: values,
            process,
            loaded,
        })
    }
    /// 功能：查询系统环境优先、文件其次的配置值。
    /// 参数：name为配置字段或Host显式凭据环境变量名。
    /// 返回：选定值；空字符串保留，不静默回退到另一来源。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：不可变快照可共享，无等待。
    /// 权限：返回值可能为秘密，调用方只能交给Provider或检查有无。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn value(&self, name: &str) -> Option<&str> {
        self.process_value(name)
            .or_else(|| self.file.get(name).map(String::as_str))
    }
    fn process_value(&self, name: &str) -> Option<&str> {
        // Windows的原生环境查询不区分大小写；快照不能改变原std::env::var语义。
        #[cfg(windows)]
        {
            self.process
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
        #[cfg(not(windows))]
        {
            self.process.get(name).map(String::as_str)
        }
    }
    /// 功能：返回字段来源，不包含字段值。
    /// 参数：name为字段名。
    /// 返回：系统环境、.env或未设置。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：无等待。
    /// 权限：来源可展示，密钥值不可展示。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn source(&self, name: &str) -> &'static str {
        if self.process_value(name).is_some() {
            "系统环境"
        } else if self.file.contains_key(name) {
            ".env"
        } else {
            "未设置"
        }
    }
    /// 功能：返回本次选择的文件和是否实际读取。
    /// 参数：无。
    /// 返回：路径及存在标志，供配置诊断使用。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：无等待。
    /// 权限：仅本机诊断，不向浏览器广播。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn file(&self) -> (&Path, bool) {
        (&self.path, self.loaded)
    }
    /// 功能：选择显式Provider、环境Provider或原入口默认值。
    /// 参数：explicit为明确参数；default为CLI mock或Host echo。
    /// 返回：有效候选名称，最终合法性由原装配器校验。
    /// 错误：无；空值保留并由装配拒绝。
    /// 状态：只读。
    /// 并发与取消：无等待。
    /// 权限：不包含凭据。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn provider<'a>(&'a self, explicit: Option<&'a str>, default: &'a str) -> &'a str {
        explicit
            .or_else(|| self.value("OPENPAW_PROVIDER"))
            .unwrap_or(default)
    }
    /// 功能：按有效Provider选择标准密钥变量名。
    /// 参数：provider为原Provider名称。
    /// 返回：网络Provider的变量名，离线或未知Provider返回None。
    /// 错误：无。
    /// 状态：只读。
    /// 并发与取消：无等待。
    /// 权限：仅返回名称，不返回秘密。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn key_name(provider: &str) -> Option<&'static str> {
        match provider {
            "openai" => Some("OPENAI_API_KEY"),
            "anthropic" => Some("ANTHROPIC_API_KEY"),
            _ => None,
        }
    }
    /// 功能：合并明确参数与环境配置并复用原Provider装配。
    /// 参数：provider/model/base_url为显式值；key_env为Host自定义变量；default为原入口默认Provider。
    /// 返回：原Provider实例及有效模型，不重复实现模型选择或HTTP传输。
    /// 错误：缺凭据、空模型、非法端点或未知Provider由原装配器拒绝。
    /// 状态：仅装配，不发起模型请求，不修改环境。
    /// 并发与取消：启动时只读，无等待任务。
    /// 权限：文件凭据只进入Provider；不写日志或继承环境。
    /// 关联：需求/设计REQ-OP-003；DES-OP-002。
    pub fn assemble(
        &self,
        provider: Option<&str>,
        model: Option<&str>,
        base_url: Option<&str>,
        key_env: Option<&str>,
        default: &str,
    ) -> Result<(Arc<dyn LlmProvider>, String)> {
        let provider = self.provider(provider, default);
        let key = key_env
            .or_else(|| Self::key_name(provider))
            .and_then(|name| self.value(name))
            .map(str::to_owned);
        assemble_provider(
            provider,
            model.or_else(|| self.value("OPENPAW_MODEL")),
            key,
            base_url.or_else(|| self.value("OPENPAW_BASE_URL")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_process_environment_names_keep_native_case_insensitivity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "OPENAI_API_KEY=file-value\n").unwrap();
        let env = ModelEnvironment::read(
            dir.path(),
            None,
            BTreeMap::from([("openai_api_key".into(), "system-value".into())]),
        )
        .unwrap();
        assert_eq!(env.value("OPENAI_API_KEY"), Some("system-value"));
        assert_eq!(env.source("OPENAI_API_KEY"), "系统环境");
    }
    #[test]
    fn system_environment_wins_even_when_empty_and_file_secrets_stay_local() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "OPENPAW_PROVIDER=openai\nOPENPAW_MODEL=deepseek-chat\nOPENAI_API_KEY=file-secret\n",
        )
        .unwrap();
        let env = ModelEnvironment::read(
            dir.path(),
            None,
            BTreeMap::from([
                ("OPENPAW_MODEL".into(), "system-model".into()),
                ("OPENAI_API_KEY".into(), String::new()),
            ]),
        )
        .unwrap();
        assert_eq!(env.value("OPENPAW_MODEL"), Some("system-model"));
        assert_eq!(env.source("OPENPAW_MODEL"), "系统环境");
        assert_eq!(env.value("OPENAI_API_KEY"), Some(""));
        assert!(env.assemble(None, None, None, None, "mock").is_err());
    }
}
