//! 职责：提供通用图片生成和结构化SVG图表工具。
//! 输入输出：已批准的提示词/图表数据到工作区实际文件及回执。
//! 依赖：原路径校验、HTTP客户端和原工具调用上下文。
//! 禁止职责：不决定文章流程，不授予写权限，不把描述当图片。
use base64::Engine;
use openpaw_core::{Error, Result, ToolCallOutcome, ToolCategory, ToolConcurrency, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tokio_util::sync::CancellationToken;

const MAX_IMAGE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 用途：由可信调用方注入图片服务，不读取模型提交的服务地址。
/// 字段：服务根URL、模型及密钥环境变量名，不存密钥。
/// 来源去向：本机配置到原工具注册表。
/// 约束：不跟随重定向，不回显服务错误正文。
/// 稳定性：可选工具配置，默认不启用远端生图。
/// 关联：CHG-2026-0007第9.12节。
pub struct ImageConfig {
    /// 用途：图片API的可信根地址。
    /// 字段：String；如https://服务/v1。
    /// 来源去向：本机TOML到HTTP客户端。
    /// 约束：只允许HTTPS或loopback HTTP，模型不可改地址。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub base_url: String,
    /// 用途：图片服务的真实模型标识。
    /// 字段：String；不回退聊天模型。
    /// 来源去向：本机TOML到图片请求。
    /// 约束：须非空且不超过256字节。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub model: String,
    /// 用途：独立图片凭据的配置键名。
    /// 字段：String；只存名字。
    /// 来源去向：TOML到原ModelEnvironment查找。
    /// 约束：不能使用模型输入指定凭据。
    /// 稳定性：本次新增可选能力或内部控制记录。
    /// 关联：CHG-2026-0007第9.12节。
    pub api_key_env: String,
    /// 用途：兼容需要显式要求base64的图片服务。
    /// 字段：Option<String>；默认不发送，显式仅允许b64_json。
    /// 来源去向：可信TOML到HTTP请求。
    /// 约束：由服务契约选择，不从模型名字猜字段支持。
    /// 稳定性：可选字段，缺省兼容不接受response_format的服务。
    /// 关联：CHG-2026-0007第9.12节。
    #[serde(default)]
    pub response_format: Option<String>,
}

/// 用途：持有已解析凭据的只读图片服务运行对象。
/// 字段：私有可信配置与密钥，Debug隐藏凭据。
/// 来源去向：CLI原ModelEnvironment或可信Rust调用方到原工具注册表。
/// 约束：不序列化，不注入工具进程环境，不从工具输入取凭据。
/// 稳定性：可选媒体运行装配。
/// 关联：CHG-2026-0007第9.12节。
#[derive(Clone)]
pub struct ImageService {
    config: ImageConfig,
    key: String,
}
impl std::fmt::Debug for ImageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageService")
            .field("model", &self.config.model)
            .field("key", &"[redacted]")
            .finish()
    }
}
impl ImageService {
    /// 功能：绑定可信图片配置与正式配置加载器解析的密钥。
    /// 参数：config为本机图片服务配置，key为独立图片凭据。
    /// 返回：不含可公开序列化凭据的服务对象。
    /// 错误：地址、模型或密钥为空或不安全时拒绝。
    /// 状态：不联网，不修改进程环境。
    /// 并发与取消：只读对象可克隆，每次请求独立取消。
    /// 权限：仅可信本机装配调用；仅loopback允许明文HTTP。
    /// 关联：CHG-2026-0007第9.12节。
    pub fn new(config: ImageConfig, key: String) -> Result<Self> {
        let url =
            reqwest::Url::parse(&config.base_url).map_err(|_| invalid("图片服务地址不合法"))?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if (url.scheme() != "https" && !(url.scheme() == "http" && local))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
        {
            return Err(invalid(
                "图片服务必须使用HTTPS或本机HTTP，地址不能含凭据或查询参数",
            ));
        }
        if config
            .response_format
            .as_deref()
            .is_some_and(|value| value != "b64_json")
        {
            return Err(invalid("图片response_format只能省略或指定b64_json"));
        }
        if key.trim().is_empty()
            || key.contains(['\r', '\n'])
            || config.model.trim().is_empty()
            || config.model.len() > 256
            || config.api_key_env.trim().is_empty()
        {
            return Err(invalid("图片服务模型或独立密钥未配置或不合法"));
        }
        Ok(Self { config, key })
    }
}

/// 功能：生成媒体工具的原注册表声明。
/// 参数：无。
/// 返回：图片生成与SVG渲染声明。
/// 错误：无。
/// 状态：不执行网络或写入。
/// 并发与取消：纯计算。
/// 权限：两工具都要求准确输入的写入审批。
/// 关联：CHG-2026-0007第9.12节。
pub fn specs() -> Vec<ToolSpec> {
    [
        ("generate_image", "调用已配置图片服务并保存实际PNG/JPEG/WebP；没有配置时明确失败，不生成占位图片。", json!({"type":"object","properties":{"path":{"type":"string"},"prompt":{"type":"string","maxLength":16000},"size":{"type":"string","enum":["1024x1024","1024x1536","1536x1024"]}},"required":["path","prompt"],"additionalProperties":false})),
        ("render_visual", "将结构化流程图或带来源的数据柱状图渲染为实际SVG文件。kind=diagram需要nodes/edges；kind=bar_chart需要data/source。", json!({"type":"object","properties":{"path":{"type":"string"},"title":{"type":"string"},"kind":{"type":"string","enum":["diagram","bar_chart"]},"nodes":{"type":"array","maxItems":24,"items":{"type":"object","properties":{"id":{"type":"string"},"label":{"type":"string"}},"required":["id","label"],"additionalProperties":false}},"edges":{"type":"array","maxItems":48,"items":{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"},"label":{"type":"string"}},"required":["from","to"],"additionalProperties":false}},"data":{"type":"array","maxItems":24,"items":{"type":"object","properties":{"label":{"type":"string"},"value":{"type":"number","minimum":0}},"required":["label","value"],"additionalProperties":false}},"source":{"type":"string"}},"required":["path","title","kind"],"additionalProperties":false})),
    ].into_iter().map(|(name,description,input_schema)|ToolSpec { name:name.into(),description:description.into(),input_schema,category:Some(ToolCategory::Write),concurrency:Some(ToolConcurrency::PathScoped),requires_approval:true,is_destructive:Some(true),..Default::default() }).collect()
}

fn invalid(message: &str) -> Error {
    Error::ToolHost(message.into())
}
fn string<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("媒体参数缺少非空字符串"))
}
fn escape(s: &str) -> String {
    s.chars()
        .take(160)
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            '\'' => "&apos;".chars().collect(),
            c if !c.is_control() => vec![c],
            _ => vec![' '],
        })
        .collect()
}
fn svg(args: &Value) -> Result<String> {
    let title = escape(string(args, "title")?);
    let mut body = String::new();
    let height;
    match string(args, "kind")? {
        "bar_chart" => {
            let source = escape(string(args, "source")?);
            let data = args
                .get("data")
                .and_then(Value::as_array)
                .filter(|d| !d.is_empty() && d.len() <= 24)
                .ok_or_else(|| invalid("柱状图需要1至24个数据点及来源"))?;
            let values: Vec<f64> = data
                .iter()
                .map(|d| {
                    d["value"]
                        .as_f64()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .ok_or_else(|| invalid("图表数据必须为非负有限数"))
                })
                .collect::<Result<_>>()?;
            let max = values.iter().copied().fold(1.0_f64, f64::max);
            for (i, (d, value)) in data.iter().zip(values).enumerate() {
                let y = 70 + i * 42;
                body.push_str(&format!("<text x=\"20\" y=\"{}\">{}</text><rect x=\"220\" y=\"{}\" width=\"{}\" height=\"24\" fill=\"#2563eb\"/><text x=\"810\" y=\"{}\">{value}</text>",y+18,escape(string(d,"label")?),y,value/max*570.0,y+18));
            }
            height = 120 + data.len() * 42;
            body.push_str(&format!(
                "<text x=\"20\" y=\"{}\">数据来源（调用方提供，须另行核实）：{source}</text>",
                height - 20
            ));
        }
        "diagram" => {
            let nodes = args
                .get("nodes")
                .and_then(Value::as_array)
                .filter(|n| !n.is_empty() && n.len() <= 24)
                .ok_or_else(|| invalid("流程图需要1至24个节点"))?;
            let mut ids = std::collections::BTreeMap::new();
            for (i, n) in nodes.iter().enumerate() {
                if ids.insert(string(n, "id")?, i).is_some() {
                    return Err(invalid("流程图节点ID重复"));
                }
            }
            height = 120 + nodes.len().div_ceil(3) * 110;
            if let Some(edges) = args.get("edges").and_then(Value::as_array) {
                if edges.len() > 48 {
                    return Err(invalid("流程图边过多"));
                }
                for e in edges {
                    let a = *ids
                        .get(string(e, "from")?)
                        .ok_or_else(|| invalid("边的起点不存在"))?;
                    let b = *ids
                        .get(string(e, "to")?)
                        .ok_or_else(|| invalid("边的终点不存在"))?;
                    if a == b {
                        return Err(invalid("流程图暂不支持自环边"));
                    }
                    let (cx1, cy1) = ((150 + (a % 3) * 300) as f64, (100 + (a / 3) * 110) as f64);
                    let (cx2, cy2) = ((150 + (b % 3) * 300) as f64, (100 + (b / 3) * 110) as f64);
                    let (dx, dy) = (cx2 - cx1, cy2 - cy1);
                    let inset = 1.0 / (dx.abs() / 120.0).max(dy.abs() / 25.0);
                    let (x1, y1) = (cx1 + dx * inset, cy1 + dy * inset);
                    let (x2, y2) = (cx2 - dx * inset, cy2 - dy * inset);
                    // 标签以边中点居中，避免从中点向右延伸而被相邻节点遮住。
                    body.push_str(&format!("<path d=\"M{x1} {y1} L{x2} {y2}\" stroke=\"#64748b\" stroke-width=\"2\" marker-end=\"url(#arrow)\"/><text x=\"{}\" y=\"{}\" text-anchor=\"middle\">{}</text>",(x1+x2)/2.0,(y1+y2)/2.0-8.0,escape(e["label"].as_str().unwrap_or(""))));
                }
            }
            for (i, n) in nodes.iter().enumerate() {
                let (x, y) = (30 + (i % 3) * 300, 75 + (i / 3) * 110);
                body.push_str(&format!("<rect x=\"{x}\" y=\"{y}\" width=\"240\" height=\"50\" rx=\"8\" fill=\"#eff6ff\" stroke=\"#2563eb\"/><text x=\"{}\" y=\"{}\" text-anchor=\"middle\">{}</text>",x+120,y+30,escape(string(n,"label")?)));
            }
        }
        _ => return Err(invalid("未知图表类型")),
    }
    Ok(format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"{height}\" viewBox=\"0 0 960 {height}\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><defs><marker id=\"arrow\" markerWidth=\"10\" markerHeight=\"10\" refX=\"9\" refY=\"3\" orient=\"auto\"><path d=\"M0,0 L0,6 L9,3 z\" fill=\"#64748b\"/></marker></defs><g font-family=\"sans-serif\" font-size=\"15\" fill=\"#172033\"><text x=\"20\" y=\"35\" font-size=\"22\">{title}</text>{body}</g></svg>"))
}

async fn image(
    service: &ImageService,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let config = &service.config;
    let url = reqwest::Url::parse(&format!(
        "{}/images/generations",
        config.base_url.trim_end_matches('/')
    ))
    .map_err(|_| invalid("图片服务地址不合法"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("图片服务地址不能包含凭据或查询参数"));
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(240))
        .build()
        .map_err(|_| invalid("图片HTTP客户端初始化失败"))?;
    let prompt = string(args, "prompt")?;
    if prompt.chars().count() > 16000 {
        return Err(invalid("图片提示词过长"));
    }
    let mut body = json!({"model":config.model,"prompt":prompt,"n":1,"size":args["size"].as_str().unwrap_or("1024x1024")});
    if let Some(format) = &config.response_format {
        body["response_format"] = json!(format);
    }
    let mut response = tokio::select! { biased;_=cancel.cancelled()=>return Err(Error::Cancelled),r=client.post(url).bearer_auth(&service.key).json(&body).send()=>r.map_err(|_|invalid("图片服务连接失败"))? };
    if !response.status().is_success() {
        return Err(invalid("图片服务返回失败状态；未保存图片"));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {biased;_=cancel.cancelled()=>return Err(Error::Cancelled),r=response.chunk()=>r.map_err(|_|invalid("图片响应读取失败"))?};
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > MAX_IMAGE * 2 {
            return Err(invalid("图片响应超过上限"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let data: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("图片服务返回无效JSON"))?;
    let encoded = data["data"][0]["b64_json"]
        .as_str()
        .ok_or_else(|| invalid("图片响应缺少实际base64图片，不接受占位URL"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid("图片base64无效"))?;
    if bytes.len() > MAX_IMAGE
        || !(bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || bytes.starts_with(b"\xff\xd8\xff")
            || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")))
    {
        return Err(invalid("响应不是受支持的实际图片或超过上限"));
    }
    let path = string(args, "path")?.to_ascii_lowercase();
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes.len() >= 45
        && bytes.get(12..16) == Some(b"IHDR")
        && bytes.ends_with(b"\x00\x00\x00\x00IEND\xaeB`\x82")
        && bytes.get(16..24).is_some_and(|b| {
            u32::from_be_bytes(b[..4].try_into().unwrap()) > 0
                && u32::from_be_bytes(b[4..].try_into().unwrap()) > 0
        });
    let jpeg =
        bytes.starts_with(b"\xff\xd8\xff") && bytes.ends_with(b"\xff\xd9") && bytes.len() > 16;
    let webp = bytes.len() > 20
        && bytes.starts_with(b"RIFF")
        && bytes.get(8..12) == Some(b"WEBP")
        && u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize == bytes.len() - 8
        && matches!(bytes.get(12..16), Some(b"VP8 " | b"VP8L" | b"VP8X"));
    if !((png && path.ends_with(".png"))
        || (jpeg && (path.ends_with(".jpg") || path.ends_with(".jpeg")))
        || (webp && path.ends_with(".webp")))
    {
        return Err(invalid("图片结构或扩展名与实际格式不符；未保存图片"));
    }
    Ok(bytes)
}

/// 功能：执行已授权媒体生成并原子保存结果。
/// 参数：workspace为授权根，config为已绑定独立凭据的可信图片服务，name/args为最终输入，cancel为调用取消信号。
/// 返回：实际文件路径、字节数和生成类型；不返回密钥或图片正文。
/// 错误：配置/输入/服务/路径/写入错误；取消返回Cancelled。
/// 状态：完整生成后才提交文件，临时文件失败自动清理。
/// 并发与取消：取消不提交；调用方仍须持工作区写锁并等待完成。
/// 权限：调用方的原注册表负责准确输入批准；本函数复验路径。
/// 关联：CHG-2026-0007第9.12节。
pub async fn run(
    workspace: &Path,
    config: Option<&ImageService>,
    name: &str,
    args: Value,
    cancel: CancellationToken,
) -> Result<ToolCallOutcome> {
    let path = string(&args, "path")?;
    let target = crate::safety::resolve(workspace, path)?;
    let bytes = match name {
        "render_visual" => {
            if !path.ends_with(".svg") {
                return Err(invalid("SVG图表路径必须以.svg结尾"));
            }
            svg(&args)?.into_bytes()
        }
        "generate_image" => {
            image(
                config.ok_or_else(|| invalid("尚未配置图片生成服务"))?,
                &args,
                &cancel,
            )
            .await?
        }
        _ => return Err(invalid("未知媒体工具")),
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let parent = target
        .parent()
        .ok_or_else(|| invalid("媒体路径缺少父目录"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| invalid("创建媒体目录失败"))?;
    let bytes_len = bytes.len();
    let checked = crate::safety::resolve(workspace, path)?;
    if checked != target {
        return Err(invalid("媒体路径在生成期间发生变化"));
    }
    let output = target.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new_in(parent_path(&output)?)
            .map_err(|_| invalid("创建媒体临时文件失败"))?;
        tmp.write_all(&bytes)
            .and_then(|_| tmp.as_file().sync_all())
            .map_err(|_| invalid("保存媒体失败"))?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        tmp.persist(&output)
            .map_err(|_| invalid("提交媒体文件失败"))?;
        Ok(())
    })
    .await
    .map_err(|_| invalid("媒体写入任务异常"))??;
    Ok(ToolCallOutcome::ok(json!({"path":path,"bytes":bytes_len,"kind":name,"saved":true,"visual_review":"not_performed"}).to_string()))
}
fn parent_path(path: &Path) -> Result<&Path> {
    path.parent().ok_or_else(|| invalid("媒体路径无父目录"))
}
